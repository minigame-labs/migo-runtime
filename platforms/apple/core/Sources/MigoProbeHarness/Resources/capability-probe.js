// The window half of measurement gate 1.
//
// Gate 1 runs with no renderer and no validator. What it answers is which of
// the candidate architectures a device can host at all, and the answers cut the
// candidate set every later gate draws from -- benchmarking an arm the device
// cannot run benchmarks whatever ran instead.
//
// Every answer carries evidence. A capability reported as available with
// nothing behind it is indistinguishable from a probe that returned a default,
// and the record shape refuses to encode one.
//
// This file is loaded from two different origins in one session: the loopback
// listener and the app's custom scheme. Which questions it can answer depends
// on which, so it reports its own origin and the native side merges.

"use strict";

const ORIGIN_KIND = (function () {
  return location.protocol === "http:" || location.protocol === "https:"
    ? "loopback"
    : "custom_scheme";
})();

function answer(state, evidence, value) {
  const result = { state: state, evidence: evidence };
  if (value !== undefined && value !== null) {
    result.value = String(value);
  }
  return result;
}

function available(evidence, value) {
  return answer("available", evidence, value);
}
function unavailable(evidence) {
  return answer("unavailable", evidence);
}
function unsupported(evidence) {
  return answer("unsupported", evidence);
}

// --- JIT -------------------------------------------------------------------
//
// A24. Lockdown Mode disables JIT for web content, so "WebContent has JIT" is a
// device state and not an admission invariant. There is no API that reports it,
// so it is measured: a hot integer loop run in batches speeds up by a large
// factor once the tiers engage and by essentially nothing when they cannot.
//
// The ratio is recorded rather than only the verdict. The threshold below is a
// judgement -- the ratio is the observation, and a reader who disagrees with
// the cut can apply their own to the number in the record.
const JIT_WARMUP_RATIO_THRESHOLD = 3.0;

function probeJit() {
  function work(iterations) {
    let accumulator = 0;
    for (let i = 0; i < iterations; i += 1) {
      accumulator = (accumulator + Math.imul(i, 2654435761)) >>> 0;
    }
    return accumulator;
  }

  const batchSize = 3000000;
  const batches = 12;
  const timings = [];
  let sink = 0;
  for (let batch = 0; batch < batches; batch += 1) {
    const started = performance.now();
    sink += work(batchSize);
    timings.push(performance.now() - started);
  }
  // `sink` is read so the loop cannot be eliminated as dead by any tier.
  if (sink === -1) {
    throw new Error("unreachable");
  }

  const first = timings[0];
  const warm = Math.min.apply(null, timings.slice(batches / 2));
  if (!(first > 0) || !(warm > 0)) {
    return unsupported(
      "performance.now() did not advance across a " +
        batchSize +
        "-iteration batch, so the warmup ratio cannot be computed"
    );
  }
  const ratio = first / warm;
  const evidence =
    "a " +
    batchSize +
    "-iteration integer loop took " +
    first.toFixed(1) +
    " ms cold and " +
    warm.toFixed(1) +
    " ms warm over " +
    batches +
    " batches";
  return ratio >= JIT_WARMUP_RATIO_THRESHOLD
    ? available(evidence, ratio.toFixed(2))
    : answer(
        "unavailable",
        evidence +
          ", a ratio of " +
          ratio.toFixed(2) +
          " below the " +
          JIT_WARMUP_RATIO_THRESHOLD +
          " cut; the tiers did not engage",
        ratio.toFixed(2)
      );
}

// --- the origin-scoped answers ---------------------------------------------

function probeSecureContext() {
  return self.isSecureContext
    ? available("isSecureContext === true at " + location.origin)
    : unavailable("isSecureContext === false at " + location.origin);
}

function probeCrossOriginIsolated() {
  if (typeof self.crossOriginIsolated === "undefined") {
    return unsupported(
      "crossOriginIsolated is not defined in this WebKit build at " + location.origin
    );
  }
  return self.crossOriginIsolated
    ? available("crossOriginIsolated === true at " + location.origin)
    : unavailable(
        "crossOriginIsolated === false at " +
          location.origin +
          "; the COOP/COEP headers were served but not honoured"
      );
}

