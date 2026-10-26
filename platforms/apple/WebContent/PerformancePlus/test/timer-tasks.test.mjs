// Run the shared timer policy through the WebKit backend, including chained
// promise jobs. No synchronous microtask-drain API is assumed in this engine.
import assert from "node:assert/strict";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const root = pathToFileURL(resolve(process.argv[2])).href;
globalThis.self = globalThis;
globalThis.addEventListener = () => {};
const { FrameSession } = await import(`${root}/frame-session.mjs`);
const { bindEngineHost, readEngineSessionConfig } = await import(`${root}/engine-host.mjs`);
bindEngineHost({
  session: new FrameSession({ send() {}, sendControl() {} }),
  identity: readEngineSessionConfig({
    launchNonce: "0x0123456789abcdeffedcba9876543210",
    runtimeGeneration: "1", surfaceGeneration: "1", resourceEpoch: "0",
    surfaceWidth: 64, surfaceHeight: 64,
  }),
  socketCeilingBytes: 65536,
  report() {},
});
const {
  setTimeout: timerTimeout, clearTimeout: timerClear, setImmediate: timerImmediate,
  _internalSetTimerBackgrounded: backgrounded,
} = await import(`${root}/engine/host_v8_web/02_timers.js`);
const { getTimerDepth } = await import(`${root}/timer-task-context.mjs`);
const { runTask } = await import(`${root}/task-gate.mjs`);
const { platform } = await import(`${root}/platform.mjs`);
const nextTask = () => new Promise((resolve) => platform.setTimeout(() => runTask(resolve), 0));

const order = [];
await new Promise((resolve) => {
  timerTimeout(() => {
    order.push("first");
    Promise.resolve().then(() => {
      assert.equal(getTimerDepth(), 1);
      order.push("promise");
    }).then(() => {
      assert.equal(getTimerDepth(), 1);
      timerClear(cancelled);
      order.push("chained");
    });
  }, 0);
  const cancelled = timerTimeout(() => order.push("cancelled"), 0);
  timerTimeout(() => { order.push("last"); resolve(); }, 0);
});
assert.deepEqual(order, ["first", "promise", "chained", "last"]);
await nextTask();
assert.equal(getTimerDepth(), 0, "non-timer task resets nesting");

let nested = 0;
await new Promise((resolve) => {
  function tick() {
    assert.equal(getTimerDepth(), ++nested);
    if (nested === 7) { resolve(); return; }
    Promise.resolve().then(() => Promise.resolve()).then(() => timerTimeout(tick, 0));
  }
  timerTimeout(tick, 0);
});
await nextTask();
assert.equal(getTimerDepth(), 0);

backgrounded(true);
let fired = 0;
timerImmediate(() => fired++);
const cancelled = timerTimeout(() => { throw new Error("cancelled hidden timer"); }, 0);
timerClear(cancelled);
await nextTask();
assert.equal(fired, 0);
backgrounded(false);
await new Promise((resolve) => platform.setTimeout(resolve, 20));
assert.equal(fired, 1);
console.log("timer tasks: shared policy / promise order / nesting / cancellation / lifecycle PASS");
