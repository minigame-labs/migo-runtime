import {
  op_audio_create_context,
  op_audio_close_context,
  op_audio_release_context,
  op_audio_decode_audio_data,
  op_audio_reserve_buffer,
  op_audio_abort_buffer,
  op_audio_create_buffer_source,
  op_audio_create_gain,
  op_audio_take_decoded_buffer_data,
  op_audio_create_oscillator,
  op_audio_create_delay,
  op_audio_create_biquad_filter,
  op_audio_create_wave_shaper,
  op_audio_create_analyser,
  op_audio_create_dynamics_compressor,
  op_audio_create_panner,
  op_audio_create_channel_merger,
  op_audio_create_channel_splitter,
  op_audio_create_constant_source,
  op_audio_create_iir_filter,
  op_audio_resume_context,
  op_audio_suspend_context,
} from "ext:core/ops";
import { AudioParam } from "ext:host_v8_audio/00_audio_param.js";
import { AudioBuffer, createDecodedAudioBuffer } from "ext:host_v8_audio/00_audio_buffer.js";
import {
  AudioNode,
  AudioDestinationNode,
  createReleaseQueue,
} from "ext:host_v8_audio/00_audio_node.js";
import { AudioBufferSourceNode } from "ext:host_v8_audio/00_buffer_source_node.js";
import { GainNode } from "ext:host_v8_audio/00_gain_node.js";
import { OscillatorNode } from "ext:host_v8_audio/00_oscillator_node.js";
import { DelayNode } from "ext:host_v8_audio/00_delay_node.js";
import { BiquadFilterNode } from "ext:host_v8_audio/00_biquad_filter_node.js";
import { WaveShaperNode } from "ext:host_v8_audio/00_wave_shaper_node.js";
import { AnalyserNode } from "ext:host_v8_audio/00_analyser_node.js";
import { DynamicsCompressorNode } from "ext:host_v8_audio/00_dynamics_compressor_node.js";
import { PannerNode } from "ext:host_v8_audio/00_panner_node.js";
import { ChannelMergerNode } from "ext:host_v8_audio/00_channel_merger_node.js";
import { ChannelSplitterNode } from "ext:host_v8_audio/00_channel_splitter_node.js";
import { ConstantSourceNode } from "ext:host_v8_audio/00_constant_source_node.js";
import { IIRFilterNode } from "ext:host_v8_audio/00_iir_filter_node.js";
import { ScriptProcessorNode } from "ext:host_v8_audio/00_script_processor_node.js";
import { PeriodicWave } from "ext:host_v8_audio/00_periodic_wave.js";
import { AudioListener } from "ext:host_v8_audio/00_audio_listener.js";
import { onHide, onShow } from "ext:host_v8_lifecycle/01_lifecycle.js";
import { allocateHostCallbackId } from "ext:host_v8_base/02_async.js";

const CONTEXT_REGISTRY = new Map();

// Retry and de-duplication live in `createReleaseQueue`, shared with the node
// finalizer, so the bounded-queue backoff rule has one definition instead of one
// per resource kind.
const releaseNativeAudioContext = createReleaseQueue(op_audio_release_context);

const CONTEXT_FINALIZER = new FinalizationRegistry((ctxId) => {
  CONTEXT_REGISTRY.delete(ctxId);
  releaseNativeAudioContext(ctxId, ctxId);
});

function forEachLiveContext(callback) {
  for (const [ctxId, reference] of CONTEXT_REGISTRY) {
    const context = reference.deref();
    if (context) {
      callback(context);
    } else {
      CONTEXT_REGISTRY.delete(ctxId);
    }
  }
}

// While the app is hidden the audio thread is paused (OnHide -> PauseAll), so
// native currentTime freezes. Freeze every context's JS clock to match, and
// resume it on foreground, so scheduled start(when)/stop(when) stay aligned
// with the native timeline across a background/foreground cycle. The
// module-level flag also lets contexts CREATED while backgrounded start frozen.
//
// KNOWN LIMITATION: onHide/onShow are delivered only to the main JS runtime.
// A Worker's AudioContext (non-standard - browsers keep WebAudio on the main
// thread) therefore won't freeze on background, so its currentTime can drift
// from the globally-paused native clock. Accepted as low-impact given how rare
// worker WebAudio is; revisit with host->worker lifecycle forwarding or a
// native-authoritative clock if it becomes a real use case.
//
// Audio interruptions (phone calls / focus loss) are intentionally NOT tied to
// this freeze: the native audio thread keeps processing during an interruption
// (the OS ducks/pauses the actual output), so currentTime stays consistent with
// the native clock. Games pause/resume playback themselves via
// onAudioInterruptionBegin/End if they want it to stop.
let _appBackgrounded = false;
onHide(() => {
  _appBackgrounded = true;
  forEachLiveContext((ctx) => ctx._setBackgrounded(true));
});
onShow(() => {
  _appBackgrounded = false;
  forEachLiveContext((ctx) => ctx._setBackgrounded(false));
});

