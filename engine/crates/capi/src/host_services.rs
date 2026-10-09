//! The host-service channel's engine half.
//!
//! Content's ads, payments, sign-in, sharing and mini-program navigation reach a
//! C host as [`MigoHostServiceCall`]s through its dispatcher, and come back as a
//! completion (`migo_session_complete_host_service_call`) or a service event
//! (`migo_session_post_host_service_event`), which this module turns into the
//! host-bridge hook content is already waiting on -- the same hooks the Android
//! SDK's handlers answer through. See `include/migo/host_services.h` for the
//! host's side and `contracts/runtime/host-services.json` for the numbers.
//!
//! # A call id carries its own routing
//!
//! A call's id encodes the service, the method and content's request id, so a
//! completion needs no table to be routed: there is nothing to grow when a host
//! never answers, nothing to clean up when content goes away, and nothing to
//! lock on the way back. What that gives up is telling a completed call from one
//! never made -- and content does that already: request ids come from one
//! allocator per Host that a restart does not reset, a Session has one Host for
//! its life, and a result for an id content is not waiting on is discarded.
//! What a forged id could do is settle a request of the *same* method content
//! has outstanding, which is the host misreporting its own work, not an escape.

use std::sync::Arc;

use migo_capi_abi::{
    MIGO_ERROR_INTERNAL, MIGO_ERROR_INVALID_ARGUMENT, MIGO_OK, MigoResult,
    host_services::{
        HostServiceOutcome, MIGO_AD_CREATE, MIGO_AD_DESTROY, MIGO_AD_EVENT_LIFECYCLE, MIGO_AD_HIDE,
        MIGO_AD_LOAD, MIGO_AD_SHOW, MIGO_AD_UPDATE_STYLE, MIGO_AUTH_CHECK_SESSION,
        MIGO_AUTH_GET_PHONE_NUMBER, MIGO_AUTH_GET_USER_INFO, MIGO_AUTH_LOGIN, MIGO_HOST_SERVICE_AD,
        MIGO_HOST_SERVICE_AUTH, MIGO_HOST_SERVICE_NAVIGATE, MIGO_HOST_SERVICE_PAYMENT,
        MIGO_HOST_SERVICE_SHARE, MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM,
        MIGO_NAVIGATE_NAVIGATE_TO_MINI_PROGRAM, MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION,
        MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT, MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM,
        MIGO_SHARE_SHARE_APP_MESSAGE, MigoHostServiceResult, copy_bounded,
    },
};
use migo_core::services::{AdService, AuthService, NavigateService, PaymentService, ShareService};
use serde_json::{Map, Value};
use shared::{
    js_escape::hook_args_one, protocol::error::ServiceError, protocol::host_cmd::HostCommand,
};

use crate::{MigoSession, callbacks::Notifier, panic_barrier::guard, pin_session};

/// One request content makes that the host answers.
#[derive(Debug, PartialEq, Eq)]
struct Call {
    service: u32,
    method: u32,
    /// Where the outcome is delivered.
    hook: &'static str,
    /// The field a failure's numeric code travels in; `None` where the API
    /// defines none.
    error_code_field: Option<&'static str>,
}

