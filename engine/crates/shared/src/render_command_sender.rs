//! Render command sender — wraps a crossbeam bounded channel with
//! command-kind-aware backpressure.
//!
//! Lives in `shared` so both `graphics` (render_thread) and
//! `core`/`runtime-v8` (producers) can reference the type without
//! circular dependencies.
//!
//! ## Backpressure model
//!
//! Historically `send()` was non-blocking and dropped commands on
//! full channel.  That was fine for idempotent per-frame mutators
//! (scissor, viewport, clear-colour) but catastrophic for
//! non-idempotent draw commands (`FillRect`, `DrawImage`, a full
//! `CanvasBatchPayload`): dropping the only carrier of a frame's
//! pixels produces visible glitches with no caller feedback.
//!
//! The current model classifies every [`RenderCommand`] into a
//! [`BackpressurePolicy`] at send time:
//!
//! 1. **`DropOnFull`**: non-blocking; increments a per-class drop
//!    counter in [`CommandDropCounters`].  Used for command kinds
//!    that either (a) naturally replay each frame (fps change,
//!    invalidate), or (b) have no observable effect if lost
//!    (pause/resume/shutdown racing with the render thread exit).
//! 2. **`BlockBounded`**: waits up to [`BLOCKING_SEND_DEADLINE`]
//!    for queue capacity and returns `Err(SendError::Timeout)` on
//!    expiry.  This is the default for draw-class commands
//!    (`Canvas2DBatch`, `FramePacket`, `GLBatch`, per-image
//!    lifecycle `Canvas(…)` ops) and for any command carrying a
//!    sync reply responder.
//!
//! Neither policy can freeze the producer indefinitely: the bounded
//! wait is at most a sub-frame window even under sustained load.
//!
//! Counters from [`CommandDropCounters::snapshot`] flow into
//! `DebugStats` so the Java debug overlay can show per-class
//! pressure live, replacing the previous single monolithic
//! `send_overflow_total` figure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::protocol::render_cmd::RenderCommand;

/// Default render command queue capacity.
/// 512 provides ~8ms of buffering at 60fps with typical command rates.
const CHANNEL_CAPACITY: usize = 512;

/// Deadline the blocking-send fallback waits before giving up.
/// Chosen to be well below a 60 Hz frame (16.7 ms) so a stalled
/// render thread cannot keep a sync op pinned past a single frame.
const BLOCKING_SEND_DEADLINE: Duration = Duration::from_millis(8);

/// Process-global counter of dropped sends (channel was full).
/// Surfaced into `DebugStats.command_drops` so the Java debug
/// overlay can show command-channel pressure without the render
/// thread having to keep a back-reference to session stats.
static SEND_OVERFLOW: AtomicU64 = AtomicU64::new(0);

/// Read the cumulative number of dropped sends across all classes.
#[inline]
pub fn send_overflow_total() -> u64 {
    SEND_OVERFLOW.load(Ordering::Relaxed)
}

/// Per-command-kind drop / timeout counters.
///
/// The classification is coarse by design: the overlay only needs
/// to answer "which *kind* of command is the engine shedding?" so
/// the operator can decide where to look (draws vs lifecycle
/// signals vs sync ops).  Finer-grained per-variant counters would
/// inflate the enum without adding diagnostic value.
#[derive(Debug, Default)]
pub struct CommandDropCounters {
    pub draw_dropped: AtomicU64,
    pub draw_timeout: AtomicU64,
    pub lifecycle_dropped: AtomicU64,
    pub sync_timeout: AtomicU64,
}

/// Process-global snapshot point for [`CommandDropCounters`].
static DROP_COUNTERS: CommandDropCounters = CommandDropCounters {
    draw_dropped: AtomicU64::new(0),
    draw_timeout: AtomicU64::new(0),
    lifecycle_dropped: AtomicU64::new(0),
    sync_timeout: AtomicU64::new(0),
};

/// Read a snapshot of per-class drop counters.  Values are
/// monotonic; consumers typically diff across frames.
#[inline]
pub fn drop_counter_snapshot() -> (u64, u64, u64, u64) {
    (
        DROP_COUNTERS.draw_dropped.load(Ordering::Relaxed),
        DROP_COUNTERS.draw_timeout.load(Ordering::Relaxed),
        DROP_COUNTERS.lifecycle_dropped.load(Ordering::Relaxed),
        DROP_COUNTERS.sync_timeout.load(Ordering::Relaxed),
    )
}