class BaseAudioContext {
  #nativeId = null;
  #sampleRate;
  #destination = null;
  #state = "suspended";
  // Clock bookkeeping so `currentTime` mirrors the native frame clock
  // (frames_processed / sampleRate): it starts at 0 when the context is
  // created and freezes whenever native processing stops - both on explicit
  // suspend() and while the app is backgrounded (OnHide pauses the audio
  // thread, freezing native frames_processed, without changing #state).
  #clockEpoch = 0; // performance.now() (ms) at the start of the running segment
  #accumulated = 0; // running seconds banked before the current segment
  #backgrounded = false; // app hidden -> audio thread paused
  #clockRunning = false; // whether the epoch is currently counting
  #finalizerToken = {};

  constructor(sampleRate) {
    this.#sampleRate = sampleRate;
  }

  // Recompute whether the clock should advance ((running state) AND (foreground))
  // and bank/restart the epoch on any change, so `currentTime` stays aligned
  // with native current_time across suspend/resume and background/foreground.
  #reconcileClock() {
    const shouldRun = this.#state === "running" && !this.#backgrounded;
    if (shouldRun === this.#clockRunning) return;
    const now = performance.now();
    if (this.#clockRunning) {
      this.#accumulated += (now - this.#clockEpoch) / 1000; // freezing: bank elapsed
    } else {
      this.#clockEpoch = now; // resuming: restart the epoch
    }
    this.#clockRunning = shouldRun;
  }

  /** Internal: freeze/resume the clock when the app is hidden/shown. */
  _setBackgrounded(backgrounded) {
    this.#backgrounded = !!backgrounded;
    this.#reconcileClock();
  }

