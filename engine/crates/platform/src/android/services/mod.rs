//! Android device service implementations.
//!
//! Implements `migo_core::services::DeviceServices` traits using JNI calls.

use std::sync::{Arc, OnceLock};

use migo_core::services::{
    AccelerometerService, AdService, AudioPlatformService, AuthService, BatteryService,
    BlePeripheralService, BluetoothService, CameraService, ClipboardService, CodecService,
    CommerceServices, CompassService, ConnectivityServices, DeviceMotionService, EcosystemService,
    FileService, GameLogService, GamepadRumble, GamepadService, GyroscopeService, ImageApiService,
    InteractionService, KeyboardService, LocationService, MediaServices, NavigateService,
    NetworkService, PaymentService, PermissionService, RecorderService, ScanCodeService, Scope,
    ScopeState, ScreenService, SensorServices, ServiceError, ServiceErrorCode, ShareService,
    SubpackageService, SystemInfoService, SystemUtilServices, UpdateService, VibrationService,
    VideoDecoderService, VideoService, WindowService,
};

use crate::android::jni;
use crate::android_permission_gate::{PermissionGate, SessionGate};

/// Run one gated Android device call under a session's own admission.
///
/// Takes the handle rather than the id because the steady-state requirement forbids a per-event path
/// acquiring a lock shared beyond its own session: resolving the id here would take the
/// gate's process-wide live-host map on every call, including the Bluetooth
/// characteristic writes, which are a steady hot path.
fn permission_jni_call<T>(
    session: &SessionGate,
    scope: Option<Scope>,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, ServiceError> {
    match session.run(scope, operation) {
        Ok(result) => result.map_err(ServiceError::from),
        Err(_) => {
            if let Some(scope) = scope {
                Err(ServiceError {
                    code: ServiceErrorCode::PermissionDenied,
                    message: format!("auth deny: {} is not granted", scope.as_minigame_str()),
                })
            } else {
                Err(ServiceError::system("Android session is closing"))
            }
        }
    }
}

/// Android device services aggregator.
pub struct AndroidDeviceServices {
    host_id: i32,
    /// The permission gate handle, resolved once here so no gated device call has to
    /// look this session up in a process-wide map. See [`permission_jni_call`].
    session: SessionGate,
    /// Set when an embedding host services the keyboard itself; see
    /// `AndroidPlatform::with_host_keyboard`.
    host_keyboard: Option<Arc<dyn KeyboardService>>,
}

impl AndroidDeviceServices {
    pub fn new(host_id: i32) -> Self {
        Self::with_host_keyboard(host_id, None)
    }

    pub fn with_host_keyboard(
        host_id: i32,
        host_keyboard: Option<Arc<dyn KeyboardService>>,
    ) -> Self {
        Self {
            host_id,
            session: permission_gate().open_session(host_id),
            host_keyboard,
        }
    }
}

// ---- SensorServices ----
impl SensorServices for AndroidDeviceServices {
    #[cfg(feature = "api-sensors")]
    fn battery(&self) -> Option<Arc<dyn BatteryService>> {
        Some(Arc::new(AndroidBattery))
    }
    #[cfg(feature = "api-sensors")]
    fn vibration(&self) -> Option<Arc<dyn VibrationService>> {
        Some(Arc::new(AndroidVibration))
    }
    #[cfg(feature = "api-sensors")]
    fn screen(&self) -> Option<Arc<dyn ScreenService>> {
        Some(Arc::new(AndroidScreen {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-sensors")]
    fn device_motion(&self) -> Option<Arc<dyn DeviceMotionService>> {
        Some(Arc::new(AndroidDeviceMotion {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-sensors")]
    fn gyroscope(&self) -> Option<Arc<dyn GyroscopeService>> {
        Some(Arc::new(AndroidGyroscope {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-sensors")]
    fn compass(&self) -> Option<Arc<dyn CompassService>> {
        Some(Arc::new(AndroidCompass {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-sensors")]
    fn accelerometer(&self) -> Option<Arc<dyn AccelerometerService>> {
        Some(Arc::new(AndroidAccelerometer {
            host_id: self.host_id,
        }))
    }
}

// ---- MediaServices ----
impl MediaServices for AndroidDeviceServices {
    #[cfg(feature = "api-media")]
    fn audio_platform(&self) -> Option<Arc<dyn AudioPlatformService>> {
        Some(Arc::new(AndroidAudioPlatform {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-media")]
    fn recorder(&self) -> Option<Arc<dyn RecorderService>> {
        Some(Arc::new(AndroidRecorder {
            session: self.session.clone(),
        }))
    }
    #[cfg(feature = "api-media")]
    fn camera(&self) -> Option<Arc<dyn CameraService>> {
        Some(Arc::new(AndroidCamera {
            session: self.session.clone(),
        }))
    }
    #[cfg(feature = "api-media")]
    fn image_api(&self) -> Option<Arc<dyn ImageApiService>> {
        Some(Arc::new(AndroidImageApi {
            session: self.session.clone(),
        }))
    }
    #[cfg(feature = "api-media")]
    fn video(&self) -> Option<Arc<dyn VideoService>> {
        Some(Arc::new(AndroidVideo {
            host_id: self.host_id,
        }))
    }
    /// The NDK's extractor and codec, in the engine: no Java in the way of
    /// frames content reads as pixels.
    #[cfg(feature = "api-media")]
    fn video_decoder(&self) -> Option<Arc<dyn VideoDecoderService>> {
        Some(Arc::new(crate::android::video_decoder::AndroidVideoDecoder))
    }
}

// ---- ConnectivityServices ----
impl ConnectivityServices for AndroidDeviceServices {
    #[cfg(feature = "api-sensors")]
    fn network(&self) -> Option<Arc<dyn NetworkService>> {
        Some(Arc::new(AndroidNetwork {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-connectivity")]
    fn bluetooth(&self) -> Option<Arc<dyn BluetoothService>> {
        Some(Arc::new(AndroidBluetooth {
            session: self.session.clone(),
        }))
    }
    #[cfg(feature = "api-connectivity")]
    fn ble_peripheral(&self) -> Option<Arc<dyn BlePeripheralService>> {
        Some(Arc::new(AndroidBlePeripheral {
            session: self.session.clone(),
        }))
    }
    #[cfg(feature = "api-sensors")]
    fn location(&self) -> Option<Arc<dyn LocationService>> {
        Some(Arc::new(AndroidLocation {
            session: self.session.clone(),
        }))
    }
}

// ---- CommerceServices ----
impl CommerceServices for AndroidDeviceServices {
    #[cfg(feature = "api-connectivity")]
    fn game_log(&self) -> Option<Arc<dyn GameLogService>> {
        Some(Arc::new(AndroidGameLog {
            session: self.session.clone(),
        }))
    }
    #[cfg(feature = "api-connectivity")]
    fn auth(&self) -> Option<Arc<dyn AuthService>> {
        Some(Arc::new(AndroidAuth {
            session: self.session.clone(),
        }))
    }
    fn subpackage(&self) -> Option<Arc<dyn SubpackageService>> {
        Some(Arc::new(AndroidSubpackage {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-commerce")]
    fn share(&self) -> Option<Arc<dyn ShareService>> {
        Some(Arc::new(AndroidShare {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-commerce")]
    fn payment(&self) -> Option<Arc<dyn PaymentService>> {
        Some(Arc::new(AndroidPayment {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-commerce")]
    fn ad(&self) -> Option<Arc<dyn AdService>> {
        Some(Arc::new(AndroidAd {
            host_id: self.host_id,
        }))
    }
}

// ---- SystemUtilServices ----
impl SystemUtilServices for AndroidDeviceServices {
    #[cfg(feature = "api-system")]
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        Some(Arc::new(AndroidPermission {
            session: self.session.clone(),
        }))
    }
    #[cfg(feature = "api-sensors")]
    fn clipboard(&self) -> Option<Arc<dyn ClipboardService>> {
        Some(Arc::new(AndroidClipboard {
            host_id: self.host_id,
        }))
    }
    fn keyboard(&self) -> Option<Arc<dyn KeyboardService>> {
        // The host's own comes first: `AndroidKeyboard` reaches the Java SDK
        // over JNI, which a pure-native host has not got, and this accessor
        // would otherwise claim a capability it cannot deliver.
        if let Some(host_keyboard) = &self.host_keyboard {
            return Some(Arc::clone(host_keyboard));
        }
        Some(Arc::new(AndroidKeyboard {
            host_id: self.host_id,
        }))
    }
    fn gamepad(&self) -> Option<Arc<dyn GamepadService>> {
        Some(Arc::new(AndroidGamepad {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-system")]
    fn interaction(&self) -> Option<Arc<dyn InteractionService>> {
        Some(Arc::new(AndroidInteraction {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-connectivity")]
    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        Some(Arc::new(AndroidSystemInfo {
            host_id: self.host_id,
        }))
    }
    fn codec(&self) -> Option<Arc<dyn CodecService>> {
        Some(Arc::new(AndroidCodec))
    }
    fn file(&self) -> Option<Arc<dyn FileService>> {
        Some(Arc::new(AndroidFile))
    }
    #[cfg(feature = "api-sensors")]
    fn scan_code(&self) -> Option<Arc<dyn ScanCodeService>> {
        Some(Arc::new(AndroidScanCode {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-connectivity")]
    fn navigate(&self) -> Option<Arc<dyn NavigateService>> {
        Some(Arc::new(AndroidNavigate {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-system")]
    fn window(&self) -> Option<Arc<dyn WindowService>> {
        Some(Arc::new(AndroidWindow {
            host_id: self.host_id,
        }))
    }
    #[cfg(feature = "api-system")]
    fn ecosystem(&self) -> Option<Arc<dyn EcosystemService>> {
        Some(Arc::new(AndroidEcosystem {
            host_id: self.host_id,
        }))
    }

    /// The update handler the embedder installed; none, and there is no
    /// update to report.
    #[cfg(feature = "api-system")]
    fn update(&self) -> Option<Arc<dyn UpdateService>> {
        jni::update_available(self.host_id)
            .unwrap_or(false)
            .then(|| {
                Arc::new(AndroidUpdate {
                    host_id: self.host_id,
                }) as Arc<dyn UpdateService>
            })
    }
}

// DeviceServices is auto-implemented via blanket impl in shared::services::device

// ==================== Clipboard ====================

struct AndroidClipboard {
    host_id: i32,
}

/// Android's clipboard is reachable synchronously from this thread, so each
/// request is carried out here and answered at once -- through the result hook
/// every platform answers on, so content sees one shape however the host
/// reaches its clipboard.
impl ClipboardService for AndroidClipboard {
    fn set_data(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("setClipboardData", request_json)?;
        let data = request
            .fields
            .get("data")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                ServiceError::invalid_param("setClipboardData:fail data must be a string")
            })?;
        let outcome = jni::set_clipboard_data(self.host_id, data).map(|()| serde_json::Map::new());
        request.answer(self.host_id, "_internalOnSetClipboardDataResult", outcome);
        Ok(())
    }

    fn get_data(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("getClipboardData", request_json)?;
        let outcome = jni::get_clipboard_data(self.host_id).map(|data| {
            let mut fields = serde_json::Map::new();
            fields.insert("data".into(), data.into());
            fields
        });
        request.answer(self.host_id, "_internalOnGetClipboardDataResult", outcome);
        Ok(())
    }
}

/// One request content made that the SDK answers synchronously: its id and its
/// options. The answer still travels through the request's hook, in the shape
/// every host result takes, so content settles it the same way whether this SDK
/// or a C ABI host answered.
struct HostRequest {
    request_id: u64,
    fields: serde_json::Map<String, serde_json::Value>,
}

impl HostRequest {
    fn parse(api: &str, request_json: &str) -> Result<Self, ServiceError> {
        let malformed = || ServiceError::invalid_param(format!("{api}:fail malformed request"));
        let serde_json::Value::Object(fields) =
            serde_json::from_str::<serde_json::Value>(request_json).map_err(|_| malformed())?
        else {
            return Err(malformed());
        };
        let request_id = fields
            .get("requestId")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(malformed)?;
        Ok(Self { request_id, fields })
    }

    /// The string option `name`, or the request's failure.
    fn str_field(&self, api: &str, name: &str) -> Result<&str, ServiceError> {
        self.fields
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ServiceError::invalid_param(format!("{api}:fail {name} must be a string"))
            })
    }

    /// Deliver the outcome to content's hook, keyed by the request's id.
    fn answer(
        &self,
        host_id: i32,
        hook: &'static str,
        outcome: Result<serde_json::Map<String, serde_json::Value>, String>,
    ) {
        let mut result = match outcome {
            Ok(fields) => fields,
            Err(reason) => {
                let mut fields = serde_json::Map::new();
                fields.insert("error".into(), reason.into());
                fields
            }
        };
        result.insert("requestId".into(), self.request_id.into());
        let _ = migo_core::send_reliable_command_to_host(
            host_id,
            shared::protocol::host_cmd::HostCommand::InvokeHostHook {
                hook,
                args_json: shared::js_escape::hook_args_one(
                    serde_json::Value::Object(result).to_string(),
                ),
            },
        );
    }
}

// ==================== Battery ====================

struct AndroidBattery;

impl BatteryService for AndroidBattery {
    fn get_info_json(&self) -> Result<String, ServiceError> {
        Ok(jni::get_battery_info_json()?)
    }
}

// ==================== Vibration ====================

struct AndroidVibration;

impl VibrationService for AndroidVibration {
    fn vibrate_short(&self, type_: &str) -> Result<(), ServiceError> {
        jni::vibrate_short(type_).map(|_| ()).map_err(Into::into)
    }

    fn vibrate_long(&self) -> Result<(), ServiceError> {
        jni::vibrate_long().map(|_| ()).map_err(Into::into)
    }
}

// ==================== Screen ====================

struct AndroidScreen {
    host_id: i32,
}

impl ScreenService for AndroidScreen {
    fn get_brightness(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("getScreenBrightness", request_json)?;
        // The host answers 0..1 -- the window's own brightness, or the system's setting where the window follows it --
        // or NaN when it has no window to ask.
        let outcome = jni::get_screen_brightness(self.host_id).and_then(|value| {
            if !(0.0..=1.0).contains(&value) {
                return Err("no activity".to_string());
            }
            let mut fields = serde_json::Map::new();
            fields.insert("value".into(), f64::from(value).into());
            Ok(fields)
        });
        request.answer(
            self.host_id,
            "_internalOnGetScreenBrightnessResult",
            outcome,
        );
        Ok(())
    }

    fn set_brightness(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("setScreenBrightness", request_json)?;
        let value = request
            .fields
            .get("value")
            .and_then(serde_json::Value::as_f64)
            .filter(|value| (0.0..=1.0).contains(value))
            .ok_or_else(|| {
                ServiceError::invalid_param("setScreenBrightness:fail value must be from 0 to 1")
            })?;
        let outcome = jni::set_screen_brightness(self.host_id, value as f32).and_then(|code| {
            if code == 0 {
                Ok(serde_json::Map::new())
            } else {
                Err("no activity".to_string())
            }
        });
        request.answer(
            self.host_id,
            "_internalOnSetScreenBrightnessResult",
            outcome,
        );
        Ok(())
    }

    fn set_keep_screen_on(&self, keep_on: bool) -> Result<(), ServiceError> {
        jni::set_keep_screen_on(self.host_id, keep_on)
            .map(|_| ())
            .map_err(Into::into)
    }

    fn set_orientation(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("setDeviceOrientation", request_json)?;
        let value = request.str_field("setDeviceOrientation", "value")?;
        let outcome =
            jni::set_device_orientation(self.host_id, value).and_then(|code| match code {
                0 => Ok(serde_json::Map::new()),
                -2 => Err(format!("invalid value {value}")),
                _ => Err("no activity".to_string()),
            });
        request.answer(
            self.host_id,
            "_internalOnSetDeviceOrientationResult",
            outcome,
        );
        Ok(())
    }

    fn start_capture_screen(&self) -> Result<(), ServiceError> {
        Ok(jni::start_capture_screen(self.host_id)?)
    }

    fn stop_capture_screen(&self) -> Result<(), ServiceError> {
        Ok(jni::stop_capture_screen(self.host_id)?)
    }

    fn get_screen_recording_state(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("getScreenRecordingState", request_json)?;
        let outcome = jni::get_screen_recording_state(self.host_id).and_then(|state| {
            let state = match state {
                1 => "on",
                0 => "off",
                -1 => return Err("not supported".to_string()),
                _ => return Err("no activity".to_string()),
            };
            let mut fields = serde_json::Map::new();
            fields.insert("state".into(), state.into());
            Ok(fields)
        });
        request.answer(
            self.host_id,
            "_internalOnGetScreenRecordingStateResult",
            outcome,
        );
        Ok(())
    }

    fn start_screen_recording_observer(&self) -> Result<(), ServiceError> {
        Ok(jni::start_screen_recording_observer(self.host_id)?)
    }

    fn stop_screen_recording_observer(&self) -> Result<(), ServiceError> {
        Ok(jni::stop_screen_recording_observer(self.host_id)?)
    }

    fn set_visual_effect_on_capture(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("setVisualEffectOnCapture", request_json)?;
        let hidden = request.str_field("setVisualEffectOnCapture", "visualEffect")? == "hidden";
        let outcome = jni::set_visual_effect_on_capture(self.host_id, hidden).and_then(|code| {
            if code == 0 {
                Ok(serde_json::Map::new())
            } else {
                Err("no activity".to_string())
            }
        });
        request.answer(
            self.host_id,
            "_internalOnSetVisualEffectOnCaptureResult",
            outcome,
        );
        Ok(())
    }

    fn set_enable_debug(&self, enabled: bool) -> Result<(), ServiceError> {
        jni::set_enable_debug(self.host_id, enabled)
            .map(|_| ())
            .map_err(Into::into)
    }
}

// ==================== Motion sensors ====================

/// A sensor start's answer: started, or the device has no such sensor.
fn sensor_started(started: bool) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    if started {
        Ok(serde_json::Map::new())
    } else {
        Err("no such sensor on this device".to_string())
    }
}

// ==================== Device Motion ====================

struct AndroidDeviceMotion {
    host_id: i32,
}

impl DeviceMotionService for AndroidDeviceMotion {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("startDeviceMotionListening", request_json)?;
        let outcome = jni::start_device_motion(
            self.host_id,
            request.str_field("startDeviceMotionListening", "interval")?,
        )
        .and_then(sensor_started);
        request.answer(
            self.host_id,
            "_internalOnStartDeviceMotionListeningResult",
            outcome,
        );
        Ok(())
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Ok(jni::stop_device_motion(self.host_id)?)
    }
}

// ==================== Gyroscope ====================

struct AndroidGyroscope {
    host_id: i32,
}

impl GyroscopeService for AndroidGyroscope {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("startGyroscope", request_json)?;
        let outcome = jni::start_gyroscope(
            self.host_id,
            request.str_field("startGyroscope", "interval")?,
        )
        .and_then(sensor_started);
        request.answer(self.host_id, "_internalOnStartGyroscopeResult", outcome);
        Ok(())
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Ok(jni::stop_gyroscope(self.host_id)?)
    }
}

// ==================== Compass ====================

struct AndroidCompass {
    host_id: i32,
}

impl CompassService for AndroidCompass {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("startCompass", request_json)?;
        let outcome = jni::start_compass(self.host_id).and_then(sensor_started);
        request.answer(self.host_id, "_internalOnStartCompassResult", outcome);
        Ok(())
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Ok(jni::stop_compass(self.host_id)?)
    }
}

// ==================== Accelerometer ====================

struct AndroidAccelerometer {
    host_id: i32,
}

impl AccelerometerService for AndroidAccelerometer {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = HostRequest::parse("startAccelerometer", request_json)?;
        let outcome = jni::start_accelerometer(
            self.host_id,
            request.str_field("startAccelerometer", "interval")?,
        )
        .and_then(sensor_started);
        request.answer(self.host_id, "_internalOnStartAccelerometerResult", outcome);
        Ok(())
    }

    fn stop(&self) -> Result<(), ServiceError> {
        Ok(jni::stop_accelerometer(self.host_id)?)
    }
}

// ==================== Network ====================

struct AndroidNetwork {
    host_id: i32,
}

impl NetworkService for AndroidNetwork {
    fn start_monitoring(&self) -> Result<(), ServiceError> {
        Ok(jni::start_network_monitoring(self.host_id)?)
    }

    fn stop_monitoring(&self) -> Result<(), ServiceError> {
        Ok(jni::stop_network_monitoring(self.host_id)?)
    }

    fn get_network_type_json(&self) -> Result<String, ServiceError> {
        Ok(jni::get_network_type_json(self.host_id)?)
    }
}

// ==================== Audio Platform ====================

struct AndroidAudioPlatform {
    host_id: i32,
}

impl AudioPlatformService for AndroidAudioPlatform {
    /// Nothing is left for Android to do: it has no silent switch over a game's
    /// media, and the game's audio focus, exclusive as the reference's is with the
    /// speaker on, is the session's own (`AudioFocusManager`). It used to take a second
    /// focus here, with no listener and never given back -- ducking other apps for
    /// `mixWithOther`, against the option's meaning -- and to switch the device's
    /// call routing with `setSpeakerphoneOn`.
    fn set_inner_audio_option(&self, _obey_mute_switch: bool) -> Result<(), ServiceError> {
        Ok(())
    }

    fn get_available_audio_sources(&self) -> Result<Vec<String>, ServiceError> {
        let csv = jni::get_available_audio_sources(self.host_id)?;
        // Parse comma-separated list: "auto,mic,camcorder,voice_communication,voice_recognition"
        let sources: Vec<String> = csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(sources)
    }
}

// ==================== Recorder ====================

struct AndroidRecorder {
    session: SessionGate,
}

/// Every request goes to the SDK as content made it, `{"recordingId", ...}`;
/// what happens comes back through `onRecorderEvent`.
impl RecorderService for AndroidRecorder {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Record), || {
            jni::recorder_start(self.session.host_id(), request_json)
        })
    }

    fn pause(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Record), || {
            jni::recorder_pause(self.session.host_id(), request_json)
        })
    }

    fn resume(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Record), || {
            jni::recorder_resume(self.session.host_id(), request_json)
        })
    }

    fn stop(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::recorder_stop(self.session.host_id(), request_json)
        })
    }
}

// ==================== Camera ====================

struct AndroidCamera {
    session: SessionGate,
}

/// Every request goes to the SDK as content made it; a call is answered through
/// `onCameraResult` once it has happened.
impl CameraService for AndroidCamera {
    fn create(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Camera), || {
            jni::camera_create(self.session.host_id(), request_json)
        })
    }

    fn destroy(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::camera_destroy(self.session.host_id(), request_json)
        })
    }

    fn set(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Camera), || {
            jni::camera_set(self.session.host_id(), request_json)
        })
    }

    fn take_photo(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Camera), || {
            jni::camera_take_photo(self.session.host_id(), request_json)
        })
    }

    fn start_record(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Camera), || {
            jni::camera_start_record(self.session.host_id(), request_json)
        })
    }

    fn stop_record(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::camera_stop_record(self.session.host_id(), request_json)
        })
    }

    fn set_zoom(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Camera), || {
            jni::camera_set_zoom(self.session.host_id(), request_json)
        })
    }

    fn listen_frame_change(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Camera), || {
            jni::camera_listen_frame_change(self.session.host_id(), request_json)
        })
    }

    fn close_frame_change(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::camera_close_frame_change(self.session.host_id(), request_json)
        })
    }
}

// ==================== UI Interaction ====================

struct AndroidInteraction {
    host_id: i32,
}

impl InteractionService for AndroidInteraction {
    fn show_toast(&self, json: &str) -> Result<(), ServiceError> {
        Ok(jni::show_toast(self.host_id, json)?)
    }

    fn hide_toast(&self) -> Result<(), ServiceError> {
        Ok(jni::hide_toast(self.host_id)?)
    }

    fn show_modal(&self, json: &str) -> Result<(), ServiceError> {
        Ok(jni::show_modal(self.host_id, json)?)
    }

    fn show_loading(&self, json: &str) -> Result<(), ServiceError> {
        Ok(jni::show_loading(self.host_id, json)?)
    }

    fn hide_loading(&self) -> Result<(), ServiceError> {
        Ok(jni::hide_loading(self.host_id)?)
    }

    fn show_action_sheet(&self, json: &str) -> Result<(), ServiceError> {
        Ok(jni::show_action_sheet(self.host_id, json)?)
    }
}

// ==================== System Info ====================

struct AndroidSystemInfo {
    host_id: i32,
}

impl SystemInfoService for AndroidSystemInfo {
    fn open_bluetooth_settings(&self, request_id: i32) -> Result<(), ServiceError> {
        Ok(jni::open_bluetooth_settings(self.host_id, request_id)?)
    }

    fn open_app_authorize_setting(&self, request_id: i32) -> Result<(), ServiceError> {
        Ok(jni::open_app_authorize_setting(self.host_id, request_id)?)
    }

    fn get_window_info_json(&self) -> Result<String, ServiceError> {
        let info = jni::get_window_info(self.host_id)?;
        serde_json::to_string(&info)
            .map_err(|e| ServiceError::system(format!("getWindowInfo:fail {}", e)))
    }

    fn get_system_settings_json(&self) -> Result<String, ServiceError> {
        let settings = jni::get_system_settings(self.host_id)?;
        serde_json::to_string(&settings)
            .map_err(|e| ServiceError::system(format!("getSystemSetting:fail {}", e)))
    }

    fn get_device_info_json(&self) -> Result<String, ServiceError> {
        Ok(jni::get_device_info_json()?)
    }

    /// The app's report. One that cannot be read is no report: content is told
    /// what the engine knows, and the failure is logged rather than invented over.
    fn host_app_info(&self) -> shared::services::app_info::HostAppInfo {
        jni::get_host_app_info_json(self.host_id)
            .map_err(|error| error.to_string())
            .and_then(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
            .unwrap_or_else(|error| {
                tracing::warn!("host app info unavailable: {error}");
                shared::services::app_info::HostAppInfo::default()
            })
    }

    fn get_app_authorization_setting_json(&self) -> Result<String, ServiceError> {
        Ok(jni::get_app_authorization_setting_json(self.host_id)?)
    }

    fn open_setting(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::open_setting(self.host_id, options_json)?)
    }
}

// ==================== Bluetooth ====================

struct AndroidBluetooth {
    session: SessionGate,
}

// The SDK answers every request through `onBluetoothResult`; using Bluetooth
// needs `scope.bluetooth`, releasing what was acquired under it does not.
impl BluetoothService for AndroidBluetooth {
    fn open_adapter(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_open_adapter(self.session.host_id(), request_json)
        })
    }

    fn close_adapter(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::bluetooth_close_adapter(self.session.host_id(), request_json)
        })
    }

    fn get_adapter_state(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_get_adapter_state(self.session.host_id(), request_json)
        })
    }

    fn start_devices_discovery(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_start_devices_discovery(self.session.host_id(), request_json)
        })
    }

    fn stop_devices_discovery(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::bluetooth_stop_devices_discovery(self.session.host_id(), request_json)
        })
    }

    fn get_devices(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_get_devices(self.session.host_id(), request_json)
        })
    }

    fn get_connected_devices(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_get_connected_devices(self.session.host_id(), request_json)
        })
    }

    fn make_pair(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_make_pair(self.session.host_id(), request_json)
        })
    }

    fn is_device_paired(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_is_device_paired(self.session.host_id(), request_json)
        })
    }

    fn start_beacon_discovery(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_start_beacon_discovery(self.session.host_id(), request_json)
        })
    }

    fn stop_beacon_discovery(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::bluetooth_stop_beacon_discovery(self.session.host_id(), request_json)
        })
    }

    fn get_beacons(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::bluetooth_get_beacons(self.session.host_id(), request_json)
        })
    }

    fn create_ble_connection(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_create_connection(self.session.host_id(), request_json)
        })
    }

    fn close_ble_connection(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::ble_close_connection(self.session.host_id(), request_json)
        })
    }

    fn get_ble_device_services(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_get_device_services(self.session.host_id(), request_json)
        })
    }

    fn get_ble_device_characteristics(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_get_device_characteristics(self.session.host_id(), request_json)
        })
    }

    fn read_ble_characteristic_value(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_read_characteristic_value(self.session.host_id(), request_json)
        })
    }

    fn write_ble_characteristic_value(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_write_characteristic_value(self.session.host_id(), request_json)
        })
    }

    fn notify_ble_characteristic_value_change(
        &self,
        request_json: &str,
    ) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_notify_characteristic_value_change(self.session.host_id(), request_json)
        })
    }

    fn get_ble_device_rssi(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_get_device_rssi(self.session.host_id(), request_json)
        })
    }

    fn set_ble_mtu(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_set_mtu(self.session.host_id(), request_json)
        })
    }

    fn get_ble_mtu(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_get_mtu(self.session.host_id(), request_json)
        })
    }
}

