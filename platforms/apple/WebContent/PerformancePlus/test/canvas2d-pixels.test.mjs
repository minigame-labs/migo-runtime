// The two 2D reads whose answer is pixels: a snapshot's, and a rectangle's.
//
// `getImageData` in the engine's facade captures into the host's snapshot pool
// and hands content a lazy `ImageData`; only a read of the bytes asks for them.
// The capture is a record, and the parity fixture beside this one compares it
// against the op command for command. What that fixture cannot reach is the
// asking, because a synchronous call needs a host endpoint -- so this gives it
// one, and checks what is asked for, what comes back, and what happens when the
// host cannot answer.
//
// The rule both reads share: an empty answer where the in-process op answers an
// empty `Vec`. `getImageData` has no way to report a failure to content, and a
// zero-filled picture would be a blank label with nothing saying why.
//
// Run:  node test/canvas2d-pixels.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_force_readback_snapshot, op_get_image_data, op_load_font } from "../src/lane-sync.mjs";
import {
  op_capture_canvas2d_snapshot,
  op_capture_canvas2d_snapshot_for_cache,
  op_frame_end_unified,
} from "../src/lane-stream.mjs";
import {
  CANVAS2D_PIXELS_PARAM_BYTES,
  CANVAS2D_QUERY_LOAD_FONT,
  SYNC_OP_CANVAS2D_FONT,
  SYNC_ERROR_OPERATION_FAILED,
  SYNC_ERROR_TIMED_OUT,
  SYNC_OP_CANVAS2D_IMAGE_DATA,
  SYNC_OP_CANVAS2D_SNAPSHOT,
  SyncRequestError,
} from "../src/sync-mailbox.mjs";
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

const session = new FrameSession({
  send(bytes) {
    session.handleMessage(
      encodeBytes([
        {
          kind: DOWN_FRAME_VERDICT,
          generation: 1,
          decision: 1,
          wireErrorCode: 0,
          remainingCredits: 2,
          acceptedSequence: sequenceOf(bytes),
        },
      ]),
    );
  },
  sendControl() {},
});

let asked = null;
let failWith = null;
const sync = {
  call(request) {
    asked = request;
    if (failWith !== null) {
      const error = failWith;
      failWith = null;
      throw error;
    }
    // The host answers exactly the rectangle it was asked for, which is what it
    // refuses to do otherwise.
    return new Uint8Array(request.maxReplyBytes).fill(0x7f);
  },
};

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
  sync,
  report() {},
});

/** The six words of a 2D pixel read's arguments. */
function paramsOf(request) {
  const view = new DataView(request.params.buffer, request.params.byteOffset, request.params.byteLength);
  return {
    bytes: request.params.byteLength,
    target: view.getUint32(0, true),
    x: view.getInt32(4, true),
    y: view.getInt32(8, true),
    width: view.getUint32(12, true),
    height: view.getUint32(16, true),
    reserved: view.getUint32(20, true),
  };
}

// ---- the rectangle read -----------------------------------------------------

asked = null;
let pixels = op_get_image_data(7, -3, 5, 4, 2);
check(
  pixels.length === 4 * 2 * 4 && pixels.every((byte) => byte === 0x7f),
  "a rectangle read answers the rectangle's RGBA8 rows",
);
check(
  asked.operation === SYNC_OP_CANVAS2D_IMAGE_DATA &&
    asked.maxReplyBytes === 4 * 2 * 4 &&
    JSON.stringify(paramsOf(asked)) ===
      JSON.stringify({
        bytes: CANVAS2D_PIXELS_PARAM_BYTES,
        target: 7,
        x: -3,
        y: 5,
        width: 4,
        height: 2,
        reserved: 0,
      }),
  "the request names the canvas, the rectangle and nothing else",
);

asked = null;
check(
  op_get_image_data(1, 0, 0, 0, 8).length === 0 && asked === null,
  "a zero-area read answers empty without asking",
);
check(
  op_get_image_data(1, 0, 0, 8193, 1).length === 0 && asked === null,
  "a rectangle past the surface cap answers empty without asking",
);

failWith = new SyncRequestError(SYNC_ERROR_OPERATION_FAILED);
check(
  op_get_image_data(1, 0, 0, 2, 2).length === 0,
  "a read the host could not do answers empty, as the op does",
);
failWith = new SyncRequestError(SYNC_ERROR_TIMED_OUT);
let threw = "nothing";
try {
  op_get_image_data(1, 0, 0, 2, 2);
} catch (error) {
  threw = error.name;
}
check(threw === "SyncRequestError", "a timeout or a dead session still reaches the caller");

