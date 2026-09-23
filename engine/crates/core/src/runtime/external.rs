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
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use parking_lot::{Condvar, Mutex};
use tracing::{debug, error, info, warn};

use shared::{
    config::InitOptions,
    error::{EngineResult, ErrorCode},
    js_escape::{HOOK_ARGS_NONE, hook_args_one},
    protocol::host_cmd::HostCommand,
    render_event::RenderEvent,
    surface::SurfaceRef,
};

use frame_wire::control::{ControlError, ControlRecord, read_control};
use frame_wire::downlink::{DownlinkQueue, DownlinkRecord};
use frame_wire::sync::{
    ReadPixelsParams, SYNC_OP_AWAIT_WINDOW, SYNC_OP_READ_PIXELS, SyncAnswer, SyncError,
    SyncMailbox, SyncRequest, SyncState, WINDOW_REPLY_BYTES, WindowReply,
};
use frame_wire::{FrameIngress, IngressOutcome, PooledFrame, stream};
use frame_wire::{IngressDecision, WindowSource};

use crate::runtime::external_services::{
    RenderHandles, ServiceAdmission, ServiceContext, ServiceDispatcher, ServiceHandle, ServiceHost,
    ServiceSubmitError, ServiceWork, WakerSlot,
};
use crate::runtime::host_events::ServiceEventSink;
use crate::runtime::input_route;
use crate::runtime::input_state::InputState;
use crate::runtime::restart_boundary::is_retired_callback;
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
    sync: Arc<SyncPath>,
    clock: Arc<ExternalFrameClock>,
    /// What the host owes the producer: a verdict for every frame it submitted,
    /// and the frame clock's ticks. Shared with the clock and the submit path,
    /// which are what fill it, and drained by the transport through
    /// [`ExternalFrameSession::take_downlink`].
    ///
    /// The records are stamped with the runtime generation by whoever pushes
    /// them, not here -- the clock and the submit path each carry their own
    /// copy so neither has to reach for the ingress lock to answer.
    downlink: Arc<Mutex<DownlinkQueue>>,
    /// The service stream: files, storage, images, audio, network. Shared with
    /// the transports that admit into it and the session thread that runs it.
    services: Arc<ServiceHost>,
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

/// Hard per-frame decoded-storage ceiling; see
/// [`frame_decode::MAX_DECODED_FRAME_BYTES`], which a producer splits against.
use frame_decode::MAX_DECODED_FRAME_BYTES;

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
    /// Canvases whose transform feedback is capturing, as the decoded begin,
    /// pause, resume and end records left it. What `bindBufferBase` on a
    /// feedback buffer is refused against; the embedded runtime keeps the same
    /// state beside its error queues. Bounded like the queues.
    capturing: Mutex<Vec<u32>>,
}

impl ExternalGlErrors {
    fn set_transform_feedback(&self, canvas_id: u32, phase: frame_decode::TransformFeedbackPhase) {
        let mut capturing = self.capturing.lock();
        let index = capturing.iter().position(|id| *id == canvas_id);
        match (phase, index) {
            (frame_decode::TransformFeedbackPhase::Active, None) => {
                if capturing.len() < MAX_ERROR_CANVASES {
                    capturing.push(canvas_id);
                }
            }
            (frame_decode::TransformFeedbackPhase::Active, Some(_)) => {}
            (_, Some(index)) => {
                capturing.swap_remove(index);
            }
            (_, None) => {}
        }
    }

    fn transform_feedback_captures(&self, canvas_id: u32) -> bool {
        self.capturing.lock().contains(&canvas_id)
    }

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
    /// The session's services, whose image table resolves `texImage2D(…,
    /// image)`. `None` on a submit path built without them.
    services: Option<&'a ServiceContext>,
    builder: shared::FramePacketBuilder,
}

impl frame_decode::GlDecodeContext for ExternalDecodeContext<'_> {
    fn push_error(&mut self, canvas_id: u32, code: u32) {
        self.errors.push(canvas_id, code);
    }

    fn transform_feedback_captures(&self, canvas_id: u32) -> bool {
        self.errors.transform_feedback_captures(canvas_id)
    }

    fn set_transform_feedback(
        &mut self,
        canvas_id: u32,
        phase: frame_decode::TransformFeedbackPhase,
    ) {
        self.errors.set_transform_feedback(canvas_id, phase);
    }

    fn image_upload(
        &mut self,
        upload: frame_decode::ImageUpload,
    ) -> Option<shared::protocol::render_cmd::GLCmd> {
        self.services?.image_upload(upload)
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
/// What a poll of the mailbox reports, in one read.
///
/// A struct rather than four getters because the four values are only
/// meaningful together: a `reply_bytes` read a moment after a `state` can
/// describe a different request, and the producer this is forwarded to is
/// blocked on the pair agreeing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncSnapshot {
    pub request_id: u32,
    pub state: SyncState,
    pub reply_bytes: u32,
    pub error: Option<SyncError>,
}

/// The synchronous barrier, usable without holding the session's own lock.
///
/// See [`ExternalFrameSession::sync_handle`] for why it exists.
#[derive(Clone)]
pub struct SyncHandle(Arc<SyncPath>);

impl SyncHandle {
    /// Post a request and answer it; blocks until it is settled, and reports
    /// where THIS request ended rather than whatever the mailbox holds by the
    /// time a separate poll would run.
    pub fn post(
        &self,
        request: SyncRequest,
        params: &[u8],
        now_nanos: u64,
    ) -> Result<SyncSnapshot, SyncError> {
        self.0.post(request, params, now_nanos)
    }

    /// Answer a call carried as one body; blocks until it is settled.
    pub fn answer(&self, body: &[u8], now_nanos: u64) -> AnsweredCall {
        self.0.answer(body, now_nanos)
    }

    /// Where the outstanding request is.
    pub fn poll(&self, now_nanos: u64) -> SyncSnapshot {
        self.0.snapshot(now_nanos)
    }

    /// Copy a ready answer out and free the slot.
    pub fn take_reply(&self, out: &mut [u8]) -> Result<usize, SyncError> {
        self.0.take_reply(out)
    }
}

/// The host's half of the synchronous barrier.
///
/// `readPixels` cannot be answered where the producer runs, because its return
/// value *is* the answer and the pixels are here. So the producer blocks, the
/// transport carries the request across, this executes it against the renderer,
/// and the reply travels back.
///
/// The reply buffer is owned here rather than written into the caller's memory
/// during execution. The host copies it out afterwards with an explicit call,
/// which is what lets the mailbox refuse an oversized or mismatched answer
/// BEFORE any of it reaches a producer -- a truncated `readPixels` is a wrong
/// answer that looks like a right one.
/// What the synchronous path needs from ingress: the admitted sequence, and a
/// way to be woken when it moves.
///
/// A read answers for the frame the producer had submitted when it blocked
/// (`triggering_sequence`), and on the Apple uplink that frame can still be in
/// flight on the other stream when the read arrives -- a synchronous request
/// and a frame are two independent streams too. Answering before the frame is
/// admitted reads a surface it has not touched yet.
#[derive(Clone)]
struct Admission {
    ingress: Arc<Mutex<FrameIngress>>,
    admitted: Arc<Condvar>,
    /// The window, readable and waitable without the ingress lock -- which a
    /// frame's whole decode holds, and which a credit wait must not.
    window: WindowSource,
}

impl Admission {
    fn new(ingress: Arc<Mutex<FrameIngress>>) -> Self {
        let window = ingress.lock().window_source();
        Self {
            ingress,
            admitted: Arc::new(Condvar::new()),
            window,
        }
    }
}

struct SyncPath {
    mailbox: Mutex<SyncMailbox>,
    /// The errors this producer's own records made while being decoded, which
    /// is what `getError` answers from: the queue is the host's, so there is no
    /// renderer round trip to make.
    errors: Arc<ExternalGlErrors>,
    /// Holds the vector the renderer answered with, moved rather than copied
    /// into. Its capacity is whatever the last reply needed and is released
    /// when the next one replaces it, so a session that reads a full screen
    /// once does not carry that buffer for the rest of its life.
    reply: Mutex<Vec<u8>>,
    dispatch: Arc<OnceLock<RenderDispatch>>,
    admission: Admission,
    /// The service stream, for `SYNC_OP_SERVICE`. Absent on a path built
    /// without one, which answers that operation as unsupported.
    services: Option<Arc<ServiceHost>>,
}

impl SyncPath {
    fn new(
        runtime_generation: u64,
        dispatch: Arc<OnceLock<RenderDispatch>>,
        admission: Admission,
        errors: Arc<ExternalGlErrors>,
    ) -> Self {
        Self {
            mailbox: Mutex::new(SyncMailbox::new(runtime_generation)),
            errors,
            reply: Mutex::new(Vec::new()),
            dispatch,
            admission,
            services: None,
        }
    }

    /// Answer `SYNC_OP_SERVICE` through `services`.
    fn with_services(mut self, services: Arc<ServiceHost>) -> Self {
        self.services = Some(services);
        self
    }

    /// Post a request and answer it.
    ///
    /// The answer is produced inline, on the caller's thread, and that is a
    /// decision rather than a shortcut. The caller is the transport thread, and
    /// the only thing it has to do until this returns is carry a reply back to
    /// an agent that is already blocked inside `Atomics.wait`; handing the work
    /// to another thread would add a scheduling hop to a latency path whose
    /// whole cost is already the readback. What it must NOT do is wait longer
    /// than the producer agreed to, which is why the wait is bounded by the
    /// request's own deadline rather than by the renderer's default readback
    /// timeout -- the producer said how long it would wait, and that is the
    /// number that matters.
    fn post(
        &self,
        request: SyncRequest,
        params: &[u8],
        now_nanos: u64,
    ) -> Result<SyncSnapshot, SyncError> {
        // NOT CHECKED HERE, and the reason is worth the paragraph: the request
        // carries `surface_generation` and `resource_epoch`, and
        // contracts/frame-wire/wire-v1.md lists "a generation or epoch moves
        // under the request" among the ways a waiter is woken. Enforcing it was
        // written and then taken out again, because it made the barrier
        // unusable rather than safe.
        //
        // The session's surface generation starts at 0 and becomes the attached
        // one only when the renderer reports the surface created; nothing tells
        // a host when that happened, and no entry point exposes the value. So a
        // producer cannot construct a request that would pass the check, and a
        // check nothing can satisfy refuses every request rather than the stale
        // ones. Measured: with it in place, every case in
        // `MigoSyncBarrierABITests` came back STALE_GENERATION.
        //
        // The frame path has the same shape -- a packet naming the wrong
        // generation is answered GENERATION_LOST, and the producer has no way to
        // learn the right one either -- so this is one gap, not two, and it
        // closes when A3's control channel tells the producer what the current
        // generation and epoch are. The check belongs with that mechanism.
        let deadline_nanos = request.deadline_nanos;
        let max_reply_bytes = request.max_reply_bytes;
        let operation = request.operation;
        let triggering_sequence = request.triggering_sequence;

        let id = {
            let mut mailbox = self.mailbox.lock();
            // A request whose deadline has already passed is settled before a
            // new one is judged, so "already pending" cannot be reported for a
            // request nobody is waiting on any more.
            mailbox.expire_if_due(now_nanos);
            mailbox.post(request, now_nanos)?
        };

        let answered = self.execute(
            operation,
            params,
            max_reply_bytes,
            triggering_sequence,
            // The shared record has no field for it: a producer on that path
            // orders its service calls itself.
            0,
            deadline_nanos,
            now_nanos,
        );
        let mut mailbox = self.mailbox.lock();
        // Stored only once the mailbox has taken the answer, and under its lock.
        // A readback that finished after its request timed out and a newer
        // request was posted would otherwise overwrite the newer request's
        // bytes, and that request would hand its producer this one's pixels
        // under its own id. If the mailbox refuses, it is settled and carries
        // the reason; the post itself still succeeded.
        if let Some(pixels) = settle(&mut mailbox, id, answered) {
            *self.reply.lock() = pixels;
        }
        Ok(settled_snapshot(&mailbox, id))
    }

    /// Answer a call that arrived as one body.
    ///
    /// The verdict is read under the mailbox lock that settles it, the reply is
    /// the vector this call's own readback produced -- moved, never copied --
    /// and the slot is freed in the same critical section: a producer holding
    /// the response has the bytes, which is the event the record's producer
    /// signals by clearing the slot. So there is no window in which another
    /// request's answer can be taken for this one, and no second step for a
    /// transport to forget.
    ///
    /// Every outcome is an answer: the producer is blocked on the response
    /// whatever happened, and the verdict belongs in it.
    fn answer(&self, body: &[u8], now_nanos: u64) -> AnsweredCall {
        let call = match frame_wire::sync::SyncCall::decode(body) {
            Ok(call) => call,
            Err(error) => return AnsweredCall::failed(0, error),
        };
        let request = call.request(now_nanos);

        let id = {
            let mut mailbox = self.mailbox.lock();
            mailbox.expire_if_due(now_nanos);
            match mailbox.post(request, now_nanos) {
                Ok(id) => id,
                Err(error) => return AnsweredCall::failed(0, error),
            }
        };

        let answered = self.execute(
            request.operation,
            call.params,
            request.max_reply_bytes,
            request.triggering_sequence,
            call.service_sequence,
            request.deadline_nanos,
            now_nanos,
        );
        let mut mailbox = self.mailbox.lock();
        let pixels = settle(&mut mailbox, id, answered);
        let snapshot = settled_snapshot(&mailbox, id);
        let answered = match (snapshot.state, pixels) {
            // `settle` returns the pixels only when the mailbox took exactly
            // their length as this request's answer.
            (SyncState::Ready, Some(pixels)) => AnsweredCall {
                answer: SyncAnswer {
                    state: SyncState::Ready,
                    error: None,
                    request_id: id,
                    reply_bytes: snapshot.reply_bytes,
                },
                reply: pixels,
            },
            (SyncState::Cancelled, _) => AnsweredCall {
                answer: SyncAnswer {
                    state: SyncState::Cancelled,
                    error: None,
                    request_id: id,
                    reply_bytes: 0,
                },
                reply: Vec::new(),
            },
            // Failed, or settled under another verdict than the one this call
            // produced: the snapshot says which.
            _ => AnsweredCall::failed(id, snapshot.error.unwrap_or(SyncError::LateReply)),
        };
        // Only this request's slot: a newer request that was posted after this
        // one timed out is still somebody's, and freeing it would lose its
        // answer.
        if mailbox
            .request()
            .is_some_and(|request| request.request_id == id)
        {
            mailbox.acknowledge();
        }
        answered
    }

    /// Settle the mailbox for good and wake a request waiting for its frame.
    ///
    /// The flag is set under the ingress lock the fence waits on, and the wake
    /// follows it, so a request that checked the flag a moment before cannot
    /// miss both.
    fn end_session(&self) -> bool {
        let ingress = self.admission.ingress.lock();
        let settled = self.mailbox.lock().end_session();
        drop(ingress);
        self.admission.admitted.notify_all();
        settled
    }

    /// Run one operation and return its bytes.
    ///
    /// Returned rather than stored, so the caller decides under the mailbox lock
    /// whether they still answer anything: this runs without that lock, for as
    /// long as the readback takes, and the request it was for can be settled
    /// and replaced in the meantime.
    #[allow(clippy::too_many_arguments)]
    fn execute(
        &self,
        operation: u32,
        params: &[u8],
        max_reply_bytes: u32,
        triggering_sequence: u64,
        service_sequence: u64,
        deadline_nanos: u64,
        now_nanos: u64,
    ) -> Result<Vec<u8>, SyncError> {
        match operation {
            frame_wire::sync::SYNC_OP_SERVICE => self.service_call(
                params,
                max_reply_bytes,
                service_sequence,
                deadline_nanos,
                now_nanos,
            ),
            SYNC_OP_READ_PIXELS => self.read_pixels(
                params,
                max_reply_bytes,
                triggering_sequence,
                deadline_nanos,
                now_nanos,
            ),
            SYNC_OP_AWAIT_WINDOW => self.await_window(
                params,
                max_reply_bytes,
                triggering_sequence,
                deadline_nanos,
                now_nanos,
            ),
            frame_wire::sync::SYNC_OP_CANVAS2D_METRICS
            | frame_wire::sync::SYNC_OP_CANVAS2D_NUMBER
            | frame_wire::sync::SYNC_OP_CANVAS2D_FONT => self.canvas2d_query(
                operation,
                params,
                max_reply_bytes,
                triggering_sequence,
                deadline_nanos,
                now_nanos,
            ),
            frame_wire::sync::SYNC_OP_CANVAS2D_IMAGE_DATA
            | frame_wire::sync::SYNC_OP_CANVAS2D_SNAPSHOT => self.canvas2d_pixels(
                operation,
                params,
                max_reply_bytes,
                triggering_sequence,
                deadline_nanos,
                now_nanos,
            ),
            frame_wire::sync::SYNC_OP_GL_QUERY_SCALAR
            | frame_wire::sync::SYNC_OP_GL_QUERY_TEXT
            | frame_wire::sync::SYNC_OP_GL_QUERY_ACTIVE => self.gl_query(
                operation,
                params,
                max_reply_bytes,
                triggering_sequence,
                deadline_nanos,
                now_nanos,
            ),
            _ => Err(SyncError::UnsupportedOperation),
        }
    }

    /// `SYNC_OP_SERVICE`: a service op whose return value is the answer --
    /// `readFileSync`, `getStorageSync`.
    ///
    /// Waits for the service message the producer sent before it, then runs in
    /// order behind everything admitted (see `external_services`). The op's own
    /// failure is part of the reply; only the barrier failing is an error here.
    fn service_call(
        &self,
        params: &[u8],
        max_reply_bytes: u32,
        service_sequence: u64,
        deadline_nanos: u64,
        now_nanos: u64,
    ) -> Result<Vec<u8>, SyncError> {
        let Some(services) = &self.services else {
            return Err(SyncError::UnsupportedOperation);
        };
        let budget = deadline_nanos.saturating_sub(now_nanos);
        if budget == 0 {
            return Err(SyncError::TimedOut);
        }
        let until = std::time::Instant::now() + std::time::Duration::from_nanos(budget);
        services.wait_admitted(service_sequence, until)?;
        let reply = services.call_sync(params, until)?;
        if reply.len() > max_reply_bytes as usize {
            // Refused rather than cut: a truncated file is a wrong answer that
            // looks like a right one.
            return Err(SyncError::ReplyTooLarge);
        }
        Ok(reply)
    }

    /// Wait until ingress has admitted `triggering_sequence`, within `budget`.
    ///
    /// Admission dispatches a frame to the renderer before it records the
    /// sequence, so once the sequence is here the frame is already ahead of
    /// anything this request queues next. Zero means the producer had submitted
    /// nothing, and there is nothing to wait for.
    fn wait_for_admission(&self, triggering_sequence: u64, budget: u64) -> Result<(), SyncError> {
        if triggering_sequence == 0 {
            return Ok(());
        }
        let until = std::time::Instant::now() + std::time::Duration::from_nanos(budget);
        let mut ingress = self.admission.ingress.lock();
        while ingress.last_accepted_sequence() < triggering_sequence {
            // Checked under the ingress lock, which is the lock `end_session`
            // takes to set it: a session ending between this check and the wait
            // below would otherwise be a wake-up nobody receives.
            if self.mailbox.lock().is_ended() {
                return Err(SyncError::SessionEnded);
            }
            if self
                .admission
                .admitted
                .wait_until(&mut ingress, until)
                .timed_out()
                && ingress.last_accepted_sequence() < triggering_sequence
            {
                return Err(SyncError::TimedOut);
            }
        }
        Ok(())
    }

