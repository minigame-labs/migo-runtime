import {
    op_open_system_bluetooth_setting,
    op_open_bluetooth_adapter,
    op_close_bluetooth_adapter,
    op_get_bluetooth_adapter_state,
    op_start_bluetooth_devices_discovery,
    op_stop_bluetooth_devices_discovery,
    op_get_bluetooth_devices,
    op_get_connected_bluetooth_devices,
    op_make_bluetooth_pair,
    op_is_bluetooth_device_paired,
    op_start_beacon_discovery,
    op_stop_beacon_discovery,
    op_get_beacons,
    op_create_ble_connection,
    op_close_ble_connection,
    op_get_ble_device_services,
    op_get_ble_device_characteristics,
    op_read_ble_characteristic_value,
    op_write_ble_characteristic_value,
    op_notify_ble_characteristic_value_change,
    op_get_ble_device_rssi,
    op_set_ble_mtu,
    op_get_ble_mtu,
} from "ext:core/ops";
import { createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";
import { bytesToHex, hexToBytes } from "ext:host_v8_system/00_host_binary.js";

// ==================== System Bluetooth Setting ====================

// Named for the API content calls: its errMsg is `openSystemBluetoothSetting:ok`.
const _openBluetoothSettingApi = createDeferredApi('openSystemBluetoothSetting');

function openSystemBluetoothSetting(options = {}) {
    return _openBluetoothSettingApi.invoke(options, function (opts, requestId) {
        // The result is correlated by this id and nothing else.
        op_open_system_bluetooth_setting(requestId);
    });
}

function _internalOnOpenBluetoothSettingResult(resultJson) {
    _openBluetoothSettingApi.settle(resultJson);
}

// ==================== The wire ====================
//
// Every Bluetooth operation is a request the host answers when the operation
// has happened -- a connection made, a write acknowledged -- with the result or
// with `{error, errCode}` in the platform's Bluetooth codes, which reach content
// as `errCode` beside `errMsg`. Binary values cross as lower-case hex and reach
// content as ArrayBuffers.

// A device as content sees it: its advertising data and service data as
// ArrayBuffers, the rest as the host described it.
function _device(raw) {
    var device = Object.assign({}, raw);
    if (typeof raw.advertisData === 'string') device.advertisData = hexToBytes(raw.advertisData) || new ArrayBuffer(0);
    if (raw.serviceData !== null && typeof raw.serviceData === 'object') {
        device.serviceData = {};
        var keys = Object.keys(raw.serviceData);
        for (var i = 0; i < keys.length; i++) {
            device.serviceData[keys[i]] = hexToBytes(raw.serviceData[keys[i]]) || new ArrayBuffer(0);
        }
    }
    return device;
}

function _devices(list) {
    return Array.isArray(list) ? list.map(_device) : [];
}

// One request kind: its deferred API, the op that sends it, and its result hook.
// `prepare` turns content's options into the request; `shape` turns the host's
// answer into what content receives.
function _request(apiName, timeoutMs, op, prepare, shape) {
    var api = createDeferredApi(apiName, timeoutMs);
    function call(options) {
        return api.invoke(options, function (opts, requestId) {
            var request = prepare ? prepare(opts) : {};
            request.requestId = requestId;
            op(JSON.stringify(request));
        });
    }
    function onResult(resultJson) {
        if (!shape) {
            api.settle(resultJson);
            return;
        }
        var parsed;
        try { parsed = JSON.parse(resultJson); } catch (_) { parsed = {}; }
        if (parsed !== null && typeof parsed === 'object' && !parsed.error) shape(parsed);
        api.settleParsed(parsed);
    }
    return { call: call, onResult: onResult };
}

function _required(opts, name) {
    var value = opts[name];
    if (typeof value !== 'string' || value.length === 0) throw new Error(name + ' is required');
    return value;
}

function _characteristic(opts) {
    return {
        deviceId: _required(opts, 'deviceId'),
        serviceId: _required(opts, 'serviceId'),
        characteristicId: _required(opts, 'characteristicId'),
    };
}

// The ones that wait on a person or carry their own timeout -- a permission
// prompt, a connection, pairing -- have none here.

// ==================== Adapter ====================

const _openAdapter = _request('openBluetoothAdapter', 0, op_open_bluetooth_adapter, function (o) {
    return { mode: o.mode === 'peripheral' ? 'peripheral' : 'central' };
});
const _closeAdapter = _request('closeBluetoothAdapter', undefined, op_close_bluetooth_adapter);
const _getAdapterState = _request('getBluetoothAdapterState', undefined, op_get_bluetooth_adapter_state);

// ==================== Discovery ====================

const _startDiscovery = _request('startBluetoothDevicesDiscovery', undefined,
    op_start_bluetooth_devices_discovery, function (o) {
        return {
            services: Array.isArray(o.services) ? o.services : [],
            allowDuplicatesKey: o.allowDuplicatesKey === true,
            interval: typeof o.interval === 'number' && o.interval > 0 ? o.interval : 0,
            powerLevel: o.powerLevel === 'low' || o.powerLevel === 'high' ? o.powerLevel : 'medium',
        };
    });
const _stopDiscovery = _request('stopBluetoothDevicesDiscovery', undefined,
    op_stop_bluetooth_devices_discovery);
const _getDevices = _request('getBluetoothDevices', undefined, op_get_bluetooth_devices, null,
    function (res) { res.devices = _devices(res.devices); });
const _getConnectedDevices = _request('getConnectedBluetoothDevices', undefined,
    op_get_connected_bluetooth_devices, function (o) {
        if (!Array.isArray(o.services)) throw new Error('services is required');
        return { services: o.services };
    });

// ==================== Pairing ====================

const _makePair = _request('makeBluetoothPair', 0, op_make_bluetooth_pair, function (o) {
    return {
        deviceId: _required(o, 'deviceId'),
        pin: bytesToHex(o.pin) || '',
        timeout: typeof o.timeout === 'number' && o.timeout > 0 ? o.timeout : 20000,
    };
});
const _isPaired = _request('isBluetoothDevicePaired', undefined, op_is_bluetooth_device_paired,
    function (o) { return { deviceId: _required(o, 'deviceId') }; });

// ==================== iBeacon ====================

const _startBeacons = _request('startBeaconDiscovery', undefined, op_start_beacon_discovery,
    function (o) {
        if (!Array.isArray(o.uuids) || o.uuids.length === 0) throw new Error('uuids is required');
        return { uuids: o.uuids, ignoreBluetoothAvailable: o.ignoreBluetoothAvailable === true };
    });
const _stopBeacons = _request('stopBeaconDiscovery', undefined, op_stop_beacon_discovery);
const _getBeacons = _request('getBeacons', undefined, op_get_beacons);

// ==================== BLE GATT ====================

const _createConnection = _request('createBLEConnection', 0, op_create_ble_connection,
    function (o) {
        var request = { deviceId: _required(o, 'deviceId') };
        if (typeof o.timeout === 'number' && o.timeout > 0) request.timeout = o.timeout;
        return request;
    });
const _closeConnection = _request('closeBLEConnection', undefined, op_close_ble_connection,
    function (o) { return { deviceId: _required(o, 'deviceId') }; });
const _getServices = _request('getBLEDeviceServices', undefined, op_get_ble_device_services,
    function (o) { return { deviceId: _required(o, 'deviceId') }; });
const _getCharacteristics = _request('getBLEDeviceCharacteristics', undefined,
    op_get_ble_device_characteristics, function (o) {
        return { deviceId: _required(o, 'deviceId'), serviceId: _required(o, 'serviceId') };
    });
const _readCharacteristic = _request('readBLECharacteristicValue', undefined,
    op_read_ble_characteristic_value, _characteristic);
const _writeCharacteristic = _request('writeBLECharacteristicValue', undefined,
    op_write_ble_characteristic_value, function (o) {
        var request = _characteristic(o);
        if (!(o.value instanceof ArrayBuffer) && !ArrayBuffer.isView(o.value)) {
            throw new Error('value must be an ArrayBuffer');
        }
        request.value = bytesToHex(o.value);
        request.writeType = o.writeType === 'writeNoResponse' ? 'writeNoResponse' : 'write';
        return request;
    });
const _notifyCharacteristic = _request('notifyBLECharacteristicValueChange', undefined,
    op_notify_ble_characteristic_value_change, function (o) {
        var request = _characteristic(o);
        if (typeof o.state !== 'boolean') throw new Error('state is required');
        request.state = o.state;
        request.type = o.type === 'notification' ? 'notification' : 'indication';
        return request;
    });
const _getRSSI = _request('getBLEDeviceRSSI', undefined, op_get_ble_device_rssi,
    function (o) { return { deviceId: _required(o, 'deviceId') }; });
const _setMTU = _request('setBLEMTU', undefined, op_set_ble_mtu, function (o) {
    if (typeof o.mtu !== 'number' || !(o.mtu >= 22 && o.mtu <= 512)) {
        throw new Error('mtu must be a number from 22 to 512');
    }
    return { deviceId: _required(o, 'deviceId'), mtu: o.mtu };
});
const _getMTU = _request('getBLEMTU', undefined, op_get_ble_mtu, function (o) {
    return {
        deviceId: _required(o, 'deviceId'),
        writeType: o.writeType === 'writeNoResponse' ? 'writeNoResponse' : 'write',
    };
});

// ==================== Events ====================

const _adapterStateChangeListeners = createListenerGroup('onBluetoothAdapterStateChange');
const _deviceFoundListeners = createListenerGroup('onBluetoothDeviceFound');
const _beaconUpdateListeners = createListenerGroup('onBeaconUpdate');
const _beaconServiceChangeListeners = createListenerGroup('onBeaconServiceChange');
const _bleConnectionStateChangeListeners = createListenerGroup('onBLEConnectionStateChange');
const _bleCharacteristicValueChangeListeners = createListenerGroup('onBLECharacteristicValueChange');
const _bleMTUChangeListeners = createListenerGroup('onBLEMTUChange');

function onBluetoothAdapterStateChange(listener) { _adapterStateChangeListeners.on(listener); }
function offBluetoothAdapterStateChange(listener) { _adapterStateChangeListeners.off(listener); }
function onBluetoothDeviceFound(listener) { _deviceFoundListeners.on(listener); }
function offBluetoothDeviceFound(listener) { _deviceFoundListeners.off(listener); }
function onBeaconUpdate(listener) { _beaconUpdateListeners.on(listener); }
function offBeaconUpdate(listener) { _beaconUpdateListeners.off(listener); }
function onBeaconServiceChange(listener) { _beaconServiceChangeListeners.on(listener); }
function offBeaconServiceChange(listener) { _beaconServiceChangeListeners.off(listener); }
function onBLEConnectionStateChange(listener) { _bleConnectionStateChangeListeners.on(listener); }
function offBLEConnectionStateChange(listener) { _bleConnectionStateChangeListeners.off(listener); }
function onBLECharacteristicValueChange(listener) { _bleCharacteristicValueChangeListeners.on(listener); }
function offBLECharacteristicValueChange(listener) { _bleCharacteristicValueChangeListeners.off(listener); }
function onBLEMTUChange(listener) { _bleMTUChangeListeners.on(listener); }
function offBLEMTUChange(listener) { _bleMTUChangeListeners.off(listener); }

// The typed path (Android's commands) calls these with values; the host-service
// events below call them with the JSON a C host posted.

function _internalTriggerBluetoothAdapterStateChange(available, discovering) {
    _adapterStateChangeListeners.trigger({ available: available, discovering: discovering });
}

function _internalTriggerBluetoothDeviceFound(devicesJson) {
    var devices;
    try { devices = JSON.parse(devicesJson); } catch (_) { return; }
    _deviceFoundListeners.trigger({ devices: _devices(devices) });
}

function _internalTriggerBeaconUpdate(beaconsJson) {
    var beacons;
    try { beacons = JSON.parse(beaconsJson); } catch (_) { return; }
    _beaconUpdateListeners.trigger({ beacons: Array.isArray(beacons) ? beacons : [] });
}

function _internalTriggerBeaconServiceChange(available, discovering) {
    _beaconServiceChangeListeners.trigger({ available: available, discovering: discovering });
}

function _internalTriggerBLEConnectionStateChange(deviceId, connected) {
    _bleConnectionStateChangeListeners.trigger({ deviceId: deviceId, connected: connected });
}

function _internalTriggerBLECharacteristicValueChange(deviceId, serviceId, characteristicId, value) {
    _bleCharacteristicValueChangeListeners.trigger({
        deviceId: deviceId,
        serviceId: serviceId,
        characteristicId: characteristicId,
        value: value,
    });
}

function _internalTriggerBLEMTUChange(deviceId, mtu) {
    _bleMTUChangeListeners.trigger({ deviceId: deviceId, mtu: mtu });
}

function _event(eventJson) {
    var event;
    try { event = JSON.parse(eventJson); } catch (_) { return null; }
    return event !== null && typeof event === 'object' ? event : null;
}

function _internalOnBluetoothAdapterStateEvent(eventJson) {
    var e = _event(eventJson);
    if (e) _internalTriggerBluetoothAdapterStateChange(e.available === true, e.discovering === true);
}

function _internalOnBluetoothDeviceFoundEvent(eventJson) {
    var e = _event(eventJson);
    if (e) _deviceFoundListeners.trigger({ devices: _devices(e.devices) });
}

function _internalOnBLEConnectionStateEvent(eventJson) {
    var e = _event(eventJson);
    if (e && typeof e.deviceId === 'string') {
        _internalTriggerBLEConnectionStateChange(e.deviceId, e.connected === true);
    }
}

function _internalOnBLEMTUEvent(eventJson) {
    var e = _event(eventJson);
    if (e && typeof e.deviceId === 'string' && typeof e.mtu === 'number') {
        _internalTriggerBLEMTUChange(e.deviceId, e.mtu);
    }
}

function _internalOnBeaconUpdateEvent(eventJson) {
    var e = _event(eventJson);
    if (e) _beaconUpdateListeners.trigger({ beacons: Array.isArray(e.beacons) ? e.beacons : [] });
}

function _internalOnBeaconServiceEvent(eventJson) {
    var e = _event(eventJson);
    if (e) _internalTriggerBeaconServiceChange(e.available === true, e.discovering === true);
}

// ==================== Content's API ====================

const openBluetoothAdapter = _openAdapter.call;
const _internalOnOpenBluetoothAdapterResult = _openAdapter.onResult;
const closeBluetoothAdapter = _closeAdapter.call;
const _internalOnCloseBluetoothAdapterResult = _closeAdapter.onResult;
const getBluetoothAdapterState = _getAdapterState.call;
const _internalOnGetBluetoothAdapterStateResult = _getAdapterState.onResult;
const startBluetoothDevicesDiscovery = _startDiscovery.call;
const _internalOnStartBluetoothDevicesDiscoveryResult = _startDiscovery.onResult;
const stopBluetoothDevicesDiscovery = _stopDiscovery.call;
const _internalOnStopBluetoothDevicesDiscoveryResult = _stopDiscovery.onResult;
const getBluetoothDevices = _getDevices.call;
const _internalOnGetBluetoothDevicesResult = _getDevices.onResult;
const getConnectedBluetoothDevices = _getConnectedDevices.call;
const _internalOnGetConnectedBluetoothDevicesResult = _getConnectedDevices.onResult;
const makeBluetoothPair = _makePair.call;
const _internalOnMakeBluetoothPairResult = _makePair.onResult;
const isBluetoothDevicePaired = _isPaired.call;
const _internalOnIsBluetoothDevicePairedResult = _isPaired.onResult;
const startBeaconDiscovery = _startBeacons.call;
const _internalOnStartBeaconDiscoveryResult = _startBeacons.onResult;
const stopBeaconDiscovery = _stopBeacons.call;
const _internalOnStopBeaconDiscoveryResult = _stopBeacons.onResult;
const getBeacons = _getBeacons.call;
const _internalOnGetBeaconsResult = _getBeacons.onResult;
const createBLEConnection = _createConnection.call;
const _internalOnCreateBLEConnectionResult = _createConnection.onResult;
const closeBLEConnection = _closeConnection.call;
const _internalOnCloseBLEConnectionResult = _closeConnection.onResult;
const getBLEDeviceServices = _getServices.call;
const _internalOnGetBLEDeviceServicesResult = _getServices.onResult;
const getBLEDeviceCharacteristics = _getCharacteristics.call;
const _internalOnGetBLEDeviceCharacteristicsResult = _getCharacteristics.onResult;
const readBLECharacteristicValue = _readCharacteristic.call;
const _internalOnReadBLECharacteristicValueResult = _readCharacteristic.onResult;
const writeBLECharacteristicValue = _writeCharacteristic.call;
const _internalOnWriteBLECharacteristicValueResult = _writeCharacteristic.onResult;
const notifyBLECharacteristicValueChange = _notifyCharacteristic.call;
const _internalOnNotifyBLECharacteristicValueChangeResult = _notifyCharacteristic.onResult;
const getBLEDeviceRSSI = _getRSSI.call;
const _internalOnGetBLEDeviceRSSIResult = _getRSSI.onResult;
const setBLEMTU = _setMTU.call;
const _internalOnSetBLEMTUResult = _setMTU.onResult;
const getBLEMTU = _getMTU.call;
const _internalOnGetBLEMTUResult = _getMTU.onResult;

export {
    // System bluetooth setting
    openSystemBluetoothSetting,
    _internalOnOpenBluetoothSettingResult,
    // Requests and their result hooks
    openBluetoothAdapter,
    _internalOnOpenBluetoothAdapterResult,
    closeBluetoothAdapter,
    _internalOnCloseBluetoothAdapterResult,
    getBluetoothAdapterState,
    _internalOnGetBluetoothAdapterStateResult,
    startBluetoothDevicesDiscovery,
    _internalOnStartBluetoothDevicesDiscoveryResult,
    stopBluetoothDevicesDiscovery,
    _internalOnStopBluetoothDevicesDiscoveryResult,
    getBluetoothDevices,
    _internalOnGetBluetoothDevicesResult,
    getConnectedBluetoothDevices,
    _internalOnGetConnectedBluetoothDevicesResult,
    makeBluetoothPair,
    _internalOnMakeBluetoothPairResult,
    isBluetoothDevicePaired,
    _internalOnIsBluetoothDevicePairedResult,
    startBeaconDiscovery,
    _internalOnStartBeaconDiscoveryResult,
    stopBeaconDiscovery,
    _internalOnStopBeaconDiscoveryResult,
    getBeacons,
    _internalOnGetBeaconsResult,
    createBLEConnection,
    _internalOnCreateBLEConnectionResult,
    closeBLEConnection,
    _internalOnCloseBLEConnectionResult,
    getBLEDeviceServices,
    _internalOnGetBLEDeviceServicesResult,
    getBLEDeviceCharacteristics,
    _internalOnGetBLEDeviceCharacteristicsResult,
    readBLECharacteristicValue,
    _internalOnReadBLECharacteristicValueResult,
    writeBLECharacteristicValue,
    _internalOnWriteBLECharacteristicValueResult,
    notifyBLECharacteristicValueChange,
    _internalOnNotifyBLECharacteristicValueChangeResult,
    getBLEDeviceRSSI,
    _internalOnGetBLEDeviceRSSIResult,
    setBLEMTU,
    _internalOnSetBLEMTUResult,
    getBLEMTU,
    _internalOnGetBLEMTUResult,
    // Events
    onBluetoothAdapterStateChange,
    offBluetoothAdapterStateChange,
    onBluetoothDeviceFound,
    offBluetoothDeviceFound,
    onBeaconUpdate,
    offBeaconUpdate,
    onBeaconServiceChange,
    offBeaconServiceChange,
    onBLEConnectionStateChange,
    offBLEConnectionStateChange,
    onBLECharacteristicValueChange,
    offBLECharacteristicValueChange,
    onBLEMTUChange,
    offBLEMTUChange,
    _internalTriggerBluetoothAdapterStateChange,
    _internalTriggerBluetoothDeviceFound,
    _internalTriggerBeaconUpdate,
    _internalTriggerBeaconServiceChange,
    _internalTriggerBLEConnectionStateChange,
    _internalTriggerBLECharacteristicValueChange,
    _internalTriggerBLEMTUChange,
    _internalOnBluetoothAdapterStateEvent,
    _internalOnBluetoothDeviceFoundEvent,
    _internalOnBLEConnectionStateEvent,
    _internalOnBLEMTUEvent,
    _internalOnBeaconUpdateEvent,
    _internalOnBeaconServiceEvent,
};
