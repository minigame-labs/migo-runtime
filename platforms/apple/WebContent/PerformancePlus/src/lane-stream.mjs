// Ops whose work rides the frame's command stream to the host.
//
// See engine-frames.mjs for the packet each frame becomes. The contract gives
// these ops no answer to send back; where the Rust op returns a status, the
// contract's `local_answer` says what the producer answers instead.

import {
  appendCanvas2DRecord,
  appendDrawImage,
  appendRecord,
  appendStream,
  endFrame,
  flushToHost,
} from "./engine-frames.mjs";
import { engineHost } from "./engine-host.mjs";
import { recordProducerError } from "./lane-local.mjs";
import {
  bytesOf,
  f32BitsOf,
  optionalBytesOf,
  smiU32,
  smiU8,
  stringOf,
  toI32,
  u32ArrayOf,
} from "./op-args.mjs";
import { parseFontShorthand } from "./css-font.mjs";
import * as R from "./render-opcodes.mjs";

const OUT_OF_MEMORY = 0x0505;
const INVALID_VALUE = 0x0501;

// One scratch record, reused: the header and fixed words of the call being
// encoded, ending with `byte_length` or `count` for a payload record. Fifteen
// words holds the longest (texSubImage3D's fourteen plus its length).
const record = new Uint32Array(15);
const utf8 = new TextEncoder();

/** Fill `record` with a header for `opcode` and these words; its length. */
function fixed(opcode, ...args) {
  record[0] = (((args.length + 1) << 12) | opcode) >>> 0;
  for (let i = 0; i < args.length; i += 1) record[i + 1] = args[i] >>> 0;
  return args.length + 1;
}

/** Append a fixed record. */
function emit(opcode, ...args) {
  const words = fixed(opcode, ...args);
  if (!appendRecord(record, words, null)) throw new RangeError(`a ${words}-word record did not fit a packet`);
}

/**
 * Append a byte-payload record: `prefix` words, then `byte_length`, then the
 * bytes. A payload larger than one packet can carry is refused the way GL
 * refuses an allocation it cannot make -- OUT_OF_MEMORY, and nothing done -- and
 * the host log says why, because the cause is this lane's packet size and not
 * the device: uploads that large need the resource lane.
 */
function emitBytes(canvasId, opcode, bytes, ...prefix) {
  const headerWords = prefix.length + 2;
  const wordCount = headerWords + Math.ceil(bytes.byteLength / 4);
  // Checked before the header is built: twenty bits of word count cannot say
  // more, and a packet could not carry it anyway.
  if (wordCount > MAX_RECORD_WORDS) {
    refuseUpload(canvasId, bytes.byteLength);
    return;
  }
  record[0] = ((wordCount << 12) | opcode) >>> 0;
  for (let i = 0; i < prefix.length; i += 1) record[i + 1] = prefix[i] >>> 0;
  record[headerWords - 1] = bytes.byteLength;
  if (!appendRecord(record, headerWords, bytes)) refuseUpload(canvasId, bytes.byteLength);
}

const MAX_RECORD_WORDS = 0xfffff;

function refuseUpload(canvasId, byteLength) {
  recordProducerError(canvasId, OUT_OF_MEMORY);
  engineHost().report({
    type: "console",
    level: 2,
    message:
      `a ${byteLength}-byte WebGL upload is larger than one frame packet carries; ` +
      "it was refused with OUT_OF_MEMORY until the resource lane carries uploads that large",
  });
}

/** Append a word-list record: `prefix` words, then `count`, then the words. */
function emitWords(canvasId, opcode, list, ...prefix) {
  if (list.length > R.MAX_RESOURCE_WORD_LIST) {
    // More attachments than any GL has: GL answers INVALID_VALUE, and the record
    // could not carry them anyway.
    recordProducerError(canvasId, INVALID_VALUE);
    return;
  }
  const headerWords = prefix.length + 2;
  record[0] = (((headerWords + list.length) << 12) | opcode) >>> 0;
  for (let i = 0; i < prefix.length; i += 1) record[i + 1] = prefix[i] >>> 0;
  record[headerWords - 1] = list.length;
  if (!appendRecord(record, headerWords, list)) throw new RangeError("a word list did not fit a packet");
}

