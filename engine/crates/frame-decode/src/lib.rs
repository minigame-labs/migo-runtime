//! Pass 2: a structurally-valid GL command stream becomes owned `GLCmd`s.
//!
//! Pass 1 -- `frame_wire::stream` -- checks the shape of the words: record
//! headers, word counts, opcodes in range. This is the pass that reads them,
//! and it produces exactly the `GLCmd` the corresponding raw op would build:
//! the same numeric conversions, the same null-id mapping, the same validator
//! calls. A command that arrived as a typed stream and one that arrived as a
//! direct op have to be indistinguishable downstream, or the two paths drift
//! and the difference shows up as a rendering bug on whichever one is less
//! tested.
//!
//! # Why this is not in the runtime crate
//!
//! It was. Decoding is engine-neutral work -- words in, render commands out --
//! but it lived beside the JavaScript runtime because that is where its only
//! caller was. On iOS the producer's JavaScript runs in WebKit's WebContent
//! process and this side links no engine at all, so a decoder reachable only
//! through the engine crate is a decoder that product cannot use.
//!
//! # The one thing it needs from its host
//!
//! Error reporting, and one piece of GL state. WebGL says a rejected call
//! pushes an error and is skipped rather than aborting the frame, so the
//! decoder has to be able to push one; and `bindBufferBase` on a transform
//! feedback buffer is illegal while capture is active, which only the host
//! knows. Both go through [`GlDecodeContext`], generic rather than a trait
//! object: this runs once per command on the render path, and a virtual call
//! per command is a cost with nothing to show for it.

use shared::protocol::render_cmd::{GLCmd, UniformF32Values, UniformI32Values};

use frame_wire::gl::{
    OP_ACTIVE_TEXTURE, OP_BIND_BUFFER, OP_BIND_BUFFER_BASE, OP_BIND_BUFFER_RANGE,
    OP_BIND_FRAMEBUFFER, OP_BIND_RENDERBUFFER, OP_BIND_SAMPLER, OP_BIND_TEXTURE,
    OP_BIND_VERTEX_ARRAY, OP_BLEND_COLOR, OP_BLEND_EQUATION, OP_BLEND_EQUATION_SEPARATE,
    OP_BLEND_FUNC, OP_BLEND_FUNC_SEPARATE, OP_CLEAR, OP_CLEAR_COLOR, OP_CLEAR_DEPTH,
    OP_CLEAR_STENCIL, OP_COLOR_MASK, OP_CULL_FACE, OP_DEPTH_FUNC, OP_DEPTH_MASK, OP_DEPTH_RANGE,
    OP_DISABLE, OP_DISABLE_VERTEX_ATTRIB_ARRAY, OP_DRAW_ARRAYS, OP_DRAW_ARRAYS_INSTANCED,
    OP_DRAW_ELEMENTS, OP_DRAW_ELEMENTS_INSTANCED, OP_ENABLE, OP_ENABLE_VERTEX_ATTRIB_ARRAY,
    OP_FRONT_FACE, OP_GENERATE_MIPMAP, OP_HINT, OP_LINE_WIDTH, OP_PIXEL_STORE_I, OP_POLYGON_OFFSET,
    OP_READ_BUFFER, OP_SAMPLER_PARAMETER_F, OP_SAMPLER_PARAMETER_I, OP_SCISSOR, OP_STENCIL_FUNC,
    OP_STENCIL_FUNC_SEPARATE, OP_STENCIL_MASK, OP_STENCIL_MASK_SEPARATE, OP_STENCIL_OP,
    OP_STENCIL_OP_SEPARATE, OP_TEX_PARAMETER_F, OP_TEX_PARAMETER_I, OP_UNIFORM_MATRIX2FV,
    OP_UNIFORM_MATRIX3FV, OP_UNIFORM_MATRIX4FV, OP_UNIFORM1F, OP_UNIFORM1FV, OP_UNIFORM1I,
    OP_UNIFORM1IV, OP_UNIFORM2F, OP_UNIFORM2FV, OP_UNIFORM2IV, OP_UNIFORM3F, OP_UNIFORM3FV,
    OP_UNIFORM3IV, OP_UNIFORM4F, OP_UNIFORM4FV, OP_UNIFORM4IV, OP_USE_PROGRAM,
    OP_VERTEX_ATTRIB_DIVISOR, OP_VERTEX_ATTRIB_POINTER, OP_VIEWPORT,
};
use frame_wire::stream::{ValidatedStream, opcode_of, word_count_of};

mod budget;
/// Canvas2D records. See its module docs for why 2D and GL share one stream.
pub mod canvas2d;
pub mod codes;
mod scratch;
pub mod validate;

pub use budget::{FrameDecodeBudget, FrameDecodeBudgetError, validate_frame_budget};
pub use validate::GlDecodeContext;

use validate::{
    validate_bind_buffer_base, validate_bind_buffer_range, validate_bind_buffer_target,
    validate_vertex_attrib_pointer, validate_viewport_like,
};

/// Reinterpret uniform words as floats. The producer wrote the bit patterns;
/// this is a reinterpretation, not a conversion, and doing it as one would
/// turn a NaN payload into a different NaN.
#[inline]
pub fn copy_f32_words(words: &[u32]) -> UniformF32Values {
    words.iter().map(|word| f32::from_bits(*word)).collect()
}

