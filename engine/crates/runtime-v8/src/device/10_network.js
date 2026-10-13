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
let _lastWeakNet = false;

function _updateMonitoring() {
    const shouldMonitor = _listeners.size() > 0 || _weakNetListeners.size() > 0;
    if (shouldMonitor && !_isMonitoring) {
        try {
            op_start_network_monitoring();
            _isMonitoring = true;
            // Initialize _lastWeakNet
            try {
                const res = JSON.parse(op_get_network_type());
                _lastWeakNet = res.weakNet === true;
            } catch (e) {
                // Ignore
            }
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

function _internalTriggerNetworkStatusChange(isConnected, networkType) {
    // 1. Trigger standard network status change listeners
    if (_listeners.size() > 0) {
        _listeners.trigger({ isConnected: isConnected, networkType: networkType });
    }

    // 2. Trigger weak network change listeners
    // We fetch the latest status to get weakNet field which is not passed in the event arguments
    if (_weakNetListeners.size() > 0 || _isMonitoring) {
        try {
            const res = JSON.parse(op_get_network_type());
            // Ensure we handle potential error response structure if op fails silently or returns error obj
            if (res && !res._error) {
                const currentWeakNet = res.weakNet === true;
                
                if (currentWeakNet !== _lastWeakNet) {
                    _lastWeakNet = currentWeakNet;
                    
                    if (_weakNetListeners.size() > 0) {
                        const weakData = {
                            weakNet: currentWeakNet, 
                            networkType: res.networkType 
                        };
                        _weakNetListeners.trigger(weakData);
                    }
                }
            }
        } catch (e) {
            console.error('Error checking weak net status:', e);
        }
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
