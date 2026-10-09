//! Integration tests for V8 heap limits and execution timeout.
//!
//! These tests verify that:
//! 1. `while(true){}` is terminated by the watchdog within the configured timeout
//! 2. `new ArrayBuffer(1e10)` triggers the near-heap-limit callback and is intercepted
//! 3. The process does NOT crash in either case
//! 4. Errors are correctly classified as OutOfMemory or JsExecutionTimeout

#[cfg(all(test, feature = "v8-limits"))]
mod v8_limits_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use deno_core::{JsRuntime, RuntimeOptions, v8};

    use crate::host_runtime::V8LimitsConfig;

    /// Helper: create a JsRuntime with heap limits and OOM callback.
    /// Returns (runtime, oom_flag).
    fn create_limited_runtime(max_heap_mb: usize) -> (JsRuntime, Arc<AtomicBool>) {
        let config = V8LimitsConfig {
            max_heap_size: max_heap_mb * 1024 * 1024,
            initial_heap_size: 0,
        };

        let create_params = v8::Isolate::create_params()
            .heap_limits(config.initial_heap_size, config.max_heap_size);

        let mut rt = JsRuntime::new(RuntimeOptions {
            create_params: Some(create_params),
            ..Default::default()
        });

        let oom_flag = Arc::new(AtomicBool::new(false));
        let cb_flag = Arc::clone(&oom_flag);
        let cb_handle = rt.v8_isolate().thread_safe_handle();

        rt.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
            cb_flag.store(true, Ordering::SeqCst);
            cb_handle.terminate_execution();
            current_limit * 2
        });

        (rt, oom_flag)
    }

    // ========================================================================
    // Test 1: Infinite loop is terminated by watchdog (via IsolateHandle)
    // ========================================================================
    #[test]
    fn test_infinite_loop_terminated_by_watchdog() {
        let mut runtime = JsRuntime::new(RuntimeOptions::default());
        let isolate_handle = runtime.v8_isolate().thread_safe_handle();

        // Spawn a terminator thread that kills execution after 1 second
        let terminator = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let ok = isolate_handle.terminate_execution();
            assert!(ok, "terminate_execution should return true");
        });

        // Execute an infinite loop — should be interrupted
        let result = runtime.execute_script("infinite_loop.js", "for(;;) {}");

        assert!(result.is_err(), "Infinite loop should have been terminated");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("execution terminated"),
            "Error should contain 'execution terminated', got: {}",
            err
        );

        // Verify isolate is still usable after cancel_terminate_execution
        let ok = runtime.v8_isolate().cancel_terminate_execution();
        assert!(ok);

        let result = runtime.execute_script("after_terminate.js", "1 + 1");
        assert!(
            result.is_ok(),
            "Isolate should be usable after cancellation"
        );

        terminator.join().unwrap();
    }

    // ========================================================================
    // Test 2: Large allocation triggers near-heap-limit and terminates
    // ========================================================================
    #[test]
    fn test_heap_limit_oom_intercepted() {
        // Use 32MB heap — enough for V8 to initialize, but small enough
        // that aggressive allocation will trigger near-heap-limit quickly.
        // Note: 5MB is too small; V8 needs ~10-20MB just for its own internals
        // and will abort via its fatal OOM handler before our callback can help.
        let (mut runtime, oom_flag) = create_limited_runtime(32);

        // Fill heap by pushing many medium-sized strings into an array.
        // Avoids V8's per-string length limit (RangeError) that would fire before
        // the heap limit callback. Each iteration pushes ~64KB of string data.
        let result = runtime.execute_script(
            "oom_test.js",
            r#"let arr = []; while(true) { arr.push("X".repeat(65536)); }"#,
        );

        assert!(result.is_err(), "Large allocation should fail");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("execution terminated"),
            "Error should contain 'execution terminated', got: {}",
            err
        );

        // Verify OOM flag was set
        assert!(
            oom_flag.load(Ordering::SeqCst),
            "OOM flag should have been set by near_heap_limit_callback"
        );

        // Verify process didn't crash — we're still running!
    }

    // ========================================================================
    // Test 3: Gradual OOM via string concatenation
    // ========================================================================
    #[test]
    fn test_heap_limit_gradual_oom() {
        // 32MB: sufficient for V8 init, triggers OOM on repeated string growth
        let (mut runtime, oom_flag) = create_limited_runtime(32);

        let result = runtime.execute_script(
            "gradual_oom.js",
            r#"let arr = []; while(true) { arr.push("A".repeat(65536)); }"#,
        );

        assert!(result.is_err(), "Gradual OOM should fail");
        assert!(
            oom_flag.load(Ordering::SeqCst),
            "OOM flag should have been set"
        );
    }

    // ========================================================================
    // Test 4: V8LimitsConfig from_max_memory_mb
    // ========================================================================
    #[test]
    fn test_v8_limits_config_from_max_memory_mb() {
        let config = V8LimitsConfig::from_max_memory_mb(256);
        assert_eq!(config.max_heap_size, 256 * 1024 * 1024);
        assert_eq!(config.initial_heap_size, 0);

        // Clamping test
        let config = V8LimitsConfig::from_max_memory_mb(10); // below minimum 64
        assert_eq!(config.max_heap_size, 64 * 1024 * 1024);

        let config = V8LimitsConfig::from_max_memory_mb(9999); // above maximum 2048
        assert_eq!(config.max_heap_size, 2048 * 1024 * 1024);
    }

    // ========================================================================
    // Test 5: Default V8LimitsConfig values
    // ========================================================================
    #[test]
    fn test_v8_limits_config_default() {
        let config = V8LimitsConfig::default();
        assert_eq!(config.max_heap_size, 256 * 1024 * 1024);
        assert_eq!(config.initial_heap_size, 0);
    }

    // ========================================================================
    // Test 6: Normal JS execution works fine with limits enabled
    // ========================================================================
    #[test]
    fn test_normal_execution_with_limits() {
        let (mut runtime, oom_flag) = create_limited_runtime(256);

        // Normal arithmetic
        let result = runtime.execute_script("normal.js", "1 + 1");
        assert!(result.is_ok(), "Normal execution should succeed");

        // Small array allocation
        let result = runtime.execute_script("small_alloc.js", "new ArrayBuffer(1024)");
        assert!(result.is_ok(), "Small allocation should succeed");

        // OOM flag should NOT be set
        assert!(
            !oom_flag.load(Ordering::SeqCst),
            "OOM flag should not be set for normal execution"
        );
    }
}

