// Real structuredClone / MessagePorts / OS threads, using production bootstrap
// and the staged shared Worker facade. No mocked serializer or credit release.
import assert from "node:assert/strict";
import { test, after } from "node:test";
import { Worker as NodeWorker, MessageChannel } from "node:worker_threads";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const stage = pathToFileURL(resolve(process.argv[2])).href;
if (process.env.MIGO_TEST_LEGACY_WORKER_CLONE === "1") globalThis.structuredClone = undefined;
const { encodeMessage, decodeMessage, wireBytes, createBudget } = await import(`${stage}/worker-clone.mjs`);

test("native clone preserves graph/types, ignores toJSON, invokes getters once and copies buffers", () => {
  const buffer = new ArrayBuffer(32); new Uint8Array(buffer)[7] = 42;
  let gets = 0;
  const value = { buffer, view: new Uint8Array(buffer, 7, 4), dataView: new DataView(buffer, 3, 12),
    map: new Map(), set: new Set(), date: new Date(1234), re: /mini/giu,
    nan: NaN, negative: -0, missing: undefined, bigint: 12345678901234567890n,
    get getter() { gets++; return 42; } };
  value.self = value; value.map.set(value, value.view); value.set.add(value);
  // A non-enumerable toJSON has no part in the structured clone algorithm.
  Object.defineProperty(value, "toJSON", { value() { throw new Error("JSON path was used"); } });
  const wire = encodeMessage(value, 1024 * 1024), copy = decodeMessage(wire);
  assert.equal(gets, 1); assert.equal(copy.getter, 42);
  assert.equal(copy.self, copy); assert.equal(copy.map.get(copy), copy.view); assert(copy.set.has(copy));
  assert(copy.date instanceof Date); assert.equal(+copy.date, 1234); assert.equal(copy.re.flags, "giu");
  assert(Number.isNaN(copy.nan)); assert(Object.is(copy.negative, -0)); assert.equal(copy.missing, undefined);
  assert.equal(copy.bigint, value.bigint); assert.equal(copy.dataView.buffer, copy.buffer);
  assert.equal(copy.view.buffer, copy.buffer); assert.equal(copy.view.byteOffset, 7);
  new Uint8Array(buffer)[7] = 99;
  assert.equal(copy.view[0], 42); assert.equal(buffer.byteLength, 32); assert(wireBytes(wire) > 32);
  const boxed = decodeMessage(encodeMessage({ number: new Number(NaN), boolean: new Boolean(false),
    string: new String("mini"), bigint: Object(123n), error: new Error("mini", { cause: { code: 42 } }) }, 65536));
  assert(Number.isNaN(boxed.number.valueOf())); assert.equal(boxed.boolean.valueOf(), false);
  assert.equal(boxed.string.valueOf(), "mini"); assert.equal(boxed.bigint.valueOf(), 123n);
  assert.equal(boxed.error.message, "mini"); assert.equal(boxed.error.cause.code, 42);
  for (const bad of [() => {}, Symbol(), new Proxy({}, {}), new WeakMap(), new SharedArrayBuffer(32),
                     new WebAssembly.Module(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]))]) {
    assert.throws(() => encodeMessage({ bad }, 1024), { name: "DataCloneError" });
  }
  assert.throws(() => encodeMessage(new Uint8Array(1024), 128), RangeError);
});

test("credit budgets retain committed messages until dequeue and close releases every reservation", () => {
  const budget = createBudget();
  const first = budget.reserve(1024); budget.acknowledge(first.ticket);
  assert.deepEqual(budget.usage(), { count: 1, bytes: 1024, closed: false });
  budget.commit(first.ticket, { bytes: 100 }); budget.abort(first.ticket);
  assert.equal(budget.usage().bytes, 100);
  budget.acknowledge(first.ticket); assert.equal(budget.usage().count, 0);
  for (let i = 0; i < 4096; i++) {
    const item = budget.reserve(1024); budget.commit(item.ticket, { bytes: 1 });
  }
  assert.throws(() => budget.reserve(1), /queue full/);
  budget.close(); assert.deepEqual(budget.usage(), { count: 0, bytes: 0, closed: true });
  assert.throws(() => budget.reserve(1), /terminated/);
  const bytes = createBudget();
  for (let i = 0; i < 4; i++) {
    const item = bytes.reserve(16 * 1024 * 1024); bytes.commit(item.ticket, { bytes: 16 * 1024 * 1024 });
  }
  assert.throws(() => bytes.reserve(1), /byte limit/);
  bytes.close(); assert.equal(bytes.usage().bytes, 0);
});

