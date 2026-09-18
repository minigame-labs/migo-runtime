// Ops whose work rides the frame's command stream to the host.
//
// See engine-frames.mjs for the packet each frame becomes. The contract gives
// these ops no answer to send back; where the Rust op returns a status, the
// contract's `local_answer` says what the producer answers instead.

import { appendRecord, appendStream, endFrame, flushToHost } from "./engine-frames.mjs";
import { engineHost } from "./engine-host.mjs";
import { recordProducerError } from "./lane-local.mjs";
import { optionalBytesOf, bytesOf, stringOf, toI32, toU32, u32ArrayOf } from "./op-args.mjs";
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
  appendStream(words, usedWords);
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
    emit(opcode, toU32(canvasId, "canvas_id"), toU32(clientId, "client_id"));
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
  emit(R.OPR_CREATE_SHADER, toU32(canvasId, "canvas_id"), toU32(clientId, "client_id"), toU32(ty, "ty"));
}

const remove = (opcode) =>
  function (id) {
    emit(opcode, toU32(id, "id"));
  };
export const op_delete_buffer = remove(R.OPR_DELETE_BUFFER);
export const op_delete_framebuffer = remove(R.OPR_DELETE_FRAMEBUFFER);
export const op_delete_program = remove(R.OPR_DELETE_PROGRAM);
export const op_delete_query = remove(R.OPR_DELETE_QUERY);
export const op_delete_renderbuffer = remove(R.OPR_DELETE_RENDERBUFFER);
export const op_delete_sampler = remove(R.OPR_DELETE_SAMPLER);
export const op_delete_shader = remove(R.OPR_DELETE_SHADER);
export const op_delete_sync = remove(R.OPR_DELETE_SYNC);
export const op_delete_texture = remove(R.OPR_DELETE_TEXTURE);
export const op_delete_transform_feedback = remove(R.OPR_DELETE_TRANSFORM_FEEDBACK);
export const op_delete_vertex_array = remove(R.OPR_DELETE_VERTEX_ARRAY);

