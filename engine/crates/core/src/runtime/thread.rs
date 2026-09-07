//! The embedded execution's thread body: a JavaScript runtime in this process.
//!
//! Everything about *starting* a session thread -- registration, the panic
//! barrier, unregistering on every exit path, the ready handshake -- is in
//! `session_thread.rs`, shared with the external-frame execution. What is left
//! here is the part that only makes sense with an engine on this side of the
//! boundary: constructing a `Host`, entering its Tokio runtime, and running its
//! event loop until it exits.

use std::sync::Arc;

use tracing::{debug, error, info};

use shared::{
    config::InitOptions,
    error::{EngineError, EngineResult, ErrorCode},
    protocol::host_cmd::HostCommand,
    surface::{PublicSurfaceGeneration, SurfaceRef},
};

use crate::runtime::host::Host;
use crate::runtime::session_thread::{
    HostThread, SessionThreadContext, SpawnedSurfaceHost, create_basic_runtime,
    create_runtime_before_ready, spawn_session_thread,
};
use crate::services::PlatformServices;

/// Start a Host, with or without the window Surface it will render into.
///
/// `surface` is `None` for a warm start: the host and render threads come up
/// and do every part of GPU bring-up that does not name a window -- EGL display
/// and config, the pbuffer resource context, the 709-entry GLES dispatch table,
/// capability detection, Skia, the system font scan -- and then park. The
/// Surface arrives later through the ordinary `UpdateSurface` path, which
/// installs it exactly as an initial Surface would have been installed.
///
/// The point is not to make bring-up cheaper but to move it off a critical
/// path. An Android Activity spends ~150 ms between `onCreate` and
/// `surfaceCreated` -- more when it rotates for a landscape game -- and until
/// this existed the engine could not start until the far end of it.
pub fn spawn_host_thread(
    surface: Option<SurfaceRef>,
    graphics_platform: graphics::egl_platform::GraphicsPlatform,
    platform: Arc<dyn PlatformServices>,
    opt: InitOptions,
) -> EngineResult<HostThread> {
    spawn_session_thread(
        surface,
        graphics_platform,
        platform,
        opt,
        None,
        run_embedded_session,
    )
    .map(|started| started.host)
}

/// Start a Host while preserving the embedding host's generation and a
/// resource lease for its unique public attachment handle.
pub fn spawn_host_thread_tracked(
    surface: SurfaceRef,
    public_generation: PublicSurfaceGeneration,
    graphics_platform: graphics::egl_platform::GraphicsPlatform,
    platform: Arc<dyn PlatformServices>,
    opt: InitOptions,
) -> EngineResult<SpawnedSurfaceHost> {
    spawn_session_thread(
        Some(surface),
        graphics_platform,
        platform,
        opt,
        Some(public_generation),
        run_embedded_session,
    )
    .map(|started| SpawnedSurfaceHost {
        host: started.host,
        // Infallible by construction: this entry point always hands the inner
        // one a Surface, and the inner one mints a resource lease for exactly
        // the Surfaces it is given.
        resource: started
            .resource
            .expect("tracked spawn passes a Surface, so a resource lease exists"),
        ingress: started.ingress,
    })
}