const REQUEST_MIDAS_PAYMENT: Call = Call {
    service: MIGO_HOST_SERVICE_PAYMENT,
    method: MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT,
    hook: "_internalOnMidasPaymentResult",
    error_code_field: Some("errCode"),
};
const REQUEST_MIDAS_PAYMENT_GAME_ITEM: Call = Call {
    service: MIGO_HOST_SERVICE_PAYMENT,
    method: MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM,
    hook: "_internalOnMidasPaymentGameItemResult",
    error_code_field: Some("errCode"),
};
const LOGIN: Call = Call {
    service: MIGO_HOST_SERVICE_AUTH,
    method: MIGO_AUTH_LOGIN,
    hook: "_internalOnLoginResult",
    error_code_field: Some("errno"),
};
const CHECK_SESSION: Call = Call {
    service: MIGO_HOST_SERVICE_AUTH,
    method: MIGO_AUTH_CHECK_SESSION,
    hook: "_internalOnCheckSessionResult",
    error_code_field: Some("errno"),
};
const GET_USER_INFO: Call = Call {
    service: MIGO_HOST_SERVICE_AUTH,
    method: MIGO_AUTH_GET_USER_INFO,
    hook: "_internalOnGetUserInfoResult",
    error_code_field: None,
};
const GET_PHONE_NUMBER: Call = Call {
    service: MIGO_HOST_SERVICE_AUTH,
    method: MIGO_AUTH_GET_PHONE_NUMBER,
    hook: "_internalOnGetPhoneNumberResult",
    error_code_field: Some("errno"),
};
const SHARE_APP_MESSAGE: Call = Call {
    service: MIGO_HOST_SERVICE_SHARE,
    method: MIGO_SHARE_SHARE_APP_MESSAGE,
    hook: "_internalOnShareAppMessageResult",
    error_code_field: Some("errCode"),
};
const NAVIGATE_TO_MINI_PROGRAM: Call = Call {
    service: MIGO_HOST_SERVICE_NAVIGATE,
    method: MIGO_NAVIGATE_NAVIGATE_TO_MINI_PROGRAM,
    hook: "_internalOnNavigateToMiniProgramResult",
    error_code_field: Some("errCode"),
};

const CALLS: &[&Call] = &[
    &REQUEST_MIDAS_PAYMENT,
    &REQUEST_MIDAS_PAYMENT_GAME_ITEM,
    &LOGIN,
    &CHECK_SESSION,
    &GET_USER_INFO,
    &GET_PHONE_NUMBER,
    &SHARE_APP_MESSAGE,
    &NAVIGATE_TO_MINI_PROGRAM,
];

/// Fire-and-forget requests, as `(service, method)`. Listed so the contract test
/// can hold the whole method set to the contract, not only the calls.
const COMMANDS: &[(u32, u32)] = &[
    (MIGO_HOST_SERVICE_AD, MIGO_AD_CREATE),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_LOAD),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_SHOW),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_HIDE),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_UPDATE_STYLE),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_DESTROY),
    (
        MIGO_HOST_SERVICE_NAVIGATE,
        MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM,
    ),
    (
        MIGO_HOST_SERVICE_NAVIGATE,
        MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION,
    ),
];

/// A service's own events, as `(service, event, hook)`.
const EVENTS: &[(u32, u32, &str)] = &[(
    MIGO_HOST_SERVICE_AD,
    MIGO_AD_EVENT_LIFECYCLE,
    "_internalOnAdEvent",
)];

fn event_hook(service: u32, event: u32) -> Option<&'static str> {
    EVENTS
        .iter()
        .find(|(s, e, _)| *s == service && *e == event)
        .map(|(_, _, hook)| *hook)
}

// ---- call ids ---------------------------------------------------------------

const METHOD_SHIFT: u32 = 32;
const SERVICE_SHIFT: u32 = 48;
/// Content's request ids, as `parseHostCallbackId` in `02_async.js` accepts them.
const MAX_REQUEST_ID: u64 = i32::MAX as u64;

fn encode_call_id(call: &Call, request_id: u32) -> u64 {
    debug_assert!(request_id != 0 && u64::from(request_id) <= MAX_REQUEST_ID);
    (u64::from(call.service) << SERVICE_SHIFT)
        | (u64::from(call.method) << METHOD_SHIFT)
        | u64::from(request_id)
}

fn decode_call_id(call_id: u64) -> Option<(&'static Call, u32)> {
    let request_id = call_id & 0xffff_ffff;
    if request_id == 0 || request_id > MAX_REQUEST_ID {
        return None;
    }
    let method = ((call_id >> METHOD_SHIFT) & 0xffff) as u32;
    let service = (call_id >> SERVICE_SHIFT) as u32;
    let call = CALLS
        .iter()
        .copied()
        .find(|call| call.service == service && call.method == method)?;
    Some((call, request_id as u32))
}

// ---- outbound ---------------------------------------------------------------

