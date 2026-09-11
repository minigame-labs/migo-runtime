import assert from "node:assert/strict";
import test from "node:test";

import { CHANNEL_SCHEME, CHANNEL_SOCKET, createHybridSender } from "../src/uplink.mjs";

/// The threshold the host measured. Written here ONLY as a test fixture: the
/// production path takes it from the host's injected configuration, which is
/// what keeps `MigoFrameChannelPolicy` the single source. A test that imported
/// it from the module under test could not tell a wrong constant from a right
/// one.
const CEILING = 64 * 1024;

function collector() {
  const sent = [];
  return { sent, send: (bytes) => sent.push(bytes.byteLength) };
}

test("a packet at the ceiling goes over the socket", () => {
  const socket = collector();
  const send = createHybridSender({
    sendOverSocket: socket.send,
    schemeUrl: "migo-content://content/__migo/frame",
    socketCeilingBytes: CEILING,
  });
  // Inclusive, and this is the case that says so: at exactly 64 KiB latency was
  // level and host CPU still favoured the socket.
  assert.equal(send(new Uint8Array(CEILING)), CHANNEL_SOCKET);
  assert.deepEqual(socket.sent, [CEILING]);
});

test("one byte over the ceiling goes over the scheme", async () => {
  const socket = collector();
  const requests = [];
  globalThis.fetch = (url, init) => {
    requests.push({ url, method: init.method, length: init.body.byteLength });
    return Promise.resolve({ ok: true, status: 200 });
  };
  try {
    const send = createHybridSender({
      sendOverSocket: socket.send,
      schemeUrl: "migo-content://content/__migo/frame",
      socketCeilingBytes: CEILING,
    });
    assert.equal(send(new Uint8Array(CEILING + 1)), CHANNEL_SCHEME);
    assert.deepEqual(socket.sent, [], "the socket must not have seen it");
    assert.deepEqual(requests, [
      {
        url: "migo-content://content/__migo/frame",
        method: "POST",
        length: CEILING + 1,
      },
    ]);
  } finally {
    delete globalThis.fetch;
  }
});

test("with no scheme URL everything goes over the socket", () => {
  const socket = collector();
  const send = createHybridSender({ sendOverSocket: socket.send, socketCeilingBytes: CEILING });
  assert.equal(send(new Uint8Array(4 * CEILING)), CHANNEL_SOCKET);
  assert.deepEqual(socket.sent, [4 * CEILING]);
});

test("a scheme failure is reported rather than thrown", async () => {
  // The send is asynchronous. By the time it fails, the caller that produced the
  // frame has returned and there is nobody left to catch -- so a throw here
  // would become an unhandled rejection and the host would learn nothing.
  const failures = [];
  globalThis.fetch = () => Promise.resolve({ ok: false, status: 507 });
  try {
    const send = createHybridSender({
      sendOverSocket: () => assert.fail("the socket must not have been used"),
      schemeUrl: "migo-content://content/__migo/frame",
      socketCeilingBytes: CEILING,
      onSchemeFailure: (error, byteCount) => failures.push([String(error), byteCount]),
    });
    send(new Uint8Array(CEILING + 1));
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(failures.length, 1);
    assert.match(failures[0][0], /507/);
    assert.equal(failures[0][1], CEILING + 1);
  } finally {
    delete globalThis.fetch;
  }
});

test("a rejected request is reported too", async () => {
  const failures = [];
  globalThis.fetch = () => Promise.reject(new Error("the handler went away"));
  try {
    const send = createHybridSender({
      sendOverSocket: () => assert.fail("the socket must not have been used"),
      schemeUrl: "migo-content://content/__migo/frame",
      socketCeilingBytes: CEILING,
      onSchemeFailure: (error) => failures.push(String(error)),
    });
    send(new Uint8Array(CEILING + 1));
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(failures, ["Error: the handler went away"]);
  } finally {
    delete globalThis.fetch;
  }
});

test("a scheme URL without the host's ceiling is refused, not defaulted", () => {
  // Defaulting would put a second copy of a measured number in this file, and
  // the two would drift the first time the measurement is redone.
  assert.throws(
    () =>
      createHybridSender({
        sendOverSocket: () => {},
        schemeUrl: "migo-content://content/__migo/frame",
      }),
    /socketCeilingBytes/,
  );
});
