//! Parameter validation, shared by the decoder and the raw op handlers.
//!
//! WebGL's rule for an illegal call is that it pushes an error and does
//! nothing -- it does not abort the frame, and it does not throw. So these
//! return a bool and report through the context, and every caller skips
//! dispatch on `false`.
//!
//! They are here rather than beside the error queue because the queue lives in
//! the JavaScript runtime and these do not need it: what they need is somewhere
//! to put an error and one piece of GL state, which is what
//! [`GlDecodeContext`] is.

use crate::codes;

/// What the decoder needs from whoever is hosting it.
///
/// Two methods, and both of them exist because WebGL semantics require them
/// rather than because the decoder wants state. A trait rather than a concrete
/// type because the two hosts are genuinely different -- one has a JavaScript
/// runtime's op state behind it and the other has the external session's --
/// and generic rather than `dyn` because this is called once per command on
/// the render path.
/// Where a canvas's transform feedback is. Only `Active` refuses a rebind of
/// the feedback buffers; see [`GlDecodeContext::transform_feedback_captures`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TransformFeedbackPhase {
    #[default]
    Inactive,
    Active,
    Paused,
}

pub trait GlDecodeContext {
    /// Record a WebGL error for a canvas. The call that produced it is then
    /// skipped, per the specification.
    fn push_error(&mut self, canvas_id: u32, code: u32);

    /// Whether transform feedback is currently capturing on this canvas.
    ///
    /// `bindBufferBase` and `bindBufferRange` on a transform feedback buffer
    /// are illegal while capture is active, and only the host knows.
    fn transform_feedback_captures(&self, canvas_id: u32) -> bool;

    /// Record a transform-feedback transition, which is what the answer above
    /// is read from. Called by begin, pause, resume and end, on both paths.
    fn set_transform_feedback(&mut self, canvas_id: u32, phase: TransformFeedbackPhase);
}

const GL_TRANSFORM_FEEDBACK_BUFFER: u32 = 0x8C8E;
const GL_UNIFORM_BUFFER: u32 = 0x8A11;

// ---- Validators (pure param checks, no GL state peek) ---------------

/// Validate the `target` argument of `bindBuffer`.  Returns `true`
/// when the target is legal for WebGL 1.0 or 2.0; on illegal
/// targets it pushes `INVALID_ENUM` and returns `false`, signalling
/// the caller to skip GL dispatch.
///
/// WebGL 1.0 valid: `ARRAY_BUFFER` (0x8892), `ELEMENT_ARRAY_BUFFER` (0x8893).
/// WebGL 2.0 adds: `COPY_READ_BUFFER` (0x8F36), `COPY_WRITE_BUFFER` (0x8F37),
/// `TRANSFORM_FEEDBACK_BUFFER` (0x8C8E), `UNIFORM_BUFFER` (0x8A11),
/// `PIXEL_PACK_BUFFER` (0x88EB), `PIXEL_UNPACK_BUFFER` (0x88EC).
#[inline]
pub fn validate_bind_buffer_target<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
) -> bool {
    match target {
        0x8892 | 0x8893 // ARRAY_BUFFER / ELEMENT_ARRAY_BUFFER (WebGL 1+)
        | 0x8F36 | 0x8F37 // COPY_READ/WRITE (WebGL 2)
        | 0x8C8E | 0x8A11 // TRANSFORM_FEEDBACK / UNIFORM (WebGL 2)
        | 0x88EB | 0x88EC // PIXEL_PACK/UNPACK (WebGL 2)
        => true,
        _ => {
            context.push_error(canvas_id, codes::INVALID_ENUM);
            false
        }
    }
}

#[inline]
fn validate_bind_buffer_indexed_target<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
) -> bool {
    match target {
        GL_TRANSFORM_FEEDBACK_BUFFER | GL_UNIFORM_BUFFER => true,
        _ => {
            context.push_error(canvas_id, codes::INVALID_ENUM);
            false
        }
    }
}

#[inline]
pub fn validate_bind_buffer_base<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    _index: u32,
    _buffer: Option<u32>,
) -> bool {
    if !validate_bind_buffer_indexed_target(context, canvas_id, target) {
        return false;
    }
    if target == GL_TRANSFORM_FEEDBACK_BUFFER && context.transform_feedback_captures(canvas_id) {
        context.push_error(canvas_id, codes::INVALID_OPERATION);
        return false;
    }
    true
}

#[inline]
pub fn validate_bind_buffer_range<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    index: u32,
    buffer: Option<u32>,
    offset: i32,
    size: i32,
) -> bool {
    if buffer.is_some() && offset < 0 {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    if buffer.is_some() && size <= 0 {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    if !validate_bind_buffer_base(context, canvas_id, target, index, buffer) {
        return false;
    }
    if target == GL_TRANSFORM_FEEDBACK_BUFFER
        && buffer.is_some()
        && ((offset % 4) != 0 || (size % 4) != 0)
    {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    true
}

/// Validate the parameter tuple of `vertexAttribPointer`.  Returns
/// `true` when the call is legal, `false` after pushing the right
/// error code.
///
/// Rules (WebGL 1.0 s5.14.10, WebGL 2.0 s3.7.8):
///   * `size` MUST be 1, 2, 3, or 4 → INVALID_VALUE otherwise
///   * `type` MUST be a legal `GLenum` — `BYTE`, `UNSIGNED_BYTE`,
///     `SHORT`, `UNSIGNED_SHORT`, `FLOAT`, `HALF_FLOAT` (WebGL 2),
///     `INT` (WebGL 2), `UNSIGNED_INT` (WebGL 2) → INVALID_ENUM
///   * `stride` MUST be in `[0, 255]` → INVALID_VALUE
///   * `offset` MUST be `>= 0` → INVALID_VALUE
///
/// Does NOT validate the "ARRAY_BUFFER must be bound" condition —
/// that requires peeking at render-thread shadow state which isn't
/// accessible from the JS thread at op dispatch time.  The render
/// thread will surface it through a later `glGetError` if needed.
#[inline]
pub fn validate_vertex_attrib_pointer<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    size: i32,
    type_: u32,
    stride: i32,
    offset: i32,
) -> bool {
    if !(1..=4).contains(&size) {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    match type_ {
        0x1400 | 0x1401 | 0x1402 | 0x1403 | 0x1406 // BYTE/UBYTE/SHORT/USHORT/FLOAT
        | 0x140B | 0x1404 | 0x1405 // HALF_FLOAT / INT / UNSIGNED_INT
        => {}
        _ => {
            context.push_error(canvas_id, codes::INVALID_ENUM);
            return false;
        }
    }
    if !(0..=255).contains(&stride) {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    if offset < 0 {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    true
}

/// Validate the parameters of a `viewport` / `scissor` call.  Width
/// and height must be non-negative.  Emits `INVALID_VALUE` on
/// violation.
#[inline]
pub fn validate_viewport_like<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    width: i32,
    height: i32,
) -> bool {
    if width < 0 || height < 0 {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    true
}
