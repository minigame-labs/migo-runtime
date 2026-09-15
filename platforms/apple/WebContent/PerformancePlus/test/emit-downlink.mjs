// Both halves of the downlink interop corpus, in one file.
//
//   emit-downlink.mjs write <dir>   -- write the corpus with the JavaScript
//                                      encoder, for the Rust reader
//   emit-downlink.mjs read  <dir>   -- read what the Rust writer produced and
//                                      check it against the same corpus
//
// One file because the corpus must be the same list on both sides, and two
// copies of a list are two lists. `engine/crates/frame-wire/tests/
// downlink_js_interop.rs` holds the Rust copy and asserts the lengths match, so
// a corpus that grows on one side only fails rather than silently covering less.
//
// Driven by scripts/test-frame-wire-js-encoder.sh.

import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  DOWN_CLOCK_TICK,
  DOWN_FRAME_VERDICT,
  decodeBytes,
  encodeBytes,
} from "../src/downlink.mjs";

const verdict = (acceptedSequence, decision) => ({
  kind: DOWN_FRAME_VERDICT,
  generation: 0x12345678,
  decision,
  wireErrorCode: decision === 3 ? 0x2001 : 0,
  remainingCredits: 3,
  acceptedSequence,
});

const tick = (timestampNs, frameId) => ({
  kind: DOWN_CLOCK_TICK,
  generation: 0x12345678,
  frameId,
  timestampNs,
});

// The same spread the Rust side carries, in the same order. Values past 2^32 in
// every 64-bit field, because the two-word split is the mistake a hand-written
// reader makes.
const CORPUS = [
  [],
  [verdict(1, 1)],
  [tick(16_666_667, 1)],
  [tick(0xff_ffff_ffff, 2), verdict(0x100_0000_0001, 1)],
  [verdict(0x7_ffff_ffff_ffff, 3)],
  Array.from({ length: 64 }, (_, i) =>
    i % 2 === 0 ? verdict(i + 0x1_0000_0000, 1) : tick(i * 16_666_667 + 0x2_0000_0000, i),
  ),
];

function stable(records) {
  return JSON.stringify(records);
}

const [, , mode, directory] = process.argv;
if (!mode || !directory) {
  console.error("usage: emit-downlink.mjs <write|read> <directory>");
  process.exit(2);
}

if (mode === "write") {
  mkdirSync(directory, { recursive: true });
  CORPUS.forEach((records, index) => {
    const bytes = encodeBytes(records);
    // Round-tripped on this side too, so an entry this encoder cannot read back
    // fails here rather than in another language.
    const read = decodeBytes(bytes);
    if (stable(read) !== stable(records)) {
      console.error(`FAIL: entry ${index} did not survive its own round trip`);
      process.exit(1);
    }
    writeFileSync(join(directory, `downlink-${String(index).padStart(3, "0")}.bin`), bytes);
  });
  console.log(`wrote ${CORPUS.length} downlink messages for the Rust reader`);
} else if (mode === "read") {
  const files = readdirSync(directory)
    .filter((name) => name.endsWith(".bin"))
    .sort();
  if (files.length !== CORPUS.length) {
    console.error(
      `FAIL: ${files.length} messages in ${directory}, and this corpus has ${CORPUS.length}`,
    );
    process.exit(1);
  }
  files.forEach((name, index) => {
    const bytes = readFileSync(join(directory, name));
    const read = decodeBytes(new Uint8Array(bytes));
    if (stable(read) !== stable(CORPUS[index])) {
      console.error(`FAIL: ${name} decoded to different records`);
      console.error(`  expected ${stable(CORPUS[index])}`);
      console.error(`  got      ${stable(read)}`);
      process.exit(1);
    }
  });
  console.log(`read ${files.length} Rust-encoded downlink messages`);
} else {
  console.error(`unknown mode ${mode}`);
  process.exit(2);
}
