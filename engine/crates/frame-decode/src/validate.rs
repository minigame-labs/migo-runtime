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

/// Where a canvas's transform feedback is. Only `Active` refuses a rebind of
/// the feedback buffers; see [`GlDecodeContext::transform_feedback_captures`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TransformFeedbackPhase {
    #[default]
    Inactive,
    Active,
    Paused,
}

/// What the decoder needs from whoever is hosting it.
///
/// Two methods, and both of them exist because WebGL semantics require them
/// rather than because the decoder wants state. A trait rather than a concrete
/// type because the two hosts are genuinely different -- one has a JavaScript
/// runtime's op state behind it and the other has the external session's --
/// and generic rather than `dyn` because this is called once per command on
/// the render path.
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

    /// Where the pixels of an image the host loaded are, for an upload that names
    /// it: `texImage2D(…, image)` and the rest. The pixels are the host's -- the
    /// image was decoded where the texture lives -- so only the host's image
    /// table can say what the id names. `None` when it names nothing, which the
    /// resolver logs; the call is then skipped, as the embedded op skips it.
    ///
    /// Required, not defaulted: a wrapper that forgot to forward a defaulted
    /// method would drop every upload silently -- which is what the first
    /// version of this did, and what a test caught.
    fn image_source(
        &mut self,
        image_id: u32,
    ) -> Option<shared::protocol::render_cmd::TextureSource>;

    /// Where this host stages uploads larger than a record, or `None` if it
    /// takes none. Only a producer in another process sends them -- the
    /// embedded runtime's uploads are ops, never records -- so every other host
    /// answers `None`, and an upload that names staged bytes there fails as an
    /// allocation GL cannot make.
    ///
    /// Required for the reason `image_source` is: a wrapper that forgot to
    /// forward it would fail every large upload, quietly.
    fn staged_payload(&mut self) -> Option<&mut crate::staging::StagedPayload>;
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

/// The bytes one component of a vertex attribute of `type_` takes -- a packed type its whole word --, or `None` for a
/// type no attribute has.
#[inline]
fn attribute_component_bytes(type_: u32) -> Option<i32> {
    match type_ {
        0x1400 | 0x1401 => Some(1),          // BYTE, UNSIGNED_BYTE
        0x1402 | 0x1403 | 0x140B => Some(2), // SHORT, UNSIGNED_SHORT, HALF_FLOAT
        0x1404 | 0x1405 | 0x1406 => Some(4), // INT, UNSIGNED_INT, FLOAT
        0x8D9F | 0x8368 => Some(4),          // INT_2_10_10_10_REV, UNSIGNED_INT_2_10_10_10_REV
        _ => None,
    }
}

/// The rules both pointer calls share once the type is known to be one of theirs, in a browser's order: `size` 1 to 4
/// (INVALID_VALUE, judged first), `stride` 0 to 255 and `offset` not negative (INVALID_VALUE), a packed type of size 4
/// only (INVALID_OPERATION, ES 3.0 2.8), and `offset` and `stride` multiples of the type's size (INVALID_OPERATION,
/// WebGL 1.0 6.4).
#[inline]
fn attribute_pointer_error(
    size: i32,
    type_: u32,
    stride: i32,
    offset: i32,
    types: fn(u32) -> bool,
) -> Option<u32> {
    if !(1..=4).contains(&size) {
        return Some(codes::INVALID_VALUE);
    }
    let bytes = match attribute_component_bytes(type_) {
        Some(bytes) if types(type_) => bytes,
        _ => return Some(codes::INVALID_ENUM),
    };
    if !(0..=255).contains(&stride) || offset < 0 {
        return Some(codes::INVALID_VALUE);
    }
    if (matches!(type_, 0x8D9F | 0x8368) && size != 4) || offset % bytes != 0 || stride % bytes != 0
    {
        return Some(codes::INVALID_OPERATION);
    }
    None
}

