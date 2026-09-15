// Emit `readPixels` argument records for the Rust decoder to read.
//
// The producer encodes these and `frame_wire::sync::ReadPixelsParams::decode`
// reads them, and until this file existed nothing had ever put one through
// both. The two sides were written from the same table in
// contracts/frame-wire/wire-v1.md, which is the right way to write them and no
// evidence at all that they agree: a table can be read two ways, and the way
// that disagreement reaches a user is a `readPixels` answered over the wrong
// rectangle.
//
// Deterministic: same seed, same bytes, every run. A generator that produced
// different records each run would make a failure unreproducible, which is the
// property that gets a test deleted.
//
// Usage: node emit-sync-params.mjs <output-directory> [count]

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  encodeReadPixelsParams,
  readPixelsReplyBytes,
  GL_RGBA,
  GL_UNSIGNED_BYTE,
} from "../src/sync-mailbox.mjs";

const outputDirectory = process.argv[2];
if (!outputDirectory) {
  console.error("usage: emit-sync-params.mjs <output-directory> [count]");
  process.exit(2);
}
const count = Number(process.argv[3] ?? 64);
mkdirSync(outputDirectory, { recursive: true });

// A small xorshift, so the spread is reproducible without a dependency.
let seed = 0x9e3779b9;
function next(bound) {
  seed ^= seed << 13;
  seed ^= seed >>> 17;
  seed ^= seed << 5;
  seed >>>= 0;
  return seed % bound;
}

const manifest = [];
for (let index = 0; index < count; index += 1) {
  // Rectangles a real producer asks for: a single pixel, a full screen at 4x,
  // and a spread between. Offsets go negative too -- GL allows it, and a decoder
  // that only ever saw non-negative x would sign-extend wrongly without anyone
  // noticing until a game scrolled.
  const width = 1 + next(2048);
  const height = 1 + next(2048);
  const record = {
    canvasId: 1 + next(8),
    x: next(2) === 0 ? next(512) : -next(512),
    y: next(2) === 0 ? next(512) : -next(512),
    width,
    height,
    format: GL_RGBA,
    type: GL_UNSIGNED_BYTE,
  };
  const bytes = encodeReadPixelsParams(record);
  const name = `params-${String(index).padStart(4, "0")}.bin`;
  writeFileSync(join(outputDirectory, name), bytes);
  manifest.push({ ...record, file: name, replyBytes: readPixelsReplyBytes(width, height) });
}

// One flat JSON object per line. A JSON parser is a large thing to add to the
// crate whose point is a small trust boundary, and five lines of string search
// read a format this repository writes itself.
writeFileSync(
  join(outputDirectory, "manifest.jsonl"),
  manifest
    .map(
      (entry) =>
        `{"file":"${entry.file}","canvas_id":${entry.canvasId},"x":${entry.x},` +
        `"y":${entry.y},"width":${entry.width},"height":${entry.height},` +
        `"format":${entry.format},"type":${entry.type},"reply_bytes":${entry.replyBytes}}`,
    )
    .join("\n") + "\n",
);

console.log(`emitted ${manifest.length} params records into ${outputDirectory}`);