/// A facade's flushed command buffer joins the frame. Answers 0, success: the
/// producer builds the packet, and a packet the host refuses is answered on the
/// downlink as a verdict, not here.
export function op_submit_render_stream(words, usedWords) {
  appendStream(u32ArrayOf(words, "words"), smiU32(usedWords, "used_words"));
  return 0;
}

/// The frame ends; its packet is sent or held for the window.
export function op_frame_end_unified() {
  endFrame();
}

/// `gl.flush()`: what is recorded reaches the host now, and the frame goes on.
///
/// The embedded op sends the collector's pending commands to the renderer as a
/// barrier packet; this sends them as a barrier packet. Advisory in WebGL, and
/// not free -- a packet per call -- which is the embedded runtime's cost too.
export function op_gl_flush() {
  flushToHost();
}

// ---- WebGL resources ----------------------------------------------------------
//
// Each op is its Rust namesake's arguments, converted as deno_core converts them,
// written as the record frame_wire::gl_resource defines. The host decodes each
// into the command the Rust op builds, through the same builder where the op has
// one (frame_decode::resource).

const create = (opcode) =>
  function (canvasId, clientId) {
    emit(opcode, smiU32(canvasId, "canvas_id"), smiU32(clientId, "client_id"));
  };
export const op_create_buffer = create(R.OPR_CREATE_BUFFER);
export const op_create_framebuffer = create(R.OPR_CREATE_FRAMEBUFFER);
export const op_create_program = create(R.OPR_CREATE_PROGRAM);
export const op_create_query = create(R.OPR_CREATE_QUERY);
export const op_create_renderbuffer = create(R.OPR_CREATE_RENDERBUFFER);
export const op_create_sampler = create(R.OPR_CREATE_SAMPLER);
export const op_create_texture = create(R.OPR_CREATE_TEXTURE);
export const op_create_transform_feedback = create(R.OPR_CREATE_TRANSFORM_FEEDBACK);
export const op_create_vertex_array = create(R.OPR_CREATE_VERTEX_ARRAY);

export function op_create_shader(canvasId, clientId, ty) {
  emit(R.OPR_CREATE_SHADER, smiU32(canvasId, "canvas_id"), smiU32(clientId, "client_id"), smiU32(ty, "ty"));
}

// Written out rather than stamped by a factory: each op's one argument has its
// own Rust name, and the conversion check matches conversions to parameters by
// that name.
export function op_delete_buffer(bufferId) {
  emit(R.OPR_DELETE_BUFFER, smiU32(bufferId, "buffer_id"));
}
export function op_delete_framebuffer(framebufferId) {
  emit(R.OPR_DELETE_FRAMEBUFFER, smiU32(framebufferId, "framebuffer_id"));
}
export function op_delete_program(programId) {
  emit(R.OPR_DELETE_PROGRAM, smiU32(programId, "program_id"));
}
export function op_delete_query(query) {
  emit(R.OPR_DELETE_QUERY, smiU32(query, "query"));
}
export function op_delete_renderbuffer(renderbufferId) {
  emit(R.OPR_DELETE_RENDERBUFFER, smiU32(renderbufferId, "renderbuffer_id"));
}
export function op_delete_sampler(sampler) {
  emit(R.OPR_DELETE_SAMPLER, smiU32(sampler, "sampler"));
}
export function op_delete_shader(shaderId) {
  emit(R.OPR_DELETE_SHADER, smiU32(shaderId, "shader_id"));
}
export function op_delete_sync(sync) {
  emit(R.OPR_DELETE_SYNC, smiU32(sync, "sync"));
}
export function op_delete_texture(textureId) {
  emit(R.OPR_DELETE_TEXTURE, smiU32(textureId, "texture_id"));
}
export function op_delete_transform_feedback(tf) {
  emit(R.OPR_DELETE_TRANSFORM_FEEDBACK, smiU32(tf, "tf"));
}
export function op_delete_vertex_array(vao) {
  emit(R.OPR_DELETE_VERTEX_ARRAY, smiU32(vao, "vao"));
}

