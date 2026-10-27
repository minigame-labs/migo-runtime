//! getLocation and getFuzzyLocation: the host answers WGS-84 and the engine
//! does the rest -- GCJ-02 when content asked for it, and a fuzzy position
//! rounded to 0.01 degrees whatever the host's precision -- so every host gives
//! the same answer and none can forget the rounding.
//!
//! Each call passes `_timeout: 0`: the test answers between two runs of the
//! event loop, which would otherwise run the request's timeout out first.

use std::sync::{Arc, Mutex};

use deno_core::{FastString, JsRuntime, serde_json};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, LocationService, MediaServices,
        PermissionService, Scope, ScopeState, SensorServices, SystemUtilServices,
    },
};

use super::support::{SETTLE, boot_with_services, run_to_idle as run};

#[derive(Default)]
struct FakeLocation {
    requests: Mutex<Vec<(&'static str, String)>>,
}

impl LocationService for FakeLocation {
    fn get_location(&self, options_json: &str) -> Result<(), ServiceError> {
        self.requests
            .lock()
            .unwrap()
            .push(("precise", options_json.to_owned()));
        Ok(())
    }

    fn get_fuzzy_location(&self, options_json: &str) -> Result<(), ServiceError> {
        self.requests
            .lock()
            .unwrap()
            .push(("fuzzy", options_json.to_owned()));
        Ok(())
    }
}

/// scope.userLocation granted, as the call needs before it reaches the host.
struct Granted;

impl PermissionService for Granted {
    fn scope_state(&self, scope: Scope) -> ScopeState {
        if scope == Scope::UserLocation {
            ScopeState::Granted
        } else {
            ScopeState::Unknown
        }
    }
}

struct Host(Arc<FakeLocation>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl CommerceServices for Host {}
impl ConnectivityServices for Host {
    fn location(&self) -> Option<Arc<dyn LocationService>> {
        Some(self.0.clone())
    }
}
impl SystemUtilServices for Host {
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        Some(Arc::new(Granted))
    }
}

fn boot(host: Arc<FakeLocation>) -> JsRuntime {
    let mut runtime =
        boot_with_services(Some(Arc::new(Host(host)) as Arc<dyn DeviceServices>), None);
    runtime
        .execute_script("<test:location-setup>", FastString::from_static(SETTLE))
        .unwrap();
    runtime
}

/// The host's answer to `request`, through the hook its method names.
fn answer(hook: &str, request: &str, fields: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(request).unwrap();
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('{hook}', \
         JSON.stringify([JSON.stringify(Object.assign({{ requestId: {} }}, {fields}))]));",
        request["requestId"]
    )
}

/// Tiananmen in WGS-84, as a positioning system reports it.
const BEIJING: &str = "{ latitude: 39.9087, longitude: 116.3975, speed: 0, accuracy: 5 }";

fn position(result: &serde_json::Value) -> (f64, f64) {
    (
        result["res"]["latitude"].as_f64().unwrap(),
        result["res"]["longitude"].as_f64().unwrap(),
    )
}

fn assert_near(actual: (f64, f64), expected: (f64, f64)) {
    assert!(
        (actual.0 - expected.0).abs() < 1e-9 && (actual.1 - expected.1).abs() < 1e-9,
        "{actual:?} is not {expected:?}"
    );
}

#[test]
fn the_host_answers_wgs84_and_the_engine_converts_to_gcj02() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeLocation::default());
    let mut runtime = boot(host.clone());
    run(
        &mut runtime,
        "__settle('getLocation', { type: 'gcj02', altitude: true, _timeout: 0 });",
        "0",
    );
    let requests = host.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let (kind, request) = &requests[0];
    assert_eq!(*kind, "precise");
    let fields: serde_json::Value = serde_json::from_str(request).unwrap();
    // The host is not asked for a datum: it answers the one it has.
    assert!(fields.get("type").is_none(), "{request}");
    assert_eq!(fields["altitude"], true);

    let result = run(
        &mut runtime,
        &answer("_internalOnLocationResult", request, BEIJING),
        "JSON.stringify(__results.getLocation)",
    );
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["ok"], true);
    // The published transform's value for this point.
    assert_near(position(&result), (39.91010349934476, 116.40374357265176));
    // What the host measured besides the position is passed on as it was.
    assert_eq!(result["res"]["accuracy"], 5);
}

#[test]
fn wgs84_and_points_outside_china_are_the_hosts_own() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeLocation::default());
    let mut runtime = boot(host.clone());
    run(
        &mut runtime,
        "__settle('getLocation', { _timeout: 0 }); \
         globalThis.__paris = migo.getLocation({ type: 'gcj02', _timeout: 0 });",
        "0",
    );
    let requests = host.requests.lock().unwrap().clone();
    let result = run(
        &mut runtime,
        &(answer("_internalOnLocationResult", &requests[0].1, BEIJING)
            + &answer(
                "_internalOnLocationResult",
                &requests[1].1,
                "{ latitude: 48.8566, longitude: 2.3522 }",
            )
            + "__paris.then(function (res) { __results.paris = { ok: true, res: res }; });"),
        "JSON.stringify([__results.getLocation, __results.paris])",
    );
    let results: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_near(position(&results[0]), (39.9087, 116.3975));
    assert_near(position(&results[1]), (48.8566, 2.3522));
}

#[test]
fn a_fuzzy_position_is_rounded_by_the_engine_then_converted() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeLocation::default());
    let mut runtime = boot(host.clone());
    run(
        &mut runtime,
        "__settle('getFuzzyLocation', { _timeout: 0 }); \
         globalThis.__gcj = migo.getFuzzyLocation({ type: 'gcj02', _timeout: 0 });",
        "0",
    );
    let requests = host.requests.lock().unwrap().clone();
    assert_eq!(requests[0].0, "fuzzy");
    let fields: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
    assert!(fields.get("type").is_none(), "{}", requests[0].1);
    let result = run(
        &mut runtime,
        &(answer("_internalOnFuzzyLocationResult", &requests[0].1, BEIJING)
            + &answer("_internalOnFuzzyLocationResult", &requests[1].1, BEIJING)
            + "__gcj.then(function (res) { __results.gcj = { ok: true, res: res }; });"),
        "JSON.stringify([__results.getFuzzyLocation, __results.gcj])",
    );
    let results: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_near(position(&results[0]), (39.91, 116.4));
    // Only the position: what the host knew of its precision stays with it.
    assert_eq!(
        results[0]["res"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["errMsg", "latitude", "longitude"]
    );
    // Rounded first, in the datum the kilometre is measured in.
    assert_near(
        position(&results[1]),
        (39.91140396985828, 116.40624405978318),
    );
}

#[test]
fn a_failure_is_the_hosts_unchanged() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeLocation::default());
    let mut runtime = boot(host.clone());
    run(
        &mut runtime,
        "__settle('getLocation', { type: 'gcj02', _timeout: 0 });",
        "0",
    );
    let request = host.requests.lock().unwrap()[0].1.clone();
    let result = run(
        &mut runtime,
        &answer(
            "_internalOnLocationResult",
            &request,
            "{ error: 'location service disabled' }",
        ),
        "JSON.stringify(__results.getLocation)",
    );
    assert_eq!(
        result,
        r#"{"ok":false,"res":{"errMsg":"getLocation:fail location service disabled"}}"#
    );
}
