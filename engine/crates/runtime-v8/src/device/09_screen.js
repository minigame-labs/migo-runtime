import {
    op_get_screen_brightness,
    op_set_screen_brightness,
    op_set_keep_screen_on,
    op_set_device_orientation,
    op_start_capture_screen,
    op_stop_capture_screen,
    op_get_screen_recording_state,
    op_start_screen_recording_observer,
    op_stop_screen_recording_observer,
    op_set_visual_effect_on_capture,
    op_set_enable_debug,
} from "ext:core/ops";
import { wrapAsync, createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

// Brightness, orientation and capture are the host's: each is a request it
// answers, and may refuse.
const _getBrightnessApi = createDeferredApi('getScreenBrightness');

function getScreenBrightness(options) {
    return _getBrightnessApi.invoke(options, function (opts, requestId) {
        op_get_screen_brightness(JSON.stringify({ requestId: requestId }));
    });
}

function _internalOnGetScreenBrightnessResult(resultJson) {
    _getBrightnessApi.settle(resultJson);
}

const _setBrightnessApi = createDeferredApi('setScreenBrightness');

function setScreenBrightness(options) {
    return _setBrightnessApi.invoke(options, function (opts, requestId) {
        if (typeof opts.value !== 'number' || !(opts.value >= 0 && opts.value <= 1)) {
            throw new Error('value must be a number from 0 to 1');
        }
        op_set_screen_brightness(JSON.stringify({ requestId: requestId, value: opts.value }));
    });
}

function _internalOnSetScreenBrightnessResult(resultJson) {
    _setBrightnessApi.settle(resultJson);
}

function setKeepScreenOn(options = {}) {
    return wrapAsync('setKeepScreenOn', function () {
        var keepScreenOn = options.keepScreenOn;
        if (typeof keepScreenOn !== 'boolean') {
            throw new Error('keepScreenOn is required and must be a boolean');
        }
        op_set_keep_screen_on(keepScreenOn);
    }, options);
}

const _setOrientationApi = createDeferredApi('setDeviceOrientation');

function setDeviceOrientation(options) {
    return _setOrientationApi.invoke(options, function (opts, requestId) {
        if (opts.value !== 'landscape' && opts.value !== 'portrait') {
            throw new Error('value must be "landscape" or "portrait"');
        }
        op_set_device_orientation(JSON.stringify({ requestId: requestId, value: opts.value }));
    });
}

function _internalOnSetDeviceOrientationResult(resultJson) {
    _setOrientationApi.settle(resultJson);
}

// ---- screen recording --------------------------------------------------------

const _getRecordingStateApi = createDeferredApi('getScreenRecordingState');

function getScreenRecordingState(options) {
    return _getRecordingStateApi.invoke(options, function (opts, requestId) {
        op_get_screen_recording_state(JSON.stringify({ requestId: requestId }));
    });
}

function _internalOnGetScreenRecordingStateResult(resultJson) {
    _getRecordingStateApi.settle(resultJson);
}

var _recordingListeners = createListenerGroup('onScreenRecordingStateChanged');

// The host observes only while someone listens, as for screenshots.
function onScreenRecordingStateChanged(listener) {
    if (typeof listener === 'function') {
        var hadListeners = _recordingListeners.size() > 0;
        _recordingListeners.on(listener);
        if (!hadListeners) {
            try { op_start_screen_recording_observer(); } catch (_) {}
        }
    }
}

function offScreenRecordingStateChanged(listener) {
    _recordingListeners.off(listener);
    if (_recordingListeners.size() === 0) {
        try { op_stop_screen_recording_observer(); } catch (_) {}
    }
}

// `{"state": "on" | "off"}`, as the host reports each change.
function _internalOnScreenRecordingStateEvent(eventJson) {
    var event;
    try { event = JSON.parse(eventJson); } catch (_) { return; }
    if (event !== null && typeof event === 'object' && (event.state === 'on' || event.state === 'off')) {
        _recordingListeners.trigger({ state: event.state });
    }
}

const _visualEffectApi = createDeferredApi('setVisualEffectOnCapture');

function setVisualEffectOnCapture(options) {
    return _visualEffectApi.invoke(options, function (opts, requestId) {
        if (opts.visualEffect !== 'none' && opts.visualEffect !== 'hidden') {
            throw new Error('visualEffect must be "none" or "hidden"');
        }
        op_set_visual_effect_on_capture(JSON.stringify({
            requestId: requestId,
            visualEffect: opts.visualEffect,
        }));
    });
}

function _internalOnSetVisualEffectOnCaptureResult(resultJson) {
    _visualEffectApi.settle(resultJson);
}

// ==================== Debug ====================

function setEnableDebug(options = {}) {
    return wrapAsync('setEnableDebug', function () {
        var enableDebug = options.enableDebug;
        if (typeof enableDebug !== 'boolean') {
            throw new Error('enableDebug is required and must be a boolean');
        }
        op_set_enable_debug(enableDebug);
    }, options);
}

// ==================== User Capture Screen ====================

var _captureScreenListeners = createListenerGroup('onUserCaptureScreen');

function onUserCaptureScreen(listener) {
    if (typeof listener === 'function') {
        var hadListeners = _captureScreenListeners.size() > 0;
        _captureScreenListeners.on(listener);
        if (!hadListeners) {
            try { op_start_capture_screen(); } catch (_) {}
        }
    }
}

function offUserCaptureScreen(listener) {
    _captureScreenListeners.off(listener);
    if (_captureScreenListeners.size() === 0) {
        try { op_stop_capture_screen(); } catch (_) {}
    }
}

function _internalTriggerUserCaptureScreen() {
    _captureScreenListeners.trigger({});
}

export {
    getScreenBrightness,
    _internalOnGetScreenBrightnessResult,
    setScreenBrightness,
    _internalOnSetScreenBrightnessResult,
    setKeepScreenOn,
    setDeviceOrientation,
    _internalOnSetDeviceOrientationResult,
    getScreenRecordingState,
    _internalOnGetScreenRecordingStateResult,
    onScreenRecordingStateChanged,
    offScreenRecordingStateChanged,
    _internalOnScreenRecordingStateEvent,
    setVisualEffectOnCapture,
    _internalOnSetVisualEffectOnCaptureResult,
    setEnableDebug,
    onUserCaptureScreen,
    offUserCaptureScreen,
    _internalTriggerUserCaptureScreen,
};
