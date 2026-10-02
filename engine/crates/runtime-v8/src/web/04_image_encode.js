// Encoding pixels as an image: what `canvas.toDataURL()` answers with.
//
// RGBA8 in, a PNG or a baseline JPEG out, in JavaScript, so that one implementation serves every lane (the embedded
// runtime and the Performance+ producer run this same file) and no native encoder has to be linked into a build that
// does not otherwise need one. Both are small and neither is fast: a megapixel takes a few hundred milliseconds,
// which is what a call that happens when a player takes a screenshot can afford.
//
// PNG: filters chosen per row by the usual sum-of-absolute-differences heuristic, then DEFLATE with LZ77 matching
// (hash chains over the 32 KiB window) and the fixed Huffman code. An opaque image is written as RGB, not RGBA.
// JPEG: baseline, 4:4:4, the standard quantisation tables scaled by quality, and Huffman tables built for the image
// (JPEG Annex K.2), which is smaller than the standard tables and does not depend on remembering them.
//
// `image/png` and `image/jpeg` are encoded; any other type is answered with a PNG, as the specification has it for a
// type the implementation does not support.

const PNG_SIGNATURE = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]);

// ---- checksums ----------------------------------------------------------------------------------------------------

let _crcTable = null;
function crc32(bytes, start, end, crc) {
    if (_crcTable === null) {
        _crcTable = new Int32Array(256);
        for (let n = 0; n < 256; n++) {
            let c = n;
            for (let k = 0; k < 8; k++) c = (c & 1) ? (0xedb88320 ^ (c >>> 1)) : (c >>> 1);
            _crcTable[n] = c;
        }
    }
    let c = crc ^ -1;
    for (let i = start; i < end; i++) c = _crcTable[(c ^ bytes[i]) & 255] ^ (c >>> 8);
    return c ^ -1;
}

