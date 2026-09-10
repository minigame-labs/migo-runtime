// The producer's half of the synchronous barrier, against the document.
//
// The layout half is checked against contracts/frame-wire/wire-v1.md rather
// than against the Rust, for the reason the golden corpus exists: two
// implementations that agree with each other and not with the specification is
// the failure the specification is there to catch. The Rust side checks itself
// against the same table (frame-wire's `wire_document_agreement`), so neither
// encoder is the other's reference.
//
// The behaviour half is checked with a real `Atomics.wait` woken from a real
// worker, because "it blocks" is the entire claim. A test whose host answered
// before the wait began would exercise every line except the one that matters.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/sync-mailbox.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads";

import {
  SyncMailbox,
  SyncRequestError,
  SYNC_RECORD_BYTES,
  SYNC_STATE_FREE,
  SYNC_STATE_PENDING,
  SYNC_STATE_READY,
  SYNC_STATE_FAILED,
  SYNC_STATE_CANCELLED,
  SYNC_ERROR_ALREADY_PENDING,
  SYNC_ERROR_BAD_DEADLINE,
  SYNC_ERROR_BAD_REPLY_RESERVATION,
  SYNC_ERROR_TIMED_OUT,
  SYNC_ERROR_UNSUPPORTED_OPERATION,
  SYNC_OP_READ_PIXELS,
  MAX_REPLY_BYTES,
  OFF_STATE,
  OFF_REQUEST_ID,
  OFF_RUNTIME_GENERATION,
  OFF_SURFACE_GENERATION,
  OFF_RESOURCE_EPOCH,
  OFF_TRIGGERING_SEQUENCE,
  OFF_OPERATION,
  OFF_MAX_REPLY_BYTES,
  OFF_REPLY_BYTES,
  OFF_ERROR,
  OFF_DEADLINE_NANOS,
  READ_PIXELS_PARAM_BYTES,
  GL_RGBA,
  GL_UNSIGNED_BYTE,
  encodeReadPixelsParams,
  readPixelsReplyBytes,
} from "../src/sync-mailbox.mjs";

// ---------------------------------------------------------------------------
// The worker half: settle the record the main thread is blocked on.
// ---------------------------------------------------------------------------

