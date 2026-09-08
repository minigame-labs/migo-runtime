//! A session whose JavaScript runs somewhere else.
//!
//! On iOS the content's JavaScript and WebAssembly run inside WebKit's
//! WebContent process, because that is the only process Apple grants a JIT to.
//! What arrives here is a bounded, validated packet of drawing work per frame.
//! So this session owns everything a session normally owns -- the render
//! thread, the surface, the frame clock, lifecycle, generations -- and owns no
//! script runtime at all.
//!
//! That absence is the product claim `MigoApplePerformancePlus` rests on, and
//! it is structural rather than intended: this module compiles only under
//! `external-frames`, which `lib.rs` refuses to combine with `embedded-v8`, and
//! `scripts/test-apple-performance-rust-closure.sh` measures the resolved
//! dependency graph to prove no engine is reachable from here.
//!
//! Accepted packets are decoded into owned render operations. Their admission
//! credits follow those operations until execution or discard; frame-clock ticks
//! only request production and do not acknowledge completion. Surface and
//! resource generations are updated from the renderer's lifecycle events.

use std::collections::VecDeque;
use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicU64, Ordering},
};

use parking_lot::Mutex;
use tracing::{debug, error, info, warn};

use shared::{
    config::InitOptions,
    error::{EngineResult, ErrorCode},
    protocol::host_cmd::HostCommand,
    render_event::RenderEvent,
    surface::SurfaceRef,
};

#[cfg(test)]
use frame_wire::IngressDecision;
use frame_wire::{FrameIngress, IngressOutcome, PooledFrame, stream};

use crate::runtime::session_thread::{
    HostThread, SessionThreadContext, StartedHost, create_basic_runtime,
    create_runtime_before_ready, spawn_session_thread,
};
use crate::runtime::shell::SessionShell;
use crate::services::PlatformServices;

/// A running external-frame session.
///
/// The ingress is shared rather than owned by the thread because the transport
/// that will feed it runs on whichever thread the host's networking uses, and a
/// frame has to be validated and credited before it is queued. The lock is
/// taken once per frame through queue submission, so concurrent producers cannot
/// dispatch accepted sequences out of order.
#[must_use = "a spawned session must be shut down and joined"]
pub struct ExternalFrameSession {
    host: HostThread,
    submit: SubmitPath,
    clock: Arc<ExternalFrameClock>,
}

/// A started external session and, when it was given a Surface, the lease for
/// the embedding host's public attachment handle.
///
/// The lease is handed back rather than kept inside the session for the same
/// reason `SpawnedSurfaceHost` hands one back: the C boundary owns the public
/// handle's lifetime, and a lease held somewhere the boundary cannot see is a
/// generation the boundary cannot retire.
#[must_use = "a spawned session must be shut down and joined"]
pub struct SpawnedExternalSession {
    pub session: ExternalFrameSession,
    pub resource: Option<shared::surface::SurfaceResourceLease>,
    /// The session's own direct data-plane handles. Handed over by the spawn rather
    /// than looked up afterwards, so a caller cannot race this session's teardown
    /// for its own ingress -- which on the iOS simulator it lost about two runs in
    /// three whenever the renderer failed to start.
    pub ingress: crate::runtime::registry::HostIngress,
}

/// Why a frame that the ingress accepted still did not reach the renderer.
///
/// Numbered above the wire and ingress ranges so one telemetry field carries
/// any of the three without ambiguity.
pub const EXTERNAL_ERROR_RENDERER_NOT_READY: u32 = 2001;
pub const EXTERNAL_ERROR_NO_COMMAND_STREAM: u32 = 2002;
pub const EXTERNAL_ERROR_BAD_COMMAND_STREAM: u32 = 2003;
pub const EXTERNAL_ERROR_RENDERER_UNREACHABLE: u32 = 2004;

/// Hard per-frame decoded-storage ceiling. Together with the two-credit window
/// this bounds queued command storage independently of the 4 MiB wire ceiling.
/// It is a safety limit, not a measurement of total process or GPU memory.
const MAX_DECODED_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// The most WebGL errors kept per canvas before the oldest is dropped.
///
/// WebGL's own queue is unbounded in the specification and bounded in every
/// implementation, for the obvious reason: a game in a bad state can generate
/// one per call. Sixteen is enough for `getError` to drain a burst and small
/// enough that a runaway producer cannot spend memory here.
const MAX_PENDING_ERRORS_PER_CANVAS: usize = 16;
// Canvas identifiers arrive from content. A bound within each queue alone
// allows arbitrary identifiers to grow the outer table for the whole session.
const MAX_ERROR_CANVASES: usize = 256;

/// WebGL errors the decoder recorded, waiting for the producer to ask.
///
/// In this lane `getError` is a synchronous call from another process, so the
/// answers accumulate here until the control channel carries the question.
/// Keeps the latest bounded burst per canvas. The total table is bounded too;
/// when full, existing canvas queues keep their errors and new ids are ignored.
#[derive(Debug, Default)]
pub struct ExternalGlErrors {
    queues: Mutex<Vec<(u32, VecDeque<u32>)>>,
}

impl ExternalGlErrors {
    fn push(&self, canvas_id: u32, code: u32) {
        let mut queues = self.queues.lock();
        let queue = match queues.iter_mut().find(|(id, _)| *id == canvas_id) {
            Some((_, queue)) => queue,
            None => {
                if queues.len() == MAX_ERROR_CANVASES {
                    return;
                }
                queues.push((
                    canvas_id,
                    VecDeque::with_capacity(MAX_PENDING_ERRORS_PER_CANVAS),
                ));
                &mut queues.last_mut().expect("just pushed").1
            }
        };
        if queue.len() >= MAX_PENDING_ERRORS_PER_CANVAS {
            queue.pop_front();
        }
        queue.push_back(code);
    }

    fn take(&self, canvas_id: u32) -> Option<u32> {
        let mut queues = self.queues.lock();
        let index = queues.iter().position(|(id, _)| *id == canvas_id)?;
        let error = queues[index].1.pop_front();
        if queues[index].1.is_empty() {
            queues.swap_remove(index);
        }
        error
    }
}

