//! Handing a render command to the render thread, from the producer's side.
//!
//! Split out of `protocol/mod.rs` rather than left there because these are the
//! only items in the protocol module that take a [`CanvasOpState`]. That type
//! holds `VirtualFS`, `MountTable` and `GamePaths` handles, so every one of them
//! dragged the whole virtual filesystem -- and, through it, a C compression
//! library -- into anything that merely wanted to *name* a render command. The
//! engine-free decoder names them and sends none, which is why the boundary is
//! drawn here: a vocabulary must not require the transport that carries it.
//!
//! The classification and timeout policy stays in the parent module. It answers
//! questions about op names and needs no host state, and both sides use it.

use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, bounded};
use tokio::{sync::oneshot, time::timeout};
use tracing::error;

use crate::error::{EngineError, ErrorCode};
use crate::op_state::CanvasOpState;
use crate::protocol::render_cmd::{GLCmd, RenderCmdResp, RenderCommand};

use super::{COMMAND_TIMEOUT_MS, MEASURE_TIMEOUT_MS, SyncOpClass, class_for_op};

/// What the thread does around a blocking wait for the renderer's answer.
///
/// A JavaScript thread's runtime installs these so the deadline it keeps for runaway script spends nothing while the
/// script waits on the renderer: a slow GPU -- a software renderer, a low-end phone compiling a game's shaders --
/// makes a synchronous call take seconds without the script running at all. The wait is bounded by its own timeout.
#[derive(Clone, Copy, Debug)]
pub struct BlockingWaitHooks {
    pub begin: fn(),
    pub end: fn(),
}

thread_local! {
    static BLOCKING_WAIT_HOOKS: Cell<Option<BlockingWaitHooks>> = const { Cell::new(None) };
}

/// Install this thread's [`BlockingWaitHooks`].
pub fn set_blocking_wait_hooks(hooks: BlockingWaitHooks) {
    BLOCKING_WAIT_HOOKS.with(|h| h.set(Some(hooks)));
}

/// One blocking wait: `begin` now, `end` when dropped.
#[must_use = "the wait ends when this is dropped"]
pub struct BlockingWait(Option<BlockingWaitHooks>);

impl BlockingWait {
    pub fn begin() -> Self {
        let hooks = BLOCKING_WAIT_HOOKS.with(Cell::get);
        if let Some(hooks) = hooks {
            (hooks.begin)();
        }
        Self(hooks)
    }
}

impl Drop for BlockingWait {
    fn drop(&mut self) {
        if let Some(hooks) = self.0 {
            (hooks.end)();
        }
    }
}

/// The op name every synchronous GL command travels under.
///
/// Lives here rather than beside `class_for_op` because it is what *this*
/// path passes; the classifier itself is vocabulary both sides share.
const OP_GL: &str = "gl command";

#[inline]
fn timeout_for_op(op: &str) -> Duration {
    match class_for_op(op) {
        SyncOpClass::Measure => Duration::from_millis(MEASURE_TIMEOUT_MS.load(Ordering::Relaxed)),
        SyncOpClass::Readback | SyncOpClass::Default => {
            Duration::from_millis(COMMAND_TIMEOUT_MS.load(Ordering::Relaxed))
        }
    }
}

#[inline(always)]
fn timeout_err(op: &'static str, to: Duration) -> EngineError {
    EngineError::from_detail(
        ErrorCode::Timeout,
        format!("{op} timed out (timeout={to:?})"),
    )
}

#[inline(always)]
fn disconnected_err(op: &'static str) -> EngineError {
    EngineError::from_detail(
        ErrorCode::Disconnected,
        format!("{op} failed: channel disconnected"),
    )
}

#[inline(always)]
fn send_err(op: &'static str, e: impl ToString) -> EngineError {
    EngineError::from_detail(
        ErrorCode::Internal,
        format!("{op} send failed: {}", e.to_string()),
    )
}

#[inline(always)]
fn canceled_err(op: &'static str) -> EngineError {
    EngineError::from_detail(
        ErrorCode::Timeout,
        format!("{op} failed: response channel canceled"),
    )
}

#[inline]
fn recv_timeout<T>(
    rx: &Receiver<Result<T, EngineError>>,
    op: &'static str,
) -> Result<T, EngineError> {
    let to = timeout_for_op(op);
    match rx.recv_timeout(to) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e),
        Err(RecvTimeoutError::Timeout) => {
            error!("{op} timed out (timeout={to:?})");
            Err(timeout_err(op, to))
        }
        Err(RecvTimeoutError::Disconnected) => {
            error!("{op} failed: channel disconnected");
            Err(disconnected_err(op))
        }
    }
}

#[inline]
async fn oneshot_timeout<T>(
    rx: oneshot::Receiver<Result<T, EngineError>>,
    op: &'static str,
) -> Result<T, EngineError> {
    let to = timeout_for_op(op);
    match timeout(to, rx).await {
        Ok(Ok(Ok(v))) => Ok(v),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_canceled)) => Err(canceled_err(op)),
        Err(_elapsed) => Err(timeout_err(op, to)),
    }
}

/// Fire-and-forget GL command.  Routed as a draw-class command
/// (`BackpressurePolicy::BlockBounded`) because silently dropping
/// a `bindBuffer` / `uniform*` / `drawElements` produces visible
/// glitches — the next frame's batch is not guaranteed to replay
/// the same mutator sequence.
pub fn send_gl(ctx: &CanvasOpState, cmd: GLCmd) {
    if let Err(e) = ctx.tx.dispatch(RenderCommand::GL(cmd)) {
        error!("send_gl failed: {e}");
    }
}

