// The values an op argument is converted from, for the op-argument agreement
// gate.
//
// Evaluated as a classic script twice: in deno_core, where each value is handed
// to a real op of each argument kind (engine/crates/runtime-v8/src/tests/
// op_args_agreement.rs), and in node, where the producer's `op-args.mjs`
// converts the same value (test/op-args-agreement.mjs). The two answers must
// be the same for every value and every kind, which is how the producer's
// restatement of deno_core's conversion rules is kept honest: the rules are
// read from V8, not from deno_core's source.
//
// The values are the ones that tell the rules apart -- integers either side of
// the 32-bit ranges, fractions, non-finite numbers, doubles past 2^53 and 2^63,
// BigInts past 64 bits, and everything that is not a number or not a buffer.
// Nothing here may depend on the runtime it runs in: a value only one of the
// two can construct is a value the other cannot be compared on.

globalThis.OP_ARG_PROBES = (() => {
  const shared = typeof SharedArrayBuffer === "function" ? new SharedArrayBuffer(4) : null;
  const resizable = new ArrayBuffer(4, { maxByteLength: 8 });
  new Uint8Array(resizable).set([1, 2, 3, 4]);
  const probes = [
    ["0", 0],
    ["7", 7],
    ["-0", -0],
    ["-1", -1],
    ["i32 min", -2147483648],
    ["i32 max", 2147483647],
    ["2^31", 2147483648],
    ["u32 max", 4294967295],
    ["2^32", 4294967296],
    ["2^32 + 1.5", 4294967297.5],
    ["1.9", 1.9],
    ["-1.5", -1.5],
    ["below i32 min", -2147483649],
    ["-(2^32 + 1)", -4294967297],
    ["NaN", NaN],
    ["Infinity", Infinity],
    ["-Infinity", -Infinity],
    ["2^53", 2 ** 53],
    ["2^60 + 2^33 + 256", 2 ** 60 + 2 ** 33 + 256],
    ["2^63", 2 ** 63],
    ["-(2^63)", -(2 ** 63)],
    ["2^64", 2 ** 64],
    ["1e300", 1e300],
    ["5n", 5n],
    ["-1n", -1n],
    ["2^40 + 3 n", (1n << 40n) + 3n],
    ["2^64 + 5 n", (1n << 64n) + 5n],
    ["-(2^63) n", -(1n << 63n)],
    ["string 1", "1"],
    ["empty string", ""],
    ["string abc", "abc"],
    ["string non-ascii", "é中"],
    ["null", null],
    ["undefined", undefined],
    ["true", true],
    ["false", false],
    ["object", {}],
    ["array", [1, 2]],
    ["boxed number", new Number(5)],
    ["symbol", Symbol("s")],
    ["Uint8Array", new Uint8Array([1, 2, 3])],
    ["Uint8Array window", new Uint8Array(new Uint8Array([9, 8, 7, 6, 5]).buffer, 1, 3)],
    ["empty Uint8Array", new Uint8Array(0)],
    ["Uint8ClampedArray", new Uint8ClampedArray([9])],
    ["Int8Array", new Int8Array([-1])],
    ["Uint16Array", new Uint16Array([0x0201])],
    ["Uint32Array", new Uint32Array([1, 2])],
    ["Int32Array", new Int32Array([1])],
    ["Float32Array", new Float32Array([1.5])],
    ["DataView", new DataView(new ArrayBuffer(2))],
    ["ArrayBuffer", new ArrayBuffer(4)],
    ["resizable Uint8Array", new Uint8Array(resizable)],
  ];
  if (shared !== null) {
    new Uint8Array(shared).set([4, 3, 2, 1]);
    probes.push(["shared Uint8Array", new Uint8Array(shared)]);
  }
  return probes;
})();
