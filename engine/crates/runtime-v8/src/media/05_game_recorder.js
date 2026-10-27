import {
    op_game_recorder_supported,
    op_game_recorder_start,
    op_game_recorder_pause,
    op_game_recorder_resume,
    op_game_recorder_stop,
    op_game_recorder_abort,
    op_game_recorder_next_event,
    op_share_game_recording,
} from "ext:core/ops";
import { createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";
import { NativeButton } from "ext:host_v8_ui/02_buttons.js";
import { invalidateButtonLayer, measureButtonText } from "ext:host_v8_ui/05_button_layer.js";
import { createImage } from "ext:host_v8_image/01_image.js";

// getGameRecorder: the one recorder of the game's frames, as the reference
// documents it. Content's calls are judged here, with the reference's error
// codes: a call the recording's state does not allow, or an option out of its
// range, is reported as an `error` event and, for the calls that return a
// Promise, rejected with the same `{ error: { code, message } }`. What the
// recording does on its own -- its time moving on, a failure -- comes from the
// render thread. The video is the host's to share; content never sees it.

// The reference's codes (game-recorder/GameRecorder, its error table).
const INTERNAL_FAILED = [22012, "internal failed"];
const FRAME_NOT_SUPPORTED = [22022, "frame not supported"];
const DURATION_INVALID = [22103, "duration invalid"];
const BITRATE_INVALID = [22113, "bitrate invalid"];
const FPS_INVALID = [22123, "fps invalid"];
const GOP_INVALID = [22133, "gop invalid"];
const START_WHILE_RECORDING = [22143, "start while already start recording"];
const START_WHILE_PAUSED = [22153, "start while already paused"];
const PAUSE_WHILE_NOT_RECORDING = [22203, "pause while not start recording"];
const PAUSE_WHILE_PAUSED = [22213, "pause while already paused"];
const RESUME_WHILE_NOT_RECORDING = [22303, "resume while not start recording"];
const RESUME_WHILE_RECORDING = [22313, "resume while recording"];
const ABORT_WHILE_NOT_RECORDING = [22403, "abort while not start recording"];
const STOP_WHILE_NOT_RECORDING = [22503, "stop while not start recording"];
const NO_RECORDED_VIDEO = [22603, "no recorded video"];
const BGM_NOT_FOUND = [22613, "bgm not found"];
const TIME_RANGE_INVALID = [22623, "time range invalid"];
const DURATION_OUT_OF_LIMIT = [22633, "duration out of limit"];
const TIME_RANGE_TOO_SHORT = [22643, "time range too short.It should be longer than 2s"];

// What the ops answer.
const DONE = 0;
const NOT_SUPPORTED = 1;

const EVENTS = ["start", "stop", "pause", "resume", "abort", "timeUpdate", "error"];

// Each option's default and whether a value is one the reference allows.
const OPTIONS = {
    fps: [24, (v) => Number.isInteger(v) && v >= 1 && v <= 60, FPS_INVALID],
    duration: [7200, (v) => typeof v === "number" && v >= 5 && v <= 7200, DURATION_INVALID],
    bitrate: [1000, (v) => typeof v === "number" && v >= 600 && v <= 3000, BITRATE_INVALID],
    gop: [12, (v) => Number.isInteger(v) && v >= 1, GOP_INVALID],
};

// Reports an error on the recorder as its own calls do: for a share, whose
// failures the reference reports there too.
let _reportError;
// Gives the one share button what a later createGameRecorderShareButton asks.
let _configureShareButton;

/** The game's recorder: `migo.getGameRecorder()` hands out this one. */
class GameRecorder {
    #listeners = new Map(EVENTS.map((event) => [event, createListenerGroup(`GameRecorder ${event}`)]));
    // "idle", "starting", "recording", "paused" or "stopping".
    #state = "idle";
    // Which recording the events being heard belong to.
    #recording = 0;

    /** Whether this device can record the game. */
    isFrameSupported() {
        return op_game_recorder_supported();
    }

    /** The game's sound is not recorded: the engine has no one mix of it. */
    isSoundSupported() {
        return false;
    }

    /** A recording's volume is the host's to change when it shares it. */
    isVolumeSupported() {
        return false;
    }

    /** So is its rate. */
    isAtempoSupported() {
        return false;
    }

    on(event, callback) {
        this.#listeners.get(event)?.on(callback);
    }

    off(event, callback) {
        this.#listeners.get(event)?.off(callback);
    }

    #emit(event, res) {
        this.#listeners.get(event).trigger(res);
    }

    static {
        _reportError = (recorder, error) => recorder.#error(error);
    }

    // `[code, message]` reported as the reference reports it, and the value a
    // Promise is rejected with.
    #error([code, message]) {
        const res = { error: { code, message } };
        this.#emit("error", res);
        return res;
    }

    /** Start recording; the `start` event says when frames are being taken. */
    start(options) {
        const opts = options || {};
        if (this.#state === "paused") {
            this.#error(START_WHILE_PAUSED);
            return;
        }
        if (this.#state !== "idle") {
            this.#error(START_WHILE_RECORDING);
            return;
        }
        const values = {};
        for (const [name, [fallback, allowed, invalid]] of Object.entries(OPTIONS)) {
            const value = opts[name] === undefined ? fallback : opts[name];
            if (!allowed(value)) {
                this.#error(invalid);
                return;
            }
            values[name] = value;
        }
        this.#state = "starting";
        const recording = ++this.#recording;
        op_game_recorder_start(values.fps, Math.round(values.bitrate), values.gop, Math.round(values.duration * 1000))
            .then((status) => {
                if (recording !== this.#recording) return;
                if (status !== DONE) {
                    this.#state = "idle";
                    this.#error(status === NOT_SUPPORTED ? FRAME_NOT_SUPPORTED : INTERNAL_FAILED);
                    return;
                }
                this.#state = "recording";
                this.#hear(recording);
                this.#emit("start", undefined);
            });
    }

    // What the recording does on its own, until it ends.
    #hear(recording) {
        op_game_recorder_next_event().then((event) => {
            if (recording !== this.#recording || event.kind === "closed") return;
            if (event.kind === "failed") {
                this.#state = "idle";
                this.#error(INTERNAL_FAILED);
                return;
            }
            this.#emit("timeUpdate", { currentTime: event.ms / 1000 });
            this.#hear(recording);
        });
    }

    // A call the recording's state allows when `allowed`, made through `op`;
    // `then` moves the state on and says what happened.
    #control(allowed, refused, op, then) {
        const refusal = allowed(this.#state);
        if (refusal) return Promise.reject(this.#error(refused(refusal)));
        const recording = this.#recording;
        return op().then((outcome) => {
            if (recording !== this.#recording) return Promise.reject(this.#error(INTERNAL_FAILED));
            const res = then(outcome);
            return res === null ? Promise.reject(this.#error(INTERNAL_FAILED)) : res;
        });
    }

    /** Pause: frames stop being taken, and the recording's time stops. */
    pause() {
        return this.#control(
            (state) => state === "recording" ? null : state,
            (state) => state === "paused" ? PAUSE_WHILE_PAUSED : PAUSE_WHILE_NOT_RECORDING,
            op_game_recorder_pause,
            (status) => {
                if (status !== DONE) return null;
                this.#state = "paused";
                this.#emit("pause", undefined);
                return {};
            },
        );
    }

    resume() {
        return this.#control(
            (state) => state === "paused" ? null : state,
            (state) => state === "recording" ? RESUME_WHILE_RECORDING : RESUME_WHILE_NOT_RECORDING,
            op_game_recorder_resume,
            (status) => {
                if (status !== DONE) return null;
                this.#state = "recording";
                this.#emit("resume", undefined);
                return {};
            },
        );
    }

    /** End the recording; its video is complete when this settles. */
    stop() {
        return this.#control(
            (state) => state === "recording" || state === "paused" ? null : state,
            () => STOP_WHILE_NOT_RECORDING,
            () => {
                this.#state = "stopping";
                return op_game_recorder_stop();
            },
            (duration) => {
                this.#state = "idle";
                this.#recording += 1;
                if (duration < 0) return null;
                const res = { duration };
                this.#emit("stop", res);
                return res;
            },
        );
    }

    /** End the recording and drop what it recorded. */
    abort() {
        return this.#control(
            (state) => state === "recording" || state === "paused" ? null : state,
            () => ABORT_WHILE_NOT_RECORDING,
            () => {
                this.#state = "stopping";
                return op_game_recorder_abort();
            },
            (status) => {
                this.#state = "idle";
                this.#recording += 1;
                if (status !== DONE) return null;
                this.#emit("abort", undefined);
                return {};
            },
        );
    }
}

