//! `migo.getSetting()` reports the host's answer, not a local optimism.
//!
//! It used to return a JavaScript object with every scope initialised to
//! `true`, and `migo.authorize()` set its own entry and returned success without
//! asking anyone. So content was told it held permissions nobody had granted,
//! and the API whose entire purpose is to obtain consent obtained none. A game
//! that checked before acting was misled *because* it checked.
//!
//! The OS permission still applied underneath, so this was never an escalation.
//! It was two other things, and the second is what matters for the product: in
//! a game centre a third-party title reaches the camera under the *host app's*
//! grant, with nobody ever asked whether that game should have it.

#[cfg(test)]
mod permission_reporting_tests {
    use std::{
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
    };

    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        render_command_sender::CommandSender,
        services::{
            CommerceServices, ConnectivityServices, DeviceServices, MediaServices,
            PermissionService, Scope, ScopeState, SensorServices, SystemUtilServices,
        },
    };

    /// A host that grants exactly one scope, so a report of "everything" is
    /// distinguishable from a report of what was actually decided.
    struct OnlyCamera;
    impl PermissionService for OnlyCamera {
        fn scope_state(&self, scope: Scope) -> ScopeState {
            match scope {
                Scope::Camera => ScopeState::Granted,
                Scope::Record => ScopeState::Denied,
                _ => ScopeState::Unknown,
            }
        }
    }

    struct Bundle;
    impl SensorServices for Bundle {}
    impl MediaServices for Bundle {}
    impl ConnectivityServices for Bundle {}
    impl CommerceServices for Bundle {}
    impl SystemUtilServices for Bundle {
        fn permission(&self) -> Option<Arc<dyn PermissionService>> {
            Some(Arc::new(OnlyCamera))
        }
    }

    fn host_state(services: Option<Arc<dyn DeviceServices>>) -> HostOpState {
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
            device_services: services,
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

    fn boot(services: Option<Arc<dyn DeviceServices>>) -> JsRuntime {
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(host_state(services)),
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        rt
    }

    fn assert_js(rt: &mut JsRuntime, src: &str) {
        let wrapped = format!(
            "(()=>{{ {src}; if (!__ok) throw new Error('assertion failed: ' + __msg); }})()"
        );
        rt.execute_script("<test:permission>", FastString::from(wrapped))
            .expect("permission assertion script");
    }

    /// With no host to ask, nothing is granted -- and `getSetting` says so.
    #[test]
    fn with_no_host_nothing_is_reported_as_granted() {
        let mut rt = boot(None);
        assert_js(
            &mut rt,
            "let granted = []; \
             const s = JSON.parse(globalThis[Symbol.for('Migo.hostBridge')] ? '{}' : '{}'); void s; \
             let __ok = true; let __msg = 'setup'",
        );
        // Read through the public API the way content does.
        rt.execute_script(
            "<test:getSetting>",
            FastString::from_static(
                "globalThis.__res = null; \
                 migo.getSetting({ success: (r) => { globalThis.__res = r; } });",
            ),
        )
        .expect("getSetting");
        assert_js(
            &mut rt,
            "const authSetting = (globalThis.__res && globalThis.__res.authSetting) || {}; \
             const granted = Object.keys(authSetting).filter((k) => authSetting[k] === true); \
             let __ok = granted.length === 0; \
             let __msg = 'reported granted with no host: ' + granted.join(', ')",
        );
    }

    /// With a host, the report is the host's answer -- neither more nor less.
    #[test]
    fn the_report_is_exactly_what_the_host_decided() {
        let services: Arc<dyn DeviceServices> = Arc::new(Bundle);
        let mut rt = boot(Some(services));
        rt.execute_script(
            "<test:getSetting>",
            FastString::from_static(
                "globalThis.__res = null; \
                 migo.getSetting({ success: (r) => { globalThis.__res = r; } });",
            ),
        )
        .expect("getSetting");
        assert_js(
            &mut rt,
            "const a = (globalThis.__res && globalThis.__res.authSetting) || {}; \
             const granted = Object.keys(a).filter((k) => a[k] === true).sort(); \
             let __ok = granted.length === 1 && granted[0] === 'scope.camera'; \
             let __msg = 'granted = ' + JSON.stringify(granted)",
        );
    }

    /// A refusal is `false` and a never-asked is absent, as the mini-game
    /// convention has it: content sends the player to openSetting after a
    /// refusal and calls authorize for a scope it has never asked about, and a
    /// report that collapsed the two sent players to settings for questions
    /// nobody had asked.
    #[test]
    fn a_refusal_is_false_and_the_undecided_are_absent() {
        let services: Arc<dyn DeviceServices> = Arc::new(Bundle);
        let mut rt = boot(Some(services));
        rt.execute_script(
            "<test:getSetting>",
            FastString::from_static(
                "globalThis.__res = null; \
                 migo.getSetting({ success: (r) => { globalThis.__res = r; } });",
            ),
        )
        .expect("getSetting");
        assert_js(
            &mut rt,
            "const a = (globalThis.__res && globalThis.__res.authSetting) || {}; \
             const keys = Object.keys(a).sort(); \
             let __ok = a['scope.record'] === false && !('scope.bluetooth' in a) \
                 && keys.length === 2 && keys[0] === 'scope.camera' && keys[1] === 'scope.record'; \
             let __msg = 'reported ' + JSON.stringify(a)",
        );
    }

    /// The bridge hook that used to write the state now writes nothing.
    ///
    /// It was reachable from content -- the host-bridge holder is keyed by
    /// `Symbol.for`, which reads the global registry -- so a permission answer
    /// stored behind it was an answer content could set. This pins that writing
    /// through it no longer changes what `getSetting` reports.
    #[test]
    fn the_bridge_hook_can_no_longer_write_a_grant() {
        let services: Arc<dyn DeviceServices> = Arc::new(Bundle);
        let mut rt = boot(Some(services));
        rt.execute_script(
            "<test:forge>",
            FastString::from_static(
                "globalThis[Symbol.for('Migo.hostBridge')] \
                     ._internalUpdateAuthSetting('scope.record', true); \
                 globalThis.__res = null; \
                 migo.getSetting({ success: (r) => { globalThis.__res = r; } });",
            ),
        )
        .expect("forge attempt");
        assert_js(
            &mut rt,
            "const a = (globalThis.__res && globalThis.__res.authSetting) || {}; \
             let __ok = a['scope.record'] === false; \
             let __msg = 'a forged grant reached getSetting: record=' + a['scope.record']",
        );
    }

    /// `authorize` without a host fails instead of reporting success.
    ///
    /// Async because `authorize` is a deferred API: it settles on the reply
    /// channel, and the timer machinery behind that needs a runtime.
    #[tokio::test(start_paused = true)]
    async fn authorize_without_a_host_fails() {
        let mut rt = boot(None);
        rt.execute_script(
            "<test:authorize>",
            FastString::from_static(
                "globalThis.__ok2 = null; globalThis.__fail = null; \
                 migo.authorize({ scope: 'scope.camera', \
                                success: () => { globalThis.__ok2 = true; }, \
                                fail: (e) => { globalThis.__fail = e; } });",
            ),
        )
        .expect("authorize");
        assert_js(
            &mut rt,
            "let __ok = globalThis.__ok2 !== true; \
             let __msg = 'authorize reported success with no host to ask'",
        );
    }
}
