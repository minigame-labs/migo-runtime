// A read larger than one service answer, asked for in pieces.
//
// The lanes read into a caller's buffer in pieces of at most READ_CHUNK_BYTES
// (files.mjs), so no single answer approaches the synchronous reply ceiling or
// the host's outbox bound. That is only the embedded op's read if the pieces
// continue exactly where each other ended -- by position when one was given,
// by the descriptor's cursor when not -- and stop where the op's single read
// stops: at the first short piece, which is end of file. A zero-length read
// still asks once, because the op seeks even to read nothing.
//
// Run:  node test/files.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import assert from "node:assert/strict";

import { READ_CHUNK_BYTES, readIntoAsync, readIntoSync } from "../src/files.mjs";

let failures = 0;
async function check(name, fn) {
  try {
    await fn();
    console.log(`  ok   ${name}`);
  } catch (error) {
    failures += 1;
    console.log(`  FAIL ${name}\n       ${error && error.message}`);
  }
}

/** A file of `size` bytes whose byte at offset i is i mod 251, read as the host reads it. */
function file(size) {
  let cursor = 0;
  const asked = [];
  const piece = (length, position) => {
    asked.push([length, position]);
    const start = position === null ? cursor : Number(position);
    const end = Math.min(size, start + length);
    const out = new Uint8Array(Math.max(0, end - start));
    for (let at = 0; at < out.length; at += 1) out[at] = (start + at) % 251;
    cursor = start + out.length;
    return out;
  };
  return { piece, asked };
}

function holdsFileBytes(view, count, from) {
  for (let at = 0; at < count; at += 1) {
    if (view[at] !== (from + at) % 251) throw new Error(`byte ${at} is ${view[at]}, not ${(from + at) % 251}`);
  }
}

await check("a buffer larger than a piece fills in pieces that continue by position", () => {
  const { piece, asked } = file(READ_CHUNK_BYTES + 10);
  const view = new Uint8Array(READ_CHUNK_BYTES + 20);
  const count = readIntoSync(view, 3n, piece);
  assert.equal(count, READ_CHUNK_BYTES + 7, "the file past position 3");
  assert.deepEqual(asked, [
    [READ_CHUNK_BYTES, 3n],
    [20, 3n + BigInt(READ_CHUNK_BYTES)],
  ]);
  holdsFileBytes(view, count, 3);
});

await check("without a position, the pieces follow the cursor", () => {
  const { piece, asked } = file(2 * READ_CHUNK_BYTES + 1);
  const view = new Uint8Array(2 * READ_CHUNK_BYTES + 1);
  assert.equal(readIntoSync(view, null, piece), 2 * READ_CHUNK_BYTES + 1);
  assert.deepEqual(asked, [
    [READ_CHUNK_BYTES, null],
    [READ_CHUNK_BYTES, null],
    [1, null],
  ]);
  holdsFileBytes(view, view.length, 0);
});

await check("a short piece is end of file, and nothing more is asked", () => {
  const { piece, asked } = file(5);
  const view = new Uint8Array(READ_CHUNK_BYTES + 1);
  assert.equal(readIntoSync(view, null, piece), 5);
  assert.equal(asked.length, 1);
});

await check("a zero-length read still asks once, at its position", () => {
  const { piece, asked } = file(5);
  assert.equal(readIntoSync(new Uint8Array(0), 2n, piece), 0);
  assert.deepEqual(asked, [[0, 2n]]);
});

await check("awaited: one piece answers the host's bytes as they came", async () => {
  const bytes = Uint8Array.of(1, 2, 3);
  const answer = await readIntoAsync(8, 0n, () => Promise.resolve(bytes));
  assert.equal(answer, bytes, "no staging copy");
});

await check("awaited: pieces stage into one buffer, cut to what arrived", async () => {
  const { piece, asked } = file(READ_CHUNK_BYTES + 4);
  const answer = await readIntoAsync(READ_CHUNK_BYTES + 9, 0n, (length, position) =>
    Promise.resolve(piece(length, position)),
  );
  assert.equal(answer.byteLength, READ_CHUNK_BYTES + 4);
  assert.deepEqual(asked, [
    [READ_CHUNK_BYTES, 0n],
    [9, BigInt(READ_CHUNK_BYTES)],
  ]);
  holdsFileBytes(answer, answer.byteLength, 0);
});

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
