// Emit packets for the Rust reader to validate.
//
// The golden corpus pins three shapes exactly. That is the right tool for
// "both encoders agree on these bytes" and the wrong one for "the JavaScript
// encoder never produces something the reader refuses" -- three cases cannot
// cover section counts, ragged payload lengths, the padding they imply, or the
// wide-field values that only appear at run time.
//
// So this writes a spread of packets and a manifest of what each one claims,
// and engine/crates/frame-wire/tests/js_interop.rs validates every one and
// checks the fields came back unchanged. A disagreement here is a real
// interoperability bug found on a Linux runner instead of on a phone.
//
// Deterministic: same seed, same bytes, every run. A generator that produced
// different packets each time would make a failure unreproducible, which is the
// property that gets a test deleted.
//
// Usage: node emit-packets.mjs <output-directory> [count]

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  encodeFrame,
  SECTION_KIND_COMMAND_STREAM,
  SECTION_KIND_INLINE_DATA,
  SECTION_KIND_RESOURCE_REFERENCES,
  SECTION_KIND_DAMAGE,
  SECTION_KIND_TIMING,
  RESOURCE_REFERENCE_BYTES,
  DAMAGE_RECT_BYTES,
} from "../src/wire-frame-packet.mjs";

const [, , outputDirectory, countArgument] = process.argv;
if (!outputDirectory) {
  console.error("usage: node emit-packets.mjs <output-directory> [count]");
  process.exit(2);
}
const count = Number.parseInt(countArgument ?? "128", 10);

// xorshift32, so the sequence is fixed and readable rather than whatever the
// engine's Math.random happens to be this release.
let state = 0x9e3779b9;
function next() {
  state ^= state << 13;
  state ^= state >>> 17;
  state ^= state << 5;
  return state >>> 0;
}
const pick = (limit) => next() % limit;

function payload(length) {
  const bytes = new Uint8Array(length);
  for (let index = 0; index < length; index += 1) bytes[index] = next() & 0xff;
  return bytes;
}

mkdirSync(outputDirectory, { recursive: true });
const manifest = [];

for (let index = 0; index < count; index += 1) {
  // The command stream is required and word-aligned; everything else is
  // optional, and the optional kinds have record widths the reader pins.
  const words = pick(24);
  const sections = [
    {
      kind: SECTION_KIND_COMMAND_STREAM,
      itemCount: words === 0 ? 0 : pick(words + 1),
      payload: payload(words * 4),
    },
  ];
  if (pick(2)) {
    // A ragged length on purpose: it is what produces an inter-section pad,
    // and the pad bytes are a rule the reader enforces.
    const length = pick(37);
    sections.push({ kind: SECTION_KIND_INLINE_DATA, itemCount: pick(length + 1), payload: payload(length) });
  }
  if (pick(2)) {
    const references = pick(6);
    sections.push({
      kind: SECTION_KIND_RESOURCE_REFERENCES,
      itemCount: references,
      payload: payload(references * RESOURCE_REFERENCE_BYTES),
    });
  }
  if (pick(2)) {
    const rects = pick(4);
    sections.push({
      kind: SECTION_KIND_DAMAGE,
      itemCount: rects,
      payload: payload(rects * DAMAGE_RECT_BYTES),
    });
  }
  if (pick(3) === 0) {
    const length = pick(19);
    sections.push({ kind: SECTION_KIND_TIMING, itemCount: pick(length + 1), payload: payload(length) });
  }

  // Wide values across the whole range, including the top of each field. A
  // reader that truncated would pass on small numbers forever.
  const wide = () => (BigInt(next()) << 32n) | BigInt(next());
  const frame = {
    launchNonce: (wide() << 64n) | wide(),
    sequence: BigInt(index + 1),
    runtimeGeneration: wide(),
    surfaceGeneration: pick(2) ? wide() : 0n,
    resourceEpoch: wide(),
    frameId: next(),
    // One in four a barrier, so both flag values cross in every run.
    present: pick(4) !== 0,
    sections,
  };

  const bytes = encodeFrame(frame);
  const name = `packet-${String(index).padStart(4, "0")}.bin`;
  writeFileSync(join(outputDirectory, name), bytes);
  manifest.push({
    name,
    bytes: bytes.length,
    launch_nonce: frame.launchNonce.toString(),
    sequence: frame.sequence.toString(),
    runtime_generation: frame.runtimeGeneration.toString(),
    surface_generation: frame.surfaceGeneration.toString(),
    resource_epoch: frame.resourceEpoch.toString(),
    frame_id: frame.frameId,
    presents: frame.present,
    section_count: sections.length,
    section_kinds: sections.map((section) => section.kind),
  });
}

// One flat JSON object per line. The Rust side reads this without a JSON
// dependency -- that crate's whole point is a small trust boundary, and a
// parser is a large thing to add for a manifest this repository writes itself.
// Line-delimited and flat is the shape that makes a five-line reader correct
// rather than nearly correct.
writeFileSync(
  join(outputDirectory, "manifest.jsonl"),
  `${manifest.map((entry) => JSON.stringify({ ...entry, section_kinds: entry.section_kinds.join(" ") })).join("\n")}\n`,
);
console.log(`emitted ${manifest.length} packets into ${outputDirectory}`);
