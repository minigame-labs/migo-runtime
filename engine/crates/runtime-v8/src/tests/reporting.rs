//! What the game reports -- realtime logs, analytics -- reaches the host, and
//! the secure random source is the OS's.

use std::sync::{Arc, Mutex};

use deno_core::serde_json::{self, Value};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, EcosystemService, GameLogService,
        MediaServices, SensorServices, SystemUtilServices,
    },
};

use super::support::{SETTLE, boot_with_services, run_to_idle};

#[derive(Default)]
struct FakeHost {
    logs: Mutex<Vec<Value>>,
    reports: Mutex<Vec<Value>>,
}

impl GameLogService for FakeHost {
    fn report_log(&self, log_json: &str) -> Result<(), ServiceError> {
        self.logs
            .lock()
            .unwrap()
            .push(serde_json::from_str(log_json).unwrap());
        Ok(())
    }
}

impl EcosystemService for FakeHost {
    fn call(&self, request_json: &str) -> Result<(), ServiceError> {
        self.reports
            .lock()
            .unwrap()
            .push(serde_json::from_str(request_json).unwrap());
        Ok(())
    }
}

struct Host(Arc<FakeHost>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {
    fn game_log(&self) -> Option<Arc<dyn GameLogService>> {
        Some(self.0.clone())
    }
}
impl SystemUtilServices for Host {
    fn ecosystem(&self) -> Option<Arc<dyn EcosystemService>> {
        Some(self.0.clone())
    }
}

fn boot(host: Arc<FakeHost>) -> deno_core::JsRuntime {
    boot_with_services(Some(Arc::new(Host(host)) as Arc<dyn DeviceServices>), None)
}

#[test]
fn realtime_logs_reach_the_host_s_game_log_with_their_filters() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeHost::default());
    let mut runtime = boot(host.clone());
    run_to_idle(
        &mut runtime,
        r#"
        const logger = migo.getRealtimeLogManager();
        logger.setFilterMsg('room-7');
        logger.addFilterMsg('boss');
        logger.info({ hp: 3 }, 'entered');
        logger.tag('combat').error('damage', 12);
        logger.warn('x'.repeat(6000));
        "#,
        "0",
    );
    let logs = host.logs.lock().unwrap().clone();
    assert_eq!(
        logs,
        [
            serde_json::json!({
                "source": "realtime", "level": "info", "key": "default",
                "value": [{ "hp": 3 }, "entered"], "filterMsg": ["room-7", "boss"],
            }),
            serde_json::json!({
                "source": "realtime", "level": "error", "key": "combat",
                "value": ["damage", 12], "filterMsg": ["room-7", "boss"],
            }),
        ],
        "an entry over five kilobytes is dropped"
    );
}

#[test]
fn a_game_log_manager_answers_once_it_is_returned_and_keeps_its_state_to_itself() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeHost::default());
    let mut runtime = boot(host);
    // The reference's own example uses what the call returns; its callbacks
    // run once there is something to use.
    let said = run_to_idle(
        &mut runtime,
        r#"
        globalThis.__said = [];
        const logger = migo.getGameLogManager({
            commonInfo: { version: '1.0.0' },
            success(res) { globalThis.__said.push(res.errMsg, typeof logger.log); },
            complete() { globalThis.__said.push('complete'); },
        });
        globalThis.__own = Object.keys(logger).join();
        globalThis.__info = JSON.stringify(logger.getCommonInfo());
        "#,
        "JSON.stringify([globalThis.__said, globalThis.__own, globalThis.__info])",
    );
    assert_eq!(
        said,
        r#"[["getGameLogManager:ok","function","complete"],"","{\"version\":\"1.0.0\"}"]"#
    );
}

#[test]
fn a_game_s_logs_are_limited_per_run_and_by_the_bytes_they_weigh() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeHost::default());
    let mut runtime = boot(host.clone());
    let said = run_to_idle(
        &mut runtime,
        r#"
        globalThis.__fails = [];
        const fail = (res) => globalThis.__fails.push(res.errMsg);
        // 6000 characters of three bytes each: 18 KB, though fewer than 16 K characters.
        migo.getGameLogManager().log({ level: 'info', value: '\u4e2d'.repeat(6000), fail });
        // The run's 999, each through a manager of its own, and one more.
        for (let i = 0; i < 999; i++) migo.getGameLogManager().log({ level: 'info', value: i, fail });
        migo.getGameLogManager().log({ level: 'info', value: 'one too many', fail });
        "#,
        "JSON.stringify(globalThis.__fails)",
    );
    assert_eq!(host.logs.lock().unwrap().len(), 999);
    assert_eq!(
        said,
        r#"["gameLog.log:fail log size exceeds limit (max 16KB)","gameLog.log:fail log count limit exceeded (max 999)"]"#
    );
}

#[test]
fn analytics_reach_the_host_and_a_scene_is_checked_and_reported_once() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let host = Arc::new(FakeHost::default());
    let mut runtime = boot(host.clone());
    runtime
        .execute_script("<test:settle>", deno_core::FastString::from_static(SETTLE))
        .unwrap();
    run_to_idle(
        &mut runtime,
        r#"
        migo.reportEvent('level_up', { level: 3 });
        migo.reportMonitor('1', 1);
        migo.reportPerformance(1101, 680, 'custom');
        globalThis.__scene = [];
        migo.reportScene({ sceneId: 7, costTime: 350, dimension: { d1: '2.1.0' }, metric: { m1: '546' },
            fail(res) { __scene.push(res.errMsg); } });
        migo.reportScene({ sceneId: 8, metric: { m1: 'many' }, fail(res) { __scene.push(res.errMsg); } });
        migo.reportScene({ sceneId: 7, fail(res) { __scene.push(res.errMsg); } });
        "#,
        "0",
    );
    let reports = host.reports.lock().unwrap().clone();
    let apis: Vec<&str> = reports.iter().map(|r| r["api"].as_str().unwrap()).collect();
    assert_eq!(
        apis,
        [
            "reportEvent",
            "reportMonitor",
            "reportPerformance",
            "reportScene"
        ]
    );
    assert_eq!(
        reports[0]["options"],
        serde_json::json!({ "eventId": "level_up", "data": { "level": 3 } })
    );
    assert_eq!(
        reports[3]["options"],
        serde_json::json!({ "sceneId": 7, "costTime": 350, "dimension": { "d1": "2.1.0" }, "metric": { "m1": "546" } })
    );
    let checked = run_to_idle(&mut runtime, "0", "JSON.stringify(__scene)");
    assert_eq!(
        checked,
        r#"["reportScene:fail parameter.metric.m1 needs to be a numeric value of type string","reportScene:fail report sceneId:7 repeatedly"]"#
    );
}

#[test]
fn random_values_are_the_os_s_and_as_many_as_asked_for() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let mut runtime = boot_with_services(None, None);
    runtime
        .execute_script("<test:settle>", deno_core::FastString::from_static(SETTLE))
        .unwrap();
    let checked = run_to_idle(
        &mut runtime,
        r#"
        const crypto = migo.getUserCryptoManager();
        globalThis.__random = [];
        crypto.getRandomValues({ length: 64, success(res) {
            const bytes = new Uint8Array(res.randomValues);
            __random.push(bytes.length, bytes.some(function (b) { return b !== 0; }));
        } });
        crypto.getRandomValues({ length: 1048577, fail(res) { __random.push(res.errMsg); } });
        "#,
        "JSON.stringify(__random)",
    );
    assert_eq!(
        checked,
        r#"[64,true,"getRandomValues:fail length must be an integer from 1 to 1048576"]"#
    );
}
