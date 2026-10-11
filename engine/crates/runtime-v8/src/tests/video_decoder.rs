//! createVideoDecoder end to end: content's calls, the engine's decode thread
//! over a platform source, and the events and frames that come back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use deno_core::{FastString, JsRuntime, RuntimeOptions, serde_json};
use shared::{
    host_channel::HostCommandReceiver,
    protocol::{error::ServiceError, host_cmd::HostCommand},
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, FrameTiming, MediaServices,
        SensorServices, SystemUtilServices, VideoDecoderService, VideoFrameSource,
    },
};

use super::support::{Sandbox, run_to_idle as run, test_host_state};

/// `count` frames of 2x1 pixels, each frame's pixels its index, every 40 ms.
struct Counting {
    next: u32,
    count: u32,
}

impl VideoFrameSource for Counting {
    fn size(&self) -> (u32, u32) {
        (2, 1)
    }
    fn next_frame(&mut self, rgba: &mut [u8]) -> Result<Option<FrameTiming>, ServiceError> {
        if self.next >= self.count {
            return Ok(None);
        }
        rgba.fill(self.next as u8);
        let pts = f64::from(self.next) * 40.0;
        self.next += 1;
        Ok(Some(FrameTiming {
            pts_ms: pts,
            dts_ms: pts,
        }))
    }
    fn seek(&mut self, position_ms: f64) -> Result<(), ServiceError> {
        self.next = (position_ms / 40.0) as u32;
        Ok(())
    }
}

struct Decoders;

impl VideoDecoderService for Decoders {
    fn open(&self, source: &str) -> Result<Box<dyn VideoFrameSource>, ServiceError> {
        if !source.ends_with("video.mp4") {
            return Err(ServiceError::not_supported("unrecognised container"));
        }
        Ok(Box::new(Counting { next: 0, count: 4 }))
    }
}

struct Host;

impl SensorServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {}
impl MediaServices for Host {
    fn video_decoder(&self) -> Option<Arc<dyn VideoDecoderService>> {
        Some(Arc::new(Decoders))
    }
}

fn boot(sandbox: &Sandbox, decodes: bool) -> (JsRuntime, HostCommandReceiver) {
    let mut state = test_host_state();
    let (host_tx, _critical, host_rx) = shared::host_channel::channel(64);
    state.host_tx = host_tx;
    state.vfs = Some(sandbox.vfs());
    state.device_services = decodes.then(|| Arc::new(Host) as Arc<dyn DeviceServices>);
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: crate::main_extensions(state),
        ..Default::default()
    });
    crate::harden_global_scope(&mut runtime);
    (runtime, host_rx)
}

