//! Platform services for a C host.
//!
//! A C host is its own platform integration boundary. It must expose only the
//! capabilities the host actually installed; forwarding Android's Java-backed
//! services from a pure-native embedding would advertise a JVM that does not
//! exist. Window information is the one always-available service because it is
//! supplied directly by the versioned Surface descriptor.

use migo_capi_abi::host_services::{
    MIGO_HOST_SERVICE_AD, MIGO_HOST_SERVICE_AUTH, MIGO_HOST_SERVICE_BLUETOOTH,
    MIGO_HOST_SERVICE_CLIPBOARD, MIGO_HOST_SERVICE_ECOSYSTEM, MIGO_HOST_SERVICE_IMAGE,
    MIGO_HOST_SERVICE_INTERACTION, MIGO_HOST_SERVICE_LOCATION, MIGO_HOST_SERVICE_MOTION,
    MIGO_HOST_SERVICE_NAVIGATE, MIGO_HOST_SERVICE_PAYMENT, MIGO_HOST_SERVICE_PERMISSION,
    MIGO_HOST_SERVICE_SCAN_CODE, MIGO_HOST_SERVICE_SCREEN, MIGO_HOST_SERVICE_SHARE,
    MIGO_HOST_SERVICE_SUBPACKAGE, MIGO_HOST_SERVICE_WINDOW,
};
use migo_core::services::{
    AccelerometerService, AdService, AuthService, BatteryService, BluetoothService,
    ClipboardService, CommerceServices, CompassService, ConnectivityServices, DeviceMotionService,
    EcosystemService, GameLogService, GyroscopeService, ImageApiService, InteractionService,
    KeyboardService, LocationService, MediaServices, NavigateService, NetworkService,
    PaymentService, PermissionService, ScanCodeService, ScreenService, SensorServices,
    ShareService, SubpackageService, SystemInfoService, SystemUtilServices, VibrationService,
    WindowService,
};
use migo_core::{DeviceServiceProvider, FrameClock, HostNotifier};
use shared::protocol::error::ServiceError;
use shared::surface::HostWindowState;
use std::sync::{Arc, Weak};

use crate::{
    MigoSession,
    callbacks::{
        MIGO_KEYBOARD_CONFIRM_DONE, MIGO_KEYBOARD_CONFIRM_GO, MIGO_KEYBOARD_CONFIRM_NEXT,
        MIGO_KEYBOARD_CONFIRM_SEARCH, MIGO_KEYBOARD_CONFIRM_SEND, MIGO_KEYBOARD_FLAG_CONFIRM_HOLD,
        MIGO_KEYBOARD_FLAG_MULTIPLE, MIGO_KEYBOARD_FLAG_NONE, MIGO_KEYBOARD_TYPE_NUMBER,
        MIGO_KEYBOARD_TYPE_TEXT, Notifier, ShowOptions,
    },
    device::{CapiDevice, DeviceState},
    host_services::CapiHostServices,
    settings::{CapiSystemInfo, HostReports},
};
use migo_capi_abi::MIGO_ERROR_INTERNAL;

/// The common mini-game platform's default when content does not ask for one.
const MINIGAME_DEFAULT_MAX_LENGTH: u32 = 140;

