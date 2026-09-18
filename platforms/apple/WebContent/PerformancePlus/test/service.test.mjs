// The service stream's producer half: the value codec and the channel.
//
// The byte-for-byte agreement with the Rust reader is `emit-service.mjs`'s job;
// this is the behaviour that only the producer has -- batching per task, the
// hybrid send, settling requests from answers and parked answers, and breaking
// the stream on a refusal.

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  DOWN_EVENT,
  DOWN_REFUSED,
  DOWN_REPLY,
  DOWN_REPLY_PARKED,
  MAGIC_SERVICE,
  MAGIC_SERVICE_DOWN,
  OUTCOME_ERROR,
  OUTCOME_OK,
  SERVICE_HEADER_BYTES,
  ServiceChannel,
  ServiceRefusedError,
  UP_COMMAND,
  UP_REQUEST,
  decodeServiceOutcome,
  encodeServiceCall,
  isServiceDownMessage,
} from "../src/service.mjs";
import { ValueWriter, readValue, readValues } from "../src/service-value.mjs";

function framed(kind, write) {
  const writer = new ValueWriter();
  writer.word(kind);
  writer.word(0);
  write(writer);
  writer.patchWord(4, writer.length - 8);
  return writer.finish();
}

function downMessage(generation, records) {
  const writer = new ValueWriter();
  writer.word(MAGIC_SERVICE_DOWN);
  writer.word(1);
  writer.word(generation);
  writer.word(0);
  for (const record of records) writer.raw(record);
  return writer.finish().slice();
}

function reply(requestId, value) {
  return framed(DOWN_REPLY, (w) => {
    w.word(requestId);
    w.word(OUTCOME_OK);
    value(w);
  });
}

function failure(requestId, className, message) {
  return framed(DOWN_REPLY, (w) => {
    w.word(requestId);
    w.word(OUTCOME_ERROR);
    w.str(className);
    w.str(message);
  });
}

/** A channel whose socket and task boundary the test holds. */
function channel(options = {}) {
  const sent = [];
  const posted = [];
  const tasks = [];
  const services = new ServiceChannel({
    generation: 0x1_0000_0007n,
    socketCeilingBytes: 1024,
    serviceUrl: "migo://game/__migo/service",
    replyUrl: "migo://game/__migo/reply",
    schedule: (callback) => tasks.push(callback),
    post: async (url, body) => {
      posted.push({ url, body });
    },
    ...options,
  });
  services.attachSocket((bytes) => sent.push(bytes.slice()));
  const endTask = () => {
    while (tasks.length > 0) tasks.shift()();
  };
  return { services, sent, posted, endTask };
}

function messageRecords(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  assert.equal(view.getUint32(0, true), MAGIC_SERVICE);
  const records = [];
  let at = SERVICE_HEADER_BYTES;
  while (at < bytes.byteLength) {
    const kind = view.getUint32(at, true);
    const length = view.getUint32(at + 4, true);
    records.push({ kind, body: bytes.subarray(at + 8, at + 8 + length) });
    at += 8 + length + ((4 - (length % 4)) % 4);
  }
  return { generation: view.getUint32(8, true), sequence: view.getBigUint64(16, true), records };
}

test("values survive the writer and the reader, 64-bit fields exactly", () => {
  const writer = new ValueWriter(16);
  writer.null();
  writer.bool(true);
  writer.u32(0xffff_ffff);
  writer.i32(-7);
  writer.f64(-0.5);
  writer.u64(2n ** 53n + 1n);
  writer.i64(-(2n ** 63n));
  writer.str("héllo");
  writer.bytes(new Uint8Array([1, 2, 3]));
  writer.json('{"a":[1,2]}');
  writer.array(2);
  writer.u32(9);
  writer.array(2);
  writer.u32(64);
  writer.u32(32);
  assert.deepEqual(readValues(writer.finish()), [
    null,
    true,
    0xffff_ffff,
    -7,
    -0.5,
    2n ** 53n + 1n,
    -(2n ** 63n),
    "héllo",
    new Uint8Array([1, 2, 3]),
    { a: [1, 2] },
    [9, [64, 32]],
  ]);
});

test("a lone surrogate travels as U+FFFD, as V8's string conversion makes it", () => {
  const writer = new ValueWriter();
  writer.str("a\ud800b");
  assert.equal(readValue(writer.finish()), "a�b");
});

test("calls made in one task leave as one message, in order, stamped with the low 32 bits", () => {
  const { services, sent, endTask } = channel();
  services.command(7, (w) => w.f64(0.25));
  services.request(3, (w) => w.str("k"));
  services.command(8);
  assert.equal(sent.length, 0, "nothing leaves before the task ends");
  endTask();
  assert.equal(sent.length, 1);
  const message = messageRecords(sent[0]);
  assert.equal(message.generation, 7);
  assert.equal(message.sequence, 1n);
  assert.deepEqual(
    message.records.map((record) => record.kind),
    [UP_COMMAND, UP_REQUEST, UP_COMMAND],
  );
});

