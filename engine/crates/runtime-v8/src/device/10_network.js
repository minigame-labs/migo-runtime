import {
    op_start_network_monitoring,
    op_stop_network_monitoring,
    op_get_network_type,
    op_get_local_ip_address,
} from "ext:core/ops";
import { wrapAsync, createListenerGroup } from "ext:host_v8_base/02_async.js";

const _listeners = createListenerGroup('onNetworkStatusChange');
const _weakNetListeners = createListenerGroup('onNetworkWeakChange');
let _isMonitoring = false;
// What content's listeners were last told, or held when they began listening: a
// report tells each group only of a change in what that group hears about.
let _lastStatus = null;
let _lastWeakNet = false;

// The host's last report, or null when there is none to read.
function _report() {
    try {
        const res = JSON.parse(op_get_network_type());
        return res && !res._error ? res : null;
    } catch (e) {
        return null;
    }
}

function _updateMonitoring() {
    const shouldMonitor = _listeners.size() > 0 || _weakNetListeners.size() > 0;
    if (shouldMonitor && !_isMonitoring) {
        try {
            op_start_network_monitoring();
            _isMonitoring = true;
            const res = _report();
            _lastStatus = res ? { isConnected: res.isConnected, networkType: res.networkType } : null;
            _lastWeakNet = res ? res.weakNet === true : false;
        } catch (e) {
            console.error('Failed to start network monitoring:', e);
        }
    } else if (!shouldMonitor && _isMonitoring) {
        try {
            op_stop_network_monitoring();
            _isMonitoring = false;
        } catch (e) {
            // Ignore stop errors
        }
    }
}

function onNetworkStatusChange(listener) {
    if (typeof listener === 'function') {
        _listeners.on(listener);
        _updateMonitoring();
    }
}

function offNetworkStatusChange(listener) {
    _listeners.off(listener);
    _updateMonitoring();
}

function onNetworkWeakChange(listener) {
    if (typeof listener === 'function') {
        _weakNetListeners.on(listener);
        _updateMonitoring();
    }
}

function offNetworkWeakChange(listener) {
    _weakNetListeners.off(listener);
    _updateMonitoring();
}

// The host's report changed. Its listeners hear what changed for them: a status
// change when the connection or its type did, a weak-network change when the host's
// judgement did. Every report used to reach the status listeners, changed or not, and
// a change in the judgement alone reached nobody.
function _internalTriggerNetworkStatusChange(isConnected, networkType) {
    const res = _report();
    const statusChanged = _lastStatus === null ||
        _lastStatus.isConnected !== isConnected || _lastStatus.networkType !== networkType;
    _lastStatus = { isConnected: isConnected, networkType: networkType };
    if (statusChanged) {
        _listeners.trigger({ isConnected: isConnected, networkType: networkType });
    }
    const weakNet = res ? res.weakNet === true : _lastWeakNet;
    if (weakNet !== _lastWeakNet) {
        _lastWeakNet = weakNet;
        _weakNetListeners.trigger({ weakNet: weakNet, networkType: networkType });
    }
}

function getNetworkType(options) {
    return wrapAsync('getNetworkType', function () {
        const res = JSON.parse(op_get_network_type());
        if (res && res._error) {
            let msg = res._error.errMsg || 'unknown error';
            msg = msg.replace(/^getNetworkType:fail:?/, '').trim();
            throw new Error(msg);
        }
        return res;
    }, options);
}

// {localip, netmask}: the engine reads them from the system on every platform.
function getLocalIPAddress(options) {
    return wrapAsync('getLocalIPAddress', function () {
        return JSON.parse(op_get_local_ip_address());
    }, options);
}

export {
    onNetworkStatusChange,
    offNetworkStatusChange,
    onNetworkWeakChange,
    offNetworkWeakChange,
    _internalTriggerNetworkStatusChange,
    getNetworkType,
    getLocalIPAddress,
};