/// Translate the engine's internal options JSON into the owned form the C
/// struct is built from.
///
/// The JSON is how the engine already carries these options -- the Java SDK
/// parses the same string -- but it must not reach C. A host should not have to
/// link a JSON parser to open a keyboard, and the option set is small, closed
/// and stable, so the boundary translates it. That is what a boundary is for.
///
/// Malformed input yields the defaults rather than an error: the producer is
/// our own JS layer, so a parse failure is an engine bug, and refusing the call
/// would convert it into a content-visible one.
fn show_options_from_json(options_json: &str) -> ShowOptions {
    let parsed: serde_json::Value =
        serde_json::from_str(options_json).unwrap_or(serde_json::Value::Null);

    let mut flags = MIGO_KEYBOARD_FLAG_NONE;
    if parsed.get("multiple").and_then(|value| value.as_bool()) == Some(true) {
        flags |= MIGO_KEYBOARD_FLAG_MULTIPLE;
    }
    if parsed.get("confirmHold").and_then(|value| value.as_bool()) == Some(true) {
        flags |= MIGO_KEYBOARD_FLAG_CONFIRM_HOLD;
    }

    ShowOptions {
        flags,
        max_length: parsed
            .get("maxLength")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(MINIGAME_DEFAULT_MAX_LENGTH),
        confirm_type: match parsed.get("confirmType").and_then(|value| value.as_str()) {
            Some("next") => MIGO_KEYBOARD_CONFIRM_NEXT,
            Some("search") => MIGO_KEYBOARD_CONFIRM_SEARCH,
            Some("go") => MIGO_KEYBOARD_CONFIRM_GO,
            Some("send") => MIGO_KEYBOARD_CONFIRM_SEND,
            // "done" and anything unrecognised alike: the platform's default.
            _ => MIGO_KEYBOARD_CONFIRM_DONE,
        },
        keyboard_type: match parsed.get("keyboardType").and_then(|value| value.as_str()) {
            Some("number") => MIGO_KEYBOARD_TYPE_NUMBER,
            _ => MIGO_KEYBOARD_TYPE_TEXT,
        },
        default_value: parsed
            .get("defaultValue")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
    }
}

pub struct CapiHostKit {
    notifier: Option<Arc<Notifier>>,
    device_services: Arc<CapiDeviceServices>,
    session: Weak<MigoSession>,
}

impl CapiHostKit {
    pub fn new(
        notifier: Option<Arc<Notifier>>,
        session: Weak<MigoSession>,
        window: Arc<HostWindowState>,
        device: Arc<DeviceState>,
        reports: Arc<HostReports>,
    ) -> Self {
        // Offered exactly when the host installed the callbacks -- never
        // because the platform claims a keyboard. On Android the platform's own
        // accessor claims one unconditionally and reaches a JVM a pure-native
        // host does not have, so deferring to it would hand content a
        // capability that cannot work.
        let host_keyboard: Option<Arc<dyn KeyboardService>> = notifier
            .as_ref()
            .filter(|notifier| notifier.supplies_keyboard())
            .map(|notifier| {
                Arc::new(CapiKeyboard {
                    notifier: Arc::clone(notifier),
                }) as Arc<dyn KeyboardService>
            });
        // Built once and shared by every service the host declared; which of
        // them content is offered is decided per accessor, by the declaration.
        let host_services = notifier
            .as_ref()
            .map(|notifier| CapiHostServices::new(Arc::clone(notifier), Arc::clone(&reports)));
        // Also built once: content asks for system information on every
        // getWindowInfo and getSystemSetting, and a service that is the same
        // for the Host's life has no reason to be allocated per call.
        let system_info: Arc<dyn SystemInfoService> =
            Arc::new(CapiSystemInfo::new(window, reports, host_services.as_ref()));
        Self {
            device_services: Arc::new(CapiDeviceServices {
                keyboard: host_keyboard,
                system_info,
                device: CapiDevice::new(notifier.clone(), device),
                host_services,
            }),
            notifier,
            session,
        }
    }
}

struct CapiDeviceServices {
    keyboard: Option<Arc<dyn KeyboardService>>,
    system_info: Arc<dyn SystemInfoService>,
    device: Arc<CapiDevice>,
    host_services: Option<Arc<CapiHostServices>>,
}

impl CapiDeviceServices {
    /// The channel, when the host declared `service` on it. Offered exactly
    /// when declared -- the keyboard's rule -- so content's API for a service
    /// the host left out fails as it does on a platform without it.
    fn host_service(&self, service: u32) -> Option<Arc<CapiHostServices>> {
        self.host_services
            .as_ref()
            .filter(|services| services.supplies(service))
            .map(Arc::clone)
    }
}

impl SensorServices for CapiDeviceServices {
    fn battery(&self) -> Option<Arc<dyn BatteryService>> {
        Some(self.device.battery())
    }

    fn vibration(&self) -> Option<Arc<dyn VibrationService>> {
        self.device.vibration()
    }

    fn screen(&self) -> Option<Arc<dyn ScreenService>> {
        let keep_on = self.device.screen();
        let channel = self.host_service(MIGO_HOST_SERVICE_SCREEN);
        (keep_on.is_some() || channel.is_some())
            .then(|| Arc::new(CapiScreen { keep_on, channel }) as Arc<dyn ScreenService>)
    }

