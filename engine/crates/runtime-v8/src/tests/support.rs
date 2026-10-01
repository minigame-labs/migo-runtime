//! What a test that boots a real runtime needs and does not care about: a host state with nothing behind it.
//!
//! Several tests carry their own copy of this; new ones should use it.

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

use shared::{
    channel::ThreadWakeup,
    device::gpu_caps::GpuCaps,
    op_state::{AudioSender, HostOpState, NetworkPolicy},
    render_command_sender::CommandSender,
};

pub(super) fn test_host_state() -> HostOpState {
    test_host_state_with_audio(shared::audio_channel::disconnected())
}

/// The same, with an audio command channel something answers (a test that plays the audio thread).
pub(super) fn test_host_state_with_audio(
    audio_tx: shared::audio_channel::AudioCommandSender,
) -> HostOpState {
    let (render_tx, _render_rx) = CommandSender::new();
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
        audio_tx: AudioSender::new(audio_tx, ThreadWakeup::new()),
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
