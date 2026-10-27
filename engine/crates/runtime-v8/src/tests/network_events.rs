//! What content's network listeners hear when the host's report changes: a status change when the connection or its
//! type changed, a weak-network change when the host's judgement did -- each only of its own.

#[cfg(all(test, feature = "api-sensors"))]
mod network_events_tests {
    use std::sync::{Arc, Mutex};

    use deno_core::{FastString, JsRuntime, RuntimeOptions};
    use shared::services::{
        CommerceServices, ConnectivityServices, MediaServices, NetworkService, SensorServices,
        ServiceError, SystemUtilServices,
    };

    use super::super::support::test_host_state;

    /// The host's last report, which the test rewrites between events.
    #[derive(Default)]
    struct Report(Mutex<String>);
    impl NetworkService for Report {
        fn start_monitoring(&self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn stop_monitoring(&self) -> Result<(), ServiceError> {
            Ok(())
        }
        fn get_network_type_json(&self) -> Result<String, ServiceError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    struct Device(Arc<Report>);
    impl SensorServices for Device {}
    impl MediaServices for Device {}
    impl CommerceServices for Device {}
    impl SystemUtilServices for Device {}
    impl ConnectivityServices for Device {
        fn network(&self) -> Option<Arc<dyn NetworkService>> {
            Some(Arc::clone(&self.0) as Arc<dyn NetworkService>)
        }
    }

    fn report(network_type: &str, connected: bool, weak: bool) -> String {
        format!(
            r#"{{"networkType":"{network_type}","isConnected":{connected},"signalStrength":0,"hasSystemProxy":false,"weakNet":{weak}}}"#
        )
    }

    #[test]
    fn each_listener_hears_only_the_change_that_is_its_own() {
        let v8_executor = crate::tests::support::v8_test_executor();
        let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
        let host = Arc::new(Report::default());
        *host.0.lock().unwrap() = report("wifi", true, false);
        let mut state = test_host_state();
        state.device_services = Some(Arc::new(Device(Arc::clone(&host))));
        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: crate::main_extensions(state),
            ..Default::default()
        });
        let mut run = |script: &str| {
            runtime
                .execute_script(
                    "<test:network-events>",
                    FastString::from(script.to_string()),
                )
                .expect("the script runs");
        };
        run(r#"
            globalThis.log = [];
            globalThis.bridge = globalThis[Symbol.for('Migo.hostBridge')];
            migo.onNetworkStatusChange((res) => log.push(`status ${res.networkType} ${res.isConnected}`));
            migo.onNetworkWeakChange((res) => log.push(`weak ${res.weakNet} ${res.networkType}`));
        "#);

        // The judgement alone changed: only the weak-network listener hears it.
        *host.0.lock().unwrap() = report("wifi", true, true);
        run("bridge._internalTriggerNetworkStatusChange(true, 'wifi');");
        // The type changed, the judgement did not: only the status listener.
        *host.0.lock().unwrap() = report("4g", true, true);
        run("bridge._internalTriggerNetworkStatusChange(true, '4g');");
        // A report with nothing new for either: nobody.
        run("bridge._internalTriggerNetworkStatusChange(true, '4g');");
        // Both changed: both, once each.
        *host.0.lock().unwrap() = report("none", false, false);
        run("bridge._internalTriggerNetworkStatusChange(false, 'none');");

        run(r#"
            const expected = ['weak true wifi', 'status 4g true', 'status none false', 'weak false none'];
            if (JSON.stringify(log) !== JSON.stringify(expected)) throw new Error(JSON.stringify(log));
        "#);
    }
}
