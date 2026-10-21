import { core, primordials } from "ext:core/mod.js";
import { op_now, op_timer_is_backgrounded } from "ext:core/ops";

const {
    MathCeil,
    MathFloor,
    MathMax,
    MathMin,
    MapPrototypeDelete,
    MapPrototypeForEach,
    MapPrototypeGet,
    MapPrototypeHas,
    MapPrototypeSet,
    Number,
    NumberIsFinite,
    RangeError,
    ReflectApply,
    SafeMap,
    TypeError,
    TypedArrayPrototypeGetBuffer,
    Uint8Array,
    Uint32Array,
} = primordials;

const {
    getAsyncContext,
    setAsyncContext,
} = core;

const MAX_LIVE_TIMERS = 1024;
const MAX_TIMER_ID = 0x7fffffff;
const MAX_DELAY_MS = 0x7fffffff;
const MAX_UNCLAMPED_NESTING_LEVEL = 5;
const MIN_NESTED_DELAY_MS = 4;

const _clockBytes = new Uint8Array(8);
const _clockWords = new Uint32Array(TypedArrayPrototypeGetBuffer(_clockBytes));
const _timers = new SafeMap();

let _backgrounded = false;
let _lifecycleInitialized = false;
let _liveTimerCount = 0;
let _nextTimerId = 1;

function checkThis(thisArg) {
    if (thisArg !== null && thisArg !== undefined && thisArg !== globalThis) {
        throw new TypeError("Illegal invocation");
    }
}

function ensureLifecycleInitialized() {
    if (_lifecycleInitialized) return;
    _backgrounded = op_timer_is_backgrounded();
    _lifecycleInitialized = true;
}

// Instants and what remains of a delay are whole microseconds: exact, where a
// millisecond float is not. A deadline `start + 25` less `start` read as
// milliseconds since the process began can come to 25.0000000002, which the
// native timer's whole milliseconds round up to 26, and a timer restored the
// moment its runtime is shown fired a millisecond late.
function monotonicNowUs() {
    op_now(_clockBytes);
    return _clockWords[0] * 1_000_000 + MathFloor(_clockWords[1] / 1000);
}

function normalizeDelay(value) {
    const number = Number(value);
    if (!NumberIsFinite(number) || number <= 0) return 0;
    return MathMin(MAX_DELAY_MS, MathFloor(number));
}

function allocateTimerId() {
    if (_liveTimerCount >= MAX_LIVE_TIMERS) {
        throw new RangeError(`Too many live timers (limit: ${MAX_LIVE_TIMERS})`);
    }

    for (let attempts = 0; attempts < MAX_LIVE_TIMERS + 1; attempts++) {
        const id = _nextTimerId;
        _nextTimerId = id === MAX_TIMER_ID ? 1 : id + 1;
        if (!MapPrototypeHas(_timers, id)) return id;
    }

    throw new RangeError("Unable to allocate a timer id");
}

function invokeEntry(entry) {
    const oldContext = getAsyncContext();
    try {
        setAsyncContext(entry.asyncContext);
        ReflectApply(entry.callback, globalThis, entry.args);
    } finally {
        setAsyncContext(oldContext);
    }
}

function isCurrentNative(entry, generation, nativeId) {
    return MapPrototypeGet(_timers, entry.id) === entry &&
        entry.generation === generation &&
        entry.nativeId === nativeId;
}

function applyRefState(entry) {
    if (entry.nativeId !== null && !entry.refed) {
        core.unrefTimer(entry.nativeId);
    }
}

function queueNative(entry, repeat, delayUs, logicalStartUs, onFire) {
    entry.generation++;
    const generation = entry.generation;
    const deadlineUs = logicalStartUs + delayUs;
    const nativeDelayMs = MathCeil(MathMax(0, deadlineUs - monotonicNowUs()) / 1000);
    let nativeId = null;

    nativeId = core.queueUserTimer(
        entry.depth,
        repeat,
        nativeDelayMs,
        () => {
            if (!isCurrentNative(entry, generation, nativeId)) return;
            onFire(entry);
        },
    );

    entry.nativeId = nativeId;
    entry.activeSinceUs = logicalStartUs;
    entry.deadlineUs = deadlineUs;
    applyRefState(entry);
}

function armTimeout(entry, delayUs, logicalStartUs = monotonicNowUs()) {
    entry.remainingUs = MathMax(0, delayUs);
    queueNative(entry, false, entry.remainingUs, logicalStartUs, (current) => {
        current.nativeId = null;
        current.generation++;
        MapPrototypeDelete(_timers, current.id);
        _liveTimerCount--;
        invokeEntry(current);
    });
}

function armRepeatingInterval(
    entry,
    firstDelayUs,
    logicalStartUs = monotonicNowUs(),
) {
    entry.remainingUs = MathMax(0, firstDelayUs);
    queueNative(entry, true, entry.remainingUs, logicalStartUs, (current) => {
        current.remainingUs = current.periodMs * 1000;
        current.activeSinceUs = monotonicNowUs();
        current.deadlineUs = current.activeSinceUs + current.remainingUs;
        invokeEntry(current);
    });
}

function armResumedInterval(entry, delayUs, logicalStartUs) {
    entry.remainingUs = MathMax(0, delayUs);
    queueNative(entry, false, entry.remainingUs, logicalStartUs, (current) => {
        current.nativeId = null;
        armRepeatingInterval(current, current.periodMs * 1000);
        invokeEntry(current);
    });
}