    fn accelerometer(&self) -> Option<Arc<dyn AccelerometerService>> {
        self.host_service(MIGO_HOST_SERVICE_MOTION)
            .map(|services| services as Arc<dyn AccelerometerService>)
    }

    fn gyroscope(&self) -> Option<Arc<dyn GyroscopeService>> {
        self.host_service(MIGO_HOST_SERVICE_MOTION)
            .map(|services| services as Arc<dyn GyroscopeService>)
    }

    fn compass(&self) -> Option<Arc<dyn CompassService>> {
        self.host_service(MIGO_HOST_SERVICE_MOTION)
            .map(|services| services as Arc<dyn CompassService>)
    }

    fn device_motion(&self) -> Option<Arc<dyn DeviceMotionService>> {
        self.host_service(MIGO_HOST_SERVICE_MOTION)
            .map(|services| services as Arc<dyn DeviceMotionService>)
    }
}

/// The screen as a C host supplies it: keeping it on is the typed
/// `on_keep_screen_on` callback the ABI has always had, and everything else is the
/// channel's screen service. Either may be absent; each method fails as not
/// supported without its own.
struct CapiScreen {
    keep_on: Option<Arc<dyn ScreenService>>,
    channel: Option<Arc<CapiHostServices>>,
}

impl CapiScreen {
    fn channel(&self, what: &str) -> Result<&CapiHostServices, ServiceError> {
        self.channel
            .as_deref()
            .ok_or_else(|| ServiceError::not_supported(format!("{what}:fail not supported")))
    }
}

impl ScreenService for CapiScreen {
    fn set_keep_screen_on(&self, keep_on: bool) -> Result<(), ServiceError> {
        match &self.keep_on {
            Some(screen) => screen.set_keep_screen_on(keep_on),
            None => Err(ServiceError::not_supported(
                "setKeepScreenOn:fail not supported",
            )),
        }
    }
    fn get_brightness(&self, request_json: &str) -> Result<(), ServiceError> {
        self.channel("getScreenBrightness")?
            .get_brightness(request_json)
    }
    fn set_brightness(&self, request_json: &str) -> Result<(), ServiceError> {
        self.channel("setScreenBrightness")?
            .set_brightness(request_json)
    }
    fn set_orientation(&self, request_json: &str) -> Result<(), ServiceError> {
        self.channel("setDeviceOrientation")?
            .set_orientation(request_json)
    }
    fn start_capture_screen(&self) -> Result<(), ServiceError> {
        self.channel("onUserCaptureScreen")?.start_capture_screen()
    }
    fn stop_capture_screen(&self) -> Result<(), ServiceError> {
        self.channel("offUserCaptureScreen")?.stop_capture_screen()
    }
    fn get_screen_recording_state(&self, request_json: &str) -> Result<(), ServiceError> {
        self.channel("getScreenRecordingState")?
            .get_screen_recording_state(request_json)
    }
    fn start_screen_recording_observer(&self) -> Result<(), ServiceError> {
        self.channel("onScreenRecordingStateChanged")?
            .start_screen_recording_observer()
    }
    fn stop_screen_recording_observer(&self) -> Result<(), ServiceError> {
        self.channel("offScreenRecordingStateChanged")?
            .stop_screen_recording_observer()
    }
    fn set_visual_effect_on_capture(&self, request_json: &str) -> Result<(), ServiceError> {
        self.channel("setVisualEffectOnCapture")?
            .set_visual_effect_on_capture(request_json)
    }
}

impl MediaServices for CapiDeviceServices {
    fn image_api(&self) -> Option<Arc<dyn ImageApiService>> {
        self.host_service(MIGO_HOST_SERVICE_IMAGE)
            .map(|services| services as Arc<dyn ImageApiService>)
    }
}

impl ConnectivityServices for CapiDeviceServices {
    fn network(&self) -> Option<Arc<dyn NetworkService>> {
        Some(self.device.network())
    }

    fn location(&self) -> Option<Arc<dyn LocationService>> {
        self.host_service(MIGO_HOST_SERVICE_LOCATION)
            .map(|services| services as Arc<dyn LocationService>)
    }

