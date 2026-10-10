import { op_start_compass, op_stop_compass } from "ext:core/ops";
import { wrapAsync, createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

const _grp = createListenerGroup('onCompassChange');

function onCompassChange(listener) { _grp.on(listener); }
function offCompassChange(listener) { _grp.off(listener); }

function _internalTriggerCompassChange(direction, accuracy) {
    _grp.trigger({ direction: direction, accuracy: accuracy });
}

// Starting is a request the host answers -- it fails when the device has no such
// sensor or may not use it. Stopping cannot fail.
const _startApi = createDeferredApi('startCompass');

function startCompass(options) {
    return _startApi.invoke(options, function (opts, requestId) {
        op_start_compass(JSON.stringify({ requestId: requestId }));
    });
}

function _internalOnStartCompassResult(resultJson) {
    _startApi.settle(resultJson);
}

function stopCompass(options) {
    return wrapAsync('stopCompass', function () {
        op_stop_compass();
    }, options);
}

export {
    onCompassChange,
    offCompassChange,
    _internalTriggerCompassChange,
    startCompass,
    _internalOnStartCompassResult,
    stopCompass,
};
