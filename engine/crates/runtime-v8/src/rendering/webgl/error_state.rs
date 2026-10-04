//! Per-context WebGL error queue + cached context attributes.
//!
//! The Khronos WebGL 1.0 spec (§5.14.3) requires `getError()` to
//! return errors from a queue — the spec explicitly says
//! "implementations should track and return errors as they occur",
//! not "always return NO_ERROR".  Our previous stub returned `0`
//! unconditionally, which hid misuse bugs in games and misreported
//! WebGL conformance.  Firefox's `WebGLContextGL::GetError` maintains
//! a "webgl-side" error list that drains BEFORE consulting the
//! driver's `glGetError()`, so validation-stage errors don't get lost
//! behind a downstream driver error.
//!
//! We mirror that two-level design here, but only the host-side
//! queue is populated today: validation ops that detect an illegal
//! enum / operation / value before dispatch push the WebGL error
//! code into this queue; an optional driver-side drain (via a
//! future synchronous op against `gl.get_error()`) can be layered
//! on later without JS-facing changes.
//!
//! Error codes are raw u32 values matching the WebGL constants:
//! `INVALID_ENUM = 0x0500`, `INVALID_VALUE = 0x0501`,
//! `INVALID_OPERATION = 0x0502`, `OUT_OF_MEMORY = 0x0505`,
//! `INVALID_FRAMEBUFFER_OPERATION = 0x0506`, `CONTEXT_LOST_WEBGL = 0x9242`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;

use deno_core::OpState;
use shared::op_state::HostOpState;

/// Pushed by host-side validators; drained by `op_get_error`.
///
/// Separate `HashMap<canvas_id, queue>` (instead of one global queue)
/// because each WebGL context has its own error queue in the spec:
/// `getError()` on `ctxA` must not return errors that originated on
/// `ctxB`.
#[derive(Default)]
pub struct WebGLErrorState {
    queues: HashMap<u32, VecDeque<u32>>,
    /// Cached context attributes per canvas.  Returned as-is by
    /// `getContextAttributes()`.
    attrs: HashMap<u32, ContextAttributes>,
    /// Per-context transform feedback lifecycle.  Used by host-side
    /// validators for `bindBufferBase/Range`.
    transform_feedback: HashMap<u32, TransformFeedback>,
}

/// Where a context's transform feedback object is in its lifecycle.
///
/// One value rather than an `active` bit plus a `paused` bit, because
/// "paused but not active" is not a state WebGL2 has and a pair of bools
/// can spell it. The distinction is load-bearing: WebGL2 §3.7.15 refuses
/// `bindBufferBase`/`bindBufferRange` on `TRANSFORM_FEEDBACK_BUFFER` only
/// while feedback is active **and not paused** — rebinding the feedback
/// buffers is the reason `pauseTransformFeedback` exists.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TransformFeedback {
    #[default]
    Inactive,
    Active,
    Paused,
}

/// Mirror of WebGLContextAttributes IDL dictionary.  Values are the
/// *actual* parameters the runtime chose, so JS gets truthful info
/// (not the defaults it might have requested).
#[derive(Clone, Copy, Debug)]
pub struct ContextAttributes {
    pub alpha: bool,
    pub antialias: bool,
    pub depth: bool,
    pub stencil: bool,
    pub premultiplied_alpha: bool,
    pub preserve_drawing_buffer: bool,
    /// `"default" | "high-performance" | "low-power"` — stored as a
    /// small enum to keep the op table lean.
    pub power_preference: PowerPreference,
    pub fail_if_major_performance_caveat: bool,
    pub desynchronized: bool,
    pub xr_compatible: bool,
}