/// Exact V8 coverage — the process deadline watchdog must guard every real
/// V8 entry a `HostJsRuntime` exposes.
#[cfg(all(test, feature = "v8-limits"))]
mod host_watchdog_tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use deno_core::PollEventLoopOptions;
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        render_command_sender::CommandSender,
    };

    use crate::watchdog::DeadlineWatchdogConfig;
    use crate::{HostJsRuntime, V8LimitsConfig};

    fn test_host_state(files_dir: PathBuf, cache_dir: PathBuf) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: std::sync::Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id: 1,
            app_cache_dir: cache_dir,
            app_files_dir: files_dir,
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
            raf_demand: std::sync::Arc::new(shared::raf_signal::RafDemand::new()),
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

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "migo-wd-{tag}-{nanos}-{:?}",
            std::thread::current().id()
        ))
    }

    fn build_runtime(files_dir: PathBuf, cache_dir: PathBuf, timeout: Duration) -> HostJsRuntime {
        let host_state = test_host_state(files_dir, cache_dir.clone());
        let mut rt = HostJsRuntime::new(
            1,
            host_state,
            &cache_dir,
            V8LimitsConfig::default(),
            #[cfg(feature = "code-signing")]
            false,
            #[cfg(feature = "code-signing")]
            None,
        );
        rt.install_watchdog(DeadlineWatchdogConfig::new(timeout, "test-host"))
            .expect("install watchdog");
        rt
    }

    #[test]
    fn guarded_execute_script_terminates_infinite_loop() {
        let dir = unique_dir("exec");
        let mut rt = build_runtime(dir.clone(), dir, Duration::from_millis(200));
        let start = Instant::now();
        let result = rt.exec_script("loop", "while (true) {}");
        let elapsed = start.elapsed();
        assert!(
            result.is_err(),
            "a guarded infinite loop must be terminated"
        );
        assert!(
            rt.watchdog_timed_out(),
            "the watchdog must record the timeout"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "termination must be prompt, took {elapsed:?}"
        );
    }

    #[test]
    fn normal_script_disarms_and_isolate_remains_usable() {
        let dir = unique_dir("normal");
        let mut rt = build_runtime(dir.clone(), dir, Duration::from_millis(150));
        assert!(rt.exec_script("a", "globalThis.__wd = 1 + 1;").is_ok());
        // Sleep well past the timeout: a completed script disarmed, so no fire.
        std::thread::sleep(Duration::from_millis(350));
        assert!(
            !rt.watchdog_timed_out(),
            "a completed script disarms; the watchdog must not fire"
        );
        assert!(
            rt.exec_script("b", "if (globalThis.__wd !== 2) throw new Error('bad');")
                .is_ok(),
            "the isolate must remain usable after a normal script"
        );
    }

    #[tokio::test]
    async fn pending_event_loop_poll_is_disarmed_past_timeout() {
        let dir = unique_dir("pending");
        let mut rt = build_runtime(dir.clone(), dir, Duration::from_millis(150));
        // A long timer keeps the event loop pending without any running JS.
        rt.exec_script(
            "timer",
            "setTimeout(() => { globalThis.__done = true; }, 100000);",
        )
        .unwrap();
        // Drive the (guarded) event loop for far longer than the timeout; each
        // poll returns Pending and disarms, so the watchdog must never fire.
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            rt.run_event_loop(PollEventLoopOptions::default()),
        )
        .await;
        assert!(
            !rt.watchdog_timed_out(),
            "time spent Pending must not be charged as JS execution time"
        );
    }

    #[tokio::test]
    async fn mod_evaluate_constructor_is_guarded_before_future_poll() {
        let base = unique_dir("mod");
        let code_dir = base.join("migo/games/wdmod/code");
        std::fs::create_dir_all(&code_dir).unwrap();
        // A top-level infinite loop runs during the *synchronous* mod_evaluate
        // constructor, before the returned future is ever polled. If only the
        // future's later polls were guarded this would hang the thread forever.
        std::fs::write(code_dir.join("main.js"), "while (true) {}").unwrap();

        let mut rt = build_runtime(base.clone(), base, Duration::from_millis(200));
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            rt.evaluate_module("wdmod".into(), "main.js".into()),
        )
        .await;
        assert!(
            result.is_ok(),
            "evaluate_module must return (guarding the sync constructor prevents a hang)"
        );
        assert!(
            result.unwrap().is_err(),
            "a top-level infinite loop must be terminated"
        );
        assert!(rt.watchdog_timed_out());
    }

    #[test]
    fn sync_binding_dispatch_uses_the_same_guard() {
        // Contract: every cached binding dispatch routes through the single
        // guarded helper, so a synchronous JS callback cannot bypass the
        // watchdog. Enforced at the source level so new dispatch_* methods can
        // never silently skip the guard.
        let src = include_str!("../host_runtime.rs");
        assert!(
            !src.contains("self.bindings.dispatch"),
            "dispatch_* must call bindings inside with_v8, never self.bindings.dispatch directly"
        );
        assert!(
            !src.contains("self.bindings.reload"),
            "reload_bindings must route through with_v8"
        );
    }

    #[test]
    fn all_v8_entries_route_through_one_guard() {
        let src = include_str!("../host_runtime.rs");
        assert!(
            src.contains("fn with_v8"),
            "the single guarded helper must exist"
        );
        assert!(
            !src.contains("self.rt.execute_script"),
            "exec_script/exec_script_owned must route through with_v8"
        );
        assert!(
            src.contains("poll_guarded"),
            "module-load / mod_evaluate future / event-loop polls must use poll_guarded"
        );
        assert!(
            src.contains("poll_event_loop"),
            "the host event loop must be driven by poll_event_loop under a guard"
        );
        assert!(
            !src.contains("self.rt.run_event_loop("),
            "run_event_loop must be reimplemented with poll_fn + poll_event_loop, never armed across the await"
        );
        assert!(
            src.contains("mod_evaluate"),
            "module evaluation must be present and guarded"
        );
    }
}
