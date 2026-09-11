import {
  op_audio_start_constant_source,
  op_audio_stop_constant_source,
} from "ext:core/ops";
import { AudioParam } from "ext:host_v8_audio/00_audio_param.js";
import {
  AudioNode,
  validateScheduledTime,
  warnUnsupportedCapability,
} from "ext:host_v8_audio/00_audio_node.js";

class ConstantSourceNode extends AudioNode {
  #offset;
  #started = false;
  #onended = null;

  constructor(context, nodeId) {
    super(context, nodeId, {
      numberOfInputs: 0,
      numberOfOutputs: 1,
    });

    this.#offset = new AudioParam(1.0, -3.4028235e38, 3.4028235e38);
    this.#offset._bind(nodeId, "offset");
  }

  get offset() {
    return this.#offset;
  }

  get onended() {
    return this.#onended;
  }

  set onended(value) {
    this.#onended = typeof value === "function" ? value : null;
    if (typeof value === "function") {
      warnUnsupportedCapability(
        "onended",
        "ConstantSourceNode onended callbacks are not dispatched by the native graph",
      );
    }
  }

  start(when = 0) {
    if (this.#started) {
      throw new DOMException("ConstantSourceNode can only be started once", "InvalidStateError");
    }
    when = validateScheduledTime(when, "when");
    op_audio_start_constant_source(this._nodeId, when);
    this.#started = true;
  }

  stop(when = 0) {
    if (!this.#started) {
      throw new DOMException("ConstantSourceNode has not been started", "InvalidStateError");
    }
    when = validateScheduledTime(when, "when");
    op_audio_stop_constant_source(this._nodeId, when);
  }
}

export { ConstantSourceNode };