  // Synchronous: allocate the id in JS and fire the create command. It rides
  // the same FIFO channel as every subsequent node op, so the audio thread
  // always creates the context before any node that references it. This keeps
  // `new AudioContext()` synchronously usable, matching the browser.
  _initNative(sampleRate) {
    this.#nativeId = allocateHostCallbackId();
    op_audio_create_context(this.#nativeId, sampleRate || 0);
    this.#destination = new AudioDestinationNode(this, 0, 2);
    // Anchor the clock origin at creation, matching native frames_processed=0.
    // If the app is already backgrounded, start frozen (native is paused).
    this.#accumulated = 0;
    this.#backgrounded = _appBackgrounded;
    this.#state = "running";
    this.#reconcileClock(); // start counting only if running + foreground
    CONTEXT_REGISTRY.set(this.#nativeId, new WeakRef(this));
    CONTEXT_FINALIZER.register(this, this.#nativeId, this.#finalizerToken);
  }

  _forgetNativeRegistration() {
    CONTEXT_FINALIZER.unregister(this.#finalizerToken);
    CONTEXT_REGISTRY.delete(this.#nativeId);
  }

  get _nativeId() {
    return this.#nativeId;
  }

  get sampleRate() {
    return this.#sampleRate;
  }

  get destination() {
    return this.#destination;
  }

  get state() {
    return this.#state;
  }

  get currentTime() {
    // Sample-frame clock (W3C): starts at 0 at creation, advances in real time
    // while running in the foreground, freezes while suspended/closed/hidden.
    if (this.#clockRunning) {
      return this.#accumulated + (performance.now() - this.#clockEpoch) / 1000;
    }
    return this.#accumulated;
  }

  async decodeAudioData(audioData, successCallback, errorCallback) {
    if (!(audioData instanceof ArrayBuffer)) {
      throw new TypeError("audioData must be an ArrayBuffer");
    }
    // Constructing even a zero-length view rejects an already-detached input.
    // Translate that engine-specific TypeError to the Web Audio exception.
    try {
      new Uint8Array(audioData, 0, 0);
    } catch (_) {
      throw new DOMException("audioData is already detached", "DataCloneError");
    }

    const decodePromise = (async () => {
      const info = await op_audio_decode_audio_data(
        this.#nativeId,
        audioData
      );

      let buffer;
      let globalBufferId = 0;
      try {
        globalBufferId = op_audio_reserve_buffer(
          info.channels,
          info.length,
          info.sample_rate,
        );
        // Atomically move the temporary native decode into one exact,
        // channel-major planar ArrayBuffer. The audio thread drops its
        // interleaved allocation before this promise resumes.
        const flat = await op_audio_take_decoded_buffer_data(this.#nativeId, info.id);
        buffer = createDecodedAudioBuffer(globalBufferId, info, flat);
      } catch (error) {
        if (globalBufferId !== 0) op_audio_abort_buffer(globalBufferId);
        throw error;
      }

      return buffer;
    })();

    // Legacy callbacks observe the same settlement but are deliberately
    // scheduled separately: their return value or exception cannot resolve or
    // reject the Promise returned by decodeAudioData.
    if (typeof successCallback === "function") {
      void decodePromise.then(
        (buffer) => setTimeout(() => successCallback(buffer), 0),
        () => {},
      );
    }
    if (typeof errorCallback === "function") {
      void decodePromise.then(
        () => {},
        (error) => setTimeout(() => errorCallback(error), 0),
      );
    }
    return decodePromise;
  }

  createBuffer(numberOfChannels, length, sampleRate) {
    return new AudioBuffer({ numberOfChannels, length, sampleRate });
  }

  createBufferSource() {
    // Generate ID in JS, notify native asynchronously
    const nodeId = allocateHostCallbackId();
    // Fire and forget - native will create the node
    op_audio_create_buffer_source(this.#nativeId, nodeId);
    return new AudioBufferSourceNode(this, nodeId);
  }

  createGain() {
    const nodeId = allocateHostCallbackId();
    op_audio_create_gain(this.#nativeId, nodeId);
    return new GainNode(this, nodeId);
  }

  createOscillator() {
    const nodeId = allocateHostCallbackId();
    op_audio_create_oscillator(this.#nativeId, nodeId);
    return new OscillatorNode(this, nodeId);
  }

  createDelay(maxDelayTime = 1.0) {
    // Match the native 16MB per-node delay-buffer budget so delayTime.maxValue
    // reflects what native will actually honor (native clamps too; assume stereo).
    const MAX_DELAY_BYTES = 16 * 1024 * 1024;
    const budgetSecs = MAX_DELAY_BYTES / (this.sampleRate * 2 * 4);
    maxDelayTime = Math.min(Math.max(0.001, maxDelayTime), Math.min(180, budgetSecs));
    const nodeId = allocateHostCallbackId();
    op_audio_create_delay(this.#nativeId, nodeId, maxDelayTime);
    return new DelayNode(this, nodeId, maxDelayTime);
  }

  createBiquadFilter() {
    const nodeId = allocateHostCallbackId();
    op_audio_create_biquad_filter(this.#nativeId, nodeId);
    return new BiquadFilterNode(this, nodeId);
  }

  createWaveShaper() {
    const nodeId = allocateHostCallbackId();
    op_audio_create_wave_shaper(this.#nativeId, nodeId);
    return new WaveShaperNode(this, nodeId);
  }

  createAnalyser() {
    const nodeId = allocateHostCallbackId();
    op_audio_create_analyser(this.#nativeId, nodeId);
    return new AnalyserNode(this, nodeId);
  }

  createDynamicsCompressor() {
    const nodeId = allocateHostCallbackId();
    op_audio_create_dynamics_compressor(this.#nativeId, nodeId);
    return new DynamicsCompressorNode(this, nodeId);
  }

  createPanner() {
    const nodeId = allocateHostCallbackId();
    op_audio_create_panner(this.#nativeId, nodeId);
    return new PannerNode(this, nodeId);
  }

  createChannelMerger(numberOfInputs = 6) {
    const nodeId = allocateHostCallbackId();
    op_audio_create_channel_merger(this.#nativeId, nodeId, numberOfInputs);
    return new ChannelMergerNode(this, nodeId, numberOfInputs);
  }

  createChannelSplitter(numberOfOutputs = 6) {
    const nodeId = allocateHostCallbackId();
    op_audio_create_channel_splitter(this.#nativeId, nodeId, numberOfOutputs);
    return new ChannelSplitterNode(this, nodeId, numberOfOutputs);
  }

  createConstantSource() {
    const nodeId = allocateHostCallbackId();
    op_audio_create_constant_source(this.#nativeId, nodeId);
    return new ConstantSourceNode(this, nodeId);
  }

  createIIRFilter(feedforward, feedback) {
    // Accept any sequence<double> (incl. Float32Array/Float64Array), per Web Audio.
    const ff = Array.from(feedforward ?? []);
    const fb = Array.from(feedback ?? []);
    if (ff.length === 0 || fb.length === 0) {
      throw new Error("createIIRFilter: feedforward and feedback must be non-empty");
    }
    if (ff.length > 20 || fb.length > 20) {
      throw new Error("createIIRFilter: coefficient arrays must have at most 20 elements");
    }
    if (fb[0] === 0) {
      throw new Error("createIIRFilter: feedback[0] must not be zero");
    }
    if (!ff.every(Number.isFinite) || !fb.every(Number.isFinite)) {
      throw new Error("createIIRFilter: coefficients must be finite numbers");
    }
    const nodeId = allocateHostCallbackId();
    op_audio_create_iir_filter(this.#nativeId, nodeId, ff, fb);
    return new IIRFilterNode(this, nodeId, ff, fb);
  }

  createScriptProcessor(bufferSize = 0, numberOfInputChannels = 2, numberOfOutputChannels = 2) {
    const nodeId = allocateHostCallbackId();
    return new ScriptProcessorNode(this, nodeId, bufferSize, numberOfInputChannels, numberOfOutputChannels);
  }

  createPeriodicWave(options = {}) {
    const real = options.real || undefined;
    const imag = options.imag || undefined;
    const disableNormalization = options.disableNormalization || false;
    return new PeriodicWave({ real, imag, disableNormalization });
  }

  get listener() {
    if (!this._listener) {
      this._listener = new AudioListener(this);
    }
    return this._listener;
  }

  _setState(state) {
    // Keep the sample-frame clock consistent across suspend/resume by
    // recomputing the advancing predicate (see #reconcileClock).
    this.#state = state;
    this.#reconcileClock();
  }

  _setSampleRate(rate) {
    this.#sampleRate = rate;
  }
}

class AudioContext extends BaseAudioContext {
  #baseLatency = 0.005;
  #outputLatency = 0.01;
  #ready;
  // In-flight close(), so repeated calls share one command.
  #closing = null;

  constructor(options = {}) {
    const sampleRate = options.sampleRate || 44100;
    super(sampleRate);

    // Initialize synchronously so the context is immediately usable. Expose a
    // resolved promise for callers that `await context.ready`.
    this._initNative(sampleRate);
    this.#ready = Promise.resolve(this);
  }

  get baseLatency() {
    return this.#baseLatency;
  }

  get outputLatency() {
    return this.#outputLatency;
  }

  get ready() {
    return this.#ready;
  }

  async close() {
    if (this.state === "closed") {
      return;
    }
    // One in-flight close per context. Two `close()` calls before the first
    // settles both used to pass the state guard above, so the second sent a
    // second CloseContext for a context the audio thread had already dropped.
    if (this.#closing !== null) {
      return this.#closing;
    }
    this.#closing = (async () => {
      try {
        await op_audio_close_context(this._nativeId);
      } catch (error) {
        // The native context may still exist -- a full command queue is
        // transient -- so leave the state and the finalizer registration
        // alone; the registration is what still guarantees its release.
        this.#closing = null;
        throw error;
      }
      this._setState("closed");
      // Nothing to cancel in the release queue: only the finalizer enqueues a
      // release, it cannot have run while this method held `this`, and the
      // unregister below stops it from ever running for this id.
      this._forgetNativeRegistration();
    })();
    return this.#closing;
  }

  async resume() {
    if (this.state === "closed") {
      throw new Error("Cannot resume a closed AudioContext");
    }
    await op_audio_resume_context(this._nativeId);
    this._setState("running");
  }

  async suspend() {
    if (this.state === "closed") {
      throw new Error("Cannot suspend a closed AudioContext");
    }
    await op_audio_suspend_context(this._nativeId);
    this._setState("suspended");
  }
}

function createWebAudioContext(options) {
  return new AudioContext(options);
}

export {
  createWebAudioContext,
  AudioContext,
  BaseAudioContext,
  AudioBuffer,
  AudioNode,
  AudioDestinationNode,
  AudioBufferSourceNode,
  GainNode,
  OscillatorNode,
  DelayNode,
  BiquadFilterNode,
  WaveShaperNode,
  AnalyserNode,
  DynamicsCompressorNode,
  PannerNode,
  ChannelMergerNode,
  ChannelSplitterNode,
  ConstantSourceNode,
  IIRFilterNode,
  ScriptProcessorNode,
  PeriodicWave,
  AudioListener,
  AudioParam,
};
