import {
    op_create_ble_peripheral_server,
    op_ble_peripheral_add_service,
    op_ble_peripheral_remove_service,
    op_ble_peripheral_start_advertising,
    op_ble_peripheral_stop_advertising,
    op_ble_peripheral_write_characteristic_value,
} from "ext:core/ops";
import {
    allocateHostCallbackId,
    createDeferredApi,
    createListenerGroup,
    failMessage,
    invokeCallback,
    withCallbacks,
    withoutCallbacks,
} from "ext:host_v8_base/02_async.js";
import { bytesToHex, hexToBytes } from "ext:host_v8_system/00_host_binary.js";

// This device as a Bluetooth LE peripheral: the servers content makes, the
// services it puts on them, the advertising that makes them findable, and what
// the centrals that connect do. Every request names its server by an id from
// the Host id space, and every event the host reports names it back. Binary
// values cross as lower-case hex and reach content as ArrayBuffers, as the
// central role's do.

// The servers content holds, by id: where the host's events go.
const _servers = new Map();

// A person may be asked to turn Bluetooth on, or to allow it: making a server
// has no timeout. The rest are the host's own work.
const _createApi = createDeferredApi("createBLEPeripheralServer", 0);
const _addServiceApi = createDeferredApi("BLEPeripheralServer.addService");
const _removeServiceApi = createDeferredApi("BLEPeripheralServer.removeService");
const _startAdvertisingApi = createDeferredApi("BLEPeripheralServer.startAdvertising");
const _stopAdvertisingApi = createDeferredApi("BLEPeripheralServer.stopAdvertising");
const _writeApi = createDeferredApi("BLEPeripheralServer.writeCharacteristicValue");

const _connectionStateListeners = createListenerGroup("onBLEPeripheralConnectionStateChanged");

const POWER_LEVELS = ["low", "medium", "high"];

function required(value, name) {
    if (typeof value !== "string" || value.length === 0) throw new Error(`${name} is required`);
    return value;
}

function bytes(value, name) {
    if (!(value instanceof ArrayBuffer) && !ArrayBuffer.isView(value)) {
        throw new Error(`${name} must be an ArrayBuffer`);
    }
    return bytesToHex(value);
}

// Only the flags the reference names, each a boolean.
function flags(object, names) {
    const out = {};
    for (const name of names) out[name] = object !== null && typeof object === "object" && object[name] === true;
    return out;
}

// A service as the reference describes it, its values hex.
function service(described) {
    if (described === null || typeof described !== "object") throw new Error("service is required");
    if (!Array.isArray(described.characteristics)) throw new Error("service.characteristics is required");
    return {
        uuid: required(described.uuid, "service.uuid"),
        characteristics: described.characteristics.map((characteristic, i) => ({
            uuid: required(characteristic && characteristic.uuid, `characteristics[${i}].uuid`),
            properties: flags(characteristic.properties, ["write", "writeNoResponse", "read", "notify", "indicate"]),
            permission: flags(characteristic.permission,
                ["readable", "writeable", "readEncryptionRequired", "writeEncryptionRequired"]),
            value: characteristic.value === undefined ? "" : bytes(characteristic.value, `characteristics[${i}].value`),
            descriptors: (Array.isArray(characteristic.descriptors) ? characteristic.descriptors : []).map(
                (descriptor, j) => ({
                    uuid: required(descriptor && descriptor.uuid, `descriptors[${j}].uuid`),
                    permission: flags(descriptor.permission, ["write", "read"]),
                    value: descriptor.value === undefined ? "" : bytes(descriptor.value, `descriptors[${j}].value`),
                })),
        })),
    };
}

// A manufacturer's id: hex with 0x before it, 16 bits.
const MANUFACTURER_ID = /^0x[0-9a-fA-F]{1,4}$/;

function manufacturer(id, i) {
    if (typeof id !== "string" || !MANUFACTURER_ID.test(id)) {
        throw new Error(`manufacturerData[${i}].manufacturerId must be 0x and hex`);
    }
    return id;
}

// What to advertise, as the reference describes it: a beacon is advertised on
// its own.
function advertising(request) {
    if (request === null || typeof request !== "object") throw new Error("advertiseRequest is required");
    if (request.beacon !== undefined && (request.deviceName !== undefined || request.serviceUuids !== undefined
        || request.manufacturerData !== undefined)) {
        throw new Error("a beacon is advertised without deviceName, serviceUuids or manufacturerData");
    }
    const out = { connectable: request.connectable !== false };
    if (typeof request.deviceName === "string") out.deviceName = request.deviceName;
    if (Array.isArray(request.serviceUuids)) {
        out.serviceUuids = request.serviceUuids.map((uuid, i) => required(uuid, `serviceUuids[${i}]`));
    }
    if (Array.isArray(request.manufacturerData)) {
        out.manufacturerData = request.manufacturerData.map((data, i) => ({
            manufacturerId: manufacturer(data && data.manufacturerId, i),
            manufacturerSpecificData: data.manufacturerSpecificData === undefined
                ? "" : bytes(data.manufacturerSpecificData, `manufacturerData[${i}].manufacturerSpecificData`),
        }));
    }
    if (request.beacon !== undefined) {
        const beacon = request.beacon;
        const uint16 = (n) => Number.isInteger(n) && n >= 0 && n <= 0xffff;
        if (beacon === null || typeof beacon !== "object" || !uint16(beacon.major) || !uint16(beacon.minor)) {
            throw new Error("beacon needs uuid, and major and minor from 0 to 65535");
        }
        out.beacon = { uuid: required(beacon.uuid, "beacon.uuid"), major: beacon.major, minor: beacon.minor };
        if (typeof beacon.measuredPower === "number") out.beacon.measuredPower = beacon.measuredPower;
    }
    return out;
}