/// The decoder's view of an external session.
struct ExternalDecodeContext<'a> {
    errors: &'a ExternalGlErrors,
    builder: shared::FramePacketBuilder,
}

impl frame_decode::GlDecodeContext for ExternalDecodeContext<'_> {
    fn push_error(&mut self, canvas_id: u32, code: u32) {
        self.errors.push(canvas_id, code);
    }

    fn transform_feedback_captures(&self, _canvas_id: u32) -> bool {
        // The producer runs the WebGL shim and knows its own feedback state; it
        // is not mirrored here. Answering `false` is what the in-process path
        // does for a canvas it has no record of, and the render thread rejects
        // the call for real if it is genuinely illegal.
        false
    }
}

impl frame_decode::RenderSink for ExternalDecodeContext<'_> {
    fn canvas_batch(
        &mut self,
        canvas_id: u32,
        commands: shared::command_vec_pool::PooledVec<shared::protocol::render_cmd::Canvas2DCmd>,
    ) {
        self.builder.push_op(shared::FrameOp::CanvasBatch(
            shared::protocol::render_cmd::CanvasBatchPayload {
                canvas_id,
                commands,
                present: false,
                dirty_rect: None,
            },
        ));
    }

    fn gl_batch(
        &mut self,
        commands: shared::command_vec_pool::PooledVec<shared::protocol::render_cmd::GLCmd>,
        _approx_bytes: usize,
    ) {
        self.builder.push_op(shared::FrameOp::GlBatch(
            shared::protocol::render_cmd::GlBatchPayload { commands },
        ));
    }

    fn materialize(&mut self, canvas_id: u32) {
        self.builder
            .push_op(shared::FrameOp::Materialize { canvas_id });
    }
}

/// What the submit path needs from the session thread, published once the
/// renderer is up.
struct RenderDispatch {
    // The running session thread owns the strong sender. A public session
    // handle may outlive that thread; retaining a sender here would also keep
    // abandoned queue contents and their frame credits alive after shutdown.
    sender: Weak<shared::render_command_sender::CommandSender>,
    /// Reused word buffer for the byte-to-word copy. One per session, behind
    /// the same lock as the submit path, because submits are serialized by the
    /// ingress anyway.
    words: Mutex<Vec<u32>>,
}

/// The frame clock, reachable from whichever thread the transport runs on.
///
/// A producer in another process asks for a frame; the host arms one vsync and
/// forwards the timestamp when it arrives. That is the same demand-driven shape
/// every other Migo platform already uses -- `requestAnimationFrame` here has
/// never been the browser's, it is `await op_await_next_frame()` fed by the
/// host -- so the cross-process version changes where the tick is delivered and
/// nothing about who drives it.
///
/// Deliberately not a channel. Arming a frame is a per-frame operation on the
/// latency path this lane exists to shorten, and putting it through the bounded
/// command queue would put it behind whatever else is queued.
#[derive(Default)]
pub struct ExternalFrameClock {
    /// Populated by the session thread once the renderer is up. A producer that
    /// asks before then is told no rather than silently ignored: a warm start
    /// has no clock yet, and a request that appears to succeed and produces no
    /// frame is indistinguishable from a hung renderer.
    inner: OnceLock<FrameClockParts>,
    ticks: AtomicU64,
    last_timestamp_millis: AtomicU64,
}

struct FrameClockParts {
    demand: shared::raf_signal::RafDemandRef,
    arm: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ExternalFrameClock {
    /// Ask for one frame. Returns `false` if the session is not yet rendering.
    pub fn request_frame(&self) -> bool {
        let Some(parts) = self.inner.get() else {
            return false;
        };
        parts.demand.mark_waiting();
        if let Some(arm) = &parts.arm {
            arm();
        }
        true
    }

    /// How many frame signals the renderer has delivered to this session.
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }

    /// The most recent frame timestamp, in whole milliseconds.
    ///
    /// Whole milliseconds because it crosses an atomic, and the sub-millisecond
    /// part belongs in the packet the producer builds rather than in a counter
    /// anyone can read. The exact `f64` is what gets forwarded.
    pub fn last_timestamp_millis(&self) -> u64 {
        self.last_timestamp_millis.load(Ordering::Relaxed)
    }

    fn record(&self, timestamp_millis: f64) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        self.last_timestamp_millis
            .store(timestamp_millis as u64, Ordering::Relaxed);
    }
}

/// Everything the submit path needs, separated from the thread handle.
///
/// The handle owns a running thread and cannot be built without one; this can,
/// which is what makes the acceptance rules testable without a renderer. The
/// separation is not only for tests: the transport calls into exactly these
/// three things and has no business reaching a `JoinHandle`.
struct SubmitPath {
    ingress: Arc<Mutex<FrameIngress>>,
    errors: Arc<ExternalGlErrors>,
    dispatch: Arc<OnceLock<RenderDispatch>>,
}

impl SubmitPath {
    /// Offer one packet produced by the external agent.
    ///
    /// Called on whichever thread the transport runs on -- on Apple that is the
    /// one handling the connection, not the session thread -- because a frame
    /// has to be validated and credited before it is queued, and a channel hop
    /// to do that would put a scheduling delay on the latency path this lane
    /// exists to shorten.
    ///
    /// The bytes are borrowed only for this call. Decode returns the wire
    /// storage to its pool while the owned render packet keeps the credit.
    pub fn submit_frame(&self, bytes: &[u8]) -> IngressOutcome {
        // Serialize through decode and queue submission too: releasing this
        // lock earlier lets concurrent callers dispatch N+1 before N, and
        // commits rejected frames before their decoder or queue can refuse them.
        self.ingress
            .lock()
            .submit_with(bytes, |frame| self.render(frame))
    }

