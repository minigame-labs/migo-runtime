// The producer's reader for the host-to-producer direction.
//
// The bytes half is checked against the Rust writer by
// `scripts/test-frame-wire-js-encoder.sh`, which makes one side produce and the
// other consume in both directions. What is checked here is the part a
// cross-language round trip cannot see: that a malformed message is refused
// with a reason rather than read as something plausible, and that a
// zero-length record cannot stall the reader.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/downlink.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import {
  DOWN_CLOCK_TICK,
  DOWN_FRAME_VERDICT,
  DOWNLINK_VERSION,
  DownlinkFormatError,
  ENVELOPE_WORDS,
  FRAME_VERDICT_WORDS,
  CLOCK_TICK_WORDS,
  MAGIC_DOWN,
  decodeBytes,
  decodeMessage,
  encodeBytes,
  encodeMessage,
  packHeader,
} from "../src/downlink.mjs";

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

function refuses(fn, fragment) {
  let thrown = null;
  try {
    fn();
  } catch (error) {
    thrown = error;
  }
  if (thrown === null) throw new Error("expected a refusal and got a value");
  if (!(thrown instanceof DownlinkFormatError)) {
    throw new Error(`expected DownlinkFormatError, got ${thrown.name}: ${thrown.message}`);
  }
  if (!thrown.message.includes(fragment)) {
    throw new Error(`the reason does not mention ${fragment}: ${thrown.message}`);
  }
}

const verdict = {
  kind: DOWN_FRAME_VERDICT,
  generation: 7,
  decision: 1,
  wireErrorCode: 0,
  remainingCredits: 2,
  // Past 32 bits deliberately: the two-word split is what a hand-written reader
  // gets wrong, and a small number would not notice.
  acceptedSequence: 0xff_1234_5678,
};

const tick = {
  kind: DOWN_CLOCK_TICK,
  generation: 7,
  frameId: 99,
  timestampNs: 0x123_4567_89ab,
};

console.log("The producer's downlink reader");

check("a batch survives the round trip in order", () => {
  const read = decodeMessage(encodeMessage([tick, verdict, tick]));
  assertEqual(read.length, 3, "record count");
  assertEqual(read[0].kind, DOWN_CLOCK_TICK, "first record kind");
  assertEqual(read[1].kind, DOWN_FRAME_VERDICT, "second record kind");
  assertEqual(read[1].acceptedSequence, verdict.acceptedSequence, "sequence across 32 bits");
  assertEqual(read[2].timestampNs, tick.timestampNs, "timestamp across 32 bits");
});

check("bytes and words agree", () => {
  const viaBytes = decodeBytes(encodeBytes([verdict]));
  const viaWords = decodeMessage(encodeMessage([verdict]));
  assertEqual(JSON.stringify(viaBytes), JSON.stringify(viaWords), "the two paths");
});

check("an envelope with no records reads as none", () => {
  assertEqual(decodeMessage(encodeMessage([])).length, 0, "record count");
});

check("the uplink magic is refused rather than read", () => {
  const words = encodeMessage([verdict]);
  words[0] = 0x4d474c31; // stream::MAGIC
  refuses(() => decodeMessage(words), "magic");
});

check("a version this build does not read is named", () => {
  const words = encodeMessage([verdict]);
  words[1] = DOWNLINK_VERSION + 1;
  refuses(() => decodeMessage(words), `version ${DOWNLINK_VERSION + 1}`);
});

check("a record claiming more words than the message holds is refused", () => {
  const words = encodeMessage([verdict]);
  words[ENVELOPE_WORDS] = packHeader(DOWN_FRAME_VERDICT, FRAME_VERDICT_WORDS + 1);
  refuses(() => decodeMessage(words), "words with");
});

check("a zero-length record cannot stall the reader", () => {
  // Without the `count < 1` check this loop would not advance, and a malformed
  // message would hang the producer rather than fail it.
  const words = encodeMessage([verdict]);
  words[ENVELOPE_WORDS] = packHeader(DOWN_FRAME_VERDICT, 0);
  refuses(() => decodeMessage(words), "claims 0 words");
});

check("a known kind with the wrong length is not read as that kind", () => {
  const words = encodeMessage([verdict, tick]);
  words[ENVELOPE_WORDS] = packHeader(DOWN_FRAME_VERDICT, CLOCK_TICK_WORDS);
  refuses(() => decodeMessage(words), `claims ${CLOCK_TICK_WORDS}`);
});

check("an unknown kind is refused rather than skipped", () => {
  const words = encodeMessage([verdict]);
  words[ENVELOPE_WORDS] = packHeader(0xabc, FRAME_VERDICT_WORDS);
  refuses(() => decodeMessage(words), "kind 2748");
});

check("a byte length that is not a whole number of words is refused", () => {
  refuses(() => decodeBytes(new Uint8Array([0, 1, 2])), "whole number of words");
});

check("a truncated envelope is named as such", () => {
  refuses(() => decodeMessage([MAGIC_DOWN]), "too short");
});

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
