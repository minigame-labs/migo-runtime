// `readPixels`: what this side refuses, what it asks for, and where it puts the
// answer.
//
// The op is split across the boundary in a way no other query is. This side
// owns the destination view -- its kind, its length, the caller's offset into
// it -- and the host owns the framebuffer and the `PACK_*` state that decides
// where the rows land, which this side cannot see at all: the engine's own
// encoder writes `pixelStorei` straight into the command stream this producer
// forwards unread. So the host answers with the layout in front of the rows, and
// what has to be checked here is the half this side decides.
//
// The other half -- that the host's layout is the one its renderer used -- is
// checked where the renderer is: platforms/apple/Tests/MigoApplePerformancePlusTests
// reads back real frames through the C ABI and holds the header to the compact
// layout a read with no pack state must produce.
//
// Run:  node test/read-pixels.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { drainProducerError } from "../src/lane-local.mjs";
import { op_read_pixels } from "../src/lane-sync.mjs";
import {
  READ_PIXELS_LAYOUT_BYTES,
  SYNC_ERROR_OPERATION_FAILED,
  SYNC_OP_READ_PIXELS,
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

const GL_RGBA = 0x1908;
const GL_UNSIGNED_BYTE = 0x1401;
const GL_FLOAT = 0x1406;
const GL_INVALID_ENUM = 0x0500;
const GL_INVALID_VALUE = 0x0501;
const GL_INVALID_OPERATION = 0x0502;

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

// The host, as this test plays it: it records the request and answers with a
// layout of the test's choosing followed by rows of `0xA0 + row`.
let asked = null;
let answer = { firstByte: 0, rowBytes: 0, rowStride: 0, height: 0 };
let failWith = null;
const sync = {
  call(request, into) {
    asked = request;
    if (failWith !== null) {
      const error = failWith;
      failWith = null;
      throw error;
    }
    const pixels = request.maxReplyBytes - READ_PIXELS_LAYOUT_BYTES;
    const reply = new Uint8Array(READ_PIXELS_LAYOUT_BYTES + pixels);
    const view = new DataView(reply.buffer);
    view.setUint32(0, answer.firstByte, true);
    view.setUint32(4, answer.rowBytes, true);
    view.setUint32(8, answer.rowStride, true);
    view.setUint32(12, answer.height, true);
    for (let index = 0; index < pixels; index += 1) {
      reply[READ_PIXELS_LAYOUT_BYTES + index] = 0xa0 + Math.floor(index / Math.max(answer.rowBytes, 1));
    }
    if (into === undefined) return reply;
    into.set(reply);
    return into.subarray(0, reply.length);
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

/** Every error this canvas has recorded since the last call, in order. */
function errorsOf(canvasId) {
  const codes = [];
  for (let code = drainProducerError(canvasId); code !== 0; code = drainProducerError(canvasId)) {
    codes.push(code);
  }
  return codes;
}

/** What the engine's own facade does with the op's answer. */
function place(result, view) {
  const bytes = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
  if (result.rowStride === result.rowBytes) {
    bytes.set(result.data, result.firstByte);
    return;
  }
  for (let row = 0; row < result.height; row += 1) {
    bytes.set(
      result.data.subarray(row * result.rowBytes, (row + 1) * result.rowBytes),
      result.firstByte + row * result.rowStride,
    );
  }
}

// ---- what this side refuses, before anything is asked -----------------------

const view = new Uint8Array(4 * 4 * 4);
asked = null;
check(
  op_read_pixels(1, 0, 0, 1, 1, 0x1234, GL_UNSIGNED_BYTE, view, 0) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_ENUM]) &&
    asked === null,
  "a format the table does not know is INVALID_ENUM and nothing is asked",
);
check(
  op_read_pixels(1, 0, 0, -1, 1, GL_RGBA, GL_UNSIGNED_BYTE, view, 0) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_VALUE]),
  "a negative rectangle is INVALID_VALUE",
);
check(
  op_read_pixels(1, 0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, null, 0) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_VALUE]),
  "no destination at all is INVALID_VALUE",
);
check(
  op_read_pixels(1, 0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, new Uint16Array(8), 0) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_OPERATION]),
  "a view of the wrong kind for the pixel type is INVALID_OPERATION",
);
check(
  op_read_pixels(1, 0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, new Uint8Array(15), 0) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_OPERATION]),
  "a view one byte short of the rectangle is INVALID_OPERATION",
);
check(
  op_read_pixels(1, 0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, new Uint8Array(16), 1) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_OPERATION]),
  "an offset that leaves too little room is INVALID_OPERATION",
);
check(
  op_read_pixels(1, 0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, new Uint8Array(32), -1) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_OPERATION]),
  "a negative offset wraps rather than reading from the start of the view",
);
asked = null;
check(
  op_read_pixels(1, 0, 0, 0, 4, GL_RGBA, GL_UNSIGNED_BYTE, view, 0) === null &&
    errorsOf(1).length === 0 &&
    asked === null,
  "an empty rectangle reads nothing, records nothing and asks nothing",
);
asked = null;
check(
  op_read_pixels(1, 0, 0, 1, 1, GL_RGBA, GL_FLOAT, new Float32Array(4), 0) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_OPERATION]) &&
    asked === null,
  "a pair this lane's readback does not carry is INVALID_OPERATION, as an unoffered pair is",
);

