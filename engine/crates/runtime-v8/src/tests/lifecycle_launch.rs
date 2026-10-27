//! What a game is launched with: the host's options, installed before content runs (`_internalLaunch`), or -- from a
//! host that carries them on its shows instead -- its first show's. A show enters the game; it never relaunches it.

#[cfg(test)]
mod launch_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};

    use super::super::support::test_host_state;

    /// Run `checks` with `bridge` and `fail(why, value)` in scope; a failed check throws.
    fn run(checks: &str) {
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(test_host_state()),
            ..Default::default()
        });
        runtime
            .execute_script(
                "<test:launch>",
                FastString::from(format!(
                    "(() => {{ const bridge = globalThis[Symbol.for('Migo.hostBridge')]; \
                     const fail = (why, value) => {{ throw new Error(why + ': ' + JSON.stringify(value)); }}; \
                     {checks} }})()"
                )),
            )
            .expect("the game is launched with what the host gave");
    }

    /// Launched with the host's options, content reads them from its first line, and is entered with them; a show
    /// after it enters the game with its own and leaves the launch as it was.
    #[test]
    fn a_game_is_launched_with_the_host_s_options_and_a_show_only_enters_it() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        run(r#"
            bridge._internalLaunch({ scene: 1007, query: { level: "2" }, referrerInfo: { appId: "referrer.example" } });
            const launch = migo.getLaunchOptionsSync();
            if (launch.scene !== 1007 || launch.query.level !== "2") fail("the host's launch", launch);
            if (launch.referrerInfo.appId !== "referrer.example") fail("the host's referrer", launch);
            if (launch.shareTicket !== "" || launch.apiCategory !== "default") fail("the defaults", launch);
            if (migo.getEnterOptionsSync().scene !== 1007) fail("entered with it", migo.getEnterOptionsSync());
            bridge._internalTriggerOnShow({ scene: 1038, query: {} });
            if (migo.getLaunchOptionsSync().scene !== 1007) fail("a show relaunched it", migo.getLaunchOptionsSync());
            if (migo.getEnterOptionsSync().scene !== 1038) fail("a show enters it", migo.getEnterOptionsSync());
        "#);
    }

    /// A host that gives no options at launch and carries them on its shows launches the game with the first show's.
    #[test]
    fn a_game_launched_without_options_is_launched_with_its_first_show_s() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        run(r#"
            if (migo.getLaunchOptionsSync().scene !== 1001) fail("the defaults before a show", migo.getLaunchOptionsSync());
            bridge._internalTriggerOnShow({ scene: 1011, query: { from: "share" } });
            bridge._internalTriggerOnShow({ scene: 1038 });
            const launch = migo.getLaunchOptionsSync();
            if (launch.scene !== 1011 || launch.query.from !== "share") fail("the first show's launch", launch);
            if (migo.getEnterOptionsSync().scene !== 1038) fail("the last show's entry", migo.getEnterOptionsSync());
        "#);
    }
}
