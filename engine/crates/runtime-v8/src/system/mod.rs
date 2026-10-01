//! System info ops and ESM modules.
//!
//! This module provides cross-platform system information APIs (window info,
//! device info, system settings, bluetooth/authorization settings) using
//! trait-based services injected via `HostOpState.device_services`.

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::services::{Scope, ScopeState};

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

// ==================== Bluetooth Adapter ====================

#[op2(fast)]
pub fn op_open_bluetooth_adapter(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.open_adapter(&options_json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "openBluetoothAdapter:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_close_bluetooth_adapter(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.close_adapter().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "closeBluetoothAdapter:fail not supported",
    ))
}

#[op2]
#[string]
pub fn op_get_bluetooth_adapter_state(state: &mut OpState) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.get_adapter_state().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "getBluetoothAdapterState:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_start_bluetooth_devices_discovery(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .start_devices_discovery(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "startBluetoothDevicesDiscovery:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_stop_bluetooth_devices_discovery(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.stop_devices_discovery().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "stopBluetoothDevicesDiscovery:fail not supported",
    ))
}

#[op2]
#[string]
pub fn op_get_bluetooth_devices(state: &mut OpState) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.get_devices().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "getBluetoothDevices:fail not supported",
    ))
}

#[op2]
#[string]
pub fn op_get_connected_bluetooth_devices(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .get_connected_devices(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "getConnectedBluetoothDevices:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_make_bluetooth_pair(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.make_pair(&options_json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("makeBluetoothPair:fail not supported"))
}

#[op2(fast)]
pub fn op_is_bluetooth_device_paired(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .is_device_paired(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "isBluetoothDevicePaired:fail not supported",
    ))
}

// ==================== BLE GATT ====================

#[op2(fast)]
pub fn op_create_ble_connection(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .create_ble_connection(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "createBLEConnection:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_close_ble_connection(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .close_ble_connection(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("closeBLEConnection:fail not supported"))
}

#[op2]
#[string]
pub fn op_get_ble_device_services(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .get_ble_device_services(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "getBLEDeviceServices:fail not supported",
    ))
}

#[op2]
#[string]
pub fn op_get_ble_device_characteristics(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .get_ble_device_characteristics(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "getBLEDeviceCharacteristics:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_read_ble_characteristic_value(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .read_ble_characteristic_value(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "readBLECharacteristicValue:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_write_ble_characteristic_value(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .write_ble_characteristic_value(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "writeBLECharacteristicValue:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_notify_ble_characteristic_value_change(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .notify_ble_characteristic_value_change(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "notifyBLECharacteristicValueChange:fail not supported",
    ))
}

#[op2]
#[string]
pub fn op_get_ble_device_rssi(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .get_ble_device_rssi(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getBLEDeviceRSSI:fail not supported"))
}

#[op2(fast)]
pub fn op_set_ble_mtu(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.set_ble_mtu(&options_json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("setBLEMTU:fail not supported"))
}

#[op2]
#[string]
pub fn op_get_ble_mtu(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.get_ble_mtu(&options_json).map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getBLEMTU:fail not supported"))
}

// ==================== Beacon ====================

#[op2(fast)]
pub fn op_start_beacon_discovery(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt
                .start_beacon_discovery(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "startBeaconDiscovery:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_stop_beacon_discovery(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.stop_beacon_discovery().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "stopBeaconDiscovery:fail not supported",
    ))
}

#[op2]
#[string]
pub fn op_get_beacons(state: &mut OpState) -> Result<String, JsErrorBox> {
    crate::permission::require_scope(state, Scope::Bluetooth)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(bt) = services.bluetooth() {
            return bt.get_beacons().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getBeacons:fail not supported"))
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
    let mut out = String::from("{");
    for (index, scope) in Scope::ALL.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let granted = crate::permission::scope_state(state, *scope) == ScopeState::Granted;
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
        "16_jssdk.js",
        "17_analytics.js",
        "18_crypto.js",
        "19_log_manager.js",
        "99_global_scope.js",
    ]
);

pub fn system_extensions() -> Vec<Extension> {
    vec![host_v8_system::init()]
}
