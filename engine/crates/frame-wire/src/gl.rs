//! The WebGL block: opcodes 1..=70 fixed, 256..=276 variable, and the shape of
//! each record.
//!
//! One number per call is all that crosses. The producer writes an opcode and
//! the reader switches on it, so a value added here and not to the encoders --
//! this crate's JavaScript one and the WebContent producer's -- is a record the
//! reader rejects, on a device, with the frame not drawing and nothing in the
//! log that names it. `scripts/test-render-opcode-agreement.sh` parses all three
//! tables and requires them to agree.

use crate::stream::{RecordSpec, UniformElementKind};

/// The most payload words one variable-uniform record carries.
///
/// A packet already bounds the stream, so this is the record's own bound: a
/// header that claims more than a packet could have held is refused before the
/// decoder is asked to copy it.
///
/// It is deliberately NOT the engine encoder's inline bound (512 words, in
/// `00_render_command_stream.js`). That one decides when the engine stops
/// writing a uniform into its own stream buffer and calls the op instead --
/// and a uniform past it still has to reach the host, because the embedded
/// execution sets it: a skinned mesh's bone matrices are thousands of words,
/// and a lane bounded at the encoder's number would drop the character rather
/// than the optimisation. The two numbers answer different questions.
///
/// 64 Ki words is 256 KiB: four thousand `mat4`s, past any uniform array a
/// device will hold, and far enough below a packet that a header claiming more
/// is refused by this one comparison rather than by arithmetic over the stream.
pub const MAX_STREAM_UNIFORM_WORDS: u32 = 64 * 1024;

// ─── Fixed opcode constants (1..=70) ─────────────────────────────────────────

pub const OP_VIEWPORT: u32 = 1;
pub const OP_CLEAR: u32 = 2;
pub const OP_CLEAR_COLOR: u32 = 3;
pub const OP_CLEAR_DEPTH: u32 = 4;
pub const OP_CLEAR_STENCIL: u32 = 5;
pub const OP_ENABLE: u32 = 6;
pub const OP_DISABLE: u32 = 7;
pub const OP_USE_PROGRAM: u32 = 8;
pub const OP_BIND_BUFFER: u32 = 9;
pub const OP_BIND_TEXTURE: u32 = 10;
pub const OP_ACTIVE_TEXTURE: u32 = 11;
pub const OP_BIND_FRAMEBUFFER: u32 = 12;
pub const OP_BIND_RENDERBUFFER: u32 = 13;
pub const OP_BIND_VERTEX_ARRAY: u32 = 14;
pub const OP_BIND_SAMPLER: u32 = 15;
pub const OP_ENABLE_VERTEX_ATTRIB_ARRAY: u32 = 16;
pub const OP_DISABLE_VERTEX_ATTRIB_ARRAY: u32 = 17;
pub const OP_VERTEX_ATTRIB_POINTER: u32 = 18;
pub const OP_VERTEX_ATTRIB_DIVISOR: u32 = 19;
pub const OP_BLEND_FUNC: u32 = 20;
pub const OP_BLEND_FUNC_SEPARATE: u32 = 21;
pub const OP_BLEND_EQUATION: u32 = 22;
pub const OP_BLEND_EQUATION_SEPARATE: u32 = 23;
pub const OP_BLEND_COLOR: u32 = 24;
pub const OP_DEPTH_FUNC: u32 = 25;
pub const OP_DEPTH_MASK: u32 = 26;
pub const OP_DEPTH_RANGE: u32 = 27;
pub const OP_STENCIL_FUNC: u32 = 28;
pub const OP_STENCIL_FUNC_SEPARATE: u32 = 29;
pub const OP_STENCIL_OP: u32 = 30;
pub const OP_STENCIL_OP_SEPARATE: u32 = 31;
pub const OP_STENCIL_MASK: u32 = 32;
pub const OP_STENCIL_MASK_SEPARATE: u32 = 33;
pub const OP_CULL_FACE: u32 = 34;
pub const OP_FRONT_FACE: u32 = 35;
pub const OP_COLOR_MASK: u32 = 36;
pub const OP_SCISSOR: u32 = 37;
pub const OP_LINE_WIDTH: u32 = 38;
pub const OP_POLYGON_OFFSET: u32 = 39;
pub const OP_TEX_PARAMETER_I: u32 = 40;
pub const OP_TEX_PARAMETER_F: u32 = 41;
pub const OP_GENERATE_MIPMAP: u32 = 42;
pub const OP_PIXEL_STORE_I: u32 = 43;
pub const OP_HINT: u32 = 44;
pub const OP_SAMPLER_PARAMETER_I: u32 = 45;
pub const OP_SAMPLER_PARAMETER_F: u32 = 46;
pub const OP_DRAW_ARRAYS: u32 = 47;
pub const OP_DRAW_ELEMENTS: u32 = 48;
pub const OP_DRAW_ARRAYS_INSTANCED: u32 = 49;
pub const OP_DRAW_ELEMENTS_INSTANCED: u32 = 50;
pub const OP_BIND_BUFFER_BASE: u32 = 51;
pub const OP_BIND_BUFFER_RANGE: u32 = 52;
pub const OP_READ_BUFFER: u32 = 53;
pub const OP_UNIFORM1I: u32 = 54;
pub const OP_UNIFORM1F: u32 = 55;
pub const OP_UNIFORM2F: u32 = 56;
pub const OP_UNIFORM3F: u32 = 57;
pub const OP_UNIFORM4F: u32 = 58;

