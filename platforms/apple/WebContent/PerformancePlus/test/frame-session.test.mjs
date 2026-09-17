// The producer's window accounting, frame clock and frame requests, against
// real downlink bytes.
//
// The bytes are built with `downlink.mjs`'s own encoder, which the interop gate
// checks against the Rust writer -- so what is exercised here is this module's
// behaviour on messages that are known to be the ones the host sends, rather
// than on a shape invented for the test.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/frame-session.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { DOWN_CLOCK_TICK, DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { UP_REQUEST_FRAME, decodeControlBytes } from "../src/control.mjs";
import {
  DECISION_ACCEPTED,
  DECISION_GENERATION_LOST,
  DECISION_WOULD_BLOCK,
  FrameSession,
  SUBMIT_CLOSED,
  SUBMIT_NO_CREDIT,
} from "../src/frame-session.mjs";

let failures = 0;

function check(name, fn) {
  try {
    fn();
    console.log(`  ok   ${name}`);
  } catch (error) {
    failures += 1;
    console.log(`  FAIL ${name}`);
    console.log(`       ${error && error.message}`);
  }
}

function assertEqual(actual, expected, message) {
  if (actual !== expected) throw new Error(`${message}: expected ${expected}, got ${actual}`);
}

/// A packet as far as the session reads one: its sequence at offset 32. The
/// rest of the header is the host's business, and the session does not look.
function packet(sequence) {
  const bytes = new Uint8Array(80);
  new DataView(bytes.buffer).setBigUint64(32, BigInt(sequence), true);
  return bytes;
}

const verdict = (remainingCredits, acceptedSequence, decision = DECISION_ACCEPTED) =>
  encodeBytes([
    {
      kind: DOWN_FRAME_VERDICT,
      generation: 3,
      decision,
      wireErrorCode: 0,
      remainingCredits,
      acceptedSequence: decision === DECISION_ACCEPTED ? acceptedSequence : 0,
    },
  ]);

const tick = (frameId, { timestampNs = frameId * 16_000_000, remainingCredits = 2, acceptedSequence = 0 } = {}) =>
  encodeBytes([
    { kind: DOWN_CLOCK_TICK, generation: 3, frameId, timestampNs, remainingCredits, acceptedSequence },
  ]);

function session(overrides = {}) {
  const sent = [];
  const control = [];
  const instance = new FrameSession({
    send: (bytes) => sent.push(bytes),
    // Copied: the session reuses its request buffer, which is sound because a
    // real socket copies on send, and this fake has to behave like one.
    sendControl: (bytes) => control.push(bytes.slice()),
    ...overrides,
  });
  return { instance, sent, control };
}

console.log("The producer's frame session");

check("the first frame goes before any advertisement, and only the first", () => {
  // The window is (1, 0) until the host says otherwise. Two packets sent back
  // to back before the first is accepted can reorder on the two uplinks, and
  // ingress holds nothing ahead of a generation's first packet.
  const { instance, sent } = session();
  assertEqual(instance.credits, 1, "credits before any advertisement");
  assertEqual(instance.submit(packet(1)), true, "the first submit");
  assertEqual(instance.submit(packet(2)), SUBMIT_NO_CREDIT, "the second, before a verdict");
  assertEqual(sent.length, 1, "nothing more went on the wire");
  instance.handleMessage(verdict(1, 1));
  assertEqual(instance.credits, 1, "one free, nothing sent past sequence 1");
  assertEqual(instance.submit(packet(2)), true, "the second, after the verdict");
});

check("packets sent after an advertisement are counted against it", () => {
  const { instance } = session();
  instance.submit(packet(1));
  instance.handleMessage(verdict(2, 1));
  assertEqual(instance.credits, 2, "two free after sequence 1");
  instance.submit(packet(2));
  assertEqual(instance.credits, 1, "sequence 2 is not in that advertisement");
  instance.submit(packet(3));
  assertEqual(instance.credits, 0, "nor is 3");
  // A late advertisement from before 2 and 3 were admitted: still counted.
  instance.handleMessage(tick(1, { remainingCredits: 2, acceptedSequence: 1 }));
  assertEqual(instance.credits, 0, "an older window never lets more through");
  instance.handleMessage(tick(2, { remainingCredits: 1, acceptedSequence: 3 }));
  assertEqual(instance.credits, 1, "and a newer one opens exactly what is free");
});

check("a tick reopens a producer whose last verdict said zero", () => {
  // The stall this record exists to prevent: a verdict is only sent for a
  // packet, so a producer that stopped sending would never hear the credit
  // came back. The tick it asked for carries the window.
  const { instance } = session();
  instance.submit(packet(1));
  instance.handleMessage(verdict(1, 1));
  instance.submit(packet(2));
  instance.handleMessage(verdict(0, 2));
  assertEqual(instance.credits, 0, "closed by the verdict");
  instance.handleMessage(tick(1, { remainingCredits: 2, acceptedSequence: 2 }));
  assertEqual(instance.credits, 2, "reopened by the tick");
});

check("a refusal's verdict is not an advertisement", () => {
  const { instance } = session();
  instance.submit(packet(1));
  instance.handleMessage(verdict(2, 1));
  instance.submit(packet(2));
  assertEqual(instance.credits, 1, "before the refusal");
  // A WOULD_BLOCK carries no accepted sequence. Applied, its zero would count
  // both packets against a window of zero.
  instance.handleMessage(verdict(0, 0, DECISION_WOULD_BLOCK));
  assertEqual(instance.credits, 1, "unchanged by it");
});

