// The view resized or turned, as the producer applies it.
//
// The game is told its window before its first line runs, and on every platform
// it is told again when the window changes: the embedded host resizes its
// surface and calls the engine's surface-change ingress
// (`_internalTriggerWindowResize`), which makes the main canvas follow the surface
// unless content sized it and tells content through `onWindowResize`. On this
// lane the host resizes the engine's surface and hands the producer the new size
// and the device it now describes (`MigoPerformancePlusHost.resize`); this checks
// that the producer adopts both before the engine's ingress runs -- the
// listeners read them -- including a resize that arrived before the engine host
// was bound.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/resize-state.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import assert from "node:assert/strict";
import test from "node:test";

import {
  applyResize,
  bindEngineHost,
  bindResizeEvents,
  engineHost,
  readEngineSessionConfig,
} from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_get_canvas_info, op_get_window_info, recordCanvasSize } from "../src/lane-local.mjs";

const window = (width, height) =>
  JSON.stringify({ window_width: width, window_height: height, screen_width: width, screen_height: height, pixel_ratio: 2 });

// Laid out again before the engine loaded: the newest resize is kept and
// adopted when the engine host is bound.
applyResize({ surfaceWidth: 400, surfaceHeight: 800, device: { windowInfo: window(200, 400) } });
bindEngineHost({
  session: new FrameSession({ send: () => {} }),
  identity: readEngineSessionConfig({
    launchNonce: "0x0123456789abcdeffedcba9876543210",
    runtimeGeneration: "1",
    surfaceGeneration: "1",
    resourceEpoch: "0",
    surfaceWidth: 800,
    surfaceHeight: 400,
    device: { windowInfo: window(400, 200) },
  }),
  socketCeilingBytes: 64 * 1024,
  report: () => {},
});
// What content would see from inside its listener, each time the ingress runs.
const seen = [];
bindResizeEvents({
  _internalTriggerWindowResize: () =>
    seen.push({ window: JSON.parse(op_get_window_info()), canvas: op_get_canvas_info(1) }),
});
const take = () => seen.splice(0);

test("a resize that arrived before the engine was bound is adopted when it is", () => {
  const { state } = engineHost();
  assert.equal(state.surfaceWidth, 400);
  assert.equal(state.surfaceHeight, 800);
  assert.equal(JSON.parse(op_get_window_info()).window_width, 200);
  // The canvas content never sized is the surface in CSS pixels.
  assert.deepEqual(op_get_canvas_info(1), [200, 400]);
});

test("the size and the device are adopted before the engine's ingress runs", () => {
  applyResize({ surfaceWidth: 800, surfaceHeight: 400, device: { windowInfo: window(400, 200) } });
  assert.deepEqual(take(), [{ window: { ...JSON.parse(window(400, 200)) }, canvas: [400, 200] }]);
});

test("a canvas content sized keeps its size", () => {
  recordCanvasSize(1, 300, 150);
  applyResize({ surfaceWidth: 400, surfaceHeight: 800, device: { windowInfo: window(200, 400) } });
  const [heard] = take();
  assert.equal(heard.window.window_width, 200);
  assert.deepEqual(heard.canvas, [300, 150]);
});

test("a device that changed alone is answered next, and is no resize", () => {
  const insets = JSON.stringify({ ...JSON.parse(window(200, 400)), safe_area: { left: 0, top: 47, right: 0, bottom: 34 } });
  applyResize({ surfaceWidth: 400, surfaceHeight: 800, device: { windowInfo: insets } });
  assert.deepEqual(take(), []);
  assert.deepEqual(JSON.parse(op_get_window_info()).safe_area, { left: 0, top: 47, right: 0, bottom: 34 });
});

test("a device the host no longer describes is no device", () => {
  applyResize({ surfaceWidth: 800, surfaceHeight: 400 });
  assert.throws(() => op_get_window_info(), /getWindowInfo:fail not supported/);
  take();
});

test("a resize that is not a pixel size is refused and changes nothing", () => {
  const before = { ...engineHost().state };
  assert.throws(() => applyResize({ surfaceWidth: 0, surfaceHeight: 800 }), /positive integer pixel count/);
  assert.throws(() => applyResize(null), /a resize is an object/);
  assert.throws(
    () => applyResize({ surfaceWidth: 400, surfaceHeight: 800, device: { windowInfo: {} } }),
    /as a string/,
  );
  assert.deepEqual({ ...engineHost().state }, before);
  assert.deepEqual(take(), []);
});
