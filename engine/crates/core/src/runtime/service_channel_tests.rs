use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use frame_wire::service::{ServiceDownRecord, read_down_message};
use frame_wire::value::OwnedValue;
use parking_lot::Mutex;
use shared::services::{
    ConnectivityServices, InteractionService, LocationService, MediaServices, PermissionService,
    Scope, ScopeState, SensorServices, ServiceError, SystemInfoService, SystemUtilServices,
};

use super::super::external_services::{ServiceHost, WakerSlot};
use super::super::host_events::{ServiceEventSink, event};
use super::super::service_ops::{ALL, id};
use super::*;

fn contract(name: &str) -> serde_json::Value {
    let text = match name {
        "host-services" => include_str!("../../../../../contracts/runtime/host-services.json"),
        "op-boundary" => include_str!("../../../../../contracts/runtime/op-boundary.json"),
        _ => unreachable!(),
    };
    serde_json::from_str(text).expect("the contract is JSON")
}

/// Every host-service method by op: its hook, `None` for a command.
fn methods() -> BTreeMap<String, Option<String>> {
    let services = contract("host-services");
    let mut methods = BTreeMap::new();
    for service in services["services"].as_object().unwrap().values() {
        for method in service["methods"].as_object().unwrap().values() {
            methods.insert(
                method["op"].as_str().unwrap().to_owned(),
                method["hook"].as_str().map(str::to_owned),
            );
        }
    }
    methods
}

/// The channel is every host-service method the contract sends this host as a
/// command, with the hook the contract delivers its result to: a method the
/// producer sends that the table lacks would reach the host and be refused as
/// unknown, and a wrong hook would leave content's call waiting.
#[test]
fn the_table_is_the_contract_s_commands_with_the_contract_s_hooks() {
    let boundary = contract("op-boundary");
    let mut lanes = BTreeMap::new();
    for extension in boundary["extensions"].as_object().unwrap().values() {
        for (op, lane) in extension["ops"].as_object().into_iter().flatten() {
            // A lane, or a lane with what the producer answers locally.
            let lane = lane.as_str().or_else(|| lane["lane"].as_str()).unwrap();
            lanes.insert(op.clone(), lane.to_owned());
        }
    }
    let methods = methods();
    // Sent as a command and answered elsewhere: the package's own files.
    let elsewhere = ["op_download_subpackage"];
    let sent: BTreeSet<&str> = methods
        .keys()
        .map(String::as_str)
        .filter(|op| lanes.get(*op).map(String::as_str) == Some("command"))
        .filter(|op| !elsewhere.contains(op))
        .collect();
    assert_eq!(OPS.iter().copied().collect::<BTreeSet<_>>(), sent);

    let numbers: BTreeMap<&str, u32> = ALL.iter().copied().collect();
    for op in OPS {
        let number = numbers[op];
        assert!(is_channel(number), "{op}");
        assert_eq!(hook(number), methods[*op].as_deref(), "{op}'s hook");
    }
    assert!(
        !is_channel(id::op_vibrate_short),
        "a command of the host's own"
    );

    // The requests that name content's files arrive awaited, and are the
    // contract's awaited methods -- every one of them.
    let awaited: BTreeSet<&str> = methods
        .keys()
        .map(String::as_str)
        .filter(|op| lanes.get(*op).map(String::as_str) == Some("async"))
        .collect();
    assert_eq!(FILE_OPS.iter().copied().collect::<BTreeSet<_>>(), awaited);
    for op in FILE_OPS {
        assert!(names_files(numbers[op]), "{op}");
        assert!(!is_channel(numbers[op]), "{op} is not also a command");
    }
}

#[derive(Default)]
struct Recorded {
    calls: Mutex<Vec<String>>,
    granted: Vec<Scope>,
}

impl Recorded {
    fn push(&self, call: String) -> Result<(), ServiceError> {
        self.calls.lock().push(call);
        Ok(())
    }
}

impl InteractionService for Recorded {
    fn show_toast(&self, json: &str) -> Result<(), ServiceError> {
        self.push(format!("toast {json}"))
    }
    fn hide_toast(&self) -> Result<(), ServiceError> {
        self.push("hide toast".into())
    }
    fn show_modal(&self, _json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("busy"))
    }
}

impl SystemInfoService for Recorded {
    fn open_app_authorize_setting(&self, request_id: i32) -> Result<(), ServiceError> {
        self.push(format!("authorize setting {request_id}"))
    }
}

impl LocationService for Recorded {
    fn get_location(&self, json: &str) -> Result<(), ServiceError> {
        self.push(format!("location {json}"))
    }
}

impl PermissionService for Recorded {
    fn scope_state(&self, scope: Scope) -> ScopeState {
        if self.granted.contains(&scope) {
            ScopeState::Granted
        } else {
            ScopeState::Denied
        }
    }
}

/// A platform whose device services are this one recorder, the location
/// service only when `located`.
struct Platform {
    recorded: Arc<Recorded>,
    located: bool,
}

impl SensorServices for Platform {}
impl MediaServices for Platform {}
impl ConnectivityServices for Platform {
    fn location(&self) -> Option<Arc<dyn LocationService>> {
        self.located
            .then(|| Arc::clone(&self.recorded) as Arc<dyn LocationService>)
    }
}
impl shared::services::CommerceServices for Platform {}
impl SystemUtilServices for Platform {
    fn interaction(&self) -> Option<Arc<dyn InteractionService>> {
        Some(Arc::clone(&self.recorded) as Arc<dyn InteractionService>)
    }
    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        Some(Arc::clone(&self.recorded) as Arc<dyn SystemInfoService>)
    }
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        Some(Arc::clone(&self.recorded) as Arc<dyn PermissionService>)
    }
}