check("resending a packet that was held back does not count it twice", () => {
  const { instance } = session();
  instance.submit(packet(1));
  instance.handleMessage(verdict(2, 1));
  instance.submit(packet(2));
  instance.submit(packet(2));
  assertEqual(instance.credits, 1, "one sequence is one packet against the window");
});

check("a frame request goes on the control uplink, once per tick", () => {
  const { instance, sent, control } = session();
  let ran = 0;
  assertEqual(instance.requestFrame(1n, () => (ran += 1)), true, "the first request");
  assertEqual(instance.requestFrame(1n, () => (ran += 1)), true, "a second before the tick");
  assertEqual(control.length, 1, "coalesced: one message for both");
  assertEqual(sent.length, 0, "and none on the frame uplink");
  const records = decodeControlBytes(control[0]);
  assertEqual(records.length, 1, "one record");
  assertEqual(records[0].kind, UP_REQUEST_FRAME, "a frame request");
  assertEqual(records[0].generation, 1, "for generation 1");

  instance.handleMessage(tick(1));
  assertEqual(ran, 2, "both callbacks ran on the tick");
  instance.handleMessage(tick(2));
  assertEqual(ran, 2, "and not again on a tick nobody asked for");
});

check("a callback that asks again sends a new request", () => {
  const { instance, control } = session();
  const frames = [];
  const loop = (timestampMillis, frameId) => {
    frames.push(frameId);
    if (frames.length < 3) instance.requestFrame(1n, loop);
  };
  instance.requestFrame(1n, loop);
  for (let id = 1; id <= 4; id += 1) instance.handleMessage(tick(id));
  assertEqual(frames.join(","), "1,2,3", "one frame per request");
  assertEqual(control.length, 3, "and one request per frame");
});

check("the generation is carried as its low 32 bits", () => {
  const { instance, control } = session();
  instance.requestFrame(0x1_0000_0007n, () => {});
  assertEqual(decodeControlBytes(control[0])[0].generation, 7, "BigInt generation");
  instance.handleMessage(tick(1));
  instance.requestFrame(9, () => {});
  assertEqual(decodeControlBytes(control[1])[0].generation, 9, "Number generation");
  let threw = false;
  try {
    instance.requestFrame("1", () => {});
  } catch (error) {
    threw = error instanceof TypeError;
  }
  assertEqual(threw, true, "a string is not a generation");
});

check("a callback that throws does not stop the others or the rest of the message", () => {
  const { instance } = session();
  let second = false;
  instance.requestFrame(1n, () => {
    throw new Error("content");
  });
  instance.requestFrame(1n, () => (second = true));
  instance.submit(packet(1));
  const message = encodeBytes([
    { kind: DOWN_CLOCK_TICK, generation: 3, frameId: 1, timestampNs: 1, remainingCredits: 0, acceptedSequence: 0 },
    {
      kind: DOWN_FRAME_VERDICT,
      generation: 3,
      decision: DECISION_ACCEPTED,
      wireErrorCode: 0,
      remainingCredits: 2,
      acceptedSequence: 1,
    },
  ]);
  let thrown = null;
  try {
    instance.handleMessage(message);
  } catch (error) {
    thrown = error;
  }
  assertEqual(thrown && thrown.message, "content", "the error still surfaces");
  assertEqual(second, true, "the other callback ran");
  assertEqual(instance.credits, 2, "and the verdict behind the tick was applied");
});

check("a tick reaches content in milliseconds", () => {
  let seen = null;
  const { instance } = session({
    onFrame: (timestampMillis, frameId) => {
      seen = { timestampMillis, frameId };
    },
  });
  instance.handleMessage(tick(7, { timestampNs: 16_700_000 }));
  assertEqual(seen.frameId, 7, "frame id");
  assertEqual(seen.timestampMillis, 16.7, "requestAnimationFrame's unit is milliseconds");
  assertEqual(instance.lastFrame.frameId, 7, "and the session remembers it");
});

check("a lost generation closes the session rather than retrying", () => {
  let lost = null;
  const { instance, sent, control } = session({ onGenerationLost: (generation) => (lost = generation) });
  instance.handleMessage(verdict(4, 0, DECISION_GENERATION_LOST));
  assertEqual(lost, 3, "the generation is named");
  assertEqual(instance.isClosed, true, "the session is closed");
  assertEqual(instance.submit(packet(1)), SUBMIT_CLOSED, "submits stop");
  assertEqual(instance.requestFrame(1n, () => {}), SUBMIT_CLOSED, "and so do requests");
  assertEqual(sent.length + control.length, 0, "nothing was sent into a runtime that is gone");
});

check("close stops submits without needing a verdict", () => {
  const { instance } = session();
  instance.close();
  assertEqual(instance.submit(packet(1)), SUBMIT_CLOSED, "after close");
});

check("a session without a send function is refused at construction", () => {
  let threw = false;
  try {
    // eslint-disable-next-line no-new
    new FrameSession({});
  } catch (error) {
    threw = error instanceof TypeError;
  }
  assertEqual(threw, true, "a session that cannot send is not a session");
});

check("control messages default to the frame uplink when no socket is named", () => {
  const sent = [];
  const instance = new FrameSession({ send: (bytes) => sent.push(bytes.slice()) });
  instance.requestFrame(1n, () => {});
  assertEqual(sent.length, 1, "the request went somewhere");
  assertEqual(decodeControlBytes(sent[0])[0].kind, UP_REQUEST_FRAME, "and it is a request");
});

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