#[inline]
pub fn copy_i32_words(words: &[u32]) -> UniformI32Values {
    words
        .iter()
        .map(|word| i32::from_ne_bytes(word.to_ne_bytes()))
        .collect()
}

/// Decode a structurally-validated GL command stream into owned `GLCmd` values.
///
/// For each record, builds the same `GLCmd` that the corresponding raw op
/// would build — identical numeric conversions, identical null-id mapping,
/// identical validator calls.  Records that fail semantic validation push
/// an error into the per-canvas `WebGLErrorState` (just like raw ops) and
/// are skipped; decoding continues with the next record.
///
/// The GL-only view, and no product's submission path any more: both lanes
/// carry 2D records in the same stream and go through
/// [`decode_render_stream_into`]. What keeps this one is that it returns the
/// commands rather than filing them away, which is what a case checking that
/// one opcode decodes to the same `GLCmd` as the raw op needs to see. The
/// per-record work is the same function either way.
///
/// Returns the saturating approximate byte count for all accepted commands.
pub fn decode_validated_stream<C: GlDecodeContext>(
    context: &mut C,
    stream: ValidatedStream<'_>,
    out: &mut Vec<GLCmd>,
) -> usize {
    let words = stream.words();
    // words[0] = MAGIC, words[1] = VERSION, words[2..] = records.
    // Pass 1 guarantees: magic, version, all record headers/bodies are valid.
    let mut cursor: usize = 2;
    let used = words.len();
    let mut approx_bytes: usize = 0;

    while cursor < used {
        // Safe: Pass 1 guarantees header is present and wc > 0.
        let header = words[cursor];
        let opcode = opcode_of(header);
        let wc = word_count_of(header) as usize;

        // Safety: Pass 1 guarantees cursor + wc <= used.
        let record = &words[cursor..cursor + wc];
        // record[0] = header
        // record[1] = canvas_id  (for most opcodes; sampler_parameter* has no canvas field)

        let cmd_opt = decode_record(context, opcode, record);
        if let Some(cmd) = cmd_opt {
            approx_bytes = approx_bytes.saturating_add(cmd_approx_bytes(&cmd));
            out.push(cmd);
        }
        cursor += wc;
    }

    approx_bytes
}

/// Compute approximate byte cost of a single GLCmd.
/// Fixed/scalar: size_of::<GLCmd>().
/// Spilled variable uniform: size_of::<GLCmd>() + owned payload bytes.
#[inline]
fn cmd_approx_bytes(cmd: &GLCmd) -> usize {
    let base = size_of::<GLCmd>();
    match cmd {
        GLCmd::Uniform1iv { value, .. }
        | GLCmd::Uniform2iv { value, .. }
        | GLCmd::Uniform3iv { value, .. }
        | GLCmd::Uniform4iv { value, .. } => {
            if value.spilled() {
                base.saturating_add(value.capacity() * size_of::<i32>())
            } else {
                base
            }
        }
        GLCmd::Uniform1fv { value, .. }
        | GLCmd::Uniform2fv { value, .. }
        | GLCmd::Uniform3fv { value, .. }
        | GLCmd::Uniform4fv { value, .. }
        | GLCmd::UniformMatrix2fv { value, .. }
        | GLCmd::UniformMatrix3fv { value, .. }
        | GLCmd::UniformMatrix4fv { value, .. } => {
            if value.spilled() {
                base.saturating_add(value.capacity() * size_of::<f32>())
            } else {
                base
            }
        }
        _ => base,
    }
}