const scratch = mkdtempSync(join(tmpdir(), "migo-worker-test-"));
const script = pathToFileURL(join(scratch, "echo.js")).href;
writeFileSync(new URL(script), "worker.onMessage(({message}) => worker.postMessage(message));\n");
const spawned = new Set();
globalThis.Worker = class {
  constructor(url) {
    this.native = new NodeWorker(new URL("./worker-node-fixture.mjs", import.meta.url), { workerData: {
      bootstrap: url.href, legacyClone: process.env.MIGO_TEST_LEGACY_WORKER_CLONE === "1" } });
    spawned.add(this.native);
    this.native.on("error", (error) => this.onerror?.({ message: error.message, filename: url.href, lineno: 1 }));
  }
  postMessage(message, transfer) { this.native.postMessage(message, transfer); }
  terminate() { return this.native.terminate(); }
};
globalThis.location = new URL(`${stage}/page.html`);
const { port1, port2 } = new MessageChannel();
let terminateBeforeContinuation = null;
port2.addEventListener("message", (event) => {
  if (event.data.k !== "created") return;
  const dataPort = event.data.port;
  // Install after the relay's own handler. On a data event it first resolves
  // the receive Promise, then this observer terminates before await resumes.
  queueMicrotask(() => dataPort.addEventListener("message", (event) => {
    if (event.data.k === "msg" && terminateBeforeContinuation !== null) {
      terminateBeforeContinuation.terminate(); terminateBeforeContinuation = null;
    }
  }));
});
const { bindWorkerRelay } = await import(`${stage}/worker-page-relay.mjs`);
bindWorkerRelay(port1);
const { FrameSession } = await import(`${stage}/frame-session.mjs`);
const { bindEngineHost, readEngineSessionConfig } = await import(`${stage}/engine-host.mjs`);
bindEngineHost({ session: new FrameSession({ send() {}, sendControl() {} }),
  identity: readEngineSessionConfig({ launchNonce: "1", runtimeGeneration: "1", surfaceGeneration: "1",
    resourceEpoch: "0", surfaceWidth: 64, surfaceHeight: 64 }),
  socketCeilingBytes: 65536, report() {}, workerRelayPort: port2 });
const { createWorker } = await import(`${stage}/engine/host_v8_worker/01_worker.js`);
const relay = await import(`${stage}/worker-relay-channel.mjs`);

function exchange(worker, value) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("Worker echo timed out")), 10000);
    let settled = false;
    worker.onMessage(({ message }) => { if (!settled) { settled = true; clearTimeout(timeout); resolve(message); } });
    worker.onError(({ error }) => { if (!settled) { settled = true; clearTimeout(timeout); reject(error); } });
    worker.postMessage(value);
  });
}

after(async () => {
  relay.terminateWorkerAttempt(); port1.close(); port2.close();
  await Promise.all([...spawned].map((worker) => worker.terminate()));
  rmSync(scratch, { recursive: true, force: true });
});

test("shared pre-ready facade clones immediately, then exchanges types and a 1MiB payload over a direct port", async () => {
  const worker = createWorker(script);
  try {
    const original = { date: new Date(1234), map: new Map([["key", new Uint8Array([42])]]) };
    const result = exchange(worker, original); original.map.get("key")[0] = 99;
    const copy = await result;
    assert.equal(+copy.date, 1234); assert(copy.map instanceof Map); assert.equal(copy.map.get("key")[0], 42);
    const buffer = new ArrayBuffer(1024 * 1024); new Uint8Array(buffer)[1023] = 77;
    const payload = await exchange(worker, { buffer, view: new Uint8Array(buffer, 1023, 1) });
    assert.equal(payload.buffer.byteLength, 1024 * 1024); assert.equal(payload.view.buffer, payload.buffer);
    assert.equal(payload.view[0], 77); assert.equal(buffer.byteLength, 1024 * 1024);
    assert.equal(await exchange(worker, undefined), undefined);
    assert(Number.isNaN(await exchange(worker, NaN)));
  } finally { worker.terminate(); }
});

test("pre-ready saturation rejects before a getter and an old create completion cannot terminate its replacement", async () => {
  const first = createWorker(script);
  for (let i = 0; i < 4096; i++) first.postMessage(i);
  let getters = 0;
  assert.throws(() => first.postMessage({ get value() { getters++; return 1; } }), /queue full/);
  assert.equal(getters, 0); first.terminate();
  const replacement = createWorker(script);
  try { assert.equal(await exchange(replacement, "replacement"), "replacement"); }
  finally { replacement.terminate(); }
});

test("ready saturation rejects before clone, all committed credits drain, then the channel recovers", async () => {
  const worker = createWorker(script);
  try {
    await exchange(worker, "ready");
    let received = 0;
    const drained = new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("4096 replies did not drain")), 15000);
      worker.onMessage(({ message }) => {
        if (typeof message === "number" && ++received === 4096) { clearTimeout(timeout); resolve(); }
      });
    });
    for (let i = 0; i < 4096; i++) worker.postMessage(i);
    let getters = 0;
    assert.throws(() => worker.postMessage({ get value() { getters++; return 1; } }), { name: "WorkerError" });
    assert.equal(getters, 0);
    await drained;
    assert.equal(await exchange(worker, "recovered"), "recovered");
  } finally { worker.terminate(); }
});

test("a reservation cannot survive terminate/restart, and a terminated receive does not reach listeners", async () => {
  const worker = createWorker(script);
  await exchange(worker, "ready");
  const reservation = relay.reserveWorkerMessage(1024);
  let delivered = 0; worker.onMessage(() => delivered++);
  const pending = relay.nextWorkerMessage();
  worker.terminate(); assert.equal(await pending, undefined);
  const next = createWorker(script);
  try {
    await exchange(next, "new");
    assert.throws(() => relay.commitWorkerMessage(reservation.ticket, encodeMessage("old", 1024)), /expired/);
    relay.abortWorkerMessage(reservation.ticket);
    assert.equal(delivered, 0);
  } finally { next.terminate(); }
});

test("a fulfilled receive is suppressed when terminate runs before its await continuation", async () => {
  const worker = createWorker(script);
  try {
    await exchange(worker, "ready");
    let delivered = 0; worker.onMessage(() => delivered++);
    terminateBeforeContinuation = worker;
    worker.postMessage("must not reach a retired listener");
    const started = Date.now();
    while (terminateBeforeContinuation !== null && Date.now() - started < 10000) {
      await new Promise((resolve) => setTimeout(resolve, 1));
    }
    assert.equal(terminateBeforeContinuation, null, "the fulfilled receive race was not reached");
    assert.equal(delivered, 0);
  } finally { terminateBeforeContinuation = null; worker.terminate(); }
});
