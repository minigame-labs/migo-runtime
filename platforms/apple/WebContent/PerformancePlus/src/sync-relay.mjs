// The page's half of the synchronous barrier.
//
// Three parties, and it is worth naming them before the code, because which one
// may block is the whole design:
//
//   the Worker  -- runs the content's JavaScript and BLOCKS in `Atomics.wait`
//                  when it calls `readPixels`. It may block: nothing else is
//                  waiting on that agent.
//   this relay  -- runs on the page's main thread and MUST NOT BLOCK. Blocking
//                  here would stop the page that owns the WebView.
//   the host    -- has the pixels, and answers over the transport.
//
// The Worker and this share a `SharedArrayBuffer` -- same process, so the
// atomics work, which is why the topology puts the producer in a Worker of this
// page rather than anywhere else. The reply travels back over the transport and
// is copied in here.
//
// No imports, no DOM API beyond `postMessage`, no Node API: it runs in
// WebContent and in `node` for its test.

import {
  SYNC_RECORD_BYTES,
  SYNC_STATE_PENDING,
  SYNC_STATE_READY,
  SYNC_STATE_FAILED,
  SYNC_ERROR_REPLY_TOO_LARGE,
  SYNC_ERROR_SESSION_ENDED,
  SYNC_ERROR_UNSUPPORTED_OPERATION,
  OFF_STATE,
  OFF_REQUEST_ID,
  OFF_MAX_REPLY_BYTES,
  OFF_REPLY_BYTES,
  OFF_ERROR,
} from "./sync-mailbox.mjs";

export class SyncRelay {
  /**
   * @param {SharedArrayBuffer} record  the mailbox the Worker blocks on
   * @param {SharedArrayBuffer} reply   where an answer's bytes are written
   * @param {{request: (params: Uint8Array) => Promise<{ok: boolean, bytes?: Uint8Array, error?: number, requestId?: number}>}} transport
   */
  constructor(record, reply, transport) {
    if (record.byteLength < SYNC_RECORD_BYTES) {
      throw new RangeError(
        `the mailbox needs ${SYNC_RECORD_BYTES} bytes, was given ${record.byteLength}`,
      );
    }
    this.words = new Int32Array(record, 0, SYNC_RECORD_BYTES / 4);
    this.view = new DataView(record, 0, SYNC_RECORD_BYTES);
    this.reply = new Uint8Array(reply);
    this.transport = transport;
    this.serves = 0;
  }

  /**
   * Serve one request the Worker has posted.
   *
   * Driven by the Worker's `postMessage`, not by polling the record: a poll
   * would either burn the main thread or add its own interval to a latency the
   * producer is blocked on, and the message arrives at the moment the request
   * was published anyway.
   *
   * Never throws. A relay that threw would leave the producer blocked until its
   * deadline for a reason nobody recorded, and the deadline is the slowest way
   * to learn anything.
   */
  async serve(params) {
    // The request this call is answering, so a reply that comes back after the
    // producer moved on cannot be published onto whatever moved in.
    //
    // That is not hypothetical. A producer whose deadline passes withdraws its
    // request and frees the slot; the host may still answer afterwards, and
    // without this token that answer would be written into a record the NEXT
    // request is using -- handing the producer another call's pixels, which is
    // the one outcome this whole protocol is arranged to prevent.
    const token = ++this.serves;

    // Read what the producer reserved BEFORE going to the host, because it is
    // what decides whether an answer may be delivered at all, and the record is
    // the producer's statement of it.
    const reserved = this.view.getUint32(OFF_MAX_REPLY_BYTES, true);
    let answer;
    try {
      answer = await this.transport.request(params);
    } catch (error) {
      // The transport failed. From the producer's side the session is what has
      // become unreachable, which is the code the document gives for it.
      if (this.#stillOurs(token)) this.#fail(SYNC_ERROR_SESSION_ENDED);
      return;
    }
    if (!this.#stillOurs(token)) return;

    if (!answer || answer.ok !== true) {
      this.#fail(
        (answer && Number.isInteger(answer.error) && answer.error) ||
          SYNC_ERROR_UNSUPPORTED_OPERATION,
        answer && answer.requestId,
      );
      return;
    }

    const bytes = answer.bytes ?? new Uint8Array(0);
    // Refused, never truncated -- neither past what the producer reserved nor
    // past the buffer it will read from. A short `readPixels` is a wrong answer
    // that looks like a right one, and this is the last place that can tell.
    if (bytes.byteLength > reserved || bytes.byteLength > this.reply.byteLength) {
      this.#fail(SYNC_ERROR_REPLY_TOO_LARGE, answer.requestId);
      return;
    }

    this.reply.set(bytes, 0);
    if (Number.isInteger(answer.requestId)) {
      this.view.setUint32(OFF_REQUEST_ID, answer.requestId, true);
    }
    this.view.setUint32(OFF_REPLY_BYTES, bytes.byteLength, true);
    this.view.setUint32(OFF_ERROR, 0, true);
    // The bytes are in place before the state is published, so a producer that
    // observes READY observes a complete answer. The state store is the release
    // and the producer's atomic load is the acquire.
    //
    // Compare-and-exchange rather than a plain store, so the publish and the
    // "is it still outstanding" check cannot be separated by a withdrawal. A
    // failed exchange means the producer stopped waiting between the check
    // above and here; the bytes already written are then unread, because
    // nothing reads them without READY.
    if (
      Atomics.compareExchange(
        this.words,
        OFF_STATE / 4,
        SYNC_STATE_PENDING,
        SYNC_STATE_READY,
      ) !== SYNC_STATE_PENDING
    ) {
      return;
    }
    Atomics.notify(this.words, OFF_STATE / 4);
  }

  /// Whether the request this call set out to answer is still the outstanding
  /// one. A newer request supersedes it, and a withdrawal ends it.
  #stillOurs(token) {
    return (
      token === this.serves &&
      Atomics.load(this.words, OFF_STATE / 4) === SYNC_STATE_PENDING
    );
  }

  /** Whether a request is outstanding, for a caller deciding whether to serve. */
  get isPending() {
    return Atomics.load(this.words, OFF_STATE / 4) === SYNC_STATE_PENDING;
  }

  #fail(code, requestId) {
    if (Number.isInteger(requestId)) {
      this.view.setUint32(OFF_REQUEST_ID, requestId, true);
    }
    this.view.setUint32(OFF_REPLY_BYTES, 0, true);
    this.view.setUint32(OFF_ERROR, code, true);
    // Same exchange as the success path and for the same reason: a failure
    // published onto a slot the producer already left is a verdict the next
    // request would read as its own.
    if (
      Atomics.compareExchange(
        this.words,
        OFF_STATE / 4,
        SYNC_STATE_PENDING,
        SYNC_STATE_FAILED,
      ) !== SYNC_STATE_PENDING
    ) {
      return;
    }
    Atomics.notify(this.words, OFF_STATE / 4);
  }
}
