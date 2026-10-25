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
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
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

/// The mouse, from a desktop host: a press that begins on a button is the button's from down to up, the pointer over
/// a button is the button's, and one click a host delivers as both a mouse press and a touch taps once -- the button
/// taps when the last press on it lifts on it.
#[test]
fn a_mouse_press_on_a_button_is_the_button_s_and_one_click_taps_once() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeHost::default());
    let mut runtime = boot(Some(host));
    let checked = run_to_idle(
        &mut runtime,
        r#"
        globalThis.__content = [];
        globalThis.__taps = 0;
        const bridge = globalThis[Symbol.for('Migo.hostBridge')];
        migo.onMouseDown(function (e) { __content.push('down:' + e.x); });
        migo.onMouseMove(function (e) { __content.push('move:' + e.x + ':' + e.movementX); });
        migo.onMouseUp(function (e) { __content.push('up:' + e.x); });
        migo.onTouchStart(function () { __content.push('touch'); });
        const button = migo.createFeedbackButton({
            type: 'text', text: 'feedback', style: { left: 10, top: 10, width: 100, height: 40 },
        });
        button.onTap(function () { __taps += 1; });
        const settle = function () { return new Promise(function (resolve) { setTimeout(resolve, 0); }); };
        (async function () {
            bridge._internalTriggerMouseMove(200, 200, 0, 0);   // content's
            bridge._internalTriggerMouseMove(20, 20, 0, 0);     // over the button: its
            bridge._internalTriggerMouseDown(20, 20, 0, 0);
            bridge._internalTriggerMouseUp(20, 20, 0, 0);       // a click: one tap
            await settle();
            // The same click as a mouse press and a touch, interleaved as a host
            // that delivers both does: still one tap.
            bridge._internalTriggerMouseDown(30, 20, 0, 0);
            __touch(0, 1, 30, 20, 1);
            __touch(2, 1, 30, 20, 3);
            await settle();
            bridge._internalTriggerMouseUp(30, 20, 0, 0);
            await settle();
            // Pressed on the button, released off it: no tap, and the release
            // is still the button's.
            bridge._internalTriggerMouseDown(20, 20, 0, 0);
            bridge._internalTriggerMouseMove(300, 300, 0, 0);
            bridge._internalTriggerMouseUp(300, 300, 0, 0);
            await settle();
            bridge._internalTriggerMouseMove(310, 300, 0, 0);   // content's again
            bridge._internalTriggerMouseDown(310, 300, 0, 0);
            bridge._internalTriggerMouseUp(310, 300, 0, 0);
            await settle();
        })();
        "#,
        "JSON.stringify([__content, __taps])",
    );
    assert_eq!(
        checked,
        r#"[["move:200:0","move:310:10","down:310","up:310"],2]"#
    );
}

/// A window of 400 x 800 CSS pixels at two device pixels each.
struct Window;

impl shared::services::SystemInfoService for Window {
    fn get_window_info_json(&self) -> Result<String, ServiceError> {
        Ok(r#"{"pixel_ratio":2,"screen_width":400,"screen_height":800,"window_width":400,"window_height":800,
               "status_bar_height":0,"screen_top":0,"safe_area":{"left":0,"top":0,"right":0,"bottom":0}}"#
            .to_string())
    }
}

struct Windowed;

impl SensorServices for Windowed {}
impl MediaServices for Windowed {}
impl ConnectivityServices for Windowed {}
impl CommerceServices for Windowed {}
impl SystemUtilServices for Windowed {
    fn system_info(&self) -> Option<Arc<dyn shared::services::SystemInfoService>> {
        Some(Arc::new(Window))
    }
}

/// The overlay records the engine sent, frame by frame: `(canvas, x, y, shown)`, and whether the canvas carrying them
/// was also drawn on in that frame.
fn overlays(
    frames: &crossbeam_channel::Receiver<shared::protocol::render_cmd::RenderCommand>,
) -> Vec<(u32, i32, i32, bool, bool)> {
    use shared::protocol::{
        frame_packet::FrameOp,
        render_cmd::{Canvas2DCmd, RenderCommand},
    };
    let mut found = Vec::new();
    // The renderer stub hands frames over from its own thread: wait for the
    // step's frame, then take whatever follows it closely.
    let mut wait = std::time::Duration::from_secs(5);
    while let Ok(command) = frames.recv_timeout(wait) {
        wait = std::time::Duration::from_millis(50);
        let RenderCommand::FramePacket(packet) = command else {
            continue;
        };
        packet.into_ops().consume(|ops| {
            for op in ops.iter() {
                let FrameOp::CanvasBatch(batch) = op else {
                    continue;
                };
                let drawn = batch.commands.iter().any(|command| {
                    matches!(command, Canvas2DCmd::FillText { .. } | Canvas2DCmd::Fill)
                });
                for command in batch.commands.iter() {
                    if let Canvas2DCmd::SetOverlay { x, y, shown } = command {
                        found.push((u32::from(batch.canvas_id), *x, *y, *shown, drawn));
                    }
                }
            }
        });
    }
    found
}