/// The embedded session, start to finish, on its own thread.
fn run_embedded_session(ctx: SessionThreadContext) {
    let SessionThreadContext {
        id,
        host_tx,
        critical_host_tx,
        mut host_rx,
        initial_surface,
        graphics_platform,
        platform,
        platform_for_error,
        opt,
        surface_control,
        vsync_rx,
        restart_boundary,
        ready_tx,
    } = ctx;
    let host = match Host::new(
        id,
        host_tx,
        critical_host_tx,
        initial_surface,
        graphics_platform,
        platform,
        opt,
        Arc::clone(&surface_control),
        vsync_rx,
        restart_boundary,
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("[Host {}] failed to create host: {}", id, e);
            // Notify Java of the initialization failure
            platform_for_error.notify_error(
                id,
                e.code.as_u16(),
                &e.msg,
                e.detail.as_deref().unwrap_or(""),
            );
            // ready_tx will be dropped without sending, triggering error in caller
            return;
        }
    };

    let runtime = match create_runtime_before_ready(ready_tx, create_basic_runtime) {
        Ok(rt) => rt,
        Err(e) => {
            error!("[Host {}] failed to enter tokio runtime: {}", id, e);
            platform_for_error.notify_error(
                id,
                e.code.as_u16(),
                &e.msg,
                e.detail.as_deref().unwrap_or(""),
            );
            return;
        }
    };

    // Keep a reference to platform_for_error for use in the event loop
    let platform_ref = &platform_for_error;

    // Owned handle to the shutdown flag so the `async move` loop
    // below can poll it (the thread closure only lends it to us).
    let surface_control = Arc::clone(&surface_control);
    runtime.block_on(async move {
                    let mut host = host;

                    // Coalescing wake signals replace the deleted 3-second
                    // heartbeat. The render event channel calls `notify_one()` on
                    // every successfully enqueued event; the lazy-audio start
                    // signal fires on the first pre-start command. Both are Tokio
                    // `Notify`s that latch one permit even with no waiter
                    // registered, so a signal emitted while the host is inside a
                    // `run_event_loop` poll is delivered on the next select
                    // iteration rather than lost. Cloned once, outside the loop.
                    let render_notify = host.render_notify();
                    let audio_signal = host.audio.start_signal();

                    // Track whether to notify Java on exit.
                    // Error paths set this to false (they already call notify_error).
                    // Normal Shutdown keeps it true so Java learns the host exited
                    // (needed when JS calls exitMiniProgram — Java didn't initiate
                    // the shutdown and must finish the Activity).
                    let mut notify_exit = true;

                    'outer: loop {
                        // Shutdown check, decoupled from the command queue so a
                        // full queue can't swallow the request (see shutdown_host).
                        // Stops the loop the next time control returns here; a
                        // runaway JS section that never yields is handled by the
                        // deadline watchdog, not this flag.
                        if surface_control.is_shutting_down() {
                            break 'outer;
                        }

                        tokio::select! {
                            biased;

                            host_event = host.js.pump_event_loop() => {
                                if let Err(e) = host_event {
                                    // Check if this was a watchdog/OOM termination
                                    #[cfg(feature = "v8-limits")]
                                    {
                                        let classified = classify_termination_error(&host, &e);
                                        if let Some(engine_err) = classified {
                                            // Report fatal error via DebugStats for Java layer polling
                                            if let Some(stats) = shared::stats::get_stats(id) {
                                                stats.fatal_error_code.store(
                                                    engine_err.code.as_u16() as u32,
                                                    std::sync::atomic::Ordering::SeqCst,
                                                );
                                            }
                                            error!(
                                                "[Host {}] V8 terminated: code={:?}, msg={}, detail={:?}",
                                                id, engine_err.code, engine_err.msg, engine_err.detail
                                            );
                                            // Notify Java via JNI callback
                                            platform_ref.notify_error(
                                                id,
                                                engine_err.code.as_u16(),
                                                &engine_err.msg,
                                                engine_err.detail.as_deref().unwrap_or(""),
                                            );
                                            notify_exit = false;
                                            break 'outer;
                                        }
                                    }

                                    error!(
                                        "[Host {}] event loop error: code={:?}, msg={}, detail={:?}",
                                        id, e.code, e.msg, e.detail
                                    );
                                    // Notify Java about unclassified JS errors too
                                    platform_ref.notify_error(
                                        id,
                                        e.code.as_u16(),
                                        &e.msg,
                                        e.detail.as_deref().unwrap_or(""),
                                    );
                                    notify_exit = false;
                                    break 'outer;
                                }
                                // Event loop returned Ok — no pending ops/timers/promises.
                                // Ordinary once the game is running, because the pending
                                // op_await_next_frame keeps the loop alive; ordinary *before* it
                                // is running too, which is what this used to claim was abnormal.
                                // It fires three times on every single launch, in the window
                                // between the host thread coming up and the game registering its
                                // first rAF -- a warning on the guaranteed path is how a log
                                // teaches its reader to skip warnings.
                                //
                                // Park on the command channel + render/audio signals (no polling
                                // timer) so a ContextLost or a first audio command during an idle
                                // stretch is still handled promptly.
                                debug!("[Host {}] event loop idle, parking on command/render/audio signals", id);
                                // Every arm leaves this park, because every arm can
                                // create work the event loop is the only thing that
                                // drives. A render drain dispatches
                                // `webglcontext{lost,restored}` into JS, and a handler
                                // that resolves a promise or asks for a frame leaves a
                                // pending op behind; staying parked here would hold it
                                // until some *unrelated* host command arrived, and vsync
                                // does not arrive as one. That stranded a game after a
                                // GPU context loss until the player touched the screen.
                                tokio::select! {
                                    biased;
                                    maybe_msg = host_rx.recv() => {
                                        match maybe_msg {
                                            Some(HostCommand::Shutdown) => break 'outer,
                                            Some(msg) => host.handle_command(msg).await,
                                            None => break 'outer,
                                        }
                                    }
                                    _ = render_notify.notified() => {
                                        host.drain_render_events();
                                    }
                                    _ = audio_signal.notified() => {
                                        if let Err(e) = host.audio.check_and_start() {
                                            error!("[Host {}] failed to start audio thread: {}", host.id, e);
                                        }
                                    }
                                }
                            }

                            maybe_msg = host_rx.recv() => {
                                match maybe_msg {
                                    Some(HostCommand::Shutdown) => break 'outer,
                                    Some(msg) => host.handle_command(msg).await,
                                    None => break 'outer,
                                }
                            }

                            _ = render_notify.notified() => {
                                // A render-thread event was enqueued: drain +
                                // reconcile (notably ContextLost/Recovered) now,
                                // instead of waiting for the next command.
                                host.drain_render_events();
                            }

                            _ = audio_signal.notified() => {
                                // First pre-start audio command: lazily spawn the
                                // AudioThread. The signal disables itself
                                // (mark_started) once the thread is installed, so
                                // this branch stops firing afterwards.
                                if let Err(e) = host.audio.check_and_start() {
                                    error!("[Host {}] failed to start audio thread: {}", host.id, e);
                                    // Non-fatal: game continues without audio.
                                }
                            }
                        }
                    }

                    // Notify Java that the host is exiting.  When JS calls
                    // exitMiniProgram, this is the only way Java learns about it
                    // so it can finish() the Activity.  When Java itself called
                    // shutdown_host(), it can simply ignore this notification.
                    if notify_exit {
                        platform_ref.notify_exit(id);
                    }
                });

    info!("[Host {}] host thread exited", id);
}

