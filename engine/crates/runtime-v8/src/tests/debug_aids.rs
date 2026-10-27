//! `setEnableDebug`: the host's debugging aids, and the console they show.

use std::sync::{Arc, Mutex};

use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, ScreenService,
        SensorServices, SystemUtilServices,
    },
};

use super::support::{boot_with_services, run_to_idle};

#[derive(Default)]
struct Screen(Mutex<Vec<bool>>);

impl ScreenService for Screen {
    fn set_enable_debug(&self, enabled: bool) -> Result<(), ServiceError> {
        self.0.lock().unwrap().push(enabled);
        Ok(())
    }
}

struct Platform(Option<Arc<Screen>>);

impl SensorServices for Platform {
    fn screen(&self) -> Option<Arc<dyn ScreenService>> {
        self.0
            .clone()
            .map(|screen| screen as Arc<dyn ScreenService>)
    }
}
impl ConnectivityServices for Platform {}
impl CommerceServices for Platform {}
impl MediaServices for Platform {}
impl SystemUtilServices for Platform {}

/// The request reaches the host's screen service, content's success follows,
/// and the session's console records what content writes while the aids are
/// on -- turned on while the game runs -- and nothing before or after.
#[test]
fn the_aids_reach_the_host_and_the_console_records_while_they_are_on() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    // The support state's session id; nothing else in these tests records
    // under it.
    let host_id = 1;
    let console = shared::console_log::register_console_log(host_id, false, None);
    crate::console::bind_thread_console(host_id);
    let screen = Arc::new(Screen::default());
    let services: Arc<dyn DeviceServices> = Arc::new(Platform(Some(Arc::clone(&screen))));
    let mut runtime = boot_with_services(Some(services), None);
    let heard = run_to_idle(
        &mut runtime,
        r#"
        globalThis.heard = [];
        console.log("before");
        migo.setEnableDebug({ enableDebug: true, success(res) {
            heard.push(res.errMsg);
            console.warn("during");
            migo.setEnableDebug({ enableDebug: false, success(res) {
                heard.push(res.errMsg);
                console.log("after");
            } });
        } });
        "#,
        "JSON.stringify(heard)",
    );
    shared::console_log::unregister_console_log(host_id);
    crate::console::bind_thread_console(host_id);

    assert_eq!(heard, r#"["setEnableDebug:ok","setEnableDebug:ok"]"#);
    assert_eq!(*screen.0.lock().unwrap(), [true, false]);
    let (entries, _) = console.read_since(0);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.level, entry.message.as_str()))
            .collect::<Vec<_>>(),
        [(2, "during")]
    );
}

/// Where the platform has no aids, content's call fails in the platform's
/// words, and nothing is recorded.
#[test]
fn without_aids_the_call_fails_and_nothing_is_recorded() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let services: Arc<dyn DeviceServices> = Arc::new(Platform(None));
    let mut runtime = boot_with_services(Some(services), None);
    let heard = run_to_idle(
        &mut runtime,
        r#"
        globalThis.heard = [];
        migo.setEnableDebug({ enableDebug: true, fail(err) { heard.push(err.errMsg); } });
        "#,
        "JSON.stringify(heard)",
    );
    assert_eq!(heard, r#"["setEnableDebug:fail not supported"]"#);
}
