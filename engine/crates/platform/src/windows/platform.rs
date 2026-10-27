use migo_core::services::{
    CommerceServices, ConnectivityServices, DeviceServices, KeyboardService, MediaServices,
    SensorServices, SystemInfoService, SystemUtilServices,
};
use migo_core::{DeviceServiceProvider, FrameClock, HostNotifier};
use shared::services::app_info::HostAppInfo;
use shared::surface::{
    HostWindowInfo, HostWindowState, PublicSurfaceGeneration, SurfaceLossReason,
};
use std::sync::Arc;

/// Windows platform capabilities.
///
/// Shaped like `LinuxPlatform` rather than `AndroidPlatform` because the
/// situation is the same: the engine layer has no device services of its own
/// here, so anything content asks for has to come from the embedding host.
pub struct WindowsPlatform {
    /// Supplied by an embedding host that services the keyboard itself.
    host_keyboard: Option<Arc<dyn KeyboardService>>,
    /// The window the host is presenting into, in physical pixels.
    ///
    /// See `LinuxPlatform` — same gap, same fix, and the player drives both
    /// through the same code.
    window: Option<Arc<HostWindowState>>,
    /// What the host says about itself.
    host_app: HostAppInfo,
    /// Told what the engine reports to its host -- the launch's outcome, errors, an exit --
    /// when an embedder supplies one; see [`Self::with_notifier`].
    notifier: Option<Arc<dyn HostNotifier>>,
}

impl WindowsPlatform {
    pub fn new() -> Self {
        Self {
            host_keyboard: None,
            window: None,
            host_app: HostAppInfo::default(),
            notifier: None,
        }
    }

    /// Build a platform that offers a host-supplied keyboard.
    ///
    /// Offered exactly when the host supplied one — the same rule
    /// `FrameClock::uses_external_vsync` follows: report what the host actually
    /// installed, not what the platform would prefer.
    pub fn with_host_keyboard(host_keyboard: Option<Arc<dyn KeyboardService>>) -> Self {
        Self {
            host_keyboard,
            window: None,
            host_app: HostAppInfo::default(),
            notifier: None,
        }
    }

    /// Report the window the host is presenting into.
    ///
    /// Shared rather than copied, because a Windows window resizes while the
    /// session runs: the host keeps its own handle to this state and publishes
    /// a new measurement through `HostWindowState::replace`, which every
    /// service already handed to content reads. Taking a value here instead
    /// would confidently report the size the window had at start-up, which is
    /// the exact defect this exists to fix.
    pub fn with_window(mut self, window: Arc<HostWindowState>) -> Self {
        self.window = Some(window);
        self
    }

    /// What the host says about itself -- its id (`host.appId`) above all, which the platform has no way to know.
    pub fn with_host_app(mut self, app: HostAppInfo) -> Self {
        self.host_app = app;
        self
    }

    /// Tell `notifier` what the engine reports to its host: the launch's outcome, errors, the
    /// game asking to exit. Without one the reports go nowhere, as they did before: a headless
    /// run whose game failed to start waited out its whole present ceiling and then said only
    /// that nothing was presented, with the reason in the log.
    pub fn with_notifier(mut self, notifier: Arc<dyn HostNotifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }
}

impl Default for WindowsPlatform {
    fn default() -> Self {
        Self::new()
    }
}

/// Windows' bundle exists only to carry host-supplied capabilities.
///
/// Every other accessor keeps its default, which is not an omission: there is
/// nothing behind them to forward to yet, and a forwarding layer that answered
/// `None` for a service the platform gained later would drop it silently.
struct WindowsDeviceServices {
    host_keyboard: Option<Arc<dyn KeyboardService>>,
    window: Option<Arc<HostWindowState>>,
    host_app: HostAppInfo,
}

impl SensorServices for WindowsDeviceServices {}
impl MediaServices for WindowsDeviceServices {}
impl ConnectivityServices for WindowsDeviceServices {}
impl CommerceServices for WindowsDeviceServices {}
impl SystemUtilServices for WindowsDeviceServices {
    fn keyboard(&self) -> Option<Arc<dyn KeyboardService>> {
        self.host_keyboard.clone()
    }

    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        Some(Arc::new(
            HostWindowInfo::new(Arc::clone(self.window.as_ref()?)).with_app(self.host_app.clone()),
        ))
    }
}

impl DeviceServiceProvider for WindowsPlatform {
    fn create_device_services(&self, _host_id: i32) -> Option<Arc<dyn DeviceServices>> {
        // With nothing supplied there is still nothing to offer, so returning
        // a bundle unconditionally would only change what callers see.
        if self.host_keyboard.is_none() && self.window.is_none() {
            return None;
        }
        Some(Arc::new(WindowsDeviceServices {
            host_keyboard: self.host_keyboard.clone(),
            window: self.window.clone(),
            host_app: self.host_app.clone(),
        }))
    }
}

impl FrameClock for WindowsPlatform {}

