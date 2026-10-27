
import {
  op_recorder_start,
  op_recorder_pause,
  op_recorder_resume,
  op_recorder_stop,
} from "ext:core/ops";
import { allocateHostCallbackId, createListenerGroup, errorMessage, withScope } from "ext:host_v8_base/02_async.js";

// The microphone, as the reference's RecorderManager drives it: start, pause,
// resume and stop are commands, and what happens is heard through events.
//
// Each recording has an id from the Host id space, which every request names
// and every event and frame the host reports carries. The recorder content
// holds is the last one it started; an event or a frame of any other -- one a
// replaced runtime started, or one a later start took the place of -- is not
// this content's to hear, and is dropped.

// The recording content holds: from start until it stops. 0 when there is none.
let _recordingId = 0;

const _listeners = {
  start: createListenerGroup("RecorderManager start"),
  pause: createListenerGroup("RecorderManager pause"),
  resume: createListenerGroup("RecorderManager resume"),
  stop: createListenerGroup("RecorderManager stop"),
  frameRecorded: createListenerGroup("RecorderManager frameRecorded"),
  error: createListenerGroup("RecorderManager error"),
  interruptionBegin: createListenerGroup("RecorderManager interruptionBegin"),
  interruptionEnd: createListenerGroup("RecorderManager interruptionEnd"),
};

function _fail(errMsg) {
  _listeners.error.trigger({ errMsg });
}

// A command for the recording content holds, if it holds one: a request for a
// recording that is not there has no recording to act on.
function _command(op) {
  if (_recordingId === 0) return;
  const request = JSON.stringify({ recordingId: _recordingId });
  try {
    op(request);
  } catch (e) {
    _fail(errorMessage(e));
  }
}

/**
 * RecorderManager - Global audio recording manager.
 *
 * Manages microphone recording with configurable format, sample rate,
 * channels, and encoding. Supports pause/resume and frame-level callbacks.
 */
class RecorderManager {
  constructor() {
    // Prevent external construction - use getRecorderManager()
  }

  // ==================== Recording Control ====================

  /**
   * Start recording; a recording already under way stops, unheard, first.
   *
   * @param {Object} [options={}] Recording options
   * @param {number} [options.duration=60000] Max duration in ms (max 600000)
   * @param {number} [options.sampleRate=8000] Sample rate Hz
   * @param {number} [options.numberOfChannels=2] 1 (mono) or 2 (stereo)
   * @param {number} [options.encodeBitRate=48000] Encode bit rate in bps
   * @param {string} [options.format="aac"] Format: "mp3","aac","wav","PCM"
   * @param {number} [options.frameSize] Frame size in KB (enables onFrameRecorded)
   * @param {string} [options.audioSource="auto"] Audio input source
   * @param {string} [options.audioMode="default"] "default", "voiceChat", "voiceRecognition", "recording"
   */
  start(options = {}) {
    let recordingId;
    try {
      recordingId = allocateHostCallbackId();
    } catch (e) {
      _fail(errorMessage(e));
      return;
    }
    const request = {
      recordingId,
      duration: options.duration ?? 60000,
      sampleRate: options.sampleRate ?? 8000,
      numberOfChannels: options.numberOfChannels ?? 2,
      encodeBitRate: options.encodeBitRate ?? 48000,
      format: options.format ?? "aac",
      audioSource: options.audioSource ?? "auto",
      audioMode: options.audioMode ?? "default",
    };
    request.duration = Math.max(0, Math.min(600000, request.duration));
    // Only include frameSize if explicitly set. Clamp to [0, 1024] KB: 0 disables
    // frame mode; a host allocates frameSize*1024 bytes per chunk, so an unbounded
    // value would overflow int / OOM.
    if (options.frameSize != null) {
      const fs = Math.trunc(Number(options.frameSize));
      request.frameSize = Number.isFinite(fs) ? Math.max(0, Math.min(1024, fs)) : 0;
    }
    // scope.record undecided, the call asks for it first (withScope). The new
    // recording is content's once the host has it: a start refused before it
    // got there replaced nothing, and the recording under way stays the one
    // pause and stop reach.
    const json = JSON.stringify(request);
    withScope(() => {
      const previous = _recordingId;
      _recordingId = recordingId;
      try {
        op_recorder_start(json);
      } catch (e) {
        _recordingId = previous;
        throw e;
      }
    }, () => {}, (e) => _fail(errorMessage(e)));
  }

  /** Pause recording. */
  pause() {
    if (_recordingId === 0) return;
    withScope(() => _command(op_recorder_pause), () => {}, (e) => _fail(errorMessage(e)));
  }

  /** Resume recording after pause. */
  resume() {
    if (_recordingId === 0) return;
    withScope(() => _command(op_recorder_resume), () => {}, (e) => _fail(errorMessage(e)));
  }

