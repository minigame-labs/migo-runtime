//! Full-runtime tests for logical timer lifecycle and quota semantics.

#[cfg(test)]
mod timer_tests {
    use std::{
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
        time::Duration,
    };

    use deno_core::{FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions};
    use futures::future::poll_fn;
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        network_policy::NetworkPolicy,
        op_state::{AudioSender, HostOpState},
        render_command_sender::CommandSender,
    };

    deno_core::extension!(
        timer_test_bridge,
        deps = [host_v8_web],
        esm_entry_point = "ext:timer_test_bridge/bridge.js",
        esm = [dir "src/tests", "timer_heap_prototype.js", "ext:timer_test_bridge/bridge.js" = {
            source = r#"
                    import * as timers from "ext:host_v8_web/02_timers.js";
                    import { core } from "ext:core/mod.js";
                    import { op_now } from "ext:core/ops";
                    import { installHeapPrototype } from "ext:timer_test_bridge/timer_heap_prototype.js";
                    globalThis.__timerInternals = timers;
                    globalThis.__coreInternals = timers.__testTimerCore ?? core;
                    globalThis.__timerUseHeapPrototype = () => {
                        const bytes = new Uint8Array(8);
                        const words = new Uint32Array(bytes.buffer);
                        installHeapPrototype(timers.__testTimerCore ?? core, () => {
                            op_now(bytes);
                            return words[0] * 1000000 + Math.floor(words[1] / 1000);
                        });
                    };
                "#
        },],
    );

    fn test_host_state(timer_backgrounded: bool) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: std::sync::Arc::new(shared::callback_id::CallbackIdAllocator::default()),
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
            raf_demand: std::sync::Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(timer_backgrounded)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    fn boot_runtime() -> JsRuntime {
        let mut extensions = crate::main_extensions(test_host_state(false));
        extensions.push(timer_test_bridge::init());
        prepare_selected_prototype(&mut extensions);
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });
        install_selected_prototype(&mut rt);
        crate::harden_global_scope(&mut rt);
        rt
    }

    fn boot_hidden_runtime() -> JsRuntime {
        let mut extensions = crate::main_extensions(test_host_state(true));
        extensions.push(timer_test_bridge::init());
        prepare_selected_prototype(&mut extensions);
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });
        install_selected_prototype(&mut rt);
        crate::harden_global_scope(&mut rt);
        rt
    }

    fn boot_runtime_with_timer_internals() -> JsRuntime {
        let mut extensions = crate::main_extensions(test_host_state(false));
        extensions.push(timer_test_bridge::init());
        prepare_selected_prototype(&mut extensions);
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });
        install_selected_prototype(&mut rt);
        crate::harden_global_scope(&mut rt);
        rt
    }

    fn install_selected_prototype(rt: &mut JsRuntime) {
        if std::env::var("MIGO_TIMER_PROTOTYPE").as_deref() == Ok("heap") {
            exec(rt, "globalThis.__timerUseHeapPrototype()");
        }
    }

    fn prepare_selected_prototype(extensions: &mut [deno_core::Extension]) {
        if std::env::var("MIGO_TIMER_PROTOTYPE").as_deref() != Ok("heap") {
            return;
        }
        // Core is frozen. Substitute a test-only facade in the exact same
        // policy source, rather than weakening production core hardening.
        let extension = extensions
            .iter_mut()
            .find(|ext| ext.name == "host_v8_web")
            .unwrap();
        let source = extension
            .esm_files
            .to_mut()
            .iter_mut()
            .find(|file| file.specifier == "ext:host_v8_web/02_timers.js")
            .unwrap();
        let original = include_str!("../web/02_timers.js");
        let import = "import { core, primordials } from \"ext:core/mod.js\";";
        assert_eq!(original.matches(import).count(), 1);
        let mut modified = original.replacen(import,
            "import { core as upstreamCore, primordials } from \"ext:core/mod.js\";\nconst core = { ...upstreamCore };", 1);
        modified.push_str("\nexport { core as __testTimerCore };\n");
        source.code = deno_core::ExtensionFileSourceCode::Computed(modified.into());
    }

    fn exec(rt: &mut JsRuntime, source: impl Into<String>) {
        rt.execute_script("<test:timer>", FastString::from(source.into()))
            .expect("timer script");
    }

    fn assert_js(rt: &mut JsRuntime, expression: &str) {
        exec(
            rt,
            format!(
                "if (!({expression})) throw new Error('timer assertion failed: ' + ({expression}));"
            ),
        );
    }

    async fn poll_once(rt: &mut JsRuntime) {
        poll_fn(|cx| {
            let _ = rt.poll_event_loop(cx, PollEventLoopOptions::default());
            std::task::Poll::Ready(())
        })
        .await;
    }

    async fn drain_ready(rt: &mut JsRuntime) {
        for _ in 0..4 {
            poll_once(rt).await;
            tokio::task::yield_now().await;
        }
    }

    async fn advance_and_drain(rt: &mut JsRuntime, duration: Duration) {
        poll_once(rt).await;
        tokio::time::advance(duration).await;
        // deno_core 0.385 treats a timer exactly equal to `Instant::now()` as
        // the next batch; cross the boundary without adding a visible ms.
        tokio::time::advance(Duration::from_nanos(1)).await;
        tokio::task::yield_now().await;
        drain_ready(rt).await;
    }

    fn hide(rt: &mut JsRuntime) {
        exec(
            rt,
            "globalThis[Symbol.for('Migo.hostBridge')]._internalTriggerOnHide()",
        );
    }

    fn show(rt: &mut JsRuntime) {
        exec(
            rt,
            "globalThis[Symbol.for('Migo.hostBridge')]._internalTriggerOnShow()",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_freezes_its_remaining_delay_while_hidden() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            "globalThis.__timerCount = 0; setTimeout(() => __timerCount++, 100)",
        );

        advance_and_drain(&mut rt, Duration::from_millis(30)).await;
        hide(&mut rt);
        advance_and_drain(&mut rt, Duration::from_secs(10)).await;
        assert_js(&mut rt, "__timerCount === 0");

        show(&mut rt);
        assert_js(&mut rt, "__timerCount === 0");
        advance_and_drain(&mut rt, Duration::from_millis(69)).await;
        assert_js(&mut rt, "__timerCount === 0");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__timerCount === 1");
    }

    #[tokio::test(start_paused = true)]
    async fn interval_keeps_remainder_then_original_period_without_catch_up() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            "globalThis.__intervalCount = 0; \
             globalThis.__intervalId = setInterval(() => { \
               __intervalCount++; \
               if (__intervalCount === 2) clearInterval(__intervalId); \
             }, 100)",
        );

        advance_and_drain(&mut rt, Duration::from_millis(30)).await;
        hide(&mut rt);
        advance_and_drain(&mut rt, Duration::from_secs(5)).await;
        assert_js(&mut rt, "__intervalCount === 0");

        show(&mut rt);
        advance_and_drain(&mut rt, Duration::from_millis(70)).await;
        assert_js(&mut rt, "__intervalCount === 1");
        advance_and_drain(&mut rt, Duration::from_millis(99)).await;
        assert_js(&mut rt, "__intervalCount === 1");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__intervalCount === 2");
    }

    #[tokio::test(start_paused = true)]
    async fn clear_while_hidden_prevents_timeout_resurrection() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            "globalThis.__fired = false; globalThis.__id = setTimeout(() => __fired = true, 50)",
        );
        hide(&mut rt);
        exec(&mut rt, "clearTimeout(__id)");
        show(&mut rt);
        advance_and_drain(&mut rt, Duration::from_secs(1)).await;
        assert_js(&mut rt, "__fired === false");
    }

    #[tokio::test(start_paused = true)]
    async fn timer_created_hidden_gets_its_full_delay_after_show() {
        let mut rt = boot_runtime();
        hide(&mut rt);
        exec(
            &mut rt,
            "globalThis.__fired = false; setTimeout(() => __fired = true, 40)",
        );
        advance_and_drain(&mut rt, Duration::from_secs(2)).await;
        show(&mut rt);
        assert_js(&mut rt, "__fired === false");
        advance_and_drain(&mut rt, Duration::from_millis(39)).await;
        assert_js(&mut rt, "__fired === false");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__fired === true");
    }

    #[tokio::test(start_paused = true)]
    async fn interval_created_hidden_starts_full_period_then_repeats_normally() {
        let mut rt = boot_runtime();
        hide(&mut rt);
        exec(
            &mut rt,
            "globalThis.__hiddenIntervalCount = 0; \
             globalThis.__hiddenIntervalId = setInterval(() => { \
               __hiddenIntervalCount++; \
               if (__hiddenIntervalCount === 2) clearInterval(__hiddenIntervalId); \
             }, 40)",
        );
        advance_and_drain(&mut rt, Duration::from_secs(2)).await;
        assert_js(&mut rt, "__hiddenIntervalCount === 0");

        show(&mut rt);
        advance_and_drain(&mut rt, Duration::from_millis(39)).await;
        assert_js(&mut rt, "__hiddenIntervalCount === 0");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__hiddenIntervalCount === 1");
        advance_and_drain(&mut rt, Duration::from_millis(40)).await;
        assert_js(&mut rt, "__hiddenIntervalCount === 2");
    }

    #[tokio::test(start_paused = true)]
    async fn interval_remainder_survives_two_hide_show_cycles() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            "globalThis.__cycledIntervalCount = 0; \
             globalThis.__cycledIntervalId = setInterval(() => { \
               __cycledIntervalCount++; \
               clearInterval(__cycledIntervalId); \
             }, 100)",
        );

        advance_and_drain(&mut rt, Duration::from_millis(30)).await;
        hide(&mut rt);
        advance_and_drain(&mut rt, Duration::from_secs(1)).await;
        show(&mut rt);
        advance_and_drain(&mut rt, Duration::from_millis(20)).await;
        hide(&mut rt);
        advance_and_drain(&mut rt, Duration::from_secs(1)).await;
        show(&mut rt);

        advance_and_drain(&mut rt, Duration::from_millis(49)).await;
        assert_js(&mut rt, "__cycledIntervalCount === 0");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__cycledIntervalCount === 1");
    }

    #[tokio::test(start_paused = true)]
    async fn unref_state_survives_freeze_and_rearm() {
        let mut rt = boot_runtime_with_timer_internals();
        exec(
            &mut rt,
            "globalThis.__unrefId = setTimeout(() => {}, 1000); \
             __timerInternals.unrefTimer(__unrefId)",
        );
        assert_js(&mut rt, "__coreInternals.eventLoopHasMoreWork() === false");

        hide(&mut rt);
        show(&mut rt);
        assert_js(&mut rt, "__coreInternals.eventLoopHasMoreWork() === false");

        exec(&mut rt, "__timerInternals.refTimer(__unrefId)");
        assert_js(&mut rt, "__coreInternals.eventLoopHasMoreWork() === true");
        exec(&mut rt, "clearTimeout(__unrefId)");
        assert_js(&mut rt, "__coreInternals.eventLoopHasMoreWork() === false");
    }

    #[tokio::test(start_paused = true)]
    async fn async_context_survives_freeze_and_is_restored_after_callback() {
        let mut rt = boot_runtime_with_timer_internals();
        exec(
            &mut rt,
            "globalThis.__contextMarker = {}; \
             globalThis.__contextBefore = __coreInternals.getAsyncContext(); \
             globalThis.__contextMatched = false; \
             __coreInternals.setAsyncContext(__contextMarker); \
             setTimeout(() => { \
               __contextMatched = __coreInternals.getAsyncContext() === __contextMarker; \
             }, 20); \
             __coreInternals.setAsyncContext(__contextBefore)",
        );

        advance_and_drain(&mut rt, Duration::from_millis(5)).await;
        hide(&mut rt);
        advance_and_drain(&mut rt, Duration::from_secs(1)).await;
        show(&mut rt);
        advance_and_drain(&mut rt, Duration::from_millis(15)).await;
        assert_js(
            &mut rt,
            "__contextMatched === true && \
             __coreInternals.getAsyncContext() === __contextBefore",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn first_timer_observes_an_initially_hidden_runtime() {
        let mut rt = boot_hidden_runtime();
        exec(
            &mut rt,
            "globalThis.__fired = false; setTimeout(() => __fired = true, 25)",
        );
        advance_and_drain(&mut rt, Duration::from_secs(2)).await;
        assert_js(&mut rt, "__fired === false");

        show(&mut rt);
        advance_and_drain(&mut rt, Duration::from_millis(24)).await;
        assert_js(&mut rt, "__fired === false");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__fired === true");
    }

    /// A timer restored the moment its runtime is shown fires on its own
    /// millisecond wherever the clock stands: the deadline is not worked out in
    /// fractional milliseconds, where `start + 25 - start` can exceed 25 and
    /// the native timer's whole milliseconds round it up to 26.
    #[tokio::test(start_paused = true)]
    async fn restored_timers_fire_on_their_millisecond_wherever_the_clock_stands() {
        let mut rt = boot_runtime();
        for _ in 0..200 {
            // A clock that stands somewhere else each time, between milliseconds.
            advance_and_drain(&mut rt, Duration::from_micros(1_337)).await;
            hide(&mut rt);
            exec(
                &mut rt,
                "globalThis.__fired = false; setTimeout(() => __fired = true, 25)",
            );
            show(&mut rt);
            advance_and_drain(&mut rt, Duration::from_millis(24)).await;
            assert_js(&mut rt, "__fired === false");
            advance_and_drain(&mut rt, Duration::from_millis(1)).await;
            assert_js(&mut rt, "__fired === true");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn equal_deadline_timeouts_keep_creation_order() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            "globalThis.__order = []; \
             setTimeout(() => __order.push('a'), 20); \
             setTimeout(() => __order.push('b'), 20)",
        );
        hide(&mut rt);
        show(&mut rt);
        advance_and_drain(&mut rt, Duration::from_millis(20)).await;
        exec(
            &mut rt,
            "if (JSON.stringify(__order) !== '[\"a\",\"b\"]') \
             throw new Error('order=' + JSON.stringify(__order))",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn live_timer_quota_is_released_by_clear() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            "globalThis.__ids = []; globalThis.__quotaError = false; \
             for (let i = 0; i < 1024; i++) __ids.push(setTimeout(() => {}, 1000000)); \
             try { setTimeout(() => {}, 1000000); } \
             catch (e) { __quotaError = e instanceof RangeError; }",
        );
        assert_js(&mut rt, "__quotaError === true");
        exec(
            &mut rt,
            "clearTimeout(__ids.pop()); globalThis.__replacement = setTimeout(() => {}, 1000000)",
        );
        assert_js(&mut rt, "typeof __replacement === 'number'");
    }

    #[tokio::test(start_paused = true)]
    async fn set_immediate_is_a_managed_hidden_one_shot() {
        let mut rt = boot_runtime();
        hide(&mut rt);
        exec(
            &mut rt,
            "globalThis.__immediate = 0; setImmediate(() => __immediate++)",
        );
        drain_ready(&mut rt).await;
        assert_js(&mut rt, "__immediate === 0");
        show(&mut rt);
        advance_and_drain(&mut rt, Duration::ZERO).await;
        assert_js(&mut rt, "__immediate === 1");
    }

    #[tokio::test(start_paused = true)]
    async fn deeply_nested_zero_delay_timeouts_are_clamped_to_four_ms() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            "globalThis.__nestedCount = 0; \
             function nestedTimeout() { \
               __nestedCount++; \
               if (__nestedCount < 7) setTimeout(nestedTimeout, 0); \
             } \
             setTimeout(nestedTimeout, 0)",
        );

        for _ in 0..10 {
            advance_and_drain(&mut rt, Duration::from_nanos(1)).await;
        }
        assert_js(&mut rt, "__nestedCount === 5");

        advance_and_drain(&mut rt, Duration::from_millis(3)).await;
        assert_js(&mut rt, "__nestedCount === 5");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__nestedCount === 6");
        // Four milliseconds is a lower bound, not a delivery deadline. Allow
        // the next deno_core poll turn for the timer created inside callback 6.
        advance_and_drain(&mut rt, Duration::from_millis(5)).await;
        assert_js(&mut rt, "__nestedCount === 7");
    }

    #[tokio::test(start_paused = true)]
    async fn promise_jobs_finish_before_the_next_equal_deadline_timer() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            r#"
            globalThis.__order = [];
            setTimeout(() => {
                __order.push('first');
                Promise.resolve().then(() => {
                    __order.push('promise');
                    return Promise.resolve();
                }).then(() => __order.push('chained'));
            }, 0);
            setTimeout(() => __order.push('second'), 0);
        "#,
        );
        assert_js(&mut rt, "__order.length === 0");
        advance_and_drain(&mut rt, Duration::ZERO).await;
        assert_js(
            &mut rt,
            "JSON.stringify(__order) === '[\"first\",\"promise\",\"chained\",\"second\"]'",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn promise_jobs_inherit_timer_nesting_until_the_task_finishes() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            r#"
            globalThis.__nestedCount = 0;
            function nested() {
                __nestedCount++;
                if (__nestedCount < 6) {
                    Promise.resolve().then(() => Promise.resolve())
                        .then(() => setTimeout(nested, 0));
                }
            }
            setTimeout(nested, 0);
        "#,
        );
        advance_and_drain(&mut rt, Duration::ZERO).await;
        assert_js(&mut rt, "__nestedCount === 5");
        advance_and_drain(&mut rt, Duration::from_millis(3)).await;
        assert_js(&mut rt, "__nestedCount === 5");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__nestedCount === 6");
    }

    #[tokio::test(start_paused = true)]
    async fn a_promise_can_cancel_the_next_ready_timer_without_losing_its_successor() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            r#"
            globalThis.__order = [];
            setTimeout(() => {
                __order.push('first');
                Promise.resolve().then(() => clearTimeout(cancelled));
            }, 0);
            const cancelled = setTimeout(() => __order.push('cancelled'), 0);
            setTimeout(() => __order.push('last'), 0);
        "#,
        );
        advance_and_drain(&mut rt, Duration::ZERO).await;
        assert_js(
            &mut rt,
            "JSON.stringify(__order) === '[\"first\",\"last\"]'",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_delay_interval_has_one_ready_tick_then_uses_its_period() {
        let mut rt = boot_runtime();
        exec(
            &mut rt,
            "globalThis.__count = 0; globalThis.__id = setInterval(() => __count++, 0)",
        );
        advance_and_drain(&mut rt, Duration::ZERO).await;
        assert_js(&mut rt, "__count === 1");
        advance_and_drain(&mut rt, Duration::from_millis(1)).await;
        assert_js(&mut rt, "__count === 2");
        exec(&mut rt, "clearInterval(__id)");
    }

    #[tokio::test(start_paused = true)]
    async fn zero_delay_timer_ref_state_and_cancellation_do_not_keep_the_loop_alive() {
        let mut rt = boot_runtime_with_timer_internals();
        exec(
            &mut rt,
            "globalThis.__id = setTimeout(() => { throw new Error('cancelled'); }, 0)",
        );
        exec(&mut rt, "__timerInternals.unrefTimer(__id)");
        assert_js(&mut rt, "!__coreInternals.eventLoopHasMoreWork()");
        exec(&mut rt, "__timerInternals.refTimer(__id)");
        assert_js(&mut rt, "__coreInternals.eventLoopHasMoreWork()");
        exec(&mut rt, "clearTimeout(__id)");
        assert_js(&mut rt, "!__coreInternals.eventLoopHasMoreWork()");
        advance_and_drain(&mut rt, Duration::ZERO).await;
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_timer_churn_releases_quota_and_core_handles() {
        let mut rt = boot_runtime_with_timer_internals();
        exec(
            &mut rt,
            r#"
            for (let round = 0; round < 10; round++) {
                const ids = [];
                for (let i = 0; i < 1024; i++) ids.push(setTimeout(() => {}, i & 1 ? 0 : 1000));
                for (const id of ids) clearTimeout(id);
            }
        "#,
        );
        assert_js(&mut rt, "!__coreInternals.eventLoopHasMoreWork()");
        advance_and_drain(&mut rt, Duration::ZERO).await;
    }

    #[tokio::test]
    #[ignore = "explicit same-cohort scheduler performance experiment"]
    async fn scheduler_performance_experiment() {
        let mut rt = boot_runtime_with_timer_internals();
        let backend = std::env::var("MIGO_TIMER_PROTOTYPE").unwrap_or_else(|_| "core".into());
        for population in [16, 256, 1024] {
            for distinct in [false, true] {
                let source = format!(
                    r#"(() => {{
                    const population = {population};
                    for (let round = 0; round < 10240 / population; round++) {{
                        const ids = [];
                        for (let i = 0; i < population; i++) {{
                            ids.push(setTimeout(() => {{}}, {delay}));
                        }}
                        for (let i = ids.length - 1; i >= 0; i--) clearTimeout(ids[i]);
                    }}
                }})()"#,
                    delay = if distinct { "10000 + i" } else { "10000" }
                );
                let mut samples = Vec::new();
                for iteration in 0..9 {
                    let start = std::time::Instant::now();
                    exec(&mut rt, source.clone());
                    if iteration >= 2 {
                        samples.push(start.elapsed().as_micros() as u64);
                    }
                }
                println!(
                    "TIMER_AB {}",
                    deno_core::serde_json::json!({
                        "backend": backend, "case": "create_cancel", "population": population,
                        "distinct_delays": distinct, "operations": 10240, "samples_us": samples,
                    })
                );
            }
        }
        for iteration in 0..9 {
            let start = std::time::Instant::now();
            exec(
                &mut rt,
                "globalThis.__benchmarkFired = 0; for (let i = 0; i < 1024; i++) setTimeout(() => __benchmarkFired++, 0)",
            );
            rt.run_event_loop(PollEventLoopOptions::default())
                .await
                .expect("timer batch must drain");
            let elapsed = start.elapsed().as_micros() as u64;
            assert_js(&mut rt, "__benchmarkFired === 1024");
            if iteration >= 2 {
                println!(
                    "TIMER_AB {}",
                    deno_core::serde_json::json!({
                        "backend": backend, "case": "dispatch_1024", "sample_us": elapsed,
                    })
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_repeating_core_handle_in_its_callback_does_not_rearm_a_wakeup() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct WakeCount(AtomicUsize);
        impl std::task::Wake for WakeCount {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let mut rt = boot_runtime_with_timer_internals();
        exec(
            &mut rt,
            r#"
            globalThis.fired = 0;
            globalThis.handle = __coreInternals.createTimer(() => {
                fired++;
                __coreInternals.cancelTimer(handle);
            }, 1, undefined, true, true, false);
        "#,
        );
        let count = Arc::new(WakeCount(AtomicUsize::new(0)));
        let waker = std::task::Waker::from(count.clone());
        let mut context = std::task::Context::from_waker(&waker);
        let _ = rt.poll_event_loop(&mut context, PollEventLoopOptions::default());
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(
            count.0.load(Ordering::SeqCst) > 0,
            "the live timer must wake its consumer"
        );
        let _ = rt.poll_event_loop(&mut context, PollEventLoopOptions::default());
        assert_js(&mut rt, "fired === 1");
        count.0.store(0, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            count.0.load(Ordering::SeqCst),
            0,
            "cancelled repeat scheduled a ghost wakeup"
        );
    }
}
