// The engine's frames, from the words its WebGL and Canvas2D facades encode to
// the packets the frame channel sends.
//
// In the embedded runtime a facade flushes its command buffer into Rust with
// `op_submit_render_stream`, as often as the buffer fills, and
// `op_frame_end_unified` turns what was collected into one frame for the
// renderer. Here the same two calls fill a packet and send it.
//
// A FRAME IS USUALLY ONE PACKET, AND SOMETIMES SEVERAL. The host refuses a
// packet above the wire ceiling or whose decoded storage would exceed its
// budget, and a refusal ends the content. So as records arrive they are
// charged against both (`DecodeBudget`, `FramePacketWriter.fits`), and a record
// that would not fit first sends what the packet holds as a barrier -- executed,
// not presented -- and starts the next. The frame's last packet is the one that
// presents. `flushToHost` sends a barrier on demand, for the synchronous calls
// that need the host to have run everything recorded so far.
//
// BACKPRESSURE IS THE FRAME CLOCK'S. A presenting packet the window does not
// admit is held -- never dropped, because a frame can carry state later frames
// depend on -- and the next `requestAnimationFrame` waits until nothing is held.
// A barrier cannot wait that way: it is sent from inside a synchronous call, so
// when the window is closed the producer blocks in the host until a credit
// returns (SYNC_OP_AWAIT_WINDOW), sending anything held first so the sequence
// stays contiguous.

import { DecodeBudget } from "./decode-budget.mjs";
import { engineHost } from "./engine-host.mjs";
import { SUBMIT_CLOSED, SUBMIT_NO_CREDIT } from "./frame-session.mjs";
import { MAGIC, OP2D_SELECT_CANVAS, STREAM_VERSION } from "./render-opcodes.mjs";
import { SYNC_OP_AWAIT_WINDOW, WINDOW_REPLY_BYTES, decodeWindowReply } from "./sync-mailbox.mjs";
import { leavesOnScheme } from "./uplink.mjs";
import { FramePacketWriter } from "./wire-frame-packet.mjs";

const HEADER_OPCODE_MASK = 0xfff;
const HEADER_WORD_SHIFT = 12;
const SELECT_CANVAS_WORDS = 2;

// How long a producer blocked for a credit waits before the frame channel is
// treated as stalled. A renderer returns a credit every frame; a minute without
// one is a host that is not rendering, and the call fails rather than hangs.
const AWAIT_WINDOW_TIMEOUT_MILLIS = 60_000;

let writer = null;
const budget = new DecodeBudget();
let sequence = 0;
let frameId = 0;
// The canvas the stream last selected for 2D records, as the two words of that
// record, or 0 when none has been. A packet split after a selection has to
// repeat it: the host reads a 2D record before any selection as an error.
const selectCanvas = new Uint32Array(SELECT_CANVAS_WORDS);
let canvasSelected = false;
// Presenting packets finished but not yet admitted by the window, oldest first.
// At most one in practice, because the frame clock waits on it; a queue so an
// extra frame end outside the clock cannot overwrite a held one.
const held = [];
// What this producer has sent, for diagnostics and the acceptance tests that
// have to tell a frame that crossed as one packet from one that was split.
const statistics = { packets: 0, barriers: 0, windowWaits: 0 };
// Resolvers for "nothing is held".
let drainWaiters = [];
let draining = false;

function currentWriter() {
  if (writer === null) {
    const host = engineHost();
    writer = new FramePacketWriter({
      launchNonce: host.launchNonce,
      runtimeGeneration: host.runtimeGeneration,
      surfaceGeneration: host.state.surfaceGeneration,
      resourceEpoch: host.state.resourceEpoch,
      magic: MAGIC,
      streamVersion: STREAM_VERSION,
    });
  }
  return writer;
}

/**
 * Take one flushed command buffer: `words[0, usedWords)`, which starts with its
 * own two-word stream header. The records after it join this frame's stream,
 * whose single header the packet writer supplies. Every buffer the engine
 * flushes is a self-contained stream -- it resets its 2D canvas selection when
 * it starts one -- so joining them is the same command sequence.
 */
