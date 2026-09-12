import {
  op_audio_set_analyser_fft_size,
  op_audio_analyser_byte_time_domain,
  op_audio_analyser_float_time_domain,
  op_audio_analyser_byte_frequency,
  op_audio_analyser_float_frequency,
  op_audio_set_analyser_scalar,
} from "ext:core/ops";
import {
  AudioNode,
  warnUnsupportedCapability,
} from "ext:host_v8_audio/00_audio_node.js";

class AnalyserNode extends AudioNode {
  #fftSize = 2048;
  #minDecibels = -100;
  #maxDecibels = -30;
  #smoothingTimeConstant = 0.8;

  constructor(context, nodeId) {
    super(context, nodeId, {
      numberOfInputs: 1,
      numberOfOutputs: 1,
    });
  }

  get fftSize() {
    return this.#fftSize;
  }

  set fftSize(value) {
    // Must be power of 2 between 32 and 32768
    const v = Number(value);
    if (v >= 32 && v <= 32768 && (v & (v - 1)) === 0) {
      this.#fftSize = v;
      op_audio_set_analyser_fft_size(this._nodeId, v);
    }
  }

  get frequencyBinCount() {
    return this.#fftSize / 2;
  }

  get minDecibels() {
    return this.#minDecibels;
  }

  set minDecibels(value) {
    this.#minDecibels = Number(value);
    op_audio_set_analyser_scalar(this._nodeId, "minDecibels", this.#minDecibels);
  }

  get maxDecibels() {
    return this.#maxDecibels;
  }

  set maxDecibels(value) {
    this.#maxDecibels = Number(value);
    op_audio_set_analyser_scalar(this._nodeId, "maxDecibels", this.#maxDecibels);
  }

  get smoothingTimeConstant() {
    return this.#smoothingTimeConstant;
  }

  set smoothingTimeConstant(value) {
    this.#smoothingTimeConstant = Math.max(0, Math.min(1, Number(value)));
    op_audio_set_analyser_scalar(
      this._nodeId,
      "smoothingTimeConstant",
      this.#smoothingTimeConstant,
    );
  }

  async getByteTimeDomainData(array) {
    warnUnsupportedCapability(
      "analyser-read",
      "AnalyserNode reads use asynchronous native snapshots",
    );
    const data = await op_audio_analyser_byte_time_domain(this._nodeId);
    const len = Math.min(array.length, data.length);
    array.set(data.subarray(0, len));
  }

  async getFloatTimeDomainData(array) {
    warnUnsupportedCapability(
      "analyser-read",
      "AnalyserNode reads use asynchronous native snapshots",
    );
    const bytes = await op_audio_analyser_float_time_domain(this._nodeId);
    const floats = new Float32Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / 4);
    const len = Math.min(array.length, floats.length);
    array.set(floats.subarray(0, len));
  }
  async getByteFrequencyData(array) {
    warnUnsupportedCapability(
      "analyser-read",
      "AnalyserNode reads use asynchronous native snapshots",
    );
    const data = await op_audio_analyser_byte_frequency(this._nodeId);
    const len = Math.min(array.length, data.length);
    for (let i = 0; i < len; i++) {
      array[i] = data[i];
    }
  }

  async getFloatFrequencyData(array) {
    warnUnsupportedCapability(
      "analyser-read",
      "AnalyserNode reads use asynchronous native snapshots",
    );
    const data = await op_audio_analyser_float_frequency(this._nodeId);
    const len = Math.min(array.length, data.length);
    for (let i = 0; i < len; i++) {
      array[i] = data[i];
    }
  }
}

export { AnalyserNode };