/// Map a single record to a GLCmd (or None if semantic validation fails).
/// `record[0]` = header word (includes opcode + word count).
/// `record[1]` = canvas_id for most ops; sampler_parameter* omit canvas.
#[allow(clippy::too_many_lines)]
fn decode_record<C: GlDecodeContext>(
    context: &mut C,
    opcode: u32,
    record: &[u32],
) -> Option<GLCmd> {
    // Helper: convert a u32 word to i32 (two's-complement reinterpretation).
    #[inline]
    fn i(w: u32) -> i32 {
        w as i32
    }
    // Helper: convert a u32 word to f32 (bit-exact, preserves NaN/-0/Inf).
    #[inline]
    fn f(w: u32) -> f32 {
        f32::from_bits(w)
    }
    // Helper: convert a u32 word to bool (Pass 1 guaranteed 0 or 1).
    #[inline]
    fn b(w: u32) -> bool {
        w != 0
    }
    // Helper: signed-id → Option<u32>: negative means None.
    #[inline]
    fn signed_id(w: u32) -> Option<u32> {
        if (w as i32) < 0 { None } else { Some(w) }
    }
    // Helper: zero-or-positive id → Option<u32>: 0 means None.
    #[inline]
    fn nonzero_id(w: u32) -> Option<u32> {
        if w == 0 { None } else { Some(w) }
    }
    // Helper: uniform location: negative means None.
    #[inline]
    fn location(w: u32) -> Option<u32> {
        if (w as i32) < 0 { None } else { Some(w) }
    }

    // For most opcodes: record[1] = canvas_id, record[2..] = payload.
    // SAMPLER_PARAMETER_I/F are exceptions (no canvas).

    match opcode {
        // ── 1: VIEWPORT: H C I I U U ───────────────────────────────────────────
        OP_VIEWPORT => {
            let canvas_id = record[1];
            let x = i(record[2]);
            let y = i(record[3]);
            let width = record[4];
            let height = record[5];
            Some(GLCmd::Viewport {
                canvas_id,
                x,
                y,
                width,
                height,
            })
        }

        // ── 2: CLEAR: H C U ────────────────────────────────────────────────────
        OP_CLEAR => {
            let canvas_id = record[1];
            let bit_field = record[2];
            Some(GLCmd::Clear {
                canvas_id,
                bit_field,
            })
        }

        // ── 3: CLEAR_COLOR: H C F F F F ────────────────────────────────────────
        OP_CLEAR_COLOR => {
            let canvas_id = record[1];
            Some(GLCmd::ClearColor {
                canvas_id,
                r: f(record[2]),
                g: f(record[3]),
                b: f(record[4]),
                a: f(record[5]),
            })
        }

        // ── 4: CLEAR_DEPTH: H C F ───────────────────────────────────────────────
        OP_CLEAR_DEPTH => {
            let canvas_id = record[1];
            Some(GLCmd::ClearDepth {
                canvas_id,
                depth: f(record[2]),
            })
        }

        // ── 5: CLEAR_STENCIL: H C I ─────────────────────────────────────────────
        OP_CLEAR_STENCIL => {
            let canvas_id = record[1];
            Some(GLCmd::ClearStencil {
                canvas_id,
                s: i(record[2]),
            })
        }

        // ── 6: ENABLE: H C U ────────────────────────────────────────────────────
        OP_ENABLE => {
            let canvas_id = record[1];
            Some(GLCmd::Enable {
                canvas_id,
                cap: record[2],
            })
        }

        // ── 7: DISABLE: H C U ───────────────────────────────────────────────────
        OP_DISABLE => {
            let canvas_id = record[1];
            Some(GLCmd::Disable {
                canvas_id,
                cap: record[2],
            })
        }

        // ── 8: USE_PROGRAM: H C U ───────────────────────────────────────────────
        OP_USE_PROGRAM => {
            let canvas_id = record[1];
            Some(GLCmd::UseProgram {
                canvas_id,
                program_id: record[2],
            })
        }

        // ── 9: BIND_BUFFER: H C U I ─────────────────────────────────────────────
        // Uses validate_bind_buffer_target; invalid target → error + skip.
        // buffer < 0 → None.
        OP_BIND_BUFFER => {
            let canvas_id = record[1];
            let target = record[2];
            let buffer = signed_id(record[3]);
            if !validate_bind_buffer_target(context, canvas_id, target) {
                return None;
            }
            Some(GLCmd::BindBuffer {
                canvas_id,
                target,
                buffer,
            })
        }

        // ── 10: BIND_TEXTURE: H C U I ───────────────────────────────────────────
        // texture < 0 → None.
        OP_BIND_TEXTURE => {
            let canvas_id = record[1];
            Some(GLCmd::BindTexture {
                canvas_id,
                target: record[2],
                texture: signed_id(record[3]),
            })
        }

        // ── 11: ACTIVE_TEXTURE: H C U ───────────────────────────────────────────
        OP_ACTIVE_TEXTURE => {
            let canvas_id = record[1];
            Some(GLCmd::ActiveTexture {
                canvas_id,
                unit: record[2],
            })
        }

        // ── 12: BIND_FRAMEBUFFER: H C U I ───────────────────────────────────────
        // framebuffer < 0 → None.
        OP_BIND_FRAMEBUFFER => {
            let canvas_id = record[1];
            Some(GLCmd::BindFramebuffer {
                canvas_id,
                target: record[2],
                framebuffer: signed_id(record[3]),
            })
        }

        // ── 13: BIND_RENDERBUFFER: H C U I ──────────────────────────────────────
        // renderbuffer < 0 → None.
        OP_BIND_RENDERBUFFER => {
            let canvas_id = record[1];
            Some(GLCmd::BindRenderbuffer {
                canvas_id,
                target: record[2],
                renderbuffer: signed_id(record[3]),
            })
        }

        // ── 14: BIND_VERTEX_ARRAY: H C U ────────────────────────────────────────
        // vao == 0 → None.
        OP_BIND_VERTEX_ARRAY => {
            let canvas_id = record[1];
            Some(GLCmd::BindVertexArray {
                canvas_id,
                vao: nonzero_id(record[2]),
            })
        }

        // ── 15: BIND_SAMPLER: H C U U ───────────────────────────────────────────
        // sampler == 0 → None.
        OP_BIND_SAMPLER => {
            let canvas_id = record[1];
            Some(GLCmd::BindSampler {
                canvas_id,
                unit: record[2],
                sampler: nonzero_id(record[3]),
            })
        }

        // ── 16: ENABLE_VERTEX_ATTRIB_ARRAY: H C U ───────────────────────────────
        OP_ENABLE_VERTEX_ATTRIB_ARRAY => {
            let canvas_id = record[1];
            Some(GLCmd::EnableVertexAttribArray {
                canvas_id,
                index: record[2],
            })
        }

        // ── 17: DISABLE_VERTEX_ATTRIB_ARRAY: H C U ──────────────────────────────
        OP_DISABLE_VERTEX_ATTRIB_ARRAY => {
            let canvas_id = record[1];
            Some(GLCmd::DisableVertexAttribArray {
                canvas_id,
                index: record[2],
            })
        }

        // ── 18: VERTEX_ATTRIB_POINTER: H C U I U B I I ──────────────────────────
        // Uses validate_vertex_attrib_pointer; invalid → error + skip.
        OP_VERTEX_ATTRIB_POINTER => {
            let canvas_id = record[1];
            let index = record[2];
            let size = i(record[3]);
            let type_ = record[4];
            let normalized = b(record[5]);
            let stride = i(record[6]);
            let offset = i(record[7]);
            if !validate_vertex_attrib_pointer(context, canvas_id, size, type_, stride, offset) {
                return None;
            }
            Some(GLCmd::VertexAttribPointer {
                canvas_id,
                index,
                size,
                type_,
                normalized,
                stride,
                offset,
            })
        }

        // ── 19: VERTEX_ATTRIB_DIVISOR: H C U U ──────────────────────────────────
        OP_VERTEX_ATTRIB_DIVISOR => {
            let canvas_id = record[1];
            Some(GLCmd::VertexAttribDivisor {
                canvas_id,
                index: record[2],
                divisor: record[3],
            })
        }

        // ── 20: BLEND_FUNC: H C U U ─────────────────────────────────────────────
        OP_BLEND_FUNC => {
            let canvas_id = record[1];
            Some(GLCmd::BlendFunc {
                canvas_id,
                sfactor: record[2],
                dfactor: record[3],
            })
        }

        // ── 21: BLEND_FUNC_SEPARATE: H C U U U U ────────────────────────────────
        OP_BLEND_FUNC_SEPARATE => {
            let canvas_id = record[1];
            Some(GLCmd::BlendFuncSeparate {
                canvas_id,
                src_rgb: record[2],
                dst_rgb: record[3],
                src_alpha: record[4],
                dst_alpha: record[5],
            })
        }

        // ── 22: BLEND_EQUATION: H C U ───────────────────────────────────────────
        OP_BLEND_EQUATION => {
            let canvas_id = record[1];
            Some(GLCmd::BlendEquation {
                canvas_id,
                mode: record[2],
            })
        }

        // ── 23: BLEND_EQUATION_SEPARATE: H C U U ────────────────────────────────
        OP_BLEND_EQUATION_SEPARATE => {
            let canvas_id = record[1];
            Some(GLCmd::BlendEquationSeparate {
                canvas_id,
                mode_rgb: record[2],
                mode_alpha: record[3],
            })
        }

        // ── 24: BLEND_COLOR: H C F F F F ────────────────────────────────────────
        OP_BLEND_COLOR => {
            let canvas_id = record[1];
            Some(GLCmd::BlendColor {
                canvas_id,
                r: f(record[2]),
                g: f(record[3]),
                b: f(record[4]),
                a: f(record[5]),
            })
        }

        // ── 25: DEPTH_FUNC: H C U ───────────────────────────────────────────────
        OP_DEPTH_FUNC => {
            let canvas_id = record[1];
            Some(GLCmd::DepthFunc {
                canvas_id,
                func: record[2],
            })
        }

        // ── 26: DEPTH_MASK: H C B ────────────────────────────────────────────────
        OP_DEPTH_MASK => {
            let canvas_id = record[1];
            Some(GLCmd::DepthMask {
                canvas_id,
                flag: b(record[2]),
            })
        }

        // ── 27: DEPTH_RANGE: H C F F ─────────────────────────────────────────────
        OP_DEPTH_RANGE => {
            let canvas_id = record[1];
            Some(GLCmd::DepthRange {
                canvas_id,
                near: f(record[2]),
                far: f(record[3]),
            })
        }

        // ── 28: STENCIL_FUNC: H C U I U ─────────────────────────────────────────
        OP_STENCIL_FUNC => {
            let canvas_id = record[1];
            Some(GLCmd::StencilFunc {
                canvas_id,
                func: record[2],
                ref_: i(record[3]),
                mask: record[4],
            })
        }

        // ── 29: STENCIL_FUNC_SEPARATE: H C U U I U ──────────────────────────────
        OP_STENCIL_FUNC_SEPARATE => {
            let canvas_id = record[1];
            Some(GLCmd::StencilFuncSeparate {
                canvas_id,
                face: record[2],
                func: record[3],
                ref_: i(record[4]),
                mask: record[5],
            })
        }

        // ── 30: STENCIL_OP: H C U U U ───────────────────────────────────────────
        OP_STENCIL_OP => {
            let canvas_id = record[1];
            Some(GLCmd::StencilOp {
                canvas_id,
                fail: record[2],
                zfail: record[3],
                zpass: record[4],
            })
        }

        // ── 31: STENCIL_OP_SEPARATE: H C U U U U ────────────────────────────────
        OP_STENCIL_OP_SEPARATE => {
            let canvas_id = record[1];
            Some(GLCmd::StencilOpSeparate {
                canvas_id,
                face: record[2],
                fail: record[3],
                zfail: record[4],
                zpass: record[5],
            })
        }

        // ── 32: STENCIL_MASK: H C U ─────────────────────────────────────────────
        OP_STENCIL_MASK => {
            let canvas_id = record[1];
            Some(GLCmd::StencilMask {
                canvas_id,
                mask: record[2],
            })
        }

        // ── 33: STENCIL_MASK_SEPARATE: H C U U ──────────────────────────────────
        OP_STENCIL_MASK_SEPARATE => {
            let canvas_id = record[1];
            Some(GLCmd::StencilMaskSeparate {
                canvas_id,
                face: record[2],
                mask: record[3],
            })
        }

        // ── 34: CULL_FACE: H C U ────────────────────────────────────────────────
        OP_CULL_FACE => {
            let canvas_id = record[1];
            Some(GLCmd::CullFace {
                canvas_id,
                mode: record[2],
            })
        }

        // ── 35: FRONT_FACE: H C U ───────────────────────────────────────────────
        OP_FRONT_FACE => {
            let canvas_id = record[1];
            Some(GLCmd::FrontFace {
                canvas_id,
                mode: record[2],
            })
        }

        // ── 36: COLOR_MASK: H C B B B B ─────────────────────────────────────────
        OP_COLOR_MASK => {
            let canvas_id = record[1];
            Some(GLCmd::ColorMask {
                canvas_id,
                r: b(record[2]),
                g: b(record[3]),
                b: b(record[4]),
                a: b(record[5]),
            })
        }

        // ── 37: SCISSOR: H C I I I I ─────────────────────────────────────────────
        // Uses validate_viewport_like (same as op_scissor); invalid → error + skip.
        OP_SCISSOR => {
            let canvas_id = record[1];
            let x = i(record[2]);
            let y = i(record[3]);
            let width = i(record[4]);
            let height = i(record[5]);
            if !validate_viewport_like(context, canvas_id, width, height) {
                return None;
            }
            Some(GLCmd::Scissor {
                canvas_id,
                x,
                y,
                width,
                height,
            })
        }

        // ── 38: LINE_WIDTH: H C F ────────────────────────────────────────────────
        OP_LINE_WIDTH => {
            let canvas_id = record[1];
            Some(GLCmd::LineWidth {
                canvas_id,
                width: f(record[2]),
            })
        }

        // ── 39: POLYGON_OFFSET: H C F F ─────────────────────────────────────────
        OP_POLYGON_OFFSET => {
            let canvas_id = record[1];
            Some(GLCmd::PolygonOffset {
                canvas_id,
                factor: f(record[2]),
                units: f(record[3]),
            })
        }

        // ── 40: TEX_PARAMETER_I: H C U U I ──────────────────────────────────────
        OP_TEX_PARAMETER_I => {
            let canvas_id = record[1];
            Some(GLCmd::TexParameteri {
                canvas_id,
                target: record[2],
                pname: record[3],
                param: i(record[4]),
            })
        }

        // ── 41: TEX_PARAMETER_F: H C U U F ──────────────────────────────────────
        OP_TEX_PARAMETER_F => {
            let canvas_id = record[1];
            Some(GLCmd::TexParameterf {
                canvas_id,
                target: record[2],
                pname: record[3],
                param: f(record[4]),
            })
        }

        // ── 42: GENERATE_MIPMAP: H C U ───────────────────────────────────────────
        OP_GENERATE_MIPMAP => {
            let canvas_id = record[1];
            Some(GLCmd::GenerateMipmap {
                canvas_id,
                target: record[2],
            })
        }

        // ── 43: PIXEL_STORE_I: H C U I ───────────────────────────────────────────
        OP_PIXEL_STORE_I => {
            let canvas_id = record[1];
            Some(GLCmd::PixelStorei {
                canvas_id,
                pname: record[2],
                param: i(record[3]),
            })
        }

        // ── 44: HINT: H C U U ────────────────────────────────────────────────────
        OP_HINT => {
            let canvas_id = record[1];
            Some(GLCmd::Hint {
                canvas_id,
                target: record[2],
                mode: record[3],
            })
        }

        // ── 45: SAMPLER_PARAMETER_I: H U U I ────────────────────────────────────
        // NOTE: no canvas_id field in wire format; record layout: H sampler pname param.
        OP_SAMPLER_PARAMETER_I => Some(GLCmd::SamplerParameteri {
            sampler: record[1],
            pname: record[2],
            param: i(record[3]),
        }),

        // ── 46: SAMPLER_PARAMETER_F: H U U F ────────────────────────────────────
        // NOTE: no canvas_id field; record layout: H sampler pname param.
        OP_SAMPLER_PARAMETER_F => Some(GLCmd::SamplerParameterf {
            sampler: record[1],
            pname: record[2],
            param: f(record[3]),
        }),

        // ── 47: DRAW_ARRAYS: H C U I I ───────────────────────────────────────────
        OP_DRAW_ARRAYS => {
            let canvas_id = record[1];
            Some(GLCmd::DrawArrays {
                canvas_id,
                mode: record[2],
                first: i(record[3]),
                count: i(record[4]),
            })
        }

        // ── 48: DRAW_ELEMENTS: H C U I U I ───────────────────────────────────────
        OP_DRAW_ELEMENTS => {
            let canvas_id = record[1];
            Some(GLCmd::DrawElements {
                canvas_id,
                mode: record[2],
                count: i(record[3]),
                index_type: record[4],
                offset: i(record[5]),
            })
        }

        // ── 49: DRAW_ARRAYS_INSTANCED: H C U I I I ───────────────────────────────
        OP_DRAW_ARRAYS_INSTANCED => {
            let canvas_id = record[1];
            Some(GLCmd::DrawArraysInstanced {
                canvas_id,
                mode: record[2],
                first: i(record[3]),
                count: i(record[4]),
                instance_count: i(record[5]),
            })
        }

        // ── 50: DRAW_ELEMENTS_INSTANCED: H C U I U I I ───────────────────────────
        OP_DRAW_ELEMENTS_INSTANCED => {
            let canvas_id = record[1];
            Some(GLCmd::DrawElementsInstanced {
                canvas_id,
                mode: record[2],
                count: i(record[3]),
                index_type: record[4],
                offset: i(record[5]),
                instance_count: i(record[6]),
            })
        }

        // ── 51: BIND_BUFFER_BASE: H C U U U ─────────────────────────────────────
        // Delegates to bind_buffer_base_impl which validates + pushes to collector.
        // Because the impl already calls queue_gl_fire_and_forget we do NOT push to
        // `out` here — the command is already in the collector.  Task 3 will change
        // this flow; for now decode_validated_stream is called before the collector
        // is set up in the test harness, so we call the impl and separately capture
        // the GLCmd for tests using the decode-only path.
        //
        // Actually: Task 2's contract says decode pushes to `out` for batch append
        // (Task 3 will do the appending).  We must NOT call bind_buffer_base_impl
        // here because that would double-push via queue_gl_fire_and_forget.
        // Instead mirror its exact logic manually.
        OP_BIND_BUFFER_BASE => {
            let canvas_id = record[1];
            let target = record[2];
            let index = record[3];
            let buffer_raw = record[4];
            let buffer = nonzero_id(buffer_raw);
            if !validate_bind_buffer_base(context, canvas_id, target, index, buffer) {
                return None;
            }
            Some(GLCmd::BindBufferBase {
                canvas_id,
                target,
                index,
                buffer,
            })
        }

        // ── 52: BIND_BUFFER_RANGE: H C U U U I I ────────────────────────────────
        // Same as bind_buffer_base: mirror the impl logic manually.
        OP_BIND_BUFFER_RANGE => {
            let canvas_id = record[1];
            let target = record[2];
            let index = record[3];
            let buffer_raw = record[4];
            let buffer = nonzero_id(buffer_raw);
            let offset = i(record[5]);
            let size = i(record[6]);
            if !validate_bind_buffer_range(context, canvas_id, target, index, buffer, offset, size)
            {
                return None;
            }
            Some(GLCmd::BindBufferRange {
                canvas_id,
                target,
                index,
                buffer,
                offset,
                size,
            })
        }

        // ── 53: READ_BUFFER: H C U ───────────────────────────────────────────────
        OP_READ_BUFFER => {
            let canvas_id = record[1];
            Some(GLCmd::ReadBuffer {
                canvas_id,
                src: record[2],
            })
        }

        // ── 54: UNIFORM1I: H C I I ───────────────────────────────────────────────
        OP_UNIFORM1I => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let x = i(record[3]);
            Some(GLCmd::Uniform1i {
                canvas_id,
                location: loc,
                x,
            })
        }

        // ── 55: UNIFORM1F: H C I F ───────────────────────────────────────────────
        OP_UNIFORM1F => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let x = f(record[3]);
            Some(GLCmd::Uniform1f {
                canvas_id,
                location: loc,
                x,
            })
        }

        // ── 56: UNIFORM2F: H C I F F ─────────────────────────────────────────────
        OP_UNIFORM2F => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            Some(GLCmd::Uniform2f {
                canvas_id,
                location: loc,
                x: f(record[3]),
                y: f(record[4]),
            })
        }

        // ── 57: UNIFORM3F: H C I F F F ───────────────────────────────────────────
        OP_UNIFORM3F => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            Some(GLCmd::Uniform3f {
                canvas_id,
                location: loc,
                x: f(record[3]),
                y: f(record[4]),
                z: f(record[5]),
            })
        }

        // ── 58: UNIFORM4F: H C I F F F F ─────────────────────────────────────────
        OP_UNIFORM4F => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            Some(GLCmd::Uniform4f {
                canvas_id,
                location: loc,
                x: f(record[3]),
                y: f(record[4]),
                z: f(record[5]),
                w: f(record[6]),
            })
        }

        // ── Variable vector uniforms (256..263) ───────────────────────────────────
        // Layout: H C location:I payload...  (word_count = 3 + payload_words)
        // record[0]=H, record[1]=C, record[2]=location, record[3..]=payload
        OP_UNIFORM1IV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let value: UniformI32Values = copy_i32_words(&record[3..]);
            Some(GLCmd::Uniform1iv {
                canvas_id,
                location: loc,
                value,
            })
        }

        OP_UNIFORM1FV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let value: UniformF32Values = copy_f32_words(&record[3..]);
            Some(GLCmd::Uniform1fv {
                canvas_id,
                location: loc,
                value,
            })
        }

        OP_UNIFORM2IV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let value: UniformI32Values = copy_i32_words(&record[3..]);
            Some(GLCmd::Uniform2iv {
                canvas_id,
                location: loc,
                value,
            })
        }

        OP_UNIFORM2FV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let value: UniformF32Values = copy_f32_words(&record[3..]);
            Some(GLCmd::Uniform2fv {
                canvas_id,
                location: loc,
                value,
            })
        }

        OP_UNIFORM3IV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let value: UniformI32Values = copy_i32_words(&record[3..]);
            Some(GLCmd::Uniform3iv {
                canvas_id,
                location: loc,
                value,
            })
        }

        OP_UNIFORM3FV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let value: UniformF32Values = copy_f32_words(&record[3..]);
            Some(GLCmd::Uniform3fv {
                canvas_id,
                location: loc,
                value,
            })
        }

        OP_UNIFORM4IV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let value: UniformI32Values = copy_i32_words(&record[3..]);
            Some(GLCmd::Uniform4iv {
                canvas_id,
                location: loc,
                value,
            })
        }

        OP_UNIFORM4FV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let value: UniformF32Values = copy_f32_words(&record[3..]);
            Some(GLCmd::Uniform4fv {
                canvas_id,
                location: loc,
                value,
            })
        }

        // ── Variable matrix uniforms (264..266) ────────────────────────────────────
        // Layout: H C location:I transpose:B payload...  (word_count = 4 + payload_words)
        // record[0]=H, record[1]=C, record[2]=location, record[3]=transpose, record[4..]=payload
        OP_UNIFORM_MATRIX2FV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let transpose = b(record[3]);
            let value: UniformF32Values = copy_f32_words(&record[4..]);
            Some(GLCmd::UniformMatrix2fv {
                canvas_id,
                location: loc,
                transpose,
                value,
            })
        }

        OP_UNIFORM_MATRIX3FV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let transpose = b(record[3]);
            let value: UniformF32Values = copy_f32_words(&record[4..]);
            Some(GLCmd::UniformMatrix3fv {
                canvas_id,
                location: loc,
                transpose,
                value,
            })
        }

        OP_UNIFORM_MATRIX4FV => {
            let canvas_id = record[1];
            let loc = location(record[2]);
            let transpose = b(record[3]);
            let value: UniformF32Values = copy_f32_words(&record[4..]);
            Some(GLCmd::UniformMatrix4fv {
                canvas_id,
                location: loc,
                transpose,
                value,
            })
        }

        // Unreachable: Pass 1 guarantees all opcodes are in the allowed set.
        _ => {
            debug_assert!(
                false,
                "decode_record: opcode {opcode} slipped through pass 1"
            );
            None
        }
    }
}