    /// `SYNC_OP_AWAIT_WINDOW`: every packet the producer sent is admitted and a
    /// credit is free, and here is the window.
    ///
    /// The advertisement is read after both, so it is the one a producer
    /// sending next is entitled to -- the same read, under the same rule, a
    /// verdict or a tick makes.
    fn await_window(
        &self,
        params: &[u8],
        max_reply_bytes: u32,
        triggering_sequence: u64,
        deadline_nanos: u64,
        now_nanos: u64,
    ) -> Result<Vec<u8>, SyncError> {
        if !params.is_empty() {
            return Err(SyncError::UnsupportedOperation);
        }
        if (max_reply_bytes as usize) < WINDOW_REPLY_BYTES {
            return Err(SyncError::ReplyTooLarge);
        }
        let budget = deadline_nanos.saturating_sub(now_nanos);
        if budget == 0 {
            return Err(SyncError::TimedOut);
        }
        let until = std::time::Instant::now() + std::time::Duration::from_nanos(budget);
        self.wait_for_admission(triggering_sequence, budget)?;
        if !self.admission.window.wait_for_credit(until) {
            // A renderer that holds every credit past the producer's deadline.
            // The session ending returns its credits as the queue is dropped, so
            // this is a stall, not a teardown.
            return Err(if self.mailbox.lock().is_ended() {
                SyncError::SessionEnded
            } else {
                SyncError::TimedOut
            });
        }
        let window = self.admission.window.read();
        Ok(WindowReply {
            remaining_credits: window.remaining_credits,
            accepted_sequence: window.accepted_sequence,
        }
        .encode()
        .to_vec())
    }

    /// `SYNC_OP_GL_QUERY_*`: one WebGL query, answered after the frame it names.
    ///
    /// Every one of these is a call whose return value IS the answer -- a link
    /// status, a uniform location, an info log -- so there is no default to
    /// return and no way to defer. The producer records its work, sends a
    /// barrier so the host executes it, and blocks here naming that barrier's
    /// sequence; this waits for it to be admitted and then asks the renderer.
    ///
    /// `getError` is the exception that proves the shape: the error queue is the
    /// host's, filled while decoding this producer's own records, so it is
    /// answered here without a round trip -- after the same wait, because an
    /// error recorded by the frame the producer is asking about has to be in the
    /// queue before it is read.
    fn gl_query(
        &self,
        operation: u32,
        params: &[u8],
        max_reply_bytes: u32,
        triggering_sequence: u64,
        deadline_nanos: u64,
        now_nanos: u64,
    ) -> Result<Vec<u8>, SyncError> {
        use frame_wire::sync::{
            ACTIVE_VARIABLE_HEADER_BYTES, GlQueryParams, SYNC_OP_GL_QUERY_ACTIVE,
            SYNC_OP_GL_QUERY_SCALAR, SYNC_OP_GL_QUERY_TEXT, gl_query,
        };
        use shared::protocol::render_cmd::{GLCmd, RenderCmdResp, RenderCommand};

        let query = GlQueryParams::decode(params)?;
        // The operation sizes the reply, so a kind sent under the wrong one
        // would be answered in a shape the producer is not reading.
        if GlQueryParams::operation(query.kind) != operation {
            return Err(SyncError::UnsupportedOperation);
        }
        if (max_reply_bytes as usize) < 4 {
            return Err(SyncError::ReplyTooLarge);
        }

        let budget = deadline_nanos.saturating_sub(now_nanos);
        if budget == 0 {
            return Err(SyncError::TimedOut);
        }
        self.wait_for_admission(triggering_sequence, budget)?;

        if query.kind == gl_query::GET_ERROR {
            let code = self.errors.take(query.canvas_id).unwrap_or(0);
            return Ok(code.to_le_bytes().to_vec());
        }

        let Some(dispatch) = self.dispatch.get() else {
            // The renderer is not up yet. Not "unsupported": this host does
            // implement the query, and a producer told otherwise stops asking.
            return Err(SyncError::SessionEnded);
        };
        let Some(sender) = dispatch.sender.upgrade() else {
            return Err(SyncError::SessionEnded);
        };
        let deadline = std::time::Duration::from_nanos(budget);

        /// Send a command carrying a reply channel, and wait for the answer.
        macro_rules! ask {
            ($build:expr) => {{
                let (tx, rx) = crossbeam_channel::bounded(1);
                if sender
                    .send_blocking_bounded(RenderCommand::GL($build(RenderCmdResp::from_sync(tx))))
                    .is_err()
                {
                    return Err(SyncError::SessionEnded);
                }
                match rx.recv_timeout(deadline) {
                    Ok(Ok(answer)) => answer,
                    // The renderer answered and the answer was an error: an
                    // object that does not exist, a context that went away. Not
                    // "unsupported", which is permanent and would stop the
                    // producer asking for the rest of the session.
                    Ok(Err(_)) => return Err(SyncError::OperationFailed),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        return Err(SyncError::TimedOut);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        return Err(SyncError::SessionEnded);
                    }
                }
            }};
        }

        let name = query.name_str().into_owned();
        match operation {
            SYNC_OP_GL_QUERY_SCALAR => {
                let value: i32 = match query.kind {
                    gl_query::PROGRAM_PARAMETER => ask!(|resp| GLCmd::GetProgramParameter {
                        program_id: query.object,
                        pname: query.pname,
                        resp,
                    }),
                    gl_query::SHADER_PARAMETER => ask!(|resp| GLCmd::GetShaderParameter {
                        shader_id: query.object,
                        pname: query.pname,
                        resp,
                    }),
                    gl_query::QUERY_PARAMETER => {
                        let value: u32 = ask!(|resp| GLCmd::GetQueryParameter {
                            query: query.object,
                            pname: query.pname,
                            resp,
                        });
                        value as i32
                    }
                    gl_query::CHECK_FRAMEBUFFER_STATUS => {
                        let value: u32 = ask!(|resp| GLCmd::CheckFramebufferStatus {
                            canvas_id: query.canvas_id,
                            target: query.pname,
                            resp,
                        });
                        value as i32
                    }
                    gl_query::CLIENT_WAIT_SYNC => {
                        let value: u32 = ask!(|resp| GLCmd::ClientWaitSync {
                            sync: query.object,
                            flags: query.pname,
                            resp,
                        });
                        value as i32
                    }
                    gl_query::UNIFORM_LOCATION => {
                        // `null` is -1, the value WebGL's own location type is
                        // compared against; the producer hands it straight back.
                        let found: Option<u32> = ask!(|resp| GLCmd::GetUniformLocation {
                            canvas_id: query.canvas_id,
                            program_id: query.object,
                            name: name.clone(),
                            resp,
                        });
                        found.map_or(-1, |location| location as i32)
                    }
                    gl_query::ATTRIB_LOCATION => {
                        let found: Option<u32> = ask!(|resp| GLCmd::GetAttribLocation {
                            canvas_id: query.canvas_id,
                            program_id: query.object,
                            name: name.clone(),
                            resp,
                        });
                        found.map_or(-1, |location| location as i32)
                    }
                    gl_query::UNIFORM_BLOCK_INDEX => {
                        let index: u32 = ask!(|resp| GLCmd::GetUniformBlockIndex {
                            program_id: query.object,
                            name: name.clone(),
                            resp,
                        });
                        index as i32
                    }
                    _ => return Err(SyncError::UnsupportedOperation),
                };
                Ok(value.to_le_bytes().to_vec())
            }
            SYNC_OP_GL_QUERY_TEXT => {
                let text: String = match query.kind {
                    gl_query::PROGRAM_INFO_LOG => {
                        let log: Option<String> = ask!(|resp| GLCmd::GetProgramInfoLog {
                            program_id: query.object,
                            resp,
                        });
                        log.unwrap_or_default()
                    }
                    gl_query::SHADER_INFO_LOG => {
                        let log: Option<String> = ask!(|resp| GLCmd::GetShaderInfoLog {
                            shader_id: query.object,
                            resp,
                        });
                        log.unwrap_or_default()
                    }
                    gl_query::PARAMETER => ask!(|resp| GLCmd::GetParameter {
                        canvas_id: query.canvas_id,
                        pname: query.pname,
                        resp,
                    }),
                    _ => return Err(SyncError::UnsupportedOperation),
                };
                // Truncating an info log would be a wrong answer that looks
                // like a right one, which is what this whole barrier refuses.
                if text.len() > max_reply_bytes as usize {
                    return Err(SyncError::ReplyTooLarge);
                }
                Ok(text.into_bytes())
            }
            SYNC_OP_GL_QUERY_ACTIVE => {
                let found: Option<(String, i32, u32)> = match query.kind {
                    gl_query::ACTIVE_ATTRIB => ask!(|resp| GLCmd::GetActiveAttrib {
                        canvas_id: query.canvas_id,
                        program_id: query.object,
                        index: query.pname,
                        resp,
                    }),
                    gl_query::ACTIVE_UNIFORM => ask!(|resp| GLCmd::GetActiveUniform {
                        canvas_id: query.canvas_id,
                        program_id: query.object,
                        index: query.pname,
                        resp,
                    }),
                    gl_query::TRANSFORM_FEEDBACK_VARYING => {
                        ask!(|resp| GLCmd::GetTransformFeedbackVarying {
                            program: query.object,
                            index: query.pname,
                            resp,
                        })
                    }
                    _ => return Err(SyncError::UnsupportedOperation),
                };
                // No such index is `null` in WebGL, and here a reply with a zero
                // type and no name: every real variable has a type.
                let (name, size, type_) = found.unwrap_or_default();
                if ACTIVE_VARIABLE_HEADER_BYTES + name.len() > max_reply_bytes as usize {
                    return Err(SyncError::ReplyTooLarge);
                }
                let mut reply = Vec::with_capacity(ACTIVE_VARIABLE_HEADER_BYTES + name.len());
                reply.extend_from_slice(&size.to_le_bytes());
                reply.extend_from_slice(&type_.to_le_bytes());
                reply.extend_from_slice(name.as_bytes());
                Ok(reply)
            }
            _ => Err(SyncError::UnsupportedOperation),
        }
    }

    /// `SYNC_OP_CANVAS2D_IMAGE_DATA` / `SYNC_OP_CANVAS2D_SNAPSHOT`: the pixels
    /// of a 2D canvas, answered after the frame that drew them.
    ///
    /// Two operations, one shape. `getImageData` in the engine's own facade
    /// captures into the host's snapshot pool and reads the bytes only if
    /// content asks for them -- so the snapshot read is the common one, and the
    /// direct rectangle read is the fallback the facade takes for a read it
    /// cannot capture (zero area, out of bounds).
    ///
    /// The barrier matters for the same reason it does for `measureText`: the
    /// capture is a record in the frame being built, so a read that overtook it
    /// would find an empty pool and answer zeros -- which is a blank texture in
    /// a game rather than an error anyone sees.
    fn canvas2d_pixels(
        &self,
        operation: u32,
        params: &[u8],
        max_reply_bytes: u32,
        triggering_sequence: u64,
        deadline_nanos: u64,
        now_nanos: u64,
    ) -> Result<Vec<u8>, SyncError> {
        use frame_wire::sync::{Canvas2DPixelsParams, SYNC_OP_CANVAS2D_SNAPSHOT};
        use shared::protocol::render_cmd::{Canvas2DCmd, RenderCmdResp, RenderCommand};

        let read = Canvas2DPixelsParams::decode(params)?;
        let wanted = read.reply_bytes().ok_or(SyncError::ReplyTooLarge)?;
        // Refused before the renderer is asked, as the readback is: a rectangle
        // the producer did not reserve for is one nobody can answer.
        if wanted > max_reply_bytes {
            return Err(SyncError::ReplyTooLarge);
        }

        let budget = deadline_nanos.saturating_sub(now_nanos);
        if budget == 0 {
            return Err(SyncError::TimedOut);
        }
        self.wait_for_admission(triggering_sequence, budget)?;

        let Some(dispatch) = self.dispatch.get() else {
            return Err(SyncError::SessionEnded);
        };
        let Some(sender) = dispatch.sender.upgrade() else {
            return Err(SyncError::SessionEnded);
        };

        let (tx, rx) = crossbeam_channel::bounded(1);
        let command = if operation == SYNC_OP_CANVAS2D_SNAPSHOT {
            RenderCommand::Canvas2D {
                // The snapshot pool is the renderer's own and the canvas it was
                // taken from is already recorded in it, so the id is what names
                // the pixels; the embedded op passes 1 here for the same reason.
                canvas_id: 1,
                cmd: Canvas2DCmd::ReadSnapshotPixels {
                    snapshot_id: read.target,
                    resp: RenderCmdResp::from_sync(tx),
                },
            }
        } else {
            RenderCommand::Canvas2D {
                canvas_id: read.target,
                cmd: Canvas2DCmd::GetImageData {
                    x: read.x,
                    y: read.y,
                    width: read.width,
                    height: read.height,
                    resp: RenderCmdResp::from_sync(tx),
                },
            }
        };
        // Blocking-bounded, as the readback is: this command carries a reply
        // channel a producer is waiting on.
        if sender.send_blocking_bounded(command).is_err() {
            return Err(SyncError::SessionEnded);
        }

        let pixels = match rx.recv_timeout(std::time::Duration::from_nanos(budget)) {
            Ok(Ok(pixels)) => pixels,
            Ok(Err(_)) => return Err(SyncError::OperationFailed),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => return Err(SyncError::TimedOut),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                return Err(SyncError::SessionEnded);
            }
        };
        // The rectangle said one size and the renderer produced another: a
        // snapshot that had been dropped answers empty, and copying that as if
        // it were the picture is how a game draws a blank label and nothing
        // says why. The producer sized its buffer from the same rectangle.
        if u32::try_from(pixels.len()).map_err(|_| SyncError::OperationFailed)? != wanted {
            return Err(SyncError::OperationFailed);
        }
        Ok(pixels)
    }

    /// `SYNC_OP_CANVAS2D_*`: one Canvas2D query, answered after the frame it
    /// names.
    ///
    /// `measureText` is the one every game with a label calls, and it asks
    /// about state the frame being built set: the font two records ago. So it
    /// takes the same route as a WebGL query -- a barrier, then a blocked call
    /// naming its sequence -- and the renderer measures with the canvas's own
    /// font, which the barrier has by then applied.
    fn canvas2d_query(
        &self,
        operation: u32,
        params: &[u8],
        max_reply_bytes: u32,
        triggering_sequence: u64,
        deadline_nanos: u64,
        now_nanos: u64,
    ) -> Result<Vec<u8>, SyncError> {
        use frame_wire::sync::{
            Canvas2DQueryParams, MAX_FONT_FAMILY_REPLY_BYTES, SYNC_OP_CANVAS2D_FONT,
            SYNC_OP_CANVAS2D_METRICS, SYNC_OP_CANVAS2D_NUMBER, TEXT_METRICS_BYTES, canvas2d_query,
        };
        use shared::protocol::render_cmd::{Canvas2DCmd, RenderCmdResp, RenderCommand};

        let query = Canvas2DQueryParams::decode(params)?;
        if Canvas2DQueryParams::operation(query.kind) != operation {
            return Err(SyncError::UnsupportedOperation);
        }

        let budget = deadline_nanos.saturating_sub(now_nanos);
        if budget == 0 {
            return Err(SyncError::TimedOut);
        }
        self.wait_for_admission(triggering_sequence, budget)?;

        let Some(dispatch) = self.dispatch.get() else {
            return Err(SyncError::SessionEnded);
        };
        let Some(sender) = dispatch.sender.upgrade() else {
            return Err(SyncError::SessionEnded);
        };
        let deadline = std::time::Duration::from_nanos(budget);

        match operation {
            SYNC_OP_CANVAS2D_METRICS => {
                if (max_reply_bytes as usize) < TEXT_METRICS_BYTES {
                    return Err(SyncError::ReplyTooLarge);
                }
                let (tx, rx) = crossbeam_channel::bounded(1);
                let command = RenderCommand::Canvas2D {
                    canvas_id: query.canvas_id,
                    cmd: Canvas2DCmd::MeasureText {
                        text: query.text_str().into_owned(),
                        resp: RenderCmdResp::from_sync(tx),
                    },
                };
                if sender.send_blocking_bounded(command).is_err() {
                    return Err(SyncError::SessionEnded);
                }
                let metrics = match rx.recv_timeout(deadline) {
                    Ok(Ok(metrics)) => metrics,
                    Ok(Err(_)) => return Err(SyncError::OperationFailed),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        return Err(SyncError::TimedOut);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        return Err(SyncError::SessionEnded);
                    }
                };
                Ok(frame_decode::canvas2d::encode_text_metrics(&metrics))
            }
            // `loadFont(path, family)`: the file is the game's, the registration
            // is the renderer's, and the answer is the family key content will
            // name the face by. Every step the in-process op takes, in its
            // order -- the two decisions before the file is even read are
            // `shared::font_registration`'s, so a font named one thing here and
            // another there is not possible.
            SYNC_OP_CANVAS2D_FONT => {
                if max_reply_bytes > MAX_FONT_FAMILY_REPLY_BYTES {
                    return Err(SyncError::ReplyTooLarge);
                }
                let path = query.text_str().into_owned();
                let requested = query.font_str().into_owned();
                let Some(services) = &self.services else {
                    return Err(SyncError::SessionEnded);
                };
                let sources = services.context.local_sources();
                let Ok(resolved) = shared::font_registration::resolve_font_src_path(
                    sources.code_dir.as_deref().unwrap_or(""),
                    sources.vfs.as_deref(),
                    &path,
                ) else {
                    // The op logs and answers an empty family for a path it
                    // cannot resolve; so does this, because content reads the
                    // answer rather than an error.
                    return Ok(Vec::new());
                };
                let Ok(bytes) = std::fs::read(&resolved) else {
                    return Ok(Vec::new());
                };
                if bytes.is_empty() {
                    return Ok(Vec::new());
                }
                let request = shared::font_registration::build_font_registration_request(
                    &path,
                    (!requested.is_empty()).then_some(requested.as_str()),
                );
                let (tx, rx) = crossbeam_channel::bounded(1);
                let command = RenderCommand::LoadFont {
                    family: request.family.clone(),
                    aliases: std::sync::Arc::new(request.aliases.clone()),
                    bytes: std::sync::Arc::new(bytes),
                    resp: RenderCmdResp::from_sync(tx),
                };
                if sender.send_blocking_bounded(command).is_err() {
                    return Err(SyncError::SessionEnded);
                }
                let family = match rx.recv_timeout(deadline) {
                    // A renderer that refused the face answers the empty family
                    // the op answers, not a failure: `loadFont` reports itself
                    // through its return value.
                    Ok(Ok(family)) => family,
                    Ok(Err(_)) => String::new(),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        return Err(SyncError::TimedOut);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        return Err(SyncError::SessionEnded);
                    }
                };
                if family.len() > max_reply_bytes as usize {
                    return Err(SyncError::ReplyTooLarge);
                }
                Ok(family.into_bytes())
            }
            SYNC_OP_CANVAS2D_NUMBER => {
                if (max_reply_bytes as usize) < 8 {
                    return Err(SyncError::ReplyTooLarge);
                }
                if query.kind != canvas2d_query::TEXT_LINE_HEIGHT {
                    return Err(SyncError::UnsupportedOperation);
                }
                let (tx, rx) = crossbeam_channel::bounded(1);
                let command = RenderCommand::GetTextLineHeight {
                    font_family: query.font_str().into_owned(),
                    font_size: query.number_f32(),
                    bold: query.flags & canvas2d_query::FLAG_BOLD != 0,
                    italic: query.flags & canvas2d_query::FLAG_ITALIC != 0,
                    resp: RenderCmdResp::from_sync(tx),
                };
                if sender.send_blocking_bounded(command).is_err() {
                    return Err(SyncError::SessionEnded);
                }
                let height = match rx.recv_timeout(deadline) {
                    Ok(Ok(height)) => height,
                    Ok(Err(_)) => return Err(SyncError::OperationFailed),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        return Err(SyncError::TimedOut);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        return Err(SyncError::SessionEnded);
                    }
                };
                Ok(f64::from(height).to_le_bytes().to_vec())
            }
            _ => Err(SyncError::UnsupportedOperation),
        }
    }

    /// `SYNC_OP_READ_PIXELS`.
    fn read_pixels(
        &self,
        params: &[u8],
        max_reply_bytes: u32,
        triggering_sequence: u64,
        deadline_nanos: u64,
        now_nanos: u64,
    ) -> Result<Vec<u8>, SyncError> {
        let params = ReadPixelsParams::decode(params)?;
        let wanted = params.pixel_bytes().ok_or(SyncError::ReplyTooLarge)?;
        let reply_bytes = params.reply_bytes().ok_or(SyncError::ReplyTooLarge)?;
        // Checked here as well as by the mailbox, because refusing before the
        // renderer is asked saves a full-screen readback nobody may have.
        if reply_bytes > max_reply_bytes {
            return Err(SyncError::ReplyTooLarge);
        }

        let Some(dispatch) = self.dispatch.get() else {
            // The session thread has not finished bringing the renderer up.
            // Not "unsupported": this host does implement the operation, it
            // just cannot answer yet, and a producer told otherwise would stop
            // asking.
            return Err(SyncError::SessionEnded);
        };
        let Some(sender) = dispatch.sender.upgrade() else {
            return Err(SyncError::SessionEnded);
        };

        let budget = deadline_nanos.saturating_sub(now_nanos);
        if budget == 0 {
            return Err(SyncError::TimedOut);
        }

        // The frame the read is about, before the read: see `wait_for_admission`.
        self.wait_for_admission(triggering_sequence, budget)?;

        let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
        let command = shared::protocol::render_cmd::RenderCommand::GL(
            shared::protocol::render_cmd::GLCmd::ReadPixels {
                canvas_id: params.canvas_id.into(),
                x: params.x,
                y: params.y,
                width: params.width,
                height: params.height,
                format: params.format,
                type_: params.type_,
                // Unbounded, because the view this bound protects is not on this
                // side of the boundary. In-process, the renderer refuses a PACK
                // footprint (skips and row padding included) that overruns the
                // content's ArrayBufferView, since it is about to be copied into
                // that view. Here the view is in WebContent and the reply is only
                // the compact rows, which `wanted` and `max_reply_bytes` already
                // bound. Passing the reservation instead would refuse a read the
                // producer's view has room for whenever PACK_ALIGNMENT pads a row
                // or PACK_SKIP_* is set -- a false INVALID_OPERATION for a
                // footprint only the producer can check, against a view only it
                // holds. The footprint it checks against is the layout below,
                // which is why that is answered rather than dropped.
                destination_byte_length: usize::MAX,
                resp: shared::protocol::render_cmd::RenderCmdResp::from_sync(resp_tx),
            },
        );
        // Blocking-bounded rather than best-effort: this command carries a
        // reply channel a producer is waiting on, and a dropped one is a
        // producer that waits out its whole deadline for nothing.
        if sender.send_blocking_bounded(command).is_err() {
            return Err(SyncError::SessionEnded);
        }

        // The layout is answered, not dropped. It places rows in a destination
        // view from `PACK_*` state that only this side holds: the producer never
        // sees `pixelStorei`, because the engine's own encoder writes it into the
        // command stream the producer forwards unread. An earlier version of this
        // said the producer derives the placement itself, which it cannot.
        let (pixels, layout) = match resp_rx.recv_timeout(std::time::Duration::from_nanos(budget)) {
            Ok(Ok(readback)) => (readback.pixels, readback.layout),
            // The renderer answered and the answer was an error: a canvas that
            // does not exist, a GL failure, a surface that went away mid-read.
            // Not "unsupported" -- that is permanent and would stop the
            // producer asking again for the rest of the session.
            Ok(Err(_)) => return Err(SyncError::OperationFailed),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => return Err(SyncError::TimedOut),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                return Err(SyncError::SessionEnded);
            }
        };

        // The renderer answering with a different number of bytes than the
        // rectangle implies is not something to paper over by copying what
        // arrived: the producer sized its buffer from the same rectangle.
        let produced = u32::try_from(pixels.len()).map_err(|_| SyncError::OperationFailed)?;
        if produced != wanted {
            // The rectangle said one size and the readback produced another.
            // Copying what arrived would answer over a rectangle the producer
            // did not ask for, and calling it ReplyTooLarge would blame the
            // producer's reservation for the renderer's disagreement.
            return Err(SyncError::OperationFailed);
        }

        // The layout in front of the rows a producer is waiting for. The vector
        // the renderer allocated is kept rather than copied -- up to 14 MiB for a
        // full-screen phone at 4x, on the path a producer is blocked on -- and
        // the sixteen-byte header is spliced in front of it, which is one move of
        // the tail rather than a second allocation of the whole readback.
        let header = frame_wire::sync::ReadPixelsLayout {
            first_byte: u32::try_from(layout.first_byte).map_err(|_| SyncError::ReplyTooLarge)?,
            row_bytes: u32::try_from(layout.row_bytes).map_err(|_| SyncError::ReplyTooLarge)?,
            row_stride: u32::try_from(layout.row_stride).map_err(|_| SyncError::ReplyTooLarge)?,
            height: u32::try_from(layout.height).map_err(|_| SyncError::ReplyTooLarge)?,
        }
        .encode();
        let mut pixels = pixels;
        pixels.splice(0..0, header);
        Ok(pixels)
    }

    fn snapshot(&self, now_nanos: u64) -> SyncSnapshot {
        let mut mailbox = self.mailbox.lock();
        // A poll is the only thing that runs while a request is outstanding, so
        // it is where a passed deadline becomes visible.
        mailbox.expire_if_due(now_nanos);
        SyncSnapshot {
            request_id: mailbox.request().map(|r| r.request_id).unwrap_or(0),
            state: mailbox.state(),
            reply_bytes: mailbox.reply_bytes(),
            error: mailbox.error(),
        }
    }

    /// Copy the answer out and free the slot.
    ///
    /// Taking and acknowledging are one step because they are one event: the
    /// producer has the bytes, so the request is over. Two steps would admit a
    /// state where the answer has been read and the slot still refuses the next
    /// request.
    fn take_reply(&self, out: &mut [u8]) -> Result<usize, SyncError> {
        let mut mailbox = self.mailbox.lock();
        if mailbox.state() != SyncState::Ready {
            return Err(mailbox.error().unwrap_or(SyncError::LateReply));
        }
        let reply = self.reply.lock();
        let bytes = mailbox.reply_bytes() as usize;
        if out.len() < bytes || reply.len() < bytes {
            // Refused, and the request stays READY: a caller that arrived with
            // too small a buffer may come back with a large enough one, and
            // clearing the slot here would lose an answer that is still valid.
            return Err(SyncError::ReplyTooLarge);
        }
        out[..bytes].copy_from_slice(&reply[..bytes]);
        drop(reply);
        mailbox.acknowledge();
        Ok(bytes)
    }
}