    fn bluetooth(&self) -> Option<Arc<dyn BluetoothService>> {
        self.host_service(MIGO_HOST_SERVICE_BLUETOOTH)
            .map(|services| services as Arc<dyn BluetoothService>)
    }
}

impl CommerceServices for CapiDeviceServices {
    fn game_log(&self) -> Option<Arc<dyn GameLogService>> {
        self.device.game_log()
    }

    fn ad(&self) -> Option<Arc<dyn AdService>> {
        self.host_service(MIGO_HOST_SERVICE_AD)
            .map(|services| services as Arc<dyn AdService>)
    }

    fn auth(&self) -> Option<Arc<dyn AuthService>> {
        self.host_service(MIGO_HOST_SERVICE_AUTH)
            .map(|services| services as Arc<dyn AuthService>)
    }

    fn share(&self) -> Option<Arc<dyn ShareService>> {
        self.host_service(MIGO_HOST_SERVICE_SHARE)
            .map(|services| services as Arc<dyn ShareService>)
    }

    fn payment(&self) -> Option<Arc<dyn PaymentService>> {
        self.host_service(MIGO_HOST_SERVICE_PAYMENT)
            .map(|services| services as Arc<dyn PaymentService>)
    }

    fn subpackage(&self) -> Option<Arc<dyn SubpackageService>> {
        self.host_service(MIGO_HOST_SERVICE_SUBPACKAGE)
            .map(|services| services as Arc<dyn SubpackageService>)
    }
}

impl SystemUtilServices for CapiDeviceServices {
    fn keyboard(&self) -> Option<Arc<dyn KeyboardService>> {
        self.keyboard.clone()
    }

    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        Some(Arc::clone(&self.system_info))
    }

    /// Without one every scope is denied, which is the engine's answer to a
    /// question nobody can be asked.
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        self.host_service(MIGO_HOST_SERVICE_PERMISSION)
            .map(|services| services as Arc<dyn PermissionService>)
    }

    fn interaction(&self) -> Option<Arc<dyn InteractionService>> {
        self.host_service(MIGO_HOST_SERVICE_INTERACTION)
            .map(|services| services as Arc<dyn InteractionService>)
    }

    fn clipboard(&self) -> Option<Arc<dyn ClipboardService>> {
        self.host_service(MIGO_HOST_SERVICE_CLIPBOARD)
            .map(|services| services as Arc<dyn ClipboardService>)
    }

    fn scan_code(&self) -> Option<Arc<dyn ScanCodeService>> {
        self.host_service(MIGO_HOST_SERVICE_SCAN_CODE)
            .map(|services| services as Arc<dyn ScanCodeService>)
    }

    fn ecosystem(&self) -> Option<Arc<dyn EcosystemService>> {
        self.host_service(MIGO_HOST_SERVICE_ECOSYSTEM)
            .map(|services| services as Arc<dyn EcosystemService>)
    }

    fn window(&self) -> Option<Arc<dyn WindowService>> {
        self.host_service(MIGO_HOST_SERVICE_WINDOW)
            .map(|services| services as Arc<dyn WindowService>)
    }

    fn navigate(&self) -> Option<Arc<dyn NavigateService>> {
        self.host_service(MIGO_HOST_SERVICE_NAVIGATE)
            .map(|services| services as Arc<dyn NavigateService>)
    }
}

/// Content's `migo.showKeyboard` and friends, routed to the host's callbacks.
///
/// Every call returns once the host's dispatcher has taken the task, not once
/// the host has acted: the ABI is asynchronous and cannot promise more. A
/// dispatcher that refuses becomes a `ServiceError`, so content sees
/// `showKeyboard:fail` rather than a success it cannot act on.
struct CapiKeyboard {
    notifier: Arc<Notifier>,
}

impl KeyboardService for CapiKeyboard {
    fn show(&self, options_json: &str) -> Result<(), ServiceError> {
        if self
            .notifier
            .show_keyboard(show_options_from_json(options_json))
        {
            Ok(())
        } else {
            Err(ServiceError::not_supported(
                "showKeyboard:fail host dispatcher refused the request",
            ))
        }
    }

    fn hide(&self) -> Result<(), ServiceError> {
        if self.notifier.hide_keyboard() {
            Ok(())
        } else {
            Err(ServiceError::not_supported(
                "hideKeyboard:fail host dispatcher refused the request",
            ))
        }
    }