/// A shown button is drawn in the engine's next frame into a canvas of the engine's own, presented as the overlay at
/// the buttons' box in window pixels; moving it moves the box, and hiding the last one withdraws the overlay. Content
/// runs no loop of its own here: the change alone makes the frame.
#[test]
fn shown_buttons_are_presented_as_the_overlay_and_hidden_ones_withdrawn() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    use std::sync::atomic::{AtomicBool, Ordering};

    let (raf_tx, raf_rx) = shared::raf_signal::create_raf_pair();
    let (render_tx, render_rx) = shared::render_command_sender::CommandSender::new();
    // The render thread, as far as this needs one: it answers the canvas queries
    // the JavaScript half waits on and hands every frame to the assertions.
    let (frames_tx, frames) = crossbeam_channel::unbounded();
    let frame_came = Arc::new(tokio::sync::Notify::new());
    let renderer = {
        let frame_came = Arc::clone(&frame_came);
        std::thread::spawn(move || {
            use shared::protocol::render_cmd::{CanvasCmd, RenderCommand};
            while let Ok(command) = render_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                match command {
                    RenderCommand::Canvas(CanvasCmd::GetInfo { resp, .. }) => resp.ok((1, 1)),
                    RenderCommand::Canvas(CanvasCmd::DestroyCanvas { resp, .. }) => resp.ok(()),
                    frame @ RenderCommand::FramePacket(_) => {
                        let _ = frames_tx.send(frame);
                        frame_came.notify_one();
                    }
                    _ => {}
                }
            }
        })
    };
    let mut state = super::support::test_host_state();
    state.render_tx = render_tx;
    state.raf_rx = Some(raf_rx);
    state.device_services = Some(Arc::new(Windowed));
    // A display ticking every millisecond, as the render thread signals every
    // vsync while animating; each signal answers whoever is waiting.
    let stop = Arc::new(AtomicBool::new(false));
    let ticker = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                raf_tx.signal(shared::time_origin::elapsed_ms(), u64::MAX);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    // One reactor for the whole test: a timer the engine arms in one step must
    // still be able to fire in the next.
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _inside = reactor.enter();
    let mut runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
        extensions: crate::main_extensions(state),
        ..Default::default()
    });
    crate::harden_global_scope(&mut runtime);
    let mut step = |runtime: &mut deno_core::JsRuntime, source: &'static str| {
        runtime
            .execute_script("<test:step>", deno_core::FastString::from_static(source))
            .expect("test step");
        // Until the step's frame is presented, not until the event loop is idle: once a canvas
        // has a context the engine waits for the next content for as long as the session lives.
        reactor.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::select! {
                    result = runtime.run_event_loop(deno_core::PollEventLoopOptions::default()) => {
                        result.expect("event loop");
                    }
                    () = frame_came.notified() => {}
                }
            })
            .await
            .expect("the frame came");
        });
    };

    step(
        &mut runtime,
        r#"
        globalThis.__button = migo.createUserInfoButton({
            type: 'text', text: 'sign in',
            style: { left: 10.5, top: 20, width: 100, height: 40, backgroundColor: '#ff0000', borderRadius: 4 },
        });
        "#,
    );
    let shown = overlays(&frames);
    step(&mut runtime, "__button.style.left = 30;");
    let moved = overlays(&frames);
    step(&mut runtime, "__button.hide();");
    let hidden = overlays(&frames);
    stop.store(true, Ordering::Relaxed);
    ticker.join().unwrap();
    drop(runtime);
    renderer.join().unwrap();

    // floor(10.5 * 2), 20 * 2: the box's corner in window pixels, drawn in the same frame.
    assert_eq!(shown.len(), 1, "{shown:?}");
    let (canvas, x, y, presented, drawn) = shown[0];
    assert_ne!(canvas, 1, "never the content's canvas");
    assert_eq!((x, y, presented, drawn), (21, 40, true, true));
    assert_eq!(moved, [(canvas, 60, 40, true, true)]);
    assert_eq!(hidden, [(canvas, 0, 0, false, false)]);
}

#[test]
fn the_user_info_button_hands_on_tap_the_host_s_answer() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
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
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
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
