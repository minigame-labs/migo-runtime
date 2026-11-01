// The engine's source identity must survive the external op adapter. Requests
// are sent in invocation order, even while earlier replies are still pending.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_inner_audio_load_url } from "../src/lane-async.mjs";
import { op_inner_audio_set_start_time, op_inner_audio_play } from "../src/lane-command.mjs";
import { SERVICE_OP } from "../src/service-ops.mjs";

const calls = [];
const replies = [];
const configurationCommands = [];
bindEngineHost({
  session: new FrameSession({ send() {}, sendControl() {} }),
  identity: readEngineSessionConfig({
    launchNonce: "0x0123456789abcdeffedcba9876543210",
    runtimeGeneration: "1", surfaceGeneration: "1", resourceEpoch: "0",
    surfaceWidth: 64, surfaceHeight: 64,
  }),
  socketCeilingBytes: 64 * 1024,
  report() {},
  services: {
    command(op, write) {
      const args = [];
      write({ u32: value => args.push(["u32", value]), f64: value => args.push(["f64", value]) });
      configurationCommands.push([op, args]);
    },
    request(op, write) {
      assert.equal(op, SERVICE_OP.op_inner_audio_load_url);
      const args = [];
      write({ u32: value => args.push(["u32", value]), str: value => args.push(["str", value]) });
      calls.push(args);
      return new Promise(resolve => replies.push(resolve));
    },
  },
});
const first = op_inner_audio_load_url(7, 701, "clip-a.mp3");
const second = op_inner_audio_load_url(7, 702, "");
assert.deepEqual(calls, [
  [["u32", 7], ["u32", 701], ["str", "clip-a.mp3"]],
  [["u32", 7], ["u32", 702], ["str", ""]],
]);
assert.throws(() => op_inner_audio_load_url(7, "703", "clip-b.mp3"));
assert.equal(calls.length, 2);
replies[1]();
assert.equal(await second, undefined);
replies[0]();
assert.equal(await first, undefined);
console.log("source load adapter: invocation order, identity, clear and strict fields passed");
op_inner_audio_set_start_time(7, 0.25);
op_inner_audio_play(7);
assert.deepEqual(configurationCommands, [
  [SERVICE_OP.op_inner_audio_set_start_time, [["u32", 7], ["f64", 0.25]]],
  [SERVICE_OP.op_inner_audio_play, [["u32", 7]]],
]);

// Execute the shared engine facade itself. A loading play used to mutate the
// persistent autoplay setting and skip the command lane; pause/play ordering
// could then never reach the native source's pending controls.
const sourceUrl = new URL("../../../../../engine/crates/runtime-v8/src/audio/02_inner_audio_context.js", import.meta.url);
const source = readFileSync(process.env.MIGO_LOADING_FACADE || sourceUrl, "utf8");
const names = source.match(/import\s*\{([\s\S]*?)\}\s*from "ext:core\/ops";/)[1]
  .split(",").map(name => name.trim()).filter(Boolean);
const commands = [];
const pendingLoads = [];
let identity = 2000;
const globals = {
  allocateHostCallbackId: () => ++identity,
  createListenerGroup() {
    const handlers = new Set();
    return { on(fn) { handlers.add(fn); }, off(fn) { if (fn) handlers.delete(fn); else handlers.clear(); }, trigger(value) { for (const fn of handlers) fn(value); } };
  },
  errorMessage: error => error.message,
  promisify: (_name, fn) => options => Promise.resolve(fn(options)),
};
for (const name of names) globals[name] = (...args) => {
  commands.push([name, ...args]);
  if (name === "op_inner_audio_load_url") return new Promise((resolve, reject) => pendingLoads.push({ resolve, reject, args }));
};
const context = vm.createContext(globals);
const executable = source.replace(/import\s*\{[\s\S]*?\}\s*from "[^"]+";/g, "")
  .replace(/export\s*\{[\s\S]*?\};/, "globalThis.api = { InnerAudioContext, _internalEnqueueInnerAudioEvent };");
