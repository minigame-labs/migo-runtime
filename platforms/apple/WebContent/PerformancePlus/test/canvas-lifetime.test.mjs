// What the producer writes when a canvas is created, resized and destroyed --
// including the calls its parity fixture cannot make.
//
// `test/fixtures/canvas-lifetime-calls.js` drives the engine's own facade in
// both runtimes and the effects are compared command for command
// (engine/crates/runtime-v8/src/rendering/webgl/canvas_lifetime_parity.rs). Two
// things that comparison cannot reach, because no facade call reaches them
// either:
//
//   * `op_destroy_canvas`. The engine calls it from a `FinalizationRegistry`,
//     which no test can schedule in both a V8 test runtime and WebKit.
//   * `op_resize_canvas` with neither dimension, which the width and height
//     setters cannot produce -- each names exactly one.
//
// So they are checked here against the records the reader expects
// (`frame_wire::canvas2d`, whose own cases check the other end), plus the
// refusals the ops make before writing anything: the surface pixel cap, and the
// onscreen canvas, which neither lane may destroy.
//
// Run:  node test/canvas-lifetime.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { endFrame } from "../src/engine-frames.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_get_canvas_info } from "../src/lane-local.mjs";
import {
  op_create_offscreen_canvas,
  op_destroy_canvas,
  op_resize_canvas,
} from "../src/lane-stream.mjs";
import {
  OP2D_DESTROY_CANVAS,
  OP2D_REGISTER_CANVAS,
  OP2D_RESIZE_CANVAS,
  OP2D_SELECT_CANVAS,
  RESIZE_CANVAS_HEIGHT,
  RESIZE_CANVAS_WIDTH,
} from "../src/render-opcodes.mjs";
import { sequenceOf } from "../src/wire-frame-packet.mjs";

let failures = 0;
function check(condition, message) {
  if (condition) {
    console.log(`  ok   ${message}`);
  } else {
    failures += 1;
    console.log(`  FAIL ${message}`);
  }
}

const sent = [];
const session = new FrameSession({
  send(bytes) {
    const packet = bytes.slice();
    sent.push(packet);
    session.handleMessage(
      encodeBytes([
        {
          kind: DOWN_FRAME_VERDICT,
          generation: 1,
          decision: 1,
          wireErrorCode: 0,
          remainingCredits: 2,
          acceptedSequence: sequenceOf(packet),
        },
      ]),
    );
  },
  sendControl() {},
});
bindEngineHost({
  session,
  identity: readEngineSessionConfig({
    launchNonce: "0x0123456789abcdeffedcba9876543210",
    runtimeGeneration: "1",
    surfaceGeneration: "1",
    resourceEpoch: "0",
    surfaceWidth: 64,
    surfaceHeight: 64,
  }),
  socketCeilingBytes: 64 * 1024,
  report() {},
});

/** A packet's command-stream words, header and version included. */
function wordsOf(packet) {
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  const headerBytes = view.getUint32(8, true);
  const offset = view.getUint32(headerBytes + 4, true);
  const count = view.getUint32(headerBytes + 12, true);
  return new Uint32Array(
    packet.buffer.slice(packet.byteOffset + offset, packet.byteOffset + offset + count * 4),
  );
}

/** Every record of every packet sent since the last call, as `[opcode, ...words]`. */
function recordsSent() {
  endFrame();
  const records = [];
  for (const packet of sent.splice(0)) {
    const words = wordsOf(packet);
    // Past the magic and the version.
    for (let cursor = 2; cursor < words.length; ) {
      const count = words[cursor] >>> 12;
      records.push([words[cursor] & 0xfff, ...words.slice(cursor + 1, cursor + count)]);
      cursor += count;
    }
  }
  return records;
}

// ---- creation ---------------------------------------------------------------