if (!isMainThread) {
  const words = new Int32Array(workerData.buffer, 0, SYNC_RECORD_BYTES / 4);
  const view = new DataView(workerData.buffer, 0, SYNC_RECORD_BYTES);
  // Wait for the producer to publish PENDING, exactly as a relay would.
  for (let spin = 0; spin < 20000; spin += 1) {
    if (Atomics.load(words, OFF_STATE / 4) === SYNC_STATE_PENDING) break;
    Atomics.wait(words, OFF_STATE / 4, SYNC_STATE_FREE, 1);
  }
  view.setUint32(OFF_REQUEST_ID, workerData.requestId, true);
  if (workerData.settle === "ready") {
    view.setUint32(OFF_REPLY_BYTES, workerData.replyBytes, true);
    Atomics.store(words, OFF_STATE / 4, SYNC_STATE_READY);
  } else {
    view.setUint32(OFF_ERROR, workerData.error, true);
    Atomics.store(words, OFF_STATE / 4, SYNC_STATE_FAILED);
  }
  Atomics.notify(words, OFF_STATE / 4);
  parentPort.postMessage("settled");
  process.exit(0);
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

let failures = 0;
function check(name, fn) {
  try {
    fn();
    console.log(`  ok   ${name}`);
  } catch (error) {
    failures += 1;
    console.log(`  FAIL ${name}`);
    console.log(`       ${error && error.message}`);
  }
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function assertEqual(actual, expected, message) {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${expected}, got ${actual}`);
  }
}

function mailbox(channel) {
  return new SyncMailbox(new SharedArrayBuffer(SYNC_RECORD_BYTES), channel);
}

const NOW = 1_000_000_000n;
const DEADLINE = NOW + 200_000_000n;

function baseRequest(overrides = {}) {
  return {
    operation: SYNC_OP_READ_PIXELS,
    params: encodeReadPixelsParams({ canvasId: 1, x: 0, y: 0, width: 2, height: 2 }),
    maxReplyBytes: 4096,
    runtimeGeneration: 1n,
    surfaceGeneration: 1n,
    resourceEpoch: 1n,
    triggeringSequence: 7n,
    deadlineNanos: DEADLINE,
    nowNanos: NOW,
    ...overrides,
  };
}

/** A relay that answers inside `post`, so the wait finds the cell settled. */
function answeringChannel(record, settle) {
  const words = new Int32Array(record, 0, SYNC_RECORD_BYTES / 4);
  const view = new DataView(record, 0, SYNC_RECORD_BYTES);
  return {
    posted: null,
    reply: null,
    withdrawn: false,
    post(params) {
      this.posted = params;
      settle(words, view, this);
    },
    takeReply(bytes, into) {
      this.reply = bytes;
      this.into = into;
      if (into) {
        // What a real channel does: one copy, straight out of the shared
        // buffer into the view the caller already had.
        into.set(new Uint8Array(bytes), 0);
        return bytes;
      }
      return new Uint8Array(bytes);
    },
    withdraw() {
      this.withdrawn = true;
    },
  };
}

console.log("The producer's synchronous mailbox");

// ---------------------------------------------------------------------------
// Layout, against the document
// ---------------------------------------------------------------------------

const here = dirname(fileURLToPath(import.meta.url));
const document = readFileSync(
  join(here, "..", "..", "..", "..", "..", "contracts", "frame-wire", "wire-v1.md"),
  "utf8",
);

check("the record's fields sit where contracts/frame-wire/wire-v1.md puts them", () => {
  const section = document.slice(document.indexOf("### Record — 64 bytes, fixed"));
  const table = section.slice(0, section.indexOf("### Every way"));
  const rows = [];
  for (const line of table.split("\n")) {
    const match = /^\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*`(\w+)`/.exec(line.trim());
    if (match) rows.push({ offset: Number(match[1]), size: Number(match[2]), name: match[3] });
  }
  // A parse that matches nothing reports nothing, so say so rather than pass.
  assert(rows.length > 0, "no rows parsed out of the document's record table");

  const declared = {
    state: OFF_STATE,
    request_id: OFF_REQUEST_ID,
    runtime_generation: OFF_RUNTIME_GENERATION,
    surface_generation: OFF_SURFACE_GENERATION,
    resource_epoch: OFF_RESOURCE_EPOCH,
    triggering_sequence: OFF_TRIGGERING_SEQUENCE,
    operation: OFF_OPERATION,
    max_reply_bytes: OFF_MAX_REPLY_BYTES,
    reply_bytes: OFF_REPLY_BYTES,
    error: OFF_ERROR,
    deadline_nanos: OFF_DEADLINE_NANOS,
  };
  assertEqual(rows.length, Object.keys(declared).length, "field count");

  let end = 0;
  for (const row of rows) {
    assert(row.name in declared, `the document has a field this encoder does not: ${row.name}`);
    assertEqual(declared[row.name], row.offset, `offset of ${row.name}`);
    // Gapless, checked as we walk: a record with a hole in it is a record whose
    // two readers can disagree about where the next field starts.
    assertEqual(row.offset, end, `${row.name} does not start where the previous field ended`);
    end = row.offset + row.size;
  }
  assertEqual(end, SYNC_RECORD_BYTES, "the fields do not fill the record");
});

check("the reply ceiling and the in-flight rule match the document", () => {
  assert(
    document.includes("16777216"),
    "the document no longer states the reply ceiling this encoder enforces",
  );
  assertEqual(MAX_REPLY_BYTES, 16777216, "reply ceiling");
  assert(
    document.includes("One request may be outstanding per session"),
    "the document no longer states the one-in-flight rule",
  );
});

// ---------------------------------------------------------------------------
// The arguments record
// ---------------------------------------------------------------------------

check("readPixels' arguments encode as eight little-endian words", () => {
  const bytes = encodeReadPixelsParams({ canvasId: 3, x: 5, y: 7, width: 11, height: 13 });
  assertEqual(bytes.byteLength, READ_PIXELS_PARAM_BYTES, "params length");
  const view = new DataView(bytes.buffer);
  assertEqual(view.getUint32(0, true), 3, "canvas id");
  assertEqual(view.getInt32(4, true), 5, "x");
  assertEqual(view.getInt32(8, true), 7, "y");
  assertEqual(view.getInt32(12, true), 11, "width");
  assertEqual(view.getInt32(16, true), 13, "height");
  assertEqual(view.getUint32(20, true), GL_RGBA, "format");
  assertEqual(view.getUint32(24, true), GL_UNSIGNED_BYTE, "type");
  assertEqual(view.getUint32(28, true), 0, "reserved");
  assertEqual(readPixelsReplyBytes(11, 13), 11 * 13 * 4, "reply size");
});

// ---------------------------------------------------------------------------
// Refusals that never reach the host
// ---------------------------------------------------------------------------

check("a second request while one is outstanding is refused here", () => {
  const box = mailbox({ post() {}, takeReply: () => new Uint8Array(0) });
  Atomics.store(box.words, OFF_STATE / 4, SYNC_STATE_PENDING);
  let thrown = null;
  try {
    box.request(baseRequest());
  } catch (error) {
    thrown = error;
  }
  assert(thrown instanceof SyncRequestError, "expected a SyncRequestError");
  assertEqual(thrown.code, SYNC_ERROR_ALREADY_PENDING, "code");
});

check("a reservation outside the protocol's bounds is refused here", () => {
  for (const maxReplyBytes of [0, MAX_REPLY_BYTES + 1]) {
    const box = mailbox({ post() {}, takeReply: () => new Uint8Array(0) });
    let thrown = null;
    try {
      box.request(baseRequest({ maxReplyBytes }));
    } catch (error) {
      thrown = error;
    }
    assertEqual(thrown && thrown.code, SYNC_ERROR_BAD_REPLY_RESERVATION, "code");
    assertEqual(box.state, SYNC_STATE_FREE, "the slot must not be occupied by a refused request");
  }
});
check("a deadline that is not in the future is refused here", () => {
  const box = mailbox({ post() {}, takeReply: () => new Uint8Array(0) });
  let thrown = null;
  try {
    box.request(baseRequest({ deadlineNanos: NOW }));
  } catch (error) {
    thrown = error;
  }
  assertEqual(thrown && thrown.code, SYNC_ERROR_BAD_DEADLINE, "code");
  assertEqual(box.state, SYNC_STATE_FREE, "slot");
});

// ---------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------

check("an answered request returns the bytes and frees the slot", () => {
  const record = new SharedArrayBuffer(SYNC_RECORD_BYTES);
  const channel = answeringChannel(record, (words, view) => {
    view.setUint32(OFF_REQUEST_ID, 1, true);
    view.setUint32(OFF_REPLY_BYTES, 16, true);
    Atomics.store(words, OFF_STATE / 4, SYNC_STATE_READY);
  });
  const box = new SyncMailbox(record, channel);
  const reply = box.request(baseRequest());
  assertEqual(reply.byteLength, 16, "reply length");
  assertEqual(channel.posted.byteLength, READ_PIXELS_PARAM_BYTES, "the params were posted");
  assertEqual(box.state, SYNC_STATE_FREE, "the slot is reusable");
});

check("a failed request throws the host's reason rather than returning bytes", () => {
  const record = new SharedArrayBuffer(SYNC_RECORD_BYTES);
  const channel = answeringChannel(record, (words, view) => {
    view.setUint32(OFF_REQUEST_ID, 1, true);
    view.setUint32(OFF_ERROR, SYNC_ERROR_UNSUPPORTED_OPERATION, true);
    Atomics.store(words, OFF_STATE / 4, SYNC_STATE_FAILED);
  });
  const box = new SyncMailbox(record, channel);
  let thrown = null;
  try {
    box.request(baseRequest());
  } catch (error) {
    thrown = error;
  }
  assert(thrown instanceof SyncRequestError, "expected a SyncRequestError");
  assertEqual(thrown.code, SYNC_ERROR_UNSUPPORTED_OPERATION, "code");
  assertEqual(channel.reply, null, "no reply may be taken for a failed request");
  assertEqual(box.state, SYNC_STATE_FREE, "the slot is reusable");
});

check("a withdrawn request throws without inventing an error code", () => {
  const record = new SharedArrayBuffer(SYNC_RECORD_BYTES);
  const channel = answeringChannel(record, (words) => {
    Atomics.store(words, OFF_STATE / 4, SYNC_STATE_CANCELLED);
  });
  const box = new SyncMailbox(record, channel);
  let thrown = null;
  try {
    box.request(baseRequest());
  } catch (error) {
    thrown = error;
  }
  // Nothing went wrong, so there is no error code to report -- the document is
  // explicit that CANCELLED carries none.
  assertEqual(thrown && thrown.code, 0, "code");
});

check("a deadline that passes withdraws the request rather than abandoning it", () => {
  const record = new SharedArrayBuffer(SYNC_RECORD_BYTES);
  // A host that never answers. The wait is bounded by the producer's own
  // deadline, so this returns rather than hanging the test.
  const channel = answeringChannel(record, () => {});
  const box = new SyncMailbox(record, channel);
  let thrown = null;
  try {
    box.request(baseRequest({ deadlineNanos: NOW + 30_000_000n }));
  } catch (error) {
    thrown = error;
  }
  assertEqual(thrown && thrown.code, SYNC_ERROR_TIMED_OUT, "code");
  assert(channel.withdrawn, "the relay must be told, or a late reply lands on the next request");
  assertEqual(box.state, SYNC_STATE_FREE, "the slot is reusable");
});

check("an answer is written straight into the caller's destination when it has one", () => {
  // `readPixels` writes into a view its caller already allocated. Without a
  // destination the answer is copied twice more than it needs to be -- out of
  // the shared buffer into a fresh array, and out of that into the view -- and
  // a full-screen readback on a phone at 4x is about 14 MiB with the producer
  // blocked for every byte.
  const record = new SharedArrayBuffer(SYNC_RECORD_BYTES);
  const channel = answeringChannel(record, (words, view) => {
    view.setUint32(OFF_REQUEST_ID, 1, true);
    view.setUint32(OFF_REPLY_BYTES, 16, true);
    Atomics.store(words, OFF_STATE / 4, SYNC_STATE_READY);
  });
  const box = new SyncMailbox(record, channel);
  const destination = new Uint8Array(64).fill(0xee);
  const written = box.request(baseRequest({ into: destination }));
  assertEqual(written, 16, "the byte count is what a destination-taking channel returns");
  assertEqual(channel.into, destination, "the destination reached the channel");
  assertEqual(destination[0], 0, "and the answer was written into it");
  assertEqual(destination[16], 0xee, "without touching a byte past the answer");
  assertEqual(box.state, SYNC_STATE_FREE, "the slot is reusable");
});

// ---------------------------------------------------------------------------
// It actually blocks
// ---------------------------------------------------------------------------

const record = new SharedArrayBuffer(SYNC_RECORD_BYTES);
const started = Date.now();
const worker = new Worker(fileURLToPath(import.meta.url), {
  workerData: { buffer: record, settle: "ready", requestId: 1, replyBytes: 16, error: 0 },
});
const box = new SyncMailbox(record, {
  post() {},
  takeReply: (bytes) => new Uint8Array(bytes),
});
let blockingReply = null;
let blockingError = null;
try {
  blockingReply = box.request(baseRequest({ deadlineNanos: NOW + 5_000_000_000n }));
} catch (error) {
  blockingError = error;
}
const elapsed = Date.now() - started;
await worker.terminate();

check("a real Atomics.wait is woken by a real settle from another agent", () => {
  assert(blockingError === null, `the blocked request failed: ${blockingError?.message}`);
  assertEqual(blockingReply.byteLength, 16, "reply length");
  // Not a timing assertion, a liveness one: five seconds was the budget, so
  // anything near it means the wake did not happen and the deadline did.
  assert(elapsed < 4000, `the wait ran for ${elapsed} ms, which is the deadline, not a wake`);
});

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
