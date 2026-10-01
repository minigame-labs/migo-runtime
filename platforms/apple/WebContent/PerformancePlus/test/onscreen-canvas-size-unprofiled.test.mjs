// The onscreen canvas's size, as the producer reports it to content: a host that described no window
//
// `graphics::canvas::surface_install::engine_default_backing` gives a canvas the content never sized the surface in CSS
// pixels (physical / pixel ratio, single precision, rounded, at least 1), and the renderer's own buffer follows that. The
// producer reported the physical surface instead, so on an iPhone 12 (1170 x 2532 at 3x) content drew in coordinates three
// times the canvas it drew on: a DPR-naive game's world was cropped to a ninth, and a read past the real 390 x 844 came back
// empty -- which is how the conformance image-decode suite found it. A size the content sets is the content's from then on.
//
// Run:  node test/onscreen-canvas-size-unprofiled.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_get_canvas_info } from "../src/lane-local.mjs";
import { op_resize_canvas } from "../src/lane-stream.mjs";
import { sequenceOf } from "../src/wire-frame-packet.mjs";

let failures = 0;
function check(condition, message) {
  if (condition) {
    console.log(`  ok   ${message}`);
  } else {
    failures += 1;
    console.log(`  FAIL ${message}`);
  }
}

const session = new FrameSession({
  send(bytes) {
    session.handleMessage(
      encodeBytes([
        {
          kind: DOWN_FRAME_VERDICT,
          generation: 1,
          decision: 1,
          wireErrorCode: 0,
          remainingCredits: 2,
          acceptedSequence: sequenceOf(bytes),
        },
      ]),
    );
  },
  sendControl() {},
});

bindEngineHost({
  session,
  identity: readEngineSessionConfig({
    launchNonce: "0x0123456789abcdeffedcba9876543210",
    runtimeGeneration: "1",
    surfaceGeneration: "1",
    resourceEpoch: "0",
    surfaceWidth: 800,
    surfaceHeight: 600,
    
  }),
  socketCeilingBytes: 64 * 1024,
  sync: { call() { throw new Error("no sync call expected"); } },
  report() {},
});


check(
  JSON.stringify(op_get_canvas_info(1)) === JSON.stringify([800, 600]),
  "with no ratio to divide by the surface is the answer, as in the renderer at a ratio of 1",
);

if (failures > 0) {
  console.log(`FAIL (${failures})`);
  process.exit(1);
}
console.log("PASS (a host that described no window)");
