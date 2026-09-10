// The page's half of the barrier, and the two halves against each other.
//
// The mailbox test checks the producer against the document. This checks the
// relay against the producer -- with a real Worker blocked in `Atomics.wait`
// and a real relay answering it -- because the property that matters is not
// "each half is correct alone" but "a blocked agent is always woken, and never
// with the wrong bytes".
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/sync-relay.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { fileURLToPath } from "node:url";
import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads";

import {
  SyncMailbox,
  SyncRequestError,
  SYNC_RECORD_BYTES,
  SYNC_STATE_FREE,
  SYNC_ERROR_REPLY_TOO_LARGE,
  SYNC_ERROR_SESSION_ENDED,
  SYNC_ERROR_TIMED_OUT,
  SYNC_OP_READ_PIXELS,
  OFF_STATE,
  OFF_ERROR,
  encodeReadPixelsParams,
} from "../src/sync-mailbox.mjs";
import { SyncRelay } from "../src/sync-relay.mjs";

const NOW = 1_000_000_000n;

// ---------------------------------------------------------------------------
// The producer, in its own agent, exactly as it runs in WebContent.
// ---------------------------------------------------------------------------

if (!isMainThread) {
  const { record, reply } = workerData;
  const replyBytes = new Uint8Array(reply);
  const box = new SyncMailbox(record, {
    post(params) {
      parentPort.postMessage({ params }, [params.buffer.slice(0)]);
      // postMessage of a plain copy: the params are small and the Worker keeps
      // its own. Nothing here may block -- the block happens in Atomics.wait.
    },
    takeReply(bytes) {
      return replyBytes.slice(0, bytes);
    },
    withdraw() {
      parentPort.postMessage({ withdrawn: true });
    },
  });
  let result;
  try {
    const answer = box.request({
      operation: SYNC_OP_READ_PIXELS,
      params: encodeReadPixelsParams({ canvasId: 1, x: 0, y: 0, width: 2, height: 2 }),
      maxReplyBytes: workerData.maxReplyBytes,
      runtimeGeneration: 1n,
      surfaceGeneration: 1n,
      resourceEpoch: 1n,
      triggeringSequence: 3n,
      deadlineNanos: NOW + BigInt(workerData.budgetMillis) * 1_000_000n,
      nowNanos: NOW,
    });
    result = { ok: true, bytes: Array.from(answer) };
  } catch (error) {
    result = {
      ok: false,
      code: error instanceof SyncRequestError ? error.code : -1,
      message: error.message,
    };
  }
  parentPort.postMessage({ result });
  process.exit(0);
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

let failures = 0;
function assertEqual(actual, expected, message) {
  if (actual !== expected) throw new Error(`${message}: expected ${expected}, got ${actual}`);
}

/**
 * Run one producer/relay exchange end to end.
 *
 * The producer really blocks and the relay really answers, so what is measured
 * is the pair rather than either side's idea of the other.
 */
async function exchange({ transport, maxReplyBytes = 4096, replyCapacity = 4096, budgetMillis = 5000 }) {
  const record = new SharedArrayBuffer(SYNC_RECORD_BYTES);
  const reply = new SharedArrayBuffer(replyCapacity);
  const relay = new SyncRelay(record, reply, transport);
  const worker = new Worker(fileURLToPath(import.meta.url), {
    workerData: { record, reply, maxReplyBytes, budgetMillis },
  });
  const events = [];
  const outcome = await new Promise((resolve, reject) => {
    worker.on("message", (message) => {
      events.push(message);
      if (message.params) relay.serve(message.params).catch(reject);
      if (message.result) resolve(message.result);
    });
    worker.on("error", reject);
  });
  await worker.terminate();
  return { outcome, events, record, relay, reply };
}

async function check(name, fn) {
  try {
    await fn();
    console.log(`  ok   ${name}`);
  } catch (error) {
    failures += 1;
    console.log(`  FAIL ${name}`);
    console.log(`       ${error && error.message}`);
  }
}

console.log("The producer and the relay, against each other");

await check("a blocked producer gets exactly the bytes the host answered with", async () => {
  const answer = Uint8Array.from({ length: 16 }, (_, index) => index * 3);
  const { outcome } = await exchange({
    transport: { request: async () => ({ ok: true, bytes: answer, requestId: 1 }) },
  });
  assertEqual(outcome.ok, true, `the request failed: ${outcome.message}`);
  assertEqual(outcome.bytes.length, 16, "reply length");
  assertEqual(outcome.bytes.join(","), Array.from(answer).join(","), "reply bytes");
});

await check("a host that refuses wakes the producer with the host's own reason", async () => {
  const { outcome } = await exchange({
    transport: { request: async () => ({ ok: false, error: 7, requestId: 1 }) },
  });
  assertEqual(outcome.ok, false, "the request must not report success");
  assertEqual(outcome.code, 7, "code");
});

await check("a transport that throws wakes the producer rather than stranding it", async () => {
  const { outcome } = await exchange({
    transport: {
      request: async () => {
        throw new Error("the socket closed");
      },
    },
  });
  assertEqual(outcome.ok, false, "the request must not report success");
  assertEqual(outcome.code, SYNC_ERROR_SESSION_ENDED, "code");
});

await check("a reply larger than the producer reserved is refused, not truncated", async () => {
  // 4 KiB reserved, 8 KiB answered. Truncating would hand the producer a buffer
  // that looks like a correct readPixels and is not.
  const { outcome } = await exchange({
    maxReplyBytes: 4096,
    replyCapacity: 16384,
    transport: { request: async () => ({ ok: true, bytes: new Uint8Array(8192), requestId: 1 }) },
  });
  assertEqual(outcome.ok, false, "the request must not report success");
  assertEqual(outcome.code, SYNC_ERROR_REPLY_TOO_LARGE, "code");
});

await check("a reply larger than the shared buffer is refused before it is written", async () => {
  // The producer reserved more than the buffer can hold. Writing would run past
  // the end of a SharedArrayBuffer the producer is about to read.
  const { outcome } = await exchange({
    maxReplyBytes: 16384,
    replyCapacity: 4096,
    transport: { request: async () => ({ ok: true, bytes: new Uint8Array(8192), requestId: 1 }) },
  });
  assertEqual(outcome.ok, false, "the request must not report success");
  assertEqual(outcome.code, SYNC_ERROR_REPLY_TOO_LARGE, "code");
});

await check("a host that never answers lets the producer's own deadline end the wait", async () => {
  const { outcome, events } = await exchange({
    budgetMillis: 250,
    transport: { request: () => new Promise(() => {}) },
  });
  assertEqual(outcome.ok, false, "the request must not report success");
  assertEqual(outcome.code, SYNC_ERROR_TIMED_OUT, "code");
  // And the relay is told, or a reply that arrives afterwards lands on whatever
  // the next request put in the slot.
  assertEqual(
    events.some((event) => event.withdrawn === true),
    true,
    "the producer must withdraw a request it stopped waiting for",
  );
});

await check("an answer that arrives after the producer gave up is dropped", async () => {
  // The producer's deadline passes, it withdraws, and the slot goes back to
  // FREE. The host answers a moment later. That answer must go nowhere: written
  // into the record it would be read by whatever request comes next, and the
  // producer would return another call's pixels -- the one outcome the whole
  // protocol is arranged to prevent.
  let answerLate = () => {};
  const late = new Promise((resolve) => {
    answerLate = () => resolve({ ok: true, bytes: new Uint8Array(16), requestId: 1 });
  });
  const { outcome, record } = await exchange({
    budgetMillis: 200,
    transport: { request: () => late },
  });
  assertEqual(outcome.ok, false, "the producer must have given up");
  assertEqual(outcome.code, SYNC_ERROR_TIMED_OUT, "code");

  answerLate();
  // Let the relay's continuation run.
  await new Promise((resolve) => setTimeout(resolve, 50));

  const words = new Int32Array(record, 0, SYNC_RECORD_BYTES / 4);
  assertEqual(
    Atomics.load(words, OFF_STATE / 4),
    SYNC_STATE_FREE,
    "a late answer must not publish onto a slot the producer already left",
  );
  assertEqual(Atomics.load(words, OFF_ERROR / 4), 0, "nor leave an error behind");
});

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
