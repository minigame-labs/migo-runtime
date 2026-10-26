//! Real V8 ops and the embedded facade share the service's network cancel id.
//! These cancellation paths terminate before DNS; no loopback policy bypass.

use deno_core::{JsRuntime, RuntimeOptions};
use shared::network_policy::NetworkPolicy;

use super::support::{test_host_state, v8_test_executor};

#[test]
fn closing_connecting_tasks_reclaims_both_resource_tables_and_stays_silent() {
    let executor = v8_test_executor();
    let _entered = executor.as_ref().map(|executor| executor.enter());
    let mut state = test_host_state();
    state.network_policy = NetworkPolicy::unrestricted();
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: crate::main_extensions(state),
        ..Default::default()
    });
    let resources = runtime
        .op_state()
        .borrow()
        .borrow::<crate::network::fetch::NetworkResources>()
        .0
        .clone();
    let baseline = runtime.op_state().borrow().resource_table.names().count();
    runtime
        .execute_script(
            "<test:close-connecting>",
            r#"
            globalThis.socketCallbacks = 0;
            for (let i = 0; i < 32; ++i) {
                const unexpected = () => socketCallbacks++;
                const task = migo.connectSocket({
                    url: "wss://not-resolved.invalid/game",
                    success: unexpected, fail: unexpected, complete: unexpected,
                    perMessageDeflate: true,
                });
                task.onOpen(unexpected);
                task.onError(unexpected);
                task.onClose(unexpected);
                task.close();
            }
            "#,
        )
        .unwrap();
    assert!(
        resources.is_empty(),
        "close must release cancel ids immediately"
    );
    executor.as_ref().unwrap().block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            runtime.run_event_loop(Default::default()),
        )
        .await
        .expect("cancelled connections cannot wait for DNS or their deadline")
        .unwrap();
    });
    runtime
        .execute_script(
            "<test:close-connecting-result>",
            "if (socketCallbacks !== 0) throw new Error('post-close callback');",
        )
        .unwrap();
    assert!(resources.is_empty());
    assert_eq!(
        runtime.op_state().borrow().resource_table.names().count(),
        baseline
    );
}
