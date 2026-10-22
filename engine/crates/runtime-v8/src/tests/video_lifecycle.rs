//! A video player's life as content sees it: its promises settle when the host
//! answers, its events reach it by id -- listeners and the reference's assigned
//! handlers alike -- and a destroyed player refuses what follows without
//! reaching the host.

use std::sync::{Arc, Mutex};

use deno_core::{JsRuntime, serde_json};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, SensorServices,
        SystemUtilServices, VideoService,
    },
};

use super::support::{boot_with_services, run_to_idle as run};

/// What the host was asked, in order: `method request`.
#[derive(Default)]
struct FakeVideo(Mutex<Vec<String>>);

impl FakeVideo {
    fn record(&self, method: &str, request_json: &str) -> Result<(), ServiceError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("{method} {request_json}"));
        Ok(())
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|line| {
                let (method, json) = line.split_once(' ').unwrap();
                let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
                value["method"] = method.into();
                value
            })
            .collect()
    }
}

impl VideoService for FakeVideo {
    fn create(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("create", request_json)
    }
    fn destroy(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("destroy", request_json)
    }
    fn set(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("set", request_json)
    }
    fn play(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("play", request_json)
    }
    fn seek(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("seek", request_json)
    }
    fn request_full_screen(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("request_full_screen", request_json)
    }
}

struct Host(Arc<FakeVideo>);

impl SensorServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {}
impl MediaServices for Host {
    fn video(&self) -> Option<Arc<dyn VideoService>> {
        Some(self.0.clone())
    }
}

fn boot(video: Arc<FakeVideo>) -> JsRuntime {
    boot_with_services(Some(Arc::new(Host(video)) as Arc<dyn DeviceServices>), None)
}

/// The host answering call `request` through `hook` with `fields`.
fn answer(hook: &str, request: &serde_json::Value, fields: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('{hook}', \
         JSON.stringify([JSON.stringify(Object.assign({{ requestId: {} }}, {fields}))]));",
        request["requestId"]
    )
}

/// The host reporting `hook`'s event with `fields` for player `id`.
fn event(hook: &str, id: &serde_json::Value, fields: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('{hook}', \
         JSON.stringify([JSON.stringify(Object.assign({{ videoId: {id} }}, {fields}))]));"
    )
}

#[test]
fn a_players_promises_settle_when_done_and_its_events_reach_it_by_id() {
    let video = Arc::new(FakeVideo::default());
    let mut runtime = boot(video.clone());
    run(
        &mut runtime,
        "globalThis.__heard = []; \
         globalThis.__video = migo.createVideo({ src: '', x: 10, \
             objectFit: 'cover', playbackRate: 3, controls: false }); \
         globalThis.__other = migo.createVideo({}); \
         __video.onPlay(() => __heard.push('play')); \
         __video.onplay = () => __heard.push('onplay'); \
         __video.onTimeUpdate((res) => __heard.push('time ' + JSON.stringify(res))); \
         __other.onPlay(() => __heard.push('other play')); \
         __video.play().then((res) => __heard.push('played ' + res.errMsg)); \
         __video.seek(1.5).then((res) => __heard.push('sought ' + res.errMsg)); \
         __video.seek('soon').catch((res) => __heard.push(res.errMsg)); \
         __video.requestFullScreen(45).catch((res) => __heard.push(res.errMsg)); \
         __video.width = 'wide'; \
         __video.loop = true;",
        "0",
    );
    let requests = video.requests();
    let methods: Vec<&str> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        ["create", "create", "play", "seek", "set"],
        "{requests:?}"
    );
    let created = &requests[0];
    assert_eq!(created["x"], 10);
    assert_eq!(created["objectFit"], "cover");
    assert_eq!(
        created["playbackRate"], 1.0,
        "a rate the reference does not allow"
    );
    assert_eq!(created["controls"], false);
    assert_eq!(created["width"], 300);
    let id = &created["videoId"];
    assert_ne!(id, &requests[1]["videoId"]);
    assert_eq!(requests[3]["time"], 1.5);
    assert_eq!(requests[4]["loop"], true);

    // Settled when the host answers, not before.
    let heard = run(
        &mut runtime,
        &answer("_internalOnVideoSeekResult", &requests[3], "{}"),
        "JSON.stringify(__heard)",
    );
    assert_eq!(
        heard,
        r#"["video.seek:fail time must be a number of seconds","video.requestFullScreen:fail direction must be 0, 90 or -90","sought video.seek:ok"]"#
    );

    let script = [
        "__heard.length = 0;".to_owned(),
        event("_internalOnVideoPlayEvent", id, "{}"),
        event(
            "_internalOnVideoTimeUpdateEvent",
            id,
            "{ position: 0.25, duration: 1 }",
        ),
        answer("_internalOnVideoPlayResult", &requests[2], "{}"),
    ]
    .concat();
    let heard = run(&mut runtime, &script, "JSON.stringify(__heard)");
    assert_eq!(
        heard,
        r#"["play","onplay","time {\"position\":0.25,\"duration\":1}","played video.play:ok"]"#
    );

    // Destroyed: the host hears it once, nothing more reaches the player, and
    // its calls are refused before the host.
    let heard = run(
        &mut runtime,
        &format!(
            "__heard.length = 0; __video.destroy(); __video.destroy(); \
             __video.play().catch((res) => __heard.push(res.errMsg)); {}",
            event("_internalOnVideoPlayEvent", id, "{}")
        ),
        "JSON.stringify(__heard)",
    );
    assert_eq!(heard, r#"["video.play:fail video destroyed"]"#);
    let requests = video.requests();
    assert_eq!(requests.len(), 6, "{requests:?}");
    assert_eq!(requests[5]["method"], "destroy");
    assert_eq!(&requests[5]["videoId"], id);
}
