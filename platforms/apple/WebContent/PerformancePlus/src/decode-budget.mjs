// How much owned storage the host will decode a packet into, estimated as the
// packet is written.
//
// The host refuses a packet whose decoded storage exceeds MAX_DECODED_FRAME_BYTES
// -- and on this lane a refusal ends the content -- so a frame larger than that
// has to cross as several packets, all but the last of them barriers. Where to
// split cannot be guessed: the host charges per decoded command at its own type
// sizes, rounds every batch up to a power of two, and charges the frame's op list
// and scratch too. This is that formula ("Decoded storage" in
// contracts/frame-wire/wire-v1.md, `frame_decode::budget::estimate` in Rust)
// over the contract's upper bounds for those sizes, which the Rust crate asserts
// its real sizes are within. The formula only grows with each size, so a packet
// this estimates within budget is never one the host refuses.
//
// Kept equal to `frame_decode::producer_estimated_bytes` on the same streams by
// engine/crates/frame-decode/tests/decode_budget_js_agreement.rs.

import {
  OP2D_BASE,
  OP2D_DRAW_IMAGE_BATCH,
  OP2D_FILL_TEXT,
  OP2D_SELECT_CANVAS,
  OP2D_SET_FONT,
  OP2D_SET_LINE_DASH,
  OP2D_STROKE_TEXT,
  OP_UNIFORM1IV,
  OP_UNIFORM_MATRIX2FV,
  OP_UNIFORM_MATRIX4FV,
  OPR_BIND_ATTRIB_LOCATION,
  OPR_BUFFER_DATA,
  OPR_BUFFER_SUB_DATA,
  OPR_COMPRESSED_TEX_IMAGE_2D,
  OPR_COMPRESSED_TEX_SUB_IMAGE_2D,
  OPR_DRAW_BUFFERS,
  OPR_INVALIDATE_FRAMEBUFFER,
  OPR_SHADER_SOURCE,
  OPR_TEX_IMAGE_2D,
  OPR_TEX_IMAGE_3D,
  OPR_TEX_SUB_IMAGE_2D,
  OPR_TEX_SUB_IMAGE_3D,
  OPR_TRANSFORM_FEEDBACK_VARYINGS,
} from "./render-opcodes.mjs";

export const MAX_DECODED_FRAME_BYTES = 4 * 1024 * 1024;

export const GL_COMMAND_BYTES = 144;
export const CANVAS2D_COMMAND_BYTES = 64;
export const FRAME_OP_BYTES = 64;
export const FRAME_PACKET_BYTES = 64;
export const UNIFORM_INLINE_WORDS = 16;
export const GL_BATCH_MIN_CAPACITY = 16;
export const CANVAS_BATCH_MIN_CAPACITY = 8;
export const FRAME_OP_MIN_CAPACITY = 8;
export const PENDING_CANVAS_MIN_CAPACITY = 4;
export const PAYLOAD_OVERHEAD_BYTES = 64;
export const STRING_BYTES = 24;

const HEADER_WORD_SHIFT = 12;
const HEADER_OPCODE_MASK = 0xfff;

/**
 * The payload records' shapes: words before `byte_length` (a byte payload) or
 * before `count` (a word list), the header included. The encoder writes these
 * records with the same prefixes; engine/crates/frame-wire/src/gl_resource.rs is
 * the source, and a record written with the wrong prefix is one the Rust
 * envelope refuses in engine-frames.test.mjs's interop run.
 */
export const PAYLOAD_PREFIX_WORDS = new Map([
  [OPR_SHADER_SOURCE, 3],
  [OPR_BIND_ATTRIB_LOCATION, 3],
  [OPR_BUFFER_DATA, 6],
  [OPR_BUFFER_SUB_DATA, 4],
  [OPR_TEX_IMAGE_2D, 11],
  [OPR_TEX_SUB_IMAGE_2D, 10],
  [OPR_COMPRESSED_TEX_IMAGE_2D, 8],
  [OPR_COMPRESSED_TEX_SUB_IMAGE_2D, 9],
  [OPR_TEX_IMAGE_3D, 13],
  [OPR_TEX_SUB_IMAGE_3D, 14],
  [OPR_TRANSFORM_FEEDBACK_VARYINGS, 4],
]);
export const WORD_LIST_PREFIX_WORDS = new Map([
  [OPR_DRAW_BUFFERS, 2],
  [OPR_INVALIDATE_FRAMEBUFFER, 3],
]);
/**
 * The 2D payload records' shapes, from engine/crates/frame-wire/src/canvas2d.rs:
 * a font or a text is a string the decode copies out, a dash list or an image
 * batch a vector of the record's own words.
 */
