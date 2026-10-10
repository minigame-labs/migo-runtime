import { createListenerGroup } from "ext:host_v8_base/02_async.js";

const _listeners = createListenerGroup('onDeviceOrientationChange');

function onDeviceOrientationChange(listener) {
    _listeners.on(listener);
}

function offDeviceOrientationChange(listener) {
    _listeners.off(listener);
}

function _internalTriggerDeviceOrientationChange(value) {
    _listeners.trigger({ value });
}

// The same change reported as a host-service event, `{"value"}`.
function _internalOnDeviceOrientationEvent(eventJson) {
    var event;
    try { event = JSON.parse(eventJson); } catch (_) { return; }
    if (event !== null && typeof event === 'object' && typeof event.value === 'string') {
        _internalTriggerDeviceOrientationChange(event.value);
    }
}

export {
    onDeviceOrientationChange,
    offDeviceOrientationChange,
    _internalTriggerDeviceOrientationChange,
    _internalOnDeviceOrientationEvent,
};
