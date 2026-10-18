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
        esm = ["ext:timer_test_bridge/bridge.js" = {
            source = r#"
                    import * as timers from "ext:host_v8_web/02_timers.js";
                    import { core } from "ext:core/mod.js";
                    globalThis.__timerInternals = timers;
                    globalThis.__coreInternals = core;
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
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state(false)),
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        rt
    }

    fn boot_hidden_runtime() -> JsRuntime {
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state(true)),
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        rt
    }

    fn boot_runtime_with_timer_internals() -> JsRuntime {
        let mut extensions = crate::main_extensions(test_host_state(false));
        extensions.push(timer_test_bridge::init());
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        rt
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
}
