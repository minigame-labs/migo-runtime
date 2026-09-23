// The producer's half of the synchronous barrier, as it runs inside a Dedicated
// Worker in WebKit's WebContent process.
//
// This is the second of the two implementations the barrier has. The first is
// `engine/crates/frame-wire/src/sync.rs`; neither is the specification, which is
// contracts/frame-wire/wire-v1.md, and both are checked against it rather than
// against each other. Two implementations that agree with each other and not
// with the document is exactly the failure the document exists to catch.
//
// No imports, no DOM, no Node API. It runs in a Worker, where none of that
// exists, and in `node` for its test.
//
// WHY THIS BLOCKS AT ALL. Almost everything a WebGL or Canvas2D producer asks
// for is answered locally: object names are allocated here and written into the
// command stream without waiting, `getError` is a shadow of state this side
// already has, limits are fetched once. What is left is the handful of calls
// whose *return value is the answer* -- `readPixels`, `getImageData`,
// `toDataURL` -- and a function that must return pixels cannot return before
// the pixels exist. So the agent stops, on `Atomics.wait`, which is why this
// code may only ever run in a Worker: doing it on the main thread would stop
// the page.

export const SYNC_RECORD_BYTES = 64;
export const MAX_REPLY_BYTES = 16 * 1024 * 1024;
export const MAX_IN_FLIGHT = 1;

export const SYNC_STATE_FREE = 0;
export const SYNC_STATE_PENDING = 1;
export const SYNC_STATE_READY = 2;
export const SYNC_STATE_FAILED = 3;
export const SYNC_STATE_CANCELLED = 4;

export const SYNC_ERROR_ALREADY_PENDING = 1;
export const SYNC_ERROR_REQUEST_ID_MISMATCH = 2;
export const SYNC_ERROR_STALE_GENERATION = 3;
export const SYNC_ERROR_REPLY_TOO_LARGE = 4;
export const SYNC_ERROR_TIMED_OUT = 5;
export const SYNC_ERROR_SESSION_ENDED = 6;
export const SYNC_ERROR_UNSUPPORTED_OPERATION = 7;
export const SYNC_ERROR_LATE_REPLY = 8;
export const SYNC_ERROR_BAD_DEADLINE = 9;
export const SYNC_ERROR_BAD_REPLY_RESERVATION = 10;
export const SYNC_ERROR_OPERATION_FAILED = 11;

export const SYNC_OP_READ_PIXELS = 1;

// Wait for the frame window to open, and report it. No parameters; the reply
// is WINDOW_REPLY_BYTES: remaining credits (u32), a zero u32, the accepted
// sequence (u64), little-endian -- the advertisement a verdict or a tick
// carries. See `SYNC_OP_AWAIT_WINDOW` in engine/crates/frame-wire/src/sync.rs.
export const SYNC_OP_AWAIT_WINDOW = 2;
export const WINDOW_REPLY_BYTES = 16;

// One WebGL query, answered after the frame the producer names. Three
// operations, one per reply shape, because the operation is what sizes the
// reply: a scalar is four bytes, text is the bytes themselves, and an active
// variable is a size, a type and a name. Which query is in the parameters.
export const SYNC_OP_GL_QUERY_SCALAR = 3;
export const SYNC_OP_GL_QUERY_TEXT = 4;
export const SYNC_OP_GL_QUERY_ACTIVE = 5;

// The Canvas2D queries: a measurement and a line height, whose answers come
// back in shapes of their own. Two operations rather than one, for the reason
// the WebGL queries have three: the operation is what sizes the reply.
export const SYNC_OP_CANVAS2D_METRICS = 6;
export const SYNC_OP_CANVAS2D_NUMBER = 7;
/// A service op called synchronously -- `readFileSync`, `getStorageSync`. Its
/// body and reply have the service stream's bounds, not the barrier's; see
/// `frame_wire::sync::SYNC_OP_SERVICE`.
export const SYNC_OP_SERVICE = 8;
/// What a SERVICE call may be answered with: a whole file and its framing.
export const MAX_SERVICE_REPLY_BYTES = 128 * 1024 * 1024;

