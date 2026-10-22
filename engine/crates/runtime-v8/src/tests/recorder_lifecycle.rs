//! A recording's life as content hears it: every request names the recording,
//! and the host's events and frames reach content only while they are of the
//! recording it holds -- not of one a later start replaced, not after it stopped.

use std::sync::{Arc, Mutex};

use deno_core::{JsRuntime, serde_json};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, PermissionService,
        RecorderService, Scope, ScopeState, SensorServices, SystemUtilServices,
    },
};

use super::support::{boot_with_services, run_to_idle as run};

/// What the host was asked, in order: `method request`.
#[derive(Default)]
struct FakeRecorder(Mutex<Vec<String>>);

impl FakeRecorder {
    fn record(&self, method: &str, request_json: &str) -> Result<(), ServiceError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("{method} {request_json}"));
        Ok(())
    }

    fn requests(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl RecorderService for FakeRecorder {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("start", request_json)
    }

    fn pause(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("pause", request_json)
    }

    fn stop(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("stop", request_json)
    }
}

struct Granted;

impl PermissionService for Granted {
    fn scope_state(&self, _scope: Scope) -> ScopeState {
        ScopeState::Granted
    }
}

struct Host(Arc<FakeRecorder>);

impl SensorServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        Some(Arc::new(Granted))
    }
}
impl MediaServices for Host {
    fn recorder(&self) -> Option<Arc<dyn RecorderService>> {
        Some(self.0.clone())
    }
}

fn boot(recorder: Arc<FakeRecorder>) -> JsRuntime {
    boot_with_services(
        Some(Arc::new(Host(recorder)) as Arc<dyn DeviceServices>),
        None,
    )
}

/// The recording id a start request named.
fn started(request: &str) -> u64 {
    let json: serde_json::Value =
        serde_json::from_str(request.strip_prefix("start ").expect("a start")).unwrap();
    json["recordingId"].as_u64().expect("a recording id")
}

/// The host reporting `hook`'s event with `fields` for recording `id`.
fn event(hook: &str, id: u64, fields: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('{hook}', \
         JSON.stringify([JSON.stringify(Object.assign({{ recordingId: {id} }}, {fields}))]));"
    )
}

/// The host handing over a frame of recording `id`.
fn frame(id: u64) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalOnRecorderFrameData({id}, \
         new ArrayBuffer(4), false);"
    )
}

#[test]
fn content_hears_the_recording_it_holds_and_no_other() {
    let recorder = Arc::new(FakeRecorder::default());
    let mut runtime = boot(recorder.clone());
    run(
        &mut runtime,
        "globalThis.__heard = []; \
         globalThis.__recorder = migo.getRecorderManager(); \
         __recorder.onStart(() => __heard.push('start')); \
         __recorder.onPause(() => __heard.push('pause')); \
         __recorder.onStop((res) => __heard.push('stop ' + JSON.stringify(res))); \
         __recorder.onError((res) => __heard.push('error ' + res.errMsg)); \
         __recorder.onFrameRecorded((res) => __heard.push('frame ' + res.frameBuffer.byteLength)); \
         __recorder.start({ format: 'PCM', frameSize: 1, audioMode: 'voiceChat' }); \
         __recorder.start({});",
        "0",
    );
    let requests = recorder.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let first: serde_json::Value =
        serde_json::from_str(requests[0].strip_prefix("start ").unwrap()).unwrap();
    assert_eq!(first["frameSize"], 1);
    assert_eq!(first["audioMode"], "voiceChat");
    let (replaced, current) = (started(&requests[0]), started(&requests[1]));
    assert_ne!(replaced, current);

    // The replaced recording's reports are not this content's; the current one's are.
    let script = [
        event("_internalOnRecorderStartEvent", replaced, "{}"),
        frame(replaced),
        event(
            "_internalOnRecorderErrorEvent",
            replaced,
            "{ errMsg: 'stale' }",
        ),
        event("_internalOnRecorderStartEvent", current, "{}"),
        frame(current),
        event("_internalOnRecorderPauseEvent", current, "{}"),
        "__recorder.pause();".to_owned(),
        event(
            "_internalOnRecorderStopEvent",
            current,
            "{ tempFilePath: '/tmp/host-1.aac', duration: 1200, fileSize: 9600 }",
        ),
        // Stopped: nothing more of it, and no request for a recording content no longer holds.
        frame(current),
        event("_internalOnRecorderStartEvent", current, "{}"),
        "__recorder.stop(); __recorder.pause();".to_owned(),
    ]
    .concat();
    let heard = run(&mut runtime, &script, "JSON.stringify(__heard)");
    assert_eq!(
        heard,
        r#"["start","frame 4","pause","stop {\"tempFilePath\":\"/tmp/host-1.aac\",\"duration\":1200,\"fileSize\":9600}"]"#
    );
    let requests = recorder.requests();
    assert_eq!(
        requests[2..],
        [format!(r#"pause {{"recordingId":{current}}}"#)],
        "{requests:?}"
    );
}
