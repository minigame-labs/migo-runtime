//! Test support utilities: mock DeviceServices and helpers.
//!
//! Enable with `#[cfg(test)]` -- this module is only compiled for tests.

use crate::services::*;
use std::sync::Arc;

/// Mock implementation of all DeviceServices sub-traits.
///
/// All service getters return `None` by default. Use builder methods
/// to configure specific services for your test.
///
/// # Example
/// ```rust,ignore
/// let services = MockDeviceServices::new();
/// assert!(services.clipboard().is_none());
/// ```
pub struct MockDeviceServices {
    // -- SensorServices overrides --
    battery_svc: Option<Arc<dyn BatteryService>>,
    vibration_svc: Option<Arc<dyn VibrationService>>,
    screen_svc: Option<Arc<dyn ScreenService>>,
    device_motion_svc: Option<Arc<dyn DeviceMotionService>>,
    gyroscope_svc: Option<Arc<dyn GyroscopeService>>,
    compass_svc: Option<Arc<dyn CompassService>>,
    accelerometer_svc: Option<Arc<dyn AccelerometerService>>,

    // -- MediaServices overrides --
    audio_platform_svc: Option<Arc<dyn AudioPlatformService>>,
    recorder_svc: Option<Arc<dyn RecorderService>>,
    camera_svc: Option<Arc<dyn CameraService>>,
    image_api_svc: Option<Arc<dyn ImageApiService>>,

    // -- ConnectivityServices overrides --
    network_svc: Option<Arc<dyn NetworkService>>,
    bluetooth_svc: Option<Arc<dyn BluetoothService>>,
    location_svc: Option<Arc<dyn LocationService>>,

    // -- CommerceServices overrides --
    game_log_svc: Option<Arc<dyn GameLogService>>,
    auth_svc: Option<Arc<dyn AuthService>>,
    subpackage_svc: Option<Arc<dyn SubpackageService>>,
    share_svc: Option<Arc<dyn ShareService>>,
    payment_svc: Option<Arc<dyn PaymentService>>,

    // -- SystemUtilServices overrides --
    clipboard_svc: Option<Arc<dyn ClipboardService>>,
    keyboard_svc: Option<Arc<dyn KeyboardService>>,
    interaction_svc: Option<Arc<dyn InteractionService>>,
    system_info_svc: Option<Arc<dyn SystemInfoService>>,
    codec_svc: Option<Arc<dyn CodecService>>,
    file_svc: Option<Arc<dyn FileService>>,
    scan_code_svc: Option<Arc<dyn ScanCodeService>>,
    navigate_svc: Option<Arc<dyn NavigateService>>,
}

impl Default for MockDeviceServices {
    fn default() -> Self {
        Self::new()
    }
}

impl MockDeviceServices {
    /// Create a new mock with all services returning `None`.
    pub fn new() -> Self {
        Self {
            battery_svc: None,
            vibration_svc: None,
            screen_svc: None,
            device_motion_svc: None,
            gyroscope_svc: None,
            compass_svc: None,
            accelerometer_svc: None,
            audio_platform_svc: None,
            recorder_svc: None,
            camera_svc: None,
            image_api_svc: None,
            network_svc: None,
            bluetooth_svc: None,
            location_svc: None,
            game_log_svc: None,
            auth_svc: None,
            subpackage_svc: None,
            share_svc: None,
            payment_svc: None,
            clipboard_svc: None,
            keyboard_svc: None,
            interaction_svc: None,
            system_info_svc: None,
            codec_svc: None,
            file_svc: None,
            scan_code_svc: None,
            navigate_svc: None,
        }
    }

    // -- SensorServices builder methods --

    /// Set a custom battery service implementation.
    pub fn with_battery(mut self, svc: Arc<dyn BatteryService>) -> Self {
        self.battery_svc = Some(svc);
        self
    }

    /// Set a custom vibration service implementation.
    pub fn with_vibration(mut self, svc: Arc<dyn VibrationService>) -> Self {
        self.vibration_svc = Some(svc);
        self
    }

    /// Set a custom screen service implementation.
    pub fn with_screen(mut self, svc: Arc<dyn ScreenService>) -> Self {
        self.screen_svc = Some(svc);
        self
    }