export const CANVAS2D_QUERY_MEASURE_TEXT = 1;
export const CANVAS2D_QUERY_TEXT_LINE_HEIGHT = 2;

/** The four fields and the two lengths before a 2D query's strings. */
export const CANVAS2D_QUERY_HEADER_BYTES = 24;
/** Twelve `f32`: the `TextMetrics` fields, in the order the host writes them. */
export const TEXT_METRICS_BYTES = 48;
/** The host's cap on either string a 2D query carries. */
export const CANVAS2D_QUERY_MAX_TEXT_BYTES = 2048;

export const CANVAS2D_FLAG_BOLD = 1;
export const CANVAS2D_FLAG_ITALIC = 2;

/**
 * Encode a Canvas2D query's arguments: four words, two lengths, then the two
 * strings, each padded to a word with zeros.
 *
 * Two strings rather than one joined pair, because a text that contained the
 * separator would otherwise be a different query.
 */
export function encodeCanvas2DQueryParams({ kind, canvasId = 0, number = 0, flags = 0, text = "", font = "" }) {
  const textBytes = queryNameEncoder.encode(text);
  const fontBytes = queryNameEncoder.encode(font);
  for (const bytes of [textBytes, fontBytes]) {
    if (bytes.byteLength > CANVAS2D_QUERY_MAX_TEXT_BYTES) {
      throw new RangeError(
        `a 2D query string of ${bytes.byteLength} bytes is past the ${CANVAS2D_QUERY_MAX_TEXT_BYTES}-byte limit`,
      );
    }
  }
  const textPadded = (textBytes.byteLength + 3) & ~3;
  const fontPadded = (fontBytes.byteLength + 3) & ~3;
  const out = new Uint8Array(CANVAS2D_QUERY_HEADER_BYTES + textPadded + fontPadded);
  const view = new DataView(out.buffer);
  view.setUint32(0, kind, true);
  view.setUint32(4, canvasId, true);
  // The size crosses as an `f32`'s bits, which is the width the op takes.
  view.setFloat32(8, number, true);
  view.setUint32(12, flags, true);
  view.setUint32(16, textBytes.byteLength, true);
  view.setUint32(20, fontBytes.byteLength, true);
  out.set(textBytes, CANVAS2D_QUERY_HEADER_BYTES);
  out.set(fontBytes, CANVAS2D_QUERY_HEADER_BYTES + textPadded);
  return out;
}

/** The twelve `f32` of a `measureText` answer, as the facade reads them. */
export function decodeMetricsReply(reply) {
  if (reply.byteLength !== TEXT_METRICS_BYTES) {
    throw new RangeError(`text metrics are ${TEXT_METRICS_BYTES} bytes, not ${reply.byteLength}`);
  }
  // Copied rather than viewed: the reply may be a view over a buffer the next
  // call reuses, and the facade keeps these numbers.
  return new Float32Array(reply.slice().buffer);
}

/** The `f64` of a number answer. */
export function decodeNumberReply(reply) {
  if (reply.byteLength !== 8) {
    throw new RangeError(`a number answer is eight bytes, not ${reply.byteLength}`);
  }
  return new DataView(reply.buffer, reply.byteOffset, 8).getFloat64(0, true);
}

// Which query. The host's table is `frame_wire::sync::gl_query`, and the
// interop gate holds the two together.
export const GL_QUERY_PROGRAM_PARAMETER = 1;
export const GL_QUERY_SHADER_PARAMETER = 2;
export const GL_QUERY_QUERY_PARAMETER = 3;
export const GL_QUERY_CHECK_FRAMEBUFFER_STATUS = 4;
export const GL_QUERY_CLIENT_WAIT_SYNC = 5;
export const GL_QUERY_GET_ERROR = 6;
export const GL_QUERY_UNIFORM_LOCATION = 7;
export const GL_QUERY_ATTRIB_LOCATION = 8;
export const GL_QUERY_UNIFORM_BLOCK_INDEX = 9;
export const GL_QUERY_PROGRAM_INFO_LOG = 10;
export const GL_QUERY_SHADER_INFO_LOG = 11;
export const GL_QUERY_PARAMETER = 12;
export const GL_QUERY_ACTIVE_ATTRIB = 13;
export const GL_QUERY_ACTIVE_UNIFORM = 14;
export const GL_QUERY_TRANSFORM_FEEDBACK_VARYING = 15;