let _recorder = null;

/** The game's one recorder. */
function getGameRecorder() {
    if (_recorder === null) _recorder = new GameRecorder();
    return _recorder;
}

// ---- sharing the last finished recording ----------------------------------------
//
// The video is the host's to share -- its editor, its audience -- and both ways
// content asks (operateGameRecorderVideo, the share button) hand it the same
// request: the engine's file, never a path content names, and the options the
// reference documents, judged here in its codes.

const ATEMPOS = [0.3, 0.5, 1, 1.5, 2, 2.5, 3];
const MAX_SHARED_MS = 60000;
const MIN_SHARED_MS = 2000;

// `[code, message]` thrown out of a share's start: the call fails with the
// message and the code as its `errCode`.
function refusal([code, message]) {
    return Object.assign(new Error(message), { errCode: code });
}

// What the host is handed for the share settings `share`, or a refusal thrown.
// The button's share needs bgm and timeRange; the call's does not.
function shareRequest(share, required) {
    const s = share || {};
    const request = {};
    for (const key of ["title", "desc", "query", "path"]) {
        if (typeof s[key] === "string") request[key] = s[key];
    }
    if (typeof s.bgm === "string" && s.bgm !== "") {
        request.bgm = s.bgm;
    } else if (required) {
        throw refusal(BGM_NOT_FOUND);
    }
    if (s.timeRange !== undefined || required) {
        const ranges = s.timeRange;
        const valid = Array.isArray(ranges) && ranges.length > 0 && ranges.every((range) => Array.isArray(range)
            && range.length === 2 && Number.isFinite(range[0]) && Number.isFinite(range[1])
            && range[0] >= 0 && range[1] > range[0]);
        if (!valid) throw refusal(TIME_RANGE_INVALID);
        const total = ranges.reduce((sum, range) => sum + (range[1] - range[0]), 0);
        if (total > MAX_SHARED_MS) throw refusal(DURATION_OUT_OF_LIMIT);
        if (total < MIN_SHARED_MS) throw refusal(TIME_RANGE_TOO_SHORT);
        request.timeRange = ranges.map((range) => [range[0], range[1]]);
    }
    request.volume = typeof s.volume === "number" && s.volume >= 0 && s.volume <= 1 ? s.volume : 1;
    request.atempo = ATEMPOS.includes(s.atempo) ? s.atempo : 1;
    request.audioMix = s.audioMix === true;
    return request;
}