export function op_attach_shader(programId, shaderId) {
  emit(R.OPR_ATTACH_SHADER, smiU32(programId, "program_id"), smiU32(shaderId, "shader_id"));
}
export function op_compile_shader(shaderId) {
  emit(R.OPR_COMPILE_SHADER, smiU32(shaderId, "shader_id"));
}
export function op_link_program(programId) {
  emit(R.OPR_LINK_PROGRAM, smiU32(programId, "program_id"));
}
export function op_begin_query(canvasId, target, query) {
  emit(R.OPR_BEGIN_QUERY, smiU32(canvasId, "canvas_id"), smiU32(target, "target"), smiU32(query, "query"));
}
export function op_end_query(canvasId, target) {
  emit(R.OPR_END_QUERY, smiU32(canvasId, "canvas_id"), smiU32(target, "target"));
}
export function op_begin_transform_feedback(canvasId, primitiveMode) {
  emit(R.OPR_BEGIN_TRANSFORM_FEEDBACK, smiU32(canvasId, "canvas_id"), smiU32(primitiveMode, "primitive_mode"));
}
export function op_end_transform_feedback(canvasId) {
  emit(R.OPR_END_TRANSFORM_FEEDBACK, smiU32(canvasId, "canvas_id"));
}
export function op_pause_transform_feedback(canvasId) {
  emit(R.OPR_PAUSE_TRANSFORM_FEEDBACK, smiU32(canvasId, "canvas_id"));
}
export function op_resume_transform_feedback(canvasId) {
  emit(R.OPR_RESUME_TRANSFORM_FEEDBACK, smiU32(canvasId, "canvas_id"));
}
export function op_bind_transform_feedback(canvasId, target, tf) {
  emit(R.OPR_BIND_TRANSFORM_FEEDBACK, smiU32(canvasId, "canvas_id"), smiU32(target, "target"), smiU32(tf, "tf"));
}
export function op_blit_framebuffer(canvasId, srcX0, srcY0, srcX1, srcY1, dstX0, dstY0, dstX1, dstY1, mask, filter) {
  emit(
    R.OPR_BLIT_FRAMEBUFFER,
    smiU32(canvasId, "canvas_id"),
    toI32(srcX0, "src_x0"),
    toI32(srcY0, "src_y0"),
    toI32(srcX1, "src_x1"),
    toI32(srcY1, "src_y1"),
    toI32(dstX0, "dst_x0"),
    toI32(dstY0, "dst_y0"),
    toI32(dstX1, "dst_x1"),
    toI32(dstY1, "dst_y1"),
    smiU32(mask, "mask"),
    smiU32(filter, "filter"),
  );
}
export function op_fence_sync(canvasId, clientId, condition, flags) {
  emit(
    R.OPR_FENCE_SYNC,
    smiU32(canvasId, "canvas_id"),
    smiU32(clientId, "client_id"),
    smiU32(condition, "condition"),
    smiU32(flags, "flags"),
  );
}
export function op_framebuffer_renderbuffer(canvasId, target, attachment, renderbuffertarget, renderbuffer) {
  emit(
    R.OPR_FRAMEBUFFER_RENDERBUFFER,
    smiU32(canvasId, "canvas_id"),
    smiU32(target, "target"),
    smiU32(attachment, "attachment"),
    smiU32(renderbuffertarget, "renderbuffertarget"),
    toI32(renderbuffer, "renderbuffer"),
  );
}
export function op_framebuffer_texture_2d(canvasId, target, attachment, textarget, texture, level) {
  emit(
    R.OPR_FRAMEBUFFER_TEXTURE_2D,
    smiU32(canvasId, "canvas_id"),
    smiU32(target, "target"),
    smiU32(attachment, "attachment"),
    smiU32(textarget, "textarget"),
    toI32(texture, "texture"),
    toI32(level, "level"),
  );
}
export function op_renderbuffer_storage(canvasId, target, internalformat, width, height) {
  emit(
    R.OPR_RENDERBUFFER_STORAGE,
    smiU32(canvasId, "canvas_id"),
    smiU32(target, "target"),
    smiU32(internalformat, "internalformat"),
    toI32(width, "width"),
    toI32(height, "height"),
  );
}
export function op_renderbuffer_storage_multisample(canvasId, target, samples, internalFormat, width, height) {
  emit(
    R.OPR_RENDERBUFFER_STORAGE_MULTISAMPLE,
    smiU32(canvasId, "canvas_id"),
    smiU32(target, "target"),
    toI32(samples, "samples"),
    smiU32(internalFormat, "internal_format"),
    toI32(width, "width"),
    toI32(height, "height"),
  );
}
export function op_tex_storage_2d(canvasId, target, levels, internalFormat, width, height) {
  emit(
    R.OPR_TEX_STORAGE_2D,
    smiU32(canvasId, "canvas_id"),
    smiU32(target, "target"),
    toI32(levels, "levels"),
    smiU32(internalFormat, "internal_format"),
    toI32(width, "width"),
    toI32(height, "height"),
  );
}
export function op_tex_storage_3d(canvasId, target, levels, internalFormat, width, height, depth) {
  emit(
    R.OPR_TEX_STORAGE_3D,
    smiU32(canvasId, "canvas_id"),
    smiU32(target, "target"),
    toI32(levels, "levels"),
    smiU32(internalFormat, "internal_format"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(depth, "depth"),
  );
}
export function op_uniform_block_binding(programId, index, binding) {
  emit(
    R.OPR_UNIFORM_BLOCK_BINDING,
    smiU32(programId, "program_id"),
    smiU32(index, "uniform_block_index"),
    smiU32(binding, "uniform_block_binding"),
  );
}
export function op_gl_lose_context(canvasId) {
  emit(R.OPR_LOSE_CONTEXT, smiU32(canvasId, "canvas_id"));
}

export function op_shader_source(canvasId, shaderId, source) {
  const canvas = smiU32(canvasId, "canvas_id");
  const shader = smiU32(shaderId, "shader_id");
  emitBytes(canvas, R.OPR_SHADER_SOURCE, utf8.encode(stringOf(source, "source")), canvas, shader);
}
export function op_bind_attrib_location(programId, index, name) {
  const program = smiU32(programId, "program_id");
  const location = smiU32(index, "index");
  // No canvas among this op's arguments; an upload refusal it cannot have.
  emitBytes(0, R.OPR_BIND_ATTRIB_LOCATION, utf8.encode(stringOf(name, "name")), program, location);
}
export function op_buffer_data(canvasId, target, size, data, usage) {
  const canvas = smiU32(canvasId, "canvas_id");
  const bytes = optionalBytesOf(data, "data");
  emitBytes(
    canvas,
    R.OPR_BUFFER_DATA,
    bytes ?? EMPTY,
    canvas,
    smiU32(target, "target"),
    toI32(size, "size"),
    smiU32(usage, "usage"),
    bytes === null ? 0 : 1,
  );
}
export function op_buffer_sub_data(canvasId, target, offset, data) {
  const canvas = smiU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_BUFFER_SUB_DATA,
    bytesOf(data, "data"),
    canvas,
    smiU32(target, "target"),
    toI32(offset, "offset"),
  );
}
export function op_tex_image_2d(canvasId, target, level, internalformat, width, height, border, format, type, data) {
  const canvas = smiU32(canvasId, "canvas_id");
  const bytes = optionalBytesOf(data, "data");
  emitBytes(
    canvas,
    R.OPR_TEX_IMAGE_2D,
    bytes ?? EMPTY,
    canvas,
    smiU32(target, "target"),
    toI32(level, "level"),
    toI32(internalformat, "internalformat"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(border, "border"),
    smiU32(format, "format"),
    smiU32(type, "type_"),
    bytes === null ? 0 : 1,
  );
}
export function op_tex_sub_image_2d(canvasId, target, level, xoffset, yoffset, width, height, format, type, data) {
  const canvas = smiU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_TEX_SUB_IMAGE_2D,
    bytesOf(data, "data"),
    canvas,
    smiU32(target, "target"),
    toI32(level, "level"),
    toI32(xoffset, "xoffset"),
    toI32(yoffset, "yoffset"),
    toI32(width, "width"),
    toI32(height, "height"),
    smiU32(format, "format"),
    smiU32(type, "type_"),
  );
}
export function op_compressed_tex_image_2d(canvasId, target, level, internalformat, width, height, border, data) {
  const canvas = smiU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_COMPRESSED_TEX_IMAGE_2D,
    bytesOf(data, "data"),
    canvas,
    smiU32(target, "target"),
    toI32(level, "level"),
    smiU32(internalformat, "internalformat"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(border, "border"),
  );
}
export function op_compressed_tex_sub_image_2d(canvasId, target, level, xoffset, yoffset, width, height, format, data) {
  const canvas = smiU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_COMPRESSED_TEX_SUB_IMAGE_2D,
    bytesOf(data, "data"),
    canvas,
    smiU32(target, "target"),
    toI32(level, "level"),
    toI32(xoffset, "xoffset"),
    toI32(yoffset, "yoffset"),
    toI32(width, "width"),
    toI32(height, "height"),
    smiU32(format, "format"),
  );
}

