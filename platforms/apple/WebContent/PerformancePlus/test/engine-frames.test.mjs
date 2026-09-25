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
import { appendCanvas2DRecord, appendStream, endFrame, flushToHost } from "../src/engine-frames.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import {
  MAGIC,
  OP2D_DRAW_IMAGE_BATCH,
  OP2D_FILL_RECT,
  OP2D_FILL_TEXT,
  OP2D_SAVE,
  OP2D_SELECT_CANVAS,
  OP2D_SET_LINE_DASH,
  OP_CLEAR,
  OP_UNIFORM4FV,
  OP_UNIFORM_MATRIX4FV,
  OPR_CREATE_BUFFER,
  OPR_DRAW_BUFFERS,
  OPR_SHADER_SOURCE,
  OPR_TEX_IMAGE_2D,
  OPR_TRANSFORM_FEEDBACK_VARYINGS,
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

/** A packet's command-stream words, header and version included. */
function wordsOf(packet) {
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  const headerBytes = view.getUint32(8, true);
  const offset = view.getUint32(headerBytes + 4, true);
  const count = view.getUint32(headerBytes + 12, true);
  return new Uint32Array(packet.buffer.slice(packet.byteOffset + offset, packet.byteOffset + offset + count * 4));
}

// xorshift32: the same streams every run, so a disagreement is reproducible.
let state = 0x2545f491;
function next() {
  state ^= state << 13;
  state ^= state >>> 17;
  state ^= state << 5;
  return state >>> 0;
}
const pick = (limit) => next() % limit;

/** `prefix` then `byte_length` then the bytes, zero-padded to a word. */
function payloadRecord(opcode, prefix, bytes) {
  const words = [0, ...prefix, bytes.length];
  for (let i = 0; i < bytes.length; i += 4) {
    let word = 0;
    for (let b = 0; b < 4 && i + b < bytes.length; b += 1) word |= bytes[i + b] << (8 * b);
    words.push(word >>> 0);
  }
  words[0] = header(opcode, words.length);
  return words;
}
const ascii = (length) => Array.from({ length }, () => 0x20 + pick(0x5f));

