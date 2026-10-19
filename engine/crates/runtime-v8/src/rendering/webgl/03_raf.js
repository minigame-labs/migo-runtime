import {
    op_set_preferred_fps,
    op_await_next_frame,
    op_await_frame_content,
    op_frame_loop_available,
} from "ext:core/ops";
import { errorToString } from "ext:host_v8_base/02_async.js";
import { core, primordials } from "ext:core/mod.js";
import { frameEndAll } from "ext:host_v8_webgl/02_2d_context.js";
import { setFirstContentListener } from "ext:host_v8_webgl/00_render_command_stream.js";

// Registered for the reason `IOError` is (see 02_file_manager.js): the Rust side
// names this class in `#[class("RafError")]`, and deno_core can only build the
// object if a constructor exists under that exact name. Without one it produces
// `undefined`, and the async op path then calls `ErrorCaptureStackTrace` on it
// without the `typeof === "object"` guard `buildCustomError` has -- so V8 raises
// `TypeError: invalid_argument` and that replaces the real error on its way out.
const { Error: _PrimErrorR, PromiseResolve, PromisePrototypeThen } = primordials;
class RafError extends _PrimErrorR {
  constructor(msg) {
    super(msg);
    this.name = "RafError";
  }
}
core.registerErrorClass("RafError", RafError);

let __nextRafId = 0;
let __raf_callbacks = Object.create(null);
let __rafLoopRunning = false;

const requestAnimationFrame = (cb) => {
    const id = ++__nextRafId;
    __raf_callbacks[id] = cb;
    if (!__rafLoopRunning) {
        __rafLoopRunning = true;
        _startRafLoop();
    }
    return id;
};

const cancelAnimationFrame = (id) => {
    delete __raf_callbacks[id];
};

// R1 demand-driven RAF loop: awaits a frame signal only while callbacks are
// queued, and stops the instant none remain (no idle-frame tail). Under the
// on-demand VSync model the render thread arms exactly one frame per pending
// waiter, so an idle tail would leave the display clock running with nothing to
// draw.
async function _startRafLoop() {
    try {
        while (true) {
            // Stop immediately when no callbacks are queued. This check and the
            // `__rafLoopRunning = false` below run synchronously with no `await`
            // in between, so a requestAnimationFrame() call (which cannot
            // interrupt synchronous JS) either observed a non-empty queue before
            // this check (we await) or runs after we return (it restarts the
            // loop). Either way the request is never lost.
            if (Object.keys(__raf_callbacks).length === 0) {
                __rafLoopRunning = false;
                return;
            }

            const ts = await op_await_next_frame();

            // Snapshot + clear AFTER the await so callbacks registered during the
            // wait (including while backgrounded) are included, and callbacks
            // that (re-)register from within a callback continue the loop.
            const callbacks = __raf_callbacks;
            __raf_callbacks = Object.create(null);
            const ids = Object.keys(callbacks);

            for (let i = 0; i < ids.length; i++) {
                try {
                    callbacks[ids[i]](ts);
                } catch (e) {
                    console.error('RAF callback error: ' + errorToString(e));
                }
            }

            frameEndAll();
        }
    } catch (e) {
        console.error(`RAF loop terminated: ${errorToString(e)}`);
    }
    __rafLoopRunning = false;
}

// Native lifecycle hook: when Android returns from background there may be RAF
// callbacks that were queued while the loop had stopped. Restart the loop so
// they run -- but only if there is actually something queued, otherwise
// restarting would arm a one-shot VSync with no work to do (R1: no wasted frame
// on resume when idle). Mirrors browser engines kicking their compositor/RAF
// scheduler on visibility changes. Exposed as a global so Host::enter_foreground
// can call it once the render thread has a valid surface again.
globalThis._internalRestartRafLoop = function () {
    if (!__rafLoopRunning && Object.keys(__raf_callbacks).length > 0) {
        __rafLoopRunning = true;
        _startRafLoop();
    }
};

// The Number is forwarded as it arrived. `fps | 0` here used to be two silent
// failures at once -- a non-finite argument became 0 and a value past 2^31
// wrapped negative, so both ended up clamped to 1 fps, the opposite of what
// either caller meant -- and it made the range rule live in two places.
const setPreferredFramesPerSecond = (fps) => {
    op_set_preferred_fps(fps);
};

// Content that draws outside requestAnimationFrame -- at its top level, in a timer, in an input
// handler -- is presented at the next frame, as a browser presents what a task drew at its next
// rendering update. The frame end that publishes a frame's commands runs only in the loop above,
// and the loop runs only while callbacks are queued, so such content drew and was never shown: a
// game drawing from setInterval stayed black on Android, and the Linux player said it never drew.
// The collector answers op_await_frame_content at the first command after each frame end; when
// nothing is still waiting for a frame end, or the loop is running and will end one, there is
// nothing to do. Started by the first context, since nothing draws without one, and not at module
// evaluation, which the V8 snapshot captures.
let _frameContentWatched = false;
let _frameEndQueued = false;
const _endFrameDrawnOutsideRaf = () => {};
// Decided at the end of the task that drew, not inside the draw call that noticed: a fill must not
// start the frame loop, and an async op with it, in the middle of the content's own code. A frame
// end is coming anyway if the loop is running: its iteration ends with one.
const _endFrameSoon = () => {
    if (_frameEndQueued) return;
    _frameEndQueued = true;
    PromisePrototypeThen(PromiseResolve(), _endFrameAtTaskEnd);
};
function _endFrameAtTaskEnd() {
    _frameEndQueued = false;
    if (!__rafLoopRunning && op_frame_loop_available()) requestAnimationFrame(_endFrameDrawnOutsideRaf);
}
// Two ways content reaches a frame: records in the JS command stream, which say so as they are
// written, and the ops that push to the collector directly, which say so through the wait below.
setFirstContentListener(_endFrameSoon);

function watchFrameContent() {
    if (_frameContentWatched) return;
    _frameContentWatched = true;
    PromisePrototypeThen(PromiseResolve(), _startFrameContentWatch);
}
function _startFrameContentWatch() {
    if (!op_frame_loop_available()) {
        // Asked again by the next context, which may be made where one can run.
        _frameContentWatched = false;
        return;
    }
    PromisePrototypeThen(_watchFrameContent(), undefined, (e) => {
        console.error(`frame content watch ended: ${errorToString(e)}`);
    });
}
async function _watchFrameContent() {
    while (true) {
        // Referenced, so the event loop is never idle once a context exists. A host stops polling
        // an idle loop until a command arrives, and the poll in which a timer's last callback
        // draws is the one that would report it idle: an unreferenced wait left that draw unshown.
        if (await op_await_frame_content()) _endFrameSoon();
    }
}

export { requestAnimationFrame, cancelAnimationFrame, setPreferredFramesPerSecond, watchFrameContent };
