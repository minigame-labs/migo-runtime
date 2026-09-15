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
import {
  MAGIC,
  STREAM_VERSION,
  OP_CLEAR,
  OP_CLEAR_COLOR,
  OP_DISABLE,
  OP_ENABLE,
  OP_SCISSOR,
  OP2D_SELECT_CANVAS,
  OP2D_CREATE_CONTEXT,
  OP2D_SET_FILL_STYLE,
  OP2D_FILL_RECT,
} from "../src/render-opcodes.mjs";

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

/**
 * The two frames, and why there are two.
 *
 * One frame that clears to blue, read back as blue, is satisfied by a readback
 * that returns a constant -- and "the pixels happened to be the colour we
 * expected" is exactly the shape of green this repository keeps finding. So the
 * consumer submits both in one session and asserts the pixels CHANGED. A
 * readback that ignores the frame cannot pass that.
 */
export const FRAMES = [
  { name: "clear-blue-frame", sequence: 1n, rgba: [0, 0, 1, 1], readback: [0, 0, 255, 255] },
  { name: "clear-red-frame", sequence: 2n, rgba: [1, 0, 0, 1], readback: [255, 0, 0, 255] },
];

export const GL_SCISSOR_TEST = 0x0c11;

/**
 * A frame with two colours in it, and why one is not enough.
 *
 * The two frames above prove the pixels track the FRAME. They cannot prove the
 * readback's rectangle is honoured, because the whole surface is one flat
 * colour -- a readback that ignored x and y would return the same bytes for
 * every rectangle and pass.
 *
 * So this one clears to blue, then clears the lower-left quadrant to red behind
 * a scissor. Reading a pixel inside that quadrant and one outside it gives
 * different answers, which is what an ignored origin cannot produce. It also
 * establishes that the records execute in the order they were written, which
 * nothing before this did.
 *
 * GL's origin is bottom-left, so the scissor's (0,0) and `readPixels`' (0,0) are
 * the same corner.
 */
export const SCISSOR_FRAME = {
  name: "clear-scissor-frame",
  sequence: 3n,
  background: [0, 0, 255, 255],
  inside: [255, 0, 0, 255],
  quadrant: 32,
};

export function clearFrameBytes({ sequence, rgba }) {
  const words = [
    MAGIC,
    STREAM_VERSION,
    // CLEAR_COLOR: H C F F F F
    packHeader(OP_CLEAR_COLOR, 6),
    CANVAS_ID,
    floatBits(rgba[0]),
    floatBits(rgba[1]),
    floatBits(rgba[2]),
    floatBits(rgba[3]),
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
    sequence,
    runtimeGeneration: 1n,
    surfaceGeneration: 1n,
    resourceEpoch: 0n,
    // No `flags` argument: `encodeFrame` sets FLAG_PRESENT on every packet it
    // builds. Passing one would read as a choice this fixture does not have.
    sections: [{ kind: SECTION_KIND_COMMAND_STREAM, payload: stream }],
  });
}

export function scissorFrameBytes() {
  const words = [
    MAGIC,
    STREAM_VERSION,
    // The whole surface, blue.
    packHeader(OP_CLEAR_COLOR, 6),
    CANVAS_ID,
    floatBits(0),
    floatBits(0),
    floatBits(1),
    floatBits(1),
    packHeader(OP_CLEAR, 3),
    CANVAS_ID,
    GL_COLOR_BUFFER_BIT,
    // Then the lower-left quadrant, red, behind a scissor.
    packHeader(OP_ENABLE, 3),
    CANVAS_ID,
    GL_SCISSOR_TEST,
    // SCISSOR: H C I I I I
    packHeader(OP_SCISSOR, 6),
    CANVAS_ID,
    0,
    0,
    SCISSOR_FRAME.quadrant,
    SCISSOR_FRAME.quadrant,
    packHeader(OP_CLEAR_COLOR, 6),
    CANVAS_ID,
    floatBits(1),
    floatBits(0),
    floatBits(0),
    floatBits(1),
    packHeader(OP_CLEAR, 3),
    CANVAS_ID,
    GL_COLOR_BUFFER_BIT,
    // Left off, so the state this frame changed does not leak into the next.
    packHeader(OP_DISABLE, 3),
    CANVAS_ID,
    GL_SCISSOR_TEST,
  ];
  const stream = new Uint8Array(words.length * 4);
  const view = new DataView(stream.buffer);
  words.forEach((word, index) => view.setUint32(index * 4, word, true));

  return encodeFrame({
    launchNonce: 0xa3n,
    sequence: SCISSOR_FRAME.sequence,
    runtimeGeneration: 1n,
    surfaceGeneration: 1n,
    resourceEpoch: 0n,
    sections: [{ kind: SECTION_KIND_COMMAND_STREAM, payload: stream }],
  });
}