export const CANVAS2D_PAYLOAD_PREFIX_WORDS = new Map([
  [OP2D_SET_FONT, 1],
  [OP2D_FILL_TEXT, 4],
  [OP2D_STROKE_TEXT, 4],
]);
export const CANVAS2D_WORD_LIST_PREFIX_WORDS = new Map([
  [OP2D_SET_LINE_DASH, 1],
  [OP2D_DRAW_IMAGE_BATCH, 1],
]);

/** What a selected 2D record owns beyond its command. `canvas2d_payload_bytes`. */
function canvas2dPayloadBytes(words, start, opcode, wordCount) {
  const prefix = CANVAS2D_PAYLOAD_PREFIX_WORDS.get(opcode);
  if (prefix !== undefined) return words[start + prefix] + PAYLOAD_OVERHEAD_BYTES;
  const listPrefix = CANVAS2D_WORD_LIST_PREFIX_WORDS.get(opcode);
  if (listPrefix !== undefined) return (wordCount - listPrefix - 1) * 4 + PAYLOAD_OVERHEAD_BYTES;
  return 0;
}

/** `max(count, minimum).next_power_of_two()`, as Rust computes it (0 -> 1). */
function capacity(count, minimum) {
  const value = count > minimum ? count : minimum;
  if (value <= 1) return 1;
  // Integer arithmetic, not Math.log2: counts here stay far below 2^31, and a
  // floating logarithm is one rounding away from charging double.
  return 2 ** (32 - Math.clz32(value - 1));
}

/**
 * What a GL record owns beyond its command: a uniform array's spill, or a
 * payload record's bytes and their allocation. `frame_decode::budget`'s
 * `owned_payload_bytes`, over the same bounds.
 */
function ownedPayloadBytes(words, start, opcode, wordCount) {
  if (opcode >= OP_UNIFORM1IV && opcode <= OP_UNIFORM_MATRIX4FV) {
    const payload = wordCount - (opcode >= OP_UNIFORM_MATRIX2FV ? 4 : 3);
    return payload > UNIFORM_INLINE_WORDS ? capacity(payload, 0) * 4 : 0;
  }
  const prefix = PAYLOAD_PREFIX_WORDS.get(opcode);
  if (prefix !== undefined) {
    const length = words[start + prefix];
    if (opcode === OPR_TRANSFORM_FEEDBACK_VARYINGS) {
      return length + STRING_BYTES * (length + 1) + PAYLOAD_OVERHEAD_BYTES;
    }
    return length + PAYLOAD_OVERHEAD_BYTES;
  }
  const listPrefix = WORD_LIST_PREFIX_WORDS.get(opcode);
  if (listPrefix !== undefined) return (wordCount - listPrefix - 1) * 4 + PAYLOAD_OVERHEAD_BYTES;
  return 0;
}

/**
 * The running estimate for one packet's command stream.
 *
 * `fits` asks whether one more record keeps the finished packet within budget
 * without changing anything; `add` accounts it. Both take the record where it is
 * -- `words[start]` is its header -- because a payload record's charge is read
 * from its `byte_length`. Both are a handful of integer operations, because they
 * run once per record on the frame path.
 */
export class DecodeBudget {
  #bytes = 0;
  #ops = 0;
  #canvasCommands = 0;
  #glCommands = 0;
  #pendingCanvases = 0;
  #peakPendingCanvases = 0;
  #canvasSelected = false;

  reset() {
    this.#bytes = 0;
    this.#ops = 0;
    this.#canvasCommands = 0;
    this.#glCommands = 0;
    this.#pendingCanvases = 0;
    this.#peakPendingCanvases = 0;
    this.#canvasSelected = false;
  }

  /** The estimate if the stream ended after the records added so far. */
  get estimatedBytes() {
    return finish(
      this.#bytes,
      this.#ops,
      this.#canvasCommands,
      this.#glCommands,
      this.#pendingCanvases,
      this.#peakPendingCanvases,
    );
  }

