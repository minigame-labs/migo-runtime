// The service stream: everything this producer asks the host to do that is not
// drawing -- read a file, write a save, load an image, play a sound.
//
// The formats are contracts/frame-wire/wire-v1.md, "The service stream"; the
// Rust half is `frame_wire::service`, checked against this through
// `test/emit-service.mjs`. The host half is the external session's service
// host, which admits messages strictly in sequence order and answers on a
// return stream of its own (`MDS1`).
//
// HOW A CALL TRAVELS. Records made in one task are batched into one message at
// the end of it, so a frame's worth of audio parameter edits is one message and
// not forty. A message of at most the socket's ceiling goes on the socket; a
// larger one -- a file write -- is POSTed to the service endpoint. The two paths
// reorder, and the host puts them back in sequence order; this side never
// waits for one message to land before sending the next, because a synchronous
// call made meanwhile blocks the Worker, and a producer holding messages behind
// a request only this Worker's event loop can settle would hold them past the
// call that needs them.
//
// No imports beyond siblings, no DOM, no Node API: it runs in a Worker and in
// `node` for its test.

import { ServiceValueError, ValueWriter, readValue, readValues } from "./service-value.mjs";

// The Worker's own `queueMicrotask`, bound to the global it belongs to and
// taken before the engine installs its globals. Bound because WebKit refuses
// the call with any other `this` -- "Can only call
// WorkerGlobalScope.queueMicrotask on instances of WorkerGlobalScope", measured
// on the simulator, where a channel calling it as its own method failed every
// batch -- and node does not check, so only a device-side run would say so.
const platformQueueMicrotask = globalThis.queueMicrotask.bind(globalThis);

export const MAGIC_SERVICE = 0x4d555331; // "MUS1"
export const MAGIC_SERVICE_DOWN = 0x4d445331; // "MDS1"
export const SERVICE_VERSION = 1;
export const SERVICE_HEADER_BYTES = 24;
export const SERVICE_DOWN_HEADER_BYTES = 16;
export const RECORD_HEADER_BYTES = 8;
export const MAX_SERVICE_MESSAGE_BYTES = 64 * 1024 * 1024;

export const UP_REQUEST = 1;
export const UP_COMMAND = 2;
export const UP_CANCEL = 3;

export const DOWN_REPLY = 1;
export const DOWN_REPLY_PARKED = 2;
export const DOWN_EVENT = 3;
export const DOWN_REFUSED = 4;

export const OUTCOME_OK = 0;
export const OUTCOME_ERROR = 1;

/** Whether a message from the host is a service message. */
export function isServiceDownMessage(bytes) {
  return (
    bytes.byteLength >= 4 &&
    new DataView(bytes.buffer, bytes.byteOffset, 4).getUint32(0, true) === MAGIC_SERVICE_DOWN
  );
}

/** The host refused a service message; the stream is broken from there. */
export class ServiceRefusedError extends Error {
  constructor(code, sequence) {
    super(`the host refused service message ${sequence} with code ${code}`);
    this.name = "ServiceRefusedError";
    this.code = code;
    this.sequence = sequence;
  }
}

/** The service stream could not carry a call: closed, or broken by a refusal. */
export class ServiceUnavailableError extends Error {
  constructor(message) {
    super(message);
    this.name = "ServiceUnavailableError";
  }
}

/**
 * Write one record's body with `writeBody(writer)`, framed as `kind`.
 * Returns the framed bytes.
 */
function framedRecord(kind, writeBody) {
  const writer = new ValueWriter(64);
  writer.word(kind);
  writer.word(0); // length, patched once the body is written
  writeBody(writer);
  const bodyBytes = writer.length - RECORD_HEADER_BYTES;
  writer.patchWord(4, bodyBytes);
  return writer.finish();
}

/**
 * Encode one `MUS1` message from framed records.
 *
 * @param {number} generation  the low 32 bits of the runtime generation.
 * @param {bigint} sequence
 * @param {Uint8Array[]} records  framed, each a whole number of words.
 */
export function encodeServiceMessage(generation, sequence, records) {
  let total = SERVICE_HEADER_BYTES;
  for (const record of records) total += record.byteLength;
  const out = new Uint8Array(total);
  const view = new DataView(out.buffer);
  view.setUint32(0, MAGIC_SERVICE, true);
  view.setUint32(4, SERVICE_VERSION, true);
  view.setUint32(8, generation >>> 0, true);
  view.setUint32(12, 0, true);
  view.setBigUint64(16, BigInt(sequence), true);
  let at = SERVICE_HEADER_BYTES;
  for (const record of records) {
    out.set(record, at);
    at += record.byteLength;
  }
  return out;
}

