//! One-shot framebuffer capture for the headless dev player (offscreen PNG).
//!
//! Correctness first: the render thread reads the default framebuffer (FBO 0)
//! **after** the DrawingBuffer→surface blit and **before** `eglSwapBuffers`,
//! while the onscreen GL context is current and the back buffer is still valid
//! — so the captured pixels are exactly what is about to be presented,
//! independent of the game's internals.
//!
//! Performance: the render thread pays two atomic loads per presented frame
//! (`pending_seq`); the `glReadPixels` cost is incurred once, when a capture has
//! actually been requested.
//!
//! This is a diagnostic/dev-tool hook (the player). It never runs unless
//! `request()` is called, so it has no effect on shipping render behaviour.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use glow::HasContext;

/// Which request the render thread is capturing for, and which one the consumer
/// has already taken. A capture is pending while `REQUESTED > TAKEN`.
///
/// Generations rather than a flag, because "capture the next presented frame" is
/// a claim no earlier frame may satisfy. A resize acceptance run requests twice
/// -- once at start-up and once after the transition -- and under a flag the
/// second request was already satisfied by the frame the first left in the slot,
/// so a resize that never presented wrote the pre-resize picture and exited
/// successfully. Clearing the slot inside `request` would only narrow that,
/// since a capture already between its `glReadPixels` and its store still lands
/// afterwards; stamping each frame with the request it answers closes it.
///
/// `REQUESTED` only ever increases, so a generation is never reused. A single
/// counter reset to zero by `take` would hand the next cycle a number an
/// in-flight capture from the previous one had already used, and that frame
/// landing late would then out-rank -- and permanently suppress -- every frame
/// the new cycle presents.
static REQUESTED: AtomicU64 = AtomicU64::new(0);
static TAKEN: AtomicU64 = AtomicU64::new(0);
static RESULT: Mutex<Option<(u64, CapturedFrame)>> = Mutex::new(None);

/// A captured frame. `rgba_bottom_up` is tightly packed RGBA8 in GL row order
/// (bottom-up); consumers flip to top-down for PNG.
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba_bottom_up: Vec<u8>,
}

/// Request a one-shot capture of the next presented frame.
///
/// Called again before [`take`] it supersedes the earlier request: from then on
/// only a newly presented frame can satisfy it.
pub fn request() {
    REQUESTED.fetch_add(1, Ordering::AcqRel);
}

/// Stop capturing and take the most recent frame presented since the latest
/// [`request`]. A frame that answered an earlier request is discarded rather
/// than returned, so a run that presented nothing after its last request gets
/// `None` instead of a stale picture.
pub fn take() -> Option<CapturedFrame> {
    let wanted = REQUESTED.load(Ordering::Acquire);
    // Marks every generation up to `wanted` as consumed, which is what stops the
    // render thread capturing without ever letting a generation repeat.
    TAKEN.store(wanted, Ordering::Release);
    let captured = RESULT.lock().expect("frame_capture result mutex").take();
    captured
        .filter(|(seq, _)| *seq == wanted)
        .map(|(_, frame)| frame)
}

/// The generation a capture would answer, or `None` when none is pending.
#[inline]
fn pending_seq() -> Option<u64> {
    let requested = REQUESTED.load(Ordering::Acquire);
    (requested > TAKEN.load(Ordering::Acquire)).then_some(requested)
}

/// Read FBO 0 into the result slot while a capture is pending, overwriting any
/// previous frame so the slot always holds the latest presented frame. MUST be
/// called on the render thread with the onscreen context current, after the
/// final blit and before `eglSwapBuffers`. On the common path (no capture
/// requested — e.g. every shipping Android frame) it costs only two atomic loads.
pub(crate) fn capture_default_fbo(gl: &glow::Context, width: u32, height: u32) {
    let Some(seq) = pending_seq() else {
        return;
    };
    if width == 0 || height == 0 {
        return;
    }
    let Ok(readback) = crate::backend::gl::readback::Rgba8Readback::new(width, height) else {
        return;
    };
    let buf = unsafe {
        // Read from the presented surface (default framebuffer), not the
        // DrawingBuffer that the blit left bound as the read target -- and put
        // that binding back, because this is an engine write to state the content
        // owns. The dedup shadow still claims whatever the read target was, so a
        // capture that walked away from FBO 0 would have the content's next
        // `bindFramebuffer(READ_FRAMEBUFFER, sameName)` deduped against a claim
        // the driver no longer honours, and the read would come from the window
        // instead of the content's own framebuffer. Restoring keeps the shadow
        // true by construction, which is what the sibling paths in
        // `drawing_buffer` do with the texture and renderbuffer bindings.
        let previous_read = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING) as u32;
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        let buf = readback.read(gl);
        let restored = std::num::NonZeroU32::new(previous_read).map(glow::NativeFramebuffer);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, restored);
        buf
    };
    let mut slot = RESULT.lock().expect("frame_capture result mutex");
    // A request that arrived while this frame was being read owns the slot now:
    // this frame predates it, and is exactly the stale capture `take` refuses.
    if slot.as_ref().is_some_and(|(stored, _)| *stored > seq) {
        return;
    }
    *slot = Some((
        seq,
        CapturedFrame {
            width,
            height,
            rgba_bottom_up: buf,
        },
    ));
    // Intentionally do NOT clear the request here: keep the latest frame until
    // the consumer calls take(), so blank warmup frames are replaced by content.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::gl::readback_test_gl as test_gl;

    #[test]
    fn internal_readback_capture_uses_compact_pack_and_restores_content_state() {
        let gl = test_gl::context();
        let original = test_gl::Bindings {
            pack: [8, 9, 2, 3],
            pack_buffer: 17,
            read_framebuffer: 23,
            ..Default::default()
        };
        test_gl::set_bindings(original);
        request();
        capture_default_fbo(&gl, 8193, 1);
        assert!(take().is_none());
        assert!(test_gl::reads().is_empty());
        assert_eq!(
            test_gl::mutations(),
            0,
            "reject before binding a framebuffer"
        );
        request();
        capture_default_fbo(&gl, 3, 2);
        let frame = take().expect("requested frame");
        let reads = test_gl::reads();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].bindings.pack, [1, 0, 0, 0]);
        assert_eq!(reads[0].bindings.pack_buffer, 0);
        assert_eq!(reads[0].bindings.read_framebuffer, 0);
        assert_eq!(test_gl::bindings(), original);
        assert_eq!(
            (frame.width, frame.height, frame.rgba_bottom_up.len()),
            (3, 2, 24)
        );
    }
}
