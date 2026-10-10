//! System information service traits for window info, device info, settings, and authorization.

use crate::protocol::error::ServiceError;

/// The `platform` string `getSystemInfoSync()` and `getDeviceInfo()` carry, for the operating system this
/// engine was built for: `"android"`, `"ios"`, `"mac"`, `"windows"`, `"linux"` or `"ohos"`.
///
/// What a host that does not describe its device answers with. It used to be `"android"` whatever the
/// build, so content on a desktop or an iPhone branched as if it ran on Android. The build target is not
/// a guess: the engine knows what it was compiled for.
pub const fn host_platform_name() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(all(target_os = "linux", target_env = "ohos")) {
        "ohos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// The name of that operating system as `getSystemInfoSync().system` leads with it. A host that knows the
/// version appends it (`"Android 14"`); this is the part the build knows.
pub const fn host_os_name() -> &'static str {
    if cfg!(target_os = "android") {
        "Android"
    } else if cfg!(target_os = "ios") {
        "iOS"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(all(target_os = "linux", target_env = "ohos")) {
        "OpenHarmony"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "Unknown"
    }
}

/// The device info a host that has none to give answers with: only what the build knows (the platform, the OS
/// name, the CPU architecture), and nothing it would have to invent -- `model` stays `"unknown"`, `brand`
/// and the sizes empty, `benchmarkLevel` `-1`. The fields are the ones `get_device_info_json` documents.
pub fn default_device_info_json() -> String {
    let arch = std::env::consts::ARCH;
    format!(
        r#"{{"abi":"{arch}","deviceAbi":"{arch}","benchmarkLevel":-1,"brand":"","model":"unknown","system":"{}","platform":"{}","cpuType":"","memorySize":""}}"#,
        host_os_name(),
        host_platform_name(),
    )
}

/// System information and settings service.
///
/// Provides cross-platform access to device info, window info, system settings,
/// and system-level operations like opening bluetooth/authorization settings.
pub trait SystemInfoService: Send + Sync {
    /// Open the system Bluetooth settings page.
    fn open_bluetooth_settings(&self, _request_id: i32) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "openSystemBluetoothSetting:fail not supported",
        ))
    }

    /// Open the app authorization settings page.
    fn open_app_authorize_setting(&self, _request_id: i32) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "openAppAuthorizeSetting:fail not supported",
        ))
    }

    /// The bounding client rect of the host's menu button (the capsule), in the
    /// window's CSS pixels: `width`, `height`, `top`, `bottom`, `left`, `right`.
    ///
    /// A host has a menu button only when it says so by overriding this. One
    /// that does not has none: an empty rect at the top-right corner of the
    /// safe area, so content that lays itself out around the button loses
    /// nothing. A rect made up to look like the common platform's capsule --
    /// what this once answered, sized for a 375-pixel-wide window -- would have
    /// content leave room for a button that is not there, wherever it is not.
    fn get_menu_button_bounding_client_rect_json(&self) -> Result<String, ServiceError> {
        let window: crate::surface::WindowInfo =
            serde_json::from_str(&self.get_window_info_json()?).map_err(|error| {
                ServiceError::system(format!("getMenuButtonBoundingClientRect:fail {error}"))
            })?;
        let right = window.window_width - window.safe_area.right;
        let top = window.safe_area.top;
        Ok(serde_json::json!({
            "width": 0, "height": 0, "top": top, "bottom": top, "left": right, "right": right,
        })
        .to_string())
    }

    /// Get window info as JSON string.
    ///
    /// Expected JSON fields: `pixel_ratio`, `screen_width`, `screen_height`,
    /// `window_width`, `window_height`, `status_bar_height`, `screen_top`,
    /// `safe_area: { left, top, right, bottom }`.
    fn get_window_info_json(&self) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getWindowInfo:fail not supported",
        ))
    }

    /// Get system settings as JSON string.
    ///
    /// Expected JSON fields: `bluetooth_enabled`, `location_enabled`,
    /// `wifi_enabled`, `orientation`.
    fn get_system_settings_json(&self) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getSystemSetting:fail not supported",
        ))
    }

    /// Get device info as JSON string.
    ///
    /// Expected JSON fields: `abi`, `deviceAbi`, `benchmarkLevel`, `brand`,
    /// `model`, `system`, `platform`, `cpuType`, `memorySize`.
    ///
    /// Defaults to what the build knows ([`default_device_info_json`]); a host that can say more (a brand, a
    /// model, an OS version) overrides it.
    fn get_device_info_json(&self) -> Result<String, ServiceError> {
        Ok(default_device_info_json())
    }

    /// Open the mini program setting page (Mode C, async).
    ///
    /// Result delivered via `onOpenSettingResult` callback with JSON:
    /// `{"authSetting":{"scope.userInfo":true,...}}`
    fn open_setting(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "openSetting:fail not supported",
        ))
    }

    /// Get app authorization setting as JSON string.
    ///
    /// Expected JSON fields: `albumAuthorized`, `bluetoothAuthorized`,
    /// `cameraAuthorized`, `locationAuthorized`, `microphoneAuthorized`, etc.
    fn get_app_authorization_setting_json(&self) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getAppAuthorizeSetting:fail not supported",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that attached no device services is answered with the platform it is, not another's.
    #[test]
    fn the_default_names_the_platform_this_build_is_for() {
        let json: serde_json::Value = serde_json::from_str(&default_device_info_json()).unwrap();
        assert_eq!(json["platform"], host_platform_name());
        assert!(
            json["system"].as_str().unwrap().starts_with(host_os_name()),
            "{json}"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(json["platform"], "mac");
        #[cfg(target_os = "windows")]
        assert_eq!(json["platform"], "windows");
        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        assert_eq!(json["platform"], "linux");
        #[cfg(target_os = "android")]
        assert_eq!(json["platform"], "android");
        #[cfg(target_os = "ios")]
        assert_eq!(json["platform"], "ios");
        // The one thing it must never be: a name for a platform this is not.
        let not_this = if cfg!(target_os = "android") {
            "ios"
        } else {
            "android"
        };
        assert_ne!(json["platform"], not_this);
    }

    /// The fields content reads are all present, and nothing is invented: the model is unknown, the
    /// brand empty and the benchmark level the "not measured" `-1`.
    #[test]
    fn the_default_has_the_documented_fields_and_invents_nothing() {
        let json: serde_json::Value = serde_json::from_str(&default_device_info_json()).unwrap();
        for field in [
            "abi",
            "deviceAbi",
            "benchmarkLevel",
            "brand",
            "model",
            "system",
            "platform",
            "cpuType",
            "memorySize",
        ] {
            assert!(json.get(field).is_some(), "missing {field}: {json}");
        }
        assert_eq!(json["model"], "unknown");
        assert_eq!(json["brand"], "");
        assert_eq!(json["benchmarkLevel"], -1);
        assert_eq!(json["abi"], std::env::consts::ARCH);
    }

    /// A service that does not describe the device gets the default through the trait, so the desktop
    /// hosts that implement only window info answer `getDeviceInfo()` truthfully too.
    #[test]
    fn a_service_that_does_not_override_the_device_info_answers_with_the_default() {
        struct WindowOnly;
        impl SystemInfoService for WindowOnly {}
        assert_eq!(
            WindowOnly.get_device_info_json().unwrap(),
            default_device_info_json()
        );
    }
}