/** One random record, as words. */
function randomRecord(selected) {
  switch (pick(selected ? 15 : 10)) {
    // The 2D payload records, which own what they carry: a text, a dash list,
    // an image batch of whole nine-word entries.
    case 12:
      return payloadRecord(OP2D_FILL_TEXT, [next(), next(), next()], ascii(pick(80)));
    case 13: {
      const count = pick(12);
      return [header(OP2D_SET_LINE_DASH, 2 + count), count, ...Array.from({ length: count }, next)];
    }
    case 14: {
      const words = 9 * (1 + pick(4));
      return [header(OP2D_DRAW_IMAGE_BATCH, 2 + words), words, ...Array.from({ length: words }, next)];
    }
    case 5:
      return [header(OPR_CREATE_BUFFER, 3), 1, next()];
    case 6:
      return payloadRecord(OPR_SHADER_SOURCE, [1, next()], ascii(pick(60)));
    case 7: {
      const hasData = pick(2);
      const bytes = hasData ? Array.from({ length: pick(400) }, () => pick(256)) : [];
      return payloadRecord(OPR_TEX_IMAGE_2D, [1, 0x0de1, 0, 0x1908, 1, 1, 0, 0x1908, 0x1401, hasData], bytes);
    }
    case 8: {
      const count = pick(9);
      return [header(OPR_DRAW_BUFFERS, 3 + count), 1, count, ...Array.from({ length: count }, () => 0x8ce0 + pick(8))];
    }
    case 9:
      return payloadRecord(OPR_TRANSFORM_FEEDBACK_VARYINGS, [1, next(), 0x8c8c], [...ascii(pick(20)), 0x1f, ...ascii(pick(20))]);
    case 10:
      return [header(OP2D_FILL_RECT, 5), next(), next(), next(), next()];
    case 11:
      return [header(OP2D_SAVE, 1)];
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
    default:
      return [header(OP2D_SELECT_CANVAS, 2), 1 + pick(3)];
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
    const typed = Uint32Array.from(record);
    const fits = budget.fits(typed, 0);
    budget.add(typed, 0);
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
const reports = [];
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
  report: (message) => reports.push(message),
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

// An upload no packet can carry: refused the way GL refuses an allocation, with
// the reason in the host's log, and nothing sent.
{
  const { op_tex_image_2d } = await import("../src/lane-stream.mjs");
  const { drainProducerError } = await import("../src/lane-local.mjs");
  const before = sent.length;
  op_tex_image_2d(1, 0x0de1, 0, 0x1908, 2048, 1024, 0, 0x1908, 0x1401, new Uint8Array(2048 * 1024 * 4));
  endFrame();
  check(drainProducerError(1) === 0x0505, "an upload larger than a packet is OUT_OF_MEMORY on the producer");
  check(
    reports.some((report) => report.type === "console" && report.level === 2 && /resource lane/.test(report.message)),
    "and the host's log says why",
  );
  check(sent.length === before, "and nothing was sent for it");
}

// ---- 3. a 2D record after a barrier ----------------------------------------
//
// A selection holds inside the packet that carries it and nowhere else, so a
// `fillText` after a `measureText` -- which sends a barrier -- is in a packet
// whose canvas nothing selected. The host drops a 2D record with no selection:
// an accepted frame that draws no text, which is how this was found, on a
// simulator, by a test that counted green pixels.

console.log("A 2D record after a barrier");
answerVerdicts = true;
const beforeText = sent.length;
const textRecord = Uint32Array.of(
  // OP2D_SET_TEXT_ALIGN, two words: the smallest 2D record with an argument.
  ((2 << 12) | 553) >>> 0,
  2,
);
check(appendCanvas2DRecord(7, textRecord, 2, null), "the first 2D record was appended");
flushToHost();
check(appendCanvas2DRecord(7, textRecord, 2, null), "and one more after the barrier");
endFrame();
const afterBarrier = sent.slice(beforeText);
check(afterBarrier.length >= 2, `the barrier and the frame both went (${afterBarrier.length})`);
const lastPacket = afterBarrier.at(-1);
const lastWords = wordsOf(lastPacket);
check(
  lastWords[2] === (((2 << 12) | 512) >>> 0) && lastWords[3] === 7,
  "the packet after a barrier selects its canvas again before the 2D record",
);

// ---- 4. adjacent drawImage calls fold into one batch -------------------------
//
// Games draw a sprite per `drawImage`. Adjacent draws on one canvas leave as one
// DRAW_IMAGE_BATCH record; a canvas switch, anything else that reaches the
// stream, or a frame end ends the run -- but the facade's empty flush before
// each draw must not.

console.log("drawImage runs");
{
  const { op_draw_image } = await import("../src/lane-stream.mjs");
  const EMPTY = Uint32Array.of(MAGIC, STREAM_VERSION);
  const drawn = (id) => op_draw_image(7, id, 0, 0, 8, 8, 0, 0, 8, 8);
  const before = sent.length;
  drawn(0x40000001);
  appendStream(EMPTY, 2); // the facade's barrier before the next draw
  drawn(0x40000002);
  drawn(0x40000003);
  op_draw_image(8, 0x40000004, 0, 0, 8, 8, 0, 0, 8, 8); // another canvas
  appendStream(Uint32Array.of(MAGIC, STREAM_VERSION, header(OP2D_SELECT_CANVAS, 2), 8, header(OP2D_SAVE, 1)), 5);
  drawn(0x40000005);
  endFrame();
  const records = sent.slice(before).flatMap(recordsOf);
  const shapes = records.map((record) => record[0] & 0xfff);
  const batches = records.filter((record) => (record[0] & 0xfff) === 558);
  const singles = records.filter((record) => (record[0] & 0xfff) === 557);
  check(batches.length === 1 && batches[0][1] === 27, "three adjacent draws on canvas 7 left as one batch of three entries");
  check(
    batches.length === 1 && [batches[0][2], batches[0][11], batches[0][20]].join() === [0x40000001, 0x40000002, 0x40000003].join(),
    "with their ids exact and in order",
  );
  check(singles.length === 2, "a draw on another canvas and a draw after other work each left alone");
  const order = shapes.filter((opcode) => opcode === 557 || opcode === 558 || opcode === OP2D_SAVE);
  check(
    order.join() === [558, 557, OP2D_SAVE, 557].join(),
    `in the order they were made (${order.join()})`,
  );
}

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