// ---- the snapshot read ------------------------------------------------------

asked = null;
op_capture_canvas2d_snapshot(1, 4, 8, 16, 32, 501);
pixels = op_force_readback_snapshot(501);
check(
  pixels.length === 16 * 32 * 4,
  "a snapshot read is sized from the capture, which is the only place that size is",
);
check(
  asked.operation === SYNC_OP_CANVAS2D_SNAPSHOT &&
    JSON.stringify(paramsOf(asked)) ===
      JSON.stringify({
        bytes: CANVAS2D_PIXELS_PARAM_BYTES,
        target: 501,
        x: 0,
        y: 0,
        width: 16,
        height: 32,
        reserved: 0,
      }),
  "the request names the snapshot and the size it was captured at",
);

asked = null;
check(
  op_force_readback_snapshot(501).length === 0 && asked === null,
  "a snapshot is read once: the second ask is the pool miss the op answers empty",
);
check(
  op_force_readback_snapshot(0).length === 0 && asked === null,
  "snapshot 0 is not a snapshot",
);
check(
  op_force_readback_snapshot(9999).length === 0 && asked === null,
  "a snapshot this producer never captured is empty rather than a guess at its size",
);

// A capture the frame ended without anyone reading: the host's pool drains at
// the same point, so the size goes with it.
op_capture_canvas2d_snapshot(1, 0, 0, 8, 8, 502);
op_frame_end_unified();
asked = null;
check(
  op_force_readback_snapshot(502).length === 0 && asked === null,
  "a snapshot whose frame has ended is empty, as the drained pool would answer",
);

// The capture's own refusals, which are the op's: an id of zero and a rectangle
// past the cap are dropped before anything is written.
op_capture_canvas2d_snapshot(1, 0, 0, 8, 8, 0);
op_capture_canvas2d_snapshot(1, 0, 0, 8193, 1, 503);
asked = null;
check(
  op_force_readback_snapshot(503).length === 0 && asked === null,
  "a capture the op would have dropped leaves nothing to read",
);

// ---- the capture from the text-cache path -----------------------------------
//
// `fillText` asks whether the host already holds this exact label's texture;
// this host keeps no such cache and answers miss, always, so the record path
// through the cache is the capture and nothing else. The key's arguments are
// still converted, because deno_core converts them before the op's body runs.

op_capture_canvas2d_snapshot_for_cache(
  1, 0, 0, 24, 12, 601,
  "99", "16px sans-serif", 16, 400, false, 0xffffffff, 0, 3, 24, 12,
);
asked = null;
check(
  op_force_readback_snapshot(601).length === 24 * 12 * 4,
  "a capture from the text-cache path is a capture: the size is registered and readable",
);
check(
  paramsOf(asked).target === 601 && paramsOf(asked).width === 24,
  "and it is the same snapshot the plain capture would have made",
);

let refused = "nothing";
try {
  op_capture_canvas2d_snapshot_for_cache(
    1, 0, 0, 8, 8, 602,
    "99", "16px sans-serif", 16, 400, false, 0xffffffff, 0, 3, 24, {},
  );
} catch (error) {
  refused = error.name;
}
check(
  refused === "TypeError",
  "an argument the embedded op's conversion would refuse is refused here too",
);

// ---- loadFont ---------------------------------------------------------------
//
// Every step of it is the host's -- the sandbox, the file, the family the two
// shared helpers derive, the renderer -- so what this side owns is the question
// and the reading of the answer. An empty answer is the failure the op reports
// the same way: content checks the key it got back.

asked = null;
let family = op_load_font("fonts/MyFont.ttf", "Brand Sans");
const fontParams = new DataView(asked.params.buffer, asked.params.byteOffset, asked.params.byteLength);
check(
  asked.operation === SYNC_OP_CANVAS2D_FONT &&
    fontParams.getUint32(0, true) === CANVAS2D_QUERY_LOAD_FONT &&
    new TextDecoder().decode(asked.params.subarray(24, 24 + fontParams.getUint32(16, true))) ===
      "fonts/MyFont.ttf",
  "the request carries the path content named, under the loadFont kind",
);
check(family === "\u007f".repeat(asked.maxReplyBytes), "the answer is the family the host registered");

asked = null;
family = op_load_font("fonts/MyFont.ttf", undefined);
check(
  new DataView(asked.params.buffer, asked.params.byteOffset, asked.params.byteLength)
    .getUint32(20, true) === 0,
  "no family asked for is an empty one on the wire, not a missing field",
);

console.log(failures === 0 ? "PASS (Canvas2D pixels)" : `FAIL (${failures})`);
process.exit(failures === 0 ? 0 : 1);
