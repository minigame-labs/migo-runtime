//! getAppBaseInfo, getAccountInfoSync and getDeviceBenchmarkInfo -- and the copies
//! getSystemInfoSync and getDeviceInfo carry -- as content reads them: what the
//! host reports, what the engine knows of itself, and nothing made up.

use std::sync::Arc;

use deno_core::{FastString, JsRuntime, RuntimeOptions, serde_json};
use shared::services::{
    CommerceServices, ConnectivityServices, DeviceServices, MediaServices, SensorServices,
    SystemInfoService, SystemUtilServices,
    app_info::{API_LIBRARY_LEVEL, ContentEnvironment, HostAppInfo, RUNTIME_VERSION},
};

use super::support::{SETTLE, Sandbox, run_to_idle as run, test_host_state};

struct Reporting(HostAppInfo);

impl SystemInfoService for Reporting {
    fn host_app_info(&self) -> HostAppInfo {
        self.0.clone()
    }
}

struct Host(Arc<Reporting>);

impl SensorServices for Host {}
impl MediaServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {
    fn system_info(&self) -> Option<Arc<dyn SystemInfoService>> {
        Some(self.0.clone())
    }
}

/// A runtime whose content is `sandbox`'s, over a host that reports `report` (or none).
fn boot(sandbox: &Sandbox, report: Option<HostAppInfo>) -> JsRuntime {
    let mut state = test_host_state();
    state.device_services =
        report.map(|report| Arc::new(Host(Arc::new(Reporting(report)))) as Arc<dyn DeviceServices>);
    state.vfs = Some(sandbox.vfs());
    state.game_paths = Some(Arc::new(sandbox.paths.clone()));
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: crate::main_extensions(state),
        ..Default::default()
    });
    crate::harden_global_scope(&mut runtime);
    runtime
        .execute_script("<test:app-info-setup>", FastString::from_static(SETTLE))
        .unwrap();
    runtime
}

const READ_ALL: &str = r#"
    __settle('getDeviceBenchmarkInfo');
"#;

const REPORT: &str = r#"JSON.stringify({
    base: migo.getAppBaseInfo(),
    account: migo.getAccountInfoSync(),
    benchmark: __results.getDeviceBenchmarkInfo,
    system: migo.getSystemInfoSync(),
    device: migo.getDeviceInfo(),
})"#;

fn read(runtime: &mut JsRuntime) -> serde_json::Value {
    serde_json::from_str(&run(runtime, READ_ALL, REPORT)).unwrap()
}

#[test]
fn what_the_host_reports_is_what_content_reads_everywhere_it_is_asked() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let sandbox = Sandbox::new("app-info-host");
    let mut runtime = boot(
        &sandbox,
        Some(HostAppInfo {
            host_app_id: Some("com.example.host".into()),
            language: Some("zh-Hans-CN".into()),
            font_scale: Some(1.25),
            debug: true,
            benchmark_level: Some(31),
            model_level: Some(1),
            content_environment: ContentEnvironment::Release,
            content_version: Some("1.4.2".into()),
        }),
    );
    let read = read(&mut runtime);

    let base = &read["base"];
    assert_eq!(base["SDKVersion"], API_LIBRARY_LEVEL, "{base}");
    assert_eq!(base["version"], RUNTIME_VERSION, "{base}");
    assert_eq!(
        base["host"],
        serde_json::json!({ "appId": "com.example.host" })
    );
    assert_eq!(base["language"], "zh_CN");
    assert_eq!(base["fontSizeScaleFactor"], 1.25);
    assert_eq!(base["fontSizeSetting"], 20);
    assert_eq!(base["enableDebug"], true);
    // The platform does not give mini games a theme.
    assert!(base.get("theme").is_none(), "{base}");
    // The kernel's version is the platform's only on a PC, where the engine is that kernel.
    let desktop = cfg!(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ));
    if desktop {
        assert_eq!(base["PCKernelVersion"], RUNTIME_VERSION, "{base}");
    } else {
        assert!(base.get("PCKernelVersion").is_none(), "{base}");
    }

    assert_eq!(
        read["account"],
        serde_json::json!({
            "miniProgram": { "appId": "app-info-host", "envVersion": "release", "version": "1.4.2" }
        })
    );

    let benchmark = &read["benchmark"];
    assert_eq!(benchmark["ok"], true, "{benchmark}");
    assert_eq!(benchmark["res"]["benchmarkLevel"], 31);
    assert_eq!(benchmark["res"]["modelLevel"], 1);
    assert_eq!(benchmark["res"]["errMsg"], "getDeviceBenchmarkInfo:ok");

    // The APIs that repeat these facts read them from the same place.
    let system = &read["system"];
    for field in [
        "SDKVersion",
        "version",
        "language",
        "fontSizeSetting",
        "enableDebug",
        "host",
    ] {
        assert_eq!(
            system[field], base[field],
            "getSystemInfoSync().{field}: {system}"
        );
    }
    assert_eq!(system["benchmarkLevel"], 31);
    assert_eq!(read["device"]["benchmarkLevel"], 31);
}

#[test]
fn a_host_that_says_nothing_is_told_unknown_never_a_constant() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let sandbox = Sandbox::new("app-info-silent");
    let mut runtime = boot(&sandbox, None);
    let read = read(&mut runtime);

    let base = &read["base"];
    assert_eq!(base["SDKVersion"], API_LIBRARY_LEVEL);
    assert_eq!(base["version"], RUNTIME_VERSION);
    // No host app named itself, so there is no host app id -- not a made-up one.
    assert_eq!(base["host"], serde_json::json!({}));
    assert_eq!(base["enableDebug"], false);
    assert_eq!(base["fontSizeScaleFactor"], 1.0);
    assert_eq!(base["fontSizeSetting"], 16);

    // The content's id is the engine's; a version only a host could know is absent.
    assert_eq!(
        read["account"],
        serde_json::json!({
            "miniProgram": { "appId": "app-info-silent", "envVersion": "release", "version": "" }
        })
    );
    assert_eq!(read["benchmark"]["res"]["benchmarkLevel"], -1);
    assert_eq!(read["benchmark"]["res"]["modelLevel"], 0);
    assert_eq!(read["system"]["benchmarkLevel"], -1);
    assert_eq!(read["device"]["benchmarkLevel"], -1);
}

#[test]
fn a_build_that_is_not_a_release_has_no_version() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let sandbox = Sandbox::new("app-info-trial");
    let mut runtime = boot(
        &sandbox,
        Some(HostAppInfo {
            content_environment: ContentEnvironment::Trial,
            content_version: Some("1.4.2".into()),
            ..HostAppInfo::default()
        }),
    );
    let read = read(&mut runtime);
    assert_eq!(read["account"]["miniProgram"]["envVersion"], "trial");
    assert_eq!(read["account"]["miniProgram"]["version"], "");
}
