// One real frame: clear the canvas to blue, and present.
//
// WHY A FIXTURE AND NOT A BUILDER IN THE CONSUMER. The consumer is a Swift test
// that needs a valid packet to hand to `migo_session_submit_external_frame`.
// Building one there would be a THIRD implementation of the wire format, in a
// language neither the document nor the corpus checks -- and the whole reason
// contracts/frame-wire/wire-v1.md exists is that implementations which agree
// with each other and not with the specification are the failure mode. So the
// packet is produced here, by the encoder the corpus already checks, and
// committed; `scripts/test-frame-wire-js-encoder.sh` fails if this stops
// reproducing the committed bytes.
//
// WHAT IS IN IT, and why each field has the value it does:
//
//   launch_nonce       0xA3, matching what the consuming tests put in
//                      MigoSessionConfig. An all-zero nonce is what an
//                      uninitialised struct holds, which the session refuses.
//   runtime_generation 1, which is INITIAL_RUNTIME_GENERATION.
//   surface_generation 1, which is what the tests attach with. It is accepted
//                      whether or not the renderer has reported the surface
//                      yet: the ingress skips this check while its own
//                      generation is still 0, and matches it once it is 1. Both
//                      readings accept 1, which is what makes this fixture
//                      deterministic rather than a race.
//   resource_epoch     0. This one is checked exactly, and nothing has advanced
//                      it -- the packet names no resources.
//
// Usage: node emit-clear-frame.mjs [output-file]

import { mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import {
  encodeFrame,
  SECTION_KIND_COMMAND_STREAM,
} from "../src/wire-frame-packet.mjs";
import { MAGIC, STREAM_VERSION, OP_CLEAR, OP_CLEAR_COLOR } from "../src/render-opcodes.mjs";

/** Low twelve bits opcode, high twenty word count -- including the header. */
function packHeader(opcode, wordCount) {
  return ((wordCount << 12) | (opcode & 0xfff)) >>> 0;
}

/** A float's bit pattern, which is what the stream carries. */
const scratch = new DataView(new ArrayBuffer(4));
function floatBits(value) {
  scratch.setFloat32(0, value, true);
  return scratch.getUint32(0, true);
}

export const CANVAS_ID = 1;
export const GL_COLOR_BUFFER_BIT = 0x4000;
/** The colour this frame clears to, as the consumer will read it back. */
export const CLEAR_RGBA = [0, 0, 255, 255];

export function clearFrameBytes() {
  const words = [
    MAGIC,
    STREAM_VERSION,
    // CLEAR_COLOR: H C F F F F
    packHeader(OP_CLEAR_COLOR, 6),
    CANVAS_ID,
    floatBits(0),
    floatBits(0),
    floatBits(1),
    floatBits(1),
    // CLEAR: H C U
    packHeader(OP_CLEAR, 3),
    CANVAS_ID,
    GL_COLOR_BUFFER_BIT,
  ];
  const stream = new Uint8Array(words.length * 4);
  const view = new DataView(stream.buffer);
  words.forEach((word, index) => view.setUint32(index * 4, word, true));

  return encodeFrame({
    launchNonce: 0xa3n,
    sequence: 1n,
    runtimeGeneration: 1n,
    surfaceGeneration: 1n,
    resourceEpoch: 0n,
    // No `flags` argument: `encodeFrame` sets FLAG_PRESENT on every packet it
    // builds. Passing one would read as a choice this fixture does not have.
    sections: [{ kind: SECTION_KIND_COMMAND_STREAM, payload: stream }],
  });
}

const here = dirname(fileURLToPath(import.meta.url));
// It lives under the Swift test target, which is a strange home for a wire
// fixture and is the only one that works. SwiftPM resources must sit inside the
// target that declares them, and the macOS diagnostic package is a generated
// COPY of `platforms/apple` that carries `Tests/` and nothing else -- so a
// fixture anywhere neutral would be reachable from the repository and not from
// the package the tests actually run in. One file, three readers: this emitter
// writes it, `frame-wire`'s clear_frame_fixture test asserts what is in it, and
// MigoSyncBarrierABITests submits it to a live session.
const output =
  process.argv[2] ??
  join(
    here,
    "..", "..", "..",
    "Tests", "MigoAppleRendererTests", "Fixtures", "clear-blue-frame.bin",
  );
mkdirSync(dirname(output), { recursive: true });
const bytes = clearFrameBytes();
writeFileSync(output, bytes);
console.log(`emitted a ${bytes.length}-byte clear-to-blue frame into ${output}`);