  /** Stop recording. Results delivered via onStop callback. */
  stop() {
    _command(op_recorder_stop);
  }

  // ==================== Event Listeners (multi-listener) ====================

  /** Called when recording starts. */
  onStart(fn) {
    _listeners.start.on(fn);
  }

  /** Called when recording is paused. */
  onPause(fn) {
    _listeners.pause.on(fn);
  }

  /** Called when recording resumes after pause. */
  onResume(fn) {
    _listeners.resume.on(fn);
  }

  /**
   * Called when recording stops.
   * Callback receives: { tempFilePath: string, duration: number, fileSize: number }
   */
  onStop(fn) {
    _listeners.stop.on(fn);
  }

  /**
   * Called when a frame of audio data is recorded (requires frameSize in start options).
   * Callback receives: { frameBuffer: ArrayBuffer, isLastFrame: boolean }
   */
  onFrameRecorded(fn) {
    _listeners.frameRecorded.on(fn);
  }

  /**
   * Called when a recording error occurs.
   * Callback receives: { errMsg: string }
   */
  onError(fn) {
    _listeners.error.on(fn);
  }

  /** Called when recording is interrupted (e.g., incoming call). */
  onInterruptionBegin(fn) {
    _listeners.interruptionBegin.on(fn);
  }

  /** Called when recording interruption ends. */
  onInterruptionEnd(fn) {
    _listeners.interruptionEnd.on(fn);
  }

  // ==================== Listener Removal ====================

  offStart(fn) {
    _listeners.start.off(fn);
  }

  offPause(fn) {
    _listeners.pause.off(fn);
  }

  offResume(fn) {
    _listeners.resume.off(fn);
  }

  offStop(fn) {
    _listeners.stop.off(fn);
  }

  offFrameRecorded(fn) {
    _listeners.frameRecorded.off(fn);
  }

  offError(fn) {
    _listeners.error.off(fn);
  }

  offInterruptionBegin(fn) {
    _listeners.interruptionBegin.off(fn);
  }

  offInterruptionEnd(fn) {
    _listeners.interruptionEnd.off(fn);
  }
}

// Singleton instance
let _instance = null;

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

// ==================== The host's events ====================

// An event's `{recordingId, ...}`, if it is of the recording content holds.
function _ofThisRecording(json) {
  let event;
  try {
    event = JSON.parse(json);
  } catch (_) {
    return null;
  }
  if (event === null || typeof event !== "object" || event.recordingId !== _recordingId || _recordingId === 0) {
    return null;
  }
  return event;
}

function _internalOnRecorderStartEvent(json) {
  if (_ofThisRecording(json)) _listeners.start.trigger();
}

function _internalOnRecorderPauseEvent(json) {
  if (_ofThisRecording(json)) _listeners.pause.trigger();
}

function _internalOnRecorderResumeEvent(json) {
  if (_ofThisRecording(json)) _listeners.resume.trigger();
}

function _internalOnRecorderStopEvent(json) {
  const event = _ofThisRecording(json);
  if (!event) return;
  _recordingId = 0;
  _listeners.stop.trigger({
    tempFilePath: event.tempFilePath,
    duration: event.duration,
    fileSize: event.fileSize,
  });
}

function _internalOnRecorderErrorEvent(json) {
  const event = _ofThisRecording(json);
  if (event) _fail(String(event.errMsg ?? "recorderManager:fail unknown error"));
}

function _internalOnRecorderInterruptionBeginEvent(json) {
  if (_ofThisRecording(json)) _listeners.interruptionBegin.trigger();
}

function _internalOnRecorderInterruptionEndEvent(json) {
  if (_ofThisRecording(json)) _listeners.interruptionEnd.trigger();
}

/**
 * Internal: one frame of a recording, from native.
 * @param {number} recordingId - The recording it belongs to
 * @param {ArrayBuffer} frameBuffer - Audio frame data
 * @param {boolean} isLastFrame - Whether this is the last frame
 */
function _internalOnRecorderFrameData(recordingId, frameBuffer, isLastFrame) {
  if (recordingId === 0 || recordingId !== _recordingId) return;
  _listeners.frameRecorded.trigger({ frameBuffer, isLastFrame });
}

export {
  RecorderManager,
  getRecorderManager,
  _internalOnRecorderStartEvent,
  _internalOnRecorderPauseEvent,
  _internalOnRecorderResumeEvent,
  _internalOnRecorderStopEvent,
  _internalOnRecorderErrorEvent,
  _internalOnRecorderInterruptionBeginEvent,
  _internalOnRecorderInterruptionEndEvent,
  _internalOnRecorderFrameData,
};
