import { op_start_gyroscope, op_stop_gyroscope } from "ext:core/ops";
import { wrapAsync, createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

const _grp = createListenerGroup('onGyroscopeChange');

function onGyroscopeChange(listener) { _grp.on(listener); }
function offGyroscopeChange(listener) { _grp.off(listener); }

function _internalTriggerGyroscopeChange(x, y, z) {
    _grp.trigger({ x, y, z });
}

// `game` (20 ms), `ui` (60 ms) or `normal` (200 ms); anything else is `normal`.
function intervalOf(opts) {
    const interval = opts.interval;
    return interval === 'game' || interval === 'ui' ? interval : 'normal';
}

// Starting is a request the host answers -- it fails when the device has no such
// sensor or may not use it. Stopping cannot fail.
const _startApi = createDeferredApi('startGyroscope');

function startGyroscope(options) {
    return _startApi.invoke(options, function (opts, requestId) {
        op_start_gyroscope(JSON.stringify({ requestId: requestId, interval: intervalOf(opts) }));
    });
}

function _internalOnStartGyroscopeResult(resultJson) {
    _startApi.settle(resultJson);
}

function stopGyroscope(options) {
    return wrapAsync('stopGyroscope', function () {
        op_stop_gyroscope();
    }, options);
}

export {
    onGyroscopeChange,
    offGyroscopeChange,
    _internalTriggerGyroscopeChange,
    startGyroscope,
    _internalOnStartGyroscopeResult,
    stopGyroscope,
};