impl Default for ContextAttributes {
    /// What a context created with no attributes has: the WebGL 1.0 defaults (5.2), which the drawing buffer honours
    /// (a stencil buffer only when asked for), except antialiasing, which this implementation declines -- its drawing
    /// buffer is single-sampled -- and so reports as off.
    fn default() -> Self {
        Self {
            alpha: true,
            antialias: false,
            depth: true,
            stencil: false,
            premultiplied_alpha: true,
            preserve_drawing_buffer: false,
            power_preference: PowerPreference::Default,
            fail_if_major_performance_caveat: false,
            desynchronized: false,
            xr_compatible: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PowerPreference {
    Default,
    HighPerformance,
    LowPower,
}

impl PowerPreference {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::HighPerformance => "high-performance",
            Self::LowPower => "low-power",
        }
    }
}

impl WebGLErrorState {
    /// Record a WebGL error code for `canvas_id`.
    ///
    /// GL's errors are flags, one per code (ES 3.0 2.5): a code already
    /// recorded and not yet read by `getError` is not recorded again, so
    /// one `getError` clears what any number of calls raised. Chrome keeps
    /// its synthetic errors the same way, and Firefox keeps only the first.
    /// Codes of different kinds are each held, oldest first. Held this way the
    /// queue is as long as the codes there are, so a script that raises errors
    /// and never reads them cannot grow it.
    pub fn push(&mut self, canvas_id: u32, code: u32) {
        let queue = self.queues.entry(canvas_id).or_default();
        if !queue.contains(&code) {
            queue.push_back(code);
        }
    }

    /// Drain the oldest error for `canvas_id`, or return
    /// `NO_ERROR (0)` when the queue is empty.
    pub fn drain_one(&mut self, canvas_id: u32) -> u32 {
        self.queues
            .get_mut(&canvas_id)
            .and_then(|q| q.pop_front())
            .unwrap_or(0)
    }

    /// Query current queue depth.  Used in tests; production code
    /// should use `drain_one` + comparison.
    #[allow(dead_code)]
    pub fn len(&self, canvas_id: u32) -> usize {
        self.queues.get(&canvas_id).map_or(0, |q| q.len())
    }

    pub fn set_attrs(&mut self, canvas_id: u32, attrs: ContextAttributes) {
        self.attrs.insert(canvas_id, attrs);
    }

    pub fn get_attrs(&self, canvas_id: u32) -> Option<ContextAttributes> {
        self.attrs.get(&canvas_id).copied()
    }

    pub fn set_transform_feedback(&mut self, canvas_id: u32, phase: TransformFeedback) {
        self.transform_feedback.insert(canvas_id, phase);
    }

    /// Whether feedback is capturing right now, which is the only phase that
    /// refuses a rebind of the feedback buffers.
    pub fn transform_feedback_captures(&self, canvas_id: u32) -> bool {
        self.transform_feedback
            .get(&canvas_id)
            .copied()
            .unwrap_or_default()
            == TransformFeedback::Active
    }
}

/// WebGL error codes — mirror of the GL ES / WebGL constants so
/// call sites can cite them by name rather than magic numbers.
/// The values are stable across WebGL 1.0 / 2.0.
///
/// `#[allow(dead_code)]`: the full set is the contract between
/// host-side validators and JS; current callers only emit
/// `INVALID_ENUM` / `INVALID_VALUE` / `INVALID_OPERATION`, but the
/// rest are the public constants future validators will reach for
/// — removing them just to satisfy the lint would force a
/// rebuild-and-rename every time a new validator is added.
#[allow(dead_code)]
/// Re-exported rather than redefined. These are the values `gl.getError()`
/// returns to content, and the decoder that produces them lives in
/// `frame-decode`; two copies would be two places for a GL enum to be wrong.
pub use frame_decode::codes;

/// Convenience for host-side validators: push a WebGL error code
/// without needing to thread `WebGLErrorState` manually.
#[inline]
pub fn push_error(state: &mut OpState, canvas_id: u32, code: u32) {
    let q = state.borrow_mut::<WebGLErrorState>();
    q.push(canvas_id, code);
}

#[inline]
pub fn set_transform_feedback(state: &mut OpState, canvas_id: u32, phase: TransformFeedback) {
    let q = state.borrow_mut::<WebGLErrorState>();
    q.set_transform_feedback(canvas_id, phase);
}

// `transform_feedback_captures` and the two buffer-target constants moved with
// the validators: they existed only to serve them, and leaving a second copy
// here would be a second definition of a GL enum.

// ---- The decoder's view of this runtime -----------------------------
//
// The validators moved to `frame-decode`, which links no JavaScript engine.
// They are parameter checks, and the only things they needed from here were
// somewhere to put a WebGL error and one piece of GL state. This is that
// somewhere.
//
// The wrappers below keep the `&mut OpState` signatures the raw op handlers in
// `webgl.rs` already call, so moving the checks changed no call site. A caller
// that has to learn a new shape is a caller that gets rewritten under time
// pressure, and this move is meant to be invisible to the path that already
// worked.

/// `OpState`, as the shared decoder sees it.
pub struct OpStateDecodeContext<'a>(pub &'a mut OpState);