    fn update(&self, value: &str) -> Result<(), ServiceError> {
        if self.notifier.update_keyboard(value.to_string()) {
            Ok(())
        } else {
            Err(ServiceError::not_supported(
                "updateKeyboard:fail host dispatcher refused the request",
            ))
        }
    }
}

impl DeviceServiceProvider for CapiHostKit {
    fn create_device_services(
        &self,
        host_id: i32,
    ) -> Option<std::sync::Arc<dyn shared::services::DeviceServices>> {
        let _ = host_id;
        Some(self.device_services.clone())
    }
}

impl FrameClock for CapiHostKit {
    /// Whether the host offered to drive frames, not what the platform would
    /// prefer.
    ///
    /// Android's own `PlatformServices` answers true and drives frames by
    /// calling into the Java SDK over JNI, which a C host does not have. The
    /// honest answer for a C host is whatever it actually supplied: a host that
    /// installed `on_request_frame` paces frames itself -- with AChoreographer,
    /// a compositor frame callback, whatever its platform offers -- and one
    /// that did not is paced by the engine.
    ///
    /// Answering the platform's preference instead left the engine waiting for
    /// a vsync nobody would deliver: on device the window attached, content
    /// loaded and reported ready, and not one frame was drawn.
    fn uses_external_vsync(&self) -> bool {
        self.notifier
            .as_ref()
            .is_some_and(|notifier| notifier.drives_frames())
    }

    fn request_vsync(&self, _host_id: i32) {
        if let Some(notifier) = &self.notifier {
            notifier.request_frame();
        }
    }
}

impl HostNotifier for CapiHostKit {
    fn notify_game_ready(&self, host_id: i32) {
        let _ = host_id;
        if let Some(notifier) = &self.notifier {
            notifier.ready();
        }
    }

    fn notify_exit(&self, host_id: i32) {
        let _ = host_id;
        if let Some(notifier) = &self.notifier {
            notifier.exit_requested();
        }
    }

    fn notify_error(&self, host_id: i32, code: u16, msg: &str, detail: &str) {
        let _ = host_id;
        if let Some(notifier) = &self.notifier {
            // The engine's own code space is not the ABI's, so report a stable
            // ABI code and carry the engine's numbering in the message where a
            // host can still log it.
            let text = if detail.is_empty() {
                format!("{msg} (engine code {code})")
            } else {
                format!("{msg}: {detail} (engine code {code})")
            };
            notifier.error(MIGO_ERROR_INTERNAL, text);
        }
    }

    fn notify_surface_lost(
        &self,
        host_id: i32,
        public_generation: shared::surface::PublicSurfaceGeneration,
        reason: shared::surface::SurfaceLossReason,
    ) {
        let _ = host_id;
        let generation = public_generation.get();
        let Some(session) = self.session.upgrade() else {
            return;
        };
        if !session.mark_surface_lost(generation, reason) {
            return;
        }
        if let Some(notifier) = &self.notifier {
            notifier.surface_lost(generation, reason.as_u32());
        }
    }
}

