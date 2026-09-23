// The network's service calls, run by the producer and answered by the host.
//
//   emit-network-calls.mjs write <dir>  -- run the script below through the lane
//                                          functions and `core-stream.mjs`,
//                                          recording every service call it makes
//   emit-network-calls.mjs read  <dir>  -- run the same script again, answered by
//                                          what the host answered when it replayed
//                                          those calls, and check the results
//
// Between the two, engine/crates/core/src/runtime/external_services.rs
// (`the_producer_s_network_calls_run_on_the_host`) replays the recorded calls,
// in order, through the host's own dispatch and writes each answer. So the
// arguments are the ones the host takes, the host runs the same
// migo_services::network code the embedded ops call, and the results are what
// the producer makes of the host's real answers.
//
// WHAT IT COVERS. A request that is built and then aborted -- the abort is a
// close of the cancel handle, and the send after it is refused -- and a `data:`
// URL carried the whole way: built, sent, its body read through `core.read` in
// the chunks a caller asked for, and closed. A `data:` URL is the one scheme
// that needs no connection and still goes through every step, so this runs
// without a network and answers the same on every machine.
//
// THE HANDLES. In write mode the host is not there, so the producer is answered
// canned handles; the replay substitutes the host's own for them, by the canned
// answer each call was given. A rid is never invented by the producer.
//
// Driven by scripts/test-performance-plus-engine-contract.sh.

import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import * as coreStream from "../src/core-stream.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import * as asyncLane from "../src/lane-async.mjs";
import * as commandLane from "../src/lane-command.mjs";
import * as syncLane from "../src/lane-sync.mjs";
import { decodeServiceOutcome, OUTCOME_OK } from "../src/service.mjs";
import { SERVICE_OP } from "../src/service-ops.mjs";
import { ValueWriter } from "../src/service-value.mjs";

const [mode, dir] = process.argv.slice(2);
if ((mode !== "write" && mode !== "read") || !dir) {
  console.error("usage: emit-network-calls.mjs write|read <dir>");
  process.exit(2);
}

const NAME_OF = new Map(Object.entries(SERVICE_OP).map(([name, id]) => [id, name]));

/** The body the `data:` URL below carries, and the buffer it is read through. */
const BODY = "hello world";
const READ_BUFFER_BYTES = 4;

const calls = []; // write mode: what the lanes sent
let answers = []; // read mode: what the host answered, in order
let next = 0;

/**
 * The host's answer to the next call.
 *
 * In write mode it is a canned one of the right shape -- the handles the
 * producer then names, which the replay maps onto the host's. The canned bytes
 * are recorded beside the call so the replay knows which handle each answer
 * gave out.
 */
function answerFor(op) {
  if (mode === "write") {
    const w = new ValueWriter(64);
    w.word(OUTCOME_OK);
    canned(op, w);
    const reply = w.finish();
    calls[calls.length - 1].canned = Buffer.from(reply).toString("hex");
    return reply;
  }
  const answer = answers[next];
  next += 1;
  assert.equal(answer.op, NAME_OF.get(op), `answer ${next} is for ${answer.op}, the call was ${NAME_OF.get(op)}`);
  return Uint8Array.from(Buffer.from(answer.outcome, "hex"));
}

/** How many fetches have been built, so each is answered its own handles. */
let cannedFetches = 0;
let cannedRid = 0;

function canned(op, w) {
  const name = NAME_OF.get(op);
  if (name === "op_fetch") {
    cannedFetches += 1;
    // The first is an https request, which has a connection to abort; the
    // second is a `data:` URL, which has none.
    w.array(2);
    w.u32((cannedRid += 1));
    if (cannedFetches === 1) w.u32((cannedRid += 1));
    else w.null();
  } else if (name === "op_fetch_send") {
    w.array(9);
    w.u32(200);
    w.str("OK");
    w.array(0);
    w.str("");
    w.u32((cannedRid += 1));
    w.null();
    w.null();
    w.null();
    w.null();
  } else if (name === "core_read") {
    w.bytes(new Uint8Array(0));
  } else {
    w.null();
  }
}

function record(shape, op, writeArgs) {
  const w = new ValueWriter(64);
  writeArgs?.(w);
  calls.push({ shape, op: NAME_OF.get(op), args: Buffer.from(w.finish()).toString("hex") });
}

const services = {
  request(op, writeArgs) {
    if (mode === "write") record("async", op, writeArgs);
    const outcome = decodeServiceOutcome(answerFor(op));
    if (outcome.ok) return Promise.resolve(outcome.value);
    return Promise.reject(Object.assign(new Error(outcome.message), { name: outcome.className }));
  },
  command(op, writeArgs) {
    if (mode === "write") {
      record("command", op, writeArgs);
      answerFor(op);
      return;
    }
    // A command has no answer for content; the replay still writes what the
    // host made of it, so a command the host refused is seen here rather than
    // silently dropped.
    const outcome = decodeServiceOutcome(answerFor(op));
    assert.ok(outcome.ok, `the host refused ${NAME_OF.get(op)}: ${outcome.message}`);
  },
  flush() {
    return 0n;
  },
};

