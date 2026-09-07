//! Starting and stopping a session thread, with no opinion about what runs on it.
//!
//! Registration, the panic barrier, the unregister-on-every-exit-path rule and
//! the ready handshake are the same for every execution mode, and they are the
//! parts where being almost right is invisible until a device is in someone's
//! hand: a session that fails to unregister leaves a sender the JNI layer can
//! still find, a panic that escapes the barrier takes the process instead of the
//! session, and a handshake that reports ready before construction finished
//! turns a construction failure into a hang.
//!
//! So they live here once, and each mode supplies only its thread body. The
//! alternative -- an `external.rs` with its own copy of the same sixty lines --
//! is the shape the frame ingress's own documentation warns about: a second
//! place for the rules to be almost right.

use std::{
    panic,
    sync::Arc,
    thread::{self, JoinHandle},
};

use tokio::runtime::{Builder, Runtime};
use tracing::error;

use shared::{
    config::InitOptions,
    error::{EngineError, EngineResult, ErrorCode},
    host_channel::{CriticalHostCommandSender, HostCommandReceiver},
    op_state::HostTx,
    surface::{
        PublicSurfaceGeneration, SurfaceControl, SurfaceLease, SurfaceRef, SurfaceResourceLease,
    },
};

// Tokio still backs `tokio::fs` uploads/local-audio reads and resolver
// fallbacks. Keep a small lazy compatibility pool; bounded engine I/O uses the
// process-wide Migo-IO executor instead.
const HOST_BLOCKING_FALLBACK_THREADS: usize = 4;

use crate::runtime::{HostId, registry, registry::HostIngress, restart_boundary::RestartBoundary};
use crate::services::PlatformServices;

/// Result of starting a Host whose initial Surface belongs to a public
/// embedding attachment.
pub struct SpawnedSurfaceHost {
    pub host: HostThread,
    pub resource: SurfaceResourceLease,
    /// The session's own direct data-plane handles. Handed over by the spawn rather
    /// than looked up afterwards, so a caller cannot race this session's teardown
    /// for its own ingress.
    pub ingress: HostIngress,
}

/// Owning handle for one Migo Host thread.
///
/// Command producers may copy [`HostId`], but a native host must retain this
/// value until it can request shutdown and join. Dropping it is a synchronous
/// fail-safe, not a detach operation.
#[must_use = "a spawned Host must be shut down and joined"]
#[derive(Debug)]
pub struct HostThread {
    host_id: HostId,
    join: Option<JoinHandle<()>>,
}

