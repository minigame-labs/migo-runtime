// The text codecs, answered where the answer is owed.
//
// `migo.encodeMultiFormats(text, "utf16le")` and its decoding half are
// synchronous and take no host resource: they turn a string into bytes and
// back. The op boundary puts them on the local lane for that reason, and a
// local lane means the conversion happens here.
//
// THIS IS A PORT, NOT A SECOND OPINION. `shared::codec` is what the host runs,
// and `scripts/test-text-codec-agreement.sh` puts a corpus through both and
// requires the same bytes and the same refusals. The encodings where two
// implementations drift are the ones this file is careful about: `ucs2` is
// UTF-16 **big**-endian with a BOM in this API (not the UCS-2 anyone would
// guess), `ascii` masks rather than refuses, `latin1` refuses rather than
// masks, and `decode(base64)` ENCODES to base64 -- the op's own asymmetry,
// which content depends on.
//
// GBK is the one this side cannot do. WebKit decodes it (`TextDecoder("gbk")`)
// and encodes nothing but UTF-8, because the encoding standard removed the
// legacy encoders. So decoding works and encoding refuses, saying why, rather
// than shipping a 200 KB table or answering with mojibake.

const utf8Encoder = new TextEncoder();

/** The host's `normalize_encoding`, with its exact spellings. */
function normalize(encoding) {
  const text = String(encoding ?? "").trim();
  if (text.length === 0) return "utf8";
  const lower = text.toLowerCase();
  if (lower === "utf8" || lower === "utf-8") return "utf8";
  if (lower === "ucs2" || lower === "ucs-2") return "ucs2";
  if (lower === "utf16le" || lower === "utf-16le") return "utf16le";
  if (lower === "ascii") return "ascii";
  if (lower === "latin1" || lower === "binary") return "latin1";
  if (lower === "base64") return "base64";
  if (lower === "hex") return "hex";
  if (lower === "gbk") return "gbk";
  return text;
}

/** UTF-16 code units, which is what a JavaScript string already is. */
function codeUnits(text) {
  const units = new Uint16Array(text.length);
  for (let index = 0; index < text.length; index += 1) units[index] = text.charCodeAt(index);
  return units;
}

const BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/** Base64 of bytes, without `btoa`'s detour through a binary string. */
function toBase64(bytes) {
  let out = "";
  for (let index = 0; index < bytes.length; index += 3) {
    const a = bytes[index];
    const b = bytes[index + 1];
    const c = bytes[index + 2];
    out += BASE64_ALPHABET[a >> 2];
    out += BASE64_ALPHABET[((a & 3) << 4) | ((b ?? 0) >> 4)];
    out += b === undefined ? "=" : BASE64_ALPHABET[((b & 15) << 2) | ((c ?? 0) >> 6)];
    out += c === undefined ? "=" : BASE64_ALPHABET[c & 63];
  }
  return out;
}

/** The bytes a base64 string stands for, or null when it is not one. */
function fromBase64(text) {
  // The host strips a `data:` URL prefix, and only when it names base64:
  // `data:image/png;base64,AAAA`. Content passes both forms.
  const marker = text.length >= 5 && text.slice(0, 5).toLowerCase() === "data:" ? text.indexOf(";base64,") : -1;
  const pure = marker >= 0 ? text.slice(marker + ";base64,".length) : text;
  const clean = pure.replace(/\s+/g, "");
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(clean) || clean.length % 4 !== 0) return null;
  const out = new Uint8Array((clean.length / 4) * 3);
  let written = 0;
  for (let index = 0; index < clean.length; index += 4) {
    const chunk = [0, 1, 2, 3].map((at) => BASE64_ALPHABET.indexOf(clean[index + at]));
    const padding = clean[index + 2] === "=" ? 2 : clean[index + 3] === "=" ? 1 : 0;
    const a = chunk[0] < 0 ? 0 : chunk[0];
    const b = chunk[1] < 0 ? 0 : chunk[1];
    const c = chunk[2] < 0 ? 0 : chunk[2];
    const d = chunk[3] < 0 ? 0 : chunk[3];
    out[written] = (a << 2) | (b >> 4);
    if (padding < 2) out[written + 1] = ((b & 15) << 4) | (c >> 2);
    if (padding < 1) out[written + 2] = ((c & 3) << 6) | d;
    written += 3 - padding;
  }
  return out.subarray(0, written);
}

const HEX = "0123456789abcdef";

/**
 * A string as bytes, in one of the encodings the host's codec names.
 *
 * Throws with the host's own message where it refuses, so content sees one
 * failure rather than two spellings of it.
 */
