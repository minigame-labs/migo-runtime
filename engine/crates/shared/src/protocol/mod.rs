use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub mod ahb;
pub mod audio_cmd;
pub mod camera_frame;
pub mod canvas_id_set;
pub mod color;
pub mod error;
pub mod frame_packet;
pub mod host_cmd;
pub mod io_cmd;
pub mod pixel_pack;
pub mod render_cmd;
// The producer-side send helpers. Behind `vfs` because they take a
// `CanvasOpState`; see that module's own header for why naming a command must
// not require the transport that carries it.
#[cfg(feature = "vfs")]
pub mod send;

pub use self::{
    canvas_id_set::CanvasIdSet,
    frame_packet::{FrameOp, FrameOps, FramePacket, FramePacketBuilder},
    render_cmd::{CanvasBatchPayload, DirtyRect, GlBatchPayload},
};
// Re-exported at the old path so every call site keeps saying
// `shared::protocol::send_gl`; the move is a layering fact, not an API change.
#[cfg(feature = "vfs")]
pub use self::send::{
    send_gl, send_gl_with_resp_async, send_gl_with_resp_sync, send_render_with_resp_async,
    send_render_with_resp_sync,
};

/// Default timeout for readback-class sync ops: 10 seconds.
/// Used for `GetImageData`, `ReadPixels`, shader info logs, etc.
/// that can legitimately take a while on slow drivers.
static COMMAND_TIMEOUT_MS: AtomicU64 = AtomicU64::new(10_000);

/// Stricter deadline for latency-sensitive measure-class ops.
///
/// `measureText` and `GetTextLineHeight` are called hundreds of
/// times per frame from UI code auto-sizing labels; a render
/// thread stall should surface as a conservative fallback in JS
/// within a couple of milliseconds rather than eating the full
/// 10 s budget and freezing the whole tick.  Keep it shorter
/// than a 60 Hz frame (16.7 ms) so an overrun is observable as a
/// sub-frame hiccup instead of a visible one.
static MEASURE_TIMEOUT_MS: AtomicU64 = AtomicU64::new(4);

/// Classify a sync op for timeout selection.
///
/// Declared `pub` so downstream crates (e.g. `runtime-v8`)
/// can tag their own op names without threading through
/// `shared::protocol` internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOpClass {
    /// Measurement / layout — must return fast for interactive
    /// UI; failure degrades to a conservative JS-side estimate.
    Measure,
    /// Readback / creation — can legitimately take longer
    /// (`glReadPixels` a full-screen readback, shader link info).
    Readback,
    /// Legacy / unclassified — uses the readback deadline.
    Default,
}

/// Pick a deadline class from an op's name.
///
/// # The mechanism is a substring match, with two consequences worth knowing
///
/// **A rename is a silent 2500x change.** The names are `'static` literals
/// declared next to their ops, so editing `OP_MEASURE_TEXT`'s string — a
/// reasonable-looking tidy-up — moves `measureText` from the 4 ms deadline to
/// the 10 s one, and nothing fails. `every_sync_op_name_lands_in_its_intended_
/// deadline_class` pins the current mapping for exactly that reason.
///
/// **It cannot distinguish the synchronous GL ops, because they share one
/// name.** All 18 `GLCmd` variants that carry a `resp` — `GetParameter`,
/// `CheckFramebufferStatus`, `ClientWaitSync`, `GetQueryParameter`,
/// `GetShaderInfoLog`, `GetProgramInfoLog` and the rest — travel through
/// `send_gl_with_resp_sync`, which passes the single name [`OP_GL`]
/// (`"gl command"`). So they all land on `Default`, and:
///
/// * The `get_shader_info_log` / `get_program_info_log` arms below are
///   unreachable. No op is named either of those; they are waiting for a name
///   that is never passed.
/// * `ClientWaitSync` and `GetQueryParameter` are *designed* to be polled every
///   frame — a fence probe and a timer/occlusion query respectively — and yet
///   they get the 10 s deadline. That is at odds with the policy
///   [`MEASURE_TIMEOUT_MS`] states for per-frame ops: surface a render-thread
///   stall "within a couple of milliseconds rather than eating the full 10 s
///   budget and freezing the whole tick".
///
/// The deadline never fires in normal operation — a healthy round-trip is
/// sub-millisecond, and `send_render_with_resp_sync` already warns past 5 ms.
/// What it bounds is the damage when the render thread is wedged, and for a
/// fence poll that bound is currently 10 s of frozen JS.
///
/// **Not changed here, deliberately.** Shortening it means deciding what a
/// spuriously-failed `clientWaitSync` does to a game's async-readback logic,
/// which is a product call wanting a device measurement, not a constant edit.
/// Making the classification *able* to tell these ops apart is a small change —
/// thread the name through `send_gl_with_resp_sync` — but on its own it alters
/// nothing except log text, so it belongs with the decision rather than before
/// it.
/// `pub` for the same reason [`SyncOpClass`] is: a downstream crate declares
/// the op names, so it is the only place that can check one lands in the class
/// its op needs. Exposing the type without the classifier left that
/// unverifiable — see
/// `context2d::tests::each_sync_op_name_still_selects_the_deadline_its_op_needs`.
pub fn class_for_op(op: &str) -> SyncOpClass {
    // Cheap prefix match — op names are `'static` string
    // constants so the comparison compiles to byte-tests.  Kept
    // here (not on the op site) so we don't have to touch every
    // `send_render_with_resp_*` call to annotate with a class.
    if op.contains("measure_text") || op.contains("get_text_line_height") {
        SyncOpClass::Measure
    } else if op.contains("get_image_data")
        || op.contains("read_pixels")
        || op.contains("get_shader_info_log")
        || op.contains("get_program_info_log")
    {
        SyncOpClass::Readback
    } else {
        SyncOpClass::Default
    }
}

#[inline]
pub fn command_timeout() -> Duration {
    Duration::from_millis(COMMAND_TIMEOUT_MS.load(Ordering::Relaxed))
}

/// Allows caller to tune command timeout globally.
pub fn set_command_timeout(dur: Duration) {
    // Avoid 0ms timeout that can cause flakiness.
    let ms = dur.as_millis().max(1) as u64;
    COMMAND_TIMEOUT_MS.store(ms, Ordering::Relaxed);
}

/// Override the measure-class deadline.  Exposed so load-testing
/// code can shorten it to amplify backpressure, or widen it for
/// older devices where the render thread is naturally slower.
pub fn set_measure_timeout(dur: Duration) {
    let ms = dur.as_millis().max(1) as u64;
    MEASURE_TIMEOUT_MS.store(ms, Ordering::Relaxed);
}
