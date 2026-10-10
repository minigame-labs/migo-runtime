//! Device service traits for sensors, battery, vibration, and screen.

use std::sync::Arc;

use crate::protocol::error::ServiceError;

use super::{
    AdService, AuthService, CameraService, ClipboardService, CodecService, FileService,
    GameLogService, ImageApiService, InteractionService, LocationService, NavigateService,
    NetworkService, PaymentService, PermissionService, ScanCodeService, ShareService,
    SubpackageService, SystemInfoService, VideoService, WindowService,
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

/// The screen: brightness, orientation, keep-awake, and what is captured of it.
///
/// A method taking `request_json` is a request the host answers through its hook
/// with `{"requestId", ...}` or `{"requestId", "error"}`. The `start_*`/`stop_*`
/// observers are commands: what they observe arrives as an event.
pub trait ScreenService: Send + Sync {
    /// `getScreenBrightness`: answers `{"value"}` (0.0-1.0) through
    /// `_internalOnGetScreenBrightnessResult`.
    fn get_brightness(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getScreenBrightness:fail not supported",
        ))
    }

    /// `setScreenBrightness` `{"value"}` (0.0-1.0): answers through
    /// `_internalOnSetScreenBrightnessResult`.
    fn set_brightness(&self, _request_json: &str) -> Result<(), ServiceError> {
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

    /// `setDeviceOrientation` `{"value"}` (`portrait` or `landscape`): answers
    /// through `_internalOnSetDeviceOrientationResult`.
    fn set_orientation(&self, _request_json: &str) -> Result<(), ServiceError> {
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

    /// `getScreenRecordingState`: answers `{"state"}` (`on` or `off`) through
    /// `_internalOnGetScreenRecordingStateResult`.
    fn get_screen_recording_state(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getScreenRecordingState:fail not supported",
        ))
    }

    /// Start observing whether the screen is being recorded; each change is the
    /// event `{"state"}`.
    fn start_screen_recording_observer(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "onScreenRecordingStateChanged:fail not supported",
        ))
    }

    /// Stop observing screen recording.
    fn stop_screen_recording_observer(&self) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "offScreenRecordingStateChanged:fail not supported",
        ))
    }

    /// `setVisualEffectOnCapture` `{"visualEffect"}` (`none` or `hidden`: keep the
    /// game out of screenshots and recordings): answers through
    /// `_internalOnSetVisualEffectOnCaptureResult`.
    fn set_visual_effect_on_capture(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setVisualEffectOnCapture:fail not supported",
        ))
    }

    /// Set whether to enable debug mode at runtime.
    fn set_enable_debug(&self, _enabled: bool) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "setEnableDebug:fail not supported",
        ))
    }
}

// ==================== Motion sensors ====================
//
// Each `start` is a request `{"requestId", "interval"}` -- `game` (20 ms), `ui`
// (60 ms) or `normal` (200 ms); the compass has none -- that the host answers
// through the sensor's hook, failing it when the device has no such sensor or may
// not use it. `stop` is a command. Readings arrive as typed samples
// (`HostCommand::OnAccelerometerChange` and its siblings), never as JSON: they
// come up to fifty times a second.

