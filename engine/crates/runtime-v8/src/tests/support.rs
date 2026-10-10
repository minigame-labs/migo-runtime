//! What a test that boots a real runtime needs and does not care about: a host state with nothing behind it.
//!
//! Several tests carry their own copy of this; new ones should use it.

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

use deno_core::{FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions};
use shared::{
    channel::ThreadWakeup,
    device::gpu_caps::GpuCaps,
    op_state::{AudioSender, HostOpState, NetworkPolicy},
    render_command_sender::CommandSender,
    services::DeviceServices,
    vfs::{GamePaths, VirtualFS},
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

/// Real directories behind `/user`, `/cache` and `/tmp`, removed when dropped.
pub(super) struct Sandbox {
    root: PathBuf,
    pub(super) paths: GamePaths,
}

impl Sandbox {
    pub(super) fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths =
            GamePaths::new(root.join("files"), root.join("cache"), tag, 1).expect("game paths");
        paths.ensure_directories().expect("sandbox directories");
        Self { root, paths }
    }

    pub(super) fn vfs(&self) -> Arc<VirtualFS> {
        Arc::new(VirtualFS::from_game_paths(&self.paths))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A runtime over `services` and `vfs`, with the global scope content sees.
pub(super) fn boot_with_services(
    services: Option<Arc<dyn DeviceServices>>,
    vfs: Option<Arc<VirtualFS>>,
) -> JsRuntime {
    let mut state = test_host_state();
    state.device_services = services;
    state.vfs = vfs;
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: crate::main_extensions(state),
        ..Default::default()
    });
    crate::harden_global_scope(&mut runtime);
    runtime
}

/// Run `source`, let every op and microtask it started finish, then evaluate
/// `check` and return it as a string.
pub(super) fn run_to_idle(runtime: &mut JsRuntime, source: &str, check: &str) -> String {
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    reactor.block_on(async {
        runtime
            .execute_script("<test:run>", FastString::from(source.to_owned()))
            .expect("test script");
        runtime
            .run_event_loop(PollEventLoopOptions::default())
            .await
            .expect("event loop");
    });
    let value = runtime
        .execute_script("<test:check>", FastString::from(check.to_owned()))
        .expect("test check");
    deno_core::scope!(scope, runtime);
    let local = deno_core::v8::Local::new(scope, value);
    local.to_rust_string_lossy(scope)
}

/// Whether `source` evaluates to `true`.
pub(super) fn is_true(runtime: &mut JsRuntime, source: &'static str) -> bool {
    let value = runtime
        .execute_script("<test:check>", FastString::from_static(source))
        .expect("test check");
    deno_core::scope!(scope, runtime);
    deno_core::v8::Local::new(scope, value).is_true()
}

/// `__settle(name, options)` calls `migo[name]` and records its outcome in
/// `__results[name]` as `{ok, res}`.
pub(super) const SETTLE: &str = r#"
    globalThis.__results = {};
    globalThis.__settle = function (name, options) {
        const opts = Object.assign({}, options || {});
        opts.success = function (res) { globalThis.__results[name] = { ok: true, res: res }; };
        opts.fail = function (res) { globalThis.__results[name] = { ok: false, res: res }; };
        migo[name](opts);
    };
"#;
