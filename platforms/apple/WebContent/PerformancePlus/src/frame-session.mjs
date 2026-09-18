// The producer's side of the frame channel: what content JavaScript talks to.
//
// It owns the three things the host and the producer tell each other about --
// the credit window, the frame clock, and the producer's demand for the next
// frame -- and nothing else. The sockets are injected, because a socket is not
// testable in `node` without a dependency and this module runs in WebContent
// beside untrusted content, where a dependency is one more thing inside that
// boundary. `worker-bootstrap.mjs` is the few lines that wire a real
// `WebSocket` to it.
//
// The host half is `MigoFrameChannel` in the Swift package; the formats are
// `downlink.mjs` and `control.mjs`, which the interop gate checks against Rust.

import { DOWN_CLOCK_TICK, DOWN_FRAME_VERDICT, decodeBytes } from "./downlink.mjs";
import { RequestFrameMessage, generationWord } from "./control.mjs";
import { sequenceOf } from "./wire-frame-packet.mjs";

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
   * @param {(bytes: Uint8Array) => void} options.send  the uplink for frames.
   * @param {(bytes: Uint8Array) => void} [options.sendControl] the uplink for
   *        control messages -- the socket, always. Defaults to `send`.
   * @param {(timestampMillis: number, frameId: number) => void} [options.onFrame]
   *        every frame-clock tick, before the callbacks `requestFrame` queued.
   * @param {(verdict: object) => void} [options.onVerdict] every verdict, for a
   *        producer that wants to log or measure. The window accounting below
   *        happens whether or not anyone is listening.
   * @param {(generation: number) => void} [options.onGenerationLost] the host
   *        replaced the runtime under us. Not an error on anyone's part and not
   *        something a retry fixes; the content has to be rebuilt.
   */
  constructor({ send, sendControl, onFrame, onVerdict, onGenerationLost } = {}) {
    if (typeof send !== "function") {
      throw new TypeError("FrameSession needs a send function");
    }
    if (sendControl !== undefined && typeof sendControl !== "function") {
      throw new TypeError("sendControl, when given, is a function");
    }
    this.#send = send;
    this.#sendControl = sendControl ?? send;
    this.#onFrame = onFrame;
    this.#onVerdict = onVerdict;
    this.#onGenerationLost = onGenerationLost;
  }

  #send;
  #sendControl;
  #onFrame;
  #onVerdict;
  #onGenerationLost;

  // The window, as "Having accepted every packet through `#accepted`, this many
  // credits were free" -- the latest advertisement the host sent, from an
  // accepted verdict or a tick -- and the highest sequence this producer sent.
  // What may be sent is `#remaining - (#sent - #accepted)`; see "The window" in
  // contracts/frame-wire/wire-v1.md for why that is safe in any order.
  //
  // `(1, 0)` before any advertisement. Not zero, which would wait for an
  // advertisement only a packet produces; not unbounded, because two packets
  // sent back to back before the first is accepted can reorder on the two
  // uplinks, and ingress holds nothing ahead of a generation's first packet.
  #remaining = 1;
  #accepted = 0;
  #sent = 0;

  #closed = false;
  #lastFrameId = 0;
  #lastTimestampMillis = 0;
  #generation = 0;

  // Demand for the next tick. One request is outstanding at most: the host
  // coalesces requests, so a second one before the tick would buy nothing.
  #requestOutstanding = false;
  #request = new RequestFrameMessage();
  // Callbacks for the next tick, and the array the previous tick ran from --
  // swapped rather than reallocated, because this runs every frame.
  #callbacks = [];
  #running = [];

  /// Packets that may be sent now.
  get credits() {
    const unadvertised = this.#sent - this.#accepted;
    if (unadvertised <= 0) return this.#remaining;
    return unadvertised >= this.#remaining ? 0 : this.#remaining - unadvertised;
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
  /// rather than a throw: a closed window is the window doing its job, which
  /// happens every time the renderer is the slower end, and a throw would make
  /// the normal case an exception. A packet that was not sent keeps its
  /// sequence; send the same packet again when the window opens.
  submit(packet) {
    if (this.#closed) return SUBMIT_CLOSED;
    if (this.credits === 0) return SUBMIT_NO_CREDIT;
    this.#send(packet);
    const sequence = sequenceOf(packet);
    if (sequence > this.#sent) {
      this.#sent = sequence;
    } else if (sequence < 0) {
      // Too short to be a packet. The host refuses it; counting it keeps the
      // window conservative rather than letting a malformed send be free.
      this.#sent += 1;
    }
    return true;
  }

  /// Ask for the next frame-clock tick, and run `callback` on it.
  ///
  /// This is what content's `requestAnimationFrame` is: the host arms one
  /// vsync per request, and the tick that answers carries the window content
  /// schedules its next frame against. `runtimeGeneration` is the generation
  /// content encodes its packets with; the host ignores a request from any
  /// other, because nothing is owed to a producer that has been replaced.
  ///
  /// Returns `true`, or `SUBMIT_CLOSED` once the session is closed.
  requestFrame(runtimeGeneration, callback) {
    if (typeof callback !== "function") {
      throw new TypeError("requestFrame needs a callback");
    }
    if (this.#closed) return SUBMIT_CLOSED;
    // Checked on every call, not only the ones that send, so a wrong argument
    // fails where it was written rather than on whichever frame asks first.
    const generation = generationWord(runtimeGeneration);
    this.#callbacks.push(callback);
    if (!this.#requestOutstanding) {
      this.#requestOutstanding = true;
      this.#sendControl(this.#request.forGeneration(generation));
    }
    return true;
  }

  /// Feed one downlink message in. Returns the records it carried, for a caller
  /// that wants to see them; the dispatch has already happened.
  handleMessage(bytes) {
    const records = decodeBytes(bytes);
    let failure = null;
    for (const record of records) {
      this.#generation = record.generation;
      if (record.kind === DOWN_FRAME_VERDICT) {
        // Only an accepted packet's verdict is an advertisement. A refusal
        // carries no accepted sequence, and applying its zero would count every
        // packet this generation ever sent against the window.
        if (record.decision === DECISION_ACCEPTED) {
          this.#advertised(record.remainingCredits, record.acceptedSequence);
        } else if (record.decision === DECISION_GENERATION_LOST) {
          this.close();
          this.#onGenerationLost?.(record.generation);
        }
        this.#onVerdict?.(record);
      } else if (record.kind === DOWN_CLOCK_TICK) {
        this.#advertised(record.remainingCredits, record.acceptedSequence);
        this.#lastFrameId = record.frameId;
        // Milliseconds for content, because that is what
        // `requestAnimationFrame` hands a callback everywhere else. The wire
        // carries nanoseconds so the host does not have to round.
        this.#lastTimestampMillis = record.timestampNs / 1_000_000;
        failure ??= this.#tick(this.#lastTimestampMillis, record.frameId);
      }
    }
    // After every record: a callback that threw must not cost the producer a
    // verdict queued behind the tick it ran on.
    if (failure !== null) throw failure;
    return records;
  }

  /// The transport went away. Further submits are refused rather than sent into
  /// a closed socket.
  close() {
    this.#closed = true;
    this.#settleCreditWaiters(false);
  }

  get isClosed() {
    return this.#closed;
  }

  /// The highest sequence this session has sent; 0 before any.
  get sentSequence() {
    return this.#sent;
  }

  /// Apply a window advertisement that did not come over the downlink: the
  /// answer to a synchronous AWAIT_WINDOW, which a producer blocked in a GL
  /// call receives while its downlink messages wait behind that call.
  ///
  /// Same meaning and same effect as the advertisement a verdict or tick
  /// carries. The downlink records queued meanwhile are older, and applying one
  /// of those afterwards only makes the window more conservative until the next
  /// arrives -- every accepted packet is answered on the downlink, so the last
  /// record there is never older than this.
  applyWindow(remainingCredits, acceptedSequence) {
    if (!Number.isInteger(remainingCredits) || remainingCredits < 0) {
      throw new TypeError("remainingCredits is a non-negative integer");
    }
    if (!Number.isSafeInteger(acceptedSequence) || acceptedSequence < 0 || acceptedSequence > this.#sent) {
      throw new RangeError(`accepted sequence ${acceptedSequence} is not one this session sent`);
    }
    this.#advertised(remainingCredits, acceptedSequence);
  }

  #advertised(remainingCredits, acceptedSequence) {
    this.#remaining = remainingCredits;
    this.#accepted = acceptedSequence;
    if (this.#creditWaiters.length > 0 && this.credits > 0) this.#settleCreditWaiters(true);
  }

  // Resolvers waiting for the window to open, settled together.
  #creditWaiters = [];

  #settleCreditWaiters(open) {
    const waiters = this.#creditWaiters;
    this.#creditWaiters = [];
    for (const settle of waiters) settle(open);
  }

  /// A promise for the window being open: `true` once a packet may be sent,
  /// `false` if the session closes first. Resolved at once when it already is.
  ///
  /// What a producer holding a packet it could not send waits on. Polling
  /// `credits` instead would either spin or add a timer's latency to every frame
  /// the renderer was briefly behind on.
  whenCredit() {
    if (this.#closed) return Promise.resolve(false);
    if (this.credits > 0) return Promise.resolve(true);
    return new Promise((resolve) => this.#creditWaiters.push(resolve));
  }

  #tick(timestampMillis, frameId) {
    // Cleared before any callback runs, so a callback that asks for the next
    // frame sends a new request rather than being folded into this answered one.
    this.#requestOutstanding = false;
    this.#onFrame?.(timestampMillis, frameId);

    const running = this.#callbacks;
    this.#callbacks = this.#running;
    this.#running = running;
    // Every callback runs even if one throws, as in a browser; the first error
    // is returned, and rethrown once the whole message has been applied, so it
    // still reaches whoever reports errors.
    let failure = null;
    for (let index = 0; index < running.length; index += 1) {
      try {
        running[index](timestampMillis, frameId);
      } catch (error) {
        failure ??= error;
      }
    }
    running.length = 0;
    return failure;
  }
}
