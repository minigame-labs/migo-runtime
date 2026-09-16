// Both halves of the one-body synchronous call, across the two languages.
//
//   emit-sync-calls.mjs write <dir>  -- encode calls with the producer's encoder,
//                                       for `frame_wire::sync::SyncCall::decode`
//   emit-sync-calls.mjs read  <dir>  -- decode the answers the Rust host wrote,
//                                       with the producer's decoder
//
// The two sides were written from the same tables in
// contracts/frame-wire/wire-v1.md, which is the right way to write them and no
// evidence that they agree. The way a disagreement would reach a user is a
// `readPixels` answered over the wrong rectangle or with another field's value,
// which nothing but a screenshot would catch.
//
// Deterministic: same seed, same bytes, every run.
//
// Driven by scripts/test-frame-wire-js-encoder.sh; the Rust half is
// engine/crates/frame-wire/tests/sync_js_interop.rs.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  SYNC_CALL_HEADER_BYTES,
  SYNC_CALL_MAX_BYTES,
  SYNC_CALL_MAX_TIMEOUT_MILLIS,
  decodeSyncAnswer,
  encodeSyncCall,
} from "../src/sync-call.mjs";
import { MAX_REPLY_BYTES, SYNC_OP_READ_PIXELS } from "../src/sync-mailbox.mjs";

const [mode, directory] = process.argv.slice(2);
if ((mode !== "write" && mode !== "read") || !directory) {
  console.error("usage: emit-sync-calls.mjs write|read <directory>");
  process.exit(2);
}

// A small xorshift, so the spread is reproducible without a dependency.
let seed = 0x2545f491;
function next(bound) {
  seed ^= seed << 13;
  seed ^= seed >>> 17;
  seed ^= seed << 5;
  seed >>>= 0;
  return seed % bound;
}
/** A 64-bit value with both halves populated: the two-word split is the mistake. */
function wide() {
  return (BigInt(next(0x1_0000_0000)) << 32n) | BigInt(next(0x1_0000_0000));
}

if (mode === "write") {
  mkdirSync(directory, { recursive: true });
  const manifest = [];
  const count = 64;
  for (let index = 0; index < count; index += 1) {
    // The bounds themselves in the first rows, then a spread.
    const call = {
      runtimeGeneration: wide(),
      surfaceGeneration: wide(),
      resourceEpoch: wide(),
      triggeringSequence: index === 0 ? 0n : wide(),
      operation: index === 1 ? 0xffff_ffff : SYNC_OP_READ_PIXELS,
      maxReplyBytes: index === 2 ? MAX_REPLY_BYTES : 1 + next(MAX_REPLY_BYTES),
      timeoutMillis: index === 3 ? SYNC_CALL_MAX_TIMEOUT_MILLIS : 1 + next(SYNC_CALL_MAX_TIMEOUT_MILLIS),
      params: Uint8Array.from(
        { length: index === 4 ? SYNC_CALL_MAX_BYTES - SYNC_CALL_HEADER_BYTES : next(64) },
        () => next(256),
      ),
    };
    const name = `call-${String(index).padStart(4, "0")}.bin`;
    writeFileSync(join(directory, name), encodeSyncCall(call));
    manifest.push(
      `{"file":"${name}","runtime_generation":"${call.runtimeGeneration}",` +
        `"surface_generation":"${call.surfaceGeneration}","resource_epoch":"${call.resourceEpoch}",` +
        `"triggering_sequence":"${call.triggeringSequence}","operation":${call.operation},` +
        `"max_reply_bytes":${call.maxReplyBytes},"timeout_millis":${call.timeoutMillis},` +
        `"params_bytes":${call.params.length},"params_sum":${call.params.reduce((a, b) => a + b, 0)}}`,
    );
  }
  // One flat JSON object per line, 64-bit values as strings: the Rust side
  // reads it with string search rather than a JSON dependency, and a 64-bit
  // value written as a JSON number is already rounded by the time it is read.
  writeFileSync(join(directory, "calls.jsonl"), manifest.join("\n") + "\n");
  console.log(`encoded ${manifest.length} synchronous calls into ${directory}`);
} else {
  const lines = readFileSync(join(directory, "answers.jsonl"), "utf8")
    .split("\n")
    .filter((line) => line.trim() !== "");
  // An empty manifest would pass while reading nothing.
  if (lines.length < 8) {
    console.error(`the Rust writer produced ${lines.length} answers; it writes at least 8`);
    process.exit(1);
  }
  let checked = 0;
  for (const line of lines) {
    const expected = JSON.parse(line);
    const bytes = readFileSync(join(directory, expected.file));
    // A fresh ArrayBuffer of exactly the response's length, as XHR hands one over.
    const buffer = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    const answer = decodeSyncAnswer(buffer);
    const got = {
      state: answer.state,
      error: answer.error,
      request_id: answer.requestId,
      reply_bytes: answer.replyBytes,
      reply_sum: answer.reply.reduce((a, b) => a + b, 0),
    };
    for (const key of Object.keys(got)) {
      if (got[key] !== expected[key]) {
        console.error(`${expected.file}: ${key} is ${got[key]}, the Rust writer wrote ${expected[key]}`);
        process.exit(1);
      }
    }
    checked += 1;
  }
  console.log(`read ${checked} Rust-written synchronous answers`);
}
