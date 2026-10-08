//! Device service traits for sensors, battery, vibration, and screen.

use std::sync::Arc;

use crate::protocol::error::ServiceError;

use super::{
    AdService, AuthService, CameraService, ClipboardService, CodecService, FileService,
    GameLogService, ImageApiService, InteractionService, LocationService, NavigateService,
    NetworkService, PaymentService, PermissionService, ScanCodeService, ShareService,
    SubpackageService, SystemInfoService, VideoService,
};

// ==================== Battery ====================

/// Battery information service.
pub trait BatteryService: Send + Sync {
    /// Get battery info as JSON: `{"level": 80, "isCharging": true}`
    fn get_info_json(&self) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getBatteryInfo:fail not supported",
        ))
    }
}

// ==================== Vibration ====================

/// Vibration service for haptic feedback.
pub trait VibrationService: Send + Sync {
    /// Short vibration (15ms). type_: "heavy", "medium", "light"
    fn vibrate_short(&self, _type_: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "vibrateShort:fail not supported",
        ))
    }

    /// Long vibration (400ms).
    fn vibrate_long(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "vibrateLong:fail not supported",
        ))
    }
}

// ==================== Screen ====================

/// Screen brightness and orientation service.
pub trait ScreenService: Send + Sync {
    /// Get screen brightness (0.0-1.0).
    fn get_brightness(&self) -> Result<f32, ServiceError> {
        Err(ServiceError::not_supported(
            "getScreenBrightness:fail not supported",
        ))
    }

    /// Set screen brightness (0.0-1.0).
    fn set_brightness(&self, _value: f32) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setScreenBrightness:fail not supported",
        ))
    }

    /// Set keep screen on.
    fn set_keep_screen_on(&self, _keep_on: bool) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setKeepScreenOn:fail not supported",
        ))
    }

    /// Set device orientation: "portrait", "landscape", "landscapeReverse"
    fn set_orientation(&self, _value: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setDeviceOrientation:fail not supported",
        ))
    }

    /// Start observing user screenshot events.
    fn start_capture_screen(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "onUserCaptureScreen:fail not supported",
        ))
    }

    /// Stop observing user screenshot events.
    fn stop_capture_screen(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "offUserCaptureScreen:fail not supported",
        ))
    }

    /// Set whether to enable debug mode at runtime.
    fn set_enable_debug(&self, _enabled: bool) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setEnableDebug:fail not supported",
        ))
    }
}

// ==================== Device Motion ====================

/// Device motion sensor (rotation angles).
pub trait DeviceMotionService: Send + Sync {
    /// Start listening. interval: "game" (20ms), "ui" (60ms), "normal" (200ms)
    fn start(&self, _interval: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "startDeviceMotionListening:fail not supported",
        ))
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "stopDeviceMotionListening:fail not supported",
        ))
    }
}

// ==================== Gyroscope ====================

/// Gyroscope sensor (angular velocity rad/s).
pub trait GyroscopeService: Send + Sync {
    fn start(&self, _interval: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "startGyroscope:fail not supported",
        ))
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "stopGyroscope:fail not supported",
        ))
    }
}

// ==================== Compass ====================

/// Compass sensor (magnetic heading degrees).
pub trait CompassService: Send + Sync {
    fn start(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "startCompass:fail not supported",
        ))
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "stopCompass:fail not supported",
        ))
    }
}

// ==================== Accelerometer ====================

/// Accelerometer sensor (acceleration m/s²).
pub trait AccelerometerService: Send + Sync {
    fn start(&self, _interval: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "startAccelerometer:fail not supported",
        ))
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "stopAccelerometer:fail not supported",
        ))
    }
}

// ==================== Audio Platform ====================

/// Audio platform service for device-level audio configuration.
///
/// Handles operations that require platform AudioManager access,
/// such as audio focus, speaker routing, and input source queries.
pub trait AudioPlatformService: Send + Sync {
    /// Configure inner audio behavior.
    ///
    /// - `mix_with_other`: Allow mixing with other audio apps (Android: abandon focus vs duck)
    /// - `obey_mute_switch`: Respect device mute/ringer mode
    /// - `speaker_on`: Route audio to speaker (vs earpiece/headset)
    fn set_inner_audio_option(
        &self,
        _mix_with_other: bool,
        _obey_mute_switch: bool,
        _speaker_on: bool,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setInnerAudioOption:fail not supported",
        ))
    }

    /// Get available audio input sources.
    ///
    /// Returns source identifiers matching `RecorderManager.start()` audioSource param.
    /// Maps to Android `MediaRecorder.AudioSource` constants:
    /// - "auto" (DEFAULT=0)
    /// - "buildInMic" (built-in microphone)
    /// - "headsetMic" (headset microphone, if connected)
    /// - "mic" (MIC=1)
    /// - "camcorder" (CAMCORDER=5)
    /// - "voice_recognition" (VOICE_RECOGNITION=6)
    /// - "voice_communication" (VOICE_COMMUNICATION=7)
    fn get_available_audio_sources(&self) -> Result<Vec<String>, ServiceError> {
        Err(ServiceError::not_supported(
            "getAvailableAudioSources:fail not supported",
        ))
    }
}