/** Words before a query's name: five fields and the name's byte length. */
export const GL_QUERY_HEADER_BYTES = 24;
/** `size` and `type` before an active variable's name. */
export const ACTIVE_VARIABLE_HEADER_BYTES = 8;
/** The longest name a query may carry, as the host bounds it. */
export const GL_QUERY_MAX_NAME_BYTES = 1024;

const queryNameEncoder = new TextEncoder();
const queryReplyDecoder = new TextDecoder();

/**
 * Encode a WebGL query's arguments: five words, the name's length, then the
 * name's UTF-8 bytes padded to a word with zeros -- the frame stream's payload
 * rule, and the host refuses padding that is not zero.
 */
export function encodeGlQueryParams({ kind, canvasId = 0, object = 0, pname = 0, extra = 0, name = "" }) {
  const encoded = name.length === 0 ? EMPTY_NAME : queryNameEncoder.encode(name);
  if (encoded.byteLength > GL_QUERY_MAX_NAME_BYTES) {
    throw new RangeError(`a query name of ${encoded.byteLength} bytes is past the ${GL_QUERY_MAX_NAME_BYTES}-byte limit`);
  }
  const padded = (encoded.byteLength + 3) & ~3;
  const bytes = new Uint8Array(GL_QUERY_HEADER_BYTES + padded);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, kind, true);
  view.setUint32(4, canvasId, true);
  view.setUint32(8, object, true);
  view.setUint32(12, pname, true);
  view.setUint32(16, extra, true);
  view.setUint32(20, encoded.byteLength, true);
  bytes.set(encoded, GL_QUERY_HEADER_BYTES);
  return bytes;
}

const EMPTY_NAME = new Uint8Array(0);

/** The four bytes of a scalar answer, as a signed integer. */
export function decodeScalarReply(reply) {
  if (reply.byteLength !== 4) {
    throw new RangeError(`a scalar query answers with four bytes, not ${reply.byteLength}`);
  }
  return new DataView(reply.buffer, reply.byteOffset, 4).getInt32(0, true);
}

/** A text answer: the reply's bytes are the whole of it. */
export function decodeTextReply(reply) {
  return queryReplyDecoder.decode(reply);
}

/**
 * An active variable's answer. A zero type is the index the program does not
 * have, which WebGL reports as `null`.
 */
export function decodeActiveReply(reply) {
  if (reply.byteLength < ACTIVE_VARIABLE_HEADER_BYTES) {
    throw new RangeError(`an active variable answers with at least ${ACTIVE_VARIABLE_HEADER_BYTES} bytes, not ${reply.byteLength}`);
  }
  const view = new DataView(reply.buffer, reply.byteOffset, ACTIVE_VARIABLE_HEADER_BYTES);
  const type = view.getUint32(4, true);
  if (type === 0) return null;
  return {
    size: view.getInt32(0, true),
    type,
    name: queryReplyDecoder.decode(reply.subarray(ACTIVE_VARIABLE_HEADER_BYTES)),
  };
}

/**
 * Read an AWAIT_WINDOW reply. Refuses what the host would never send: a reply
 * of the wrong length, or a reserved word that is not zero.
 *
 * @param {Uint8Array} reply
 * @returns {{remainingCredits: number, acceptedSequence: number}}
 */
