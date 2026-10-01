
import {
  op_inner_audio_create,
  op_inner_audio_destroy,
  op_inner_audio_load_url,
  op_inner_audio_play,
  op_inner_audio_pause,
  op_inner_audio_stop,
  op_inner_audio_seek,
  op_inner_audio_set_volume,
  op_inner_audio_set_loop,
  op_inner_audio_set_playback_rate,
  op_inner_audio_set_autoplay,
  op_inner_audio_get_state,
  op_audio_set_inner_audio_option,
  op_audio_get_available_audio_sources,
} from "ext:core/ops";
import { allocateHostCallbackId, createListenerGroup, invokeCallback, errorMessage, failMessage } from "ext:host_v8_base/02_async.js";

// ID counter for InnerAudioContext instances

// Registry of all active InnerAudioContext instances (for event dispatch)
const audioContextRegistry = new Map();

// Event queue and scheduling (microtask batching like touch events)
let _eventQueue = [];
let _scheduled = false;

/**
 * Called from native (Rust / V8 binding) to enqueue audio events.
 * Events are batched and dispatched in the next microtask.
 *
 * @param {number} id - InnerAudioContext ID
 * @param {string} eventType - Event type (canPlay, play, pause, etc.)
 * @param {number} currentTime - Current playback time
 */
function _internalEnqueueInnerAudioEvent(id, eventType, currentTime) {
  _eventQueue.push({ id, eventType, currentTime });

  if (!_scheduled) {
    _scheduled = true;
    Promise.resolve().then(_drainEvents);
  }
}

/**
 * Drain and dispatch all queued events
 */
function _drainEvents() {
  _scheduled = false;

  // Swap queue to avoid re-entrancy issues
  const batch = _eventQueue;
  _eventQueue = [];

  for (const event of batch) {
    const ctx = audioContextRegistry.get(event.id);
    if (ctx) {
      ctx._handleNativeEvent(event);
    }
  }
}

/**
 * Register a context for event dispatch
 */
function registerContext(ctx) {
  audioContextRegistry.set(ctx._getId(), ctx);
}

/**
 * Unregister a context
 */
function unregisterContext(ctx) {
  audioContextRegistry.delete(ctx._getId());
}

class InnerAudioContext {
  #id;
  #src = "";
  #startTime = 0;
  #autoplay = false;
  #loop = false;
  #obeyMuteSwitch = true;
  #volume = 1.0;
  #playbackRate = 1.0;
  #offlineMode = false;
  // Read-only properties (cached from native)
  #duration = 0;
  #currentTime = 0;
  #paused = true;
  #buffered = false;

