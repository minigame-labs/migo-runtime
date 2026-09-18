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

import { OP2D_BASE, OP2D_SELECT_CANVAS, OP_UNIFORM1IV, OP_UNIFORM_MATRIX2FV, OP_UNIFORM_MATRIX4FV } from "./render-opcodes.mjs";

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

/** `max(count, minimum).next_power_of_two()`, as Rust computes it (0 -> 1). */
function capacity(count, minimum) {
  const value = count > minimum ? count : minimum;
  if (value <= 1) return 1;
  // Integer arithmetic, not Math.log2: counts here stay far below 2^31, and a
  // floating logarithm is one rounding away from charging double.
  return 2 ** (32 - Math.clz32(value - 1));
}

/**
 * Payload words of a uniform-array record, or 0. The same split `record_spec`
 * makes: vector uniforms carry a three-word prefix, matrix uniforms four.
 */
function uniformPayloadWords(opcode, wordCount) {
  if (opcode >= OP_UNIFORM_MATRIX2FV && opcode <= OP_UNIFORM_MATRIX4FV) return wordCount - 4;
  if (opcode >= OP_UNIFORM1IV && opcode < OP_UNIFORM_MATRIX2FV) return wordCount - 3;
  return 0;
}

/**
 * The running estimate for one packet's command stream.
 *
 * `fits` asks whether one more record keeps the finished packet within budget
 * without changing anything; `add` accounts it. Both are a handful of integer
 * operations, because they run once per record on the frame path.
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

  /** Whether adding this record keeps the finished estimate within budget. */
  fits(opcode, wordCount) {
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
      const payload = uniformPayloadWords(opcode, wordCount);
      if (payload > UNIFORM_INLINE_WORDS) bytes += capacity(payload, 0) * 4;
    }
    return finish(bytes, ops, canvasCommands, glCommands, pending, peak) <= MAX_DECODED_FRAME_BYTES;
  }

  /** Account one record. */
  add(opcode, wordCount) {
    if (opcode === OP2D_SELECT_CANVAS) {
      this.#closeCanvasBatch();
      this.#canvasSelected = true;
    } else if (opcode >= OP2D_BASE) {
      if (this.#canvasSelected) {
        this.#closeGlBatch();
        this.#canvasCommands += 1;
      }
    } else {
      this.#closeCanvasBatch();
      this.#ops += this.#pendingCanvases;
      this.#pendingCanvases = 0;
      this.#glCommands += 1;
      const payload = uniformPayloadWords(opcode, wordCount);
      if (payload > UNIFORM_INLINE_WORDS) this.#bytes += capacity(payload, 0) * 4;
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
