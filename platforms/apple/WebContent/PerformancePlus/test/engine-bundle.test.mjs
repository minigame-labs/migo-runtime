// The engine's own JavaScript API layer, staged for the producer, run end to end.
//
// `scripts/gen-performance-plus-engine.py --with-producer` writes a resource root
// -- the producer's modules and `engine/` -- and this loads it the way
// `producer-worker.mjs` does: bind the engine host, import `engine/boot.mjs`, and
// let content call `migo.createCanvas().getContext("webgl")`. A fake host answers
// the frame channel: a tick for every frame request, a verdict for every packet.
// What is asserted is what leaves: packets whose header carries the session's
// identity and whose command stream is exactly the words the engine's WebGL
// facade encodes for the calls content made.
//
// Run:  node test/engine-bundle.test.mjs <staged resource root> [packet out dir]
// Gate: scripts/test-performance-plus-engine-contract.sh

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

let [, , rootArg, packetDir] = process.argv;
if (!rootArg) {
  // Run on its own, stage a resource root first: the generator is the only
  // thing that can say what the producer's engine is.
  const repository = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../..");
  rootArg = join(mkdtempSync(join(tmpdir(), "migo-engine-")), "resources");
  execFileSync(
    "python3",
    [join(repository, "scripts/gen-performance-plus-engine.py"), "--root", repository, "--out", rootArg, "--with-producer"],
    { stdio: "inherit" },
  );
}
const root = pathToFileURL(resolve(rootArg)).href;

// Captured before the engine replaces `console` with its own, which routes to
// an op this test does not answer.
const log = console.log.bind(console);

let failures = 0;
function check(condition, message) {
  if (condition) {
    log(`  ok   ${message}`);
  } else {
    failures += 1;
    log(`  FAIL ${message}`);
  }
}

// A Worker's global, as far as the engine touches it at start.
globalThis.self = globalThis;
const listeners = [];
globalThis.addEventListener = (type, listener) => listeners.push([type, listener]);

const { FrameSession } = await import(`${root}/frame-session.mjs`);
const { bindEngineHost, readEngineSessionConfig } = await import(`${root}/engine-host.mjs`);
const { DOWN_CLOCK_TICK, DOWN_FRAME_VERDICT, encodeBytes } = await import(`${root}/downlink.mjs`);
const { decodeControlBytes } = await import(`${root}/control.mjs`);
const { sequenceOf, checksum, HEADER_BYTES, WIRE_MAGIC, SECTION_KIND_COMMAND_STREAM } = await import(
  `${root}/wire-frame-packet.mjs`
);
const opcodes = await import(`${root}/render-opcodes.mjs`);

const identity = readEngineSessionConfig({
  launchNonce: "0x0123456789abcdeffedcba9876543210",
  runtimeGeneration: "1",
  surfaceGeneration: "7",
  resourceEpoch: "0",
  surfaceWidth: 64,
  surfaceHeight: 48,
});

const packets = [];
const requests = [];
let tickId = 0;
let accepted = 0;
const session = new FrameSession({
  send(bytes) {
    packets.push(bytes.slice());
    const sequence = sequenceOf(bytes);
    // The host admits it and answers with a verdict carrying its window.
    queueMicrotask(() => {
      accepted = sequence;
      session.handleMessage(
        encodeBytes([
          {
            kind: DOWN_FRAME_VERDICT,
            generation: 1,
            decision: 1,
            wireErrorCode: 0,
            remainingCredits: 2,
            acceptedSequence: sequence,
          },
        ]),
      );
    });
  },
  sendControl(bytes) {
    requests.push(...decodeControlBytes(bytes.slice()));
    setTimeout(() => {
      tickId += 1;
      session.handleMessage(
        encodeBytes([
          {
            kind: DOWN_CLOCK_TICK,
            generation: 1,
            frameId: tickId,
            timestampNs: tickId * 16_666_667,
            remainingCredits: 2,
            acceptedSequence: accepted,
          },
        ]),
      );
    }, 1);
  },
});
const reports = [];
bindEngineHost({ session, identity, socketCeilingBytes: 64 * 1024, report: (message) => reports.push(message) });

log("The engine's API layer on the producer");

await import(`${root}/engine/boot.mjs`);
check(typeof globalThis.migo === "object" && globalThis.migo !== null, "the engine installed the migo namespace");