impl frame_decode::GlDecodeContext for OpStateDecodeContext<'_> {
    #[inline]
    fn push_error(&mut self, canvas_id: u32, code: u32) {
        push_error(self.0, canvas_id, code);
    }

    #[inline]
    fn transform_feedback_captures(&self, canvas_id: u32) -> bool {
        let queue = self.0.borrow::<WebGLErrorState>();
        queue.transform_feedback_captures(canvas_id)
    }

    #[inline]
    fn set_transform_feedback(
        &mut self,
        canvas_id: u32,
        phase: frame_decode::TransformFeedbackPhase,
    ) {
        set_transform_feedback(self.0, canvas_id, phase.into());
    }

    fn image_upload(
        &mut self,
        upload: frame_decode::ImageUpload,
    ) -> Option<shared::protocol::render_cmd::GLCmd> {
        image_upload(self.0, upload)
    }
    fn staged_payload(&mut self) -> Option<&mut frame_decode::StagedPayload> {
        None
    }
}

/// A record's texture upload from a loaded image, resolved against this
/// isolate's image table -- the function the `*_from_image` ops call, so a
/// record and an op for the same call build the same command.
pub(crate) fn image_upload(
    state: &OpState,
    upload: frame_decode::ImageUpload,
) -> Option<shared::protocol::render_cmd::GLCmd> {
    use migo_services::image::gl;
    let images = state.borrow::<crate::rendering::image::ImageCacheState>();
    match upload {
        frame_decode::ImageUpload::Full {
            canvas_id,
            target,
            level,
            internalformat,
            format,
            type_,
            image_id,
        } => gl::tex_image_2d_from_image(
            &images.aliases,
            images.session,
            canvas_id,
            target,
            level,
            internalformat,
            format,
            type_,
            image_id,
        ),
        frame_decode::ImageUpload::Sub {
            canvas_id,
            target,
            level,
            xoffset,
            yoffset,
            format,
            type_,
            image_id,
        } => gl::tex_sub_image_2d_from_image(
            &images.aliases,
            images.session,
            canvas_id,
            target,
            level,
            xoffset,
            yoffset,
            format,
            type_,
            image_id,
        ),
    }
}

impl From<frame_decode::TransformFeedbackPhase> for TransformFeedback {
    fn from(phase: frame_decode::TransformFeedbackPhase) -> Self {
        match phase {
            frame_decode::TransformFeedbackPhase::Inactive => Self::Inactive,
            frame_decode::TransformFeedbackPhase::Active => Self::Active,
            frame_decode::TransformFeedbackPhase::Paused => Self::Paused,
        }
    }
}

#[inline]
pub fn validate_bind_buffer_target(state: &mut OpState, canvas_id: u32, target: u32) -> bool {
    frame_decode::validate::validate_bind_buffer_target(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
    )
}

#[inline]
pub fn validate_bind_buffer_base(
    state: &mut OpState,
    canvas_id: u32,
    target: u32,
    index: u32,
    buffer: Option<u32>,
) -> bool {
    frame_decode::validate::validate_bind_buffer_base(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        index,
        buffer,
    )
}

#[inline]
pub fn validate_bind_buffer_range(
    state: &mut OpState,
    canvas_id: u32,
    target: u32,
    index: u32,
    buffer: Option<u32>,
    offset: i32,
    size: i32,
) -> bool {
    frame_decode::validate::validate_bind_buffer_range(
        &mut OpStateDecodeContext(state),
        canvas_id,
        target,
        index,
        buffer,
        offset,
        size,
    )
}