export function appendStream(words, usedWords) {
  if (usedWords < 2 || words[0] !== MAGIC || words[1] !== STREAM_VERSION) {
    throw new TypeError(`a flushed command buffer must start with the stream header; got ${usedWords} words`);
  }
  const frame = currentWriter();
  // A new buffer starts with no canvas selected, whatever the last one chose.
  canvasSelected = false;
  // Contiguous records that fit are appended as one range: one copy per range,
  // not per record, which is the common case of a buffer that fits entirely.
  let runStart = 2;
  let cursor = 2;
  while (cursor < usedWords) {
    const header = words[cursor];
    const opcode = header & HEADER_OPCODE_MASK;
    const wordCount = header >>> HEADER_WORD_SHIFT;
    if (wordCount === 0 || cursor + wordCount > usedWords) {
      throw new TypeError(`a record at word ${cursor} claims ${wordCount} words of ${usedWords}`);
    }
    if (!budget.fits(words, cursor) || !frame.fits(cursor + wordCount - runStart)) {
      frame.appendWords(words, runStart, cursor);
      runStart = cursor;
      if (frame.wordCount === 0) {
        // Alone in an empty packet and still too large: no split can carry it.
        throw new RangeError(`a ${wordCount}-word record does not fit in one frame packet`);
      }
      sendBarrier(frame);
      // Repeated whether or not the next record is 2D: a GL record between the
      // split and this buffer's next 2D record would otherwise leave that 2D
      // record in a packet with no selection. Before a GL record it selects and
      // draws nothing.
      if (canvasSelected) {
        frame.appendWords(selectCanvas, 0, SELECT_CANVAS_WORDS);
        budget.add(selectCanvas, 0);
      }
      if (!budget.fits(words, cursor) || !frame.fits(wordCount)) {
        throw new RangeError(`a ${wordCount}-word record does not fit in one frame packet`);
      }
    }
    if (opcode === OP2D_SELECT_CANVAS) {
      selectCanvas.set(words.subarray(cursor, cursor + SELECT_CANVAS_WORDS));
      canvasSelected = true;
    }
    budget.add(words, cursor);
    cursor += wordCount;
  }
  frame.appendWords(words, runStart, usedWords);
}

/**
 * Append one record the producer writes itself rather than receives in a flushed
 * buffer: a resource call. `record[0, headerWords)` is its header and fixed
 * words -- ending with `byte_length` or `count` when it carries a payload -- and
 * `payload` is the bytes (a Uint8Array) or words (a Uint32Array) that follow, or
 * null. Split into a new packet first if it does not fit this one.
 *
 * Returns false, appending nothing, when the record cannot fit even an empty
 * packet: an upload above what one packet carries. Those need the resource
 * lane; the caller reports it the way GL reports an allocation it cannot make.
 */
export function appendRecord(record, headerWords, payload) {
  const frame = currentWriter();
  const wordCount = record[0] >>> 12;
  if (!budget.fits(record, 0) || !frame.fits(wordCount)) {
    if (frame.wordCount === 0) return false;
    sendBarrier(frame);
    if (!budget.fits(record, 0) || !frame.fits(wordCount)) return false;
  }
  frame.appendWords(record, 0, headerWords);
  if (payload instanceof Uint8Array) {
    frame.appendPayload(payload);
  } else if (payload instanceof Uint32Array) {
    frame.appendWords(payload, 0, payload.length);
  }
  budget.add(record, 0);
  // A resource record is GL work between the engine's flushed buffers, and the
  // next buffer selects its own canvas.
  canvasSelected = false;
  return true;
}

/** End the frame: send its packet, or hold it until the window opens. */
export function endFrame() {
  const frame = currentWriter();
  if (frame.wordCount === 0) return;
  const host = engineHost();
  const packet = finishPacket(frame, host, true);

  if (held.length === 0 && host.session.submit(packet) === true) {
    releaseBuffer(frame, packet, host);
    return;
  }
  // Held: the bytes stay where they are, and the next frame gets a new buffer.
  frame.detach();
  held.push(packet);
  drain();
}

/**
 * Make the host execute everything recorded so far, without ending the frame,
 * and return the sequence it will have admitted when it has: what a synchronous
 * call names as its triggering sequence. 0 when nothing was ever sent.
 */
