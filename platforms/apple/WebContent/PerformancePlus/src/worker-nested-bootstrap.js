// Classic Worker: retain importScripts for existing mini-game Worker scripts.
// The init event carries a private data port. Content never receives this port
// or the admission protocol, and global postMessage cannot forge its credits.
(() => {
const scriptPath = new URLSearchParams(self.location.search).get("script");
const importClassic = self.importScripts.bind(self);
importClassic(new URL("./worker-clone.js", self.location.href).href);
const { encodeMessage, createBudget } = self.__migoWorkerCloneBackend;
delete self.__migoWorkerCloneBackend;
const pushListener = Function.prototype.call.bind(Array.prototype.push);
const ErrorClass = Error;
let dataPort = null;
let post = null;
const messageListeners = [];
const budget = createBudget();

self.worker = {
  postMessage(message) {
    const { ticket, maxBytes } = budget.reserve(16 * 1024 * 1024);
    try {
      const wire = encodeMessage(message, maxBytes);
      budget.commit(ticket, wire);
      try { post({ k: "msg", seq: ticket, data: wire.value, bytes: wire.bytes }, wire.transfers); }
      catch (error) { budget.acknowledge(ticket); throw error; }
    } finally { budget.abort(ticket); }
  },
  onMessage(listener) {
    if (typeof listener !== "function") throw new TypeError("listener must be a function");
    pushListener(messageListeners, listener);
  },
};

self.onmessage = (event) => {
  if (dataPort !== null || event.data?.k !== "migo-worker-init" || !event.data.port) return;
  dataPort = event.data.port;
  post = dataPort.postMessage.bind(dataPort);
  self.onmessage = null;
  dataPort.onmessage = (event) => {
    const envelope = event.data;
    if (envelope.k === "ack") { budget.acknowledge(envelope.seq); return; }
    if (envelope.k !== "post") return;
    // Native MessagePort dequeue is the release boundary, before user code.
    post({ k: "ack", seq: envelope.seq });
    for (let index = 0; index < messageListeners.length; index++) {
      try { messageListeners[index]({ message: envelope.data }); }
      catch (error) { console.error("[Worker-JS] onMessage listener error: " + String(error?.stack || error)); }
    }
  };
  dataPort.start();
  if (!scriptPath) throw new ErrorClass("this worker's query string names no script to run");
  importClassic(scriptPath);
};
})();
