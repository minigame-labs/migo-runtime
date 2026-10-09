// `getError` on the producer: the errors this side found itself, and the ones the host did.
//
// A call the facade refuses before it is encoded -- a vertex attribute list that is too short, an index past
// MAX_VERTEX_ATTRIBS, an upload above the budget -- is an error this side records
// (`op_webgl_record_error`, `recordProducerError`). The host never sees the call, so its queue has nothing for it, and
// `getError` has to answer from this side's queue: the doc comment on `drainProducerError` says the sync lane's
// `op_webgl_get_error` does it before crossing, and it did not -- every such error was recorded and none was ever
// returned (the conformance suite `webgl-spec/query-bad-index-is-null-and-invalid-value` on an iPhone 12).
//
// This answers each from where it lives: the producer's queue first (one error per call, oldest first, without
// crossing), then the host's.
//
// Run:  node test/get-error.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_webgl_record_error } from "../src/lane-local.mjs";
import { op_webgl_get_error } from "../src/lane-sync.mjs";
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

const INVALID_ENUM = 0x0500;
const INVALID_VALUE = 0x0501;
const INVALID_OPERATION = 0x0502;

const session = new FrameSession({
  send(bytes) {
    session.handleMessage(
      encodeBytes([
        { kind: DOWN_FRAME_VERDICT, generation: 1, decision: 1, wireErrorCode: 0, remainingCredits: 2, acceptedSequence: sequenceOf(bytes) },
      ]),
    );
  },
  sendControl() {},
});

// The host, as this test plays it: a queue of the errors its decoder found, one answered per query.
const hostErrors = [];
let crossings = 0;
const sync = {
  call() {
    crossings += 1;
    const reply = new Uint8Array(4);
    new DataView(reply.buffer).setUint32(0, hostErrors.length > 0 ? hostErrors.shift() : 0, true);
    return reply;
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

check(op_webgl_get_error(1) === 0 && crossings === 1, "with nothing queued anywhere it asks the host, and the answer is NO_ERROR");

op_webgl_record_error(1, INVALID_VALUE);
const before = crossings;
check(op_webgl_get_error(1) === INVALID_VALUE, "an error this side recorded is what getError returns");
check(crossings === before, "and it was answered here, without crossing to the host");
check(op_webgl_get_error(1) === 0 && crossings === before + 1, "once drained, the next call asks the host again");

op_webgl_record_error(1, INVALID_ENUM);
op_webgl_record_error(1, INVALID_OPERATION);
check(
  [op_webgl_get_error(1), op_webgl_get_error(1)].join() === [INVALID_ENUM, INVALID_OPERATION].join(),
  "several are returned one per call, oldest first",
);

// GL's errors are flags: a code raised again before it is read is held once.
op_webgl_record_error(1, INVALID_ENUM);
op_webgl_record_error(1, INVALID_VALUE);
op_webgl_record_error(1, INVALID_ENUM);
check(
  [op_webgl_get_error(1), op_webgl_get_error(1)].join() === [INVALID_ENUM, INVALID_VALUE].join(),
  "a code raised twice before it is read is held once",
);
check(op_webgl_get_error(1) === 0, "and nothing is left behind");

// The facade's own judgement of a framebuffer is one of WebGL's errors, kept as it is rather than read as another.
op_webgl_record_error(1, 0x0506);
op_webgl_record_error(1, 0xdead);
check(
  [op_webgl_get_error(1), op_webgl_get_error(1)].join() === [0x0506, INVALID_OPERATION].join(),
  "INVALID_FRAMEBUFFER_OPERATION is recorded as itself, and a code that is not WebGL's as INVALID_OPERATION",
);

// This side's queue is per canvas, as the host's is.
op_webgl_record_error(2, INVALID_ENUM);
check(op_webgl_get_error(1) === 0, "another canvas's error is not this canvas's");
check(op_webgl_get_error(2) === INVALID_ENUM, "it is returned for its own");

// An error the host found is still returned once this side has none, and this side's come first.
hostErrors.push(INVALID_OPERATION);
op_webgl_record_error(1, INVALID_VALUE);
check(
  [op_webgl_get_error(1), op_webgl_get_error(1), op_webgl_get_error(1)].join() === [INVALID_VALUE, INVALID_OPERATION, 0].join(),
  "the producer's own error first, then the host's, then NO_ERROR",
);

// Only the four codes content may record; anything else is INVALID_OPERATION (op_webgl_record_error's rule).
op_webgl_record_error(1, 0x9999);
check(op_webgl_get_error(1) === INVALID_OPERATION, "a code content may not record is INVALID_OPERATION");

console.log(failures === 0 ? "PASS (getError)" : `FAIL (${failures})`);
process.exit(failures === 0 ? 0 : 1);