const EMPTY = new Uint8Array(0);

/**
 * A 3D upload's pixels from the caller's element offset on -- the slice the Rust
 * op makes before its builder copies -- or null when a pixel-unpack offset is
 * given (which wins) or there are none.
 */
function pixels3d(pixels, srcOffset, bytesPerElement, pboOffset) {
  if (pboOffset >= 0 || pixels === null) return null;
  const start = Math.max(bytesPerElement, 1) * srcOffset;
  return start >= pixels.byteLength ? EMPTY : pixels.subarray(start);
}

export function op_tex_image_3d(
  canvasId,
  target,
  level,
  internalFormat,
  width,
  height,
  depth,
  border,
  format,
  ty,
  pixels,
  srcOffset,
  bytesPerElement,
  pboOffset,
) {
  const canvas = smiU32(canvasId, "canvas_id");
  const pbo = toI32(pboOffset, "pbo_offset");
  const bytes = pixels3d(
    optionalBytesOf(pixels, "pixels"),
    smiU32(srcOffset, "src_offset"),
    smiU32(bytesPerElement, "bytes_per_element"),
    pbo,
  );
  emitBytes(
    canvas,
    R.OPR_TEX_IMAGE_3D,
    bytes ?? EMPTY,
    canvas,
    smiU32(target, "target"),
    toI32(level, "level"),
    toI32(internalFormat, "internal_format"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(depth, "depth"),
    toI32(border, "border"),
    smiU32(format, "format"),
    smiU32(ty, "ty"),
    pbo,
    bytes === null ? 0 : 1,
  );
}

export function op_tex_sub_image_3d(
  canvasId,
  target,
  level,
  xoffset,
  yoffset,
  zoffset,
  width,
  height,
  depth,
  format,
  ty,
  pixels,
  srcOffset,
  bytesPerElement,
  pboOffset,
) {
  const canvas = smiU32(canvasId, "canvas_id");
  const pbo = toI32(pboOffset, "pbo_offset");
  const bytes = pixels3d(
    optionalBytesOf(pixels, "pixels"),
    smiU32(srcOffset, "src_offset"),
    smiU32(bytesPerElement, "bytes_per_element"),
    pbo,
  );
  emitBytes(
    canvas,
    R.OPR_TEX_SUB_IMAGE_3D,
    bytes ?? EMPTY,
    canvas,
    smiU32(target, "target"),
    toI32(level, "level"),
    toI32(xoffset, "xoffset"),
    toI32(yoffset, "yoffset"),
    toI32(zoffset, "zoffset"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(depth, "depth"),
    smiU32(format, "format"),
    smiU32(ty, "ty"),
    pbo,
    bytes === null ? 0 : 1,
  );
}

export function op_draw_buffers(canvasId, buffers) {
  const canvas = smiU32(canvasId, "canvas_id");
  emitWords(canvas, R.OPR_DRAW_BUFFERS, u32ArrayOf(buffers, "buffers"), canvas);
}
export function op_invalidate_framebuffer(canvasId, target, attachments) {
  const canvas = smiU32(canvasId, "canvas_id");
  emitWords(
    canvas,
    R.OPR_INVALIDATE_FRAMEBUFFER,
    u32ArrayOf(attachments, "attachments"),
    canvas,
    smiU32(target, "target"),
  );
}
export function op_transform_feedback_varyings(canvasId, program, varyingsJoined, bufferMode) {
  const canvas = smiU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_TRANSFORM_FEEDBACK_VARYINGS,
    utf8.encode(stringOf(varyingsJoined, "varyings_joined")),
    canvas,
    smiU32(program, "program"),
    smiU32(bufferMode, "buffer_mode"),
  );
}

// ---- Canvas2D text ----------------------------------------------------------
//
// The 2D block's records carry no canvas: the selection before them does, and
// `appendCanvas2DRecord` writes one when this packet does not already have the
// right canvas selected. Everything else here is the op's arguments in the
// order the record declares them.

/** Append a fixed 2D record for `canvasId`. */
function emit2D(canvasId, opcode, ...args) {
  const words = fixed(opcode, ...args);
  if (!appendCanvas2DRecord(canvasId, record, words, null)) {
    throw new RangeError(`a ${words}-word 2D record did not fit a packet`);
  }
}

/** Append a 2D record that ends in text. */
function emit2DText(canvasId, opcode, text, ...prefix) {
  const bytes = utf8.encode(text);
  const headerWords = prefix.length + 2;
  const wordCount = headerWords + Math.ceil(bytes.byteLength / 4);
  if (wordCount > MAX_RECORD_WORDS) {
    // A string this long is not text anyone draws; the record could not carry
    // it, and a 2D call that fails does nothing rather than raising.
    recordProducerError(canvasId, OUT_OF_MEMORY);
    return;
  }
  record[0] = ((wordCount << 12) | opcode) >>> 0;
  for (let i = 0; i < prefix.length; i += 1) record[i + 1] = prefix[i] >>> 0;
  record[headerWords - 1] = bytes.byteLength;
  if (!appendCanvas2DRecord(canvasId, record, headerWords, bytes)) {
    recordProducerError(canvasId, OUT_OF_MEMORY);
  }
}

/**
 * `canvas.getContext("2d")`.
 *
 * Answers success, as the contract's `local_answer` says: the canvas id is the
 * producer's, and the record is what brings the context into existence on the
 * host. A canvas the host cannot give a context to is reported as a
 * context-loss event, not as a return value nobody is waiting for.
 */
export function op_create_context_2d(canvasId) {
  emit2D(smiU32(canvasId, "canvas_id"), R.OP2D_CREATE_CONTEXT);
  return 0;
}

/**
 * `ctx.font = "..."`, which answers whether the shorthand parsed.
 *
 * The parser is the producer's own port of the host's, held to it by
 * `scripts/test-css-font-agreement.sh`. A shorthand that does not parse is a
 * no-op that keeps the previous font -- what a browser does, and what the op
 * this stands in for does -- so no record is written for one.
 */
export function op_set_font(canvasId, font) {
  const text = stringOf(font, "font");
  if (parseFontShorthand(text) === null) return false;
  emit2DText(smiU32(canvasId, "canvas_id"), R.OP2D_SET_FONT, text);
  return true;
}

export function op_fill_text(canvasId, text, x, y, maxWidth) {
  emit2DText(
    smiU32(canvasId, "canvas_id"),
    R.OP2D_FILL_TEXT,
    stringOf(text, "text"),
    f32BitsOf(x, "x"),
    f32BitsOf(y, "y"),
    f32BitsOf(maxWidth, "max_width"),
  );
}

export function op_stroke_text(canvasId, text, x, y, maxWidth) {
  emit2DText(
    smiU32(canvasId, "canvas_id"),
    R.OP2D_STROKE_TEXT,
    stringOf(text, "text"),
    f32BitsOf(x, "x"),
    f32BitsOf(y, "y"),
    f32BitsOf(maxWidth, "max_width"),
  );
}

export function op_set_text_align(canvasId, align) {
  emit2D(smiU32(canvasId, "canvas_id"), R.OP2D_SET_TEXT_ALIGN, smiU8(align, "align"));
}

export function op_set_text_baseline(canvasId, baseline) {
  emit2D(smiU32(canvasId, "canvas_id"), R.OP2D_SET_TEXT_BASELINE, smiU8(baseline, "baseline"));
}

export function op_set_text_direction(canvasId, direction) {
  emit2D(smiU32(canvasId, "canvas_id"), R.OP2D_SET_TEXT_DIRECTION, smiU8(direction, "direction"));
}

/**
 * `setLineDash([...])`.
 *
 * The op takes the segments as bytes -- a `Float32Array`'s bytes, which is what
 * the facade passes -- and the record carries them as the words they are.
 */
export function op_set_line_dash(canvasId, segments) {
  const canvas = smiU32(canvasId, "canvas_id");
  const bytes = bytesOf(segments, "segments");
  const words = new Uint32Array(bytes.byteLength >> 2);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let index = 0; index < words.length; index += 1) words[index] = view.getUint32(index * 4, true);
  if (words.length > R.MAX_LINE_DASH_SEGMENTS) {
    // Longer than any pattern that draws differently from a shorter one; the
    // host refuses the record, so the producer does not write it.
    recordProducerError(canvas, INVALID_VALUE);
    return;
  }
  const headerWords = 2;
  record[0] = (((headerWords + words.length) << 12) | R.OP2D_SET_LINE_DASH) >>> 0;
  record[1] = words.length;
  if (!appendCanvas2DRecord(canvas, record, headerWords, words)) {
    recordProducerError(canvas, OUT_OF_MEMORY);
  }
}

