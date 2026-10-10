//! Reward integrity for incentivised video.
//!
//! `isEnded` on a rewarded-video `close` event is the field content uses to
//! decide whether to grant a reward. If the runtime can produce a truthy
//! `isEnded` on its own, a publisher pays out rewards for adverts that were
//! never watched. These tests pin the invariant from both directions:
//!
//! - with no host ad service, no code path yields a reward;
//! - with a host ad service, the verdict is exactly what the host reported,
//!   and only a strict boolean `true` counts.
//!
//! The companion source-level guard is
//! `scripts/test-ad-reward-integrity-contract.sh`, which fails if the embedded
//! JS ever regains the ability to originate a truthy `isEnded`.

#[cfg(test)]
mod ad_reward_integrity_tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex, atomic::AtomicBool},
        time::Duration,
    };

    use deno_core::{FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions};
    use futures::future::poll_fn;
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        protocol::error::ServiceError,
        render_command_sender::CommandSender,
        services::{
            AdService, CommerceServices, ConnectivityServices, DeviceServices, MediaServices,
            SensorServices, SystemUtilServices,
        },
    };

    // ---------------------------------------------------------------
    // A host that records every ad command it is asked to perform.
    // ---------------------------------------------------------------

    #[derive(Default)]
    struct RecordingAdService {
        calls: Arc<Mutex<Vec<String>>>,
        fail_show: bool,
    }

    impl RecordingAdService {
        fn record(&self, method: &str, request_json: &str) {
            self.calls
                .lock()
                .expect("ad call log")
                .push(format!("{method}:{request_json}"));
        }
    }

    impl AdService for RecordingAdService {
        fn create_ad(&self, request_json: &str) -> Result<(), ServiceError> {
            self.record("create", request_json);
            Ok(())
        }
        fn load_ad(&self, request_json: &str) -> Result<(), ServiceError> {
            self.record("load", request_json);
            Ok(())
        }
        fn show_ad(&self, request_json: &str) -> Result<(), ServiceError> {
            self.record("show", request_json);
            if self.fail_show {
                return Err(ServiceError::system("showAd:fail no fill"));
            }
            Ok(())
        }
        fn hide_ad(&self, request_json: &str) -> Result<(), ServiceError> {
            self.record("hide", request_json);
            Ok(())
        }
        fn update_ad_style(&self, request_json: &str) -> Result<(), ServiceError> {
            self.record("style", request_json);
            Ok(())
        }
        fn destroy_ad(&self, request_json: &str) -> Result<(), ServiceError> {
            self.record("destroy", request_json);
            Ok(())
        }
    }

    /// A device-services bundle whose only capability is advertising.
    ///
    /// Every sub-trait method has a default, so overriding `ad()` alone is all
    /// it takes -- and the blanket impl then makes this a `DeviceServices`.
    struct AdOnlyServices {
        ad: Arc<RecordingAdService>,
    }

    impl SensorServices for AdOnlyServices {}
    impl MediaServices for AdOnlyServices {}
    impl ConnectivityServices for AdOnlyServices {}
    impl SystemUtilServices for AdOnlyServices {}
    impl CommerceServices for AdOnlyServices {
        fn ad(&self) -> Option<Arc<dyn AdService>> {
            Some(self.ad.clone())
        }
    }

    // ---------------------------------------------------------------
    // Runtime boot
    // ---------------------------------------------------------------

    fn host_state(device_services: Option<Arc<dyn DeviceServices>>) -> HostOpState {
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
            device_services,
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

    fn boot(device_services: Option<Arc<dyn DeviceServices>>) -> JsRuntime {
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(host_state(device_services)),
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        rt
    }

    /// Boot with a recording ad host, returning the call log alongside.
    fn boot_hosted() -> (JsRuntime, Arc<Mutex<Vec<String>>>) {
        boot_hosted_with(false)
    }

    fn boot_hosted_with(fail_show: bool) -> (JsRuntime, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ad = Arc::new(RecordingAdService {
            calls: calls.clone(),
            fail_show,
        });
        let services: Arc<dyn DeviceServices> = Arc::new(AdOnlyServices { ad });
        (boot(Some(services)), calls)
    }

    fn boot_unhosted() -> JsRuntime {
        boot(None)
    }

    fn exec(rt: &mut JsRuntime, source: impl Into<String>) {
        rt.execute_script("<test:ad>", FastString::from(source.into()))
            .expect("ad script");
    }

    fn assert_js(rt: &mut JsRuntime, expression: &str) {
        exec(
            rt,
            format!(
                "if (!({expression})) throw new Error('ad assertion failed: ' + ({expression}));"
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
        tokio::time::advance(Duration::from_nanos(1)).await;
        tokio::task::yield_now().await;
        drain_ready(rt).await;
    }

    /// Deliver one ad event exactly the way the host does: through the
    /// Symbol-keyed bridge holder, carrying a JSON string.
    fn deliver_ad_event(rt: &mut JsRuntime, json: &str) {
        exec(
            rt,
            format!(
                "globalThis[Symbol.for('Migo.hostBridge')]._internalOnAdEvent('{json}')",
                json = json.replace('\\', "\\\\").replace('\'', "\\'")
            ),
        );
    }

    /// Create a rewarded video ad and record every close verdict it reports.
    const WATCH_REWARDED: &str = "\
        globalThis.__closes = []; \
        globalThis.__ad = createRewardedVideoAd({ adUnitId: 'unit-1' }); \
        globalThis.__ad.onClose((res) => { globalThis.__closes.push(res); });";

    // ---------------------------------------------------------------
    // 1. No host ad service: no reward can be produced at all.
    // ---------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn unhosted_rewarded_video_never_reports_a_completed_view() {
        let mut rt = boot_unhosted();
        exec(&mut rt, WATCH_REWARDED);
        exec(&mut rt, "globalThis.__ad.show();");

        // Well past the fallback close delay.
        advance_and_drain(&mut rt, Duration::from_millis(2_000)).await;

        assert_js(&mut rt, "globalThis.__closes.length === 1");
        // The close still arrives (content must not hang), but no reward is owed.
        assert_js(&mut rt, "globalThis.__closes[0].isEnded === false");
    }

    // ---------------------------------------------------------------
    // 2. Hosted: the verdict is exactly what the host reported.
    // ---------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn hosted_close_grants_the_reward_the_host_reported() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        exec(&mut rt, "globalThis.__ad.show();");
        drain_ready(&mut rt).await;

        deliver_ad_event(&mut rt, r#"{"adId":1,"event":"close","isEnded":true}"#);
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "globalThis.__closes.length === 1");
        assert_js(&mut rt, "globalThis.__closes[0].isEnded === true");
    }

    #[tokio::test(start_paused = true)]
    async fn hosted_close_denies_the_reward_when_the_host_says_unfinished() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        exec(&mut rt, "globalThis.__ad.show();");
        drain_ready(&mut rt).await;

        deliver_ad_event(&mut rt, r#"{"adId":1,"event":"close","isEnded":false}"#);
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "globalThis.__closes[0].isEnded === false");
    }

    /// A missing `isEnded` must read as "not completed", not as "unknown".
    #[tokio::test(start_paused = true)]
    async fn hosted_close_without_a_verdict_denies_the_reward() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        exec(&mut rt, "globalThis.__ad.show();");
        drain_ready(&mut rt).await;

        deliver_ad_event(&mut rt, r#"{"adId":1,"event":"close"}"#);
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "globalThis.__closes[0].isEnded === false");
    }

    /// Truthy-but-not-boolean values must not be forwarded as a reward. This is
    /// what the strict `=== true` comparison in `_closePayload` buys: a host
    /// (or a compromised transport) that sends `"true"` or `1` does not get a
    /// payout, and content always sees a real boolean.
    #[tokio::test(start_paused = true)]
    async fn truthy_non_boolean_verdicts_do_not_grant_a_reward() {
        for payload in [
            r#"{"adId":1,"event":"close","isEnded":"true"}"#,
            r#"{"adId":1,"event":"close","isEnded":1}"#,
            r#"{"adId":1,"event":"close","isEnded":"yes"}"#,
            r#"{"adId":1,"event":"close","isEnded":{}}"#,
        ] {
            let (mut rt, _calls) = boot_hosted();
            exec(&mut rt, WATCH_REWARDED);
            exec(&mut rt, "globalThis.__ad.show();");
            drain_ready(&mut rt).await;

            deliver_ad_event(&mut rt, payload);
            drain_ready(&mut rt).await;

            assert_js(&mut rt, "globalThis.__closes.length === 1");
            assert_js(
                &mut rt,
                "globalThis.__closes[0].isEnded === false && \
                 typeof globalThis.__closes[0].isEnded === 'boolean'",
            );
        }
    }

    // ---------------------------------------------------------------
    // 3. What the delivery channel does and does not hide.
    // ---------------------------------------------------------------

    /// The ad event hook is not a string-keyed global.
    ///
    /// That is all this proves, and the name says so -- it looks at string keys
    /// only, and says nothing about symbol-keyed ones.
    ///
    /// The side it does not cover used to be open: the holder was installed at
    /// `globalThis[Symbol.for('Migo.hostBridge')]`, and `Symbol.for` reads the
    /// **global** registry, so content could retrieve the same symbol and call
    /// any of the 78 hooks on it -- `_internalOnAdEvent` among them. It is
    /// closed now: the runtime resolves the holder once, keeps a handle, and
    /// deletes the name. Covered by `tests/host_bridge_dispatch.rs`, which is
    /// where that invariant belongs; this file stays about reward integrity.
    #[tokio::test(start_paused = true)]
    async fn the_ad_event_hook_is_not_a_string_keyed_global() {
        let (mut rt, _calls) = boot_hosted();
        assert_js(
            &mut rt,
            "globalThis._internalOnAdEvent === undefined && \
             Object.getOwnPropertyNames(globalThis) \
                 .filter((k) => k.indexOf('_internalOnAdEvent') !== -1).length === 0",
        );
    }

    /// Content holding an ad object must not be able to walk back to the
    /// listener groups and fire `close` itself.
    #[tokio::test(start_paused = true)]
    async fn ad_listener_groups_are_not_reachable_from_content() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        assert_js(
            &mut rt,
            "Object.getOwnPropertyNames(globalThis.__ad) \
                 .filter((k) => k.indexOf('listener') !== -1).length === 0",
        );
        // The private field is genuinely private: reflection finds nothing.
        assert_js(
            &mut rt,
            "Object.getOwnPropertyNames(globalThis.__ad).length === 0",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ad_event_ingress_is_absent_from_the_content_object_graph() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        assert_js(
            &mut rt,
            "(() => { \
                 const names = []; \
                 for (let p = globalThis.__ad; p && p !== Object.prototype; \
                      p = Object.getPrototypeOf(p)) { \
                   names.push(...Object.getOwnPropertyNames(p)); \
                 } \
                 return names.indexOf('_fire') === -1 \
                   && names.indexOf('_handleHostEvent') === -1; \
             })()",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn content_object_methods_cannot_synthesize_ad_events() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        exec(
            &mut rt,
            "if (typeof globalThis.__ad._fire === 'function') { \
                 globalThis.__ad._fire('close', { isEnded: true }); \
             } \
             if (typeof globalThis.__ad._handleHostEvent === 'function') { \
                 globalThis.__ad._handleHostEvent('close', { isEnded: true }); \
             }",
        );
        drain_ready(&mut rt).await;
        assert_js(&mut rt, "globalThis.__closes.length === 0");
    }

    // ---------------------------------------------------------------
    // 4. Routing robustness -- a stray event must not pay out.
    // ---------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn events_for_an_unknown_ad_id_are_ignored() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        drain_ready(&mut rt).await;

        deliver_ad_event(&mut rt, r#"{"adId":9999,"event":"close","isEnded":true}"#);
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "globalThis.__closes.length === 0");
    }

    #[tokio::test(start_paused = true)]
    async fn events_after_destroy_are_ignored() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        exec(&mut rt, "globalThis.__ad.destroy();");
        drain_ready(&mut rt).await;

        deliver_ad_event(&mut rt, r#"{"adId":1,"event":"close","isEnded":true}"#);
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "globalThis.__closes.length === 0");
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_event_payloads_are_ignored() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        drain_ready(&mut rt).await;

        for payload in [
            "",
            "not json",
            "[]",
            r#"{"event":"close","isEnded":true}"#,
            r#"{"adId":1,"isEnded":true}"#,
            r#"{"adId":"one","event":"close","isEnded":true}"#,
        ] {
            deliver_ad_event(&mut rt, payload);
        }
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "globalThis.__closes.length === 0");
    }

    // ---------------------------------------------------------------
    // 5. The bridge is actually wired (not merely compiled).
    // ---------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn ad_commands_reach_the_host_service() {
        let (mut rt, calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        exec(&mut rt, "globalThis.__ad.load(); globalThis.__ad.show();");
        exec(&mut rt, "globalThis.__ad.destroy();");
        drain_ready(&mut rt).await;

        let log = calls.lock().expect("ad call log").clone();
        let methods: Vec<&str> = log
            .iter()
            .map(|entry| entry.split(':').next().unwrap_or(""))
            .collect();
        assert_eq!(
            methods,
            vec!["create", "load", "show", "destroy"],
            "ad commands should reach the host in order; got {log:?}"
        );

        // The create request must carry what a host needs to resolve a slot.
        assert!(
            log[0].contains("\"adType\":\"rewardedVideo\"")
                && log[0].contains("\"adUnitId\":\"unit-1\""),
            "create request should identify the slot; got {}",
            log[0]
        );
        // Every command must be addressed to the same handle.
        for entry in &log {
            assert!(
                entry.contains("\"adId\":1"),
                "command should carry its adId; got {entry}"
            );
        }
    }

    /// A hosted ad must not run the unhosted timers: if it did, a host that is
    /// slow to report would be raced by a locally-produced close event.
    #[tokio::test(start_paused = true)]
    async fn hosted_ads_do_not_self_close_on_a_timer() {
        let (mut rt, _calls) = boot_hosted();
        exec(&mut rt, WATCH_REWARDED);
        exec(&mut rt, "globalThis.__ad.show();");

        advance_and_drain(&mut rt, Duration::from_millis(10_000)).await;

        assert_js(&mut rt, "globalThis.__closes.length === 0");
    }

    /// The unhosted timers must not fire for a hosted ad in *either* direction.
    /// A locally-produced `load` is not harmless: content treats `load` as "an
    /// advert is ready" and calls `show()`, so a fabricated load turns into a
    /// show request for inventory the host never fetched.
    #[tokio::test(start_paused = true)]
    async fn hosted_ads_do_not_self_report_load_on_a_timer() {
        let (mut rt, _calls) = boot_hosted();
        exec(
            &mut rt,
            "globalThis.__loads = []; \
             globalThis.__ad = createRewardedVideoAd({ adUnitId: 'unit-1' }); \
             globalThis.__ad.onLoad((res) => { globalThis.__loads.push(res); });",
        );

        advance_and_drain(&mut rt, Duration::from_millis(10_000)).await;
        assert_js(&mut rt, "globalThis.__loads.length === 0");

        // ... and the host's own load event still gets through.
        deliver_ad_event(
            &mut rt,
            r#"{"adId":1,"event":"load","useFallbackSharePage":true}"#,
        );
        drain_ready(&mut rt).await;
        assert_js(&mut rt, "globalThis.__loads.length === 1");
        assert_js(
            &mut rt,
            "globalThis.__loads[0].useFallbackSharePage === true",
        );
    }

    /// A host-side failure surfaces as an `error` event, not as an exception
    /// out of `show()` -- mini-game content listens on `onError` and does not wrap
    /// `show()` in try/catch.
    #[tokio::test(start_paused = true)]
    async fn host_command_failures_surface_as_error_events() {
        let (mut rt, _calls) = boot_hosted_with(true);
        exec(
            &mut rt,
            "globalThis.__errors = []; \
             globalThis.__ad = createRewardedVideoAd({ adUnitId: 'unit-1' }); \
             globalThis.__ad.onError((e) => { globalThis.__errors.push(e); });",
        );
        exec(&mut rt, "globalThis.__ad.show();");
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "globalThis.__errors.length === 1");
        assert_js(
            &mut rt,
            "typeof globalThis.__errors[0].errMsg === 'string' && \
             globalThis.__errors[0].errMsg.indexOf('no fill') !== -1",
        );
    }

    // ---------------------------------------------------------------
    // 6. Non-reward ad types keep working across both modes.
    // ---------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn banner_style_writes_reach_the_host() {
        let (mut rt, calls) = boot_hosted();
        exec(
            &mut rt,
            "globalThis.__banner = createBannerAd({ adUnitId: 'b-1', style: { left: 0, top: 0, width: 300 } }); \
             globalThis.__banner.style.top = 120;",
        );
        drain_ready(&mut rt).await;

        let log = calls.lock().expect("ad call log").clone();
        assert!(
            log.iter()
                .any(|entry| entry.starts_with("style:") && entry.contains("\"top\":120")),
            "style write should reach the host; got {log:?}"
        );
        // The local object still reflects the write.
        assert_js(&mut rt, "globalThis.__banner.style.top === 120");

        // A tracked write must not cost the style object its plain-object
        // behaviour: content serialises and iterates it.
        assert_js(
            &mut rt,
            "JSON.parse(JSON.stringify(globalThis.__banner.style)).top === 120",
        );
        assert_js(
            &mut rt,
            "Object.keys(globalThis.__banner.style).indexOf('top') !== -1",
        );
    }

    /// Writing a field the host does not lay out must not generate traffic.
    /// `realWidth` is a rendered-size readback, not a layout input; forwarding
    /// it would have the host chasing its own reported geometry.
    #[tokio::test(start_paused = true)]
    async fn untracked_style_fields_do_not_reach_the_host() {
        let (mut rt, calls) = boot_hosted();
        exec(
            &mut rt,
            "globalThis.__banner = createBannerAd({ adUnitId: 'b-1', style: { left: 0, top: 0, width: 300 } }); \
             globalThis.__banner.style.realWidth = 999;",
        );
        drain_ready(&mut rt).await;

        let log = calls.lock().expect("ad call log").clone();
        assert!(
            !log.iter().any(|entry| entry.starts_with("style:")),
            "an untracked style field should send nothing; got {log:?}"
        );
        assert_js(&mut rt, "globalThis.__banner.style.realWidth === 999");
    }

    #[tokio::test(start_paused = true)]
    async fn unhosted_banner_still_reports_load_and_resize() {
        let mut rt = boot_unhosted();
        exec(
            &mut rt,
            "globalThis.__loads = 0; globalThis.__resizes = 0; \
             globalThis.__banner = createBannerAd({ adUnitId: 'b-1', style: { left: 0, top: 0, width: 300 } }); \
             globalThis.__banner.onLoad(() => { globalThis.__loads += 1; }); \
             globalThis.__banner.onResize(() => { globalThis.__resizes += 1; }); \
             globalThis.__banner.show();",
        );
        advance_and_drain(&mut rt, Duration::from_millis(500)).await;

        assert_js(&mut rt, "globalThis.__loads === 1");
        assert_js(&mut rt, "globalThis.__resizes === 1");
    }

    /// Each ad object gets its own handle, so events cannot be cross-delivered.
    #[tokio::test(start_paused = true)]
    async fn distinct_ads_get_distinct_handles() {
        let (mut rt, calls) = boot_hosted();
        exec(
            &mut rt,
            "globalThis.__a = createRewardedVideoAd({ adUnitId: 'u-1' }); \
             globalThis.__b = createRewardedVideoAd({ adUnitId: 'u-2' }); \
             globalThis.__closesA = []; globalThis.__closesB = []; \
             globalThis.__a.onClose((r) => globalThis.__closesA.push(r)); \
             globalThis.__b.onClose((r) => globalThis.__closesB.push(r));",
        );
        drain_ready(&mut rt).await;

        let log = calls.lock().expect("ad call log").clone();
        assert!(
            log[0].contains("\"adId\":1") && log[1].contains("\"adId\":2"),
            "handles should be distinct; got {log:?}"
        );

        deliver_ad_event(&mut rt, r#"{"adId":2,"event":"close","isEnded":true}"#);
        drain_ready(&mut rt).await;

        assert_js(
            &mut rt,
            "globalThis.__closesA.length === 0 && globalThis.__closesB.length === 1",
        );
        assert_js(&mut rt, "globalThis.__closesB[0].isEnded === true");
    }

    // ---------------------------------------------------------------
    // The ad skip card: the host sells it, so its outcomes are the host's.
    // ---------------------------------------------------------------

    const WATCH_SKIP_CARD: &str = "\
        globalThis.__card = createAdSkipCard(); \
        globalThis.__seen = []; \
        __card.onLoad(() => __seen.push('load')); \
        __card.onShow(() => __seen.push('show')); \
        __card.onClose((res) => __seen.push('close:' + res.result)); \
        __card.onUse((res) => __seen.push('use:' + res.usedCount)); \
        __card.onError((res) => __seen.push('error:' + res.errCode));";

    #[tokio::test(start_paused = true)]
    async fn the_skip_card_reports_what_the_host_saw() {
        let (mut rt, calls) = boot_hosted();
        exec(&mut rt, WATCH_SKIP_CARD);
        assert_js(&mut rt, "createAdSkipCard() === globalThis.__card");
        exec(
            &mut rt,
            "globalThis.__loaded = false; globalThis.__shown = false; \
             __card.load().then(() => { __loaded = true; }); \
             __card.show().then(() => { __shown = true; });",
        );
        drain_ready(&mut rt).await;
        let create = calls.lock().unwrap()[0].clone();
        assert!(create.starts_with("create:"), "{create}");
        assert!(create.contains(r#""adType":"skipCard""#), "{create}");

        deliver_ad_event(&mut rt, r#"{"adId":1,"event":"load"}"#);
        deliver_ad_event(&mut rt, r#"{"adId":1,"event":"show"}"#);
        deliver_ad_event(
            &mut rt,
            r#"{"adId":1,"event":"close","result":"pay_success"}"#,
        );
        deliver_ad_event(&mut rt, r#"{"adId":1,"event":"use","usedCount":2}"#);
        deliver_ad_event(
            &mut rt,
            r#"{"adId":1,"event":"close","result":"free_pass"}"#,
        );
        drain_ready(&mut rt).await;

        assert_js(&mut rt, "globalThis.__loaded && globalThis.__shown");
        assert_js(
            &mut rt,
            "globalThis.__seen.join() === 'load,show,close:pay_success,use:2,close:close'",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_host_the_skip_card_fails_with_the_platform_codes() {
        let mut rt = boot_unhosted();
        exec(&mut rt, WATCH_SKIP_CARD);
        exec(
            &mut rt,
            "globalThis.__failed = []; \
             __card.load().catch((e) => __failed.push(e.errCode + ' ' + e.errMsg)); \
             __card.show().catch((e) => __failed.push(e.errCode + ' ' + e.errMsg));",
        );
        drain_ready(&mut rt).await;
        assert_js(
            &mut rt,
            "globalThis.__failed.join() === '-1001 load:fail not supported,-1002 show:fail not supported'",
        );
        assert_js(
            &mut rt,
            "globalThis.__seen.join() === 'error:-1001,error:-1002'",
        );
    }
}