/// Classify a V8 "execution terminated" error into a more specific error code
/// by checking the watchdog state and OOM flag.
///
/// Returns `Some(EngineError)` if this is a recognized termination, `None` otherwise.
#[cfg(feature = "v8-limits")]
fn classify_termination_error(host: &Host, original: &EngineError) -> Option<EngineError> {
    // Check if this is a termination error (V8 sends "execution terminated")
    let is_termination = original.msg.contains("execution terminated")
        || original
            .detail
            .as_ref()
            .is_some_and(|d| d.contains("execution terminated"));

    if !is_termination {
        return None;
    }

    // Check OOM first (the near-heap-limit callback sets this flag)
    if host.js.was_oom_terminated() {
        return Some(
            EngineError::new(ErrorCode::OutOfMemory)
                .with_msg("V8 heap limit exceeded")
                .with_detail("near_heap_limit_callback triggered terminate_execution"),
        );
    }

    // Then the process deadline watchdog. OOM stays higher priority than a
    // timeout; the timeout is sticky per isolate.
    if host.js.watchdog_timed_out() {
        return Some(
            EngineError::new(ErrorCode::JsExecutionTimeout)
                .with_msg("JS execution exceeded watchdog timeout")
                .with_detail("watchdog detected unresponsive JS execution and terminated isolate"),
        );
    }

    // Generic termination (unknown source)
    Some(
        EngineError::new(ErrorCode::JsException)
            .with_msg("execution terminated")
            .with_detail("V8 isolate was terminated by unknown source"),
    )
}

#[cfg(test)]
mod tests {
    use shared::error::{EngineError, ErrorCode};

    use crate::runtime::session_thread::{create_basic_runtime, create_runtime_before_ready};

    #[test]
    fn runtime_creation_failure_is_not_published_as_ready() {
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let original =
            EngineError::new(ErrorCode::Internal).with_msg("injected runtime construction failure");

        let error = create_runtime_before_ready(ready_tx, || Err(original))
            .expect_err("runtime construction must fail");

        assert_eq!(error.msg, "injected runtime construction failure");
        assert!(
            ready_rx.recv().is_err(),
            "startup readiness was published before runtime construction"
        );
    }

    #[test]
    fn nobody_waiting_for_readiness_is_not_a_startup_failure() {
        let (ready_tx, ready_rx) = crossbeam_channel::bounded::<()>(1);
        drop(ready_rx);

        let runtime = create_runtime_before_ready(ready_tx, create_basic_runtime)
            .expect("a dropped readiness receiver must not fail Host startup");

        drop(runtime);
    }
}
