//! An InnerAudioContext source that will not load is reported with the reference's errCode for why: 10002 a network
//! error, 10003 a file error, 10004 a format error. Every failure used to be 10002, so a game told a missing packaged
//! sound from an unreachable server by nothing.

#[cfg(test)]
mod inner_audio_error_codes_tests {
    use super::super::support::test_host_state_with_audio;
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::error::{EngineError, ErrorCode};
    use shared::protocol::audio_cmd::AudioCmd;

    /// An audio thread whose decoder refuses every file it is given.
    fn boot() -> (JsRuntime, std::thread::JoinHandle<()>) {
        let (tx, rx) = shared::audio_channel::channel();
        let responder = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                match rx.try_recv() {
                    Ok(AudioCmd::InnerAudioLoad { resp, .. }) => {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::InvalidArgument,
                            "not a format the decoder knows",
                        )));
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

    #[test]
    fn a_source_that_will_not_load_says_why() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let undecodable = std::env::temp_dir().join(format!(
            "migo-inner-audio-undecodable-{}.mp3",
            std::process::id()
        ));
        std::fs::write(&undecodable, b"not audio").expect("write the file");
        let (mut runtime, responder) = boot();
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        let _guard = executor.enter();
        let script = format!(
            r#"
            globalThis.__done = false;
            const codes = {{}};
            const heard = (name, src) => new Promise((resolve) => {{
                const ctx = migo.createInnerAudioContext();
                ctx.onError((res) => {{ codes[name] = res.errCode; resolve(); }});
                ctx.src = src;
            }});
            Promise.all([
                heard("missing", "/no/such/dir/clip.mp3"),
                heard("malformed", "https://"),
                heard("undecodable", {undecodable:?}),
            ]).then(() => {{
                // A file that is not there, a URL that is not one, bytes the decoder refuses.
                const want = {{ missing: 10003, malformed: 10002, undecodable: 10004 }};
                if (Object.keys(want).some((name) => codes[name] !== want[name])) {{
                    globalThis.__failure = "heard " + JSON.stringify(codes) + ", want " + JSON.stringify(want);
                }} else {{
                    globalThis.__done = true;
                }}
            }});
            "#,
        );
        runtime
            .execute_script("<test:inner-audio-error-codes>", FastString::from(script))
            .expect("script executes");
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .expect("event loop drains");
        runtime
            .execute_script(
                "<test:inner-audio-error-codes-result>",
                FastString::from_static(
                    "if (!globalThis.__done) throw new Error(globalThis.__failure || 'unfinished');",
                ),
            )
            .expect("each source's errCode says why it would not load");
        drop(runtime);
        responder.join().expect("responder");
        let _ = std::fs::remove_file(undecodable);
    }
}