// ---- images ------------------------------------------------------------------
//
// A loaded image is a texture the host already holds -- `Image.src` decoded
// and uploaded it there, and answered with its shared id -- so drawing or
// uploading one names the id, and no pixel crosses.

/// `drawImage(image, sx, sy, sw, sh, dx, dy, dw, dh)`, the facade having
/// expanded the shorter forms. Adjacent draws on one canvas leave as one batch
/// record; see `appendDrawImage`.
const drawImageEntry = new Uint32Array(R.DRAW_IMAGE_BATCH_ENTRY_WORDS);
export function op_draw_image(canvasId, imageId, sx, sy, sw, sh, dx, dy, dw, dh) {
  const canvas = smiU32(canvasId, "canvas_id");
  drawImageEntry[0] = smiU32(imageId, "image_id");
  drawImageEntry[1] = f32BitsOf(sx, "sx");
  drawImageEntry[2] = f32BitsOf(sy, "sy");
  drawImageEntry[3] = f32BitsOf(sw, "sw");
  drawImageEntry[4] = f32BitsOf(sh, "sh");
  drawImageEntry[5] = f32BitsOf(dx, "dx");
  drawImageEntry[6] = f32BitsOf(dy, "dy");
  drawImageEntry[7] = f32BitsOf(dw, "dw");
  drawImageEntry[8] = f32BitsOf(dh, "dh");
  if (!appendDrawImage(canvas, drawImageEntry)) recordProducerError(canvas, OUT_OF_MEMORY);
}