  /** Whether adding the record at `words[start]` keeps the finished estimate within budget. */
  fits(words, start) {
    const header = words[start];
    const opcode = header & HEADER_OPCODE_MASK;
    const wordCount = header >>> HEADER_WORD_SHIFT;
    let bytes = this.#bytes;
    let ops = this.#ops;
    let canvasCommands = this.#canvasCommands;
    let glCommands = this.#glCommands;
    let pending = this.#pendingCanvases;
    let peak = this.#peakPendingCanvases;
    if (opcode === OP2D_SELECT_CANVAS) {
      if (canvasCommands !== 0) {
        bytes += capacity(canvasCommands, CANVAS_BATCH_MIN_CAPACITY) * CANVAS2D_COMMAND_BYTES;
        canvasCommands = 0;
        ops += 1;
        pending += 1;
        if (pending > peak) peak = pending;
      }
    } else if (opcode >= OP2D_BASE) {
      if (this.#canvasSelected) {
        if (glCommands !== 0) {
          bytes += capacity(glCommands, GL_BATCH_MIN_CAPACITY) * GL_COMMAND_BYTES;
          glCommands = 0;
          ops += 1;
        }
        canvasCommands += 1;
        bytes += canvas2dPayloadBytes(words, start, opcode, wordCount);
      }
    } else {
      if (canvasCommands !== 0) {
        bytes += capacity(canvasCommands, CANVAS_BATCH_MIN_CAPACITY) * CANVAS2D_COMMAND_BYTES;
        canvasCommands = 0;
        ops += 1;
        pending += 1;
        if (pending > peak) peak = pending;
      }
      ops += pending;
      pending = 0;
      glCommands += 1;
      bytes += ownedPayloadBytes(words, start, opcode, wordCount);
    }
    return finish(bytes, ops, canvasCommands, glCommands, pending, peak) <= MAX_DECODED_FRAME_BYTES;
  }

  /** Account the record at `words[start]`. */
  add(words, start) {
    const header = words[start];
    const opcode = header & HEADER_OPCODE_MASK;
    const wordCount = header >>> HEADER_WORD_SHIFT;
    if (opcode === OP2D_SELECT_CANVAS) {
      this.#closeCanvasBatch();
      this.#canvasSelected = true;
    } else if (opcode >= OP2D_BASE) {
      if (this.#canvasSelected) {
        this.#closeGlBatch();
        this.#canvasCommands += 1;
        this.#bytes += canvas2dPayloadBytes(words, start, opcode, wordCount);
      }
    } else {
      this.#closeCanvasBatch();
      this.#ops += this.#pendingCanvases;
      this.#pendingCanvases = 0;
      this.#glCommands += 1;
      this.#bytes += ownedPayloadBytes(words, start, opcode, wordCount);
    }
  }

  #closeCanvasBatch() {
    if (this.#canvasCommands === 0) return;
    this.#bytes += capacity(this.#canvasCommands, CANVAS_BATCH_MIN_CAPACITY) * CANVAS2D_COMMAND_BYTES;
    this.#canvasCommands = 0;
    this.#ops += 1;
    this.#pendingCanvases += 1;
    if (this.#pendingCanvases > this.#peakPendingCanvases) this.#peakPendingCanvases = this.#pendingCanvases;
  }

  #closeGlBatch() {
    if (this.#glCommands === 0) return;
    this.#bytes += capacity(this.#glCommands, GL_BATCH_MIN_CAPACITY) * GL_COMMAND_BYTES;
    this.#glCommands = 0;
    this.#ops += 1;
  }
}

/** The end of `estimate`: close both batches, materialize, charge the frame. */
function finish(bytes, ops, canvasCommands, glCommands, pending, peak) {
  if (canvasCommands !== 0) {
    bytes += capacity(canvasCommands, CANVAS_BATCH_MIN_CAPACITY) * CANVAS2D_COMMAND_BYTES;
    ops += 1;
    pending += 1;
    if (pending > peak) peak = pending;
  }
  if (glCommands !== 0) {
    bytes += capacity(glCommands, GL_BATCH_MIN_CAPACITY) * GL_COMMAND_BYTES;
    ops += 1;
  }
  ops += pending;
  const frameOps = capacity(ops + 2, FRAME_OP_MIN_CAPACITY);
  const pendingCapacity = peak === 0 ? 0 : capacity(peak, PENDING_CANVAS_MIN_CAPACITY);
  return bytes + frameOps * FRAME_OP_BYTES + pendingCapacity * 4 + FRAME_PACKET_BYTES;
}
