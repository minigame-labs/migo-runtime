//! The native buttons: what a touch on one does, and that content never sees it.

use std::sync::{Arc, Mutex};

use deno_core::serde_json::{self, Value};
use shared::{
    protocol::error::ServiceError,
    services::{
        AuthService, CommerceServices, ConnectivityServices, DeviceServices, EcosystemService,
        MediaServices, PermissionService, Scope, ScopeState, SensorServices, SystemUtilServices,
    },
};

use super::support::{boot_with_services, run_to_idle};

#[derive(Default)]
struct FakeHost {
    requests: Mutex<Vec<(&'static str, Value)>>,
}

impl FakeHost {
    fn record(&self, what: &'static str, json: &str) -> Result<(), ServiceError> {
        self.requests
            .lock()
            .unwrap()
            .push((what, serde_json::from_str(json).unwrap()));
        Ok(())
    }

    fn requests(&self) -> Vec<(&'static str, Value)> {
        self.requests.lock().unwrap().clone()
    }
}

impl AuthService for FakeHost {
    fn get_user_info(&self, options_json: &str) -> Result<(), ServiceError> {
        self.record("getUserInfo", options_json)
    }
}

// The player already said yes to sharing their profile.
impl PermissionService for FakeHost {
    fn scope_state(&self, scope: Scope) -> ScopeState {
        if scope == Scope::UserInfo {
            ScopeState::Granted
        } else {
            ScopeState::Unknown
        }
    }
}

impl EcosystemService for FakeHost {
    fn call(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("ecosystem", request_json)
    }
}

struct Host(Arc<FakeHost>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {
    fn auth(&self) -> Option<Arc<dyn AuthService>> {
        Some(self.0.clone())
    }
}
impl SystemUtilServices for Host {
    fn ecosystem(&self) -> Option<Arc<dyn EcosystemService>> {
        Some(self.0.clone())
    }
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        Some(self.0.clone())
    }
}

/// `__touch(type, id, x, y, flags)` posts one raw touch the way a host does:
/// type 0 start, 1 move, 2 end; flags 1 changed, 2 lifted.
const TOUCH: &str = r#"
    globalThis.__touch = function (type, id, x, y, flags) {
        const buffer = new ArrayBuffer(20);
        const view = new DataView(buffer);
        view.setUint32(0, id, true);
        view.setFloat32(4, x, true);
        view.setFloat32(8, y, true);
        view.setFloat32(12, 1, true);
        view.setUint32(16, flags, true);
        globalThis[Symbol.for('Migo.hostBridge')]._internalEnqueueRawTouchEvent(type, buffer, 1, 0);
    };
    // A whole tap, dispatched -- touches reach content a microtask after they arrive.
    globalThis.__tap = function (id, x, y, liftX, liftY) {
        __touch(0, id, x, y, 1);
        __touch(2, id, liftX === undefined ? x : liftX, liftY === undefined ? y : liftY, 3);
        return new Promise(function (resolve) { setTimeout(resolve, 0); });
    };
"#;

fn boot(host: Option<Arc<FakeHost>>) -> deno_core::JsRuntime {
    let mut runtime = boot_with_services(
        host.map(|host| Arc::new(Host(host)) as Arc<dyn DeviceServices>),
        None,
    );
    runtime
        .execute_script("<test:touch>", deno_core::FastString::from_static(TOUCH))
        .unwrap();
    runtime
}

#[test]
fn a_touch_on_a_button_is_the_button_s_and_a_tap_does_what_the_button_does() {
    let host = Arc::new(FakeHost::default());
    let mut runtime = boot(Some(host.clone()));
    let checked = run_to_idle(
        &mut runtime,
        r#"
        globalThis.__content = [];
        globalThis.__taps = 0;
        const ids = (event) => event.changedTouches.map(function (t) { return t.identifier; }).join();
        migo.onTouchStart(function (e) { __content.push('start:' + ids(e)); });
        migo.onTouchEnd(function (e) { __content.push('end:' + ids(e)); });
        const button = migo.createFeedbackButton({
            type: 'text', text: 'feedback', style: { left: 10, top: 10, width: 100, height: 40 },
        });
        button.onTap(function () { __taps += 1; });
        (async function () {
            await __tap(1, 20, 20);             // on the button: a tap, not content's
            await __tap(2, 200, 200);           // elsewhere: content's
            await __tap(3, 20, 20, 300, 300);   // began on it, lifted off it: neither
            button.style.left = 500;
            await __tap(4, 20, 20);             // the button moved away: content's
            button.style.left = 10;
            button.hide();
            await __tap(5, 20, 20);             // hidden: content's
            button.show();
            button.destroy();
            await __tap(6, 20, 20);             // destroyed: content's
        })();
        "#,
        "JSON.stringify([__content, __taps])",
    );
    assert_eq!(
        checked,
        r#"[["start:2","end:2","start:4","end:4","start:5","end:5","start:6","end:6"],1]"#
    );
    let opened: Vec<Value> = host
        .requests()
        .into_iter()
        .filter(|(what, _)| *what == "ecosystem")
        .map(|(_, request)| request["api"].clone())
        .collect();
    assert_eq!(
        opened,
        ["FeedbackButton.open"],
        "one tap, one feedback page"
    );
}

#[test]
fn the_user_info_button_hands_on_tap_the_host_s_answer() {
    let host = Arc::new(FakeHost::default());
    let mut runtime = boot(Some(host.clone()));
    run_to_idle(
        &mut runtime,
        r#"
        globalThis.__taps = [];
        const button = migo.createUserInfoButton({
            type: 'text', text: 'sign in', style: { left: 0, top: 0, width: 50, height: 50 },
        });
        button.onTap(function (res) { __taps.push(res); });
        __tap(1, 10, 10);
        "#,
        "0",
    );
    let request = host
        .requests()
        .into_iter()
        .find(|(what, _)| *what == "getUserInfo")
        .map(|(_, request)| request)
        .expect("the tap asked the host for the player's info");
    assert_eq!(request["withCredentials"], true);
    let answer = format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('_internalOnGetUserInfoResult', \
         JSON.stringify([JSON.stringify({{ requestId: {}, userInfo: {{ nickName: 'player' }}, rawData: 'r' }})]));",
        request["requestId"]
    );
    let checked = run_to_idle(&mut runtime, &answer, "JSON.stringify(__taps)");
    let taps: Value = serde_json::from_str(&checked).unwrap();
    assert_eq!(taps[0]["errMsg"], "getUserInfo:ok");
    assert_eq!(taps[0]["userInfo"]["nickName"], "player");
}

#[test]
fn without_a_host_a_tap_says_so_and_no_menu_button_is_made_up() {
    let mut runtime = boot(None);
    let checked = run_to_idle(
        &mut runtime,
        r#"
        globalThis.__taps = [];
        migo.createUserInfoButton({ type: 'text', style: { left: 0, top: 0, width: 50, height: 50 } })
            .onTap(function (res) { __taps.push(res.errMsg); });
        __tap(1, 10, 10);
        "#,
        "JSON.stringify([__taps, migo.getMenuButtonBoundingClientRect()])",
    );
    assert_eq!(
        checked,
        r#"[["getUserInfo:fail no permission handler"],{"width":0,"height":0,"top":0,"bottom":0,"left":0,"right":0}]"#
    );
}
