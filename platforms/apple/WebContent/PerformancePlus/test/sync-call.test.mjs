// The one-body synchronous call, against the document and against a host that
// misbehaves.
//
// The layout half reads contracts/frame-wire/wire-v1.md and places a distinct
// value in every field, then reads each back at the offset the DOCUMENT gives:
// an encoder checked against its own constants agrees with itself whatever it
// writes. The Rust half checks itself against the same tables, and
// `emit-sync-calls.mjs` puts bytes through both.
//
// The behaviour half stands in for the blocking request, because what this
// module decides is what to make of a response -- and the responses worth
// testing are the ones a correct host never sends.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/sync-call.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  SYNC_ANSWER_HEADER_BYTES,
  SYNC_CALL_HEADER_BYTES,
  SYNC_CALL_MAX_BYTES,
  SYNC_CALL_MAX_TIMEOUT_MILLIS,
  SYNC_CALL_TRANSPORT_GRACE_MILLIS,
  SyncCaller,
  SyncTransportError,
  decodeSyncAnswer,
  encodeSyncCall,
} from "../src/sync-call.mjs";
import {
  MAX_REPLY_BYTES,
  SYNC_ERROR_BAD_DEADLINE,
  SYNC_ERROR_BAD_REPLY_RESERVATION,
  SYNC_ERROR_TIMED_OUT,
  SYNC_ERROR_UNSUPPORTED_OPERATION,
  SYNC_OP_READ_PIXELS,
  SYNC_STATE_CANCELLED,
  SYNC_STATE_FAILED,
  SYNC_STATE_PENDING,
  SYNC_STATE_READY,
  SyncRequestError,
} from "../src/sync-mailbox.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const document = readFileSync(
  join(here, "..", "..", "..", "..", "..", "contracts", "frame-wire", "wire-v1.md"),
  "utf8",
);

/** The first table after `heading`, as {offset, size, name} rows. */
function table(heading) {
  const start = document.indexOf(heading);
  assert.ok(start >= 0, `the document has no ${heading} heading`);
  const rows = [];
  let seen = false;
  for (const line of document.slice(start).split("\n").slice(1)) {
    const match = /^\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*`(\w+)`/.exec(line.trim());
    if (match) {
      rows.push({ offset: Number(match[1]), size: Number(match[2]), name: match[3] });
      seen = true;
    } else if (seen && !line.trim().startsWith("|")) {
      break;
    }
  }
  // A parse that matches nothing reports nothing, so say so rather than pass.
  assert.ok(rows.length > 0, `no rows parsed under ${heading}`);
  return rows;
}

const CALL = {
  runtimeGeneration: 0x0102_0304_0506_0708n,
  surfaceGeneration: 0x1112_1314_1516_1718n,
  resourceEpoch: 0x2122_2324_2526_2728n,
  // Past 2^53: a Number encoder gets this one wrong and every smaller one right.
  triggeringSequence: 0xf1f2_f3f4_f5f6_f7f8n,
  operation: SYNC_OP_READ_PIXELS,
  maxReplyBytes: 0x0a0b0c,
  timeoutMillis: 45_678,
};

test("every call field sits where the document puts it", () => {
  const params = new Uint8Array([9, 8, 7, 6]);
  const body = encodeSyncCall({ ...CALL, params });
  const view = new DataView(body.buffer);
  const expected = {
    runtime_generation: CALL.runtimeGeneration,
    surface_generation: CALL.surfaceGeneration,
    resource_epoch: CALL.resourceEpoch,
    triggering_sequence: CALL.triggeringSequence,
    operation: CALL.operation,
    max_reply_bytes: CALL.maxReplyBytes,
    timeout_millis: CALL.timeoutMillis,
    reserved: 0,
  };
  const rows = table("### A request as one body");
  assert.deepEqual(
    rows.map((row) => row.name),
    Object.keys(expected),
    "the document names different fields than this test places",
  );
  for (const { offset, size, name } of rows) {
    const read = size === 8 ? view.getBigUint64(offset, true) : view.getUint32(offset, true);
    assert.equal(read, expected[name], `${name} at ${offset}`);
  }
  const last = rows[rows.length - 1];
  assert.equal(last.offset + last.size, SYNC_CALL_HEADER_BYTES);
  assert.deepEqual([...body.subarray(SYNC_CALL_HEADER_BYTES)], [...params], "arguments follow the header");
});

