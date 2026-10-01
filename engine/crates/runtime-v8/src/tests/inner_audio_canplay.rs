//! `canplay` is the moment a game reads `duration`, so the duration must be there when it fires.
//!
//! The context pulled `duration` from the audio thread with an async op when the native `canPlay` event arrived and
//! fired the `canplay` listeners without waiting for the answer: every `onCanplay` callback read `duration === 0` (it
//! was right 50 ms later). Howler.js's HTML5 path reads `node.duration` in exactly that callback, computed a zero-length
//! sound and ended it before it had started.

#[cfg(test)]
mod inner_audio_canplay_tests {
    use super::super::support::test_host_state_with_audio;
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::protocol::audio_cmd::{AudioCmd, InnerAudioState};

    /// An audio thread that answers `InnerAudioGetState` with a 0.25 s sound, a little late, as a real one does.
    fn boot() -> (JsRuntime, std::thread::JoinHandle<()>) {
        let (tx, rx) = shared::audio_channel::channel();
        let responder = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                match rx.try_recv() {
                    Ok(AudioCmd::InnerAudioGetState { resp, .. }) => {
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        let _ = resp.send(Ok(InnerAudioState {
                            current_time: 0.0,
                            duration: 0.25,
                            paused: true,
                            volume: 1.0,
                            loop_enabled: false,
                            playback_rate: 1.0,
                            buffered: true,
                        }));
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
                    "if (!globalThis.__done) throw new Error(globalThis.__failure || 'unfinished');",
                ),
            )
            .expect("assertions hold");
    }

    #[test]
    fn duration_is_known_when_canplay_fires_and_events_keep_their_order() {
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
                    bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'canPlay', 0);
                    bridge._internalEnqueueInnerAudioEvent(ctx._getId(), 'play', 0);
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
}