struct AndroidBlePeripheral {
    session: SessionGate,
}

// The SDK answers every request through `onBlePeripheralResult`; serving and
// advertising need `scope.bluetooth`, releasing what was acquired under it
// does not.
impl BlePeripheralService for AndroidBlePeripheral {
    fn create_server(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_peripheral_create_server(self.session.host_id(), request_json)
        })
    }

    fn add_service(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_peripheral_add_service(self.session.host_id(), request_json)
        })
    }

    fn remove_service(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::ble_peripheral_remove_service(self.session.host_id(), request_json)
        })
    }

    fn start_advertising(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_peripheral_start_advertising(self.session.host_id(), request_json)
        })
    }

    fn stop_advertising(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, None, || {
            jni::ble_peripheral_stop_advertising(self.session.host_id(), request_json)
        })
    }

    fn write_characteristic_value(&self, request_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::Bluetooth), || {
            jni::ble_peripheral_write_characteristic_value(self.session.host_id(), request_json)
        })
    }
}

// ==================== Keyboard ====================

struct AndroidKeyboard {
    host_id: i32,
}

impl KeyboardService for AndroidKeyboard {
    fn show(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::keyboard_show(self.host_id, options_json)?)
    }

    fn hide(&self) -> Result<(), ServiceError> {
        Ok(jni::keyboard_hide(self.host_id)?)
    }

    fn update(&self, value: &str) -> Result<(), ServiceError> {
        Ok(jni::keyboard_update(self.host_id, value)?)
    }
}

