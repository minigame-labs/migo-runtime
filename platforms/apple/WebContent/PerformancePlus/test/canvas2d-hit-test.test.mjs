// `isPointInPath` / `isPointInStroke` on the producer: a synchronous question the host answers.
//
// The facade checks its arguments and hands the op a point, flags and a `Path2D`'s segments; the op asks the host,
// which runs the question on the renderer after everything recorded before it. What the parity fixtures cannot reach
// is the asking, because a synchronous call needs a host endpoint -- so this gives it one, and checks what is asked,
// how the one-word answer is read, the requests the in-process op refuses (answered false here without asking), and
// the host that cannot answer.
//
// Run:  node test/canvas2d-hit-test.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_canvas2d_hit_test } from "../src/lane-sync.mjs";
import {
  CANVAS2D_HIT_TEST_EVEN_ODD,
  CANVAS2D_HIT_TEST_HEADER_BYTES,
  CANVAS2D_HIT_TEST_PATH,
  CANVAS2D_HIT_TEST_REPLY_BYTES,
  CANVAS2D_HIT_TEST_STROKE,
  SYNC_ERROR_OPERATION_FAILED,
  SYNC_ERROR_TIMED_OUT,
  SYNC_OP_CANVAS2D_HIT_TEST,
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
let answer = 1;
let failWith = null;
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
  sync: {
    call(request) {
      asked = request;
      if (failWith !== null) {
        const error = failWith;
        failWith = null;
        throw error;
      }
      return Uint8Array.of(answer, 0, 0, 0);
    },
  },
  report() {},
});

/** The hit test's arguments as the host reads them. */
function paramsOf(request) {
  const view = new DataView(request.params.buffer, request.params.byteOffset, request.params.byteLength);
  const count = view.getUint32(16, true);
  const path = [];
  for (let i = 0; i < count; i++) path.push(view.getUint32(CANVAS2D_HIT_TEST_HEADER_BYTES + i * 4, true));
  return {
    bytes: request.params.byteLength,
    canvas: view.getUint32(0, true),
    flags: view.getUint32(4, true),
    x: view.getFloat32(8, true),
    y: view.getFloat32(12, true),
    path,
  };
}

const lineTo = Uint32Array.of(2, 0x41200000, 0x41a00000); // LINE_TO 10 20
const none = new Uint32Array(0);

asked = null;
answer = 1;
check(op_canvas2d_hit_test(3, 0, 1.25, -2.5, none) === true, "the host's 1 is inside");
check(
  asked.operation === SYNC_OP_CANVAS2D_HIT_TEST && asked.maxReplyBytes === CANVAS2D_HIT_TEST_REPLY_BYTES &&
    JSON.stringify(paramsOf(asked)) ===
      JSON.stringify({ bytes: CANVAS2D_HIT_TEST_HEADER_BYTES, canvas: 3, flags: 0, x: 1.25, y: -2.5, path: [] }),
  "the current default path is asked about with no words",
);

asked = null;
answer = 0;
const flags = CANVAS2D_HIT_TEST_PATH | CANVAS2D_HIT_TEST_EVEN_ODD;
check(op_canvas2d_hit_test(3, flags, 4, 5, lineTo) === false, "the host's 0 is outside");
check(
  JSON.stringify(paramsOf(asked).path) === JSON.stringify(Array.from(lineTo)) && paramsOf(asked).flags === flags,
  "a Path2D travels as its segments, with the flag that says so",
);

// The requests the in-process op refuses, answered false without asking.
for (const [args, why] of [
  [[3, 8, 0, 0, none], "a flag this build does not read"],
  [[3, CANVAS2D_HIT_TEST_STROKE | CANVAS2D_HIT_TEST_EVEN_ODD, 0, 0, none], "a stroke has no fill rule"],
  [[3, CANVAS2D_HIT_TEST_PATH, 0, 0, none], "a path announced and absent"],
  [[3, 0, 0, 0, lineTo], "a path the flags do not announce"],
  [[3, 0, 1e39, 0, none], "a point no f32 holds"],
  [[3, 0, 0, NaN, none], "a point that is not one"],
]) {
  asked = null;
  answer = 1;
  check(op_canvas2d_hit_test(...args) === false && asked === null, `${why}: false, unasked`);
}

asked = null;
failWith = new SyncRequestError(SYNC_ERROR_OPERATION_FAILED);
check(
  op_canvas2d_hit_test(3, 0, 0, 0, none) === false,
  "a canvas the host has no 2D context for is outside, as the op's failed command is",
);

failWith = new SyncRequestError(SYNC_ERROR_TIMED_OUT);
let thrown = null;
try {
  op_canvas2d_hit_test(3, 0, 0, 0, none);
} catch (error) {
  thrown = error;
}
check(thrown instanceof SyncRequestError && thrown.code === SYNC_ERROR_TIMED_OUT, "a host that did not answer is not an answer");

console.log(failures === 0 ? "PASS (Canvas2D hit tests)" : `FAIL (${failures})`);
process.exit(failures === 0 ? 0 : 1);
