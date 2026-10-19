//! What a video player is handed for the `src` and `poster` content names.

use std::sync::{Arc, Mutex};

use deno_core::serde_json::{self, Value};
use shared::{
    network_policy::NetworkPolicy,
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, MediaServices, SensorServices,
        SystemUtilServices, VideoService,
    },
};

use super::support::{Sandbox, boot, run_to_idle, test_host_state};

#[derive(Default)]
struct FakeVideo {
    calls: Mutex<Vec<(&'static str, Value)>>,
}

impl FakeVideo {
    fn record(&self, call: &'static str, json: &str) {
        let json = serde_json::from_str(json).expect("a video request is JSON");
        self.calls.lock().unwrap().push((call, json));
    }

    fn calls(&self) -> Vec<(&'static str, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

impl VideoService for FakeVideo {
    fn create(&self, options_json: &str) -> Result<String, ServiceError> {
        self.record("create", options_json);
        Ok("{}".to_string())
    }
    fn play(&self, _: u32) -> Result<(), ServiceError> {
        Ok(())
    }
    fn pause(&self, _: u32) -> Result<(), ServiceError> {
        Ok(())
    }
    fn stop(&self, _: u32) -> Result<(), ServiceError> {
        Ok(())
    }
    fn seek(&self, _: u32, _: f64) -> Result<(), ServiceError> {
        Ok(())
    }
    fn request_fullscreen(&self, _: u32, _: i32) -> Result<(), ServiceError> {
        Ok(())
    }
    fn exit_fullscreen(&self, _: u32) -> Result<(), ServiceError> {
        Ok(())
    }
    fn set_property(&self, _: u32, property_json: &str) -> Result<(), ServiceError> {
        self.record("set_property", property_json);
        Ok(())
    }
    fn destroy(&self, _: u32) -> Result<(), ServiceError> {
        Ok(())
    }
}

struct Host(Arc<FakeVideo>);

impl SensorServices for Host {}
impl ConnectivityServices for Host {}
impl SystemUtilServices for Host {}
impl CommerceServices for Host {}
impl MediaServices for Host {
    fn video(&self) -> Option<Arc<dyn VideoService>> {
        Some(self.0.clone())
    }
}

fn canonical(path: &Value) -> std::path::PathBuf {
    std::fs::canonicalize(path.as_str().expect("a path")).expect("the path names a file")
}

/// A player opened content's string verbatim: any URL under a policy that
/// granted nothing, any file the host process could read. It is handed the real
/// file or the URL the download allowlist let through, and nothing else.
#[test]
fn a_video_player_is_handed_the_sandbox_file_or_the_checked_url_and_nothing_else() {
    let sandbox = Sandbox::new("video");
    let clip = sandbox.paths.user_data_dir().join("clip.mp4");
    std::fs::write(&clip, b"mp4").unwrap();
    let cover = sandbox.paths.user_data_dir().join("cover.png");
    std::fs::write(&cover, b"png").unwrap();
    let video = Arc::new(FakeVideo::default());
    let mut state = test_host_state();
    state.device_services = Some(Arc::new(Host(video.clone())) as Arc<dyn DeviceServices>);
    state.vfs = Some(sandbox.vfs());
    state.network_policy = NetworkPolicy::from_json(r#"{"download":["example.com"]}"#).unwrap();
    let mut runtime = boot(state);

    run_to_idle(
        &mut runtime,
        r#"
        var granted = migo.createVideo({ src: 'https://Example.COM/v.mp4', poster: '/user/cover.png' });
        migo.createVideo({ src: 'https://elsewhere.example/v.mp4' });
        migo.createVideo({ src: '/etc/hosts' });
        migo.createVideo({ src: 'https://example.com/v.mp4', poster: '/user/../../outside.png' });
        migo.createVideo({});
        granted.src = '/user/clip.mp4';
        granted.src = 'http://example.com/v.mp4';
        granted.src = '/etc/hosts';
        "#,
        "0",
    );

    let calls = video.calls();
    assert_eq!(
        calls.iter().map(|(call, _)| *call).collect::<Vec<_>>(),
        ["create", "create", "set_property"],
        "only what the sandbox and the policy allow reaches the player: {calls:?}"
    );
    let first = &calls[0].1;
    assert_eq!(
        first["src"], "https://example.com/v.mp4",
        "the URL the gate read"
    );
    assert_eq!(
        canonical(&first["poster"]),
        std::fs::canonicalize(&cover).unwrap()
    );
    assert_eq!(
        calls[1].1["src"], "",
        "a video created without a source names none"
    );
    assert_eq!(
        canonical(&calls[2].1["src"]),
        std::fs::canonicalize(&clip).unwrap()
    );
}