const sync = {
  call(call) {
    const view = new DataView(call.params.buffer, call.params.byteOffset, call.params.byteLength);
    const op = view.getUint32(0, true);
    if (mode === "write") {
      calls.push({
        shape: "sync",
        op: NAME_OF.get(op),
        args: Buffer.from(call.params.subarray(4)).toString("hex"),
      });
    }
    return answerFor(op);
  },
};

bindEngineHost({
  session: new FrameSession({ send() {}, sendControl() {} }),
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
  services,
  report() {},
});

// ---- the script -------------------------------------------------------------------
//
// The same calls in the same order in both modes: nothing branches on an answer.

const checks = [];
const check = (what, fn) => checks.push([what, fn]);

async function rejection(promise) {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  return null;
}

/** `op_fetch`'s ten arguments for a GET with no body and no client. */
const get = (url) => [url, [], null, false, null, null, 30000, false, false];

async function script() {
  // A request built and then aborted. The abort is a close of the cancel
  // handle -- what `RequestTask.abort()` does -- and the send after it is
  // refused rather than connecting, so no connection is ever made.
  const aborted = syncLane.op_fetch("GET", ...get("https://allowed.example/slow"));
  check("a request has a handle and one that aborts it", () => {
    assert.equal(typeof aborted.requestRid, "number");
    assert.equal(typeof aborted.cancelHandleRid, "number");
  });
  coreStream.tryClose(aborted.cancelHandleRid);
  const refused = await rejection(asyncLane.op_fetch_send(aborted.requestRid));
  check("the send after an abort says it was cancelled", () => {
    assert.equal(refused?.message, "request was cancelled");
  });

  // A `data:` URL, the whole way.
  const inline = syncLane.op_fetch("GET", ...get(`data:text/plain;base64,${Buffer.from(BODY).toString("base64")}`));
  check("a data: URL has nothing to abort", () => {
    assert.equal(inline.cancelHandleRid, null);
  });
  const response = await asyncLane.op_fetch_send(inline.requestRid);
  check("the head is the one the embedded op answers", () => {
    assert.equal(response.status, 200);
    assert.equal(response.statusText, "OK");
    assert.equal(response.error, null);
    assert.deepEqual(
      response.headers.find(([name]) => name === "content-type"),
      ["content-type", "text/plain"],
    );
  });

  // The body, read as the engine's `ReadableStream` reads it: a bounded
  // buffer, until a read answers nothing. Four bytes at a time, so the
  // chunking is the caller's and visible.
  const buffer = new Uint8Array(READ_BUFFER_BYTES);
  const chunks = [];
  for (let reads = 0; reads < BODY.length / READ_BUFFER_BYTES + 2; reads += 1) {
    const count = await coreStream.read(response.responseRid, buffer);
    chunks.push(Buffer.from(buffer.subarray(0, count)).toString("latin1"));
  }
  check("the body arrives in the caller's chunks and then ends", () => {
    assert.equal(chunks.join(""), BODY);
    assert.deepEqual(
      chunks.map((chunk) => chunk.length),
      [4, 4, 3, 0, 0],
    );
  });
  coreStream.close(response.responseRid);

  // A prefetch is a command: nothing answers it, and a host the policy refuses
  // is skipped rather than reported.
  commandLane.op_prefetch_dns(JSON.stringify(["allowed.example"]));
}

// ---- run ----------------------------------------------------------------------------

if (mode === "read") {
  answers = JSON.parse(readFileSync(join(dir, "answers.json"), "utf8"));
}
await script();

if (mode === "write") {
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "calls.json"), JSON.stringify(calls, null, 1));
  const ops = new Set(calls.map((call) => call.op));
  const expected = ["op_fetch", "op_fetch_send", "op_prefetch_dns", "core_read", "core_close", "core_try_close"];
  assert.deepEqual(
    expected.filter((name) => !ops.has(name)),
    [],
    "the script leaves network ops uncalled",
  );
  console.log(`wrote ${calls.length} network calls covering ${ops.size} ops to ${dir}`);
} else {
  assert.equal(next, answers.length, `the script took ${next} of ${answers.length} answers`);
  let failed = 0;
  for (const [what, fn] of checks) {
    try {
      await fn();
      console.log(`  ok   ${what}`);
    } catch (error) {
      failed += 1;
      console.log(`  FAIL ${what}\n       ${error.message}`);
    }
  }
  if (failed > 0) {
    console.log(`FAIL: ${failed} of ${checks.length} checks`);
    process.exit(1);
  }
  console.log(`PASS: ${checks.length} checks against ${answers.length} host answers`);
}