export function encodeString(text, encoding) {
  const coding = normalize(encoding);
  switch (coding) {
    case "utf8":
      return utf8Encoder.encode(text);
    case "ascii": {
      // Node-style: the low seven bits of each code unit, not a refusal.
      const out = new Uint8Array(text.length);
      for (let index = 0; index < text.length; index += 1) out[index] = text.charCodeAt(index) & 0x7f;
      return out;
    }
    case "latin1": {
      const out = new Uint8Array(text.length);
      for (let index = 0; index < text.length; index += 1) {
        const code = text.charCodeAt(index);
        if (code > 0xff) throw new Error("Latin1 encode error: out of range");
        out[index] = code;
      }
      return out;
    }
    case "base64": {
      const bytes = fromBase64(text);
      if (bytes === null) throw new Error("Base64 decode error");
      return bytes;
    }
    case "hex": {
      if (text.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(text)) throw new Error("Hex decode error");
      const out = new Uint8Array(text.length / 2);
      for (let index = 0; index < out.length; index += 1) {
        out[index] = Number.parseInt(text.slice(index * 2, index * 2 + 2), 16);
      }
      return out;
    }
    case "utf16le": {
      const units = codeUnits(text);
      const out = new Uint8Array(units.length * 2);
      const view = new DataView(out.buffer);
      units.forEach((unit, index) => view.setUint16(index * 2, unit, true));
      return out;
    }
    case "ucs2": {
      // Big-endian with a BOM, which is this API's `ucs2` and nobody else's.
      const units = codeUnits(text);
      const out = new Uint8Array(2 + units.length * 2);
      out[0] = 0xfe;
      out[1] = 0xff;
      const view = new DataView(out.buffer);
      units.forEach((unit, index) => view.setUint16(2 + index * 2, unit, false));
      return out;
    }
    case "gbk":
      // The encoding standard removed every encoder but UTF-8, so WebKit has
      // no GBK encoder to ask. Refused rather than approximated: mojibake in a
      // file a game writes is worse than a call that failed.
      throw new Error("GBK encode error: this runtime has no GBK encoder");
    default:
      throw new Error(`Unsupported encoding: ${coding}`);
  }
}

/** Bytes as a string, in the same set of encodings. */
export function decodeBytes(bytes, encoding) {
  const coding = normalize(encoding);
  switch (coding) {
    case "utf8": {
      try {
        return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
      } catch {
        throw new Error("UTF-8 decode error");
      }
    }
    case "ascii": {
      let out = "";
      for (const byte of bytes) out += String.fromCharCode(byte & 0x7f);
      return out;
    }
    case "latin1": {
      let out = "";
      for (const byte of bytes) out += String.fromCharCode(byte);
      return out;
    }
    case "base64":
      // Yes, `decode` to base64: the op turns bytes into their base64 text,
      // which is the asymmetry content depends on.
      return toBase64(bytes);
    case "hex": {
      let out = "";
      for (const byte of bytes) out += HEX[byte >> 4] + HEX[byte & 15];
      return out;
    }
    case "utf16le": {
      if (bytes.byteLength % 2 !== 0) throw new Error("UTF-16LE decode error: odd number of bytes");
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      let out = "";
      for (let at = 0; at < bytes.byteLength; at += 2) out += String.fromCharCode(view.getUint16(at, true));
      return checkedUtf16(out, "UTF-16LE decode error");
    }
    case "ucs2": {
      const hasBom = bytes.byteLength >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff;
      const start = hasBom ? 2 : 0;
      if ((bytes.byteLength - start) % 2 !== 0) throw new Error("UCS-2 decode error: odd number of bytes");
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      let out = "";
      for (let at = start; at < bytes.byteLength; at += 2) out += String.fromCharCode(view.getUint16(at, false));
      return checkedUtf16(out, "UCS-2 decode error");
    }
    case "gbk": {
      try {
        return new TextDecoder("gbk", { fatal: true }).decode(bytes);
      } catch {
        throw new Error("GBK decode error");
      }
    }
    default:
      throw new Error(`Unsupported encoding: ${coding}`);
  }
}

/**
 * The host builds a `String` from the code units and fails on an unpaired
 * surrogate; JavaScript strings hold one happily, so the check is here.
 */
function checkedUtf16(text, message) {
  for (let index = 0; index < text.length; index += 1) {
    const code = text.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = text.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) throw new Error(message);
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      throw new Error(message);
    }
  }
  return text;
}
