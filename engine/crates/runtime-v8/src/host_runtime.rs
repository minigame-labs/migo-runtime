use std::{cell::RefCell, future::Future, path::PathBuf, rc::Rc, sync::Arc, time::Instant};

use deno_core::{JsRuntime, ModuleLoader, PollEventLoopOptions, RuntimeOptions, resolve_path, v8};

use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    op_state::HostOpState,
    protocol::host_cmd::{TouchPoint, TouchType},
    vfs::{GamePaths, MountTable, VirtualFS},
};

/// Shared reference to the mount table, injected into the module loader.
/// The loader holds a clone and checks it on every resolve/load.
pub type SharedMountTableRef = Rc<RefCell<Option<Arc<MountTable>>>>;

#[cfg(feature = "code-signing")]
use shared::vfs::integrity::IntegrityVerifier;

#[cfg(feature = "code-signing")]
use migo_io::task::PriorityClass;

use crate::{js_bindings::JsBindings, main_extensions};

#[cfg(feature = "v8-limits")]
use crate::watchdog::{DeadlineWatchdog, DeadlineWatchdogConfig};

/// Configuration for V8 heap limits and execution watchdog.
///
/// When `v8-limits` feature is enabled, these settings are applied during
/// runtime creation to protect the host process from runaway JS code.
#[derive(Debug, Clone)]
pub struct V8LimitsConfig {
    /// Maximum V8 heap size in bytes. Default: 256 MB.
    pub max_heap_size: usize,
    /// Initial V8 heap size in bytes. 0 = V8 default.
    pub initial_heap_size: usize,
}

impl Default for V8LimitsConfig {
    fn default() -> Self {
        Self {
            max_heap_size: 256 * 1024 * 1024, // 256 MB
            initial_heap_size: 0,             // V8 default
        }
    }
}

impl V8LimitsConfig {
    /// Create config from `InitOptions.max_memory_mb`.
    pub fn from_max_memory_mb(mb: i32) -> Self {
        let mb = mb.clamp(64, 2048) as usize;
        Self {
            max_heap_size: mb * 1024 * 1024,
            initial_heap_size: 0,
        }
    }
}

pub struct HostJsRuntime {
    /// Process deadline watchdog for this isolate. Declared FIRST so struct-drop
    /// order disarms + unregisters it before the V8 `JsRuntime`/isolate is
    /// dropped. Present only when `v8-limits` is enabled; installed by the host
    /// after trusted bootstrap and before any game prelude/module runs.
    #[cfg(feature = "v8-limits")]
    watchdog: Option<DeadlineWatchdog>,
    host_id: i32,
    rt: JsRuntime,
    bindings: JsBindings,
    /// Shared mount table reference — the module loader holds a clone.
    /// Updated in evaluate_module() so the loader can enforce sandbox.
    loader_mount_ref: SharedMountTableRef,
    /// Shared termination state for OOM callback + watchdog integration.
    /// Only present when `v8-limits` feature is enabled.
    #[cfg(feature = "v8-limits")]
    oom_terminated: Arc<std::sync::atomic::AtomicBool>,
    /// Code integrity verifier (Ed25519 + SHA256).
    /// Present when code signing is enabled and configured correctly.
    #[cfg(feature = "code-signing")]
    integrity_verifier: Option<IntegrityVerifier>,
    /// Sticky configuration error for code signing.
    ///
    /// We don't fail runtime construction to keep API shape unchanged, but
    /// module evaluation will fail closed if this is set.
    #[cfg(feature = "code-signing")]
    code_signing_error: Option<EngineError>,
    /// Whether code signing enforcement is enabled for this runtime.
    #[cfg(feature = "code-signing")]
    code_signing_enabled: bool,
}

