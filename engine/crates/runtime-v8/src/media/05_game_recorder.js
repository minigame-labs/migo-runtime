import {
    op_game_recorder_supported,
    op_game_recorder_start,
    op_game_recorder_pause,
    op_game_recorder_resume,
    op_game_recorder_stop,
    op_game_recorder_abort,
    op_game_recorder_next_event,
} from "ext:core/ops";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";

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

export { getGameRecorder };