/**
 * A frame drawn with Canvas2D rather than WebGL.
 *
 * Half of what this engine draws is 2D, and nothing had ever put a 2D record
 * through this lane. The first record is `OP2D_CREATE_CONTEXT`, which did not
 * exist until this fixture needed it: the block was a complete drawing
 * vocabulary with no way to bring a context into existence, and a canvas
 * without one drops every record it is sent -- accepted, decoded, batched, and
 * silently not drawn.
 *
 * Two fills rather than one, for the reason the scissored frame has two
 * colours: a single fill read back as its own colour cannot show that the
 * records ran in order, and a whole-surface fill cannot show that the
 * rectangle was honoured.
 *
 * ★ Canvas2D's origin is TOP-left and `readPixels`' is bottom-left, so the
 * quadrant this fills at 2D (0,0) is the one `readPixels` finds at the TOP of
 * its own coordinate space. The consumer asserts both points, so a flip shows
 * up as two named colours in the wrong places rather than as a puzzle.
 */
export const CANVAS2D_FRAME = {
  name: "canvas2d-fill-frame",
  sequence: 4n,
  background: [0, 0, 255, 255],
  quadrant: [0, 255, 0, 255],
  size: 64,
  quadrantSize: 32,
};

export function canvas2dFrameBytes() {
  const words = [
    MAGIC,
    STREAM_VERSION,
    // Which canvas the records below apply to.
    packHeader(OP2D_SELECT_CANVAS, 2),
    CANVAS_ID,
    // And the context they need in order to do anything at all.
    packHeader(OP2D_CREATE_CONTEXT, 1),
    // The whole surface, blue.
    packHeader(OP2D_SET_FILL_STYLE, 5),
    floatBits(0),
    floatBits(0),
    floatBits(1),
    floatBits(1),
    packHeader(OP2D_FILL_RECT, 5),
    floatBits(0),
    floatBits(0),
    floatBits(CANVAS2D_FRAME.size),
    floatBits(CANVAS2D_FRAME.size),
    // Then one quadrant, green, at Canvas2D's origin.
    packHeader(OP2D_SET_FILL_STYLE, 5),
    floatBits(0),
    floatBits(1),
    floatBits(0),
    floatBits(1),
    packHeader(OP2D_FILL_RECT, 5),
    floatBits(0),
    floatBits(0),
    floatBits(CANVAS2D_FRAME.quadrantSize),
    floatBits(CANVAS2D_FRAME.quadrantSize),
  ];
  const stream = new Uint8Array(words.length * 4);
  const view = new DataView(stream.buffer);
  words.forEach((word, index) => view.setUint32(index * 4, word, true));

  return encodeFrame({
    launchNonce: 0xa3n,
    sequence: CANVAS2D_FRAME.sequence,
    runtimeGeneration: 1n,
    surfaceGeneration: 1n,
    resourceEpoch: 0n,
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
const directory =
  process.argv[2] ??
  join(here, "..", "..", "..", "Sources", "MigoAppleFrameHarness", "Fixtures");
mkdirSync(directory, { recursive: true });
for (const frame of FRAMES) {
  const bytes = clearFrameBytes(frame);
  writeFileSync(join(directory, `${frame.name}.bin`), bytes);
  console.log(`emitted a ${bytes.length}-byte ${frame.name} into ${directory}`);
}
for (const [frame, build] of [
  [SCISSOR_FRAME, scissorFrameBytes],
  [CANVAS2D_FRAME, canvas2dFrameBytes],
]) {
  const bytes = build();
  writeFileSync(join(directory, `${frame.name}.bin`), bytes);
  console.log(`emitted a ${bytes.length}-byte ${frame.name} into ${directory}`);
}