const manifest = JSON.parse(readFileSync(join(resolve(rootArg), "engine/manifest.json"), "utf8"));
const { core } = await import(`${root}/engine/core/mod.mjs`);
const missing = manifest.core_members.filter((member) => !(member in core));
check(missing.length === 0, `core provides every member the engine uses (${manifest.core_members.length})`);
if (missing.length) log(`       missing: ${missing.join(", ")}`);

// The engine's console is the engine's op, and it reaches the host as a report
// at the level the embedded op logs at -- including for a value whose ToString
// throws, which the Rust op logs as "<invalid value>" rather than raising.
console.warn("careful", 3);
console.error({ toString() { throw new Error("no"); } });
check(
  reports.some((r) => r.type === "console" && r.level === 2 && r.message.includes("careful")),
  "console.warn reached the host at warn level",
);
check(
  reports.some((r) => r.type === "console" && r.level === 3),
  "console.error with a throwing value still reached the host",
);

const canvas = migo.createCanvas();
check(canvas.width === 64 && canvas.height === 48, "the main canvas has the surface's size");
const gl = canvas.getContext("webgl");
check(gl && typeof gl.clear === "function", "the engine's own WebGL context was created");
const attributes = gl.getContextAttributes();
check(attributes.stencil === true && attributes.powerPreference === "default", "context attributes read back");

await new Promise((resolveFrame) => {
  requestAnimationFrame(() => {
    gl.clearColor(0, 0, 1, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    requestAnimationFrame(() => {
      gl.clearColor(1, 0, 0, 1);
      gl.clear(gl.COLOR_BUFFER_BIT);
      resolveFrame();
    });
  });
});
// Let the second frame end and its verdict arrive.
await new Promise((resolveSettle) => setTimeout(resolveSettle, 20));

check(packets.length === 2, `two frames became two packets (${packets.length})`);
check(requests.length >= 2 && requests.every((r) => r.generation === 1), "each frame was requested for generation 1");

const f32 = new DataView(new ArrayBuffer(4));
const bits = (value) => {
  f32.setFloat32(0, value, true);
  return f32.getUint32(0, true);
};
const header = (op, words) => ((words << 12) | op) >>> 0;
const expectedStream = (r, g, b, a) => [
  opcodes.MAGIC,
  opcodes.STREAM_VERSION,
  header(opcodes.OP_CLEAR_COLOR, 6),
  1,
  bits(r),
  bits(g),
  bits(b),
  bits(a),
  header(opcodes.OP_CLEAR, 3),
  1,
  gl.COLOR_BUFFER_BIT,
];

packets.forEach((packet, index) => {
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  check(view.getUint32(0, true) === WIRE_MAGIC, `packet ${index + 1}: magic`);
  check(view.getUint32(76, true) === checksum(packet), `packet ${index + 1}: checksum`);
  check(sequenceOf(packet) === index + 1, `packet ${index + 1}: sequence ${index + 1}`);
  const nonceLow = view.getBigUint64(16, true);
  const nonceHigh = view.getBigUint64(24, true);
  check(((nonceHigh << 64n) | nonceLow) === identity.launchNonce, `packet ${index + 1}: launch nonce`);
  check(view.getBigUint64(40, true) === 1n, `packet ${index + 1}: runtime generation`);
  check(view.getBigUint64(48, true) === 7n, `packet ${index + 1}: surface generation`);
  check(view.getUint32(HEADER_BYTES, true) === SECTION_KIND_COMMAND_STREAM, `packet ${index + 1}: command stream`);
  const offset = view.getUint32(HEADER_BYTES + 4, true);
  const length = view.getUint32(HEADER_BYTES + 8, true);
  const words = Array.from({ length: length / 4 }, (_, i) => view.getUint32(offset + i * 4, true));
  const expected = index === 0 ? expectedStream(0, 0, 1, 1) : expectedStream(1, 0, 0, 1);
  check(
    words.length === expected.length && words.every((word, i) => word === expected[i]),
    `packet ${index + 1}: the stream is exactly the facade's clearColor + clear`,
  );
});

if (packetDir) {
  mkdirSync(packetDir, { recursive: true });
  packets.forEach((packet, index) => writeFileSync(join(packetDir, `engine-frame-${index + 1}.bin`), packet));
  log(`wrote ${packets.length} engine frames to ${packetDir}`);
}

log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
