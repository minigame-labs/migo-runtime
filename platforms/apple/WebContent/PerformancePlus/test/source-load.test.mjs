// The engine's source identity must survive the external op adapter. Requests
// are sent in invocation order, even while earlier replies are still pending.
import assert from "node:assert/strict";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_inner_audio_load_url } from "../src/lane-async.mjs";
import { SERVICE_OP } from "../src/service-ops.mjs";

const calls = [];
const replies = [];
bindEngineHost({
  session: new FrameSession({ send() {}, sendControl() {} }),
  identity: readEngineSessionConfig({
    launchNonce: "0x0123456789abcdeffedcba9876543210",
    runtimeGeneration: "1", surfaceGeneration: "1", resourceEpoch: "0",
    surfaceWidth: 64, surfaceHeight: 64,
  }),
  socketCeilingBytes: 64 * 1024,
  report() {},
  services: {
    request(op, write) {
      assert.equal(op, SERVICE_OP.op_inner_audio_load_url);
      const args = [];
      write({ u32: value => args.push(["u32", value]), str: value => args.push(["str", value]) });
      calls.push(args);
      return new Promise(resolve => replies.push(resolve));
    },
  },
});
const first = op_inner_audio_load_url(7, 701, "clip-a.mp3");
const second = op_inner_audio_load_url(7, 702, "");
assert.deepEqual(calls, [
  [["u32", 7], ["u32", 701], ["str", "clip-a.mp3"]],
  [["u32", 7], ["u32", 702], ["str", ""]],
]);
assert.throws(() => op_inner_audio_load_url(7, "703", "clip-b.mp3"));
assert.equal(calls.length, 2);
replies[1]();
assert.equal(await second, undefined);
replies[0]();
assert.equal(await first, undefined);
console.log("source load adapter: invocation order, identity, clear and strict fields passed");