impl HostJsRuntime {
    /// Create a fully initialized JS runtime + bindings cache.
    ///
    /// - `host_state` will be consumed by runtime-v8 extensions
    /// - `code_cache_root` is the directory the V8 code cache persists compiled
    ///   bytecode under; the module loader and code cache are assembled
    ///   internally. It is `InitOptions::code_cache_root()`, not the ordinary
    ///   cache directory -- those are separate options and a host can point them
    ///   at different volumes.
    /// - `v8_limits` configures heap limits when `v8-limits` feature is enabled
    pub fn new(
        host_id: i32,
        host_state: HostOpState,
        code_cache_root: &std::path::Path,
        #[cfg(feature = "v8-limits")] v8_limits: V8LimitsConfig,
        #[cfg(feature = "code-signing")] code_signing_enabled: bool,
        #[cfg(feature = "code-signing")] code_signing_pubkey: Option<&str>,
    ) -> Self {
        // Backend-owned module loader + V8 code cache assembly. Moved out of
        // core (Phase B) so the orchestration layer never names deno_core. The
        // disk code cache is shared between the module loader and the V8
        // extension code cache; the mount ref is populated later by
        // evaluate_module.
        let shared_cache = crate::code_cache::create_code_cache(code_cache_root);
        let loader_mount_ref: SharedMountTableRef = Rc::new(RefCell::new(None));
        let module_loader: Option<Rc<dyn ModuleLoader>> =
            Some(Rc::new(crate::loader::MyModuleLoader::new(
                Some(shared_cache.clone()),
                loader_mount_ref.clone(),
            )));
        let extension_code_cache: Option<Rc<dyn deno_core::ExtCodeCache>> =
            Some(crate::code_cache::ExtCodeCacheAdapter::new(shared_cache));
        // V8 startup snapshot support.
        //
        // When a snapshot is available, we use lazy_extensions() to create
        // extensions with JS already captured in the snapshot (no re-parsing).
        // The state callbacks are deferred and applied via
        // lazy_init_extensions() after the runtime is created.
        //
        // When no snapshot is available, we fall back to main_extensions()
        // which creates fully-initialized extensions with JS from source.
        let t0 = Instant::now();
        // This crate's own unit tests boot from source, never from the snapshot.
        //
        // V8's snapshot mode is decided process-wide on first init, so one test
        // binary cannot hold both snapshot-booted and source-booted runtimes:
        // mixing them aborts with SIGILL during teardown, after every test has
        // already reported success. `tests/snapshot_roundtrip.rs` records the same
        // constraint from the creation side and answers it the same way -- its own
        // binary, its own process.
        //
        // Without this, `cargo test -p migo-runtime-v8` passes on CI and aborts on
        // a developer machine, purely because CI has no host V8 archive for
        // build.rs to validate a snapshot against and so embeds none. The suite
        // should not depend on which machine it runs on.
        //
        // `cfg(test)` covers exactly the unit-test harness. Integration tests under
        // `tests/` link the library compiled without it, so they still get the real
        // snapshot -- which is what makes a dedicated binary the place to exercise
        // that path -- and shipping builds are untouched.
        #[cfg(test)]
        let snapshot_bytes: Option<&'static [u8]> = None;
        #[cfg(not(test))]
        let snapshot_bytes = crate::snapshot::SNAPSHOT_BYTES;
        let use_snapshot = snapshot_bytes.is_some();

        let exts = if use_snapshot {
            tracing::info!(
                "[Host {}] using V8 startup snapshot ({} bytes)",
                host_id,
                snapshot_bytes.map(|b| b.len()).unwrap_or(0)
            );
            crate::snapshot::lazy_extensions()
        } else {
            tracing::info!("[Host {}] no snapshot, loading JS from source", host_id);
            main_extensions(host_state.clone())
        };
        let t_exts = Instant::now();
        tracing::info!(
            "[Host {}] extensions assembled: {:.1}ms (snapshot={})",
            host_id,
            t_exts.duration_since(t0).as_secs_f64() * 1000.0,
            use_snapshot,
        );

        // Build create_params with heap limits when feature is enabled
        #[cfg(feature = "v8-limits")]
        let create_params = {
            Some(
                v8::Isolate::create_params()
                    .heap_limits(v8_limits.initial_heap_size, v8_limits.max_heap_size),
            )
        };
        #[cfg(not(feature = "v8-limits"))]
        let create_params: Option<v8::CreateParams> = None;

        // Jitless measurement build (`--features v8-jitless`, off by default).
        //
        // HarmonyOS 5.0.0(12) stopped letting anonymous memory become executable,
        // so no third-party VM may JIT there; Migo bundles V8, so on NEXT it runs
        // interpreted whether it asks to or not. `--jitless` is the closest proxy
        // available on hardware we can actually measure, and the number it
        // produces is what gates whether NEXT performance may be spoken about at
        // all.
        //
        // A build feature rather than a runtime switch, for two reasons. The
        // measurement has to run against a release build -- a debug build is
        // opt-level 0, and a constant native overhead in both arms would flatter
        // jitless, which is the dangerous direction to be wrong in -- and the
        // debug-only file path below is compiled out of release on purpose.
        // Adding a shipping runtime switch to answer a one-off question would be
        // paying in permanent surface for a number.
        //
        // Worth knowing before reading any result: `--jitless` does not merely
        // slow WebAssembly down, it removes it. V8 cannot instantiate a module
        // without generating code. Content that ships `.wasm` does not run
        // slower under this flag; it does not run.
        #[cfg(feature = "v8-jitless")]
        {
            use std::sync::Once;
            static V8_JITLESS: Once = Once::new();
            V8_JITLESS.call_once(|| {
                tracing::warn!("jitless measurement build: applying --jitless to V8");
                deno_core::v8::V8::set_flags_from_string("--jitless");
            });
        }

        // Debug-only V8 flag injection for on-device profiling. On debug builds,
        // any V8 flags placed in /data/local/tmp/v8flags.txt are applied before
        // the isolate is created, e.g.:
        //   --prof --logfile=/data/data/<pkg>/files/v8.log   (SIGPROF sampling; no perf_event)
        //   --trace-deopt / --trace-opt                      (deopt/optimization tracing)
        // Compiled out of release builds (cfg!(debug_assertions) is false there),
        // so shipping builds never read the world-writable temp file. See scripts/
        // profile-migo.sh + tickparse.mjs in the bench repo for the full workflow.
        #[cfg(debug_assertions)]
        {
            use std::sync::Once;
            static V8_FLAGS: Once = Once::new();
            V8_FLAGS.call_once(|| {
                let flags =
                    std::fs::read_to_string("/data/local/tmp/v8flags.txt").unwrap_or_default();
                let flags = flags.trim();
                if !flags.is_empty() {
                    tracing::error!("applying debug V8 flags from v8flags.txt: {flags}");
                    deno_core::v8::V8::set_flags_from_string(flags);
                }
            });
        }

        let t_rt_start = Instant::now();
        let sab_store = deno_core::SharedArrayBufferStore::default();

        let mut rt = JsRuntime::new(RuntimeOptions {
            module_loader,
            extensions: exts,
            create_params,
            startup_snapshot: snapshot_bytes,
            extension_code_cache,
            shared_array_buffer_store: Some(sab_store.clone()),
            // Ops are already registered inside the snapshot. Without this,
            // InitMode::FromSnapshot{skip_op_registration:false} makes deno_core
            // re-bind ops via initialize_deno_core_ops_bindings → get(Deno.core.ops),
            // which panics ("unable to convert"). We trust the snapshot's ops;
            // per-extension state is restored below via lazy_init_extensions().
            skip_op_registration: use_snapshot,
            ..Default::default()
        });
        let t_rt_created = Instant::now();
        tracing::info!(
            "[Host {}] JsRuntime::new: {:.1}ms",
            host_id,
            t_rt_created.duration_since(t_rt_start).as_secs_f64() * 1000.0,
        );

        // When using snapshot, apply deferred state callbacks now
        if use_snapshot {
            let ext_args = crate::snapshot::extension_args(host_state);
            if let Err(e) = rt.lazy_init_extensions(ext_args) {
                tracing::error!(
                    "[Host {}] failed to init extensions from snapshot: {}, falling back",
                    host_id,
                    e
                );
                // NOTE: In a production scenario, we'd recreate without snapshot.
                // For now, the error is logged and the runtime continues (ops
                // without state will panic when called — this is a build/deploy
                // mismatch that should be caught in CI).
            }
            tracing::info!(
                "[Host {}] lazy_init_extensions: {:.1}ms",
                host_id,
                t_rt_created.elapsed().as_secs_f64() * 1000.0,
            );
        }

        // Harden the game-visible global scope: remove deno_core's bootstrap
        // internals (`Deno`, `__bootstrap`) at RUNTIME, for BOTH the snapshot and
        // non-snapshot paths. This is intentionally not done in the bootstrap
        // module (99_main.js) so `Deno.core` survives in the V8 startup snapshot
        // for deno_core's restore path. See `crate::harden_global_scope`.
        crate::harden_global_scope(&mut rt);

        // Register near-heap-limit callback that terminates execution on OOM
        #[cfg(feature = "v8-limits")]
        let oom_terminated = {
            let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cb_terminated = Arc::clone(&terminated);
            let cb_handle = rt.v8_isolate().thread_safe_handle();
            let hard_cap = v8_limits.max_heap_size.saturating_add(8 * 1024 * 1024);

            rt.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
                // Mark OOM once, then terminate execution. Keep a tiny
                // bounded headroom instead of unbounded growth.
                let first = cb_terminated
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok();
                if first {
                    cb_handle.terminate_execution();
                }
                current_limit.saturating_add(1024 * 1024).min(hard_cap)
            });

