//! `onWindowResize`: a listener hears the window's size change after it was added, once per change, with the size it
//! changed to -- as the reference documents ("the window's width after the change"), and nothing else.

use std::sync::{Arc, Mutex};

use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, SensorServices,
        SystemInfoService, SystemUtilServices,
    },
};

use super::support::{boot_with_services, run_to_idle};

/// A window whose size the test moves, in CSS pixels at two device pixels each.
#[derive(Clone)]
struct Window(Arc<Mutex<(u32, u32)>>);

impl SystemInfoService for Window {
    fn get_window_info_json(&self) -> Result<String, ServiceError> {
        let (width, height) = *self.0.lock().unwrap();
        Ok(format!(
            r#"{{"pixel_ratio":2,"screen_width":{width},"screen_height":{height},"window_width":{width},
               "window_height":{height},"status_bar_height":0,"screen_top":0,
               "safe_area":{{"left":0,"top":0,"right":0,"bottom":0}}}}"#
        ))
    }
}

struct Device(Window);

impl SensorServices for Device {}
impl MediaServices for Device {}
impl ConnectivityServices for Device {}
impl CommerceServices for Device {}
impl SystemUtilServices for Device {
    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        Some(Arc::new(self.0.clone()))
    }
}

const SURFACE_CHANGED: &str =
    "globalThis[Symbol.for('Migo.hostBridge')]._internalTriggerWindowResize();";

/// Adding a listener calls nothing: the size it was added at is not a change, and content reads it with
/// `getWindowInfo`. The engine used to call every new listener at once with the current size, which no game written
/// for the reference expects -- and a test waiting for the next resize took that call for it.
#[test]
fn a_listener_hears_changes_after_it_was_added_and_only_changes() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let window = Window(Arc::new(Mutex::new((400, 800))));
    let mut runtime = boot_with_services(
        Some(Arc::new(Device(window.clone())) as Arc<dyn DeviceServices>),
        None,
    );
    let heard = "JSON.stringify(globalThis.heard)";

    let added = run_to_idle(
        &mut runtime,
        "globalThis.heard = []; migo.onWindowResize((res) => globalThis.heard.push([res.windowWidth, res.windowHeight]));",
        heard,
    );
    assert_eq!(added, "[]", "adding a listener is not a resize");

    // The surface was reported again at the size the listener was added at: no change, nothing heard.
    assert_eq!(run_to_idle(&mut runtime, SURFACE_CHANGED, heard), "[]");

    *window.0.lock().unwrap() = (500, 700);
    assert_eq!(
        run_to_idle(&mut runtime, SURFACE_CHANGED, heard),
        "[[500,700]]"
    );

    // Reported again at the size it changed to: still the one change.
    assert_eq!(
        run_to_idle(&mut runtime, SURFACE_CHANGED, heard),
        "[[500,700]]"
    );
}