/// A one-body call's answer: the header, and the reply that follows it.
///
/// Two parts rather than one buffer, because the reply is the vector the
/// renderer allocated to answer with and prefixing it would mean copying it --
/// up to 16 MiB on the path a producer is blocked on. A transport sends the
/// header and then the reply; the producer receives one body.
#[derive(Debug, Eq, PartialEq)]
pub struct AnsweredCall {
    pub answer: SyncAnswer,
    /// Exactly `answer.reply_bytes` long. Empty, and unallocated, unless READY.
    pub reply: Vec<u8>,
}

impl AnsweredCall {
    fn failed(request_id: u32, error: SyncError) -> Self {
        Self {
            answer: SyncAnswer::failed(request_id, error),
            reply: Vec::new(),
        }
    }
}

/// Record `id`'s verdict, if the mailbox still holds that request, and return
/// the pixels when the mailbox took them as the answer.
///
/// A readback can outlive its request: the deadline passes, the slot is settled,
/// and a newer request is posted while this one's pixels are still on their
/// way. The mailbox answers a reply naming a request other than the outstanding
/// one by failing the outstanding one, which is right for a reply that came
/// from outside and wrong for one that is merely late -- the newer request did
/// nothing, and this host is the one that knows why the ids differ.
fn settle(
    mailbox: &mut SyncMailbox,
    id: u32,
    answered: Result<Vec<u8>, SyncError>,
) -> Option<Vec<u8>> {
    if mailbox
        .request()
        .is_none_or(|request| request.request_id != id)
    {
        return None;
    }
    match answered {
        // A length past u32 cannot be a reservation, so the mailbox refuses it
        // as too large rather than this truncating it into one.
        Ok(pixels) => mailbox
            .complete(id, u32::try_from(pixels.len()).unwrap_or(u32::MAX))
            .is_ok()
            .then_some(pixels),
        Err(error) => {
            let _ = mailbox.fail_request(id, error);
            None
        }
    }
}

/// Where request `id` ended, read under the lock that settled it.
///
/// A request that is no longer the mailbox's was settled and replaced before its
/// readback returned; nothing it produced answers anything, and it is reported
/// as the late reply it is.
fn settled_snapshot(mailbox: &SyncMailbox, id: u32) -> SyncSnapshot {
    match mailbox.request() {
        Some(request) if request.request_id == id => SyncSnapshot {
            request_id: id,
            state: mailbox.state(),
            reply_bytes: mailbox.reply_bytes(),
            error: mailbox.error(),
        },
        _ => SyncSnapshot {
            request_id: id,
            state: SyncState::Failed,
            reply_bytes: 0,
            error: Some(SyncError::LateReply),
        },
    }
}

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
pub struct ExternalFrameClock {
    /// Populated by the session thread once the renderer is up.
    inner: OnceLock<FrameClockParts>,
    /// A request made before `inner` was published, waiting to be armed.
    ///
    /// Held rather than refused, because the producer's first request races the
    /// host's bring-up and nothing tells it to ask again: a dropped request is a
    /// producer waiting for a tick that is never sent. [`Self::request_frame`]
    /// sets this and then looks for the parts; [`Self::publish`] sets the parts
    /// and then drains this. Whichever runs second sees the other's write, so a
    /// request racing publication is armed once or twice and never zero times --
    /// and twice is free, because demand is a latch rather than a count.
    held: AtomicBool,
    ticks: AtomicU64,
    last_timestamp_millis: AtomicU64,
    /// The same queue the session drains. The clock holds it because the tick
    /// is produced here, on the render signal, and routing it through the
    /// session would mean the session waking up to forward something it did not
    /// produce.
    downlink: Arc<Mutex<DownlinkQueue>>,
    /// Stamped into every tick. Copied rather than read from the ingress
    /// because the clock runs on the render signal and must not take the
    /// ingress lock to answer it.
    runtime_generation: u64,
    /// Where each tick's window is read from, without the ingress lock.
    window: WindowSource,
    /// Called after a tick is queued, so the transport sends it.
    ///
    /// A tick is the one record nothing on the transport's side caused -- a
    /// verdict is queued inside a submit the transport made and drains right
    /// after -- so without this a tick waits in the queue until the producer
    /// happens to send something, and a producer waiting for a tick sends
    /// nothing. Held under its lock for the call, so clearing it returns only
    /// once no call is in progress, which is what lets a host free whatever the
    /// waker points at.
    waker: Arc<WakerSlot>,
}

/// Called on the session thread whenever a record the transport did not cause
/// is queued. It must return promptly and must not call back into the session:
/// schedule the drain, do not perform it.
pub type DownlinkWaker = Box<dyn Fn() + Send + Sync>;

struct FrameClockParts {
    demand: shared::raf_signal::RafDemandRef,
    arm: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl FrameClockParts {
    fn arm(&self) {
        self.demand.mark_waiting();
        if let Some(arm) = &self.arm {
            arm();
        }
    }
}

/// What became of a request for a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameRequest {
    /// The renderer is up and one frame is armed.
    Armed,
    /// The renderer is not up yet; the request is held and armed when it is.
    Held,
}

/// What a control message asked for, once it was read in full.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControlOutcome {
    /// Requests for a frame from the current generation. They coalesce: any
    /// number of them arm one frame.
    pub frames_requested: u32,
    /// Records from another generation, ignored rather than refused: the
    /// producer that sent them is gone, and nothing it asked for is owed to its
    /// replacement.
    pub other_generation: u32,
}

impl ExternalFrameClock {
    /// A clock with a waker slot of its own, for the tests that drive one alone.
    #[cfg(test)]
    fn new(
        downlink: Arc<Mutex<DownlinkQueue>>,
        window: WindowSource,
        runtime_generation: u64,
    ) -> Self {
        Self::new_with(downlink, window, runtime_generation, Arc::default())
    }

    /// With the waker slot the session's service outbox shares, so one
    /// installed waker drains both.
    fn new_with(
        downlink: Arc<Mutex<DownlinkQueue>>,
        window: WindowSource,
        runtime_generation: u64,
        waker: Arc<WakerSlot>,
    ) -> Self {
        Self {
            inner: OnceLock::new(),
            held: AtomicBool::new(false),
            ticks: AtomicU64::new(0),
            last_timestamp_millis: AtomicU64::new(0),
            downlink,
            runtime_generation,
            window,
            waker,
        }
    }

    /// The clock as the session holds it: shared, and listening to the credit
    /// window.
    ///
    /// The listener is installed here rather than by the caller because
    /// forgetting it is not a compile error and not a test failure -- it is a
    /// session that stops drawing with its last frame unsent, once, under load.
    /// `Weak`, so the clock and the window that calls it do not own each other.
    #[cfg(any(test, feature = "test-support"))]
    fn shared(
        downlink: Arc<Mutex<DownlinkQueue>>,
        window: WindowSource,
        runtime_generation: u64,
    ) -> Arc<Self> {
        Self::shared_with(downlink, window, runtime_generation, Arc::default())
    }

    fn shared_with(
        downlink: Arc<Mutex<DownlinkQueue>>,
        window: WindowSource,
        runtime_generation: u64,
        waker: Arc<WakerSlot>,
    ) -> Arc<Self> {
        let clock = Arc::new(Self::new_with(downlink, window, runtime_generation, waker));
        let weak = Arc::downgrade(&clock);
        clock.window.on_credit_returned(Box::new(move || {
            if let Some(clock) = weak.upgrade() {
                clock.window_opened();
            }
        }));
        clock
    }

    /// A credit came back on the render thread. Tell the producer, if that is
    /// news to it.
    ///
    /// News exactly when the last window it was given was zero: it holds a
    /// packet only then, and a held packet is why it is not asking for the next
    /// frame -- the tick that would otherwise carry this. While it is drawing
    /// normally the queue's own rule makes this a load and a return.
    ///
    /// Installed on the credit window by [`Self::watch_credits`], and called
    /// from wherever the renderer finished with a frame.
    fn window_opened(&self) {
        let queued = {
            let mut downlink = self.downlink.lock();
            if downlink.last_advertised_credits() != Some(0) {
                return;
            }
            let window = self.window.read();
            if window.remaining_credits == 0 {
                // Another packet took the credit between the release and this
                // read. Nothing to tell, and the producer will hear from the
                // verdict for that packet.
                return;
            }
            downlink.push_window_open(DownlinkRecord::WindowOpen {
                generation: self.runtime_generation as u32,
                remaining_credits: window.remaining_credits,
                accepted_sequence: window.accepted_sequence,
            })
        };
        // Outside the queue lock, and only when something was queued: the waker
        // schedules a drain, and a drain takes this lock.
        if queued {
            self.waker.wake();
        }
    }

    /// Ask for one frame.
    ///
    /// Requests coalesce: one tick answers every request made before it. A
    /// request made before the renderer is up is held and armed when it is, so
    /// it is never lost; the `held` field says why that cannot race.
    pub fn request_frame(&self) -> FrameRequest {
        // `SeqCst` on both sides of the hand-off: correctness here is about the
        // order of writes to two different locations -- this flag, and the
        // `OnceLock` the session thread publishes into -- which is exactly the
        // case the weaker orderings make no promise about.
        self.held.store(true, Ordering::SeqCst);
        let Some(parts) = self.inner.get() else {
            return FrameRequest::Held;
        };
        if self.held.swap(false, Ordering::SeqCst) {
            parts.arm();
        }
        FrameRequest::Armed
    }

    /// Publish the renderer's frame demand, and arm any request that arrived
    /// before it. Called once, by the session thread, when the renderer is up.
    fn publish(&self, parts: FrameClockParts) {
        if self.inner.set(parts).is_err() {
            return;
        }
        if self.held.swap(false, Ordering::SeqCst)
            && let Some(parts) = self.inner.get()
        {
            parts.arm();
        }
    }

    /// Read one control message and act on it.
    ///
    /// Validated in full before anything is acted on, so a refused message
    /// arms nothing.
    pub fn handle_control(&self, bytes: &[u8]) -> Result<ControlOutcome, ControlError> {
        let message = read_control(bytes)?;
        // The wire carries the low 32 bits; see *Uplink control messages*.
        let current = self.runtime_generation as u32;
        let mut outcome = ControlOutcome::default();
        for record in message.records() {
            match record {
                ControlRecord::RequestFrame { generation } if generation == current => {
                    outcome.frames_requested += 1;
                }
                ControlRecord::RequestFrame { .. } => outcome.other_generation += 1,
            }
        }
        if outcome.frames_requested > 0 {
            self.request_frame();
        }
        Ok(outcome)
    }

    /// Install or clear the downlink waker.
    ///
    /// Clearing returns only after any call in progress has returned. Must not
    /// be called from inside the waker, which holds the same lock.
    pub fn set_downlink_waker(&self, waker: Option<DownlinkWaker>) {
        self.waker.set(waker);
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
        let frame_id = self.ticks.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.last_timestamp_millis
            .store(timestamp_millis as u64, Ordering::Relaxed);
        // Nanoseconds on the wire and milliseconds in the counter, deliberately:
        // the counter crosses an atomic and is read by humans, the wire value is
        // what the producer schedules against and a millisecond of rounding is a
        // sixteenth of a frame. `f64` milliseconds hold nanosecond precision for
        // the first 104 days of a session, which is longer than one runs.
        let timestamp_ns = (timestamp_millis * 1_000_000.0).max(0.0) as u64;
        {
            let mut downlink = self.downlink.lock();
            // Read under the downlink lock, so this tick's window is at least as
            // new as every verdict queued ahead of it: a verdict is queued under
            // this lock after its sequence was committed, so a verdict already in
            // the queue describes a state this read has seen. The producer
            // applies the last advertisement it reads, and this keeps the last
            // one the newest one.
            let window = self.window.read();
            downlink.push_tick(DownlinkRecord::ClockTick {
                generation: self.runtime_generation as u32,
                frame_id: frame_id as u32,
                timestamp_ns,
                remaining_credits: window.remaining_credits,
                accepted_sequence: window.accepted_sequence,
            });
        }
        // After the queue lock is released, so a transport that drains from
        // inside its wake-up does not find the lock still held by this thread.
        self.waker.wake();
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
    /// Notified whenever the admitted sequence moves; see [`Admission`].
    admitted: Arc<Condvar>,
    errors: Arc<ExternalGlErrors>,
    dispatch: Arc<OnceLock<RenderDispatch>>,
    /// Where the verdict goes. Held here rather than on the session so it can
    /// be queued while the ingress lock is still held -- see `submit_frame`.
    downlink: Arc<Mutex<DownlinkQueue>>,
    runtime_generation: u64,
    /// The session's services, for the records whose answer is the host's:
    /// an upload from an image it loaded.
    services: Option<Arc<ServiceContext>>,
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
        let mut ingress = self.ingress.lock();
        let outcome = ingress.submit_with(bytes, |frame| self.render(frame));
        // A packet that arrived ahead of its predecessor was held by ingress,
        // and this one may be the predecessor. Admitted here, under the same
        // lock, so it executes directly behind the frame that closed the gap and
        // its verdict follows that frame's.
        let released = if outcome.decision == IngressDecision::Accepted {
            ingress.admit_deferred_with(|frame| self.render(frame))
        } else {
            None
        };

        // Queued while the ingress lock is STILL HELD, and that is the whole
        // reason this lives here rather than on the session. The lock above
        // exists because concurrent callers must not dispatch N+1 before N;
        // queueing the verdict after releasing it would put the verdicts back
        // in scheduler order while the frames stayed in sequence order, so a
        // producer could read the older credit level second and send against a
        // level it had already been told was lower.
        //
        // Queued for every decision the producer acts on, including the
        // rejections: a producer told nothing about a frame it sent has to time
        // out to find out, and a timeout is indistinguishable from a host that
        // died. Not for a held packet: the credit level now does not include the
        // frame that will close the gap, and a producer told it would send
        // against a window it does not have. Its verdict is the `released` one.
        //
        // Lock order is ingress then downlink, and nothing takes them the other
        // way round -- the frame clock takes only the downlink.
        {
            let mut downlink = self.downlink.lock();
            for verdict in [Some(outcome), released]
                .into_iter()
                .flatten()
                .filter(|verdict| verdict.decision != IngressDecision::Deferred)
            {
                downlink.push_verdict(DownlinkRecord::FrameVerdict {
                    generation: self.runtime_generation as u32,
                    decision: verdict.decision as u32,
                    wire_error_code: verdict.wire_error_code,
                    remaining_credits: verdict.remaining_credits,
                    accepted_sequence: verdict.accepted_sequence,
                });
            }
        }
        drop(ingress);
        // After the lock is released, so a woken reader does not wake straight
        // into the lock this thread still holds.
        self.admitted.notify_all();
        outcome
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
            services: self.services.as_deref(),
            builder: shared::FramePacketBuilder::with_op_capacity(
                u64::from(parsed.frame_id()),
                0.0,
                budget.frame_op_capacity(),
            )
            .push(shared::FrameOp::BeginFrame),
        };
        frame_decode::decode_render_stream_into_with_plan(&mut sink, validated, budget);
        drop(scratch);
        // A barrier ends here: its commands run, its trailing 2D work is
        // materialized by the decoder, and the frame goes on -- the shape of the
        // embedded runtime's own barrier flush. Presenting it would put half a
        // frame on screen whenever content asked a question mid-frame.
        let builder = if parsed.presents() {
            sink.builder.push(shared::FrameOp::Present)
        } else {
            sink.builder
        };
        let packet = builder.finish().with_credit(frame.into_credit());

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