/// Run `source`, then keep delivering the decode thread's events until `until`
/// holds; panics after two seconds.
fn pump(runtime: &mut JsRuntime, events: &mut HostCommandReceiver, source: &str, until: &str) {
    run(runtime, source, "0");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        while let Ok(command) = events.try_recv() {
            if let HostCommand::InvokeHostHook { hook, args_json } = command {
                let script = format!(
                    "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('{hook}', {});",
                    serde_json::to_string(&args_json).unwrap()
                );
                run(runtime, &script, "0");
            }
        }
        if run(runtime, "0", until) == "true" {
            return;
        }
        assert!(Instant::now() < deadline, "never: {until}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn frames_arrive_in_order_with_their_times_and_the_events_tell_the_story() {
    let sandbox = Sandbox::new("video-decoder");
    std::fs::write(sandbox.paths.user_data_dir().join("video.mp4"), b"mp4").unwrap();
    let (mut runtime, mut events) = boot(&sandbox, true);
    pump(
        &mut runtime,
        &mut events,
        r#"
        globalThis.__events = [];
        globalThis.__frames = [];
        globalThis.__d = migo.createVideoDecoder();
        for (const name of ['start', 'stop', 'seek', 'ended']) {
            __d.on(name, function (res) { __events.push([name, res]); });
        }
        __d.start({ source: '/user/video.mp4' }).then(function (res) { globalThis.__started = res; });
        "#,
        "globalThis.__started !== undefined",
    );
    // Take frames as they come, until the stream ends.
    pump(
        &mut runtime,
        &mut events,
        "0",
        r#"(function () {
            for (let f = __d.getFrameData(); f !== null; f = __d.getFrameData()) {
                __frames.push([f.width, f.height, f.pkPts, Array.from(new Uint8Array(f.data)),
                    f.data instanceof ArrayBuffer]);
            }
            return __events.some(function (e) { return e[0] === 'ended'; }) && __frames.length === 4;
        })()"#,
    );
    let read: serde_json::Value = serde_json::from_str(&run(
        &mut runtime,
        "0",
        "JSON.stringify([__started, __frames, __events])",
    ))
    .unwrap();
    assert_eq!(
        read[0],
        serde_json::json!({ "errMsg": "start:ok", "width": 2, "height": 1 })
    );
    assert_eq!(
        read[1],
        serde_json::json!([
            [2, 1, 0, [0, 0, 0, 0, 0, 0, 0, 0], true],
            [2, 1, 40, [1, 1, 1, 1, 1, 1, 1, 1], true],
            [2, 1, 80, [2, 2, 2, 2, 2, 2, 2, 2], true],
            [2, 1, 120, [3, 3, 3, 3, 3, 3, 3, 3], true],
        ])
    );
    assert_eq!(
        read[2],
        serde_json::json!([["start", { "width": 2, "height": 1 }], ["ended", {}]])
    );

    // A seek after the end starts the frames again from there.
    pump(
        &mut runtime,
        &mut events,
        "__d.seek(80).then(function (res) { globalThis.__sought = res; });",
        "globalThis.__sought !== undefined",
    );
    pump(
        &mut runtime,
        &mut events,
        "0",
        "(function () { const f = __d.getFrameData(); if (f) globalThis.__after = f.pkPts; \
         return globalThis.__after !== undefined; })()",
    );
    assert_eq!(run(&mut runtime, "0", "String(__after)"), "80");

    // Stopping ends the decoding: no more frames, and a stop event.
    pump(
        &mut runtime,
        &mut events,
        "__d.stop().then(function (res) { globalThis.__stopped = res; });",
        "globalThis.__stopped !== undefined",
    );
    assert_eq!(
        run(
            &mut runtime,
            "0",
            "JSON.stringify([__stopped, __d.getFrameData(), __events.map(function (e) { return e[0]; })])",
        ),
        // The two frames after the seek decode ahead, and the stream ends again.
        r#"[{"errMsg":"stop:ok"},null,["start","ended","seek","ended","stop"]]"#
    );

    // Removed, it starts nothing.
    pump(
        &mut runtime,
        &mut events,
        "__d.remove(); __d.start({ source: '/user/video.mp4' }).catch(function (e) { globalThis.__gone = e; });",
        "globalThis.__gone !== undefined",
    );
    assert_eq!(
        run(&mut runtime, "0", "JSON.stringify(__gone)"),
        r#"{"errMsg":"start:fail the decoder is removed"}"#
    );
}

#[test]
fn a_source_that_does_not_open_fails_the_start() {
    let sandbox = Sandbox::new("video-decoder-bad");
    std::fs::write(sandbox.paths.user_data_dir().join("clip.webm"), b"webm").unwrap();
    let (mut runtime, mut events) = boot(&sandbox, true);
    pump(
        &mut runtime,
        &mut events,
        "migo.createVideoDecoder().start({ source: '/user/clip.webm' })\
         .catch(function (e) { globalThis.__failed = e; });\
         migo.createVideoDecoder().start({ source: '/etc/hosts' })\
         .catch(function (e) { globalThis.__outside = e; });",
        "globalThis.__failed !== undefined && globalThis.__outside !== undefined",
    );
    assert_eq!(
        run(&mut runtime, "0", "__failed.errMsg"),
        "start:fail unrecognised container"
    );
    assert!(run(&mut runtime, "0", "__outside.errMsg").starts_with("start:fail"));
}

#[test]
fn where_no_decoder_is_integrated_start_says_so_instead_of_waiting_forever() {
    let sandbox = Sandbox::new("video-decoder-none");
    let (mut runtime, mut events) = boot(&sandbox, false);
    runtime
        .execute_script("<test:none>", FastString::from_static("0"))
        .unwrap();
    pump(
        &mut runtime,
        &mut events,
        "const d = migo.createVideoDecoder(); globalThis.__frame = d.getFrameData();\
         d.start({ source: '/user/video.mp4' }).catch(function (e) { globalThis.__e = e; });",
        "globalThis.__e !== undefined",
    );
    assert_eq!(
        run(&mut runtime, "0", "JSON.stringify([__e, __frame])"),
        r#"[{"errMsg":"start:fail not supported"},null]"#
    );
}
