// Platform-specific serializer. Only private wire buffers leave this module.
import { primordials } from "ext:core/mod.js";
import { op_worker_serialize } from "ext:core/ops";
const { Error, TypedArrayPrototypeGetByteLength } = primordials;

function cloneError(message) {
    const error = new Error(message);
    error.name = "DataCloneError";
    throw error;
}
function encodeMessage(value, maxBytes) {
    return op_worker_serialize(value, maxBytes, cloneError);
}
function decodeMessage(envelope) { return envelope[0]; }
function wireBytes(wire) { return TypedArrayPrototypeGetByteLength(wire); }
export { encodeMessage, decodeMessage, wireBytes };
