// The synchronous barrier as one request body and one response body.
//
// `sync-mailbox.mjs` is the barrier over a shared record: the Worker blocks in
// `Atomics.wait` on a `SharedArrayBuffer` and a relay in the page carries the
// request across. That needs `SharedArrayBuffer`, and the Apple lane's content
// origin is a custom scheme on which WebKit does not isolate the page -- G0
// measured `SharedArrayBuffer is not a constructor` there. What that origin does
// have, in a Worker, is a synchronous request. So the same request is POSTed as
// a body, the Worker blocks in `send`, and the host's answer comes back as the
// response: no record, no relay, and no second agent on the path.
//
// The layouts are contracts/frame-wire/wire-v1.md, "A request as one body" and
// "An answer as one body". The Rust half is `frame_wire::sync::{SyncCall,
// SyncAnswer}`, and the two are checked against each other through
// `test/emit-sync-calls.mjs`, not by reading both.
//
// No imports beyond siblings, no DOM, no Node API: it runs in a Worker and in
// `node` for its test.

import {
  MAX_REPLY_BYTES,
  SYNC_ERROR_BAD_DEADLINE,
  SYNC_ERROR_BAD_REPLY_RESERVATION,
  SYNC_ERROR_UNSUPPORTED_OPERATION,
  SYNC_STATE_CANCELLED,
  SYNC_STATE_FAILED,
  SYNC_STATE_READY,
  SyncRequestError,
} from "./sync-mailbox.mjs";

export const SYNC_CALL_HEADER_BYTES = 48;
export const SYNC_CALL_MAX_BYTES = 4096;
export const SYNC_CALL_MAX_TIMEOUT_MILLIS = 60_000;
export const SYNC_ANSWER_HEADER_BYTES = 16;

/**
 * How much longer the request itself may take than the wait the call names.
 *
 * The host turns `timeoutMillis` into a deadline when the body arrives, which is
 * after this side started counting, so a request timed out at exactly
 * `timeoutMillis` would sometimes give up a moment before the host's own
 * TIMED_OUT verdict arrived -- and a producer that gives up early and asks
 * again finds the first request still outstanding. The request's timeout is for
 * a host that will never answer; the margin is what keeps it from also being a
 * second opinion on one that will.
 */
export const SYNC_CALL_TRANSPORT_GRACE_MILLIS = 2_000;

// Call body offsets, in the order the document lists them.
const CALL_OFF_RUNTIME_GENERATION = 0;
const CALL_OFF_SURFACE_GENERATION = 8;
const CALL_OFF_RESOURCE_EPOCH = 16;
const CALL_OFF_TRIGGERING_SEQUENCE = 24;
const CALL_OFF_OPERATION = 32;
const CALL_OFF_MAX_REPLY_BYTES = 36;
const CALL_OFF_TIMEOUT_MILLIS = 40;
const CALL_OFF_RESERVED = 44;

// Answer body offsets.
const ANSWER_OFF_STATE = 0;
const ANSWER_OFF_ERROR = 4;
const ANSWER_OFF_REQUEST_ID = 8;
const ANSWER_OFF_REPLY_BYTES = 12;

/** The response did not carry an answer at all. Not a protocol verdict. */
export class SyncTransportError extends Error {
  constructor(message) {
    super(message);
    this.name = "SyncTransportError";
  }
}

/**
 * Encode one call.
 *
 * The 64-bit fields are BigInt, for the reason the mailbox gives: a Number
 * carries 53 bits exactly, and an encoder that reached for one would be right
 * for every value written by hand and corrupt a real session's identity.
 *
 * Refuses locally what the host would refuse, with the host's code: a call the
 * format cannot carry is not worth a round trip, and the producer should see
 * the same error either way.
 */
export function encodeSyncCall({
  runtimeGeneration,
  surfaceGeneration,
  resourceEpoch,
  triggeringSequence,
  operation,
  maxReplyBytes,
  timeoutMillis,
  params = new Uint8Array(0),
}) {
  if (!(Number.isInteger(maxReplyBytes) && maxReplyBytes >= 1 && maxReplyBytes <= MAX_REPLY_BYTES)) {
    throw new SyncRequestError(SYNC_ERROR_BAD_REPLY_RESERVATION);
  }
  if (
    !(Number.isInteger(timeoutMillis) && timeoutMillis >= 1 && timeoutMillis <= SYNC_CALL_MAX_TIMEOUT_MILLIS)
  ) {
    throw new SyncRequestError(SYNC_ERROR_BAD_DEADLINE);
  }
  if (SYNC_CALL_HEADER_BYTES + params.byteLength > SYNC_CALL_MAX_BYTES) {
    throw new SyncRequestError(SYNC_ERROR_UNSUPPORTED_OPERATION);
  }

  const body = new Uint8Array(SYNC_CALL_HEADER_BYTES + params.byteLength);
  const view = new DataView(body.buffer);
  view.setBigUint64(CALL_OFF_RUNTIME_GENERATION, BigInt(runtimeGeneration), true);
  view.setBigUint64(CALL_OFF_SURFACE_GENERATION, BigInt(surfaceGeneration), true);
  view.setBigUint64(CALL_OFF_RESOURCE_EPOCH, BigInt(resourceEpoch), true);
  view.setBigUint64(CALL_OFF_TRIGGERING_SEQUENCE, BigInt(triggeringSequence), true);
  view.setUint32(CALL_OFF_OPERATION, operation, true);
  view.setUint32(CALL_OFF_MAX_REPLY_BYTES, maxReplyBytes, true);
  view.setUint32(CALL_OFF_TIMEOUT_MILLIS, timeoutMillis, true);
  view.setUint32(CALL_OFF_RESERVED, 0, true);
  body.set(params, SYNC_CALL_HEADER_BYTES);
  return body;
}

