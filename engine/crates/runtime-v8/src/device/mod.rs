//! Device service ops and ESM modules.
//!
//! This module provides cross-platform device APIs using trait-based services
//! injected via `HostOpState.device_services`.

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::services::Scope;

// ==================== Clipboard Ops ====================

#[op2(fast)]
pub fn op_set_clipboard_data(
    state: &mut OpState,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    match host
        .device_services
        .as_ref()
        .and_then(|services| services.clipboard())
    {
        Some(clipboard) => clipboard
            .set_data(&request_json)
            .map_err(JsErrorBox::generic),
        None => Err(JsErrorBox::generic("setClipboardData:fail not supported")),
    }
}

#[op2(fast)]
pub fn op_get_clipboard_data(
    state: &mut OpState,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    match host
        .device_services
        .as_ref()
        .and_then(|services| services.clipboard())
    {
        Some(clipboard) => clipboard
            .get_data(&request_json)
            .map_err(JsErrorBox::generic),
        None => Err(JsErrorBox::generic("getClipboardData:fail not supported")),
    }
}

// ==================== Battery Ops ====================

#[op2]
#[string]
pub fn op_get_battery_info(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(battery) = services.battery() {
            return battery.get_info_json().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getBatteryInfo:fail not supported"))
}

// ==================== Vibration Ops ====================

#[op2(fast)]
pub fn op_vibrate_short(
    state: &mut OpState,
    #[string] vibrate_type: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(vibration) = services.vibration() {
            return vibration
                .vibrate_short(&vibrate_type)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("vibrateShort:fail not supported"))
}

#[op2(fast)]
pub fn op_vibrate_long(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(vibration) = services.vibration() {
            return vibration.vibrate_long().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("vibrateLong:fail not supported"))
}

// ==================== Screen Ops ====================

/// The screen service, or `err_msg` when the host supplies none.
fn screen(
    state: &OpState,
    err_msg: &'static str,
) -> Result<std::sync::Arc<dyn shared::services::ScreenService>, JsErrorBox> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.screen())
        .ok_or_else(|| JsErrorBox::generic(err_msg))
}

