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

    /// The command that uploads an image the host loaded into a texture:
    /// `texImage2D(…, image)` or `texSubImage2D(…, image)`. The pixels are the
    /// host's -- the image was decoded where the texture lives -- so only the
    /// host's image table can say what the id names. `None` when it names
    /// nothing, which the resolver logs; the call is then skipped, as the
    /// embedded op skips it.
    ///
    /// Required, not defaulted: a wrapper that forgot to forward a defaulted
    /// method would drop every upload silently -- which is what the first
    /// version of this did, and what a test caught.
    fn image_upload(&mut self, upload: ImageUpload) -> Option<shared::protocol::render_cmd::GLCmd>;

    /// Where this host stages uploads larger than a record, or `None` if it
    /// takes none. Only a producer in another process sends them -- the
    /// embedded runtime's uploads are ops, never records -- so every other host
    /// answers `None`, and an upload that names staged bytes there fails as an
    /// allocation GL cannot make.
    ///
    /// Required for the reason `image_upload` is: a wrapper that forgot to
    /// forward it would fail every large upload, quietly.
    fn staged_payload(&mut self) -> Option<&mut crate::staging::StagedPayload>;
}

/// A texture upload whose source is a loaded image, as a record carries it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageUpload {
    /// `texImage2D(target, level, internalformat, format, type, image)`.
    Full {
        canvas_id: u32,
        target: u32,
        level: i32,
        internalformat: i32,
        format: u32,
        type_: u32,
        image_id: u32,
    },
    /// `texSubImage2D(target, level, xoffset, yoffset, format, type, image)`.
    Sub {
        canvas_id: u32,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        format: u32,
        type_: u32,
        image_id: u32,
    },
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

/// Validate the parameters of a `vertexAttribIPointer` call: the same shape as `vertexAttribPointer`, but only the
/// integer types are accepted (there is nothing to convert them to).
#[inline]
pub fn validate_vertex_attrib_ipointer<C: GlDecodeContext>(
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
        0x1400 | 0x1401 | 0x1402 | 0x1403 | 0x1404 | 0x1405 => {} // BYTE .. UNSIGNED_INT
        _ => {
            context.push_error(canvas_id, codes::INVALID_ENUM);
            return false;
        }
    }
    if !(0..=255).contains(&stride) || offset < 0 {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    true
}

/// Which `clearBuffer*` call a record is. What the buffer enum may be depends on the type of the values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClearBufferKind {
    /// `clearBufferfv`: COLOR or DEPTH.
    Float,
    /// `clearBufferiv`: COLOR or STENCIL.
    Int,
    /// `clearBufferuiv`: COLOR only.
    Uint,
    /// `clearBufferfi`: DEPTH_STENCIL only.
    DepthStencil,
}