/// Split content's request into its id and the options the host is given.
///
/// The id is content's bookkeeping, not part of the request, and the host never
/// needs it: it answers by call id. A request without a valid one is an engine
/// defect -- every call site allocates one -- so it is refused, not guessed.
fn split_request(options_json: &str) -> Option<(u32, String)> {
    let Value::Object(mut options) = serde_json::from_str::<Value>(options_json).ok()? else {
        return None;
    };
    let request_id = options.remove("requestId")?.as_u64()?;
    if request_id == 0 || request_id > MAX_REQUEST_ID {
        return None;
    }
    Some((request_id as u32, Value::Object(options).to_string()))
}

/// Content's ads, payments, sign-in, sharing and navigation, carried to a C host
/// that declared it supplies them.
pub(crate) struct CapiHostServices {
    notifier: Arc<Notifier>,
}

impl CapiHostServices {
    pub(crate) fn new(notifier: Arc<Notifier>) -> Arc<Self> {
        Arc::new(Self { notifier })
    }

    fn command(&self, service: u32, method: u32, options_json: &str) -> Result<(), ServiceError> {
        if self
            .notifier
            .host_service_call(service, method, 0, options_json.to_owned())
        {
            Ok(())
        } else {
            Err(ServiceError::not_supported(
                "host dispatcher refused the request",
            ))
        }
    }

    fn call(&self, call: &Call, options_json: &str) -> Result<(), ServiceError> {
        let Some((request_id, payload)) = split_request(options_json) else {
            return Err(ServiceError::invalid_param("malformed request"));
        };
        if self.notifier.host_service_call(
            call.service,
            call.method,
            encode_call_id(call, request_id),
            payload,
        ) {
            Ok(())
        } else {
            Err(ServiceError::not_supported(
                "host dispatcher refused the request",
            ))
        }
    }
}

impl AdService for CapiHostServices {
    fn create_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_CREATE, request_json)
    }
    fn load_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_LOAD, request_json)
    }
    fn show_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_SHOW, request_json)
    }
    fn hide_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_HIDE, request_json)
    }
    fn update_ad_style(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_UPDATE_STYLE, request_json)
    }
    fn destroy_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_DESTROY, request_json)
    }
}

impl PaymentService for CapiHostServices {
    /// A host that declared the payment service is one that can take payments;
    /// one that cannot right now -- no store account, a region without it --
    /// says so by failing the payment, which is what content handles anyway.
    fn check_is_support_midas_payment(&self, _options_json: &str) -> Result<String, ServiceError> {
        Ok(r#"{"data":{"allow_pay":true}}"#.to_owned())
    }
    fn request_midas_payment(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&REQUEST_MIDAS_PAYMENT, options_json)
    }
    fn request_midas_payment_game_item(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&REQUEST_MIDAS_PAYMENT_GAME_ITEM, options_json)
    }
}

impl AuthService for CapiHostServices {
    fn login(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&LOGIN, options_json)
    }
    fn check_session(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&CHECK_SESSION, options_json)
    }
    fn get_user_info(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_USER_INFO, options_json)
    }
    fn get_phone_number(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_PHONE_NUMBER, options_json)
    }
}

impl ShareService for CapiHostServices {
    fn share_app_message(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&SHARE_APP_MESSAGE, options_json)
    }
}

impl NavigateService for CapiHostServices {
    fn navigate_to_mini_program(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&NAVIGATE_TO_MINI_PROGRAM, options_json)
    }
    fn navigate_back_mini_program(&self, options_json: &str) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_NAVIGATE,
            MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM,
            options_json,
        )
    }
    fn open_customer_service_conversation(&self, options_json: &str) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_NAVIGATE,
            MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION,
            options_json,
        )
    }
}

// ---- inbound ----------------------------------------------------------------

/// Names content's result hooks write themselves, which a host's result must
/// not supply: `error` would turn a success into a failure, `requestId` would
/// redirect it, and `errMsg` is composed by content from the outcome.
const RESERVED_RESULT_FIELDS: [&str; 3] = ["requestId", "error", "errMsg"];

