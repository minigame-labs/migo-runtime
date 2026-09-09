// The Dedicated Worker half of measurement gate 1.
//
// Three of the four questions here have a different answer on the main thread
// than in the context the design would use, and the fourth is not allowed on the
// main thread at all:
//
//   * Atomics.wait throws on a Window. Asking it there measures the throw.
//   * Synchronous XMLHttpRequest may set responseType in a Worker and may not on
//     a Window, so a Window answer to "can a synchronous request return binary"
//     is a fact about the restriction, not about the primitive. A21 turns on
//     this distinction: if a Worker can block on a binary synchronous request,
//     then SharedArrayBuffer is not the only way to serve synchronous content,
//     and the Window arm stops being excluded on capability grounds.
//   * WebSocket in a Worker is what A22 got wrong. It IS exposed here; the
//     reason a blocked content Worker needs a relay is that it cannot service
//     its own socket while blocked, which is a scheduling fact and admits a
//     second Worker as the third topology.

"use strict";

function answer(state, evidence, value) {
  const result = { state: state, evidence: evidence };
  if (value !== undefined && value !== null) {
    result.value = String(value);
  }
  return result;
}

// --- SharedArrayBuffer ------------------------------------------------------
//
// A15, asked here rather than on the window because the contract asks whether
// it constructs in a WORKER at this origin. The two contexts have been observed
// to disagree, and the Worker is the one the design would use.
function probeSharedArrayBuffer() {
  if (typeof SharedArrayBuffer !== "function") {
    return answer("unsupported", "SharedArrayBuffer is not a constructor in this Worker");
  }
  try {
    const buffer = new SharedArrayBuffer(8);
    return answer(
      "available",
      "new SharedArrayBuffer(8) in a Worker returned " + buffer.byteLength + " bytes"
    );
  } catch (error) {
    return answer("unavailable", "new SharedArrayBuffer(8) in a Worker threw " + error);
  }
}

// --- Atomics.wait -----------------------------------------------------------
//
// That the function exists is not the question. The question is whether it
// blocks, so the probe asks for a 50 ms timeout and checks both the verdict and
// the elapsed time: a call that returns "timed-out" in under a millisecond did
// not block, and would have been reported as a working blocking primitive by
// any check that only read the return value.
function probeAtomicsWait() {
  if (typeof SharedArrayBuffer !== "function") {
    return answer(
      "unsupported",
      "SharedArrayBuffer is not a constructor in this Worker, so Atomics.wait has nothing to wait on"
    );
  }
  if (typeof Atomics === "undefined" || typeof Atomics.wait !== "function") {
    return answer("unsupported", "Atomics.wait is not a function in this Worker");
  }
  try {
    const cell = new Int32Array(new SharedArrayBuffer(4));
    const timeoutMs = 50;
    const started = Date.now();
    const verdict = Atomics.wait(cell, 0, 0, timeoutMs);
    const elapsed = Date.now() - started;
    if (verdict !== "timed-out") {
      return answer(
        "unavailable",
        "Atomics.wait returned " + verdict + " for a cell holding its expected value"
      );
    }
    if (elapsed < timeoutMs / 2) {
      return answer(
        "unavailable",
        "Atomics.wait returned timed-out after " +
          elapsed +
          " ms of a " +
          timeoutMs +
          " ms timeout, so it did not block",
        elapsed
      );
    }
    return answer(
      "available",
      "Atomics.wait blocked for " + elapsed + " ms of a " + timeoutMs + " ms timeout",
      elapsed
    );
  } catch (error) {
    return answer("unavailable", "Atomics.wait threw " + error);
  }
}

// --- synchronous XHR --------------------------------------------------------
function probeSyncXhrBinary(loopbackOrigin) {
  if (!loopbackOrigin) {
    return answer(
      "not_probed",
      "no loopback listener was supplied, so there was nothing to make a synchronous request to"
    );
  }
  if (typeof XMLHttpRequest !== "function") {
    return answer("unsupported", "XMLHttpRequest is not a constructor in this Worker");
  }
  const payload = new Uint8Array([0x6d, 0x69, 0x67, 0x6f, 0x00, 0xff]);
  try {
    const request = new XMLHttpRequest();
    request.open("POST", loopbackOrigin + "/echo-body", false);
    try {
      request.responseType = "arraybuffer";
    } catch (error) {
      return answer(
        "unavailable",
        "setting responseType on a synchronous request threw " +
          error +
          ", so a synchronous round trip cannot carry binary"
      );
    }
    const started = Date.now();
    request.send(payload);
    const elapsed = Date.now() - started;

    if (request.status !== 200) {
      return answer(
        "unavailable",
        "the synchronous request completed with status " + request.status
      );
    }
    // WebKit answers a synchronous request whose responseType it refused with a
    // string in `responseText` and an empty `response`. Reading the type rather
    // than the length is what tells that apart from a zero-byte reply.
    if (!(request.response instanceof ArrayBuffer)) {
      return answer(
        "unavailable",
        "the synchronous response was " +
          Object.prototype.toString.call(request.response) +
          ", not an ArrayBuffer"
      );
    }
    const echoed = new Uint8Array(request.response);
    if (echoed.length !== payload.length) {
      return answer(
        "unavailable",
        "the synchronous round trip returned " +
          echoed.length +
          " of " +
          payload.length +
          " bytes"
      );
    }
    return answer(
      "available",
      "a synchronous XHR in a Worker blocked for " +
        elapsed +
        " ms and returned " +
        echoed.length +
        " bytes as an ArrayBuffer",
      elapsed
    );
  } catch (error) {
    return answer("unavailable", "the synchronous request threw " + error);
  }
}

