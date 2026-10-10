import {
    op_set_window_size,
    op_set_cursor,
    op_request_pointer_lock,
    op_exit_pointer_lock,
} from "ext:core/ops";
import { createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

// The desktop window: its size, the cursor over it, pointer lock. The common
// mini-game platform offers these on its desktop clients only; where the host
// has no window to offer, setCursor answers false, pointer lock does nothing and
// setWindowSize fails as not supported.

// ---- setWindowSize ----

const _setWindowSizeApi = createDeferredApi('setWindowSize');

function setWindowSize(options) {
    return _setWindowSizeApi.invoke(options, function (opts, requestId) {
        if (typeof opts.width !== 'number' || typeof opts.height !== 'number'
            || !(opts.width > 0) || !(opts.height > 0)) {
            throw new Error('width and height must be positive numbers');
        }
        op_set_window_size(JSON.stringify({
            requestId: requestId,
            width: Math.round(opts.width),
            height: Math.round(opts.height),
        }));
    });
}

function _internalOnSetWindowSizeResult(resultJson) {
    _setWindowSizeApi.settle(resultJson);
}

// ---- onWindowStateChange ----

const _windowStateListeners = createListenerGroup('onWindowStateChange');

function onWindowStateChange(listener) {
    _windowStateListeners.on(listener);
}

function offWindowStateChange(listener) {
    _windowStateListeners.off(listener);
}

// `{"state": "minimize" | "normalize" | "maximize"}`, as the host reports it.
function _internalOnWindowStateEvent(eventJson) {
    var event;
    try { event = JSON.parse(eventJson); } catch (_) { return; }
    if (event !== null && typeof event === 'object'
        && (event.state === 'minimize' || event.state === 'normalize' || event.state === 'maximize')) {
        _windowStateListeners.trigger({ state: event.state });
    }
}

// ---- setCursor ----

// A code-package or local path to an image (ico and cur, or anything the
// platform reads), or a CSS cursor keyword -- 'default' restores the system's.
// Returns whether the cursor was taken.
function setCursor(path, x, y) {
    if (typeof path !== 'string' || path.length === 0) return false;
    return op_set_cursor(path, typeof x === 'number' ? x : 0, typeof y === 'number' ? y : 0);
}

// ---- pointer lock ----

// Taken only when the host reports it, so isPointerLocked tells the truth: a host
// may refuse a request that did not follow a user action, and the player may
// release the lock (Esc) at any time.
let _pointerLocked = false;

function requestPointerLock() {
    try { op_request_pointer_lock(); } catch (_) { /* no window: nothing to lock */ }
}

function exitPointerLock() {
    try { op_exit_pointer_lock(); } catch (_) { /* no window: nothing locked */ }
}

function isPointerLocked() {
    return _pointerLocked;
}

// `{"locked"}`, each time the lock is taken or lost.
function _internalOnPointerLockEvent(eventJson) {
    var event;
    try { event = JSON.parse(eventJson); } catch (_) { return; }
    if (event !== null && typeof event === 'object' && typeof event.locked === 'boolean') {
        _pointerLocked = event.locked;
    }
}

export {
    setWindowSize,
    _internalOnSetWindowSizeResult,
    onWindowStateChange,
    offWindowStateChange,
    _internalOnWindowStateEvent,
    setCursor,
    requestPointerLock,
    exitPointerLock,
    isPointerLocked,
    _internalOnPointerLockEvent,
};