    /// Decode one accepted frame and hand it to the renderer.
    fn render(&self, frame: PooledFrame) -> Result<(), u32> {
        let Some(dispatch) = self.dispatch.get() else {
            // The session thread has not finished bringing the renderer up.
            return Err(EXTERNAL_ERROR_RENDERER_NOT_READY);
        };
        let sender = dispatch
            .sender
            .upgrade()
            .ok_or(EXTERNAL_ERROR_RENDERER_UNREACHABLE)?;

        let parsed = frame.frame().map_err(|error| error.code())?;
        let stream = parsed
            .command_stream()
            .ok_or(EXTERNAL_ERROR_NO_COMMAND_STREAM)?;

        // Words, copied rather than cast. The packet's own base address carries
        // no alignment guarantee -- it arrived from a transport that promises
        // none -- so a pointer cast would work on every machine this is tested
        // on and fault on one it is not. The scratch buffer is reused, so the
        // copy costs no allocation after the first frame.
        let mut scratch = dispatch.words.lock();
        scratch.clear();
        scratch.reserve(stream.bytes.len() / 4);
        scratch.extend(
            stream
                .bytes
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]])),
        );
        let validated = stream::validate_frame_stream(&scratch, scratch.len() as u32)
            .map_err(|_| EXTERNAL_ERROR_BAD_COMMAND_STREAM)?;

        let budget = frame_decode::validate_frame_budget(&validated, MAX_DECODED_FRAME_BYTES)
            .map_err(|_| EXTERNAL_ERROR_BAD_COMMAND_STREAM)?;
        let mut sink = ExternalDecodeContext {
            errors: &self.errors,
            builder: shared::FramePacketBuilder::with_op_capacity(
                u64::from(parsed.frame_id()),
                0.0,
                budget.frame_op_capacity(),
            )
            .push(shared::FrameOp::BeginFrame),
        };
        frame_decode::decode_render_stream_into(&mut sink, validated);
        drop(scratch);
        let packet = sink
            .builder
            .push(shared::FrameOp::Present)
            .finish()
            .with_credit(frame.into_credit());

        sender
            .dispatch(shared::protocol::render_cmd::RenderCommand::FramePacket(
                packet,
            ))
            .map_err(|_| EXTERNAL_ERROR_RENDERER_UNREACHABLE)?;

        Ok(())
    }
}

impl ExternalFrameSession {
    #[inline]
    pub fn id(&self) -> crate::runtime::HostId {
        self.host.id()
    }

    /// Credits currently available to the producer.
    pub fn remaining_credits(&self) -> u32 {
        self.submit.ingress.lock().remaining_credits()
    }

    /// The surface generation packets must be addressed to.
    pub fn surface_generation(&self) -> u64 {
        self.submit.ingress.lock().surface_generation()
    }

    /// The resource epoch packets must name ids from.
    pub fn resource_epoch(&self) -> u64 {
        self.submit.ingress.lock().resource_epoch()
    }

    /// Whether the host has declared this epoch's resources verified.
    pub fn resources_ready(&self) -> bool {
        self.submit.ingress.lock().resources_ready()
    }

    /// The host has verified every resource this epoch admits.
    pub fn mark_resources_ready(&self) {
        self.submit.ingress.lock().mark_resources_ready();
    }

    /// The frame clock, for the transport that drives the producer.
    pub fn clock(&self) -> &Arc<ExternalFrameClock> {
        &self.clock
    }

    /// WebGL errors the decoder recorded, for the producer's `getError`.
    pub fn drain_gl_error(&self, canvas_id: u32) -> Option<u32> {
        self.submit.errors.take(canvas_id)
    }

    /// Offer one packet produced by the external agent.
    ///
    /// Called on whichever thread the transport runs on -- on Apple that is the
    /// one handling the connection, not the session thread -- because a frame
    /// has to be validated and credited before it is queued, and a channel hop
    /// to do that would put a scheduling delay on the latency path this lane
    /// exists to shorten.
    pub fn submit_frame(&self, bytes: &[u8]) -> IngressOutcome {
        self.submit.submit_frame(bytes)
    }

    /// Whether the caller is the session's own thread.
    ///
    /// Exposed for the same reason `HostThread` exposes it: joining from inside
    /// the thread being joined deadlocks, and a C host that owns both a session
    /// and a callback running on it has no other way to tell.
    pub fn is_current_thread(&self) -> bool {
        self.host.is_current_thread()
    }

    pub fn request_shutdown(&self) -> Result<(), String> {
        self.host.request_shutdown()
    }

    pub fn join(&mut self) -> EngineResult<()> {
        self.host.join()
    }

    pub fn shutdown_and_join(&mut self) -> EngineResult<()> {
        self.host.shutdown_and_join()
    }

    /// A session around an already-running thread, for tests that need a handle
    /// without a renderer.
    ///
    /// Mirrors `HostThread::from_join_handle_for_test` so the C boundary's own
    /// tests can build either execution mode the same way. Test-only: the
    /// submit path it returns has no renderer, which is a state a real session
    /// only passes through.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn from_join_handle_for_test(
        host_id: crate::runtime::HostId,
        join: std::thread::JoinHandle<()>,
        launch_nonce: u128,
    ) -> Self {
        Self {
            host: HostThread::from_join_handle_for_test(host_id, join),
            submit: SubmitPath {
                ingress: Arc::new(Mutex::new(FrameIngress::new(
                    launch_nonce,
                    INITIAL_RUNTIME_GENERATION,
                ))),
                errors: Arc::new(ExternalGlErrors::default()),
                dispatch: Arc::new(OnceLock::new()),
            },
            clock: Arc::new(ExternalFrameClock::default()),
        }
    }
}

/// The generation a session's first ingress accepts.
///
/// One, because a fresh `RestartBoundary` issues one, and the ingress is built
/// before the session thread exists so the two have to agree rather than one
/// telling the other. Named rather than written twice: a test asserts this
/// against the boundary, and a second copy of the literal would let the call
/// site drift away from the thing that checks it -- which is the failure where
/// every packet is rejected as belonging to a dead generation, on a device,
/// with nothing in the log saying why.
const INITIAL_RUNTIME_GENERATION: u64 = 1;

