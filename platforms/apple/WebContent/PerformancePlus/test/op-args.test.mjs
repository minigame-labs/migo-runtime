// An op's arguments, converted as deno_core converts them.
//
// The expected values are the Rust ones: `to_u32_option` / `to_i32_option` in
// deno_core's runtime/ops.rs take an int32 or uint32 Number as its 32 bits, any
// other Number through `as u64` / `as i64` (truncating toward zero, saturating,
// NaN to 0) and then its low 32 bits, a BigInt by its low 64 then 32 bits, and
// refuse everything else.
//
// Run:  node test/op-args.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { bytesOf, optionalBytesOf, stringOf, toI32, toU32, u32ArrayOf } from "../src/op-args.mjs";

let failures = 0;
function check(name, fn) {
  try {
    fn();
    console.log(`  ok   ${name}`);
  } catch (error) {
    failures += 1;
    console.log(`  FAIL ${name}\n       ${error && error.message}`);
  }
}
function equal(actual, expected, what) {
  if (!Object.is(actual, expected)) throw new Error(`${what}: expected ${expected}, got ${actual}`);
}
function throws(fn, what) {
  try {
    fn();
  } catch (error) {
    if (error instanceof TypeError) return;
    throw error;
  }
  throw new Error(`${what}: did not throw`);
}

check("u32: integers in either 32-bit range are their 32 bits", () => {
  equal(toU32(0, "x"), 0, "0");
  equal(toU32(7, "x"), 7, "7");
  equal(toU32(-1, "x"), 0xffffffff, "-1");
  equal(toU32(-2147483648, "x"), 0x80000000, "i32::MIN");
  equal(toU32(4294967295, "x"), 0xffffffff, "u32::MAX");
});

check("u32: every other Number goes through `as u64`", () => {
  equal(toU32(1.9, "x"), 1, "1.9 truncates");
  equal(toU32(-1.5, "x"), 0, "negative saturates to 0");
  equal(toU32(-2147483649, "x"), 0, "below i32 saturates to 0, not wraps");
  equal(toU32(NaN, "x"), 0, "NaN");
  equal(toU32(Infinity, "x"), 0xffffffff, "+inf saturates to u64::MAX");
  equal(toU32(-Infinity, "x"), 0, "-inf");
  equal(toU32(4294967296, "x"), 0, "2^32 keeps its low bits");
  equal(toU32(4294967297.5, "x"), 1, "2^32 + 1.5");
  // Doubles near 2^60 are 256 apart, so this value is exact, as the case needs.
  equal(toU32(2 ** 60 + 2 ** 33 + 256, "x"), 256, "2^60 + 2^33 + 256, past 2^53");
  equal(toU32(2 ** 64, "x"), 0xffffffff, "2^64 saturates");
});

check("u32: a BigInt keeps its low bits", () => {
  equal(toU32(5n, "x"), 5, "5n");
  equal(toU32(-1n, "x"), 0xffffffff, "-1n");
  equal(toU32((1n << 40n) + 3n, "x"), 3, "2^40 + 3");
});

check("i32: uint32 wraps, int32 is itself", () => {
  equal(toI32(-1, "x"), -1, "-1");
  equal(toI32(4294967295, "x"), -1, "u32::MAX wraps");
  equal(toI32(2147483648, "x"), -2147483648, "2^31 wraps");
});

check("i32: every other Number goes through `as i64`", () => {
  equal(toI32(-1.5, "x"), -1, "-1.5 truncates toward zero");
  equal(toI32(NaN, "x"), 0, "NaN");
  equal(toI32(Infinity, "x"), -1, "+inf saturates to i64::MAX, low 32 bits all ones");
  equal(toI32(-Infinity, "x"), 0, "-inf saturates to i64::MIN, low 32 bits zero");
  equal(toI32(-4294967297, "x"), -1, "-(2^32 + 1)");
  equal(toI32(4294967296 + 7, "x"), 7, "2^32 + 7");
});

check("i32: a BigInt keeps its low bits, signed", () => {
  equal(toI32(-2n, "x"), -2, "-2n");
  equal(toI32((1n << 32n) - 1n, "x"), -1, "2^32 - 1");
});

check("anything that is not a number is refused, as the op refuses it", () => {
  for (const value of ["1", null, undefined, {}, true]) {
    throws(() => toU32(value, "x"), `u32 ${String(value)}`);
    throws(() => toI32(value, "x"), `i32 ${String(value)}`);
  }
});

check("buffers: any ArrayBufferView, as its bytes; null only where optional", () => {
  const source = new Uint16Array([0x0201, 0x0403, 0x0605]);
  const view = new Uint16Array(source.buffer, 2, 2);
  const bytes = bytesOf(view, "data");
  equal(bytes.byteLength, 4, "byte length");
  equal(bytes[0], 0x03, "starts at the view's offset");
  equal(optionalBytesOf(null, "data"), null, "null is None");
  equal(optionalBytesOf(undefined, "data"), null, "undefined is None");
  throws(() => bytesOf(null, "data"), "a required buffer refuses null");
  throws(() => bytesOf(new ArrayBuffer(4), "data"), "an ArrayBuffer is not a view");
});

check("u32 arrays and strings are taken only as themselves", () => {
  const list = new Uint32Array([1, 2]);
  equal(u32ArrayOf(list, "buffers"), list, "the same array");
  throws(() => u32ArrayOf([1, 2], "buffers"), "a plain array");
  throws(() => u32ArrayOf(new Int32Array(2), "buffers"), "an Int32Array");
  equal(stringOf("p", "name"), "p", "a string");
  throws(() => stringOf(1, "name"), "a number is not converted");
});

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
