// Both halves of the service stream's interop corpus, in one file.
//
//   emit-service.mjs write <dir>  -- write the uplink corpus with the producer's
//                                    encoder, for the Rust reader
//   emit-service.mjs read  <dir>  -- read the answers the Rust writer produced
//                                    and check them against the same corpus
//
// `engine/crates/frame-wire/tests/service_js_interop.rs` holds the Rust copy of
// both lists and asserts the lengths match. Messages are written through the
// same record builders the ServiceChannel uses, because that is the writer
// production runs.
//
// Driven by scripts/test-frame-wire-js-encoder.sh.

import assert from "node:assert/strict";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  DOWN_EVENT,
  DOWN_REFUSED,
  DOWN_REPLY,
  DOWN_REPLY_PARKED,
  cancelRecord,
  commandRecord,
  decodeServiceDownMessage,
  encodeServiceMessage,
  requestRecord,
} from "../src/service.mjs";

// [generation, sequence, records]
const UPLINK = [
  [1, 1n, [requestRecord(1, 6, (w) => w.str("key"))]],
  [
    0xffffffff,
    2n ** 40n + 5n,
    [
      commandRecord(7, (w) => {
        w.f64(-0.5);
        w.u64(2n ** 53n + 1n);
        w.i64(-(2n ** 63n));
        w.i32(-7);
        w.u32(0xffffffff);
        w.bool(true);
        w.bool(false);
        w.null();
      }),
    ],
  ],
  [
    3,
    3n,
    [
      requestRecord(0xffffffff, 11, (w) => {
        w.bytes(Uint8Array.from({ length: 257 }, (_, index) => index & 0xff));
        w.json('{"a":1}');
        w.array(2);
        w.u32(1);
        w.array(2);
        w.u32(64);
        w.u32(32);
      }),
      cancelRecord(5),
    ],
  ],
  [
    1,
    4n,
    [
      requestRecord(2, 1, (w) => w.str("héllo \u{1F600}")),
      requestRecord(3, 1, (w) => {
        w.str("");
        w.bytes(new Uint8Array(0));
      }),
    ],
  ],
];

// [generation, records as the reader returns them]
const DOWNLINK = [
  [
    7,
    [
      { kind: DOWN_REPLY, requestId: 1, ok: true, value: "v" },
      {
        kind: DOWN_REPLY,
        requestId: 2,
        ok: false,
        className: "StorageError",
        message: "setStorage:fail data exceeds max size",
      },
    ],
  ],
  [
    7,
    [
      { kind: DOWN_REPLY_PARKED, requestId: 3, byteLength: 100 },
      { kind: DOWN_EVENT, event: 4, values: ["x", 5] },
      { kind: DOWN_REFUSED, code: 4013, sequence: 2n ** 33n },
    ],
  ],
  [
    0xffffffff,
    [
      { kind: DOWN_REPLY, requestId: 9, ok: true, value: [9, [640, 480]] },
      { kind: DOWN_REPLY, requestId: 10, ok: true, value: Uint8Array.from({ length: 10 }, (_, i) => i) },
      { kind: DOWN_REPLY, requestId: 11, ok: true, value: { k: [1] } },
      { kind: DOWN_REPLY, requestId: 12, ok: true, value: 2n ** 64n - 1n },
    ],
  ],
];

const [, , mode, directory] = process.argv;
if (!mode || !directory) {
  console.error("usage: emit-service.mjs <write|read> <directory>");
  process.exit(2);
}

if (mode === "write") {
  mkdirSync(directory, { recursive: true });
  UPLINK.forEach(([generation, sequence, records], index) => {
    writeFileSync(
      join(directory, `service-${String(index).padStart(3, "0")}.bin`),
      encodeServiceMessage(generation, sequence, records),
    );
  });
  console.log(`wrote ${UPLINK.length} service messages for the Rust reader`);
} else if (mode === "read") {
  const files = readdirSync(directory)
    .filter((name) => name.endsWith(".bin"))
    .sort();
  assert.equal(files.length, DOWNLINK.length, `the Rust writer wrote ${files.length} messages and this corpus has ${DOWNLINK.length}`);
  files.forEach((name, index) => {
    const decoded = decodeServiceDownMessage(new Uint8Array(readFileSync(join(directory, name))));
    const [generation, records] = DOWNLINK[index];
    assert.equal(decoded.generation, generation, `${name} generation`);
    assert.deepEqual(decoded.records, records, `${name} records`);
  });
  console.log(`read ${files.length} Rust-encoded service answer messages`);
} else {
  console.error(`unknown mode ${mode}`);
  process.exit(2);
}
