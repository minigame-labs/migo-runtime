// The image encoder behind `canvas.toDataURL()` (engine/crates/runtime-v8/src/web/04_image_encode.js), judged by decoding
// what it writes.
//
// PNG is decoded here with Node's own inflate and the PNG filters, and must come back byte for byte. JPEG is decoded by a
// small baseline decoder written for this test (Huffman, dequantisation, inverse DCT, YCbCr), independent of the encoder's
// transform, and must come back within what quality 0.92 allows. The same bytes are decoded by the engine's own decoders
// on every platform in the conformance suite (`canvas2d-spec/to-data-url-*`).
//
// Run:  node engine/crates/runtime-v8/tests/js/image-encode.test.mjs
// Gate: scripts/test-canvas-image-encode.sh

import { encodeDataUrl, encodeImage } from "../../src/web/04_image_encode.js";
import zlib from "node:zlib";

let failures = 0;
function check(condition, message) {
  if (condition) {
    console.log(`  ok   ${message}`);
  } else {
    failures += 1;
    console.log(`  FAIL ${message}`);
  }
}

function picture(width, height, transparent) {
  const px = new Uint8Array(width * height * 4);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4;
      px[i] = Math.floor((x * 255) / Math.max(1, width - 1));
      px[i + 1] = Math.floor((y * 255) / Math.max(1, height - 1));
      px[i + 2] = (x ^ y) & 1 ? 200 : 40;
      px[i + 3] = transparent ? ((x + y) % 7 === 0 ? 0 : (x * 7 + y * 3) % 256) : 255;
    }
  }
  return px;
}

// ---- PNG ----------------------------------------------------------------------------------------------------------

const crcTable = new Uint32Array(256);
for (let n = 0; n < 256; n++) {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  crcTable[n] = c >>> 0;
}
const crc = (bytes) => {
  let c = 0xffffffff;
  for (const v of bytes) c = crcTable[(c ^ v) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};

function decodePng(bytes) {
  const signature = [137, 80, 78, 71, 13, 10, 26, 10];
  if (!signature.every((v, i) => bytes[i] === v)) throw new Error("not a PNG");
  let offset = 8, width = 0, height = 0, colourType = 0, sawEnd = false;
  const data = [];
  while (offset < bytes.length) {
    const view = new DataView(bytes.buffer, bytes.byteOffset + offset);
    const length = view.getUint32(0);
    const type = String.fromCharCode(...bytes.subarray(offset + 4, offset + 8));
    const body = bytes.subarray(offset + 8, offset + 8 + length);
    if (crc(bytes.subarray(offset + 4, offset + 8 + length)) !== view.getUint32(8 + length)) throw new Error(`bad CRC in ${type}`);
    if (type === "IHDR") {
      const header = new DataView(body.buffer, body.byteOffset);
      width = header.getUint32(0);
      height = header.getUint32(4);
      colourType = body[9];
      if (body[8] !== 8 || body[10] !== 0 || body[11] !== 0 || body[12] !== 0) throw new Error("unexpected IHDR");
    } else if (type === "IDAT") {
      data.push(body);
    } else if (type === "IEND") {
      sawEnd = true;
    }
    offset += 12 + length;
  }
  if (!sawEnd || offset !== bytes.length) throw new Error("IEND is not the end");
  const raw = zlib.inflateSync(Buffer.concat(data));
  const bpp = colourType === 2 ? 3 : 4;
  const rowBytes = width * bpp;
  if (raw.length !== (rowBytes + 1) * height) throw new Error("wrong amount of image data");
  const out = new Uint8Array(width * height * 4);
  let previous = new Uint8Array(rowBytes);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (rowBytes + 1)];
    const row = raw.subarray(y * (rowBytes + 1) + 1, (y + 1) * (rowBytes + 1));
    const current = new Uint8Array(rowBytes);
    for (let x = 0; x < rowBytes; x++) {
      const a = x >= bpp ? current[x - bpp] : 0, b = previous[x], c = x >= bpp ? previous[x - bpp] : 0;
      let v = row[x];
      if (filter === 1) v += a;
      else if (filter === 2) v += b;
      else if (filter === 3) v += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      } else if (filter !== 0) throw new Error(`filter ${filter}`);
      current[x] = v & 255;
    }
    for (let x = 0; x < width; x++) {
      for (let k = 0; k < 3; k++) out[(y * width + x) * 4 + k] = current[x * bpp + k];
      out[(y * width + x) * 4 + 3] = bpp === 4 ? current[x * bpp + 3] : 255;
    }
    previous = current;
  }
  return { width, height, colourType, pixels: out };
}

