// The render command-stream opcode table, as the WebContent producer sees it.
//
// Three implementations name these numbers: the Rust table in
// `engine/crates/frame-wire/src/gl.rs`, the in-process JavaScript
// encoder in `engine/crates/runtime-v8/src/rendering/webgl/00_render_command_stream.js`,
// and this file. `scripts/test-render-opcode-agreement.sh` parses all three and
// requires them to agree exactly.
//
// The Rust table is the source. This file is derived from it, and the gate is
// what keeps that true -- an opcode added on one side and not the others is a
// producer emitting a record the reader will reject, on a device, with the
// frame simply not drawing.
//
// Numbers, not names, cross the process boundary: a record header is twelve
// bits of opcode and twenty of word count. So a mismatch here is not a type
// error anywhere, which is exactly why it needs a gate.

export const MAGIC = 0x4D474C31;
export const STREAM_VERSION = 1;

export const OP_VIEWPORT = 1;
export const OP_CLEAR = 2;
export const OP_CLEAR_COLOR = 3;
export const OP_CLEAR_DEPTH = 4;
export const OP_CLEAR_STENCIL = 5;
export const OP_ENABLE = 6;
export const OP_DISABLE = 7;
export const OP_USE_PROGRAM = 8;
export const OP_BIND_BUFFER = 9;
export const OP_BIND_TEXTURE = 10;
export const OP_ACTIVE_TEXTURE = 11;
export const OP_BIND_FRAMEBUFFER = 12;
export const OP_BIND_RENDERBUFFER = 13;
export const OP_BIND_VERTEX_ARRAY = 14;
export const OP_BIND_SAMPLER = 15;
export const OP_ENABLE_VERTEX_ATTRIB_ARRAY = 16;
export const OP_DISABLE_VERTEX_ATTRIB_ARRAY = 17;
export const OP_VERTEX_ATTRIB_POINTER = 18;
export const OP_VERTEX_ATTRIB_DIVISOR = 19;
export const OP_BLEND_FUNC = 20;
export const OP_BLEND_FUNC_SEPARATE = 21;
export const OP_BLEND_EQUATION = 22;
export const OP_BLEND_EQUATION_SEPARATE = 23;
export const OP_BLEND_COLOR = 24;
export const OP_DEPTH_FUNC = 25;
export const OP_DEPTH_MASK = 26;
export const OP_DEPTH_RANGE = 27;
export const OP_STENCIL_FUNC = 28;
export const OP_STENCIL_FUNC_SEPARATE = 29;
export const OP_STENCIL_OP = 30;
export const OP_STENCIL_OP_SEPARATE = 31;
export const OP_STENCIL_MASK = 32;
export const OP_STENCIL_MASK_SEPARATE = 33;
export const OP_CULL_FACE = 34;
export const OP_FRONT_FACE = 35;
export const OP_COLOR_MASK = 36;
export const OP_SCISSOR = 37;
export const OP_LINE_WIDTH = 38;
export const OP_POLYGON_OFFSET = 39;
export const OP_TEX_PARAMETER_I = 40;
export const OP_TEX_PARAMETER_F = 41;
export const OP_GENERATE_MIPMAP = 42;
export const OP_PIXEL_STORE_I = 43;
export const OP_HINT = 44;
export const OP_SAMPLER_PARAMETER_I = 45;
export const OP_SAMPLER_PARAMETER_F = 46;
export const OP_DRAW_ARRAYS = 47;
export const OP_DRAW_ELEMENTS = 48;
export const OP_DRAW_ARRAYS_INSTANCED = 49;
export const OP_DRAW_ELEMENTS_INSTANCED = 50;
export const OP_BIND_BUFFER_BASE = 51;
export const OP_BIND_BUFFER_RANGE = 52;
export const OP_READ_BUFFER = 53;
export const OP_UNIFORM1I = 54;
export const OP_UNIFORM1F = 55;
export const OP_UNIFORM2F = 56;
export const OP_UNIFORM3F = 57;
export const OP_UNIFORM4F = 58;
export const OP_UNIFORM1IV = 256;
export const OP_UNIFORM1FV = 257;
export const OP_UNIFORM2IV = 258;
export const OP_UNIFORM2FV = 259;
export const OP_UNIFORM3IV = 260;
export const OP_UNIFORM3FV = 261;
export const OP_UNIFORM4IV = 262;
export const OP_UNIFORM4FV = 263;
export const OP_UNIFORM_MATRIX2FV = 264;
export const OP_UNIFORM_MATRIX3FV = 265;
export const OP_UNIFORM_MATRIX4FV = 266;

