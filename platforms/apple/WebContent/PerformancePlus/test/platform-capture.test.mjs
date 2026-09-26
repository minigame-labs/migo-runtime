// The producer's platform primitives are the ones the Worker started with,
// whatever the globals are by the time they are used.
//
// Content shares the producer's global scope, and a web adapter replaces
// `XMLHttpRequest` with its own shim over `migo.request` -- one with no
// synchronous mode. The synchronous bridge looked the constructor up when it
// posted, so after a Pixi game's adapter loaded, every sync call got the shim
// and came back with status 0: the game's first `gl.getParameter` threw and it
// never drew a frame. This stands in for that sequence -- the platform's
// constructor present when the modules load, a content shim installed after --
// and requires the bridge to post through the platform's.
//
// Imported dynamically, after the platform constructor is installed: a static
// import would be evaluated before this file sets anything up.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/platform-capture.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import assert from "node:assert/strict";
import test from "node:test";

const used = [];

class PlatformXMLHttpRequest {
  open(method, url, async) {
    used.push({ via: "platform", method, url, async });
  }
  send() {
    this.status = 200;
    this.response = new ArrayBuffer(0);
  }
}

class ContentShim {
  open() {
    used.push({ via: "content shim" });
  }
  send() {
    this.status = 0;
  }
}

const platformFetch = async () => ({ via: "platform" });
const contentFetch = async () => ({ via: "content shim" });

globalThis.XMLHttpRequest = PlatformXMLHttpRequest;
globalThis.fetch = platformFetch;
const { platform } = await import("../src/platform.mjs");
const { blockingPost } = await import("../src/sync-call.mjs");

// What a web adapter does once content starts.
globalThis.XMLHttpRequest = ContentShim;
globalThis.fetch = contentFetch;

test("the synchronous bridge posts through the platform's XMLHttpRequest, not content's", () => {
  const { status } = blockingPost("migo-content://content/__migo/sync", new Uint8Array(4));
  assert.equal(status, 200);
  assert.deepEqual(used, [
    { via: "platform", method: "POST", url: "migo-content://content/__migo/sync", async: false },
  ]);
});

test("the platform fetch is the one present when the modules loaded", async () => {
  assert.deepEqual(await platform.fetch("migo-content://content/__migo/service"), { via: "platform" });
});

test("the platform record cannot be altered by content either", () => {
  assert.ok(Object.isFrozen(platform));
});