impl HostThread {
    fn new(host_id: HostId, join: JoinHandle<()>) -> Self {
        Self {
            host_id,
            join: Some(join),
        }
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn from_join_handle_for_test(host_id: HostId, join: JoinHandle<()>) -> Self {
        Self::new(host_id, join)
    }

    #[inline]
    pub const fn id(&self) -> HostId {
        self.host_id
    }

    #[inline]
    pub fn is_current_thread(&self) -> bool {
        self.join
            .as_ref()
            .is_some_and(|join| join.thread().id() == thread::current().id())
    }

    pub fn request_shutdown(&self) -> Result<(), String> {
        registry::shutdown_host(self.host_id)
    }

    pub fn join(&mut self) -> EngineResult<()> {
        self.join_inner()
    }

    pub fn shutdown_and_join(&mut self) -> EngineResult<()> {
        if self.join.is_none() {
            return Ok(());
        }
        self.request_shutdown().map_err(|error| {
            EngineError::new(ErrorCode::Internal)
                .with_msg("failed to request Host shutdown")
                .with_detail(format!("Host {}: {error}", self.host_id))
        })?;
        self.join_inner()
    }

    fn join_inner(&mut self) -> EngineResult<()> {
        if self.join.is_none() {
            return Ok(());
        }
        if self.is_current_thread() {
            return Err(EngineError::new(ErrorCode::Internal)
                .with_msg("Host thread cannot join itself")
                .with_detail(format!("Host {}", self.host_id)));
        }

        let join = self.join.take().expect("join presence checked above");
        join.join().map_err(|payload| {
            EngineError::new(ErrorCode::Internal)
                .with_msg("Host thread panicked outside its panic barrier")
                .with_detail(format!(
                    "Host {}: {}",
                    self.host_id,
                    panic_payload_message(payload.as_ref())
                ))
        })
    }
}

impl Drop for HostThread {
    fn drop(&mut self) {
        if self.join.is_none() {
            return;
        }
        if self.is_current_thread() {
            error!(
                "[Host {}] owning HostThread was dropped on its own thread",
                self.host_id
            );
            std::process::abort();
        }

        if let Err(error) = self.request_shutdown() {
            error!(
                "[Host {}] shutdown request from HostThread::drop failed: {}",
                self.host_id, error
            );
        }
        let join = self.join.take().expect("join presence checked above");
        if let Err(payload) = join.join() {
            error!(
                "[Host {}] join from HostThread::drop observed panic: {}",
                self.host_id,
                panic_payload_message(payload.as_ref())
            );
        }
    }
}

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

/// Everything a session's thread body is handed. Built by
/// [`spawn_session_thread`], consumed by whichever execution mode the product
/// compiled in.
pub(crate) struct SessionThreadContext {
    pub(crate) id: HostId,
    pub(crate) host_tx: HostTx,
    pub(crate) critical_host_tx: CriticalHostCommandSender,
    pub(crate) host_rx: HostCommandReceiver,
    pub(crate) initial_surface: Option<SurfaceLease>,
    pub(crate) graphics_platform: graphics::egl_platform::GraphicsPlatform,
    pub(crate) platform: Arc<dyn PlatformServices>,
    /// A second handle for the failure paths. `platform` is moved into the
    /// session; error reporting has to outlive that move, including from inside
    /// the panic barrier where the session no longer exists.
    pub(crate) platform_for_error: Arc<dyn PlatformServices>,
    pub(crate) opt: InitOptions,
    pub(crate) surface_control: Arc<SurfaceControl>,
    /// The externally paced frame clock's receiver, or `None` where the engine
    /// paces itself. Created with its sender before this thread exists, because the
    /// sender belongs to the ingress the caller is handed and the caller is handed
    /// it before the thread runs.
    pub(crate) vsync_rx: Option<crossbeam_channel::Receiver<f64>>,
    pub(crate) restart_boundary: RestartBoundary,
    /// Sent once construction has succeeded far enough that a caller waiting on
    /// a cold start can stop waiting. Dropped without sending on failure, which
    /// is what turns a failed construction into a synchronous error rather than
    /// a hang.
    pub(crate) ready_tx: crossbeam_channel::Sender<()>,
}

/// Report a startup failure that no session thread can report, because there is
/// no session thread yet.
///
/// Both of [`spawn_session_thread`]'s pre-thread exits come through here, which
/// is what makes that function's reporting contract total: a caller never has to
/// guess whether a returned `Err` was announced.
fn report_unspawned_failure(platform: &Arc<dyn PlatformServices>, id: HostId, error: &EngineError) {
    platform.notify_error(
        id,
        error.code.as_u16(),
        &error.msg,
        error.detail.as_deref().unwrap_or(""),
    );
}

/// Start a session thread and run `body` on it.
///
/// `body` owns the session for the thread's whole life. It is called inside the
/// panic barrier, so a panic in any execution mode is reported and unregistered
/// the same way rather than each mode remembering to.
///
/// **Every `Err` returned here has already been reported through
/// `notify_error`, exactly once.** Each failure inside the thread announces its
/// own reason before dropping `ready_tx` -- and dropping `ready_tx` unsent is
/// precisely what the caller's handshake failure below observes -- while the two
/// failures that happen before a thread exists report through
/// [`report_unspawned_failure`].
///
/// A caller that adds its own report for a returned `Err` therefore delivers two
/// `on_error` callbacks for one failure, and the host has no way to collapse
/// them: the notifier posts every event independently. Worse, the caller cannot
/// tell the two apart without matching on the error message, so the reason it
/// would add is the *generic* one while the thread's own is the specific one.
pub(crate) fn spawn_session_thread<Body>(
    surface: Option<SurfaceRef>,
    graphics_platform: graphics::egl_platform::GraphicsPlatform,
    platform: Arc<dyn PlatformServices>,
    opt: InitOptions,
    public_generation: Option<PublicSurfaceGeneration>,
    body: Body,
) -> EngineResult<StartedHost>
where
    Body: FnOnce(SessionThreadContext) + Send + 'static,
{
    let id = registry::alloc_host_id();

    // Issue generation 1 before publishing the Host. A fresh gate cannot be
    // exhausted, but keep the failure path explicit so generation wrap always
    // fails closed rather than silently creating an untracked Surface.
    let surface_control = Arc::new(SurfaceControl::new());
    // Deliberately not minted for a warm start. `attach_or_update` mints the
    // *next* generation when the gate is detached and is idempotent when it is
    // live, so a session that starts without a Surface has its first one issued
    // generation 1 by the `UpdateSurface` that delivers it -- the same number,
    // issued at the same point in the Surface's life, as if it had been passed
    // here. Minting one now for a Surface that does not exist would spend a
    // generation on nothing and put every later one off by one.
    let initial_surface = match surface {
        Some(surface) => {
            let initial_token = surface_control.attach_or_update().map_err(|_| {
                let error = EngineError::new(ErrorCode::InvalidOperation)
                    .with_msg("initial Surface generation exhausted");
                report_unspawned_failure(&platform, id, &error);
                error
            })?;
            Some(match public_generation {
                Some(public_generation) => {
                    SurfaceLease::new_tracked(surface, initial_token, public_generation)
                }
                None => SurfaceLease::new(surface, initial_token),
            })
        }
        None => None,
    };
    let initial_resource = initial_surface.as_ref().map(|lease| lease.resource_lease());
    // Having a Surface and being on the caller's critical path are the same
    // question asked twice: a caller that already holds the Surface is
    // necessarily starting the engine at the point it needs to render.
    let wait_for_ready = initial_surface.is_some();

    // Bound all normal/game-controlled traffic while allowing the four trusted
    // lifecycle/surface callbacks to share the same FIFO without consuming
    // that quota. This preserves the old 512 pending-normal-command limit.
    let (host_tx, critical_host_tx, host_rx) = shared::host_channel::channel_with_reserve(
        registry::HOST_NORMAL_COMMAND_CAPACITY,
        registry::HOST_RELIABLE_INPUT_RESERVE,
    );
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<()>(1);

    // Authoritative shutdown signal, independent of the normal-command budget:
    // `shutdown_host` sets this even when the budget is full (where its normal
    // Shutdown nudge is dropped) and the host loop polls it every iteration.
    // Constructed before registration so no sender can ever be reachable
    // without a generation to stamp with. The boundary itself moves into the
    // Host thread; the registry keeps only a reader.
    let restart_boundary = crate::runtime::restart_boundary::RestartBoundary::new();
    let log_level = opt.log_level();
    // Only platforms that publish external timestamps get a frame clock at all;
    // handing a never-fed receiver to the render thread would make it wait for
    // timestamps nobody sends. `uses_external_vsync` is a property of the platform
    // services, which exist here, so the question is answerable before the thread
    // is -- and it has to be, because the sender travels in the ingress this
    // function returns.
    let (vsync_tx, vsync_rx) = if platform.uses_external_vsync() {
        let (tx, rx) = crossbeam_channel::bounded::<f64>(2);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let ingress = registry::register_sender(
        id,
        host_tx.clone(),
        critical_host_tx.clone(),
        Arc::clone(&surface_control),
        restart_boundary.reader(),
        log_level,
        vsync_tx,
    );

    // Clone the platform Arc so we can use it in the catch_unwind path
    // to notify Java about errors from any context (host loop, panic, etc.).
    let platform_for_error = platform.clone();
    // And a third handle, for the one failure that happens after this function
    // has committed to a thread and before that thread exists. The closure below
    // moves the other two, so this has to be taken now or the spawn-failure
    // branch has nothing left to report through.
    let platform_if_never_spawned = platform.clone();

    let spawn_result = thread::Builder::new()
        .name(format!("Migo-Main-{}", id))
        .spawn(move || {
            // Bound before any of this session's work, so its own records are
            // filtered by its own level rather than by the most verbose level some
            // other live session asked for. The thread ends with the session, so
            // there is nothing to unbind.
            shared::log_level::bind_thread_level(log_level);
            let run = || {
                body(SessionThreadContext {
                    id,
                    host_tx,
                    critical_host_tx,
                    host_rx,
                    initial_surface,
                    graphics_platform,
                    platform,
                    platform_for_error: Arc::clone(&platform_for_error),
                    opt,
                    surface_control,
                    vsync_rx,
                    restart_boundary,
                    ready_tx,
                });
            };

            let r = panic::catch_unwind(panic::AssertUnwindSafe(run));
            if let Err(panic_info) = r {
                let panic_msg = panic_info
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic_info.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "Unknown panic".to_string());

                error!("[Host {}] panicked: {}", id, panic_msg);

                // Report panic to DebugStats for Java layer polling
                if let Some(stats) = shared::stats::get_stats(id) {
                    stats.fatal_error_code.store(
                        ErrorCode::HostPanic.as_u16() as u32,
                        std::sync::atomic::Ordering::SeqCst,
                    );
                }

                // Notify Java via JNI callback.
                //
                // Thread attach/detach: `platform.notify_error` uses `with_env`
                // which calls `jvm.attach_current_thread()` if the host thread
                // isn't already attached.  On Android, AttachCurrentThread is
                // safe to call from any native thread and the AttachGuard will
                // auto-detach when the thread-local drops.
                platform_for_error.notify_error(
                    id,
                    ErrorCode::HostPanic.as_u16(),
                    "host thread panic",
                    &panic_msg,
                );
            }

            // NOTE: Shutdown-unregister race window
            //
            // Between the host thread exiting and this unregister call, JNI
            // callbacks (onVsync, touch events, etc.) may still call
            // `send_command_to_host(id, ...)`.  Those calls will get a
            // "Cannot find host_id=N sender" error from the registry.
            //
            // This is benign: the host is already shutting down, so dropping
            // late-arriving commands is the correct behavior.  The JNI callers
            // already ignore send failures (they use `let _ = send_command_to_host(...)`).
            registry::unregister_sender(id);
        });

    let join = match spawn_result {
        Ok(join) => join,
        Err(error) => {
            error!("[Host {}] failed to spawn thread: {}", id, error);
            registry::unregister_sender(id);
            let startup_error = EngineError::new(ErrorCode::Internal)
                .with_msg("failed to spawn host thread")
                .with_detail(error.to_string());
            report_unspawned_failure(&platform_if_never_spawned, id, &startup_error);
            return Err(startup_error);
        }
    };
    let host = HostThread::new(id, join);

    // A warm start does not wait for the Host to finish constructing.
    //
    // The handshake below exists to turn a failed construction into a
    // synchronous error for the caller, and for a cold start that is worth what
    // it costs: the caller is on the path to first frame anyway. A warm start's
    // entire purpose is to be off that path, and waiting here would put ~20 ms
    // of V8 isolate construction back on the caller's thread -- which on
    // Android is the main thread, mid-`onCreate`, with layout still to do.
    // Measured on a Mate 30 Pro, waiting cost ~60 ms of `Displayed`: more than
    // the warm start saved.
    //
    // Nothing is lost but the *timing* of the report. The id and its command
    // senders are registered before the thread is spawned, so commands sent
    // meanwhile queue rather than fail; and the thread unregisters itself and
    // calls `notify_error` on every exit path, construction failure included,
    // so a Host that dies on its way up still tells the embedder so.
    if !wait_for_ready {
        return Ok(StartedHost {
            host,
            resource: initial_resource,
            ingress,
        });
    }

    if ready_rx.recv().is_err() {
        error!("[Host {}] failed to start (init panic / early exit)", id);
        registry::unregister_sender(id);
        // Deliberately not reported here. This branch does not observe a failure;
        // it observes `ready_tx` being dropped unsent, which every failing path
        // in the thread body does *after* announcing its own specific reason
        // (and a panic after the barrier has announced it). Reporting again
        // would replace nothing and add a second, vaguer callback.
        let error = EngineError::new(ErrorCode::Internal)
            .with_msg("host thread failed to start")
            .with_detail("init panic / early exit".to_string());
        return Err(join_failed_start(host, error));
    }

    Ok(StartedHost {
        host,
        resource: initial_resource,
        ingress,
    })
}

/// What `spawn_host_thread_inner` returns: a Host, and a resource lease for the
/// Surface it was given, if it was given one.
pub(crate) struct StartedHost {
    pub(crate) host: HostThread,
    pub(crate) resource: Option<SurfaceResourceLease>,
    /// The session's own direct data-plane handles, from the registration that
    /// published them. Handed over rather than looked up: the caller would
    /// otherwise be racing this session's teardown for its own ingress.
    pub(crate) ingress: HostIngress,
}
fn join_failed_start(mut host: HostThread, startup_error: EngineError) -> EngineError {
    match host.join() {
        Ok(()) => startup_error,
        Err(join_error) => EngineError::new(ErrorCode::Internal)
            .with_msg("Host startup failed and its thread could not be joined")
            .with_detail(format!("startup={startup_error}; join={join_error}")),
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
    };

    use shared::error::{EngineError, ErrorCode};

    use super::{HostThread, join_failed_start};

    struct DropSentinel(Arc<AtomicBool>);

    impl Drop for DropSentinel {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn join_waits_for_named_host_and_observes_sentinel_drop() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let sentinel = DropSentinel(Arc::clone(&dropped));
        let join = thread::Builder::new()
            .name("Migo-Main-test-join".to_owned())
            .spawn(move || {
                let _sentinel = sentinel;
                started_tx
                    .send(thread::current().name().map(str::to_owned))
                    .expect("publish thread name");
                release_rx.recv().expect("release test host");
            })
            .expect("spawn test host");
        let mut host = HostThread::new(41, join);
        let (joined_tx, joined_rx) = mpsc::channel();

        let joiner = thread::spawn(move || {
            joined_tx.send(host.join()).expect("publish join result");
        });

        assert_eq!(
            started_rx.recv().expect("test host started").as_deref(),
            Some("Migo-Main-test-join")
        );
        assert!(
            joined_rx.try_recv().is_err(),
            "join returned while the named Host was still blocked"
        );
        assert!(!dropped.load(Ordering::Acquire));

        release_tx.send(()).expect("release test host");
        joined_rx
            .recv()
            .expect("join result")
            .expect("Host join succeeds");
        joiner.join().expect("join caller");
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn failed_start_joins_the_spawned_thread() {
        let dropped = Arc::new(AtomicBool::new(false));
        let sentinel = DropSentinel(Arc::clone(&dropped));
        let join = thread::Builder::new()
            .name("Migo-Main-test-failed-start".to_owned())
            .spawn(move || drop(sentinel))
            .expect("spawn failed-start test host");
        let original =
            EngineError::new(ErrorCode::Internal).with_msg("host thread failed to start");

        let error = join_failed_start(HostThread::new(42, join), original);

        assert_eq!(error.msg, "host thread failed to start");
        assert!(dropped.load(Ordering::Acquire));
    }

    /// A warm start returns before the Host is ready and drops the receiver as
    /// it goes. That must leave the Host alive: the first cut of the warm start
    /// treated the dropped receiver as a startup failure, so every warm-started
    /// session built its runtime, failed to announce it, and exited -- the game
    /// never ran, and the only symptom was a missing `Fully drawn`.

    #[test]
    fn host_thread_id_is_stable() {
        let join = thread::Builder::new()
            .name("Migo-Main-test-id".to_owned())
            .spawn(|| {})
            .expect("spawn ID test host");
        let mut host = HostThread::new(73, join);

        assert_eq!(host.id(), 73);
        host.join().expect("Host join succeeds");
    }

    #[test]
    fn self_join_rejection_preserves_owner_for_another_thread() {
        let (owner_tx, owner_rx) = mpsc::channel();
        let (error_tx, error_rx) = mpsc::channel();
        let (returned_tx, returned_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("Migo-Main-test-self-join".to_owned())
            .spawn(move || {
                let mut host: HostThread = owner_rx.recv().expect("receive own Host owner");
                let error = host.join().expect_err("self-join must be rejected");
                error_tx.send(error).expect("publish self-join error");
                returned_tx.send(host).expect("return Host owner");
            })
            .expect("spawn self-join test host");
        owner_tx
            .send(HostThread::new(74, join))
            .expect("transfer owner to its Host");

        let error = error_rx.recv().expect("self-join result");
        assert_eq!(error.code, ErrorCode::Internal);
        let mut host = returned_rx.recv().expect("returned Host owner");
        host.join().expect("another thread joins the Host");
    }
}

/// Build the tokio runtime, and only then announce that startup succeeded.
///
/// The ordering is the point: a runtime that failed to build must never be
/// reported ready. Whether anyone is *listening* is a separate question, and
/// not this function's business -- a warm start deliberately stops listening,
/// because the whole reason it exists is to get its caller's thread back. A
/// dropped receiver used to abort the Host here, which turned every warm start
/// into a Host that came up and immediately died.
pub(crate) fn create_runtime_before_ready(
    ready_tx: crossbeam_channel::Sender<()>,
    create_runtime: impl FnOnce() -> EngineResult<Runtime>,
) -> EngineResult<Runtime> {
    let runtime = create_runtime()?;
    let _ = ready_tx.send(());
    Ok(runtime)
}

pub(crate) fn create_basic_runtime() -> EngineResult<Runtime> {
    let (event_interval, global_queue_interval, max_io_events_per_tick) = (61, 31, 1024);

    Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .event_interval(event_interval)
        .global_queue_interval(global_queue_interval)
        .max_io_events_per_tick(max_io_events_per_tick)
        // Bounded engine I/O runs on the process Migo-IO executor. This lazy
        // fallback remains for tokio::fs and resolver internals only.
        .max_blocking_threads(HOST_BLOCKING_FALLBACK_THREADS)
        .build()
        .map_err(|e| {
            EngineError::from_detail(
                ErrorCode::Internal,
                format!("failed to create tokio runtime: {}", e),
            )
        })
}