// ==================== Codec (GBK) ====================

struct AndroidCodec;

impl CodecService for AndroidCodec {
    fn encode_gbk(&self, data: &str) -> Result<Vec<u8>, ServiceError> {
        Ok(jni::outbound::encode_gbk(data)?)
    }

    fn decode_gbk(&self, data: &[u8]) -> Result<String, ServiceError> {
        Ok(jni::outbound::decode_gbk(data)?)
    }
}

// ==================== File (Unzip) ====================

struct AndroidFile;

impl FileService for AndroidFile {
    fn unzip(&self, zip_path: &str, dest_dir: &str) -> Result<usize, ServiceError> {
        Ok(jni::outbound::unzip_file(zip_path, dest_dir)?)
    }
}

// ==================== Image API ====================

struct AndroidImageApi {
    session: SessionGate,
}

impl ImageApiService for AndroidImageApi {
    fn save_image_to_photos_album(&self, options_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::WritePhotosAlbum), || {
            jni::image_save_to_photos_album(self.session.host_id(), options_json)
        })
    }

    fn preview_media(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::image_preview_media(
            self.session.host_id(),
            options_json,
        )?)
    }

    fn preview_image(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::image_preview_image(
            self.session.host_id(),
            options_json,
        )?)
    }

    fn compress_image(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::image_compress(self.session.host_id(), options_json)?)
    }

    fn choose_message_file(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::image_choose_message_file(
            self.session.host_id(),
            options_json,
        )?)
    }

    fn choose_image(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::image_choose_image(
            self.session.host_id(),
            options_json,
        )?)
    }

    fn choose_media(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::image_choose_media(
            self.session.host_id(),
            options_json,
        )?)
    }
}