// --- the request body -------------------------------------------------------
//
// A5. WebKit 191362 is RESOLVED FIXED, so this measures what ships rather than
// restating a bug report. Asked of THIS origin: for the custom scheme the other
// side is the WKURLSchemeHandler and for loopback it is the listener, and the
// question -- do the bytes arrive -- is the same one. Same-origin on purpose,
// so a CORS refusal cannot be recorded as a lost body.
function probeRequestBodyDelivery() {
  const payload = new Uint8Array([0x6d, 0x69, 0x67, 0x6f, 0x00, 0xff, 0x10, 0x20]);
  return fetch("echo-body", { method: "POST", body: payload })
    .then(function (response) {
      return response.arrayBuffer();
    })
    .then(function (buffer) {
      const echoed = new Uint8Array(buffer);
      if (echoed.length !== payload.length) {
        return unavailable(
          "the other side received " +
            echoed.length +
            " byte(s) of an " +
            payload.length +
            "-byte POST body"
        );
      }
      for (let i = 0; i < payload.length; i += 1) {
        if (echoed[i] !== payload[i]) {
          return unavailable(
            "the other side received " + echoed.length + " bytes that differ at index " + i
          );
        }
      }
      return available(
        "an " + payload.length + "-byte POST body arrived unchanged at " + location.origin
      );
    })
    .catch(function (error) {
      return unavailable("the POST to " + location.origin + " failed: " + error);
    });
}

// --- the worker half --------------------------------------------------------
//
// Atomics.wait, synchronous XHR and WebSocket are all asked inside a Dedicated
// Worker rather than here. Atomics.wait is forbidden on the main thread, and
// the other two answer a different question on the main thread than they do in
// the context the design would actually use.
function runWorkerProbes(workerUrl, loopbackOrigin) {
  return new Promise(function (resolve) {
    let worker;
    try {
      worker = new Worker(workerUrl);
    } catch (error) {
      resolve({
        shared_array_buffer: unsupported("the Worker could not be constructed: " + error),
        atomics_wait: unsupported("the Worker could not be constructed: " + error),
        sync_xhr_binary: unsupported("the Worker could not be constructed: " + error),
        websocket_in_worker: unsupported("the Worker could not be constructed: " + error),
        worker_raf: unsupported("the Worker could not be constructed: " + error)
      });
      return;
    }

    // A worker that never answers must not leave the run hanging: a probe that
    // hangs is reported as a probe that hung, which is a result.
    const timer = setTimeout(function () {
      worker.terminate();
      resolve({
        shared_array_buffer: answer("not_probed", "the Worker did not report within 30 s"),
        atomics_wait: answer("not_probed", "the Worker did not report within 30 s"),
        sync_xhr_binary: answer("not_probed", "the Worker did not report within 30 s"),
        websocket_in_worker: answer("not_probed", "the Worker did not report within 30 s"),
        worker_raf: answer("not_probed", "the Worker did not report within 30 s")
      });
    }, 30000);

    worker.onmessage = function (event) {
      clearTimeout(timer);
      worker.terminate();
      resolve(event.data);
    };
    worker.onerror = function (event) {
      clearTimeout(timer);
      worker.terminate();
      resolve({
        shared_array_buffer: unsupported("the Worker raised " + event.message),
        atomics_wait: unsupported("the Worker raised " + event.message),
        sync_xhr_binary: unsupported("the Worker raised " + event.message),
        websocket_in_worker: unsupported("the Worker raised " + event.message),
        worker_raf: unsupported("the Worker raised " + event.message)
      });
    };
    worker.postMessage({ loopbackOrigin: loopbackOrigin });
  });
}

// --- the run ----------------------------------------------------------------

function run(config) {
  const capabilities = {};

  capabilities.jit_enabled = probeJit();
  capabilities.cross_origin_isolated = probeCrossOriginIsolated();
  capabilities.secure_context = probeSecureContext();

  const pending = [
    probeRequestBodyDelivery().then(function (result) {
      capabilities.request_body_delivery = result;
    }),
    runWorkerProbes(config.workerUrl, config.loopbackOrigin).then(function (fromWorker) {
      Object.keys(fromWorker).forEach(function (name) {
        capabilities[name] = fromWorker[name];
      });
    })
  ];

  return Promise.all(pending).then(function () {
    // no_local_network_prompt is deliberately absent: it is an observation the
    // native side makes about whether an alert was presented, and a page cannot
    // see one. Reporting it here as anything at all would be the page answering
    // a question it cannot ask.
    return { origin_kind: ORIGIN_KIND, origin: location.origin, capabilities: capabilities };
  });
}

self.migoRunCapabilityProbe = function (config) {
  return run(config).then(function (report) {
    window.webkit.messageHandlers.migoProbe.postMessage(JSON.stringify(report));
    return report;
  });
};
