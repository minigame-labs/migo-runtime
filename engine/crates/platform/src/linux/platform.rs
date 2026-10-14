use migo_core::services::{
    CommerceServices, ConnectivityServices, DeviceServices, KeyboardService, MediaServices,
    SensorServices, SystemInfoService, SystemUtilServices,
};
use migo_core::{DeviceServiceProvider, FrameClock, HostNotifier};
use shared::services::app_info::HostAppInfo;
use shared::surface::{HostWindowInfo, HostWindowState};
use std::sync::Arc;

pub struct LinuxPlatform {
    /// Supplied by an embedding host that services the keyboard itself.
    ///
    /// The Linux engine layer has no keyboard of its own, so this is the only way content's
    /// `migo.showKeyboard` reaches anything at all.
    host_keyboard: Option<Arc<dyn KeyboardService>>,
    /// The window the host is presenting into, in physical pixels.
    ///
    /// Only the host knows this -- the engine layer is handed a surface, not a
    /// window -- and without it `migo.getSystemInfoSync()` reports a zero-sized
    /// window, so content that lays itself out from `windowWidth`/`windowHeight`
    /// stacks everything at the origin. Nothing in the rendering path is wrong
    /// when that happens, which is why no rendering test catches it.
    window: Option<Arc<HostWindowState>>,
    /// What the host says about itself.
    host_app: HostAppInfo,
}

impl LinuxPlatform {
    pub fn new() -> Self {
        Self {
            host_keyboard: None,
            window: None,
            host_app: HostAppInfo::default(),
        }
    }

    /// Build a platform that offers a host-supplied keyboard.
    ///
    /// The capability is offered exactly when the host supplied one, which is
    /// the rule `FrameClock::uses_external_vsync` already follows: report what
    /// the host actually installed, not what the platform would prefer.
    pub fn with_host_keyboard(host_keyboard: Option<Arc<dyn KeyboardService>>) -> Self {
        Self {
            host_keyboard,
            window: None,
            host_app: HostAppInfo::default(),
        }
    }

    /// Report the window the host is presenting into.
    ///
    /// Shared rather than copied, because a Linux window resizes while the
    /// session runs: the host keeps its own handle to this state and publishes a
    /// new measurement through `HostWindowState::replace`, which every service
    /// already handed to content reads. Taking a value here instead would
    /// confidently report the size the window had at start-up, which is the
    /// exact defect this exists to fix.
    pub fn with_window(mut self, window: Arc<HostWindowState>) -> Self {
        self.window = Some(window);
        self
    }

    /// What the host says about itself -- its id (`host.appId`) above all, which the platform has no way to know.
    pub fn with_host_app(mut self, app: HostAppInfo) -> Self {
        self.host_app = app;
        self
    }
}

impl Default for LinuxPlatform {
    fn default() -> Self {
        Self::new()
    }
}

/// Linux's bundle exists only to carry host-supplied capabilities.
///
/// Every other accessor keeps its default. That is not an omission: Linux has
/// nothing behind them to forward to, and a forwarding layer that answered
/// `None` for a service the platform gained later would drop it silently.
struct LinuxDeviceServices {
    host_keyboard: Option<Arc<dyn KeyboardService>>,
    window: Option<Arc<HostWindowState>>,
    host_app: HostAppInfo,
}

impl SensorServices for LinuxDeviceServices {}
impl MediaServices for LinuxDeviceServices {}
impl ConnectivityServices for LinuxDeviceServices {}
impl CommerceServices for LinuxDeviceServices {}
impl SystemUtilServices for LinuxDeviceServices {
    fn keyboard(&self) -> Option<Arc<dyn KeyboardService>> {
        self.host_keyboard.clone()
    }

    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        Some(Arc::new(
            HostWindowInfo::new(Arc::clone(self.window.as_ref()?)).with_app(self.host_app.clone()),
        ))
    }
}

impl DeviceServiceProvider for LinuxPlatform {
    fn create_device_services(&self, _host_id: i32) -> Option<Arc<dyn DeviceServices>> {
        // Returning a bundle unconditionally would change what every existing
        // Linux caller sees; with nothing supplied there is still nothing to
        // offer.
        if self.host_keyboard.is_none() && self.window.is_none() {
            return None;
        }
        Some(Arc::new(LinuxDeviceServices {
            host_keyboard: self.host_keyboard.clone(),
            window: self.window.clone(),
            host_app: self.host_app.clone(),
        }))
    }
}

impl FrameClock for LinuxPlatform {}

impl HostNotifier for LinuxPlatform {}

// Nothing per-session lives outside the isolate here, so a runtime
// replacement needs no bookkeeping on this platform.
impl migo_core::RuntimeGenerationNotifier for LinuxPlatform {}

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

    #[test]
    fn linux_player_keeps_software_frame_ticker() {
        assert!(!LinuxPlatform::new().uses_external_vsync());
    }

    /// The Linux engine layer has no device services of its own, and that must not change for
    /// a host that supplies nothing.
    #[test]
    fn a_host_that_supplies_nothing_still_gets_no_device_services() {
        assert!(LinuxPlatform::new().create_device_services(1).is_none());
    }

    /// Content lays itself out from `windowWidth`/`windowHeight`, so those have
    /// to describe the window it is drawing into. They previously came back as
    /// zero, which puts a control positioned at `windowHeight - 40` at the top
    /// of the screen -- with the rendering path entirely correct, so no
    /// rendering test would ever catch it.
    ///
    /// What the reported numbers should be is `host_window`'s to pin; this is
    /// about a supplied window reaching content at all.
    #[test]
    fn a_host_supplied_window_is_reported_to_content() {
        let json = LinuxPlatform::new()
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

    /// Who the host is reaches content (`host.appId`): a Linux app has no
    /// identifier the system hands out, so only the host can say.
    #[test]
    fn the_host_s_own_id_is_reported_to_content() {
        let info = LinuxPlatform::new()
            .with_window(window(720, 1280))
            .with_host_app(HostAppInfo {
                host_app_id: Some("com.example.game-host".to_string()),
                ..HostAppInfo::default()
            })
            .create_device_services(1)
            .expect("bundle")
            .system_info()
            .expect("system info")
            .host_app_info();
        assert_eq!(info.host_app_id.as_deref(), Some("com.example.game-host"));
    }

    /// A window and a keyboard are independent: supplying one must not withdraw
    /// the other. They arrive through separate constructors, which is exactly
    /// the shape that quietly loses one of them.
    #[test]
    fn a_window_and_a_keyboard_are_both_offered() {
        let services = LinuxPlatform::with_host_keyboard(Some(Arc::new(HostKeyboard)))
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
        let platform = LinuxPlatform::with_host_keyboard(Some(Arc::new(HostKeyboard)));
        assert!(
            platform
                .create_device_services(1)
                .expect("a supplied keyboard must produce a bundle")
                .system_info()
                .is_none(),
            "a keyboard-only host must not claim to know the window size"
        );
    }

    /// A host that supplies a keyboard gets a bundle that offers it -- and
    /// offers nothing else, because the Linux engine layer has nothing else to offer.
    #[test]
    fn a_host_supplied_keyboard_becomes_the_bundles_keyboard() {
        let platform = LinuxPlatform::with_host_keyboard(Some(Arc::new(HostKeyboard)));
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