// A C ABI host owns no per-session objects that outlive the isolate: every
// resource it holds is reached through the session handle, which a restart does
// not replace.
impl migo_core::RuntimeGenerationNotifier for CapiHostKit {}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::surface::{HostWindowMetrics, PixelRatio};

    fn window(width: u32, height: u32, ratio: f32) -> Arc<HostWindowState> {
        Arc::new(HostWindowState::new(HostWindowMetrics::new(
            width,
            height,
            PixelRatio::new(ratio).expect("valid test ratio"),
        )))
    }

    fn session() -> Arc<MigoSession> {
        crate::test_support::callback_session_pin()
    }

    /// Every field must survive the trip into the C struct. A field dropped
    /// here is a keyboard that opens with the wrong type, or that loses the
    /// text content seeded into it, with nothing in the log to say so.
    /// The capability is offered exactly when the host installed the
    /// callbacks, never because the platform claims it. Answering the
    /// platform's preference instead of the host's reality is the mistake #47
    /// paid for on device with a black screen.
    ///
    /// Desktop-only: Android's bundle answers `Some` by design, and asserting
    /// `None` there would be asserting the opposite fact.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn without_host_callbacks_no_keyboard_is_offered() {
        let session = session();
        let kit = CapiHostKit::new(
            None,
            Arc::downgrade(&session),
            window(640, 480, 2.0),
            Arc::clone(&session.device),
            Arc::clone(&session.host_reports),
        );
        assert!(
            kit.create_device_services(1)
                .and_then(|services| services.keyboard())
                .is_none(),
            "a host that installed nothing must not be handed a keyboard"
        );
    }

    /// The whole wiring, end to end on the engine side: callbacks installed ->
    /// capability offered -> content's `show` reaches the host's function
    /// pointer. Each half is covered elsewhere; this is the only test that
    /// proves they are actually connected to each other.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn installed_callbacks_reach_content_as_a_working_keyboard() {
        use crate::callbacks::{MigoHostCallbacks, MigoKeyboardShowOptions};
        use crate::test_support::callback_session_pin;
        use migo_capi_abi::{MIGO_ABI_VERSION_CURRENT, MIGO_OK};
        use std::ffi::c_void;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static SHOWS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn dispatch(
            _dispatcher: *mut c_void,
            task: crate::callbacks::MigoTaskFn,
            context: *mut c_void,
        ) -> migo_capi_abi::MigoResult {
            unsafe { task(context) };
            MIGO_OK
        }
        unsafe extern "C" fn show(
            _user: *mut c_void,
            _session: *mut c_void,
            _options: *const MigoKeyboardShowOptions,
        ) {
            SHOWS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe extern "C" fn hide(_user: *mut c_void, _session: *mut c_void) {}
        unsafe extern "C" fn update(
            _user: *mut c_void,
            _session: *mut c_void,
            _value: *const std::os::raw::c_char,
            _length: u32,
        ) {
        }

        let raw = MigoHostCallbacks {
            header: migo_capi_abi::VersionedHeader {
                struct_size: size_of::<MigoHostCallbacks>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            user_data: std::ptr::null_mut(),
            dispatcher_data: std::ptr::null_mut(),
            dispatch: Some(dispatch),
            on_ready: None,
            on_error: None,
            on_exit_requested: None,
            on_surface_lost: None,
            on_request_frame: None,
            on_show_keyboard: Some(show),
            on_hide_keyboard: Some(hide),
            on_update_keyboard: Some(update),
            on_surface_released: None,
            on_vibrate: None,
            on_keep_screen_on: None,
            on_game_log: None,
            on_host_service_call: None,
            host_services: 0,
        };
        let session = callback_session_pin();
        let notifier = Arc::new(Notifier::new(
            raw.validate()
                .expect("all three verbs are valid")
                .expect("dispatcher is configured"),
            Arc::downgrade(&session),
        ));

        SHOWS.store(0, Ordering::SeqCst);
        let kit = CapiHostKit::new(
            Some(notifier),
            Arc::downgrade(&session),
            window(640, 480, 2.0),
            Arc::clone(&session.device),
            Arc::clone(&session.host_reports),
        );
        let keyboard = kit
            .create_device_services(1)
            .and_then(|services| services.keyboard())
            .expect("an installed keyboard must be offered to content");

        assert!(keyboard.show("{}").is_ok());
        assert_eq!(
            SHOWS.load(Ordering::SeqCst),
            1,
            "content's show must reach the host's own callback"
        );
    }

    #[test]
    fn every_option_is_translated() {
        let options = show_options_from_json(
            r#"{"defaultValue":"seed","maxLength":140,"multiple":true,
                "confirmHold":true,"confirmType":"search","keyboardType":"number"}"#,
        );
        assert_eq!(options.default_value, "seed");
        assert_eq!(options.max_length, 140);
        assert_eq!(
            options.flags,
            MIGO_KEYBOARD_FLAG_MULTIPLE | MIGO_KEYBOARD_FLAG_CONFIRM_HOLD
        );
        assert_eq!(options.confirm_type, MIGO_KEYBOARD_CONFIRM_SEARCH);
        assert_eq!(options.keyboard_type, MIGO_KEYBOARD_TYPE_NUMBER);
    }

    /// The common mini-game platform's own defaults, so content that passes nothing gets the keyboard it
    /// would get on the platform this API was cloned from.
    #[test]
    fn absent_fields_fall_back_to_the_platform_defaults() {
        let options = show_options_from_json("{}");
        assert_eq!(options.default_value, "");
        assert_eq!(options.max_length, MINIGAME_DEFAULT_MAX_LENGTH);
        assert_eq!(options.flags, MIGO_KEYBOARD_FLAG_NONE);
        assert_eq!(options.confirm_type, MIGO_KEYBOARD_CONFIRM_DONE);
        assert_eq!(options.keyboard_type, MIGO_KEYBOARD_TYPE_TEXT);
    }

    /// The producer is our own JS layer, so malformed input is an engine bug.
    /// Refusing to open the keyboard would turn it into a content-visible one,
    /// which is strictly worse than opening a default keyboard.
    #[test]
    fn malformed_json_yields_the_defaults_rather_than_failing() {
        let options = show_options_from_json("this is not json");
        assert_eq!(options.max_length, MINIGAME_DEFAULT_MAX_LENGTH);
        assert_eq!(options.keyboard_type, MIGO_KEYBOARD_TYPE_TEXT);
    }

    #[test]
    fn an_unknown_confirm_type_falls_back_to_done() {
        let options = show_options_from_json(r#"{"confirmType":"teleport"}"#);
        assert_eq!(options.confirm_type, MIGO_KEYBOARD_CONFIRM_DONE);
    }

    /// A maxLength that does not fit a u32 must not wrap into a small limit:
    /// a keyboard that silently truncates at a wrapped length is worse than one
    /// that uses the documented default.
    #[test]
    fn an_out_of_range_max_length_falls_back_to_the_default() {
        let options = show_options_from_json(r#"{"maxLength":99999999999}"#);
        assert_eq!(options.max_length, MINIGAME_DEFAULT_MAX_LENGTH);
    }

    #[test]
    fn window_info_is_logical_and_tracks_the_latest_surface_metrics() {
        let state = window(1200, 800, 2.0);
        let session = session();
        let kit = CapiHostKit::new(
            None,
            Arc::downgrade(&session),
            Arc::clone(&state),
            Arc::clone(&session.device),
            Arc::clone(&session.host_reports),
        );
        let system = kit
            .create_device_services(1)
            .and_then(|services| services.system_info())
            .expect("a Surface descriptor always supplies window information");

        let initial: serde_json::Value = serde_json::from_str(
            &system
                .get_window_info_json()
                .expect("window JSON must serialize"),
        )
        .expect("valid JSON");
        assert_eq!(initial["pixel_ratio"], 2.0);
        assert_eq!(initial["window_width"], 600.0);
        assert_eq!(initial["window_height"], 400.0);
        assert_eq!(initial["safe_area"]["right"], 0.0);
        assert_eq!(initial["safe_area"]["bottom"], 0.0);

        state.replace(HostWindowMetrics::new(
            900,
            600,
            PixelRatio::new(1.5).expect("valid ratio"),
        ));
        let updated: serde_json::Value = serde_json::from_str(
            &system
                .get_window_info_json()
                .expect("window JSON must serialize"),
        )
        .expect("valid JSON");
        assert_eq!(updated["pixel_ratio"], 1.5);
        assert_eq!(updated["window_width"], 600.0);
        assert_eq!(updated["window_height"], 400.0);
    }

    #[test]
    fn readers_never_observe_fields_from_different_window_updates() {
        let state = window(100, 200, 1.0);
        let writer_state = Arc::clone(&state);
        let writer = std::thread::spawn(move || {
            let a = HostWindowMetrics::new(100, 200, PixelRatio::new(1.0).expect("valid ratio"));
            let b = HostWindowMetrics::new(300, 400, PixelRatio::new(2.0).expect("valid ratio"));
            for index in 0..20_000 {
                writer_state.replace(if index & 1 == 0 { a } else { b });
            }
        });

        for _ in 0..20_000 {
            let metrics = state.snapshot();
            assert!(
                metrics
                    == HostWindowMetrics::new(100, 200, PixelRatio::new(1.0).expect("valid ratio"),)
                    || metrics
                        == HostWindowMetrics::new(
                            300,
                            400,
                            PixelRatio::new(2.0).expect("valid ratio"),
                        )
            );
        }
        writer.join().expect("writer must finish");
    }
}