// ==================== Video ====================

struct AndroidVideo {
    host_id: i32,
}

/// Every request goes to the SDK as content made it, `{"videoId", ...}`; a call
/// is answered through `onVideoResult` once it has happened.
impl VideoService for AndroidVideo {
    fn create(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_create(self.host_id, request_json)?)
    }
    fn destroy(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_destroy(self.host_id, request_json)?)
    }
    fn set(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_set(self.host_id, request_json)?)
    }
    fn play(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_play(self.host_id, request_json)?)
    }
    fn pause(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_pause(self.host_id, request_json)?)
    }
    fn stop(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_stop(self.host_id, request_json)?)
    }
    fn seek(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_seek(self.host_id, request_json)?)
    }
    fn request_full_screen(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_request_full_screen(self.host_id, request_json)?)
    }
    fn exit_full_screen(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::video_exit_full_screen(self.host_id, request_json)?)
    }
}

// ==================== Location ====================

struct AndroidLocation {
    session: SessionGate,
}

impl LocationService for AndroidLocation {
    fn get_location(&self, options_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::UserLocation), || {
            jni::get_location(self.session.host_id(), options_json)
        })
    }

    fn get_fuzzy_location(&self, options_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::UserLocation), || {
            jni::get_fuzzy_location(self.session.host_id(), options_json)
        })
    }
}