/** A request record: `request_id`, `op`, then the arguments `writeArgs` writes. */
export function requestRecord(requestId, op, writeArgs) {
  return framedRecord(UP_REQUEST, (writer) => {
    writer.word(requestId);
    writer.word(op);
    writeArgs?.(writer);
  });
}

/** A command record: `op`, then its arguments. */
export function commandRecord(op, writeArgs) {
  return framedRecord(UP_COMMAND, (writer) => {
    writer.word(op);
    writeArgs?.(writer);
  });
}

export function cancelRecord(requestId) {
  return framedRecord(UP_CANCEL, (writer) => writer.word(requestId));
}

/** The body of a synchronous SERVICE call: `op`, then the arguments. */
export function encodeServiceCall(op, writeArgs) {
  const writer = new ValueWriter(64);
  writer.word(op);
  writeArgs?.(writer);
  return writer.finish();
}

/**
 * Read a synchronous SERVICE reply: `outcome`, then the value or the error.
 * Returns `{ok: true, value}` or `{ok: false, className, message}`.
 */
export function decodeServiceOutcome(bytes) {
  if (bytes.byteLength < 4) throw new ServiceValueError("a service reply shorter than its outcome");
  const outcome = new DataView(bytes.buffer, bytes.byteOffset, 4).getUint32(0, true);
  const rest = bytes.subarray(4);
  if (outcome === OUTCOME_OK) return { ok: true, value: readValue(rest) };
  if (outcome === OUTCOME_ERROR) {
    const values = readValues(rest);
    if (values.length !== 2 || typeof values[0] !== "string" || typeof values[1] !== "string") {
      throw new ServiceValueError("an error reply is a class and a message");
    }
    return { ok: false, className: values[0], message: values[1] };
  }
  throw new ServiceValueError(`outcome ${outcome} is not one this reader knows`);
}

function readRecords(bytes, offset) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const records = [];
  let at = offset;
  while (at < bytes.byteLength) {
    if (bytes.byteLength - at < RECORD_HEADER_BYTES) throw new ServiceValueError("a record header runs past the end");
    const kind = view.getUint32(at, true);
    const length = view.getUint32(at + 4, true);
    const start = at + RECORD_HEADER_BYTES;
    const padded = length + ((4 - (length % 4)) % 4);
    if (padded > bytes.byteLength - start) throw new ServiceValueError("a record runs past the end");
    records.push(decodeDownRecord(kind, bytes.subarray(start, start + length)));
    at = start + padded;
  }
  return records;
}

function decodeDownRecord(kind, body) {
  const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
  switch (kind) {
    case DOWN_REPLY: {
      const requestId = view.getUint32(0, true);
      return { kind, requestId, ...decodeServiceOutcome(body.subarray(4)) };
    }
    case DOWN_REPLY_PARKED:
      return { kind, requestId: view.getUint32(0, true), byteLength: view.getUint32(4, true) };
    case DOWN_EVENT:
      return { kind, event: view.getUint32(0, true), values: readValues(body.subarray(4)) };
    case DOWN_REFUSED:
      return { kind, code: view.getUint32(0, true), sequence: view.getBigUint64(8, true) };
    default:
      throw new ServiceValueError(`service record kind ${kind} is not one this reader knows`);
  }
}

/** Read one `MDS1` message: its generation and records. */
export function decodeServiceDownMessage(bytes) {
  if (bytes.byteLength < SERVICE_DOWN_HEADER_BYTES || bytes.byteLength % 4 !== 0) {
    throw new ServiceValueError("a service message shorter than its envelope");
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(0, true) !== MAGIC_SERVICE_DOWN) throw new ServiceValueError("not a service message");
  const version = view.getUint32(4, true);
  if (version !== SERVICE_VERSION) throw new ServiceValueError(`service version ${version} is not 1`);
  if (view.getUint32(12, true) !== 0) throw new ServiceValueError("the reserved word is not zero");
  return { generation: view.getUint32(8, true), records: readRecords(bytes, SERVICE_DOWN_HEADER_BYTES) };
}

/** Read one record that fills `bytes`: what a parked reply is. */
export function decodeServiceDownRecord(bytes) {
  const records = readRecords(bytes, 0);
  if (records.length !== 1) throw new ServiceValueError(`expected one record, found ${records.length}`);
  return records[0];
}