/// Device motion: the rotation angles `alpha`, `beta`, `gamma`.
pub trait DeviceMotionService: Send + Sync {
    /// Answers through `_internalOnStartDeviceMotionListeningResult`.
    fn start(&self, _request_json: &str) -> Result<(), ServiceError> {
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

/// Gyroscope: angular velocity in rad/s.
pub trait GyroscopeService: Send + Sync {
    /// Answers through `_internalOnStartGyroscopeResult`.
    fn start(&self, _request_json: &str) -> Result<(), ServiceError> {
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

/// Compass: magnetic heading in degrees, with its accuracy.
pub trait CompassService: Send + Sync {
    /// Answers through `_internalOnStartCompassResult`.
    fn start(&self, _request_json: &str) -> Result<(), ServiceError> {
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

/// Accelerometer: acceleration in units of g.
pub trait AccelerometerService: Send + Sync {
    /// Answers through `_internalOnStartAccelerometerResult`.
    fn start(&self, _request_json: &str) -> Result<(), ServiceError> {
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

/// Bluetooth: the adapter, scanning, pairing, iBeacons and BLE GATT as a central.
///
/// Every method is a request `{"requestId", ...}` the host answers through the
/// hook [`BLUETOOTH_RESULT_HOOKS`] names for its method number, with the result
/// or with `{"error", "errCode"}` -- the common mini-game platform's Bluetooth
/// codes: 10000 adapter not opened, 10001 Bluetooth unavailable, 10002 no such
/// device, 10003 connection failed, 10004 no such service, 10005 no such
/// characteristic, 10006 not connected, 10007 operation not supported by the
/// characteristic, 10008 system error, 10009 system not supported, 10012 timed
/// out, 10013 invalid data; and for iBeacons 11000-11006. Binary values travel
/// as lower-case hex strings. A request answers when the operation has happened
/// -- a connection is made, a write acknowledged -- not when it was issued.
///
/// What the host observes arrives as events: adapter state, devices found, BLE
/// connection state and MTU, iBeacon updates, and characteristic values (typed,
/// `HostCommand::OnBLECharacteristicValueChange`: a peripheral may notify a
/// hundred times a second).
pub trait BluetoothService: Send + Sync {
    /// `openBluetoothAdapter` `{"mode"}` (`central`): answers `{}`.
    fn open_adapter(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "openBluetoothAdapter:fail not supported",
        ))
    }

    /// `closeBluetoothAdapter` `{}`: answers `{}`.
    fn close_adapter(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "closeBluetoothAdapter:fail not supported",
        ))
    }

    /// `getBluetoothAdapterState` `{}`: answers `{"available", "discovering"}`.
    fn get_adapter_state(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getBluetoothAdapterState:fail not supported",
        ))
    }

    /// `startBluetoothDevicesDiscovery` `{"services", "allowDuplicatesKey", "interval", "powerLevel"}`: answers `{}`.
    fn start_devices_discovery(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "startBluetoothDevicesDiscovery:fail not supported",
        ))
    }