// ==================== Scan Code ====================

struct AndroidScanCode {
    host_id: i32,
}

impl ScanCodeService for AndroidScanCode {
    fn scan_code(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::scan_code(self.host_id, options_json)?)
    }
}

// ==================== Game Log ====================

struct AndroidGameLog {
    session: SessionGate,
}

impl GameLogService for AndroidGameLog {
    fn report_log(&self, log_json: &str) -> Result<(), ServiceError> {
        Ok(jni::game_log_report(self.session.host_id(), log_json)?)
    }
}

// ==================== Permission ====================

// Consumed by scripts/test-permission-coverage-contract.sh via source
// parsing, not by Rust call sites; that is why dead_code is allowed.
#[allow(dead_code)]
pub(crate) const ANDROID_PERMISSION_GATED_METHODS: &[(&str, Scope)] = &[
    ("RecorderService::start", Scope::Record),
    ("RecorderService::pause", Scope::Record),
    ("RecorderService::resume", Scope::Record),
    ("CameraService::create", Scope::Camera),
    ("CameraService::set", Scope::Camera),
    ("CameraService::take_photo", Scope::Camera),
    ("CameraService::start_record", Scope::Camera),
    ("CameraService::set_zoom", Scope::Camera),
    ("CameraService::listen_frame_change", Scope::Camera),
    ("BluetoothService::open_adapter", Scope::Bluetooth),
    ("BluetoothService::get_adapter_state", Scope::Bluetooth),
    (
        "BluetoothService::start_devices_discovery",
        Scope::Bluetooth,
    ),
    ("BluetoothService::get_devices", Scope::Bluetooth),
    ("BluetoothService::get_connected_devices", Scope::Bluetooth),
    ("BluetoothService::make_pair", Scope::Bluetooth),
    ("BluetoothService::is_device_paired", Scope::Bluetooth),
    ("BluetoothService::start_beacon_discovery", Scope::Bluetooth),
    ("BluetoothService::get_beacons", Scope::Bluetooth),
    ("BluetoothService::create_ble_connection", Scope::Bluetooth),
    (
        "BluetoothService::get_ble_device_services",
        Scope::Bluetooth,
    ),
    (
        "BluetoothService::get_ble_device_characteristics",
        Scope::Bluetooth,
    ),
    (
        "BluetoothService::read_ble_characteristic_value",
        Scope::Bluetooth,
    ),
    (
        "BluetoothService::write_ble_characteristic_value",
        Scope::Bluetooth,
    ),
    (
        "BluetoothService::notify_ble_characteristic_value_change",
        Scope::Bluetooth,
    ),
    ("BluetoothService::get_ble_device_rssi", Scope::Bluetooth),
    ("BluetoothService::set_ble_mtu", Scope::Bluetooth),
    ("BluetoothService::get_ble_mtu", Scope::Bluetooth),
    ("BlePeripheralService::create_server", Scope::Bluetooth),
    ("BlePeripheralService::add_service", Scope::Bluetooth),
    ("BlePeripheralService::start_advertising", Scope::Bluetooth),
    (
        "BlePeripheralService::write_characteristic_value",
        Scope::Bluetooth,
    ),
    (
        "ImageApiService::save_image_to_photos_album",
        Scope::WritePhotosAlbum,
    ),
    ("LocationService::get_location", Scope::UserLocation),
    ("LocationService::get_fuzzy_location", Scope::UserLocation),
    ("AuthService::get_user_info", Scope::UserInfo),
];

