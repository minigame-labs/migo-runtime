//! Tests for the boot-prelude script execution path.
//!
//! Boot prelude scripts are short JS snippets the host runs before
//! `EvaluateModule`. They are configured via `InitOptions::with_prelude_script`
//! and dispatched through `HostJsRuntime::exec_script_owned`. The intended
//! use case is the migo-adapter IIFE bundle, which wires browser-style
//! BOM/DOM globals on top of `migo.*`.
//!
//! These tests pin two contracts the rest of the system depends on:
//!
//! 1. A script executed via `JsRuntime::execute_script` with an *owned*
//!    `String` name (the path our `exec_script_owned` takes) is observable
//!    from a *separate* later script — globals persist across calls.
//! 2. Multiple prelude scripts execute in declaration order, so a later
//!    prelude can rely on globals an earlier one set up.
//!
//! Implementation note: deno_core 0.385 / v8 145 doesn't expose a
//! convenient `handle_scope()` getter on `JsRuntime`, so we encode
//! assertions as JS `throw`s. A failing assertion turns into
//! `execute_script` returning `Err`, which the test then surfaces.

#[cfg(test)]
mod prelude_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};

    /// Run an assertion script. The script should `throw` on failure so
    /// `execute_script` returns `Err` — the test fails with that error.
    fn assert_js(rt: &mut JsRuntime, src: &str) {
        let wrapped = format!(
            "(()=>{{ {src}; if (!__ok) throw new Error('assertion failed: ' + __msg); }})()"
        );
        rt.execute_script("<test:assert>", FastString::from(wrapped))
            .expect("assertion script");
    }

    /// Globals written by an owned-name prelude must remain visible to a
    /// subsequent call. This is the same contract the host relies on
    /// when running prelude before EvaluateModule.
    #[test]
    fn owned_name_prelude_globals_persist() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let mut rt = JsRuntime::new(RuntimeOptions::default());

        // Mimic exec_script_owned: build name from a runtime String, pass
        // FastString::from(String) to execute_script.
        let name = String::from("<prelude:adapter>");
        let source = "globalThis.__from_prelude = 'ok'; globalThis.__answer = 42;";
        rt.execute_script(name, FastString::from(source.to_string()))
            .expect("owned-name prelude executes");

        // Main script runs separately and observes prelude globals.
        let main = "globalThis.__main_saw = globalThis.__from_prelude;";
        rt.execute_script("game.js", FastString::from_static(main))
            .expect("main script executes");

        assert_js(
            &mut rt,
            "let __ok = globalThis.__from_prelude === 'ok' \
                       && globalThis.__main_saw === 'ok' \
                       && globalThis.__answer === 42; \
             let __msg = JSON.stringify({ \
                 from_prelude: globalThis.__from_prelude, \
                 main_saw: globalThis.__main_saw, \
                 answer: globalThis.__answer })",
        );
    }

    /// Multiple prelude scripts must run in declaration order so a later
    /// script can reference globals an earlier one created. The host
    /// iterates `InitOptions::prelude_scripts()` in order, so this test
    /// pins that the underlying V8 layer preserves visibility across
    /// independent execute_script calls.
    #[test]
    fn multiple_preludes_run_in_order() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let mut rt = JsRuntime::new(RuntimeOptions::default());

        let scripts: Vec<(String, &str)> = vec![
            ("<prelude:1>".to_string(), "globalThis.__seq = ['a'];"),
            ("<prelude:2>".to_string(), "globalThis.__seq.push('b');"),
            (
                "<prelude:3>".to_string(),
                "globalThis.__seq.push('c'); globalThis.__joined = globalThis.__seq.join(',');",
            ),
        ];
        for (name, source) in &scripts {
            rt.execute_script(name.clone(), FastString::from(source.to_string()))
                .expect("each prelude executes");
        }

        assert_js(
            &mut rt,
            "let __ok = globalThis.__joined === 'a,b,c'; \
             let __msg = 'joined=' + globalThis.__joined",
        );
    }

    /// A failing prelude returns an error at the point of failure, so the
    /// host's `?` can short-circuit before the main module loads. We are
    /// pinning that V8's surface gives us a fail-fast we can `?`-propagate
    /// on a per-script basis (not, e.g., a deferred microtask error that
    /// only surfaces during the event loop).
    #[test]
    fn prelude_syntax_error_fails_fast() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let mut rt = JsRuntime::new(RuntimeOptions::default());
        let name = String::from("<prelude:broken>");
        let result = rt.execute_script(name, FastString::from_static("this is not valid js (("));
        assert!(result.is_err(), "syntax error should fail synchronously");
    }
}