export function decodeWindowReply(reply) {
  if (!(reply instanceof Uint8Array) || reply.byteLength !== WINDOW_REPLY_BYTES) {
    throw new TypeError(`a window reply is ${WINDOW_REPLY_BYTES} bytes`);
  }
  const view = new DataView(reply.buffer, reply.byteOffset, reply.byteLength);
  if (view.getUint32(4, true) !== 0) throw new TypeError("a window reply's reserved word is zero");
  const accepted = view.getBigUint64(8, true);
  if (accepted > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new RangeError("an accepted sequence past 2^53 is not one this producer sent");
  }
  return { remainingCredits: view.getUint32(0, true), acceptedSequence: Number(accepted) };
}

// Record offsets, in the order the document lists them.
export const OFF_STATE = 0;
export const OFF_REQUEST_ID = 4;
export const OFF_RUNTIME_GENERATION = 8;
export const OFF_SURFACE_GENERATION = 16;
export const OFF_RESOURCE_EPOCH = 24;
export const OFF_TRIGGERING_SEQUENCE = 32;
export const OFF_OPERATION = 40;
export const OFF_MAX_REPLY_BYTES = 44;
export const OFF_REPLY_BYTES = 48;
export const OFF_ERROR = 52;
export const OFF_DEADLINE_NANOS = 56;

// `readPixels`' arguments, which are NOT in the record: the record is the
// rendezvous cell, and per-operation arguments would size it by the largest
// operation anyone ever adds. They travel beside the request.
export const READ_PIXELS_PARAM_BYTES = 32;
export const GL_RGBA = 0x1908;
export const GL_UNSIGNED_BYTE = 0x1401;

/** Every error code, so a caller can name one without a magic number. */
export const SYNC_ERROR_TEXT = {
  [SYNC_ERROR_ALREADY_PENDING]: "a request is already outstanding",
  [SYNC_ERROR_REQUEST_ID_MISMATCH]: "the reply does not answer this request",
  [SYNC_ERROR_STALE_GENERATION]: "a generation or epoch moved under the request",
  [SYNC_ERROR_REPLY_TOO_LARGE]: "the reply is larger than was reserved",
  [SYNC_ERROR_TIMED_OUT]: "the deadline passed with no reply",
  [SYNC_ERROR_SESSION_ENDED]: "the session ended while the producer was waiting",
  [SYNC_ERROR_UNSUPPORTED_OPERATION]: "this host does not implement that operation",
  [SYNC_ERROR_LATE_REPLY]: "the reply arrived after the request was settled",
  [SYNC_ERROR_BAD_DEADLINE]: "the deadline is not in the future",
  [SYNC_ERROR_BAD_REPLY_RESERVATION]: "the reserved reply size is outside the protocol's bounds",
  [SYNC_ERROR_OPERATION_FAILED]: "the host tried the operation and it failed",
};

/// A rectangle of a 2D canvas, as `getImageData` reads one when the facade
/// cannot capture it (`frame_wire::sync::SYNC_OP_CANVAS2D_IMAGE_DATA`).
/// `loadFont(path, family)`, answered with the family key the renderer
/// registered the face under -- empty when it did not load
/// (`frame_wire::sync::SYNC_OP_CANVAS2D_FONT`).
export const SYNC_OP_CANVAS2D_FONT = 11;

/// The query kind that asks it (`frame_wire::sync::canvas2d_query::LOAD_FONT`).
export const CANVAS2D_QUERY_LOAD_FONT = 3;

/// The most a family key may be, as the host bounds it.
export const MAX_FONT_FAMILY_REPLY_BYTES = 4096;

/// `readPixels` into the bound `PIXEL_PACK_BUFFER`
/// (`frame_wire::sync::SYNC_OP_READ_PIXELS_TO_BUFFER`). Nothing comes back but
/// the WebGL error it raised, or zero.
export const SYNC_OP_READ_PIXELS_TO_BUFFER = 12;

/// Serialised size of its arguments, and of its answer.
export const READ_PIXELS_TO_BUFFER_PARAM_BYTES = 40;
export const READ_PIXELS_TO_BUFFER_REPLY_BYTES = 4;