// Consumed by scripts/test-permission-coverage-contract.sh via source
// parsing, not by Rust call sites; that is why dead_code is allowed.
#[allow(dead_code)]
pub(crate) const ANDROID_PERMISSION_CLEANUP_METHODS: &[(&str, Scope)] = &[
    ("RecorderService::stop", Scope::Record),
    ("CameraService::destroy", Scope::Camera),
    ("CameraService::stop_record", Scope::Camera),
    ("CameraService::close_frame_change", Scope::Camera),
    ("BluetoothService::close_adapter", Scope::Bluetooth),
    ("BluetoothService::stop_devices_discovery", Scope::Bluetooth),
    ("BluetoothService::stop_beacon_discovery", Scope::Bluetooth),
    ("BluetoothService::close_ble_connection", Scope::Bluetooth),
    ("BlePeripheralService::remove_service", Scope::Bluetooth),
    ("BlePeripheralService::stop_advertising", Scope::Bluetooth),
];

/// Scope decisions the host has pushed for a session.
///
/// Cached on this side rather than fetched per check: `scope_state` runs on
/// every gated op -- a Bluetooth scan touches it repeatedly -- and a JNI
/// round-trip there would put a cross-language hop on a hot path to answer a
/// question whose answer changes only when a user acts.
///
/// The host is still the authority; this is its answer, held where the check
/// happens. `NativeExports.updatePermission` writes it, at session start and
/// whenever a decision changes.
static PERMISSION_GATE: OnceLock<PermissionGate> = OnceLock::new();

