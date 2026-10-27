//! Sharing the game's recording: `operateGameRecorderVideo` and the recorder's
//! share button refuse in the reference's codes, and what reaches the host is
//! the engine's own video, the music through the sandbox and the share options
//! content set. The button's tap listeners hear a share that failed, never one
//! the player cancelled.

use std::sync::{Arc, Mutex};

use deno_core::serde_json::{self, Value};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, SensorServices,
        ShareService, SystemUtilServices,
    },
};

use super::native_buttons::TOUCH;
use super::support::{SETTLE, Sandbox, boot, run_to_idle, test_host_state};

#[derive(Default)]
struct FakeShare(Mutex<Vec<Value>>);

impl FakeShare {
    fn requests(&self) -> Vec<Value> {
        self.0.lock().unwrap().clone()
    }
}

impl ShareService for FakeShare {
    fn share_game_recording(&self, request_json: &str) -> Result<(), ServiceError> {
        let request = serde_json::from_str(request_json).expect("a share request is JSON");
        self.0.lock().unwrap().push(request);
        Ok(())
    }
}

struct Host(Arc<FakeShare>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl ConnectivityServices for Host {}
impl SystemUtilServices for Host {}
impl CommerceServices for Host {
    fn share(&self) -> Option<Arc<dyn ShareService>> {
        Some(self.0.clone())
    }
}

/// A runtime over the share host and a sandbox holding `/user/bgm.mp3`.
fn boot_sharing(sandbox: &Sandbox, host: &Arc<FakeShare>) -> deno_core::JsRuntime {
    std::fs::write(sandbox.paths.user_data_dir().join("bgm.mp3"), b"mp3").unwrap();
    let mut state = test_host_state();
    state.device_services = Some(Arc::new(Host(host.clone())) as Arc<dyn DeviceServices>);
    state.vfs = Some(sandbox.vfs());
    state.game_paths = Some(Arc::new(sandbox.paths.clone()));
    let mut runtime = boot(state);
    for (name, source) in [("<test:settle>", SETTLE), ("<test:touch>", TOUCH)] {
        runtime
            .execute_script(name, deno_core::FastString::from_static(source))
            .unwrap();
    }
    runtime
}

/// The host's answer to `request`: `result`'s fields, as a JS object literal.
fn answer(request: &Value, result: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch(\
         '_internalOnShareGameRecordingResult', \
         JSON.stringify([JSON.stringify(Object.assign({{ requestId: {} }}, {result}))]));",
        request["requestId"]
    )
}

/// A recording, as one the recorder finished.
fn record(sandbox: &Sandbox) -> std::path::PathBuf {
    let video = sandbox.paths.recording_path();
    std::fs::create_dir_all(video.parent().unwrap()).unwrap();
    std::fs::write(&video, b"mp4").unwrap();
    video
}

