import { core, primordials } from "ext:core/mod.js";

const { ReflectApply } = primordials;
let timerDepth = 0;

function getTimerDepth() {
    return timerDepth;
}

// This is an engine boundary: V8 uses Explicit checkpoints; WebKit runs its
// own checkpoint after returning to the native event loop. The producer
// supplies its implementation when staging this module.
function runTimerTask(depth, callback, args, asyncContext) {
    const oldContext = core.getAsyncContext();
    const oldDepth = timerDepth;
    try {
        timerDepth = depth;
        core.setAsyncContext(asyncContext);
        ReflectApply(callback, globalThis, args);
    } finally {
        try {
            core.runMicrotasks();
        } finally {
            timerDepth = oldDepth;
            core.setAsyncContext(oldContext);
        }
    }
}

export { getTimerDepth, runTimerTask };
