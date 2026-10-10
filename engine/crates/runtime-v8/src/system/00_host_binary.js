// Binary values on the host channels.
//
// Host requests, results and events are JSON, which has no bytes. A value that
// is binary to content -- a Bluetooth characteristic, a frame-sync action --
// crosses as lower-case hex, two digits per byte, and reaches content as an
// ArrayBuffer again. One encoding for every channel, so a host writes one codec.

const DIGITS = '0123456789abcdef';
const BYTE_HEX = new Array(256);
for (let i = 0; i < 256; i++) BYTE_HEX[i] = DIGITS[i >> 4] + DIGITS[i & 15];

// A hex digit's value by its character code, or -1.
const NIBBLE = new Int8Array(128).fill(-1);
for (let i = 0; i < 10; i++) NIBBLE[48 + i] = i;
for (let i = 0; i < 6; i++) {
    NIBBLE[97 + i] = 10 + i;
    NIBBLE[65 + i] = 10 + i;
}

/** The bytes of an ArrayBuffer or a view of one, as hex; null for anything else. */
function bytesToHex(value) {
    let bytes;
    if (value instanceof ArrayBuffer) {
        bytes = new Uint8Array(value);
    } else if (ArrayBuffer.isView(value)) {
        bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    } else {
        return null;
    }
    let hex = '';
    for (let i = 0; i < bytes.length; i++) hex += BYTE_HEX[bytes[i]];
    return hex;
}

/** Hex as an ArrayBuffer; null when it is not hex. */
function hexToBytes(hex) {
    if (typeof hex !== 'string' || (hex.length & 1) !== 0) return null;
    const bytes = new Uint8Array(hex.length >> 1);
    for (let i = 0; i < bytes.length; i++) {
        const high = hex.charCodeAt(2 * i);
        const low = hex.charCodeAt(2 * i + 1);
        const h = high < 128 ? NIBBLE[high] : -1;
        const l = low < 128 ? NIBBLE[low] : -1;
        if (h < 0 || l < 0) return null;
        bytes[i] = (h << 4) | l;
    }
    return bytes.buffer;
}

export { bytesToHex, hexToBytes };