// ==================== Recorder ====================

/// Audio recording service (microphone input, encoding, file output).
///
/// Manages the platform media recorder lifecycle. Commands are fire-and-forget;
/// results and state changes are delivered asynchronously via `HostCommand::RecorderEvent`
/// and `HostCommand::RecorderFrameData`.
pub trait RecorderService: Send + Sync {
    /// Start recording with the given options (JSON-encoded).
    ///
    /// JSON fields:
    /// - `duration`: max recording duration in ms (default 60000, max 600000)
    /// - `sampleRate`: sample rate Hz (8000,11025,12000,16000,22050,24000,32000,44100,48000)
    /// - `numberOfChannels`: 1 or 2 (default 2)
    /// - `encodeBitRate`: encode bit rate in bps (default 48000)
    /// - `format`: "mp3", "aac", "wav", "PCM" (default "aac")
    /// - `frameSize`: frame size in KB (if set, triggers onFrameRecorded)
    /// - `audioSource`: "auto","buildInMic","headsetMic","mic","camcorder",
    ///                   "voice_recognition","voice_communication" (default "auto")
    fn start(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "recorderManager.start:fail not supported",
        ))
    }

    /// Pause recording.
    fn pause(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "recorderManager.pause:fail not supported",
        ))
    }

    /// Resume recording after pause.
    fn resume(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "recorderManager.resume:fail not supported",
        ))
    }

    /// Stop recording. The platform will fire a RecorderEvent("stop", ...) with the file path.
    fn stop(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "recorderManager.stop:fail not supported",
        ))
    }
}

// ==================== Keyboard ====================

/// Soft keyboard service for text input.
pub trait KeyboardService: Send + Sync {
    /// Show the soft keyboard with options (JSON-encoded).
    ///
    /// JSON fields:
    /// - `defaultValue`: default text value
    /// - `maxLength`: max input length
    /// - `multiple`: multi-line input
    /// - `confirmHold`: keep keyboard on confirm
    /// - `confirmType`: confirm button type ("done","next","search","go","send")
    /// - `keyboardType`: keyboard type ("text","number")
    fn show(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "showKeyboard:fail not supported",
        ))
    }

    /// Hide the soft keyboard.
    fn hide(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "hideKeyboard:fail not supported",
        ))
    }

    /// Update the keyboard input value.
    fn update(&self, _value: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "updateKeyboard:fail not supported",
        ))
    }
}

// ==================== Bluetooth ====================