/// Start a session that renders frames produced by an external agent.
///
/// `launch_nonce` is the 128-bit identity this session will accept packets
/// under. It is generated once per app launch by the host, paired with the
/// producer out of band, and never appears in a URL, a query string or a log --
/// it is the value that decides whether bytes arriving from another process
/// belong to this session at all.
pub fn spawn_external_frame_session(
    launch_nonce: u128,
    surface: Option<SurfaceRef>,
    public_generation: Option<shared::surface::PublicSurfaceGeneration>,
    graphics_platform: graphics::egl_platform::GraphicsPlatform,
    platform: Arc<dyn PlatformServices>,
    opt: InitOptions,
) -> EngineResult<SpawnedExternalSession> {
    // Built here rather than on the session thread so the caller has the handle
    // the moment the spawn returns: a transport that connected before the
    // thread finished bringing up the renderer would otherwise have nowhere to
    // put the first packet, and "wait a bit" is not a protocol.
    let ingress = Arc::new(Mutex::new(FrameIngress::new(
        launch_nonce,
        INITIAL_RUNTIME_GENERATION,
    )));
    let thread_ingress = Arc::clone(&ingress);
    let clock = Arc::new(ExternalFrameClock::default());
    let thread_clock = Arc::clone(&clock);
    let errors = Arc::new(ExternalGlErrors::default());
    let dispatch: Arc<OnceLock<RenderDispatch>> = Arc::new(OnceLock::new());
    let thread_dispatch = Arc::clone(&dispatch);

    let started: StartedHost = spawn_session_thread(
        surface,
        graphics_platform,
        platform,
        opt,
        public_generation,
        move |ctx| run_external_session(ctx, thread_ingress, thread_clock, thread_dispatch),
    )?;

    Ok(SpawnedExternalSession {
        session: ExternalFrameSession {
            host: started.host,
            submit: SubmitPath {
                ingress,
                errors,
                dispatch,
            },
            clock,
        },
        resource: started.resource,
        ingress: started.ingress,
    })
}

/// The external session, start to finish, on its own thread.
fn run_external_session(
    ctx: SessionThreadContext,
    ingress: Arc<Mutex<FrameIngress>>,
    clock: Arc<ExternalFrameClock>,
    dispatch: Arc<OnceLock<RenderDispatch>>,
) {
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

    let shell = match SessionShell::build(
        id,
        &host_tx,
        critical_host_tx,
        initial_surface,
        graphics_platform,
        &platform,
        &opt,
        surface_control,
        vsync_rx,
    ) {
        Ok(shell) => shell,
        Err(error) => {
            error!("[Host {id}] failed to build the session shell: {error}");
            platform_for_error.notify_error(
                id,
                error.code.as_u16(),
                &error.msg,
                error.detail.as_deref().unwrap_or(""),
            );
            // `ready_tx` drops unsent, which is what turns this into a
            // synchronous error for the caller rather than a hang.
            return;
        }
    };

    let SessionShell {
        mut startup_guard,
        mut render,
        render_events,
        render_notify,
        backgrounded,
        raf_rx,
        raf_demand,
        request_vsync,
        // Host-side audio. In this lane the WebView never touches audio at all:
        // PCM does not cross the process boundary, only low-frequency control
        // messages do, so the service belongs here exactly as it does for an
        // embedded session.
        mut audio,
        // The network policy and capability snapshot are what the control
        // channel answers a producer's synchronous queries from; both land
        // with it.
        network_policy: _network_policy,
        gpu_caps: _gpu_caps,
        context_lost: _context_lost,
        timer_backgrounded: _timer_backgrounded,
        gpu_init_started: _gpu_init_started,
        // Why the render worker stopped, read at the one place this session
        // observes it stopping. `gpu_caps` cannot answer it: a panic after the
        // first frame leaves that level saying Ready.
        render_exit,
        t_start: _t_start,
    } = shell;

    // Publish the clock only once the renderer is up. Before this point
    // `request_frame` answers no, which is the truthful answer for a session
    // that cannot yet produce a frame.
    let _ = clock.inner.set(FrameClockParts {
        demand: Arc::clone(&raf_demand),
        arm: request_vsync.clone(),
    });
    // Published together with the clock, and only now: a transport that
    // submitted before the renderer existed would be told the renderer is not
    // ready, which is the truthful answer.
    let lifecycle_sender = Arc::new(render.sender());
    let _ = dispatch.set(RenderDispatch {
        sender: Arc::downgrade(&lifecycle_sender),
        words: Mutex::new(Vec::new()),
    });

    // The generation the producer must stamp every packet with. The handle was
    // built before this thread existed, so the two have to agree rather than one
    // telling the other -- and they agree because a fresh `RestartBoundary`
    // issues generation 1 and the handle is constructed with 1. Asserted rather
    // than assumed: if that ever stops being true, every packet is rejected as
    // belonging to a dead generation, and the symptom is a black screen with no
    // error anywhere.
    {
        // The engine counts generations as `i64` and the wire carries `u64`.
        // The boundary starts at 1 and only ever increments, so the conversion
        // is total in practice; it is still checked, because the failure of an
        // unchecked cast here is a negative generation reappearing as a very
        // large positive one that happens to match nothing, which reads as
        // "the producer is broken".
        let live = u64::try_from(restart_boundary.current()).unwrap_or(u64::MAX);
        let expected = ingress.lock().runtime_generation();
        if live != expected {
            error!(
                "[Host {id}] the ingress accepts generation {expected} but the session is on \
                 {live}; every packet would be rejected as a dead generation"
            );
            platform_for_error.notify_error(
                id,
                ErrorCode::Internal.as_u16(),
                "external-frame session generation mismatch",
                "",
            );
            return;
        }
    }

    let runtime = match create_runtime_before_ready(ready_tx, create_basic_runtime) {
        Ok(runtime) => runtime,
        Err(error) => {
            error!("[Host {id}] failed to enter tokio runtime: {error}");
            platform_for_error.notify_error(
                id,
                error.code.as_u16(),
                &error.msg,
                error.detail.as_deref().unwrap_or(""),
            );
            return;
        }
    };
    startup_guard.disarm();

    let mut last_context_epoch = 0u64;
    let mut last_swap_report: Option<std::time::Instant> = None;
    runtime.block_on(async move {
        loop {
            tokio::select! {
                command = host_rx.recv() => {
                    let Some(command) = command else {
                        info!("[Host {id}] command channel closed");
                        break;
                    };
                    if !handle_command(
                        id, command, &mut render, &mut audio, &backgrounded, &ingress,
                        &platform_for_error,
                    ) {
                        break;
                    }
                }
                () = render_notify.notified() => {
                    drain_render_events(
                        id,
                        &render_events,
                        &ingress,
                        &mut last_context_epoch,
                        &platform_for_error,
                        &mut last_swap_report,
                    );
                }
                timestamp = raf_rx.recv(raf_demand.session_ticket()) => {
                    match timestamp {
                        Some(timestamp) => {
                            // A tick is demand for production; queued render
                            // packets return their own credits when consumed.
                            clock.record(timestamp);
                        }
                        None => {
                            // The render thread is gone. Nothing else will
                            // arrive on this channel, and continuing to select
                            // on it would spin.
                            //
                            // And this is the only place that observes it going,
                            // so this is where the host is told. It used to be
                            // told by nothing: the worker logged its reason and
                            // dropped this sender, and the line below was the
                            // whole of the engine's response. On the iOS
                            // simulator, where the renderer failed for want of
                            // ANGLE, the host's only signal was `attach` losing a
                            // race for a registry entry the exiting session was
                            // removing -- so it arrived about two runs in three,
                            // as a bare MIGO_ERROR_INTERNAL with no reason.
                            //
                            // `RenderExit` records nothing when the worker was
                            // asked to stop, and this branch cannot be reached
                            // that way: a requested shutdown breaks this loop from
                            // the command arm before the clock closes. So a worker
                            // that arrived here without a reason is one that
                            // stopped without saying why, and saying *that* is
                            // still more use to a host than silence.
                            let failure = render_exit.failure();
                            info!(
                                "[Host {id}] frame clock closed: {}",
                                failure.map_or_else(
                                    || "no reason recorded".to_string(),
                                    ToString::to_string
                                )
                            );
                            match failure {
                                Some(failure) => platform_for_error.notify_error(
                                    id,
                                    failure.code.as_u16(),
                                    &failure.msg,
                                    failure.detail.as_deref().unwrap_or(""),
                                ),
                                None => platform_for_error.notify_error(
                                    id,
                                    ErrorCode::Internal.as_u16(),
                                    "the renderer stopped",
                                    "it recorded no reason",
                                ),
                            }
                            break;
                        }
                    }
                }
            }
        }
        // Audio first, then render: the audio thread can still be holding a
        // decoded buffer whose lifetime is tied to this session, and stopping
        // the renderer first would leave it writing into a context that is
        // going away.
        audio.shutdown();
        render.shutdown();
        drop(lifecycle_sender);
        info!("[Host {id}] external-frame session exited");
    });
}

