// The producer's half of the resource-record parity check.
//
// Loads the staged engine the way producer-worker.mjs does, runs
// fixtures/webgl-resource-calls.js through the engine's own WebGL 2 facade, ends
// the frame, and writes what left: the packets, and the errors the producer
// recorded itself (a call it could not encode). The Rust half runs the same
// script in the embedded runtime and compares command for command:
// engine/crates/runtime-v8/src/rendering/webgl/resource_parity.rs.
//
// Run:  node test/engine-resource-parity.mjs <staged resource root> <output dir>
// Gate: scripts/test-performance-plus-engine-contract.sh

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const [, , rootArg, outputDirectory] = process.argv;
if (!rootArg || !outputDirectory) {
  console.error("usage: node engine-resource-parity.mjs <staged resource root> <output dir>");
  process.exit(2);
}
const root = pathToFileURL(resolve(rootArg)).href;
const here = dirname(fileURLToPath(import.meta.url));
const log = console.log.bind(console);

globalThis.self = globalThis;
globalThis.addEventListener = () => {};

const { FrameSession } = await import(`${root}/frame-session.mjs`);
const { bindEngineHost, readEngineSessionConfig } = await import(`${root}/engine-host.mjs`);
const { DOWN_FRAME_VERDICT, encodeBytes } = await import(`${root}/downlink.mjs`);
const { sequenceOf } = await import(`${root}/wire-frame-packet.mjs`);
const { drainProducerError } = await import(`${root}/lane-local.mjs`);

const packets = [];
const session = new FrameSession({
  send(bytes) {
    packets.push(bytes.slice());
    const sequence = sequenceOf(bytes);
    session.handleMessage(
      encodeBytes([
        { kind: DOWN_FRAME_VERDICT, generation: 1, decision: 1, wireErrorCode: 0, remainingCredits: 2, acceptedSequence: sequence },
      ]),
    );
  },
  sendControl() {},
});
const reports = [];
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
  report: (message) => reports.push(message),
});
await import(`${root}/engine/boot.mjs`);

// Indirect eval: the fixture is a classic script over the engine's globals, as
// the embedded runtime runs it.
(0, eval)(readFileSync(join(here, "fixtures/webgl-resource-calls.js"), "utf8"));
// The frame end the embedded runtime reaches through its `_internalFrameEnd`
// host hook, which the engine takes off the global after it is captured.
const { frameEndAll } = await import(`${root}/engine/host_v8_webgl/02_2d_context.js`);
frameEndAll();

const errors = [];
for (const canvasId of [0, 1]) {
  for (let code = drainProducerError(canvasId); code !== 0; code = drainProducerError(canvasId)) {
    errors.push([canvasId, code]);
  }
}

mkdirSync(outputDirectory, { recursive: true });
packets.forEach((packet, index) =>
  writeFileSync(join(outputDirectory, `packet-${String(index + 1).padStart(4, "0")}.bin`), packet),
);
writeFileSync(join(outputDirectory, "producer-errors.txt"), errors.map(([c, e]) => `${c} ${e}`).join("\n") + "\n");
const warnings = reports.filter((report) => report.type === "console");
if (warnings.length > 0) log(`the producer logged: ${warnings.map((w) => w.message).join(" | ")}`);
log(`wrote ${packets.length} resource packets and ${errors.length} producer errors to ${outputDirectory}`);
