// Page-side half of nested Worker support for content's `migo.createWorker`.
//
// Content cannot construct a Worker of its own: it already runs as one (see `producer-worker.mjs`'s own header),
// and WebKit does not offer a nested-Worker constructor inside a Worker's own global scope -- confirmed empirically,
// iPhone 12 / iOS 17.0.3, `typeof Worker === "undefined"` there. The page can: a Window context's Worker support has
// none of that gap. So content's requests cross the dedicated `MessagePort` `page-entry.mjs` hands the producer
// worker at "start", and this module answers them -- construct a real classic Worker running
// `worker-nested-bootstrap.js?script=<path>`, and relay its messages and errors back over the same port.
//
// Deliberately off the frame path: this port carries nothing `README.md`'s "a page that knew about frames" warns
// against -- a background worker's messages are not on the render loop, for this lane or the in-process one either.

const BOOTSTRAP_URL = new URL("./worker-nested-bootstrap.js", import.meta.url);

/** `{ id, worker } | null`: the one nested worker this page manages, mirroring content's own one-worker rule. */
let active = null;

function contentBaseURL() {
  // Against the package root, not `location.origin`: a custom scheme's origin is opaque ("null"), and this page's
  // own document sits under the engine's reserved `/__migo/` prefix, not where a content-relative path resolves.
  // The same rule `producer-worker.mjs` uses for `config.gameEntry`.
  return new URL("/", location.href);
}

function teardown(id) {
  if (active !== null && active.id === id) {
    try {
      active.worker.terminate();
    } catch {
      // Already gone.
    }
    active = null;
  }
}

/** Wire one dedicated port to the nested-worker protocol. Call once, with the port `page-entry.mjs` kept. */
export function bindWorkerRelay(port) {
  port.onmessage = (event) => {
    const message = event.data;
    if (!message || typeof message !== "object") return;
    switch (message.k) {
      case "create": {
        const { id, path } = message;
        if (active !== null) teardown(active.id);
        let worker;
        try {
          const scriptURL = new URL(path, contentBaseURL());
          const bootstrapURL = new URL(BOOTSTRAP_URL);
          bootstrapURL.search = "script=" + encodeURIComponent(scriptURL.href);
          worker = new Worker(bootstrapURL);
        } catch (error) {
          port.postMessage({ k: "create-failed", id, detail: String(error) });
          break;
        }
        active = { id, worker };
        worker.onmessage = (ev) => {
          if (active !== null && active.id === id) port.postMessage({ k: "msg", id, data: ev.data });
        };
        worker.onerror = (ev) => {
          if (active !== null && active.id === id) {
            port.postMessage({
              k: "err",
              id,
              detail: JSON.stringify({ message: ev.message, filename: ev.filename, lineno: ev.lineno }),
            });
          }
          // WebKit otherwise also logs an uncaught-exception report for this to the page's own console; that is
          // fine here, and `ev.preventDefault()` is not called, so it keeps doing it on every platform the same way.
        };
        port.postMessage({ k: "created", id });
        break;
      }
      case "post": {
        if (active !== null && active.id === message.id) active.worker.postMessage(message.data);
        break;
      }
      case "term": {
        teardown(message.id);
        break;
      }
      default:
        break;
    }
  };
}