    /// Post one synchronous request and answer it.
    ///
    /// Called on the transport's thread, for the same reason `submit_frame` is:
    /// the producer is blocked, and a channel hop to reach the session thread
    /// would put a scheduling delay on the one path where a delay is a stall
    /// the producer can see.
    ///
    /// Returns the request id the answer will be labelled with. A request the
    /// host could not answer still gets an id -- the mailbox carries the reason
    /// and the producer reads a verdict rather than a hang. Only a request that
    /// could not be POSTED at all comes back as an error.
    pub fn post_sync_request(
        &self,
        request: SyncRequest,
        params: &[u8],
        now_nanos: u64,
    ) -> Result<SyncSnapshot, SyncError> {
        self.sync.post(request, params, now_nanos)
    }

    /// Where the outstanding request is, and any deadline that has passed.
    pub fn poll_sync(&self, now_nanos: u64) -> SyncSnapshot {
        self.sync.snapshot(now_nanos)
    }

    /// Copy a ready answer out and free the slot.
    pub fn take_sync_reply(&self, out: &mut [u8]) -> Result<usize, SyncError> {
        self.sync.take_reply(out)
    }

    /// The producer withdrew its request. Whether this call settled it.
    pub fn cancel_sync(&self) -> bool {
        self.sync.mailbox.lock().cancel()
    }

    /// The session is going away: wake any blocked producer with a reason.
    ///
    /// Public because the C boundary tears down in a defined order and this is
    /// part of it. A producer inside `Atomics.wait` on a session that has gone
    /// stays blocked until WebKit reclaims its process, which is a game that
    /// stopped drawing and never said why.
    pub fn end_sync(&self) -> bool {
        self.sync.end_session()
    }