fn permission_gate() -> &'static PermissionGate {
    PERMISSION_GATE.get_or_init(PermissionGate::default)
}

/// Record a decision and, on denial, tear down the matching Java resource while
/// protected calls are excluded by the same session gate.
pub(crate) fn update_permission<E>(
    host_id: i32,
    scope: Scope,
    granted: bool,
    cleanup: impl FnOnce() -> Result<(), E>,
) -> Result<(), crate::android_permission_gate::UpdateError<E>> {
    permission_gate().update(host_id, scope, granted, cleanup)
}

/// Drop a session's decisions when it ends, so a later session on the same host
/// id cannot inherit them.
pub(crate) fn clear_permissions(host_id: i32) {
    permission_gate().clear(host_id);
}

struct AndroidPermission {
    session: SessionGate,
}

impl PermissionService for AndroidPermission {
    fn scope_state(&self, scope: Scope) -> ScopeState {
        let decided = self.session.scope_state(scope);
        match decided {
            Some(true) => ScopeState::Granted,
            Some(false) => ScopeState::Denied,
            // Absent means the host has not spoken about this scope. Not the
            // same as a refusal: content may still ask, and `authorize` is how.
            None => ScopeState::Unknown,
        }
    }

    fn request_scope(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::permission_request(
            self.session.host_id(),
            request_json,
        )?)
    }
}

// ==================== Auth ====================

struct AndroidAuth {
    session: SessionGate,
}

impl AuthService for AndroidAuth {
    fn login(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::auth_login(self.session.host_id(), options_json)?)
    }

    fn check_session(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::auth_check_session(
            self.session.host_id(),
            options_json,
        )?)
    }

    fn get_user_info(&self, options_json: &str) -> Result<(), ServiceError> {
        permission_jni_call(&self.session, Some(Scope::UserInfo), || {
            jni::auth_get_user_info(self.session.host_id(), options_json)
        })
    }

    fn get_phone_number(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::auth_get_phone_number(
            self.session.host_id(),
            options_json,
        )?)
    }
}