/// Where a decoded mixed stream's work goes.
///
/// Two products consume the same stream and want it in different shapes. The
/// cross-process lane builds [`shared::protocol::FrameOp`]s and hands the frame
/// to the renderer whole. The in-process runtime already owns a collector that
/// groups a frame its own way, and it needs each 2D command individually --
/// dirty rectangles are marked per command -- and each GL batch together with
/// the byte count the decode measured, because that count is what bounds the
/// memory a frame may pin.
///
/// What must *not* have two versions is the order: which batch ends where, and
/// where the materialize barriers fall. So that lives once, in
/// [`decode_render_stream_into`], and this trait is the only thing that varies.
///
/// # Why the sink and the decode context are one object at the call site
///
/// [`decode_render_stream_into`] takes a single value that is both. The
/// in-process implementation reaches its error queue and its collector through
/// the same `OpState`, and `OpState` hands out one mutable borrow at a time: a
/// separate context and sink would each want that borrow, and the two could not
/// coexist. Requiring one object costs the cross-process lane nothing -- its
/// implementation is a two-field struct -- and it is what lets the in-process
/// lane decode straight into the collector with no intermediate buffer.
pub trait RenderSink {
    /// One run of 2D commands on one canvas, in the order they were issued.
    fn canvas_batch(
        &mut self,
        canvas_id: u32,
        commands: shared::command_vec_pool::PooledVec<shared::protocol::render_cmd::Canvas2DCmd>,
    );