console.log("PNG");
for (const [w, h, transparent] of [[1, 1, false], [2, 3, true], [37, 29, false], [37, 29, true], [300, 200, true], [640, 480, false]]) {
  const px = picture(w, h, transparent);
  const { mime, bytes } = encodeImage(px, w, h, "image/png");
  const decoded = decodePng(bytes);
  let differing = 0;
  for (let i = 0; i < px.length; i++) if (px[i] !== decoded.pixels[i]) differing++;
  check(mime === "image/png" && decoded.width === w && decoded.height === h, `${w}x${h}${transparent ? " with alpha" : ""}: a PNG of that size`);
  check(differing === 0, `${w}x${h}${transparent ? " with alpha" : ""}: every byte comes back (${bytes.length} bytes from ${px.length})`);
  check(decoded.colourType === (transparent ? 6 : 2), `${w}x${h}${transparent ? " with alpha" : ""}: ${transparent ? "RGBA" : "RGB, because nothing in it is transparent"}`);
}
{
  // A flat image is where the LZ77 matching has to pay for itself: a megabyte of white is about 4000 matches of 258 bytes,
  // 13 bits each under the fixed code, so a little over 6 KB.
  const flat = new Uint8Array(512 * 512 * 4).fill(255);
  const { bytes } = encodeImage(flat, 512, 512, "image/png");
  check(bytes.length < 8000, `a flat 512x512 image is ${bytes.length} bytes, not megabytes`);
  check(decodePng(bytes).pixels.every((v) => v === 255), "and still reads back white");
}

// ---- types, quality and the data URL ------------------------------------------------------------------------------

console.log("type, quality, URL");
{
  const px = picture(8, 8, false);
  check(encodeDataUrl(px, 8, 8, undefined).startsWith("data:image/png;base64,"), "no type is a PNG");
  check(encodeDataUrl(px, 8, 8, "image/webp").startsWith("data:image/png;base64,"), "a type that is not encoded is answered with a PNG");
  check(encodeDataUrl(px, 8, 8, "IMAGE/JPEG").startsWith("data:image/jpeg;base64,"), "the type is read without regard to case");
  const url = encodeDataUrl(px, 8, 8, "image/png");
  const bytes = Uint8Array.from(Buffer.from(url.slice(url.indexOf(",") + 1), "base64"));
  check(decodePng(bytes).width === 8, "the base64 in the URL is the PNG");
  const lengths = new Set();
  for (let n = 0; n < 6; n++) lengths.add(encodeDataUrl(new Uint8Array(4 * (n + 1)).fill(7), n + 1, 1, "image/png").length > 0);
  check(lengths.size === 1, "payloads of every length modulo 3 encode");
  const low = encodeImage(picture(64, 64, false), 64, 64, "image/jpeg", 0.1).bytes.length;
  const high = encodeImage(picture(64, 64, false), 64, 64, "image/jpeg", 0.95).bytes.length;
  check(low < high, `a lower quality is a smaller JPEG (${low} < ${high})`);
  const weird = encodeImage(picture(8, 8, false), 8, 8, "image/jpeg", 7).bytes.length;
  const dflt = encodeImage(picture(8, 8, false), 8, 8, "image/jpeg", 0.92).bytes.length;
  check(weird === dflt, "a quality outside [0, 1] is the default, 0.92");
}