// `vertexAttrib{1,2,3,4}f[v]` all cross as one record (the components a call leaves out are 0, 0, 0, 1 as the
// specification has them, written by the encoder), `vertexAttribI4{i,ui}[v]` as one each, and `vertexAttribIPointer`
// is `vertexAttribPointer` without a `normalized` word.
pub const OP_VERTEX_ATTRIB_4F: u32 = 59;
pub const OP_VERTEX_ATTRIB_I4I: u32 = 60;
pub const OP_VERTEX_ATTRIB_I4UI: u32 = 61;
pub const OP_VERTEX_ATTRIB_I_POINTER: u32 = 62;

// `clearBufferfv`, `clearBufferiv` and `clearBufferuiv` (WebGL 2) cross as one record each: the buffer enum, the draw
// buffer, and four value words (COLOR uses all four, DEPTH and STENCIL the first; the encoder writes zeros for the
// rest). `clearBufferfi` carries the depth and the stencil.
pub const OP_CLEAR_BUFFER_FV: u32 = 63;
pub const OP_CLEAR_BUFFER_IV: u32 = 64;
pub const OP_CLEAR_BUFFER_UIV: u32 = 65;
pub const OP_CLEAR_BUFFER_FI: u32 = 66;

// The copies (`copyTexImage2D`, `copyTexSubImage2D`; WebGL 2's `copyTexSubImage3D` and `copyBufferSubData`) read what
// the GPU already holds -- the read framebuffer, or a buffer -- so nothing but their arguments crosses. They are
// fixed records rather than ops because a frame uses them: three.js copies the framebuffer into a texture for its
// transmission pass every frame.
pub const OP_COPY_TEX_IMAGE_2D: u32 = 67;
pub const OP_COPY_TEX_SUB_IMAGE_2D: u32 = 68;
pub const OP_COPY_TEX_SUB_IMAGE_3D: u32 = 69;
pub const OP_COPY_BUFFER_SUB_DATA: u32 = 70;

// ─── Variable opcode constants (256..=276) ────────────────────────────────────

pub const OP_UNIFORM1IV: u32 = 256;
pub const OP_UNIFORM1FV: u32 = 257;
pub const OP_UNIFORM2IV: u32 = 258;
pub const OP_UNIFORM2FV: u32 = 259;
pub const OP_UNIFORM3IV: u32 = 260;
pub const OP_UNIFORM3FV: u32 = 261;
pub const OP_UNIFORM4IV: u32 = 262;
pub const OP_UNIFORM4FV: u32 = 263;
pub const OP_UNIFORM_MATRIX2FV: u32 = 264;
pub const OP_UNIFORM_MATRIX3FV: u32 = 265;
pub const OP_UNIFORM_MATRIX4FV: u32 = 266;
// The non-square matrices (`uniformMatrix{2x3,2x4,3x2,3x4,4x2,4x3}fv`, WebGL 2) follow the square ones so that every matrix
// opcode is in one run (264..=272), and the unsigned vectors (`uniform{1,2,3,4}uiv`) close the block.
pub const OP_UNIFORM_MATRIX2X3FV: u32 = 267;
pub const OP_UNIFORM_MATRIX2X4FV: u32 = 268;
pub const OP_UNIFORM_MATRIX3X2FV: u32 = 269;
pub const OP_UNIFORM_MATRIX3X4FV: u32 = 270;
pub const OP_UNIFORM_MATRIX4X2FV: u32 = 271;
pub const OP_UNIFORM_MATRIX4X3FV: u32 = 272;
pub const OP_UNIFORM1UIV: u32 = 273;
pub const OP_UNIFORM2UIV: u32 = 274;
pub const OP_UNIFORM3UIV: u32 = 275;
pub const OP_UNIFORM4UIV: u32 = 276;