// ==================== Subpackage ====================

struct AndroidSubpackage {
    host_id: i32,
}

impl SubpackageService for AndroidSubpackage {
    fn download_subpackage(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::subpackage_download(self.host_id, options_json)?)
    }
}

// ==================== Share ====================

struct AndroidShare {
    host_id: i32,
}

impl ShareService for AndroidShare {
    fn share_app_message(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::share_app_message(self.host_id, options_json)?)
    }

    fn share_message_to_friend(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::share_message_to_friend(self.host_id, options_json)?)
    }

    fn show_share_image_menu(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::show_share_image_menu(self.host_id, options_json)?)
    }

    fn set_share_menu(&self, json: &str) -> Result<(), ServiceError> {
        Ok(jni::share_menu_changed(self.host_id, json)?)
    }

    fn menu_share_reply(&self, json: &str) -> Result<(), ServiceError> {
        Ok(jni::share_menu_reply(self.host_id, json)?)
    }
}

// ==================== Navigate ====================

struct AndroidNavigate {
    host_id: i32,
}

impl NavigateService for AndroidNavigate {
    fn navigate_to_mini_program(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::navigate_to_mini_program(self.host_id, options_json)?)
    }

    fn navigate_back_mini_program(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::navigate_back_mini_program(self.host_id, options_json)?)
    }

    fn open_customer_service_conversation(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::open_customer_service_conversation(
            self.host_id,
            options_json,
        )?)
    }
}

// ==================== Gamepad ====================

/// The motors of the session's gamepads, which the SDK announces with the
/// effects each plays: their vibrators, through `InputDevice`.
struct AndroidGamepad {
    host_id: i32,
}

impl GamepadService for AndroidGamepad {
    fn rumble(&self, rumble: &GamepadRumble) -> Result<(), ServiceError> {
        Ok(jni::gamepad_rumble(
            self.host_id,
            rumble.index,
            rumble.strong_magnitude,
            rumble.weak_magnitude,
            rumble.duration_ms,
        )?)
    }
}

// ==================== Desktop window ====================

/// The game's view on a desktop-form device: its pointer icon and pointer
/// capture. An Android app cannot size its own window, so `setWindowSize` keeps
/// the trait's not supported.
struct AndroidWindow {
    host_id: i32,
}

impl WindowService for AndroidWindow {
    fn set_cursor(&self, json: &str) -> Result<(), ServiceError> {
        if jni::set_cursor(self.host_id, json)? {
            Ok(())
        } else {
            Err(ServiceError::not_supported("setCursor:fail not supported"))
        }
    }

    fn request_pointer_lock(&self) -> Result<(), ServiceError> {
        Ok(jni::request_pointer_lock(self.host_id)?)
    }

    fn exit_pointer_lock(&self) -> Result<(), ServiceError> {
        Ok(jni::exit_pointer_lock(self.host_id)?)
    }
}

// ==================== Ecosystem ====================

/// Forwards the host's ecosystem to the Java `EcosystemHandler` the embedder
/// installed; with none installed there is no ecosystem.
struct AndroidEcosystem {
    host_id: i32,
}

impl EcosystemService for AndroidEcosystem {
    fn available(&self) -> bool {
        jni::ecosystem_available(self.host_id).unwrap_or(false)
    }

    fn call(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::ecosystem_call(self.host_id, request_json)?)
    }

    fn reply(&self, json: &str) -> Result<(), ServiceError> {
        Ok(jni::ecosystem_reply(self.host_id, json)?)
    }

    fn value(&self, name: &str) -> Option<String> {
        jni::ecosystem_value(self.host_id, name)
            .ok()
            .filter(|json| !json.is_empty())
    }
}

// ==================== Updates ====================

/// Forwards content's `applyUpdate` to the Java `UpdateHandler`; the handler's
/// reports reach content on the `onUpdateEvent` channel.
#[cfg(feature = "api-system")]
struct AndroidUpdate {
    host_id: i32,
}

#[cfg(feature = "api-system")]
impl UpdateService for AndroidUpdate {
    fn apply_update(&self) -> Result<(), ServiceError> {
        Ok(jni::apply_update(self.host_id)?)
    }
}

// ==================== Ads ====================

/// Forwards ad commands to the Java `AdHandler` the embedder installed.
///
/// Nothing is decided here: the reward verdict for incentivised video is
/// whatever the host's ad SDK reports on the `onAdEvent` channel. See
/// `shared/src/services/ad.rs` for why that has to be true.
struct AndroidAd {
    host_id: i32,
}

impl AdService for AndroidAd {
    fn create_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::ad_create(self.host_id, request_json)?)
    }

    fn load_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::ad_load(self.host_id, request_json)?)
    }

    fn show_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::ad_show(self.host_id, request_json)?)
    }

    fn hide_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::ad_hide(self.host_id, request_json)?)
    }

    fn update_ad_style(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::ad_update_style(self.host_id, request_json)?)
    }

    fn destroy_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        Ok(jni::ad_destroy(self.host_id, request_json)?)
    }
}

// ==================== Payment ====================

struct AndroidPayment {
    host_id: i32,
}

impl PaymentService for AndroidPayment {
    fn check_is_support_midas_payment(&self, options_json: &str) -> Result<String, ServiceError> {
        Ok(jni::check_is_support_midas_payment(
            self.host_id,
            options_json,
        )?)
    }

    fn request_midas_payment(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::request_midas_payment(self.host_id, options_json)?)
    }

    fn request_midas_payment_game_item(&self, options_json: &str) -> Result<(), ServiceError> {
        Ok(jni::request_midas_payment_game_item(
            self.host_id,
            options_json,
        )?)
    }
}
