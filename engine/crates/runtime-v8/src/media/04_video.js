import {
    op_video_create,
    op_video_destroy,
    op_video_set,
    op_video_play,
    op_video_pause,
    op_video_stop,
    op_video_seek,
    op_video_request_full_screen,
    op_video_exit_full_screen,
} from "ext:core/ops";
import {
    allocateHostCallbackId,
    createDeferredApi,
    createListenerGroup,
    failMessage,
} from "ext:host_v8_base/02_async.js";

// The players content holds, by id: where the host's events go.
const _videos = new Map();

// Every call is answered through its hook once what it asked has happened, as
// the reference's promises settle. Playing may wait on the network as long as
// it takes, and the host answers every call -- a destroyed player's with a
// failure -- so none has a timeout.
const _playApi = createDeferredApi("video.play", 0);
const _pauseApi = createDeferredApi("video.pause", 0);
const _stopApi = createDeferredApi("video.stop", 0);
const _seekApi = createDeferredApi("video.seek", 0);
const _requestFullScreenApi = createDeferredApi("video.requestFullScreen", 0);
const _exitFullScreenApi = createDeferredApi("video.exitFullScreen", 0);

const OBJECT_FITS = ["contain", "cover", "fill"];
const PLAYBACK_RATES = [0.5, 0.8, 1.0, 1.25, 1.5];
const DIRECTIONS = [0, 90, -90];

const finite = (value) => typeof value === "number" && Number.isFinite(value);
const nonNegative = (value) => finite(value) && value >= 0;
const bool = (value) => typeof value === "boolean";
const string = (value) => typeof value === "string";
const oneOf = (values) => (value) => values.includes(value);

// Every property the reference documents, with its default and what it takes.
const PROPERTIES = {
    x: [0, finite],
    y: [0, finite],
    width: [300, nonNegative],
    height: [150, nonNegative],
    src: ["", string],
    poster: ["", string],
    initialTime: [0, nonNegative],
    playbackRate: [1.0, oneOf(PLAYBACK_RATES)],
    live: [false, bool],
    objectFit: ["contain", oneOf(OBJECT_FITS)],
    controls: [true, bool],
    showProgress: [true, bool],
    showProgressInControlMode: [true, bool],
    backgroundColor: ["#000000", string],
    autoplay: [false, bool],
    loop: [false, bool],
    muted: [false, bool],
    obeyMuteSwitch: [false, bool],
    enableProgressGesture: [true, bool],
    enablePlayGesture: [false, bool],
    showCenterPlayBtn: [true, bool],
    underGameView: [false, bool],
    autoPauseIfNavigate: [true, bool],
    autoPauseIfOpenNative: [true, bool],
};

// A command the host carries out and does not answer: one it refuses is
// reported, since there is no call to fail.
function command(op, request) {
    try {
        op(JSON.stringify(request));
    } catch (e) {
        console.error(failMessage("video", e));
    }
}

// A player's private state, as the module needs it: set by the class, reachable
// by nothing content holds. `_dispatch` hands an event to its listeners;
// `_read` and `_change` are its properties.
let _dispatch;
let _read;
let _change;
let _handler;
let _setHandler;

/**
 * A video player, shown where content put it -- over the game, or under it.
 * Made by createVideo; its properties may be set, and play, pause, stop, seek
 * and the full-screen calls settle once done.
 */
