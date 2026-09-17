// The JavaScript encoder against the committed corpus.
//
// This is the half of the wire contract that could not be checked before this
// file existed. `contracts/frame-wire/wire-v1.md` says two encoders are measured
// against a fixed corpus rather than against each other; until now there was one
// encoder, and "the corpus is what Rust produces" is a tautology, not a contract.
//
// Nothing here re-declares the cases. Every input comes from
// contracts/frame-wire/golden/index.json, which the Rust corpus test publishes
// from the same specification it encodes from -- so a case cannot be described
// one way for one encoder and another way for the other.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/golden-corpus.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import {
  encodeFrame,
  checksum,
  HEADER_BYTES,
  MAX_SECTIONS,
  MAX_TOTAL_BYTES,
  SECTION_KIND_COMMAND_STREAM,
} from "../src/wire-frame-packet.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const corpus = join(here, "../../../../../contracts/frame-wire/golden");

let failures = 0;
let checks = 0;

function check(condition, message) {
  checks += 1;
  if (!condition) {
    failures += 1;
    console.error(`  FAIL  ${message}`);
  }
}

function hexToBytes(hex) {
  const out = new Uint8Array(hex.length / 2);
  for (let index = 0; index < out.length; index += 1) {
    out[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  }
  return out;
}

function toHex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Where two byte strings first differ, which is the only useful part of a diff here. */
function firstDifference(left, right) {
  const limit = Math.min(left.length, right.length);
  for (let index = 0; index < limit; index += 1) {
    if (left[index] !== right[index]) return index;
  }
  return left.length === right.length ? -1 : limit;
}

const index = JSON.parse(readFileSync(join(corpus, "index.json"), "utf8"));
check(Array.isArray(index.cases) && index.cases.length >= 4, "index.json lists at least four cases");

let accepted = 0;
let deviations = 0;

for (const entry of index.cases) {
  const committed = new Uint8Array(readFileSync(join(corpus, `${entry.name}.bin`)));
  check(
    committed.length === entry.bytes,
    `${entry.name}: index says ${entry.bytes} bytes, the file has ${committed.length}`,
  );

  const input = entry.input;
  check(input !== undefined, `${entry.name}: the index publishes no input specification`);
  if (input === undefined) continue;
  check(input.flags === 0 || input.flags === 1, `${entry.name}: flags ${input.flags} is not PRESENT or a barrier`);

  // The wide fields arrive as decimal strings, and BigInt is what reads them
  // without loss. Number() here would pass on three of the four cases.
  const built = () =>
    encodeFrame({
      launchNonce: BigInt(input.launch_nonce),
      sequence: BigInt(input.sequence),
      runtimeGeneration: BigInt(input.runtime_generation),
      surfaceGeneration: BigInt(input.surface_generation),
      resourceEpoch: BigInt(input.resource_epoch),
      frameId: input.frame_id,
      // The only bit v1 defines, so a boolean carries the whole field; anything
      // else in the index is a case this encoder cannot represent.
      present: input.flags === 1,
      sections: input.sections.map((section) => ({
        kind: section.kind,
        itemCount: section.item_count,
        payload: hexToBytes(section.payload_hex),
      })),
    });

  if (input.deviation !== undefined) {
    // A case whose bytes depart from the canonical encoding on purpose. This
    // encoder cannot produce it -- that is the point of it being here -- so the
    // check is that it does not, rather than a silent skip that would also
    // "pass" if the encoder had quietly started emitting gaps.
    deviations += 1;
    const produced = built();
    check(
      toHex(produced) !== toHex(committed),
      `${entry.name}: the canonical encoder reproduced a deliberately non-canonical packet ` +
        `(deviation ${input.deviation}), so it can emit ${input.deviation} too`,
    );
    check(
      entry.accepted === false,
      `${entry.name}: a case with a deviation must be one a reader rejects`,
    );
    continue;
  }

  accepted += 1;
  const produced = built();
  const difference = firstDifference(produced, committed);
  check(
    difference === -1,
    `${entry.name}: the JavaScript encoder disagrees with the committed bytes at offset ` +
      `${difference} (produced ${produced.length} bytes, committed ${committed.length})` +
      (difference === -1
        ? ""
        : `\n          produced  ${toHex(produced.slice(Math.max(0, difference - 4), difference + 8))}` +
          `\n          committed ${toHex(committed.slice(Math.max(0, difference - 4), difference + 8))}`),
  );

  // And the checksum this encoder computes must be the one in the committed
  // bytes, checked independently of the whole-packet comparison so a CRC
  // disagreement is not reported as "byte 76 differs".
  const committedChecksum = new DataView(committed.buffer).getUint32(76, true);
  check(
    checksum(committed) === committedChecksum,
    `${entry.name}: this CRC32 does not reproduce the committed checksum`,
  );
}

check(accepted >= 3, `at least three accepted cases were encoded, saw ${accepted}`);
check(
  index.cases.some((entry) => entry.input?.flags === 0 && entry.accepted === true),
  "the corpus pins an accepted barrier packet",
);
check(deviations >= 1, `at least one deliberately non-canonical case was covered, saw ${deviations}`);

// The refusals. An encoder that will build anything asked of it puts the whole
// burden on the reader, and the reader is across a process boundary.
const oneStream = [{ kind: SECTION_KIND_COMMAND_STREAM, itemCount: 0, payload: new Uint8Array(0) }];
const base = { launchNonce: 1n, sequence: 1n, runtimeGeneration: 1n, sections: oneStream };

function refuses(what, options) {
  checks += 1;
  try {
    encodeFrame(options);
    failures += 1;
    console.error(`  FAIL  the encoder accepted ${what}`);
  } catch {
    /* expected */
  }
}

refuses("a packet with no sections", { ...base, sections: [] });
refuses("a packet with no command stream", {
  ...base,
  sections: [{ kind: 2, itemCount: 0, payload: new Uint8Array(0) }],
});
refuses("more sections than the cap", {
  ...base,
  sections: Array.from({ length: MAX_SECTIONS + 1 }, (_, kind) => ({
    kind: kind === 0 ? SECTION_KIND_COMMAND_STREAM : kind + 1,
    itemCount: 0,
    payload: new Uint8Array(0),
  })),
});
refuses("a payload above the absolute ceiling", {
  ...base,
  sections: [
    { kind: SECTION_KIND_COMMAND_STREAM, itemCount: 0, payload: new Uint8Array(MAX_TOTAL_BYTES) },
  ],
});

// The 53-bit trap, asserted rather than trusted to the corpus: reading a wide
// field through Number must not silently produce the same bytes.
checks += 1;
const wide = "340282366920938463463374607431768211450";
if (BigInt(wide) === BigInt(Number(wide))) {
  failures += 1;
  console.error("  FAIL  Number() round-trips a 128-bit nonce, so this test proves nothing");
}

check(HEADER_BYTES === 80, "the encoder's header size matches the frozen v1 layout");

console.log(`${checks - failures}/${checks} checks passed`);
if (failures > 0) {
  console.error(`\nFAIL: the JavaScript encoder does not agree with the committed corpus.`);
  process.exit(1);
}
console.log("PASS: the JavaScript encoder reproduces the committed corpus byte for byte.");
