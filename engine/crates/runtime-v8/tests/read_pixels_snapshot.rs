//! Tests the official embedded snapshot in a separate V8 process. The render
//! channel is simulated; this checks startup and the JS/native copy boundary.
use deno_core::{JsRuntime, RuntimeOptions};
use std::{path::PathBuf, sync::Arc};

#[path = "support/read_pixels_responder.rs"]
mod responder;

#[test]
#[ignore = "requires the freshly generated official host snapshot; run explicitly"]
fn official_snapshot_read_pixels_preserves_destination_offsets() {
    let snapshot =
        runtime_v8::snapshot::SNAPSHOT_BYTES.expect("official snapshot must be embedded");
    let (render_tx, render_rx) = shared::render_command_sender::CommandSender::new();
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: runtime_v8::snapshot::lazy_extensions(),
        startup_snapshot: Some(snapshot),
        skip_op_registration: true,
        ..Default::default()
    });
    runtime
        .lazy_init_extensions(runtime_v8::snapshot::extension_args(test_host_state(
            render_tx,
        )))
        .unwrap();
    let responder = responder::spawn(render_rx);
    let result = runtime.execute_script(
        "snapshot-read-pixels-offsets",
        include_str!("fixtures/read_pixels_offset.js"),
    );
    drop(runtime);
    let lengths = responder.join().unwrap();
    result.unwrap();
    responder::assert_lengths(lengths);
}

fn test_host_state(
    render_tx: shared::render_command_sender::CommandSender,
) -> shared::op_state::HostOpState {
    use shared::channel::ThreadWakeup;
    use shared::device::gpu_caps::GpuCaps;
    use shared::op_state::{AudioSender, HostOpState, NetworkPolicy};
    use std::sync::atomic::AtomicBool;

    let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

    HostOpState {
        callback_ids: Arc::new(shared::callback_id::CallbackIdAllocator::default()),
        runtime_generation: 1,
        id: 1,
        app_cache_dir: PathBuf::from("/tmp/cache"),
        app_files_dir: PathBuf::from("/tmp/files"),
        code_dir: None,
        game_paths: None,
        vfs: None,
        mount_table: None,
        render_tx,
        text_measurer: None,
        audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
        host_tx,
        device_services: None,
        raf_rx: None,
        raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
        request_vsync: None,
        sub_packages: Vec::new(),
        workers_path: None,
        network_policy: NetworkPolicy::default(),
        backgrounded: Arc::new(AtomicBool::new(false)),
        timer_backgrounded: Arc::new(AtomicBool::new(false)),
        webgl_context_created: Arc::new(AtomicBool::new(false)),
        context_lost: Arc::new(shared::op_state::ContextLostState::default()),
        code_signing_enabled: false,
        gpu_caps: GpuCaps::new(),
    }
}