  // Event listener arrays (multi-listener support)
  #listeners = {
    canplay: createListenerGroup("InnerAudioContext canplay"),
    play: createListenerGroup("InnerAudioContext play"),
    pause: createListenerGroup("InnerAudioContext pause"),
    stop: createListenerGroup("InnerAudioContext stop"),
    ended: createListenerGroup("InnerAudioContext ended"),
    timeUpdate: createListenerGroup("InnerAudioContext timeUpdate"),
    error: createListenerGroup("InnerAudioContext error"),
    waiting: createListenerGroup("InnerAudioContext waiting"),
    seeking: createListenerGroup("InnerAudioContext seeking"),
    seeked: createListenerGroup("InnerAudioContext seeked"),
  };

  // Loading state
  #destroyed = false;

  constructor() {
    // From the Host's id space rather than a module counter: the audio thread
    // keeps a player per id across a runtime restart, so a counter restarting
    // at 1 would let a new context drive the retired runtime's still-playing
    // sound. Exhaustion throws out of the constructor, which is this API's
    // only failure convention -- it returns a context rather than reporting.
    this.#id = allocateHostCallbackId();
    op_inner_audio_create(this.#id);
    registerContext(this);
  }

  /** Internal: Get ID for event registry */
  _getId() {
    return this.#id;
  }

  /** Internal: Handle native event */
  _handleNativeEvent(event) {
    if (this.#destroyed) return;

    // Update current time from event
    this.#currentTime = event.currentTime;

    switch (event.eventType) {
      case "canPlay":
        this.#buffered = true;
        this.#refreshState(); // populate duration/buffered (events carry only currentTime)
        this.#fireListeners("canplay");
        break;
      case "play":
        this.#paused = false;
        this.#refreshState(); // refresh duration for streaming sources
        this.#fireListeners("play");
        break;
      case "pause":
        this.#paused = true;
        this.#fireListeners("pause");
        break;
      case "stop":
        this.#paused = true;
        this.#currentTime = 0;
        this.#fireListeners("stop");
        break;
      case "ended":
        this.#paused = true;
        this.#currentTime = 0;
        this.#fireListeners("ended");
        break;
      case "seeking":
        this.#fireListeners("seeking");
        break;
      case "seeked":
        this.#fireListeners("seeked");
        break;
      case "timeUpdate":
        this.#fireListeners("timeUpdate");
        break;
      case "waiting":
        this.#fireListeners("waiting");
        break;
      case "error":
        // A playback error stops playback (e.g. native aborted an over-budget
        // stream). Reflect that in JS state so `paused` isn't left false after
        // an earlier canplay/play already flipped it.
        this.#paused = true;
        this.#fireListeners("error", { errCode: 10001, errMsg: "Playback error" });
        break;
    }
  }

  #fireListeners(type, arg) {
    const group = this.#listeners[type];
    if (!group) return;
    group.trigger(arg);
  }

  // Pull duration/buffered (and a fresh currentTime) from native. The push-based
  // event stream only carries currentTime, so duration/buffered would otherwise
  // stay 0. Fire-and-forget; guarded against teardown.
  async #refreshState() {
    if (this.#destroyed) return;
    try {
      const st = await op_inner_audio_get_state(this.#id);
      if (this.#destroyed) return;
      if (typeof st.duration === "number") this.#duration = st.duration;
      if (typeof st.buffered === "boolean") this.#buffered = st.buffered;
      if (typeof st.current_time === "number") this.#currentTime = st.current_time;
    } catch (_) {
      /* context gone or audio thread unavailable */
    }
  }

  // ==================== Properties ====================

  /** Audio source URL or path */
  get src() {
    return this.#src;
  }

  set src(value) {
    if (this.#src === value) return;
    this.#src = value;
    this.#loadAudio();
  }

  /** Start position in seconds */
  get startTime() {
    return this.#startTime;
  }

  set startTime(value) {
    this.#startTime = Math.max(0, value);
  }

  /** Whether to autoplay */
  get autoplay() {
    return this.#autoplay;
  }

  set autoplay(value) {
    this.#autoplay = !!value;
    op_inner_audio_set_autoplay(this.#id, this.#autoplay);
  }

  /** Whether to loop */
  get loop() {
    return this.#loop;
  }

  set loop(value) {
    this.#loop = !!value;
    op_inner_audio_set_loop(this.#id, this.#loop);
  }

  /** Whether to obey mute switch (iOS only, always true on Android) */
  get obeyMuteSwitch() {
    return this.#obeyMuteSwitch;
  }

  set obeyMuteSwitch(value) {
    this.#obeyMuteSwitch = !!value;
  }

  /** Whether to enable offline mode */
  get offlineMode() {
    return this.#offlineMode;
  }

  set offlineMode(value) {
    this.#offlineMode = !!value;
  }

  /** Volume (0.0 - 1.0) */
  get volume() {
    return this.#volume;
  }

  set volume(value) {
    this.#volume = Math.max(0, Math.min(1, value));
    op_inner_audio_set_volume(this.#id, this.#volume);
  }

  /** Playback rate (0.5 - 2.0) */
  get playbackRate() {
    return this.#playbackRate;
  }

  set playbackRate(value) {
    this.#playbackRate = Math.max(0.5, Math.min(2.0, value));
    op_inner_audio_set_playback_rate(this.#id, this.#playbackRate);
  }

  /** Duration in seconds (read-only) */
  get duration() {
    return this.#duration;
  }

  /** Current playback position in seconds */
  get currentTime() {
    return this.#currentTime;
  }

  /** Set current playback position (equivalent to calling seek) */
  set currentTime(value) {
    this.seek(value);
  }

  /** Whether audio is paused (read-only) */
  get paused() {
    return this.#paused;
  }

  /** Buffered time in seconds (read-only). Returns duration when fully loaded. */
  get buffered() {
    return this.#buffered ? this.#duration : 0;
  }

  // ==================== Methods ====================

  /** Start playback. Returns a Promise per mini-game API. */
  play() {
    if (this.#destroyed) return Promise.reject(new Error("AudioContext is destroyed"));

    if (!this.#buffered) {
      // Not loaded yet, set autoplay
      this.#autoplay = true;
      op_inner_audio_set_autoplay(this.#id, true);
      return Promise.resolve();
    }

    // Seek to startTime if specified and at the beginning
    if (this.#startTime > 0 && this.#currentTime === 0) {
      op_inner_audio_seek(this.#id, this.#startTime);
    }

    op_inner_audio_play(this.#id);
    // Note: Don't update #paused here - wait for native event
    return Promise.resolve();
  }

  /** Pause playback */
  pause() {
    if (this.#destroyed) return;
    op_inner_audio_pause(this.#id);
  }

  /** Stop playback and reset position */
  stop() {
    if (this.#destroyed) return;
    op_inner_audio_stop(this.#id);
  }

  /** Seek to position in seconds */
  seek(position) {
    if (this.#destroyed) return;
    op_inner_audio_seek(this.#id, position);
  }

  /** Destroy the audio context */
  destroy() {
    if (this.#destroyed) return;
    this.#destroyed = true;
    // Clear all listeners
    for (const key in this.#listeners) {
      this.#listeners[key].off();
    }
    unregisterContext(this);
    op_inner_audio_destroy(this.#id);
  }

  // ==================== Event Listeners (multi-listener) ====================

  onCanplay(fn) {
    this.#listeners.canplay.on(fn);
  }

  onPlay(fn) {
    this.#listeners.play.on(fn);
  }

  onPause(fn) {
    this.#listeners.pause.on(fn);
  }

  onStop(fn) {
    this.#listeners.stop.on(fn);
  }

  onEnded(fn) {
    this.#listeners.ended.on(fn);
  }

  onTimeUpdate(fn) {
    this.#listeners.timeUpdate.on(fn);
  }

  onError(fn) {
    this.#listeners.error.on(fn);
  }

  onWaiting(fn) {
    this.#listeners.waiting.on(fn);
  }

  onSeeking(fn) {
    this.#listeners.seeking.on(fn);
  }

  onSeeked(fn) {
    this.#listeners.seeked.on(fn);
  }

  offCanplay(fn) {
    this.#removeListener("canplay", fn);
  }

  offPlay(fn) {
    this.#removeListener("play", fn);
  }

  offPause(fn) {
    this.#removeListener("pause", fn);
  }

  offStop(fn) {
    this.#removeListener("stop", fn);
  }

  offEnded(fn) {
    this.#removeListener("ended", fn);
  }

  offTimeUpdate(fn) {
    this.#removeListener("timeUpdate", fn);
  }

  offError(fn) {
    this.#removeListener("error", fn);
  }

  offWaiting(fn) {
    this.#removeListener("waiting", fn);
  }

  offSeeking(fn) {
    this.#removeListener("seeking", fn);
  }

  offSeeked(fn) {
    this.#removeListener("seeked", fn);
  }

  /** Remove a specific listener, or all listeners if fn is omitted */
  #removeListener(type, fn) {
    const group = this.#listeners[type];
    if (!group) return;
    group.off(fn);
  }

  // ==================== Internal Methods ====================

  async #loadAudio() {
    if (!this.#src || this.#destroyed) return;

    this.#buffered = false;

    try {
      // Use streaming load - Rust handles HTTP download and progressive decoding
      // This enables edge-download-edge-play for faster playback start
      await op_inner_audio_load_url(this.#id, this.#src);

      // Note: canplay event will be fired from native via push once enough data is buffered
      // Duration will be updated as streaming progresses
      // Autoplay is also handled by native
    } catch (e) {
      this.#fireListeners("error", { errCode: 10002, errMsg: errorMessage(e) });
    }
  }
}

function createInnerAudioContext() {
  return new InnerAudioContext();
}

function setInnerAudioOption(options = {}) {
  const {
    mixWithOther = true,
    obeyMuteSwitch = true,
    speakerOn = true,
    success,
    fail,
    complete,
  } = options;

  try {
    op_audio_set_inner_audio_option(mixWithOther, obeyMuteSwitch, speakerOn);
    const res = { errMsg: 'setInnerAudioOption:ok' };
    invokeCallback('setInnerAudioOption', 'success', success, res);
    invokeCallback('setInnerAudioOption', 'complete', complete, res);
    return Promise.resolve(res);
  } catch (err) {
    const res = { errMsg: failMessage('setInnerAudioOption', err) };
    invokeCallback('setInnerAudioOption', 'fail', fail, res);
    invokeCallback('setInnerAudioOption', 'complete', complete, res);
    return Promise.reject(res);
  }
}

function getAvailableAudioSources(options = {}) {
  const { success, fail, complete } = options;

  try {
    const sources = op_audio_get_available_audio_sources();
    const res = { errMsg: 'getAvailableAudioSources:ok', audioSources: sources };
    invokeCallback('getAvailableAudioSources', 'success', success, res);
    invokeCallback('getAvailableAudioSources', 'complete', complete, res);
    return Promise.resolve(res);
  } catch (err) {
    const res = { errMsg: failMessage('getAvailableAudioSources', err) };
    invokeCallback('getAvailableAudioSources', 'fail', fail, res);
    invokeCallback('getAvailableAudioSources', 'complete', complete, res);
    return Promise.reject(res);
  }
}

export {
  InnerAudioContext,
  createInnerAudioContext,
  setInnerAudioOption,
  getAvailableAudioSources,
  _internalEnqueueInnerAudioEvent,
};
