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
  readbackBytesPerPixel,
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

// Every format and type the readback table names, and one of each it does not:
// the records cycle through the sized pairs, and `pairs.jsonl` gives the Rust
// side this copy's size for every combination, so the two tables are compared
// pair by pair rather than only where the records happened to land.
const FORMATS = [
  0x1908, 0x8d99, 0x80e1, 0x1907, 0x8d98, 0x8227, 0x8228, 0x190a, 0x84f9,
  0x1903, 0x8d94, 0x1909, 0x1906, 0x1902, 0x1901, 0x1234,
];
const TYPES = [
  0x1400, 0x1401, 0x1402, 0x1403, 0x140b, 0x8d61, 0x1404, 0x1405, 0x1406,
  0x8363, 0x8033, 0x8034, 0x8365, 0x8366, 0x8368, 0x8c3b, 0x8c3e, 0x84fa, 0x8dad, 0x1234,
];
const pairs = [];
for (const format of FORMATS) {
  for (const type of TYPES) pairs.push({ format, type, bytesPerPixel: readbackBytesPerPixel(format, type) });
}
const sized = pairs.filter((pair) => pair.bytesPerPixel !== null);

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
    format: sized[index % sized.length].format,
    type: sized[index % sized.length].type,
  };
  const bytes = encodeReadPixelsParams(record);
  const name = `params-${String(index).padStart(4, "0")}.bin`;
  writeFileSync(join(outputDirectory, name), bytes);
  manifest.push({
    ...record,
    file: name,
    replyBytes: readPixelsReplyBytes(width, height, record.format, record.type),
  });
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

writeFileSync(
  join(outputDirectory, "pairs.jsonl"),
  pairs
    .map((pair) => `{"format":${pair.format},"type":${pair.type},"bytes_per_pixel":${pair.bytesPerPixel ?? -1}}`)
    .join("\n") + "\n",
);

console.log(`emitted ${manifest.length} params records and ${pairs.length} pixel pairs into ${outputDirectory}`);
