//! Render-thread → host event feedback channel.
//!
//! The render thread runs autonomously on its own OS thread and historically
//! only communicated failures through `tracing::error!()` — JS callers never
//! learned about `swap_buffers` failures, EGL context loss, RAF backpressure,
//! or command errors.  Real-world symptoms (a black screen, a hung UI) were
//! indistinguishable from each other because the diagnostic never crossed the
//! thread boundary.
//!
//! This module exposes a small structured event type and a one-to-many
//! broadcast-ish channel (bounded crossbeam MPMC) that the render thread
//! pushes events onto and any number of host-side consumers can drain.  The
//! channel is intentionally unidirectional and lossy-by-bound: if consumers
//! fall behind, the render thread never stalls — it drops the oldest event
//! and bumps a counter (see [`RenderEventSender::dropped`]).
//!
//! The event shape is kept intentionally small (no large payloads) so that
//! forwarding into JS can be a pure data copy instead of an allocation.
//! The consumer (typically the `core` runtime) translates the variant into
//! a JS-visible error event or a `performance.mark` entry as it sees fit.
//!

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

use crate::error::ErrorCode;

/// Maximum pending events the render thread can buffer.  A small
/// number is fine because events are advisory — dropping an older
/// one only costs telemetry detail.  The bound also guarantees the
/// render thread's [`RenderEventSender::emit`] path is
/// constant-time: either the slot is free and we push, or we drop
/// the oldest in place.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Structured render-thread event delivered to the host.
///
/// Intentionally `#[non_exhaustive]` — producers may add variants
/// without a semver break; consumers must always have a `_` arm.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum RenderEvent {
    /// A `Canvas2DBatch` / single `Canvas2D` command failed on
    /// the render thread after the dispatcher swallowed it.
    /// Carries the engine error code so JS can react
    /// selectively (e.g. re-raise only on `RenderBackendError`).
    Canvas2DError { code: ErrorCode, message: String },
    /// A `GL` / `GLBatch` command failed.
    GlError { code: ErrorCode, message: String },
    /// A `CanvasCmd` (create / destroy / resize / recreate)
    /// failed.  Usually downstream of an EGL / surface issue.
    CanvasError { code: ErrorCode, message: String },
    /// `eglSwapBuffers` returned an error that is **not**
    /// `EGL_CONTEXT_LOST`.  Recoverable by the render loop on
    /// its own; included in the channel so overlays / tests can
    /// observe the rate.
    SwapFailed { message: String },
    /// `EGL_CONTEXT_LOST` seen during swap.  A recovery attempt
    /// will be made at the top of the next frame loop.
    ContextLost,
    /// An EGL context recovery attempt completed with the given
    /// outcome.  Emitted exactly once per attempt.
    ContextRecovered { success: bool },
    /// RAF delivery has been dropped for ≥ 3 consecutive frames.
    /// Signals to the JS scheduler that the host event loop is
    /// saturated and probably should not issue more work until
    /// the overlay clears.
    RafBackpressure { consecutive_drops: u32 },
}

impl RenderEvent {
    /// Short identifier used in string logging; kept stable for
    /// tests asserting event shapes without mocking variants.
    pub fn kind(&self) -> &'static str {
        match self {
            RenderEvent::Canvas2DError { .. } => "canvas2d_error",
            RenderEvent::GlError { .. } => "gl_error",
            RenderEvent::CanvasError { .. } => "canvas_error",
            RenderEvent::SwapFailed { .. } => "swap_failed",
            RenderEvent::ContextLost => "context_lost",
            RenderEvent::ContextRecovered { .. } => "context_recovered",
            RenderEvent::RafBackpressure { .. } => "raf_backpressure",
        }
    }
}

