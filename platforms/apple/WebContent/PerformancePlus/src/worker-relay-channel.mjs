// Shared state behind content's `migo.createWorker` on Performance+, used by both `lane-async.mjs` (the three ops
// that return a promise the page settles) and `lane-command.mjs` (the two that are fire-and-forget in Rust and so
// must be fire-and-forget here too): a generator's static check reads each lane file's own exports for its op
// names, so the five cannot share one module under either file's name -- this is the module both import from
// instead of duplicating the state.
//
// See `worker-page-relay.mjs` for the page's half, and its own header for why a page, not a nested Worker of
// content's own, has to be the one constructing the real worker: content already runs in a Worker, and WebKit
// offers no nested-Worker constructor inside one.
//
// The wire here is a dedicated `MessagePort`, `engineHost().workerRelayPort` -- not the host's frame channel, and
// not `self.postMessage`: that name is the engine's once content has loaded (see `producer-worker.mjs`), and a
// background worker's messages were never on the frame path to begin with. A message or error crossing this port
// is always the content-relative JSON string `01_worker.js` already builds and parses; this moves it, it does not
// read it.
import { engineHost } from "./engine-host.mjs";

function workerRelayPort() {
  const port = engineHost().workerRelayPort;
  if (port === undefined) {
    const error = new Error("this host did not offer a worker relay");
    error.name = "WorkerError";
    throw error;
  }
  return port;
}

/** One pending reader's resolver, or a buffered item already waiting for one. FIFO either way. */
class WorkerAsyncQueue {
  #items = [];
  #waiters = [];
  #closed = false;

  push(item) {
    if (this.#closed) return;
    const waiter = this.#waiters.shift();
    if (waiter) waiter(item);
    else this.#items.push(item);
  }

  /** No more items are coming: every waiter still queued gets `undefined`, as does every later call. */
  close() {
    this.#closed = true;
    while (this.#waiters.length) this.#waiters.shift()(undefined);
  }

  next() {
    if (this.#items.length) return Promise.resolve(this.#items.shift());
    if (this.#closed) return Promise.resolve(undefined);
    return new Promise((resolve) => this.#waiters.push(resolve));
  }
}

let nextWorkerAttemptId = 1;
/** The one live attempt, or null. An id so a reply addressed to an attempt this content has already moved past (a
 *  terminated worker's last message, racing a restart) is dropped rather than misdelivered to the new one. */
let currentWorkerAttempt = null; // { id, messages, errors, createSettled: {resolve, reject} | null }

function onWorkerRelayMessage(event) {
  const message = event.data;
  if (!message || typeof message !== "object" || currentWorkerAttempt === null || message.id !== currentWorkerAttempt.id) {
    return;
  }
  switch (message.k) {
    case "created":
      currentWorkerAttempt.createSettled?.resolve();
      currentWorkerAttempt.createSettled = null;
      break;
    case "create-failed": {
      const error = new Error(message.detail || "worker construction failed");
      error.name = "WorkerError";
      currentWorkerAttempt.createSettled?.reject(error);
      currentWorkerAttempt.createSettled = null;
      break;
    }
    case "msg":
      currentWorkerAttempt.messages.push(message.data);
      break;
    case "err":
      currentWorkerAttempt.errors.push(message.detail);
      break;
    default:
      break;
  }
}

/** `op_worker_create`'s body, over an already-converted path. */
export function beginWorkerAttempt(path) {
  const port = workerRelayPort();
  // Idempotent: content's own one-worker-at-a-time rule means `currentWorkerAttempt` is already null here (a prior
  // worker was terminated first), so this never replaces a listener a live worker still needs.
  port.onmessage = onWorkerRelayMessage;
  const id = nextWorkerAttemptId++;
  return new Promise((resolve, reject) => {
    currentWorkerAttempt = {
      id,
      messages: new WorkerAsyncQueue(),
      errors: new WorkerAsyncQueue(),
      createSettled: { resolve, reject },
    };
    port.postMessage({ k: "create", id, path });
  });
}

/** `op_worker_post_message`'s body, over an already-converted JSON string. */
export function postToWorkerAttempt(json) {
  if (currentWorkerAttempt === null) return;
  workerRelayPort().postMessage({ k: "post", id: currentWorkerAttempt.id, data: json });
}

/** `op_worker_recv_message`'s body. */
export function nextWorkerMessage() {
  if (currentWorkerAttempt === null) {
    const error = new Error("no worker is running");
    error.name = "WorkerError";
    return Promise.reject(error);
  }
  return currentWorkerAttempt.messages.next();
}

/** `op_worker_recv_error`'s body. */
export function nextWorkerError() {
  if (currentWorkerAttempt === null) {
    const error = new Error("no worker is running");
    error.name = "WorkerError";
    return Promise.reject(error);
  }
  return currentWorkerAttempt.errors.next();
}

/** `op_worker_terminate`'s body. */
export function terminateWorkerAttempt() {
  if (currentWorkerAttempt === null) return;
  const { id, messages, errors, createSettled } = currentWorkerAttempt;
  currentWorkerAttempt = null;
  // A terminate racing a still-pending create (content can call `.terminate()` before `await` on the worker's own
  // readiness ever returns) must still settle that promise -- left pending, `#init()` never reaches its own
  // cleanup, and this content could never make another worker at all.
  createSettled?.reject(Object.assign(new Error("terminated before it was ready"), { name: "WorkerError" }));
  messages.close();
  errors.close();
  workerRelayPort().postMessage({ k: "term", id });
}