new vm.Script(executable, { filename: String(sourceUrl) }).runInContext(context);
const facade = new context.api.InnerAudioContext();
facade.src = "pending.m4a";
const mark = commands.length;
await facade.play(); facade.pause(); await facade.play(); facade.stop(); facade.seek(0.25); await facade.play();
assert.equal(facade.autoplay, false, "loading play must not change autoplay");
assert.equal(facade.paused, true, "only the native source snapshot announces playing");
assert.deepEqual(commands.slice(mark).map(row => row[0]), [
  "op_inner_audio_play", "op_inner_audio_pause", "op_inner_audio_play", "op_inner_audio_stop", "op_inner_audio_seek", "op_inner_audio_play",
]);
assert.equal(commands.at(-2)[2], 0.25);
facade.autoplay = true;
const explicitAutoplay = commands.filter(row => row[0] === "op_inner_audio_set_autoplay").length;
facade.pause(); facade.stop(); await facade.play();
assert.equal(facade.autoplay, true);
assert.equal(commands.filter(row => row[0] === "op_inner_audio_set_autoplay").length, explicitAutoplay);
const firstSource = pendingLoads[0].args[1];
let played = 0;
facade.onPlay(() => played++);
context.api._internalEnqueueInnerAudioEvent(facade._getId(), "play", 0.25, firstSource, 1.0, true, false);
await Promise.resolve();
assert.equal(facade.paused, false); assert.equal(facade.currentTime, 0.25); assert.equal(played, 1);
facade.src = "replacement.m4a";
context.api._internalEnqueueInnerAudioEvent(facade._getId(), "play", 0.75, firstSource, 1.0, true, false);
pendingLoads[0].reject(new Error("retired load"));
await Promise.resolve(); await Promise.resolve();
assert.equal(facade.paused, true); assert.equal(facade.currentTime, 0); assert.equal(played, 1);
facade.destroy();
const beforeDestroyed = commands.length;
await assert.rejects(facade.play(), /destroyed/);
facade.pause(); facade.stop(); facade.seek(1);
assert.equal(commands.length, beforeDestroyed);
console.log("loading facade: native command order/autoplay/source snapshots/destroy passed");


// startTime is persistent native configuration. It must not synthesize a seek
// from a delayed JS currentTime snapshot, nor override an explicit zero target.
const starts = new context.api.InnerAudioContext();
const startMark = commands.length;
starts.startTime = 0.5;
starts.autoplay = true;
starts.src = "start-time.m4a";
starts.startTime = 0.25;
starts.seek(0);
await starts.play();
assert.equal(starts.startTime, 0.25);
assert.equal(starts.currentTime, 0);
assert.equal(starts.paused, true);
assert.deepEqual(commands.slice(startMark).map(row => [row[0], ...row.slice(2)]), [
  ["op_inner_audio_set_start_time", 0.5], ["op_inner_audio_set_autoplay", true],
  ["op_inner_audio_load_url", pendingLoads.at(-1).args[1], "start-time.m4a"],
  ["op_inner_audio_set_start_time", 0.25], ["op_inner_audio_seek", 0], ["op_inner_audio_play"],
]);
const startSource = pendingLoads.at(-1).args[1];
context.api._internalEnqueueInnerAudioEvent(starts._getId(), "canPlay", 0, startSource, 1, true, true);
await Promise.resolve();
const bufferedMark = commands.length;
await starts.play(); starts.pause(); starts.startTime = 0.75; await starts.play(); starts.stop(); await starts.play();
assert.deepEqual(commands.slice(bufferedMark).map(row => row[0]), [
  "op_inner_audio_play", "op_inner_audio_pause", "op_inner_audio_set_start_time", "op_inner_audio_play", "op_inner_audio_stop", "op_inner_audio_play",
]);
starts.startTime = Number.NaN; starts.startTime = Number.POSITIVE_INFINITY;
assert.equal(starts.startTime, 0.75);
starts.startTime = -1; assert.equal(starts.startTime, 0);
starts.startTime = "0.125"; assert.equal(starts.startTime, 0.125);
starts.src = "start-time-replacement.m4a";
assert.equal(starts.startTime, 0.125);
starts.destroy();
const destroyedStartMark = commands.length;
starts.startTime = 0.5;
assert.equal(commands.length, destroyedStartMark);
console.log("startTime facade: ordered native configuration, explicit zero, stale snapshots, finite values and destroy passed");