function adler32(bytes) {
    let a = 1, b = 0;
    const n = bytes.length;
    let i = 0;
    while (i < n) {
        // 5552 is the longest run that cannot overflow 32 bits before the modulo.
        const stop = Math.min(i + 5552, n);
        for (; i < stop; i++) {
            a += bytes[i];
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    return ((b << 16) | a) >>> 0;
}

// ---- a growing byte buffer ----------------------------------------------------------------------------------------

class ByteSink {
    constructor(capacity) {
        this.bytes = new Uint8Array(capacity > 64 ? capacity : 64);
        this.length = 0;
    }
    reserve(extra) {
        const need = this.length + extra;
        if (need <= this.bytes.length) return;
        let size = this.bytes.length * 2;
        while (size < need) size *= 2;
        const grown = new Uint8Array(size);
        grown.set(this.bytes.subarray(0, this.length));
        this.bytes = grown;
    }
    byte(value) {
        if (this.length === this.bytes.length) this.reserve(1);
        this.bytes[this.length++] = value;
    }
    u16(value) { this.byte((value >>> 8) & 255); this.byte(value & 255); }
    u32(value) { this.byte((value >>> 24) & 255); this.byte((value >>> 16) & 255); this.byte((value >>> 8) & 255); this.byte(value & 255); }
    append(source) {
        this.reserve(source.length);
        this.bytes.set(source, this.length);
        this.length += source.length;
    }
    finish() { return this.bytes.subarray(0, this.length); }
}

// ---- DEFLATE (fixed Huffman, LZ77) ---------------------------------------------------------------------------------

const LENGTH_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

let _deflateTables = null;
function deflateTables() {
    if (_deflateTables !== null) return _deflateTables;
    const reverse = (code, bits) => {
        let r = 0;
        for (let i = 0; i < bits; i++) { r = (r << 1) | (code & 1); code >>>= 1; }
        return r;
    };
    // The fixed literal/length code, each code reversed because DEFLATE writes a Huffman code from its most
    // significant bit and everything else least-significant-first.
    const litCode = new Uint16Array(288);
    const litBits = new Uint8Array(288);
    for (let s = 0; s < 288; s++) {
        let code, bits;
        if (s < 144) { code = 0x30 + s; bits = 8; }
        else if (s < 256) { code = 0x190 + (s - 144); bits = 9; }
        else if (s < 280) { code = s - 256; bits = 7; }
        else { code = 0xc0 + (s - 280); bits = 8; }
        litCode[s] = reverse(code, bits);
        litBits[s] = bits;
    }
    const distCode = new Uint8Array(30);
    for (let d = 0; d < 30; d++) distCode[d] = reverse(d, 5);
    // length (3..258) -> its symbol index in LENGTH_BASE
    const lengthIndex = new Uint8Array(259);
    for (let i = 0; i < LENGTH_BASE.length; i++) {
        const top = i + 1 < LENGTH_BASE.length ? LENGTH_BASE[i + 1] : 259;
        for (let l = LENGTH_BASE[i]; l < top && l < 259; l++) lengthIndex[l] = i;
    }
    lengthIndex[258] = 28;
    // distance (1..32768) -> its symbol index in DIST_BASE
    const distIndex = new Uint8Array(32769);
    for (let i = 0; i < DIST_BASE.length; i++) {
        const top = i + 1 < DIST_BASE.length ? DIST_BASE[i + 1] : 32769;
        for (let d = DIST_BASE[i]; d < top; d++) distIndex[d] = i;
    }
    _deflateTables = { litCode, litBits, distCode, lengthIndex, distIndex };
    return _deflateTables;
}

// The zlib stream (header, one fixed-Huffman DEFLATE block, Adler-32) of `data`.
function zlibCompress(data) {
    const { litCode, litBits, distCode, lengthIndex, distIndex } = deflateTables();
    const n = data.length;
    const out = new ByteSink(Math.max(256, (n >>> 1) + 64));
    out.byte(0x78);
    out.byte(0x9c);
    let bitBuffer = 0, bitCount = 0;
    const put = (value, bits) => {
        bitBuffer |= value << bitCount;
        bitCount += bits;
        while (bitCount >= 8) {
            out.byte(bitBuffer & 255);
            bitBuffer >>>= 8;
            bitCount -= 8;
        }
    };
    put(1, 1); // the final block
    put(1, 2); // fixed Huffman

    const HASH_SIZE = 1 << 15;
    const WINDOW = 32768;
    const MAX_CHAIN = 24;
    const NICE = 96;
    const head = new Int32Array(HASH_SIZE).fill(-1);
    const prev = new Int32Array(WINDOW).fill(-1);
    let i = 0;
    while (i < n) {
        let bestLength = 0, bestDistance = 0;
        if (i + 2 < n) {
            const h = ((data[i] << 10) ^ (data[i + 1] << 5) ^ data[i + 2]) & (HASH_SIZE - 1);
            let candidate = head[h];
            let chain = MAX_CHAIN;
            const limit = n - i < 258 ? n - i : 258;
            while (candidate >= 0 && i - candidate <= WINDOW && chain-- > 0) {
                if (data[candidate + bestLength] === data[i + bestLength] || bestLength === 0) {
                    let length = 0;
                    while (length < limit && data[candidate + length] === data[i + length]) length++;
                    if (length > bestLength) {
                        bestLength = length;
                        bestDistance = i - candidate;
                        if (length >= NICE || length === limit) break;
                    }
                }
                candidate = prev[candidate & (WINDOW - 1)];
            }
        }
        let advance;
        if (bestLength >= 3) {
            const li = lengthIndex[bestLength];
            put(litCode[257 + li], litBits[257 + li]);
            if (LENGTH_EXTRA[li] > 0) put(bestLength - LENGTH_BASE[li], LENGTH_EXTRA[li]);
            const di = distIndex[bestDistance];
            put(distCode[di], 5);
            if (DIST_EXTRA[di] > 0) put(bestDistance - DIST_BASE[di], DIST_EXTRA[di]);
            advance = bestLength;
        } else {
            put(litCode[data[i]], litBits[data[i]]);
            advance = 1;
        }
        // Every position the step covers goes into the hash chains, so a later match can start inside this one.
        for (let k = 0; k < advance; k++, i++) {
            if (i + 2 < n) {
                const h = ((data[i] << 10) ^ (data[i + 1] << 5) ^ data[i + 2]) & (HASH_SIZE - 1);
                prev[i & (WINDOW - 1)] = head[h];
                head[h] = i;
            }
        }
    }
    put(litCode[256], litBits[256]); // end of block
    if (bitCount > 0) out.byte(bitBuffer & 255);
    out.u32(adler32(data));
    return out.finish();
}

// ---- PNG ---------------------------------------------------------------------------------------------------------

function pngChunk(sink, type, data) {
    sink.u32(data.length);
    const start = sink.length;
    for (let i = 0; i < 4; i++) sink.byte(type.charCodeAt(i));
    sink.append(data);
    sink.u32(crc32(sink.bytes, start, sink.length, 0) >>> 0);
}

function encodePng(rgba, width, height) {
    // An image with no transparency is stored as RGB: a quarter smaller before any compression.
    let opaque = true;
    for (let i = 3; i < rgba.length; i += 4) {
        if (rgba[i] !== 255) { opaque = false; break; }
    }
    const bpp = opaque ? 3 : 4;
    const rowBytes = width * bpp;
    const raw = opaque ? new Uint8Array(width * height * 3) : rgba;
    if (opaque) {
        for (let s = 0, d = 0; s < rgba.length; s += 4) {
            raw[d++] = rgba[s];
            raw[d++] = rgba[s + 1];
            raw[d++] = rgba[s + 2];
        }
    }

    // Each row is stored with the filter that makes it smallest, by the sum of the absolute differences read as
    // signed bytes: the heuristic the PNG specification recommends.
    const filtered = new Uint8Array((rowBytes + 1) * height);
    const candidates = [];
    for (let f = 0; f < 5; f++) candidates.push(new Uint8Array(rowBytes));
    const zeroRow = new Uint8Array(rowBytes);
    for (let y = 0; y < height; y++) {
        const row = raw.subarray(y * rowBytes, (y + 1) * rowBytes);
        const above = y > 0 ? raw.subarray((y - 1) * rowBytes, y * rowBytes) : zeroRow;
        const none = candidates[0], sub = candidates[1], up = candidates[2], average = candidates[3], paeth = candidates[4];
        for (let x = 0; x < rowBytes; x++) {
            const left = x >= bpp ? row[x - bpp] : 0;
            const upLeft = x >= bpp ? above[x - bpp] : 0;
            const a = above[x];
            none[x] = row[x];
            sub[x] = (row[x] - left) & 255;
            up[x] = (row[x] - a) & 255;
            average[x] = (row[x] - ((left + a) >> 1)) & 255;
            const p = left + a - upLeft;
            const pa = p > left ? p - left : left - p;
            const pb = p > a ? p - a : a - p;
            const pc = p > upLeft ? p - upLeft : upLeft - p;
            const predictor = pa <= pb && pa <= pc ? left : pb <= pc ? a : upLeft;
            paeth[x] = (row[x] - predictor) & 255;
        }
        let best = 0, bestScore = Infinity;
        for (let f = 0; f < 5; f++) {
            const c = candidates[f];
            let score = 0;
            for (let x = 0; x < rowBytes; x++) {
                const v = c[x];
                score += v < 128 ? v : 256 - v;
            }
            if (score < bestScore) { bestScore = score; best = f; }
        }
        filtered[y * (rowBytes + 1)] = best;
        filtered.set(candidates[best], y * (rowBytes + 1) + 1);
    }

    const out = new ByteSink(1024 + (filtered.length >>> 2));
    out.append(PNG_SIGNATURE);
    const header = new ByteSink(13);
    header.u32(width);
    header.u32(height);
    header.byte(8);
    header.byte(opaque ? 2 : 6);
    header.byte(0);
    header.byte(0);
    header.byte(0);
    pngChunk(out, "IHDR", header.finish());
    pngChunk(out, "IDAT", zlibCompress(filtered));
    pngChunk(out, "IEND", new Uint8Array(0));
    return out.finish();
}

// ---- JPEG --------------------------------------------------------------------------------------------------------

const ZIGZAG = new Uint8Array([
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
]);

const LUMA_QUANT = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62,
    18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const CHROMA_QUANT = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

let _cosines = null;
// cos((2x + 1) u pi / 16), scaled so that the separable transform below is the 8x8 DCT-II with its usual normalisation.
function dctMatrix() {
    if (_cosines !== null) return _cosines;
    _cosines = new Float64Array(64);
    for (let u = 0; u < 8; u++) {
        const scale = u === 0 ? Math.SQRT1_2 / 2 : 0.5;
        for (let x = 0; x < 8; x++) _cosines[u * 8 + x] = scale * Math.cos(((2 * x + 1) * u * Math.PI) / 16);
    }
    return _cosines;
}

// JPEG Annex K.2: the code length of each used symbol, from its frequency, with no length above 16. `frequencies` has
// 257 entries; the last is a reserved symbol of frequency 1 that takes the all-ones code, so no real code is all ones.
function huffmanLengths(frequencies) {
    const freq = new Float64Array(257);
    let used = false;
    for (let i = 0; i < 256; i++) { freq[i] = frequencies[i]; if (frequencies[i] > 0) used = true; }
    // A class no block used still needs a table the decoder will accept: one symbol.
    if (!used) freq[0] = 1;
    freq[256] = 1;
    const codesize = new Int32Array(257);
    const others = new Int32Array(257).fill(-1);
    for (;;) {
        // c1: the least frequent symbol (the highest index among ties), c2: the next
        let c1 = -1, v = 1e18;
        for (let i = 0; i <= 256; i++) if (freq[i] > 0 && freq[i] <= v) { v = freq[i]; c1 = i; }
        let c2 = -1; v = 1e18;
        for (let i = 0; i <= 256; i++) if (freq[i] > 0 && freq[i] <= v && i !== c1) { v = freq[i]; c2 = i; }
        if (c2 < 0) break;
        freq[c1] += freq[c2];
        freq[c2] = 0;
        codesize[c1]++;
        while (others[c1] >= 0) { c1 = others[c1]; codesize[c1]++; }
        others[c1] = c2;
        codesize[c2]++;
        while (others[c2] >= 0) { c2 = others[c2]; codesize[c2]++; }
    }
    const bits = new Int32Array(33);
    for (let i = 0; i <= 256; i++) if (codesize[i] > 0) bits[codesize[i]]++;
    for (let i = 32; i > 16; i--) {
        while (bits[i] > 0) {
            let j = i - 2;
            while (bits[j] === 0) j--;
            bits[i] -= 2;
            bits[i - 1]++;
            bits[j + 1] += 2;
            bits[j]--;
        }
    }
    // the reserved symbol's code is dropped: remove one code from the longest length in use
    let i = 16;
    while (bits[i] === 0) i--;
    bits[i]--;
    // symbols in order of increasing code length, by symbol value within a length
    const symbols = [];
    for (let length = 1; length <= 32; length++) {
        for (let s = 0; s < 256; s++) if (codesize[s] === length) symbols.push(s);
    }
    return { counts: bits.subarray(1, 17), symbols };
}

// The code and its length for each symbol, from `{ counts, symbols }`, in the canonical assignment.
function huffmanCodes(table) {
    const code = new Uint16Array(256);
    const size = new Uint8Array(256);
    let value = 0, k = 0;
    for (let length = 1; length <= 16; length++) {
        for (let n = 0; n < table.counts[length - 1]; n++) {
            code[table.symbols[k]] = value;
            size[table.symbols[k]] = length;
            value++;
            k++;
        }
        value <<= 1;
    }
    return { code, size };
}

function encodeJpeg(rgba, width, height, quality) {
    let q = Math.round(quality * 100);
    if (!(q >= 1)) q = 1;
    if (q > 100) q = 100;
    const scale = q < 50 ? 5000 / q : 200 - q * 2;
    const quantLuma = new Int32Array(64), quantChroma = new Int32Array(64);
    for (let i = 0; i < 64; i++) {
        const l = Math.floor((LUMA_QUANT[i] * scale + 50) / 100);
        const c = Math.floor((CHROMA_QUANT[i] * scale + 50) / 100);
        quantLuma[i] = l < 1 ? 1 : l > 255 ? 255 : l;
        quantChroma[i] = c < 1 ? 1 : c > 255 ? 255 : c;
    }
    const cosines = dctMatrix();
    const blocksX = (width + 7) >> 3, blocksY = (height + 7) >> 3;
    const blockCount = blocksX * blocksY;
    // Quantised coefficients in zigzag order: three components, 64 per block.
    const coefficients = [new Int16Array(blockCount * 64), new Int16Array(blockCount * 64), new Int16Array(blockCount * 64)];
    const planes = [new Float64Array(64), new Float64Array(64), new Float64Array(64)];
    const tmp = new Float64Array(64);
    for (let by = 0; by < blocksY; by++) {
        for (let bx = 0; bx < blocksX; bx++) {
            // The block's pixels as Y, Cb, Cr, composited over black (a JPEG has no alpha) and edge-replicated.
            for (let y = 0; y < 8; y++) {
                const py = Math.min(by * 8 + y, height - 1);
                for (let x = 0; x < 8; x++) {
                    const px = Math.min(bx * 8 + x, width - 1);
                    const at = (py * width + px) * 4;
                    const a = rgba[at + 3] / 255;
                    const r = rgba[at] * a, g = rgba[at + 1] * a, b = rgba[at + 2] * a;
                    const k = y * 8 + x;
                    planes[0][k] = 0.299 * r + 0.587 * g + 0.114 * b - 128;
                    planes[1][k] = -0.168736 * r - 0.331264 * g + 0.5 * b;
                    planes[2][k] = 0.5 * r - 0.418688 * g - 0.081312 * b;
                }
            }
            for (let c = 0; c < 3; c++) {
                const plane = planes[c];
                const quant = c === 0 ? quantLuma : quantChroma;
                // rows, then columns
                for (let y = 0; y < 8; y++) {
                    for (let u = 0; u < 8; u++) {
                        let sum = 0;
                        for (let x = 0; x < 8; x++) sum += cosines[u * 8 + x] * plane[y * 8 + x];
                        tmp[y * 8 + u] = sum;
                    }
                }
                const out = coefficients[c];
                const base = (by * blocksX + bx) * 64;
                for (let u = 0; u < 8; u++) {
                    for (let v = 0; v < 8; v++) {
                        let sum = 0;
                        for (let y = 0; y < 8; y++) sum += cosines[v * 8 + y] * tmp[y * 8 + u];
                        // sum is the coefficient at frequency (row v, column u); `index` is its raster position
                        const index = v * 8 + u;
                        const value = Math.round(sum / quant[index]);
                        out[base + ZIGZAG_INVERSE[index]] = value;
                    }
                }
            }
        }
    }

    // Statistics for the Huffman tables: DC differences and AC (run, size) symbols, luma and chroma separately.
    const bitLength = (value) => {
        let v = value < 0 ? -value : value;
        let n = 0;
        while (v > 0) { n++; v >>= 1; }
        return n;
    };
    const dcFreq = [new Float64Array(256), new Float64Array(256)];
    const acFreq = [new Float64Array(256), new Float64Array(256)];
    const forEachSymbol = (visit) => {
        const last = [0, 0, 0];
        for (let b = 0; b < blockCount; b++) {
            for (let c = 0; c < 3; c++) {
                const set = c === 0 ? 0 : 1;
                const block = coefficients[c];
                const base = b * 64;
                const diff = block[base] - last[c];
                last[c] = block[base];
                visit(set, 0, bitLength(diff), diff, true);
                let run = 0;
                for (let k = 1; k < 64; k++) {
                    const value = block[base + k];
                    if (value === 0) { run++; continue; }
                    while (run > 15) { visit(set, 0xf0, 0, 0, false); run -= 16; }
                    const size = bitLength(value);
                    visit(set, (run << 4) | size, size, value, false);
                    run = 0;
                }
                if (run > 0) visit(set, 0x00, 0, 0, false);
            }
        }
    };
    forEachSymbol((set, symbol, size, value, isDc) => {
        if (isDc) dcFreq[set][size]++;
        else acFreq[set][symbol]++;
    });
    const tables = {
        dc: [huffmanLengths(dcFreq[0]), huffmanLengths(dcFreq[1])],
        ac: [huffmanLengths(acFreq[0]), huffmanLengths(acFreq[1])],
    };
    const codes = {
        dc: [huffmanCodes(tables.dc[0]), huffmanCodes(tables.dc[1])],
        ac: [huffmanCodes(tables.ac[0]), huffmanCodes(tables.ac[1])],
    };

    const out = new ByteSink(1024 + width * height / 2);
    out.u16(0xffd8); // SOI
    // APP0 / JFIF
    out.u16(0xffe0); out.u16(16);
    for (const b of [0x4a, 0x46, 0x49, 0x46, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]) out.byte(b);
    // DQT: both tables, in zigzag order
    for (let t = 0; t < 2; t++) {
        out.u16(0xffdb); out.u16(67); out.byte(t);
        const quant = t === 0 ? quantLuma : quantChroma;
        for (let k = 0; k < 64; k++) out.byte(quant[ZIGZAG[k]]);
    }
    // SOF0: baseline, 8 bit, three components, no subsampling
    out.u16(0xffc0); out.u16(17); out.byte(8); out.u16(height); out.u16(width); out.byte(3);
    out.byte(1); out.byte(0x11); out.byte(0);
    out.byte(2); out.byte(0x11); out.byte(1);
    out.byte(3); out.byte(0x11); out.byte(1);
    // DHT
    const writeTable = (klass, id, table) => {
        out.u16(0xffc4);
        out.u16(2 + 1 + 16 + table.symbols.length);
        out.byte((klass << 4) | id);
        for (let i = 0; i < 16; i++) out.byte(table.counts[i]);
        for (const s of table.symbols) out.byte(s);
    };
    writeTable(0, 0, tables.dc[0]);
    writeTable(1, 0, tables.ac[0]);
    writeTable(0, 1, tables.dc[1]);
    writeTable(1, 1, tables.ac[1]);
    // SOS
    out.u16(0xffda); out.u16(12); out.byte(3);
    out.byte(1); out.byte(0x00);
    out.byte(2); out.byte(0x11);
    out.byte(3); out.byte(0x11);
    out.byte(0); out.byte(63); out.byte(0);

    // The entropy-coded data, with 0xff bytes stuffed.
    let bitBuffer = 0, bitCount = 0;
    const put = (value, bits) => {
        if (bits === 0) return;
        bitBuffer = (bitBuffer << bits) | (value & ((1 << bits) - 1));
        bitCount += bits;
        while (bitCount >= 8) {
            const byte = (bitBuffer >>> (bitCount - 8)) & 255;
            out.byte(byte);
            if (byte === 255) out.byte(0);
            bitCount -= 8;
        }
        bitBuffer &= (1 << bitCount) - 1;
    };
    forEachSymbol((set, symbol, size, value, isDc) => {
        const table = isDc ? codes.dc[set] : codes.ac[set];
        const key = isDc ? size : symbol;
        put(table.code[key], table.size[key]);
        // the magnitude bits: a negative value is its one's complement
        if (size > 0) put(value < 0 ? value + (1 << size) - 1 : value, size);
    });
    if (bitCount > 0) put((1 << (8 - bitCount)) - 1, 8 - bitCount); // pad with ones
    out.u16(0xffd9); // EOI
    return out.finish();
}

// raster index -> its position in the zigzag order
const ZIGZAG_INVERSE = new Uint8Array(64);
for (let k = 0; k < 64; k++) ZIGZAG_INVERSE[ZIGZAG[k]] = k;

// ---- the entry points --------------------------------------------------------------------------------------------

const BASE64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

function base64(bytes) {
    const n = bytes.length;
    let text = "";
    const chunk = [];
    for (let i = 0; i < n; i += 3) {
        const b0 = bytes[i], b1 = i + 1 < n ? bytes[i + 1] : 0, b2 = i + 2 < n ? bytes[i + 2] : 0;
        chunk.push(
            BASE64.charCodeAt(b0 >> 2),
            BASE64.charCodeAt(((b0 & 3) << 4) | (b1 >> 4)),
            i + 1 < n ? BASE64.charCodeAt(((b1 & 15) << 2) | (b2 >> 6)) : 61,
            i + 2 < n ? BASE64.charCodeAt(b2 & 63) : 61,
        );
        if (chunk.length >= 8192) {
            text += String.fromCharCode.apply(null, chunk);
            chunk.length = 0;
        }
    }
    if (chunk.length > 0) text += String.fromCharCode.apply(null, chunk);
    return text;
}

// The image type a `toDataURL` call names: `image/jpeg` is JPEG and anything else is PNG.
function imageMime(type) {
    return typeof type === "string" && type.trim().toLowerCase() === "image/jpeg" ? "image/jpeg" : "image/png";
}

// `quality` as `toDataURL` takes it: a number in [0, 1] for JPEG, the default 0.92 for anything else.
function imageQuality(quality) {
    return typeof quality === "number" && quality >= 0 && quality <= 1 ? quality : 0.92;
}

// `rgba`: width * height * 4 bytes, not premultiplied, rows top to bottom. `{ mime, bytes }`.
function encodeImage(rgba, width, height, type, quality) {
    const mime = imageMime(type);
    const bytes = mime === "image/jpeg"
        ? encodeJpeg(rgba, width, height, imageQuality(quality))
        : encodePng(rgba, width, height);
    return { mime, bytes };
}

function encodeDataUrl(rgba, width, height, type, quality) {
    const { mime, bytes } = encodeImage(rgba, width, height, type, quality);
    return "data:" + mime + ";base64," + base64(bytes);
}

export { encodeImage, encodeDataUrl, base64 };