/// Classify a [`RenderCommand`] into draw / lifecycle / sync for
/// policy and counter purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    /// Non-idempotent pixel-producing work: dropping it produces
    /// visible glitches.  Uses blocking backpressure.
    Draw,
    /// Control-plane commands that may safely race with shutdown
    /// (pause/resume/FPS/invalidate).  Non-blocking.
    Lifecycle,
    /// Commands that carry a sync reply responder — the caller is
    /// blocked on the reply channel and MUST be delivered even if
    /// it means waiting.  Blocking with a longer deadline than
    /// draws.
    Sync,
}

impl CommandClass {
    /// Policy lookup.  Sync and draw commands use bounded blocking
    /// so they can't be silently dropped; lifecycle ones fall
    /// through to the non-blocking path.
    #[inline]
    pub const fn policy(self) -> BackpressurePolicy {
        match self {
            Self::Draw | Self::Sync => BackpressurePolicy::BlockBounded,
            Self::Lifecycle => BackpressurePolicy::DropOnFull,
        }
    }
}

impl RenderCommand {
    /// Classify this command for backpressure routing.  Pure
    /// inspection — never observes or mutates channel state.
    #[inline]
    pub fn class(&self) -> CommandClass {
        match self {
            // Pixel work — must reach the render thread or the
            // frame is visibly broken.
            RenderCommand::FramePacket(_)
            | RenderCommand::Canvas2DBatch(_)
            | RenderCommand::Canvas2D { .. }
            | RenderCommand::GL(_) => CommandClass::Draw,

            // Resource / image / canvas lifecycle ops carry sync
            // responders; route them through the blocking sync
            // lane so the responder is guaranteed to arrive at
            // the render thread.
            RenderCommand::Canvas(_)
            | RenderCommand::LoadFont { .. }
            | RenderCommand::ReleaseOnscreen { .. } => CommandClass::Sync,
            RenderCommand::GetTextLineHeight { .. } => CommandClass::Sync,

            // Control plane — safe to race with shutdown.
            RenderCommand::FrameRate(_)
            | RenderCommand::Pause
            | RenderCommand::Resume
            | RenderCommand::Shutdown
            | RenderCommand::Invalidate
            | RenderCommand::SurfaceDestroyed { .. }
            | RenderCommand::TrimTextCache { .. } => CommandClass::Lifecycle,
        }
    }
}

/// Backpressure policy applied to a send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackpressurePolicy {
    /// Non-blocking `try_send`.  Drops on full.
    DropOnFull,
    /// Bounded blocking wait up to [`BLOCKING_SEND_DEADLINE`].
    BlockBounded,
}

/// Producer-side handle for sending render commands.
#[derive(Clone)]
pub struct CommandSender {
    inner: crossbeam_channel::Sender<RenderCommand>,
}

impl std::fmt::Debug for CommandSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandSender")
            .field("len", &self.inner.len())
            .finish()
    }
}

impl CommandSender {
    /// Create a new sender/receiver pair.
    ///
    /// Returns `(sender, cmd_rx)`:
    /// - `sender`: clone and distribute to producers
    /// - `cmd_rx`: the render thread receives from this in its `select!` loop
    pub fn new() -> (Self, crossbeam_channel::Receiver<RenderCommand>) {
        let (tx, rx) = crossbeam_channel::bounded(CHANNEL_CAPACITY);
        (Self { inner: tx }, rx)
    }

    /// Classify the command and apply the corresponding
    /// backpressure policy in one step.  This is the *preferred*
    /// entry point for producers because it guarantees non-
    /// idempotent draw work never gets silently dropped — only
    /// explicit lifecycle ops fall to the non-blocking path.
    ///
    /// The legacy `send()` / `send_blocking_bounded()` methods
    /// remain for call sites that have their own policy decision
    /// to make (e.g. shutdown path that deliberately prefers a
    /// dropped `Shutdown` over a blocked producer).
    pub fn dispatch(&self, cmd: RenderCommand) -> Result<(), SendError> {
        let class = cmd.class();
        match class.policy() {
            BackpressurePolicy::DropOnFull => self.send_lifecycle(cmd, class),
            BackpressurePolicy::BlockBounded => self.send_blocking(cmd, class),
        }
    }

