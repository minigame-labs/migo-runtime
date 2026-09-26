// The GL context's state, as the producer applies it.
//
// A context loss rebuilds the renderer's resource table and the host advances
// the resource epoch, refusing any packet that names the old one. Before
// `DOWN_CONTEXT_STATE` existed the producer never learned the new epoch: every
// packet after a loss was refused and the game stopped for good -- on an
// iPhone, at startup, because Pixi probes WebGL with `loseContext()`. This
// feeds the record through the frame session the producer uses and checks what
// the producer then names and what content is told, against the embedded
// runtime's reconciliation rule (engine/crates/core/src/runtime/host.rs,
// `reconcile_context_lost`).
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/context-state.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import assert from "node:assert/strict";
import test from "node:test";

import { DOWN_CONTEXT_STATE, encodeBytes } from "../src/downlink.mjs";
import {
  applyContextState,
  bindContextEvents,
  bindEngineHost,
  engineHost,
  readEngineSessionConfig,
} from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";

const session = new FrameSession({ send: () => {}, onContextState: applyContextState });
const deliver = (lost, resourceEpoch) =>
  session.handleMessage(
    encodeBytes([{ kind: DOWN_CONTEXT_STATE, generation: 1, lost, resourceEpoch }]),
  );

// A loss the host reported before the engine was bound: the frame channel opens
// first, and a record arriving in between must still be applied.
deliver(true, 1);
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
  report: () => {},
});
const told = [];
bindContextEvents({ _internalTriggerWebglContextEvent: (type) => told.push(type) });
const { state } = engineHost();
const take = () => told.splice(0);

test("a state that arrived before the engine was bound is adopted when it is", () => {
  assert.equal(state.resourceEpoch, 1n);
  assert.equal(state.contextLost, true);
});

test("recovery and loss are each told once, with the epoch adopted first", () => {
  deliver(false, 1);
  assert.deepEqual(take(), ["webglcontextrestored"]);
  assert.equal(state.contextLost, false);

  deliver(true, 2);
  assert.deepEqual(take(), ["webglcontextlost"]);
  assert.equal(state.resourceEpoch, 2n, "packets name the new epoch from here on");
  assert.equal(state.contextLost, true, "set before content hears, which reads isContextLost()");

  deliver(true, 2);
  assert.deepEqual(take(), [], "the same level again tells nothing");

  deliver(false, 2);
  assert.deepEqual(take(), ["webglcontextrestored"]);
});

test("a whole loss and recovery missed in between is played as the pair", () => {
  deliver(false, 4);
  assert.deepEqual(take(), ["webglcontextlost", "webglcontextrestored"]);
  assert.equal(state.resourceEpoch, 4n);
  assert.equal(state.contextLost, false);
});

test("the epoch never moves backwards", () => {
  deliver(false, 3);
  assert.deepEqual(take(), []);
  assert.equal(state.resourceEpoch, 4n);
});
