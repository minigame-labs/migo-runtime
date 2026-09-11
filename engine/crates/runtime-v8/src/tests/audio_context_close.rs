//! `AudioContext.close()` must settle the JS wrapper exactly once.
//!
//! Audit `docs/audits/2026-09-09/audio.md:107-115`: `close()` awaited the native
//! close and then touched `PENDING_CONTEXT_RELEASES`, a name that exists nowhere
//! in the tree. Every close therefore threw `ReferenceError` *after* the audio
//! thread had already dropped the context, so `_setState("closed")` and the
//! finalizer unregister never ran: JS kept reporting `running`, and the next
//! `close()` sent a second `CloseContext` for a context that was gone.
//!
//! The second half is the concurrency the state guard cannot cover on its own.
//! `close()` is async, so two calls issued before the first settled both saw
//! `state === "running"` and both sent a command. One in-flight close per
//! context is what makes the guard mean anything.

#[cfg(test)]
mod audio_context_close_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        protocol::audio_cmd::AudioCmd,
        render_command_sender::CommandSender,
    };
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    fn test_host_state(audio_tx: shared::audio_channel::AudioCommandSender) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id: 1,
            app_cache_dir: PathBuf::from("/tmp/cache"),
            app_files_dir: PathBuf::from("/tmp/files"),
            code_dir: None,
            game_paths: None,
            vfs: None,
            mount_table: None,
            render_tx,
            text_measurer: None,
            audio_tx: AudioSender::new(audio_tx, ThreadWakeup::new()),
            host_tx,
            device_services: None,
            raf_rx: None,
            raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    /// Answer `CloseContext` like the audio thread does, and count the commands.
    ///
    /// The count is the whole point: "closed once" is not observable from JS
    /// state alone, because a duplicate command also succeeds against this
    /// fixture. The real audio thread would have nothing left to close.
    fn boot(closes: Arc<AtomicUsize>) -> (JsRuntime, std::thread::JoinHandle<()>) {
        let (tx, rx) = shared::audio_channel::channel();
        let responder = std::thread::spawn(move || {
            // The audio-command receiver only polls; the real audio thread does
            // the same and parks on its own wakeup. A deadline keeps a missing
            // command from hanging the suite.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                match rx.try_recv() {
                    Ok(AudioCmd::CloseContext { resp, .. }) => {
                        closes.fetch_add(1, Ordering::Relaxed);
                        let _ = resp.send(Ok(()));
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
            extensions: crate::main_extensions(test_host_state(tx)),
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
            .execute_script("<test:audio-close>", FastString::from_static(source))
            .expect("script executes");
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .expect("event loop drains without an unhandled rejection");
        runtime
            .execute_script(
                "<test:audio-close-result>",
                FastString::from_static(
                    "if (!globalThis.__done) throw new Error(globalThis.__failure || 'unfinished');",
                ),
            )
            .expect("assertions hold");
    }

    #[test]
    fn close_reaches_the_closed_state_and_sends_one_command() {
        let closes = Arc::new(AtomicUsize::new(0));
        let (mut runtime, responder) = boot(Arc::clone(&closes));
        run(
            &mut runtime,
            r#"
            globalThis.__done = false;
            (async () => {
                try {
                    const ctx = new AudioContext();
                    await ctx.close();
                    if (ctx.state !== 'closed') throw new Error('state is ' + ctx.state);
                    // A second close must be a no-op, not a command for a
                    // context the audio thread has already dropped.
                    await ctx.close();
                    globalThis.__done = true;
                } catch (error) {
                    globalThis.__failure = String(error);
                }
            })();
        "#,
        );
        drop(runtime);
        responder.join().expect("responder");
        assert_eq!(closes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn concurrent_close_calls_share_one_command() {
        let closes = Arc::new(AtomicUsize::new(0));
        let (mut runtime, responder) = boot(Arc::clone(&closes));
        run(
            &mut runtime,
            r#"
            globalThis.__done = false;
            (async () => {
                try {
                    const ctx = new AudioContext();
                    // All three are issued before any of them settles.
                    await Promise.all([ctx.close(), ctx.close(), ctx.close()]);
                    if (ctx.state !== 'closed') throw new Error('state is ' + ctx.state);
                    globalThis.__done = true;
                } catch (error) {
                    globalThis.__failure = String(error);
                }
            })();
        "#,
        );
        drop(runtime);
        responder.join().expect("responder");
        assert_eq!(closes.load(Ordering::Relaxed), 1);
    }
}