/// The most entries a batch may carry: the engine's own bound, which the op
/// refuses above and so does this.
const MAX_DRAW_IMAGE_BATCH_ENTRIES = 65_536;
const DRAW_IMAGE_BATCH_ENTRY_BYTES = R.DRAW_IMAGE_BATCH_ENTRY_WORDS * 4;

/**
 * `drawImageBatch(draws)`: the facade's buffer of nine-word entries -- the image
 * id's own bits, then eight floats -- carried as the words they are.
 *
 * A buffer that is not whole entries, or holds more than the engine allows, is
 * dropped as the op drops it; an empty one draws nothing.
 */
export function op_draw_image_batch(canvasId, data) {
  const canvas = smiU32(canvasId, "canvas_id");
  const bytes = bytesOf(data, "data");
  if (bytes.byteLength % DRAW_IMAGE_BATCH_ENTRY_BYTES !== 0) return;
  const entries = bytes.byteLength / DRAW_IMAGE_BATCH_ENTRY_BYTES;
  if (entries === 0 || entries > MAX_DRAW_IMAGE_BATCH_ENTRIES) return;
  const words = new Uint32Array(bytes.byteLength >> 2);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let index = 0; index < words.length; index += 1) words[index] = view.getUint32(index * 4, true);
  const headerWords = 2;
  record[0] = (((headerWords + words.length) << 12) | R.OP2D_DRAW_IMAGE_BATCH) >>> 0;
  record[1] = words.length;
  if (!appendCanvas2DRecord(canvas, record, headerWords, words)) {
    recordProducerError(canvas, OUT_OF_MEMORY);
  }
}