const PRODUCER_CANVAS_ID_BASE = 1 << 24;
const first = op_create_offscreen_canvas(2, 3);
check(
  first >= PRODUCER_CANVAS_ID_BASE,
  `a created canvas is numbered from the base the renderer requires (got ${first})`,
);
const second = op_create_offscreen_canvas(4, 5);
check(second === first + 1, "ids move forward rather than being reused");
check(
  JSON.stringify(recordsSent()) ===
    JSON.stringify([
      [OP2D_SELECT_CANVAS, first],
      [OP2D_REGISTER_CANVAS, 2, 3],
      [OP2D_SELECT_CANVAS, second],
      [OP2D_REGISTER_CANVAS, 4, 5],
    ]),
  "each creation selects its canvas and registers it at the size asked for",
);
check(
  JSON.stringify(op_get_canvas_info(first)) === JSON.stringify([2, 3]),
  "the size a creation chose is what the canvas reports locally",
);

// ---- resizing ---------------------------------------------------------------

op_resize_canvas(first, 8, undefined);
op_resize_canvas(first, undefined, 9);
op_resize_canvas(first, 10, 11);
// Neither dimension: the in-process op sends a resize the renderer applies to
// neither axis, and the record for it is one the reader refuses, so nothing is
// written.
op_resize_canvas(first, undefined, undefined);
check(
  JSON.stringify(recordsSent()) ===
    JSON.stringify([
      [OP2D_SELECT_CANVAS, first],
      [OP2D_RESIZE_CANVAS, RESIZE_CANVAS_WIDTH, 8, 0],
      [OP2D_RESIZE_CANVAS, RESIZE_CANVAS_HEIGHT, 0, 9],
      [OP2D_RESIZE_CANVAS, RESIZE_CANVAS_WIDTH | RESIZE_CANVAS_HEIGHT, 10, 11],
    ]),
  "a resize names the dimensions it was given, and one that names none is not written",
);
check(
  JSON.stringify(op_get_canvas_info(first)) === JSON.stringify([10, 11]),
  "the local size follows the resizes",
);

// ---- what the ops refuse before writing anything ----------------------------

function refused(call) {
  try {
    call();
    return "nothing was thrown";
  } catch (error) {
    return error.message;
  }
}

check(
  refused(() => op_create_offscreen_canvas(8193, 1)) ===
    "[InvalidArgument] offscreen canvas dimensions exceed the surface pixel cap",
  "a canvas wider than the cap is refused as the op refuses it",
);
check(
  refused(() => op_create_offscreen_canvas(8192, 8192)) === "nothing was thrown",
  "the largest canvas the cap allows is not refused",
);
check(
  refused(() => op_resize_canvas(first, 1, 8193)) ===
    "[InvalidArgument] canvas resize dimension exceeds the surface pixel cap",
  "a resize past the cap is refused as the op refuses it",
);
const afterRefusals = recordsSent();
check(
  JSON.stringify(afterRefusals) ===
    JSON.stringify([
      [OP2D_SELECT_CANVAS, second + 1],
      [OP2D_REGISTER_CANVAS, 8192, 8192],
    ]),
  "a refused call writes nothing; only the accepted creation is in the frame",
);
check(
  JSON.stringify(op_get_canvas_info(first)) === JSON.stringify([10, 11]),
  "a refused resize leaves the size where it was",
);

// ---- destroying -------------------------------------------------------------

op_destroy_canvas(second);
// The onscreen canvas: the in-process op returns before it sends anything, and
// the renderer refuses it as well, so no record is written for it.
op_destroy_canvas(1);
check(
  JSON.stringify(recordsSent()) ===
    JSON.stringify([
      [OP2D_SELECT_CANVAS, second],
      [OP2D_DESTROY_CANVAS],
    ]),
  "a destroy selects its canvas and says so once, and the onscreen canvas is not destroyed",
);
check(
  refused(() => op_get_canvas_info(second)) === `canvas ${second} not found`,
  "a destroyed canvas is no longer one this producer can describe",
);

console.log(failures === 0 ? "PASS (canvas lifetime records)" : `FAIL (${failures})`);
process.exit(failures === 0 ? 0 : 1);
