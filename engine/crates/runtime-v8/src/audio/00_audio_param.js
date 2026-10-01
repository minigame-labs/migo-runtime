import {
  op_audio_param_set_value_at_time,
  op_audio_param_linear_ramp,
  op_audio_param_exponential_ramp,
  op_audio_param_set_target,
  op_audio_param_cancel_scheduled,
  op_audio_set_node_param,
} from "ext:core/ops";
import { core, primordials } from "ext:core/mod.js";

// The Rust side declares this error's class as "AudioError" (see audio/ops.rs). deno_core
// can only construct it if a JS constructor is registered under that exact name;
// without one the throw arrives in JS as literal `undefined`, and every handler
// that reads `.message` off it fails instead of reporting the error. Registered
// here for the same reason `IOError` is registered in 02_file_manager.js.
const { Error: _PrimError } = primordials;
class AudioError extends _PrimError {
  constructor(msg) {
    super(msg);
    this.name = "AudioError";
  }
}
core.registerErrorClass("AudioError", AudioError);

// What a decode that could not read its bytes is thrown as by the op; `decodeAudioData` turns it into the
// DOMException the specification names (see 01_audio_context.js). Registered for the same reason as AudioError.
class AudioEncodingError extends _PrimError {
  constructor(msg) {
    super(msg);
    this.name = "EncodingError";
  }
}
core.registerErrorClass("EncodingError", AudioEncodingError);


class AudioParam {
  #value;
  #defaultValue;
  #minValue;
  #maxValue;
  // Node ID and param name for native automation dispatch
  #nodeId = null;
  #paramName = null;

  constructor(defaultValue, minValue = -3.4028235e38, maxValue = 3.4028235e38) {
    this.#value = defaultValue;
    this.#defaultValue = defaultValue;
    this.#minValue = minValue;
    this.#maxValue = maxValue;
  }

  // Internal: bind this param to a specific node and param name
  _bind(nodeId, paramName) {
    this.#nodeId = nodeId;
    this.#paramName = paramName;
  }

  get value() {
    return this.#value;
  }

  set value(v) {
    const val = Number(v);
    if (Number.isNaN(val)) return;
    this.#value = Math.max(this.#minValue, Math.min(this.#maxValue, val));
    this._onValueSet(this.#value);
  }

  // Push a direct `.value = x` to native. Bound params (oscillator freq, biquad,
  // delay, compressor, panner, buffer-source playbackRate, ...) have native DSP
  // that reads the param, so a plain assignment must reach native or it's a no-op.
  // Sets the intrinsic value directly (no timeline event), so repeated writes and
  // interaction with scheduled automation stay coherent. Overridden by
  // NativeAudioParam for params with a dedicated native setter.
  _onValueSet(val) {
    if (this.#nodeId !== null) {
      op_audio_set_node_param(this.#nodeId, this.#paramName, val);
    }
  }

  get defaultValue() {
    return this.#defaultValue;
  }

  get minValue() {
    return this.#minValue;
  }

  get maxValue() {
    return this.#maxValue;
  }

  setValueAtTime(value, startTime) {
    this.#value = value;
    if (this.#nodeId !== null) {
      op_audio_param_set_value_at_time(this.#nodeId, this.#paramName, value, startTime);
    }
    return this;
  }

  linearRampToValueAtTime(value, endTime) {
    this.#value = value;
    if (this.#nodeId !== null) {
      op_audio_param_linear_ramp(this.#nodeId, this.#paramName, value, endTime);
    }
    return this;
  }

  exponentialRampToValueAtTime(value, endTime) {
    this.#value = value;
    if (this.#nodeId !== null) {
      op_audio_param_exponential_ramp(this.#nodeId, this.#paramName, value, endTime);
    }
    return this;
  }

  setTargetAtTime(target, startTime, timeConstant) {
    this.#value = target;
    if (this.#nodeId !== null) {
      op_audio_param_set_target(this.#nodeId, this.#paramName, target, startTime, timeConstant);
    }
    return this;
  }

  cancelScheduledValues(cancelTime) {
    if (this.#nodeId !== null) {
      op_audio_param_cancel_scheduled(this.#nodeId, this.#paramName, cancelTime);
    }
    return this;
  }
}

/**
 * AudioParam with native callback support for immediate value changes.
 * Used by GainNode, etc. where .value assignment must immediately
 * update the native side.
 */
class NativeAudioParam extends AudioParam {
  #onChangeCallback;

  constructor(defaultValue, minValue, maxValue, onChangeCallback) {
    super(defaultValue, minValue, maxValue);
    this.#onChangeCallback = onChangeCallback;
  }

  // Use the dedicated native setter (e.g. op_audio_set_gain_value) instead of the
  // generic set-value-at-time op, so a `.value =` doesn't double-dispatch.
  _onValueSet(val) {
    if (this.#onChangeCallback) {
      this.#onChangeCallback(val);
    }
  }
}

export { AudioParam, NativeAudioParam };
