//! `onended` of the scheduled source nodes: the node asks the audio thread to watch it only when a handler is set, and the
//! host-bridge hook the audio thread's report arrives as fires the handler once.
//!
//! The nodes accepted `onended` and told the console that "callbacks are not dispatched by the native graph": Phaser's
//! `complete`, three.js's `onEnded` and every sound library that chains one sound after another never heard a sound end.

#[cfg(test)]
mod audio_source_ended_tests {
    use super::super::support::test_host_state_with_audio;
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::protocol::audio_cmd::AudioCmd;
    use std::sync::{Arc, Mutex};

    /// An audio thread that records the watch commands it is sent.
    fn boot() -> (
        JsRuntime,
        std::thread::JoinHandle<()>,
        Arc<Mutex<Vec<(u32, bool)>>>,
    ) {
        let (tx, rx) = shared::audio_channel::channel();
        let watches = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&watches);
        let responder = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                match rx.try_recv() {
                    Ok(AudioCmd::WatchSourceEnded { node_id, enabled }) => {
                        seen.lock().unwrap().push((node_id, enabled));
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
        (runtime, responder, watches)
    }

    fn run(runtime: &mut JsRuntime, source: &'static str) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        let _guard = executor.enter();
        runtime
            .execute_script("<test:audio-source-ended>", FastString::from_static(source))
            .expect("script executes");
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .expect("event loop drains without an unhandled rejection");
        runtime
            .execute_script(
                "<test:audio-source-ended-result>",
                FastString::from_static(
                    "if (!globalThis.__done) throw new Error(globalThis.__failure || 'unfinished');",
                ),
            )
            .expect("assertions hold");
    }

    #[test]
    fn a_handler_starts_the_watch_and_the_report_fires_it_once() {
        let (mut runtime, responder, watches) = boot();
        run(
            &mut runtime,
            r#"
            globalThis.__done = false;
            (() => {
                try {
                    const bridge = globalThis[Symbol.for('Migo.hostBridge')];
                    const ctx = new AudioContext();
                    const quiet = ctx.createBufferSource();      // never given a handler: costs the audio thread nothing
                    const source = ctx.createBufferSource();
                    const seen = [];
                    source.onended = function (event) {
                        seen.push(event.type + ':' + (event.target === source) + ':' + (this === source));
                    };
                    const report = (node) => bridge._internalDispatch('_internalTriggerAudioSourceEnded', '[' + node._nodeId + ']');
                    report(source);
                    report(source);                              // the spec fires ended once
                    report(quiet);                               // nobody is listening: nothing happens
                    if (JSON.stringify(seen) !== JSON.stringify(['ended:true:true'])) {
                        throw new Error('saw ' + JSON.stringify(seen));
                    }
                    // a handler that throws does not stop the next sound's
                    const thrower = ctx.createBufferSource();
                    thrower.onended = () => { throw new Error('a game bug'); };
                    const after = ctx.createBufferSource();
                    after.onended = () => seen.push('after');
                    report(thrower);
                    report(after);
                    if (seen[seen.length - 1] !== 'after') throw new Error('a throwing handler stopped the next: ' + JSON.stringify(seen));
                    globalThis.__sourceId = source._nodeId;
                    globalThis.__quietId = quiet._nodeId;
                    globalThis.__done = true;
                } catch (error) {
                    globalThis.__failure = String(error);
                }
            })();
        "#,
        );
        drop(runtime);
        responder.join().expect("responder");
        let watches = watches.lock().unwrap().clone();
        assert!(
            watches.iter().all(|&(_, enabled)| enabled),
            "only watches were asked for: {watches:?}"
        );
        assert_eq!(
            watches.len(),
            3,
            "source, thrower and after were watched; quiet was not: {watches:?}"
        );
    }

    #[test]
    fn clearing_the_handler_stops_the_watch_and_the_node_never_reports() {
        let (mut runtime, responder, watches) = boot();
        run(
            &mut runtime,
            r#"
            globalThis.__done = false;
            (() => {
                try {
                    const bridge = globalThis[Symbol.for('Migo.hostBridge')];
                    const ctx = new AudioContext();
                    const source = ctx.createBufferSource();
                    let fired = 0;
                    source.onended = () => fired++;
                    source.onended = null;
                    source.onended = 'not a function';           // anything but a function clears
                    bridge._internalDispatch('_internalTriggerAudioSourceEnded', '[' + source._nodeId + ']');
                    if (fired !== 0) throw new Error('a cleared handler fired');
                    // oscillators and constant sources take part too
                    const osc = ctx.createOscillator();
                    let oscEnded = 0;
                    osc.onended = () => oscEnded++;
                    bridge._internalDispatch('_internalTriggerAudioSourceEnded', '[' + osc._nodeId + ']');
                    if (oscEnded !== 1) throw new Error('oscillator onended did not fire');
                    globalThis.__done = true;
                } catch (error) {
                    globalThis.__failure = String(error);
                }
            })();
        "#,
        );
        drop(runtime);
        responder.join().expect("responder");
        let watches = watches.lock().unwrap().clone();
        assert_eq!(
            watches
                .iter()
                .map(|&(_, enabled)| enabled)
                .collect::<Vec<_>>(),
            [true, false, true],
            "watch, unwatch (once: clearing twice is not two commands), then the oscillator's watch: {watches:?}"
        );
    }
}
