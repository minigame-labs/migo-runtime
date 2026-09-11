// The producer's credit accounting and frame clock, against real downlink bytes.
//
// The bytes are built with `downlink.mjs`'s own encoder, which the interop gate
// checks against the Rust writer -- so what is exercised here is this module's
// behaviour on messages that are known to be the ones the host sends, rather
// than on a shape invented for the test.
//
// Run:  node platforms/apple/WebContent/PerformancePlus/test/frame-session.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import {
  DOWN_CLOCK_TICK,
  DOWN_FRAME_VERDICT,
  encodeBytes,
} from "../src/downlink.mjs";
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

const verdict = (remainingCredits, decision = DECISION_ACCEPTED) =>
  encodeBytes([
    {
      kind: DOWN_FRAME_VERDICT,
      generation: 3,
      decision,
      wireErrorCode: 0,
      remainingCredits,
      acceptedSequence: 1,
    },
  ]);

const tick = (frameId, timestampNs) =>
  encodeBytes([{ kind: DOWN_CLOCK_TICK, generation: 3, frameId, timestampNs }]);

function session(overrides = {}) {
  const sent = [];
  const instance = new FrameSession({ send: (bytes) => sent.push(bytes), ...overrides });
  return { instance, sent };
}

console.log("The producer's frame session");

check("the first frame goes before any verdict has arrived", () => {
  // The level the producer needs is published by the verdict its first frame
  // produces. A session that started at zero would deadlock against it.
  const { instance, sent } = session();
  assertEqual(instance.credits, null, "credits before the first verdict");
  assertEqual(instance.submit(new Uint8Array([1, 2, 3, 4])), true, "the first submit");
  assertEqual(sent.length, 1, "sent");
});

check("a verdict replaces the estimate rather than adjusting it", () => {
  const { instance } = session();
  instance.handleMessage(verdict(2));
  assertEqual(instance.credits, 2, "after the verdict");
  instance.submit(new Uint8Array([1]));
  assertEqual(instance.credits, 1, "the optimistic decrement keeps frames in flight");
  // The host's number wins, whatever the estimate had reached. This is what
  // makes a dropped verdict survivable.
  instance.handleMessage(verdict(2));
  assertEqual(instance.credits, 2, "the host's level is absolute");
});

check("a producer out of credit is told, not thrown at", () => {
  const { instance, sent } = session();
  instance.handleMessage(verdict(0, DECISION_WOULD_BLOCK));
  assertEqual(instance.submit(new Uint8Array([1])), SUBMIT_NO_CREDIT, "the refusal");
  assertEqual(sent.length, 0, "and nothing went on the wire");
  // And it recovers the moment the host says so.
  instance.handleMessage(verdict(1));
  assertEqual(instance.submit(new Uint8Array([1])), true, "after a credit is returned");
});

check("a tick reaches content in milliseconds", () => {
  let seen = null;
  const { instance } = session({
    onFrame: (timestampMillis, frameId) => {
      seen = { timestampMillis, frameId };
    },
  });
  instance.handleMessage(tick(7, 16_700_000));
  assertEqual(seen.frameId, 7, "frame id");
  assertEqual(seen.timestampMillis, 16.7, "requestAnimationFrame's unit is milliseconds");
  assertEqual(instance.lastFrame.frameId, 7, "and the session remembers it");
});

check("one message can carry a tick and a verdict, in order", () => {
  const order = [];
  const { instance } = session({
    onFrame: () => order.push("tick"),
    onVerdict: () => order.push("verdict"),
  });
  const bytes = encodeBytes([
    { kind: DOWN_CLOCK_TICK, generation: 3, frameId: 1, timestampNs: 1_000_000 },
    {
      kind: DOWN_FRAME_VERDICT,
      generation: 3,
      decision: DECISION_ACCEPTED,
      wireErrorCode: 0,
      remainingCredits: 2,
      acceptedSequence: 9,
    },
  ]);
  instance.handleMessage(bytes);
  assertEqual(order.join(","), "tick,verdict", "wire order is dispatch order");
  assertEqual(instance.credits, 2, "and the verdict still landed");
});

check("a lost generation closes the session rather than retrying", () => {
  let lost = null;
  const { instance, sent } = session({ onGenerationLost: (generation) => (lost = generation) });
  instance.handleMessage(verdict(4, DECISION_GENERATION_LOST));
  assertEqual(lost, 3, "the generation is named");
  assertEqual(instance.isClosed, true, "the session is closed");
  assertEqual(instance.submit(new Uint8Array([1])), SUBMIT_CLOSED, "and submits stop");
  assertEqual(sent.length, 0, "nothing was sent into a runtime that no longer exists");
});

check("close stops submits without needing a verdict", () => {
  const { instance } = session();
  instance.close();
  assertEqual(instance.submit(new Uint8Array([1])), SUBMIT_CLOSED, "after close");
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

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