class Video {
    #id;
    #destroyed = false;
    #properties;
    // The reference's assignable handlers -- video.onplay = fn -- by event
    // type, each called after the event's listeners.
    #handlers = {
        play: null,
        pause: null,
        ended: null,
        timeUpdate: null,
        error: null,
        waiting: null,
        progress: null,
    };
    #listeners = {
        play: createListenerGroup("Video play"),
        pause: createListenerGroup("Video pause"),
        ended: createListenerGroup("Video ended"),
        timeUpdate: createListenerGroup("Video timeUpdate"),
        error: createListenerGroup("Video error"),
        waiting: createListenerGroup("Video waiting"),
        progress: createListenerGroup("Video progress"),
    };

    static {
        _dispatch = (video, type, res) => {
            video.#listeners[type].trigger(res);
            const handler = video.#handlers[type];
            if (typeof handler === "function") {
                try {
                    handler.call(video, res);
                } catch (e) {
                    console.error(failMessage(`Video on${type.toLowerCase()}`, e));
                }
            }
        };
        _handler = (video, type) => video.#handlers[type];
        _setHandler = (video, type, fn) => {
            if (!video.#destroyed) video.#handlers[type] = typeof fn === "function" ? fn : null;
        };
        _read = (video, name) => video.#properties[name];
        // Kept and sent when the reference allows the value, ignored otherwise.
        _change = (video, name, value) => {
            if (video.#destroyed || !PROPERTIES[name][1](value)) return;
            video.#properties[name] = value;
            command(op_video_set, { videoId: video.#id, [name]: value });
        };
    }

    constructor(id, properties) {
        this.#id = id;
        this.#properties = properties;
    }

    /** Play. Settles once it plays. */
    play() {
        return this.#call(_playApi, "video.play", op_video_play, {});
    }

    /** Pause. Settles once paused. */
    pause() {
        return this.#call(_pauseApi, "video.pause", op_video_pause, {});
    }

    /** Stop, back at the start. Settles once stopped. */
    stop() {
        return this.#call(_stopApi, "video.stop", op_video_stop, {});
    }

    /** Go to `time` seconds. Settles once there. */
    seek(time) {
        if (!nonNegative(time)) {
            return Promise.reject({ errMsg: "video.seek:fail time must be a number of seconds" });
        }
        return this.#call(_seekApi, "video.seek", op_video_seek, { time });
    }

    /** Fill the screen, turned `direction`: 0, 90 (counterclockwise) or -90. */
    requestFullScreen(direction) {
        const turned = direction === undefined ? 0 : direction;
        if (!DIRECTIONS.includes(turned)) {
            return Promise.reject({ errMsg: "video.requestFullScreen:fail direction must be 0, 90 or -90" });
        }
        return this.#call(_requestFullScreenApi, "video.requestFullScreen", op_video_request_full_screen,
            { direction: turned });
    }

    /** Leave full screen. Settles once back where content put it. */
    exitFullScreen() {
        return this.#call(_exitFullScreenApi, "video.exitFullScreen", op_video_exit_full_screen, {});
    }

    /** Take the player away; its calls still unanswered fail. */
    destroy() {
        if (this.#destroyed) return;
        this.#destroyed = true;
        _videos.delete(this.#id);
        command(op_video_destroy, { videoId: this.#id });
        for (const key of Object.keys(this.#listeners)) {
            this.#listeners[key].off();
            this.#handlers[key] = null;
        }
    }

    onPlay(callback) { this.#listeners.play.on(callback); }
    offPlay(callback) { this.#listeners.play.off(callback); }
    onPause(callback) { this.#listeners.pause.on(callback); }
    offPause(callback) { this.#listeners.pause.off(callback); }
    onEnded(callback) { this.#listeners.ended.on(callback); }
    offEnded(callback) { this.#listeners.ended.off(callback); }
    onTimeUpdate(callback) { this.#listeners.timeUpdate.on(callback); }
    offTimeUpdate(callback) { this.#listeners.timeUpdate.off(callback); }
    onError(callback) { this.#listeners.error.on(callback); }
    offError(callback) { this.#listeners.error.off(callback); }
    onWaiting(callback) { this.#listeners.waiting.on(callback); }
    offWaiting(callback) { this.#listeners.waiting.off(callback); }
    onProgress(callback) { this.#listeners.progress.on(callback); }
    offProgress(callback) { this.#listeners.progress.off(callback); }

    // One request of this player, `name`, answered through `api`'s hook.
    #call(api, name, op, request) {
        if (this.#destroyed) {
            return Promise.reject({ errMsg: failMessage(name, "video destroyed") });
        }
        request.videoId = this.#id;
        return api.invoke({}, (opts, requestId) => {
            request.requestId = requestId;
            op(JSON.stringify(request));
        });
    }
}

// video.onplay, .onpause, .onended, .ontimeupdate, .onerror, .onwaiting,
// .onprogress: one handler each, as the reference documents them.
for (const type of ["play", "pause", "ended", "timeUpdate", "error", "waiting", "progress"]) {
    Object.defineProperty(Video.prototype, `on${type.toLowerCase()}`, {
        get() { return _handler(this, type); },
        set(fn) { _setHandler(this, type, fn); },
        enumerable: false,
        configurable: true,
    });
}

for (const name of Object.keys(PROPERTIES)) {
    Object.defineProperty(Video.prototype, name, {
        get() { return _read(this, name); },
        set(value) { _change(this, name, value); },
        enumerable: false,
        configurable: true,
    });
}

/**
 * Create a video player and show it.
 *
 * @param {Object} options - The reference's: src (required), x, y, width,
 *   height, poster, initialTime, playbackRate, live, objectFit, controls,
 *   showProgress, showProgressInControlMode, backgroundColor, autoplay, loop,
 *   muted, obeyMuteSwitch, enableProgressGesture, enablePlayGesture,
 *   showCenterPlayBtn, underGameView, autoPauseIfNavigate, autoPauseIfOpenNative
 * @returns {Video}
 */
function createVideo(options) {
    const opts = options || {};
    const properties = {};
    for (const [name, [fallback, valid]] of Object.entries(PROPERTIES)) {
        properties[name] = valid(opts[name]) ? opts[name] : fallback;
    }
    // From the Host's id space, not a module counter: the platform keeps its
    // players in a map that outlives this isolate, so a counter restarting at 1
    // hands the replacement runtime an id the retired one still owns -- and the
    // events of the video that is still playing would arrive at the new object.
    const videoId = allocateHostCallbackId();
    command(op_video_create, Object.assign({ videoId }, properties));
    const video = new Video(videoId, properties);
    _videos.set(videoId, video);
    return video;
}

// ==================== The host's answers and events ====================

function _internalOnVideoPlayResult(json) { _playApi.settle(json); }
function _internalOnVideoPauseResult(json) { _pauseApi.settle(json); }
function _internalOnVideoStopResult(json) { _stopApi.settle(json); }
function _internalOnVideoSeekResult(json) { _seekApi.settle(json); }
function _internalOnVideoRequestFullScreenResult(json) { _requestFullScreenApi.settle(json); }
function _internalOnVideoExitFullScreenResult(json) { _exitFullScreenApi.settle(json); }

// An event's player and fields, if it is a player content holds.
function _event(json) {
    let event;
    try {
        event = JSON.parse(json);
    } catch (_) {
        return null;
    }
    const video = event && typeof event === "object" ? _videos.get(event.videoId) : undefined;
    return video === undefined ? null : [video, event];
}

function _on(type, shape) {
    return (json) => {
        const found = _event(json);
        if (found) _dispatch(found[0], type, shape(found[1]));
    };
}

const _internalOnVideoPlayEvent = _on("play", () => ({}));
const _internalOnVideoPauseEvent = _on("pause", () => ({}));
const _internalOnVideoEndedEvent = _on("ended", () => ({}));
const _internalOnVideoWaitingEvent = _on("waiting", () => ({}));
const _internalOnVideoTimeUpdateEvent = _on("timeUpdate",
    (event) => ({ position: Number(event.position) || 0, duration: Number(event.duration) || 0 }));
const _internalOnVideoProgressEvent = _on("progress",
    (event) => ({ buffered: Number(event.buffered) || 0, duration: Number(event.duration) || 0 }));
const _internalOnVideoErrorEvent = _on("error",
    (event) => ({ errMsg: String(event.errMsg ?? "video:fail unknown error") }));

export {
    Video,
    createVideo,
    _internalOnVideoPlayResult,
    _internalOnVideoPauseResult,
    _internalOnVideoStopResult,
    _internalOnVideoSeekResult,
    _internalOnVideoRequestFullScreenResult,
    _internalOnVideoExitFullScreenResult,
    _internalOnVideoPlayEvent,
    _internalOnVideoPauseEvent,
    _internalOnVideoEndedEvent,
    _internalOnVideoTimeUpdateEvent,
    _internalOnVideoErrorEvent,
    _internalOnVideoWaitingEvent,
    _internalOnVideoProgressEvent,
};
