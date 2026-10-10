import { op_start_accelerometer, op_stop_accelerometer } from "ext:core/ops";
import { wrapAsync, createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

const _grp = createListenerGroup('onAccelerometerChange');

function onAccelerometerChange(listener) { _grp.on(listener); }
function offAccelerometerChange(listener) { _grp.off(listener); }

function _internalTriggerAccelerometerChange(x, y, z) {
    _grp.trigger({ x: x, y: y, z: z });
}

// `game` (20 ms), `ui` (60 ms) or `normal` (200 ms); anything else is `normal`.
function intervalOf(opts) {
    const interval = opts.interval;
    return interval === 'game' || interval === 'ui' ? interval : 'normal';
}

// Starting is a request the host answers -- it fails when the device has no such
// sensor or may not use it. Stopping cannot fail.
const _startApi = createDeferredApi('startAccelerometer');

function startAccelerometer(options) {
    return _startApi.invoke(options, function (opts, requestId) {
        op_start_accelerometer(JSON.stringify({ requestId: requestId, interval: intervalOf(opts) }));
    });
}

function _internalOnStartAccelerometerResult(resultJson) {
    _startApi.settle(resultJson);
}

function stopAccelerometer(options) {
    return wrapAsync('stopAccelerometer', function () {
        op_stop_accelerometer();
    }, options);
}

export {
    onAccelerometerChange,
    offAccelerometerChange,
    _internalTriggerAccelerometerChange,
    startAccelerometer,
    _internalOnStartAccelerometerResult,
    stopAccelerometer,
};