/// Returns `false` when the session should stop.
fn handle_command(
    id: crate::runtime::HostId,
    command: HostCommand,
    render: &mut crate::services::RenderService,
    audio: &mut crate::services::AudioService,
    backgrounded: &Arc<std::sync::atomic::AtomicBool>,
    ingress: &Arc<Mutex<FrameIngress>>,
    platform: &Arc<dyn PlatformServices>,
) -> bool {
    use std::sync::atomic::Ordering;

    match command {
        HostCommand::Shutdown => return false,

        HostCommand::UpdateSurface { lease, pixel_ratio } => {
            // The surface generation advances *before* the renderer is told, so
            // a packet built against the previous surface cannot be accepted in
            // the window between the two. The producer is in another process
            // and does not stop when the screen rotates.
            let generation = lease.generation().get();
            if !ingress.lock().set_surface_generation(generation) {
                error!(
                    "[Host {id}] refused a surface generation that moves backwards: {generation}"
                );
            }
            if let Err(error) = render.update_surface(lease, pixel_ratio) {
                error!("[Host {id}] update_surface failed: {error:?}");
                // The host handed over a Surface the renderer could not even be asked
                // about, and `attach` has already returned, so nothing else can answer
                // for it -- the embedded execution has told its host about this class of
                // failure all along, through the render-error path this lane had none of.
                //
                // Reaching here means the request never entered the queue: a local
                // preflight refusal or a dispatch the queue would not take within its
                // 8 ms bound. Nothing will report it later, which is what makes it this
                // arm's to report. An install that *was* queued reports its own outcome
                // on the must-deliver stream -- `SurfaceInstalled` or `SurfaceLost` --
                // so this is not the place to guess at one.
                //
                // Cancelled is excluded, and not as a special case: it means this update
                // did not happen because what it was for is gone. Either the host retired
                // the Surface, in which case the party that made it gone is the party
                // being told, or the render worker is gone, in which case `RenderExit`
                // reports it with the reason this arm does not have. A normal
                // attach/detach race would otherwise reach the host as
                // MIGO_ERROR_INTERNAL, because that is what the C boundary maps every
                // engine error onto.
                if error.code != ErrorCode::Cancelled {
                    platform.notify_error(
                        id,
                        error.code.as_u16(),
                        &error.msg,
                        error.detail.as_deref().unwrap_or(""),
                    );
                }
            }
            // Nothing to resume on here: the request has only been queued. The resume
            // belongs to the `SurfaceInstalled` arm, which is where a Surface actually
            // becomes usable -- see the handover `OnShow` documents and this arm used to
            // perform against a merely-queued request.
        }

        HostCommand::SurfaceDestroyed { generation } => {
            render.on_surface_destroyed(generation);
        }

        HostCommand::SurfaceInstalled { revision } => {
            // Where a Surface becomes usable, and the only place: nothing waits for a
            // recreate, so this report is what commits the attachment and what the
            // resume hangs off. The `UpdateSurface` arm used to do both against a
            // request that had only been queued.
            //
            // Guarded on `backgrounded` for the reason the embedded execution guards it:
            // a host may install a Surface while hidden, and resuming then runs render
            // and audio in the background. The Surface is live but paused, and `OnShow`
            // drives the resume once it clears.
            if render.confirm_install(revision) && !backgrounded.load(Ordering::Relaxed) {
                render.resume();
                audio.resume();
            }
        }

        HostCommand::SurfaceLost {
            public_generation,
            reason,
        } => {
            warn!("[Host {id}] surface {public_generation:?} lost: {reason:?}");
            // `MigoOnSurfaceLostFn` is declared in `session.h`, wired through the C
            // boundary, and was never fired by this execution -- a public callback
            // that could not happen on the Apple product. The embedded execution has
            // forwarded this since the command existed; the difference was an
            // omission, not a decision.
            platform.notify_surface_lost(id, public_generation, reason);
            render.pause();
        }

        HostCommand::OnShow { .. } => {
            backgrounded.store(false, Ordering::Relaxed);
            // Only resume against a surface that is actually live. Android
            // fires `onResume` before `surfaceCreated`, so on that path the old
            // surface is already gone and the resume belongs to the
            // `UpdateSurface` that follows; resuming here would run a renderer
            // with nothing to present into.
            if render.has_live_surface() {
                render.resume();
                audio.resume();
            } else {
                debug!("[Host {id}] OnShow with no live surface; resume waits for UpdateSurface");
            }
        }

        HostCommand::OnHide => {
            backgrounded.store(true, Ordering::Relaxed);
            render.pause();
            audio.pause();
        }

        HostCommand::OnAudioInterruptionBegin => audio.pause(),
        HostCommand::OnAudioInterruptionEnd => audio.resume(),

        // Everything else is addressed to a script runtime this session does
        // not have. Logged rather than silently dropped: the two that will
        // arrive in production -- input and the frame clock -- belong to the
        // control channel that carries them to the producer, and until that
        // exists a host sending them is a host expecting something to happen.
        other => {
            debug!(
                "[Host {id}] {other:?} has no consumer in an external-frame session; \
                 input and clock delivery arrive with the control channel"
            );
        }
    }
    true
}

