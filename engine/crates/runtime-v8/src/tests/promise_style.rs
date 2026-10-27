//! A call answered by Promise because content passed no callbacks -- migo's rule for every asynchronous API -- to an
//! API the reference gives no Promise style. Content written for the reference does not look at what such a call
//! returns, so its failure is not an unhandled rejection: one was, and with no rejection listener the engine reports
//! an unhandled rejection to the game's `onError`, as an error the platform never raises.

#[cfg(test)]
mod promise_style_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};

    use super::super::support::test_host_state;

    /// Run `source` (which fills `globalThis.log`), settle the event loop, then run `check` against the log; a failed
    /// check throws, with the log in the message.
    fn run(source: &str, check: &str) {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        let _guard = executor.enter();
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state()),
            ..Default::default()
        });
        runtime
            .execute_script(
                "<test:promise-style>",
                FastString::from(format!("globalThis.log = [];\n{source}")),
            )
            .expect("script executes");
        executor
            .block_on(runtime.run_event_loop(Default::default()))
            .expect("the event loop settles");
        let check = format!(
            "(() => {{ const log = globalThis.log; \
             const fail = (why) => {{ throw new Error(why + ': ' + JSON.stringify(log)); }}; \
             {check} }})()"
        );
        runtime
            .execute_script("<test:promise-style-check>", FastString::from(check))
            .expect("what content saw is what it should have seen");
    }

    /// saveFileToDisk has no Promise style in the reference: a failure nobody waits for is not reported, and one that
    /// is waited for still rejects. getStorage has one, and its failure nobody waits for is unhandled, as on the
    /// platform.
    #[test]
    fn a_failure_of_an_api_without_a_promise_style_is_not_an_unhandled_rejection() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        run(
            r#"
            migo.onUnhandledRejection((event) => log.push("unhandled " + (event.reason && event.reason.errMsg)));
            migo.saveFileToDisk({});
            migo.saveFileToDisk({}).then(() => log.push("resolved"), (res) => log.push("rejected " + res.errMsg));
            migo.getStorage({ key: "promise-style-nothing" });
        "#,
            r#"
            if (!log.some((entry) => entry.startsWith("rejected saveFileToDisk:fail"))) fail("the awaited call rejects");
            if (log.some((entry) => entry.startsWith("unhandled saveFileToDisk"))) fail("the ignored call was reported");
            if (!log.some((entry) => entry.startsWith("unhandled getStorage:fail"))) {
                fail("a Promise-style API's ignored failure is unhandled");
            }
        "#,
        );
    }

    /// The same for a call whose Promise is made before its callbacks are known (`withCallbacks`): reportScene's own
    /// checks fail it before the report leaves.
    #[cfg(feature = "api-connectivity")]
    #[test]
    fn a_failure_checked_before_the_call_leaves_is_not_an_unhandled_rejection_either() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        run(
            r#"
            migo.onUnhandledRejection((event) => log.push("unhandled " + (event.reason && event.reason.errMsg)));
            migo.reportScene({});
            migo.reportScene({}).then(() => log.push("resolved"), (res) => log.push("rejected " + res.errMsg));
        "#,
            r#"
            if (!log.some((entry) => entry.startsWith("rejected reportScene:fail"))) fail("the awaited call rejects");
            if (log.some((entry) => entry.startsWith("unhandled reportScene"))) fail("the ignored call was reported");
        "#,
        );
    }
}