// ---- JPEG ---------------------------------------------------------------------------------------------------------

const ZIGZAG = [
  0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
  35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

// A baseline decoder for exactly the files this encoder writes: 8 bit, three components, no subsampling, no restart markers.
function decodeJpeg(bytes) {
  let p = 2;
  if (bytes[0] !== 0xff || bytes[1] !== 0xd8) throw new Error("no SOI");
  const quant = {}, huffman = {};
  let width = 0, height = 0, components = [], scanStart = 0;
  while (p < bytes.length) {
    if (bytes[p] !== 0xff) throw new Error("expected a marker");
    const marker = bytes[p + 1];
    const length = (bytes[p + 2] << 8) | bytes[p + 3];
    const body = bytes.subarray(p + 4, p + 2 + length);
    if (marker === 0xdb) {
      for (let q = 0; q < body.length; q += 65) {
        const table = new Int32Array(64);
        for (let k = 0; k < 64; k++) table[ZIGZAG[k]] = body[q + 1 + k];
        quant[body[q] & 15] = table;
      }
    } else if (marker === 0xc0) {
      height = (body[1] << 8) | body[2];
      width = (body[3] << 8) | body[4];
      for (let c = 0; c < body[5]; c++) components.push({ id: body[6 + c * 3], q: body[8 + c * 3], sampling: body[7 + c * 3] });
    } else if (marker === 0xc4) {
      let q = 0;
      while (q < body.length) {
        const klass = body[q] >> 4, id = body[q] & 15;
        const counts = body.subarray(q + 1, q + 17);
        let total = 0;
        for (const c of counts) total += c;
        const symbols = body.subarray(q + 17, q + 17 + total);
        const map = new Map();
        let code = 0, k = 0;
        for (let len = 1; len <= 16; len++) {
          for (let n = 0; n < counts[len - 1]; n++) map.set(`${len}:${code++}`, symbols[k++]);
          code <<= 1;
        }
        huffman[`${klass}${id}`] = map;
        q += 17 + total;
      }
    } else if (marker === 0xda) {
      scanStart = p + 2 + length;
      for (let c = 0; c < components.length; c++) { components[c].dc = body[2 + c * 2] >> 4; components[c].ac = body[2 + c * 2] & 15; }
      break;
    }
    p += 2 + length;
  }
  // entropy-coded data, with the stuffed zero after each 0xff removed, up to EOI
  const bits = [];
  let q = scanStart;
  for (; q < bytes.length - 2; q++) {
    if (bytes[q] === 0xff) { if (bytes[q + 1] !== 0) throw new Error("a marker inside the scan"); q++; bits.push(0xff); } else bits.push(bytes[q]);
  }
  if (bytes[bytes.length - 2] !== 0xff || bytes[bytes.length - 1] !== 0xd9) throw new Error("no EOI");
  let bitPos = 0;
  const readBit = () => { const b = (bits[bitPos >> 3] >> (7 - (bitPos & 7))) & 1; bitPos++; return b; };
  const readBits = (n) => { let v = 0; for (let i = 0; i < n; i++) v = (v << 1) | readBit(); return v; };
  const decodeSymbol = (map) => {
    let code = 0;
    for (let len = 1; len <= 16; len++) {
      code = (code << 1) | readBit();
      const s = map.get(`${len}:${code}`);
      if (s !== undefined) return s;
    }
    throw new Error("a code that is in no table");
  };
  const extend = (v, n) => (n === 0 ? 0 : v < 1 << (n - 1) ? v - (1 << n) + 1 : v);
  const blocksX = (width + 7) >> 3, blocksY = (height + 7) >> 3;
  const planes = components.map(() => new Float64Array(blocksX * 8 * blocksY * 8));
  const last = components.map(() => 0);
  const cos = [];
  for (let x = 0; x < 8; x++) { cos.push([]); for (let u = 0; u < 8; u++) cos[x].push((u === 0 ? Math.SQRT1_2 : 1) * Math.cos(((2 * x + 1) * u * Math.PI) / 16)); }
  for (let by = 0; by < blocksY; by++) {
    for (let bx = 0; bx < blocksX; bx++) {
      components.forEach((component, ci) => {
        const coefficient = new Float64Array(64);
        const size = decodeSymbol(huffman[`0${component.dc}`]);
        last[ci] += extend(readBits(size), size);
        coefficient[0] = last[ci] * quant[component.q][0];
        for (let k = 1; k < 64;) {
          const rs = decodeSymbol(huffman[`1${component.ac}`]);
          const run = rs >> 4, s = rs & 15;
          if (s === 0) { if (run === 15) { k += 16; continue; } break; }
          k += run;
          coefficient[ZIGZAG[k]] = extend(readBits(s), s) * quant[component.q][ZIGZAG[k]];
          k++;
        }
        for (let y = 0; y < 8; y++) {
          for (let x = 0; x < 8; x++) {
            let sum = 0;
            for (let v = 0; v < 8; v++) for (let u = 0; u < 8; u++) sum += coefficient[v * 8 + u] * cos[x][u] * cos[y][v];
            planes[ci][(by * 8 + y) * blocksX * 8 + bx * 8 + x] = sum / 4;
          }
        }
      });
    }
  }
  const out = new Uint8Array(width * height * 4);
  const stride = blocksX * 8;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const Y = planes[0][y * stride + x] + 128, cb = planes[1][y * stride + x], cr = planes[2][y * stride + x];
      const clamp = (v) => Math.max(0, Math.min(255, Math.round(v)));
      const i = (y * width + x) * 4;
      out[i] = clamp(Y + 1.402 * cr);
      out[i + 1] = clamp(Y - 0.344136 * cb - 0.714136 * cr);
      out[i + 2] = clamp(Y + 1.772 * cb);
      out[i + 3] = 255;
    }
  }
  return { width, height, pixels: out };
}

