// The WireFramePacket encoder, as it runs inside WebKit's WebContent process.
//
// This is the second of the two implementations the format has. The first is
// the Rust reader in engine/crates/frame-wire; neither is the specification,
// which is contracts/frame-wire/wire-v1.md. Both are checked against the fixed
// corpus in contracts/frame-wire/golden, because two implementations that agree
// with each other and not with a corpus is exactly the failure a corpus exists
// to catch -- and the way that failure reaches a user is a frame the renderer
// silently misreads on a device neither implementation was tested on.
//
// No imports, no DOM, no Node API. It has to run in a Dedicated Worker inside
// WebContent, where none of that exists, and in `node` for the corpus test.
//
// EVERY WIDE FIELD IS A BigInt, and that is not stylistic. `launch_nonce` is
// 128-bit and the generations and sequence are 64-bit; a JavaScript `Number`
// carries 53 bits exactly. An encoder that reached for `Number` would work on
// every small value anyone writes by hand and corrupt the identity of a real
// session, which is the field whose whole job is to be exact. The corpus case
// `all-section-kinds` is built from values past 2^53 so that mistake fails in
// the test rather than on a phone.

export const WIRE_MAGIC = 0x4d475046;
export const WIRE_VERSION = 1;
export const HEADER_BYTES = 80;
export const SECTION_ENTRY_BYTES = 16;
export const SECTION_ALIGNMENT = 8;
export const MAX_SECTIONS = 8;
export const MAX_TOTAL_BYTES = 4 * 1024 * 1024;

export const SECTION_KIND_COMMAND_STREAM = 1;
export const SECTION_KIND_INLINE_DATA = 2;
export const SECTION_KIND_RESOURCE_REFERENCES = 3;
export const SECTION_KIND_DAMAGE = 0x80000001;
export const SECTION_KIND_TIMING = 0x80000002;

export const RESOURCE_REFERENCE_BYTES = 4;
export const DAMAGE_RECT_BYTES = 16;

/// Set when a packet ends a frame. A packet without it is a barrier: the host
/// executes it and the frame goes on. See "Flags" in wire-v1.md.
export const FLAG_PRESENT = 1 << 0;

// Header offsets, in the order the document lists them.
const OFF_MAGIC = 0;
const OFF_WIRE_VERSION = 4;
const OFF_HEADER_BYTES = 8;
const OFF_TOTAL_BYTES = 12;
const OFF_LAUNCH_NONCE = 16;
const OFF_SEQUENCE = 32;
const OFF_RUNTIME_GENERATION = 40;
const OFF_SURFACE_GENERATION = 48;
const OFF_RESOURCE_EPOCH = 56;
const OFF_FRAME_ID = 64;
const OFF_FLAGS = 68;
const OFF_SECTION_COUNT = 72;
const OFF_CHECKSUM = 76;

// CRC32 (IEEE), the same polynomial crc32fast uses on the reading side, table
// driven eight bytes at a time ("slicing-by-8"). Built here rather than pulled
// from a dependency: this file is loaded into the process that runs untrusted
// game code, and every import is one more thing inside that boundary. Eight at
// a time because the producer checksums every frame it sends, whole, and a
// byte-at-a-time loop is the difference between a checksum that shows up in a
// frame's profile and one that does not.
const CRC_TABLES = (() => {
  const tables = new Uint32Array(256 * 8);
  for (let index = 0; index < 256; index += 1) {
    let value = index;
    for (let bit = 0; bit < 8; bit += 1) {
      value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    }
    tables[index] = value >>> 0;
  }
  for (let slice = 1; slice < 8; slice += 1) {
    for (let index = 0; index < 256; index += 1) {
      const previous = tables[(slice - 1) * 256 + index];
      tables[slice * 256 + index] = (previous >>> 8) ^ tables[previous & 0xff];
    }
  }
  return tables;
})();

/** Fold bytes[from, to) into a running CRC (pre-inverted state in, out). */
function crcUpdate(state, bytes, from, to) {
  const t = CRC_TABLES;
  let crc = state;
  let index = from;
  for (; to - index >= 8; index += 8) {
    const word =
      crc ^ (bytes[index] | (bytes[index + 1] << 8) | (bytes[index + 2] << 16) | (bytes[index + 3] << 24));
    crc =
      t[1792 + (word & 0xff)] ^
      t[1536 + ((word >>> 8) & 0xff)] ^
      t[1280 + ((word >>> 16) & 0xff)] ^
      t[1024 + (word >>> 24)] ^
      t[768 + bytes[index + 4]] ^
      t[512 + bytes[index + 5]] ^
      t[256 + bytes[index + 6]] ^
      t[bytes[index + 7]];
  }
  for (; index < to; index += 1) {
    crc = t[(crc ^ bytes[index]) & 0xff] ^ (crc >>> 8);
  }
  return crc >>> 0;
}