export function flushToHost() {
  const frame = currentWriter();
  if (frame.wordCount !== 0) {
    sendBarrier(frame);
  } else {
    drainHeldSynchronously(engineHost());
  }
  return sequence;
}

function finishPacket(frame, host, present) {
  statistics.packets += 1;
  if (!present) statistics.barriers += 1;
  frame.surfaceGeneration = host.state.surfaceGeneration;
  frame.resourceEpoch = host.state.resourceEpoch;
  sequence += 1;
  frameId = (frameId + 1) >>> 0;
  const packet = frame.finish(sequence, frameId, present);
  budget.reset();
  return packet;
}

function releaseBuffer(frame, packet, host) {
  // A socket packet was copied as it left, so its buffer is reusable; a scheme
  // request's body is read after this returns, so it keeps the buffer.
  if (leavesOnScheme(packet.byteLength, host.socketCeilingBytes)) frame.detach();
  else frame.reset();
}

/** Send the packet so far as a barrier, blocking for the window if it is shut. */
function sendBarrier(frame) {
  const host = engineHost();
  drainHeldSynchronously(host);
  const packet = finishPacket(frame, host, false);
  submitBlocking(host, packet);
  releaseBuffer(frame, packet, host);
}

/** Send every held packet now, in order, blocking for credits as needed. */
function drainHeldSynchronously(host) {
  while (held.length > 0) {
    submitBlocking(host, held[0]);
    held.shift();
  }
}

function submitBlocking(host, packet) {
  for (;;) {
    const outcome = host.session.submit(packet);
    if (outcome === true) return;
    if (outcome === SUBMIT_CLOSED) {
      const error = new Error("the frame channel is closed");
      error.name = "RafError";
      throw error;
    }
    if (outcome !== SUBMIT_NO_CREDIT) throw new Error(`unexpected submit outcome ${outcome}`);
    awaitWindow(host);
  }
}

/** Block in the host until a credit is free, and adopt the window it reports. */
function awaitWindow(host) {
  if (host.sync === undefined) {
    throw new Error(
      "the window is closed and this producer has no synchronous endpoint to wait on; " +
        "a frame larger than one packet needs one",
    );
  }
  const { state } = host;
  statistics.windowWaits += 1;
  const reply = host.sync.call({
    runtimeGeneration: host.runtimeGeneration,
    surfaceGeneration: state.surfaceGeneration,
    resourceEpoch: state.resourceEpoch,
    triggeringSequence: host.session.sentSequence,
    operation: SYNC_OP_AWAIT_WINDOW,
    maxReplyBytes: WINDOW_REPLY_BYTES,
    timeoutMillis: AWAIT_WINDOW_TIMEOUT_MILLIS,
  });
  const window = decodeWindowReply(reply);
  host.session.applyWindow(window.remainingCredits, window.acceptedSequence);
}

async function drain() {
  if (draining) return;
  draining = true;
  const host = engineHost();
  try {
    while (held.length > 0) {
      const outcome = host.session.submit(held[0]);
      if (outcome === true) {
        held.shift();
      } else if (outcome === SUBMIT_NO_CREDIT) {
        if (!(await host.session.whenCredit())) held.length = 0;
      } else if (outcome === SUBMIT_CLOSED) {
        // The channel is gone and nothing held will ever be admitted. Content
        // is told by the frame clock, which rejects on a closed session.
        held.length = 0;
      } else {
        throw new Error(`unexpected submit outcome ${outcome}`);
      }
    }
  } finally {
    draining = false;
    const waiters = drainWaiters;
    drainWaiters = [];
    for (const resolve of waiters) resolve();
  }
}

/** A promise for nothing being held. */
export function drained() {
  if (held.length === 0 && !draining) return Promise.resolve();
  return new Promise((resolve) => drainWaiters.push(resolve));
}

/** Packets finished, how many were barriers, and how often a barrier waited for the window. */
export function frameStatistics() {
  return { ...statistics };
}

/** The sequence of the last packet this producer finished; 0 before any. */
export function lastSequence() {
  return sequence;
}