/// Bluetooth service for BLE and Beacon operations.
pub trait BluetoothService: Send + Sync {
    /// Initialize the Bluetooth adapter.
    ///
    /// JSON fields:
    /// - `mode`: "central" (default) or "peripheral" (iOS only)
    fn open_adapter(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "openBluetoothAdapter:fail not supported",
        ))
    }

    /// Close the Bluetooth adapter and release resources.
    fn close_adapter(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "closeBluetoothAdapter:fail not supported",
        ))
    }

    /// Get Bluetooth adapter state.
    /// Returns JSON: `{"discovering": bool, "available": bool}`
    fn get_adapter_state(&self) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getBluetoothAdapterState:fail not supported",
        ))
    }

    /// Start scanning for BLE devices.
    ///
    /// JSON fields:
    /// - `services`: array of service UUID strings to filter
    /// - `allowDuplicatesKey`: bool (default false)
    /// - `interval`: number in ms (default 0)
    /// - `powerLevel`: "low" | "medium" | "high" (default "medium")
    fn start_devices_discovery(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "startBluetoothDevicesDiscovery:fail not supported",
        ))
    }

    /// Stop scanning for BLE devices.
    fn stop_devices_discovery(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "stopBluetoothDevicesDiscovery:fail not supported",
        ))
    }

    /// Get all discovered Bluetooth devices.
    /// Returns JSON: `{"devices": [...]}`
    fn get_devices(&self) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getBluetoothDevices:fail not supported",
        ))
    }

    /// Get connected Bluetooth devices by service UUIDs.
    ///
    /// JSON fields:
    /// - `services`: array of service UUID strings
    ///
    /// Returns JSON: `{"devices": [...]}`
    fn get_connected_devices(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getConnectedBluetoothDevices:fail not supported",
        ))
    }

    /// Pair with a Bluetooth device (Android only).
    ///
    /// JSON fields:
    /// - `deviceId`: string
    /// - `pin`: string (Base64)
    /// - `timeout`: number in ms (default 20000)
    fn make_pair(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "makeBluetoothPair:fail not supported",
        ))
    }

    /// Check if a Bluetooth device is paired (Android only).
    ///
    /// JSON fields:
    /// - `deviceId`: string
    fn is_device_paired(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "isBluetoothDevicePaired:fail not supported",
        ))
    }

    /// Start Beacon discovery.
    ///
    /// JSON fields:
    /// - `uuids`: array of UUID strings
    /// - `ignoreBluetoothAvailable`: bool (default false)
    fn start_beacon_discovery(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "startBeaconDiscovery:fail not supported",
        ))
    }

    /// Stop Beacon discovery.
    fn stop_beacon_discovery(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "stopBeaconDiscovery:fail not supported",
        ))
    }

    /// Get all discovered Beacon devices.
    /// Returns JSON: `{"beacons": [...]}`
    fn get_beacons(&self) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported("getBeacons:fail not supported"))
    }

    // ==================== BLE GATT APIs ====================

    /// Connect to a BLE peripheral device.
    ///
    /// JSON fields:
    /// - `deviceId`: string (device MAC address or identifier)
    /// - `timeout`: number in ms (default 0, system default)
    fn create_ble_connection(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "createBLEConnection:fail not supported",
        ))
    }

    /// Disconnect from a BLE peripheral device.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    fn close_ble_connection(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "closeBLEConnection:fail not supported",
        ))
    }

    /// Get all GATT services of a connected BLE device.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    ///
    /// Returns JSON: `{"services": [{"uuid": "...", "isPrimary": true}]}`
    fn get_ble_device_services(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getBLEDeviceServices:fail not supported",
        ))
    }

    /// Get all characteristics of a BLE GATT service.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    /// - `serviceId`: string (service UUID)
    ///
    /// Returns JSON: `{"characteristics": [{"uuid": "...", "properties": {...}}]}`
    fn get_ble_device_characteristics(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getBLEDeviceCharacteristics:fail not supported",
        ))
    }

    /// Read a BLE characteristic value.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    /// - `serviceId`: string (service UUID)
    /// - `characteristicId`: string (characteristic UUID)
    fn read_ble_characteristic_value(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "readBLECharacteristicValue:fail not supported",
        ))
    }

    /// Write a value to a BLE characteristic.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    /// - `serviceId`: string (service UUID)
    /// - `characteristicId`: string (characteristic UUID)
    /// - `value`: string (hex-encoded bytes, e.g. "0a1b2c")
    /// - `writeType`: string ("write" or "writeNoResponse")
    fn write_ble_characteristic_value(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "writeBLECharacteristicValue:fail not supported",
        ))
    }

    /// Subscribe or unsubscribe to BLE characteristic value changes.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    /// - `serviceId`: string (service UUID)
    /// - `characteristicId`: string (characteristic UUID)
    /// - `state`: bool (true = subscribe, false = unsubscribe)
    fn notify_ble_characteristic_value_change(
        &self,
        _options_json: &str,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "notifyBLECharacteristicValueChange:fail not supported",
        ))
    }

    /// Get the RSSI (signal strength) of a connected BLE device.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    ///
    /// Returns JSON: `{"RSSI": -50}`
    fn get_ble_device_rssi(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported(
            "getBLEDeviceRSSI:fail not supported",
        ))
    }

    /// Set the MTU for a BLE connection.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    /// - `mtu`: number (22-512)
    fn set_ble_mtu(&self, _options_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("setBLEMTU:fail not supported"))
    }

    /// Get the current MTU of a BLE connection.
    ///
    /// JSON fields:
    /// - `deviceId`: string
    ///
    /// Returns JSON: `{"mtu": 23}`
    fn get_ble_mtu(&self, _options_json: &str) -> Result<String, ServiceError> {
        Err(ServiceError::not_supported("getBLEMTU:fail not supported"))
    }
}

// ==================== Domain Sub-Traits ====================
//
// DeviceServices is split into 5 domain groups to improve organization,
// enable per-domain feature gating, and support domain-scoped testing.
// Each sub-trait has default `None` implementations so platforms only need
// to override the services they support.

