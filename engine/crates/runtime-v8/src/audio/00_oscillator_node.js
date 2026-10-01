import {
  op_audio_set_oscillator_type,
  op_audio_start_oscillator,
  op_audio_stop_oscillator,
} from "ext:core/ops";
import { domException } from "ext:host_v8_base/06_dom_exception.js";
import { watchSourceEnded } from "ext:host_v8_audio/00_source_ended.js";
import { AudioParam } from "ext:host_v8_audio/00_audio_param.js";
import {
  AudioNode,
  validateScheduledTime,
} from "ext:host_v8_audio/00_audio_node.js";

class OscillatorNode extends AudioNode {
  #type = "sine";
  #frequency;
  #detune;
  #started = false;
  #onended = null;

  constructor(context, nodeId) {
    super(context, nodeId, {
      numberOfInputs: 0,
      numberOfOutputs: 1,
    });

    this.#frequency = new AudioParam(440.0, -22050.0, 22050.0);
    this.#frequency._bind(nodeId, "frequency");
    this.#detune = new AudioParam(0, -153600, 153600);
    this.#detune._bind(nodeId, "detune");
  }

  get type() {
    return this.#type;
  }

  set type(value) {
    const valid = ["sine", "square", "sawtooth", "triangle"];
    if (valid.includes(value)) {
      this.#type = value;
      op_audio_set_oscillator_type(this._nodeId, value);
    }
  }

  get frequency() {
    return this.#frequency;
  }

  get detune() {
    return this.#detune;
  }

  get onended() {
    return this.#onended;
  }

  set onended(value) {
    this.#onended = typeof value === "function" ? value : null;
    watchSourceEnded(this, this.#onended);
  }

  start(when = 0) {
    if (this.#started) {
      throw domException("OscillatorNode can only be started once", "InvalidStateError");
    }
    when = validateScheduledTime(when, "when");
    op_audio_start_oscillator(this._nodeId, when);
    this.#started = true;
  }

  stop(when = 0) {
    if (!this.#started) {
      throw domException("OscillatorNode has not been started", "InvalidStateError");
    }
    when = validateScheduledTime(when, "when");
    op_audio_stop_oscillator(this._nodeId, when);
  }
}

export { OscillatorNode };
