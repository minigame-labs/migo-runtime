// The entry script for a real, nested WKWebView Worker the page constructs on content's behalf (see
// `worker-relay.mjs` and `page-entry.mjs`). Loaded as a classic (non-module) worker, because the script it runs --
// content's own, the same file the in-process lane runs under `02_worker_inner.js` -- is authored without
// import/export and uses `importScripts`, which only a classic worker has.
//
// This worker has no document and no `window`, so it needs none of `98_global_scope_worker.js`'s retirement of
// Window-only names: it is already the environment the in-process lane spends a whole file producing. What it does
// not have natively is the `worker.postMessage` / `worker.onMessage` shape `02_worker_inner.js` gives content -- the
// engine's own API, not the platform's `self.postMessage` / `self.onmessage` -- so that shape is built here, over
// this worker's own native primitives, with no Rust and no page relay involved: a message to or from content's
// script never needs to leave this thread.
//
// The user script's path arrives as this worker's own query string, not a message, so it can `importScripts`
// synchronously on its very first turn -- before anything content sends could possibly be lost. A throw out of it
// is left uncaught: a classic worker's uncaught throw is what the host Worker object's own `onerror` is for, and
// catching it here would only hide it from that.
const scriptPath = new URLSearchParams(self.location.search).get("script");

const messageListeners = [];

self.worker = {
    postMessage(message) {
        self.postMessage(JSON.stringify(message));
    },
    onMessage(listener) {
        if (typeof listener !== "function") {
            throw new TypeError("listener must be a function");
        }
        messageListeners.push(listener);
    },
};

self.onmessage = (event) => {
    const json = event.data;
    let message;
    try {
        message = JSON.parse(json);
    } catch (_) {
        message = json;
    }
    for (const listener of messageListeners) {
        try {
            listener({ message });
        } catch (error) {
            // Logged and swallowed, as `02_worker_inner.js`'s own listener group has it (`createListenerGroup`):
            // one listener's throw must not stop the others, and must not read as the worker itself crashing.
            console.error("[Worker-JS] onMessage listener error: " + String((error && error.stack) || error));
        }
    }
};

if (!scriptPath) {
    throw new Error("this worker's query string names no script to run");
}
importScripts(scriptPath);