    /// Set a custom device motion service implementation.
    pub fn with_device_motion(mut self, svc: Arc<dyn DeviceMotionService>) -> Self {
        self.device_motion_svc = Some(svc);
        self
    }

    /// Set a custom gyroscope service implementation.
    pub fn with_gyroscope(mut self, svc: Arc<dyn GyroscopeService>) -> Self {
        self.gyroscope_svc = Some(svc);
        self
    }

    /// Set a custom compass service implementation.
    pub fn with_compass(mut self, svc: Arc<dyn CompassService>) -> Self {
        self.compass_svc = Some(svc);
        self
    }

    /// Set a custom accelerometer service implementation.
    pub fn with_accelerometer(mut self, svc: Arc<dyn AccelerometerService>) -> Self {
        self.accelerometer_svc = Some(svc);
        self
    }

    // -- MediaServices builder methods --

    /// Set a custom audio platform service implementation.
    pub fn with_audio_platform(mut self, svc: Arc<dyn AudioPlatformService>) -> Self {
        self.audio_platform_svc = Some(svc);
        self
    }

    /// Set a custom recorder service implementation.
    pub fn with_recorder(mut self, svc: Arc<dyn RecorderService>) -> Self {
        self.recorder_svc = Some(svc);
        self
    }

    /// Set a custom camera service implementation.
    pub fn with_camera(mut self, svc: Arc<dyn CameraService>) -> Self {
        self.camera_svc = Some(svc);
        self
    }

    /// Set a custom image API service implementation.
    pub fn with_image_api(mut self, svc: Arc<dyn ImageApiService>) -> Self {
        self.image_api_svc = Some(svc);
        self
    }

    // -- ConnectivityServices builder methods --

    /// Set a custom network service implementation.
    pub fn with_network(mut self, svc: Arc<dyn NetworkService>) -> Self {
        self.network_svc = Some(svc);
        self
    }

    /// Set a custom bluetooth service implementation.
    pub fn with_bluetooth(mut self, svc: Arc<dyn BluetoothService>) -> Self {
        self.bluetooth_svc = Some(svc);
        self
    }

    /// Set a custom location service implementation.
    pub fn with_location(mut self, svc: Arc<dyn LocationService>) -> Self {
        self.location_svc = Some(svc);
        self
    }

    // -- CommerceServices builder methods --

    /// Set a custom game log service implementation.
    pub fn with_game_log(mut self, svc: Arc<dyn GameLogService>) -> Self {
        self.game_log_svc = Some(svc);
        self
    }

    /// Set a custom auth service implementation.
    pub fn with_auth(mut self, svc: Arc<dyn AuthService>) -> Self {
        self.auth_svc = Some(svc);
        self
    }

    /// Set a custom subpackage service implementation.
    pub fn with_subpackage(mut self, svc: Arc<dyn SubpackageService>) -> Self {
        self.subpackage_svc = Some(svc);
        self
    }

    /// Set a custom share service implementation.
    pub fn with_share(mut self, svc: Arc<dyn ShareService>) -> Self {
        self.share_svc = Some(svc);
        self
    }

    /// Set a custom payment service implementation.
    pub fn with_payment(mut self, svc: Arc<dyn PaymentService>) -> Self {
        self.payment_svc = Some(svc);
        self
    }

    // -- SystemUtilServices builder methods --

    /// Set a custom clipboard service implementation.
    pub fn with_clipboard(mut self, svc: Arc<dyn ClipboardService>) -> Self {
        self.clipboard_svc = Some(svc);
        self
    }

    /// Set a custom keyboard service implementation.
    pub fn with_keyboard(mut self, svc: Arc<dyn KeyboardService>) -> Self {
        self.keyboard_svc = Some(svc);
        self
    }

    /// Set a custom interaction service implementation.
    pub fn with_interaction(mut self, svc: Arc<dyn InteractionService>) -> Self {
        self.interaction_svc = Some(svc);
        self
    }

    /// Set a custom system info service implementation.
    pub fn with_system_info(mut self, svc: Arc<dyn SystemInfoService>) -> Self {
        self.system_info_svc = Some(svc);
        self
    }

    /// Set a custom codec service implementation.
    pub fn with_codec(mut self, svc: Arc<dyn CodecService>) -> Self {
        self.codec_svc = Some(svc);
        self
    }