test("a message larger than the socket's ceiling is POSTed, and the next leaves without waiting", () => {
  const { services, sent, posted, endTask } = channel();
  services.command(1, (w) => w.bytes(new Uint8Array(2000)));
  endTask();
  services.command(2);
  endTask();
  assert.equal(posted.length, 1, "the large one went to the service endpoint");
  assert.equal(messageRecords(posted[0].body).sequence, 1n);
  assert.equal(sent.length, 1, "and the next one did not wait for it");
  assert.equal(messageRecords(sent[0]).sequence, 2n);
  assert.equal(services.lastSentSequence, 2n);
});

test("flush sends at once and answers the sequence a synchronous call names", () => {
  const { services, sent } = channel();
  assert.equal(services.flush(), 0n, "nothing sent yet");
  services.command(1);
  assert.equal(services.flush(), 1n);
  assert.equal(sent.length, 1);
  assert.equal(services.flush(), 1n, "an empty flush sends nothing");
});

test("an answer settles its request, and an error is built as the class the host named", async () => {
  const built = [];
  const { services, endTask } = channel({
    errorFor: (className, message) => {
      built.push(className);
      const error = new Error(message);
      error.name = className;
      return error;
    },
  });
  const value = services.request(1);
  const refused = services.request(2);
  endTask();
  services.handleMessage(
    downMessage(7, [reply(1, (w) => w.str("saved")), failure(2, "StorageError", "setStorage:fail data exceeds max size")]),
  );
  assert.equal(await value, "saved");
  await assert.rejects(refused, { name: "StorageError", message: "setStorage:fail data exceeds max size" });
  assert.deepEqual(built, ["StorageError"]);
});

test("a parked answer is taken from the host and settles its request", async () => {
  const fetched = [];
  const body = reply(1, (w) => w.bytes(new Uint8Array(100_000).fill(5)));
  const { services, endTask } = channel({
    fetchParked: async (url) => {
      fetched.push(url);
      return body;
    },
  });
  const read = services.request(1);
  endTask();
  services.handleMessage(
    downMessage(7, [framed(DOWN_REPLY_PARKED, (w) => {
      w.word(1);
      w.word(body.byteLength);
    })]),
  );
  const bytes = await read;
  assert.equal(bytes.byteLength, 100_000);
  assert.deepEqual(fetched, ["migo://game/__migo/reply/7/1"]);
});

test("a message for another generation is not this producer's", async () => {
  const { services, endTask } = channel();
  let settled = false;
  services.request(1).then(() => (settled = true));
  endTask();
  services.handleMessage(downMessage(8, [reply(1, (w) => w.null())]));
  await Promise.resolve();
  assert.equal(settled, false);
});

test("an event reaches the listener with its values", () => {
  const events = [];
  const { services } = channel({ onEvent: (event, values) => events.push([event, values]) });
  services.handleMessage(downMessage(7, [framed(DOWN_EVENT, (w) => {
    w.word(4);
    w.str("x");
  })]));
  assert.deepEqual(events, [[4, ["x"]]]);
});

test("a refusal breaks the stream: pending requests fail and later calls fail at once", async () => {
  const failures = [];
  const { services, endTask } = channel({ onFailure: (error) => failures.push(error) });
  const pending = services.request(1);
  endTask();
  services.handleMessage(
    downMessage(7, [framed(DOWN_REFUSED, (w) => {
      w.word(4013);
      w.word(0);
      w.word64(1n);
    })]),
  );
  await assert.rejects(pending, ServiceRefusedError);
  await assert.rejects(services.request(2), ServiceRefusedError);
  assert.throws(() => services.command(3), ServiceRefusedError);
  assert.equal(failures.length, 1);
});

test("a POST that does not arrive breaks the stream", async () => {
  const failures = [];
  const { services, endTask } = channel({
    post: async () => {
      throw new Error("connection lost");
    },
    onFailure: (error) => failures.push(error),
  });
  const pending = services.request(1, (w) => w.bytes(new Uint8Array(4096)));
  endTask();
  await assert.rejects(pending, /did not reach the host/);
  assert.equal(failures.length, 1);
});

test("a synchronous call's body is the op and its arguments; its reply is the outcome", () => {
  const body = encodeServiceCall(5, (w) => w.str("k"));
  assert.equal(new DataView(body.buffer, body.byteOffset).getUint32(0, true), 5);
  assert.deepEqual(readValues(body.subarray(4)), ["k"]);
  const ok = new ValueWriter();
  ok.word(OUTCOME_OK);
  ok.str("v");
  assert.deepEqual(decodeServiceOutcome(ok.finish()), { ok: true, value: "v" });
  const failed = new ValueWriter();
  failed.word(OUTCOME_ERROR);
  failed.str("Error");
  failed.str("no");
  assert.deepEqual(decodeServiceOutcome(failed.finish()), { ok: false, className: "Error", message: "no" });
});

test("only the host's service envelope is routed here", () => {
  assert.equal(isServiceDownMessage(downMessage(1, [])), true);
  assert.equal(isServiceDownMessage(new Uint8Array([0x31, 0x4c, 0x44, 0x4d])), false, "MDL1 is the frame downlink's");
});