console.log("JPEG");
for (const [w, h, transparent] of [[1, 1, false], [8, 8, false], [37, 29, false], [300, 200, true], [640, 480, false]]) {
  const px = picture(w, h, transparent);
  const { mime, bytes } = encodeImage(px, w, h, "image/jpeg", 0.92);
  const decoded = decodeJpeg(bytes);
  let sum = 0, count = 0, worst = 0;
  for (let i = 0; i < w * h; i++) {
    // a JPEG has no alpha: what is transparent is black, and the colour is composited over it
    const a = px[i * 4 + 3] / 255;
    for (let k = 0; k < 3; k++) {
      const err = Math.abs(decoded.pixels[i * 4 + k] - px[i * 4 + k] * a);
      sum += err;
      count++;
      if (err > worst) worst = err;
    }
  }
  check(mime === "image/jpeg" && decoded.width === w && decoded.height === h, `${w}x${h}${transparent ? " with alpha" : ""}: a JPEG of that size`);
  check(sum / count < 6, `${w}x${h}${transparent ? " with alpha" : ""}: mean error ${(sum / count).toFixed(2)} of 255 (worst ${worst.toFixed(0)}), ${bytes.length} bytes`);
}
{
  const flat = new Uint8Array(100 * 100 * 4).fill(0);
  for (let i = 0; i < flat.length; i += 4) { flat[i] = 90; flat[i + 1] = 180; flat[i + 2] = 30; flat[i + 3] = 255; }
  const decoded = decodeJpeg(encodeImage(flat, 100, 100, "image/jpeg", 0.92).bytes);
  check(Math.abs(decoded.pixels[0] - 90) < 3 && Math.abs(decoded.pixels[1] - 180) < 3 && Math.abs(decoded.pixels[2] - 30) < 3, "a flat colour stays that colour");
}

console.log(failures === 0 ? "PASS" : `FAIL (${failures})`);
process.exit(failures === 0 ? 0 : 1);