#[op2(fast)]
pub fn op_get_screen_brightness(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    screen(state, "getScreenBrightness:fail not supported")?
        .get_brightness(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_set_screen_brightness(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    screen(state, "setScreenBrightness:fail not supported")?
        .set_brightness(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_set_keep_screen_on(state: &mut OpState, keep_on: bool) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(screen) = services.screen() {
            return screen
                .set_keep_screen_on(keep_on)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("setKeepScreenOn:fail not supported"))
}

#[op2(fast)]
pub fn op_set_device_orientation(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    screen(state, "setDeviceOrientation:fail not supported")?
        .set_orientation(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_get_screen_recording_state(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    screen(state, "getScreenRecordingState:fail not supported")?
        .get_screen_recording_state(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_start_screen_recording_observer(state: &mut OpState) -> Result<(), JsErrorBox> {
    screen(state, "onScreenRecordingStateChanged:fail not supported")?
        .start_screen_recording_observer()
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_stop_screen_recording_observer(state: &mut OpState) -> Result<(), JsErrorBox> {
    screen(state, "offScreenRecordingStateChanged:fail not supported")?
        .stop_screen_recording_observer()
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_set_visual_effect_on_capture(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    screen(state, "setVisualEffectOnCapture:fail not supported")?
        .set_visual_effect_on_capture(request_json)
        .map_err(JsErrorBox::generic)
}

// ==================== Debug Ops ====================

#[op2(fast)]
pub fn op_set_enable_debug(state: &mut OpState, enabled: bool) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(screen) = services.screen() {
            return screen
                .set_enable_debug(enabled)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("setEnableDebug:fail not supported"))
}

// ==================== Screen Capture Ops ====================

#[op2(fast)]
pub fn op_start_capture_screen(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(screen) = services.screen() {
            return screen.start_capture_screen().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "onUserCaptureScreen:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_stop_capture_screen(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(screen) = services.screen() {
            return screen.stop_capture_screen().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "offUserCaptureScreen:fail not supported",
    ))
}

// ==================== Device Motion Ops ====================

#[op2(fast)]
pub fn op_start_device_motion(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    host.device_services
        .as_ref()
        .and_then(|services| services.device_motion())
        .ok_or_else(|| JsErrorBox::generic("startDeviceMotionListening:fail not supported"))?
        .start(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_stop_device_motion(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(motion) = services.device_motion() {
            return motion.stop().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "stopDeviceMotionListening:fail not supported",
    ))
}

// ==================== Gyroscope Ops ====================

#[op2(fast)]
pub fn op_start_gyroscope(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    host.device_services
        .as_ref()
        .and_then(|services| services.gyroscope())
        .ok_or_else(|| JsErrorBox::generic("startGyroscope:fail not supported"))?
        .start(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_stop_gyroscope(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(gyro) = services.gyroscope() {
            return gyro.stop().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("stopGyroscope:fail not supported"))
}

// ==================== Compass Ops ====================

#[op2(fast)]
pub fn op_start_compass(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    host.device_services
        .as_ref()
        .and_then(|services| services.compass())
        .ok_or_else(|| JsErrorBox::generic("startCompass:fail not supported"))?
        .start(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_stop_compass(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(compass) = services.compass() {
            return compass.stop().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("stopCompass:fail not supported"))
}

// ==================== Accelerometer Ops ====================

#[op2(fast)]
pub fn op_start_accelerometer(
    state: &mut OpState,
    #[string] request_json: &str,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    host.device_services
        .as_ref()
        .and_then(|services| services.accelerometer())
        .ok_or_else(|| JsErrorBox::generic("startAccelerometer:fail not supported"))?
        .start(request_json)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub fn op_stop_accelerometer(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(accel) = services.accelerometer() {
            return accel.stop().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("stopAccelerometer:fail not supported"))
}

// ==================== Network Ops ====================

#[op2(fast)]
pub fn op_start_network_monitoring(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(network) = services.network() {
            return network.start_monitoring().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "onNetworkStatusChange:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_stop_network_monitoring(state: &mut OpState) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(network) = services.network() {
            return network.stop_monitoring().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "offNetworkStatusChange:fail not supported",
    ))
}

#[op2]
#[string]
pub fn op_get_network_type(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(network) = services.network() {
            return network.get_network_type_json().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getNetworkType:fail not supported"))
}

#[op2]
#[string]
pub fn op_get_local_ip_address(state: &mut OpState) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(network) = services.network() {
            return network.get_local_ip_json().map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getLocalIPAddress:fail not supported"))
}

// ==================== Location Ops ====================

#[op2(fast)]
pub fn op_get_location(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::UserLocation)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(location) = services.location() {
            return location
                .get_location(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getLocation:fail not supported"))
}

#[op2(fast)]
pub fn op_get_fuzzy_location(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    crate::permission::require_scope(state, Scope::UserLocation)?;
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(location) = services.location() {
            return location
                .get_fuzzy_location(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("getFuzzyLocation:fail not supported"))
}

// ==================== Scan Code Ops ====================

#[op2(fast)]
pub fn op_scan_code(state: &mut OpState, #[string] options_json: String) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(scan_code) = services.scan_code() {
            return scan_code
                .scan_code(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic("scanCode:fail not supported"))
}

// ==================== Extension Definition ====================

deno_core::extension!(
    host_v8_device,
    deps = [host_v8_base],
    ops = [
        // Clipboard
        op_set_clipboard_data,
        op_get_clipboard_data,
        // Battery
        op_get_battery_info,
        // Vibration
        op_vibrate_short,
        op_vibrate_long,
        // Screen
        op_get_screen_brightness,
        op_set_screen_brightness,
        op_set_keep_screen_on,
        op_set_device_orientation,
        op_start_capture_screen,
        op_stop_capture_screen,
        op_get_screen_recording_state,
        op_start_screen_recording_observer,
        op_stop_screen_recording_observer,
        op_set_visual_effect_on_capture,
        // Debug
        op_set_enable_debug,
        // Device Motion
        op_start_device_motion,
        op_stop_device_motion,
        // Gyroscope
        op_start_gyroscope,
        op_stop_gyroscope,
        // Compass
        op_start_compass,
        op_stop_compass,
        // Accelerometer
        op_start_accelerometer,
        op_stop_accelerometer,
        // Network
        op_start_network_monitoring,
        op_stop_network_monitoring,
        op_get_network_type,
        op_get_local_ip_address,
        // Location
        op_get_location,
        op_get_fuzzy_location,
        // Scan Code
        op_scan_code,
    ],
    esm_entry_point = "ext:host_v8_device/99_global_scope.js",
    esm = [
        dir "src/device",
        "01_device_motion.js",
        "02_gyroscope.js",
        "03_orientation.js",
        "04_compass.js",
        "05_accelerometer.js",
        "06_battery.js",
        "07_clipboard.js",
        "08_vibrate.js",
        "09_screen.js",
        "10_network.js",
        "11_memory.js",
        "12_location.js",
        "13_scan_code.js",
        "99_global_scope.js",
    ]
);

pub fn device_extensions() -> Vec<Extension> {
    vec![host_v8_device::init()]
}

pub fn device_lazy_extensions() -> Vec<Extension> {
    vec![host_v8_device::lazy_init()]
}