    /// The synchronous half, detached from this handle's lifetime lock.
    ///
    /// A synchronous request blocks -- for the frame it names, then for the
    /// readback -- and the frame it waits for arrives through
    /// [`Self::submit_frame`]. A caller that held whatever lock guards this
    /// session while it waited would be holding the door that frame has to come
    /// through. The C boundary takes this handle under its lock and releases the
    /// lock before it calls.
    pub fn sync_handle(&self) -> SyncHandle {
        SyncHandle(Arc::clone(&self.sync))
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
    /// Release the transport-side frame storage once no more submissions can
    /// reach this session. The C boundary calls this before handing ownership to
    /// the retirement reaper; replacing the ingress also makes late producers
    /// fail validation instead of retaining the old pool through an Arc clone.
    pub fn release_submit_resources(&mut self) {
        self.submit.ingress = Arc::new(Mutex::new(FrameIngress::new(0, 0)));
        if let Some(dispatch) = self.submit.dispatch.get() {
            let mut words = dispatch.words.lock();
            words.clear();
            words.shrink_to_fit();
        }
    }

    /// Fill `out` with the next downlink message, and return its length.
    ///
    /// Zero means there is nothing to send, which is the normal answer between
    /// frames. The transport calls this after every submit and every tick; a
    /// message it did not ask for is a message it would have to buffer, and the
    /// queue is a better place to buffer than a socket.
    ///
    /// Whole records only: a short buffer sends fewer of them rather than a
    /// truncated one, and what does not fit stays queued in order.
    pub fn take_downlink(&self, out: &mut [u8]) -> usize {
        self.downlink.lock().drain_into(out)
    }

    /// How many downlink records were dropped for lack of room, and clear the
    /// count.
    ///
    /// Exposed because a non-zero value means the transport is not draining,
    /// which is a host-side fault the host can see and the producer cannot. It
    /// is not sent to the producer: every record is absolute, so the next one
    /// it receives is already correct.
    pub fn take_downlink_drops(&self) -> u32 {
        self.downlink.lock().take_dropped()
    }

    /// Read one uplink control message and act on it.
    ///
    /// Called on the transport's thread, like [`Self::submit_frame`]. A refusal
    /// names the rule the message broke; see *Control refusals* in the wire
    /// contract for the codes.
    pub fn submit_control(&self, bytes: &[u8]) -> Result<ControlOutcome, ControlError> {
        self.clock.handle_control(bytes)
    }

    /// Admit one service message (`MUS1`), or hold it until the one before it
    /// arrives -- the socket and a scheme request reorder. Blocks while the
    /// session's work queue is full. Must not be called from inside a Tokio
    /// runtime.
    pub fn submit_service(&self, bytes: &[u8]) -> Result<ServiceAdmission, ServiceSubmitError> {
        self.services.submit(bytes)
    }

    /// The service stream, to use without holding whatever lock guards this
    /// session; see [`ServiceHandle`].
    pub fn service_handle(&self) -> ServiceHandle {
        ServiceHandle(Arc::clone(&self.services))
    }

    /// One `MDS1` message of queued answers and events, or `None`.
    pub fn take_service_message(&self) -> Option<Vec<u8>> {
        self.services.outbox.take_message()
    }

    /// A parked answer, taken once.
    pub fn take_parked_reply(&self, generation: u32, request_id: u32) -> Option<Vec<u8>> {
        self.services.outbox.take_parked(generation, request_id)
    }

    /// Mount the content the host names -- `migo_session_load_content` on this
    /// execution -- and answer with the directory its code is served from.
    ///
    /// Synchronous, unlike the embedded execution's, because what the embedded
    /// execution does next -- evaluate the entry module -- happens in another
    /// process here, and that process's host needs this directory before it can
    /// serve the module at all.
    pub fn load_content(&self, game_id: &str) -> EngineResult<std::path::PathBuf> {
        self.services.context.load_content(game_id)
    }

    /// Where the loaded content's code is, or `None` before any is loaded.
    pub fn content_root(&self) -> Option<std::path::PathBuf> {
        self.services.context.content_root()
    }

    /// Install or clear what is called when a tick is queued. See
    /// [`ExternalFrameClock::set_downlink_waker`].
    pub fn set_downlink_waker(&self, waker: Option<DownlinkWaker>) {
        self.clock.set_downlink_waker(waker);
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
        // Before the thread is asked to stop, not after it has: a producer
        // inside `Atomics.wait` is woken by the mailbox being settled, and a
        // session that goes away without settling it leaves that agent blocked
        // until WebKit reclaims its process. Which is a game that stopped
        // drawing and never said why.
        self.end_sync();
        self.services.end();
        self.host.request_shutdown()
    }

    pub fn join(&mut self) -> EngineResult<()> {
        self.host.join()
    }

    pub fn shutdown_and_join(&mut self) -> EngineResult<()> {
        // Both entry points, because either may be the one a host calls, and
        // waking the producer is not something to do only on the path somebody
        // happened to test.
        self.end_sync();
        self.services.end();
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
        // One `OnceLock`, shared, exactly as the real spawn shares it: a sync
        // path with a dispatch of its own would report "the renderer is not up"
        // on a session whose renderer is, and the difference would only show on
        // whichever test reached for a synchronous call first.
        let dispatch: Arc<OnceLock<RenderDispatch>> = Arc::new(OnceLock::new());
        let downlink = Arc::new(Mutex::new(DownlinkQueue::new()));
        let admission = Admission::new(Arc::new(Mutex::new(FrameIngress::new(
            launch_nonce,
            INITIAL_RUNTIME_GENERATION,
        ))));
        let window = admission.ingress.lock().window_source();
        let errors = Arc::new(ExternalGlErrors::default());
        Self {
            host: HostThread::from_join_handle_for_test(host_id, join),
            submit: SubmitPath {
                ingress: Arc::clone(&admission.ingress),
                admitted: Arc::clone(&admission.admitted),
                errors: Arc::clone(&errors),
                dispatch: Arc::clone(&dispatch),
                downlink: Arc::clone(&downlink),
                runtime_generation: INITIAL_RUNTIME_GENERATION,
                services: None,
            },
            sync: Arc::new(SyncPath::new(
                INITIAL_RUNTIME_GENERATION,
                dispatch,
                admission,
                errors,
            )),
            clock: ExternalFrameClock::shared(
                Arc::clone(&downlink),
                window,
                INITIAL_RUNTIME_GENERATION,
            ),
            services: ServiceHost::new(
                INITIAL_RUNTIME_GENERATION,
                std::path::PathBuf::new(),
                std::path::PathBuf::new(),
                Arc::default(),
            )
            .0,
            downlink,
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
    let admission = Admission::new(Arc::clone(&ingress));
    let downlink = Arc::new(Mutex::new(DownlinkQueue::new()));
    // One slot for the one drain: frame records and service answers both wake it.
    let waker = Arc::new(WakerSlot::default());
    let clock = ExternalFrameClock::shared_with(
        Arc::clone(&downlink),
        ingress.lock().window_source(),
        INITIAL_RUNTIME_GENERATION,
        Arc::clone(&waker),
    );
    let thread_clock = Arc::clone(&clock);
    let (services, service_work) = ServiceHost::new(
        INITIAL_RUNTIME_GENERATION,
        opt.files_dir().to_path_buf(),
        opt.cache_dir().to_path_buf(),
        waker,
    );
    let thread_services = Arc::clone(&services);
    let errors = Arc::new(ExternalGlErrors::default());
    let dispatch: Arc<OnceLock<RenderDispatch>> = Arc::new(OnceLock::new());
    let thread_dispatch = Arc::clone(&dispatch);

    let started: StartedHost = spawn_session_thread(
        surface,
        graphics_platform,
        platform,
        opt,
        public_generation,
        move |ctx| {
            run_external_session(
                ctx,
                thread_ingress,
                thread_clock,
                thread_dispatch,
                thread_services,
                service_work,
            )
        },
    )?;
    // Before the session is handed out, so nothing that can reach the services
    // -- only the session handle can -- finds them without their scheduler.
    services.context.bind_session(started.host.id());

    Ok(SpawnedExternalSession {
        session: ExternalFrameSession {
            host: started.host,
            sync: Arc::new(
                SyncPath::new(
                    INITIAL_RUNTIME_GENERATION,
                    Arc::clone(&dispatch),
                    admission.clone(),
                    Arc::clone(&errors),
                )
                .with_services(Arc::clone(&services)),
            ),
            submit: SubmitPath {
                ingress,
                admitted: Arc::clone(&admission.admitted),
                errors,
                dispatch,
                downlink: Arc::clone(&downlink),
                runtime_generation: INITIAL_RUNTIME_GENERATION,
                services: Some(Arc::clone(&services.context)),
            },
            clock,
            downlink,
            services,
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
    services: Arc<ServiceHost>,
    mut service_work: tokio::sync::mpsc::Receiver<ServiceWork>,
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
        // Every packet ends a frame, and the producer's read of it arrives through
        // the synchronous barrier afterwards -- possibly after it presented.
        graphics::DefaultFramebufferReads::AfterTheirPresent,
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
        // with it -- and the policy is also what a streamed audio source is
        // held to, as it is in the embedded execution.
        network_policy,
        gpu_caps,
        context_lost: _context_lost,
        timer_backgrounded: _timer_backgrounded,
        gpu_init_started: _gpu_init_started,
        // Why the render worker stopped, read at the one place this session
        // observes it stopping. `gpu_caps` cannot answer it: a panic after the
        // first frame leaves that level saying Ready.
        render_exit,
        t_start: _t_start,
    } = shell;

    // Publish the clock only once the renderer is up. A request made before
    // this point was held, and is armed here.
    clock.publish(FrameClockParts {
        demand: Arc::clone(&raf_demand),
        arm: request_vsync.clone(),
    });
    // Published together with the clock, and only now: a transport that
    // submitted before the renderer existed would be told the renderer is not
    // ready, which is the truthful answer.
    let lifecycle_sender = Arc::new(render.sender());
    // Audio plays here, driven by the service stream: its commands go through
    // the audio service's own sender, and buffer ids are scoped to the one
    // runtime generation this session has. Bound before the first service
    // work can be dispatched, which is below.
    services.context.bind_audio(
        audio.sender(),
        restart_boundary.current(),
        network_policy.clone(),
    );
    let audio_signal = audio.start_signal();
    // The services' context, kept before the dispatcher shadows `services`:
    // the network is bound to it once the runtime exists, below.
    let service_context = Arc::clone(&services.context);
    // What `exitMiniProgram` and `restartMiniProgram` send, which is this
    // session's own command channel -- the one the embedded ops send on.
    service_context.bind_lifecycle(host_tx.clone());
    // The services that hand the renderer work -- image uploads -- reach it
    // through these, owned by this thread's dispatcher so the sender goes when
    // the session does.
    let services = ServiceDispatcher::new(
        &services,
        RenderHandles {
            canvas: shared::op_state::CanvasOpState::for_host(render.sender(), id),
            gpu_caps: Arc::clone(&gpu_caps),
        },
    );
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
    // Content's requests are made by this host, under the same policy: one
    // session, one allow list, one set of clients. Bound once the runtime
    // exists, because a synchronous request is built on the calling thread and
    // needs the session's reactor to build it -- and before any service work
    // can be dispatched, which is below.
    service_context.bind_network(
        network_policy.clone(),
        Arc::clone(&backgrounded),
        runtime.handle().clone(),
    );

    let mut last_context_epoch = 0u64;
    let mut last_swap_report: Option<std::time::Instant> = None;
    // What content has been told is held down, so losing focus can release it.
    let mut input = InputState::default();
    // What content has been told about being shown and hidden.
    let mut lifecycle = Lifecycle::default();
    runtime.block_on(async move {
        loop {
            tokio::select! {
                command = host_rx.recv() => {
                    let Some(command) = command else {
                        info!("[Host {id}] command channel closed");
                        break;
                    };
                    // A callback for a runtime that has since been replaced is
                    // dropped before it reaches content, as in the embedded
                    // execution; then input goes to the producer, by the routing
                    // both executions share.
                    if is_retired_callback(&command, restart_boundary.current()) {
                        debug!("[Host {id}] dropping a retired generation's callback");
                        continue;
                    }
                    let mut sink = ServiceEventSink { outbox: services.outbox() };
                    let Some(command) = input_route::route(&mut input, &mut sink, command) else {
                        continue;
                    };
                    let Some(command) = route_audio_event(&sink, command) else {
                        continue;
                    };
                    if !handle_command(
                        id, command, &mut render, &mut audio, &backgrounded, &ingress,
                        &platform_for_error, &sink, &mut lifecycle,
                    ) {
                        break;
                    }
                }
                // Admitted service work, in admission order. The branch is
                // disabled once every sender is gone, which is the session's
                // service host being dropped -- teardown, not an error.
                Some(work) = service_work.recv() => {
                    services.dispatch(work);
                }
                // Content's first audio command: start the audio thread it is
                // waiting in the queue for. The signal disables itself once the
                // thread is installed, so this stops firing.
                () = audio_signal.notified() => {
                    if let Err(error) = audio.check_and_start() {
                        // Not fatal: the game runs without sound, as it does
                        // in process when the device will not open.
                        error!("[Host {id}] failed to start the audio thread: {error}");
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

/// What the host's lifecycle tells content, and when.
///
/// The hooks are the embedded execution's, in its order and with its arguments
/// (`Host::handle_command`, `enter_foreground`): `onHide` as the app goes away,
/// `onShow` when it comes back -- but only once there is a surface to come back
/// to, because a host that shows before its surface exists (Android's
/// `onResume` before `surfaceCreated`) would otherwise tell content it is
/// visible while nothing can be presented. A foreground with a live surface
/// also restarts the frame loop content stopped while hidden and re-measures
/// the window, which is what the embedded execution does there.
#[derive(Default)]
struct Lifecycle {
    /// `onShow`'s arguments, waiting for a surface.
    pending_show: Option<String>,
}

impl Lifecycle {
    /// The host says the app is visible. The arguments are its options object,
    /// as JSON, in the array `_internalDispatch` applies -- the embedded
    /// execution's `build_on_show_args`, including its fallbacks.
    fn show(&mut self, options_json: Option<&str>) {
        self.pending_show = Some(on_show_args(options_json));
    }

    /// A surface is live and the app is not hidden.
    fn entered_foreground(&mut self, sink: &ServiceEventSink<'_>) {
        // The frame loop self-stops after a few idle frames while hidden, and
        // the window may have changed size behind the app's back.
        sink.host_hook("_internalRestartRafLoop", HOOK_ARGS_NONE);
        sink.host_hook("_internalTriggerWindowResize", HOOK_ARGS_NONE);
        if let Some(args) = self.pending_show.take() {
            sink.host_hook("_internalTriggerOnShow", &args);
        }
    }

    fn hide(&mut self, sink: &ServiceEventSink<'_>) {
        // A show that never reached content is not delivered late, after the
        // hide that overtook it.
        self.pending_show = None;
        sink.host_hook("_internalTriggerOnHide", HOOK_ARGS_NONE);
    }
}

/// `onShow`'s options as the hook takes them: one argument, or none when the
/// host named none and when what it named is not an object -- the embedded
/// execution's rule, so a host's malformed options are ignored the same way
/// rather than passed to content on one lane only.
fn on_show_args(options_json: Option<&str>) -> String {
    let Some(options_json) = options_json.map(str::trim).filter(|json| !json.is_empty()) else {
        return HOOK_ARGS_NONE.to_string();
    };
    match serde_json::from_str::<serde_json::Value>(options_json) {
        Ok(value) if value.is_object() => hook_args_one(value).into_owned(),
        Ok(_) => HOOK_ARGS_NONE.to_string(),
        Err(error) => {
            warn!("an onShow options JSON the host sent is not JSON: {error}");
            HOOK_ARGS_NONE.to_string()
        }
    }
}

/// The audio thread's events and the host's audio interruptions, delivered to
/// content as the embedded runtime delivers them: an InnerAudioContext event
/// to its enqueue hook, an interruption to the engine's interruption hooks
/// through the bridge's dispatch entry point. Neither pauses anything here --
/// in process an interruption is the game's to act on (`03_audio_interruption.js`),
/// and a lane that paused natively besides would play the same game
/// differently. Anything else is returned for [`handle_command`].
fn route_audio_event(sink: &ServiceEventSink<'_>, command: HostCommand) -> Option<HostCommand> {
    match command {
        HostCommand::InnerAudioEvent {
            id,
            event_type,
            current_time,
        } => sink.inner_audio_event(id, event_type.as_str(), current_time),
        HostCommand::OnAudioInterruptionBegin => {
            sink.host_hook("_internalTriggerAudioInterruptionBegin", HOOK_ARGS_NONE)
        }
        HostCommand::OnAudioInterruptionEnd => {
            sink.host_hook("_internalTriggerAudioInterruptionEnd", HOOK_ARGS_NONE)
        }
        other => return Some(other),
    }
    None
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
    sink: &ServiceEventSink<'_>,
    lifecycle: &mut Lifecycle,
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
                lifecycle.entered_foreground(sink);
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

        HostCommand::OnShow { options_json } => {
            backgrounded.store(false, Ordering::Relaxed);
            lifecycle.show(options_json.as_deref());
            // Only resume against a surface that is actually live. Android
            // fires `onResume` before `surfaceCreated`, so on that path the old
            // surface is already gone and the resume belongs to the
            // `UpdateSurface` that follows; resuming here would run a renderer
            // with nothing to present into -- and content is told it is shown
            // when that surface arrives, not before.
            if render.has_live_surface() {
                render.resume();
                audio.resume();
                lifecycle.entered_foreground(sink);
            } else {
                debug!("[Host {id}] OnShow with no live surface; resume waits for UpdateSurface");
            }
        }

        HostCommand::OnHide => {
            backgrounded.store(true, Ordering::Relaxed);
            render.pause();
            audio.pause();
            lifecycle.hide(sink);
        }

        HostCommand::OnUserCaptureScreen { .. } => {
            sink.host_hook("_internalTriggerUserCaptureScreen", HOOK_ARGS_NONE);
        }

        // Input was routed to the producer before this match. What is left is
        // addressed to a capability this lane does not carry yet -- sensors,
        // camera, Bluetooth -- and is logged rather than silently dropped: a
        // host sending it is a host expecting something to happen.
        other => {
            debug!("[Host {id}] {other:?} has no consumer in an external-frame session");
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

    /// The lifecycle content is told about, in the calls the embedded
    /// execution makes: the hooks, their order, and the arguments.
    #[test]
    fn a_show_reaches_content_when_its_surface_does_and_a_hide_cancels_it() {
        let (host, _work) = ServiceHost::new(
            1,
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
            Arc::new(WakerSlot::default()),
        );
        let sink = ServiceEventSink {
            outbox: &host.outbox,
        };
        let hooks = |sink: &ServiceEventSink<'_>| {
            let mut called = Vec::new();
            while let Some(message) = sink.outbox.take_message() {
                let (_, records) =
                    frame_wire::service::read_down_message(&message).expect("a down message");
                for record in records {
                    let frame_wire::service::ServiceDownRecord::Event { event, values } = record
                    else {
                        panic!("a lifecycle hook is an event");
                    };
                    assert_eq!(event, crate::runtime::host_events::event::_internalDispatch);
                    let [
                        frame_wire::value::OwnedValue::Str(hook),
                        frame_wire::value::OwnedValue::Str(args),
                    ] = values.as_slice()
                    else {
                        panic!("a hook call is its name and its arguments");
                    };
                    called.push((hook.clone(), args.clone()));
                }
            }
            called
        };

        let mut lifecycle = Lifecycle::default();
        // Shown before there is a surface: content hears nothing yet, as it
        // hears nothing in the embedded execution until the surface arrives.
        lifecycle.show(Some(r#"{"scene": 1001}"#));
        assert!(hooks(&sink).is_empty());

        lifecycle.entered_foreground(&sink);
        assert_eq!(
            hooks(&sink),
            vec![
                ("_internalRestartRafLoop".to_string(), "[]".to_string()),
                ("_internalTriggerWindowResize".to_string(), "[]".to_string()),
                (
                    "_internalTriggerOnShow".to_string(),
                    r#"[{"scene":1001}]"#.to_string()
                ),
            ]
        );

        // A second foreground without a show does not repeat it.
        lifecycle.entered_foreground(&sink);
        assert_eq!(
            hooks(&sink),
            vec![
                ("_internalRestartRafLoop".to_string(), "[]".to_string()),
                ("_internalTriggerWindowResize".to_string(), "[]".to_string()),
            ]
        );

        lifecycle.hide(&sink);
        assert_eq!(
            hooks(&sink),
            vec![("_internalTriggerOnHide".to_string(), "[]".to_string())]
        );

        // A show the surface never arrived for is dropped by the hide that
        // overtook it, rather than delivered late.
        lifecycle.show(None);
        lifecycle.hide(&sink);
        lifecycle.entered_foreground(&sink);
        assert_eq!(
            hooks(&sink),
            vec![
                ("_internalTriggerOnHide".to_string(), "[]".to_string()),
                ("_internalRestartRafLoop".to_string(), "[]".to_string()),
                ("_internalTriggerWindowResize".to_string(), "[]".to_string()),
            ]
        );
    }

    /// The embedded execution's rule for a host's `onShow` options, including
    /// what it does with options that are not an object.
    #[test]
    fn on_show_options_are_one_argument_or_none() {
        assert_eq!(on_show_args(None), "[]");
        assert_eq!(on_show_args(Some("   ")), "[]");
        assert_eq!(on_show_args(Some("[1]")), "[]");
        assert_eq!(on_show_args(Some("not json")), "[]");
        assert_eq!(on_show_args(Some(r#"{"a":1}"#)), r#"[{"a":1}]"#);
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

    /// A clock over a fresh ingress, and the queue it fills.
    fn clock_with_queue() -> (ExternalFrameClock, Arc<Mutex<DownlinkQueue>>, FrameIngress) {
        let queue = Arc::new(Mutex::new(DownlinkQueue::new()));
        let ingress = FrameIngress::new(NONCE, INITIAL_RUNTIME_GENERATION);
        let clock = ExternalFrameClock::new(
            Arc::clone(&queue),
            ingress.window_source(),
            INITIAL_RUNTIME_GENERATION,
        );
        (clock, queue, ingress)
    }

    /// Parts whose arm is counted, so a test can see a frame being armed.
    fn counted_parts() -> (
        FrameClockParts,
        shared::raf_signal::RafDemandRef,
        Arc<AtomicU64>,
    ) {
        let demand = Arc::new(shared::raf_signal::RafDemand::new());
        let armed = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&armed);
        (
            FrameClockParts {
                demand: Arc::clone(&demand),
                arm: Some(Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                })),
            },
            demand,
            armed,
        )
    }

    fn drain_records(queue: &Mutex<DownlinkQueue>) -> Vec<DownlinkRecord> {
        let mut out = [0u8; 4096];
        let written = queue.lock().drain_into(&mut out);
        frame_wire::downlink::decode_bytes(&out[..written])
            .expect("the queue writes what the producer reads")
    }

    /// A producer's first request races the host's bring-up. Dropped, it is a
    /// producer waiting for a tick nothing will send; so it is held, and armed
    /// the moment the renderer is.
    #[test]
    fn a_request_before_the_renderer_is_up_is_held_and_armed_when_it_starts() {
        let (clock, _queue, _ingress) = clock_with_queue();
        assert_eq!(clock.request_frame(), FrameRequest::Held);
        assert_eq!(clock.ticks(), 0);

        let (parts, demand, armed) = counted_parts();
        clock.publish(parts);
        assert!(demand.is_waiting(), "the held request became demand");
        assert_eq!(armed.load(Ordering::SeqCst), 1, "and armed one frame");

        assert_eq!(clock.request_frame(), FrameRequest::Armed);
        assert_eq!(
            armed.load(Ordering::SeqCst),
            2,
            "a later request arms directly"
        );
    }

    #[test]
    fn publishing_with_nothing_held_arms_nothing() {
        let (clock, _queue, _ingress) = clock_with_queue();
        let (parts, demand, armed) = counted_parts();
        clock.publish(parts);
        assert!(!demand.is_waiting());
        assert_eq!(armed.load(Ordering::SeqCst), 0);
    }

    /// The hand-off's claim, under real scheduling: whichever of a request and
    /// the renderer's publication runs second sees the other, so the request is
    /// armed at least once.
    #[test]
    fn a_request_racing_the_renderer_start_is_never_lost() {
        for _ in 0..2_000 {
            let (clock, _queue, _ingress) = clock_with_queue();
            let clock = Arc::new(clock);
            let (parts, demand, armed) = counted_parts();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let requester = {
                let clock = Arc::clone(&clock);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    clock.request_frame()
                })
            };
            barrier.wait();
            clock.publish(parts);
            requester.join().expect("requester");
            assert!(demand.is_waiting(), "a request was lost to the race");
            assert!(armed.load(Ordering::SeqCst) >= 1, "and nothing was armed");
        }
    }

    #[test]
    fn recorded_ticks_accumulate_and_keep_the_latest_timestamp() {
        let (clock, _queue, _ingress) = clock_with_queue();
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

    /// Every decision is answered, including the ones that are not "accepted".
    ///
    /// A producer told nothing about a frame it sent has to time out to find
    /// out, and a timeout is indistinguishable from a host that died. This one
    /// is refused -- the renderer is not up -- which is the case most likely to
    /// be forgotten, because nothing rendered and it is tempting to treat that
    /// as nothing to report.
    #[test]
    fn a_refused_frame_is_still_answered_on_the_downlink() {
        let downlink = Arc::new(Mutex::new(DownlinkQueue::new()));
        let submit = SubmitPath {
            ingress: Arc::new(Mutex::new(FrameIngress::new(
                NONCE,
                INITIAL_RUNTIME_GENERATION,
            ))),
            admitted: Arc::new(Condvar::new()),
            errors: Arc::new(ExternalGlErrors::default()),
            dispatch: Arc::new(OnceLock::new()),
            downlink: Arc::clone(&downlink),
            runtime_generation: INITIAL_RUNTIME_GENERATION,
            services: None,
        };

        let outcome = submit.submit_frame(&packet(1));
        assert_ne!(
            outcome.decision,
            IngressDecision::Accepted,
            "this fixture has no renderer, so the point of the test is the refusal"
        );

        let mut out = [0u8; 256];
        let written = downlink.lock().drain_into(&mut out);
        let records = frame_wire::downlink::decode_bytes(&out[..written])
            .expect("the queue writes what the producer reads");
        assert_eq!(records.len(), 1, "one verdict for one frame");
        match records[0] {
            DownlinkRecord::FrameVerdict {
                decision,
                remaining_credits,
                ..
            } => {
                assert_eq!(
                    decision, outcome.decision as u32,
                    "the wire carries the decision"
                );
                assert_eq!(
                    remaining_credits, outcome.remaining_credits,
                    "and the level the producer schedules against"
                );
            }
            other => panic!("expected a verdict, got {other:?}"),
        }
    }

    /// The tick the producer actually reads, not the counter a human does.
    ///
    /// `ticks()` and `last_timestamp_millis()` are diagnostics; what schedules
    /// the next frame in another process is the record queued here. They are
    /// fed by the same call and could drift without either one noticing, which
    /// is why this asserts the queued bytes rather than the counters.
    #[test]
    fn a_recorded_tick_is_queued_for_the_producer_in_nanoseconds() {
        let (clock, queue, _ingress) = clock_with_queue();
        clock.record(16.7);

        let records = drain_records(&queue);
        assert_eq!(records.len(), 1, "one record for one tick");
        match records[0] {
            DownlinkRecord::ClockTick {
                generation,
                frame_id,
                timestamp_ns,
                ..
            } => {
                assert_eq!(generation, INITIAL_RUNTIME_GENERATION as u32);
                assert_eq!(frame_id, 1, "the first tick is frame 1, not frame 0");
                assert_eq!(
                    timestamp_ns, 16_700_000,
                    "milliseconds go out as nanoseconds; the counter rounds and this must not"
                );
            }
            other => panic!("expected a tick, got {other:?}"),
        }
    }

    /// Ticks coalesce, and that has to hold through the clock rather than only
    /// in the queue: a renderer that ticks faster than the transport drains
    /// must not grow the queue.
    #[test]
    fn a_producer_that_never_reads_does_not_grow_the_queue() {
        let (clock, queue, _ingress) = clock_with_queue();
        for i in 0..1_000 {
            clock.record(f64::from(i) * 16.7);
        }
        assert_eq!(
            queue.lock().len(),
            1,
            "a thousand ticks are one queued tick"
        );
        assert_eq!(
            queue.lock().dropped(),
            0,
            "coalescing is not dropping: nothing was lost that the newest tick does not carry"
        );
    }

    /// The deadlock this exists to break.
    ///
    /// A frame whose last packet the window would not admit is held by the
    /// producer; the producer's next frame request waits for that packet to go;
    /// the tick that would carry the returned credit waits for that request. So
    /// a credit coming back has to reach a producer that is asking for nothing,
    /// and this is the record that does it.
    #[test]
    fn a_credit_that_comes_back_reaches_a_producer_that_asked_for_nothing() {
        let queue = Arc::new(Mutex::new(DownlinkQueue::new()));
        let ingress = Arc::new(Mutex::new(FrameIngress::new(
            NONCE,
            INITIAL_RUNTIME_GENERATION,
        )));
        let clock = ExternalFrameClock::shared(
            Arc::clone(&queue),
            ingress.lock().window_source(),
            INITIAL_RUNTIME_GENERATION,
        );
        // Kept alive: the window holds only a `Weak` to it.
        let _clock = Arc::clone(&clock);

        // Fill the window and tell the producer it is shut, which is the state a
        // producer is in when it holds a packet it could not send.
        let mut frames = Vec::new();
        let capacity = ingress.lock().credits().max() as u64;
        for sequence in 1..=capacity {
            let (outcome, frame) = ingress.lock().submit(&packet(sequence));
            assert_eq!(outcome.decision, IngressDecision::Accepted);
            frames.push(frame);
        }
        queue.lock().push_verdict(DownlinkRecord::FrameVerdict {
            generation: INITIAL_RUNTIME_GENERATION as u32,
            decision: 0,
            wire_error_code: 0,
            remaining_credits: 0,
            accepted_sequence: capacity,
        });
        drain_records(&queue);

        // The renderer finishes with one frame. No tick, no verdict, nothing the
        // producer did -- and it still hears about it.
        drop(frames.pop().expect("a frame in flight"));
        assert!(
            matches!(
                drain_records(&queue)[..],
                [DownlinkRecord::WindowOpen {
                    remaining_credits: 1,
                    ..
                }]
            ),
            "a returned credit is advertised when the producer was last told zero"
        );

        // And a second return says nothing: the producer already knows the
        // window is open, and a message per returned credit is what this avoids.
        drop(frames.pop().expect("another frame in flight"));
        assert_eq!(
            queue.lock().len(),
            0,
            "the advertisement is sent when it is news, not on every credit"
        );
    }

    /// The tick is how a waiting producer hears that a credit came back, so it
    /// carries the window -- read from the ingress without its lock.
    #[test]
    fn a_tick_carries_the_window_as_of_the_moment_it_was_queued() {
        let (clock, queue, mut ingress) = clock_with_queue();

        clock.record(1.0);
        assert!(
            matches!(
                drain_records(&queue)[..],
                [DownlinkRecord::ClockTick {
                    remaining_credits: 1,
                    accepted_sequence: 0,
                    ..
                }]
            ),
            "before the first packet, at most one: sequences 1 and 2 sent together can reorder"
        );

        let (outcome, frame) = ingress.submit(&packet(1));
        assert_eq!(outcome.decision, IngressDecision::Accepted);
        clock.record(2.0);
        assert!(
            matches!(
                drain_records(&queue)[..],
                [DownlinkRecord::ClockTick {
                    remaining_credits: 1,
                    accepted_sequence: 1,
                    ..
                }]
            ),
            "the accepted frame still holds its credit"
        );

        drop(frame);
        clock.record(3.0);
        assert!(
            matches!(
                drain_records(&queue)[..],
                [DownlinkRecord::ClockTick {
                    remaining_credits: 2,
                    accepted_sequence: 1,
                    frame_id: 3,
                    ..
                }]
            ),
            "and the credit it returned is advertised on the next tick"
        );
    }

    #[test]
    fn a_request_from_this_generation_arms_a_frame_and_one_from_another_does_not() {
        use frame_wire::control::{ControlRecord, encode_control};
        let (clock, _queue, _ingress) = clock_with_queue();
        let (parts, demand, armed) = counted_parts();
        clock.publish(parts);

        let stale = encode_control(&[ControlRecord::RequestFrame {
            generation: INITIAL_RUNTIME_GENERATION as u32 + 1,
        }]);
        assert_eq!(
            clock.handle_control(&stale),
            Ok(ControlOutcome {
                frames_requested: 0,
                other_generation: 1,
            })
        );
        assert!(
            !demand.is_waiting(),
            "nothing is owed to a generation that is gone"
        );
        assert_eq!(armed.load(Ordering::SeqCst), 0);

        let current = encode_control(&[
            ControlRecord::RequestFrame {
                generation: INITIAL_RUNTIME_GENERATION as u32,
            },
            ControlRecord::RequestFrame {
                generation: INITIAL_RUNTIME_GENERATION as u32,
            },
        ]);
        assert_eq!(
            clock.handle_control(&current),
            Ok(ControlOutcome {
                frames_requested: 2,
                other_generation: 0,
            })
        );
        assert!(demand.is_waiting());
        assert_eq!(
            armed.load(Ordering::SeqCst),
            1,
            "requests coalesce into one frame"
        );
    }

    #[test]
    fn a_refused_control_message_arms_nothing_and_holds_nothing() {
        use frame_wire::control::{ControlRecord, encode_control};
        let (clock, _queue, _ingress) = clock_with_queue();
        let mut bytes = encode_control(&[ControlRecord::RequestFrame {
            generation: INITIAL_RUNTIME_GENERATION as u32,
        }]);
        bytes.push(0);
        assert_eq!(
            clock.handle_control(&bytes),
            Err(ControlError::TrailingBytes)
        );
        let (parts, demand, armed) = counted_parts();
        clock.publish(parts);
        assert!(
            !demand.is_waiting(),
            "a refused request must not be held and armed later"
        );
        assert_eq!(armed.load(Ordering::SeqCst), 0);
    }

    /// A tick is the one record nothing on the transport's side caused, so the
    /// transport is told when one is queued -- and not after its waker is gone.
    #[test]
    fn a_queued_tick_wakes_the_transport_until_the_waker_is_cleared() {
        let (clock, _queue, _ingress) = clock_with_queue();
        let woken = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&woken);
        clock.set_downlink_waker(Some(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })));
        clock.record(1.0);
        clock.record(2.0);
        assert_eq!(woken.load(Ordering::SeqCst), 2, "one wake per queued tick");

        clock.set_downlink_waker(None);
        clock.record(3.0);
        assert_eq!(
            woken.load(Ordering::SeqCst),
            2,
            "a cleared waker is not called"
        );
    }

    /// The waker may drain the queue from inside the call; the tick must already
    /// be there, and the queue's lock must already be free.
    #[test]
    fn a_waker_that_drains_at_once_finds_the_tick() {
        let (clock, queue, _ingress) = clock_with_queue();
        let clock = Arc::new(clock);
        let seen = Arc::new(AtomicU64::new(0));
        {
            let queue = Arc::clone(&queue);
            let seen = Arc::clone(&seen);
            clock.set_downlink_waker(Some(Box::new(move || {
                seen.fetch_add(drain_records(&queue).len() as u64, Ordering::SeqCst);
            })));
        }
        clock.record(1.0);
        assert_eq!(seen.load(Ordering::SeqCst), 1);
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
            admitted: Arc::new(Condvar::new()),
            errors: Arc::new(ExternalGlErrors::default()),
            dispatch: Arc::new(OnceLock::new()),
            downlink: Arc::new(Mutex::new(DownlinkQueue::new())),
            runtime_generation: INITIAL_RUNTIME_GENERATION,
            services: None,
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
                admitted: Arc::new(Condvar::new()),
                errors: Arc::new(ExternalGlErrors::default()),
                dispatch: Arc::new(dispatch),
                downlink: Arc::new(Mutex::new(DownlinkQueue::new())),
                runtime_generation: INITIAL_RUNTIME_GENERATION,
                services: None,
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

    /// A packet that overtook its predecessor on the other uplink is executed
    /// after it, and the producer hears about the two in that order.
    ///
    /// Both halves are asserted on what leaves the session: the renderer's queue
    /// and the downlink. A held packet sends no verdict -- the credit level at
    /// that moment does not yet include the frame that will close the gap, and a
    /// producer told it would send against a window it does not have.
    #[test]
    fn a_frame_that_overtakes_its_predecessor_runs_after_it_and_is_answered_after_it() {
        let (submit, receiver, _lifecycle_sender) = ready_submit();
        // The frame id is the sequence, so what the renderer received can be read
        // back as an order.
        let frame = |sequence: u64| {
            let words: Vec<u8> = [stream::MAGIC, stream::STREAM_VERSION]
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect();
            let mut frame = frame_wire::builder::WireFrameBuilder::new();
            frame.launch_nonce = NONCE;
            frame.sequence = sequence;
            frame.frame_id = sequence as u32;
            frame
                .section(frame_wire::SECTION_KIND_COMMAND_STREAM, 2, &words)
                .build()
        };
        let mut out = [0u8; 256];
        let mut verdicts = |submit: &SubmitPath| {
            let written = submit.downlink.lock().drain_into(&mut out);
            if written == 0 {
                return Vec::new();
            }
            frame_wire::downlink::decode_bytes(&out[..written])
                .expect("the queue writes what the producer reads")
                .into_iter()
                .map(|record| match record {
                    DownlinkRecord::FrameVerdict {
                        decision,
                        accepted_sequence,
                        ..
                    } => (decision, accepted_sequence),
                    other => panic!("expected a verdict, got {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        let executed = |receiver: &crossbeam_channel::Receiver<
            shared::protocol::render_cmd::RenderCommand,
        >| {
            let mut ids = Vec::new();
            while let Ok(command) = receiver.try_recv() {
                if let shared::protocol::render_cmd::RenderCommand::FramePacket(packet) = command {
                    ids.push(packet.frame_id());
                }
            }
            ids
        };
        let accepted = IngressDecision::Accepted as u32;

        assert_eq!(
            submit.submit_frame(&frame(1)).decision,
            IngressDecision::Accepted
        );
        assert_eq!(verdicts(&submit), [(accepted, 1)]);
        assert_eq!(executed(&receiver), [1]);

        assert_eq!(
            submit.submit_frame(&frame(3)).decision,
            IngressDecision::Deferred
        );
        assert_eq!(verdicts(&submit), [], "a held packet was answered");
        assert_eq!(
            executed(&receiver),
            Vec::<u64>::new(),
            "a held packet executed early"
        );

        assert_eq!(
            submit.submit_frame(&frame(2)).decision,
            IngressDecision::Accepted
        );
        assert_eq!(verdicts(&submit), [(accepted, 2), (accepted, 3)]);
        assert_eq!(executed(&receiver), [2, 3]);
        assert_eq!(submit.ingress.lock().last_accepted_sequence(), 3);
    }

    /// A barrier runs its commands and does not end the frame; the packet after
    /// it that does is the one that presents.
    ///
    /// Asserted on what reaches the renderer, which is the only place the
    /// difference exists: both packets are admitted, credited and answered the
    /// same way.
    #[test]
    fn a_barrier_executes_without_presenting_and_the_packet_that_ends_the_frame_presents() {
        let (submit, receiver, _lifecycle_sender) = ready_submit();
        let packet = |sequence: u64, flags: u32| {
            let words: Vec<u8> = [
                stream::MAGIC,
                stream::STREAM_VERSION,
                (3 << 12) | frame_wire::gl::OP_CLEAR,
                1,
                0x4000,
            ]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
            let mut frame = frame_wire::builder::WireFrameBuilder::new();
            frame.launch_nonce = NONCE;
            frame.sequence = sequence;
            frame.flags = flags;
            frame
                .section(frame_wire::SECTION_KIND_COMMAND_STREAM, 5, &words)
                .build()
        };
        let received = |receiver: &crossbeam_channel::Receiver<
            shared::protocol::render_cmd::RenderCommand,
        >| {
            let Ok(shared::protocol::render_cmd::RenderCommand::FramePacket(packet)) =
                receiver.try_recv()
            else {
                panic!("the renderer was not handed a frame packet");
            };
            let presents = packet
                .ops()
                .iter()
                .any(|op| matches!(op, shared::FrameOp::Present));
            let draws = packet
                .ops()
                .iter()
                .any(|op| matches!(op, shared::FrameOp::GlBatch(_)));
            (presents, draws)
        };

        assert_eq!(
            submit.submit_frame(&packet(1, 0)).decision,
            IngressDecision::Accepted
        );
        assert_eq!(
            received(&receiver),
            (false, true),
            "a barrier's commands run and it does not present"
        );
        assert_eq!(
            submit
                .submit_frame(&packet(2, frame_wire::FLAG_PRESENT))
                .decision,
            IngressDecision::Accepted
        );
        assert_eq!(received(&receiver), (true, true));
        assert_eq!(submit.ingress.lock().last_accepted_sequence(), 2);
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

#[cfg(test)]
mod sync_tests {
    use super::*;

    const NOW: u64 = 1_000_000_000;
    const DEADLINE: u64 = NOW + 50_000_000;

    /// A `SyncPath` with no renderer behind it.
    ///
    /// Every case below is one the boundary must answer without a GPU, and
    /// answering them here rather than in a device test is deliberate: these
    /// are the paths a producer hits when something has gone wrong, and a
    /// blocked producer's fate should not depend on a lane that needs hardware
    /// to run at all.
    fn path() -> SyncPath {
        SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            Arc::new(OnceLock::new()),
            Admission::new(Arc::new(Mutex::new(FrameIngress::new(
                0,
                INITIAL_RUNTIME_GENERATION,
            )))),
            Arc::new(ExternalGlErrors::default()),
        )
    }

    fn post(
        path: &SyncPath,
        request: SyncRequest,
        params: &[u8],
        now: u64,
    ) -> Result<SyncSnapshot, SyncError> {
        let posted = path.post(request, params, now);
        if let Ok(own) = posted {
            // What a post reports is where its own request ended, which with
            // nothing racing it is also what the mailbox holds.
            assert_eq!(own, path.snapshot(now), "post reported another verdict");
        }
        posted
    }

    fn request(operation: u32, max_reply_bytes: u32) -> SyncRequest {
        SyncRequest {
            request_id: 0,
            runtime_generation: INITIAL_RUNTIME_GENERATION,
            surface_generation: 1,
            resource_epoch: 1,
            triggering_sequence: 1,
            operation,
            max_reply_bytes,
            deadline_nanos: DEADLINE,
        }
    }

    fn read_pixels_params(width: i32, height: i32) -> Vec<u8> {
        let mut bytes = Vec::new();
        for word in [
            1u32,
            0,
            0,
            width as u32,
            height as u32,
            frame_wire::sync::GL_RGBA,
            frame_wire::sync::GL_UNSIGNED_BYTE,
            0,
        ] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn an_operation_this_host_does_not_implement_is_refused_rather_than_answered() {
        let path = path();
        // Operation 0 is not an operation. The producer must be woken with a
        // reason, because the alternative to an answer here is an agent that
        // sits in `Atomics.wait` until its process is reclaimed.
        post(&path, request(0, 1024), &read_pixels_params(2, 2), NOW)
            .expect("the request is posted even when it cannot be answered");
        let snapshot = path.snapshot(NOW);
        assert_eq!(snapshot.state, SyncState::Failed);
        assert_eq!(snapshot.error, Some(SyncError::UnsupportedOperation));
        assert_eq!(snapshot.reply_bytes, 0);
    }

    #[test]
    fn a_rectangle_larger_than_the_producer_reserved_is_refused_before_the_renderer_is_asked() {
        let path = path();
        // 64x64 RGBA8 is 16 KiB; the producer reserved 1 KiB. Refusing here
        // rather than after the readback saves a full readback nobody may have
        // -- and refusing at all rather than truncating is the point: a short
        // `readPixels` is a wrong answer that looks like a right one.
        post(
            &path,
            request(SYNC_OP_READ_PIXELS, 1024),
            &read_pixels_params(64, 64),
            NOW,
        )
        .expect("posted");
        let snapshot = path.snapshot(NOW);
        assert_eq!(snapshot.state, SyncState::Failed);
        assert_eq!(snapshot.error, Some(SyncError::ReplyTooLarge));
    }

    #[test]
    fn a_request_that_arrives_before_the_renderer_is_up_says_so_rather_than_unsupported() {
        let path = path();
        // The distinction matters to the producer: "this host does not do
        // readPixels" is permanent and "not yet" is not, and a producer told
        // the first will stop asking.
        post(
            &path,
            request(SYNC_OP_READ_PIXELS, 4 * 1024 * 1024),
            &read_pixels_params(16, 16),
            NOW,
        )
        .expect("posted");
        let snapshot = path.snapshot(NOW);
        assert_eq!(snapshot.state, SyncState::Failed);
        assert_eq!(snapshot.error, Some(SyncError::SessionEnded));
    }

    /// The two 2D reads reach the renderer as the commands the ops send, and
    /// their answers reach the reply slot whole.
    ///
    /// The stand-in renderer answers each with the rectangle it was asked for.
    /// What that pins is the pairing: an image-data read is a `GetImageData` on
    /// the canvas the params name, a snapshot read is a `ReadSnapshotPixels` on
    /// the id -- swapping them would answer a picture of the wrong thing, at the
    /// right size, which nothing downstream can tell apart from the right one.
    #[test]
    fn the_two_canvas2d_reads_reach_the_renderer_as_their_own_commands() {
        use shared::protocol::render_cmd::{Canvas2DCmd, RenderCommand};

        for (operation, target) in [
            (frame_wire::sync::SYNC_OP_CANVAS2D_IMAGE_DATA, 9u32),
            (frame_wire::sync::SYNC_OP_CANVAS2D_SNAPSHOT, 77u32),
        ] {
            let (sender, commands) = shared::render_command_sender::CommandSender::new();
            let sender = Arc::new(sender);
            let dispatch = Arc::new(OnceLock::new());
            assert!(
                dispatch
                    .set(RenderDispatch {
                        sender: Arc::downgrade(&sender),
                        words: Mutex::new(Vec::new()),
                    })
                    .is_ok()
            );
            let path = SyncPath::new(
                INITIAL_RUNTIME_GENERATION,
                dispatch,
                Admission::new(Arc::new(Mutex::new(FrameIngress::new(
                    0,
                    INITIAL_RUNTIME_GENERATION,
                )))),
                Arc::new(ExternalGlErrors::default()),
            );

            let renderer = std::thread::spawn(move || {
                let Ok(RenderCommand::Canvas2D { canvas_id, cmd }) = commands.recv() else {
                    panic!("the barrier sent something other than a Canvas2D command");
                };
                match cmd {
                    Canvas2DCmd::GetImageData {
                        x,
                        y,
                        width,
                        height,
                        resp,
                    } => {
                        let answer = ((canvas_id, x, y), width, height);
                        resp.send(Ok(vec![0xab; (width * height * 4) as usize]));
                        ("image data", answer)
                    }
                    Canvas2DCmd::ReadSnapshotPixels { snapshot_id, resp } => {
                        resp.send(Ok(vec![0xab; 3 * 2 * 4]));
                        ("snapshot", ((snapshot_id, 0, 0), 3, 2))
                    }
                    other => panic!("the barrier sent {other:?}"),
                }
            });

            let params = frame_wire::sync::Canvas2DPixelsParams {
                target,
                x: 5,
                y: -6,
                width: 3,
                height: 2,
            };
            let mut read = request(operation, 3 * 2 * 4);
            read.deadline_nanos = NOW + 30_000_000_000;
            read.triggering_sequence = 0;
            post(&path, read, &params.encode(), NOW).expect("posted");
            let (kind, seen) = renderer.join().expect("the stand-in renderer answered");

            let snapshot = path.snapshot(NOW);
            assert_eq!(
                (snapshot.state, snapshot.error),
                (SyncState::Ready, None),
                "{kind}: the renderer answered and the barrier did not accept its reply"
            );
            assert_eq!(snapshot.reply_bytes, 3 * 2 * 4);
            if operation == frame_wire::sync::SYNC_OP_CANVAS2D_IMAGE_DATA {
                assert_eq!(
                    seen,
                    ((target, 5, -6), 3, 2),
                    "an image-data read names its canvas and its rectangle"
                );
            } else {
                assert_eq!(
                    seen.0.0, target,
                    "a snapshot read names the snapshot, not a canvas"
                );
            }
            let mut out = [0u8; 24];
            assert_eq!(path.take_reply(&mut out), Ok(24));
            assert!(out.iter().all(|byte| *byte == 0xab), "{kind}: the rows");
            drop(sender);
        }
    }

    /// `loadFont` reads the game's own file and registers what the renderer
    /// answers, and a path it cannot read is the empty family the op answers.
    ///
    /// Every step here is one the producer cannot take: the sandbox, the file,
    /// the family the two shared helpers derive from the path, and the renderer.
    /// What the answer is for content is the key it will name the face by -- so
    /// an empty one has to reach it as an answer rather than as a failure, which
    /// is what the in-process op does with the same outcome.
    #[test]
    fn load_font_reads_the_games_file_and_answers_the_family_the_renderer_registered() {
        use shared::protocol::render_cmd::RenderCommand;

        let root =
            std::env::temp_dir().join(format!("migo-external-load-font-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files = root.join("files");
        let cache = root.join("cache");
        let installed = shared::vfs::GamePaths::new(&files, &cache, "g", 1).expect("paths");
        std::fs::create_dir_all(installed.code_dir().join("fonts")).expect("the font directory");
        std::fs::write(
            installed.code_dir().join("fonts/MyFont.ttf"),
            b"not really a font",
        )
        .expect("the font file");
        std::fs::write(installed.code_dir().join("fonts/Empty.ttf"), b"").expect("the empty file");

        let (services, _work) = ServiceHost::new(
            INITIAL_RUNTIME_GENERATION,
            files,
            cache,
            Arc::new(crate::runtime::external_services::WakerSlot::default()),
        );
        services.context.bind_session(1);
        services
            .context
            .load_content("g")
            .expect("installed content mounts");

        let (sender, commands) = new_render_channel();
        let renderer = std::thread::spawn(move || {
            let mut seen = Vec::new();
            while let Ok(command) = commands.recv_timeout(std::time::Duration::from_secs(5)) {
                if let RenderCommand::LoadFont {
                    family,
                    aliases,
                    bytes,
                    resp,
                } = command
                {
                    seen.push((family.clone(), aliases.to_vec(), bytes.len()));
                    // The renderer's own canonical key, which is what content
                    // gets back rather than what the caller asked for.
                    resp.send(Ok(format!("{family}-registered")));
                }
            }
            seen
        });

        let ask = |path: &str, family: &str, reply_bytes: u32| {
            let path_owned = path.to_string();
            let params = frame_wire::sync::Canvas2DQueryParams {
                kind: frame_wire::sync::canvas2d_query::LOAD_FONT,
                canvas_id: 1,
                number: 0,
                flags: 0,
                text: path_owned.as_bytes(),
                font: family.as_bytes(),
            }
            .encode();
            let path = path_with_dispatch(&sender).with_services(Arc::clone(&services));
            let mut request = request(frame_wire::sync::SYNC_OP_CANVAS2D_FONT, reply_bytes);
            request.deadline_nanos = NOW + 30_000_000_000;
            request.triggering_sequence = 0;
            post(&path, request, &params, NOW).expect("posted");
            let snapshot = path.snapshot(NOW);
            let mut out = vec![0u8; snapshot.reply_bytes as usize];
            if !out.is_empty() {
                assert_eq!(path.take_reply(&mut out), Ok(out.len()));
            }
            (
                snapshot.state,
                String::from_utf8(out).expect("a family is text"),
            )
        };

        assert_eq!(
            ask("fonts/MyFont.ttf", "Brand Sans", 4096),
            (SyncState::Ready, "Brand Sans-registered".to_string()),
            "the family content asked for is the one the renderer was given"
        );
        assert_eq!(
            ask("fonts/Missing.ttf", "", 4096),
            (SyncState::Ready, String::new()),
            "a font the game does not ship answers the empty family, not a failure"
        );
        assert_eq!(
            ask("fonts/Empty.ttf", "", 4096),
            (SyncState::Ready, String::new()),
            "an empty file is not a face"
        );

        drop(sender);
        let seen = renderer.join().expect("the stand-in renderer");
        assert_eq!(
            seen.len(),
            1,
            "only the font that could be read reached the renderer"
        );
        assert_eq!(seen[0].0, "Brand Sans");
        assert!(
            seen[0].1.iter().any(|alias| alias == "MyFont"),
            "the file's own name stays an alias: {:?}",
            seen[0].1
        );
        assert_eq!(seen[0].2, b"not really a font".len());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A renderer that answers a different number of bytes than the rectangle
    /// implies is refused rather than copied: a snapshot the pool has dropped
    /// answers empty, and passing that on is a blank picture with nothing said.
    #[test]
    fn a_canvas2d_read_answered_at_the_wrong_size_is_refused() {
        use shared::protocol::render_cmd::{Canvas2DCmd, RenderCommand};

        let (sender, commands) = shared::render_command_sender::CommandSender::new();
        let sender = Arc::new(sender);
        let dispatch = Arc::new(OnceLock::new());
        assert!(
            dispatch
                .set(RenderDispatch {
                    sender: Arc::downgrade(&sender),
                    words: Mutex::new(Vec::new()),
                })
                .is_ok()
        );
        let path = SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            dispatch,
            Admission::new(Arc::new(Mutex::new(FrameIngress::new(
                0,
                INITIAL_RUNTIME_GENERATION,
            )))),
            Arc::new(ExternalGlErrors::default()),
        );
        let renderer = std::thread::spawn(move || {
            let Ok(RenderCommand::Canvas2D {
                cmd: Canvas2DCmd::ReadSnapshotPixels { resp, .. },
                ..
            }) = commands.recv()
            else {
                panic!("the barrier sent something other than a snapshot read");
            };
            // What the pool answers for a snapshot it no longer holds.
            resp.send(Ok(Vec::new()));
        });

        let params = frame_wire::sync::Canvas2DPixelsParams {
            target: 3,
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        let mut read = request(frame_wire::sync::SYNC_OP_CANVAS2D_SNAPSHOT, 4 * 4 * 4);
        read.deadline_nanos = NOW + 30_000_000_000;
        read.triggering_sequence = 0;
        post(&path, read, &params.encode(), NOW).expect("posted");
        renderer.join().expect("the stand-in renderer answered");

        let snapshot = path.snapshot(NOW);
        assert_eq!(
            (snapshot.state, snapshot.error),
            (SyncState::Failed, Some(SyncError::OperationFailed)),
            "an answer of the wrong size was accepted"
        );
        drop(sender);
    }

    #[test]
    fn malformed_arguments_are_refused_and_never_reach_the_renderer() {
        // A fresh path per case, because a mailbox that has already settled one
        // request would report that verdict for the next.
        for params in [
            // No arguments at all.
            Vec::new(),
            // A rectangle with no pixels in it.
            read_pixels_params(0, 4),
            // One byte short: the remaining fields would be read out of
            // whatever followed the record.
            read_pixels_params(4, 4)[..31].to_vec(),
        ] {
            let path = path();
            post(&path, request(SYNC_OP_READ_PIXELS, 4096), &params, NOW).expect("posted");
            let snapshot = path.snapshot(NOW);
            assert_eq!(snapshot.state, SyncState::Failed);
            assert_eq!(snapshot.error, Some(SyncError::UnsupportedOperation));
        }
    }

    #[test]
    fn a_deadline_that_has_already_passed_is_refused_at_post() {
        let path = path();
        let mut stale = request(SYNC_OP_READ_PIXELS, 4096);
        stale.deadline_nanos = NOW;
        // Refused rather than posted: a request nobody will wait for should not
        // occupy the one slot a session has.
        assert_eq!(
            post(&path, stale, &read_pixels_params(4, 4), NOW),
            Err(SyncError::BadDeadline)
        );
        assert_eq!(path.snapshot(NOW).state, SyncState::Free);
    }

    #[test]
    fn a_request_from_another_runtime_generation_is_refused() {
        let path = path();
        let mut stale = request(SYNC_OP_READ_PIXELS, 4096);
        stale.runtime_generation = INITIAL_RUNTIME_GENERATION + 1;
        assert_eq!(
            post(&path, stale, &read_pixels_params(4, 4), NOW),
            Err(SyncError::StaleGeneration)
        );
    }

    #[test]
    fn taking_a_reply_there_is_no_answer_for_reports_the_reason_and_changes_nothing() {
        let path = path();
        let mut buffer = [0u8; 64];
        assert!(path.take_reply(&mut buffer).is_err());
        assert_eq!(path.snapshot(NOW).state, SyncState::Free);

        post(&path, request(0, 1024), &read_pixels_params(2, 2), NOW).expect("posted");
        // FAILED, so there is nothing to take -- and taking must not clear the
        // verdict out from under a producer that has not read it.
        assert!(path.take_reply(&mut buffer).is_err());
        assert_eq!(path.snapshot(NOW).state, SyncState::Failed);
    }

    #[test]
    fn a_settled_request_frees_the_slot_for_the_next_one() {
        let path = path();
        post(&path, request(0, 1024), &read_pixels_params(2, 2), NOW).expect("posted");
        assert_eq!(path.snapshot(NOW).state, SyncState::Failed);

        // A second request is accepted, because the first is settled. If it
        // were not, a producer whose first call failed could never make another
        // -- one failure would end synchronous calls for the session.
        path.mailbox.lock().acknowledge();
        post(&path, request(0, 1024), &read_pixels_params(2, 2), NOW)
            .expect("the slot is reusable once the first is acknowledged");
    }

    /// A readback the renderer answers reaches the reply slot, byte for byte,
    /// under PACK state that makes the destination footprint larger than the
    /// reply.
    ///
    /// The renderer here is a stand-in that decides the way the real one does
    /// (`read_webgl_pixels`): it refuses a PACK footprint that overruns the
    /// destination it was told about, and otherwise answers with the compact
    /// rows. 3x2 RGBA8 under `PACK_ALIGNMENT` 8 with one skipped row is 24 bytes
    /// of pixels and a 44-byte footprint, and the producer reserved exactly the
    /// 24 -- so a barrier that handed the renderer its reservation as the
    /// destination would turn a read the producer can place into a refusal.
    ///
    /// Until this existed nothing in the crate exercised the answered path at
    /// all: every case above fails before the renderer is asked, so a change to
    /// what the renderer replies with broke the Apple builds and no test here.
    #[test]
    fn a_readback_the_renderer_answers_reaches_the_reply_slot_under_padded_pack_state() {
        use shared::protocol::pixel_pack::PixelPackLayout;
        const LAYOUT: usize = frame_wire::sync::READ_PIXELS_LAYOUT_BYTES;
        use shared::protocol::render_cmd::{GLCmd, ReadPixelsData, RenderCommand};

        let (sender, commands) = shared::render_command_sender::CommandSender::new();
        let sender = Arc::new(sender);
        let dispatch = Arc::new(OnceLock::new());
        assert!(
            dispatch
                .set(RenderDispatch {
                    sender: Arc::downgrade(&sender),
                    words: Mutex::new(Vec::new()),
                })
                .is_ok()
        );
        let path = SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            dispatch,
            Admission::new(Arc::new(Mutex::new(FrameIngress::new(
                0,
                INITIAL_RUNTIME_GENERATION,
            )))),
            Arc::new(ExternalGlErrors::default()),
        );

        let renderer = std::thread::spawn(move || {
            let Ok(RenderCommand::GL(GLCmd::ReadPixels {
                width,
                height,
                destination_byte_length,
                resp,
                ..
            })) = commands.recv()
            else {
                panic!("the barrier sent something other than a readPixels");
            };
            let layout = PixelPackLayout::new(width, height, 4, 8, 0, 1, 0).expect("valid PACK");
            if layout.required_bytes > destination_byte_length {
                resp.err_code(shared::error::ErrorCode::InvalidOperation);
                return Some((layout.required_bytes, destination_byte_length));
            }
            let pixels = (0..layout.compact_bytes)
                .map(|byte| byte as u8 + 1)
                .collect();
            resp.ok(ReadPixelsData { pixels, layout });
            None
        });

        // A generous deadline: the stand-in is a thread that has to be
        // scheduled, and this asserts what it answers, not how fast.
        let mut read = request(SYNC_OP_READ_PIXELS, 24 + LAYOUT as u32);
        read.deadline_nanos = NOW + 30_000_000_000;
        // No frame has been submitted to this path, so there is nothing for the
        // read to wait for; the wait itself is covered by the tests below.
        read.triggering_sequence = 0;
        post(&path, read, &read_pixels_params(3, 2), NOW).expect("posted");
        // Asserted apart from the reply, because both failures read back as
        // OPERATION_FAILED and they want opposite fixes.
        let refusal = renderer.join().expect("the stand-in renderer answered");
        assert_eq!(
            refusal, None,
            "the renderer refused a {:?} (footprint, destination) PACK read: the barrier bounded \
             it by a destination view that is not on this side",
            refusal
        );

        let snapshot = path.snapshot(NOW);
        assert_eq!(
            (snapshot.state, snapshot.error),
            (SyncState::Ready, None),
            "the renderer answered and the barrier did not accept its reply"
        );
        assert_eq!(snapshot.reply_bytes, (24 + LAYOUT) as u32);
        let mut out = [0u8; 24 + LAYOUT];
        assert_eq!(path.take_reply(&mut out), Ok(24 + LAYOUT));
        // The layout the renderer used, in front of the rows: 3x2 RGBA8 under
        // `PACK_ALIGNMENT` 8 with one skipped row is a 12-byte row on a 16-byte
        // stride, starting 16 bytes in. The producer cannot derive any of that
        // -- it never sees `pixelStorei` -- so a reply that dropped it would
        // place every row of a padded read in the wrong place.
        assert_eq!(
            frame_wire::sync::ReadPixelsLayout::decode(&out),
            Some(frame_wire::sync::ReadPixelsLayout {
                first_byte: 16,
                row_bytes: 12,
                row_stride: 16,
                height: 2,
            })
        );
        let expected: Vec<u8> = (1..=24).collect();
        assert_eq!(&out[LAYOUT..], expected.as_slice());
        drop(sender);
    }

    /// A command channel and a dispatch that points at it: what a query needs
    /// to reach a stand-in renderer.
    fn new_render_channel() -> (
        Arc<shared::render_command_sender::CommandSender>,
        crossbeam_channel::Receiver<shared::protocol::render_cmd::RenderCommand>,
    ) {
        let (sender, commands) = shared::render_command_sender::CommandSender::new();
        (Arc::new(sender), commands)
    }

    fn path_with_dispatch(sender: &Arc<shared::render_command_sender::CommandSender>) -> SyncPath {
        let dispatch = Arc::new(OnceLock::new());
        assert!(
            dispatch
                .set(RenderDispatch {
                    sender: Arc::downgrade(sender),
                    words: Mutex::new(Vec::new()),
                })
                .is_ok()
        );
        SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            dispatch,
            Admission::new(Arc::new(Mutex::new(FrameIngress::new(
                0,
                INITIAL_RUNTIME_GENERATION,
            )))),
            Arc::new(ExternalGlErrors::default()),
        )
    }

    /// A scalar query: the producer asks, the renderer answers, and the four
    /// bytes that come back are the number it gave.
    #[test]
    fn a_scalar_query_is_answered_with_the_renderers_number() {
        use frame_wire::sync::{GlQueryParams, SYNC_OP_GL_QUERY_SCALAR, gl_query};
        use shared::protocol::render_cmd::{GLCmd, RenderCommand};

        let (sender, commands) = new_render_channel();
        let path = path_with_dispatch(&sender);
        let renderer = std::thread::spawn(move || {
            let Ok(RenderCommand::GL(GLCmd::GetUniformLocation { name, resp, .. })) =
                commands.recv()
            else {
                panic!("the barrier sent something other than a uniform location query");
            };
            // The name has to arrive whole: a location looked up under a
            // truncated name is answered, not refused, with `None`.
            assert_eq!(name, "uColor");
            resp.ok(Some(7));
        });

        let params = GlQueryParams {
            kind: gl_query::UNIFORM_LOCATION,
            canvas_id: 1,
            object: 4,
            pname: 0,
            extra: 0,
            name: b"uColor",
        }
        .encode();
        let mut call = request(SYNC_OP_GL_QUERY_SCALAR, 4);
        call.deadline_nanos = NOW + 30_000_000_000;
        call.triggering_sequence = 0;
        post(&path, call, &params, NOW).expect("posted");
        renderer.join().expect("the stand-in renderer answered");

        let snapshot = path.snapshot(NOW);
        assert_eq!((snapshot.state, snapshot.error), (SyncState::Ready, None));
        let mut out = [0u8; 4];
        assert_eq!(path.take_reply(&mut out), Ok(4));
        assert_eq!(i32::from_le_bytes(out), 7);
        drop(sender);
    }

    /// A location nothing has is -1, which is what WebGL compares against --
    /// not a refusal, and not zero, which is a real location.
    #[test]
    fn a_location_that_does_not_exist_is_minus_one() {
        use frame_wire::sync::{GlQueryParams, SYNC_OP_GL_QUERY_SCALAR, gl_query};
        use shared::protocol::render_cmd::{GLCmd, RenderCommand};

        let (sender, commands) = new_render_channel();
        let path = path_with_dispatch(&sender);
        let renderer = std::thread::spawn(move || {
            let Ok(RenderCommand::GL(GLCmd::GetAttribLocation { resp, .. })) = commands.recv()
            else {
                panic!("the barrier sent something other than an attribute location query");
            };
            resp.ok(None);
        });

        let params = GlQueryParams {
            kind: gl_query::ATTRIB_LOCATION,
            canvas_id: 1,
            object: 4,
            pname: 0,
            extra: 0,
            name: b"missing",
        }
        .encode();
        let mut call = request(SYNC_OP_GL_QUERY_SCALAR, 4);
        call.deadline_nanos = NOW + 30_000_000_000;
        call.triggering_sequence = 0;
        post(&path, call, &params, NOW).expect("posted");
        renderer.join().expect("the stand-in renderer answered");
        let mut out = [0u8; 4];
        assert_eq!(path.take_reply(&mut out), Ok(4));
        assert_eq!(i32::from_le_bytes(out), -1);
        drop(sender);
    }

    /// An active variable: a size, a type and a name, in one reply the producer
    /// turns back into the object the engine's facade parses.
    #[test]
    fn an_active_variable_answers_with_its_size_type_and_name() {
        use frame_wire::sync::{
            ACTIVE_VARIABLE_HEADER_BYTES, GlQueryParams, SYNC_OP_GL_QUERY_ACTIVE, gl_query,
        };
        use shared::protocol::render_cmd::{GLCmd, RenderCommand};

        let (sender, commands) = new_render_channel();
        let path = path_with_dispatch(&sender);
        let renderer = std::thread::spawn(move || {
            let Ok(RenderCommand::GL(GLCmd::GetActiveUniform { index, resp, .. })) =
                commands.recv()
            else {
                panic!("the barrier sent something other than an active uniform query");
            };
            assert_eq!(index, 2);
            resp.ok(Some(("uColor".to_owned(), 1, 0x8B52)));
        });

        let params = GlQueryParams {
            kind: gl_query::ACTIVE_UNIFORM,
            canvas_id: 1,
            object: 4,
            pname: 2,
            extra: 0,
            name: &[],
        }
        .encode();
        let mut call = request(SYNC_OP_GL_QUERY_ACTIVE, 128);
        call.deadline_nanos = NOW + 30_000_000_000;
        call.triggering_sequence = 0;
        post(&path, call, &params, NOW).expect("posted");
        renderer.join().expect("the stand-in renderer answered");

        let mut out = [0u8; 128];
        let written = path.take_reply(&mut out).expect("a reply");
        assert_eq!(written, ACTIVE_VARIABLE_HEADER_BYTES + "uColor".len());
        assert_eq!(i32::from_le_bytes(out[0..4].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 0x8B52);
        assert_eq!(&out[8..written], b"uColor");
        drop(sender);
    }

    /// `getError` is answered from the host's own queue -- the errors its
    /// decoder recorded for this producer's records -- without asking the
    /// renderer anything, and it drains one per call.
    #[test]
    fn get_error_drains_the_queue_the_host_filled_while_decoding() {
        use frame_wire::sync::{GlQueryParams, SYNC_OP_GL_QUERY_SCALAR, gl_query};

        let errors = Arc::new(ExternalGlErrors::default());
        errors.push(1, frame_decode::codes::INVALID_VALUE);
        let path = SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            // No renderer at all: a query that needed one would fail here, and
            // that is the point -- this one must not need one.
            Arc::new(OnceLock::new()),
            Admission::new(Arc::new(Mutex::new(FrameIngress::new(
                0,
                INITIAL_RUNTIME_GENERATION,
            )))),
            Arc::clone(&errors),
        );

        let ask = |path: &SyncPath| {
            let params = GlQueryParams {
                kind: gl_query::GET_ERROR,
                canvas_id: 1,
                object: 0,
                pname: 0,
                extra: 0,
                name: &[],
            }
            .encode();
            let mut call = request(SYNC_OP_GL_QUERY_SCALAR, 4);
            call.deadline_nanos = NOW + 1_000_000_000;
            call.triggering_sequence = 0;
            post(path, call, &params, NOW).expect("posted");
            let mut out = [0u8; 4];
            assert_eq!(path.take_reply(&mut out), Ok(4));
            u32::from_le_bytes(out)
        };

        assert_eq!(ask(&path), frame_decode::codes::INVALID_VALUE);
        assert_eq!(ask(&path), 0, "the queue drains one error per call");
    }

    /// `measureText` asks the renderer and answers with the twelve numbers the
    /// engine's facade reads a `TextMetrics` back from.
    #[test]
    fn a_text_measurement_is_answered_with_the_metrics_the_renderer_gave() {
        use shared::protocol::render_cmd::{Canvas2DCmd, RenderCommand, TextMetrics};

        use frame_wire::sync::{
            Canvas2DQueryParams, SYNC_OP_CANVAS2D_METRICS, TEXT_METRICS_BYTES, canvas2d_query,
        };

        let (sender, commands) = new_render_channel();
        let path = path_with_dispatch(&sender);
        let renderer = std::thread::spawn(move || {
            let Ok(RenderCommand::Canvas2D {
                canvas_id,
                cmd: Canvas2DCmd::MeasureText { text, resp },
            }) = commands.recv()
            else {
                panic!("the barrier sent something other than a text measurement");
            };
            assert_eq!(canvas_id, 1);
            // The text has to arrive whole: a measurement of a truncated string
            // is a number, not a failure, and it lays the label out wrong.
            assert_eq!(text, "score: 120");
            resp.ok(TextMetrics {
                width: 64.5,
                actual_bounding_box_left: 0.0,
                actual_bounding_box_right: 0.0,
                actual_bounding_box_ascent: 12.0,
                actual_bounding_box_descent: 0.0,
                font_bounding_box_ascent: 0.0,
                font_bounding_box_descent: 0.0,
                em_height_ascent: 0.0,
                em_height_descent: 0.0,
                hanging_baseline: 0.0,
                alphabetic_baseline: 0.0,
                ideographic_baseline: 0.0,
            });
        });

        let params = Canvas2DQueryParams {
            kind: canvas2d_query::MEASURE_TEXT,
            canvas_id: 1,
            number: 0,
            flags: 0,
            text: b"score: 120",
            font: b"16px sans-serif",
        }
        .encode();
        let mut call = request(SYNC_OP_CANVAS2D_METRICS, TEXT_METRICS_BYTES as u32);
        call.deadline_nanos = NOW + 30_000_000_000;
        call.triggering_sequence = 0;
        post(&path, call, &params, NOW).expect("posted");
        renderer.join().expect("the stand-in renderer answered");

        let mut out = [0u8; TEXT_METRICS_BYTES];
        assert_eq!(path.take_reply(&mut out), Ok(TEXT_METRICS_BYTES));
        let width = f32::from_le_bytes(out[0..4].try_into().unwrap());
        assert_eq!(width, 64.5, "the first field is the advance width");
        // The ascent is the eighth field, which is where a layout that restated
        // the order rather than sharing it would put something else.
        let ascent = f32::from_le_bytes(out[28..32].try_into().unwrap());
        assert_eq!(ascent, 12.0);
        drop(sender);
    }

    /// A kind sent under the wrong operation is refused rather than answered:
    /// the operation is what sizes the reply, so an info log answered as a
    /// scalar would be four bytes of a string.
    #[test]
    fn a_query_under_the_wrong_operation_is_refused() {
        use frame_wire::sync::{GlQueryParams, SYNC_OP_GL_QUERY_SCALAR, gl_query};

        let path = path();
        let params = GlQueryParams {
            kind: gl_query::PROGRAM_INFO_LOG,
            canvas_id: 1,
            object: 4,
            pname: 0,
            extra: 0,
            name: &[],
        }
        .encode();
        let mut call = request(SYNC_OP_GL_QUERY_SCALAR, 4);
        call.deadline_nanos = NOW + 1_000_000_000;
        call.triggering_sequence = 0;
        let snapshot = post(&path, call, &params, NOW).expect("the request is posted");
        assert_eq!(
            (snapshot.state, snapshot.error),
            (SyncState::Failed, Some(SyncError::UnsupportedOperation)),
            "the call is answered as failed, not answered with four bytes of something else"
        );
        assert_eq!(snapshot.reply_bytes, 0);
    }

    #[test]
    fn ending_the_session_refuses_every_later_request() {
        let path = path();
        assert!(
            !path.mailbox.lock().end_session(),
            "nothing was outstanding"
        );
        assert_eq!(
            post(
                &path,
                request(SYNC_OP_READ_PIXELS, 4096),
                &read_pixels_params(4, 4),
                NOW
            ),
            Err(SyncError::SessionEnded)
        );
    }
}

#[cfg(test)]
mod sync_answer_tests {
    use super::*;
    use frame_wire::sync::{GL_RGBA, GL_UNSIGNED_BYTE, MAX_REPLY_BYTES, SYNC_CALL_HEADER_BYTES};

    const NOW: u64 = 1_000_000_000;

    fn path_with(dispatch: Arc<OnceLock<RenderDispatch>>) -> SyncPath {
        SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            dispatch,
            Admission::new(Arc::new(Mutex::new(FrameIngress::new(
                0,
                INITIAL_RUNTIME_GENERATION,
            )))),
            Arc::new(ExternalGlErrors::default()),
        )
    }

    /// A call body as the producer writes one: the layout in
    /// contracts/frame-wire/wire-v1.md, "A request as one body".
    fn call(width: i32, height: i32, max_reply_bytes: u32, timeout_millis: u32) -> Vec<u8> {
        let mut body = Vec::with_capacity(SYNC_CALL_HEADER_BYTES + 32);
        for word in [INITIAL_RUNTIME_GENERATION, 1, 1, 0] {
            body.extend_from_slice(&word.to_le_bytes());
        }
        for word in [SYNC_OP_READ_PIXELS, max_reply_bytes, timeout_millis, 0] {
            body.extend_from_slice(&word.to_le_bytes());
        }
        // service_sequence: nothing sent on the service stream.
        body.extend_from_slice(&0u64.to_le_bytes());
        for word in [
            1u32,
            0,
            0,
            width as u32,
            height as u32,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            0,
        ] {
            body.extend_from_slice(&word.to_le_bytes());
        }
        body
    }

    /// Answer `body`, and check the two parts agree with each other.
    fn answer_of(path: &SyncPath, body: &[u8]) -> AnsweredCall {
        let answered = path.answer(body, NOW);
        assert_eq!(
            answered.reply.len(),
            answered.answer.reply_bytes as usize,
            "the header names a different reply length than the reply has"
        );
        if answered.answer.state != SyncState::Ready {
            assert_eq!(
                answered.reply.capacity(),
                0,
                "an answer with no reply allocated one"
            );
        }
        answered
    }

    /// A stand-in renderer that answers one readPixels with bytes 1..=n.
    fn renderer() -> (
        Arc<shared::render_command_sender::CommandSender>,
        Arc<OnceLock<RenderDispatch>>,
        std::thread::JoinHandle<()>,
    ) {
        use shared::protocol::pixel_pack::PixelPackLayout;
        use shared::protocol::render_cmd::{GLCmd, ReadPixelsData, RenderCommand};

        let (sender, commands) = shared::render_command_sender::CommandSender::new();
        let sender = Arc::new(sender);
        let dispatch = Arc::new(OnceLock::new());
        assert!(
            dispatch
                .set(RenderDispatch {
                    sender: Arc::downgrade(&sender),
                    words: Mutex::new(Vec::new()),
                })
                .is_ok()
        );
        let thread = std::thread::spawn(move || {
            let Ok(RenderCommand::GL(GLCmd::ReadPixels {
                width,
                height,
                resp,
                ..
            })) = commands.recv()
            else {
                panic!("the barrier sent something other than a readPixels");
            };
            let layout = PixelPackLayout::new(width, height, 4, 1, 0, 0, 0).expect("valid PACK");
            let pixels = (0..layout.compact_bytes)
                .map(|byte| byte as u8 + 1)
                .collect();
            resp.ok(ReadPixelsData { pixels, layout });
        });
        (sender, dispatch, thread)
    }

    #[test]
    fn an_answered_call_carries_its_own_id_and_bytes_and_frees_the_slot() {
        const LAYOUT: u32 = frame_wire::sync::READ_PIXELS_LAYOUT_BYTES as u32;
        let (sender, dispatch, renderer) = renderer();
        let path = path_with(dispatch);
        let body = call(3, 2, 24 + LAYOUT, 30_000);
        let answered = answer_of(&path, &body);
        renderer.join().expect("renderer");

        let answer = answered.answer;
        assert_eq!(
            (answer.state, answer.error, answer.reply_bytes),
            (SyncState::Ready, None, 24 + LAYOUT)
        );
        assert_ne!(answer.request_id, 0, "an answered call was given an id");
        // The rows, behind the layout that says where they go: 3x2 RGBA8 with
        // no pack state is compact, which is what the producer places straight.
        assert_eq!(
            frame_wire::sync::ReadPixelsLayout::decode(&answered.reply),
            Some(frame_wire::sync::ReadPixelsLayout {
                first_byte: 0,
                row_bytes: 12,
                row_stride: 12,
                height: 2,
            })
        );
        let expected: Vec<u8> = (1..=24).collect();
        assert_eq!(&answered.reply[LAYOUT as usize..], expected.as_slice());
        // Freed as it was written: the producer holding the response has the
        // bytes, and a slot left READY would refuse nothing but would let a
        // later take hand these pixels to someone else.
        assert_eq!(path.snapshot(NOW).state, SyncState::Free);
        drop(sender);
    }

    #[test]
    fn a_call_that_cannot_be_answered_is_answered_failed_and_the_next_one_is_not_blocked() {
        // No renderer: the session thread has not brought one up.
        let path = path_with(Arc::new(OnceLock::new()));
        // 2x2 RGBA8 and the layout in front of it, so the reservation is not
        // what this call fails on.
        let body = call(
            2,
            2,
            16 + frame_wire::sync::READ_PIXELS_LAYOUT_BYTES as u32,
            250,
        );

        let first = answer_of(&path, &body).answer;
        assert_eq!(
            (first.state, first.error, first.reply_bytes),
            (SyncState::Failed, Some(SyncError::SessionEnded), 0)
        );
        assert_ne!(first.request_id, 0, "it was posted, so it has an id");

        // A failure that left the slot occupied would turn one failed read into
        // a session whose every later read is ALREADY_PENDING.
        let second = answer_of(&path, &body).answer;
        assert_eq!(second.error, Some(SyncError::SessionEnded));
        assert!(
            second.request_id > first.request_id,
            "the second call was posted, not refused as pending"
        );
    }

    #[test]
    fn a_malformed_body_is_answered_failed_without_an_id_and_posts_nothing() {
        let path = path_with(Arc::new(OnceLock::new()));
        for (body, error) in [
            (
                vec![0u8; SYNC_CALL_HEADER_BYTES - 1],
                SyncError::UnsupportedOperation,
            ),
            (call(2, 2, 16, 0), SyncError::BadDeadline),
            (call(2, 2, 0, 250), SyncError::BadReplyReservation),
            (
                call(2, 2, MAX_REPLY_BYTES + 1, 250),
                SyncError::BadReplyReservation,
            ),
        ] {
            let answer = answer_of(&path, &body).answer;
            assert_eq!(
                (answer.state, answer.error, answer.request_id),
                (SyncState::Failed, Some(error), 0)
            );
            assert_eq!(path.snapshot(NOW).state, SyncState::Free);
        }
    }

    #[test]
    fn a_call_while_another_is_outstanding_is_refused_and_leaves_that_one_alone() {
        let path = path_with(Arc::new(OnceLock::new()));
        let outstanding = call(2, 2, 16, 250);
        let decoded = frame_wire::sync::SyncCall::decode(&outstanding).expect("valid");
        let pending_id = path
            .mailbox
            .lock()
            .post(decoded.request(NOW), NOW)
            .expect("posted");

        // The same call again, as a producer that gave up on the first would send.
        let answer = answer_of(&path, &outstanding).answer;
        assert_eq!(
            (answer.state, answer.error, answer.request_id),
            (SyncState::Failed, Some(SyncError::AlreadyPending), 0)
        );
        let snapshot = path.snapshot(NOW);
        assert_eq!(
            (snapshot.state, snapshot.request_id),
            (SyncState::Pending, pending_id),
            "the refused call disturbed the request that was outstanding"
        );
    }

    /// A readback that returns after its request was settled and replaced must
    /// neither fail the newer request nor become its answer.
    #[test]
    fn a_late_readback_settles_nothing_that_is_not_its_own() {
        let path = path_with(Arc::new(OnceLock::new()));
        let body = call(2, 2, 16, 250);
        let decoded = frame_wire::sync::SyncCall::decode(&body).expect("valid");
        let mut mailbox = path.mailbox.lock();
        let late = mailbox.post(decoded.request(NOW), NOW).expect("posted");
        assert!(
            mailbox.expire_if_due(NOW + 250_000_000),
            "the first timed out"
        );
        let newer = mailbox
            .post(decoded.request(NOW + 1), NOW + 1)
            .expect("posted");

        assert_eq!(settle(&mut mailbox, late, Ok(vec![7; 16])), None);
        assert_eq!(
            settle(&mut mailbox, late, Err(SyncError::OperationFailed)),
            None
        );
        assert_eq!(
            (mailbox.state(), mailbox.request().map(|r| r.request_id)),
            (SyncState::Pending, Some(newer)),
            "the newer request was settled by a readback that was not its own"
        );
        assert_eq!(
            settled_snapshot(&mailbox, late),
            SyncSnapshot {
                request_id: late,
                state: SyncState::Failed,
                reply_bytes: 0,
                error: Some(SyncError::LateReply),
            }
        );
        assert_eq!(
            settle(&mut mailbox, newer, Ok(vec![9; 16])),
            Some(vec![9; 16])
        );
        assert_eq!(mailbox.state(), SyncState::Ready);
    }
}

#[cfg(test)]
mod sync_fence_tests {
    use super::*;

    const NONCE: u128 = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;
    const NOW: u64 = 1_000_000_000;

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

    fn read_pixels_params() -> Vec<u8> {
        let mut bytes = Vec::new();
        for word in [
            1u32,
            0,
            0,
            1,
            1,
            frame_wire::sync::GL_RGBA,
            frame_wire::sync::GL_UNSIGNED_BYTE,
            0,
        ] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn request(triggering_sequence: u64, deadline_nanos: u64) -> SyncRequest {
        SyncRequest {
            request_id: 0,
            runtime_generation: INITIAL_RUNTIME_GENERATION,
            surface_generation: 1,
            resource_epoch: 0,
            triggering_sequence,
            operation: SYNC_OP_READ_PIXELS,
            // One RGBA8 pixel and the layout that says where it goes.
            max_reply_bytes: 4 + frame_wire::sync::READ_PIXELS_LAYOUT_BYTES as u32,
            deadline_nanos,
        }
    }

    /// A sync path whose renderer is a channel the test reads, and the ingress
    /// it fences on.
    fn path() -> (
        Arc<SyncPath>,
        Admission,
        Arc<shared::render_command_sender::CommandSender>,
        crossbeam_channel::Receiver<shared::protocol::render_cmd::RenderCommand>,
    ) {
        let (sender, commands) = shared::render_command_sender::CommandSender::new();
        let sender = Arc::new(sender);
        let dispatch = Arc::new(OnceLock::new());
        assert!(
            dispatch
                .set(RenderDispatch {
                    sender: Arc::downgrade(&sender),
                    words: Mutex::new(Vec::new()),
                })
                .is_ok()
        );
        let admission = Admission::new(Arc::new(Mutex::new(FrameIngress::new(
            NONCE,
            INITIAL_RUNTIME_GENERATION,
        ))));
        let path = Arc::new(SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            dispatch,
            admission.clone(),
            Arc::new(ExternalGlErrors::default()),
        ));
        (path, admission, sender, commands)
    }

    /// The read for frame N reaches the renderer only after frame N was admitted.
    ///
    /// On the Apple uplink the read and the frame are two independent streams,
    /// and the read can arrive first. Answering then reads a surface the frame
    /// has not touched, which is a wrong answer that looks like a right one.
    #[test]
    fn a_read_waits_for_the_frame_it_answers_for() {
        let (path, admission, sender, commands) = path();
        let reader = {
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                path.post(request(1, NOW + 30_000_000_000), &read_pixels_params(), NOW)
            })
        };

        // Long enough for a read that did not wait to have been sent.
        assert!(
            matches!(
                commands.recv_timeout(std::time::Duration::from_millis(200)),
                Err(crossbeam_channel::RecvTimeoutError::Timeout)
            ),
            "the read reached the renderer before the frame it answers for was admitted"
        );

        {
            let mut ingress = admission.ingress.lock();
            let outcome = ingress.submit_with(&packet(1), |frame| {
                drop(frame);
                Ok(())
            });
            assert_eq!(outcome.decision, IngressDecision::Accepted);
        }
        admission.admitted.notify_all();

        let Ok(shared::protocol::render_cmd::RenderCommand::GL(
            shared::protocol::render_cmd::GLCmd::ReadPixels { resp, .. },
        )) = commands.recv_timeout(std::time::Duration::from_secs(10))
        else {
            panic!("the read never reached the renderer after its frame was admitted");
        };
        let layout = shared::protocol::pixel_pack::PixelPackLayout::new(1, 1, 4, 4, 0, 0, 0)
            .expect("valid PACK");
        resp.ok(shared::protocol::render_cmd::ReadPixelsData {
            pixels: vec![0, 0, 255, 255],
            layout,
        });
        reader.join().expect("reader").expect("posted");
        assert_eq!(path.snapshot(NOW).state, SyncState::Ready);
        drop(sender);
    }

    /// A read waiting for its frame is released the moment the session ends,
    /// with that reason, rather than holding the producer to its deadline.
    #[test]
    fn ending_the_session_releases_a_read_waiting_for_its_frame() {
        let (path, _admission, sender, _commands) = path();
        let reader = {
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let posted =
                    path.post(request(1, NOW + 30_000_000_000), &read_pixels_params(), NOW);
                (posted, started.elapsed())
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(path.end_session(), "a request was outstanding");
        let (posted, waited) = reader.join().expect("reader");
        assert!(
            posted.is_ok(),
            "the request was posted before the session ended"
        );
        assert_eq!(
            path.snapshot(NOW).error,
            Some(SyncError::SessionEnded),
            "released for the reason it was released"
        );
        assert!(
            waited < std::time::Duration::from_secs(5),
            "the read waited {waited:?}; ending the session did not wake it"
        );
        drop(sender);
    }

    /// A read whose frame never arrives times out; it is not answered early.
    #[test]
    fn a_read_whose_frame_never_arrives_times_out_instead_of_answering() {
        let (path, _admission, sender, commands) = path();
        path.post(request(1, NOW + 50_000_000), &read_pixels_params(), NOW)
            .expect("posted");
        let snapshot = path.snapshot(NOW);
        assert_eq!(
            (snapshot.state, snapshot.error),
            (SyncState::Failed, Some(SyncError::TimedOut))
        );
        assert!(
            commands.try_recv().is_err(),
            "a read was sent to the renderer for a frame that was never admitted"
        );
        drop(sender);
    }
}

#[cfg(test)]
mod sync_teardown_tests {
    use super::*;

    /// Shutting a session down settles an outstanding request.
    ///
    /// Asserted on the handle rather than on `SyncPath`, because the bug this
    /// prevents is not in the mailbox -- which has always been able to end a
    /// session -- but in nobody calling it. A `SyncPath` test would pass with
    /// the entire teardown path unwired.
    #[test]
    fn shutting_down_refuses_later_requests_through_the_public_handle() {
        let path = SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            Arc::new(OnceLock::new()),
            Admission::new(Arc::new(Mutex::new(FrameIngress::new(
                0,
                INITIAL_RUNTIME_GENERATION,
            )))),
            Arc::new(ExternalGlErrors::default()),
        );
        // The state the wiring has to reach. `request_shutdown` needs a running
        // thread, so this asserts the same call the two entry points make.
        assert!(!path.mailbox.lock().end_session());
        assert_eq!(
            path.post(
                SyncRequest {
                    request_id: 0,
                    runtime_generation: INITIAL_RUNTIME_GENERATION,
                    surface_generation: 1,
                    resource_epoch: 1,
                    triggering_sequence: 1,
                    operation: SYNC_OP_READ_PIXELS,
                    max_reply_bytes: 4096,
                    deadline_nanos: 2_000_000_000,
                },
                &[0u8; 32],
                1_000_000_000,
            ),
            Err(SyncError::SessionEnded)
        );
    }

    /// Both teardown entry points wake the producer, not just the tested one.
    ///
    /// Read from the source, because the thing that goes wrong here is an entry
    /// point that forgets -- and a behavioural test would need a live session
    /// thread per entry point to say anything about the other.
    #[test]
    fn every_teardown_entry_point_settles_the_mailbox() {
        let source = include_str!("external.rs");
        for entry in ["fn request_shutdown", "fn shutdown_and_join"] {
            let at = source.find(entry).expect("the entry point exists");
            let body = &source[at..at + 600];
            let end = body.find("\n    }").unwrap_or(body.len());
            assert!(
                body[..end].contains("self.end_sync()"),
                "{entry} tears the session down without settling the synchronous \
                 mailbox, so a producer blocked in Atomics.wait is never woken"
            );
        }
    }
}

#[cfg(test)]
mod await_window_tests {
    use super::*;

    const NONCE: u128 = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;
    const NOW: u64 = 1_000_000_000;

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

    fn request(triggering_sequence: u64, max_reply_bytes: u32, deadline_nanos: u64) -> SyncRequest {
        SyncRequest {
            request_id: 0,
            runtime_generation: INITIAL_RUNTIME_GENERATION,
            surface_generation: 1,
            resource_epoch: 0,
            triggering_sequence,
            operation: SYNC_OP_AWAIT_WINDOW,
            max_reply_bytes,
            deadline_nanos,
        }
    }

    /// No renderer: the window is answered from ingress alone, and a path that
    /// needed one would fail these with `SessionEnded`.
    fn path() -> (Arc<SyncPath>, Admission) {
        let admission = Admission::new(Arc::new(Mutex::new(FrameIngress::new(
            NONCE,
            INITIAL_RUNTIME_GENERATION,
        ))));
        let path = Arc::new(SyncPath::new(
            INITIAL_RUNTIME_GENERATION,
            Arc::new(OnceLock::new()),
            admission.clone(),
            Arc::new(ExternalGlErrors::default()),
        ));
        (path, admission)
    }

    /// Admit `sequence` and keep its credit, as a renderer still working on it does.
    fn admit_and_hold(admission: &Admission, sequence: u64, held: &mut Vec<PooledFrame>) {
        let mut taken = None;
        let outcome = admission
            .ingress
            .lock()
            .submit_with(&packet(sequence), |frame| {
                taken = Some(frame);
                Ok(())
            });
        assert_eq!(outcome.decision, IngressDecision::Accepted);
        held.push(taken.expect("the frame was handed over"));
        admission.admitted.notify_all();
    }

    fn reply(path: &SyncPath) -> WindowReply {
        let mut out = [0u8; 64];
        let written = path.take_reply(&mut out).expect("a ready reply");
        WindowReply::decode(&out[..written]).expect("a window reply")
    }

    #[test]
    fn an_open_window_is_answered_at_once() {
        let (path, _admission) = path();
        let snapshot = path
            .post(request(0, 16, NOW + 1_000_000_000), &[], NOW)
            .expect("posted");
        assert_eq!(snapshot.state, SyncState::Ready);
        // Nothing accepted yet: one, not two -- the cap every advertisement
        // before the first packet carries.
        assert_eq!(
            reply(&path),
            WindowReply {
                remaining_credits: 1,
                accepted_sequence: 0
            }
        );
    }

    /// The case the operation exists for: the renderer holds every credit, the
    /// producer is blocked, and the answer comes when one frame finishes.
    #[test]
    fn a_closed_window_is_answered_when_the_renderer_returns_a_credit() {
        let (path, admission) = path();
        let mut held = Vec::new();
        admit_and_hold(&admission, 1, &mut held);
        admit_and_hold(&admission, 2, &mut held);
        assert_eq!(admission.window.read().remaining_credits, 0);

        let waiter = {
            let path = Arc::clone(&path);
            std::thread::spawn(move || path.post(request(2, 16, NOW + 30_000_000_000), &[], NOW))
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(
            path.snapshot(NOW).state,
            SyncState::Pending,
            "answered while every credit was held"
        );

        drop(held.remove(0));
        let snapshot = waiter.join().expect("waiter").expect("posted");
        assert_eq!(snapshot.state, SyncState::Ready);
        assert_eq!(
            reply(&path),
            WindowReply {
                remaining_credits: 1,
                accepted_sequence: 2
            }
        );
    }

    /// Admission first: a producer that sent through 1 is not told the window
    /// until 1 is in, even with credits free -- an advertisement that did not
    /// count its packet would let it send past the window.
    #[test]
    fn the_answer_waits_for_the_producer_s_last_packet_to_be_admitted() {
        let (path, admission) = path();
        let waiter = {
            let path = Arc::clone(&path);
            std::thread::spawn(move || path.post(request(1, 16, NOW + 30_000_000_000), &[], NOW))
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(path.snapshot(NOW).state, SyncState::Pending);

        let mut held = Vec::new();
        admit_and_hold(&admission, 1, &mut held);
        waiter.join().expect("waiter").expect("posted");
        assert_eq!(
            reply(&path),
            WindowReply {
                remaining_credits: 1,
                accepted_sequence: 1
            }
        );
    }

    #[test]
    fn a_window_that_never_opens_is_a_timeout_not_a_hang() {
        let (path, admission) = path();
        let mut held = Vec::new();
        admit_and_hold(&admission, 1, &mut held);
        admit_and_hold(&admission, 2, &mut held);
        let started = std::time::Instant::now();
        let snapshot = path
            .post(request(2, 16, NOW + 50_000_000), &[], NOW)
            .expect("posted");
        assert_eq!(snapshot.state, SyncState::Failed);
        assert_eq!(snapshot.error, Some(SyncError::TimedOut));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn parameters_or_a_short_reservation_are_refused() {
        let (path, _admission) = path();
        let snapshot = path
            .post(request(0, 16, NOW + 1_000_000_000), &[1, 2, 3, 4], NOW)
            .expect("posted");
        assert_eq!(snapshot.error, Some(SyncError::UnsupportedOperation));
        path.mailbox.lock().acknowledge();

        let snapshot = path
            .post(request(0, 15, NOW + 1_000_000_000), &[], NOW)
            .expect("posted");
        assert_eq!(snapshot.error, Some(SyncError::ReplyTooLarge));
    }
}
