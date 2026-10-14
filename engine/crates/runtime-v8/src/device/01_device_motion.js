import { op_start_device_motion, op_stop_device_motion } from "ext:core/ops";
import { wrapAsync, createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

const _grp = createListenerGroup('onDeviceMotionChange');

function onDeviceMotionChange(listener) { _grp.on(listener); }
function offDeviceMotionChange(listener) { _grp.off(listener); }

// Every host reports the W3C DeviceOrientation angles in degrees -- alpha [0, 360), beta [-180, 180), gamma [-90, 90)
// -- and content hears them as the reference documents them, in radians: alpha [0, 2*PI), beta [-PI, PI), gamma
// [-PI/2, PI/2). Converted here, once, so no host has to know the reference's unit.
const RADIANS_PER_DEGREE = Math.PI / 180;
function _internalTriggerDeviceMotionChange(alpha, beta, gamma) {
    _grp.trigger({
        alpha: alpha * RADIANS_PER_DEGREE,
        beta: beta * RADIANS_PER_DEGREE,
        gamma: gamma * RADIANS_PER_DEGREE,
    });
}

// `game` (20 ms), `ui` (60 ms) or `normal` (200 ms); anything else is `normal`.
function intervalOf(opts) {
    const interval = opts.interval;
    return interval === 'game' || interval === 'ui' ? interval : 'normal';
}

// Starting is a request the host answers -- it fails when the device has no such
// sensor or may not use it. Stopping cannot fail.
const _startApi = createDeferredApi('startDeviceMotionListening');

function startDeviceMotionListening(options) {
    return _startApi.invoke(options, function (opts, requestId) {
        op_start_device_motion(JSON.stringify({ requestId: requestId, interval: intervalOf(opts) }));
    });
}

function _internalOnStartDeviceMotionListeningResult(resultJson) {
    _startApi.settle(resultJson);
}

function stopDeviceMotionListening(options) {
    return wrapAsync('stopDeviceMotionListening', function () {
        op_stop_device_motion();
    }, options);
}

export {
    onDeviceMotionChange,
    offDeviceMotionChange,
    _internalTriggerDeviceMotionChange,
    startDeviceMotionListening,
    _internalOnStartDeviceMotionListeningResult,
    stopDeviceMotionListening,
};