/// Validate the buffer and draw buffer of a `clearBuffer*` call: the buffer enum is `INVALID_ENUM` unless the kind
/// takes it, and the draw buffer is `INVALID_VALUE` when it is negative or, for anything but COLOR, not 0. COLOR's
/// upper bound (`MAX_DRAW_BUFFERS`) is a limit of the device, which the facade knows and this does not.
#[inline]
pub fn validate_clear_buffer<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    kind: ClearBufferKind,
    buffer: u32,
    drawbuffer: i32,
) -> bool {
    const COLOR: u32 = 0x1800;
    const DEPTH: u32 = 0x1801;
    const STENCIL: u32 = 0x1802;
    const DEPTH_STENCIL: u32 = 0x84F9;
    let allowed = match kind {
        ClearBufferKind::Float => buffer == COLOR || buffer == DEPTH,
        ClearBufferKind::Int => buffer == COLOR || buffer == STENCIL,
        ClearBufferKind::Uint => buffer == COLOR,
        ClearBufferKind::DepthStencil => buffer == DEPTH_STENCIL,
    };
    if !allowed {
        context.push_error(canvas_id, codes::INVALID_ENUM);
        return false;
    }
    if drawbuffer < 0 || (buffer != COLOR && drawbuffer != 0) {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    true
}

/// `TEXTURE_2D` or one of the six cube map faces: the targets of a 2D image (`texImage2D`, `copyTexImage2D`, ...).
#[inline]
fn is_image_2d_target(target: u32) -> bool {
    target == 0x0DE1 || (0x8515..=0x851A).contains(&target)
}

/// The internal formats `copyTexImage2D` takes: WebGL 1's unsized five and OpenGL ES 3.0's colour-renderable sized
/// formats (table 3.14), with the float ones `EXT_color_buffer_float` makes renderable. A depth or stencil format is
/// a format the call cannot copy into (INVALID_OPERATION); anything else is not a format (INVALID_ENUM). WebGL 1 takes
/// only the unsized five: the facade, which knows which interface it is, refuses the rest there.
fn copy_tex_image_format_error(internalformat: u32) -> Option<u32> {
    match internalformat {
        // ALPHA, RGB, RGBA, LUMINANCE, LUMINANCE_ALPHA
        0x1906 | 0x1907 | 0x1908 | 0x1909 | 0x190A => None,
        // R8, RG8, RGB8, RGBA4, RGB5_A1, RGBA8, RGB10_A2, RGB565, SRGB8, SRGB8_ALPHA8
        0x8229 | 0x822B | 0x8051 | 0x8056 | 0x8057 | 0x8058 | 0x8059 | 0x8D62 | 0x8C41 | 0x8C43 => {
            None
        }
        // R8I, R8UI, R16I, R16UI, R32I, R32UI, RG8I, RG8UI, RG16I, RG16UI, RG32I, RG32UI
        0x8231..=0x823C => None,
        // RGBA32UI, RGBA16UI, RGBA8UI, RGBA32I, RGBA16I, RGBA8I, RGB10_A2UI
        0x8D70 | 0x8D76 | 0x8D7C | 0x8D82 | 0x8D88 | 0x8D8E | 0x906F => None,
        // R16F, RG16F, R32F, RG32F, RGBA32F, RGBA16F, R11F_G11F_B10F (EXT_color_buffer_float)
        0x822D | 0x822F | 0x822E | 0x8230 | 0x8814 | 0x881A | 0x8C3A => None,
        // DEPTH_COMPONENT, DEPTH_COMPONENT16/24/32F, DEPTH_STENCIL, DEPTH24_STENCIL8, DEPTH32F_STENCIL8
        0x1902 | 0x81A5 | 0x81A6 | 0x8CAC | 0x84F9 | 0x88F0 | 0x8CAD => {
            Some(codes::INVALID_OPERATION)
        }
        _ => Some(codes::INVALID_ENUM),
    }
}

/// Validate a `copyTexImage2D` call: the target is a 2D image (INVALID_ENUM), the internal format one the call
/// takes (INVALID_ENUM, or INVALID_OPERATION for depth and stencil), the level, width and height not negative and
/// the border 0 (INVALID_VALUE), and a cube face square (INVALID_VALUE). That the read framebuffer has a format the
/// copy can convert from is the driver's to judge.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn validate_copy_tex_image_2d<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    level: i32,
    internalformat: u32,
    width: i32,
    height: i32,
    border: i32,
) -> bool {
    let error = if !is_image_2d_target(target) {
        Some(codes::INVALID_ENUM)
    } else if let Some(code @ codes::INVALID_ENUM) = copy_tex_image_format_error(internalformat) {
        Some(code)
    } else if level < 0
        || width < 0
        || height < 0
        || border != 0
        || (target != 0x0DE1 && width != height)
    {
        Some(codes::INVALID_VALUE)
    } else {
        copy_tex_image_format_error(internalformat)
    };
    match error {
        Some(code) => {
            context.push_error(canvas_id, code);
            false
        }
        None => true,
    }
}

/// Validate a `copyTexSubImage2D` (`three_d` false: a 2D image target) or `copyTexSubImage3D` (`three_d` true:
/// `TEXTURE_3D` or `TEXTURE_2D_ARRAY`) call: a target of the other kind is INVALID_ENUM, a negative level, offset,
/// width or height INVALID_VALUE. Whether the rectangle fits the level is the driver's to judge.
#[inline]
pub fn validate_copy_tex_sub_image<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    three_d: bool,
    target: u32,
    level: i32,
    offsets_and_size: &[i32],
) -> bool {
    let target_ok = if three_d {
        target == 0x806F || target == 0x8C1A
    } else {
        is_image_2d_target(target)
    };
    if !target_ok {
        context.push_error(canvas_id, codes::INVALID_ENUM);
        return false;
    }
    if level < 0 || offsets_and_size.iter().any(|v| *v < 0) {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return false;
    }
    true
}

/// Validate a `copyBufferSubData` call: both targets are buffer binding points (INVALID_ENUM), and the offsets and
/// size are not negative (INVALID_VALUE). That they fit the buffers bound there, and that a copy within one buffer
/// does not overlap, is the driver's to judge.
#[inline]
pub fn validate_copy_buffer_sub_data<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    read_target: u32,
    write_target: u32,
    read_offset: i32,
    write_offset: i32,
    size: i32,
) -> bool {
    let is_target = |t: u32| {
        matches!(
            t,
            0x8892 | 0x8893 | 0x8F36 | 0x8F37 | 0x8C8E | 0x8A11 | 0x88EB | 0x88EC
        )
    };
    if !is_target(read_target) || !is_target(write_target) {
        context.push_error(canvas_id, codes::INVALID_ENUM);
        return false;
    }
    if read_offset < 0 || write_offset < 0 || size < 0 {
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