            terminated
        };

        // Set thread-local host ID so op_console can route logs to the
        // per-session ring buffer without accessing OpState.
        crate::console::bind_thread_console(host_id);

        let bindings = JsBindings::new(&mut rt, host_id);
        tracing::info!(
            "[Host {}] HostJsRuntime::new total: {:.1}ms",
            host_id,
            t0.elapsed().as_secs_f64() * 1000.0,
        );

        // Initialize code signing verifier from hex-encoded public key.
        // Enforced fail-closed: when code signing is enabled but misconfigured,
        // module load returns a deterministic signature/config error.
        #[cfg(feature = "code-signing")]
        let (integrity_verifier, code_signing_error) = if code_signing_enabled {
            match code_signing_pubkey {
                Some(hex_key) if !hex_key.is_empty() => {
                    match IntegrityVerifier::from_hex_pubkey(hex_key) {
                        Ok(v) => {
                            tracing::info!("[Host {}] code signing enabled", host_id);
                            (Some(v), None)
                        }
                        Err(e) => {
                            tracing::error!("[Host {}] code signing key invalid: {}", host_id, e);
                            (None, Some(e))
                        }
                    }
                }
                _ => {
                    let err = EngineError::new(ErrorCode::CodeSignatureInvalid)
                        .with_msg("code signing enabled but public key is missing")
                        .with_detail(
                            "set InitOptions.code_signing_pubkey (hex Ed25519 public key)",
                        );
                    tracing::error!("[Host {}] {}", host_id, err);
                    (None, Some(err))
                }
            }
        } else {
            tracing::info!("[Host {}] code signing disabled by init options", host_id);
            (None, None)
        };

        // Store the SAB store in OpState, which is where `worker::spawn` reads
        // it from. `rt` owns that OpState and `RuntimeOptions` above holds its
        // own clone, so those two copies are what keep the store alive; the
        // struct field that used to sit here was a third copy nothing ever read,
        // carrying an `#[allow(dead_code)]` that made it look load-bearing.
        rt.op_state().borrow_mut().put(sab_store);

        Self {
            #[cfg(feature = "v8-limits")]
            watchdog: None,
            host_id,
            rt,
            bindings,
            loader_mount_ref,
            #[cfg(feature = "v8-limits")]
            oom_terminated,
            #[cfg(feature = "code-signing")]
            integrity_verifier,
            #[cfg(feature = "code-signing")]
            code_signing_error,
            #[cfg(feature = "code-signing")]
            code_signing_enabled,
        }
    }

    /// Get a thread-safe handle to the V8 isolate for cross-thread termination.
    pub fn isolate_handle(&mut self) -> v8::IsolateHandle {
        self.rt.v8_isolate().thread_safe_handle()
    }

    /// Check if the runtime was terminated due to OOM (near-heap-limit callback).
    #[cfg(feature = "v8-limits")]
    pub fn was_oom_terminated(&self) -> bool {
        self.oom_terminated
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Reset OOM termination state. Called after `cancel_terminate_execution`
    /// during restart to allow the new runtime to operate normally.
    #[cfg(feature = "v8-limits")]
    pub fn reset_oom_state(&self) {
        self.oom_terminated
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Install the process deadline watchdog for this isolate. Call once, after
    /// trusted runtime/bootstrap construction and before any game prelude or
    /// module executes. Fails only if the one process monitor thread cannot be
    /// created.
    #[cfg(feature = "v8-limits")]
    pub fn install_watchdog(&mut self, config: DeadlineWatchdogConfig) -> EngineResult<()> {
        let handle = self.rt.v8_isolate().thread_safe_handle();
        self.watchdog = Some(DeadlineWatchdog::register_isolate(handle, config)?);
        Ok(())
    }

    /// Whether the deadline watchdog terminated this isolate (sticky). Used by
    /// `core` error classification after the OOM check.
    #[cfg(feature = "v8-limits")]
    pub fn watchdog_timed_out(&self) -> bool {
        self.watchdog
            .as_ref()
            .is_some_and(DeadlineWatchdog::timed_out)
    }

    /// Run `f` with an owner-thread execution guard armed for the duration of the
    /// synchronous V8 call. The guard arms on entry and disarms on return, so a
    /// runaway synchronous callback is force-terminated while non-V8 host work is
    /// never charged as JS time. With `v8-limits` disabled this is a transparent
    /// pass-through with no guard.
    #[cfg(feature = "v8-limits")]
    #[inline]
    fn with_v8<R>(&mut self, f: impl FnOnce(&mut JsRuntime, &mut JsBindings) -> R) -> R {
        let _scope = self.watchdog.as_ref().map(DeadlineWatchdog::enter);
        f(&mut self.rt, &mut self.bindings)
    }

    #[cfg(not(feature = "v8-limits"))]
    #[inline]
    fn with_v8<R>(&mut self, f: impl FnOnce(&mut JsRuntime, &mut JsBindings) -> R) -> R {
        f(&mut self.rt, &mut self.bindings)
    }

    /// The op state, for cases that read a counter the runtime keeps there.
    #[cfg(test)]
    pub(crate) fn op_state_for_test(&self) -> std::rc::Rc<std::cell::RefCell<deno_core::OpState>> {
        self.rt.op_state()
    }

    /// Access HostOpState for mutation.
    pub fn update_host_op_state<F>(&mut self, updater: F)
    where
        F: FnOnce(&mut HostOpState),
    {
        let op_state_rc = self.rt.op_state();
        let mut op_state = op_state_rc.borrow_mut();
        updater(op_state.borrow_mut::<HostOpState>());
    }

    /// Publish the timer lifecycle level and notify the active Worker. The
    /// caller owns sequencing relative to the main isolate's lifecycle script.
    pub fn set_timer_backgrounded(&mut self, backgrounded: bool) {
        let op_state_rc = self.rt.op_state();
        #[cfg(feature = "api-system")]
        {
            let mut op_state = op_state_rc.borrow_mut();
            let previous = op_state
                .borrow::<HostOpState>()
                .timer_backgrounded
                .swap(backgrounded, std::sync::atomic::Ordering::AcqRel);
            if previous != backgrounded {
                crate::worker::set_timer_backgrounded(&mut op_state, backgrounded);
            }
        }

        #[cfg(not(feature = "api-system"))]
        {
            op_state_rc
                .borrow()
                .borrow::<HostOpState>()
                .timer_backgrounded
                .store(backgrounded, std::sync::atomic::Ordering::Release);
        }
    }

    pub fn set_code_dir(&mut self, dir: Option<String>) {
        self.update_host_op_state(|s| s.code_dir = dir);
    }

    /// Set the VirtualFS for sandboxed file access.
    pub fn set_vfs(&mut self, vfs: Option<Arc<VirtualFS>>) {
        self.update_host_op_state(|s| s.vfs = vfs);
    }

    /// Set the MountTable for `/code` path resolution.
    /// Also injects it into the module loader for sandbox enforcement.
    pub fn set_mount_table(&mut self, mt: Option<Arc<MountTable>>) {
        *self.loader_mount_ref.borrow_mut() = mt.clone();
        self.update_host_op_state(|s| s.mount_table = mt);
    }

    /// Set the GamePaths for the current game.
    pub fn set_game_paths(&mut self, paths: Option<Arc<GamePaths>>) {
        self.update_host_op_state(|s| s.game_paths = paths);
    }

    /// Read HostOpState values.
    pub fn get_base_dirs(&self) -> (PathBuf, PathBuf) {
        let op_state_rc = self.rt.op_state();
        let op_state = op_state_rc.borrow();
        let host = op_state.borrow::<HostOpState>();
        (host.app_files_dir.clone(), host.app_cache_dir.clone())
    }

    /// The runtime's own op state.
    ///
    /// Exists so a caller can put a question to the *production* resolver over
    /// the state a real `evaluate_module` left behind, rather than over a
    /// hand-built `OpState` that agrees with it by construction. That is the
    /// difference between "the resolver separates two `GamePaths`" — already
    /// covered — and "two live isolates hold two different ones".
    #[cfg(test)]
    pub(crate) fn op_state(&self) -> Rc<RefCell<deno_core::OpState>> {
        self.rt.op_state()
    }

    /// Close the IO scheduler's domain, rejecting all in-flight and future IO.
    /// Call before dropping the runtime during restart to prevent stale async
    /// tasks from executing against an orphaned domain.
    pub fn close_io_scheduler(&self) {
        let op_state_rc = self.rt.op_state();
        let op_state = op_state_rc.borrow();
        op_state
            .borrow::<crate::io_state::IoSchedulerState>()
            .0
            .close();
    }

    fn io_scheduler(&self) -> Arc<migo_io::scheduler::IoScheduler> {
        let op_state_rc = self.rt.op_state();
        let op_state = op_state_rc.borrow();
        Arc::clone(&op_state.borrow::<crate::io_state::IoSchedulerState>().0)
    }

    pub fn reload_bindings(&mut self) {
        let host_id = self.host_id;
        self.with_v8(|rt, bindings| bindings.reload(rt, host_id));
    }

    // ---- JS global calls ----

    pub fn dispatch_touch(
        &mut self,
        touch_type: TouchType,
        points: &[TouchPoint],
        timestamp_ms: i64,
    ) {
        let host_id = self.host_id;
        self.with_v8(|rt, bindings| {
            bindings.dispatch_touch(rt, host_id, touch_type, points, timestamp_ms)
        });
    }

    /// Fire a WebGL context-loss lifecycle event on the main canvas
    /// (`webglcontextlost` / `webglcontextrestored`).
    /// Call one host-bridge hook through the handle resolved at start-up.
    ///
    /// The boundary-safe replacement for handing `core` a string of JavaScript
    /// to evaluate: that string has to name the bridge holder, and the holder
    /// is reachable from content because `Symbol.for` reads the global symbol
    /// registry. Dispatching through a retained handle needs no name.
    pub fn invoke_host_hook(&mut self, hook: &str, args_json: &str) {
        self.with_v8(|rt, bindings| bindings.invoke_host_hook(rt, hook, args_json));
    }

    pub fn dispatch_webgl_context_event(&mut self, kind: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_webgl_context_event(rt, kind));
    }

    /// Forward host focus without conflating it with app visibility or
    /// render/audio pause state.
    pub fn dispatch_focus_changed(&mut self, focused: bool) {
        self.with_v8(|rt, bindings| bindings.dispatch_focus_changed(rt, focused));
    }

    pub fn dispatch_inner_audio_event(&mut self, id: u32, event_type: &str, current_time: f64) {
        let host_id = self.host_id;
        self.with_v8(|rt, bindings| {
            bindings.dispatch_inner_audio_event(rt, host_id, id, event_type, current_time)
        });
    }

    // ---- Sensor event dispatch (zero-alloc, no JS parsing) ----

    #[inline]
    pub fn dispatch_device_motion(&mut self, alpha: f64, beta: f64, gamma: f64) {
        self.with_v8(|rt, bindings| bindings.dispatch_device_motion(rt, alpha, beta, gamma));
    }

    #[inline]
    pub fn dispatch_gyroscope(&mut self, x: f64, y: f64, z: f64) {
        self.with_v8(|rt, bindings| bindings.dispatch_gyroscope(rt, x, y, z));
    }

    #[inline]
    pub fn dispatch_accelerometer(&mut self, x: f64, y: f64, z: f64) {
        self.with_v8(|rt, bindings| bindings.dispatch_accelerometer(rt, x, y, z));
    }

    #[inline]
    pub fn dispatch_compass(&mut self, direction: f64, accuracy: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_compass(rt, direction, accuracy));
    }

    #[inline]
    pub fn dispatch_device_orientation(&mut self, value: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_device_orientation(rt, value));
    }

    #[inline]
    pub fn dispatch_network_status(&mut self, is_connected: bool, network_type: &str) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_network_status(rt, is_connected, network_type)
        });
    }

    // ---- Recorder event dispatch ----

    pub fn dispatch_recorder_event(&mut self, event_type: &str, json_payload: &str) {
        let host_id = self.host_id;
        self.with_v8(|rt, bindings| {
            bindings.dispatch_recorder_event(rt, host_id, event_type, json_payload)
        });
    }

    pub fn dispatch_recorder_frame_data(&mut self, data: Vec<u8>, is_last_frame: bool) {
        let host_id = self.host_id;
        self.with_v8(|rt, bindings| {
            bindings.dispatch_recorder_frame_data(rt, host_id, data, is_last_frame)
        });
    }

    // ---- Camera event dispatch ----

    pub fn dispatch_camera_event(&mut self, camera_id: u32, event_type: &str, json_payload: &str) {
        let host_id = self.host_id;
        self.with_v8(|rt, bindings| {
            bindings.dispatch_camera_event(rt, host_id, camera_id, event_type, json_payload)
        });
    }

    pub fn dispatch_camera_frame_data(
        &mut self,
        camera_id: u32,
        data: Vec<u8>,
        width: u32,
        height: u32,
    ) {
        let host_id = self.host_id;
        self.with_v8(|rt, bindings| {
            bindings.dispatch_camera_frame_data(rt, host_id, camera_id, data, width, height)
        });
    }

    // ---- Bluetooth event dispatch ----

    #[inline]
    pub fn dispatch_bluetooth_adapter_state_change(&mut self, available: bool, discovering: bool) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_bluetooth_adapter_state_change(rt, available, discovering)
        });
    }

    #[inline]
    pub fn dispatch_bluetooth_device_found(&mut self, devices_json: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_bluetooth_device_found(rt, devices_json));
    }

    #[inline]
    pub fn dispatch_beacon_update(&mut self, beacons_json: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_beacon_update(rt, beacons_json));
    }

    #[inline]
    pub fn dispatch_beacon_service_change(&mut self, available: bool, discovering: bool) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_beacon_service_change(rt, available, discovering)
        });
    }

    // ---- BLE GATT event dispatch ----

    #[inline]
    pub fn dispatch_ble_connection_state_change(&mut self, device_id: &str, connected: bool) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_ble_connection_state_change(rt, device_id, connected)
        });
    }

    #[inline]
    pub fn dispatch_ble_characteristic_value_change(
        &mut self,
        device_id: &str,
        service_id: &str,
        characteristic_id: &str,
        value: &[u8],
    ) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_ble_characteristic_value_change(
                rt,
                device_id,
                service_id,
                characteristic_id,
                value,
            )
        });
    }

    #[inline]
    pub fn dispatch_ble_mtu_change(&mut self, device_id: &str, mtu: u32) {
        self.with_v8(|rt, bindings| bindings.dispatch_ble_mtu_change(rt, device_id, mtu));
    }

    // ---- Memory warning dispatch ----

    #[inline]
    pub fn dispatch_memory_warning(&mut self, level: i32) {
        self.with_v8(|rt, bindings| bindings.dispatch_memory_warning(rt, level));
    }

    // ---- Keyboard event dispatch ----

    #[inline]
    pub fn dispatch_keyboard_input(&mut self, value: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_keyboard_input(rt, value));
    }

    #[inline]
    pub fn dispatch_keyboard_height_change(&mut self, height: f64) {
        self.with_v8(|rt, bindings| bindings.dispatch_keyboard_height_change(rt, height));
    }

    #[inline]
    pub fn dispatch_keyboard_confirm(&mut self, value: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_keyboard_confirm(rt, value));
    }

    #[inline]
    pub fn dispatch_keyboard_complete(&mut self, value: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_keyboard_complete(rt, value));
    }

    #[inline]
    pub fn dispatch_composition_start(&mut self, data: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_composition_start(rt, data));
    }

    pub fn dispatch_composition_update(&mut self, data: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_composition_update(rt, data));
    }

    pub fn dispatch_composition_end(&mut self, data: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_composition_end(rt, data));
    }

    pub fn dispatch_gamepad_connected(
        &mut self,
        index: u32,
        id: &str,
        mapping: &str,
        axis_count: u8,
        button_count: u8,
    ) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_gamepad_connected(rt, index, id, mapping, axis_count, button_count)
        });
    }

    pub fn dispatch_gamepad_disconnected(&mut self, index: u32) {
        self.with_v8(|rt, bindings| bindings.dispatch_gamepad_disconnected(rt, index));
    }

    pub fn dispatch_gamepad_state(&mut self, state: &shared::protocol::host_cmd::GamepadState) {
        self.with_v8(|rt, bindings| bindings.dispatch_gamepad_state(rt, state));
    }

    pub fn dispatch_key_down(
        &mut self,
        key: &str,
        code: &str,
        timestamp_ms: f64,
        modifiers: u32,
        repeat: bool,
    ) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_key_down(rt, key, code, timestamp_ms, modifiers, repeat)
        });
    }

    #[inline]
    pub fn dispatch_key_up(
        &mut self,
        key: &str,
        code: &str,
        timestamp_ms: f64,
        modifiers: u32,
        repeat: bool,
    ) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_key_up(rt, key, code, timestamp_ms, modifiers, repeat)
        });
    }

    // ---- Desktop pointer dispatch ----

    #[inline]
    pub fn dispatch_mouse_down(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64) {
        self.with_v8(|rt, bindings| bindings.dispatch_mouse_down(rt, x, y, button, timestamp_ms));
    }

    #[inline]
    pub fn dispatch_mouse_move(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64) {
        self.with_v8(|rt, bindings| bindings.dispatch_mouse_move(rt, x, y, button, timestamp_ms));
    }

    #[inline]
    pub fn dispatch_mouse_up(&mut self, x: f32, y: f32, button: u32, timestamp_ms: f64) {
        self.with_v8(|rt, bindings| bindings.dispatch_mouse_up(rt, x, y, button, timestamp_ms));
    }

    #[inline]
    pub fn dispatch_wheel(
        &mut self,
        delta_x: f64,
        delta_y: f64,
        delta_z: f64,
        delta_mode: u32,
        timestamp_ms: f64,
    ) {
        self.with_v8(|rt, bindings| {
            bindings.dispatch_wheel(rt, delta_x, delta_y, delta_z, delta_mode, timestamp_ms)
        });
    }

    // ---- Video event dispatch ----

    #[inline]
    pub fn dispatch_video_event(&mut self, video_id: u32, event_type: &str, data: &str) {
        self.with_v8(|rt, bindings| bindings.dispatch_video_event(rt, video_id, event_type, data));
    }

    // ---- Scripts / modules ----

    /// Execute a script without pumping the event loop.
    ///
    /// The op-based RAF (op_await_next_frame) creates a permanently-pending op,
    /// so run_event_loop() would block forever once the RAF loop is active.
    /// The main tokio::select! loop in thread.rs continuously drives the event
    /// loop, handling any resulting microtasks, promises, or timers.
    pub fn exec_script(&mut self, name: &'static str, source: &str) -> EngineResult<()> {
        let source = deno_core::FastString::from(source.to_string());
        self.with_v8(|rt, _bindings| rt.execute_script(name, source))
            .map_err(|e| {
                EngineError::new(ErrorCode::JsException)
                    .with_msg(name)
                    .with_detail(e.to_string())
            })?;

        Ok(())
    }

    /// Variant of [`Self::exec_script`] that accepts an owned `name`.
    ///
    /// Use this when the script name is built at runtime (e.g. boot prelude
    /// scripts whose names come from `InitOptions`). For static call sites
    /// prefer [`Self::exec_script`] to avoid the allocation.
    pub fn exec_script_owned(&mut self, name: String, source: &str) -> EngineResult<()> {
        let source = deno_core::FastString::from(source.to_string());
        let name_for_err = name.clone();
        self.with_v8(|rt, _bindings| rt.execute_script(name, source))
            .map_err(|e| {
                EngineError::new(ErrorCode::JsException)
                    .with_msg(name_for_err)
                    .with_detail(e.to_string())
            })?;

        Ok(())
    }

    /// Evaluate a module with VFS sandboxing.
    ///
    /// Creates game-specific paths from the game_id and base directories,
    /// then sets up the VFS for sandboxed file access.
    ///
    /// # Arguments
    /// * `game_id` - Unique game identifier (1-64 lower-case alphanumeric, underscore, hyphen)
    /// * `entry` - Entry point file (e.g., "main.js")
    pub async fn evaluate_module(&mut self, game_id: String, entry: String) -> EngineResult<()> {
        let t_eval = Instant::now();
        // Get base directories from HostOpState
        let (files_dir, cache_dir) = self.get_base_dirs();

        // Create GamePaths from base dirs + game_id
        let game_paths =
            GamePaths::new(&files_dir, &cache_dir, &game_id, self.host_id).map_err(|e| {
                EngineError::new(ErrorCode::InvalidArgument)
                    .with_msg("create game paths")
                    .with_detail(e.to_string())
            })?;

        // Ensure directories exist
        game_paths.ensure_directories().map_err(|e| {
            EngineError::new(ErrorCode::IoError)
                .with_msg("create game directories")
                .with_detail(e.to_string())
        })?;

        // Pin every sandbox root before exposing the VFS to untrusted code.
        // A path-only VFS would allow the mapping root itself to be replaced
        // between construction and a later read.
        let vfs = VirtualFS::try_from_game_paths(&game_paths).map_err(|_| {
            EngineError::new(ErrorCode::IoError)
                .with_msg("initialize sandbox filesystem")
                .with_detail("failed to pin sandbox roots")
        })?;
        let code_dir = game_paths.code_dir().to_path_buf();
        let code_dir_str = code_dir.to_string_lossy().into_owned();
        let game_paths = Arc::new(game_paths);

        // ---- Code signing verification (before loading any JS) ----
        #[cfg(feature = "code-signing")]
        if self.code_signing_enabled {
            if let Some(err) = &self.code_signing_error {
                return Err(err.clone());
            }
            let verifier = self
                .integrity_verifier
                .as_ref()
                .ok_or_else(|| {
                    EngineError::new(ErrorCode::CodeSignatureInvalid)
                        .with_msg("code signing verifier is not initialized")
                })?
                .clone();
            let receipt_path = game_paths.integrity_receipt_path();
            tracing::info!(
                "[Host {}] checking code integrity receipt for entry '{}'",
                self.host_id,
                entry
            );
            let receipt_hit = verifier
                .verify_launch_receipt(&code_dir, &receipt_path, &entry)
                .map_err(|error| {
                    tracing::error!(
                        "[Host {}] signed manifest verification failed: {}",
                        self.host_id,
                        error
                    );
                    error
                })?;
            let verified = if let Some(verified) = receipt_hit {
                verified
            } else {
                let scheduler = self.io_scheduler();
                let verify_code_dir = code_dir.clone();
                let verify_receipt_path = receipt_path.clone();
                let verify_entry = entry.clone();
                scheduler
                    .run_package_verification(
                        verify_receipt_path.clone(),
                        PriorityClass::ForegroundBlocking,
                        move || {
                            verifier.verify_and_promote_for_launch(
                                &verify_code_dir,
                                &verify_receipt_path,
                                &verify_entry,
                            )
                        },
                    )
                    .await
                    .map_err(EngineError::from)?
                    .map_err(|error| {
                        tracing::error!(
                            "[Host {}] full package integrity verification failed: {}",
                            self.host_id,
                            error
                        );
                        error
                    })?
            };
            tracing::info!(
                "[Host {}] code integrity verification passed (mode={:?}, generation={}, hashed_files={})",
                self.host_id,
                verified.mode,
                verified.generation,
                verified.files_hashed
            );
        }

        // Create MountTable for /code path resolution.
        let mount_table = Arc::new(MountTable::new(code_dir.clone()));

        // Restore previously installed subpackages from the per-game manifest.
        // This makes preDownloadSubpackage results survive across sessions.
        // Skipped when code signing is enabled (downloaded packages lack signatures).
        #[cfg(feature = "code-signing")]
        let cs_enabled = self.code_signing_enabled;
        #[cfg(not(feature = "code-signing"))]
        let cs_enabled = false;
        shared::vfs::mount::restore_installed_packages(
            &mount_table,
            game_paths.cache_dir(),
            cs_enabled,
        );

        // Bound the derived texture cache. It gains a sidecar on every decode
        // miss and nothing else caps the directory, so its budget only holds
        // if someone prunes at session start. Fire-and-forget on the
        // background lane — launch does not wait on a directory scan.
        migo_io::schedule_derived_cache_prune(&self.io_scheduler(), game_paths.cache_dir());

        // Store paths, VFS, and mount table in op state.
        self.set_game_paths(Some(game_paths));
        self.set_vfs(Some(Arc::new(vfs)));
        self.set_mount_table(Some(mount_table));
        self.set_code_dir(Some(code_dir_str));

        // Resolve and load module
        let resolved = resolve_path(&entry, &code_dir).map_err(|e| {
            EngineError::new(ErrorCode::InvalidArgument)
                .with_msg("resolve module path")
                .with_detail(e.to_string())
        })?;

        // Guard every poll of module loading: V8 parse/compile of untrusted code
        // counts against the budget. Scope the load future so its `&mut self.rt`
        // borrow is released before `mod_evaluate` below. `wd` reads the disjoint
        // `self.watchdog` field so both borrows coexist.
        let module_id = {
            #[cfg(feature = "v8-limits")]
            let wd = self.watchdog.as_ref();
            #[cfg(not(feature = "v8-limits"))]
            let wd: Option<&crate::watchdog::DeadlineWatchdog> = None;
            let mut load = std::pin::pin!(self.rt.load_main_es_module(&resolved));
            std::future::poll_fn(|cx| crate::watchdog::poll_guarded(wd, load.as_mut(), cx)).await
        }
        .map_err(|e| {
            EngineError::new(ErrorCode::ModuleLoadError)
                .with_msg("load main es module")
                .with_detail(e.to_string())
        })?;

        let t_mod_loaded = Instant::now();
        tracing::info!(
            "[Host {}] module loaded: {:.1}ms",
            self.host_id,
            t_mod_loaded.duration_since(t_eval).as_secs_f64() * 1000.0,
        );

        // Keep pumping the event loop until module evaluation actually resolves.
        //
        // `run_event_loop()` may return early while module evaluation is still pending
        // (for example with top-level await waiting on later callbacks). Returning on
        // the first `run_event_loop()` completion would leave the module half-initialized.
        //
        // Guard the SYNCHRONOUS `mod_evaluate` call: deno_core 0.385 enters V8 to
        // run the module top-level while constructing the returned future, so a
        // top-level `while (true) {}` must be covered here, not only in later polls.
        let evaluation_fut = {
            #[cfg(feature = "v8-limits")]
            let _scope = self.watchdog.as_ref().map(DeadlineWatchdog::enter);
            self.rt.mod_evaluate(module_id)
        };
        let mut evaluation = std::pin::pin!(evaluation_fut);

        // Drive evaluation and the event loop, arming immediately before each poll
        // and disarming when the combined poll returns. Never hold a guard across
        // the pending await.
        #[cfg(feature = "v8-limits")]
        let wd = self.watchdog.as_ref();
        #[cfg(not(feature = "v8-limits"))]
        let wd: Option<&crate::watchdog::DeadlineWatchdog> = None;
        let rt = &mut self.rt;
        std::future::poll_fn(move |cx| -> std::task::Poll<EngineResult<()>> {
            let _scope = wd.map(crate::watchdog::DeadlineWatchdog::enter);
            if let std::task::Poll::Ready(result) = evaluation.as_mut().poll(cx) {
                return std::task::Poll::Ready(result.map_err(|e| {
                    EngineError::new(ErrorCode::ModuleLoadError)
                        .with_msg("load main es module")
                        .with_detail(e.to_string())
                }));
            }
            match rt.poll_event_loop(cx, PollEventLoopOptions::default()) {
                std::task::Poll::Ready(Err(e)) => {
                    std::task::Poll::Ready(Err(EngineError::new(ErrorCode::JsException)
                        .with_msg("event loop error during module evaluation")
                        .with_detail(e.to_string())))
                }
                // Idle or pending: park. Evaluation's waker fires when the module
                // settles (its receiver is woken by the event loop), so a normal
                // module makes progress and a genuinely stuck one parks instead of
                // busy-spinning.
                _ => std::task::Poll::Pending,
            }
        })
        .await?;
        tracing::info!(
            "[Host {}] module evaluated: {:.1}ms (total evaluate_module={:.1}ms)",
            self.host_id,
            t_mod_loaded.elapsed().as_secs_f64() * 1000.0,
            t_eval.elapsed().as_secs_f64() * 1000.0,
        );
        Ok(())
    }

    /// Backend-neutral event-loop pump. Wraps `run_event_loop` with default
    /// poll options so non-V8 callers (the core host thread) never name
    /// `PollEventLoopOptions`.
    pub async fn pump_event_loop(&mut self) -> EngineResult<()> {
        self.run_event_loop(PollEventLoopOptions::default()).await
    }

    /// Run one drive of the event loop (used by host thread main loop).
    ///
    /// Implemented with `poll_fn` around `JsRuntime::poll_event_loop` so each poll
    /// is guarded (armed on entry, disarmed on return). It must NOT arm across the
    /// whole `JsRuntime::run_event_loop().await`, which may legitimately stay
    /// pending indefinitely (e.g. the op-based RAF loop), which would otherwise
    /// charge async wait time as JS execution.
    pub async fn run_event_loop(&mut self, opt: PollEventLoopOptions) -> EngineResult<()> {
        #[cfg(feature = "v8-limits")]
        let wd = self.watchdog.as_ref();
        #[cfg(not(feature = "v8-limits"))]
        let wd: Option<&crate::watchdog::DeadlineWatchdog> = None;
        let rt = &mut self.rt;
        std::future::poll_fn(move |cx| {
            let _scope = wd.map(crate::watchdog::DeadlineWatchdog::enter);
            rt.poll_event_loop(cx, opt)
        })
        .await
        .map_err(|e| {
            EngineError::new(ErrorCode::JsException)
                .with_msg("run_event_loop")
                .with_detail(e.to_string())
        })
    }
}
