// Web Audio and InnerAudioContext on the service stream: the checks a command
// makes before it is sent, and the answers as the embedded ops hand them to
// JavaScript.
//
// Audio plays on the host (engine/crates/core/src/runtime/service_audio.rs);
// what crosses is the control stream. Most audio ops are commands, which have
// no answer -- so an argument the embedded op refuses has to be refused here,
// before the command leaves, with the class and the words the embedded op
// throws. Each check below restates one of `migo_services::audio`'s, and
// test/audio-checks.test.mjs holds them to the answers that crate generates
// (test/fixtures/audio-check-answers.json). What a producer cannot know -- a
// full audio queue, an id the host has never seen -- the host logs, which is
// what the command lane promises.
//
// A decode does not bring its PCM back: the host adopts it as the buffer's
// samples and answers the buffer's shape and id, and the engine's AudioBuffer
// starts with no backing. So an AudioBuffer's samples cross only when content
// reads its channels (`op_audio_materialize_buffer`) or starts a buffer it
// wrote.

import { constructOpError } from "./engine-core.mjs";

/** The class every audio failure is thrown as. */
export const CLASS_AUDIO_ERROR = "AudioError";
/** A name or enum-like string's bound, in UTF-8 bytes. */
export const AUDIO_CONTROL_STRING_LIMIT = 4 * 1024;
const MAX_WAVE_SHAPER_CURVE_BYTES = 16 * 1024 * 1024;

export function audioError(message) {
  return constructOpError(CLASS_AUDIO_ERROR, message);
}

// An engine error as the audio ops show one: `[Code] summary (detail)`, with
// the summary `ErrorCode::default_message` gives.
const saturated = (detail) => `[InputSaturated] input transport saturated (${detail})`;
const invalidArgument = (detail) => `[InvalidArgument] invalid argument (${detail})`;

/** A time on the context's timeline: finite and not negative. */
export function checkScheduledTime(name, value) {
  if (!Number.isFinite(value) || value < 0) {
    throw audioError(`${name} must be a finite, non-negative number`);
  }
}

/** A `start()` duration, where -1 is "until the end". */
export function checkOptionalDuration(value) {
  if (value !== -1) checkScheduledTime("duration", value);
}

/**
 * The UTF-8 length of `text` as the op's `#[string]` conversion encodes it: a
 * lone surrogate becomes U+FFFD, three bytes, as it does on the wire.
 */
export function utf8Length(text) {
  let bytes = 0;
  for (let index = 0; index < text.length; index += 1) {
    const unit = text.charCodeAt(index);
    if (unit < 0x80) bytes += 1;
    else if (unit < 0x800) bytes += 2;
    else if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = index + 1 < text.length ? text.charCodeAt(index + 1) : 0;
      if (next >= 0xdc00 && next <= 0xdfff) {
        bytes += 4;
        index += 1;
      } else {
        bytes += 3;
      }
    } else bytes += 3;
  }
  return bytes;
}

/** A name or enum-like string, bounded so it cannot fill the host's queue. */
export function checkControlString(field, text) {
  // No UTF-16 unit encodes to more than three bytes, so a string this short
  // is within the bound without being counted -- which is every string a
  // well-formed call passes.
  if (text.length * 3 <= AUDIO_CONTROL_STRING_LIMIT) return;
  const bytes = utf8Length(text);
  if (bytes > AUDIO_CONTROL_STRING_LIMIT) {
    throw audioError(saturated(`${field} is ${bytes} bytes; limit is ${AUDIO_CONTROL_STRING_LIMIT}`));
  }
}

/** Loop points: any finite numbers. */
export function checkLoopPoints(loopStart, loopEnd) {
  if (!Number.isFinite(loopStart) || !Number.isFinite(loopEnd)) {
    throw audioError("loopStart and loopEnd must be finite numbers");
  }
}

/** IIR coefficients, as Web Audio requires them. */
export function checkIirCoefficients(feedforward, feedback) {
  if (feedforward.length === 0 || feedback.length === 0) {
    throw audioError("createIIRFilter: feedforward and feedback must be non-empty");
  }
  if (feedforward.length > 20 || feedback.length > 20) {
    throw audioError("createIIRFilter: coefficient arrays must have at most 20 elements");
  }
  if (feedback[0] === 0) {
    throw audioError("createIIRFilter: feedback[0] must not be zero");
  }
  if (!feedforward.every(Number.isFinite) || !feedback.every(Number.isFinite)) {
    throw audioError("createIIRFilter: coefficients must be finite");
  }
}

const MAX_ENCODED_AUDIO_BYTES = 16 * 1024 * 1024;

/**
 * Encoded audio a decode takes. The host refuses a larger one too; refusing it
 * here is what keeps it from crossing first.
 */