#[inline]
pub fn validate_vertex_attrib_pointer(
    state: &mut OpState,
    canvas_id: u32,
    size: i32,
    type_: u32,
    stride: i32,
    offset: i32,
) -> bool {
    frame_decode::validate::validate_vertex_attrib_pointer(
        &mut OpStateDecodeContext(state),
        canvas_id,
        size,
        type_,
        stride,
        offset,
    )
}

#[inline]
pub fn validate_vertex_attrib_ipointer(
    state: &mut OpState,
    canvas_id: u32,
    size: i32,
    type_: u32,
    stride: i32,
    offset: i32,
) -> bool {
    frame_decode::validate::validate_vertex_attrib_ipointer(
        &mut OpStateDecodeContext(state),
        canvas_id,
        size,
        type_,
        stride,
        offset,
    )
}

#[inline]
pub fn validate_viewport_like(
    state: &mut OpState,
    canvas_id: u32,
    width: i32,
    height: i32,
) -> bool {
    frame_decode::validate::validate_viewport_like(
        &mut OpStateDecodeContext(state),
        canvas_id,
        width,
        height,
    )
}

/// Record the attributes `canvas_id`'s context has, for `getContextAttributes()`. Called once per context, by its
/// constructor, with what the drawing buffer was given: the renderer allocates exactly the alpha, depth and stencil
/// buffers asked for (`GLCmd::WebglContext`), and antialiasing, which it declines, is recorded as off.
pub fn record_context_attrs(state: &mut OpState, canvas_id: u32, attrs: ContextAttributes) {
    let q = state.borrow_mut::<WebGLErrorState>();
    q.set_attrs(canvas_id, attrs);
}

// ---- Ops ------------------------------------------------------------

/// `gl.getError()` — drain one pending error, or return `NO_ERROR`.
#[deno_core::op2(fast)]
pub fn op_webgl_get_error(state: &mut OpState, #[smi] canvas_id: u32) -> u32 {
    let q = state.borrow_mut::<WebGLErrorState>();
    q.drain_one(canvas_id)
}

/// Records a host-side allocation rejection performed by the JS facade before
/// it passes a large payload through the op boundary.
#[deno_core::op2(fast)]
pub fn op_webgl_record_out_of_memory(state: &mut OpState, #[smi] canvas_id: u32) {
    push_error(state, canvas_id, codes::OUT_OF_MEMORY);
}

#[inline]
fn validated_external_error(code: u32) -> u32 {
    match code {
        codes::INVALID_ENUM
        | codes::INVALID_VALUE
        | codes::INVALID_OPERATION
        | codes::OUT_OF_MEMORY
        | codes::INVALID_FRAMEBUFFER_OPERATION => code,
        _ => codes::INVALID_OPERATION,
    }
}

/// Record a JS preflight rejection without allowing arbitrary values into the
/// bounded WebGL error queue.
#[deno_core::op2(fast)]
pub fn op_webgl_record_error(state: &mut OpState, #[smi] canvas_id: u32, #[smi] code: u32) {
    push_error(state, canvas_id, validated_external_error(code));
}

/// The renderer's capabilities that decide which WebGL extensions a context offers, as the bits of
/// `GpuCaps::webgl_bits`: 0 ETC2/EAC, 1 ASTC LDR, 2 float colour buffers (EXT_color_buffer_float). One fast op for
/// all of them. `0` before the render thread has published them.
#[deno_core::op2(fast)]
pub fn op_webgl_query_gpu_caps(state: &mut OpState) -> u32 {
    let Some(host) = state.try_borrow::<shared::op_state::HostOpState>() else {
        return 0;
    };
    host.gpu_caps.webgl_bits()
}

