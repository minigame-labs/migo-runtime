// An op's arguments, converted as V8 running deno_core converts them.
//
// fixtures/op-arg-probes.js holds the values that tell the conversion rules
// apart; fixtures/op-arg-answers.json holds what real deno_core ops made of
// each one, per argument kind, and is itself checked against V8 by
// engine/crates/runtime-v8/src/tests/op_args_agreement.rs. This test puts the
// same values through op-args.mjs and requires the same answer for every value
// and every kind -- the result, or the error class and message.
//
// Which function answers for which kind is read from op-args.mjs's own `@kind`
// tags -- the tags the build's conversion check trusts -- so a function cannot
// claim a kind V8 has not been asked about, and every kind it claims is tested.
//
// Run:  node test/op-args.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { readFileSync } from "node:fs";
import { runInThisContext } from "node:vm";

import * as opArgs from "../src/op-args.mjs";

const fixtures = new URL("./fixtures/", import.meta.url);
runInThisContext(readFileSync(new URL("op-arg-probes.js", fixtures), "utf8"), { filename: "op-arg-probes.js" });
const answers = JSON.parse(readFileSync(new URL("op-arg-answers.json", fixtures), "utf8"));

// The tags, read as scripts/gen-performance-plus-engine.py reads them.
const source = readFileSync(new URL("../src/op-args.mjs", import.meta.url), "utf8");
const TAGGED = /\/\*\*(?:(?!\*\/)[\s\S])*?@kind\s+([a-z0-9_, ]+?)\s*\n(?:(?!\*\/)[\s\S])*?\*\/\s*export\s+function\s+(\w+)/g;
const converters = new Map();
for (const [, listed, name] of source.matchAll(TAGGED)) {
  for (const kind of listed.split(",").map((part) => part.trim()).filter(Boolean)) {
    if (converters.has(kind)) throw new Error(`${kind} is claimed by both ${converters.get(kind)} and ${name}`);
    converters.set(kind, name);
  }
}

const hex = (bytes) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
const F64 = new Float64Array(1);
const F64_BITS = new BigUint64Array(F64.buffer);

/** What the probe op for `kind` in op_args_agreement.rs renders for a value it received. */
function render(kind, value) {
  if (kind.startsWith("option_")) return value === null ? "none" : render(kind.slice("option_".length), value);
  if (kind === "f32") return value.toString(16).padStart(8, "0");
  if (kind === "f64") {
    F64[0] = value;
    return F64_BITS[0].toString(16).padStart(16, "0");
  }
  if (kind === "string") return JSON.stringify(value);
  if (kind === "u8_buffer") return `bytes:${hex(value)}`;
  if (kind === "u32_buffer") return `u32s:${Array.from(value).join(",")}`;
  return String(value);
}

let failures = 0;
let compared = 0;
for (const kind of Object.keys(answers)) {
  if (!converters.has(kind)) {
    failures += 1;
    console.log(`  FAIL ${kind}: V8 answered for a kind no op-args.mjs function is tagged with`);
  }
}
for (const [kind, name] of converters) {
  const expectedRow = answers[kind];
  if (expectedRow === undefined) {
    failures += 1;
    console.log(`  FAIL ${name} claims ${kind}, which op-arg-answers.json has no V8 answers for`);
    continue;
  }
  const convert = opArgs[name];
  let kindFailures = 0;
  for (const [label, value] of globalThis.OP_ARG_PROBES) {
    if (!(label in expectedRow)) continue; // a probe only one runtime could build
    compared += 1;
    let actual;
    try {
      actual = render(kind, convert(value, "value"));
    } catch (error) {
      actual = `${error.name}: ${error.message}`;
    }
    if (actual !== expectedRow[label]) {
      kindFailures += 1;
      console.log(`  FAIL ${kind} (${name}) / ${label}: V8 ${expectedRow[label]}, producer ${actual}`);
    }
  }
  failures += kindFailures;
  if (kindFailures === 0) console.log(`  ok   ${kind} (${name})`);
}

console.log(failures === 0 ? `PASS (${compared} conversions agree with V8)` : `FAIL: ${failures} conversion(s) differ`);
process.exit(failures === 0 ? 0 : 1);
