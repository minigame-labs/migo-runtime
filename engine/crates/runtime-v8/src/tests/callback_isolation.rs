//! A caller's callback that throws must not decide whether the others run.
//!
//! `success`, `fail` and `complete` are the app's functions. The engine calls
//! them in sequence and then settles its own promise, so before this was fixed
//! a `success` that threw took the rest of the sequence with it: `complete`
//! never ran, even though every mini-game API documents it as running either
//! way. Content that hides a loading spinner in `complete` left it on screen,
//! and the only clue was the app's own exception.
//!
//! The dispatch under test is the real `createDeferredApi`, settled the way the
//! platform settles it, so this asserts about the code that ships rather than a
//! reimplementation of it.
//!
//! The last test is the same property one layer earlier: a callback that is
//! never reached cannot honour any contract, and an API whose own failure
//! handler throws reaches none of them.

#[cfg(test)]
mod callback_isolation_tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::callback_id::CallbackIdAllocator;
    use shared::channel::ThreadWakeup;
    use shared::device::gpu_caps::GpuCaps;
    use shared::op_state::{AudioSender, HostOpState, NetworkPolicy};
    use shared::render_command_sender::CommandSender;

    deno_core::extension!(
        callback_isolation_bridge,
        deps = [host_v8_base],
        esm_entry_point = "ext:callback_isolation_bridge/bridge.js",
        esm = ["ext:callback_isolation_bridge/bridge.js" = {
            source = r#"
                import { createDeferredApi } from "ext:host_v8_base/02_async.js";
                globalThis.__createDeferredApi = createDeferredApi;
            "#
        },],
    );

    // These probes name ops that deliberately do not exist in the slim profile.
    // Keeping them in a full-only extension lets the profile-independent callback
    // tests boot against the exact slim surface instead of making test code widen it.
    #[cfg(all(feature = "api-media", feature = "api-system"))]
    deno_core::extension!(
        callback_error_bridge,
        deps = [callback_isolation_bridge],
        esm_entry_point = "ext:callback_error_bridge/bridge.js",
        esm = ["ext:callback_error_bridge/bridge.js" = {
            source = r#"
                import { op_audio_create_context, op_worker_terminate } from "ext:core/ops";
                globalThis.__audioOp = function () { return op_audio_create_context(1, 44100); };
                globalThis.__workerOp = function () { return op_worker_terminate(); };
            "#
        },],
    );

    fn test_host_state() -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: Arc::new(CallbackIdAllocator::default()),
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

    fn exec(rt: &mut JsRuntime, source: impl Into<String>) {
        rt.execute_script("<test:callbacks>", FastString::from(source.into()))
            .expect("callback probe script");
    }

    fn assert_js(rt: &mut JsRuntime, expression: &str) {
        let script = format!(
            "if (!({expression})) throw new Error('log=' + JSON.stringify(globalThis.__log));"
        );
        if let Err(error) = rt.execute_script("<test:callbacks>", FastString::from(script)) {
            panic!("assertion failed: {expression}\n{error}");
        }
    }

    /// Timeouts disabled, so nothing settles on a clock instead of on a result.
    fn boot() -> JsRuntime {
        let mut extensions = crate::main_extensions(test_host_state());
        extensions.push(callback_isolation_bridge::init());
        #[cfg(all(feature = "api-media", feature = "api-system"))]
        extensions.push(callback_error_bridge::init());
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });
        crate::harden_global_scope(&mut rt);
        exec(
            &mut rt,
            r#"
            globalThis.__log = [];
            globalThis.__api = globalThis.__createDeferredApi('probe', 0);
            globalThis.__ids = [];
            "#,
        );
        rt
    }

    /// A `success` that throws must not swallow `complete`.
    #[test]
    fn a_throwing_success_callback_still_runs_complete() {
        let mut rt = boot();
        exec(
            &mut rt,
            r#"
            globalThis.__api.invoke(
                {
                    success: function () {
                        globalThis.__log.push('success');
                        throw new Error('boom');
                    },
                    complete: function () { globalThis.__log.push('complete'); },
                },
                function (_opts, id) { globalThis.__ids.push(id); },
            );
            "#,
        );
        exec(
            &mut rt,
            "globalThis.__api.settle(JSON.stringify({ requestId: globalThis.__ids[0] }));",
        );
        assert_js(&mut rt, "globalThis.__log.join(',') === 'success,complete'");
    }

    /// The same on the failure path: a throwing `fail` must not swallow
    /// `complete` either. Separate from the success case because the two are
    /// separate branches, and a fix that reached only one would still pass the
    /// other test.
    #[test]
    fn a_throwing_fail_callback_still_runs_complete() {
        let mut rt = boot();
        exec(
            &mut rt,
            r#"
            globalThis.__api.invoke(
                {
                    fail: function () {
                        globalThis.__log.push('fail');
                        throw new Error('boom');
                    },
                    complete: function () { globalThis.__log.push('complete'); },
                },
                function (_opts, id) { globalThis.__ids.push(id); },
            );
            "#,
        );
        exec(
            &mut rt,
            "globalThis.__api.settle(JSON.stringify({ requestId: globalThis.__ids[0], \
             error: 'probe:fail nope' }));",
        );
        assert_js(&mut rt, "globalThis.__log.join(',') === 'fail,complete'");
    }

    /// A failing op must reach `fail` and `complete`, not throw past them.
    ///
    /// `setInnerAudioOption` is backed by an `#[op2(fast)]` that returns
    /// `Err(AudioError)` whenever the host offers no audio platform -- true of
    /// this harness and of any real platform without one. That error arrives in
    /// JS as literal `undefined`, so the handler's `(err.message || ...)` threw
    /// a TypeError out of the API itself: content got an uncaught exception
    /// from a call documented to report failure through `fail`. Verified
    /// against the shipped 0.9.1 Linux SDK before the fix:
    ///
    /// ```text
    /// THREW OUT OF setInnerAudioOption: TypeError: Cannot read properties of
    /// undefined (reading 'message')
    /// ```
    #[cfg(feature = "api-media")]
    #[test]
    fn a_failing_op_reports_through_fail_rather_than_throwing() {
        let mut rt = boot();
        exec(
            &mut rt,
            r#"
            globalThis.__threw = false;
            try {
                migo.setInnerAudioOption({
                    fail: function (res) {
                        globalThis.__log.push('fail:' + (res && typeof res.errMsg));
                    },
                    complete: function () { globalThis.__log.push('complete'); },
                });
            } catch (e) {
                globalThis.__threw = true;
            }
            "#,
        );
        assert_js(
            &mut rt,
            "globalThis.__threw === false \
             && globalThis.__log.join(',') === 'fail:string,complete'",
        );
    }

    /// An op's error must reach JS as a constructible Error, not as `undefined`.
    ///
    /// `#[class("AudioError")]` on the Rust side only names the class; deno_core
    /// still needs a JS constructor registered under that exact name to build
    /// the object. `IOError` had one (`02_file_manager.js`), `AudioError` and
    /// `WorkerError` did not, so their ops threw literal `undefined` -- which is
    /// what made `setInnerAudioOption` throw a TypeError out of itself, and what
    /// would still leave every audio and worker failure carrying an errMsg of
    /// "undefined" now that #102 has made that non-fatal.
    ///
    /// Asserted per class rather than in one loop: each name is registered
    /// separately, so one that is missed has to fail on its own.
    #[cfg(all(feature = "api-media", feature = "api-system"))]
    #[test]
    fn a_failing_op_error_is_constructible_in_js() {
        let mut rt = boot();
        for (probe, class) in [("__audioOp", "AudioError"), ("__workerOp", "WorkerError")] {
            exec(
                &mut rt,
                &format!(
                    r#"
                    globalThis.__log = [];
                    try {{
                        globalThis.{probe}();
                        globalThis.__log.push('no-throw');
                    }} catch (e) {{
                        globalThis.__log.push(
                            (e === undefined ? 'undefined' : (e.name || 'unnamed'))
                            + '/' + (typeof (e && e.message)));
                    }}
                    "#
                ),
            );
            assert_js(
                &mut rt,
                &format!("globalThis.__log.join(',') === '{class}/string'"),
            );
        }
    }

    /// Every asynchronous API answers by callback or by Promise, by the common
    /// mini-game platform's rule:
    /// options carrying `success`, `fail` or `complete` are answered through
    /// them and the call returns nothing; options carrying none get a Promise.
    ///
    /// The sign-in APIs once broke the Promise half -- `login`, `getUserInfo`
    /// and `getPhoneNumber` returned `undefined`, so `await migo.login()`
    /// resolved on nothing before the call had finished -- and then, with every
    /// other API, broke the callback half: a Promise returned to callback-style
    /// content that nobody held turned each handled failure into an unhandled
    /// rejection.
    #[cfg(feature = "api-connectivity")]
    #[test]
    fn the_login_apis_answer_by_callback_or_by_promise() {
        let mut rt = boot();
        for api in [
            "login",
            "getUserInfo",
            "getPhoneNumber",
            "checkSession",
            "getUserProfile",
        ] {
            exec(
                &mut rt,
                &format!(
                    r#"
                    globalThis.__log = [];
                    var withCallbacks = migo.{api}({{ desc: 'd', fail: function () {{}}, complete: function () {{}} }});
                    globalThis.__log.push(withCallbacks === undefined ? 'nothing' : 'RETURNED');
                    var without = migo.{api}({{ desc: 'd' }});
                    globalThis.__log.push(without && typeof without.then === 'function' ? 'thenable' : 'MISSING');
                    if (without && without.catch) {{ without.catch(function () {{}}); }}
                    "#
                ),
            );
            assert_js(&mut rt, "globalThis.__log.join(',') === 'nothing,thenable'");
        }
    }
}