/// Drain what the renderer has to say, and keep the ingress on the same
/// timeline it is.
/// Minimum spacing between repeats of one render-error report.
///
/// Only presentation needs it: a Surface that rejects every swap produces one
/// event per frame, and `on_error` is a call into host code. The other reports here
/// happen at most once per attach or per loss episode, so they are not spaced --
/// suppressing a report that fires once would suppress the only one there was.
const RENDER_ERROR_NOTIFY_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

fn drain_render_events(
    id: crate::runtime::HostId,
    events: &shared::render_event::RenderEventReceiver,
    ingress: &Arc<Mutex<FrameIngress>>,
    last_context_epoch: &mut u64,
    platform: &Arc<dyn PlatformServices>,
    last_swap_report: &mut Option<std::time::Instant>,
) {
    while let Ok(event) = events.try_recv() {
        match event {
            RenderEvent::ContextLost => {
                // Every resource id the producer holds now names nothing, or
                // worse, names whatever the rebuilt table put in its place. The
                // epoch advance is what makes those ids fail loudly, and it
                // withdraws readiness in the same call so a frame cannot name a
                // resource between the loss and the host re-verifying the table.
                *last_context_epoch += 1;
                let epoch = *last_context_epoch;
                if !ingress.lock().set_resource_epoch(epoch) {
                    error!("[Host {id}] refused a resource epoch that moves backwards: {epoch}");
                }
                warn!("[Host {id}] GL context lost; resource epoch is now {epoch}");
            }
            RenderEvent::ContextRecovered { success } => {
                info!("[Host {id}] GL context recovered: success={success}");
                // The unrecoverable case, and the only one of the pair the host is
                // told about. A loss on its own is recoverable -- the render thread
                // rebuilds the share group and always follows up with this event --
                // and `on_error` is delivered as non-recoverable, so reporting every
                // loss would tell a compliant host to tear down a session that
                // recovered milliseconds later. Not spaced: the render thread already
                // reports once per loss episode.
                if !success {
                    platform.notify_error(
                        id,
                        ErrorCode::RenderBackendError.as_u16(),
                        "render context recovery failed",
                        "",
                    );
                }
            }
            RenderEvent::SwapFailed { message } => {
                warn!("[Host {id}] swap failed: {message}");
                // Presentation is the host's own Surface refusing the frame, so the
                // host is the one who can act on it. Spaced, because a Surface that
                // rejects every swap produces one of these per frame.
                let now = std::time::Instant::now();
                let due = last_swap_report.is_none_or(|last| {
                    now.duration_since(last) >= RENDER_ERROR_NOTIFY_MIN_INTERVAL
                });
                if due {
                    *last_swap_report = Some(now);
                    platform.notify_error(
                        id,
                        ErrorCode::RenderBackendError.as_u16(),
                        "render swap failed",
                        &message,
                    );
                }
            }
            // Deliberately not reported to the host: a failed drawing command is the
            // producer's, not the embedder's. In this lane the producer is a separate
            // process that already learns about its own errors -- WebGL through the
            // queue `getError` drains, and anything that invalidates its resource ids
            // through the epoch bump above, which makes the next packet naming a dead
            // id fail loudly. Handing them to `on_error` as well would report a
            // content bug to the one party that cannot fix it.
            other => {
                debug!("[Host {id}] render event: {other:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: u128 = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;

    /// One valid packet carrying an empty command stream.
    fn packet(sequence: u64) -> Vec<u8> {
        let stream: [u8; 0] = [];
        let mut frame = frame_wire::builder::WireFrameBuilder::new();
        frame.launch_nonce = NONCE;
        frame.runtime_generation = INITIAL_RUNTIME_GENERATION;
        frame.sequence = sequence;
        frame
            .section(frame_wire::SECTION_KIND_COMMAND_STREAM, 0, &stream)
            .build()
    }

    /// The source of this module, for the invariants that are about what it
    /// does *not* contain. A dependency-closure gate proves no engine is
    /// reachable from the built product; this proves the module was written
    /// that way rather than happening to resolve that way today.
    const SOURCE_WITH_TESTS: &str = include_str!("external.rs");

    /// Everything above this test module. Scanning the whole file would find
    /// the forbidden names in the list below and fail on its own words -- the
    /// self-reference this repository has already been caught by once, in an
    /// audit that came up four short because the auditing file counted itself.
    fn source() -> &'static str {
        let end = SOURCE_WITH_TESTS
            .find("#[cfg(test)]")
            .expect("this module has a test section");
        &SOURCE_WITH_TESTS[..end]
    }

    #[test]
    fn the_external_session_never_names_a_script_runtime() {
        for forbidden in [
            "HostJsRuntime",
            "JsRuntimeSlot",
            "runtime_v8",
            "invoke_host_hook",
            "EvaluateModule",
        ] {
            assert!(
                !source().contains(forbidden),
                "external.rs names {forbidden}; this session exists because there is no \
                 script runtime in this process"
            );
        }
    }

    /// A producer that asks for a frame before the renderer is up is told no.
    ///
    /// The alternative -- returning success and arming nothing -- is
    /// indistinguishable at the far end from a renderer that has hung, and the
    /// far end is in another process with no way to tell them apart.
    #[test]
    fn the_clock_refuses_before_the_renderer_is_up() {
        let clock = ExternalFrameClock::default();
        assert!(!clock.request_frame());
        assert_eq!(clock.ticks(), 0);
        assert_eq!(clock.last_timestamp_millis(), 0);
    }

    #[test]
    fn recorded_ticks_accumulate_and_keep_the_latest_timestamp() {
        let clock = ExternalFrameClock::default();
        clock.record(16.7);
        clock.record(33.4);
        clock.record(50.1);
        assert_eq!(clock.ticks(), 3);
        assert_eq!(
            clock.last_timestamp_millis(),
            50,
            "the counter carries whole milliseconds; the exact value is what gets forwarded"
        );
    }

    /// Every packet is measured against the generation the session is on, and
    /// the handle is built before the session thread exists. The two agree
    /// because a fresh boundary issues generation 1; if that ever changes, the
    /// session refuses to start rather than rejecting every frame.
    #[test]
    fn a_fresh_boundary_and_a_fresh_ingress_agree_on_the_generation() {
        let boundary = crate::runtime::restart_boundary::RestartBoundary::new();
        assert_eq!(
            u64::try_from(boundary.current()).expect("generations are positive"),
            INITIAL_RUNTIME_GENERATION,
            "spawn_external_frame_session builds the ingress with 1 because a fresh \
             RestartBoundary issues 1"
        );
    }

    /// A packet offered before the renderer exists is refused, and its credit
    /// comes straight back.
    ///
    /// The alternative -- accept it and hold the credit -- stalls a producer for
    /// a frame nobody is working on, and the producer cannot tell that apart
    /// from a renderer that is merely slow.
    #[test]
    fn a_frame_offered_before_the_renderer_is_up_is_refused_and_costs_no_credit() {
        let submit = SubmitPath {
            ingress: Arc::new(Mutex::new(FrameIngress::new(
                NONCE,
                INITIAL_RUNTIME_GENERATION,
            ))),
            errors: Arc::new(ExternalGlErrors::default()),
            dispatch: Arc::new(OnceLock::new()),
        };

        let bytes = packet(1);
        let outcome = submit.submit_frame(&bytes);
        assert_eq!(outcome.decision, IngressDecision::Rejected);
        assert_eq!(outcome.wire_error_code, EXTERNAL_ERROR_RENDERER_NOT_READY);
        assert_eq!(
            outcome.remaining_credits,
            frame_wire::ingress::MAX_CREDITS,
            "a frame that never reached the renderer is not in flight"
        );

        // And the sequence did not advance past it either, so the producer can
        // resend the same packet once the renderer is up.
        assert_eq!(submit.ingress.lock().last_accepted_sequence(), 0);
    }

    #[test]
    fn error_state_is_bounded_across_canvas_ids() {
        let errors = ExternalGlErrors::default();
        for canvas_id in 1..=4096 {
            errors.push(canvas_id, 0x0500);
        }
        assert!(errors.queues.lock().len() <= 256);
        assert_eq!(
            errors.take(1),
            Some(0x0500),
            "preserve existing errors under pressure"
        );
    }

    fn ready_submit() -> (
        SubmitPath,
        crossbeam_channel::Receiver<shared::protocol::render_cmd::RenderCommand>,
        Arc<shared::render_command_sender::CommandSender>,
    ) {
        let (sender, receiver) = shared::render_command_sender::CommandSender::new();
        let lifecycle_sender = Arc::new(sender);
        let dispatch = OnceLock::new();
        assert!(
            dispatch
                .set(RenderDispatch {
                    sender: Arc::downgrade(&lifecycle_sender),
                    words: Mutex::new(Vec::new()),
                })
                .is_ok()
        );
        (
            SubmitPath {
                ingress: Arc::new(Mutex::new(FrameIngress::new(
                    NONCE,
                    INITIAL_RUNTIME_GENERATION,
                ))),
                errors: Arc::new(ExternalGlErrors::default()),
                dispatch: Arc::new(dispatch),
            },
            receiver,
            lifecycle_sender,
        )
    }

    fn stream_packet(sequence: u64, words: &[u32]) -> Vec<u8> {
        let mut frame = frame_wire::builder::WireFrameBuilder::new();
        frame.launch_nonce = NONCE;
        frame.sequence = sequence;
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        frame
            .section(
                frame_wire::SECTION_KIND_COMMAND_STREAM,
                words.len() as u32,
                &bytes,
            )
            .build()
    }

    #[test]
    fn rejected_decode_can_retry_the_same_sequence() {
        let (submit, receiver, _lifecycle_sender) = ready_submit();
        let bad = stream_packet(1, &[0, stream::STREAM_VERSION]);
        let rejected = submit.submit_frame(&bad);
        assert_eq!(rejected.wire_error_code, EXTERNAL_ERROR_BAD_COMMAND_STREAM);
        assert_eq!(submit.ingress.lock().last_accepted_sequence(), 0);
        assert_eq!(rejected.remaining_credits, 2);
        let good = stream_packet(1, &[stream::MAGIC, stream::STREAM_VERSION]);
        assert_eq!(
            submit.submit_frame(&good).decision,
            IngressDecision::Accepted
        );
        assert!(receiver.try_recv().is_ok());
    }

    #[test]
    fn queued_frames_hold_credits_until_their_commands_are_dropped() {
        let (submit, receiver, _lifecycle_sender) = ready_submit();
        for sequence in 1..=2 {
            let bytes = stream_packet(sequence, &[stream::MAGIC, stream::STREAM_VERSION]);
            assert_eq!(
                submit.submit_frame(&bytes).decision,
                IngressDecision::Accepted
            );
        }
        assert_eq!(submit.ingress.lock().remaining_credits(), 0);
        let bytes = stream_packet(3, &[stream::MAGIC, stream::STREAM_VERSION]);
        assert_eq!(
            submit.submit_frame(&bytes).decision,
            IngressDecision::WouldBlock
        );
        drop(receiver.try_recv().unwrap());
        assert_eq!(submit.ingress.lock().remaining_credits(), 1);
        assert_eq!(
            submit.submit_frame(&bytes).decision,
            IngressDecision::Accepted
        );
        // Model renderer exit followed by its session thread ending while
        // the public SubmitPath (and its published dispatch) remains alive.
        drop(receiver);
        assert_eq!(submit.ingress.lock().remaining_credits(), 0);
        drop(_lifecycle_sender);
        assert_eq!(submit.ingress.lock().remaining_credits(), 2);
        let bytes = stream_packet(4, &[stream::MAGIC, stream::STREAM_VERSION]);
        assert_eq!(
            submit.submit_frame(&bytes).wire_error_code,
            EXTERNAL_ERROR_RENDERER_UNREACHABLE
        );
        assert_eq!(submit.ingress.lock().last_accepted_sequence(), 3);
    }

    #[test]
    fn disconnected_renderer_returns_credit_without_committing_sequence() {
        let (submit, receiver, _lifecycle_sender) = ready_submit();
        drop(receiver);
        let bytes = stream_packet(1, &[stream::MAGIC, stream::STREAM_VERSION]);
        let outcome = submit.submit_frame(&bytes);
        assert_eq!(outcome.wire_error_code, EXTERNAL_ERROR_RENDERER_UNREACHABLE);
        assert_eq!(outcome.remaining_credits, 2);
        assert_eq!(submit.ingress.lock().last_accepted_sequence(), 0);
    }

    #[test]
    fn full_renderer_queue_returns_credit_and_allows_retry() {
        let (submit, receiver, _lifecycle_sender) = ready_submit();
        let dispatch = submit.dispatch.get().unwrap();
        for _ in 0..receiver.capacity().unwrap() {
            dispatch
                .sender
                .upgrade()
                .unwrap()
                .dispatch(shared::protocol::render_cmd::RenderCommand::Pause)
                .unwrap();
        }
        let bytes = stream_packet(1, &[stream::MAGIC, stream::STREAM_VERSION]);
        let outcome = submit.submit_frame(&bytes);
        assert_eq!(outcome.wire_error_code, EXTERNAL_ERROR_RENDERER_UNREACHABLE);
        assert_eq!(outcome.remaining_credits, 2);
        assert_eq!(submit.ingress.lock().last_accepted_sequence(), 0);
        drop(receiver.try_recv().unwrap());
        assert_eq!(
            submit.submit_frame(&bytes).decision,
            IngressDecision::Accepted
        );
    }

    #[test]
    fn full_frame_accepts_more_than_one_embedded_batch() {
        let (submit, receiver, _lifecycle_sender) = ready_submit();
        let mut words = vec![
            stream::MAGIC,
            stream::STREAM_VERSION,
            stream::pack_header(frame_wire::canvas2d::OP2D_SELECT_CANVAS, 2),
            1,
        ];
        words.extend(std::iter::repeat_n(
            stream::pack_header(frame_wire::canvas2d::OP2D_SAVE, 1),
            8192,
        ));
        assert_eq!(
            submit.submit_frame(&stream_packet(1, &words)).decision,
            IngressDecision::Accepted
        );
        assert!(receiver.try_recv().is_ok());
    }

    #[test]
    fn excessive_decoded_memory_is_refused_before_queueing() {
        let (submit, receiver, _lifecycle_sender) = ready_submit();
        let mut words = vec![
            stream::MAGIC,
            stream::STREAM_VERSION,
            stream::pack_header(frame_wire::canvas2d::OP2D_SELECT_CANVAS, 2),
            1,
        ];
        words.extend(std::iter::repeat_n(
            stream::pack_header(frame_wire::canvas2d::OP2D_SAVE, 1),
            100_000,
        ));
        let outcome = submit.submit_frame(&stream_packet(1, &words));
        assert_eq!(outcome.decision, IngressDecision::Rejected);
        assert_eq!(outcome.wire_error_code, EXTERNAL_ERROR_BAD_COMMAND_STREAM);
        assert_eq!(outcome.remaining_credits, 2);
        assert_eq!(submit.ingress.lock().last_accepted_sequence(), 0);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn draining_errors_releases_the_canvas_entry() {
        let errors = ExternalGlErrors::default();
        errors.push(1, 0x0500);
        assert_eq!(errors.take(1), Some(0x0500));
        assert!(errors.queues.lock().is_empty());
    }

    #[test]
    fn the_error_queue_is_bounded_per_canvas_and_keeps_the_latest_burst() {
        let errors = ExternalGlErrors::default();
        for index in 0..(MAX_PENDING_ERRORS_PER_CANVAS + 4) {
            errors.push(1, 0x0500 + index as u32);
        }
        // Preserve the existing bounded FIFO behavior: discard the oldest
        // item when full, then drain the retained burst in arrival order.
        assert_eq!(errors.take(1), Some(0x0500 + 4));

        let mut drained = 1;
        while errors.take(1).is_some() {
            drained += 1;
        }
        assert_eq!(drained, MAX_PENDING_ERRORS_PER_CANVAS);
        assert_eq!(errors.take(1), None, "an empty queue reports no error");
    }

    #[test]
    fn errors_are_kept_per_canvas() {
        let errors = ExternalGlErrors::default();
        errors.push(1, 0x0500);
        errors.push(2, 0x0501);
        assert_eq!(errors.take(2), Some(0x0501));
        assert_eq!(errors.take(2), None);
        assert_eq!(
            errors.take(1),
            Some(0x0500),
            "another canvas's queue is untouched"
        );
        assert_eq!(
            errors.take(99),
            None,
            "a canvas with no errors reports none"
        );
    }

    #[test]
    fn a_new_ingress_admits_no_resources_and_starts_at_the_full_credit_window() {
        let ingress = FrameIngress::new(1, 1);
        assert!(!ingress.resources_ready());
        assert_eq!(ingress.resource_epoch(), 0);
        assert_eq!(ingress.surface_generation(), 0);
        assert_eq!(
            ingress.remaining_credits(),
            frame_wire::ingress::MAX_CREDITS
        );
    }
}
