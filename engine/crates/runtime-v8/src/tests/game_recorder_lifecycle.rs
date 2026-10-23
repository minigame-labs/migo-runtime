//! `getGameRecorder` as content uses it: one recorder, the reference's error
//! codes for a call its state or options do not allow -- as an `error` event,
//! and as the rejection of the calls that return a Promise -- the events of a
//! recording the render thread makes, and what reaches the render thread.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use deno_core::{FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions, serde_json};
use shared::{
    game_recorder::{GameRecorderCmd, RecordingParams},
    protocol::render_cmd::RenderCommand,
    vfs::GamePaths,
};

/// What the renderer was asked, in order.
#[derive(Default)]
struct Asked(Mutex<Vec<String>>);

/// A runtime whose renderer records like a device that can: a recording starts,
/// reports a second and a half, and stops at 3.2 s.
fn boot(
    asked: Arc<Asked>,
) -> (
    JsRuntime,
    tokio::runtime::Runtime,
    std::thread::JoinHandle<()>,
) {
    let (render_tx, render_rx) = shared::render_command_sender::CommandSender::new();
    let renderer = std::thread::spawn(move || {
        let mut events = None;
        while let Ok(command) = render_rx.recv_timeout(Duration::from_secs(5)) {
            let RenderCommand::GameRecorder(cmd) = command else {
                continue;
            };
            asked.0.lock().unwrap().push(format!("{cmd:?}"));
            match cmd {
                GameRecorderCmd::Supported { resp } => resp.ok(true),
                GameRecorderCmd::Start {
                    params,
                    path,
                    events: heard,
                    resp,
                } => {
                    assert_eq!(
                        params,
                        RecordingParams {
                            fps: 30,
                            bitrate_kbps: 2000,
                            gop: 30,
                            duration_ms: 10_000
                        }
                    );
                    assert!(path.ends_with("replay.mp4"), "{path:?}");
                    resp.ok(());
                    heard.time(1500);
                    events = Some(heard);
                }
                GameRecorderCmd::Pause { resp } | GameRecorderCmd::Resume { resp } => resp.ok(()),
                GameRecorderCmd::Stop { resp } => {
                    resp.ok(3200);
                    if let Some(events) = events.take() {
                        events.close();
                    }
                }
                GameRecorderCmd::Abort { resp } => resp.ok(()),
            }
        }
    });
    let root = std::env::temp_dir().join(format!("migo-game-recorder-{}", std::process::id()));
    let mut state = super::support::test_host_state();
    state.render_tx = render_tx;
    state.game_paths = Some(Arc::new(
        GamePaths::new(root.join("files"), root.join("cache"), "recorder-test", 1).unwrap(),
    ));
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let runtime = {
        let _inside = reactor.enter();
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(state),
            ..Default::default()
        });
        crate::harden_global_scope(&mut runtime);
        runtime
    };
    (runtime, reactor, renderer)
}

/// Run `source`, then the event loop for a moment -- the recorder's event op
/// waits for as long as a recording lasts -- and evaluate `check`.
fn run(
    runtime: &mut JsRuntime,
    reactor: &tokio::runtime::Runtime,
    source: &str,
    check: &str,
) -> String {
    let _inside = reactor.enter();
    runtime
        .execute_script("<test:recorder>", FastString::from(source.to_owned()))
        .expect("test script");
    reactor.block_on(async {
        let _ = tokio::time::timeout(
            Duration::from_millis(300),
            runtime.run_event_loop(PollEventLoopOptions::default()),
        )
        .await;
    });
    let value = runtime
        .execute_script("<test:check>", FastString::from(check.to_owned()))
        .expect("test check");
    deno_core::scope!(scope, runtime);
    deno_core::v8::Local::new(scope, value).to_rust_string_lossy(scope)
}

#[test]
fn a_recording_answers_in_the_reference_s_terms() {
    let asked = Arc::new(Asked::default());
    let (mut runtime, reactor, renderer) = boot(Arc::clone(&asked));
    let heard = run(
        &mut runtime,
        &reactor,
        "globalThis.__out = []; \
         const r = migo.getGameRecorder(); \
         __out.push(String(r === migo.getGameRecorder()), String(r.isFrameSupported()), String(r.isSoundSupported())); \
         for (const e of ['start', 'stop', 'pause', 'resume', 'abort', 'timeUpdate', 'error']) \
           r.on(e, (res) => __out.push(e + (res === undefined ? '' : ' ' + JSON.stringify(res)))); \
         r.stop().catch((res) => __out.push('stop refused ' + res.error.code)); \
         r.start({ fps: 61 }); \
         r.start({ duration: 4 }); \
         r.start({ fps: 30, bitrate: 2000, gop: 30, duration: 10 });",
        "JSON.stringify(__out)",
    );
    let heard: Vec<String> = serde_json::from_str(&heard).unwrap();
    assert_eq!(
        heard,
        [
            "true",
            "true",
            "false",
            r#"error {"error":{"code":22503,"message":"stop while not start recording"}}"#,
            r#"error {"error":{"code":22123,"message":"fps invalid"}}"#,
            r#"error {"error":{"code":22103,"message":"duration invalid"}}"#,
            "stop refused 22503",
            "start",
            r#"timeUpdate {"currentTime":1.5}"#,
        ]
    );

    let heard = run(
        &mut runtime,
        &reactor,
        "__out.length = 0; \
         r.start({}); \
         r.pause().then(() => __out.push('paused'));",
        "JSON.stringify(__out)",
    );
    assert_eq!(
        heard,
        r#"["error {\"error\":{\"code\":22143,\"message\":\"start while already start recording\"}}","pause","paused"]"#
    );

    let heard = run(
        &mut runtime,
        &reactor,
        "__out.length = 0; \
         r.pause().catch((res) => __out.push('pause refused ' + res.error.code)); \
         r.start({}); \
         r.resume().then(() => r.stop()).then((res) => __out.push('stopped ' + res.duration));",
        "JSON.stringify(__out)",
    );
    let heard: Vec<String> = serde_json::from_str(&heard).unwrap();
    assert_eq!(
        heard,
        [
            r#"error {"error":{"code":22213,"message":"pause while already paused"}}"#,
            r#"error {"error":{"code":22153,"message":"start while already paused"}}"#,
            "pause refused 22213",
            "resume",
            r#"stop {"duration":3200}"#,
            "stopped 3200",
        ]
    );
    drop(runtime);
    renderer.join().unwrap();
    let asked = asked.0.lock().unwrap().clone();
    assert_eq!(
        asked,
        ["Supported", "Start", "Pause", "Resume", "Stop"],
        "nothing refused reached the renderer"
    );
}
