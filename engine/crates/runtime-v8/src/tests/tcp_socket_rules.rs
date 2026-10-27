//! A TCPSocket's connection limits, as the reference states them (createTCPSocket, "连接限制"): at most twenty
//! created in any five minutes, and none connected to a forbidden port.

use super::support::{boot_with_services, run_to_idle};

/// The twenty-first socket of five minutes is made -- a factory that threw would stop a game that does not expect it
/// -- and its connect fails through onError, saying why. The twentieth is still within the quota: its connect reaches
/// the engine, which refuses the port before any connection is attempted.
#[test]
fn the_twenty_first_socket_fails_and_a_forbidden_port_is_refused() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let mut runtime = boot_with_services(None, None);
    let said = run_to_idle(
        &mut runtime,
        r#"
            globalThis.__said = {};
            const sockets = [];
            for (let i = 0; i < 21; i += 1) sockets.push(migo.createTCPSocket());
            sockets[20].onError((res) => { globalThis.__said.overQuota = res.errMsg; });
            sockets[20].connect({ address: 'example.com', port: 9000 });
            sockets[19].onError((res) => { globalThis.__said.forbiddenPort = res.errMsg; });
            sockets[19].connect({ address: 'example.com', port: 443 });
        "#,
        "JSON.stringify(globalThis.__said)",
    );
    let said: deno_core::serde_json::Value = deno_core::serde_json::from_str(&said).unwrap();
    assert_eq!(
        said["overQuota"],
        "connect:fail more than 20 TCPSockets were created in 5 minutes"
    );
    assert_eq!(
        said["forbiddenPort"],
        "connect:fail port 443 is not allowed"
    );
}