// What the share op refuses before the host is asked, by the words share/mod.rs
// refuses it in.
const OP_REFUSALS = new Map([[NO_RECORDED_VIDEO[1], NO_RECORDED_VIDEO], [BGM_NOT_FOUND[1], BGM_NOT_FOUND]]);

// The player's share is theirs to take: no timeout.
const _shareApi = createDeferredApi("operateGameRecorderVideo", 0);

// Share the last finished recording with `settings`, answered as `options` asks.
function share(options, settings, required) {
    return _shareApi.invoke(options, (_, requestId) => {
        const request = shareRequest(settings, required);
        request.requestId = requestId;
        return op_share_game_recording(JSON.stringify(request)).catch((e) => {
            const known = OP_REFUSALS.get(e && e.message);
            throw known === undefined ? e : refusal(known);
        });
    });
}

/** Share the last finished recording through the host, with `options`' share settings. */
function operateGameRecorderVideo(options) {
    return share(options, options, false);
}

function _internalOnShareGameRecordingResult(json) {
    _shareApi.settle(json);
}

// The share button's style, as `[default, least, most]` by the reference.
const SHARE_STYLE = {
    height: [40, 40, Infinity],
    iconMarginRight: [8, 8, Infinity],
    fontSize: [17, 17, 22],
    paddingLeft: [16, 16, Infinity],
    paddingRight: [16, 16, Infinity],
};

// The icon's fixed size, by the reference.
const ICON_WIDTH = 24;
const ICON_HEIGHT = 16;

function _styleValue(style, key) {
    const [fallback, least, most] = SHARE_STYLE[key];
    const value = Number(style[key]);
    return Number.isFinite(value) ? Math.min(most, Math.max(least, value)) : fallback;
}

/**
 * The recorder's share button: drawn by the engine with the other native
 * buttons, sized around its icon and text, and sharing the last finished
 * recording when the player taps it. A share that fails other than by the
 * player's cancelling is what its tap listeners hear, as the reference has it,
 * and one refused in the recorder's codes is the recorder's error too.
 */
class GameRecorderShareButton extends NativeButton {
    #icon = "";
    #iconPicture = null;
    #share = {};

