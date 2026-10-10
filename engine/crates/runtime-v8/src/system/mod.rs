//! System info ops and ESM modules.
//!
//! This module provides cross-platform system information APIs (window info,
//! device info, system settings, bluetooth/authorization settings) using
//! trait-based services injected via `HostOpState.device_services`.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::services::host_files::Step;
use shared::services::{Scope, ScopeState};

use crate::file::host_paths::{Remote, export_paths};

// ==================== Bluetooth Settings ====================

#[op2(fast)]
pub fn op_open_system_bluetooth_setting(
    state: &mut OpState,
    #[smi] request_id: i32,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(sys) = services.system_info() {
            return sys
                .open_bluetooth_settings(request_id)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "openSystemBluetoothSetting:fail not supported",
    ))
}

// ==================== Bluetooth ====================
//
// Every operation is a request the host answers through the hook
// `shared::services::BLUETOOTH_RESULT_HOOKS` names. Using Bluetooth needs
// `scope.bluetooth`; releasing what was acquired under it does not, so a
// revocation never traps a scan or a connection.

/// The Bluetooth service, or `err_msg` when the host supplies none.
fn bluetooth(
    state: &OpState,
    err_msg: &'static str,
) -> Result<std::sync::Arc<dyn shared::services::BluetoothService>, JsErrorBox> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.bluetooth())
        .ok_or_else(|| JsErrorBox::generic(err_msg))
}