// Hands an event to a server's listeners: set by the class, reachable by
// nothing content holds.
let _dispatch;

/**
 * This device's GATT server, made by createBLEPeripheralServer: its services,
 * its advertising, and the read and write requests of the centrals that
 * connect, each answered with writeCharacteristicValue and its callbackId.
 */
class BLEPeripheralServer {
    #id;
    #listeners = {
        readRequest: createListenerGroup("BLEPeripheralServer characteristicReadRequest"),
        writeRequest: createListenerGroup("BLEPeripheralServer characteristicWriteRequest"),
        subscribed: createListenerGroup("BLEPeripheralServer characteristicSubscribed"),
        unsubscribed: createListenerGroup("BLEPeripheralServer characteristicUnsubscribed"),
    };

    static {
        _dispatch = (server, type, res) => server.#listeners[type].trigger(res);
    }

    constructor(id) {
        this.#id = id;
    }

    /** Add `service` -- {uuid, characteristics: [...]}. */
    addService(options) {
        this.#call(_addServiceApi, "BLEPeripheralServer.addService", op_ble_peripheral_add_service, options,
            (o) => ({ service: service(o.service) }));
    }

    /** Remove service `serviceId`. */
    removeService(options) {
        this.#call(_removeServiceApi, "BLEPeripheralServer.removeService", op_ble_peripheral_remove_service,
            options,
            (o) => ({ serviceId: required(o.serviceId, "serviceId") }));
    }

    /** Advertise `advertiseRequest` at `powerLevel`. */
    startAdvertising(options) {
        this.#call(_startAdvertisingApi, "BLEPeripheralServer.startAdvertising",
            op_ble_peripheral_start_advertising, options, (o) => ({
            advertiseRequest: advertising(o.advertiseRequest),
            powerLevel: POWER_LEVELS.includes(o.powerLevel) ? o.powerLevel : "medium",
        }));
    }

    /** Stop advertising. */
    stopAdvertising(options) {
        this.#call(_stopAdvertisingApi, "BLEPeripheralServer.stopAdvertising",
            op_ble_peripheral_stop_advertising, options, () => ({}));
    }

    /**
     * Set a characteristic's value, notifying subscribed centrals when
     * `needNotify`; with `callbackId`, it answers that read or write request.
     */
    writeCharacteristicValue(options) {
        this.#call(_writeApi, "BLEPeripheralServer.writeCharacteristicValue",
            op_ble_peripheral_write_characteristic_value, options, (o) => {
            if (typeof o.needNotify !== "boolean") throw new Error("needNotify is required");
            const request = {
                serviceId: required(o.serviceId, "serviceId"),
                characteristicId: required(o.characteristicId, "characteristicId"),
                value: bytes(o.value, "value"),
                needNotify: o.needNotify,
            };
            if (o.callbackId !== undefined) {
                if (!Number.isInteger(o.callbackId)) throw new Error("callbackId must be an integer");
                request.callbackId = o.callbackId;
            }
            return request;
        });
    }

    onCharacteristicReadRequest(listener) { this.#listeners.readRequest.on(listener); }
    offCharacteristicReadRequest(listener) { this.#listeners.readRequest.off(listener); }
    onCharacteristicWriteRequest(listener) { this.#listeners.writeRequest.on(listener); }
    offCharacteristicWriteRequest(listener) { this.#listeners.writeRequest.off(listener); }
    onCharacteristicSubscribed(listener) { this.#listeners.subscribed.on(listener); }
    offCharacteristicSubscribed(listener) { this.#listeners.subscribed.off(listener); }
    onCharacteristicUnsubscribed(listener) { this.#listeners.unsubscribed.on(listener); }
    offCharacteristicUnsubscribed(listener) { this.#listeners.unsubscribed.off(listener); }

    // A request of this server, answered through `api`'s hook: the reference
    // gives these callbacks, not a promise. A request content described wrong
    // fails at once, with why.
    #call(api, name, op, options, describe) {
        const opts = options || {};
        let request;
        try {
            request = describe(opts);
        } catch (e) {
            const res = { errMsg: failMessage(name, e) };
            invokeCallback(name, "fail", opts.fail, res);
            invokeCallback(name, "complete", opts.complete, res);
            return;
        }
        request.serverId = this.#id;
        const settled = api.invoke(opts, (_, requestId) => {
            request.requestId = requestId;
            op(JSON.stringify(request));
        });
        // The reference gives these no Promise style: called without callbacks,
        // a failure content never asked to hear of is not an unhandled one.
        if (settled !== undefined) settled.catch(() => {});
    }
}

/**
 * Make this device's GATT server. Its success is {server}; content may also
 * await it.
 */
function createBLEPeripheralServer(options) {
    const opts = options || {};
    let serverId;
    try {
        serverId = allocateHostCallbackId();
    } catch (e) {
        return withCallbacks("createBLEPeripheralServer",
            Promise.reject({ errMsg: failMessage("createBLEPeripheralServer", e) }), opts);
    }
    const server = new BLEPeripheralServer(serverId);
    _servers.set(serverId, server);
    const made = _createApi.invoke(withoutCallbacks(opts), (_, requestId) => {
        op_create_ble_peripheral_server(JSON.stringify({ serverId, requestId }));
    }, (result) => {
        // The server content gets in its success.
        result.server = server;
        return result;
    });
    // A server not made is gone: nothing more reaches it.
    made.then(undefined, () => { _servers.delete(serverId); });
    return withCallbacks("createBLEPeripheralServer", made, opts);
}

function onBLEPeripheralConnectionStateChanged(listener) { _connectionStateListeners.on(listener); }
function offBLEPeripheralConnectionStateChanged(listener) { _connectionStateListeners.off(listener); }

// ==================== The host's answers and events ====================

function _internalOnCreateBLEPeripheralServerResult(json) { _createApi.settle(json); }
function _internalOnBLEPeripheralAddServiceResult(json) { _addServiceApi.settle(json); }
function _internalOnBLEPeripheralRemoveServiceResult(json) { _removeServiceApi.settle(json); }
function _internalOnBLEPeripheralStartAdvertisingResult(json) { _startAdvertisingApi.settle(json); }
function _internalOnBLEPeripheralStopAdvertisingResult(json) { _stopAdvertisingApi.settle(json); }
function _internalOnBLEPeripheralWriteCharacteristicValueResult(json) { _writeApi.settle(json); }

// An event's server and fields, if it is a server content holds.
function _event(json) {
    let event;
    try {
        event = JSON.parse(json);
    } catch (_) {
        return null;
    }
    const server = event && typeof event === "object" ? _servers.get(event.serverId) : undefined;
    return server === undefined ? null : [server, event];
}

function _characteristicEvent(type, withRequest, withValue) {
    return (json) => {
        const found = _event(json);
        if (!found) return;
        const event = found[1];
        const res = { serviceId: String(event.serviceId), characteristicId: String(event.characteristicId) };
        if (withRequest) res.callbackId = Number(event.callbackId);
        if (withValue) res.value = hexToBytes(event.value) || new ArrayBuffer(0);
        _dispatch(found[0], type, res);
    };
}

const _internalOnBLEPeripheralCharacteristicReadRequestEvent = _characteristicEvent("readRequest", true, false);
const _internalOnBLEPeripheralCharacteristicWriteRequestEvent = _characteristicEvent("writeRequest", true, true);
const _internalOnBLEPeripheralCharacteristicSubscribedEvent = _characteristicEvent("subscribed", false, false);
const _internalOnBLEPeripheralCharacteristicUnsubscribedEvent = _characteristicEvent("unsubscribed", false, false);

function _internalOnBLEPeripheralConnectionStateChangedEvent(json) {
    let event;
    try {
        event = JSON.parse(json);
    } catch (_) {
        return;
    }
    if (!event || !_servers.has(event.serverId)) return;
    _connectionStateListeners.trigger({
        deviceId: String(event.deviceId),
        serverId: String(event.serverId),
        connected: event.connected === true,
    });
}

export {
    BLEPeripheralServer,
    createBLEPeripheralServer,
    onBLEPeripheralConnectionStateChanged,
    offBLEPeripheralConnectionStateChanged,
    _internalOnCreateBLEPeripheralServerResult,
    _internalOnBLEPeripheralAddServiceResult,
    _internalOnBLEPeripheralRemoveServiceResult,
    _internalOnBLEPeripheralStartAdvertisingResult,
    _internalOnBLEPeripheralStopAdvertisingResult,
    _internalOnBLEPeripheralWriteCharacteristicValueResult,
    _internalOnBLEPeripheralCharacteristicReadRequestEvent,
    _internalOnBLEPeripheralCharacteristicWriteRequestEvent,
    _internalOnBLEPeripheralCharacteristicSubscribedEvent,
    _internalOnBLEPeripheralCharacteristicUnsubscribedEvent,
    _internalOnBLEPeripheralConnectionStateChangedEvent,
};
