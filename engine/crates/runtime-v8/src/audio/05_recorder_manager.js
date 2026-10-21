
import {
  op_recorder_start,
  op_recorder_pause,
  op_recorder_resume,
  op_recorder_stop,
} from "ext:core/ops";
import { createListenerGroup, errorMessage, withScope } from "ext:host_v8_base/02_async.js";

// Singleton instance
let _instance = null;

/**
 * RecorderManager - Global audio recording manager.
 *
 * Manages microphone recording with configurable format, sample rate,
 * channels, and encoding. Supports pause/resume and frame-level callbacks.
 */
class RecorderManager {
  // Multi-listener arrays for all event types
  #listeners = {
    start: createListenerGroup("RecorderManager start"),
    pause: createListenerGroup("RecorderManager pause"),
    resume: createListenerGroup("RecorderManager resume"),
    stop: createListenerGroup("RecorderManager stop"),
    frameRecorded: createListenerGroup("RecorderManager frameRecorded"),
    error: createListenerGroup("RecorderManager error"),
    interruptionBegin: createListenerGroup("RecorderManager interruptionBegin"),
    interruptionEnd: createListenerGroup("RecorderManager interruptionEnd"),
  };

  constructor() {
    // Prevent external construction - use getRecorderManager()
  }

  // ==================== Recording Control ====================

  /**
   * Start recording.
   *
   * @param {Object} [options={}] Recording options
   * @param {number} [options.duration=60000] Max duration in ms (max 600000)
   * @param {number} [options.sampleRate=8000] Sample rate Hz
   * @param {number} [options.numberOfChannels=2] 1 (mono) or 2 (stereo)
   * @param {number} [options.encodeBitRate=48000] Encode bit rate in bps
   * @param {string} [options.format="aac"] Format: "mp3","aac","wav","PCM"
   * @param {number} [options.frameSize] Frame size in KB (enables onFrameRecorded)
   * @param {string} [options.audioSource="auto"] Audio input source
   */
  start(options = {}) {
    const opts = {
      duration: options.duration ?? 60000,
      sampleRate: options.sampleRate ?? 8000,
      numberOfChannels: options.numberOfChannels ?? 2,
      encodeBitRate: options.encodeBitRate ?? 48000,
      format: options.format ?? "aac",
      audioSource: options.audioSource ?? "auto",
    };
    // Clamp duration
    opts.duration = Math.max(0, Math.min(600000, opts.duration));
    // Only include frameSize if explicitly set. Clamp to [0, 1024] KB: 0 disables
    // frame mode; native allocates frameSize*1024 bytes per chunk, so an unbounded
    // value would overflow int / OOM.
    if (options.frameSize != null) {
      const fs = Math.trunc(Number(options.frameSize));
      opts.frameSize = Number.isFinite(fs) ? Math.max(0, Math.min(1024, fs)) : 0;
    }

    // scope.record undecided, the call asks for it first (withScope).
    const request = JSON.stringify(opts);
    withScope(() => op_recorder_start(request), () => {}, (e) => {
      this.#fireListeners("error", { errMsg: errorMessage(e) });
    });
  }

  /** Pause recording. */
  pause() {
    withScope(() => op_recorder_pause(), () => {}, (e) => {
      this.#fireListeners("error", { errMsg: errorMessage(e) });
    });
  }

  /** Resume recording after pause. */
  resume() {
    withScope(() => op_recorder_resume(), () => {}, (e) => {
      this.#fireListeners("error", { errMsg: errorMessage(e) });
    });
  }

  /** Stop recording. Results delivered via onStop callback. */
  stop() {
    try {
      op_recorder_stop();
    } catch (e) {
      this.#fireListeners("error", { errMsg: errorMessage(e) });
    }
  }

  // ==================== Event Listeners (multi-listener) ====================

  /** Called when recording starts. */
  onStart(fn) {
    this.#listeners.start.on(fn);
  }

  /** Called when recording is paused. */
  onPause(fn) {
    this.#listeners.pause.on(fn);
  }

  /** Called when recording resumes after pause. */
  onResume(fn) {
    this.#listeners.resume.on(fn);
  }

  /**
   * Called when recording stops.
   * Callback receives: { tempFilePath: string, duration: number, fileSize: number }
   */
  onStop(fn) {
    this.#listeners.stop.on(fn);
  }

  /**
   * Called when a frame of audio data is recorded (requires frameSize in start options).
   * Callback receives: { frameBuffer: ArrayBuffer, isLastFrame: boolean }
   */
  onFrameRecorded(fn) {
    this.#listeners.frameRecorded.on(fn);
  }

  /**
   * Called when a recording error occurs.
   * Callback receives: { errMsg: string }
   */
  onError(fn) {
    this.#listeners.error.on(fn);
  }

  /** Called when recording is interrupted (e.g., incoming call). */
  onInterruptionBegin(fn) {
    this.#listeners.interruptionBegin.on(fn);
  }

  /** Called when recording interruption ends. */
  onInterruptionEnd(fn) {
    this.#listeners.interruptionEnd.on(fn);
  }

  // ==================== Listener Removal ====================

  offStart(fn) {
    this.#removeListener("start", fn);
  }

  offPause(fn) {
    this.#removeListener("pause", fn);
  }

  offResume(fn) {
    this.#removeListener("resume", fn);
  }

  offStop(fn) {
    this.#removeListener("stop", fn);
  }

  offFrameRecorded(fn) {
    this.#removeListener("frameRecorded", fn);
  }

  offError(fn) {
    this.#removeListener("error", fn);
  }

  offInterruptionBegin(fn) {
    this.#removeListener("interruptionBegin", fn);
  }

  offInterruptionEnd(fn) {
    this.#removeListener("interruptionEnd", fn);
  }

  // ==================== Internal ====================

  /** Remove a specific listener, or all listeners for the type if fn is omitted. */
  #removeListener(type, fn) {
    const group = this.#listeners[type];
    if (!group) return;
    group.off(fn);
  }

  /** Fire all listeners for the given event type. */
  #fireListeners(type, arg) {
    const group = this.#listeners[type];
    if (!group) return;
    group.trigger(arg);
  }

  /**
   * Internal: handle event from native layer.
   * Called by _internalOnRecorderEvent(eventType, jsonPayload).
   */
  _handleEvent(eventType, jsonPayload) {
    switch (eventType) {
      case "start":
        this.#fireListeners("start");
        break;
      case "pause":
        this.#fireListeners("pause");
        break;
      case "resume":
        this.#fireListeners("resume");
        break;
      case "stop": {
        let result = {};
        try {
          result = JSON.parse(jsonPayload);
        } catch (_) {}
        this.#fireListeners("stop", result);
        break;
      }
      case "error": {
        let errObj = { errMsg: "recorder.error:fail unknown error" };
        try {
          errObj = JSON.parse(jsonPayload);
        } catch (_) {}
        this.#fireListeners("error", errObj);
        break;
      }
      case "interruptionBegin":
        this.#fireListeners("interruptionBegin");
        break;
      case "interruptionEnd":
        this.#fireListeners("interruptionEnd");
        break;
    }
  }

  /**
   * Internal: handle frame data from native layer.
   * Called by _internalOnRecorderFrameData(arrayBuffer, isLastFrame).
   */
  _handleFrameData(frameBuffer, isLastFrame) {
    this.#fireListeners("frameRecorded", { frameBuffer, isLastFrame });
  }
}

/**
 * Get the global RecorderManager instance (singleton).
 * @returns {RecorderManager}
 */
function getRecorderManager() {
  if (!_instance) {
    _instance = new RecorderManager();
  }
  return _instance;
}

/**
 * Internal: called from native to dispatch recorder events.
 * @param {string} eventType - Event type
 * @param {string} jsonPayload - JSON payload
 */
function _internalOnRecorderEvent(eventType, jsonPayload) {
  if (_instance) {
    _instance._handleEvent(eventType, jsonPayload);
  }
}

/**
 * Internal: called from native to dispatch recorder frame data.
 * @param {ArrayBuffer} frameBuffer - Audio frame data
 * @param {boolean} isLastFrame - Whether this is the last frame
 */
function _internalOnRecorderFrameData(frameBuffer, isLastFrame) {
  if (_instance) {
    _instance._handleFrameData(frameBuffer, isLastFrame);
  }
}

export {
  RecorderManager,
  getRecorderManager,
  _internalOnRecorderEvent,
  _internalOnRecorderFrameData,
};
