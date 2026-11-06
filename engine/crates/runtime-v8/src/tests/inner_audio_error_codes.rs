//! An InnerAudioContext source that will not load is reported with the reference's errCode for why: 10002 a network
//! error, 10003 a file error, 10004 a format error. Every failure used to be 10002, so a game told a missing packaged
//! sound from an unreachable server by nothing.

#[cfg(test)]
mod inner_audio_error_codes_tests {
    use super::super::support::test_host_state_with_audio;
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::error::{EngineError, ErrorCode};
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

    /// An audio thread whose decoder refuses every file it is given.
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

                    Ok(AudioCmd::InnerAudioCompleteLoad { resp, .. }) => {
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
    #[test]
    fn actual_v8_audio_source_op_uses_the_session_package_mount() {
        use std::sync::Arc;
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let root =
            std::env::temp_dir().join(format!("migo-v8-package-audio-{}", std::process::id()));
        let paths = Arc::new(
            shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1)
                .unwrap(),
        );
        paths.ensure_directories().unwrap();
        std::fs::create_dir_all(paths.code_dir()).unwrap();
        std::fs::write(paths.code_dir().join("clip"), b"stale VFS shadow").unwrap();
        let package = root.join("audio.mpkg");
        let mut writer =
            shared::vfs::PackageWriter::new(std::fs::File::create(&package).unwrap()).unwrap();
        writer
            .add_entry("clip", b"actual V8 package input")
            .unwrap();
        writer.finish("audio", "1").unwrap();
        let mounts = Arc::new(shared::vfs::MountTable::new(paths.code_dir().to_path_buf()));
        mounts.swap_base(Arc::new(
            shared::vfs::PackSource::open(&package, "audio", "1").unwrap(),
        ));
        let (tx, rx) = shared::audio_channel::channel();
        let responder = std::thread::spawn(move || {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                assert!(
                    std::time::Instant::now() < end,
                    "V8 package source never reached audio command"
                );
                match rx.try_recv() {
                    Ok(AudioCmd::InnerAudioBeginLoad {
                        id,
                        source_id,
                        resp,
                    }) => {
                        assert_eq!((id, source_id), (31, 77));
                        let state = Arc::new(SourceState(tokio::sync::Notify::new()));
                        resp.send(Ok(shared::audio_load::InnerAudioLoadToken::new(
                            &state, 1, source_id,
                        )))
                        .unwrap();
                    }
                    Ok(AudioCmd::InnerAudioCompleteLoad {
                        id,
                        load,
                        data,
                        resp,
                    }) => {
                        assert_eq!((id, load.source_id()), (31, 77));
                        assert_eq!(
                            data, b"actual V8 package input",
                            "V8 package mount did not reach the audio service"
                        );
                        resp.send(Ok(shared::protocol::audio_cmd::InnerAudioInfo {
                            duration: 1.,
                            sample_rate: 48000,
                            channels: 1,
                        }))
                        .unwrap();
                        break;
                    }
                    Ok(_) => panic!("unexpected V8 package command"),
                    Err(crossbeam_channel::TryRecvError::Empty) => std::thread::yield_now(),
                    Err(e) => panic!("V8 package audio disconnected: {e}"),
                }
            }
        });
        let mut host = test_host_state_with_audio(tx);
        host.game_paths = Some(paths.clone());
        host.vfs = Some(Arc::new(
            shared::vfs::VirtualFS::try_from_game_paths(&paths).unwrap(),
        ));
        host.mount_table = Some(mounts);
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = executor.enter();
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(host),
            ..Default::default()
        });
        runtime.execute_script("<test:package-audio>",FastString::from_static("globalThis.__done = false; Deno.core.ops.op_inner_audio_load_url(31, 77, 'clip').then(() => { globalThis.__done = true; }, error => { globalThis.__failure = String(error); });")).unwrap();
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .unwrap();
        runtime.execute_script("<test:package-audio-result>",FastString::from_static("if (!globalThis.__done) throw new Error(globalThis.__failure || 'unfinished package load');")).expect("actual V8 package op failed");
        drop(runtime);
        responder.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