/// The JSON a call's hook receives, in the shape the Android SDK's results
/// already take: the request id, then either the result's fields or `error`
/// with the API's code field.
///
/// `error` is the host's reason and nothing more. Content composes the `errMsg`
/// (`<api>:fail <reason>`) because only content knows which API it was: one
/// host method can serve several -- a subpackage download is both
/// `loadSubpackage` and `preDownloadSubpackage` -- and a name chosen here would
/// be wrong for all but one.
fn completion_json(
    call: &Call,
    request_id: u32,
    outcome: HostServiceOutcome,
) -> Result<String, MigoResult> {
    let mut fields = Map::new();
    match outcome {
        HostServiceOutcome::Ok { payload_json } => {
            if !payload_json.is_empty() {
                let Ok(Value::Object(result)) = serde_json::from_str::<Value>(&payload_json) else {
                    return Err(MIGO_ERROR_INVALID_ARGUMENT);
                };
                if RESERVED_RESULT_FIELDS
                    .iter()
                    .any(|field| result.contains_key(*field))
                {
                    return Err(MIGO_ERROR_INVALID_ARGUMENT);
                }
                fields = result;
            }
            fields.insert("requestId".into(), request_id.into());
        }
        HostServiceOutcome::Fail {
            error_code,
            message,
        } => {
            // Never empty: the ABI refuses a failure without a reason, and an
            // empty `error` is what content's settlers read as success.
            debug_assert!(!message.is_empty());
            fields.insert("requestId".into(), request_id.into());
            fields.insert("error".into(), message.into());
            if let (Some(field), Some(code)) = (call.error_code_field, error_code) {
                fields.insert(field.into(), code.into());
            }
        }
    }
    Ok(Value::Object(fields).to_string())
}

/// Hand `json` to content's `hook`, if `service` is one this host declared and
/// there is content to tell.
fn deliver(session: &MigoSession, service: u32, hook: &'static str, json: String) -> MigoResult {
    let Ok(state) = session.state.lock() else {
        return MIGO_ERROR_INTERNAL;
    };
    let declared = state
        .callbacks
        .as_ref()
        .is_some_and(|callbacks| callbacks.supplies_host_service(service));
    if !declared {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    // No Host yet means no content has run, so none is waiting; one that has
    // gone with its Session cannot be reached here at all.
    let Some(host) = state.host.as_ref().map(crate::SessionEngine::id) else {
        return MIGO_OK;
    };
    drop(state);
    if let Err(error) = migo_core::send_reliable_command_to_host(
        host,
        HostCommand::InvokeHostHook {
            hook,
            args_json: hook_args_one(json),
        },
    ) {
        tracing::warn!("host service result for {hook} not delivered: {error}");
    }
    MIGO_OK
}

/// # Safety
/// `session` must be a live session handle and `result` null or readable for
/// its announced size, with each string readable for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_complete_host_service_call(
    session: *mut MigoSession,
    call_id: u64,
    result: *const MigoHostServiceResult,
) -> MigoResult {
    guard("migo_session_complete_host_service_call", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Some((call, request_id)) = decode_call_id(call_id) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let outcome = match unsafe { MigoHostServiceResult::parse(result) } {
            Ok(outcome) => outcome,
            Err(error) => return error,
        };
        match completion_json(call, request_id, outcome) {
            Ok(json) => deliver(&session, call.service, call.hook, json),
            Err(error) => error,
        }
    })
}

