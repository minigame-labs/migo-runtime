// A frame larger than one packet, and the producer's estimate of what the host
// will decode.
//
// Two halves, both checked again in Rust by
// engine/crates/frame-decode/tests/decode_budget_js_agreement.rs:
//
//   1. The estimate. Deterministic streams mixing GL records, uniform arrays
//      either side of the inline size, canvas selections and 2D records, each
//      estimated by `DecodeBudget`. Written out with the estimate, so the Rust
//      side can require its own producer estimate to be exactly equal.
//   2. The split. Frames far over the decoded budget, driven through
//      `engine-frames.mjs` as the engine's facades drive it: flushed 8192-word
//      buffers, then a frame end. Every packet but the last must be a barrier,
//      every record must arrive once and in order, and a packet that carries 2D
//      records must select its canvas first. The packets are written out so the
//      Rust side can admit them through ingress and hold each to the budget.
//
// And the case a split cannot wait out on the frame clock: the window shut when
// a barrier has to go, answered by a blocking AWAIT_WINDOW call.
//
// Run:  node test/engine-frames.test.mjs [output dir]
// Gate: scripts/test-frame-wire-js-encoder.sh

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { DecodeBudget, MAX_DECODED_FRAME_BYTES } from "../src/decode-budget.mjs";
import { DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { appendStream, endFrame, flushToHost } from "../src/engine-frames.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import {
  MAGIC,
  OP2D_FILL_RECT,
  OP2D_SAVE,
  OP2D_SELECT_CANVAS,
  OP_CLEAR,
  OP_UNIFORM4FV,
  OP_UNIFORM_MATRIX4FV,
  STREAM_VERSION,
} from "../src/render-opcodes.mjs";
import { SYNC_OP_AWAIT_WINDOW, WINDOW_REPLY_BYTES } from "../src/sync-mailbox.mjs";
import { sequenceOf } from "../src/wire-frame-packet.mjs";

const outputDirectory = process.argv[2];
let failures = 0;
function check(condition, message) {
  if (condition) {
    console.log(`  ok   ${message}`);
  } else {
    failures += 1;
    console.log(`  FAIL ${message}`);
  }
}

const header = (opcode, words) => ((words << 12) | opcode) >>> 0;

// xorshift32: the same streams every run, so a disagreement is reproducible.
let state = 0x2545f491;
function next() {
  state ^= state << 13;
  state ^= state >>> 17;
  state ^= state << 5;
  return state >>> 0;
}
const pick = (limit) => next() % limit;

/** One random record, as words. */
function randomRecord(selected) {
  switch (pick(selected ? 7 : 5)) {
    case 0:
    case 1:
      return [header(OP_CLEAR, 3), 1, 0x4000];
    case 2: {
      const payload = pick(40); // either side of the 16-word inline size
      return [header(OP_UNIFORM4FV, 3 + payload), 1, pick(8), ...Array.from({ length: payload }, next)];
    }
    case 3: {
      const payload = pick(2) ? 16 : 32;
      return [header(OP_UNIFORM_MATRIX4FV, 4 + payload), 1, pick(8), 0, ...Array.from({ length: payload }, next)];
    }
    case 4:
      return [header(OP2D_SELECT_CANVAS, 2), 1 + pick(3)];
    case 5:
      return [header(OP2D_FILL_RECT, 5), next(), next(), next(), next()];
    default:
      return [header(OP2D_SAVE, 1)];
  }
}

// ---- 1. the estimate ---------------------------------------------------------

console.log("The producer's decode estimate");
const streams = [];
let fitsAgreed = true;
for (let index = 0; index < 64; index += 1) {
  const words = [MAGIC, STREAM_VERSION];
  const budget = new DecodeBudget();
  let selected = false;
  // Short streams, ordinary ones, and a few past the budget: 40,000 records
  // of mostly GL commands is well over 4 MiB decoded.
  const records = 1 + pick(index < 8 ? 40 : index < 16 ? 60_000 : 4000);
  for (let r = 0; r < records; r += 1) {
    const record = randomRecord(selected);
    const opcode = record[0] & 0xfff;
    if (opcode === OP2D_SELECT_CANVAS) selected = true;
    const fits = budget.fits(opcode, record.length);
    budget.add(opcode, record.length);
    if (fits !== budget.estimatedBytes <= MAX_DECODED_FRAME_BYTES) fitsAgreed = false;
    words.push(...record);
  }
  streams.push({ words: Uint32Array.from(words), estimate: budget.estimatedBytes });
}
check(fitsAgreed, "fits() answers exactly what adding the record would estimate");
check(
  streams.some((stream) => stream.estimate > MAX_DECODED_FRAME_BYTES) &&
    streams.some((stream) => stream.estimate <= MAX_DECODED_FRAME_BYTES),
  "the streams fall on both sides of the budget",
);

