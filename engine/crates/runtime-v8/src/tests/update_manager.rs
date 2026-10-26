//! `migo.getUpdateManager()`: the host's updates, never invented ones.
//!
//! The manager once decided by coin flip whether an update was waiting --
//! `Math.random() < 0.3` at construction, a second flip a random 2-5 s later
//! firing `onUpdateReady` (90%) or `onUpdateFailed` (10%) -- so roughly a
//! quarter of launches offered the game's own "new version -- restart?" prompt
//! over an update that did not exist, and `applyUpdate()` restarted nothing. The
//! failure moved from launch to launch, which is how it survived testing.
//!
//! Now the host reports each step (`_internalOnUpdateEvent`) and content's
//! `applyUpdate` reaches the host (`UpdateService`); a host that offers no
//! updates has none, and says so at once. The sweep below calls every method
//! the manager publishes, discovered rather than listed, so a future one that
//! announces an update without the host fails here.
//! `scripts/test-runtime-answers-are-not-invented.sh` is the source-level
//! companion: no unjustified `Math.random` in the embedded JS.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use deno_core::JsRuntime;
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, SensorServices,
        SystemUtilServices, UpdateService,
    },
};

use super::support::{boot_with_services, run_to_idle as run};

#[derive(Default)]
struct FakeUpdates {
    applied: AtomicUsize,
}

impl UpdateService for FakeUpdates {
    fn apply_update(&self) -> Result<(), ServiceError> {
        self.applied.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Host(Arc<FakeUpdates>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {
    fn update(&self) -> Option<Arc<dyn UpdateService>> {
        Some(self.0.clone())
    }
}

fn boot(host: Option<Arc<FakeUpdates>>) -> JsRuntime {
    boot_with_services(
        host.map(|host| Arc::new(Host(host)) as Arc<dyn DeviceServices>),
        None,
    )
}

/// Script that posts the host's update event `json`.
fn report(json: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('_internalOnUpdateEvent', \
         JSON.stringify([JSON.stringify({json})]));"
    )
}

const ARM: &str = r#"
    globalThis.__heard = [];
    globalThis.__mgr = migo.getUpdateManager();
    __mgr.onCheckForUpdate(function (res) { __heard.push(['check', res.hasUpdate]); });
    __mgr.onUpdateReady(function () { __heard.push(['ready']); });
    __mgr.onUpdateFailed(function () { __heard.push(['failed']); });
"#;

#[test]
fn without_a_host_there_is_no_update_and_nothing_can_announce_one() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let mut runtime = boot(None);
    let heard = run(
        &mut runtime,
        &(String::from(ARM)
            + r#"
            // Every own method, discovered rather than listed; listener
            // registrars get a listener, the rest are called bare.
            globalThis.__called = [];
            const proto = Object.getPrototypeOf(__mgr);
            for (const key of Object.getOwnPropertyNames(proto)) {
                if (key === 'constructor' || typeof proto[key] !== 'function') continue;
                const listener = function () { __heard.push(['late ' + key]); };
                try { proto[key].call(__mgr, /^on/.test(key) ? listener : undefined); } catch (_) {}
                __called.push(key);
            }
            // The answer reaches a listener registered after it, too.
            migo.getUpdateManager().onCheckForUpdate(function (res) {
                __heard.push(['late check', res.hasUpdate]);
            });
            "#),
        "JSON.stringify([__heard, __called.sort(), migo.getUpdateManager() === __mgr])",
    );
    assert_eq!(
        heard,
        r#"[[["check",false],["late onCheckForUpdate"],["late check",false]],["applyUpdate","onCheckForUpdate","onUpdateFailed","onUpdateReady"],true]"#
    );
}

#[test]
fn the_hosts_steps_reach_listeners_before_and_after_them_and_apply_reaches_the_host() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeUpdates::default());
    let mut runtime = boot(Some(host.clone()));

    // A host that checks: nothing is said until it reports.
    let heard = run(&mut runtime, ARM, "JSON.stringify(__heard)");
    assert_eq!(heard, "[]");

    // The check finds an update; asking to apply it before it is ready asks nothing.
    let heard = run(
        &mut runtime,
        &(report("{ event: 'checkForUpdate', hasUpdate: true }") + "__mgr.applyUpdate();"),
        "JSON.stringify(__heard)",
    );
    assert_eq!(heard, r#"[["check",true]]"#);
    assert_eq!(host.applied.load(Ordering::SeqCst), 0);

    // Ready: heard by the listener there, and by one registered after it.
    let heard = run(
        &mut runtime,
        &(report("{ event: 'updateReady' }")
            + "__mgr.onUpdateReady(function () { __heard.push(['late ready']); });"
            + "__mgr.applyUpdate();"),
        "JSON.stringify(__heard)",
    );
    assert_eq!(heard, r#"[["check",true],["ready"],["late ready"]]"#);
    assert_eq!(host.applied.load(Ordering::SeqCst), 1);

    // A warm start checks again, and the story starts over: the old ready is
    // not this check's, and what the host does not define changes nothing.
    let heard = run(
        &mut runtime,
        &(String::from("__heard.length = 0;")
            + &report("{ event: 'checkForUpdate', hasUpdate: false }")
            + &report("{ event: 'checkForUpdate', hasUpdate: 'yes' }")
            + &report("{ event: 'restarted' }")
            + "__mgr.onUpdateReady(function () { __heard.push(['stale ready']); });"
            + "__mgr.applyUpdate();"),
        "JSON.stringify(__heard)",
    );
    assert_eq!(heard, r#"[["check",false]]"#);
    assert_eq!(host.applied.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failed_download_is_heard_and_applies_nothing() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeUpdates::default());
    let mut runtime = boot(Some(host.clone()));
    let heard = run(
        &mut runtime,
        &(String::from(ARM)
            + &report("{ event: 'checkForUpdate', hasUpdate: true }")
            + &report("{ event: 'updateFailed' }")
            + "__mgr.onUpdateFailed(function () { __heard.push(['late failed']); });"
            + "__mgr.applyUpdate();"),
        "JSON.stringify(__heard)",
    );
    assert_eq!(heard, r#"[["check",true],["failed"],["late failed"]]"#);
    assert_eq!(host.applied.load(Ordering::SeqCst), 0);
}