export function op_attach_shader(programId, shaderId) {
  emit(R.OPR_ATTACH_SHADER, toU32(programId, "program_id"), toU32(shaderId, "shader_id"));
}
export function op_compile_shader(shaderId) {
  emit(R.OPR_COMPILE_SHADER, toU32(shaderId, "shader_id"));
}
export function op_link_program(programId) {
  emit(R.OPR_LINK_PROGRAM, toU32(programId, "program_id"));
}
export function op_begin_query(canvasId, target, query) {
  emit(R.OPR_BEGIN_QUERY, toU32(canvasId, "canvas_id"), toU32(target, "target"), toU32(query, "query"));
}
export function op_end_query(canvasId, target) {
  emit(R.OPR_END_QUERY, toU32(canvasId, "canvas_id"), toU32(target, "target"));
}
export function op_begin_transform_feedback(canvasId, primitiveMode) {
  emit(R.OPR_BEGIN_TRANSFORM_FEEDBACK, toU32(canvasId, "canvas_id"), toU32(primitiveMode, "primitive_mode"));
}
export function op_end_transform_feedback(canvasId) {
  emit(R.OPR_END_TRANSFORM_FEEDBACK, toU32(canvasId, "canvas_id"));
}
export function op_pause_transform_feedback(canvasId) {
  emit(R.OPR_PAUSE_TRANSFORM_FEEDBACK, toU32(canvasId, "canvas_id"));
}
export function op_resume_transform_feedback(canvasId) {
  emit(R.OPR_RESUME_TRANSFORM_FEEDBACK, toU32(canvasId, "canvas_id"));
}
export function op_bind_transform_feedback(canvasId, target, tf) {
  emit(R.OPR_BIND_TRANSFORM_FEEDBACK, toU32(canvasId, "canvas_id"), toU32(target, "target"), toU32(tf, "tf"));
}
export function op_blit_framebuffer(canvasId, srcX0, srcY0, srcX1, srcY1, dstX0, dstY0, dstX1, dstY1, mask, filter) {
  emit(
    R.OPR_BLIT_FRAMEBUFFER,
    toU32(canvasId, "canvas_id"),
    toI32(srcX0, "src_x0"),
    toI32(srcY0, "src_y0"),
    toI32(srcX1, "src_x1"),
    toI32(srcY1, "src_y1"),
    toI32(dstX0, "dst_x0"),
    toI32(dstY0, "dst_y0"),
    toI32(dstX1, "dst_x1"),
    toI32(dstY1, "dst_y1"),
    toU32(mask, "mask"),
    toU32(filter, "filter"),
  );
}
export function op_fence_sync(canvasId, clientId, condition, flags) {
  emit(
    R.OPR_FENCE_SYNC,
    toU32(canvasId, "canvas_id"),
    toU32(clientId, "client_id"),
    toU32(condition, "condition"),
    toU32(flags, "flags"),
  );
}
export function op_framebuffer_renderbuffer(canvasId, target, attachment, renderbuffertarget, renderbuffer) {
  emit(
    R.OPR_FRAMEBUFFER_RENDERBUFFER,
    toU32(canvasId, "canvas_id"),
    toU32(target, "target"),
    toU32(attachment, "attachment"),
    toU32(renderbuffertarget, "renderbuffertarget"),
    toI32(renderbuffer, "renderbuffer"),
  );
}
export function op_framebuffer_texture_2d(canvasId, target, attachment, textarget, texture, level) {
  emit(
    R.OPR_FRAMEBUFFER_TEXTURE_2D,
    toU32(canvasId, "canvas_id"),
    toU32(target, "target"),
    toU32(attachment, "attachment"),
    toU32(textarget, "textarget"),
    toI32(texture, "texture"),
    toI32(level, "level"),
  );
}
export function op_renderbuffer_storage(canvasId, target, internalformat, width, height) {
  emit(
    R.OPR_RENDERBUFFER_STORAGE,
    toU32(canvasId, "canvas_id"),
    toU32(target, "target"),
    toU32(internalformat, "internalformat"),
    toI32(width, "width"),
    toI32(height, "height"),
  );
}
export function op_renderbuffer_storage_multisample(canvasId, target, samples, internalFormat, width, height) {
  emit(
    R.OPR_RENDERBUFFER_STORAGE_MULTISAMPLE,
    toU32(canvasId, "canvas_id"),
    toU32(target, "target"),
    toI32(samples, "samples"),
    toU32(internalFormat, "internal_format"),
    toI32(width, "width"),
    toI32(height, "height"),
  );
}
export function op_tex_storage_2d(canvasId, target, levels, internalFormat, width, height) {
  emit(
    R.OPR_TEX_STORAGE_2D,
    toU32(canvasId, "canvas_id"),
    toU32(target, "target"),
    toI32(levels, "levels"),
    toU32(internalFormat, "internal_format"),
    toI32(width, "width"),
    toI32(height, "height"),
  );
}
export function op_tex_storage_3d(canvasId, target, levels, internalFormat, width, height, depth) {
  emit(
    R.OPR_TEX_STORAGE_3D,
    toU32(canvasId, "canvas_id"),
    toU32(target, "target"),
    toI32(levels, "levels"),
    toU32(internalFormat, "internal_format"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(depth, "depth"),
  );
}
export function op_uniform_block_binding(programId, index, binding) {
  emit(
    R.OPR_UNIFORM_BLOCK_BINDING,
    toU32(programId, "program_id"),
    toU32(index, "uniform_block_index"),
    toU32(binding, "uniform_block_binding"),
  );
}
export function op_gl_lose_context(canvasId) {
  emit(R.OPR_LOSE_CONTEXT, toU32(canvasId, "canvas_id"));
}

export function op_shader_source(canvasId, shaderId, source) {
  const canvas = toU32(canvasId, "canvas_id");
  const shader = toU32(shaderId, "shader_id");
  emitBytes(canvas, R.OPR_SHADER_SOURCE, utf8.encode(stringOf(source, "source")), canvas, shader);
}
export function op_bind_attrib_location(programId, index, name) {
  const program = toU32(programId, "program_id");
  const location = toU32(index, "index");
  // No canvas among this op's arguments; an upload refusal it cannot have.
  emitBytes(0, R.OPR_BIND_ATTRIB_LOCATION, utf8.encode(stringOf(name, "name")), program, location);
}
export function op_buffer_data(canvasId, target, size, data, usage) {
  const canvas = toU32(canvasId, "canvas_id");
  const bytes = optionalBytesOf(data, "data");
  emitBytes(
    canvas,
    R.OPR_BUFFER_DATA,
    bytes ?? EMPTY,
    canvas,
    toU32(target, "target"),
    toI32(size, "size"),
    toU32(usage, "usage"),
    bytes === null ? 0 : 1,
  );
}
export function op_buffer_sub_data(canvasId, target, offset, data) {
  const canvas = toU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_BUFFER_SUB_DATA,
    bytesOf(data, "data"),
    canvas,
    toU32(target, "target"),
    toI32(offset, "offset"),
  );
}
export function op_tex_image_2d(canvasId, target, level, internalformat, width, height, border, format, type, data) {
  const canvas = toU32(canvasId, "canvas_id");
  const bytes = optionalBytesOf(data, "data");
  emitBytes(
    canvas,
    R.OPR_TEX_IMAGE_2D,
    bytes ?? EMPTY,
    canvas,
    toU32(target, "target"),
    toI32(level, "level"),
    toI32(internalformat, "internalformat"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(border, "border"),
    toU32(format, "format"),
    toU32(type, "type_"),
    bytes === null ? 0 : 1,
  );
}
export function op_tex_sub_image_2d(canvasId, target, level, xoffset, yoffset, width, height, format, type, data) {
  const canvas = toU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_TEX_SUB_IMAGE_2D,
    bytesOf(data, "data"),
    canvas,
    toU32(target, "target"),
    toI32(level, "level"),
    toI32(xoffset, "xoffset"),
    toI32(yoffset, "yoffset"),
    toI32(width, "width"),
    toI32(height, "height"),
    toU32(format, "format"),
    toU32(type, "type_"),
  );
}
export function op_compressed_tex_image_2d(canvasId, target, level, internalformat, width, height, border, data) {
  const canvas = toU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_COMPRESSED_TEX_IMAGE_2D,
    bytesOf(data, "data"),
    canvas,
    toU32(target, "target"),
    toI32(level, "level"),
    toU32(internalformat, "internalformat"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(border, "border"),
  );
}
export function op_compressed_tex_sub_image_2d(canvasId, target, level, xoffset, yoffset, width, height, format, data) {
  const canvas = toU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_COMPRESSED_TEX_SUB_IMAGE_2D,
    bytesOf(data, "data"),
    canvas,
    toU32(target, "target"),
    toI32(level, "level"),
    toI32(xoffset, "xoffset"),
    toI32(yoffset, "yoffset"),
    toI32(width, "width"),
    toI32(height, "height"),
    toU32(format, "format"),
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
  const canvas = toU32(canvasId, "canvas_id");
  const pbo = toI32(pboOffset, "pbo_offset");
  const bytes = pixels3d(
    optionalBytesOf(pixels, "pixels"),
    toU32(srcOffset, "src_offset"),
    toU32(bytesPerElement, "bytes_per_element"),
    pbo,
  );
  emitBytes(
    canvas,
    R.OPR_TEX_IMAGE_3D,
    bytes ?? EMPTY,
    canvas,
    toU32(target, "target"),
    toI32(level, "level"),
    toI32(internalFormat, "internal_format"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(depth, "depth"),
    toI32(border, "border"),
    toU32(format, "format"),
    toU32(ty, "ty"),
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
  const canvas = toU32(canvasId, "canvas_id");
  const pbo = toI32(pboOffset, "pbo_offset");
  const bytes = pixels3d(
    optionalBytesOf(pixels, "pixels"),
    toU32(srcOffset, "src_offset"),
    toU32(bytesPerElement, "bytes_per_element"),
    pbo,
  );
  emitBytes(
    canvas,
    R.OPR_TEX_SUB_IMAGE_3D,
    bytes ?? EMPTY,
    canvas,
    toU32(target, "target"),
    toI32(level, "level"),
    toI32(xoffset, "xoffset"),
    toI32(yoffset, "yoffset"),
    toI32(zoffset, "zoffset"),
    toI32(width, "width"),
    toI32(height, "height"),
    toI32(depth, "depth"),
    toU32(format, "format"),
    toU32(ty, "ty"),
    pbo,
    bytes === null ? 0 : 1,
  );
}

export function op_draw_buffers(canvasId, buffers) {
  const canvas = toU32(canvasId, "canvas_id");
  emitWords(canvas, R.OPR_DRAW_BUFFERS, u32ArrayOf(buffers, "buffers"), canvas);
}
export function op_invalidate_framebuffer(canvasId, target, attachments) {
  const canvas = toU32(canvasId, "canvas_id");
  emitWords(
    canvas,
    R.OPR_INVALIDATE_FRAMEBUFFER,
    u32ArrayOf(attachments, "attachments"),
    canvas,
    toU32(target, "target"),
  );
}
export function op_transform_feedback_varyings(canvasId, program, varyingsJoined, bufferMode) {
  const canvas = toU32(canvasId, "canvas_id");
  emitBytes(
    canvas,
    R.OPR_TRANSFORM_FEEDBACK_VARYINGS,
    utf8.encode(stringOf(varyingsJoined, "varyings_joined")),
    canvas,
    toU32(program, "program"),
    toU32(bufferMode, "buffer_mode"),
  );
}