/**
 * Read one answer.
 *
 * Every rule the document gives an answer is checked, and a response that
 * breaks one is a transport failure rather than a verdict: a body that is not
 * exactly its header and the reply it names is not an answer, and reading
 * pixels out of one would be reading whatever happened to follow.
 *
 * @param {ArrayBuffer} buffer the response
 * @returns {{state: number, error: number, requestId: number, replyBytes: number, reply: Uint8Array}}
 *          `reply` is a view over `buffer`, not a copy.
 */
export function decodeSyncAnswer(buffer) {
  if (!(buffer instanceof ArrayBuffer)) {
    throw new SyncTransportError(
      `the response was ${Object.prototype.toString.call(buffer)}, not an ArrayBuffer`,
    );
  }
  if (buffer.byteLength < SYNC_ANSWER_HEADER_BYTES) {
    throw new SyncTransportError(
      `the response is ${buffer.byteLength} bytes, shorter than an answer header`,
    );
  }
  const view = new DataView(buffer);
  const state = view.getUint32(ANSWER_OFF_STATE, true);
  const error = view.getUint32(ANSWER_OFF_ERROR, true);
  const requestId = view.getUint32(ANSWER_OFF_REQUEST_ID, true);
  const replyBytes = view.getUint32(ANSWER_OFF_REPLY_BYTES, true);

  if (state !== SYNC_STATE_READY && state !== SYNC_STATE_FAILED && state !== SYNC_STATE_CANCELLED) {
    throw new SyncTransportError(`the answer's state ${state} is not a settled one`);
  }
  if (buffer.byteLength !== SYNC_ANSWER_HEADER_BYTES + replyBytes) {
    throw new SyncTransportError(
      `the answer names ${replyBytes} reply bytes and the response carries ` +
        `${buffer.byteLength - SYNC_ANSWER_HEADER_BYTES}`,
    );
  }
  if (state !== SYNC_STATE_READY && replyBytes !== 0) {
    throw new SyncTransportError(`a state-${state} answer carries ${replyBytes} reply bytes`);
  }
  if ((state === SYNC_STATE_FAILED) !== (error !== 0)) {
    throw new SyncTransportError(`a state-${state} answer carries error ${error}`);
  }
  return {
    state,
    error,
    requestId,
    replyBytes,
    reply: new Uint8Array(buffer, SYNC_ANSWER_HEADER_BYTES, replyBytes),
  };
}

/**
 * One blocking POST from a Worker. The default transport.
 *
 * `responseType` is set before `send`, and a Worker is the one place a
 * synchronous request may have one: G0's `sync_xhr_binary` probe measured that
 * on the device, at both origins. The request's own timeout is allowed in a
 * Worker for the same reason, and is what releases a producer whose host died.
 */
export function blockingPost(url, body, timeoutMillis) {
  const request = new XMLHttpRequest();
  request.open("POST", url, false);
  request.responseType = "arraybuffer";
  request.timeout = timeoutMillis;
  try {
    request.send(body);
  } catch (error) {
    // A synchronous request reports a timeout and a network failure by
    // throwing from `send`, not through a status.
    throw new SyncTransportError(`the synchronous request to ${url} failed: ${error}`);
  }
  return { status: request.status, response: request.response };
}

export class SyncCaller {
  /**
   * @param {object} options
   * @param {string} options.url  the host's sync endpoint, from its configuration.
   * @param {(url: string, body: Uint8Array, timeoutMillis: number) => {status: number, response: ArrayBuffer}} [options.post]
   *        the blocking transport; injected by the test.
   */
  constructor({ url, post = blockingPost } = {}) {
    if (typeof url !== "string") {
      throw new TypeError("SyncCaller needs the host's sync endpoint");
    }
    this.url = url;
    this.post = post;
  }

  /**
   * Block until the host answers, and return the reply.
   *
   * Throws on every outcome that is not an answer, for the reason the mailbox
   * gives: the calls this stands in for are `readPixels` and `getImageData`,
   * and a caller that ignored a return value it did not know could fail would
   * read whatever was in its buffer.
   *
   * @param {object} call  the fields of "A request as one body", plus `params`.
   * @param {Uint8Array} [into] where the reply is written. `readPixels` writes
   *        into a view its caller already allocated; passing it saves copying
   *        the answer into a fresh array first.
   * @returns {Uint8Array} `into`'s prefix when given, otherwise a view over the
   *          response.
   */
  call(call, into) {
    const body = encodeSyncCall(call);
    const { status, response } = this.post(
      this.url,
      body,
      call.timeoutMillis + SYNC_CALL_TRANSPORT_GRACE_MILLIS,
    );
    if (status !== 200) {
      throw new SyncTransportError(`the host answered ${status} for a synchronous call`);
    }
    const answer = decodeSyncAnswer(response);
    if (answer.state === SYNC_STATE_FAILED) {
      throw new SyncRequestError(answer.error);
    }
    if (answer.state === SYNC_STATE_CANCELLED) {
      throw new SyncRequestError(0, "the request was withdrawn");
    }
    if (answer.replyBytes > call.maxReplyBytes) {
      // The host checks this before it answers, so reaching here is a host
      // that broke the rule -- and a reply larger than the reservation is one
      // the caller's buffer was never sized for.
      throw new SyncTransportError(
        `the host answered ${answer.replyBytes} bytes against a reservation of ${call.maxReplyBytes}`,
      );
    }
    if (into === undefined) {
      return answer.reply;
    }
    if (into.byteLength < answer.replyBytes) {
      throw new RangeError(
        `the destination holds ${into.byteLength} bytes and the answer is ${answer.replyBytes}`,
      );
    }
    into.set(answer.reply);
    return into.subarray(0, answer.replyBytes);
  }
}
