//! `restartMiniProgram`, as content calls it: the restart the host is asked for carries the query `path` names.

#[cfg(test)]
mod restart_tests {
    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::protocol::host_cmd::HostCommand;

    use super::super::support::test_host_state;

    /// The part of `path` after its `?` is the query the restarted game is launched with (the reference: it "becomes
    /// the query"); a path with no `?` launches with the empty query, and no path keeps the one the game was last shown
    /// with. The synchronous form takes the same options.
    #[test]
    fn a_restart_carries_the_query_its_path_names() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let (host_tx, _critical, mut host_rx) = shared::host_channel::channel(8);
        let mut state = test_host_state();
        state.host_tx = host_tx;
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(state),
            ..Default::default()
        });
        runtime
            .execute_script(
                "<test:restart>",
                FastString::from_static(
                    r#"
                    migo.restartMiniProgram({ path: "index?level=2&mode=hard", success() {} });
                    migo.restartMiniProgram({ path: "index", success() {} });
                    migo.restartMiniProgram({ success() {} });
                    migo.restartMiniProgramSync({ path: "?from=menu" });
                    "#,
                ),
            )
            .expect("the restarts are asked for");
        let mut queries = Vec::new();
        while let Ok(command) = host_rx.try_recv() {
            match command {
                HostCommand::Restart { launch_query } => queries.push(launch_query),
                other => panic!("only restarts, not {other:?}"),
            }
        }
        assert_eq!(
            queries,
            [
                Some("level=2&mode=hard".to_owned()),
                Some(String::new()),
                None,
                Some("from=menu".to_owned()),
            ]
        );
    }
}
