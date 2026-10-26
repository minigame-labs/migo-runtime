// Platform-specific serializer. Only private wire buffers leave this module.
import { core, primordials } from "ext:core/mod.js";
const { Error, TypedArrayPrototypeGetByteLength } = primordials;
const { serialize } = core;

function cloneError(message) {
    const error = new Error(message);
    error.name = "DataCloneError";
    throw error;
}
function encodeMessage(value, maxBytes) {
    return serialize(value, { forStorage: true, maxBytes }, cloneError);
}
function decodeMessage(envelope) { return envelope[0]; }
function wireBytes(wire) { return TypedArrayPrototypeGetByteLength(wire); }
export { encodeMessage, decodeMessage, wireBytes };