    /// Non-blocking send used by the lifecycle path.  Drops on
    /// full (lifecycle commands are idempotent-enough that the
    /// next tick delivers the effect).
    ///
    /// Accepts a pre-computed [`CommandClass`] so the counter
    /// bookkeeping stays consistent with [`Self::dispatch`]'s view
    /// of the command.
    fn send_lifecycle(&self, cmd: RenderCommand, class: CommandClass) -> Result<(), SendError> {
        match self.inner.try_send(cmd) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                SEND_OVERFLOW.fetch_add(1, Ordering::Relaxed);
                match class {
                    CommandClass::Lifecycle => {
                        DROP_COUNTERS
                            .lifecycle_dropped
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    CommandClass::Draw => {
                        DROP_COUNTERS.draw_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    CommandClass::Sync => {
                        // A sync-class command lost on the legacy
                        // `send()` path would normally never exist
                        // — sync ops go through `dispatch()` /
                        // `send_blocking_bounded` — but callers
                        // that explicitly opt into non-blocking
                        // (e.g. test harnesses filling the queue)
                        // still deserve an accurate counter.
                        DROP_COUNTERS.sync_timeout.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(SendError::Overflow)
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => Err(SendError::Disconnected),
        }
    }

    /// Bounded-blocking send used by the draw / sync paths.
    /// Returns `Err(SendError::Timeout)` when the deadline expires
    /// and bumps the matching drop counter.
    fn send_blocking(&self, cmd: RenderCommand, class: CommandClass) -> Result<(), SendError> {
        match self.inner.send_timeout(cmd, BLOCKING_SEND_DEADLINE) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {
                SEND_OVERFLOW.fetch_add(1, Ordering::Relaxed);
                match class {
                    CommandClass::Draw => {
                        DROP_COUNTERS.draw_timeout.fetch_add(1, Ordering::Relaxed);
                    }
                    CommandClass::Sync => {
                        DROP_COUNTERS.sync_timeout.fetch_add(1, Ordering::Relaxed);
                    }
                    CommandClass::Lifecycle => {
                        DROP_COUNTERS
                            .lifecycle_dropped
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(SendError::Timeout)
            }
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                Err(SendError::Disconnected)
            }
        }
    }

    /// Legacy non-blocking send retained for shutdown / lifecycle
    /// paths that have an explicit reason to prefer a silent drop
    /// over any form of backpressure.  New code should prefer
    /// [`Self::dispatch`] so the policy follows the command class.
    pub fn send(&self, cmd: RenderCommand) -> Result<(), SendError> {
        let class = cmd.class();
        self.send_lifecycle(cmd, class)
    }

    /// Legacy bounded-blocking send retained for the sync-op
    /// fast path in `shared::protocol::send_render_with_resp_sync`
    /// / `send_render_with_resp_async` — those code paths still
    /// build the `RenderCommand` inside a closure and can't hand
    /// it to [`Self::dispatch`] without a trip through
    /// `RenderCommand::class`.  The old name is kept stable to
    /// minimise churn across crates.
    pub fn send_blocking_bounded(&self, cmd: RenderCommand) -> Result<(), SendError> {
        let class = cmd.class();
        self.send_blocking(cmd, class)
    }

    /// Current backlog depth (instantaneous).  Used by the render
    /// thread to publish `render_queue_len` to `DebugStats`; the
    /// Java debug overlay surfaces it as a live gauge.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Channel capacity, i.e. the point past which `send` drops
    /// and `send_blocking_bounded` waits.  Exposed so operators
    /// can show "N / cap" in diagnostics without hard-coding the
    /// constant in two places.
    #[inline]
    pub fn capacity(&self) -> usize {
        CHANNEL_CAPACITY
    }
}

/// Error type for the [`CommandSender`] send family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// Channel was full at send time; the command was dropped.
    /// The overflow counter has been incremented.
    Overflow,
    /// Bounded-blocking send exceeded its deadline.  The command
    /// was not enqueued; caller decides whether to retry.
    Timeout,
    /// The render thread has exited; no future send will succeed.
    Disconnected,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overflow => write!(f, "render command channel full; command dropped"),
            Self::Timeout => write!(f, "render command channel full for >8ms; giving up"),
            Self::Disconnected => write!(f, "render thread has exited"),
        }
    }
}