/// Validate the parameter tuple of `vertexAttribPointer` (WebGL 1.0 5.14.10, 6.4; WebGL 2.0 3.7.8): every attribute
/// type WebGL 2 has -- WebGL 1's fewer are the facade's to judge, as it knows the version --, then
/// [`attribute_pointer_error`]'s rules. That an ARRAY_BUFFER is bound for a non-zero offset is the facade's to judge
/// too: it keeps the binding.
#[inline]
pub fn validate_vertex_attrib_pointer<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    size: i32,
    type_: u32,
    stride: i32,
    offset: i32,
) -> bool {
    let error = attribute_pointer_error(size, type_, stride, offset, |_| true);
    refuse(context, canvas_id, error)
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
    let error = attribute_pointer_error(size, type_, stride, offset, |t| {
        (0x1400..=0x1405).contains(&t)
    });
    refuse(context, canvas_id, error)
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

// ---- Fixed-function state ----------------------------------------------------------------------------------------
//
// What each state setter takes (ES 3.0 4.1, 4.2, 3.6, 2.13, and WebGL 1.0 6.13 / 6.24): a value it does not take is
// an error and the state stays as it was, as a browser has it. The driver would refuse most of these too, but its
// error never reaches `getError`, and a shadow the facade keeps of the state would believe the call.

/// A comparison function: NEVER .. ALWAYS (depthFunc, stencilFunc).
#[inline]
fn is_comparison(func: u32) -> bool {
    (0x0200..=0x0207).contains(&func)
}

/// FRONT, BACK or FRONT_AND_BACK (cullFace and the stencil calls' face).
#[inline]
fn is_face(face: u32) -> bool {
    matches!(face, 0x0404 | 0x0405 | 0x0408)
}

/// KEEP, ZERO, REPLACE, INCR, DECR, INVERT, INCR_WRAP, DECR_WRAP.
#[inline]
fn is_stencil_op(op: u32) -> bool {
    matches!(
        op,
        0x1E00 | 0 | 0x1E01 | 0x1E02 | 0x1E03 | 0x150A | 0x8507 | 0x8508
    )
}

/// ZERO, ONE, the source and destination colour and alpha factors and their complements, SRC_ALPHA_SATURATE, and the
/// constant colour and alpha factors and their complements. WebGL 1 takes SRC_ALPHA_SATURATE as a source factor only,
/// which the facade judges: it knows the version.
#[inline]
fn is_blend_factor(factor: u32) -> bool {
    matches!(factor, 0 | 1 | 0x0300..=0x0308 | 0x8001..=0x8004)
}

/// CONSTANT_COLOR or ONE_MINUS_CONSTANT_COLOR.
#[inline]
fn is_constant_colour(factor: u32) -> bool {
    factor == 0x8001 || factor == 0x8002
}

/// CONSTANT_ALPHA or ONE_MINUS_CONSTANT_ALPHA.
#[inline]
fn is_constant_alpha(factor: u32) -> bool {
    factor == 0x8003 || factor == 0x8004
}

#[inline]
fn refuse<C: GlDecodeContext>(context: &mut C, canvas_id: u32, error: Option<u32>) -> bool {
    match error {
        Some(code) => {
            context.push_error(canvas_id, code);
            false
        }
        None => true,
    }
}

/// `depthFunc`: a comparison function, else INVALID_ENUM.
#[inline]
pub fn validate_depth_func<C: GlDecodeContext>(context: &mut C, canvas_id: u32, func: u32) -> bool {
    refuse(
        context,
        canvas_id,
        (!is_comparison(func)).then_some(codes::INVALID_ENUM),
    )
}

/// `cullFace`: FRONT, BACK or FRONT_AND_BACK, else INVALID_ENUM.
#[inline]
pub fn validate_cull_face<C: GlDecodeContext>(context: &mut C, canvas_id: u32, mode: u32) -> bool {
    refuse(
        context,
        canvas_id,
        (!is_face(mode)).then_some(codes::INVALID_ENUM),
    )
}

/// `frontFace`: CW or CCW, else INVALID_ENUM.
#[inline]
pub fn validate_front_face<C: GlDecodeContext>(context: &mut C, canvas_id: u32, mode: u32) -> bool {
    refuse(
        context,
        canvas_id,
        (!matches!(mode, 0x0900 | 0x0901)).then_some(codes::INVALID_ENUM),
    )
}

/// `stencilFunc` and `stencilFuncSeparate` (whose `face` is `Some`): a face and a comparison function, else
/// INVALID_ENUM.
#[inline]
pub fn validate_stencil_func<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    face: Option<u32>,
    func: u32,
) -> bool {
    let valid = face.is_none_or(is_face) && is_comparison(func);
    refuse(context, canvas_id, (!valid).then_some(codes::INVALID_ENUM))
}

/// `stencilOp` and `stencilOpSeparate` (whose `face` is `Some`): a face and three stencil operations, else
/// INVALID_ENUM.
#[inline]
pub fn validate_stencil_op<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    face: Option<u32>,
    ops: [u32; 3],
) -> bool {
    let valid = face.is_none_or(is_face) && ops.iter().all(|&op| is_stencil_op(op));
    refuse(context, canvas_id, (!valid).then_some(codes::INVALID_ENUM))
}

/// `stencilMaskSeparate`: a face, else INVALID_ENUM.
#[inline]
pub fn validate_stencil_mask_separate<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    face: u32,
) -> bool {
    refuse(
        context,
        canvas_id,
        (!is_face(face)).then_some(codes::INVALID_ENUM),
    )
}

/// `blendFunc` (`alpha` `None`) and `blendFuncSeparate`: blend factors, else INVALID_ENUM; then a constant colour
/// factor with a constant alpha one among the colour factors is INVALID_OPERATION (WebGL 1.0 6.13: some
/// implementations cannot blend with both).
#[inline]
pub fn validate_blend_func<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    rgb: [u32; 2],
    alpha: Option<[u32; 2]>,
) -> bool {
    let [src, dst] = rgb;
    let factors_valid = is_blend_factor(src)
        && is_blend_factor(dst)
        && alpha.is_none_or(|[s, d]| is_blend_factor(s) && is_blend_factor(d));
    let error = if !factors_valid {
        Some(codes::INVALID_ENUM)
    } else if (is_constant_colour(src) && is_constant_alpha(dst))
        || (is_constant_alpha(src) && is_constant_colour(dst))
    {
        Some(codes::INVALID_OPERATION)
    } else {
        None
    };
    refuse(context, canvas_id, error)
}

/// `lineWidth`: a width above 0, else INVALID_VALUE (NaN is not one).
#[inline]
pub fn validate_line_width<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    width: f32,
) -> bool {
    refuse(
        context,
        canvas_id,
        (width.is_nan() || width <= 0.0).then_some(codes::INVALID_VALUE),
    )
}

/// `depthRange`: a near value past the far one is INVALID_OPERATION (WebGL 1.0 6.12).
#[inline]
pub fn validate_depth_range<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    near: f32,
    far: f32,
) -> bool {
    refuse(
        context,
        canvas_id,
        (near > far).then_some(codes::INVALID_OPERATION),
    )
}
