// G0's P3: what a channel costs, measured at both candidate origins.
//
// The capability probe answers whether a channel exists. This answers what it
// costs to use, which is the question that picks one. It runs in the same page
// and the same visit, because the alternative -- a second app mode, a second
// launch, a second collection -- would measure the two things on two device
// states and then compare them.
//
// WHAT A SAMPLE IS. One round trip: N bytes handed to the transport, the same N
// bytes back. The echo is the whole protocol. Nothing parses the payload and
// nothing draws with it, so the number is the channel's and not the renderer's.
//
// MEMORY. One buffer per payload class, reused for every sample of that class,
// and every echo dropped the moment its length is read. The largest class is a
// mebibyte and the largest batch is hundreds of samples: allocating per sample
// would move hundreds of mebibytes through a phone's heap and measure the
// garbage collector.
//
// CORRECTNESS IS CHECKED ONCE PER CLASS, not per sample. That the channel
// delivers bytes unchanged is a capability question and `request_body_delivery`
// already answers it; comparing a mebibyte on every sample would measure the
// comparison. The first sample of each class is compared in full, and the rest
// are checked by length -- a channel that starts truncating halfway through a
// batch is caught, and one that corrupts a byte silently is not, which is the
// trade this comment exists to make visible.

"use strict";

// The contract's four classes: an input event, a small command batch, a frame's
// worth of draw commands, a texture upload.
const TRANSPORT_PAYLOAD_CLASSES = [64, 4096, 65536, 1048576];
const TRANSPORT_SAMPLES = 200;
// Per batch, not per sample. A batch that cannot finish in this is reported as
// an error with however many samples it took, rather than hanging the visit.
const TRANSPORT_BATCH_TIMEOUT_MS = 20000;
// Below this the clock can resolve one round trip and percentiles mean
// something. Above it -- the custom scheme reads 1.000 ms -- they do not, and
// the record says null rather than reporting the clock's shape as the
// transport's.
const TRANSPORT_FINE_CLOCK_MS = 0.5;

function transportClockStepMs() {
  const started = Date.now();
  const t0 = performance.now();
  let t1 = t0;
  while (t1 === t0 && Date.now() - started < 50) {
    t1 = performance.now();
  }
  return t1 - t0;
}

function filledBuffer(bytes) {
  const buffer = new Uint8Array(bytes);
  // A pattern rather than zeros: a transport that answers with a zero-filled
  // buffer of the right length would pass a length check and a memcmp against
  // zeros, and this is the one sample per class that is compared in full.
  for (let i = 0; i < bytes; i += 1) {
    buffer[i] = (i * 31 + 7) & 0xff;
  }
  return buffer;
}

function sameBytes(sent, received) {
  if (sent.byteLength !== received.byteLength) {
    return false;
  }
  for (let i = 0; i < sent.byteLength; i += 1) {
    if (sent[i] !== received[i]) {
      return false;
    }
  }
  return true;
}

function summarise(transport, payloadBytes, timings, step, errors) {
  const measurement = {
    transport: transport,
    payload_bytes: payloadBytes,
    samples: timings.length,
    clock_step_ms: Number(step.toFixed(3)),
    errors: errors,
    mean_round_trip_ms: null,
    p50_round_trip_ms: null,
    p95_round_trip_ms: null,
    p99_round_trip_ms: null
  };
  if (timings.length === 0) {
    return measurement;
  }
  let total = 0;
  for (let i = 0; i < timings.length; i += 1) {
    total += timings[i];
  }
  // The mean survives a coarse clock: a batch of hundreds of round trips takes
  // hundreds of milliseconds, and 1 ms of quantisation on the total is a
  // rounding error. It is computed from the per-sample readings rather than
  // from one span so that a batch cut short by an error still reports what it
  // measured.
  measurement.mean_round_trip_ms = Number((total / timings.length).toFixed(4));
  if (step >= TRANSPORT_FINE_CLOCK_MS) {
    return measurement;
  }
  const sorted = timings.slice().sort(function (a, b) { return a - b; });
  function percentile(fraction) {
    const index = Math.min(sorted.length - 1, Math.floor(fraction * sorted.length));
    return Number(sorted[index].toFixed(4));
  }
  measurement.p50_round_trip_ms = percentile(0.5);
  measurement.p95_round_trip_ms = percentile(0.95);
  measurement.p99_round_trip_ms = percentile(0.99);
  return measurement;
}