    /// One run of GL commands, with the approximate heap footprint the decode
    /// measured while building them.
    fn gl_batch(
        &mut self,
        commands: shared::command_vec_pool::PooledVec<GLCmd>,
        approx_bytes: usize,
    );

    /// The renderer must flush this canvas's 2D work before what follows.
    fn materialize(&mut self, canvas_id: u32);
}

/// The [`RenderSink`] that builds `FrameOp`s, paired with a decode context.
///
/// The pairing is what makes [`decode_render_stream`] keep its old signature
/// while the decoder underneath takes one object.
struct FrameOpSink<'a, C: GlDecodeContext> {
    context: &'a mut C,
    out: &'a mut Vec<shared::protocol::FrameOp>,
}

impl<C: GlDecodeContext> GlDecodeContext for FrameOpSink<'_, C> {
    #[inline]
    fn push_error(&mut self, canvas_id: u32, code: u32) {
        self.context.push_error(canvas_id, code);
    }

    #[inline]
    fn transform_feedback_captures(&self, canvas_id: u32) -> bool {
        self.context.transform_feedback_captures(canvas_id)
    }
}

impl<C: GlDecodeContext> RenderSink for FrameOpSink<'_, C> {
    fn canvas_batch(
        &mut self,
        canvas_id: u32,
        commands: shared::command_vec_pool::PooledVec<shared::protocol::render_cmd::Canvas2DCmd>,
    ) {
        self.out.push(shared::protocol::FrameOp::CanvasBatch(
            shared::protocol::render_cmd::CanvasBatchPayload {
                canvas_id,
                commands,
                present: false,
                dirty_rect: None,
            },
        ));
    }

    fn gl_batch(
        &mut self,
        commands: shared::command_vec_pool::PooledVec<GLCmd>,
        _approx_bytes: usize,
    ) {
        self.out.push(shared::protocol::FrameOp::GlBatch(
            shared::protocol::render_cmd::GlBatchPayload { commands },
        ));
    }

    fn materialize(&mut self, canvas_id: u32) {
        self.out
            .push(shared::protocol::FrameOp::Materialize { canvas_id });
    }
}