function cancelNative(entry) {
    if (entry.nativeId !== null) {
        core.cancelTimer(entry.nativeId);
        entry.nativeId = null;
    }
    entry.generation++;
}

function createTimer(kind, callback, delay, args) {
    ensureLifecycleInitialized();

    // The main isolate updates the JS lifecycle synchronously around app
    // callbacks, while a Worker may observe the shared host level before its
    // queued lifecycle edge is delivered. Treat either source as authoritative
    // for entering the background; the ordered show edge restores Worker timers.
    const backgrounded = _backgrounded || op_timer_is_backgrounded();
    const createdAtUs = monotonicNowUs();
    const depth = core.getTimerDepth() + 1;
    let delayMs = normalizeDelay(delay);
    if (depth > MAX_UNCLAMPED_NESTING_LEVEL && delayMs < MIN_NESTED_DELAY_MS) {
        delayMs = MIN_NESTED_DELAY_MS;
    }
    const id = allocateTimerId();
    const entry = {
        id,
        kind,
        callback,
        args,
        asyncContext: getAsyncContext(),
        depth,
        delayMs,
        periodMs: kind === "interval" ? MathMax(1, delayMs) : 0,
        remainingUs: delayMs * 1000,
        createdAtUs,
        activeSinceUs: createdAtUs,
        deadlineUs: createdAtUs + delayMs * 1000,
        nativeId: null,
        generation: 0,
        refed: true,
    };

    MapPrototypeSet(_timers, id, entry);
    _liveTimerCount++;

    if (!backgrounded) {
        try {
            if (kind === "interval") {
                armRepeatingInterval(entry, delayMs * 1000, createdAtUs);
            } else {
                armTimeout(entry, delayMs * 1000, createdAtUs);
            }
        } catch (error) {
            cancelNative(entry);
            MapPrototypeDelete(_timers, id);
            _liveTimerCount--;
            throw error;
        }
    }

    return id;
}

function setTimeout(callback, timeout = 0, ...args) {
    checkThis(this);
    return createTimer("timeout", callback, timeout, args);
}

function setImmediate(callback, ...args) {
    return createTimer("timeout", callback, 0, args);
}

function setInterval(callback, timeout = 0, ...args) {
    checkThis(this);
    return createTimer("interval", callback, timeout, args);
}

function toTimerId(value) {
    const number = Number(value);
    if (!NumberIsFinite(number)) return 0;
    return MathFloor(number);
}

function clearTimer(id) {
    const entry = MapPrototypeGet(_timers, toTimerId(id));
    if (entry === undefined) return;

    cancelNative(entry);
    MapPrototypeDelete(_timers, entry.id);
    _liveTimerCount--;
}

function clearTimeout(id = 0) {
    checkThis(this);
    clearTimer(id);
}

function clearInterval(id = 0) {
    checkThis(this);
    clearTimer(id);
}

/** Mark a timer as not blocking event loop exit. */
function unrefTimer(id) {
    const entry = MapPrototypeGet(_timers, toTimerId(id));
    if (entry === undefined || !entry.refed) return;
    entry.refed = false;
    if (entry.nativeId !== null) core.unrefTimer(entry.nativeId);
}

/** Mark a timer as blocking event loop exit. */
function refTimer(id) {
    const entry = MapPrototypeGet(_timers, toTimerId(id));
    if (entry === undefined || entry.refed) return;
    entry.refed = true;
    if (entry.nativeId !== null) core.refTimer(entry.nativeId);
}

function freezeEntry(entry, transitionUs) {
    if (entry.nativeId === null) return;
    if (entry.createdAtUs <= transitionUs && entry.activeSinceUs < transitionUs) {
        entry.remainingUs = MathMax(0, entry.deadlineUs - transitionUs);
    }
    cancelNative(entry);
}

function restoreEntry(entry, transitionUs) {
    if (entry.nativeId !== null) return;
    const logicalStartUs = MathMax(entry.createdAtUs, transitionUs);
    if (entry.kind === "interval") {
        armResumedInterval(entry, entry.remainingUs, logicalStartUs);
    } else {
        armTimeout(entry, entry.remainingUs, logicalStartUs);
    }
}

function _internalSetTimerBackgrounded(value, elapsedMicros = 0) {
    const next = !!value;
    _lifecycleInitialized = true;
    if (_backgrounded === next) return;

    let elapsed = Number(elapsedMicros);
    if (!NumberIsFinite(elapsed) || elapsed < 0) elapsed = 0;
    const transitionUs = monotonicNowUs() - MathFloor(elapsed);

    _backgrounded = next;
    if (next) {
        MapPrototypeForEach(_timers, (entry) => freezeEntry(entry, transitionUs));
        return;
    }

    try {
        MapPrototypeForEach(_timers, (entry) => restoreEntry(entry, transitionUs));
    } catch (error) {
        _backgrounded = true;
        MapPrototypeForEach(_timers, (entry) => freezeEntry(entry, transitionUs));
        throw error;
    }
}

export {
    setTimeout,
    clearTimeout,
    setImmediate,
    setInterval,
    clearInterval,
    unrefTimer,
    refTimer,
    _internalSetTimerBackgrounded,
};
