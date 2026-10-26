//! This device as a BLE peripheral, as content uses it: a server made with its
//! id from the Host id space, services and advertising described as the
//! reference describes them and handed over with their values in hex, and the
//! centrals' requests reaching the server they name -- answered with
//! writeCharacteristicValue and the request's callbackId.

use std::sync::{Arc, Mutex};

use deno_core::{FastString, JsRuntime, serde_json};
use shared::{
    protocol::error::ServiceError,
    services::{
        BlePeripheralService, CommerceServices, ConnectivityServices, DeviceServices,
        MediaServices, PermissionService, Scope, ScopeState, SensorServices, SystemUtilServices,
    },
};

use super::support::boot_with_services;

/// What the host was asked, in order, each with its method.
#[derive(Default)]
struct FakePeripheral(Mutex<Vec<serde_json::Value>>);

impl FakePeripheral {
    fn record(&self, method: &str, request_json: &str) -> Result<(), ServiceError> {
        let mut request: serde_json::Value = serde_json::from_str(request_json).unwrap();
        request["method"] = method.into();
        self.0.lock().unwrap().push(request);
        Ok(())
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.0.lock().unwrap().clone()
    }
}

impl BlePeripheralService for FakePeripheral {
    fn create_server(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("create_server", request_json)
    }
    fn add_service(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("add_service", request_json)
    }
    fn start_advertising(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("start_advertising", request_json)
    }
    fn write_characteristic_value(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("write_characteristic_value", request_json)
    }
}

struct Granted;

impl PermissionService for Granted {
    fn scope_state(&self, _scope: Scope) -> ScopeState {
        ScopeState::Granted
    }
}

struct Host(Arc<FakePeripheral>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        Some(Arc::new(Granted))
    }
}
impl ConnectivityServices for Host {
    fn ble_peripheral(&self) -> Option<Arc<dyn BlePeripheralService>> {
        Some(self.0.clone())
    }
}

fn boot(peripheral: Arc<FakePeripheral>) -> JsRuntime {
    boot_with_services(
        Some(Arc::new(Host(peripheral)) as Arc<dyn DeviceServices>),
        None,
    )
}

/// Run `source` where timers can be armed, without waiting for them: the host
/// here answers nothing on its own, so a call's timeout is still pending when
/// the script ends. Callbacks and listeners run as the host's hooks are
/// dispatched. Returns `check`, evaluated after.
fn run(runtime: &mut JsRuntime, source: &str, check: &str) -> String {
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _context = reactor.enter();
    runtime
        .execute_script("<test:run>", FastString::from(source.to_owned()))
        .expect("test script");
    runtime
        .perform_microtask_checkpoint()
        .expect("host task checkpoint");
    let value = runtime
        .execute_script("<test:check>", FastString::from(check.to_owned()))
        .expect("test check");
    deno_core::scope!(scope, runtime);
    let local = deno_core::v8::Local::new(scope, value);
    local.to_rust_string_lossy(scope)
}

fn dispatch(hook: &str, fields: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('{hook}', \
         JSON.stringify([JSON.stringify({fields})]));"
    )
}