    /// Set a custom file service implementation.
    pub fn with_file(mut self, svc: Arc<dyn FileService>) -> Self {
        self.file_svc = Some(svc);
        self
    }

    /// Set a custom scan code service implementation.
    pub fn with_scan_code(mut self, svc: Arc<dyn ScanCodeService>) -> Self {
        self.scan_code_svc = Some(svc);
        self
    }

    /// Set a custom navigate service implementation.
    pub fn with_navigate(mut self, svc: Arc<dyn NavigateService>) -> Self {
        self.navigate_svc = Some(svc);
        self
    }
}

// -- Sub-trait implementations --

impl SensorServices for MockDeviceServices {
    fn battery(&self) -> Option<Arc<dyn BatteryService>> {
        self.battery_svc.clone()
    }
    fn vibration(&self) -> Option<Arc<dyn VibrationService>> {
        self.vibration_svc.clone()
    }
    fn screen(&self) -> Option<Arc<dyn ScreenService>> {
        self.screen_svc.clone()
    }
    fn device_motion(&self) -> Option<Arc<dyn DeviceMotionService>> {
        self.device_motion_svc.clone()
    }
    fn gyroscope(&self) -> Option<Arc<dyn GyroscopeService>> {
        self.gyroscope_svc.clone()
    }
    fn compass(&self) -> Option<Arc<dyn CompassService>> {
        self.compass_svc.clone()
    }
    fn accelerometer(&self) -> Option<Arc<dyn AccelerometerService>> {
        self.accelerometer_svc.clone()
    }
}

impl MediaServices for MockDeviceServices {
    fn audio_platform(&self) -> Option<Arc<dyn AudioPlatformService>> {
        self.audio_platform_svc.clone()
    }
    fn recorder(&self) -> Option<Arc<dyn RecorderService>> {
        self.recorder_svc.clone()
    }
    fn camera(&self) -> Option<Arc<dyn CameraService>> {
        self.camera_svc.clone()
    }
    fn image_api(&self) -> Option<Arc<dyn ImageApiService>> {
        self.image_api_svc.clone()
    }
}

impl ConnectivityServices for MockDeviceServices {
    fn network(&self) -> Option<Arc<dyn NetworkService>> {
        self.network_svc.clone()
    }
    fn bluetooth(&self) -> Option<Arc<dyn BluetoothService>> {
        self.bluetooth_svc.clone()
    }
    fn location(&self) -> Option<Arc<dyn LocationService>> {
        self.location_svc.clone()
    }
}

impl CommerceServices for MockDeviceServices {
    fn game_log(&self) -> Option<Arc<dyn GameLogService>> {
        self.game_log_svc.clone()
    }
    fn auth(&self) -> Option<Arc<dyn AuthService>> {
        self.auth_svc.clone()
    }
    fn subpackage(&self) -> Option<Arc<dyn SubpackageService>> {
        self.subpackage_svc.clone()
    }
    fn share(&self) -> Option<Arc<dyn ShareService>> {
        self.share_svc.clone()
    }
    fn payment(&self) -> Option<Arc<dyn PaymentService>> {
        self.payment_svc.clone()
    }
}

impl SystemUtilServices for MockDeviceServices {
    fn clipboard(&self) -> Option<Arc<dyn ClipboardService>> {
        self.clipboard_svc.clone()
    }
    fn keyboard(&self) -> Option<Arc<dyn KeyboardService>> {
        self.keyboard_svc.clone()
    }
    fn interaction(&self) -> Option<Arc<dyn InteractionService>> {
        self.interaction_svc.clone()
    }
    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        self.system_info_svc.clone()
    }
    fn codec(&self) -> Option<Arc<dyn CodecService>> {
        self.codec_svc.clone()
    }
    fn file(&self) -> Option<Arc<dyn FileService>> {
        self.file_svc.clone()
    }
    fn scan_code(&self) -> Option<Arc<dyn ScanCodeService>> {
        self.scan_code_svc.clone()
    }
    fn navigate(&self) -> Option<Arc<dyn NavigateService>> {
        self.navigate_svc.clone()
    }
}

