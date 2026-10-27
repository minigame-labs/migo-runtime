import { core, primordials } from "ext:core/mod.js";
import { op_encode_multi_formats, op_decode_multi_formats } from "ext:core/ops";

const {
    Uint8Array, TypeError, StringPrototypeToLowerCase,
    ArrayBufferIsView, ArrayBufferPrototypeGetByteLength, TypedArrayPrototypeGetBuffer,
    TypedArrayPrototypeGetSymbolToStringTag,
    TypedArrayPrototypeGetByteLength, TypedArrayPrototypeGetByteOffset,
    DataViewPrototypeGetBuffer, DataViewPrototypeGetByteLength, DataViewPrototypeGetByteOffset,
} = primordials;
const { encode: encodeUtf8, decode: decodeUtf8 } = core;

function normalizeFormat(format) {
    return (typeof format === 'string' && format.length > 0) ? format : 'utf8';
}
function isUtf8(format) {
    const name = StringPrototypeToLowerCase(format);
    return name === 'utf8' || name === 'utf-8';
}
function normalizeBytes(data) {
    let buffer, offset, length, kind;
    if (ArrayBufferIsView(data)) {
        // The captured tag getter returns undefined for a DataView. These
        // intrinsic slot checks avoid three op crossings on a common buffer.
        kind = TypedArrayPrototypeGetSymbolToStringTag(data);
        if (kind !== undefined) {
            buffer = TypedArrayPrototypeGetBuffer(data);
            offset = TypedArrayPrototypeGetByteOffset(data);
            length = TypedArrayPrototypeGetByteLength(data);
        } else {
            buffer = DataViewPrototypeGetBuffer(data);
            offset = DataViewPrototypeGetByteOffset(data);
            length = DataViewPrototypeGetByteLength(data);
        }
    } else {
        buffer = data; offset = 0; length = ArrayBufferPrototypeGetByteLength(data);
    }
    // Validate an ordinary backing store, using its slot, before borrowing it
    // as Rust bytes. Shared backing stores are outside this codec profile.
    ArrayBufferPrototypeGetByteLength(buffer);
    if (kind === 'Uint8Array') return data;
    return new Uint8Array(buffer, offset, length);
}
function encode({ data, format = 'utf8' }) {
    if (typeof data !== 'string') throw new TypeError("encode({ data }) expects string");
    const coding = normalizeFormat(format);
    const result = isUtf8(coding) ? encodeUtf8(data) : op_encode_multi_formats(data, coding);
    return TypedArrayPrototypeGetBuffer(result);
}
function decode({ data, format = 'utf8' }) {
    const coding = normalizeFormat(format), bytes = normalizeBytes(data);
    if (isUtf8(coding)) {
        // core.decode strips a leading BOM; Migo's established codec preserves
        // it. Only the matching branch delegates to core's SIMD/ASCII decoder.
        if (TypedArrayPrototypeGetByteLength(bytes) >= 3 && bytes[0] === 239 && bytes[1] === 187 && bytes[2] === 191) {
            return op_decode_multi_formats(bytes, coding);
        }
        return decodeUtf8(bytes);
    }
    // Includes the platform GBK service and the existing strict non-UTF8 codec.
    return op_decode_multi_formats(bytes, coding);
}
export { encode, decode };