// The Canvas2D block. One stream carries both kinds because 2D and GL
// interleave within a frame and the renderer needs the order they were issued
// in; the ranges are what let a reader tell them apart from the opcode alone.
//
// Derived from engine/crates/frame-wire/src/canvas2d.rs, and
// scripts/test-render-opcode-agreement.sh keeps the three tables in step.

export const OP2D_SELECT_CANVAS = 512;
export const OP2D_BEGIN_PATH = 513;
export const OP2D_CLOSE_PATH = 514;
export const OP2D_MOVE_TO = 515;
export const OP2D_LINE_TO = 516;
export const OP2D_QUADRATIC_CURVE_TO = 517;
export const OP2D_BEZIER_CURVE_TO = 518;
export const OP2D_ARC = 519;
export const OP2D_ARC_TO = 520;
export const OP2D_RECT = 521;
export const OP2D_ELLIPSE = 522;
export const OP2D_FILL = 523;
export const OP2D_STROKE = 524;
export const OP2D_CLIP = 525;
export const OP2D_FILL_RECT = 526;
export const OP2D_STROKE_RECT = 527;
export const OP2D_CLEAR_RECT = 528;
export const OP2D_SAVE = 529;
export const OP2D_RESTORE = 530;
export const OP2D_SET_TRANSFORM = 531;
export const OP2D_RESET_TRANSFORM = 532;
export const OP2D_TRANSLATE = 533;
export const OP2D_ROTATE = 534;
export const OP2D_SCALE = 535;
export const OP2D_SET_LINE_WIDTH = 536;
export const OP2D_SET_GLOBAL_ALPHA = 537;
export const OP2D_SET_MITER_LIMIT = 538;
export const OP2D_SET_LINE_DASH_OFFSET = 539;
export const OP2D_SET_SHADOW_BLUR = 540;
export const OP2D_SET_SHADOW_OFFSET_X = 541;
export const OP2D_SET_SHADOW_OFFSET_Y = 542;
export const OP2D_SET_LINE_CAP = 543;
export const OP2D_SET_LINE_JOIN = 544;
export const OP2D_SET_COMPOSITE_OPERATION = 545;
export const OP2D_SET_FILL_STYLE = 546;
export const OP2D_SET_STROKE_STYLE = 547;
export const OP2D_SET_SHADOW_COLOR = 548;
// Bring a 2D context into existence on the selected canvas.
//
// The FIRST record a producer sends to a canvas it means to draw on. Without
// it the rest of this block is a complete drawing vocabulary that cannot be
// used: a canvas with no 2D context answers NotFound, and the renderer treats
// that as "no context yet -- no draws". An accepted frame that drew nothing,
// with no error anywhere.
export const OP2D_CREATE_CONTEXT = 549;

/// Pack a record header: low twelve bits opcode, high twenty word count.
///
/// `wordCount` counts the header word itself. A fixture written from the opcode
/// name alone gets that wrong, and the structural validator catches it -- which
/// is the validator doing its job before anything else does.
export function packHeader(opcode, wordCount) {
  return ((wordCount & 0xfffff) << 12) | (opcode & 0xfff);
}

export function opcodeOf(header) {
  return header & 0xfff;
}

export function wordCountOf(header) {
  return (header >>> 12) & 0xfffff;
}
