import {
  op_audio_set_started_buffer,
  op_audio_start_buffer,
  op_audio_stop,
  op_audio_set_loop,
} from "ext:core/ops";
import { AudioParam } from "ext:host_v8_audio/00_audio_param.js";
import { AudioBuffer } from "ext:host_v8_audio/00_audio_buffer.js";
import {
  AudioNode,
  validateFiniteDouble,
  validateScheduledTime,
  warnUnsupportedCapability,
} from "ext:host_v8_audio/00_audio_node.js";

class AudioBufferSourceNode extends AudioNode {
  #buffer = null;
  #bufferWasSet = false;
  #loop = false;
  #loopStart = 0;
  #loopEnd = 0;
  #playbackRate;
  #detune;
  #started = false;
  #onended = null;

  constructor(context, nodeId) {
    super(context, nodeId, {
      numberOfInputs: 0,
      numberOfOutputs: 1,
    });
    this.#playbackRate = new AudioParam(1.0, -3.4028235e38, 3.4028235e38);
    this.#playbackRate._bind(nodeId, "playbackRate");
    this.#detune = new AudioParam(0, -3.4028235e38, 3.4028235e38);
    this.#detune._bind(nodeId, "detune");
  }

  get buffer() {
    return this.#buffer;
  }

  set buffer(value) {
    if (value !== null && !(value instanceof AudioBuffer)) {
      throw new TypeError("buffer must be an AudioBuffer or null");
    }
    if (value !== null && this.#bufferWasSet) {
      throw new DOMException(
        "AudioBufferSourceNode.buffer may only be assigned once",
        "InvalidStateError",
      );
    }
    if (!this.#started) {
      // Association before start is purely JS state. This permits a buffer to
      // move between contexts and makes the eventual start atomic.
      if (value !== null) this.#bufferWasSet = true;
      this.#buffer = value;
      return;
    }

    const acquisition = value === null
      ? { backing: null, commit() {} }
      : value._acquireBacking();
    op_audio_set_started_buffer(
      this.context._nativeId,
      this._nodeId,
      value ? value._id : 0,
      acquisition.backing,
    );
    acquisition.commit();
    if (value !== null) this.#bufferWasSet = true;
    this.#buffer = value;
  }

  get loop() {
    return this.#loop;
  }

  set loop(value) {
    this.#loop = Boolean(value);
    op_audio_set_loop(this._nodeId, this.#loop, this.#loopStart, this.#loopEnd);
  }

  get loopStart() {
    return this.#loopStart;
  }

  set loopStart(value) {
    this.#loopStart = validateFiniteDouble(value, "loopStart");
    // Push updated loop points to native regardless of setter order
    // (e.g. loop = true; loopStart = 1; loopEnd = 2).
    op_audio_set_loop(this._nodeId, this.#loop, this.#loopStart, this.#loopEnd);
  }

  get loopEnd() {
    return this.#loopEnd;
  }

  set loopEnd(value) {
    this.#loopEnd = validateFiniteDouble(value, "loopEnd");
    op_audio_set_loop(this._nodeId, this.#loop, this.#loopStart, this.#loopEnd);
  }

  get playbackRate() {
    return this.#playbackRate;
  }

  get detune() {
    return this.#detune;
  }

  get onended() {
    return this.#onended;
  }

  set onended(value) {
    this.#onended = typeof value === "function" ? value : null;
    if (typeof value === "function") {
      warnUnsupportedCapability(
        "onended",
        "AudioBufferSourceNode onended callbacks are not dispatched by the native graph",
      );
    }
  }

  start(when = 0, offset = 0, duration) {
    if (this.#started) {
      throw new DOMException("AudioBufferSourceNode can only be started once", "InvalidStateError");
    }
    when = validateScheduledTime(when, "when");
    offset = validateScheduledTime(offset, "offset");
    const nativeDuration = duration === undefined
      ? -1
      : validateScheduledTime(duration, "duration");
    const buffer = this.#buffer;
    const acquisition = buffer === null
      ? { backing: null, commit() {} }
      : buffer._acquireBacking();
    // Use -1 to indicate no duration limit. The native op detaches `backing`
    // on success, after which old exposed channel views are invalid by spec.
    op_audio_start_buffer(
      this.context._nativeId,
      this._nodeId,
      buffer ? buffer._id : 0,
      acquisition.backing,
      when,
      offset,
      nativeDuration,
    );
    acquisition.commit();
    this.#started = true;
  }

  stop(when = 0) {
    if (!this.#started) {
      throw new DOMException("AudioBufferSourceNode has not been started", "InvalidStateError");
    }
    when = validateScheduledTime(when, "when");
    op_audio_stop(this._nodeId, when);
  }
}

export { AudioBufferSourceNode };