#[test]
fn a_shared_recording_is_the_engine_s_file_with_the_options_content_set() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let sandbox = Sandbox::new("recorder-share");
    let host = Arc::new(FakeShare::default());
    let mut runtime = boot_sharing(&sandbox, &host);

    // Refused before the host is asked, each in its code.
    let refused = run_to_idle(
        &mut runtime,
        "__settle('operateGameRecorderVideo', {}); \
         const refusals = []; \
         const refuse = (options) => new Promise((resolve) => migo.operateGameRecorderVideo( \
           Object.assign({ fail: (res) => resolve(refusals.push(res.errCode + ' ' + res.errMsg)) }, options))); \
         (async () => { \
           await refuse({ timeRange: [[3000, 1000]] }); \
           await refuse({ timeRange: [[0, 30000], [40000, 71000]] }); \
           await refuse({ timeRange: [[0, 1500]] }); \
         })();",
        "JSON.stringify([__results.operateGameRecorderVideo, refusals])",
    );
    assert_eq!(
        refused,
        r#"[{"ok":false,"res":{"errMsg":"operateGameRecorderVideo:fail no recorded video","errCode":22603}},["22623 operateGameRecorderVideo:fail time range invalid","22633 operateGameRecorderVideo:fail duration out of limit","22643 operateGameRecorderVideo:fail time range too short.It should be longer than 2s"]]"#
    );

    let video = record(&sandbox);
    let refused = run_to_idle(
        &mut runtime,
        "__settle('operateGameRecorderVideo', { bgm: '/user/missing.mp3' });",
        "JSON.stringify(__results.operateGameRecorderVideo)",
    );
    assert_eq!(
        refused,
        r#"{"ok":false,"res":{"errMsg":"operateGameRecorderVideo:fail bgm not found","errCode":22613}}"#
    );
    assert!(
        host.requests().is_empty(),
        "nothing refused reached the host"
    );

    let pending = run_to_idle(
        &mut runtime,
        "delete __results.operateGameRecorderVideo; \
         __settle('operateGameRecorderVideo', { title: 'best run', query: 'from=replay', \
           bgm: '/user/bgm.mp3', timeRange: [[1000, 3000], [4000, 5000]], volume: 0.5, atempo: 4, \
           audioMix: true });",
        "JSON.stringify(__results)",
    );
    assert_eq!(pending, "{}", "the share waits on the player");
    let requests = host.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        std::fs::canonicalize(request["video"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&video).unwrap(),
        "the video is the recorder's own: {request}"
    );
    assert_eq!(
        std::fs::canonicalize(request["bgm"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(sandbox.paths.user_data_dir().join("bgm.mp3")).unwrap(),
        "the music is content's file, through the sandbox: {request}"
    );
    assert_eq!(request["title"], "best run");
    assert_eq!(request["query"], "from=replay");
    assert_eq!(
        request["timeRange"],
        serde_json::json!([[1000, 3000], [4000, 5000]])
    );
    assert_eq!(request["volume"], 0.5);
    assert_eq!(
        request["atempo"], 1,
        "a speed the reference does not offer is the original's"
    );
    assert_eq!(request["audioMix"], true);

    let shared = run_to_idle(
        &mut runtime,
        &answer(
            request,
            "{ shareToGameCenter: { errCode: 0, errMsg: 'shared' } }",
        ),
        "JSON.stringify(__results.operateGameRecorderVideo)",
    );
    assert_eq!(
        shared,
        r#"{"ok":true,"res":{"errMsg":"operateGameRecorderVideo:ok","shareToGameCenter":{"errCode":0,"errMsg":"shared"}}}"#
    );
}

#[test]
fn the_share_button_shares_on_a_tap_and_its_listeners_hear_a_failed_share() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let sandbox = Sandbox::new("recorder-share-button");
    let host = Arc::new(FakeShare::default());
    let mut runtime = boot_sharing(&sandbox, &host);

    // Laid out by the reference's minimums: 16 + the 24-wide icon + 16 across,
    // at least 40 high.
    let laid_out = run_to_idle(
        &mut runtime,
        "globalThis.__taps = []; globalThis.__errors = []; \
         migo.getGameRecorder().on('error', (res) => __errors.push(res.error.code)); \
         globalThis.__button = migo.createGameRecorderShareButton({ \
           style: { left: 10, top: 20, height: 30, paddingLeft: 4 }, share: { timeRange: [[0, 3000]] } }); \
         __button.onTap((res) => __taps.push(res.errCode === undefined ? res.errMsg : res.errCode));",
        "JSON.stringify([__button._paintBounds(), __button === migo.createGameRecorderShareButton({ \
           style: {}, share: { timeRange: [[0, 3000]] } })])",
    );
    assert_eq!(
        laid_out,
        r#"[{"left":10,"top":20,"width":56,"height":40},true]"#
    );

    // Without music the button's share is refused, as the recorder's error too.
    let refused = run_to_idle(
        &mut runtime,
        "__tap(1, 30, 40);",
        "JSON.stringify([__taps, __errors])",
    );
    assert_eq!(refused, "[[22613],[22613]]");

    record(&sandbox);
    run_to_idle(
        &mut runtime,
        "__button.style.left = 0; \
         migo.createGameRecorderShareButton({ style: { left: 10, top: 20 }, \
           share: { bgm: '/user/bgm.mp3', timeRange: [[0, 3000]] } }); \
         __tap(2, 30, 40);",
        "0",
    );
    let requests = host.requests();
    assert_eq!(requests.len(), 1, "a tap shares: {requests:?}");
    assert_eq!(requests[0]["timeRange"], serde_json::json!([[0, 3000]]));

    let cancelled = run_to_idle(
        &mut runtime,
        &answer(
            &requests[0],
            "{ error: 'operateGameRecorderVideo:fail cancel', errCode: -1 }",
        ),
        "JSON.stringify([__taps, __errors])",
    );
    assert_eq!(
        cancelled, "[[22613],[22613]]",
        "a cancelled share is no one's failure"
    );

    run_to_idle(&mut runtime, "__tap(3, 30, 40);", "0");
    let requests = host.requests();
    assert_eq!(requests.len(), 2);
    let failed = run_to_idle(
        &mut runtime,
        &answer(
            &requests[1],
            "{ error: 'operateGameRecorderVideo:fail no share target', errCode: 7 }",
        ),
        "JSON.stringify([__taps, __errors])",
    );
    assert_eq!(failed, "[[22613,7],[22613]]");

    // Hidden, it is not there to tap.
    run_to_idle(&mut runtime, "__button.hide(); __tap(4, 30, 40);", "0");
    assert_eq!(host.requests().len(), 2);
}