/// Sender half of the render event channel.  Cheap to `Clone`; the
/// render thread holds the canonical instance and each `emit` call
/// is O(1) even when the consumer is backed up.
#[derive(Clone)]
pub struct RenderEventSender {
    tx: Sender<RenderEvent>,
    /// Counter of events that were dropped because the channel
    /// was full at emit time.  Useful to distinguish "no events"
    /// from "consumer drowning".
    dropped: Arc<AtomicU64>,
    /// Optional wake callback invoked after each *successfully* enqueued event.
    /// The host installs a `tokio::sync::Notify`-backed closure so render
    /// feedback is delivered without a polling timer; `None` for the no-wake
    /// test/default constructor.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl RenderEventSender {
    /// Push an event.  On full channel, drops the NEW event and
    /// bumps the internal counter — the consumer keeps seeing the
    /// older (still more informative) events.  Returning `()`
    /// keeps call sites terse and signals that the send is
    /// advisory.
    ///
    /// The wake callback (when present) fires only on a successful enqueue, so a
    /// full/disconnected send never claims a spurious wake.
    pub fn emit(&self, ev: RenderEvent) {
        match self.tx.try_send(ev) {
            Ok(()) => {
                if let Some(wake) = &self.wake {
                    wake();
                }
            }
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                // Receiver went away.  Record the drop but
                // otherwise ignore — the render thread outlives
                // the last consumer during shutdown.
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Cumulative number of events that were dropped because the
    /// consumer was not reading fast enough.  Monotonic.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Receiver half.  `Clone`able so multiple subscribers (e.g. JS
/// runtime + Java overlay bridge) can drain concurrently — events
/// are delivered to whichever consumer calls `try_recv` first, so
/// the intended deployment is one consumer per engine host.  The
/// design deliberately picks MPMC-crossbeam semantics over proper
/// multi-subscribe broadcast to keep the backend dependency
/// surface small; upstream projects that actually need fan-out can
/// layer a broadcast adapter on top.
pub type RenderEventReceiver = Receiver<RenderEvent>;

/// Construct a new `(sender, receiver)` pair with no wake callback.  Used by
/// tests and any host that drains on its own schedule.
pub fn channel() -> (RenderEventSender, RenderEventReceiver) {
    let (tx, rx) = bounded(EVENT_CHANNEL_CAPACITY);
    (
        RenderEventSender {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            wake: None,
        },
        rx,
    )
}

/// Construct a `(sender, receiver)` pair whose sender invokes `wake` after each
/// successfully enqueued event. The host installs a `tokio::sync::Notify`-backed
/// closure so every Canvas/GL/swap/RAF-backpressure/context event is delivered
/// promptly without a polling timer; `Notify` coalesces bursts to one permit.
pub fn channel_with_wake(
    wake: Arc<dyn Fn() + Send + Sync>,
) -> (RenderEventSender, RenderEventReceiver) {
    let (tx, rx) = bounded(EVENT_CHANNEL_CAPACITY);
    (
        RenderEventSender {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            wake: Some(wake),
        },
        rx,
    )
}

#[cfg(test)]
mod tests {
    use super::{EVENT_CHANNEL_CAPACITY, RenderEvent, channel_with_wake};
    use crate::error::ErrorCode;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Poll `Notify::notified()` once with a no-op waker; returns whether a
    /// permit was present (and consumes it if so).
    fn took_permit(notify: &tokio::sync::Notify) -> bool {
        use std::task::{Context, Poll};
        let fut = notify.notified();
        let mut fut = std::pin::pin!(fut);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        matches!(
            std::future::Future::poll(fut.as_mut(), &mut cx),
            Poll::Ready(())
        )
    }

    fn all_variants() -> Vec<RenderEvent> {
        vec![
            RenderEvent::Canvas2DError {
                code: ErrorCode::RenderBackendError,
                message: "x".into(),
            },
            RenderEvent::GlError {
                code: ErrorCode::RenderBackendError,
                message: "x".into(),
            },
            RenderEvent::CanvasError {
                code: ErrorCode::RenderBackendError,
                message: "x".into(),
            },
            RenderEvent::SwapFailed {
                message: "x".into(),
            },
            RenderEvent::ContextLost,
            RenderEvent::ContextRecovered { success: true },
            RenderEvent::RafBackpressure {
                consecutive_drops: 3,
            },
        ]
    }

    #[test]
    fn every_variant_wakes_after_a_successful_enqueue() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let (tx, rx) = channel_with_wake(Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        let variants = all_variants();
        let n = variants.len();
        for (i, ev) in variants.into_iter().enumerate() {
            tx.emit(ev);
            assert_eq!(
                count.load(Ordering::SeqCst),
                i + 1,
                "each successful enqueue must signal exactly once"
            );
        }
        let mut drained = 0;
        while rx.try_recv().is_ok() {
            drained += 1;
        }
        assert_eq!(drained, n, "all events delivered");
        assert_eq!(tx.dropped(), 0);
    }

    #[test]
    fn burst_coalesces_via_a_notify_closure() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let n = Arc::clone(&notify);
        let (tx, _rx) = channel_with_wake(Arc::new(move || {
            n.notify_one();
        }));
        tx.emit(RenderEvent::ContextLost);
        tx.emit(RenderEvent::ContextLost);
        tx.emit(RenderEvent::ContextLost);
        assert!(
            took_permit(&notify),
            "the first signal of the burst is delivered"
        );
        assert!(
            !took_permit(&notify),
            "a Notify closure coalesces the burst to a single permit"
        );
    }

    #[test]
    fn full_channel_is_non_blocking_and_claims_no_wake() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        // Keep the receiver alive so overflow is Full (not Disconnected).
        let (tx, _rx) = channel_with_wake(Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        for _ in 0..EVENT_CHANNEL_CAPACITY {
            tx.emit(RenderEvent::ContextLost);
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            EVENT_CHANNEL_CAPACITY,
            "each successful enqueue woke once"
        );
        for _ in 0..10 {
            tx.emit(RenderEvent::ContextLost);
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            EVENT_CHANNEL_CAPACITY,
            "a full channel drops without claiming a wake"
        );
        assert_eq!(tx.dropped(), 10, "dropped-event count is preserved");
    }

    #[test]
    fn disconnected_send_does_not_wake() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let (tx, rx) = channel_with_wake(Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        drop(rx);
        tx.emit(RenderEvent::ContextLost);
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "a disconnected send must not wake"
        );
        assert_eq!(tx.dropped(), 1, "dropped-event count is preserved");
    }
}