#[test]
fn a_server_is_described_in_the_references_terms_and_hears_its_centrals() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let peripheral = Arc::new(FakePeripheral::default());
    let mut runtime = boot(peripheral.clone());
    run(
        &mut runtime,
        "globalThis.__heard = []; \
         migo.onBLEPeripheralConnectionStateChanged((res) => __heard.push('conn ' + JSON.stringify(res))); \
         migo.createBLEPeripheralServer({ success: (res) => { globalThis.__server = res.server; \
             __heard.push('made ' + res.errMsg + ' ' + (res.server instanceof Object)); } });",
        "0",
    );
    let created = peripheral.requests();
    assert_eq!(created.len(), 1, "{created:?}");
    let server_id = created[0]["serverId"].clone();
    run(
        &mut runtime,
        &dispatch(
            "_internalOnCreateBLEPeripheralServerResult",
            &format!("{{ requestId: {} }}", created[0]["requestId"]),
        ),
        "0",
    );

    run(
        &mut runtime,
        "__server.addService({ service: { uuid: 'FFE0', characteristics: [{ uuid: 'FFE1', \
             properties: { read: true, notify: true, bogus: true }, permission: { readable: true }, \
             value: new Uint8Array([1, 171]).buffer, descriptors: [{ uuid: '2902', value: new ArrayBuffer(2) }] }] }, \
             fail: (res) => __heard.push(res.errMsg + ' ' + res.errCode) }); \
         __server.addService({ service: { characteristics: [] }, fail: (res) => __heard.push(res.errMsg) }); \
         __server.startAdvertising({ advertiseRequest: { deviceName: 'migo', serviceUuids: ['FFE0'], \
             manufacturerData: [{ manufacturerId: '0x004C', manufacturerSpecificData: new Uint8Array([2]).buffer }] }, \
             powerLevel: 'loud' }); \
         __server.onCharacteristicReadRequest((res) => __heard.push('read ' + JSON.stringify(res))); \
         __server.onCharacteristicWriteRequest((res) => __heard.push('write ' + res.callbackId + ' ' + \
             Array.from(new Uint8Array(res.value)).join(',')));",
        "0",
    );
    let requests = peripheral.requests();
    let service = &requests[1];
    assert_eq!(service["method"], "add_service");
    assert_eq!(service["serverId"], server_id);
    let characteristic = &service["service"]["characteristics"][0];
    assert_eq!(characteristic["value"], "01ab");
    assert_eq!(
        characteristic["properties"],
        serde_json::json!({ "write": false, "writeNoResponse": false, "read": true, "notify": true, "indicate": false })
    );
    assert_eq!(characteristic["descriptors"][0]["value"], "0000");
    let advertising = &requests[2];
    assert_eq!(advertising["method"], "start_advertising");
    assert_eq!(
        advertising["powerLevel"], "medium",
        "a level the reference does not have"
    );
    assert_eq!(advertising["advertiseRequest"]["connectable"], true);
    assert_eq!(
        advertising["advertiseRequest"]["manufacturerData"][0]["manufacturerSpecificData"],
        "02"
    );
    assert_eq!(
        requests.len(),
        3,
        "a service with no uuid never reached the host"
    );

    let events = [
        // A host's failure already naming the method names it once, with its code.
        dispatch(
            "_internalOnBLEPeripheralAddServiceResult",
            &format!(
                "{{ requestId: {}, error: 'BLEPeripheralServer.addService:fail permission denied', errCode: 10008 }}",
                service["requestId"]
            ),
        ),
        dispatch(
            "_internalOnBLEPeripheralCharacteristicReadRequestEvent",
            &format!("{{ serverId: {server_id}, serviceId: 'FFE0', characteristicId: 'FFE1', callbackId: 7 }}"),
        ),
        dispatch(
            "_internalOnBLEPeripheralCharacteristicWriteRequestEvent",
            &format!(
                "{{ serverId: {server_id}, serviceId: 'FFE0', characteristicId: 'FFE1', callbackId: 8, value: '0aff' }}"
            ),
        ),
        // A server content does not hold hears nothing.
        dispatch(
            "_internalOnBLEPeripheralCharacteristicReadRequestEvent",
            "{ serverId: 999999, serviceId: 'FFE0', characteristicId: 'FFE1', callbackId: 9 }",
        ),
        dispatch(
            "_internalOnBLEPeripheralConnectionStateChangedEvent",
            &format!("{{ serverId: {server_id}, deviceId: 'AA:BB', connected: true }}"),
        ),
        "__server.writeCharacteristicValue({ serviceId: 'FFE0', characteristicId: 'FFE1', \
             value: new Uint8Array([5]).buffer, needNotify: false, callbackId: 7 });"
            .to_owned(),
    ]
    .concat();
    let heard = run(&mut runtime, &events, "JSON.stringify(__heard)");
    let heard: Vec<String> = serde_json::from_str(&heard).unwrap();
    assert_eq!(
        heard,
        [
            "made createBLEPeripheralServer:ok true".to_owned(),
            "BLEPeripheralServer.addService:fail service.uuid is required".to_owned(),
            "BLEPeripheralServer.addService:fail permission denied 10008".to_owned(),
            r#"read {"serviceId":"FFE0","characteristicId":"FFE1","callbackId":7}"#.to_owned(),
            "write 8 10,255".to_owned(),
            format!(r#"conn {{"deviceId":"AA:BB","serverId":"{server_id}","connected":true}}"#),
        ]
    );
    let answer = &peripheral.requests()[3];
    assert_eq!(answer["method"], "write_characteristic_value");
    assert_eq!(answer["callbackId"], 7);
    assert_eq!(answer["value"], "05");
    assert_eq!(answer["needNotify"], false);
}
