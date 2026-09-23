// What a network op's arguments and answers are on this lane.
//
// `op_fetch` takes two `#[serde]` parameters -- the method and the header list,
// both `ByteString` -- and the conversion gate exempts `#[serde]` because there
// is no single rule to restate for it. So the rule is restated here, from
// serde_v8's `FromV8 for ByteString`: the value must be a string, every code
// unit must fit in one byte, and the bytes are those units. A string with a
// code unit above 0xFF is refused with serde_v8's own words, because that is
// what content sees in the embedded runtime.
//
// The answers are the two structs the ops return, which cross as arrays in
// field order (`service_network.rs` writes them) and are rebuilt here into the
// objects the engine's `04_request.js` reads.

/** How V8 names a value's type in serde_v8's refusal. */
function typeRepr(value) {
  if (value === null) return "Null";
  switch (typeof value) {
    case "undefined":
      return "Undefined";
    case "boolean":
      return "Boolean";
    case "number":
      return "Number";
    case "bigint":
      return "BigInt";
    case "symbol":
      return "Symbol";
    case "function":
      return "Function";
    default:
      return Array.isArray(value) ? "Array" : "Object";
  }
}

/**
 * `ByteString`: a string of one-byte code units, as its bytes.
 *
 * serde_v8 reads the string's one-byte representation, so `"é"` (U+00E9) is
 * the byte 0xE9 -- not its UTF-8 pair. Anything above U+00FF is refused.
 */
export function byteStringOf(value) {
  if (typeof value !== "string") {
    throw new TypeError(`serde_v8 error: invalid type; expected: string, got: ${typeRepr(value)}`);
  }
  const bytes = new Uint8Array(value.length);
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code > 0xff) throw new TypeError("serde_v8 error: invalid type, expected: latin1");
    bytes[index] = code;
  }
  return bytes;
}

/**
 * `Vec<(ByteString, ByteString)>`: the header list, written as an array of
 * two-element arrays of bytes. A member that is not a pair is refused as
 * serde_v8 refuses it, before anything is sent.
 */
export function writeHeaders(writer, headers) {
  if (!Array.isArray(headers)) {
    throw new TypeError(`serde_v8 error: invalid type; expected: array, got: ${typeRepr(headers)}`);
  }
  const pairs = headers.map((pair) => {
    if (!Array.isArray(pair) || pair.length !== 2) {
      throw new TypeError(`serde_v8 error: invalid type; expected: array, got: ${typeRepr(pair)}`);
    }
    return [byteStringOf(pair[0]), byteStringOf(pair[1])];
  });
  writer.array(pairs.length);
  for (const [name, value] of pairs) {
    writer.array(2);
    writer.bytes(name);
    writer.bytes(value);
  }
}

/** `FetchReturn`: `[requestRid, cancelHandleRid]`. */
export function fetchHandles([requestRid, cancelHandleRid]) {
  return { requestRid, cancelHandleRid };
}

/**
 * `FetchResponse`, in its field order. The headers come back as bytes, and the
 * engine's `Header` reads them as the strings the embedded op's `ByteString`
 * serialization gave it: one byte per code unit.
 */
export function fetchResponse([
  status,
  statusText,
  headers,
  url,
  responseRid,
  contentLength,
  remoteAddrIp,
  remoteAddrPort,
  error,
]) {
  return {
    status,
    statusText,
    headers: headers.map(([name, value]) => [latin1(name), latin1(value)]),
    url,
    responseRid,
    contentLength,
    remoteAddrIp,
    remoteAddrPort,
    error,
  };
}

/** Bytes as the string serde_v8 makes of a `ByteString`: one byte per unit. */
function latin1(bytes) {
  let text = "";
  for (let index = 0; index < bytes.length; index += 1) text += String.fromCharCode(bytes[index]);
  return text;
}
