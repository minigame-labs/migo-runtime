// Tagged values: the arguments a service request carries and the answers that
// come back.
//
// The layout is contracts/frame-wire/wire-v1.md, "Service values"; the Rust
// half is `frame_wire::value`, and the two are checked against each other
// through `test/emit-service.mjs`, not by reading both.
//
// Every value starts with a tag word -- the tag in the low eight bits, zero
// above -- and ends on a word boundary. Variable-length values are a byte
// length, the bytes, and zero padding to a word.
//
// No imports, no DOM, no Node API: it runs in a Worker and in `node`.

export const TAG_NULL = 0;
export const TAG_FALSE = 1;
export const TAG_TRUE = 2;
export const TAG_U32 = 3;
export const TAG_I32 = 4;
export const TAG_F64 = 5;
export const TAG_U64 = 6;
export const TAG_I64 = 7;
export const TAG_STRING = 8;
export const TAG_BYTES = 9;
export const TAG_JSON = 10;
export const TAG_ARRAY = 11;

/** Arrays nest at most this deep; the host refuses deeper. */
export const MAX_DEPTH = 16;

const ENCODER = new TextEncoder();
// `fatal`: a string the host wrote that is not UTF-8 is a host that broke the
// format, and reading it with replacement characters would hand content a
// different string than the one the host meant.
const DECODER = new TextDecoder("utf-8", { fatal: true });

/** A value this reader cannot read. The host wrote something that is not the format. */
export class ServiceValueError extends Error {
  constructor(message) {
    super(message);
    this.name = "ServiceValueError";
  }
}

/**
 * Builds a run of values into one growing buffer.
 *
 * `finish()` returns a view of exactly the bytes written. The buffer is not
 * reused afterwards -- a service message is handed to a transport that may
 * still be reading it -- so each writer is one message's.
 */
export class ValueWriter {
  #bytes;
  #view;
  #length = 0;

  constructor(initialBytes = 256) {
    this.#bytes = new Uint8Array(Math.max(16, initialBytes));
    this.#view = new DataView(this.#bytes.buffer);
  }

  get length() {
    return this.#length;
  }

  #reserve(count) {
    const needed = this.#length + count;
    if (needed <= this.#bytes.byteLength) return;
    let capacity = this.#bytes.byteLength * 2;
    while (capacity < needed) capacity *= 2;
    const grown = new Uint8Array(capacity);
    grown.set(this.#bytes.subarray(0, this.#length));
    this.#bytes = grown;
    this.#view = new DataView(grown.buffer);
  }

  /** One little-endian word, not a value: a record's own fields. */
  word(value) {
    this.#reserve(4);
    this.#view.setUint32(this.#length, value >>> 0, true);
    this.#length += 4;
  }

  /** Eight bytes: a record's own 64-bit field, from a BigInt. */
  word64(value) {
    this.#reserve(8);
    this.#view.setBigUint64(this.#length, BigInt.asUintN(64, BigInt(value)), true);
    this.#length += 8;
  }

  /** Raw bytes, zero-padded to a word. */
  raw(bytes) {
    const pad = (4 - (bytes.byteLength % 4)) % 4;
    this.#reserve(bytes.byteLength + pad);
    this.#bytes.set(bytes, this.#length);
    this.#length += bytes.byteLength;
    // The buffer is zero-filled when allocated and never rewritten backwards,
    // so padding is already zero; advancing is enough.
    this.#length += pad;
  }

  /** Overwrite a word already written, at `offset`: a record's length. */
  patchWord(offset, value) {
    this.#view.setUint32(offset, value >>> 0, true);
  }

  null() {
    this.word(TAG_NULL);
  }

  bool(value) {
    this.word(value ? TAG_TRUE : TAG_FALSE);
  }

  u32(value) {
    this.word(TAG_U32);
    this.word(value);
  }

  i32(value) {
    this.word(TAG_I32);
    this.word(value | 0);
  }

  f64(value) {
    this.word(TAG_F64);
    this.#reserve(8);
    this.#view.setFloat64(this.#length, value, true);
    this.#length += 8;
  }

  /** From a BigInt or a safe integer; 64 bits exactly, never through a double. */
  u64(value) {
    this.word(TAG_U64);
    this.word64(value);
  }

  i64(value) {
    this.word(TAG_I64);
    this.#reserve(8);
    this.#view.setBigInt64(this.#length, BigInt.asIntN(64, BigInt(value)), true);
    this.#length += 8;
  }

  #sized(tag, bytes) {
    this.word(tag);
    this.word(bytes.byteLength);
    this.raw(bytes);
  }