export function checkEncodedAudioBytes(byteLength) {
  if (byteLength > MAX_ENCODED_AUDIO_BYTES) {
    throw audioError(
      saturated(`encoded audio input is ${byteLength} bytes; limit is ${MAX_ENCODED_AUDIO_BYTES} bytes`),
    );
  }
}

/** A WaveShaper curve's bytes: whole `f32`s, bounded. */
export function checkWaveShaperCurveBytes(byteLength) {
  if (byteLength % 4 !== 0) {
    throw audioError(invalidArgument("WaveShaper curve bytes must be f32 aligned"));
  }
  if (byteLength > MAX_WAVE_SHAPER_CURVE_BYTES) {
    throw audioError(
      saturated(`WaveShaper curve is ${byteLength} bytes; limit is ${MAX_WAVE_SHAPER_CURVE_BYTES}`),
    );
  }
}

const isArrayBuffer = (value) => Object.prototype.toString.call(value) === "[object ArrayBuffer]";

/**
 * `v8::Local<v8::ArrayBuffer>`: an ArrayBuffer, and nothing else -- the
 * TypeError is deno_core's for a value of the wrong type.
 */
export function arrayBufferOf(value) {
  if (!isArrayBuffer(value)) throw new TypeError("expected ArrayBuffer");
  return value;
}

/** `Option<v8::Local<v8::ArrayBuffer>>`: null and undefined are None. */
export function optionalArrayBufferOf(value) {
  return value === null || value === undefined ? null : arrayBufferOf(value);
}

/** What the embedded op says of a backing it cannot take. */
export const UNUSABLE_BACKING = "AudioBuffer backing is detached, non-detachable, or has an invalid length";

/**
 * A writable AudioBuffer backing a start publishes must be fixed-length, which
 * the embedded op checks while JavaScript is paused on it. (Its length against
 * the buffer's shape is the host's check; the engine's AudioBuffer only ever
 * passes its own.)
 */
export function checkPlanarBacking(backing) {
  if (backing.resizable === true) {
    throw audioError("AudioBuffer backing must be non-shared and fixed-length");
  }
}

/**
 * Take an ArrayBuffer's bytes from content, as the embedded op detaches it:
 * moved into a new ArrayBuffer this lane holds only long enough to write them
 * into the message, with no copy, and content's views of the old one dead --
 * by the spec, for a buffer decoded or started with them, and so the memory is
 * not held twice.
 *
 * A buffer that cannot be detached -- already detached, or one the platform
 * will not let go of, such as a WebAssembly memory's -- is refused with
 * `message`, where the embedded op refuses it. Refused by the transfer itself,
 * which is the one check every WebKit this lane supports makes the same way.
 */
export function transferOut(buffer, message) {
  try {
    return structuredClone(buffer, { transfer: [buffer] });
  } catch {
    throw audioError(message);
  }
}

// ---- answers ----------------------------------------------------------------

/**
 * `AudioBufferInfo` as serde_v8 makes it, from the host's array in field order:
 * id, duration, sample_rate, channels, length.
 */
export function bufferInfo([id, duration, sampleRate, channels, length]) {
  return { id, duration, sample_rate: sampleRate, channels, length };
}

/**
 * `InnerAudioState` as serde_v8 makes it: current_time, duration, paused,
 * volume, loop_enabled, playback_rate, buffered.
 */
export function innerAudioState([currentTime, duration, paused, volume, loopEnabled, playbackRate, buffered]) {
  return {
    current_time: currentTime,
    duration,
    paused,
    volume,
    loop_enabled: loopEnabled,
    playback_rate: playbackRate,
    buffered,
  };
}

const LITTLE_ENDIAN = new Uint8Array(new Uint16Array([1]).buffer)[0] === 1;

/**
 * `f32`s the host sent as little-endian bytes, as the Numbers a `#[serde]
 * Vec<f32>` becomes. Read through a Float32Array over the reply's own bytes
 * where that view means the same thing -- a little-endian agent, an aligned
 * copy, which is every Apple device and every reply -- and value by value
 * otherwise.
 */
export function floatsAsNumbers(bytes) {
  const count = bytes.byteLength >>> 2;
  if (LITTLE_ENDIAN && bytes.byteOffset % 4 === 0) {
    return Array.from(new Float32Array(bytes.buffer, bytes.byteOffset, count));
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out = new Array(count);
  for (let index = 0; index < count; index += 1) out[index] = view.getFloat32(index * 4, true);
  return out;
}

/** A `#[serde] Vec<u8>`: an Array of Numbers, not a Uint8Array. */
export function bytesAsNumbers(bytes) {
  return Array.from(bytes);
}

/**
 * An `#[arraybuffer] Vec<f32>`: the ArrayBuffer itself. The reply's bytes are
 * already an exact copy of their own, so its buffer is adopted, not copied.
 */
export function arrayBufferAnswer(bytes) {
  return bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength
    ? bytes.buffer
    : bytes.slice().buffer;
}