/// # Safety
/// `session` must be a live session handle and `payload_json_utf8` null with a
/// zero length or readable for `payload_length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_post_host_service_event(
    session: *mut MigoSession,
    service: u32,
    event: u32,
    payload_json_utf8: *const std::os::raw::c_char,
    payload_length: u32,
) -> MigoResult {
    guard("migo_session_post_host_service_event", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Some(hook) = event_hook(service, event) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let payload = match unsafe { copy_bounded(payload_json_utf8, payload_length) } {
            Ok(payload) => payload,
            Err(error) => return error,
        };
        // Checked here, where the host can be told, rather than in content,
        // which can only drop it.
        if !matches!(
            serde_json::from_str::<Value>(&payload),
            Ok(Value::Object(_))
        ) {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        deliver(&session, service, hook, payload)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        callbacks::{MigoHostCallbacks, MigoTaskFn},
        test_support::{callback_session_pin, with_session},
    };
    use migo_capi_abi::{
        MIGO_ABI_VERSION_CURRENT, VersionedHeader,
        host_services::{
            MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE, MIGO_HOST_SERVICE_STATUS_FAIL,
            MIGO_HOST_SERVICE_STATUS_OK, MIGO_HOST_SERVICES_KNOWN, MigoHostServiceCall,
        },
    };
    use std::{ffi::c_void, sync::Mutex};

    // ---- the contract ----

    const CONTRACT: &str = include_str!("../../../../contracts/runtime/host-services.json");

    fn abi_service(name: &str) -> u32 {
        match name {
            "ad" => MIGO_HOST_SERVICE_AD,
            "payment" => MIGO_HOST_SERVICE_PAYMENT,
            "auth" => MIGO_HOST_SERVICE_AUTH,
            "share" => MIGO_HOST_SERVICE_SHARE,
            "navigate" => MIGO_HOST_SERVICE_NAVIGATE,
            other => panic!("the contract names a service the ABI has no constant for: {other}"),
        }
    }

    /// The Rust table, the ABI's constants and the contract say the same thing,
    /// in both directions: nothing the contract lists is missing here, and
    /// nothing here is missing from the contract.
    #[test]
    fn the_table_is_the_contract() {
        let contract: Value = serde_json::from_str(CONTRACT).expect("contract parses");
        let services = contract["services"].as_object().expect("services");
        let mut declared_bits = 0u64;
        let (mut calls_seen, mut commands_seen, mut events_seen) = (0, 0, 0);
        for (name, spec) in services {
            let service = abi_service(name);
            assert_eq!(spec["id"].as_u64(), Some(u64::from(service)), "{name}'s id");
            declared_bits |= 1 << service;
            for (method_name, method) in spec["methods"].as_object().expect("methods") {
                let id = method["id"].as_u64().expect("method id") as u32;
                match method["kind"].as_str() {
                    Some("call") => {
                        let call = CALLS
                            .iter()
                            .find(|call| call.service == service && call.method == id)
                            .unwrap_or_else(|| panic!("{name}.{method_name} is not in CALLS"));
                        assert!(
                            method.get("api").is_none(),
                            "{method_name}: content composes errMsg, so a call names no api"
                        );
                        assert_eq!(
                            Some(call.hook),
                            method["hook"].as_str(),
                            "{method_name} hook"
                        );
                        assert_eq!(
                            call.error_code_field,
                            method["error_code_field"].as_str(),
                            "{method_name} error_code_field"
                        );
                        calls_seen += 1;
                    }
                    Some("command") => {
                        assert!(
                            COMMANDS.contains(&(service, id)),
                            "{name}.{method_name} is not in COMMANDS"
                        );
                        commands_seen += 1;
                    }
                    other => panic!("{name}.{method_name} has kind {other:?}"),
                }
            }
            for (event_name, event) in spec["events"].as_object().expect("events") {
                let id = event["id"].as_u64().expect("event id") as u32;
                assert_eq!(
                    event_hook(service, id),
                    event["hook"].as_str(),
                    "{name}.{event_name}"
                );
                events_seen += 1;
            }
        }
        assert_eq!(
            declared_bits, MIGO_HOST_SERVICES_KNOWN,
            "the known service set"
        );
        assert_eq!(
            calls_seen,
            CALLS.len(),
            "CALLS has an entry the contract does not"
        );
        assert_eq!(
            commands_seen,
            COMMANDS.len(),
            "COMMANDS has one the contract does not"
        );
        assert_eq!(
            events_seen,
            EVENTS.len(),
            "EVENTS has one the contract does not"
        );
    }

    // ---- ids and payloads ----

    #[test]
    fn a_call_id_carries_its_route_and_nothing_else_decodes() {
        for call in CALLS {
            let id = encode_call_id(call, 7);
            assert_ne!(id, 0, "a call is never mistaken for a command");
            assert_eq!(decode_call_id(id), Some((*call, 7)));
            assert_eq!(
                decode_call_id(encode_call_id(call, i32::MAX as u32)).map(|(_, r)| r),
                Some(i32::MAX as u32)
            );
        }
        assert_eq!(decode_call_id(0), None);
        let login = encode_call_id(&LOGIN, 1);
        assert_eq!(decode_call_id(login & !0xffff_ffff), None, "no request id");
        assert_eq!(
            decode_call_id(login | 0x8000_0000),
            None,
            "beyond content's ids"
        );
        // A command's route is not a call's: the ad service has no calls.
        assert_eq!(
            decode_call_id((u64::from(MIGO_HOST_SERVICE_AD) << SERVICE_SHIFT) | 1),
            None
        );
        assert_eq!(
            decode_call_id(login | (0x7f << METHOD_SHIFT)),
            None,
            "no such method"
        );
    }

    #[test]
    fn the_host_sees_the_request_without_content_s_id() {
        assert_eq!(
            split_request(r#"{"requestId":12,"timeout":500}"#),
            Some((12, r#"{"timeout":500}"#.to_owned()))
        );
        assert_eq!(split_request(r#"{"timeout":500}"#), None);
        assert_eq!(split_request(r#"{"requestId":0}"#), None);
        assert_eq!(split_request(r#"{"requestId":"12"}"#), None);
        assert_eq!(split_request(r#"{"requestId":2147483648}"#), None);
        assert_eq!(split_request("[12]"), None);
    }

    fn ok(payload: &str) -> HostServiceOutcome {
        HostServiceOutcome::Ok {
            payload_json: payload.to_owned(),
        }
    }

    fn fail(code: Option<i32>, message: &str) -> HostServiceOutcome {
        HostServiceOutcome::Fail {
            error_code: code,
            message: message.to_owned(),
        }
    }

    fn parsed(json: Result<String, MigoResult>) -> Value {
        serde_json::from_str(&json.expect("accepted")).expect("json")
    }

    #[test]
    fn a_success_is_the_host_s_fields_and_the_request_id() {
        assert_eq!(
            parsed(completion_json(&LOGIN, 3, ok(r#"{"code":"abc"}"#))),
            serde_json::json!({"requestId": 3, "code": "abc"})
        );
        assert_eq!(
            parsed(completion_json(&SHARE_APP_MESSAGE, 4, ok(""))),
            serde_json::json!({"requestId": 4})
        );
    }

    #[test]
    fn a_success_that_is_not_an_object_or_names_a_reserved_field_is_refused() {
        for payload in [
            "[1]",
            "\"x\"",
            "{",
            r#"{"error":"x"}"#,
            r#"{"requestId":9}"#,
            r#"{"errMsg":"x"}"#,
        ] {
            assert_eq!(
                completion_json(&LOGIN, 1, ok(payload)),
                Err(MIGO_ERROR_INVALID_ARGUMENT),
                "{payload}"
            );
        }
    }

    #[test]
    fn a_failure_is_the_host_s_reason_and_the_api_s_own_code_field() {
        assert_eq!(
            parsed(completion_json(&LOGIN, 5, fail(Some(1), "timeout"))),
            serde_json::json!({"requestId": 5, "error": "timeout", "errno": 1})
        );
        assert_eq!(
            parsed(completion_json(
                &REQUEST_MIDAS_PAYMENT,
                6,
                fail(Some(-2), "cancel")
            )),
            serde_json::json!({"requestId": 6, "error": "cancel", "errCode": -2})
        );
        // getUserInfo defines no code, so a host's is not invented a field for.
        assert_eq!(
            parsed(completion_json(&GET_USER_INFO, 7, fail(Some(9), "deny"))),
            serde_json::json!({"requestId": 7, "error": "deny"})
        );
        assert_eq!(
            parsed(completion_json(
                &NAVIGATE_TO_MINI_PROGRAM,
                8,
                fail(None, "no such app")
            )),
            serde_json::json!({"requestId": 8, "error": "no such app"})
        );
    }

    // ---- through the dispatcher ----

    #[derive(Default)]
    struct Heard(Mutex<Vec<(u64, u32, u32, String)>>);

    unsafe extern "C" fn inline_dispatch(
        _dispatcher: *mut c_void,
        task: MigoTaskFn,
        context: *mut c_void,
    ) -> MigoResult {
        unsafe { task(context) };
        MIGO_OK
    }

    unsafe extern "C" fn rejecting_dispatch(
        _dispatcher: *mut c_void,
        _task: MigoTaskFn,
        _context: *mut c_void,
    ) -> MigoResult {
        migo_capi_abi::MIGO_ERROR_DISPATCH_REJECTED
    }

    unsafe extern "C" fn on_call(
        user: *mut c_void,
        _session: *mut c_void,
        call: *const MigoHostServiceCall,
    ) {
        let call = unsafe { &*call };
        assert_eq!(
            call.header.struct_size as usize,
            size_of::<MigoHostServiceCall>()
        );
        assert_eq!(call.header.abi_version, MIGO_ABI_VERSION_CURRENT);
        let bytes = unsafe {
            std::slice::from_raw_parts(
                call.payload_json_utf8.cast::<u8>(),
                call.payload_length as usize,
            )
        };
        let heard = unsafe { &*(user as *const Heard) };
        heard.0.lock().unwrap().push((
            call.call_id,
            call.service,
            call.method,
            std::str::from_utf8(bytes).unwrap().to_owned(),
        ));
    }

    fn services_with(
        heard: &Heard,
        dispatch: migo_capi_abi::callbacks::MigoDispatchFn,
    ) -> (Arc<MigoSession>, Arc<CapiHostServices>) {
        let mut raw = MigoHostCallbacks::empty();
        raw.user_data = heard as *const Heard as *mut c_void;
        raw.dispatch = Some(dispatch);
        raw.on_host_service_call = Some(on_call);
        raw.host_services = MIGO_HOST_SERVICES_KNOWN;
        let session = callback_session_pin();
        let notifier = Arc::new(Notifier::new(
            raw.validate().expect("valid").expect("a dispatcher"),
            Arc::downgrade(&session),
        ));
        (session, CapiHostServices::new(notifier))
    }

    #[test]
    fn calls_and_commands_reach_the_host_with_their_numbers() {
        let heard = Heard::default();
        let (_session, services) = services_with(&heard, inline_dispatch);
        AuthService::login(&*services, r#"{"requestId":41,"timeout":0}"#).unwrap();
        AdService::show_ad(&*services, r#"{"adId":3}"#).unwrap();
        NavigateService::navigate_back_mini_program(&*services, r#"{"extraData":{}}"#).unwrap();
        let heard = heard.0.lock().unwrap();
        assert_eq!(
            *heard,
            vec![
                (
                    encode_call_id(&LOGIN, 41),
                    MIGO_HOST_SERVICE_AUTH,
                    MIGO_AUTH_LOGIN,
                    r#"{"timeout":0}"#.to_owned()
                ),
                (
                    0,
                    MIGO_HOST_SERVICE_AD,
                    MIGO_AD_SHOW,
                    r#"{"adId":3}"#.to_owned()
                ),
                (
                    0,
                    MIGO_HOST_SERVICE_NAVIGATE,
                    MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM,
                    r#"{"extraData":{}}"#.to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_refusing_dispatcher_fails_content_s_call_rather_than_stranding_it() {
        let heard = Heard::default();
        let (_session, services) = services_with(&heard, rejecting_dispatch);
        let error = AuthService::login(&*services, r#"{"requestId":1}"#).unwrap_err();
        assert!(
            error.to_string().contains("host dispatcher refused"),
            "{error}"
        );
        assert!(AdService::create_ad(&*services, r#"{"adId":1}"#).is_err());
        assert!(heard.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_malformed_request_is_an_engine_defect_not_a_host_call() {
        let heard = Heard::default();
        let (_session, services) = services_with(&heard, inline_dispatch);
        assert!(AuthService::login(&*services, r#"{"timeout":0}"#).is_err());
        assert!(heard.0.lock().unwrap().is_empty());
    }

    // ---- the entry points ----

    fn result(status: u32) -> MigoHostServiceResult {
        MigoHostServiceResult {
            header: VersionedHeader {
                struct_size: size_of::<MigoHostServiceResult>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            status,
            flags: 0,
            error_code: 0,
            message_length: 0,
            message_utf8: std::ptr::null(),
            payload_json_utf8: std::ptr::null(),
            payload_length: 0,
            reserved0: 0,
        }
    }

    unsafe extern "C" fn noop_call(_: *mut c_void, _: *mut c_void, _: *const MigoHostServiceCall) {}

    fn install(session: *mut MigoSession, services: u64) {
        let mut raw = MigoHostCallbacks::empty();
        raw.dispatch = Some(inline_dispatch);
        raw.on_host_service_call = Some(noop_call);
        raw.host_services = services;
        assert_eq!(
            unsafe { crate::migo_session_set_host_callbacks(session, &raw) },
            MIGO_OK
        );
    }

    #[test]
    fn completions_are_accepted_for_declared_services_and_refused_otherwise() {
        with_session("host-service-complete", |session| {
            install(session, 1 << MIGO_HOST_SERVICE_AUTH);
            let payload = br#"{"code":"abc"}"#;
            let mut success = result(MIGO_HOST_SERVICE_STATUS_OK);
            success.payload_json_utf8 = payload.as_ptr().cast();
            success.payload_length = payload.len() as u32;
            // No Host has started, so nobody is waiting: accepted, not an error.
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 1),
                        &success,
                    )
                },
                MIGO_OK
            );
            // Payment was never declared, so no call of it was ever made.
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&REQUEST_MIDAS_PAYMENT, 1),
                        &result(MIGO_HOST_SERVICE_STATUS_OK),
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            for bad_id in [0, 1, u64::MAX] {
                assert_eq!(
                    unsafe { migo_session_complete_host_service_call(session, bad_id, &success) },
                    MIGO_ERROR_INVALID_ARGUMENT
                );
            }
            let mut failure = result(MIGO_HOST_SERVICE_STATUS_FAIL);
            failure.flags = MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE;
            failure.error_code = 4;
            // A failure must say why.
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 2),
                        &failure,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            let reason = b"session expired";
            failure.message_utf8 = reason.as_ptr().cast();
            failure.message_length = reason.len() as u32;
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 2),
                        &failure,
                    )
                },
                MIGO_OK
            );
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 3),
                        std::ptr::null(),
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            let not_an_object = b"[]";
            let mut wrong = result(MIGO_HOST_SERVICE_STATUS_OK);
            wrong.payload_json_utf8 = not_an_object.as_ptr().cast();
            wrong.payload_length = 2;
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 4),
                        &wrong,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn events_are_checked_against_the_service_and_its_event_set() {
        with_session("host-service-event", |session| {
            install(session, 1 << MIGO_HOST_SERVICE_AD);
            let event = br#"{"adId":1,"event":"load"}"#;
            let post = |service, id, bytes: &[u8]| unsafe {
                migo_session_post_host_service_event(
                    session,
                    service,
                    id,
                    bytes.as_ptr().cast(),
                    bytes.len() as u32,
                )
            };
            assert_eq!(
                post(MIGO_HOST_SERVICE_AD, MIGO_AD_EVENT_LIFECYCLE, event),
                MIGO_OK
            );
            assert_eq!(
                post(MIGO_HOST_SERVICE_AD, 1, event),
                MIGO_ERROR_INVALID_ARGUMENT,
                "an event the ad service does not define"
            );
            assert_eq!(
                post(MIGO_HOST_SERVICE_AUTH, MIGO_AD_EVENT_LIFECYCLE, event),
                MIGO_ERROR_INVALID_ARGUMENT,
                "a service with no such event"
            );
            assert_eq!(
                post(MIGO_HOST_SERVICE_AD, MIGO_AD_EVENT_LIFECYCLE, b"[1]"),
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(
                post(MIGO_HOST_SERVICE_AD, MIGO_AD_EVENT_LIFECYCLE, &[0xff]),
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(
                unsafe {
                    migo_session_post_host_service_event(
                        std::ptr::null_mut(),
                        MIGO_HOST_SERVICE_AD,
                        0,
                        event.as_ptr().cast(),
                        event.len() as u32,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn an_undeclared_service_s_event_is_refused() {
        with_session("host-service-undeclared", |session| {
            install(session, 1 << MIGO_HOST_SERVICE_AUTH);
            let event = br#"{"adId":1,"event":"close","isEnded":true}"#;
            assert_eq!(
                unsafe {
                    migo_session_post_host_service_event(
                        session,
                        MIGO_HOST_SERVICE_AD,
                        MIGO_AD_EVENT_LIFECYCLE,
                        event.as_ptr().cast(),
                        event.len() as u32,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }
}