#[op2(fast)]
pub fn op_open_bluetooth_adapter(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "openBluetoothAdapter:fail not supported")?
        .open_adapter(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_close_bluetooth_adapter(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    bluetooth(state, "closeBluetoothAdapter:fail not supported")?
        .close_adapter(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_bluetooth_adapter_state(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "getBluetoothAdapterState:fail not supported")?
        .get_adapter_state(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_start_bluetooth_devices_discovery(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "startBluetoothDevicesDiscovery:fail not supported")?
        .start_devices_discovery(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_stop_bluetooth_devices_discovery(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    bluetooth(state, "stopBluetoothDevicesDiscovery:fail not supported")?
        .stop_devices_discovery(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_bluetooth_devices(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "getBluetoothDevices:fail not supported")?
        .get_devices(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_connected_bluetooth_devices(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "getConnectedBluetoothDevices:fail not supported")?
        .get_connected_devices(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_make_bluetooth_pair(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "makeBluetoothPair:fail not supported")?
        .make_pair(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_is_bluetooth_device_paired(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "isBluetoothDevicePaired:fail not supported")?
        .is_device_paired(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_start_beacon_discovery(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "startBeaconDiscovery:fail not supported")?
        .start_beacon_discovery(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_stop_beacon_discovery(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    bluetooth(state, "stopBeaconDiscovery:fail not supported")?
        .stop_beacon_discovery(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_beacons(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "getBeacons:fail not supported")?
        .get_beacons(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_create_ble_connection(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "createBLEConnection:fail not supported")?
        .create_ble_connection(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_close_ble_connection(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    bluetooth(state, "closeBLEConnection:fail not supported")?
        .close_ble_connection(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_ble_device_services(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "getBLEDeviceServices:fail not supported")?
        .get_ble_device_services(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_ble_device_characteristics(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "getBLEDeviceCharacteristics:fail not supported")?
        .get_ble_device_characteristics(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_read_ble_characteristic_value(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "readBLECharacteristicValue:fail not supported")?
        .read_ble_characteristic_value(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_write_ble_characteristic_value(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "writeBLECharacteristicValue:fail not supported")?
        .write_ble_characteristic_value(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_notify_ble_characteristic_value_change(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(
        state,
        "notifyBLECharacteristicValueChange:fail not supported",
    )?
    .notify_ble_characteristic_value_change(request_json)
    .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_ble_device_rssi(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "getBLEDeviceRSSI:fail not supported")?
        .get_ble_device_rssi(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_set_ble_mtu(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "setBLEMTU:fail not supported")?
        .set_ble_mtu(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_ble_mtu(state: &mut OpState, #[string] request_json: &str) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    bluetooth(state, "getBLEMTU:fail not supported")?
        .get_ble_mtu(request_json)
        .map_err(JsErrorBox::generic)
}

// ==================== Ecosystem ====================
//
// The host's ecosystem features, routed by content API name (the closed list in
// `20_ecosystem.js`, which the host-service contract names).

fn ecosystem(state: &OpState) -> Option<std::sync::Arc<dyn shared::services::EcosystemService>> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.ecosystem())
}

/// Whether the host offers ecosystem features at all: the few APIs with a true
/// answer for a host that has none -- no privacy agreement to accept, not in a
/// chat tool -- give it only then.
#[op2(fast)]
pub fn op_ecosystem_available(state: &mut OpState) -> bool {
    ecosystem(state).is_some_and(|service| service.available())
}

/// The ecosystem requests that name content's files, and where. Which fields
/// are files is the engine's to know, not content's to declare: each is resolved
/// through the sandbox before the request leaves (`crate::file::host_paths`).
const ECOSYSTEM_FILES: &[(&str, &[&[Step]])] = &[
    (
        "shareImageToGroup",
        &[&[Step::Key("options"), Step::Key("imagePath")]],
    ),
    (
        "shareEmojiToGroup",
        &[&[Step::Key("options"), Step::Key("imagePath")]],
    ),
    (
        "shareVideoToGroup",
        &[
            &[Step::Key("options"), Step::Key("videoPath")],
            &[Step::Key("options"), Step::Key("thumbPath")],
        ],
    ),
];

/// `{"requestId", "api", "options"}` for the API `api` names, answered through
/// `_internalOnEcosystemResult`. Eager: a request that names no package entry
/// leaves in the call's own tick, and only one that names files is parsed here.
#[op2]
pub async fn op_ecosystem_call(
    state: Rc<RefCell<OpState>>,
    #[string] api: String,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service = ecosystem(&state.borrow()).ok_or_else(|| JsErrorBox::generic("not supported"))?;
    let request = match ECOSYSTEM_FILES.iter().find(|(name, _)| *name == api) {
        Some((_, fields)) => export_paths(&state, &request_json, fields, Remote::Refused).await?,
        None => request_json,
    };
    service.call(&request).map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_ecosystem_reply(state: &mut OpState, #[string] json: &str) -> Result<(), JsErrorBox> {
    ecosystem(state)
        .ok_or_else(|| JsErrorBox::generic("not supported"))?
        .reply(json)
        .map_err(JsErrorBox::generic)
}

/// The host's value for a synchronous getter, or `null`.
#[op2]
#[string]
pub fn op_ecosystem_value(state: &mut OpState, #[string] name: &str) -> Option<String> {
    ecosystem(state)?.value(name)
}

// ==================== Open Setting (Mode C) ====================

// ==================== Permission (getSetting / authorize) ====================

/// Report the host's decision for every scope.
///
/// Backs `migo.getSetting()`. Returns the platform-shaped map, e.g.
/// `{"scope.camera":true,"scope.record":false}`.
///
/// Only `Granted` becomes `true`. `Unknown` reports `false` because content
/// reads this as "may I", and "nobody has been asked" is not a yes -- the
/// difference between never-asked and refused matters for whether to prompt,
/// which is `authorize`'s business, not this one's.
///
/// The map this replaces was a JavaScript object with every scope hardcoded to
/// `true`, so content was told it held permissions nobody had granted.
#[op2]
#[string]
pub fn op_get_auth_setting(state: &mut OpState) -> String {
    // Only scopes somebody has decided: a granted one is `true`, a refused one
    // `false`, and one nobody has been asked about is absent. Content tells the
    // last two apart -- the mini-game convention does not re-prompt after a
    // refusal, so `false` sends the player to openSetting while an absent scope
    // is one to authorize -- and reporting the undecided as `false` made every
    // game that followed it send the player to settings for a question it had
    // never asked.
    let mut out = String::from("{");
    let mut first = true;
    for scope in Scope::ALL {
        let granted = match crate::permission::scope_state(state, *scope) {
            ScopeState::Granted => true,
            ScopeState::Denied => false,
            ScopeState::Unknown => continue,
        };
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        out.push_str(scope.as_minigame_str());
        out.push_str("\":");
        out.push_str(if granted { "true" } else { "false" });
    }
    out.push('}');
    out
}

/// Ask the host to decide a scope, prompting the user if it sees fit.
///
/// Backs `migo.authorize()`. Mode C: the reply arrives on the permission-result
/// channel, because there may be a human in the loop.
///
/// Fails when no host permission service is installed rather than reporting
/// success. `migo.authorize()` exists to obtain consent; returning success with
/// nobody asked is the defect this replaces.
#[op2(fast)]
pub fn op_authorize(state: &mut OpState, #[string] request_json: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(permission) = services.permission() {
            return permission
                .request_scope(&request_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("authorize:fail no permission handler"))
}

#[op2(fast)]
pub fn op_open_setting(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(sys) = services.system_info() {
            return sys.open_setting(&options_json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("openSetting:fail not supported"))
}

// ==================== Navigate (Mode C / A) ====================

#[op2(fast)]
pub fn op_navigate_to_mini_program(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(nav) = services.navigate() {
            return nav
                .navigate_to_mini_program(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "navigateToMiniProgram:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_navigate_back_mini_program(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(nav) = services.navigate() {
            return nav
                .navigate_back_mini_program(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "navigateBackMiniProgram:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_open_customer_service_conversation(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(nav) = services.navigate() {
            return nav
                .open_customer_service_conversation(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "openCustomerServiceConversation:fail not supported",
    ))
}

// ==================== App Authorize Setting ====================

#[op2(fast)]
pub fn op_open_app_authorize_setting(
    state: &mut OpState,
    #[smi] request_id: i32,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(sys) = services.system_info() {
            return sys
                .open_app_authorize_setting(request_id)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "openAppAuthorizeSetting:fail not supported",
    ))
}

// ==================== Login ====================

#[op2(fast)]
pub fn op_login(state: &mut OpState, #[string] options_json: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(auth) = services.auth() {
            return auth.login(&options_json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("login:fail not supported"))
}

#[op2(fast)]
pub fn op_check_session(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(auth) = services.auth() {
            return auth
                .check_session(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("checkSession:fail not supported"))
}

#[op2(fast)]
pub fn op_get_user_info(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::UserInfo)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(auth) = services.auth() {
            return auth
                .get_user_info(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getUserInfo:fail not supported"))
}

#[op2(fast)]
pub fn op_get_phone_number(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(auth) = services.auth() {
            return auth
                .get_phone_number(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getPhoneNumber:fail not supported"))
}

// ==================== Window Info ====================

#[op2]
#[string]
pub fn op_get_window_info(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(sys) = services.system_info() {
            return sys.get_window_info_json().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getWindowInfo:fail not supported"))
}

// ==================== System Settings ====================

#[op2]
#[string]
pub fn op_get_system_settings(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(sys) = services.system_info() {
            return sys.get_system_settings_json().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getSystemSetting:fail not supported"))
}

// ==================== Device Info ====================

#[op2]
#[string]
pub fn op_get_device_info(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(sys) = services.system_info() {
            return sys.get_device_info_json().map_err(JsErrorBox::generic);
        }
    }
    // No device services at all (an embedder that attached none): the engine still knows what it was
    // built for, and a content branch on `platform` must not be handed another platform's name.
    Ok(shared::services::default_device_info_json())
}

// ==================== App Authorization Setting ====================

#[op2]
#[string]
pub fn op_get_app_authorization_setting(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(sys) = services.system_info() {
            return sys
                .get_app_authorization_setting_json()
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "getAppAuthorizeSetting:fail not supported",
    ))
}

// ==================== Game Log ====================

#[op2(fast)]
pub fn op_game_log_report(
    state: &mut OpState,
    #[string] log_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(svc) = services.game_log() {
            return svc.report_log(&log_json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("gameLog.log:fail not supported"))
}

// ==================== Extension Definition ====================

deno_core::extension!(
    host_v8_system,
    deps = [host_v8_base],
    ops = [
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
        op_start_beacon_discovery,
        op_stop_beacon_discovery,
        op_get_beacons,
        op_open_app_authorize_setting,
        op_login,
        op_check_session,
        op_get_user_info,
        op_get_phone_number,
        op_get_window_info,
        op_get_system_settings,
        op_get_device_info,
        op_get_app_authorization_setting,
        op_game_log_report,
        op_get_auth_setting,
        op_ecosystem_available,
        op_ecosystem_call,
        op_ecosystem_reply,
        op_ecosystem_value,
        op_authorize,
        op_open_setting,
        op_navigate_to_mini_program,
        op_navigate_back_mini_program,
        op_open_customer_service_conversation,
    ],
    esm_entry_point = "ext:host_v8_system/99_global_scope.js",
    esm = [
        dir "src/system",
        "01_bluetooth.js",
        "02_authorize.js",
        "03_window_info.js",
        "04_system_settings.js",
        "05_device_info.js",
        "06_benchmark_level.js",
        "07_app_info.js",
        "08_authorize_setting.js",
        "09_game_log.js",
        "10_system_info.js",
        "11_open_data_context.js",
        "12_window_resize.js",
        "13_login.js",
        "14_setting.js",
        "15_navigate.js",
        "17_analytics.js",
        "18_crypto.js",
        "19_log_manager.js",
        "20_ecosystem.js",
        "99_global_scope.js",
    ]
);

pub fn system_extensions() -> Vec<Extension> {
    vec![host_v8_system::init()]
}