/// Sensor-related device services: battery, vibration, motion, orientation.
pub trait SensorServices: Send + Sync {
    fn battery(&self) -> Option<Arc<dyn BatteryService>> {
        None
    }
    fn vibration(&self) -> Option<Arc<dyn VibrationService>> {
        None
    }
    fn screen(&self) -> Option<Arc<dyn ScreenService>> {
        None
    }
    fn device_motion(&self) -> Option<Arc<dyn DeviceMotionService>> {
        None
    }
    fn gyroscope(&self) -> Option<Arc<dyn GyroscopeService>> {
        None
    }
    fn compass(&self) -> Option<Arc<dyn CompassService>> {
        None
    }
    fn accelerometer(&self) -> Option<Arc<dyn AccelerometerService>> {
        None
    }
}

/// Media capture and playback services: camera, image, audio recording, video.
pub trait MediaServices: Send + Sync {
    fn audio_platform(&self) -> Option<Arc<dyn AudioPlatformService>> {
        None
    }
    fn recorder(&self) -> Option<Arc<dyn RecorderService>> {
        None
    }
    fn camera(&self) -> Option<Arc<dyn CameraService>> {
        None
    }
    fn image_api(&self) -> Option<Arc<dyn ImageApiService>> {
        None
    }
    fn video(&self) -> Option<Arc<dyn VideoService>> {
        None
    }
}

/// Network and radio connectivity services: network state, bluetooth, location.
pub trait ConnectivityServices: Send + Sync {
    fn network(&self) -> Option<Arc<dyn NetworkService>> {
        None
    }
    fn bluetooth(&self) -> Option<Arc<dyn BluetoothService>> {
        None
    }
    fn location(&self) -> Option<Arc<dyn LocationService>> {
        None
    }
}

/// Platform integration services: payment, ads, auth, sharing, game logs,
/// subpackage.
pub trait CommerceServices: Send + Sync {
    fn game_log(&self) -> Option<Arc<dyn GameLogService>> {
        None
    }
    /// Host-provided advertising bridge.
    ///
    /// Returning `None` means the host installed no ad integration. Content
    /// still gets working ad objects, but no advert is ever shown and
    /// incentivised video reports `isEnded: false` -- see [`AdService`] for
    /// why the runtime must never invent a reward.
    fn ad(&self) -> Option<Arc<dyn AdService>> {
        None
    }
    fn auth(&self) -> Option<Arc<dyn AuthService>> {
        None
    }
    fn subpackage(&self) -> Option<Arc<dyn SubpackageService>> {
        None
    }
    fn share(&self) -> Option<Arc<dyn ShareService>> {
        None
    }
    fn payment(&self) -> Option<Arc<dyn PaymentService>> {
        None
    }
}

/// System utility services: clipboard, keyboard, interaction, file, codec, etc.
pub trait SystemUtilServices: Send + Sync {
    fn clipboard(&self) -> Option<Arc<dyn ClipboardService>> {
        None
    }
    fn keyboard(&self) -> Option<Arc<dyn KeyboardService>> {
        None
    }
    fn interaction(&self) -> Option<Arc<dyn InteractionService>> {
        None
    }
    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        None
    }
    fn codec(&self) -> Option<Arc<dyn CodecService>> {
        None
    }
    fn file(&self) -> Option<Arc<dyn FileService>> {
        None
    }
    /// Host-provided permission decisions.
    ///
    /// Returning `None` means the host installed no permission integration, and
    /// every scope is then denied -- the runtime does not grant on its own. See
    /// [`PermissionService`] for why that is the only safe default here.
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        None
    }
    fn scan_code(&self) -> Option<Arc<dyn ScanCodeService>> {
        None
    }
    fn navigate(&self) -> Option<Arc<dyn NavigateService>> {
        None
    }
}

// ==================== Aggregated Device Services ====================

/// Aggregated device services provided by a platform.
///
/// This is a super-trait of all 5 domain sub-traits. Any type that implements
/// all sub-traits automatically implements `DeviceServices`.
///
/// # Domain Groups
/// - [`SensorServices`]: Battery, vibration, accelerometer, gyroscope, compass, motion, screen
/// - [`MediaServices`]: Camera, image API, audio recording, audio platform
/// - [`ConnectivityServices`]: Network, Bluetooth, location
/// - [`CommerceServices`]: Payment, auth, share, game log, subpackage
/// - [`SystemUtilServices`]: Clipboard, keyboard, interaction, system info, codec, file, scan code, navigate
pub trait DeviceServices:
    SensorServices + MediaServices + ConnectivityServices + CommerceServices + SystemUtilServices
{
}

/// Blanket impl: any type implementing all 5 sub-traits is a DeviceServices.
impl<T> DeviceServices for T where
    T: SensorServices
        + MediaServices
        + ConnectivityServices
        + CommerceServices
        + SystemUtilServices
{
}