/** A POST to the service endpoint, and the parked-reply fetch. The defaults. */
async function defaultPost(url, body) {
  const response = await fetch(url, { method: "POST", body, cache: "no-store" });
  if (!response.ok) throw new Error(`the host answered ${response.status} for a ${body.byteLength}-byte service message`);
}

async function defaultFetchParked(url) {
  const response = await fetch(url, { cache: "no-store" });
  if (!response.ok) throw new Error(`the host answered ${response.status} for a parked reply`);
  return new Uint8Array(await response.arrayBuffer());
}

export class ServiceChannel {
  /**
   * @param {object} options
   * @param {bigint|number} options.generation  the runtime generation; the low
   *        32 bits travel.
   * @param {number} options.socketCeilingBytes  the largest message the socket
   *        carries, from the host.
   * @param {string} options.serviceUrl  where a larger message is POSTed.
   * @param {string} options.replyUrl  where a parked reply is fetched from:
   *        `<replyUrl>/<generation>/<request id>`.
   * @param {(className: string, message: string) => Error} [options.errorFor]
   *        the error content is thrown for a failed op; the producer builds the
   *        class the engine registered under that name.
   * @param {(error: Error) => void} [options.onFailure]  the stream broke: a
   *        refusal, a POST that did not arrive. Terminal, like a refused frame.
   * @param {(event: number, values: any[]) => void} [options.onEvent]
   * @param {(callback: () => void) => void} [options.schedule] when a batch is
   *        sent; the end of the current task by default.
   * @param {Function} [options.post] @param {Function} [options.fetchParked]
   *        the transports; injected by the test.
   */
  constructor({
    generation,
    socketCeilingBytes,
    serviceUrl,
    replyUrl,
    errorFor = (className, message) => Object.assign(new Error(message), { name: className }),
    onFailure,
    onEvent,
    schedule = platformQueueMicrotask,
    post = defaultPost,
    fetchParked = defaultFetchParked,
  } = {}) {
    if (typeof socketCeilingBytes !== "number" || !Number.isFinite(socketCeilingBytes)) {
      throw new TypeError("the service channel needs the host's socketCeilingBytes");
    }
    if (typeof serviceUrl !== "string" || typeof replyUrl !== "string") {
      throw new TypeError("the service channel needs the host's service and reply endpoints");
    }
    this.#generation = Number(BigInt.asUintN(32, BigInt(generation)));
    this.#ceiling = socketCeilingBytes;
    this.#serviceUrl = serviceUrl;
    this.#replyUrl = replyUrl;
    this.#errorFor = errorFor;
    this.#onFailure = onFailure;
    this.#onEvent = onEvent;
    this.#schedule = schedule;
    this.#post = post;
    this.#fetchParked = fetchParked;
  }

  #generation;
  #ceiling;
  #serviceUrl;
  #replyUrl;
  #errorFor;
  #onFailure;
  #onEvent;
  #schedule;
  #post;
  #fetchParked;
  #sendOverSocket = null;

  /** Records waiting for the end of the task, and their total size. */
  #batch = [];
  #batchBytes = 0;
  #flushScheduled = false;

  #nextSequence = 1n;
  #nextRequestId = 1;
  /** request id → { resolve, reject } */
  #pending = new Map();
  #broken = null;