test("the answer header this module reads is the document's", () => {
  const rows = table("### An answer as one body");
  assert.deepEqual(
    rows.map((row) => row.name),
    ["state", "error", "request_id", "reply_bytes"],
  );
  const last = rows[rows.length - 1];
  assert.equal(last.offset + last.size, SYNC_ANSWER_HEADER_BYTES);

  const reply = [1, 2, 3, 4, 5, 6, 7, 8];
  const buffer = new ArrayBuffer(SYNC_ANSWER_HEADER_BYTES + reply.length);
  const view = new DataView(buffer);
  const at = Object.fromEntries(rows.map((row) => [row.name, row.offset]));
  view.setUint32(at.state, SYNC_STATE_READY, true);
  view.setUint32(at.error, 0, true);
  view.setUint32(at.request_id, 0xdeadbeef, true);
  view.setUint32(at.reply_bytes, reply.length, true);
  new Uint8Array(buffer, SYNC_ANSWER_HEADER_BYTES).set(reply);

  const answer = decodeSyncAnswer(buffer);
  assert.equal(answer.state, SYNC_STATE_READY);
  assert.equal(answer.requestId, 0xdeadbeef);
  assert.deepEqual([...answer.reply], reply);
  assert.equal(answer.reply.buffer, buffer, "the reply is a view, not a copy");
});

test("the bounds this module refuses at are the document's", () => {
  const prose = document.split(/\s+/).join(" ");
  assert.ok(prose.includes(`at most ${SYNC_CALL_MAX_BYTES} bytes`));
  assert.ok(prose.includes(`\`1..=${SYNC_CALL_MAX_TIMEOUT_MILLIS}\``));
  assert.ok(prose.includes(`\`1..=${MAX_REPLY_BYTES}\``));
});

test("a call the host would refuse is refused here, with the host's code", () => {
  const refusals = [
    [{ maxReplyBytes: 0 }, SYNC_ERROR_BAD_REPLY_RESERVATION],
    [{ maxReplyBytes: MAX_REPLY_BYTES + 1 }, SYNC_ERROR_BAD_REPLY_RESERVATION],
    [{ timeoutMillis: 0 }, SYNC_ERROR_BAD_DEADLINE],
    [{ timeoutMillis: SYNC_CALL_MAX_TIMEOUT_MILLIS + 1 }, SYNC_ERROR_BAD_DEADLINE],
    [{ timeoutMillis: 1.5 }, SYNC_ERROR_BAD_DEADLINE],
    [
      { params: new Uint8Array(SYNC_CALL_MAX_BYTES - SYNC_CALL_HEADER_BYTES + 1) },
      SYNC_ERROR_UNSUPPORTED_OPERATION,
    ],
  ];
  for (const [change, code] of refusals) {
    const caller = new SyncCaller({
      url: "migo://x/__migo/sync",
      post: () => assert.fail("a call refused locally must not be sent"),
    });
    assert.throws(
      () => caller.call({ ...CALL, ...change }),
      (error) => error instanceof SyncRequestError && error.code === code,
      JSON.stringify(change, (_, v) => (typeof v === "bigint" ? String(v) : v)),
    );
  }
  // The bounds are inclusive.
  encodeSyncCall({ ...CALL, maxReplyBytes: MAX_REPLY_BYTES, timeoutMillis: SYNC_CALL_MAX_TIMEOUT_MILLIS });
  encodeSyncCall({ ...CALL, params: new Uint8Array(SYNC_CALL_MAX_BYTES - SYNC_CALL_HEADER_BYTES) });
});

/** An answer body, written the way the document says. */
function answerBody(state, error, requestId, reply = []) {
  const buffer = new ArrayBuffer(SYNC_ANSWER_HEADER_BYTES + reply.length);
  const view = new DataView(buffer);
  view.setUint32(0, state, true);
  view.setUint32(4, error, true);
  view.setUint32(8, requestId, true);
  view.setUint32(12, reply.length, true);
  new Uint8Array(buffer, SYNC_ANSWER_HEADER_BYTES).set(reply);
  return buffer;
}

