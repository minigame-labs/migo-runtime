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

const CRC_TABLE = (() => {
  // CRC32 (IEEE), the same polynomial crc32fast uses on the reading side.
  // Built once rather than pulled from a dependency: this file is loaded into
  // the process that runs untrusted game code, and every import is one more
  // thing inside that boundary.
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index += 1) {
    let value = index;
    for (let bit = 0; bit < 8; bit += 1) {
      value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    }
    table[index] = value >>> 0;
  }
  return table;
})();

function crc32(bytes, from, to) {
  let crc = 0xffffffff;
  for (let index = from; index < to; index += 1) {
    crc = CRC_TABLE[(crc ^ bytes[index]) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
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
  let crc = 0xffffffff;
  const update = (byte) => {
    crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  };
  const head = Math.min(bytes.length, OFF_CHECKSUM);
  for (let index = 0; index < head; index += 1) update(bytes[index]);
  if (bytes.length >= OFF_CHECKSUM + 4) {
    for (let index = 0; index < 4; index += 1) update(0);
    for (let index = OFF_CHECKSUM + 4; index < bytes.length; index += 1) update(bytes[index]);
  } else if (bytes.length > OFF_CHECKSUM) {
    for (let index = 0; index < 4; index += 1) update(0);
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
  sections,
}) {
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
  // PRESENT is required and is the only bit v1 defines, so it is not a
  // parameter. A caller that could clear it could ask for a packet the reader
  // must refuse.
  view.setUint32(OFF_FLAGS, FLAG_PRESENT, true);
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

export { crc32 as crc32ForTesting };