/// Decode a mixed 2D/GL stream into ordered frame operations.
///
/// The `FrameOp` view of [`decode_render_stream_into`], for the lane that hands
/// a whole frame to the renderer.
///
/// Returns the number of commands decoded, for the caller's own accounting.
pub fn decode_render_stream<C: GlDecodeContext>(
    context: &mut C,
    stream: ValidatedStream<'_>,
    out: &mut Vec<shared::protocol::FrameOp>,
) -> usize {
    let mut sink = FrameOpSink { context, out };
    decode_render_stream_into(&mut sink, stream)
}

/// Decode a mixed 2D/GL stream into a [`RenderSink`].
///
/// # Order, and the barrier that preserves it
///
/// A frame draws its background with 2D, its sprites with GL, its HUD with 2D.
/// The renderer must see those in the order they were issued, and it must see
/// the 2D work *materialized* before the GL work that draws over it: Canvas2D
/// content lives in a surface the GL path reads, and a GL batch that ran before
/// the 2D batch behind it was flushed would sample whatever was there before.
/// So a 2D→GL boundary materializes every canvas with pending work, exactly as
/// the in-process collector does.
///
/// Returns the number of commands decoded, for the caller's own accounting.
pub fn decode_render_stream_into<T: GlDecodeContext + RenderSink>(
    target: &mut T,
    stream: ValidatedStream<'_>,
) -> usize {
    use shared::command_vec_pool::PooledVec;
    use shared::protocol::render_cmd::Canvas2DCmd;

    // `words()` is already the used prefix: `validate_stream` bounds the slice
    // it hands back, which is what makes the walk below need no length check of
    // its own.
    let words = stream.words();
    let used = words.len();

    let mut decoded = 0usize;
    let mut gl: Option<PooledVec<GLCmd>> = None;
    // Counted as the commands are built rather than walked again afterwards.
    // The walk is a match per command on the render path, and the sink needs
    // the number for exactly one batch at a time.
    let mut gl_bytes = 0usize;
    let mut canvas: Option<PooledVec<Canvas2DCmd>> = None;
    // The canvas the 2D records apply to. `None` until a SELECT_CANVAS arrives,
    // and a 2D record before one is a producer that never said where to draw.
    let mut canvas_id: Option<u32> = None;
    // Canvases whose 2D work the renderer has not flushed yet.
    let plan = budget::estimate(&stream);
    let mut pending_materialize = scratch::MaterializeScratch::take(plan.pending_canvas_capacity);

    // Flushing 2D before GL, never the other way round: the whole point of the
    // barrier is that GL sees materialized 2D content.
    macro_rules! flush_canvas {
        () => {
            if let Some(commands) = canvas.take() {
                let id = canvas_id.unwrap_or(0);
                target.canvas_batch(id, commands);
                if !pending_materialize.contains(&id) {
                    pending_materialize.push(id);
                }
            }
        };
    }
    macro_rules! flush_gl {
        () => {
            if let Some(commands) = gl.take() {
                target.gl_batch(commands, std::mem::take(&mut gl_bytes));
            }
        };
    }

    let mut cursor = 2; // past magic and version
    while cursor < used {
        let record_start = cursor;
        let header = words[cursor];
        let opcode = opcode_of(header);
        let word_count = word_count_of(header) as usize;
        let record = &words[cursor..cursor + word_count];
        cursor += word_count;

        if opcode == frame_wire::canvas2d::OP2D_SELECT_CANVAS {
            // A different canvas means a different batch, even for 2D work
            // either side of it: one batch carries one canvas id.
            flush_canvas!();
            canvas_id = Some(record[1]);
            continue;
        }

        if opcode >= frame_wire::canvas2d::OP2D_BASE {
            if canvas_id.is_none() {
                // Reported rather than guessed. Drawing on canvas zero because
                // the producer forgot to say which is how content ends up
                // painting over something else.
                target.push_error(0, codes::INVALID_OPERATION);
                continue;
            }
            flush_gl!();
            if let Some(command) = canvas2d::decode_record(opcode, record) {
                canvas
                    .get_or_insert_with(|| {
                        PooledVec::take_with_capacity_limit(budget::batch_capacity(
                            words,
                            record_start,
                            true,
                            true,
                        ))
                    })
                    .push(command);
                decoded += 1;
            }
            continue;
        }

        // GL. Anything pending on the 2D side has to reach the surface first.
        flush_canvas!();
        for id in pending_materialize.drain(..) {
            target.materialize(id);
        }
        if let Some(command) = decode_record(target, opcode, record) {
            gl_bytes = gl_bytes.saturating_add(cmd_approx_bytes(&command));
            gl.get_or_insert_with(|| {
                PooledVec::take_with_capacity_limit(budget::batch_capacity(
                    words,
                    record_start,
                    false,
                    canvas_id.is_some(),
                ))
            })
            .push(command);
            decoded += 1;
        }
    }

    flush_canvas!();
    flush_gl!();
    // Trailing 2D work is materialized too: a sync readback or the next frame's
    // GL has to see it, and the renderer has no other signal that the batch
    // ended.
    for id in pending_materialize.drain(..) {
        target.materialize(id);
    }

    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spilled_uniform_approx_bytes_counts_allocated_capacity() {
        let mut value = UniformF32Values::with_capacity(64);
        value.extend(std::iter::repeat_n(1.0, 17));
        assert!(value.spilled());
        assert!(value.capacity() > value.len());

        let cmd = GLCmd::Uniform1fv {
            canvas_id: 1,
            location: Some(2),
            value,
        };
        assert_eq!(cmd_approx_bytes(&cmd), cmd.approx_deep_size_bytes());
    }
}