  /** Attach the socket once it is open. Records made before then wait for it. */
  attachSocket(send) {
    this.#sendOverSocket = send;
    if (this.#batch.length > 0) this.flush();
  }

  /** The sequence of the last message sent; zero before any. What a synchronous call names. */
  get lastSentSequence() {
    return this.#nextSequence - 1n;
  }

  /**
   * Ask the host to run `op` and settle with its answer.
   *
   * @param {number} op  from `service-ops.mjs`.
   * @param {(writer: ValueWriter) => void} [writeArgs]  the op's arguments, as
   *        its Rust signature takes them.
   */
  request(op, writeArgs) {
    if (this.#broken !== null) return Promise.reject(this.#broken);
    const requestId = this.#nextRequestId;
    // Never zero, and wrapping past 2^32 skips any id still pending.
    do {
      this.#nextRequestId = this.#nextRequestId === 0xffffffff ? 1 : this.#nextRequestId + 1;
    } while (this.#pending.has(this.#nextRequestId));
    return new Promise((resolve, reject) => {
      this.#pending.set(requestId, { resolve, reject });
      this.#add(requestRecord(requestId, op, writeArgs));
    });
  }

  /** Tell the host to do `op`. Nothing answers; ordered with every request. */
  command(op, writeArgs) {
    if (this.#broken !== null) throw this.#broken;
    this.#add(commandRecord(op, writeArgs));
  }

  #add(record) {
    if (this.#batchBytes + record.byteLength + SERVICE_HEADER_BYTES > MAX_SERVICE_MESSAGE_BYTES && this.#batch.length > 0) {
      // This record does not fit beside the batch; the batch goes first so the
      // order holds.
      this.flush();
    }
    if (record.byteLength + SERVICE_HEADER_BYTES > MAX_SERVICE_MESSAGE_BYTES) {
      throw new RangeError(
        `a ${record.byteLength}-byte service call is larger than the ${MAX_SERVICE_MESSAGE_BYTES}-byte message bound`,
      );
    }
    this.#batch.push(record);
    this.#batchBytes += record.byteLength;
    if (!this.#flushScheduled) {
      this.#flushScheduled = true;
      this.#schedule(() => this.flush());
    }
  }

  /**
   * Send what is batched now. A synchronous call does this first, so the host
   * runs it after everything content asked for before it.
   *
   * @returns {bigint} the last sequence sent.
   */
  flush() {
    this.#flushScheduled = false;
    if (this.#batch.length === 0 || this.#broken !== null) return this.lastSentSequence;
    if (this.#sendOverSocket === null) return this.lastSentSequence;
    const sequence = this.#nextSequence;
    this.#nextSequence += 1n;
    const message = encodeServiceMessage(this.#generation, sequence, this.#batch);
    this.#batch = [];
    this.#batchBytes = 0;
    if (message.byteLength <= this.#ceiling) {
      this.#sendOverSocket(message);
    } else {
      // Not awaited: the next message leaves at once and the host restores the
      // order. A POST that fails breaks the stream -- the host is holding
      // everything after it for a message that will not come.
      this.#post(this.#serviceUrl, message).catch((error) =>
        this.#fail(new ServiceUnavailableError(`service message ${sequence} did not reach the host: ${error}`)),
      );
    }
    return sequence;
  }

  /** A message from the host. Returns its records, for a caller that logs them. */
  handleMessage(bytes) {
    const { generation, records } = decodeServiceDownMessage(bytes);
    if (generation !== this.#generation) return records;
    for (const record of records) {
      switch (record.kind) {
        case DOWN_REPLY:
          this.#settle(record);
          break;
        case DOWN_REPLY_PARKED:
          this.#takeParked(record.requestId, record.byteLength);
          break;
        case DOWN_EVENT:
          this.#onEvent?.(record.event, record.values);
          break;
        case DOWN_REFUSED:
          this.#fail(new ServiceRefusedError(record.code, record.sequence));
          break;
      }
    }
    return records;
  }

  #settle(record) {
    const waiter = this.#pending.get(record.requestId);
    // A request cancelled or failed locally has no waiter; its answer is
    // dropped, which is what the contract says a cancel means.
    if (waiter === undefined) return;
    this.#pending.delete(record.requestId);
    if (record.ok) waiter.resolve(record.value);
    else waiter.reject(this.#errorFor(record.className, record.message));
  }

  #takeParked(requestId, byteLength) {
    const url = `${this.#replyUrl}/${this.#generation}/${requestId}`;
    this.#fetchParked(url).then(
      (bytes) => {
        if (bytes.byteLength !== byteLength) {
          throw new ServiceValueError(`a parked reply of ${bytes.byteLength} bytes was announced as ${byteLength}`);
        }
        const record = decodeServiceDownRecord(bytes);
        if (record.kind !== DOWN_REPLY || record.requestId !== requestId) {
          throw new ServiceValueError(`the parked reply for ${requestId} is not its answer`);
        }
        this.#settle(record);
      },
    ).catch((error) => {
      const waiter = this.#pending.get(requestId);
      if (waiter === undefined) return;
      this.#pending.delete(requestId);
      waiter.reject(new ServiceUnavailableError(`the answer to request ${requestId} could not be taken: ${error}`));
    });
  }

  #fail(error) {
    if (this.#broken !== null) return;
    this.#broken = error;
    for (const { reject } of this.#pending.values()) reject(error);
    this.#pending.clear();
    this.#batch = [];
    this.#batchBytes = 0;
    this.#onFailure?.(error);
  }

  /** The transport went away: every pending request fails, and later ones at once. */
  close() {
    this.#fail(new ServiceUnavailableError("the service stream is closed"));
  }
}