function crcZeros(state, count) {
  let crc = state;
  for (let index = 0; index < count; index += 1) {
    crc = CRC_TABLES[crc & 0xff] ^ (crc >>> 8);
  }
  return crc >>> 0;
}

function crc32(bytes, from, to) {
  return (crcUpdate(0xffffffff, bytes, from, to) ^ 0xffffffff) >>> 0;
}

/**
 * The sequence an encoded packet carries, or -1 when the bytes are too short to
 * hold one.
 *
 * Read as a Number, byte by byte, with no view allocated: the producer reads it
 * on every submit to count the packet against its window. A sequence stays
 * inside 2^53 for longer than any session runs -- at sixty frames a second that
 * is four million years -- so the Number is exact wherever it is used.
 */
export function sequenceOf(bytes) {
  if (bytes.length < OFF_SEQUENCE + 8) return -1;
  const at = OFF_SEQUENCE;
  const low = (bytes[at] | (bytes[at + 1] << 8) | (bytes[at + 2] << 16) | (bytes[at + 3] << 24)) >>> 0;
  const high =
    (bytes[at + 4] | (bytes[at + 5] << 8) | (bytes[at + 6] << 16) | (bytes[at + 7] << 24)) >>> 0;
  return low + high * 0x1_0000_0000;
}