  /**
   * A string, as UTF-8. A lone surrogate becomes U+FFFD, which is what V8's
   * `#[string]` conversion does, so the host reads the string the embedded op
   * would have.
   */
  str(value) {
    this.#sized(TAG_STRING, ENCODER.encode(value));
  }

  /** Bytes, from any ArrayBufferView or ArrayBuffer, copied. */
  bytes(value) {
    const view =
      value instanceof ArrayBuffer
        ? new Uint8Array(value)
        : new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    this.#sized(TAG_BYTES, view);
  }

  json(text) {
    this.#sized(TAG_JSON, ENCODER.encode(text));
  }

  /** Start an array of `count` values; the caller writes them next. */
  array(count) {
    this.word(TAG_ARRAY);
    this.word(count);
  }

  /** The bytes written, as a view of this writer's buffer. */
  finish() {
    return this.#bytes.subarray(0, this.#length);
  }
}

class Reader {
  constructor(bytes) {
    this.bytes = bytes;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    this.at = 0;
  }

  need(count) {
    if (this.at + count > this.bytes.byteLength) {
      throw new ServiceValueError("a value runs past the end of the message");
    }
  }

  word() {
    this.need(4);
    const value = this.view.getUint32(this.at, true);
    this.at += 4;
    return value;
  }

  sized() {
    const length = this.word();
    this.need(length);
    const body = this.bytes.subarray(this.at, this.at + length);
    this.at += length;
    const pad = (4 - (length % 4)) % 4;
    this.need(pad);
    for (let index = 0; index < pad; index += 1) {
      if (this.bytes[this.at + index] !== 0) throw new ServiceValueError("padding after a value is not zero");
    }
    this.at += pad;
    return body;
  }

  text() {
    try {
      return DECODER.decode(this.sized());
    } catch (error) {
      if (error instanceof ServiceValueError) throw error;
      throw new ServiceValueError("a string value is not UTF-8");
    }
  }

  /**
   * One value, as JavaScript: null, booleans and numbers as themselves, the
   * 64-bit integers as BigInt (a Number would round them), strings as strings,
   * JSON as the structure it spells, bytes as a Uint8Array copied out of the
   * message, arrays as arrays.
   */
  value(depth) {
    const tag = this.word();
    switch (tag) {
      case TAG_NULL:
        return null;
      case TAG_FALSE:
        return false;
      case TAG_TRUE:
        return true;
      case TAG_U32:
        return this.word();
      case TAG_I32:
        return this.word() | 0;
      case TAG_F64: {
        this.need(8);
        const value = this.view.getFloat64(this.at, true);
        this.at += 8;
        return value;
      }
      case TAG_U64: {
        this.need(8);
        const value = this.view.getBigUint64(this.at, true);
        this.at += 8;
        return value;
      }
      case TAG_I64: {
        this.need(8);
        const value = this.view.getBigInt64(this.at, true);
        this.at += 8;
        return value;
      }
      case TAG_STRING:
        return this.text();
      case TAG_JSON: {
        // A structure the host serialised -- what a `#[serde]` op returns as an
        // object in the embedded runtime -- so content gets the object.
        const text = this.text();
        try {
          return JSON.parse(text);
        } catch {
          throw new ServiceValueError("a JSON value is not JSON");
        }
      }
      case TAG_BYTES:
        // Copied: the message's buffer is the transport's, and content keeps
        // what an op returns.
        return this.sized().slice();
      case TAG_ARRAY: {
        if (depth + 1 >= MAX_DEPTH) throw new ServiceValueError(`arrays nest deeper than ${MAX_DEPTH}`);
        const count = this.word();
        if (count > (this.bytes.byteLength - this.at) / 4) {
          throw new ServiceValueError("an array claims more values than the message holds");
        }
        const out = [];
        for (let index = 0; index < count; index += 1) out.push(this.value(depth + 1));
        return out;
      }
      default:
        throw new ServiceValueError(`tag word 0x${tag.toString(16)} is not a value tag`);
    }
  }
}

/** Every value in `bytes`, which must end exactly where the last one does. */
export function readValues(bytes) {
  if (bytes.byteLength % 4 !== 0) throw new ServiceValueError("the values are not a whole number of words");
  const reader = new Reader(bytes);
  const values = [];
  while (reader.at < bytes.byteLength) values.push(reader.value(0));
  return values;
}

/** Exactly one value that fills `bytes`. */
export function readValue(bytes) {
  const values = readValues(bytes);
  if (values.length !== 1) throw new ServiceValueError(`expected one value, found ${values.length}`);
  return values[0];
}
