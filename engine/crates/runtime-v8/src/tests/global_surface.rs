//! Integration tests for the game-visible global surface (P0 audit hardening).
//!
//! These tests boot a full runtime via `main_extensions()` so the runtime
//! ESM entry point (`99_main.js`) executes and registers the real global
//! scope. They pin the contract that a security audit dumping
//! `Object.getOwnPropertyNames(globalThis)` sees neither deno_core internals
//! (`Deno`, `__bootstrap`) nor the host-bridge hooks (`_internal*`), while the
//! host can still reach those hooks.
//!
//! These boot the runtime *without* `JsBindings`, which is the window in which
//! the `Symbol.for('Migo.hostBridge')` holder is still installed -- so they
//! reach hooks by name to stand in for the host. A real runtime resolves the
//! holder and deletes that name; see `tests/host_bridge_dispatch.rs`.

#[cfg(test)]
mod global_surface_tests {
    use std::{path::PathBuf, sync::Arc, sync::atomic::AtomicBool};

    use deno_core::{FastString, JsRuntime, RuntimeOptions, v8};
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        op_state::{AudioSender, HostOpState, NetworkPolicy},
        render_command_sender::CommandSender,
    };

    fn test_host_state() -> HostOpState {
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
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    /// Boot a runtime with the full main extension chain so `99_main.js` runs.
    ///
    /// Mirrors `HostJsRuntime::new`: the `Deno`/`__bootstrap` hardening is now
    /// applied at runtime via `crate::harden_global_scope` (not in 99_main.js, so
    /// it stays out of the V8 startup snapshot), so the test helper must apply it
    /// too to represent the real game-visible global surface.
    fn boot_runtime() -> JsRuntime {
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state()),
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        rt
    }

    /// Run an assertion script that should `throw` on failure, surfacing the
    /// message as a test failure (same pattern as tests_prelude).
    fn assert_js(rt: &mut JsRuntime, src: &str) {
        let wrapped = format!(
            "(()=>{{ {src}; if (!__ok) throw new Error('assertion failed: ' + __msg); }})()"
        );
        rt.execute_script("<test:assert>", FastString::from(wrapped))
            .expect("assertion script");
    }

    /// Every name this build publishes to content, pinned against a committed
    /// baseline.
    ///
    /// The C ABI has `scripts/test-c-abi-surface-candidate.sh` and the Android
    /// host API has `platforms/android/host-api-v0.txt`; this is the same gate
    /// for the third external binding, and the one games actually consume. The
    /// tests around it check named things -- these six core APIs exist, `Deno`
    /// does not -- which is a rule about the names someone thought to list. The
    /// `Deno` leak survived exactly that: the exclusion list was correct for the
    /// names on it. A whole-surface baseline has no list to be incomplete.
    ///
    /// **What this pins is the source-booted surface.** A shipped session boots
    /// from the V8 startup snapshot instead, and the two differ: snapshot boot
    /// does not publish `migo.Temporal` or `migo.Float16Array`, because `migo` is
    /// built by mirroring globalThis during bootstrap and is therefore frozen at
    /// snapshot-generation time. Pinning source boot is deliberate -- it needs no
    /// device, no GL and no CI plumbing, and it still catches every API added,
    /// removed or renamed, and every internal newly leaked onto either object.
    /// It is not a substitute for checking the shipped surface; see
    /// `scripts/dump-api-surface.sh`, which runs the real player.
    ///
    /// To change the surface on purpose:
    ///   MIGO_UPDATE_SURFACE_BASELINE=1 cargo test -p migo-runtime-v8 --lib \
    ///     --no-default-features --features profile-full published_surface
    ///   MIGO_UPDATE_SURFACE_BASELINE=1 cargo test -p migo-runtime-v8 --lib \
    ///     --no-default-features --features profile-slim published_surface
    /// and commit both profile-specific baselines with the change.
    #[test]
    fn published_surface_matches_the_committed_baseline() {
        #[cfg(feature = "profile-full")]
        let (baseline, baseline_name) = (
            include_str!("published_surface_full_v0.txt"),
            "published_surface_full_v0.txt",
        );
        #[cfg(feature = "profile-slim")]
        let (baseline, baseline_name) = (
            include_str!("published_surface_slim_v0.txt"),
            "published_surface_slim_v0.txt",
        );

        let mut rt = boot_runtime();
        let src = "JSON.stringify({g:Object.getOwnPropertyNames(globalThis).sort(), \
                    m:(typeof migo==='object'&&migo)?Object.getOwnPropertyNames(migo).sort():[]})";
        let dumped = rt
            .execute_script("<test:surface>", FastString::from(src.to_string()))
            .expect("surface dump script");
        let json = {
            let main_context = rt.main_context();
            let isolate = rt.v8_isolate();
            v8::scope_with_context!(scope, isolate, &main_context);
            let local = v8::Local::new(scope, dumped);
            local.to_rust_string_lossy(scope)
        };

        // Deliberately not serde: this file is a list of names, and a hand-rolled
        // split keeps the gate readable without a dependency on how the dump is
        // shaped.
        let names = |key: &str| -> Vec<String> {
            let head = format!("\"{key}\":[");
            let rest = &json[json.find(&head).expect("dump key") + head.len()..];
            let body = &rest[..rest.find(']').expect("dump array end")];
            body.split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim_matches('"').to_string())
                .collect()
        };
        let mut actual = vec!["[globalThis]".to_string()];
        actual.extend(names("g"));
        actual.push("[migo]".to_string());
        actual.extend(names("m"));

        if std::env::var("MIGO_UPDATE_SURFACE_BASELINE").is_ok() {
            let header: Vec<&str> = baseline
                .lines()
                .take_while(|l| l.starts_with('#'))
                .collect();
            let body = format!("{}\n{}\n", header.join("\n"), actual.join("\n"));
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src/tests")
                .join(baseline_name);
            std::fs::write(path, body).expect("rewrite profile-specific baseline");
            return;
        }

        let expected: Vec<&str> = baseline
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect();
        let actual_ref: Vec<&str> = actual.iter().map(String::as_str).collect();
        if actual_ref != expected {
            let added: Vec<&&str> = actual_ref
                .iter()
                .filter(|n| !expected.contains(n))
                .collect();
            let removed: Vec<&&str> = expected
                .iter()
                .filter(|n| !actual_ref.contains(n))
                .collect();
            panic!(
                "published JS surface changed for {baseline_name}.\n  added:   {added:?}\n  removed: {removed:?}\n\
                 If this is intended, regenerate with \
                 MIGO_UPDATE_SURFACE_BASELINE=1 and commit the diff."
            );
        }
    }

    /// A security audit dumps own property names of the global. After P0
    /// cleanup, none of the deno_core internals or host-bridge hooks may be
    /// reachable by string key.
    #[test]
    fn audit_dump_hides_internals() {
        let mut rt = boot_runtime();
        assert_js(
            &mut rt,
            "const keys = Object.getOwnPropertyNames(globalThis); \
             const bad = keys.filter(k => k === 'Deno' || k === '__bootstrap' \
                 || k === '_perf' || k === '_force_readback' \
                 || k === '__migo_frame_end_hooks' || k === '__migo_frame_end_all' \
                 || k.startsWith('_internal')); \
             let __ok = bad.length === 0; \
             let __msg = 'leaked global keys: ' + JSON.stringify(bad)",
        );
    }

    /// The host bridge holder must still expose the hooks under its Symbol key
    /// so both delivery channels (js_bindings lookup + eval) can reach them.
    #[test]
    fn host_bridge_holder_reachable() {
        let mut rt = boot_runtime();
        let optional_hooks = if cfg!(feature = "api-connectivity") && cfg!(feature = "api-commerce")
        {
            "&& typeof b._internalOnLoginResult === 'function' \
             && typeof b._internalOnMidasPaymentResult === 'function'"
        } else {
            "&& typeof b._internalOnLoginResult === 'undefined' \
             && typeof b._internalOnMidasPaymentResult === 'undefined'"
        };
        assert_js(
            &mut rt,
            &format!(
                "const b = globalThis[Symbol.for('Migo.hostBridge')]; \
             let __ok = !!b \
                 {optional_hooks} \
                 && typeof b._internalEnqueueRawTouchEvent === 'function'; \
             let __msg = 'holder=' + (b ? Object.getOwnPropertyNames(b).length + ' keys' : 'missing')"
            ),
        );
    }

    /// R6 named products must expose exactly the optional domains selected at
    /// compile time while retaining the same core game surface.
    #[test]
    fn product_profile_surface_matches_features() {
        let mut rt = boot_runtime();
        assert_js(
            &mut rt,
            "const coreNames = ['createCanvas', 'request', 'getFileSystemManager', \
                                'setStorage', 'onTouchStart', 'setTimeout']; \
             const missingMigo = coreNames.filter(k => typeof migo[k] === 'undefined'); \
             let __ok = missingMigo.length === 0; \
             let __msg = 'missing core=' + JSON.stringify(missingMigo)",
        );

        let expected = [
            ("startAccelerometer", cfg!(feature = "api-sensors")),
            ("createCamera", cfg!(feature = "api-media")),
            ("createInnerAudioContext", cfg!(feature = "api-media")),
            ("openBluetoothAdapter", cfg!(feature = "api-connectivity")),
            ("shareAppMessage", cfg!(feature = "api-commerce")),
            ("requestMidasPayment", cfg!(feature = "api-commerce")),
            ("showToast", cfg!(feature = "api-system")),
            ("createWorker", cfg!(feature = "api-system")),
            ("createBannerAd", cfg!(feature = "api-system")),
        ];
        for (name, should_exist) in expected {
            assert_js(
                &mut rt,
                &format!(
                    "const actual = typeof migo[{name:?}] !== 'undefined'; \
                     let __ok = actual === {should_exist}; \
                     let __msg = {name:?} + ' actual=' + actual + ' expected=' + {should_exist}"
                ),
            );
        }
    }

    /// Gamepad is a Web content capability, not a mini-game platform API: it must be present
    /// on `migo` and native bridge internals must stay off the public global.
    #[test]
    fn gamepad_transport_is_migo_only_and_native_hooks_remain_private() {
        let mut rt = boot_runtime();
        assert_js(
            &mut rt,
            "const names = ['getGamepads', 'onGamepadConnected', \
                            'offGamepadConnected', 'onGamepadDisconnected', \
                            'offGamepadDisconnected']; \
             const missingMigo = names.filter(k => typeof migo[k] !== 'function'); \
             const bridge = globalThis[Symbol.for('Migo.hostBridge')]; \
             let __ok = missingMigo.length === 0 \
                 && typeof bridge._internalTriggerGamepadConnected === 'function' \
                 && typeof globalThis._internalTriggerGamepadConnected === 'undefined'; \
             let __msg = 'missing migo=' + JSON.stringify(missingMigo)",
        );
    }

    #[test]
    fn gamepad_views_are_live_but_web_read_only() {
        let mut rt = boot_runtime();
        assert_js(
            &mut rt,
            "const bridge = globalThis[Symbol.for('Migo.hostBridge')]; \
             bridge._internalTriggerGamepadConnected(0, 'pad', 'standard', 2, 1); \
             const pad = migo.getGamepads()[0]; \
             const axes = pad.axes; const buttons = pad.buttons; \
             try { pad.connected = false; pad.timestamp = 99; \
                   axes[0] = 99; buttons[0].value = 99; } catch (_) {} \
             bridge._internalTriggerGamepadState(0, 42, [2, 1, 0.5, -0.25, 1, 1, 0.75]); \
             let __ok = Object.isFrozen(pad) && Object.isFrozen(axes) \
                 && Object.isFrozen(buttons) && Object.isFrozen(buttons[0]) \
                 && pad.connected === true \
                 && typeof pad.timestamp === 'number' && pad.timestamp >= 0 && pad.timestamp <= performance.now() \
                 && axes[0] === 0.5 && axes[1] === -0.25 \
                 && buttons[0].pressed === true \
                 && buttons[0].touched === true && buttons[0].value === 0.75; \
             let __msg = JSON.stringify(pad)",
        );
    }

    /// Web Gamepad slots are explicitly nullable. A controller may be assigned
    /// a non-zero stable index even when lower slots were never populated; a
    /// sparse JavaScript array would expose `undefined` instead of the Web
    /// contract's `null` for those slots.
    #[test]
    fn gamepad_slots_before_a_nonzero_index_are_explicitly_null() {
        let mut rt = boot_runtime();
        assert_js(
            &mut rt,
            "const bridge = globalThis[Symbol.for('Migo.hostBridge')]; \
             bridge._internalTriggerGamepadConnected(2, 'pad', 'standard', 2, 1); \
             const pads = migo.getGamepads(); \
             let __ok = pads.length === 3 && pads[0] === null && pads[1] === null \
                 && pads[2].index === 2; \
             let __msg = JSON.stringify(pads)",
        );
    }

    #[test]
    fn gamepad_listener_mutation_does_not_change_the_current_dispatch_set() {
        let mut rt = boot_runtime();
        assert_js(
            &mut rt,
            "const bridge = globalThis[Symbol.for('Migo.hostBridge')]; \
             const seen = []; \
             function second() { seen.push('second'); } \
             function first() { seen.push('first'); migo.offGamepadConnected(first); } \
             migo.onGamepadConnected(first); migo.onGamepadConnected(second); \
             bridge._internalTriggerGamepadConnected(0, 'pad', 'standard', 2, 1); \
             let __ok = seen.join(',') === 'first,second'; \
             let __msg = seen.join(',')",
        );
    }

    #[test]
    fn gamepad_listener_and_overridden_logger_failures_are_isolated() {
        let mut rt = boot_runtime();
        assert_js(
            &mut rt,
            "const bridge = globalThis[Symbol.for('Migo.hostBridge')]; \
             const originalError = console.error; let reached = false; let escaped = false; \
             migo.onGamepadConnected(() => { throw new Error('listener'); }); \
             migo.onGamepadConnected(() => { reached = true; }); \
             console.error = () => { throw new Error('logger'); }; \
             try { bridge._internalTriggerGamepadConnected(0, 'pad', 'standard', 2, 1); } \
             catch (_) { escaped = true; } finally { console.error = originalError; } \
             let __ok = reached && !escaped; \
             let __msg = 'reached=' + reached + ' escaped=' + escaped",
        );
    }

    /// What `console` writes is what the host log and DevTools buffer receive.
    /// An Error is a game's usual `console.error` argument, and JSON wrote one
    /// as `{}`: a failing game reported that it failed and nothing else.
    #[test]
    fn console_writes_an_error_as_its_message_and_stack() {
        let host_id = 9_401;
        let buffer = shared::console_log::register_console_log(host_id);
        crate::console::bind_thread_console(host_id);
        let mut rt = boot_runtime();
        rt.execute_script(
            "<test:console>",
            FastString::from(
                "function where() { return new TypeError('boom'); } \
                 console.error(where()); \
                 console.log(undefined, null, 1, { a: 1 }, 'text', Symbol('s'));"
                    .to_string(),
            ),
        )
        .expect("console script");
        let (entries, _) = buffer.lock().unwrap().read_since(0);
        shared::console_log::unregister_console_log(host_id);

        let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].starts_with("TypeError: boom\n") && messages[0].contains("where"),
            "the error lost its message or its stack: {:?}",
            messages[0]
        );
        assert_eq!(messages[1], "undefined null 1 {\"a\":1} text Symbol(s)");
    }

    /// GC APIs (03_gc.js) must work after switching off global `Deno.core`.
    #[test]
    fn gc_apis_work_without_global_deno() {
        let mut rt = boot_runtime();
        assert_js(
            &mut rt,
            "let __ok = typeof globalThis.triggerGC === 'function' \
                 && typeof globalThis.getHeapStatistics === 'function' \
                 && typeof globalThis.Deno === 'undefined'; \
             let __msg = 'triggerGC=' + typeof globalThis.triggerGC",
        );
    }

    /// A `touchend` must report only the pointers still on the surface in
    /// `touches`; the lifted finger belongs in `changedTouches` only. Native
    /// marks the lifted pointer with FLAG_REMOVED (bit 1) alongside FLAG_CHANGED
    /// (bit 0). Regression guard for games that detect "all fingers up" via
    /// `event.touches.length === 0`.
    #[tokio::test]
    async fn touchend_excludes_lifted_pointer_from_touches() {
        use deno_core::PollEventLoopOptions;
        use std::time::Duration;

        let mut rt = boot_runtime();

        rt.execute_script(
            "<test:setup>",
            FastString::from_static(
                "globalThis.__ev = null; \
                 globalThis.onTouchEnd((e) => { globalThis.__ev = e; });",
            ),
        )
        .expect("register touchend listener");

        // Two pointers in the raw buffer (stride 20): id 0 stays down (flags 0);
        // id 1 is the lifting finger (FLAG_CHANGED|FLAG_REMOVED = 3).
        rt.execute_script(
            "<test:enqueue>",
            FastString::from_static(
                "const b = globalThis[Symbol.for('Migo.hostBridge')]; \
                 const S = 20; const buf = new ArrayBuffer(2 * S); const dv = new DataView(buf); \
                 dv.setUint32(0, 0, true); dv.setFloat32(4, 10, true); dv.setFloat32(8, 20, true); dv.setFloat32(12, 1, true); dv.setUint32(16, 0, true); \
                 dv.setUint32(S, 1, true); dv.setFloat32(S + 4, 30, true); dv.setFloat32(S + 8, 40, true); dv.setFloat32(S + 12, 1, true); dv.setUint32(S + 16, 3, true); \
                 b._internalEnqueueRawTouchEvent(2, buf, 2, 123);",
            ),
        )
        .expect("enqueue raw touch");

        // Drain the microtask that runs `_drain`. No timers are pending, so one
        // brief poll delivers the event; the timeout guards against the loop
        // staying alive on open channels held by the op state.
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            rt.run_event_loop(PollEventLoopOptions::default()),
        )
        .await;

        assert_js(
            &mut rt,
            "const e = globalThis.__ev; \
             let __ok = !!e \
                 && e.touches.length === 1 && e.touches[0].identifier === 0 \
                 && e.changedTouches.length === 1 && e.changedTouches[0].identifier === 1; \
             let __msg = e ? ('touches=' + e.touches.length + ' changed=' + e.changedTouches.length) : 'no event delivered'",
        );
    }

    /// A touch listener that throws is the game's own bug: the listeners after it still hear the event, and
    /// the error is reported rather than swallowed (a handler that throws on every touch is otherwise a game
    /// that "ignores input", with nothing to say why).
    #[tokio::test]
    async fn a_throwing_touch_listener_is_reported_and_does_not_stop_the_others() {
        use deno_core::PollEventLoopOptions;
        use std::time::Duration;

        let mut rt = boot_runtime();

        rt.execute_script(
            "<test:setup>",
            FastString::from_static(
                "globalThis.__heard = 0; globalThis.__errors = []; \
                 console.error = (...args) => { globalThis.__errors.push(args.map(String).join(' ')); }; \
                 globalThis.onTouchStart(() => { throw new Error('a game bug in its touch handler'); }); \
                 globalThis.onTouchStart(() => { globalThis.__heard += 1; });",
            ),
        )
        .expect("register listeners");
        rt.execute_script(
            "<test:enqueue>",
            FastString::from_static(
                "const b = globalThis[Symbol.for('Migo.hostBridge')]; \
                 const buf = new ArrayBuffer(20); const dv = new DataView(buf); \
                 dv.setFloat32(4, 1, true); dv.setFloat32(8, 2, true); dv.setUint32(16, 1, true); \
                 b._internalEnqueueRawTouchEvent(0, buf, 1, 1);",
            ),
        )
        .expect("enqueue raw touch");
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            rt.run_event_loop(PollEventLoopOptions::default()),
        )
        .await;

        assert_js(
            &mut rt,
            "let __ok = globalThis.__heard === 1 \
                 && globalThis.__errors.some((m) => m.includes('a game bug in its touch handler')); \
             let __msg = 'heard=' + globalThis.__heard + ' errors=' + JSON.stringify(globalThis.__errors)",
        );
    }

    /// Touch dispatch must snapshot its listener set: a listener that registers
    /// another listener mid-dispatch must not cause the new one to run for the
    /// same event (standard event-dispatch semantics, avoids re-entrancy surprises).
    #[tokio::test]
    async fn touch_listener_set_is_snapshotted_during_dispatch() {
        use deno_core::PollEventLoopOptions;
        use std::time::Duration;

        let mut rt = boot_runtime();

        rt.execute_script(
            "<test:setup>",
            FastString::from_static(
                "globalThis.__calls = []; \
                 globalThis.onTouchEnd(function A() { \
                     globalThis.__calls.push('A'); \
                     globalThis.onTouchEnd(function B() { globalThis.__calls.push('B'); }); \
                 });",
            ),
        )
        .expect("register listener A");

        rt.execute_script(
            "<test:enqueue>",
            FastString::from_static(
                "const b = globalThis[Symbol.for('Migo.hostBridge')]; \
                 const S = 20; const buf = new ArrayBuffer(S); const dv = new DataView(buf); \
                 dv.setUint32(0, 7, true); dv.setFloat32(4, 1, true); dv.setFloat32(8, 2, true); dv.setFloat32(12, 1, true); dv.setUint32(16, 3, true); \
                 b._internalEnqueueRawTouchEvent(2, buf, 1, 1);",
            ),
        )
        .expect("enqueue raw touch");

        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            rt.run_event_loop(PollEventLoopOptions::default()),
        )
        .await;

        assert_js(
            &mut rt,
            "let __ok = globalThis.__calls.length === 1 && globalThis.__calls[0] === 'A'; \
             let __msg = 'calls=' + JSON.stringify(globalThis.__calls)",
        );
    }
}