fn platform(granted: Vec<Scope>, located: bool) -> (Platform, Arc<Recorded>) {
    let recorded = Arc::new(Recorded {
        calls: Mutex::default(),
        granted,
    });
    let platform = Platform {
        recorded: Arc::clone(&recorded),
        located,
    };
    (platform, recorded)
}

fn json(text: &str) -> Vec<OwnedValue> {
    vec![OwnedValue::Str(text.to_owned())]
}

/// Send each command to a fresh host and collect the hooks content is called
/// with: the hook, and its one argument's result parsed.
fn hooks_of(
    device: Option<&dyn DeviceServices>,
    commands: Vec<(u32, Vec<OwnedValue>)>,
) -> Vec<(String, serde_json::Value)> {
    let (host, _work) = ServiceHost::new(
        1,
        PathBuf::new(),
        PathBuf::new(),
        Arc::new(WakerSlot::default()),
        Arc::default(),
    );
    let sink = ServiceEventSink {
        outbox: &host.outbox,
    };
    for (op, args) in commands {
        command(device, op, args, &sink);
    }
    let mut called = Vec::new();
    while let Some(message) = host.outbox.take_message() {
        let (_, records) = read_down_message(&message).expect("a down message");
        for record in records {
            let ServiceDownRecord::Event { event, values } = record else {
                panic!("a hook call is an event");
            };
            assert_eq!(event, event::_internalDispatch);
            let [OwnedValue::Str(hook), OwnedValue::Str(args)] = values.as_slice() else {
                panic!("a hook call is its name and its arguments");
            };
            let args: Vec<String> = serde_json::from_str(args).expect("one JSON string");
            let [result] = args.as_slice() else {
                panic!("one argument");
            };
            called.push((
                hook.clone(),
                serde_json::from_str(result).expect("a result"),
            ));
        }
    }
    called
}

/// Each request reaches the method the embedded op calls, with what content
/// gave it, and a call the host accepts is answered by the host app alone.
#[test]
fn a_request_reaches_the_embedded_op_s_method_with_content_s_arguments() {
    let (platform, recorded) = platform(vec![Scope::UserLocation], true);
    let called = hooks_of(
        Some(&platform),
        vec![
            (id::op_show_toast, json(r#"{"title":"hi"}"#)),
            (id::op_hide_toast, vec![]),
            (id::op_open_app_authorize_setting, vec![OwnedValue::I32(4)]),
            (id::op_get_location, json(r#"{"requestId":5}"#)),
        ],
    );
    assert!(called.is_empty(), "{called:?}");
    assert_eq!(
        *recorded.calls.lock(),
        [
            r#"toast {"title":"hi"}"#,
            "hide toast",
            "authorize setting 4",
            r#"location {"requestId":5}"#,
        ]
    );
}

/// A call refused here fails content's call through its hook, with its
/// request id and the reason the embedded op throws: a scope not granted, no
/// such service, a service that turns it down.
#[test]
fn a_refused_call_fails_through_its_hook_in_the_embedded_op_s_words() {
    let (denied, recorded) = platform(vec![], true);
    assert_eq!(
        hooks_of(
            Some(&denied),
            vec![(
                id::op_get_location,
                json(r#"{"requestId":7,"altitude":false}"#)
            )],
        ),
        [(
            "_internalOnLocationResult".to_owned(),
            serde_json::json!({"requestId": 7, "error": "auth deny: scope.userLocation is not granted"}),
        )]
    );
    assert!(
        recorded.calls.lock().is_empty(),
        "a gated call never reaches the host"
    );

    let (unlocated, _) = platform(vec![Scope::UserLocation], false);
    assert_eq!(
        hooks_of(
            Some(&unlocated),
            vec![
                (id::op_get_location, json(r#"{"requestId":8}"#)),
                (id::op_show_modal, json(r#"{"requestId":9,"title":"t"}"#)),
            ],
        ),
        [
            (
                "_internalOnLocationResult".to_owned(),
                serde_json::json!({"requestId": 8, "error": "not supported"}),
            ),
            (
                "_internalOnModalResult".to_owned(),
                serde_json::json!({"requestId": 9, "error": "busy"}),
            ),
        ]
    );

    assert_eq!(
        hooks_of(
            None,
            vec![(id::op_open_app_authorize_setting, vec![OwnedValue::I32(3)])],
        ),
        [(
            "_internalOnOpenAppAuthorizeSettingFinished".to_owned(),
            serde_json::json!({"requestId": 3, "error": "not supported"}),
        )]
    );
}

/// What names no call to fail is refused in the log: a command, which nothing
/// answers, and a request that is not its op's shape.
#[test]
fn a_refusal_with_no_call_waiting_reaches_no_hook() {
    assert!(
        hooks_of(
            None,
            vec![
                (id::op_hide_toast, vec![]),
                (id::op_show_toast, json("{}")),
                (id::op_get_location, vec![]),
                (id::op_get_location, json("not json")),
                (
                    id::op_open_app_authorize_setting,
                    vec![OwnedValue::Str("1".into())]
                ),
            ],
        )
        .is_empty()
    );
}
