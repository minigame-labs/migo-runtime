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

// The contract's classes. Four of them span what the lane carries -- an input
// event, a small command batch, a frame's worth of draw commands, a texture
// upload -- and the three between 64 KiB and 1 MiB are there to locate the
// crossover, which the first run could only place inside a factor of sixteen.
// A hybrid has to switch at a size, and a range is not a size.
const TRANSPORT_PAYLOAD_CLASSES = [64, 4096, 65536, 131072, 262144, 524288, 1048576];
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

/// The host's CPU and wakeups, read across the loopback listener.
///
/// Null when it cannot be read -- a page with no listener, a kernel that
/// declined -- and null travels into the record as null. A zero would be
/// indistinguishable from a batch that cost nothing.
async function hostUsage(loopbackOrigin) {
  if (!loopbackOrigin) {
    return null;
  }
  try {
    const response = await fetch(loopbackOrigin + "/usage", { cache: "no-store" });
    if (!response.ok) {
      return null;
    }
    return await response.json();
  } catch (error) {
    return null;
  }
}

function usageDelta(before, after) {
  if (!before || !after) {
    return { cpu_ms: null, wakeups: null };
  }
  const cpu = before.cpu_ms === null || after.cpu_ms === null
    ? null
    : Number((after.cpu_ms - before.cpu_ms).toFixed(3));
  const wakeups = before.wakeups === null || after.wakeups === null
    ? null
    : after.wakeups - before.wakeups;
  return { cpu_ms: cpu, wakeups: wakeups };
}

/// A batch, bracketed by two host-usage reads.
///
/// The reads sit outside the timed samples and inside the CPU window, so the
/// window carries two extra round trips of the host's own work. Against two
/// hundred samples that is under a percent, and stating it beats the
/// alternative: sampling the counter inside the loop would charge every sample
/// for the sampling.
async function measureBatchWithUsage(transport, payloadBytes, roundTrip, step, loopbackOrigin) {
  const before = await hostUsage(loopbackOrigin);
  const measurement = await measureBatch(transport, payloadBytes, roundTrip, step);
  const after = await hostUsage(loopbackOrigin);
  const delta = usageDelta(before, after);
  measurement.host_cpu_ms = delta.cpu_ms;
  measurement.host_wakeups = delta.wakeups;
  return measurement;
}

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

function measureSyncXhrInWorker(config) {
  return new Promise(function (resolve, reject) {
    let worker;
    try {
      worker = new Worker(config.workerUrl);
    } catch (error) {
      reject(error);
      return;
    }
    // Generous: the batch blocks its own thread, and the worker enforces its
    // own per-class deadline. This only catches a worker that never answers at
    // all, which is a different failure and needs a different message.
    const timer = setTimeout(function () {
      worker.terminate();
      reject(new Error("the sync-XHR worker did not report"));
    }, 120000);
    worker.onerror = function (error) {
      clearTimeout(timer);
      worker.terminate();
      reject(new Error("the sync-XHR worker errored: " + (error.message || error)));
    };
    worker.onmessage = function (event) {
      clearTimeout(timer);
      worker.terminate();
      // Summarised HERE, with the one implementation of the rule. The worker
      // reports raw timings and the clock it took them with; deciding whether
      // percentiles are reportable is the same decision for every arm and is
      // made in one place.
      const batches = (event.data && event.data.batches) || [];
      resolve(batches.map(function (batch) {
        const measurement = summarise(
          batch.transport, batch.payload_bytes, batch.timings,
          batch.clock_step_ms, batch.errors);
        measurement.host_cpu_ms = batch.host_cpu_ms;
        measurement.host_wakeups = batch.host_wakeups;
        return measurement;
      }));
    };
    worker.postMessage({
      kind: "transport",
      loopbackOrigin: config.loopbackOrigin,
      classes: TRANSPORT_PAYLOAD_CLASSES,
      samples: TRANSPORT_SAMPLES,
      timeoutMs: TRANSPORT_BATCH_TIMEOUT_MS
    });
  });
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
          await measureBatchWithUsage("loopback_websocket", bytes, function (payload) {
            return socketRoundTrip(socket, payload);
          }, step, config.loopbackOrigin));
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

  // A21's arm, and it has to run in a Worker: a synchronous request may block
  // one and may not block a Window. It is measured last because it blocks the
  // thread it runs on for the whole batch, and a socket callback that could not
  // be serviced while it ran would be measured as the socket's cost.
  if (config.loopbackOrigin && config.workerUrl) {
    try {
      measurements.push(...(await measureSyncXhrInWorker(config)));
    } catch (error) {
      measurements.push({
        transport: "sync_xhr_rpc",
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
    }
  }

  // Same-origin only. The scheme handler is reachable from the scheme origin
  // and from nowhere else, which is the point of it.
  if (location.origin === config.schemeOrigin) {
    const url = config.schemeOrigin + "/echo-body";
    for (const bytes of TRANSPORT_PAYLOAD_CLASSES) {
      measurements.push(
        await measureBatchWithUsage("scheme_request", bytes, function (payload) {
          return schemeRoundTrip(url, payload);
        }, step, config.loopbackOrigin));
    }
  }

  return measurements;
};