impl HostNotifier for WindowsPlatform {
    fn notify_game_ready(&self, host_id: i32) {
        if let Some(notifier) = &self.notifier {
            notifier.notify_game_ready(host_id);
        }
    }

    fn notify_game_failed(&self, host_id: i32, error_code: u16, message: &str, detail: &str) {
        if let Some(notifier) = &self.notifier {
            notifier.notify_game_failed(host_id, error_code, message, detail);
        }
    }

    fn notify_exit(&self, host_id: i32) {
        if let Some(notifier) = &self.notifier {
            notifier.notify_exit(host_id);
        }
    }

    fn notify_restart(&self, host_id: i32, launch_options_json: &str) {
        if let Some(notifier) = &self.notifier {
            notifier.notify_restart(host_id, launch_options_json);
        }
    }

    fn notify_console(&self, host_id: i32, level: u8, message: &str) {
        if let Some(notifier) = &self.notifier {
            notifier.notify_console(host_id, level, message);
        }
    }

    fn notify_error(&self, host_id: i32, error_code: u16, message: &str, detail: &str) {
        if let Some(notifier) = &self.notifier {
            notifier.notify_error(host_id, error_code, message, detail);
        }
    }

    fn notify_surface_lost(
        &self,
        host_id: i32,
        public_generation: PublicSurfaceGeneration,
        reason: SurfaceLossReason,
    ) {
        if let Some(notifier) = &self.notifier {
            notifier.notify_surface_lost(host_id, public_generation, reason);
        }
    }

    fn notify_host_message(&self, host_id: i32, json: &str) {
        if let Some(notifier) = &self.notifier {
            notifier.notify_host_message(host_id, json);
        }
    }
}

// Nothing per-session lives outside the isolate here, so a runtime
// replacement needs no bookkeeping on this platform.
impl migo_core::RuntimeGenerationNotifier for WindowsPlatform {}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::protocol::error::ServiceError;
    use shared::surface::HostWindowMetrics;
    fn window(width: u32, height: u32) -> Arc<HostWindowState> {
        Arc::new(HostWindowState::new(HostWindowMetrics::new(
            width,
            height,
            shared::surface::PixelRatio::new(1.0).expect("test ratio"),
        )))
    }

    struct HostKeyboard;
    impl KeyboardService for HostKeyboard {
        fn show(&self, _options_json: &str) -> Result<(), ServiceError> {
            Ok(())
        }
    }

    /// No DWM/composition vsync channel is wired yet, so the engine must keep
    /// driving frames itself rather than wait for a signal nobody sends.
    #[test]
    fn windows_keeps_the_software_frame_ticker_until_a_host_supplies_vsync() {
        assert!(!WindowsPlatform::new().uses_external_vsync());
    }

    #[test]
    fn a_host_that_supplies_nothing_still_gets_no_device_services() {
        assert!(WindowsPlatform::new().create_device_services(1).is_none());
    }

    /// Same gap Linux had: without this, content laying itself out from
    /// `windowWidth`/`windowHeight` puts everything at the origin, and the
    /// rendering path stays entirely correct while it does.
    #[test]
    fn a_host_supplied_window_is_reported_to_content() {
        let json = WindowsPlatform::new()
            .with_window(window(720, 1280))
            .create_device_services(1)
            .expect("a supplied window must produce a bundle")
            .system_info()
            .expect("a supplied window must be reported")
            .get_window_info_json()
            .expect("window info must serialise");
        let info: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(info["window_width"], 720.0);
        assert_eq!(info["window_height"], 1280.0);
    }

    /// A window and a keyboard are independent: supplying one must not withdraw
    /// the other.
    #[test]
    fn a_window_and_a_keyboard_are_both_offered() {
        let services = WindowsPlatform::with_host_keyboard(Some(Arc::new(HostKeyboard)))
            .with_window(window(720, 1280))
            .create_device_services(1)
            .expect("bundle");
        assert!(services.keyboard().is_some(), "the keyboard was dropped");
        assert!(services.system_info().is_some(), "the window was dropped");
    }

    /// A host that supplied no window must not have one invented for it: a
    /// plausible-looking wrong number is harder to diagnose than an absent
    /// service, which content's own fallback already handles.
    #[test]
    fn a_host_that_supplied_no_window_reports_none() {
        assert!(
            WindowsPlatform::with_host_keyboard(Some(Arc::new(HostKeyboard)))
                .create_device_services(1)
                .expect("a supplied keyboard must produce a bundle")
                .system_info()
                .is_none(),
            "a keyboard-only host must not claim to know the window size"
        );
    }

    #[test]
    fn a_host_supplied_keyboard_becomes_the_bundles_keyboard() {
        let platform = WindowsPlatform::with_host_keyboard(Some(Arc::new(HostKeyboard)));
        let services = platform
            .create_device_services(1)
            .expect("a supplied keyboard must produce a bundle");
        assert!(services.clipboard().is_none());
        assert!(
            services
                .keyboard()
                .expect("the host's keyboard must be offered")
                .show("{}")
                .is_ok(),
            "the bundle must hand back the host's own keyboard, not a default"
        );
    }
}