// DeviceServices is automatically implemented via the blanket impl in device.rs
// since MockDeviceServices implements all 5 sub-traits.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::error::ServiceError;

    // -- Verify MockDeviceServices implements DeviceServices --

    fn assert_device_services<T: DeviceServices>(_: &T) {}

    #[test]
    fn mock_implements_device_services() {
        let mock = MockDeviceServices::new();
        assert_device_services(&mock);
    }

    // -- Verify all getters return None by default --

    #[test]
    fn all_sensor_services_none_by_default() {
        let mock = MockDeviceServices::new();
        assert!(mock.battery().is_none());
        assert!(mock.vibration().is_none());
        assert!(mock.screen().is_none());
        assert!(mock.device_motion().is_none());
        assert!(mock.gyroscope().is_none());
        assert!(mock.compass().is_none());
        assert!(mock.accelerometer().is_none());
    }

    #[test]
    fn all_media_services_none_by_default() {
        let mock = MockDeviceServices::new();
        assert!(mock.audio_platform().is_none());
        assert!(mock.recorder().is_none());
        assert!(mock.camera().is_none());
        assert!(mock.image_api().is_none());
    }

    #[test]
    fn all_connectivity_services_none_by_default() {
        let mock = MockDeviceServices::new();
        assert!(mock.network().is_none());
        assert!(mock.bluetooth().is_none());
        assert!(mock.location().is_none());
    }

    #[test]
    fn all_commerce_services_none_by_default() {
        let mock = MockDeviceServices::new();
        assert!(mock.game_log().is_none());
        assert!(mock.auth().is_none());
        assert!(mock.subpackage().is_none());
        assert!(mock.share().is_none());
        assert!(mock.payment().is_none());
    }

    #[test]
    fn all_system_util_services_none_by_default() {
        let mock = MockDeviceServices::new();
        assert!(mock.clipboard().is_none());
        assert!(mock.keyboard().is_none());
        assert!(mock.interaction().is_none());
        assert!(mock.system_info().is_none());
        assert!(mock.codec().is_none());
        assert!(mock.file().is_none());
        assert!(mock.scan_code().is_none());
        assert!(mock.navigate().is_none());
    }

    // -- Verify Send + Sync --

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    #[test]
    fn mock_is_send_and_sync() {
        assert_send::<MockDeviceServices>();
        assert_sync::<MockDeviceServices>();
    }

    // -- Verify builder methods wire up services --

    struct StubBattery;
    impl BatteryService for StubBattery {
        fn get_info_json(&self) -> Result<String, ServiceError> {
            Ok(r#"{"level":42,"isCharging":false}"#.to_string())
        }
    }

    #[test]
    fn builder_sets_battery_service() {
        let mock = MockDeviceServices::new().with_battery(Arc::new(StubBattery));
        let svc = mock
            .battery()
            .expect("battery should be Some after with_battery");
        let info = svc.get_info_json().unwrap();
        assert!(info.contains("42"));
    }

    struct StubClipboard;
    impl ClipboardService for StubClipboard {
        fn get_data(&self, _request_json: &str) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    #[test]
    fn builder_sets_clipboard_service() {
        let mock = MockDeviceServices::new().with_clipboard(Arc::new(StubClipboard));
        let svc = mock
            .clipboard()
            .expect("clipboard should be Some after with_clipboard");
        assert!(svc.get_data(r#"{"requestId":1}"#).is_ok());
        assert!(
            svc.set_data(r#"{"requestId":2,"data":"x"}"#).is_err(),
            "a method the stub left alone keeps the default refusal"
        );
    }

    #[test]
    fn default_impl_matches_new() {
        let d = MockDeviceServices::default();
        assert!(d.battery().is_none());
        assert!(d.clipboard().is_none());
        assert!(d.network().is_none());
    }

    // -- Verify that mock can be wrapped in Arc for multi-threaded use --

    #[test]
    fn mock_can_be_arc_wrapped() {
        let mock = Arc::new(MockDeviceServices::new());
        let clone = Arc::clone(&mock);
        assert!(clone.battery().is_none());
    }

    // -- Verify Arc<MockDeviceServices> can be used as Arc<dyn DeviceServices> --

    #[test]
    fn mock_can_be_used_as_dyn_device_services() {
        let mock: Arc<dyn DeviceServices> = Arc::new(MockDeviceServices::new());
        assert!(mock.clipboard().is_none());
        assert!(mock.battery().is_none());
    }
}