// --- rAF in a Worker --------------------------------------------------------
//
// A18 says the design does not depend on this. The probe exists so the feature
// detection has a measured answer rather than an assumption, in both directions:
// assuming it is absent would leave a cheaper clock unused on an OS that has it.
function probeWorkerRaf() {
  return typeof self.requestAnimationFrame === "function"
    ? answer("available", "requestAnimationFrame is a function in DedicatedWorkerGlobalScope")
    : answer("unsupported", "requestAnimationFrame is not defined in DedicatedWorkerGlobalScope");
}

// --- WebSocket in a Worker --------------------------------------------------
function probeWebsocketInWorker(loopbackOrigin) {
  return new Promise(function (resolve) {
    if (!loopbackOrigin) {
      resolve(
        answer("not_probed", "no loopback listener was supplied, so there was nothing to connect to")
      );
      return;
    }
    if (typeof WebSocket !== "function") {
      resolve(answer("unsupported", "WebSocket is not a constructor in this Worker"));
      return;
    }
    const url = loopbackOrigin.replace(/^http/, "ws") + "/echo";
    const payload = new Uint8Array([0x01, 0x02, 0x03, 0x04]);
    let socket;
    try {
      socket = new WebSocket(url);
    } catch (error) {
      resolve(answer("unavailable", "constructing a WebSocket in a Worker threw " + error));
      return;
    }
    socket.binaryType = "arraybuffer";

    let settled = false;
    function settle(result) {
      if (settled) {
        return;
      }
      settled = true;
      try {
        socket.close();
      } catch (ignored) {
        // A socket that cannot be closed does not change the answer.
      }
      resolve(result);
    }

    const timer = setTimeout(function () {
      settle(
        answer(
          "unavailable",
          "the Worker's WebSocket neither echoed nor errored within 10 s at " + url
        )
      );
    }, 10000);

    const started = Date.now();
    socket.onopen = function () {
      socket.send(payload);
    };
    socket.onmessage = function (event) {
      clearTimeout(timer);
      const elapsed = Date.now() - started;
      if (!(event.data instanceof ArrayBuffer)) {
        settle(
          answer(
            "unavailable",
            "the echo arrived as " + Object.prototype.toString.call(event.data)
          )
        );
        return;
      }
      const echoed = new Uint8Array(event.data);
      if (echoed.length !== payload.length) {
        settle(
          answer(
            "unavailable",
            "the echo was " + echoed.length + " of " + payload.length + " bytes"
          )
        );
        return;
      }
      settle(
        answer(
          "available",
          "a Worker opened a WebSocket to " +
            url +
            " and round-tripped " +
            echoed.length +
            " binary bytes in " +
            elapsed +
            " ms",
          elapsed
        )
      );
    };
    socket.onerror = function () {
      clearTimeout(timer);
      settle(answer("unavailable", "the Worker's WebSocket to " + url + " raised an error"));
    };
  });
}

self.onmessage = function (event) {
  const loopbackOrigin = (event.data && event.data.loopbackOrigin) || null;

  // The blocking probes run first and in this order on purpose. Atomics.wait
  // and the synchronous request both stop this thread, and a pending socket
  // callback that could not be serviced while they ran is the very scheduling
  // fact A22 turns on -- so the socket is opened only once they are done, and
  // measures a connection rather than a starvation.
  const results = {
    worker_raf: probeWorkerRaf(),
    shared_array_buffer: probeSharedArrayBuffer(),
    atomics_wait: probeAtomicsWait(),
    sync_xhr_binary: probeSyncXhrBinary(loopbackOrigin)
  };

  probeWebsocketInWorker(loopbackOrigin).then(function (websocket) {
    results.websocket_in_worker = websocket;
    self.postMessage(results);
  });
};
