// The page half: start the producer worker, and relay between it and the host.
//
// It is small on purpose. Everything with a decision in it lives in the worker,
// where content also runs, so the page cannot become a second place that knows
// about frames -- and a page that knew about frames would be a second agent on
// the latency path, which is the arrangement `README.md` records the capability
// gate eliminating.
//
// The host injects its configuration at document start rather than serving it as
// a module, because the socket's port is ephemeral: it exists only after the
// channel is listening, which is after this file was packaged and before this
// page was loaded.

const HOST_CHANNEL = "migoPerformancePlus";

/** Tell the host something. Never throws: a page that cannot report is still a
 *  page that should keep running, and the host already treats silence as a
 *  failure it can see. */
function report(message) {
  try {
    globalThis.webkit?.messageHandlers?.[HOST_CHANNEL]?.postMessage(message);
  } catch {
    // The handler is gone, which happens while a page is being torn down.
  }
}

const config = globalThis.__migoPerformancePlusConfig;
if (!config || typeof config.frameChannelUrl !== "string") {
  report({ type: "failed", stage: "configuration", detail: "no frame channel URL was injected" });
} else {
  let worker;
  try {
    // `new URL(..., import.meta.url)`, so the worker is resolved against THIS
    // module rather than against the document. The two differ as soon as the
    // engine's modules move, and a string here would silently start resolving
    // against the content package -- which is the one directory a game controls.
    worker = new Worker(new URL("./producer-worker.mjs", import.meta.url), { type: "module" });
  } catch (error) {
    // A construction failure is reported rather than thrown, because a throw
    // here reaches nobody: there is no content to catch it and the host sees
    // only that nothing ever connected.
    report({ type: "failed", stage: "worker-construction", detail: String(error) });
  }

  if (worker) {
    worker.onmessage = (event) => report(event.data);
    worker.onerror = (event) => {
      report({
        type: "failed",
        stage: "worker",
        detail: String(event.message || event.type || "error"),
      });
    };
    worker.postMessage({ type: "start", config });
  }
}