/** CRC32 of the whole packet with the checksum field's own four bytes as zero. */
export function checksum(bytes) {
  const head = Math.min(bytes.length, OFF_CHECKSUM);
  let crc = crcUpdate(0xffffffff, bytes, 0, head);
  if (bytes.length > OFF_CHECKSUM) {
    crc = crcZeros(crc, 4);
    if (bytes.length >= OFF_CHECKSUM + 4) {
      crc = crcUpdate(crc, bytes, OFF_CHECKSUM + 4, bytes.length);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function alignUp(value) {
  return Math.ceil(value / SECTION_ALIGNMENT) * SECTION_ALIGNMENT;
}

function writeU64(view, offset, value) {
  view.setBigUint64(offset, BigInt.asUintN(64, value), true);
}

function writeU128(view, offset, value) {
  const wide = BigInt.asUintN(128, value);
  view.setBigUint64(offset, wide & 0xffffffffffffffffn, true);
  view.setBigUint64(offset + 8, wide >> 64n, true);
}

/**
 * Encode one frame.
 *
 * `sections` are laid out in the order given, each starting at the 8-byte
 * aligned end of the previous one; pad bytes are zero and the packet ends at
 * the aligned end of the last section. There is deliberately no way to ask this
 * function for a gap: a producer that could emit one would be emitting bytes
 * the checksum covers and no consumer interprets, and the reader rejects that.
 */
export function encodeFrame({
  launchNonce,
  sequence,
  runtimeGeneration,
  surfaceGeneration = 0n,
  resourceEpoch = 0n,
  frameId = 0,
  present = true,
  sections,
}) {
  if (typeof present !== "boolean") throw new TypeError("present is a boolean");
  if (!Array.isArray(sections) || sections.length === 0) {
    throw new TypeError("a packet must carry at least a command stream");
  }
  if (sections.length > MAX_SECTIONS) {
    throw new RangeError(`at most ${MAX_SECTIONS} sections, got ${sections.length}`);
  }
  if (!sections.some((section) => section.kind === SECTION_KIND_COMMAND_STREAM)) {
    throw new TypeError("every packet must carry a COMMAND_STREAM section");
  }

  const tableBytes = sections.length * SECTION_ENTRY_BYTES;
  const offsets = [];
  let cursor = HEADER_BYTES + tableBytes;
  for (const section of sections) {
    cursor = alignUp(cursor);
    offsets.push(cursor);
    cursor += section.payload.length;
  }
  const total = alignUp(cursor);
  if (total > MAX_TOTAL_BYTES) {
    throw new RangeError(`packet is ${total} bytes, above the ${MAX_TOTAL_BYTES} ceiling`);
  }

  // Zero-filled, which is also what makes every pad byte zero without a
  // separate step. The reader checks those bytes, so "we never wrote there" has
  // to mean "they are zero" rather than "they are whatever the buffer held".
  const bytes = new Uint8Array(total);
  const view = new DataView(bytes.buffer);

  view.setUint32(OFF_MAGIC, WIRE_MAGIC, true);
  view.setUint32(OFF_WIRE_VERSION, WIRE_VERSION, true);
  view.setUint32(OFF_HEADER_BYTES, HEADER_BYTES, true);
  view.setUint32(OFF_TOTAL_BYTES, total, true);
  writeU128(view, OFF_LAUNCH_NONCE, launchNonce);
  writeU64(view, OFF_SEQUENCE, sequence);
  writeU64(view, OFF_RUNTIME_GENERATION, runtimeGeneration);
  writeU64(view, OFF_SURFACE_GENERATION, surfaceGeneration);
  writeU64(view, OFF_RESOURCE_EPOCH, resourceEpoch);
  view.setUint32(OFF_FRAME_ID, frameId >>> 0, true);
  // PRESENT is the only bit v1 defines, so a boolean is the whole choice: a
  // caller cannot ask for a bit the reader refuses.
  view.setUint32(OFF_FLAGS, present ? FLAG_PRESENT : 0, true);
  view.setUint32(OFF_SECTION_COUNT, sections.length, true);

  sections.forEach((section, index) => {
    const entry = HEADER_BYTES + index * SECTION_ENTRY_BYTES;
    view.setUint32(entry, section.kind >>> 0, true);
    view.setUint32(entry + 4, offsets[index], true);
    view.setUint32(entry + 8, section.payload.length, true);
    view.setUint32(entry + 12, section.itemCount >>> 0, true);
    bytes.set(section.payload, offsets[index]);
  });

  view.setUint32(OFF_CHECKSUM, checksum(bytes), true);
  return bytes;
}

// A frame's command words are copied into the packet through a Uint32Array
// view, which writes in the platform's byte order; the wire is little-endian.
// Every platform WebKit ships on is, and this says so rather than assuming.
const LITTLE_ENDIAN = new Uint8Array(new Uint32Array([1]).buffer)[0] === 1;

// The one-section packet the producer sends every frame: header, a single
// COMMAND_STREAM table entry, then the stream at the first aligned offset.
const STREAM_PAYLOAD_OFFSET = HEADER_BYTES + SECTION_ENTRY_BYTES;
const STREAM_HEADER_WORDS = 2;

/**
 * One frame's packet, built in place.
 *
 * The encoder above takes finished sections and copies them into a new packet,
 * which is right for a corpus and wrong for a frame loop: every frame would pay
 * an allocation the size of its stream and a second copy of every command. This
 * reserves the header and the section table at the front of one buffer, takes
 * the frame's command words straight into it, and fills the header in when the
 * frame ends -- so a frame costs one copy of its words, one checksum pass, and
 * no allocation once the buffer has grown to the frame sizes content produces.
 *
 * The bytes are exactly `encodeFrame`'s for the same frame: the producer's test
 * suite holds the two to each other.
 */
export class FramePacketWriter {
  constructor({ launchNonce, runtimeGeneration, surfaceGeneration = 0n, resourceEpoch = 0n, magic, streamVersion }) {
    if (!LITTLE_ENDIAN) {
      throw new Error("FramePacketWriter needs a little-endian platform; the frame wire is little-endian");
    }
    for (const [name, value] of Object.entries({ launchNonce, runtimeGeneration, surfaceGeneration, resourceEpoch })) {
      if (typeof value !== "bigint") throw new TypeError(`${name} is a BigInt`);
    }
    if (typeof magic !== "number" || typeof streamVersion !== "number") {
      throw new TypeError("the command stream's magic and version come from render-opcodes.mjs");
    }
    this.#launchNonce = launchNonce;
    this.#runtimeGeneration = runtimeGeneration;
    this.surfaceGeneration = surfaceGeneration;
    this.resourceEpoch = resourceEpoch;
    this.#magic = magic;
    this.#streamVersion = streamVersion;
    this.#allocate(64 * 1024);
  }

  #launchNonce;
  #runtimeGeneration;
  #magic;
  #streamVersion;
  #buffer;
  #bytes;
  #words;
  #view;
  // Words appended so far, the stream header included once anything is.
  #used = 0;

  #allocate(bytes) {
    this.#buffer = new ArrayBuffer(bytes);
    this.#bytes = new Uint8Array(this.#buffer);
    this.#words = new Uint32Array(this.#buffer);
    this.#view = new DataView(this.#buffer);
    this.#used = 0;
  }

  /** Command words appended to this frame, not counting the stream header. */
  get wordCount() {
    return this.#used === 0 ? 0 : this.#used - STREAM_HEADER_WORDS;
  }

  /**
   * Whether `count` more words still make a packet within the ceiling. What a
   * producer that splits frames asks before appending, rather than learning it
   * from the RangeError `appendWords` throws.
   */
  fits(count) {
    const used = this.#used === 0 ? STREAM_HEADER_WORDS : this.#used;
    return alignUp(STREAM_PAYLOAD_OFFSET + (used + count) * 4) <= MAX_TOTAL_BYTES;
  }

  /**
   * Append `words[from, to)`, which are complete command records.
   * Throws RangeError past the packet ceiling: a frame that large cannot be
   * represented, and dropping part of it would draw half a frame.
   */
  appendWords(words, from, to) {
    const count = to - from;
    if (count <= 0) return;
    if (this.#used === 0) this.#used = STREAM_HEADER_WORDS;
    const neededBytes = STREAM_PAYLOAD_OFFSET + (this.#used + count) * 4 + SECTION_ALIGNMENT;
    if (neededBytes > this.#buffer.byteLength) {
      if (neededBytes > MAX_TOTAL_BYTES) {
        throw new RangeError(`a frame of ${neededBytes} bytes is above the ${MAX_TOTAL_BYTES}-byte packet ceiling`);
      }
      let capacity = this.#buffer.byteLength;
      while (capacity < neededBytes) capacity *= 2;
      const previous = this.#bytes;
      const usedBytes = STREAM_PAYLOAD_OFFSET + this.#used * 4;
      const used = this.#used;
      this.#allocate(Math.min(capacity, MAX_TOTAL_BYTES));
      this.#bytes.set(previous.subarray(0, usedBytes));
      this.#used = used;
    }
    // Word index, not byte offset: the payload starts on a word boundary.
    this.#words.set(words.subarray(from, to), STREAM_PAYLOAD_OFFSET / 4 + this.#used);
    this.#used += count;
  }

  /**
   * Finish the packet and return it, a view over this writer's buffer.
   * `sequence` and `frameId` are Numbers; a sequence stays inside 2^53 for
   * longer than a session runs. `present` false makes it a barrier: the host
   * executes it and the frame goes on.
   */
  finish(sequence, frameId, present = true) {
    if (this.#used === 0) throw new Error("finish() on a frame with no commands");
    const words = this.#words;
    const payloadWord = STREAM_PAYLOAD_OFFSET / 4;
    words[payloadWord] = this.#magic;
    words[payloadWord + 1] = this.#streamVersion;
    const streamBytes = this.#used * 4;
    const end = STREAM_PAYLOAD_OFFSET + streamBytes;
    const total = alignUp(end);
    this.#bytes.fill(0, end, total);

    const view = this.#view;
    view.setUint32(OFF_MAGIC, WIRE_MAGIC, true);
    view.setUint32(OFF_WIRE_VERSION, WIRE_VERSION, true);
    view.setUint32(OFF_HEADER_BYTES, HEADER_BYTES, true);
    view.setUint32(OFF_TOTAL_BYTES, total, true);
    writeU128(view, OFF_LAUNCH_NONCE, this.#launchNonce);
    view.setUint32(OFF_SEQUENCE, sequence >>> 0, true);
    view.setUint32(OFF_SEQUENCE + 4, Math.floor(sequence / 0x1_0000_0000) >>> 0, true);
    writeU64(view, OFF_RUNTIME_GENERATION, this.#runtimeGeneration);
    writeU64(view, OFF_SURFACE_GENERATION, this.surfaceGeneration);
    writeU64(view, OFF_RESOURCE_EPOCH, this.resourceEpoch);
    view.setUint32(OFF_FRAME_ID, frameId >>> 0, true);
    view.setUint32(OFF_FLAGS, present ? FLAG_PRESENT : 0, true);
    view.setUint32(OFF_SECTION_COUNT, 1, true);
    view.setUint32(HEADER_BYTES, SECTION_KIND_COMMAND_STREAM, true);
    view.setUint32(HEADER_BYTES + 4, STREAM_PAYLOAD_OFFSET, true);
    view.setUint32(HEADER_BYTES + 8, streamBytes, true);
    view.setUint32(HEADER_BYTES + 12, this.#used, true);
    const packet = this.#bytes.subarray(0, total);
    view.setUint32(OFF_CHECKSUM, checksum(packet), true);
    return packet;
  }

  /** Start the next frame in the same buffer. The last packet's bytes are dead. */
  reset() {
    this.#used = 0;
  }

  /**
   * Start the next frame in a new buffer, leaving the last packet's bytes to
   * whoever still holds them -- a request body the uplink has not read yet.
   */
  detach() {
    this.#allocate(this.#buffer.byteLength);
  }
}

export { crc32 as crc32ForTesting };
