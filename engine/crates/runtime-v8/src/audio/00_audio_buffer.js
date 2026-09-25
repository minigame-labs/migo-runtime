// Web Audio AudioBuffer. PCM is owned by a global native buffer id, rather
// than by the AudioContext which first happened to use it.
import {
  op_audio_abort_buffer,
  op_audio_materialize_buffer,
  op_audio_release_buffer,
  op_audio_reserve_buffer,
} from "ext:core/ops";

const MAX_AUDIO_PCM_BYTES = 64 * 1024 * 1024;
const MAX_AUDIO_CHANNELS = 32;
const MIN_SAMPLE_RATE = 3000;
const MAX_SAMPLE_RATE = 768000;
const DECODED_BUFFER_TOKEN = {};

const AUDIO_BUFFER_FINALIZER = new FinalizationRegistry((id) => {
  // This op is synchronous and idempotent. A finalizer must not keep retry
  // state (or a context) alive merely to release an unreachable buffer.
  op_audio_release_buffer(id);
});

function notSupported(message) {
  return new DOMException(message, "NotSupportedError");
}

function checkedPcmBytes(numberOfChannels, length) {
  const bytes = numberOfChannels * length * Float32Array.BYTES_PER_ELEMENT;
  if (!Number.isSafeInteger(bytes) || bytes > MAX_AUDIO_PCM_BYTES) {
    throw notSupported("AudioBuffer PCM data exceeds the implementation limit");
  }
  return bytes;
}

class AudioBuffer {
  #id;
  #duration;
  #sampleRate;
  #numberOfChannels;
  #length;
  #backing;
  #channelData;
  #finalizerToken = {};

  constructor({ numberOfChannels = 1, length, sampleRate }, internal = undefined) {
    if (length === undefined || sampleRate === undefined) {
      throw new TypeError("AudioBuffer requires length and sampleRate");
    }
    numberOfChannels = Number(numberOfChannels);
    length = Number(length);
    sampleRate = Number(sampleRate);
    if (!Number.isInteger(numberOfChannels) || numberOfChannels < 1) {
      throw new RangeError("numberOfChannels must be a positive integer");
    }
    if (numberOfChannels > MAX_AUDIO_CHANNELS) {
      throw notSupported("numberOfChannels exceeds the implementation limit");
    }
    if (!Number.isInteger(length) || length < 1) {
      throw new RangeError("length must be a positive integer");
    }
    if (!Number.isFinite(sampleRate) || sampleRate < MIN_SAMPLE_RATE || sampleRate > MAX_SAMPLE_RATE) {
      throw notSupported("sampleRate is outside the supported range");
    }

    const bytes = checkedPcmBytes(numberOfChannels, length);
    if (internal !== undefined) {
      if (internal.token !== DECODED_BUFFER_TOKEN) {
        throw new TypeError("AudioBuffer internal construction is not public");
      }
      // A decode: its PCM is the native entry's, frozen, and this object has
      // no backing until a channel is read.
      this.#initialize(internal.id, numberOfChannels, length, sampleRate, null);
      return;
    }
    // Admission happens before the one large contiguous JS allocation. If V8
    // rejects that allocation, immediately roll back the native reservation.
    const id = op_audio_reserve_buffer(numberOfChannels, length, sampleRate);
    let backing;
    try {
      backing = new ArrayBuffer(bytes);
    } catch (error) {
      op_audio_abort_buffer(id);
      throw error;
    }

    this.#initialize(id, numberOfChannels, length, sampleRate, backing);
  }


  #initialize(id, numberOfChannels, length, sampleRate, backing) {
    this.#id = id;
    this.#duration = length / sampleRate;
    this.#sampleRate = sampleRate;
    this.#numberOfChannels = numberOfChannels;
    this.#length = length;
    if (backing === null) {
      this.#backing = null;
      this.#channelData = null;
    } else {
      this.#installBacking(backing);
    }
    AUDIO_BUFFER_FINALIZER.register(this, id, this.#finalizerToken);
  }

  #installBacking(backing) {
    if (!(backing instanceof ArrayBuffer)) {
      throw new TypeError("native AudioBuffer backing must be an ArrayBuffer");
    }
    const channelBytes = this.#length * Float32Array.BYTES_PER_ELEMENT;
    if (backing.byteLength !== channelBytes * this.#numberOfChannels) {
      throw new RangeError("native AudioBuffer backing has an invalid length");
    }
    this.#backing = backing;
    this.#channelData = [];
    for (let channel = 0; channel < this.#numberOfChannels; channel++) {
      this.#channelData.push(new Float32Array(this.#backing, channel * channelBytes, this.#length));
    }
  }

  #ensureWritableBacking() {
    if (this.#backing === null) {
      this.#installBacking(op_audio_materialize_buffer(this.#id));
    }
  }

  // The native start/set op detaches writable backing only when it succeeds.
  // Keep our views until its success is committed, so an op failure leaves the
  // JS buffer entirely usable.
  _acquireBacking() {
    if (this.#backing === null) {
      return { backing: null, commit() {} };
    }
    const backing = this.#backing;
    return {
      backing,
      commit: () => {
        if (this.#backing === backing) {
          this.#backing = null;
          this.#channelData = null;
        }
      },
    };
  }

  get _id() { return this.#id; }
  get duration() { return this.#duration; }
  get sampleRate() { return this.#sampleRate; }
  get numberOfChannels() { return this.#numberOfChannels; }
  get length() { return this.#length; }

  getChannelData(channel) {
    if (!Number.isInteger(channel) || channel < 0 || channel >= this.#numberOfChannels) {
      throw new RangeError(`channel index ${channel} out of range`);
    }
    this.#ensureWritableBacking();
    return this.#channelData[channel];
  }

  copyFromChannel(destination, channelNumber, startInChannel = 0) {
    if (!Number.isInteger(channelNumber) || channelNumber < 0 || channelNumber >= this.#numberOfChannels) {
      throw new RangeError(`channel index ${channelNumber} out of range`);
    }
    this.#ensureWritableBacking();
    const data = this.#channelData[channelNumber];
    const start = Math.max(0, Math.trunc(startInChannel));
    const len = Math.min(destination.length, Math.max(0, data.length - start));
    if (len > 0) destination.set(data.subarray(start, start + len));
  }

  copyToChannel(source, channelNumber, startInChannel = 0) {
    if (!Number.isInteger(channelNumber) || channelNumber < 0 || channelNumber >= this.#numberOfChannels) {
      throw new RangeError(`channel index ${channelNumber} out of range`);
    }
    this.#ensureWritableBacking();
    const data = this.#channelData[channelNumber];
    const start = Math.max(0, Math.trunc(startInChannel));
    const len = Math.min(source.length, Math.max(0, data.length - start));
    if (len > 0) data.set(source.subarray(0, len), start);
  }
}

// Only sibling host modules import this factory. The unforgeable module token
// prevents public `new AudioBuffer(options, payload)` from bypassing reserve.
// `id` is the native entry the decode was adopted as; its PCM stays there.
function createDecodedAudioBuffer(id, info) {
  return new AudioBuffer(
    { numberOfChannels: info.channels, length: info.length, sampleRate: info.sample_rate },
    { token: DECODED_BUFFER_TOKEN, id },
  );
}

function releaseNativeAudioBuffer(id) {
  op_audio_release_buffer(id);
}

export { AudioBuffer, createDecodedAudioBuffer, releaseNativeAudioBuffer };
