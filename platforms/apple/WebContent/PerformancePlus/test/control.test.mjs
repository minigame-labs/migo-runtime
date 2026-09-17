// The producer's control-message encoder, and a reader that refuses exactly
// where the host's does.
//
// The bytes are checked against the Rust reader, in both directions, by
// `scripts/test-frame-wire-js-encoder.sh`. What is checked here is what a round
// trip cannot see: that the reusable message writes the same bytes as the
// reference encoder, and that every refusal the contract names is a refusal.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/control.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import {
  CONTROL_VERSION,
  ControlFormatError,
  MAGIC_CONTROL,
  MAX_CONTROL_WORDS,
  REQUEST_FRAME_MESSAGE_BYTES,
  RequestFrameMessage,
  UP_REQUEST_FRAME,
  decodeControlBytes,
  encodeControlBytes,
} from "../src/control.mjs";
import { WIRE_MAGIC } from "../src/wire-frame-packet.mjs";

let failures = 0;

function check(name, fn) {
  try {
    fn();
    console.log(`  ok   ${name}`);
  } catch (error) {
    failures += 1;
    console.log(`  FAIL ${name}`);
    console.log(`       ${error && error.message}`);
  }
}

function assertEqual(actual, expected, message) {
  if (actual !== expected) throw new Error(`${message}: expected ${expected}, got ${actual}`);
}

function refuses(bytes, fragment) {
  let thrown = null;
  try {
    decodeControlBytes(bytes);
  } catch (error) {
    thrown = error;
  }
  if (!(thrown instanceof ControlFormatError)) {
    throw new Error(`expected a ControlFormatError, got ${thrown && thrown.name}`);
  }
  if (!thrown.message.includes(fragment)) {
    throw new Error(`the reason does not mention ${fragment}: ${thrown.message}`);
  }
}

function words(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return Array.from({ length: bytes.byteLength / 4 }, (_, i) => view.getUint32(i * 4, true));
}

function bytesOf(list) {
  const out = new Uint8Array(list.length * 4);
  const view = new DataView(out.buffer);
  list.forEach((word, i) => view.setUint32(i * 4, word >>> 0, true));
  return out;
}

const request = (generation) => ({ kind: UP_REQUEST_FRAME, generation });

console.log("The producer's control messages");

check("the magic is neither the frame magic nor the downlink's", () => {
  if (MAGIC_CONTROL === WIRE_MAGIC) throw new Error("the router could not tell a frame apart");
  if (MAGIC_CONTROL === 0x4d444c31) throw new Error("the downlink magic");
});

check("a request survives the round trip", () => {
  const bytes = encodeControlBytes([request(0xdeadbeef)]);
  assertEqual(bytes.byteLength, 16, "envelope and one two-word record");
  const read = decodeControlBytes(bytes);
  assertEqual(read.length, 1, "one record");
  assertEqual(read[0].generation, 0xdeadbeef, "the generation, as an unsigned word");
});

check("the reusable message writes what the reference encoder writes", () => {
  const message = new RequestFrameMessage();
  const first = message.forGeneration(7n).slice();
  assertEqual(first.byteLength, REQUEST_FRAME_MESSAGE_BYTES, "its length");
  assertEqual(words(first).join(","), words(encodeControlBytes([request(7)])).join(","), "gen 7");
  const second = message.forGeneration(0x1_2345_6789n);
  assertEqual(
    words(second).join(","),
    words(encodeControlBytes([request(0x2345_6789)])).join(","),
    "a rewritten generation, low 32 bits",
  );
  assertEqual(second, message.bytes, "the same buffer, rewritten rather than reallocated");
});

check("several records come back in order", () => {
  const read = decodeControlBytes(encodeControlBytes([request(1), request(2), request(3)]));
  assertEqual(read.map((record) => record.generation).join(","), "1,2,3", "order");
});

check("trailing bytes are refused", () => {
  const bytes = encodeControlBytes([request(1)]);
  const longer = new Uint8Array(bytes.byteLength + 1);
  longer.set(bytes);
  refuses(longer, "whole number of words");
});

check("an envelope with no record is refused", () => {
  refuses(bytesOf([MAGIC_CONTROL, CONTROL_VERSION]), "cannot hold");
});

check("a message past the ceiling is refused, and one at it is read", () => {
  const records = Array.from({ length: MAX_CONTROL_WORDS / 2 }, (_, i) => request(i));
  refuses(encodeControlBytes(records), "ceiling");
  assertEqual(
    decodeControlBytes(encodeControlBytes(records.slice(1))).length,
    MAX_CONTROL_WORDS / 2 - 1,
    "exactly the ceiling",
  );
});

check("a wrong magic or version is refused", () => {
  const list = words(encodeControlBytes([request(1)]));
  refuses(bytesOf([WIRE_MAGIC, ...list.slice(1)]), "magic");
  refuses(bytesOf([list[0], CONTROL_VERSION + 1, ...list.slice(2)]), "version");
});

check("a record that runs past the end, or has no length, is refused", () => {
  const list = words(encodeControlBytes([request(1)]));
  refuses(bytesOf([...list.slice(0, 2), (3 << 12) | UP_REQUEST_FRAME, list[3]]), "claims 3");
  refuses(bytesOf([...list.slice(0, 2), UP_REQUEST_FRAME, list[3]]), "claims 0");
});

check("an unknown kind or a wrong length is refused", () => {
  const list = words(encodeControlBytes([request(1)]));
  refuses(bytesOf([...list.slice(0, 2), (2 << 12) | 0x7ff, list[3]]), "kind 2047");
  refuses(bytesOf([...list.slice(0, 2), (1 << 12) | UP_REQUEST_FRAME]), "claims 1");
});

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
