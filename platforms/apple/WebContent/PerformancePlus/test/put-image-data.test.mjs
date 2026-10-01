// `putImageData` from the producer: the op the engine's 2D facade calls, written as OP2D_PUT_IMAGE_DATA records.
//
// The record carries one band of rows; a large ImageData is written as several, each with its own `y`, so a record stays
// well inside a packet. What is checked here is what the host reads: the header's word count, the rectangle's four words,
// the byte length, and that the bands tile the rectangle exactly. The pixel bytes are zero so a header-shaped word cannot
// occur inside them and the scan below finds records and only records.
//
// Run:  node test/put-image-data.test.mjs
// Gate: scripts/test-performance-plus-engine-contract.sh

import { DOWN_FRAME_VERDICT, encodeBytes } from "../src/downlink.mjs";
import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import { op_frame_end_unified, op_put_image_data } from "../src/lane-stream.mjs";
import { OP2D_PUT_IMAGE_DATA, PUT_IMAGE_DATA_BAND_BYTES } from "../src/render-opcodes.mjs";
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

const sent = [];
const session = new FrameSession({
  send(bytes) {
    sent.push(bytes.slice());
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
    surfaceWidth: 64,
    surfaceHeight: 64,
  }),
  socketCeilingBytes: 4 * 1024 * 1024,
  sync: { call() { throw new Error("no sync call expected"); } },
  report() {},
});

/** Every PUT_IMAGE_DATA record in what was sent: header, rectangle words, byte length. */
function recordsSent() {
  const found = [];
  for (const bytes of sent) {
    const words = new Uint32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + (bytes.byteLength & ~3)));
    for (let i = 0; i < words.length - 5; i += 1) {
      if ((words[i] & 0xfff) === OP2D_PUT_IMAGE_DATA && words[i] >>> 12 >= 6) {
        found.push({
          wordCount: words[i] >>> 12,
          x: words[i + 1] | 0,
          y: words[i + 2] | 0,
          width: words[i + 3],
          height: words[i + 4],
          byteLength: words[i + 5],
        });
      }
    }
  }
  return found;
}

// A small rectangle: one record, exactly its bytes.
op_put_image_data(3, -4, 9, 5, 2, new Uint8Array(5 * 2 * 4));
op_frame_end_unified(false);
let records = recordsSent();
check(records.length === 1, "a small ImageData is one record");
check(
  records[0]?.x === -4 && records[0]?.y === 9 && records[0]?.width === 5 && records[0]?.height === 2 &&
    records[0]?.byteLength === 40 && records[0]?.wordCount === 6 + 10,
  "the record names the rectangle, a negative x included, and carries exactly its bytes",
);

// A large one: bands of whole rows that tile the rectangle.
sent.length = 0;
const width = 300;
const height = 1000;
op_put_image_data(3, 2, 6, width, height, new Uint8Array(width * height * 4));
op_frame_end_unified(false);
records = recordsSent();
const rowsPerBand = Math.floor(PUT_IMAGE_DATA_BAND_BYTES / (width * 4));
check(records.length === Math.ceil(height / rowsPerBand), `a ${width}x${height} ImageData is ${Math.ceil(height / rowsPerBand)} bands`);
let nextRow = 6;
let tiled = true;
for (const record of records) {
  tiled &&= record.x === 2 && record.width === width && record.y === nextRow && record.byteLength === record.height * width * 4;
  tiled &&= record.byteLength <= PUT_IMAGE_DATA_BAND_BYTES;
  nextRow += record.height;
}
check(tiled && nextRow === 6 + height, "the bands are whole rows, each within the band size, and tile the rectangle exactly");

// What is not a rectangle of pixels is not sent.
sent.length = 0;
op_put_image_data(3, 0, 0, 4, 4, new Uint8Array(10));
op_put_image_data(3, 0, 0, 0, 4, new Uint8Array(0));
op_put_image_data(3, 0, 0, 4, 0, new Uint8Array(0));
op_frame_end_unified(false);
check(recordsSent().length === 0, "a buffer that is not width x height x 4 bytes, and an empty rectangle, write nothing");

if (failures > 0) {
  console.log(`FAIL (${failures})`);
  process.exit(1);
}
console.log("PASS (putImageData bands)");