/// Serializable mirror of `ContextAttributes` with camelCase field
/// names (to match the WebGLContextAttributes IDL dictionary).
#[derive(serde::Serialize)]
pub struct SerializedAttrs {
    pub alpha: bool,
    pub antialias: bool,
    pub depth: bool,
    pub stencil: bool,
    #[serde(rename = "premultipliedAlpha")]
    pub premultiplied_alpha: bool,
    #[serde(rename = "preserveDrawingBuffer")]
    pub preserve_drawing_buffer: bool,
    #[serde(rename = "powerPreference")]
    pub power_preference: &'static str,
    #[serde(rename = "failIfMajorPerformanceCaveat")]
    pub fail_if_major_performance_caveat: bool,
    pub desynchronized: bool,
    #[serde(rename = "xrCompatible")]
    pub xr_compatible: bool,
}

impl From<ContextAttributes> for SerializedAttrs {
    fn from(a: ContextAttributes) -> Self {
        Self {
            alpha: a.alpha,
            antialias: a.antialias,
            depth: a.depth,
            stencil: a.stencil,
            premultiplied_alpha: a.premultiplied_alpha,
            preserve_drawing_buffer: a.preserve_drawing_buffer,
            power_preference: a.power_preference.as_str(),
            fail_if_major_performance_caveat: a.fail_if_major_performance_caveat,
            desynchronized: a.desynchronized,
            xr_compatible: a.xr_compatible,
        }
    }
}

/// `gl.getContextAttributes()` - return cached actual attributes.
/// Never returns `null`; a fresh context that hasn't called
/// `set_attrs` yet gets the spec defaults.
#[deno_core::op2]
#[serde]
pub fn op_webgl_get_context_attributes(
    state: &mut OpState,
    #[smi] canvas_id: u32,
) -> SerializedAttrs {
    let q = state.borrow::<WebGLErrorState>();
    SerializedAttrs::from(q.get_attrs(canvas_id).unwrap_or_default())
}

