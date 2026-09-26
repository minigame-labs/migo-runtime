// Content sees the embedded runtime's global names, not a Worker's.
//
// Phaser 3 reads `typeof importScripts`, decides it is inside a Web Worker and
// reports neither Canvas nor WebGL; on an iPhone the endless-runner bench game
// threw "Cannot create Canvas context" and stayed black. What is checked here:
// the retirement itself, that the list it keeps is the embedded runtime's
// committed baseline (read by the generator, not written down again), and that
// the producer's own lazy uses of a Worker name survive the retirement.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/global-scope.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

import { retireUnpublishedGlobals } from "../src/global-scope.mjs";
import { decodeBytes } from "../src/text-codec.mjs";

const REPO = resolve(import.meta.dirname, "../../../../..");

test("a name the embedded runtime does not publish is retired, and one it does is kept", () => {
  const scope = { importScripts() {}, self: null, migo: {}, requestAnimationFrame() {} };
  assert.deepEqual(retireUnpublishedGlobals(scope, ["migo", "requestAnimationFrame"]), []);
  assert.deepEqual(Object.keys(scope).sort(), ["migo", "requestAnimationFrame"]);
});

test("a global that cannot be deleted is named, not skipped", () => {
  const scope = { kept: 1 };
  Object.defineProperty(scope, "WorkerLocation", { value: 1, configurable: false });
  assert.deepEqual(retireUnpublishedGlobals(scope, ["kept"]), ["WorkerLocation"]);
});

test("the kept names are the embedded runtime's baseline, which has no Worker names", async () => {
  const out = mkdtempSync(join(tmpdir(), "migo-pp-globals-"));
  try {
    execFileSync("python3", [join(REPO, "scripts/gen-performance-plus-engine.py"), "--root", REPO, "--out", out], {
      stdio: "ignore",
    });
    const { PUBLISHED_GLOBALS } = await import(pathToFileURL(join(out, "engine/published-globals.mjs")).href);
    for (const name of ["migo", "requestAnimationFrame", "WebGLRenderingContext", "CanvasRenderingContext2D"]) {
      assert.ok(PUBLISHED_GLOBALS.includes(name), `${name} is published and must be kept`);
    }
    for (const name of ["importScripts", "self", "navigator", "location", "close", "WorkerGlobalScope"]) {
      assert.ok(!PUBLISHED_GLOBALS.includes(name), `${name} is a Worker's, not the embedded runtime's`);
    }
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
});

test("the producer decodes text after the Worker's TextDecoder is retired", () => {
  // `TextDecoder` is not an embedded global, so it is retired with the rest; the
  // codec constructs one per call and must take it from the platform record.
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "TextDecoder");
  delete globalThis.TextDecoder;
  try {
    assert.equal(decodeBytes(new Uint8Array([0x68, 0x69]), "utf8"), "hi");
  } finally {
    Object.defineProperty(globalThis, "TextDecoder", descriptor);
  }
});
