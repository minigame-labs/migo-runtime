// This module is private. Content never receives serializer output or tickets.
// WebKit supplies a separate backend, using its own structured-clone protocol.
import { core, primordials } from "ext:core/mod.js";
import { op_worker_post_message, op_worker_reserve_message, op_worker_commit_message, op_worker_abort_message } from "ext:core/ops";
import { encodeMessage, decodeMessage, wireBytes } from "ext:host_v8_worker_codec/00_worker_serialize.js";

const { Error } = primordials;
class WorkerError extends Error {
    constructor(message) { super(message); this.name = "WorkerError"; }
}
core.registerErrorClass("WorkerError", WorkerError);

function postMessage(value) {
    op_worker_post_message(value, cloneError);
}

function cloneError(message) {
    const error = new Error(message);
    error.name = "DataCloneError";
    throw error;
}

function postEncodedMessage(wire) {
    const { ticket } = op_worker_reserve_message(wireBytes(wire));
    try { op_worker_commit_message(ticket, wire); }
    finally { op_worker_abort_message(ticket); }
}

export { encodeMessage, decodeMessage, wireBytes, postMessage, postEncodedMessage };