/** Encode a pack-buffer readback's arguments: seven words, then an i64. */
export function encodeReadPixelsToBufferParams({ canvasId, x, y, width, height, format, type, offset }) {
  const bytes = new Uint8Array(READ_PIXELS_TO_BUFFER_PARAM_BYTES);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, canvasId, true);
  view.setInt32(4, x, true);
  view.setInt32(8, y, true);
  view.setInt32(12, width, true);
  view.setInt32(16, height, true);
  view.setUint32(20, format, true);
  view.setUint32(24, type, true);
  view.setBigInt64(28, offset, true);
  view.setUint32(36, 0, true);
  return bytes;
}

export const SYNC_OP_CANVAS2D_IMAGE_DATA = 9;

/// The pixels of a snapshot the host captured
/// (`frame_wire::sync::SYNC_OP_CANVAS2D_SNAPSHOT`).
export const SYNC_OP_CANVAS2D_SNAPSHOT = 10;

/// Serialised size of the arguments both of those take.
export const CANVAS2D_PIXELS_PARAM_BYTES = 24;

/**
 * Encode a 2D pixel read's arguments: six little-endian 32-bit words.
 *
 * `target` is the canvas for an image-data read and the snapshot for a snapshot
 * read; the size travels for both, so the producer's reservation and the host's
 * expectation are one number. The last word is reserved and must be zero -- the
 * host refuses a record that sets it rather than ignoring it.
 */
export function encodeCanvas2DPixelsParams({ target, x = 0, y = 0, width, height }) {
  const bytes = new Uint8Array(CANVAS2D_PIXELS_PARAM_BYTES);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, target, true);
  view.setInt32(4, x, true);
  view.setInt32(8, y, true);
  view.setUint32(12, width, true);
  view.setUint32(16, height, true);
  view.setUint32(20, 0, true);
  return bytes;
}

/**
 * Encode `readPixels`' arguments.
 *
 * Eight little-endian 32-bit words, which is the whole record: every field is
 * four bytes, so it is 32 bytes with no interior padding on LP64 and ILP32
 * alike -- one layout, rather than two that happen to agree today.
 */
export function encodeReadPixelsParams({
  canvasId,
  x,
  y,
  width,
  height,
  format = GL_RGBA,
  type = GL_UNSIGNED_BYTE,
}) {
  const bytes = new Uint8Array(READ_PIXELS_PARAM_BYTES);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, canvasId, true);
  view.setInt32(4, x, true);
  view.setInt32(8, y, true);
  view.setInt32(12, width, true);
  view.setInt32(16, height, true);
  view.setUint32(20, format, true);
  view.setUint32(24, type, true);
  view.setUint32(28, 0, true);
  return bytes;
}

/**
 * Bytes of the layout header a `readPixels` reply begins with.
 *
 * `frame_wire::sync::READ_PIXELS_LAYOUT_BYTES`: four little-endian `u32`s --
 * first byte, row bytes, row stride, rows -- saying where the rows go in the
 * caller's view. The `PACK_*` state that decides that is the host's, and this
 * side never sees `pixelStorei`: the engine's own encoder writes it into the
 * command stream this producer forwards unread.
 */
export const READ_PIXELS_LAYOUT_BYTES = 16;

/** How many bytes `readPixels` over this rectangle answers with, header included. */
export function readPixelsReplyBytes(width, height) {
  return READ_PIXELS_LAYOUT_BYTES + width * height * 4;
}

/** Read the layout header a reply begins with. */
export function decodeReadPixelsLayout(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return {
    firstByte: view.getUint32(0, true),
    rowBytes: view.getUint32(4, true),
    rowStride: view.getUint32(8, true),
    height: view.getUint32(12, true),
  };
}

/**
 * The producer's view of the mailbox.
 *
 * Constructed over a `SharedArrayBuffer` the relay and this agent both hold.
 * Same process -- the relay runs in the page and this runs in its Worker -- so
 * the atomics work, which is the whole reason the topology puts them there.
 */
