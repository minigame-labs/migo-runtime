// Put the codec corpus through the producer's implementation.
//
// The Rust half is `engine/crates/shared/tests/codec_corpus.rs`, and
// `scripts/test-text-codec-agreement.sh` diffs the two outputs. Flat text, so a
// failure shows the case and both answers side by side.
//
// Usage: node emit-text-codec.mjs <corpus file>

import { readFileSync } from "node:fs";

import { decodeBytes, encodeString } from "../src/text-codec.mjs";

const corpus = process.argv[2];
if (!corpus) {
  console.error("usage: emit-text-codec.mjs <corpus file>");
  process.exit(2);
}

const hex = (bytes) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");

for (const line of readFileSync(corpus, "utf8").split("\n")) {
  if (line.startsWith("#") || line.trim().length === 0) continue;
  const [direction, encoding, value] = line.split("\t");
  try {
    if (direction === "enc") {
      console.log(`${line}\tok\t${hex(encodeString(JSON.parse(value), encoding))}`);
    } else {
      const bytes = Uint8Array.from(value.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
      console.log(`${line}\tok\t${JSON.stringify(decodeBytes(bytes, encoding))}`);
    }
  } catch (error) {
    // The message, not the class: what content sees is the text, and the two
    // implementations raise different classes for the same refusal.
    console.log(`${line}\trefused\t${error.message}`);
  }
}