/// Called from the WebGLRenderingContext JS constructor to record
/// the attributes the game requested.  Booleans map directly; power
/// preference comes as a u8 (0=default, 1=high-performance, 2=low-power)
/// to keep the op in the fast-call lane.
#[deno_core::op2(fast)]
#[allow(clippy::too_many_arguments)]
pub fn op_webgl_record_attributes(
    state: &mut OpState,
    #[smi] canvas_id: u32,
    alpha: bool,
    antialias: bool,
    depth: bool,
    stencil: bool,
    premultiplied_alpha: bool,
    preserve_drawing_buffer: bool,
    #[smi] power_preference: u8,
    fail_if_major_performance_caveat: bool,
    desynchronized: bool,
    xr_compatible: bool,
) {
    state
        .borrow::<HostOpState>()
        .webgl_context_created
        .store(true, Ordering::Relaxed);
    let power_preference = match power_preference {
        1 => PowerPreference::HighPerformance,
        2 => PowerPreference::LowPower,
        _ => PowerPreference::Default,
    };
    let attrs = ContextAttributes {
        alpha,
        antialias,
        depth,
        stencil,
        premultiplied_alpha,
        preserve_drawing_buffer,
        power_preference,
        fail_if_major_performance_caveat,
        desynchronized,
        xr_compatible,
    };
    record_context_attrs(state, canvas_id, attrs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_queue_returns_no_error() {
        let mut q = WebGLErrorState::default();
        assert_eq!(q.drain_one(1), 0);
        assert_eq!(q.drain_one(99), 0); // unknown canvas
    }

    #[test]
    fn pushed_errors_drain_fifo() {
        let mut q = WebGLErrorState::default();
        q.push(1, 0x0500);
        q.push(1, 0x0501);
        q.push(1, 0x0502);
        assert_eq!(q.drain_one(1), 0x0500);
        assert_eq!(q.drain_one(1), 0x0501);
        assert_eq!(q.drain_one(1), 0x0502);
        assert_eq!(q.drain_one(1), 0); // drained
    }

    #[test]
    fn queues_are_scoped_per_canvas() {
        let mut q = WebGLErrorState::default();
        q.push(1, 0x0500);
        q.push(2, 0x0502);
        assert_eq!(q.drain_one(1), 0x0500);
        assert_eq!(q.drain_one(2), 0x0502);
        assert_eq!(q.drain_one(1), 0);
        assert_eq!(q.drain_one(2), 0);
    }

    #[test]
    fn gpu_preflight_only_accepts_standard_webgl_error_codes() {
        for code in [
            codes::INVALID_ENUM,
            codes::INVALID_VALUE,
            codes::INVALID_OPERATION,
            codes::OUT_OF_MEMORY,
            codes::INVALID_FRAMEBUFFER_OPERATION,
        ] {
            assert_eq!(validated_external_error(code), code);
        }
        assert_eq!(validated_external_error(0xDEAD), codes::INVALID_OPERATION);
    }

    #[test]
    fn a_code_already_held_is_not_held_twice() {
        let mut q = WebGLErrorState::default();
        for code in [
            codes::INVALID_ENUM,
            codes::INVALID_ENUM,
            codes::INVALID_VALUE,
            codes::INVALID_ENUM,
            codes::INVALID_VALUE,
        ] {
            q.push(1, code);
        }
        assert_eq!(q.len(1), 2, "one flag per code");
        assert_eq!(q.drain_one(1), codes::INVALID_ENUM);
        q.push(1, codes::INVALID_ENUM);
        assert_eq!(
            q.drain_one(1),
            codes::INVALID_VALUE,
            "codes of different kinds are each held, oldest first"
        );
        assert_eq!(
            q.drain_one(1),
            codes::INVALID_ENUM,
            "a code read is recorded again"
        );
        assert_eq!(q.drain_one(1), codes::NO_ERROR);
    }

    #[test]
    fn default_attrs_match_spec() {
        let q = WebGLErrorState::default();
        let a = q.get_attrs(1).unwrap_or_default();
        assert!(a.alpha);
        assert!(
            !a.antialias,
            "antialiasing is declined, and reported as off"
        );
        assert!(a.depth);
        assert!(!a.stencil, "a stencil buffer only when one is asked for");
        assert!(a.premultiplied_alpha);
        assert!(!a.preserve_drawing_buffer);
        assert_eq!(a.power_preference.as_str(), "default");
    }

    #[test]
    fn set_attrs_round_trips() {
        let mut q = WebGLErrorState::default();
        let mut a = ContextAttributes::default();
        a.antialias = false;
        a.power_preference = PowerPreference::HighPerformance;
        q.set_attrs(1, a);
        let got = q.get_attrs(1).unwrap();
        assert!(!got.antialias);
        assert_eq!(got.power_preference.as_str(), "high-performance");
    }

    // ---- Validator unit tests ------------------------------------
    //
    // These exercise the pure-param checks without needing an
    // `OpState`; we pass in a tiny stand-in queue and assert that
    // bad inputs record the right error code.
    //
    // Host-side validators have to keep working even when the GL
    // render thread isn't running (e.g. in test harnesses that
    // build a context but never draw), so every validator must
    // return a pure bool decision.

    /// Tiny host harness around `WebGLErrorState` — mirrors what
    /// `push_error` does without going through deno_core's OpState.
    fn push(q: &mut WebGLErrorState, canvas_id: u32, code: u32) {
        q.push(canvas_id, code);
    }

    /// Re-implementation of `validate_bind_buffer_target` that takes
    /// the state directly.  Keeps the test structure identical to
    /// the op-level logic without pulling OpState into tests.
    fn validate_target(q: &mut WebGLErrorState, canvas_id: u32, target: u32) -> bool {
        match target {
            0x8892 | 0x8893 | 0x8F36 | 0x8F37 | 0x8C8E | 0x8A11 | 0x88EB | 0x88EC => true,
            _ => {
                push(q, canvas_id, codes::INVALID_ENUM);
                false
            }
        }
    }

    #[test]
    fn bind_buffer_legal_targets_dont_push_error() {
        let mut q = WebGLErrorState::default();
        for &t in &[
            0x8892u32, 0x8893, 0x8F36, 0x8F37, 0x8C8E, 0x8A11, 0x88EB, 0x88EC,
        ] {
            assert!(validate_target(&mut q, 1, t), "target 0x{:04X}", t);
        }
        assert_eq!(q.drain_one(1), 0);
    }

    #[test]
    fn bind_buffer_illegal_target_pushes_invalid_enum() {
        let mut q = WebGLErrorState::default();
        assert!(!validate_target(&mut q, 1, 0xDEAD));
        assert_eq!(q.drain_one(1), codes::INVALID_ENUM);
    }

    /// Same structure for `vertexAttribPointer` rules — mirror of
    /// the inline logic.
    fn validate_vap(
        q: &mut WebGLErrorState,
        canvas_id: u32,
        size: i32,
        type_: u32,
        stride: i32,
        offset: i32,
    ) -> bool {
        if !(1..=4).contains(&size) {
            push(q, canvas_id, codes::INVALID_VALUE);
            return false;
        }
        match type_ {
            0x1400 | 0x1401 | 0x1402 | 0x1403 | 0x1406 | 0x140B | 0x1404 | 0x1405 => {}
            _ => {
                push(q, canvas_id, codes::INVALID_ENUM);
                return false;
            }
        }
        if !(0..=255).contains(&stride) {
            push(q, canvas_id, codes::INVALID_VALUE);
            return false;
        }
        if offset < 0 {
            push(q, canvas_id, codes::INVALID_VALUE);
            return false;
        }
        true
    }

    #[test]
    fn vertex_attrib_pointer_rejects_size_zero_and_five() {
        let mut q = WebGLErrorState::default();
        assert!(!validate_vap(&mut q, 1, 0, 0x1406, 0, 0));
        assert!(!validate_vap(&mut q, 1, 5, 0x1406, 0, 0));
        assert_eq!(q.drain_one(1), codes::INVALID_VALUE);
        assert_eq!(q.drain_one(1), codes::NO_ERROR, "one flag per code");
    }

    #[test]
    fn vertex_attrib_pointer_rejects_bogus_type() {
        let mut q = WebGLErrorState::default();
        // 0x0000 is not a valid GL type enum.
        assert!(!validate_vap(&mut q, 1, 4, 0x0000, 0, 0));
        assert_eq!(q.drain_one(1), codes::INVALID_ENUM);
    }

    #[test]
    fn vertex_attrib_pointer_rejects_negative_offset() {
        let mut q = WebGLErrorState::default();
        assert!(!validate_vap(&mut q, 1, 4, 0x1406, 0, -1));
        assert_eq!(q.drain_one(1), codes::INVALID_VALUE);
    }

    #[test]
    fn vertex_attrib_pointer_rejects_stride_out_of_range() {
        let mut q = WebGLErrorState::default();
        assert!(!validate_vap(&mut q, 1, 4, 0x1406, 256, 0));
        assert!(!validate_vap(&mut q, 1, 4, 0x1406, -1, 0));
        assert_eq!(q.drain_one(1), codes::INVALID_VALUE);
        assert_eq!(q.drain_one(1), codes::NO_ERROR, "one flag per code");
    }

    #[test]
    fn vertex_attrib_pointer_accepts_all_spec_types() {
        let mut q = WebGLErrorState::default();
        for &t in &[
            0x1400u32, 0x1401, 0x1402, 0x1403, 0x1406, 0x140B, 0x1404, 0x1405,
        ] {
            assert!(validate_vap(&mut q, 1, 4, t, 16, 0), "type 0x{:04X}", t);
        }
        assert_eq!(q.drain_one(1), 0);
    }

    #[test]
    fn viewport_like_rejects_negative_dimensions() {
        fn validate(q: &mut WebGLErrorState, canvas_id: u32, w: i32, h: i32) -> bool {
            if w < 0 || h < 0 {
                push(q, canvas_id, codes::INVALID_VALUE);
                return false;
            }
            true
        }
        let mut q = WebGLErrorState::default();
        assert!(!validate(&mut q, 1, -1, 10));
        assert!(!validate(&mut q, 1, 10, -1));
        assert!(validate(&mut q, 1, 0, 0));
        assert!(validate(&mut q, 1, 100, 200));
        assert_eq!(q.drain_one(1), codes::INVALID_VALUE);
        assert_eq!(q.drain_one(1), codes::NO_ERROR, "one flag per code");
    }
}