// ---- 2. the split ------------------------------------------------------------

console.log("A frame larger than one packet");
const sent = [];
let accepted = 0;
let answerVerdicts = true;
const session = new FrameSession({
  send(bytes) {
    const packet = bytes.slice();
    sent.push(packet);
    if (!answerVerdicts) return;
    const sequence = sequenceOf(packet);
    // Answered at once, as a host with a free renderer does: the window stays
    // open and nothing waits.
    accepted = sequence;
    session.handleMessage(
      encodeBytes([
        { kind: DOWN_FRAME_VERDICT, generation: 1, decision: 1, wireErrorCode: 0, remainingCredits: 2, acceptedSequence: sequence },
      ]),
    );
  },
  sendControl() {},
});
const syncCalls = [];
const sync = {
  call(call) {
    syncCalls.push(call);
    // The host, once a credit returns: every packet sent is admitted and one
    // credit is free.
    accepted = session.sentSequence;
    const reply = new Uint8Array(WINDOW_REPLY_BYTES);
    const view = new DataView(reply.buffer);
    view.setUint32(0, 1, true);
    view.setBigUint64(8, BigInt(accepted), true);
    return reply;
  },
};
bindEngineHost({
  session,
  identity: readEngineSessionConfig({
    launchNonce: "0x0123456789abcdeffedcba9876543210",
    runtimeGeneration: "1",
    surfaceGeneration: "1",
    resourceEpoch: "0",
    surfaceWidth: 64,
    surfaceHeight: 64,
  }),
  socketCeilingBytes: 64 * 1024,
  sync,
  report() {},
});

const BUFFER_WORDS = 8192;
/** Flush `records` as the engine does: buffers of at most 8192 words. */
function flushInBuffers(records, prefix = []) {
  let buffer = [MAGIC, STREAM_VERSION, ...prefix];
  for (const record of records) {
    if (buffer.length + record.length > BUFFER_WORDS) {
      const words = Uint32Array.from(buffer);
      appendStream(words, words.length);
      buffer = [MAGIC, STREAM_VERSION, ...prefix];
    }
    buffer.push(...record);
  }
  const words = Uint32Array.from(buffer);
  appendStream(words, words.length);
}

/** Records in a packet's command stream, as arrays of words. */
function recordsOf(packet) {
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  const offset = view.getUint32(84, true);
  const length = view.getUint32(88, true);
  const records = [];
  let cursor = offset + 8; // past MAGIC and STREAM_VERSION
  while (cursor < offset + length) {
    const words = view.getUint32(cursor, true) >>> 12;
    const record = [];
    for (let w = 0; w < words; w += 1) record.push(view.getUint32(cursor + w * 4, true));
    records.push(record);
    cursor += words * 4;
  }
  return records;
}
const flags = (packet) => new DataView(packet.buffer, packet.byteOffset).getUint32(68, true);

// GL: 40,000 clears charge 65,536 x 144 bytes of GL commands against a 4 MiB budget.
const clears = Array.from({ length: 40_000 }, (_, i) => [header(OP_CLEAR, 3), 1, i]);
flushInBuffers(clears);
endFrame();
const glPackets = sent.splice(0);
check(glPackets.length >= 3, `40,000 clears crossed as ${glPackets.length} packets`);
check(
  glPackets.slice(0, -1).every((packet) => flags(packet) === 0) && flags(glPackets.at(-1)) === 1,
  "every packet but the last is a barrier, and the last presents",
);
const glRecords = glPackets.flatMap(recordsOf);
check(
  glRecords.length === clears.length && glRecords.every((record, i) => record[2] === i),
  "every clear arrived once, in order",
);

