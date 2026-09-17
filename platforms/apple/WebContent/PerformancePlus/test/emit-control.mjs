// Both halves of the control-message interop corpus, in one file.
//
//   emit-control.mjs write <dir>   -- write the corpus with the producer's
//                                     encoder, for the Rust reader
//   emit-control.mjs read  <dir>   -- read what the Rust writer produced and
//                                     check it against the same corpus
//
// `engine/crates/frame-wire/tests/control_js_interop.rs` holds the Rust copy of
// the list and asserts the lengths match. The first entries are written through
// `RequestFrameMessage`, the buffer the producer reuses every frame, because
// that is the writer production runs.
//
// Driven by scripts/test-frame-wire-js-encoder.sh.

import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  CONTROL_ENVELOPE_WORDS,
  MAX_CONTROL_WORDS,
  REQUEST_FRAME_WORDS,
  RequestFrameMessage,
  UP_REQUEST_FRAME,
  decodeControlBytes,
  encodeControlBytes,
} from "../src/control.mjs";

const request = (generation) => ({ kind: UP_REQUEST_FRAME, generation: generation >>> 0 });
const longest = (MAX_CONTROL_WORDS - CONTROL_ENVELOPE_WORDS) / REQUEST_FRAME_WORDS;

const CORPUS = [
  [request(1)],
  [request(0xffffffff)],
  [request(0x80000001)],
  [request(1), request(2)],
  Array.from({ length: longest }, (_, i) => request(Math.imul(i, 0x9e3779b9))),
];

// The single-request entries, written the way production writes them. Their
// generations are passed as BigInts wider than 32 bits, so the low-32-bits step
// is on the path the corpus covers.
const VIA_REUSED_MESSAGE = new Map([
  [0, 0x1_0000_0001n],
  [1, 0xffff_ffffn],
  [2, 0x7_8000_0001n],
]);

function stable(records) {
  return JSON.stringify(records);
}

const [, , mode, directory] = process.argv;
if (!mode || !directory) {
  console.error("usage: emit-control.mjs <write|read> <directory>");
  process.exit(2);
}

if (mode === "write") {
  mkdirSync(directory, { recursive: true });
  const reused = new RequestFrameMessage();
  CORPUS.forEach((records, index) => {
    const bytes = VIA_REUSED_MESSAGE.has(index)
      ? reused.forGeneration(VIA_REUSED_MESSAGE.get(index)).slice()
      : encodeControlBytes(records);
    const read = decodeControlBytes(bytes);
    if (stable(read) !== stable(records)) {
      console.error(`FAIL: entry ${index} did not survive its own round trip`);
      console.error(`  expected ${stable(records)}`);
      console.error(`  got      ${stable(read)}`);
      process.exit(1);
    }
    writeFileSync(join(directory, `control-${String(index).padStart(3, "0")}.bin`), bytes);
  });
  console.log(`wrote ${CORPUS.length} control messages for the Rust reader`);
} else if (mode === "read") {
  const files = readdirSync(directory)
    .filter((name) => name.endsWith(".bin"))
    .sort();
  if (files.length !== CORPUS.length) {
    console.error(`FAIL: ${files.length} messages in ${directory}, and this corpus has ${CORPUS.length}`);
    process.exit(1);
  }
  files.forEach((name, index) => {
    const read = decodeControlBytes(new Uint8Array(readFileSync(join(directory, name))));
    if (stable(read) !== stable(CORPUS[index])) {
      console.error(`FAIL: ${name} decoded to different records`);
      process.exit(1);
    }
  });
  console.log(`read ${files.length} Rust-encoded control messages`);
} else {
  console.error(`unknown mode ${mode}`);
  process.exit(2);
}
