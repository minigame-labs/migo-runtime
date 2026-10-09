import { createOffscreenCanvas, getMainCanvas } from "ext:host_v8_web/03_canvas.js";
import { getWindowInfo } from "ext:host_v8_system/03_window_info.js";
import { wrapAsync, createListenerGroup } from "ext:host_v8_base/02_async.js";

const _messageListeners = createListenerGroup("onMessage");

let _sharedCanvas = null;
let _openDataContext = null;

function _createSharedCanvas(mode) {
    var canvas;

    if (mode === "screenCanvas") {
        canvas = getMainCanvas();
    } else {
        canvas = createOffscreenCanvas(1, 1);
        try {
            const info = getWindowInfo();
            const dpr = info && info.pixelRatio ? info.pixelRatio : 1;
            if (info && info.windowWidth > 0) {
                canvas.width = Math.round(info.windowWidth * dpr);
            }
            if (info && info.windowHeight > 0) {
                canvas.height = Math.round(info.windowHeight * dpr);
            }
        } catch (_) {
            // Ignore dimension sync failure; canvas remains usable.
        }
    }

    _sharedCanvas = canvas;

    try {
        globalThis.sharedCanvas = _sharedCanvas;
    } catch (_) {
        // Ignore global assignment failure.
    }

    return _sharedCanvas;
}

function _dispatchMessage(message) {
    _messageListeners.trigger(message);
}

function onMessage(callback) {
    _messageListeners.on(callback);
}

function offMessage(callback) {
    _messageListeners.off(callback);
}

class OpenDataContext {
    constructor(canvas) {
        this.canvas = canvas;
    }

    postMessage(message) {
        queueMicrotask(function () {
            _dispatchMessage(message);
        });
    }
}

function getOpenDataContext(object) {
    if (_openDataContext === null) {
        var mode = "offscreenCanvas";
        if (object && object.sharedCanvasMode === "screenCanvas") {
            mode = "screenCanvas";
        }
        _openDataContext = new OpenDataContext(_createSharedCanvas(mode));
    }
    return _openDataContext;
}

function getSharedCanvas() {
    if (_sharedCanvas) {
        return _sharedCanvas;
    }
    return _createSharedCanvas("offscreenCanvas");
}

function postMessage(message) {
    getOpenDataContext().postMessage(message);
}

// ---- Cloud Storage stubs (open data context APIs) -------------------------
// These APIs are standard but require server-side backend integration.
// Stub implementations return empty data / succeed silently to prevent crashes.

function getFriendCloudStorage(options) {
    return wrapAsync('getFriendCloudStorage', function () {
        return { data: [] };
    }, options);
}

function setUserCloudStorage(options) {
    return wrapAsync('setUserCloudStorage', function () {
        return {};
    }, options);
}

function removeUserCloudStorage(options) {
    return wrapAsync('removeUserCloudStorage', function () {
        return {};
    }, options);
}

function modifyFriendInteractiveStorage(options) {
    return wrapAsync('modifyFriendInteractiveStorage', function () {
        return {};
    }, options);
}

function getPotentialFriendList(options) {
    return wrapAsync('getPotentialFriendList', function () {
        return { list: [] };
    }, options);
}

function getGameClubData(options) {
    return wrapAsync('getGameClubData', function () {
        return { data: [] };
    }, options);
}

export {
    onMessage,
    offMessage,
    postMessage,
    getOpenDataContext,
    getSharedCanvas,
    getFriendCloudStorage,
    setUserCloudStorage,
    removeUserCloudStorage,
    modifyFriendInteractiveStorage,
    getPotentialFriendList,
    getGameClubData,
};