    constructor(options) {
        const opts = options || {};
        const style = Object.assign({}, opts.style || {});
        for (const key of Object.keys(SHARE_STYLE)) {
            if (style[key] === undefined) style[key] = SHARE_STYLE[key][0];
        }
        super("GameRecorderShareButton", { type: "text", text: opts.text, image: opts.image, style });
        this.icon = opts.icon;
        this.#share = opts.share || {};
    }

    static {
        _configureShareButton = (button, opts) => button.#configure(opts);
    }

    #configure(opts) {
        if (opts.style) Object.assign(this.style, opts.style);
        this.text = opts.text;
        this.image = opts.image;
        this.icon = opts.icon;
        this.#share = opts.share || {};
    }

    get icon() { return this.#icon; }
    set icon(value) {
        const path = typeof value === "string" ? value : "";
        if (path === this.#icon) return;
        this.#icon = path;
        this.#iconPicture = null;
        if (path !== "") {
            const picture = createImage();
            picture.onload = () => {
                if (this.#iconPicture === picture) invalidateButtonLayer();
            };
            picture.src = path;
            this.#iconPicture = picture;
        }
        invalidateButtonLayer();
    }

    #font() {
        return _styleValue(this.style, "fontSize") + "px sans-serif";
    }

    _size() {
        const text = this.text === "" ? 0
            : _styleValue(this.style, "iconMarginRight") + measureButtonText(this.#font(), this.text);
        return [Math.ceil(_styleValue(this.style, "paddingLeft") + ICON_WIDTH + text
            + _styleValue(this.style, "paddingRight")), _styleValue(this.style, "height")];
    }

    _paint(context) {
        const bounds = this._paintBounds();
        if (bounds === null) return;
        const { left, top, width, height } = bounds;
        const style = this.style;
        const radius = Math.max(0, Math.min(Number(style.borderRadius) || 0, width / 2, height / 2));
        const background = this._loadedPicture();
        if (background !== null) {
            context.drawImage(background, left, top, width, height);
        } else if (style.backgroundColor !== "transparent") {
            // Without a background of content's, a dark translucent pill.
            const given = typeof style.backgroundColor === "string" && style.backgroundColor !== "";
            context.fillStyle = given ? style.backgroundColor : "rgba(0,0,0,0.5)";
            context.beginPath();
            context.roundRect(left, top, width, height, given ? radius : height / 2);
            context.fill();
        }
        const color = typeof style.color === "string" && style.color !== "" ? style.color : "#ffffff";
        const x = left + _styleValue(style, "paddingLeft");
        const y = top + (height - ICON_HEIGHT) / 2;
        const icon = this.#iconPicture;
        if (icon !== null && icon.complete && icon.width > 0) {
            context.drawImage(icon, x, y, ICON_WIDTH, ICON_HEIGHT);
        } else {
            // Without an icon of content's, a play mark in the text's colour.
            context.fillStyle = color;
            context.beginPath();
            context.moveTo(x + 6, y);
            context.lineTo(x + 6 + ICON_HEIGHT * 0.87, y + ICON_HEIGHT / 2);
            context.lineTo(x + 6, y + ICON_HEIGHT);
            context.closePath();
            context.fill();
        }
        if (this.text !== "") {
            context.font = this.#font();
            context.fillStyle = color;
            context.textAlign = "left";
            context.textBaseline = "middle";
            context.fillText(this.text, x + ICON_WIDTH + _styleValue(style, "iconMarginRight"), top + height / 2);
        }
    }

    _act() {
        share({}, this.#share, true).then(undefined, (res) => {
            if (/:fail cancel$/.test(res.errMsg)) return;
            const coded = [NO_RECORDED_VIDEO, BGM_NOT_FOUND, TIME_RANGE_INVALID, DURATION_OUT_OF_LIMIT,
                TIME_RANGE_TOO_SHORT].find(([code]) => code === res.errCode);
            if (coded !== undefined) _reportError(getGameRecorder(), coded);
            this._emitTap(res);
        });
    }
}

let _shareButton = null;

/** The recorder's share button: one, configured by the latest call. */
function createGameRecorderShareButton(options) {
    if (_shareButton === null) {
        _shareButton = new GameRecorderShareButton(options);
    } else {
        _configureShareButton(_shareButton, options || {});
        _shareButton.show();
    }
    return _shareButton;
}

export {
    getGameRecorder,
    operateGameRecorderVideo,
    createGameRecorderShareButton,
    _internalOnShareGameRecordingResult,
};
