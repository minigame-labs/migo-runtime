//! `createBLEPeripheralServer`: this device as a Bluetooth LE peripheral.

use crate::protocol::error::ServiceError;

/// This device as a Bluetooth LE peripheral: GATT servers content builds, the
/// advertising that makes them findable, and the centrals that connect to them.
///
/// Every method takes the request content made as JSON -- the server's
/// `serverId`, which content takes from the Host id space, the method's
/// options, and `requestId` for a call -- and every call is answered through
/// the hook [`BLE_PERIPHERAL_RESULT_HOOKS`] names for its method number, with
/// `{}` or `{"error"}`, once what it asked has happened. Binary values cross as
/// lower-case hex, as the central role's do.
///
/// What centrals do reaches content through [`BLE_PERIPHERAL_EVENT_HOOKS`]: a
/// read or write request carries a `callbackId` the host made, which content
/// answers with `write_characteristic_value`. A request content does not answer
/// gets no response, as the reference says; the host answers only what content
/// never sees -- the rest of a long read from the value content gave, a long
/// write's parts until it is executed, descriptors. The reference gives a
/// server no way to close: a host closes its servers with the session.
pub trait BlePeripheralService: Send + Sync {
    /// Open GATT server `serverId`. A call, answered once it serves.
    fn create_server(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "createBLEPeripheralServer:fail not supported",
        ))
    }

    /// Add `service` -- `{uuid, characteristics: [{uuid, properties,
    /// permission, value, descriptors: [{uuid, permission, value}]}]}`, values
    /// hex. A call, answered once it is served.
    fn add_service(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "BLEPeripheralServer.addService:fail not supported",
        ))
    }

    /// Remove service `serviceId`. A call, answered once removed.
    fn remove_service(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "BLEPeripheralServer.removeService:fail not supported",
        ))
    }

    /// Advertise `advertiseRequest` -- `{connectable, deviceName,
    /// serviceUuids, manufacturerData: [{manufacturerId,
    /// manufacturerSpecificData}], beacon: {uuid, major, minor,
    /// measuredPower}}` -- at `powerLevel` ("low", "medium", "high"). A call,
    /// answered once advertising.
    fn start_advertising(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "BLEPeripheralServer.startAdvertising:fail not supported",
        ))
    }

    /// Stop advertising. A call, answered once stopped.
    fn stop_advertising(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "BLEPeripheralServer.stopAdvertising:fail not supported",
        ))
    }

    /// Set characteristic `characteristicId` of service `serviceId` to `value`
    /// (hex), notifying subscribed centrals when `needNotify`; with
    /// `callbackId`, it is also the answer to that read or write request. A
    /// call, answered once set.
    fn write_characteristic_value(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "BLEPeripheralServer.writeCharacteristicValue:fail not supported",
        ))
    }
}

/// The peripheral service's methods by number (`MIGO_BLE_PERIPHERAL_*` in
/// `include/migo/host_services.h`).
pub mod ble_peripheral_method {
    pub const CREATE_SERVER: u32 = 0;
    pub const ADD_SERVICE: u32 = 1;
    pub const REMOVE_SERVICE: u32 = 2;
    pub const START_ADVERTISING: u32 = 3;
    pub const STOP_ADVERTISING: u32 = 4;
    pub const WRITE_CHARACTERISTIC_VALUE: u32 = 5;
}

/// The hook each peripheral call is answered through, indexed by its method
/// number -- every method is a call. One table for every SDK.
pub const BLE_PERIPHERAL_RESULT_HOOKS: [&str; 6] = [
    "_internalOnCreateBLEPeripheralServerResult",
    "_internalOnBLEPeripheralAddServiceResult",
    "_internalOnBLEPeripheralRemoveServiceResult",
    "_internalOnBLEPeripheralStartAdvertisingResult",
    "_internalOnBLEPeripheralStopAdvertisingResult",
    "_internalOnBLEPeripheralWriteCharacteristicValueResult",
];

/// The peripheral service's events by number (`MIGO_BLE_PERIPHERAL_EVENT_*`).
pub mod ble_peripheral_event {
    pub const CHARACTERISTIC_READ_REQUEST: u32 = 0;
    pub const CHARACTERISTIC_WRITE_REQUEST: u32 = 1;
    pub const CHARACTERISTIC_SUBSCRIBED: u32 = 2;
    pub const CHARACTERISTIC_UNSUBSCRIBED: u32 = 3;
    pub const CONNECTION_STATE_CHANGED: u32 = 4;
}

/// The hook each peripheral event reaches content through, indexed by its
/// event number: the four characteristic events with `{serverId, serviceId,
/// characteristicId}` -- a read request with `callbackId`, a write request with
/// `callbackId` and `value` -- and CONNECTION_STATE_CHANGED with `{deviceId,
/// serverId, connected}`.
pub const BLE_PERIPHERAL_EVENT_HOOKS: [&str; 5] = [
    "_internalOnBLEPeripheralCharacteristicReadRequestEvent",
    "_internalOnBLEPeripheralCharacteristicWriteRequestEvent",
    "_internalOnBLEPeripheralCharacteristicSubscribedEvent",
    "_internalOnBLEPeripheralCharacteristicUnsubscribedEvent",
    "_internalOnBLEPeripheralConnectionStateChangedEvent",
];