pub fn record_spec(opcode: u32) -> Option<RecordSpec> {
    // Bool word indices reference positions within the record (0 = header).
    // From §5 table: B fields and their 0-based positions.
    //
    // OP_VERTEX_ATTRIB_POINTER (18): H C U I U B I I — 8 words
    //   layout: [0]=H [1]=C [2]=U [3]=I [4]=U [5]=B [6]=I [7]=I  → bool at index 5
    //
    // OP_DEPTH_MASK (26): H C B — 3 words
    //   layout: [0]=H [1]=C [2]=B  → bool at index 2
    //
    // OP_COLOR_MASK (36): H C B B B B — 6 words
    //   layout: [0]=H [1]=C [2]=B [3]=B [4]=B [5]=B  → bools at 2,3,4,5

    Some(match opcode {
        OP_VIEWPORT => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_CLEAR => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_CLEAR_COLOR => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_CLEAR_DEPTH => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_CLEAR_STENCIL => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_ENABLE => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_DISABLE => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_USE_PROGRAM => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_BIND_BUFFER => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_BIND_TEXTURE => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_ACTIVE_TEXTURE => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_BIND_FRAMEBUFFER => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_BIND_RENDERBUFFER => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_BIND_VERTEX_ARRAY => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_BIND_SAMPLER => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_ENABLE_VERTEX_ATTRIB_ARRAY => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_DISABLE_VERTEX_ATTRIB_ARRAY => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        // H C U I U B I I — bool at word index 5
        OP_VERTEX_ATTRIB_POINTER => RecordSpec::Fixed {
            word_count: 8,
            bool_words: &[5],
        },
        OP_VERTEX_ATTRIB_DIVISOR => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_BLEND_FUNC => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_BLEND_FUNC_SEPARATE => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_BLEND_EQUATION => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_BLEND_EQUATION_SEPARATE => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_BLEND_COLOR => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        // H C U F F F F
        OP_VERTEX_ATTRIB_4F => RecordSpec::Fixed {
            word_count: 7,
            bool_words: &[],
        },
        // H C U I I I I
        OP_VERTEX_ATTRIB_I4I => RecordSpec::Fixed {
            word_count: 7,
            bool_words: &[],
        },
        // H C U U U U U
        OP_VERTEX_ATTRIB_I4UI => RecordSpec::Fixed {
            word_count: 7,
            bool_words: &[],
        },
        // H C U I U I I  (index, size, type, stride, offset)
        OP_VERTEX_ATTRIB_I_POINTER => RecordSpec::Fixed {
            word_count: 7,
            bool_words: &[],
        },
        // H C U I F F F F  (buffer, drawbuffer, value[4])
        OP_CLEAR_BUFFER_FV => RecordSpec::Fixed {
            word_count: 8,
            bool_words: &[],
        },
        // H C U I I I I I
        OP_CLEAR_BUFFER_IV => RecordSpec::Fixed {
            word_count: 8,
            bool_words: &[],
        },
        // H C U I U U U U
        OP_CLEAR_BUFFER_UIV => RecordSpec::Fixed {
            word_count: 8,
            bool_words: &[],
        },
        // H C U I F I  (buffer, drawbuffer, depth, stencil)
        OP_CLEAR_BUFFER_FI => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        // H C U I U I I I I I  (target, level, internalformat, x, y, width, height, border)
        OP_COPY_TEX_IMAGE_2D => RecordSpec::Fixed {
            word_count: 10,
            bool_words: &[],
        },
        // H C U I I I I I I I  (target, level, xoffset, yoffset, x, y, width, height)
        OP_COPY_TEX_SUB_IMAGE_2D => RecordSpec::Fixed {
            word_count: 10,
            bool_words: &[],
        },
        // H C U I I I I I I I I  (target, level, xoffset, yoffset, zoffset, x, y, width, height)
        OP_COPY_TEX_SUB_IMAGE_3D => RecordSpec::Fixed {
            word_count: 11,
            bool_words: &[],
        },
        // H C U U I I I  (readTarget, writeTarget, readOffset, writeOffset, size)
        OP_COPY_BUFFER_SUB_DATA => RecordSpec::Fixed {
            word_count: 7,
            bool_words: &[],
        },
        OP_DEPTH_FUNC => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        // H C B — bool at word index 2
        OP_DEPTH_MASK => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[2],
        },
        OP_DEPTH_RANGE => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_STENCIL_FUNC => RecordSpec::Fixed {
            word_count: 5,
            bool_words: &[],
        },
        OP_STENCIL_FUNC_SEPARATE => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_STENCIL_OP => RecordSpec::Fixed {
            word_count: 5,
            bool_words: &[],
        },
        OP_STENCIL_OP_SEPARATE => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_STENCIL_MASK => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_STENCIL_MASK_SEPARATE => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_CULL_FACE => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_FRONT_FACE => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        // H C B B B B — bools at word indices 2,3,4,5
        OP_COLOR_MASK => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[2, 3, 4, 5],
        },
        OP_SCISSOR => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_LINE_WIDTH => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_POLYGON_OFFSET => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_TEX_PARAMETER_I => RecordSpec::Fixed {
            word_count: 5,
            bool_words: &[],
        },
        OP_TEX_PARAMETER_F => RecordSpec::Fixed {
            word_count: 5,
            bool_words: &[],
        },
        OP_GENERATE_MIPMAP => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_PIXEL_STORE_I => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_HINT => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        // OP_SAMPLER_PARAMETER_I/F have no canvas: H U U I/F — 4 words
        OP_SAMPLER_PARAMETER_I => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_SAMPLER_PARAMETER_F => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_DRAW_ARRAYS => RecordSpec::Fixed {
            word_count: 5,
            bool_words: &[],
        },
        OP_DRAW_ELEMENTS => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_DRAW_ARRAYS_INSTANCED => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_DRAW_ELEMENTS_INSTANCED => RecordSpec::Fixed {
            word_count: 7,
            bool_words: &[],
        },
        OP_BIND_BUFFER_BASE => RecordSpec::Fixed {
            word_count: 5,
            bool_words: &[],
        },
        OP_BIND_BUFFER_RANGE => RecordSpec::Fixed {
            word_count: 7,
            bool_words: &[],
        },
        OP_READ_BUFFER => RecordSpec::Fixed {
            word_count: 3,
            bool_words: &[],
        },
        OP_UNIFORM1I => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_UNIFORM1F => RecordSpec::Fixed {
            word_count: 4,
            bool_words: &[],
        },
        OP_UNIFORM2F => RecordSpec::Fixed {
            word_count: 5,
            bool_words: &[],
        },
        OP_UNIFORM3F => RecordSpec::Fixed {
            word_count: 6,
            bool_words: &[],
        },
        OP_UNIFORM4F => RecordSpec::Fixed {
            word_count: 7,
            bool_words: &[],
        },

        // Variable vector uniforms: H C location payload...
        OP_UNIFORM1IV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Int,
        },
        OP_UNIFORM1FV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Float,
        },
        OP_UNIFORM2IV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Int,
        },
        OP_UNIFORM2FV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Float,
        },
        OP_UNIFORM3IV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Int,
        },
        OP_UNIFORM3FV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Float,
        },
        OP_UNIFORM4IV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Int,
        },
        OP_UNIFORM4FV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Float,
        },

        OP_UNIFORM1UIV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Uint,
        },
        OP_UNIFORM2UIV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Uint,
        },
        OP_UNIFORM3UIV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Uint,
        },
        OP_UNIFORM4UIV => RecordSpec::VectorUniform {
            element_kind: UniformElementKind::Uint,
        },

        // Variable matrix uniforms: H C location transpose payload...
        // transpose is at word index 3 (0=H,1=C,2=loc,3=transpose)
        OP_UNIFORM_MATRIX2FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },
        OP_UNIFORM_MATRIX3FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },
        OP_UNIFORM_MATRIX4FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },
        OP_UNIFORM_MATRIX2X3FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },
        OP_UNIFORM_MATRIX2X4FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },
        OP_UNIFORM_MATRIX3X2FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },
        OP_UNIFORM_MATRIX3X4FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },
        OP_UNIFORM_MATRIX4X2FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },
        OP_UNIFORM_MATRIX4X3FV => RecordSpec::MatrixUniform {
            element_kind: UniformElementKind::Float,
            transpose_word_idx: 3,
        },

        _ => return None,
    })
}
