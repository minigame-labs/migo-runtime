// An op's arguments, converted the way deno_core converts them before the Rust
// op body sees them.
//
// The engine's facades call ops with the values content gave them, and a lane
// that answers an op here has to see the value the Rust op would have --
// otherwise a call that is an error in one runtime is a different call in the
// other. There is one function per conversion deno_core applies, and which one
// applies is decided by the op's Rust signature, not by the type its body
// receives: `#[smi] u32` goes through `to_i32_option` and a plain `u32` through
// `to_u32_option`, and they differ for -1.5 and for everything below -2^31.
//
// Two gates hold this file to deno_core:
//
// - The conversions themselves are compared with V8's answers for the values
//   that tell them apart (test/op-args-agreement.mjs against
//   engine/crates/runtime-v8/src/tests/op_args_agreement.rs). Each function's
//   `@kind` names the parameter kinds it implements, in the vocabulary of
//   scripts/lib/runtime_ops.py's `param_kind`.
// - Every call in a lane names the Rust parameter it converts, as the second
//   argument, and scripts/gen-performance-plus-engine.py checks that the op has
//   that parameter and that its kind is one this function lists. The name is
//   not otherwise used: a conversion fails with deno_core's own message, which
//   does not name the parameter, because that message is the one content sees
//   in the embedded runtime.

const TWO_32 = 4294967296;
const TWO_63 = 9223372036854775808;
const TWO_64 = 18446744073709551616;

// deno_core's messages, verbatim: `throw_type_error(format!("expected {numeric}"))`
// in deno_ops, and to_v8_slice's refusal for a buffer.
const expected = (what) => new TypeError(`expected ${what}`);
const NOT_A_VIEW = "expected typed ArrayBufferView";

// The one property of a typed array that cannot be spoofed or subclassed away:
// the %TypedArray%.prototype[@@toStringTag] getter reads the internal slot, so
// it names the array's own kind -- across realms, for subclasses, and not for
// an object that merely claims a tag. V8's `IsUint8Array` answers the same
// question.
const typedArrayKind = Object.getOwnPropertyDescriptor(
  Object.getPrototypeOf(Uint8Array.prototype),
  Symbol.toStringTag,
).get;
const kindOf = (value) => typedArrayKind.call(value);

/** An int32 or uint32 Number: the two fast paths every integer conversion takes first. */
const isInt32Number = (value) => Number.isInteger(value) && value >= -2147483648 && value < TWO_32;

/**
 * `to_u32_option`: an int32 or uint32 Number is its 32 bits; any other Number
 * goes through `as u64` (truncating, saturating, NaN to 0) and keeps its low 32
 * bits; a BigInt keeps its low 64 then 32.
 * @kind u32
 */
export function toU32(value, name) {
  if (typeof value === "number") {
    if (isInt32Number(value)) return value >>> 0;
    return u64Bits(value) % TWO_32;
  }
  if (typeof value === "bigint") return Number(BigInt.asUintN(32, value));
  throw expected("u32");
}

/**
 * A plain `u8`: `to_u32_option`, then `as u8`.
 * @kind u8
 */
export function toU8(value, name) {
  return toU32(value, name) & 0xff;
}

/**
 * `to_i32_option`: a uint32 wraps, an int32 is itself; any other Number goes
 * through `as i64` (saturating, NaN to 0) and keeps its low 32 bits; a BigInt
 * keeps the low 32 of its 64. `#[smi] i32` converts the same way.
 * @kind i32, smi_i32
 */
export function toI32(value, name) {
  if (typeof value === "number") {
    if (isInt32Number(value)) return value | 0;
    return i64Bits(value) | 0;
  }
  if (typeof value === "bigint") return Number(BigInt.asIntN(32, value));
  throw expected("i32");
}

/**
 * `#[smi] u32`: `to_i32_option`, then `as u32`.
 * @kind smi_u32
 */
export function smiU32(value, name) {
  return toI32(value, name) >>> 0;
}

/**
 * `#[smi] Option<u32>`: null and undefined are None, anything else is
 * `#[smi] u32`.
 * @kind option_smi_u32
 */
export function optionalSmiU32(value, name) {
  return value === null || value === undefined ? null : smiU32(value, name);
}

/**
 * `Option<u32>`: null and undefined are None, anything else is `u32`.
 * @kind option_u32
 */
export function optionalU32(value, name) {
  return value === null || value === undefined ? null : toU32(value, name);
}

/**
 * `#[smi] u16`: `to_i32_option`, then `as u16`.
 * @kind smi_u16
 */
export function smiU16(value, name) {
  return toI32(value, name) & 0xffff;
}

/**
 * `#[smi] u8`: `to_i32_option`, then `as u8`.
 * @kind smi_u8
 */
export function smiU8(value, name) {
  return toI32(value, name) & 0xff;
}

/**
 * `#[smi] u64`: `to_i32_option`, then `as u64`, which sign-extends: -1 is
 * u64::MAX. A BigInt, because a Number cannot hold the result.
 * @kind smi_u64
 */