// 2D: selections repeated across the split, including after a GL record.
const twoD = [];
for (let i = 0; i < 60_000; i += 1) {
  twoD.push(i % 10_000 === 5_000 ? [header(OP_CLEAR, 3), 1, 0x4000] : [header(OP2D_FILL_RECT, 5), i, 0, 1, 1]);
}
flushInBuffers(twoD, [header(OP2D_SELECT_CANVAS, 2), 7]);
endFrame();
const canvasPackets = sent.splice(0);
check(canvasPackets.length >= 2, `60,000 2D records crossed as ${canvasPackets.length} packets`);
check(
  canvasPackets.every((packet) => {
    const records = recordsOf(packet);
    const first2d = records.findIndex((record) => (record[0] & 0xfff) === OP2D_FILL_RECT);
    const firstSelect = records.findIndex((record) => (record[0] & 0xfff) === OP2D_SELECT_CANVAS);
    return first2d === -1 || (firstSelect !== -1 && firstSelect < first2d && records[firstSelect][1] === 7);
  }),
  "every packet that draws 2D selects canvas 7 before it does",
);
const fills = canvasPackets
  .flatMap(recordsOf)
  .filter((record) => (record[0] & 0xfff) === OP2D_FILL_RECT)
  .map((record) => record[1]);
check(
  fills.length === 59_994 && fills.every((value, i) => i === 0 || value > fills[i - 1]),
  "every 2D record arrived once, in order",
);

// The window shut when a barrier has to go.
answerVerdicts = false;
const beforeCalls = syncCalls.length;
// Four or more barriers against a window of two that no verdict reopens.
const starved = Array.from({ length: 120_000 }, (_, i) => [header(OP_CLEAR, 3), 1, i]);
flushInBuffers(starved);
const afterBarriers = sent.length;
check(syncCalls.length > beforeCalls, "a barrier the window refused blocked in AWAIT_WINDOW");
check(
  syncCalls.slice(beforeCalls).every((call) => call.operation === SYNC_OP_AWAIT_WINDOW && call.maxReplyBytes === WINDOW_REPLY_BYTES),
  "the call names AWAIT_WINDOW and reserves the reply it answers with",
);
check(
  syncCalls.slice(beforeCalls).every((call) => call.triggeringSequence >= 1),
  "the call names the last packet sent as the one to wait for",
);
check(afterBarriers >= 2, `the refused barriers were sent once the window opened (${afterBarriers})`);

// A flush on demand: the host runs what is recorded, and the frame goes on.
answerVerdicts = true;
const sequenceBefore = flushToHost();
check(flags(sent.at(-1)) === 0, "flushToHost sends what is recorded as a barrier");
check(sequenceBefore === sequenceOf(sent.at(-1)), "and returns that barrier's sequence");
check(flushToHost() === sequenceBefore, "a flush with nothing recorded sends nothing and names the last packet");
endFrame();
check(sent.length === afterBarriers + 1, "a frame end with nothing recorded after the flush sends nothing");

if (outputDirectory) {
  mkdirSync(join(outputDirectory, "streams"), { recursive: true });
  mkdirSync(join(outputDirectory, "split"), { recursive: true });
  const manifest = streams.map((stream, index) => {
    const name = `stream-${String(index).padStart(3, "0")}.bin`;
    writeFileSync(join(outputDirectory, "streams", name), new Uint8Array(stream.words.buffer));
    return JSON.stringify({ name, words: stream.words.length, estimate: stream.estimate });
  });
  writeFileSync(join(outputDirectory, "streams", "manifest.jsonl"), `${manifest.join("\n")}\n`);
  const packets = [...glPackets, ...canvasPackets];
  const split = packets.map((packet, index) => {
    const name = `packet-${String(index + 1).padStart(4, "0")}.bin`;
    writeFileSync(join(outputDirectory, "split", name), packet);
    return JSON.stringify({ name, sequence: sequenceOf(packet), presents: flags(packet) === 1 });
  });
  writeFileSync(join(outputDirectory, "split", "manifest.jsonl"), `${split.join("\n")}\n`);
  console.log(`wrote ${streams.length} streams and ${packets.length} split packets to ${outputDirectory}`);
}

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
