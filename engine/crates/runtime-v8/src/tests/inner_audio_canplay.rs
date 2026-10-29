//! `canplay` is the moment a game reads `duration`, so the duration must be there when it fires.
//!
//! The context pulled `duration` from the audio thread with an async op when the native `canPlay` event arrived and
//! fired the `canplay` listeners without waiting for the answer: every `onCanplay` callback read `duration === 0` (it
//! was right 50 ms later). Howler.js's HTML5 path reads `node.duration` in exactly that callback, computed a zero-length
//! sound and ended it before it had started.

#[cfg(test)]
mod inner_audio_canplay_tests {
    use super::super::support::test_host_state_with_audio;
    use deno_core::{FastString, JsRuntime, RuntimeOptions, v8};
    use shared::protocol::audio_cmd::AudioCmd;

    struct SourceState(tokio::sync::Notify);
    impl shared::audio_load::AudioLoadState for SourceState {
        fn generation(&self) -> u64 {
            1
        }
        fn generation_changed(&self) -> &tokio::sync::Notify {
            &self.0
        }
    }

    /// A host that accepts preparations and rejects redundant state queries.
    fn boot() -> (JsRuntime, std::thread::JoinHandle<()>) {
        let (tx, rx) = shared::audio_channel::channel();
        let responder = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                match rx.try_recv() {
                    Ok(AudioCmd::InnerAudioBeginLoad {
                        source_id, resp, ..
                    }) => {
                        let state = std::sync::Arc::new(SourceState(tokio::sync::Notify::new()));
                        let _ = resp.send(Ok(shared::audio_load::InnerAudioLoadToken::new(
                            &state, 1, source_id,
                        )));
                    }

                    Ok(AudioCmd::InnerAudioGetState { .. }) => {
                        panic!("event snapshot must not query state")
                    }
                    Ok(_) => {}
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        });
        let runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state_with_audio(tx)),
            ..Default::default()
        });
        (runtime, responder)
    }

    fn run(runtime: &mut JsRuntime, source: &'static str) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        let _guard = executor.enter();
        runtime
            .execute_script(
                "<test:inner-audio-canplay>",
                FastString::from_static(source),
            )
            .expect("script executes");
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .expect("event loop drains without an unhandled rejection");
        runtime
            .execute_script(
                "<test:inner-audio-canplay-result>",
                FastString::from_static(
                    "if (!globalThis.__done) throw new Error(globalThis.__failure || 'unfinished'); if (globalThis.__check) globalThis.__check();",
                ),
            )
            .expect("assertions hold");
    }

    #[test]
    fn duration_is_known_when_canplay_fires_and_events_keep_their_order() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let (mut runtime, responder) = boot();
        run(
            &mut runtime,
            r#"
            globalThis.__done = false;
            (() => {
                try {
                    const bridge = globalThis[Symbol.for('Migo.hostBridge')];
                    const ctx = migo.createInnerAudioContext();
                    const seen = [];
                    ctx.onCanplay(() => seen.push('canplay duration=' + ctx.duration));
                    ctx.onPlay(() => seen.push('play'));
                    // The native side sends canPlay and, straight after it, play (an autoplaying source).
                    bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'canPlay', 0, 0, 0.25, true, true);
                    bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'play', 0, 0, 0.25, true, false);
                    setTimeout(() => {
                        try {
                            const expected = ['canplay duration=0.25', 'play'];
                            if (JSON.stringify(seen) !== JSON.stringify(expected)) {
                                throw new Error('saw ' + JSON.stringify(seen));
                            }
                            globalThis.__done = true;
                        } catch (error) {
                            globalThis.__failure = String(error);
                        }
                    }, 300);
                } catch (error) {
                    globalThis.__failure = String(error);
                }
            })();
        "#,
        );
        drop(runtime);
        responder.join().expect("responder");
    }
    #[test]
    fn queued_events_from_the_previous_source_cannot_change_the_new_source() {
        let executor = crate::tests::support::v8_test_executor();
        let _enter = executor.as_ref().map(|executor| executor.enter());
        let (mut runtime, responder) = boot();
        run(
            &mut runtime,
            r#"
const ctx = migo.createInnerAudioContext(); const seen = [];
ctx.onCanplay(() => seen.push('canplay')); ctx.onPlay(() => seen.push('play'));
const bridge = globalThis[Symbol.for('Migo.hostBridge')];
bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'canPlay', 0, 0, 20, true, true);
bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'play', 0, 0, 20, true, false);
ctx.src = '/missing-current-source.mp3';
globalThis.__check = () => {
 if (seen.length || ctx.duration !== 0 || !ctx.paused || ctx.buffered) throw new Error('stale source snapshot applied');
}; globalThis.__done = true;
"#,
        );
        drop(runtime);
        responder.join().unwrap();
    }
    #[test]
    fn late_load_errors_are_reported_only_for_the_latest_source() {
        let executor = crate::tests::support::v8_test_executor();
        let _enter = executor.as_ref().map(|executor| executor.enter());
        let (mut runtime, responder) = boot();
        run(
            &mut runtime,
            r#"
const ctx = migo.createInnerAudioContext(); const errors = [];
ctx.onError(error => errors.push(error.errCode));
ctx.src = '/missing-old-source.mp3'; ctx.src = 'https://';
globalThis.__check = () => {
 if (JSON.stringify(errors) !== '[10002]') throw new Error('stale load error: ' + JSON.stringify(errors));
 if (!ctx.paused || ctx.buffered || ctx.duration !== 0) throw new Error('failed current source retained old state');
}; globalThis.__done = true;
"#,
        );
        drop(runtime);
        responder.join().unwrap();
    }
    #[test]
    fn a_listener_source_switch_rejects_the_rest_of_the_old_event_batch() {
        let executor = crate::tests::support::v8_test_executor();
        let _enter = executor.as_ref().map(|executor| executor.enter());
        let (mut runtime, responder) = boot();
        run(
            &mut runtime,
            r#"
const ctx = migo.createInnerAudioContext(); const seen = [];
ctx.onCanplay(() => { seen.push(ctx.duration); ctx.src = '/missing-replacement.mp3'; });
ctx.onPlay(() => seen.push('old play'));
const bridge = globalThis[Symbol.for('Migo.hostBridge')];
bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'canPlay', 0, 0, 0.25, true, true);
bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'play', 0, 0, 0.25, true, false);
globalThis.__check = () => {
 if (JSON.stringify(seen) !== '[0.25]' || !ctx.paused || ctx.duration !== 0) throw new Error('reentrant source switch accepted old event');
}; globalThis.__done = true;
"#,
        );
        drop(runtime);
        responder.join().unwrap();
    }
    #[test]
    fn progress_events_refresh_duration_and_flags_without_a_state_request() {
        let executor = crate::tests::support::v8_test_executor();
        let _enter = executor.as_ref().map(|executor| executor.enter());
        let (mut runtime, responder) = boot();
        run(
            &mut runtime,
            r#"
const ctx = migo.createInnerAudioContext(); const seen = [];
ctx.onTimeUpdate(() => seen.push([ctx.currentTime, ctx.duration, ctx.buffered, ctx.paused]));
const bridge = globalThis[Symbol.for('Migo.hostBridge')];
bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'timeUpdate', 1.5, 0, 3, true, false);
bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'timeUpdate', 2, 0, 4.5, true, true);
globalThis.__check = () => {
 const expected = [[1.5,3,3,false],[2,4.5,4.5,true]];
 if (JSON.stringify(seen) !== JSON.stringify(expected)) throw new Error('event state snapshot: '+JSON.stringify(seen));
}; globalThis.__done = true;
"#,
        );
        drop(runtime);
        responder.join().unwrap();
    }

    #[test]
    fn direct_native_dispatch_preserves_the_entire_source_snapshot() {
        let executor = crate::tests::support::v8_test_executor();
        let _enter = executor.as_ref().map(|executor| executor.enter());
        let (mut runtime, responder) = boot();
        let value = runtime.execute_script("<audio-native-dispatch-setup>",
            "globalThis.__seen = []; globalThis.__ctx = migo.createInnerAudioContext(); __ctx.onTimeUpdate(() => __seen.push([__ctx.currentTime, __ctx.duration, __ctx.buffered, __ctx.paused])); __ctx._getId();").unwrap();
        let id = {
            let context = runtime.main_context();
            let isolate = runtime.v8_isolate();
            deno_core::v8::scope_with_context!(scope, isolate, &context);
            let value = deno_core::v8::Local::new(scope, value);
            value.uint32_value(scope).unwrap()
        };
        let bindings = crate::js_bindings::JsBindings::new(&mut runtime, 0);
        bindings.dispatch_inner_audio_event(
            &mut runtime,
            0,
            &shared::protocol::audio_cmd::InnerAudioEvent {
                id,
                source_id: 0,
                event_type: shared::protocol::host_cmd::InnerAudioEventType::TimeUpdate,
                current_time: 0.5,
                duration: 1.25,
                buffered: true,
                paused: false,
            },
        );
        run(
            &mut runtime,
            r#"
            globalThis.__done = true;
            globalThis.__check = () => {
                if (JSON.stringify(__seen) !== '[[0.5,1.25,1.25,false]]') throw new Error('native snapshot argument mismatch: ' + JSON.stringify(__seen));
            };
        "#,
        );
        drop(bindings);
        drop(runtime);
        responder.join().unwrap();
    }
}