export class SyncMailbox {
  /**
   * @param {SharedArrayBuffer} buffer at least SYNC_RECORD_BYTES long
   * @param {object} channel  how a request reaches the host and a reply returns
   */
  constructor(buffer, channel) {
    if (buffer.byteLength < SYNC_RECORD_BYTES) {
      throw new RangeError(
        `the mailbox needs ${SYNC_RECORD_BYTES} bytes, was given ${buffer.byteLength}`,
      );
    }
    // Int32Array for the atomics, DataView for the wide fields: `Atomics.wait`
    // takes an Int32Array and nothing else, and the 64-bit fields are BigInt
    // because a JavaScript Number carries 53 bits exactly -- an encoder that
    // reached for Number would work on every value anyone writes by hand and
    // corrupt the identity of a real session.
    this.words = new Int32Array(buffer, 0, SYNC_RECORD_BYTES / 4);
    this.view = new DataView(buffer, 0, SYNC_RECORD_BYTES);
    this.channel = channel;
  }

  get state() {
    return Atomics.load(this.words, OFF_STATE / 4);
  }

  get requestId() {
    return Atomics.load(this.words, OFF_REQUEST_ID / 4) >>> 0;
  }

  get replyBytes() {
    return Atomics.load(this.words, OFF_REPLY_BYTES / 4) >>> 0;
  }

  get error() {
    return Atomics.load(this.words, OFF_ERROR / 4) >>> 0;
  }

  /**
   * Block until this request is answered, and return the answer.
   *
   * Throws rather than returning a sentinel on every failure, because the calls
   * this stands in for are `readPixels` and `getImageData`: a caller that
   * ignores a return value it did not know could fail would read whatever was
   * in its buffer, and that is the wrong answer that looks like a right one the
   * whole protocol is arranged to prevent.
   */
  request({
    operation,
    params,
    maxReplyBytes,
    runtimeGeneration,
    surfaceGeneration,
    resourceEpoch,
    triggeringSequence,
    deadlineNanos,
    nowNanos,
    into,
  }) {
    if (this.state !== SYNC_STATE_FREE) {
      throw new SyncRequestError(SYNC_ERROR_ALREADY_PENDING);
    }
    if (!(maxReplyBytes >= 1 && maxReplyBytes <= MAX_REPLY_BYTES)) {
      throw new SyncRequestError(SYNC_ERROR_BAD_REPLY_RESERVATION);
    }
    if (!(deadlineNanos > nowNanos)) {
      throw new SyncRequestError(SYNC_ERROR_BAD_DEADLINE);
    }

    // Written before the state is published, so a relay that observes PENDING
    // observes a complete request. The state store is the release.
    this.view.setBigUint64(OFF_RUNTIME_GENERATION, runtimeGeneration, true);
    this.view.setBigUint64(OFF_SURFACE_GENERATION, surfaceGeneration, true);
    this.view.setBigUint64(OFF_RESOURCE_EPOCH, resourceEpoch, true);
    this.view.setBigUint64(OFF_TRIGGERING_SEQUENCE, triggeringSequence, true);
    this.view.setBigUint64(OFF_DEADLINE_NANOS, deadlineNanos, true);
    this.view.setUint32(OFF_OPERATION, operation, true);
    this.view.setUint32(OFF_MAX_REPLY_BYTES, maxReplyBytes, true);
    this.view.setUint32(OFF_REPLY_BYTES, 0, true);
    this.view.setUint32(OFF_ERROR, 0, true);
    Atomics.store(this.words, OFF_STATE / 4, SYNC_STATE_PENDING);

    this.channel.post(params);

    // The deadline is the producer's own, so the wait is bounded by it and not
    // by a number this file picked. A wait that returned "timed-out" while the
    // host was still working would be this side inventing a verdict.
    const budgetMillis = Number((deadlineNanos - nowNanos) / 1000000n);
    this.#waitWhilePending(budgetMillis);

    let state = this.state;
    if (state === SYNC_STATE_PENDING) {
      // The deadline passed with the host still holding it. Withdrawing is not
      // politeness: a reply that arrives after this returns would otherwise
      // land on a slot the next request is using, and the producer would read
      // another call's pixels. CANCELLED is the state the document gives for a
      // producer withdrawing, and it is what the host's own mailbox settles to.
      //
      // Compare-and-exchange rather than a store, and for the mirror of the
      // reason the relay uses one: between the wait returning and this line,
      // the relay may have published an answer. A plain store would overwrite
      // it -- the producer would report a timeout for a call that WAS answered,
      // and the answer would be lost at the boundary rather than anywhere it
      // could be noticed. If the exchange fails, the state it returns is the
      // verdict that landed, and it is taken below exactly as if the wait had
      // seen it.
      const previous = Atomics.compareExchange(
        this.words,
        OFF_STATE / 4,
        SYNC_STATE_PENDING,
        SYNC_STATE_CANCELLED,
      );
      if (previous === SYNC_STATE_PENDING) {
        Atomics.notify(this.words, OFF_STATE / 4);
        this.channel.withdraw?.();
        this.#clear();
        throw new SyncRequestError(SYNC_ERROR_TIMED_OUT);
      }
      state = previous;
    }
    if (state === SYNC_STATE_READY) {
      const bytes = this.replyBytes;
      // `into` is the caller's destination, and passing it down matters on the
      // path this is for. `readPixels` writes into a view its caller already
      // allocated, so without it the answer is copied twice more than it needs
      // to be: once out of the shared buffer into a fresh array, and once out
      // of that into the caller's view. A full-screen readback on a phone at 4x
      // is about 14 MiB, and the producer is blocked for every byte of it.
      const reply = this.channel.takeReply(bytes, into);
      this.#clear();
      return reply;
    }
    const error = state === SYNC_STATE_CANCELLED ? 0 : this.error;
    this.#clear();
    if (state === SYNC_STATE_CANCELLED) {
      throw new SyncRequestError(0, "the request was withdrawn");
    }
    throw new SyncRequestError(error);
  }

