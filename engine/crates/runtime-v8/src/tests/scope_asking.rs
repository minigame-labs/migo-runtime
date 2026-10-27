//! A call that needs a scope nobody has decided asks for it, as authorize does
//! -- authorize being how content asks in advance -- and goes ahead or refuses
//! once the host has decided. A decided scope is not asked again, calls made
//! while the player is asked share the one question, and scope.userInfo is
//! never asked this way.
//!
//! Calls pass `_timeout: 0` where the test answers between two runs of the
//! event loop, which would otherwise run the request's timeout out first.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use deno_core::{JsRuntime, serde_json};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, LocationService, MediaServices,
        PermissionService, RecorderService, Scope, ScopeState, SensorServices, SystemUtilServices,
    },
};

use super::support::{SETTLE, boot_with_services, run_to_idle as run};

/// The host's decisions, and what it was asked.
#[derive(Default)]
struct Host {
    decided: Mutex<HashMap<Scope, ScopeState>>,
    asked: Mutex<Vec<String>>,
    located: Mutex<Vec<String>>,
    recording: Mutex<usize>,
}

impl Host {
    fn decide(&self, scope: Scope, state: ScopeState) {
        self.decided.lock().unwrap().insert(scope, state);
    }
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
    fn located(&self) -> Vec<String> {
        self.located.lock().unwrap().clone()
    }
}

struct Decisions(Arc<Host>);

impl PermissionService for Decisions {
    fn scope_state(&self, scope: Scope) -> ScopeState {
        self.0
            .decided
            .lock()
            .unwrap()
            .get(&scope)
            .copied()
            .unwrap_or(ScopeState::Unknown)
    }
    fn request_scope(&self, request_json: &str) -> Result<(), ServiceError> {
        self.0.asked.lock().unwrap().push(request_json.to_owned());
        Ok(())
    }
}

struct Locations(Arc<Host>);

impl LocationService for Locations {
    fn get_location(&self, options_json: &str) -> Result<(), ServiceError> {
        self.0.located.lock().unwrap().push(options_json.to_owned());
        Ok(())
    }
    fn get_fuzzy_location(&self, options_json: &str) -> Result<(), ServiceError> {
        self.0.located.lock().unwrap().push(options_json.to_owned());
        Ok(())
    }
}

struct Recorder(Arc<Host>);

impl RecorderService for Recorder {
    fn start(&self, _options_json: &str) -> Result<(), ServiceError> {
        *self.0.recording.lock().unwrap() += 1;
        Ok(())
    }
}

/// A platform with a permission service, or -- `asks` false -- one with none,
/// where nobody can be asked.
struct Platform {
    host: Arc<Host>,
    asks: bool,
}

impl SensorServices for Platform {}
impl CommerceServices for Platform {}
impl MediaServices for Platform {
    fn recorder(&self) -> Option<Arc<dyn RecorderService>> {
        Some(Arc::new(Recorder(self.host.clone())))
    }
}
impl ConnectivityServices for Platform {
    fn location(&self) -> Option<Arc<dyn LocationService>> {
        Some(Arc::new(Locations(self.host.clone())))
    }
}
impl SystemUtilServices for Platform {
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        self.asks
            .then(|| Arc::new(Decisions(self.host.clone())) as Arc<dyn PermissionService>)
    }
}

fn boot(host: &Arc<Host>, asks: bool) -> JsRuntime {
    let platform = Platform {
        host: host.clone(),
        asks,
    };
    let mut runtime = boot_with_services(Some(Arc::new(platform) as Arc<dyn DeviceServices>), None);
    run(&mut runtime, SETTLE, "0");
    runtime
}

/// The host answering `request` -- an authorize or a location call -- through `hook`.
fn answer(hook: &str, request: &str, fields: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(request).unwrap();
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('{hook}', \
         JSON.stringify([JSON.stringify(Object.assign({{ requestId: {} }}, {fields}))]));",
        request["requestId"]
    )
}

fn scope_of(request: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(request).unwrap();
    request["scope"].as_str().unwrap().to_owned()
}

