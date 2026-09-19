// The host's input, delivered to the engine's own host bridge, as content sees
// it.
//
//   node test/host-events.test.mjs <staged resource root> <events dir>
//
// The events dir holds service messages the Rust host wrote
// (external_services.rs, `the_host_s_input_as_the_producer_receives_it`): real
// HostCommands -- a touch, a key, the mouse, a wheel, the soft keyboard, IME,
// a gamepad, then a focus loss with all of them still held -- routed by the
// routing both executions share and encoded by the external session's sink.
// This boots the staged engine, takes its bridge the way the producer does,
// registers content's listeners through `migo.*`, delivers every EVENT, and
// checks each listener saw the values the host sent, in the order the embedded
// runtime would deliver them -- including the releases a focus loss
// synthesizes.
//
// Driven by scripts/test-performance-plus-engine-contract.sh.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [rootArg, eventsDir] = process.argv.slice(2);
if (!rootArg || !eventsDir) {
  console.error("usage: host-events.test.mjs <staged resource root> <events dir>");
  process.exit(2);
}
const root = pathToFileURL(resolve(rootArg)).href;
// Taken before the engine boots: its `console` is its own op from then on.
const log = console.log.bind(console);

globalThis.self = globalThis;
globalThis.addEventListener = () => {};

const { FrameSession } = await import(`${root}/frame-session.mjs`);
const { bindEngineHost, readEngineSessionConfig } = await import(`${root}/engine-host.mjs`);
const { bindHostEvents, dispatchHostEvent } = await import(`${root}/host-events.mjs`);
const { DOWN_EVENT, decodeServiceDownMessage } = await import(`${root}/service.mjs`);

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
  report() {},
});
await import(`${root}/engine/boot.mjs`);

// What the producer does once the engine has loaded.
const bridgeName = Symbol.for("Migo.hostBridge");
const bound = bindHostEvents(globalThis[bridgeName]);
delete globalThis[bridgeName];

const seen = [];
const note = (name) => (event) => seen.push([name, event]);
const { migo } = globalThis;
migo.onTouchStart(note("touchstart"));
migo.onTouchMove(note("touchmove"));
migo.onTouchCancel(note("touchcancel"));
migo.onKeyDown(note("keydown"));
migo.onKeyUp(note("keyup"));
migo.onMouseDown(note("mousedown"));
migo.onMouseUp(note("mouseup"));
migo.onWheel(note("wheel"));
migo.onKeyboardInput(note("keyboardinput"));
migo.onCompositionStart(note("compositionstart"));
migo.onCompositionEnd(note("compositionend"));
migo.onGamepadConnected(note("gamepadconnected"));

let delivered = 0;
for (const name of readdirSync(eventsDir).filter((file) => file.endsWith(".bin")).sort()) {
  const bytes = new Uint8Array(readFileSync(join(eventsDir, name)));
  for (const record of decodeServiceDownMessage(bytes).records) {
    if (record.kind !== DOWN_EVENT) continue;
    assert.equal(dispatchHostEvent(record.event, record.values), true, `event ${record.event} has a hook`);
    delivered += 1;
  }
}
// Touches are drained in a microtask, as in process.
await new Promise((settle) => setTimeout(settle, 0));

let failures = 0;
function check(what, fn) {
  try {
    fn();
    log(`  ok   ${what}`);
  } catch (error) {
    failures += 1;
    log(`  FAIL ${what}\n       ${error.message}`);
  }
}

const of = (name) => seen.filter(([kind]) => kind === name).map(([, event]) => event);
const touch = (event) => ({ id: event.touches.concat(event.changedTouches)[0]?.identifier, x: event.changedTouches[0]?.clientX });

check("the bridge had a hook for every event number", () => assert.equal(bound, 18));
check("a touch start and move carry the host's point and time", () => {
  const [start] = of("touchstart");
  assert.equal(start.touches.length, 1);
  assert.deepEqual(
    { id: start.touches[0].identifier, x: start.touches[0].clientX, y: start.touches[0].clientY, force: start.touches[0].force },
    { id: 7, x: 10, y: 20, force: 0.5 },
  );
  assert.equal(start.timeStamp, 100);
  assert.deepEqual(touch(of("touchmove")[0]), { id: 7, x: 11 });
});
check("a key, the mouse and the wheel arrive with their fields", () => {
  const [key] = of("keydown");
  assert.equal(key.key, "a");
  assert.equal(key.code, "KeyA");
  assert.equal(key.timeStamp, 5);
  assert.deepEqual(of("mousedown")[0], { x: 1.5, y: 2.5, button: 0, timeStamp: 6 });
  const [wheel] = of("wheel");
  assert.deepEqual([wheel.deltaX, wheel.deltaY, wheel.deltaZ, wheel.deltaMode, wheel.timeStamp], [1, -2, 0, 1, 7]);
});
check("the soft keyboard's text and a composition arrive as text", () => {
  assert.deepEqual(of("keyboardinput")[0], { value: "héllo" });
  assert.equal(of("compositionstart")[0].data, "ni");
});
check("a gamepad connects and its sample lands in getGamepads()", () => {
  assert.equal(of("gamepadconnected").length, 1);
  const [pad] = migo.getGamepads();
  assert.deepEqual(Array.from(pad.axes), [0.5, -0.25]);
  assert.equal(pad.buttons[0].pressed, true);
  assert.equal(pad.buttons[0].value, 1);
});
// The host releases the touch first; content hears it last. `01_touch.js`
// drains touches in a microtask and the other listeners run in the call, and a
// focus loss's releases are delivered in one turn -- one host command in
// process, one service message here -- so the microtask runs after them in
// both executions.
check("losing focus releases what was held, heard in the embedded runtime's order", () => {
  const releases = seen
    .map(([kind]) => kind)
    .filter((kind) => ["touchcancel", "mouseup", "keyup", "compositionend"].includes(kind));
  assert.deepEqual(releases, ["mouseup", "keyup", "compositionend", "touchcancel"]);
  assert.equal(touch(of("touchcancel")[0]).id, 7);
  assert.deepEqual(of("mouseup")[0], { x: 1.5, y: 2.5, button: 0, timeStamp: 6 });
  assert.equal(of("keyup")[0].code, "KeyA");
  assert.equal(of("compositionend")[0].data, "");
});
check("content cannot reach the bridge once the producer has taken it", () =>
  assert.equal(globalThis[Symbol.for("Migo.hostBridge")], undefined),
);

log(failures === 0 ? `PASS (${delivered} events delivered)` : `FAIL: ${failures} check(s)`);
process.exit(failures === 0 ? 0 : 1);
