//! V05 end-to-end regression: `code_cache_dir` must be the directory that
//! actually receives V8 bytecode, not the ordinary app cache directory.
//!
//! # What was wrong
//!
//! `InitOptions::code_cache_dir` was plumbed all the way from the C ABI and
//! Android bridge into `InitOptions`, but `Host::new` then handed
//! `cache_dir()` (the ordinary app cache) to the runtime instead of
//! `code_cache_root()`.  A host that deliberately pointed the code cache at a
//! different volume silently got a subdirectory of the ordinary cache.
//!
//! The fix added `InitOptions::code_cache_root()` and pointed both call sites
//! at it.  A compiler guarantee alone is not a behavioural one: someone can
//! re-point the call inside `HostJsRuntime::new` at any path tomorrow without
//! breaking an existing test.  This test is the missing behavioural assertion.
//!
//! # What this test proves
//!
//! * `HostJsRuntime::new` actually honours its `code_cache_root` argument:
//!   when a directory distinct from `app_cache_dir` is passed, compiled
//!   bytecode lands in *that* directory and not in `app_cache_dir`.
//!
//! * The proof is RED-first: see the "Regression probe" section in the test
//!   doc below for the exact change that makes it fail.
//!
//! # Determinism
//!
//! `DiskCodeCache::Drop` drops the write channel and then joins the writer
//! thread, so every queued `.bin` write has reached the filesystem by the
//! time `drop(rt)` returns.  No sleep or wall-clock timeout is needed.

#[cfg(test)]
mod code_cache_dir_tests {
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

    /// Minimal `HostOpState` whose `app_cache_dir` is the supplied path.
    ///
    /// The parameter is the ordinary app cache directory — deliberately kept
    /// separate from the `code_cache_root` passed to `HostJsRuntime::new`
    /// so the test can assert they are treated independently.
    fn test_host_state(app_cache_dir: PathBuf) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);
        let (audio_tx, _audio_rx) = shared::audio_channel::channel();

        HostOpState {
            callback_ids: Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id: 1,
            app_cache_dir,
            app_files_dir: PathBuf::from("/tmp/migo_ccdir_files"),
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

    /// Count `.bin` files directly inside `dir/migo_code_cache/`.
    ///
    /// Returns 0 if the subdirectory does not exist.
    fn count_bin_files(dir: &std::path::Path) -> usize {
        let cache_subdir = dir.join("migo_code_cache");
        if !cache_subdir.exists() {
            return 0;
        }
        std::fs::read_dir(&cache_subdir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map_or(false, |x| x == "bin"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// V05 regression: bytecode goes to the configured directory, not to the
    /// ordinary app cache.
    ///
    /// # Mechanism
    ///
    /// In test builds `HostJsRuntime::new` always forces `snapshot_bytes = None`
    /// (see the `#[cfg(test)]` block in `host_runtime.rs`), so `JsRuntime::new`
    /// compiles all extension JS from source.  deno_core calls
    /// `ExtCodeCacheAdapter::code_cache_ready` for every extension module it
    /// compiles, which enqueues a write job.
    ///
    /// `DiskCodeCache::Drop` drops the write-channel sender and joins the writer
    /// thread, so by the time `drop(rt)` returns all queued jobs are on disk.
    ///
    /// # Regression probe (how to make this RED)
    ///
    /// Change the `HostJsRuntime::new` call below to pass `&ordinary_dir`
    /// instead of `&code_cache_dir` as `code_cache_root`.  The assertion
    /// "code_cache_dir received at least one .bin" will fail because no write
    /// ever targets that path.  Restore `&code_cache_dir` to go green again.
    #[test]
    fn bytecode_lands_in_configured_dir_not_in_ordinary_cache() {
        let pid = std::process::id();
        let ordinary_dir = std::env::temp_dir().join(format!("migo_ccdir_ordinary_{pid}"));
        let code_cache_dir = std::env::temp_dir().join(format!("migo_ccdir_cc_{pid}"));

        std::fs::create_dir_all(&ordinary_dir).expect("ordinary dir writable");
        std::fs::create_dir_all(&code_cache_dir).expect("code cache dir writable");

        // Boot and immediately drop.  Drop joins the writer thread, so every
        // enqueued .bin write has landed before we inspect the directories.
        {
            let host_state = test_host_state(ordinary_dir.clone());
            let _rt = crate::HostJsRuntime::new(
                /*host_id=*/ 1,
                host_state,
                /*code_cache_root=*/ &code_cache_dir,
                #[cfg(feature = "v8-limits")]
                crate::V8LimitsConfig::default(),
                #[cfg(feature = "code-signing")]
                false,
                #[cfg(feature = "code-signing")]
                None,
            );
            // `_rt` drops here.  `DiskCodeCache::Drop` drops write_tx (causing
            // the worker loop to exit) then joins the thread, so all disk writes
            // are complete before this block exits.
        }

        let cc_bins = count_bin_files(&code_cache_dir);
        assert!(
            cc_bins > 0,
            "expected at least one .bin in {}/migo_code_cache, found none — \
             HostJsRuntime::new is not writing to code_cache_root",
            code_cache_dir.display(),
        );

        let ordinary_bins = count_bin_files(&ordinary_dir);
        assert_eq!(
            ordinary_bins,
            0,
            "found {ordinary_bins} .bin file(s) in {}/migo_code_cache — \
             code cache must write to code_cache_root, not to app_cache_dir",
            ordinary_dir.display(),
        );

        let _ = std::fs::remove_dir_all(&ordinary_dir);
        let _ = std::fs::remove_dir_all(&code_cache_dir);
    }
}