// --- the loopback WebSocket -------------------------------------------------

function openEchoSocket(loopbackOrigin) {
  return new Promise(function (resolve, reject) {
    const url = loopbackOrigin.replace(/^http/, "ws") + "/echo";
    let socket;
    try {
      socket = new WebSocket(url);
    } catch (error) {
      reject(error);
      return;
    }
    socket.binaryType = "arraybuffer";
    const timer = setTimeout(function () {
      reject(new Error("the echo socket did not open"));
    }, 5000);
    socket.onopen = function () {
      clearTimeout(timer);
      resolve(socket);
    };
    socket.onerror = function () {
      clearTimeout(timer);
      reject(new Error("the echo socket errored while opening"));
    };
  });
}

function socketRoundTrip(socket, payload) {
  return new Promise(function (resolve, reject) {
    socket.onmessage = function (event) {
      resolve(new Uint8Array(event.data));
    };
    socket.onerror = function () {
      reject(new Error("the echo socket errored mid-batch"));
    };
    socket.send(payload);
  });
}

// --- the custom scheme's request path ---------------------------------------

function schemeRoundTrip(url, payload) {
  return fetch(url, { method: "POST", body: payload }).then(function (response) {
    if (!response.ok) {
      throw new Error("the scheme handler answered " + response.status);
    }
    return response.arrayBuffer().then(function (buffer) {
      return new Uint8Array(buffer);
    });
  });
}

// --- the batch --------------------------------------------------------------

async function measureBatch(transport, payloadBytes, roundTrip, step) {
  const payload = filledBuffer(payloadBytes);
  const timings = [];
  let errors = 0;
  const deadline = Date.now() + TRANSPORT_BATCH_TIMEOUT_MS;
  for (let sample = 0; sample < TRANSPORT_SAMPLES; sample += 1) {
    if (Date.now() > deadline) {
      errors += 1;
      break;
    }
    const started = performance.now();
    let echoed;
    try {
      echoed = await roundTrip(payload);
    } catch (error) {
      errors += 1;
      break;
    }
    const elapsed = performance.now() - started;
    if (sample === 0 ? !sameBytes(payload, echoed) : echoed.byteLength !== payloadBytes) {
      errors += 1;
      break;
    }
    timings.push(elapsed);
  }
  return summarise(transport, payloadBytes, timings, step, errors);
}

/// Every transport this origin can reach, at every payload class.
///
/// A transport this origin cannot use produces NO measurement rather than a
/// zero: a page at the loopback origin cannot POST to the custom scheme, and
/// recording that as a round trip of zero milliseconds would make the transport
/// look infinitely fast in exactly the comparison it cannot enter.
self.migoMeasureTransports = async function (config) {
  const step = transportClockStepMs();
  const measurements = [];

  if (config.loopbackOrigin) {
    let socket = null;
    try {
      socket = await openEchoSocket(config.loopbackOrigin);
      for (const bytes of TRANSPORT_PAYLOAD_CLASSES) {
        measurements.push(
          await measureBatch("loopback_websocket", bytes, function (payload) {
            return socketRoundTrip(socket, payload);
          }, step));
      }
    } catch (error) {
      measurements.push({
        transport: "loopback_websocket",
        payload_bytes: 0,
        samples: 0,
        clock_step_ms: Number(step.toFixed(3)),
        errors: 1,
        mean_round_trip_ms: null,
        p50_round_trip_ms: null,
        p95_round_trip_ms: null,
        p99_round_trip_ms: null,
        note: String(error)
      });
    } finally {
      if (socket) {
        socket.close();
      }
    }
  }

  // Same-origin only. The scheme handler is reachable from the scheme origin
  // and from nowhere else, which is the point of it.
  if (location.origin === config.schemeOrigin) {
    const url = config.schemeOrigin + "/echo-body";
    for (const bytes of TRANSPORT_PAYLOAD_CLASSES) {
      measurements.push(
        await measureBatch("scheme_request", bytes, function (payload) {
          return schemeRoundTrip(url, payload);
        }, step));
    }
  }

  return measurements;
};