function callerAnswering(status, response, seen = []) {
  return new SyncCaller({
    url: "migo://x/__migo/sync",
    post: (url, body, timeoutMillis) => {
      seen.push({ url, body, timeoutMillis });
      return { status, response };
    },
  });
}

test("a ready answer is the reply, written into the caller's view when given one", () => {
  const seen = [];
  const caller = callerAnswering(200, answerBody(SYNC_STATE_READY, 0, 7, [1, 2, 3, 4]), seen);
  const into = new Uint8Array(16).fill(0xee);
  const reply = caller.call({ ...CALL, maxReplyBytes: 4 }, into);
  assert.equal(reply.buffer, into.buffer, "written into the caller's view");
  assert.deepEqual([...reply], [1, 2, 3, 4]);
  assert.equal(into[4], 0xee, "nothing past the reply was touched");

  assert.equal(seen.length, 1);
  assert.equal(seen[0].url, "migo://x/__migo/sync");
  assert.deepEqual(seen[0].body, encodeSyncCall({ ...CALL, maxReplyBytes: 4 }));
  assert.equal(
    seen[0].timeoutMillis,
    CALL.timeoutMillis + SYNC_CALL_TRANSPORT_GRACE_MILLIS,
    "the request outlives the host's own deadline, so the host's verdict is what arrives",
  );

  const view = callerAnswering(200, answerBody(SYNC_STATE_READY, 0, 7, [5, 6])).call({
    ...CALL,
    maxReplyBytes: 2,
  });
  assert.deepEqual([...view], [5, 6]);
});

test("a failed answer throws the host's code, and a cancelled one says it was withdrawn", () => {
  assert.throws(
    () => callerAnswering(200, answerBody(SYNC_STATE_FAILED, SYNC_ERROR_TIMED_OUT, 3)).call(CALL),
    (error) => error instanceof SyncRequestError && error.code === SYNC_ERROR_TIMED_OUT,
  );
  assert.throws(
    () => callerAnswering(200, answerBody(SYNC_STATE_CANCELLED, 0, 3)).call(CALL),
    (error) => error instanceof SyncRequestError && /withdrawn/.test(error.message),
  );
});

test("a response that is not an answer is a transport failure, never pixels", () => {
  const broken = [
    [500, answerBody(SYNC_STATE_READY, 0, 1, [1, 2, 3, 4]), /answered 500/],
    [200, new ArrayBuffer(SYNC_ANSWER_HEADER_BYTES - 1), /shorter than an answer header/],
    [200, "not bytes", /not an ArrayBuffer/],
    // A state no answer may carry.
    [200, answerBody(SYNC_STATE_PENDING, 0, 1), /not a settled one/],
    // Names four bytes and carries three.
    [200, answerBody(SYNC_STATE_READY, 0, 1, [1, 2, 3, 4]).slice(0, SYNC_ANSWER_HEADER_BYTES + 3), /names 4 reply bytes/],
    // A failure with pixels in it, and a failure without a reason.
    [200, (() => {
      const buffer = answerBody(SYNC_STATE_READY, 0, 1, [1]);
      new DataView(buffer).setUint32(0, SYNC_STATE_FAILED, true);
      new DataView(buffer).setUint32(4, SYNC_ERROR_TIMED_OUT, true);
      return buffer;
    })(), /carries 1 reply bytes/],
    [200, answerBody(SYNC_STATE_FAILED, 0, 1), /carries error 0/],
    // More than the call reserved: the caller's buffer was never sized for it.
    [200, answerBody(SYNC_STATE_READY, 0, 1, [1, 2, 3, 4, 5]), /reservation of 4/],
  ];
  for (const [status, response, message] of broken) {
    assert.throws(
      () => callerAnswering(status, response).call({ ...CALL, maxReplyBytes: 4 }),
      (error) => error instanceof SyncTransportError && message.test(error.message),
      String(message),
    );
  }
});

test("a destination too small for the answer is refused rather than truncated", () => {
  const caller = callerAnswering(200, answerBody(SYNC_STATE_READY, 0, 1, [1, 2, 3, 4]));
  assert.throws(() => caller.call({ ...CALL, maxReplyBytes: 4 }, new Uint8Array(3)), RangeError);
});
