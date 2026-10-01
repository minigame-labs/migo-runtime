//! What a call that cannot be served answers.
//!
//! Found by probing every plain asynchronous `migo.*` API once on macOS (a host without most device services): three
//! permission checks answered `fail: _authSetting is not defined` -- a ReferenceError from a variable the
//! host-owns-the-answer refactor removed and these functions still named -- and every unsupported API said its own name
//! twice, `getClipboardData:fail getClipboardData:fail not supported`.

#[cfg(all(test, feature = "api-system", feature = "api-sensors"))]
mod api_failure_tests {
    use deno_core::{FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions};

    use super::super::support::test_host_state;

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
                "<test:api>",
                FastString::from(format!("globalThis.log = [];\n{source}")),
            )
            .unwrap_or_else(|error| panic!("{error}"));
        executor
            .block_on(runtime.run_event_loop(PollEventLoopOptions::default()))
            .expect("the event loop settles");
        let check = format!(
            "(() => {{ const log = globalThis.log; \
             const fail = (why) => {{ throw new Error(why + ': ' + JSON.stringify(log)); }}; {check} }})()"
        );
        runtime
            .execute_script("<test:api-check>", FastString::from(check))
            .unwrap_or_else(|error| panic!("{error}"));
    }

    /// The permission checks answer from the host's state: a host that granted nothing says "not allowed", not a
    /// ReferenceError.
    #[test]
    fn the_permission_checks_answer_from_the_hosts_state() {
        run(
            r#"
            for (const name of ["checkUserLocation", "checkWritePhotosAlbum"]) {
                migo[name]({
                    success: (res) => log.push({ name, ok: true, res }),
                    fail: (err) => log.push({ name, ok: false, errMsg: err.errMsg }),
                });
            }
        "#,
            r#"
            for (const name of ["checkUserLocation", "checkWritePhotosAlbum"]) {
                const entry = log.find((e) => e.name === name);
                if (!entry) fail(name + " never answered");
                if (!entry.ok) fail(name + " failed");
                const scope = name === "checkUserLocation" ? "scope.userLocation" : "scope.writePhotosAlbum";
                if (entry.res.authSetting[scope] !== false) fail(name + " reports a grant nobody made");
            }
        "#,
        );
    }

    /// `getWritePhotosAlbum` asks the host (it used to set its own map entry to true, granting itself the permission),
    /// and answers under its own name.
    #[test]
    fn getwritephotosalbum_asks_the_host_and_answers_under_its_own_name() {
        run(
            r#"
            migo.getWritePhotosAlbum({
                success: (res) => log.push({ ok: true, errMsg: res.errMsg }),
                fail: (err) => log.push({ ok: false, errMsg: err.errMsg }),
                complete: (res) => log.push({ complete: res.errMsg }),
            });
        "#,
            r#"
            const answered = log.find((e) => e.ok !== undefined);
            if (!answered) fail("never answered");
            if (!/^getWritePhotosAlbum:/.test(answered.errMsg)) fail("answered under another API's name");
            if (answered.errMsg.includes("_authSetting")) fail("the ReferenceError");
            const complete = log.find((e) => e.complete !== undefined);
            if (!complete || !/^getWritePhotosAlbum:/.test(complete.complete)) fail("complete under its own name");
        "#,
        );
    }

    /// An API a host does not serve names itself once in its failure.
    #[test]
    fn an_unsupported_api_names_itself_once_in_its_failure() {
        run(
            r#"
            for (const name of ["getClipboardData", "vibrateShort", "showToast", "getLocalIPAddress"]) {
                migo[name]({ fail: (err) => log.push({ name, errMsg: err.errMsg }) });
            }
        "#,
            r#"
            if (log.length !== 4) fail("every unsupported API fails through its callback");
            for (const entry of log) {
                const doubled = entry.errMsg.split(entry.name + ":fail").length - 1;
                if (!entry.errMsg.startsWith(entry.name + ":fail ")) fail(entry.name + " has the wrong prefix");
                if (doubled !== 1) fail(entry.name + " names itself " + doubled + " times");
            }
        "#,
        );
    }
}