export function smiU64(value, name) {
  return BigInt.asUintN(64, BigInt(toI32(value, name)));
}

/**
 * `#[bigint] u64`: `to_u64_option`. A uint32 is itself and an int32 is
 * sign-extended; any other Number goes through `as u64` (truncating,
 * saturating, NaN to 0); a BigInt keeps its low 64 bits. A BigInt, always.
 * @kind bigint_u64
 */
export function toU64(value, name) {
  if (typeof value === "number") {
    if (isInt32Number(value)) {
      return value >= 0 ? BigInt(value) : BigInt.asUintN(64, BigInt(value));
    }
    if (!(value > 0)) return 0n; // NaN, zero and negatives
    if (value >= TWO_64) return 0xffffffffffffffffn;
    return BigInt(Math.trunc(value));
  }
  if (typeof value === "bigint") return BigInt.asUintN(64, value);
  throw expected("u64");
}

/**
 * `#[bigint] Option<u64>`: null and undefined are None, anything else is
 * `#[bigint] u64`.
 * @kind option_bigint_u64
 */
export function optionalU64(value, name) {
  return value === null || value === undefined ? null : toU64(value, name);
}

/**
 * `bool`: V8's `IsTrue` -- the value `true`, and nothing truthy.
 * @kind bool
 */
export function toBool(value, name) {
  return value === true;
}

/**
 * `f64`: a Number is itself; a BigInt is its low 64 bits read as an i64, as
 * `i64 as f64` rounds them (to nearest, ties to even -- what `Number()` does to
 * a BigInt).
 * @kind f64
 */
export function toF64(value, name) {
  if (typeof value === "number") return value;
  if (typeof value === "bigint") return Number(BigInt.asIntN(64, value));
  throw expected("f64");
}

/**
 * `f32`, as the bits a record carries: a Number rounded once (`as f32`), a
 * BigInt's i64 value rounded once from the integer (`i64 as f32`) -- not via a
 * double, which would round twice. A non-finite value survives as itself:
 * Canvas 2D says a call with one draws nothing, and that is the renderer's rule
 * to apply, not this layer's to pre-empt.
 * @kind f32
 */
export function f32BitsOf(value, name) {
  if (typeof value === "number") F32[0] = value;
  else if (typeof value === "bigint") F32[0] = i64ToF32(BigInt.asIntN(64, value));
  else throw expected("f32");
  return F32_BITS[0];
}

const F32 = new Float32Array(1);
const F32_BITS = new Uint32Array(F32.buffer);

/** `i64 as f32`: round the integer to 24 significant bits, ties to even. */
function i64ToF32(integer) {
  const negative = integer < 0n;
  let magnitude = negative ? -integer : integer;
  const bits = magnitude.toString(2).length;
  if (bits > 24) {
    const shift = BigInt(bits - 24);
    const kept = magnitude >> shift;
    const rest = magnitude & ((1n << shift) - 1n);
    const half = 1n << (shift - 1n);
    const up = rest > half || (rest === half && (kept & 1n) === 1n);
    magnitude = (up ? kept + 1n : kept) << shift;
  }
  // At most 25 significant bits: exact as a double, and exact as a float.
  const exact = Number(magnitude);
  return negative ? -exact : exact;
}

/**
 * `#[string]` (`String`, `&str`, `Cow<str>`): a string is itself, and anything
 * else is the empty string -- deno_core's `to_string` does not convert and does
 * not refuse.
 * @kind string
 */
export function stringOf(value, name) {
  return typeof value === "string" ? value : "";
}

/**
 * `#[string] Option<String>`: null and undefined are None, anything else is
 * `#[string]`.
 * @kind option_string
 */
export function optionalStringOf(value, name) {
  return value === null || value === undefined ? null : stringOf(value, name);
}

/**
 * `#[buffer]` of bytes (`&[u8]`, `JsBuffer`, `Vec<u8>`): a Uint8Array --
 * exactly that kind of view, not a clamped array, another typed array, a
 * DataView or an ArrayBuffer -- as its bytes. Shared and resizable backing
 * stores are taken, as V8 takes them; an op that must refuse them says so
 * itself.
 * @kind u8_buffer
 */
export function bytesOf(value, name) {
  if (kindOf(value) === "Uint8Array") return value;
  throw new TypeError(NOT_A_VIEW);
}

/**
 * `#[buffer] Option<…>` of bytes: null and undefined are None.
 * @kind option_u8_buffer
 */
export function optionalBytesOf(value, name) {
  return value === null || value === undefined ? null : bytesOf(value, name);
}

/**
 * `#[buffer]` of words (`&[u32]`, `#[buffer(copy)] Vec<u32>`): a Uint32Array
 * and nothing else.
 * @kind u32_buffer
 */
export function u32ArrayOf(value, name) {
  if (kindOf(value) === "Uint32Array") return value;
  throw new TypeError(NOT_A_VIEW);
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