/// Send a render command with sync response (crossbeam).
///
/// Sync ops MUST be delivered (they're holding a reply channel
/// the caller is blocked on), so we use the bounded-blocking
/// variant that waits up to a hard deadline (~8 ms) before
/// erroring out.  A full-channel error surfaces as a reply
/// timeout in the caller; without this bound a command storm
/// could freeze the host indefinitely.
pub fn send_render_with_resp_sync<T>(
    ctx: &CanvasOpState,
    op: &'static str,
    build: impl FnOnce(RenderCmdResp<T>) -> RenderCommand,
) -> Result<T, EngineError> {
    let (resp_tx, resp_rx) = bounded(1);
    let wait = BlockingWait::begin();

    if let Err(e) = ctx
        .tx
        .send_blocking_bounded(build(RenderCmdResp::from_sync(resp_tx)))
    {
        return Err(send_err(op, e));
    }

    let started_at = std::time::Instant::now();
    let result = recv_timeout(&resp_rx, op);
    drop(wait);
    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    if elapsed_ms >= 5 {
        // Sync GL ops block V8; if the wait crosses 5 ms it is
        // almost certainly a head-of-line stall behind image
        // uploads or a heavy GL command queued earlier in the
        // batch.  Surface it so logcat can correlate with
        // [MigoPerf] readFile spikes.
        tracing::warn!("[MigoPerf][SyncOp] {op} blocked V8 {elapsed_ms}ms");
    }
    result
}

/// Send a render command with async response (oneshot) + timeout.
///
/// Like the sync variant, async sync-ops hold a reply channel and
/// must be delivered.  Use the bounded-blocking sender so a full
/// channel escalates to a timeout instead of an indefinite wait.
pub async fn send_render_with_resp_async<T>(
    ctx: &CanvasOpState,
    op: &'static str,
    build: impl FnOnce(RenderCmdResp<T>) -> RenderCommand,
) -> Result<T, EngineError> {
    let (resp_tx, resp_rx) = oneshot::channel();

    if let Err(e) = ctx
        .tx
        .send_blocking_bounded(build(RenderCmdResp::from_async(resp_tx)))
    {
        return Err(send_err(op, e));
    }

    oneshot_timeout(resp_rx, op).await
}

/// Send a GL command with sync response.
pub fn send_gl_with_resp_sync<T>(
    ctx: &CanvasOpState,
    build: impl FnOnce(RenderCmdResp<T>) -> RenderCommand,
) -> Result<T, EngineError> {
    send_render_with_resp_sync(ctx, OP_GL, build)
}

/// Send a GL command with async response (Promise-style).
pub async fn send_gl_with_resp_async<T>(
    ctx: &CanvasOpState,
    build: impl FnOnce(RenderCmdResp<T>) -> RenderCommand,
) -> Result<T, EngineError> {
    send_render_with_resp_async(ctx, OP_GL, build).await
}

#[cfg(test)]
mod tests {
    use super::{OP_GL, SyncOpClass, class_for_op, timeout_for_op};
    use std::time::Duration;

    /// The two classes must keep mapping to different deadlines, or the
    /// classification stops deciding anything.
    ///
    /// Note what this test deliberately does *not* claim. It cannot check that
    /// any real op's name lands in the right class, because the names are
    /// declared in `runtime-v8` and this crate is upstream of it. A first
    /// version of this test hardcoded the name strings and asserted their
    /// classes — and passed unchanged when `OP_MEASURE_TEXT` was renamed, which
    /// is precisely the failure it was written to catch. The duplicated literals
    /// made divergence invisible rather than loud.
    ///
    /// That check now lives where the constants do:
    /// `context2d::tests::each_sync_op_name_still_selects_the_deadline_its_op_needs`.
    #[test]
    fn the_two_deadline_classes_stay_far_apart() {
        let measure = timeout_for_op("canvas2d measure_text");
        assert_eq!(class_for_op("canvas2d measure_text"), SyncOpClass::Measure);
        assert!(
            measure < Duration::from_millis(17),
            "the measure deadline ({measure:?}) must stay inside a 60 Hz frame, \
             or a stalled measureText becomes a visible hitch rather than a \
             sub-frame one"
        );
        assert!(
            timeout_for_op(OP_GL) > measure * 100,
            "the classes have converged; classifying an op no longer changes \
             its deadline"
        );
    }

    /// The two `Readback` substrings for shader/program info logs are
    /// unreachable, and that is a fact about the code rather than a wish.
    ///
    /// Both ops exist — `GLCmd::GetShaderInfoLog` and `GetProgramInfoLog` carry
    /// a `resp` — but they travel through `send_gl_with_resp_sync`, which names
    /// every synchronous GL op `OP_GL`. Nothing ever passes a string containing
    /// `get_shader_info_log`, so the arm cannot fire.
    ///
    /// Asserted rather than deleted: the arms are harmless, they document an
    /// intent, and they become live the moment someone threads a real name
    /// through the GL sync path. What is worth pinning is that today they do
    /// *not* fire, so nobody reads the classification as evidence that those two
    /// ops are treated separately — they are not.
    #[test]
    fn the_shader_info_log_readback_arms_are_unreachable_through_the_gl_sync_path() {
        // The arms work when given a matching name...
        assert_eq!(
            class_for_op("get_shader_info_log"),
            SyncOpClass::Readback,
            "the arm itself is broken, which is a different bug"
        );
        assert_eq!(class_for_op("get_program_info_log"), SyncOpClass::Readback);

        // ...but the name the GL sync path actually passes is not one of them,
        // so every synchronous GL op — including those two — is `Default`.
        assert_eq!(class_for_op(OP_GL), SyncOpClass::Default);
        assert!(
            !OP_GL.contains("get_shader_info_log") && !OP_GL.contains("get_program_info_log"),
            "OP_GL now names an info-log op; the arms above have become live \
             and this test should be replaced by one covering the new mapping"
        );
    }
}