#[test]
fn an_undecided_scope_is_asked_for_and_the_call_goes_ahead_once_granted() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(Host::default());
    let mut runtime = boot(&host, true);
    // Two calls while the player is asked: one question.
    run(
        &mut runtime,
        "__settle('getLocation', { _timeout: 0 }); __settle('getFuzzyLocation', { _timeout: 0 });",
        "0",
    );
    let asked = host.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(scope_of(&asked[0]), "scope.userLocation");
    assert!(
        host.located().is_empty(),
        "the host was asked for a position before the scope"
    );

    // The host records the grant, then answers authorize.
    host.decide(Scope::UserLocation, ScopeState::Granted);
    run(
        &mut runtime,
        &answer("_internalOnAuthorizeResult", &asked[0], "{}"),
        "0",
    );
    let located = host.located();
    assert_eq!(located.len(), 2, "{located:?}");
    let result = run(
        &mut runtime,
        &(answer(
            "_internalOnLocationResult",
            &located[0],
            "{ latitude: 48.8566, longitude: 2.3522 }",
        ) + &answer(
            "_internalOnFuzzyLocationResult",
            &located[1],
            "{ latitude: 48.8566, longitude: 2.3522 }",
        )),
        "JSON.stringify([__results.getLocation.ok, __results.getFuzzyLocation.ok])",
    );
    assert_eq!(result, "[true,true]");

    // Granted, the next call does not ask.
    run(
        &mut runtime,
        "__settle('getLocation', { _timeout: 0 });",
        "0",
    );
    assert_eq!(host.asked().len(), 1);
    assert_eq!(host.located().len(), 3);
}

#[test]
fn a_refusal_refuses_the_call_and_is_not_asked_again() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(Host::default());
    let mut runtime = boot(&host, true);
    run(
        &mut runtime,
        "__settle('getLocation', { _timeout: 0 });",
        "0",
    );
    let asked = host.asked();
    host.decide(Scope::UserLocation, ScopeState::Denied);
    let result = run(
        &mut runtime,
        &answer(
            "_internalOnAuthorizeResult",
            &asked[0],
            "{ error: 'auth deny' }",
        ),
        "JSON.stringify(__results.getLocation)",
    );
    assert_eq!(
        result,
        r#"{"ok":false,"res":{"errMsg":"getLocation:fail auth deny: scope.userLocation is not granted"}}"#
    );
    assert!(host.located().is_empty());

    // Decided, it refuses at once.
    let result = run(
        &mut runtime,
        "__settle('getFuzzyLocation', { _timeout: 0 });",
        "JSON.stringify(__results.getFuzzyLocation)",
    );
    assert_eq!(
        result,
        r#"{"ok":false,"res":{"errMsg":"getFuzzyLocation:fail auth deny: scope.userLocation is not granted"}}"#
    );
    assert_eq!(host.asked().len(), 1);
}

#[test]
fn the_players_answer_is_not_the_calls_time() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(Host::default());
    let mut runtime = boot(&host, true);
    // The event loop runs until nothing is left to wait for: a timer still
    // counting while the player is asked would have failed the call.
    let pending = run(
        &mut runtime,
        "__settle('getLocation', { _timeout: 50 });",
        "String(__results.getLocation)",
    );
    assert_eq!(pending, "undefined");
    let asked = host.asked();
    host.decide(Scope::UserLocation, ScopeState::Denied);
    let result = run(
        &mut runtime,
        &answer(
            "_internalOnAuthorizeResult",
            &asked[0],
            "{ error: 'auth deny' }",
        ),
        "JSON.stringify(__results.getLocation)",
    );
    assert_eq!(
        result,
        r#"{"ok":false,"res":{"errMsg":"getLocation:fail auth deny: scope.userLocation is not granted"}}"#
    );
}

#[test]
fn with_nobody_to_ask_the_call_refuses_as_it_did() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(Host::default());
    let mut runtime = boot(&host, false);
    let result = run(
        &mut runtime,
        "__settle('getLocation', { _timeout: 0 });",
        "JSON.stringify(__results.getLocation)",
    );
    assert_eq!(
        result,
        r#"{"ok":false,"res":{"errMsg":"getLocation:fail auth deny: scope.userLocation is not granted"}}"#
    );
    assert!(host.located().is_empty());
}

#[test]
fn a_call_outside_the_deferred_apis_asks_too() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(Host::default());
    let mut runtime = boot(&host, true);
    run(
        &mut runtime,
        "globalThis.__errors = []; \
         const recorder = migo.getRecorderManager(); \
         recorder.onError(function (res) { __errors.push(res.errMsg); }); \
         recorder.start({});",
        "0",
    );
    let asked = host.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(scope_of(&asked[0]), "scope.record");
    assert_eq!(*host.recording.lock().unwrap(), 0);

    host.decide(Scope::Record, ScopeState::Granted);
    let errors = run(
        &mut runtime,
        &answer("_internalOnAuthorizeResult", &asked[0], "{}"),
        "JSON.stringify(__errors)",
    );
    assert_eq!(errors, "[]");
    assert_eq!(*host.recording.lock().unwrap(), 1);
}

#[test]
fn user_info_is_never_asked_this_way() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(Host::default());
    let mut runtime = boot(&host, true);
    run(
        &mut runtime,
        "__settle('getUserInfo', { _timeout: 0 });",
        "0",
    );
    assert!(host.asked().is_empty(), "{:?}", host.asked());
}
