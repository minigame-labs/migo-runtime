// The producer's side of the frame channel: what content JavaScript talks to.
//
// It owns the two things the host tells it about -- the credit level and the
// frame clock -- and nothing else. The socket is injected, because a socket is
// not testable in `node` without a dependency and this module runs in WebContent
// beside untrusted content, where a dependency is one more thing inside that
// boundary. `worker-bootstrap.mjs` is the twelve lines that wire a real
// `WebSocket` to it.
//
// The host half is `MigoFrameChannel` in the Swift package; the format is
// `downlink.mjs`, which is the half of this pair that the interop gate checks
// against Rust.

import {
  DOWN_CLOCK_TICK,
  DOWN_FRAME_VERDICT,
  decodeBytes,
} from "./downlink.mjs";

/// `IngressDecision` in `engine/crates/frame-wire/src/ingress.rs`. The numbers
/// cross the C ABI and are never renumbered, only appended.
export const DECISION_ACCEPTED = 1;
export const DECISION_WOULD_BLOCK = 2;
export const DECISION_REJECTED = 3;
export const DECISION_GENERATION_LOST = 4;

/// Why a frame was not sent.
export const SUBMIT_NO_CREDIT = "no-credit";
export const SUBMIT_CLOSED = "closed";

export class FrameSession {
  /**
   * @param {object} options
   * @param {(bytes: Uint8Array) => void} options.send  the uplink.
   * @param {(timestampMillis: number, frameId: number) => void} [options.onFrame]
   *        one frame-clock tick; this is what content's `requestAnimationFrame`
   *        resolves on.
   * @param {(verdict: object) => void} [options.onVerdict] every verdict, for a
   *        producer that wants to log or measure. The credit accounting below
   *        happens whether or not anyone is listening.
   * @param {(generation: number) => void} [options.onGenerationLost] the host
   *        replaced the runtime under us. Not an error on anyone's part and not
   *        something a retry fixes; the content has to be rebuilt.
   */
  constructor({ send, onFrame, onVerdict, onGenerationLost } = {}) {
    if (typeof send !== "function") {
      throw new TypeError("FrameSession needs a send function");
    }
    this.#send = send;
    this.#onFrame = onFrame;
    this.#onVerdict = onVerdict;
    this.#onGenerationLost = onGenerationLost;
  }

  #send;
  #onFrame;
  #onVerdict;
  #onGenerationLost;
  // Null until the first verdict: the producer has to be allowed to send the
  // first frame, and the first frame is what produces the first verdict. A zero
  // here would deadlock the session against a level nobody has published yet.
  #credits = null;
  #closed = false;
  #lastFrameId = 0;
  #lastTimestampMillis = 0;
  #generation = 0;

  /// The credit level as of the last verdict, less whatever has been sent
  /// since. Null before the first verdict.
  get credits() {
    return this.#credits;
  }

  /// The most recent tick, as content sees it.
  get lastFrame() {
    return { frameId: this.#lastFrameId, timestampMillis: this.#lastTimestampMillis };
  }

  /// The runtime generation the host last stamped a record with.
  get generation() {
    return this.#generation;
  }

  /// Send one frame packet.
  ///
  /// Returns `true` when it went, or a reason string when it did not. A reason
  /// rather than a throw: running out of credit is the window doing its job,
  /// which happens every time the renderer is the slower end, and a throw would
  /// make the normal case an exception.
  submit(packet) {
    if (this.#closed) return SUBMIT_CLOSED;
    if (this.#credits === 0) return SUBMIT_NO_CREDIT;
    this.#send(packet);
    // Decremented optimistically, because the verdict that would tell us the
    // real level has not arrived yet and a producer that waited for it would
    // never have more than one frame in flight. The verdict is absolute, so
    // this estimate is corrected rather than accumulated.
    if (this.#credits !== null && this.#credits > 0) {
      this.#credits -= 1;
    }
    return true;
  }

  /// Feed one downlink message in. Returns the records it carried, for a caller
  /// that wants to see them; the dispatch above has already happened.
  handleMessage(bytes) {
    const records = decodeBytes(bytes);
    for (const record of records) {
      this.#generation = record.generation;
      if (record.kind === DOWN_FRAME_VERDICT) {
        // Absolute, not a delta. This is the line that makes a dropped verdict
        // survivable and the reason the format has no acknowledgement.
        this.#credits = record.remainingCredits;
        if (record.decision === DECISION_GENERATION_LOST) {
          this.#closed = true;
          this.#onGenerationLost?.(record.generation);
        }
        this.#onVerdict?.(record);
      } else if (record.kind === DOWN_CLOCK_TICK) {
        this.#lastFrameId = record.frameId;
        // Milliseconds for content, because that is what
        // `requestAnimationFrame` hands a callback everywhere else. The wire
        // carries nanoseconds so the host does not have to round.
        this.#lastTimestampMillis = record.timestampNs / 1_000_000;
        this.#onFrame?.(this.#lastTimestampMillis, record.frameId);
      }
    }
    return records;
  }

  /// The transport went away. Further submits are refused rather than sent into
  /// a closed socket.
  close() {
    this.#closed = true;
  }

  get isClosed() {
    return this.#closed;
  }
}
