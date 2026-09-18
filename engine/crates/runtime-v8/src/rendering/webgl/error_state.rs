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

/// Hard cap on the per-context error queue length.
///
/// A misbehaving (or adversarial) script can keep issuing illegal
/// WebGL calls without ever calling `getError()` to drain them.
/// Without a cap, the queue grows until the process OOMs - a pure
/// JS-side memory amplification.  256 is well above any realistic
/// scripted burst (most engines drain every few frames) and costs
/// ~1 KiB of bare payload per context.
///
/// Past the cap we switch to a "sticky overflow" mode: new pushes
/// are dropped, but an `OUT_OF_MEMORY (0x0505)` sentinel is kept
/// at the tail so the next `getError()` signals the truncation,
/// matching the spirit of GL's own `GL_OUT_OF_MEMORY` semantics.
const MAX_ERRORS_PER_CTX: usize = 256;

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
    /// Per-context overflow counter (dropped pushes since last
    /// drain to `OUT_OF_MEMORY` sentinel).  Not part of the spec;
    /// used to keep exactly one sentinel in the queue regardless
    /// of how long the overflow streak is.
    overflow: HashMap<u32, u64>,
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
    fn default() -> Self {
        // Defaults mirror the WebGL 1.0 spec (§5.2.1).
        Self {
            alpha: true,
            antialias: true,
            depth: true,
            // Our GL backend is fixed-format depth24 + stencil8 (see
            // `record_context_attrs`), so an actual stencil buffer always
            // exists. Reporting it (like `depth`) lets engine mask systems
            // (Pixi/Cocos) use stencil masking instead of warning "does not
            // have a stencil buffer, masks may not render correctly".
            stencil: true,
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
    /// Push a WebGL error code onto the queue for `canvas_id`.
    ///
    /// WebGL spec semantics: if a previous call already recorded the
    /// same error code, the spec allows us to coalesce, but Chrome /
    /// Firefox queue them separately; we match the latter so
    /// conformance scripts that count errors behave identically.
    ///
    /// Bounded at `MAX_ERRORS_PER_CTX`, with the last slot reserved for a
    /// sticky `OUT_OF_MEMORY` sentinel so the next `getError()` reports the
    /// truncation. A global overflow counter is incremented for every dropped
    /// record; `render_diagnostics` surfaces the total to the Java debug
    /// overlay.
    ///
    /// Reserving the slot rather than evicting for it is the point: the overflow
    /// path used to `pop_front()` to make room, which threw away the *oldest
    /// undrained* error -- a real diagnostic the game had not read yet -- in
    /// order to report that errors were being thrown away. Normal pushes now
    /// stop one short, so the sentinel always has somewhere to go and nothing
    /// genuine is ever displaced.
    pub fn push(&mut self, canvas_id: u32, code: u32) {
        let queue = self.queues.entry(canvas_id).or_default();
        if queue.len() < MAX_ERRORS_PER_CTX - 1 {
            queue.push_back(code);
            return;
        }
        // Overflow path: drop the new code, bump counters, and guarantee an OOM
        // sentinel is the last element so the next drain signals the truncation.
        *self.overflow.entry(canvas_id).or_insert(0) += 1;
        shared::stats::bump_webgl_error_overflow(1);
        if queue.back().copied() != Some(codes::OUT_OF_MEMORY) {
            queue.push_back(codes::OUT_OF_MEMORY);
        }
    }

    /// Per-context overflow counter (cumulative dropped pushes).
    #[cfg(test)]
    #[inline]
    pub fn overflow_count(&self, canvas_id: u32) -> u64 {
        self.overflow.get(&canvas_id).copied().unwrap_or(0)
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

/// Record the attrs negotiated for `canvas_id`.  Called once per
/// `new WebGLRenderingContext(canvas, options)` so
/// `getContextAttributes()` returns real values instead of spec
/// defaults.
///
/// We accept every WebGL option as-is because our GL backend is
/// fixed-format (RGBA8, depth24, stencil8) — there's no genuine
/// negotiation step.  A real browser would clamp on unavailable
/// features (e.g. MSAA unsupported → antialias=false); our
/// runtime treats all flags as the game's stated preferences.
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
        | codes::OUT_OF_MEMORY => code,
        _ => codes::INVALID_OPERATION,
    }
}

/// Record a JS preflight rejection without allowing arbitrary values into the
/// bounded WebGL error queue.
#[deno_core::op2(fast)]
pub fn op_webgl_record_error(state: &mut OpState, #[smi] canvas_id: u32, #[smi] code: u32) {
    push_error(state, canvas_id, validated_external_error(code));
}

/// Snapshot the compressed-texture caps the render thread detected
/// during GL context init.  Returned as a bitfield so a single fast
/// op replaces multiple boolean round-trips:
///
/// * bit 0 = ETC2 / EAC (GLES 3.0 core; always true on our runtime)
/// * bit 1 = ASTC LDR (GL_KHR_texture_compression_astc_ldr / _hdr)
///
/// JS uses this to decide which `WEBGL_compressed_texture_*`
/// extensions to advertise in `getExtension()` /
/// `getSupportedExtensions()`.  Returns `0` (no compression) if the
/// caps haven't been set yet (e.g. a very early JS call before the
/// render thread has created a GL context).
#[deno_core::op2(fast)]
pub fn op_webgl_query_compressed_caps(state: &mut OpState) -> u32 {
    let Some(host) = state.try_borrow::<shared::op_state::HostOpState>() else {
        return 0;
    };
    let snap = host.gpu_caps.snapshot();
    let mut bits = 0u32;
    if snap.etc2 {
        bits |= 1 << 0;
    }
    if snap.astc {
        bits |= 1 << 1;
    }
    bits
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
        ] {
            assert_eq!(validated_external_error(code), code);
        }
        assert_eq!(validated_external_error(0xDEAD), codes::INVALID_OPERATION);
    }

    #[test]
    fn queue_is_bounded_and_overflow_is_counted() {
        let mut q = WebGLErrorState::default();
        // Normal pushes stop one short of the cap, because the last slot is
        // reserved for the sentinel. Filling to that point must not overflow.
        for _ in 0..MAX_ERRORS_PER_CTX - 1 {
            q.push(1, codes::INVALID_ENUM);
        }
        assert_eq!(q.overflow_count(1), 0);
        assert_eq!(q.len(1), MAX_ERRORS_PER_CTX - 1);

        // Push past it; each push is dropped, overflow grows, and the queue
        // reaches -- and stays at -- the cap once the sentinel is planted.
        for _ in 0..10 {
            q.push(1, codes::INVALID_VALUE);
        }
        assert_eq!(q.overflow_count(1), 10);
        assert_eq!(q.len(1), MAX_ERRORS_PER_CTX);

        // Nothing genuine was displaced to make room for the sentinel: the
        // oldest error is still the first one drained, and every real error
        // pushed is still there ahead of the sentinel. This is the property the
        // reserved slot exists for -- the previous overflow path called
        // `pop_front()`, so the first error the game had not read yet was thrown
        // away in order to report that errors were being thrown away, and a test
        // that only looked for the sentinel could not see it.
        for i in 0..MAX_ERRORS_PER_CTX - 1 {
            assert_eq!(
                q.drain_one(1),
                codes::INVALID_ENUM,
                "real error {i} was evicted by the sentinel"
            );
        }
        assert_eq!(
            q.drain_one(1),
            codes::OUT_OF_MEMORY,
            "overflow must plant an OUT_OF_MEMORY sentinel at the tail"
        );
        assert_eq!(q.drain_one(1), codes::NO_ERROR);
    }

    #[test]
    fn overflow_only_plants_one_sentinel_per_burst() {
        let mut q = WebGLErrorState::default();
        for _ in 0..MAX_ERRORS_PER_CTX - 1 {
            q.push(1, codes::INVALID_ENUM);
        }
        // First overflow plants the sentinel.
        q.push(1, codes::INVALID_VALUE);
        // Subsequent overflows must not add more sentinels — they
        // only increment the counter.
        for _ in 0..100 {
            q.push(1, codes::INVALID_VALUE);
        }
        assert_eq!(q.len(1), MAX_ERRORS_PER_CTX);
        // Exactly one OOM at the tail.
        let mut oom = 0;
        while q.len(1) > 0 {
            if q.drain_one(1) == codes::OUT_OF_MEMORY {
                oom += 1;
            }
        }
        assert_eq!(oom, 1);
    }

    #[test]
    fn default_attrs_match_spec() {
        let q = WebGLErrorState::default();
        let a = q.get_attrs(1).unwrap_or_default();
        assert!(a.alpha);
        assert!(a.antialias);
        assert!(a.depth);
        // Migo's GL backend is fixed-format depth24 + stencil8, so unlike the
        // bare WebGL spec default (stencil:false) we deliberately report
        // stencil:true (see the ContextAttributes Default impl) so Pixi/Cocos
        // stencil masking works without a "no stencil buffer" warning.
        assert!(a.stencil);
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
        assert_eq!(q.drain_one(1), codes::INVALID_VALUE);
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
        assert_eq!(q.drain_one(1), codes::INVALID_VALUE);
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
        assert_eq!(q.drain_one(1), codes::INVALID_VALUE);
        assert_eq!(q.drain_one(1), 0);
    }
}
