import "./worker-clone.js";
const backend = globalThis.__migoWorkerCloneBackend;
delete globalThis.__migoWorkerCloneBackend;
export const { encodeMessage, decodeMessage, wireBytes, receivedWire, createBudget } = backend;
