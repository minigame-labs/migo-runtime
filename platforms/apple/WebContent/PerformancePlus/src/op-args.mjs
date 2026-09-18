// An op's arguments, converted the way deno_core converts them before the Rust
// op body sees them.
//
// The engine's facades call ops with the values content gave them, and a lane
// that answers an op here has to see the same number the Rust op would have --
// otherwise a call that is an error in one runtime is a different call in the
// other. deno_core's rules (runtime/ops.rs `to_u32_option`, `to_i32_option`, and
// op2's buffer and string arguments) are small and fixed, so they are restated
// here and tested in op-args.test.mjs against the values that tell them apart:
// -1, fractions, NaN, infinities, 2^32 and above, BigInt, and non-numbers.

const TWO_32 = 4294967296;
const TWO_63 = 9223372036854775808;
const TWO_64 = 18446744073709551616;

function refuse(name, expected, value) {
  const kind = value === null ? "null" : typeof value;
  return new TypeError(`${name}: expected ${expected}, got ${kind}`);
}

/**
 * `#[smi] u32`. An integer that is a uint32 or an int32 is taken as its 32
 * bits (-1 is 0xFFFFFFFF); any other Number goes through `as u64`, which
 * truncates toward zero and saturates (NaN and negatives to 0, past 2^64 to the
 * maximum), then keeps the low 32 bits; a BigInt keeps its low 64 then low 32.
 */
export function toU32(value, name) {
  if (typeof value === "number") {
    if (Number.isInteger(value) && value >= -2147483648 && value < TWO_32) return value >>> 0;
    return u64Bits(value) % TWO_32;
  }
  if (typeof value === "bigint") return Number(BigInt.asUintN(32, value));
  throw refuse(name, "a number", value);
}

/**
 * `#[smi] i32`. A uint32 wraps to its two's complement, an int32 is itself; any
 * other Number goes through `as i64` (saturating, NaN to 0) then keeps the low
 * 32 bits; a BigInt the low 32 of its 64.
 */
export function toI32(value, name) {
  if (typeof value === "number") {
    if (Number.isInteger(value) && value >= -2147483648 && value < TWO_32) return value | 0;
    return i64Bits(value) | 0;
  }
  if (typeof value === "bigint") return Number(BigInt.asIntN(32, value));
  throw refuse(name, "a number", value);
}

/** `f64 as u64`, as a Number exact in its low 32 bits. */
function u64Bits(value) {
  if (!(value > 0)) return 0; // NaN, zero and negatives
  if (value >= TWO_64) return TWO_32 - 1; // u64::MAX, whose low 32 bits are all ones
  const whole = Math.trunc(value);
  // Past 2^53 a double is an integer already; its low 32 bits come from the
  // exact BigInt, not from `%` on a rounded Number.
  return whole < Number.MAX_SAFE_INTEGER ? whole : Number(BigInt(whole) & 0xffffffffn);
}

/** `f64 as i64` then the low 32 bits, as a signed 32-bit Number. */
function i64Bits(value) {
  if (Number.isNaN(value)) return 0;
  if (value >= TWO_63) return -1; // i64::MAX: low 32 bits all ones
  if (value <= -TWO_63) return 0; // i64::MIN: low 32 bits zero
  const whole = Math.trunc(value);
  return Math.abs(whole) < Number.MAX_SAFE_INTEGER ? whole | 0 : Number(BigInt.asIntN(32, BigInt(whole)));
}

/** `#[buffer] &[u8]`: an ArrayBufferView, as its bytes. */
export function bytesOf(value, name) {
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  throw refuse(name, "an ArrayBufferView", value);
}

/** `#[buffer] Option<&[u8]>`: null or undefined is None. */
export function optionalBytesOf(value, name) {
  return value === null || value === undefined ? null : bytesOf(value, name);
}

/** `#[buffer(copy)] Vec<u32>`: a Uint32Array. */
export function u32ArrayOf(value, name) {
  if (value instanceof Uint32Array) return value;
  throw refuse(name, "a Uint32Array", value);
}

/** `#[string]`: a string, and nothing converted to one. */
export function stringOf(value, name) {
  if (typeof value === "string") return value;
  throw refuse(name, "a string", value);
}