// ---- what it asks for, and what it does with the answer ---------------------

answer = { firstByte: 0, rowBytes: 8, rowStride: 8, height: 2 };
const compact = new Uint8Array(16);
let result = op_read_pixels(1, 3, 4, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, compact, 0);
check(
  asked !== null &&
    asked.operation === SYNC_OP_READ_PIXELS &&
    asked.maxReplyBytes === READ_PIXELS_LAYOUT_BYTES + 16,
  "the reservation is the rows plus the layout in front of them",
);
check(
  asked.params.length === 32 &&
    new DataView(asked.params.buffer).getInt32(4, true) === 3 &&
    new DataView(asked.params.buffer).getInt32(8, true) === 4 &&
    new DataView(asked.params.buffer).getUint32(20, true) === GL_RGBA,
  "the argument record carries the rectangle and the pair the caller named",
);
place(result, compact);
check(
  Array.from(compact.subarray(0, 8)).every((byte) => byte === 0xa0) &&
    Array.from(compact.subarray(8)).every((byte) => byte === 0xa1),
  "a compact layout copies the rows straight in",
);

// A padded stride and a skip, which is what any `pixelStorei(PACK_*)` produces:
// the rows go where the host said, and the gaps are left as they were.
answer = { firstByte: 4, rowBytes: 8, rowStride: 12, height: 2 };
const padded = new Uint8Array(32).fill(0x11);
result = op_read_pixels(1, 0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, padded, 0);
place(result, padded);
check(
  result.firstByte === 4 && result.rowStride === 12 && result.height === 2,
  "the layout the host answered is the one the facade is handed",
);
check(
  padded[0] === 0x11 && padded[3] === 0x11 &&
    Array.from(padded.subarray(4, 12)).every((byte) => byte === 0xa0) &&
    padded[12] === 0x11 && padded[15] === 0x11 &&
    Array.from(padded.subarray(16, 24)).every((byte) => byte === 0xa1) &&
    padded[24] === 0x11,
  "the skip and the row padding are left untouched",
);

// The caller's own offset into the view adds to the host's first byte.
answer = { firstByte: 0, rowBytes: 4, rowStride: 4, height: 1 };
const offset = new Uint8Array(12).fill(0x22);
result = op_read_pixels(1, 0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, offset, 8);
place(result, offset);
check(
  result.firstByte === 8 &&
    offset[7] === 0x22 &&
    Array.from(offset.subarray(8)).every((byte) => byte === 0xa0),
  "the caller's dstOffset is added to where the host put the rows",
);

// A footprint the view cannot hold is only knowable once the layout arrives.
answer = { firstByte: 64, rowBytes: 4, rowStride: 4, height: 1 };
const small = new Uint8Array(8);
check(
  op_read_pixels(1, 0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, small, 0) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_OPERATION]) &&
    Array.from(small).every((byte) => byte === 0),
  "a layout that does not fit the view is INVALID_OPERATION and the view is untouched",
);

// The host tried the read and it failed: `readPixels` does not throw for that.
answer = { firstByte: 0, rowBytes: 4, rowStride: 4, height: 1 };
failWith = new SyncRequestError(SYNC_ERROR_OPERATION_FAILED);
check(
  op_read_pixels(1, 0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, new Uint8Array(4), 0) === null &&
    JSON.stringify(errorsOf(1)) === JSON.stringify([GL_INVALID_OPERATION]),
  "a readback the host could not do is INVALID_OPERATION rather than an exception",
);

// Anything else is not a WebGL outcome and is left to propagate.
failWith = new SyncRequestError(5);
let threw = "nothing";
try {
  op_read_pixels(1, 0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, new Uint8Array(4), 0);
} catch (error) {
  threw = error.name;
}
check(threw === "SyncRequestError", "a timeout or a dead session reaches the caller");

console.log(failures === 0 ? "PASS (readPixels)" : `FAIL (${failures})`);
process.exit(failures === 0 ? 0 : 1);
