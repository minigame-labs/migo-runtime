// The engine's frames, from the words its WebGL and Canvas2D facades encode to
// the packets the frame channel sends.
//
// In the embedded runtime a facade flushes its command buffer into Rust with
// `op_submit_render_stream`, as often as the buffer fills, and
// `op_frame_end_unified` turns what was collected into one frame for the
// renderer. Here the same two calls fill one packet and send it: every packet
// on this lane is one frame.
//
// BACKPRESSURE IS THE FRAME CLOCK'S. A packet the window does not admit is held
// -- never dropped, because a frame can carry state later frames depend on --
// and the next `requestAnimationFrame` waits until nothing is held. So a
// renderer that falls behind slows content's frame rate to its own, which is
// what the embedded runtime's credit window does, instead of queuing frames
// without bound.

import { engineHost } from "./engine-host.mjs";
import { SUBMIT_CLOSED, SUBMIT_NO_CREDIT } from "./frame-session.mjs";
import { MAGIC, STREAM_VERSION } from "./render-opcodes.mjs";
import { leavesOnScheme } from "./uplink.mjs";
import { FramePacketWriter } from "./wire-frame-packet.mjs";

let writer = null;
let sequence = 0;
let frameId = 0;
// Packets finished but not yet admitted by the window, oldest first. At most one
// in practice, because the frame clock waits on it; a queue so an extra frame end
// outside the clock cannot overwrite a held one.
const held = [];
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
 * flushes is a self-contained stream -- it resets its 2D canvas selection when it
 * starts one -- so joining them is the same command sequence.
 */
export function appendStream(words, usedWords) {
  if (usedWords < 2 || words[0] !== MAGIC || words[1] !== STREAM_VERSION) {
    throw new TypeError(`a flushed command buffer must start with the stream header; got ${usedWords} words`);
  }
  currentWriter().appendWords(words, 2, usedWords);
}

/** End the frame: send its packet, or hold it until the window opens. */
export function endFrame() {
  const frame = currentWriter();
  if (frame.wordCount === 0) return;
  const host = engineHost();
  frame.surfaceGeneration = host.state.surfaceGeneration;
  frame.resourceEpoch = host.state.resourceEpoch;
  sequence += 1;
  frameId = (frameId + 1) >>> 0;
  const packet = frame.finish(sequence, frameId);

  if (held.length === 0 && host.session.submit(packet) === true) {
    // Sent. A socket packet was copied as it left, so its buffer is reusable; a
    // scheme request's body is read after this returns, so it keeps the buffer.
    if (leavesOnScheme(packet.byteLength, host.socketCeilingBytes)) frame.detach();
    else frame.reset();
    return;
  }
  // Held: the bytes stay where they are, and the next frame gets a new buffer.
  frame.detach();
  held.push(packet);
  drain();
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

/** The sequence of the last frame this producer finished; 0 before any. */
export function lastSequence() {
  return sequence;
}