/// `texImage2D(target, level, internalformat, format, type, image)`: the host
/// resolves the id against the images it loaded -- a GPU-side copy from the
/// image's texture where it can, the decoded bytes otherwise.
export function op_tex_image_2d_from_image(canvasId, target, level, internalformat, format, type, imageId) {
  emit(
    R.OPR_TEX_IMAGE_2D_FROM_IMAGE,
    smiU32(canvasId, "canvas_id"),
    smiU32(target, "target"),
    toI32(level, "level"),
    toI32(internalformat, "internalformat"),
    smiU32(format, "format"),
    smiU32(type, "type_"),
    smiU32(imageId, "image_id"),
  );
}

/// `texSubImage2D(target, level, x, y, format, type, image)`.
export function op_tex_sub_image_2d_from_image(canvasId, target, level, xoffset, yoffset, format, type, imageId) {
  emit(
    R.OPR_TEX_SUB_IMAGE_2D_FROM_IMAGE,
    smiU32(canvasId, "canvas_id"),
    smiU32(target, "target"),
    toI32(level, "level"),
    toI32(xoffset, "xoffset"),
    toI32(yoffset, "yoffset"),
    smiU32(format, "format"),
    smiU32(type, "type_"),
    smiU32(imageId, "image_id"),
  );
}
