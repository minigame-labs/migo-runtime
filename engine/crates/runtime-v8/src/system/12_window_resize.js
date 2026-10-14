import { getWindowInfo } from "ext:host_v8_system/03_window_info.js";
import { setWindowResizeReporter } from "ext:host_v8_web/03_canvas.js";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";

const _resize = createListenerGroup("onWindowResize");
// Engine modules that follow the window's size -- the native buttons' layer --
// observe it here, apart from content's listeners, which content's
// offWindowResize() with no listener clears.
const _observers = [];
const ConsoleError = Function.prototype.call.bind(console.error, console);

/** Engine-only: `observer()` after every change of the window's size. */
function _internalObserveWindowResize(observer) {
    if (typeof observer !== "function") {
        throw new TypeError("window resize observer must be a function");
    }
    _observers.push(observer);
}
let _lastWidth = -1;
let _lastHeight = -1;

function _readWindowSize() {
    const info = getWindowInfo();
    return {
        windowWidth: info.windowWidth,
        windowHeight: info.windowHeight,
    };
}

// A listener hears the window's size change after it was added, with the size it changed to -- not the size it has
// now, which content reads with getWindowInfo (the reference calls the listener on a change only). The size now is
// what it was added at, so a later report of that same size is no change for it.
function onWindowResize(listener) {
    if (typeof listener === "function") {
        _rememberSizeSeen();
        _resize.on(listener);
    }
}

// The size content could last have seen, where nothing has been reported yet: a report of it is not a change.
function _rememberSizeSeen() {
    if (_lastWidth !== -1) return;
    try {
        const data = _readWindowSize();
        _lastWidth = data.windowWidth;
        _lastHeight = data.windowHeight;
    } catch (_) {
        // No window to read: the first report is then the first size anyone hears.
    }
}

function offWindowResize(listener) {
    _resize.off(listener);
}

// Tell content the window geometry changed.
//
// The half of a surface change that needs a window-info service, and so the half
// that this extension may own: `api-connectivity` gates it out of a Slim build,
// which then reports no geometry to content and still resizes its canvas. The
// canvas adoption is in `host_v8_web/03_canvas.js` for exactly that reason - it
// must not be gated on whether this file was compiled.
//
// De-duplication belongs here rather than around the adoption, because it
// compares the numbers the *platform* reports, which is not what decides whether
// the surface moved.
function _reportWindowResize() {
    let data;
    try {
        data = _readWindowSize();
    } catch (e) {
        console.error("onWindowResize read window info failed:", e);
        return;
    }

    if (data.windowWidth === _lastWidth && data.windowHeight === _lastHeight) {
        return;
    }
    _lastWidth = data.windowWidth;
    _lastHeight = data.windowHeight;

    for (let i = 0; i < _observers.length; i++) {
        try {
            _observers[i]();
        } catch (e) {
            try {
                ConsoleError("window resize observer threw: " + e);
            } catch (_) {}
        }
    }
    _resize.trigger(data);
}

setWindowResizeReporter(_reportWindowResize);

export {
    onWindowResize,
    offWindowResize,
    _internalObserveWindowResize,
};
