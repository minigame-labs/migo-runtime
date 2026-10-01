// The onscreen canvas's size, as the producer reports it to content: a host that described its window
//
// `graphics::canvas::surface_install::engine_default_backing` gives a canvas the content never sized the surface in CSS
// pixels (physical / pixel ratio, single precision, rounded, at least 1), and the renderer's own buffer follows that. The
// producer reported the physical surface instead, so on an iPhone 12 (1170 x 2532 at 3x) content drew in coordinates three
// times the canvas it drew on: a DPR-naive game's world was cropped to a ninth, and a read past the real 390 x 844 came back
// empty -- which is how the conformance image-decode suite found it. A size the content sets is the content's from then on.
//
// Run:  node test/onscreen-canvas-size.test.mjs
// Gate: scripts/test-frame-wire-js-encoder.sh

import { readFileSync } from "node:fs";
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
    surfaceWidth: 1170,
    surfaceHeight: 2532,
    device: { windowInfo: JSON.stringify({ window_width: 390, window_height: 844, screen_width: 390, screen_height: 844, pixel_ratio: 3 }) },
  }),
  socketCeilingBytes: 64 * 1024,
  sync: { call() { throw new Error("no sync call expected"); } },
  report() {},
});


check(
  JSON.stringify(op_get_canvas_info(1)) === JSON.stringify([390, 844]),
  "an unsized onscreen canvas is the surface in CSS pixels: 1170 x 2532 at 3x reads 390 x 844",
);
op_resize_canvas(1, 500, undefined);
check(
  JSON.stringify(op_get_canvas_info(1)) === JSON.stringify([500, 844]),
  "a width the content sets is its own, and the height it did not set stays the default",
);
op_resize_canvas(1, undefined, 700);
check(
  JSON.stringify(op_get_canvas_info(1)) === JSON.stringify([500, 700]),
  "and so is a height set afterwards",
);

// The key the producer reads is the key the Swift host writes: a producer reading a name the host never sends
// answers the surface for every device, and a fixture written from the same wrong guess passes (it did once).
const host = readFileSync(new URL("../../../Sources/MigoApplePerformancePlus/MigoPerformancePlusHost.swift", import.meta.url), "utf8");
check(host.includes('"pixel_ratio"'), "the host describes the window with a pixel_ratio key, which lane-local.mjs reads");

if (failures > 0) {
  console.log(`FAIL (${failures})`);
  process.exit(1);
}
console.log("PASS (a host that described its window)");
