// The return path, as the WebContent producer reads it.
//
// The host's half is `engine/crates/frame-wire/src/downlink.rs`, which carries
// the reasoning for the shape: the downlink is always the loopback WebSocket
// (a custom URL scheme is request/response and the host cannot push through
// it), so message boundaries are the transport's and this format is one
// envelope per message followed by a run of records. There is no checksum, and
// that asymmetry with the uplink is deliberate -- the uplink is checksummed
// because the host must not trust the producer, and this direction is the
// reverse.
//
// Numbers, not names, cross the boundary. `scripts/test-frame-wire-js-encoder.sh`
// decodes what this file writes with the Rust reader and vice versa, because a
// constant that drifts here is a producer that silently stops understanding its
// own frame clock.

export const MAGIC_DOWN = 0x4D444C31;
export const DOWNLINK_VERSION = 1;

export const DOWN_FRAME_VERDICT = 1;
export const DOWN_CLOCK_TICK = 2;

export const FRAME_VERDICT_WORDS = 7;
export const CLOCK_TICK_WORDS = 5;

export const ENVELOPE_WORDS = 2;

// The uplink's packing, reused rather than restated: low 12 bits are the kind,
// the high 20 are the total word count including this header.
function kindOf(header) {
  return header & 0xfff;
}

function wordCountOf(header) {
  return header >>> 12;
}

export function packHeader(kind, wordCount) {
  return ((wordCount << 12) | (kind & 0xfff)) >>> 0;
}

/// Thrown rather than returned, because every call site in the producer is a
/// message loop that has nothing useful to do with a malformed message except
/// stop trusting the connection.
export class DownlinkFormatError extends Error {
  constructor(message) {
    super(message);
    this.name = "DownlinkFormatError";
  }
}

// A 64-bit field arrives as two words, low first -- the order the sync mailbox
// uses. Read through Number rather than BigInt: a frame sequence and a
// nanosecond timestamp both stay inside 2^53 for longer than any session runs
// (2^53 ns is 104 days), and BigInt would put an allocation on the frame clock's
// path for a range nothing reaches.
function readU64(words, at) {
  return words[at] + words[at + 1] * 0x1_0000_0000;
}

function writeU64(out, value) {
  out.push(value >>> 0);
  out.push(Math.floor(value / 0x1_0000_0000) >>> 0);
}

/// Decode one downlink message. Returns an array of records in wire order.
///
/// An unknown kind is an error, not a skip. The uplink's rule, and the reason
/// carries over: a producer that skipped a verdict it did not recognise would
/// keep scheduling frames against a host that had stopped agreeing with it.
export function decodeMessage(words) {
  if (words.length < ENVELOPE_WORDS) {
    throw new DownlinkFormatError("downlink message is too short to hold an envelope");
  }
  if (words[0] !== MAGIC_DOWN) {
    throw new DownlinkFormatError(
      `downlink magic is 0x${words[0].toString(16)}, expected 0x${MAGIC_DOWN.toString(16)}`,
    );
  }
  if (words[1] !== DOWNLINK_VERSION) {
    throw new DownlinkFormatError(
      `downlink version ${words[1]} is not ${DOWNLINK_VERSION}, which is the only one this build reads`,
    );
  }

  const records = [];
  let at = ENVELOPE_WORDS;
  while (at < words.length) {
    const header = words[at];
    const kind = kindOf(header);
    const count = wordCountOf(header);
    // `count < 1` is not redundant with the bound: a zero-length record would
    // leave `at` where it was and turn a malformed message into a hang.
    if (count < 1 || count > words.length - at) {
      throw new DownlinkFormatError(
        `downlink record claims ${count} words with ${words.length - at} left in the message`,
      );
    }
    const body = at + 1;
    if (kind === DOWN_FRAME_VERDICT) {
      if (count !== FRAME_VERDICT_WORDS) {
        throw new DownlinkFormatError(
          `a frame verdict is ${FRAME_VERDICT_WORDS} words and this one claims ${count}`,
        );
      }
      records.push({
        kind: DOWN_FRAME_VERDICT,
        generation: words[body],
        decision: words[body + 1],
        wireErrorCode: words[body + 2],
        remainingCredits: words[body + 3],
        acceptedSequence: readU64(words, body + 4),
      });
    } else if (kind === DOWN_CLOCK_TICK) {
      if (count !== CLOCK_TICK_WORDS) {
        throw new DownlinkFormatError(
          `a clock tick is ${CLOCK_TICK_WORDS} words and this one claims ${count}`,
        );
      }
      records.push({
        kind: DOWN_CLOCK_TICK,
        generation: words[body],
        frameId: words[body + 1],
        timestampNs: readU64(words, body + 2),
      });
    } else {
      throw new DownlinkFormatError(`downlink record kind ${kind} is not one this build reads`);
    }
    at += count;
  }
  return records;
}

/// Encode a run of records into one message.
///
/// The producer does not send downlink messages; this exists so the round-trip
/// gate can drive both directions from either language, which is what makes the
/// two readers comparable rather than merely both present.
export function encodeMessage(records) {
  const out = [MAGIC_DOWN, DOWNLINK_VERSION];
  for (const record of records) {
    if (record.kind === DOWN_FRAME_VERDICT) {
      out.push(packHeader(DOWN_FRAME_VERDICT, FRAME_VERDICT_WORDS));
      out.push(record.generation >>> 0);
      out.push(record.decision >>> 0);
      out.push(record.wireErrorCode >>> 0);
      out.push(record.remainingCredits >>> 0);
      writeU64(out, record.acceptedSequence);
    } else if (record.kind === DOWN_CLOCK_TICK) {
      out.push(packHeader(DOWN_CLOCK_TICK, CLOCK_TICK_WORDS));
      out.push(record.generation >>> 0);
      out.push(record.frameId >>> 0);
      writeU64(out, record.timestampNs);
    } else {
      throw new DownlinkFormatError(`cannot encode downlink record kind ${record.kind}`);
    }
  }
  return out;
}

/// Decode a message that arrived as bytes, which is what a WebSocket delivers.
export function decodeBytes(bytes) {
  if (bytes.byteLength % 4 !== 0) {
    throw new DownlinkFormatError(
      `downlink message is ${bytes.byteLength} bytes, which is not a whole number of words`,
    );
  }
  // A copy, because a `Uint8Array` from a WebSocket has no alignment guarantee
  // and `Uint32Array` on an unaligned offset throws. The messages are tens of
  // bytes; the copy is not the cost anyone will measure.
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const words = new Array(bytes.byteLength / 4);
  for (let i = 0; i < words.length; i += 1) {
    words[i] = view.getUint32(i * 4, /* littleEndian */ true);
  }
  return decodeMessage(words);
}

/// Encode to bytes, for the gate and for any host written in JavaScript.
export function encodeBytes(records) {
  const words = encodeMessage(records);
  const bytes = new Uint8Array(words.length * 4);
  const view = new DataView(bytes.buffer);
  words.forEach((word, i) => view.setUint32(i * 4, word, /* littleEndian */ true));
  return bytes;
}
