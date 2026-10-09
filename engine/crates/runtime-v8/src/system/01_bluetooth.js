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
import { wrapAsync, createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

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

// ==================== Bluetooth Adapter APIs ====================

function openBluetoothAdapter(options = {}) {
    const { mode = 'central' } = options;
    return wrapAsync('openBluetoothAdapter', function () {
        op_open_bluetooth_adapter(JSON.stringify({ mode }));
    }, options);
}

function closeBluetoothAdapter(options = {}) {
    return wrapAsync('closeBluetoothAdapter', function () {
        op_close_bluetooth_adapter();
    }, options);
}

function getBluetoothAdapterState(options = {}) {
    return wrapAsync('getBluetoothAdapterState', function () {
        const json = op_get_bluetooth_adapter_state();
        return JSON.parse(json);
    }, options);
}

// ==================== Device Discovery APIs ====================

var _discoveryTimer = null;

function startBluetoothDevicesDiscovery(options = {}) {
    const { services, allowDuplicatesKey = false, interval = 0, powerLevel = 'medium' } = options;
    return wrapAsync('startBluetoothDevicesDiscovery', function () {
        op_start_bluetooth_devices_discovery(JSON.stringify({
            services: services || [],
            allowDuplicatesKey,
            interval,
            powerLevel,
        }));
        // Auto-stop scanning after 30 seconds to prevent battery drain.
        if (_discoveryTimer !== null) {
            clearTimeout(_discoveryTimer);
        }
        _discoveryTimer = setTimeout(function () {
            _discoveryTimer = null;
            try { op_stop_bluetooth_devices_discovery(); } catch (_) {}
        }, 30000);
    }, options);
}

function stopBluetoothDevicesDiscovery(options = {}) {
    return wrapAsync('stopBluetoothDevicesDiscovery', function () {
        if (_discoveryTimer !== null) {
            clearTimeout(_discoveryTimer);
            _discoveryTimer = null;
        }
        op_stop_bluetooth_devices_discovery();
    }, options);
}

function getBluetoothDevices(options = {}) {
    return wrapAsync('getBluetoothDevices', function () {
        const json = op_get_bluetooth_devices();
        return JSON.parse(json);
    }, options);
}

function getConnectedBluetoothDevices(options = {}) {
    const { services } = options;
    return wrapAsync('getConnectedBluetoothDevices', function () {
        const json = op_get_connected_bluetooth_devices(JSON.stringify({
            services: services || [],
        }));
        return JSON.parse(json);
    }, options);
}

// ==================== Pairing APIs ====================

function makeBluetoothPair(options = {}) {
    const { deviceId, pin, timeout = 20000 } = options;
    return wrapAsync('makeBluetoothPair', function () {
        op_make_bluetooth_pair(JSON.stringify({ deviceId, pin, timeout }));
    }, options);
}

function isBluetoothDevicePaired(options = {}) {
    const { deviceId } = options;
    return wrapAsync('isBluetoothDevicePaired', function () {
        op_is_bluetooth_device_paired(JSON.stringify({ deviceId }));
    }, options);
}

// ==================== Bluetooth Event Listeners ====================

const _adapterStateChangeListeners = createListenerGroup('onBluetoothAdapterStateChange');
const _deviceFoundListeners = createListenerGroup('onBluetoothDeviceFound');

function onBluetoothAdapterStateChange(listener) {
    _adapterStateChangeListeners.on(listener);
}

function offBluetoothAdapterStateChange(listener) {
    _adapterStateChangeListeners.off(listener);
}

function onBluetoothDeviceFound(listener) {
    _deviceFoundListeners.on(listener);
}

function offBluetoothDeviceFound(listener) {
    _deviceFoundListeners.off(listener);
}

// ==================== Internal Trigger Functions ====================

function _internalTriggerBluetoothAdapterStateChange(available, discovering) {
    _adapterStateChangeListeners.trigger({ available, discovering });
}

function _internalTriggerBluetoothDeviceFound(devicesJson) {
    try {
        _deviceFoundListeners.trigger({ devices: JSON.parse(devicesJson) });
    } catch (e) {
        console.error('Failed to parse bluetooth device data:', e);
    }
}

// ==================== Beacon APIs ====================

function startBeaconDiscovery(options = {}) {
    const { uuids, ignoreBluetoothAvailable = false } = options;
    return wrapAsync('startBeaconDiscovery', function () {
        op_start_beacon_discovery(JSON.stringify({
            uuids: uuids || [],
            ignoreBluetoothAvailable,
        }));
    }, options);
}

function stopBeaconDiscovery(options = {}) {
    return wrapAsync('stopBeaconDiscovery', function () {
        op_stop_beacon_discovery();
    }, options);
}

function getBeacons(options = {}) {
    return wrapAsync('getBeacons', function () {
        const json = op_get_beacons();
        return JSON.parse(json);
    }, options);
}

// ==================== Beacon Event Listeners ====================

const _beaconUpdateListeners = createListenerGroup('onBeaconUpdate');
const _beaconServiceChangeListeners = createListenerGroup('onBeaconServiceChange');

function onBeaconUpdate(listener) {
    _beaconUpdateListeners.on(listener);
}

function offBeaconUpdate(listener) {
    _beaconUpdateListeners.off(listener);
}

function onBeaconServiceChange(listener) {
    _beaconServiceChangeListeners.on(listener);
}

function offBeaconServiceChange(listener) {
    _beaconServiceChangeListeners.off(listener);
}

// ==================== Beacon Internal Trigger Functions ====================

function _internalTriggerBeaconUpdate(beaconsJson) {
    try {
        _beaconUpdateListeners.trigger({ beacons: JSON.parse(beaconsJson) });
    } catch (e) {
        console.error('Failed to parse beacon data:', e);
    }
}

function _internalTriggerBeaconServiceChange(available, discovering) {
    _beaconServiceChangeListeners.trigger({ available, discovering });
}

// ==================== BLE GATT Connection APIs ====================

function createBLEConnection(options = {}) {
    const { deviceId, timeout } = options;
    return wrapAsync('createBLEConnection', function () {
        op_create_ble_connection(JSON.stringify({
            deviceId: deviceId || '',
            timeout: timeout !== undefined ? timeout : 0,
        }));
    }, options);
}

function closeBLEConnection(options = {}) {
    const { deviceId } = options;
    return wrapAsync('closeBLEConnection', function () {
        op_close_ble_connection(JSON.stringify({
            deviceId: deviceId || '',
        }));
    }, options);
}

// ==================== BLE GATT Service/Characteristic APIs ====================

function getBLEDeviceServices(options = {}) {
    const { deviceId } = options;
    return wrapAsync('getBLEDeviceServices', function () {
        const json = op_get_ble_device_services(JSON.stringify({
            deviceId: deviceId || '',
        }));
        return JSON.parse(json);
    }, options);
}

function getBLEDeviceCharacteristics(options = {}) {
    const { deviceId, serviceId } = options;
    return wrapAsync('getBLEDeviceCharacteristics', function () {
        const json = op_get_ble_device_characteristics(JSON.stringify({
            deviceId: deviceId || '',
            serviceId: serviceId || '',
        }));
        return JSON.parse(json);
    }, options);
}

function readBLECharacteristicValue(options = {}) {
    const { deviceId, serviceId, characteristicId } = options;
    return wrapAsync('readBLECharacteristicValue', function () {
        op_read_ble_characteristic_value(JSON.stringify({
            deviceId: deviceId || '',
            serviceId: serviceId || '',
            characteristicId: characteristicId || '',
        }));
    }, options);
}

function _bufferToHex(buf) {
    if (!buf) return '';
    var bytes;
    if (buf instanceof ArrayBuffer) {
        bytes = new Uint8Array(buf);
    } else if (ArrayBuffer.isView(buf)) {
        bytes = new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
    } else if (typeof buf === 'string') {
        return buf; // already hex string
    } else {
        return '';
    }
    var hex = '';
    for (var i = 0; i < bytes.length; i++) {
        var b = bytes[i].toString(16);
        hex += b.length < 2 ? '0' + b : b;
    }
    return hex;
}

function writeBLECharacteristicValue(options = {}) {
    const { deviceId, serviceId, characteristicId, value, writeType } = options;
    return wrapAsync('writeBLECharacteristicValue', function () {
        op_write_ble_characteristic_value(JSON.stringify({
            deviceId: deviceId || '',
            serviceId: serviceId || '',
            characteristicId: characteristicId || '',
            value: _bufferToHex(value),
            writeType: writeType || 'write',
        }));
    }, options);
}

function notifyBLECharacteristicValueChange(options = {}) {
    const { deviceId, serviceId, characteristicId, state } = options;
    return wrapAsync('notifyBLECharacteristicValueChange', function () {
        op_notify_ble_characteristic_value_change(JSON.stringify({
            deviceId: deviceId || '',
            serviceId: serviceId || '',
            characteristicId: characteristicId || '',
            state: state !== undefined ? state : false,
        }));
    }, options);
}

// ==================== BLE RSSI / MTU APIs ====================

function getBLEDeviceRSSI(options = {}) {
    const { deviceId } = options;
    return wrapAsync('getBLEDeviceRSSI', function () {
        const json = op_get_ble_device_rssi(JSON.stringify({
            deviceId: deviceId || '',
        }));
        return JSON.parse(json);
    }, options);
}

function setBLEMTU(options = {}) {
    const { deviceId, mtu } = options;
    return wrapAsync('setBLEMTU', function () {
        op_set_ble_mtu(JSON.stringify({
            deviceId: deviceId || '',
            mtu: mtu !== undefined ? mtu : 23,
        }));
    }, options);
}

function getBLEMTU(options = {}) {
    const { deviceId } = options;
    return wrapAsync('getBLEMTU', function () {
        const json = op_get_ble_mtu(JSON.stringify({
            deviceId: deviceId || '',
        }));
        return JSON.parse(json);
    }, options);
}

// ==================== BLE GATT Event Listeners ====================

const _bleConnectionStateChangeListeners = createListenerGroup('onBLEConnectionStateChange');
const _bleCharacteristicValueChangeListeners = createListenerGroup('onBLECharacteristicValueChange');
const _bleMTUChangeListeners = createListenerGroup('onBLEMTUChange');

function onBLEConnectionStateChange(listener) {
    _bleConnectionStateChangeListeners.on(listener);
}

function offBLEConnectionStateChange(listener) {
    _bleConnectionStateChangeListeners.off(listener);
}

function onBLECharacteristicValueChange(listener) {
    _bleCharacteristicValueChangeListeners.on(listener);
}

function offBLECharacteristicValueChange(listener) {
    _bleCharacteristicValueChangeListeners.off(listener);
}

function onBLEMTUChange(listener) {
    _bleMTUChangeListeners.on(listener);
}

function offBLEMTUChange(listener) {
    _bleMTUChangeListeners.off(listener);
}

// ==================== BLE GATT Internal Trigger Functions ====================

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

export {
    // System bluetooth setting
    openSystemBluetoothSetting,
    _internalOnOpenBluetoothSettingResult,
    // Bluetooth adapter
    openBluetoothAdapter,
    closeBluetoothAdapter,
    getBluetoothAdapterState,
    // Device discovery
    startBluetoothDevicesDiscovery,
    stopBluetoothDevicesDiscovery,
    getBluetoothDevices,
    getConnectedBluetoothDevices,
    // Pairing
    makeBluetoothPair,
    isBluetoothDevicePaired,
    // Bluetooth events
    onBluetoothAdapterStateChange,
    offBluetoothAdapterStateChange,
    onBluetoothDeviceFound,
    offBluetoothDeviceFound,
    _internalTriggerBluetoothAdapterStateChange,
    _internalTriggerBluetoothDeviceFound,
    // Beacon
    startBeaconDiscovery,
    stopBeaconDiscovery,
    getBeacons,
    onBeaconUpdate,
    offBeaconUpdate,
    onBeaconServiceChange,
    offBeaconServiceChange,
    _internalTriggerBeaconUpdate,
    _internalTriggerBeaconServiceChange,
    // BLE GATT
    createBLEConnection,
    closeBLEConnection,
    getBLEDeviceServices,
    getBLEDeviceCharacteristics,
    readBLECharacteristicValue,
    writeBLECharacteristicValue,
    notifyBLECharacteristicValueChange,
    getBLEDeviceRSSI,
    setBLEMTU,
    getBLEMTU,
    // BLE GATT events
    onBLEConnectionStateChange,
    offBLEConnectionStateChange,
    onBLECharacteristicValueChange,
    offBLECharacteristicValueChange,
    onBLEMTUChange,
    offBLEMTUChange,
    _internalTriggerBLEConnectionStateChange,
    _internalTriggerBLECharacteristicValueChange,
    _internalTriggerBLEMTUChange,
};