  /**
   * Wait until the cell leaves PENDING, or the budget runs out.
   *
   * Looped rather than waited once, because `Atomics.wait` may return `ok`
   * spuriously and because a notify that arrives between the state store and
   * the wait would otherwise be missed -- the value check inside `wait` closes
   * that window only if the wait is re-entered on every wake that is not a
   * settle.
   */
  #waitWhilePending(budgetMillis) {
    // `performance.now()`, not `Date.now()`. The budget is a duration, so it is
    // clock-independent -- but the elapsed time inside the wait is not, and wall
    // time can be stepped. That is the same hazard the record's `deadline_nanos`
    // is monotonic for: a producer blocked across a clock adjustment would
    // otherwise wake early or never, and getting that right in the field the
    // document calls out while getting it wrong in the loop that consumes it
    // would be a strange place to stop.
    const deadline = performance.now() + budgetMillis;
    for (;;) {
      const remaining = deadline - performance.now();
      if (remaining <= 0) {
        return;
      }
      if (Atomics.wait(this.words, OFF_STATE / 4, SYNC_STATE_PENDING, remaining) === "timed-out") {
        return;
      }
      if (this.state !== SYNC_STATE_PENDING) {
        return;
      }
    }
  }

  /**
   * Return the slot to FREE.
   *
   * The producer clears it rather than the host, because the producer is what
   * knows it has read the answer. A host that cleared on send would free the
   * slot while the bytes were still in flight.
   */
  #clear() {
    this.view.setUint32(OFF_REQUEST_ID, 0, true);
    this.view.setUint32(OFF_REPLY_BYTES, 0, true);
    this.view.setUint32(OFF_ERROR, 0, true);
    Atomics.store(this.words, OFF_STATE / 4, SYNC_STATE_FREE);
  }
}

/** A synchronous call that did not produce an answer. */
export class SyncRequestError extends Error {
  constructor(code, text) {
    super(text ?? SYNC_ERROR_TEXT[code] ?? `synchronous request failed (${code})`);
    this.name = "SyncRequestError";
    this.code = code;
  }
}