    /// `stopBluetoothDevicesDiscovery` `{}`: answers `{}`.
    fn stop_devices_discovery(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "stopBluetoothDevicesDiscovery:fail not supported",
        ))
    }

    /// `getBluetoothDevices` `{}`: answers `{"devices"}`, each as the device-found event describes it.
    fn get_devices(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getBluetoothDevices:fail not supported",
        ))
    }

    /// `getConnectedBluetoothDevices` `{"services"}`: answers `{"devices": [{"name", "deviceId"}]}`.
    fn get_connected_devices(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getConnectedBluetoothDevices:fail not supported",
        ))
    }

    /// `makeBluetoothPair` `{"deviceId", "pin" (hex), "timeout"}`: answers `{}` once paired.
    fn make_pair(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "makeBluetoothPair:fail not supported",
        ))
    }

    /// `isBluetoothDevicePaired` `{"deviceId"}`: answers `{}` when paired; fails when not.
    fn is_device_paired(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "isBluetoothDevicePaired:fail not supported",
        ))
    }

    /// `startBeaconDiscovery` `{"uuids", "ignoreBluetoothAvailable"}`: answers `{}`.
    fn start_beacon_discovery(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "startBeaconDiscovery:fail not supported",
        ))
    }

    /// `stopBeaconDiscovery` `{}`: answers `{}`.
    fn stop_beacon_discovery(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "stopBeaconDiscovery:fail not supported",
        ))
    }

    /// `getBeacons` `{}`: answers `{"beacons": [{"uuid", "major", "minor", "proximity", "accuracy", "rssi"}]}`.
    fn get_beacons(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("getBeacons:fail not supported"))
    }

    /// `createBLEConnection` `{"deviceId", "timeout"}`: answers `{}` once connected.
    fn create_ble_connection(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "createBLEConnection:fail not supported",
        ))
    }

    /// `closeBLEConnection` `{"deviceId"}`: answers `{}`.
    fn close_ble_connection(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "closeBLEConnection:fail not supported",
        ))
    }

    /// `getBLEDeviceServices` `{"deviceId"}`: answers `{"services": [{"uuid", "isPrimary"}]}`.
    fn get_ble_device_services(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getBLEDeviceServices:fail not supported",
        ))
    }

    /// `getBLEDeviceCharacteristics` `{"deviceId", "serviceId"}`: answers `{"characteristics": [{"uuid", "properties": {"read", "write", "notify", "indicate", "writeNoResponse", "writeDefault"}}]}`.
    fn get_ble_device_characteristics(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getBLEDeviceCharacteristics:fail not supported",
        ))
    }

    /// `readBLECharacteristicValue` `{"deviceId", "serviceId", "characteristicId"}`: answers `{}` once read; the value arrives as a characteristic-value event.
    fn read_ble_characteristic_value(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "readBLECharacteristicValue:fail not supported",
        ))
    }

    /// `writeBLECharacteristicValue` `{"deviceId", "serviceId", "characteristicId", "value" (hex), "writeType"}`: answers `{}` once written.
    fn write_ble_characteristic_value(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "writeBLECharacteristicValue:fail not supported",
        ))
    }

    /// `notifyBLECharacteristicValueChange` `{"deviceId", "serviceId", "characteristicId", "state", "type"}`: answers `{}` once the descriptor is written.
    fn notify_ble_characteristic_value_change(
        &self,
        _request_json: &str,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "notifyBLECharacteristicValueChange:fail not supported",
        ))
    }

    /// `getBLEDeviceRSSI` `{"deviceId"}`: answers `{"RSSI"}`, read from the device for this request.
    fn get_ble_device_rssi(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "getBLEDeviceRSSI:fail not supported",
        ))
    }

    /// `setBLEMTU` `{"deviceId", "mtu"}`: answers `{"mtu"}` as negotiated.
    fn set_ble_mtu(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("setBLEMTU:fail not supported"))
    }

    /// `getBLEMTU` `{"deviceId", "writeType"}`: answers `{"mtu"}`.
    fn get_ble_mtu(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported("getBLEMTU:fail not supported"))
    }
}

/// The hook each Bluetooth request is answered through, indexed by its method
/// number (`MIGO_BLUETOOTH_*` in `include/migo/host_services.h`). One table for
/// every SDK: the C ABI's routing and Android's result callback both read it.
pub const BLUETOOTH_RESULT_HOOKS: [&str; 22] = [
    "_internalOnOpenBluetoothAdapterResult",
    "_internalOnCloseBluetoothAdapterResult",
    "_internalOnGetBluetoothAdapterStateResult",
    "_internalOnStartBluetoothDevicesDiscoveryResult",
    "_internalOnStopBluetoothDevicesDiscoveryResult",
    "_internalOnGetBluetoothDevicesResult",
    "_internalOnGetConnectedBluetoothDevicesResult",
    "_internalOnMakeBluetoothPairResult",
    "_internalOnIsBluetoothDevicePairedResult",
    "_internalOnStartBeaconDiscoveryResult",
    "_internalOnStopBeaconDiscoveryResult",
    "_internalOnGetBeaconsResult",
    "_internalOnCreateBLEConnectionResult",
    "_internalOnCloseBLEConnectionResult",
    "_internalOnGetBLEDeviceServicesResult",
    "_internalOnGetBLEDeviceCharacteristicsResult",
    "_internalOnReadBLECharacteristicValueResult",
    "_internalOnWriteBLECharacteristicValueResult",
    "_internalOnNotifyBLECharacteristicValueChangeResult",
    "_internalOnGetBLEDeviceRSSIResult",
    "_internalOnSetBLEMTUResult",
    "_internalOnGetBLEMTUResult",
];

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
    /// The desktop window, where the host has one content may size, point at
    /// and lock the pointer in.
    fn window(&self) -> Option<Arc<dyn WindowService>> {
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
