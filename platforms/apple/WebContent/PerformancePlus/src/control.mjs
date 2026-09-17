// The uplink's other message: a request for the next frame.
//
// Migo's `requestAnimationFrame` is fed by host vsync on every platform, and
// that demand has to cross the process boundary for a frame-clock tick to exist.
// The contract is "Uplink control messages" in contracts/frame-wire/wire-v1.md;
// the host's reader is `engine/crates/frame-wire/src/control.rs`, and
// `scripts/test-frame-wire-js-encoder.sh` puts bytes through both in both
// directions.
//
// Its own envelope, never the frame magic: the host routes each socket message
// by its first word, so a control message can never be read as a frame.

export const MAGIC_CONTROL = 0x4d554331;
export const CONTROL_VERSION = 1;
export const CONTROL_ENVELOPE_WORDS = 2;
export const MAX_CONTROL_WORDS = 64;

export const UP_REQUEST_FRAME = 1;
export const REQUEST_FRAME_WORDS = 2;

/// The one message a producer sends every frame, in bytes.
export const REQUEST_FRAME_MESSAGE_BYTES = (CONTROL_ENVELOPE_WORDS + REQUEST_FRAME_WORDS) * 4;

// Offset of the request's `generation` word inside that message.
const REQUEST_GENERATION_OFFSET = (CONTROL_ENVELOPE_WORDS + 1) * 4;

export class ControlFormatError extends Error {
  constructor(message) {
    super(message);
    this.name = "ControlFormatError";
  }
}

function packHeader(kind, wordCount) {
  return ((wordCount << 12) | (kind & 0xfff)) >>> 0;
}

/// The low 32 bits of a runtime generation, which is what the wire carries.
/// A BigInt is what content encodes packets with; a Number is accepted too.
export function generationWord(runtimeGeneration) {
  if (typeof runtimeGeneration === "bigint") {
    return Number(BigInt.asUintN(32, runtimeGeneration));
  }
  if (typeof runtimeGeneration === "number" && Number.isInteger(runtimeGeneration)) {
    return runtimeGeneration >>> 0;
  }
  throw new TypeError(`a runtime generation is an integer, not ${typeof runtimeGeneration}`);
}

/**
 * A request-for-a-frame message whose bytes are reused.
 *
 * Allocated once per session and rewritten per request, because a producer
 * asks every frame. Reusing it is sound because `WebSocket.send` copies the
 * bytes of a BufferSource when it is called (WHATWG WebSockets, `send()`), so
 * the next rewrite cannot reach a message already handed over.
 */
export class RequestFrameMessage {
  constructor() {
    this.bytes = new Uint8Array(REQUEST_FRAME_MESSAGE_BYTES);
    this.#view = new DataView(this.bytes.buffer);
    this.#view.setUint32(0, MAGIC_CONTROL, true);
    this.#view.setUint32(4, CONTROL_VERSION, true);
    this.#view.setUint32(8, packHeader(UP_REQUEST_FRAME, REQUEST_FRAME_WORDS), true);
  }

  #view;

  /// Write the generation and return the bytes to send.
  forGeneration(runtimeGeneration) {
    this.#view.setUint32(REQUEST_GENERATION_OFFSET, generationWord(runtimeGeneration), true);
    return this.bytes;
  }
}

/// Encode a run of records into one message. The reference writer the gate
/// holds against the Rust reader; the producer's hot path is `RequestFrameMessage`.
export function encodeControlBytes(records) {
  const words = [MAGIC_CONTROL, CONTROL_VERSION];
  for (const record of records) {
    if (record.kind === UP_REQUEST_FRAME) {
      words.push(packHeader(UP_REQUEST_FRAME, REQUEST_FRAME_WORDS));
      words.push(record.generation >>> 0);
    } else {
      throw new ControlFormatError(`cannot encode control record kind ${record.kind}`);
    }
  }
  const bytes = new Uint8Array(words.length * 4);
  const view = new DataView(bytes.buffer);
  words.forEach((word, index) => view.setUint32(index * 4, word, true));
  return bytes;
}

/// Decode a whole control message, refusing it exactly where the host would.
///
/// The producer never reads one in production; this exists so the gate can
/// check the host's writer from this side too, and so the refusal rules have a
/// second implementation to be compared against.
export function decodeControlBytes(bytes) {
  if (bytes.byteLength % 4 !== 0) {
    throw new ControlFormatError(`${bytes.byteLength} bytes is not a whole number of words`);
  }
  const count = bytes.byteLength / 4;
  if (count > MAX_CONTROL_WORDS) {
    throw new ControlFormatError(`${count} words is past the ${MAX_CONTROL_WORDS}-word ceiling`);
  }
  if (count < CONTROL_ENVELOPE_WORDS + 1) {
    throw new ControlFormatError(`${count} words cannot hold an envelope and a record`);
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const word = (index) => view.getUint32(index * 4, true);
  if (word(0) !== MAGIC_CONTROL) {
    throw new ControlFormatError(`control magic is 0x${word(0).toString(16)}`);
  }
  if (word(1) !== CONTROL_VERSION) {
    throw new ControlFormatError(`control version ${word(1)} is not ${CONTROL_VERSION}`);
  }
  const records = [];
  let at = CONTROL_ENVELOPE_WORDS;
  while (at < count) {
    const header = word(at);
    const kind = header & 0xfff;
    const length = header >>> 12;
    // Zero would leave `at` in place and turn a malformed message into a hang.
    if (length === 0 || length > count - at) {
      throw new ControlFormatError(`a record claims ${length} words with ${count - at} left`);
    }
    if (kind !== UP_REQUEST_FRAME) {
      throw new ControlFormatError(`control record kind ${kind} is not one this build reads`);
    }
    if (length !== REQUEST_FRAME_WORDS) {
      throw new ControlFormatError(
        `a frame request is ${REQUEST_FRAME_WORDS} words and this one claims ${length}`,
      );
    }
    records.push({ kind: UP_REQUEST_FRAME, generation: word(at + 1) });
    at += length;
  }
  return records;
}
