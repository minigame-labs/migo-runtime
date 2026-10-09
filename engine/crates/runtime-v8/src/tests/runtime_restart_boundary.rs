//! Host callback ids as seen from JavaScript, across a runtime replacement.
//!
//! Two properties, both about a result arriving for a request that is not the
//! one it names:
//!
//! * ids come from the Host's allocator, so a replacement isolate never reissues
//!   an id the retired one already handed to the platform;
//! * a result whose `requestId` is present but is not an id settles nothing,
//!   rather than falling through to whichever request happens to be oldest.
//!
//! The runtimes are built one after another over **one** allocator, which is
//! what a restart looks like from this layer: the isolate is new, the id space
//! is not.

#[cfg(test)]
mod runtime_restart_boundary_tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::callback_id::CallbackIdAllocator;
    use shared::channel::ThreadWakeup;
    use shared::device::gpu_caps::GpuCaps;
    use shared::op_state::{AudioSender, HostOpState, NetworkPolicy};
    use shared::render_command_sender::CommandSender;
    use shared::services::{
        AdService, CommerceServices, ConnectivityServices, InteractionService, MediaServices,
        SensorServices, SystemInfoService, SystemUtilServices,
    };

    /// A host that only claims to do advertising, refusing every command.
    ///
    /// Present so ads take the *hosted* construction path, which is not a
    /// convenience: an unhosted ad schedules its fallback `load` through
    /// `setTimeout`, this harness runs without a Tokio reactor, and the panic
    /// that follows cannot unwind — it aborts the process instead of failing a
    /// test. Every command failing is fine here; the id is allocated before any
    /// of them, which is the property under test.
    struct HostedAdService;
    impl AdService for HostedAdService {}

    /// A host that accepts every dialog and settings request and answers none.
    ///
    /// Accepting is the point: an op that fails takes `invoke`'s executor
    /// `catch`, which settles the promise and removes the pending entry, so a
    /// refusing host leaves nothing to correlate. Answering nothing is also the
    /// point — the tests deliver the results themselves, in the order they
    /// choose.
    struct AcceptingUiService;
    impl InteractionService for AcceptingUiService {
        fn show_modal(&self, _json: &str) -> Result<(), shared::protocol::error::ServiceError> {
            Ok(())
        }

        fn show_action_sheet(
            &self,
            _json: &str,
        ) -> Result<(), shared::protocol::error::ServiceError> {
            Ok(())
        }
    }
    impl SystemInfoService for AcceptingUiService {
        fn open_bluetooth_settings(
            &self,
            _request_id: i32,
        ) -> Result<(), shared::protocol::error::ServiceError> {
            Ok(())
        }

        fn open_app_authorize_setting(
            &self,
            _request_id: i32,
        ) -> Result<(), shared::protocol::error::ServiceError> {
            Ok(())
        }
    }

    struct AdOnlyServices;
    impl SensorServices for AdOnlyServices {}
    impl MediaServices for AdOnlyServices {}
    impl ConnectivityServices for AdOnlyServices {}
    impl SystemUtilServices for AdOnlyServices {
        fn interaction(&self) -> Option<Arc<dyn InteractionService>> {
            Some(Arc::new(AcceptingUiService))
        }

        fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
            Some(Arc::new(AcceptingUiService))
        }
    }
    impl CommerceServices for AdOnlyServices {
        fn ad(&self) -> Option<Arc<dyn AdService>> {
            Some(Arc::new(HostedAdService))
        }
    }

    // Exposes the real factory rather than a copy of it: these tests are about
    // `createDeferredApi`'s own correlation, so a reimplementation here would
    // assert nothing about the code that ships.
    deno_core::extension!(
        deferred_test_bridge,
        deps = [host_v8_base],
        esm_entry_point = "ext:deferred_test_bridge/bridge.js",
        esm = ["ext:deferred_test_bridge/bridge.js" = {
            source = r#"
                import { allocateHostCallbackId, createDeferredApi }
                    from "ext:host_v8_base/02_async.js";
                globalThis.__createDeferredApi = createDeferredApi;
                globalThis.__allocId = allocateHostCallbackId;
            "#
        },],
    );

    fn test_host_state(callback_ids: Arc<CallbackIdAllocator>) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids,
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
            device_services: Some(Arc::new(AdOnlyServices)),
            raf_rx: None,
            raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            // One configured subpackage, so `loadSubpackage` resolves and reaches
            // the allocation instead of failing validation first -- an unreached
            // allocation site looks exactly like one that does not allocate.
            sub_packages: vec![("sub".to_owned(), "sub/".to_owned())],
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

    fn boot(callback_ids: Arc<CallbackIdAllocator>) -> JsRuntime {
        let mut extensions = crate::main_extensions(test_host_state(callback_ids));
        extensions.push(deferred_test_bridge::init());
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        // One probe API per runtime, timeouts disabled so nothing settles on a
        // clock instead of on a result.
        exec(
            &mut rt,
            r#"
            globalThis.__ids = [];
            globalThis.__settled = [];
            globalThis.__api = globalThis.__createDeferredApi('probe', 0);
            globalThis.__call = function (tag) {
                globalThis.__api.invoke(
                    { success: function () { globalThis.__settled.push(tag); } },
                    function (_opts, id) { globalThis.__ids.push(id); },
                );
            };
            // Results arrive through the host bridge, not through a global: the
            // `_internalOn*` names are retired from `globalThis` by hardening
            // and travel by handle. Going through the real dispatcher also puts
            // the argument encoding under test -- these hooks are called with
            // exactly the JSON array `hook_args_one` builds.
            globalThis.__hook = function (name, args) {
                globalThis[Symbol.for('Migo.hostBridge')]
                    ._internalDispatch(name, JSON.stringify(args));
            };
            "#,
        );
        rt
    }

    fn exec(rt: &mut JsRuntime, source: impl Into<String>) {
        rt.execute_script("<test:deferred>", FastString::from(source.into()))
            .expect("deferred probe script");
    }

    // The expression is never interpolated into a JavaScript string literal:
    // several of them contain quotes, and one that closed the literal produced a
    // SyntaxError that read like a failure of the code under test.
    fn assert_js(rt: &mut JsRuntime, expression: &str) {
        let script = format!(
            "if (!({expression})) throw new Error('ids=' + JSON.stringify(globalThis.__ids) \
             + ' settled=' + JSON.stringify(globalThis.__settled));"
        );
        if let Err(error) = rt.execute_script("<test:deferred>", FastString::from(script)) {
            panic!("assertion failed: {expression}\n{error}");
        }
    }

    #[test]
    fn a_replacement_runtime_never_reissues_a_retired_runtimes_id() {
        let callback_ids = Arc::new(CallbackIdAllocator::default());

        {
            let mut retired = boot(Arc::clone(&callback_ids));
            exec(&mut retired, "__call('a'); __call('b');");
            // Exact, not "increasing": if anything else in a booted runtime
            // allocated an id, this is where it would show.
            assert_js(&mut retired, "__ids.join(',') === '1,2'");
        }

        // Read the space from where the platform stands. Three proves the
        // retired runtime consumed exactly two and that dropping it released
        // nothing back.
        assert_eq!(callback_ids.allocate(), Ok(3));

        let mut replacement = boot(Arc::clone(&callback_ids));
        exec(&mut replacement, "__call('c');");
        assert_js(&mut replacement, "__ids.join(',') === '4'");

        // The retired runtime's first id is meaningless here. With a per-runtime
        // counter both would be 1 and this would settle 'c'.
        exec(
            &mut replacement,
            "__api.settle(JSON.stringify({ requestId: 1 }));",
        );
        assert_js(&mut replacement, "__settled.length === 0");

        exec(
            &mut replacement,
            "__api.settle(JSON.stringify({ requestId: 4 }));",
        );
        assert_js(&mut replacement, "__settled.join(',') === 'c'");
    }

    #[test]
    fn a_requestid_that_is_not_an_id_settles_nothing() {
        let mut rt = boot(Arc::new(CallbackIdAllocator::default()));
        exec(&mut rt, "__call('only');");

        // Every one of these is *present* and not an id. Before this, `Number()`
        // turned the non-numeric ones into `NaN`, which reached the fallback and
        // settled the oldest pending request -- here, the wrong one and the only
        // one.
        for value in [
            "null",
            "0",
            "-1",
            "1.5",
            "NaN",
            "Infinity",
            "-Infinity",
            "2147483648",
            "\"abc\"",
            "\"\"",
            "true",
            "{}",
            "[]",
        ] {
            exec(
                &mut rt,
                &format!("__api.settle(JSON.stringify({{ requestId: {value} }}));"),
            );
            assert_js(&mut rt, "__settled.length === 0");
        }

        // The real id still works afterwards: rejecting the impostors must not
        // have consumed or corrupted the pending entry.
        exec(
            &mut rt,
            "__api.settle(JSON.stringify({ requestId: __ids[0] }));",
        );
        assert_js(&mut rt, "__settled.join(',') === 'only'");
    }

    #[test]
    fn an_id_serialised_as_a_string_still_correlates() {
        // Tolerated on purpose, and pinned so it is a decision rather than an
        // accident: a platform that echoes the id as text is still echoing the
        // exact id. `true` and `[]` are not, which is why the parser checks the
        // type before it converts -- `Number(true)` is 1, and 1 is an id.
        let mut rt = boot(Arc::new(CallbackIdAllocator::default()));
        exec(&mut rt, "__call('only');");

        exec(
            &mut rt,
            "__api.settle(JSON.stringify({ requestId: String(__ids[0]) }));",
        );

        assert_js(&mut rt, "__settled.join(',') === 'only'");
    }

    #[test]
    fn an_id_settles_exactly_its_own_request_and_only_once() {
        let mut rt = boot(Arc::new(CallbackIdAllocator::default()));
        exec(&mut rt, "__call('first'); __call('second');");

        exec(
            &mut rt,
            "__api.settle(JSON.stringify({ requestId: __ids[1] }));",
        );
        assert_js(&mut rt, "__settled.join(',') === 'second'");

        // A duplicate of a settled id must not reach the still-pending request.
        exec(
            &mut rt,
            "__api.settle(JSON.stringify({ requestId: __ids[1] }));",
        );
        assert_js(&mut rt, "__settled.join(',') === 'second'");

        exec(
            &mut rt,
            "__api.settle(JSON.stringify({ requestId: __ids[0] }));",
        );
        assert_js(&mut rt, "__settled.join(',') === 'second,first'");
    }

    #[test]
    fn modules_with_their_own_pending_maps_draw_from_the_same_space() {
        // login, payment and subpackage each kept a module-local counter that
        // restarted at 1, so two of them handed the platform the same id for
        // different requests and a restart reissued ids it had already used.
        //
        // Bracketed with two allocations instead of counted from Rust: the
        // difference is exact regardless of how many APIs the profile ships, and
        // Slim drops commerce, so a fixed total would assert the product
        // configuration rather than the property. Absent APIs are skipped
        // out loud rather than silently making the sum work out.
        let mut rt = boot(Arc::new(CallbackIdAllocator::default()));
        exec(
            &mut rt,
            r#"
            globalThis.__checked = [];
            globalThis.__assertTakesOneId = function (name, fn) {
                if (typeof fn !== 'function') return;
                const before = globalThis.__allocId();
                try {
                    const out = fn();
                    if (out && typeof out.catch === 'function') out.catch(function () {});
                } catch (e) {
                    // The platform boundary throws in this harness -- no host is
                    // listening. Allocation happens first, which is the property.
                }
                const after = globalThis.__allocId();
                if (after - before !== 2) {
                    throw new Error(
                        name + ' took ' + (after - before - 1) + ' ids from the Host space, want 1'
                    );
                }
                globalThis.__checked.push(name);
            };

            __assertTakesOneId('migo.login', migo.login && function () { return migo.login({}); });
            __assertTakesOneId(
                'migo.requestMidasPayment',
                migo.requestMidasPayment && function () { return migo.requestMidasPayment({ offerId: 'x' }); },
            );
            __assertTakesOneId('migo.loadSubpackage', function () { return migo.loadSubpackage({ name: 'sub' }); });
            // Not a deferred call: the id names a player the platform keeps in
            // a map that outlives this isolate, and it is what every later
            // `onVideoEvent` routes by. A module counter restarting at 1 aims
            // the retired runtime's events at the replacement's objects.
            __assertTakesOneId('migo.createVideo', migo.createVideo && function () { return migo.createVideo({}); });
            // The same property for the callback-routed resources: each id names
            // something the host owns in a table that outlives this isolate, so
            // a module counter restarting at 1 points the retired runtime's
            // events -- camera frames, audio playback, an ad's reward verdict --
            // at the replacement runtime's objects.
            __assertTakesOneId('migo.createCamera', migo.createCamera && function () { return migo.createCamera({}); });
            __assertTakesOneId(
                'migo.createInnerAudioContext',
                migo.createInnerAudioContext && function () { return migo.createInnerAudioContext(); },
            );
            __assertTakesOneId(
                'migo.createRewardedVideoAd',
                migo.createRewardedVideoAd && function () { return migo.createRewardedVideoAd({ adUnitId: 'x' }); },
            );
            // The UI results that had no id at all until now: each settled by
            // `shift()` on its own array, so nothing about them was correlated.
            __assertTakesOneId('migo.showModal', migo.showModal && function () { return migo.showModal({}); });
            __assertTakesOneId(
                'migo.showActionSheet',
                migo.showActionSheet && function () { return migo.showActionSheet({ itemList: ['a'] }); },
            );
            __assertTakesOneId(
                'migo.openAppAuthorizeSetting',
                migo.openAppAuthorizeSetting && function () { return migo.openAppAuthorizeSetting({}); },
            );
            // `_timeout: 0` because this one keeps `createDeferredApi`'s default
            // 30s auto-reject, and scheduling a timer in a harness with no Tokio
            // reactor aborts the process instead of failing a test.
            __assertTakesOneId(
                'migo.openSystemBluetoothSetting',
                migo.openSystemBluetoothSetting
                    && function () { return migo.openSystemBluetoothSetting({ _timeout: 0 }); },
            );
            "#,
        );

        // At least one had to be present, or the test proved nothing at all.
        assert_js(&mut rt, "__checked.length > 0");
    }

    #[test]
    fn two_modals_settle_by_id_even_when_answered_out_of_order() {
        // These used to be an array settled by `shift()`, so the first result
        // answered the first call whatever it was actually a result for. A host
        // that answers two dialogs in the order the user dismissed them -- not
        // the order they were raised -- settled each with the other's outcome.
        //
        // Settlement is observed through `success`/`fail`, which
        // `createDeferredApi` invokes synchronously, so this needs no event
        // loop turn and nothing settles on a clock.
        let mut rt = boot(Arc::new(CallbackIdAllocator::default()));
        exec(
            &mut rt,
            r#"
            globalThis.__out = [];
            // Slim does not ship the UI APIs, and a test that requires one is a
            // test that asserts the product configuration. The Full lane runs
            // the scenario; this records which lane did, so a skip cannot be
            // read as a pass.
            globalThis.__ran = typeof showModal === 'function';
            if (__ran) {
                const base = globalThis.__allocId();
                showModal({ success: function (r) { __out.push('first:' + r.confirm); } });
                showModal({ success: function (r) { __out.push('second:' + r.confirm); } });

                // Second one first, by its own id.
                __hook('_internalOnModalResult', [JSON.stringify({ requestId: base + 2, confirm: true, cancel: false })]);
                __hook('_internalOnModalResult', [JSON.stringify({ requestId: base + 1, confirm: false, cancel: true })]);
            }
            "#,
        );

        assert_js(
            &mut rt,
            "!__ran || __out.join('|') === 'second:true|first:false'",
        );
    }

    #[test]
    fn an_action_sheet_result_reaches_its_own_call_and_an_unknown_id_reaches_none() {
        let mut rt = boot(Arc::new(CallbackIdAllocator::default()));
        exec(
            &mut rt,
            r#"
            globalThis.__out = [];
            globalThis.__ran = typeof showActionSheet === 'function';
            if (__ran) {
                const base = globalThis.__allocId();
                showActionSheet({
                    itemList: ['a'],
                    success: function (r) { __out.push('first:' + r.tapIndex); },
                    fail: function () { __out.push('first:cancel'); },
                });
                showActionSheet({
                    itemList: ['b'],
                    success: function (r) { __out.push('second:' + r.tapIndex); },
                    fail: function () { __out.push('second:cancel'); },
                });

                // An id neither call holds settles neither, not the oldest.
                __hook('_internalOnActionSheetResult', [JSON.stringify({ requestId: base + 99, tapIndex: 0 })]);
                __hook('_internalOnActionSheetResult', [JSON.stringify({ requestId: base + 2, error: 'cancel' })]);
                __hook('_internalOnActionSheetResult', [JSON.stringify({ requestId: base + 1, tapIndex: 3 })]);
            }
            "#,
        );

        assert_js(
            &mut rt,
            "!__ran || __out.join('|') === 'second:cancel|first:3'",
        );
    }

    #[test]
    fn a_result_carrying_no_id_settles_nothing() {
        // A reply without an id names no request. Settling the oldest pending
        // one with it -- the fallback this replaced -- answered the wrong call
        // whenever two were in flight, and every host path now stamps the id,
        // so an id-less reply is discarded and the call it might have been for
        // is left to its own result.
        let mut rt = boot(Arc::new(CallbackIdAllocator::default()));
        exec(
            &mut rt,
            r#"
            globalThis.__out = [];
            globalThis.__ran = typeof openAppAuthorizeSetting === 'function';
            if (__ran) {
                const base = globalThis.__allocId();
                openAppAuthorizeSetting({ success: function () { __out.push('ok'); } });
                __hook('_internalOnOpenAppAuthorizeSettingFinished', [JSON.stringify({})]);
                __out.push('after-bare');
                __hook('_internalOnOpenAppAuthorizeSettingFinished',
                    [JSON.stringify({ requestId: base + 1 })]);
            }
            "#,
        );

        assert_js(&mut rt, "!__ran || __out.join('|') === 'after-bare|ok'");
    }

    #[test]
    fn two_deferred_apis_in_one_runtime_do_not_share_a_numbering() {
        // Each `createDeferredApi` used to count from 1 independently, so two of
        // them handed the platform the same id for different requests. They now
        // draw from the Host's one space.
        let mut rt = boot(Arc::new(CallbackIdAllocator::default()));
        exec(
            &mut rt,
            r#"
            globalThis.__other = globalThis.__createDeferredApi('other', 0);
            globalThis.__otherIds = [];
            globalThis.__other.invoke({}, function (_o, id) { __otherIds.push(id); })
                .catch(function () {});
            __call('mine');
            "#,
        );

        assert_js(&mut rt, "__otherIds[0] !== __ids[0]");
    }
}
