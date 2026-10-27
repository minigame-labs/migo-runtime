//! saveFileToDisk: a file of content's, saved where the player chooses through
//! the host's save dialog -- the host handed a real path it can read, never one
//! outside the sandbox, never a remote URL.

use std::sync::{Arc, Mutex};

use deno_core::{FastString, JsRuntime, serde_json};
use shared::{
    protocol::error::ServiceError,
    services::{
        CommerceServices, ConnectivityServices, DeviceServices, ImageApiService, MediaServices,
        SensorServices, SystemUtilServices,
    },
    vfs::VirtualFS,
};

use super::support::{SETTLE, Sandbox, boot_with_services, run_to_idle as run};

#[derive(Default)]
struct FakeSaveDialog {
    requests: Mutex<Vec<String>>,
}

impl ImageApiService for FakeSaveDialog {
    fn save_file_to_disk(&self, request_json: &str) -> Result<(), ServiceError> {
        self.requests.lock().unwrap().push(request_json.to_owned());
        Ok(())
    }
}

struct Host(Arc<FakeSaveDialog>);

impl SensorServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {}
impl MediaServices for Host {
    fn image_api(&self) -> Option<Arc<dyn ImageApiService>> {
        Some(self.0.clone())
    }
}

fn boot(host: Option<Arc<FakeSaveDialog>>, vfs: Option<Arc<VirtualFS>>) -> JsRuntime {
    let mut runtime = boot_with_services(
        host.map(|host| Arc::new(Host(host)) as Arc<dyn DeviceServices>),
        vfs,
    );
    runtime
        .execute_script("<test:save-setup>", FastString::from_static(SETTLE))
        .unwrap();
    runtime
}

fn answer(request: &serde_json::Value, fields: &str) -> String {
    format!(
        "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('_internalOnSaveFileToDiskResult', \
         JSON.stringify([JSON.stringify(Object.assign({{ requestId: {} }}, {fields}))]));",
        request["requestId"]
    )
}

#[test]
fn the_host_is_handed_the_real_file_and_its_answer_settles_the_call() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let sandbox = Sandbox::new("save-file-to-disk");
    let file = sandbox.paths.user_data_dir().join("save.txt");
    std::fs::write(&file, b"saved").unwrap();
    let host = Arc::new(FakeSaveDialog::default());
    let mut runtime = boot(Some(host.clone()), Some(sandbox.vfs()));
    run(
        &mut runtime,
        r#"
        __settle('saveFileToDisk', { filePath: '/user/save.txt' });
        migo.saveFileToDisk({ filePath: 'https://example.com/a.txt',
            fail: function (res) { __results.remote = res; } });
        migo.saveFileToDisk({ filePath: '/etc/hosts',
            fail: function (res) { __results.outside = res; } });
        migo.saveFileToDisk({ fail: function (res) { __results.none = res; } });
        globalThis.__returned = migo.saveFileToDisk({ filePath: '/user/save.txt' });
        "#,
        "0",
    );
    let requests = host.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        2,
        "only the sandbox's own file crosses: {requests:?}"
    );
    let request: serde_json::Value = serde_json::from_str(&requests[0]).unwrap();
    assert_eq!(
        std::fs::canonicalize(request["filePath"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&file).unwrap()
    );

    let checked = run(
        &mut runtime,
        &answer(&request, "{}"),
        "JSON.stringify([__results.saveFileToDisk, typeof __returned.then === 'function', \
          __results.remote.errMsg.indexOf('saveFileToDisk:fail') === 0, \
          __results.outside.errMsg.indexOf('saveFileToDisk:fail') === 0, __results.none])",
    );
    assert_eq!(
        checked,
        r#"[{"ok":true,"res":{"errMsg":"saveFileToDisk:ok"}},true,true,true,{"errMsg":"saveFileToDisk:fail filePath is required"}]"#
    );

    // The player cancelling the dialog is the host's failure, and content's.
    let second: serde_json::Value = serde_json::from_str(&requests[1]).unwrap();
    run(
        &mut runtime,
        "__settle('saveFileToDisk', { filePath: '/user/save.txt' });",
        "0",
    );
    let third: serde_json::Value = serde_json::from_str(&host.requests.lock().unwrap()[2]).unwrap();
    let checked = run(
        &mut runtime,
        &(answer(&third, "{ error: 'cancel' }") + &answer(&second, "{}")),
        "JSON.stringify(__results.saveFileToDisk)",
    );
    assert_eq!(
        checked,
        r#"{"ok":false,"res":{"errMsg":"saveFileToDisk:fail cancel"}}"#
    );
}

#[test]
fn without_a_host_there_is_no_save_dialog() {
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let sandbox = Sandbox::new("save-file-no-host");
    std::fs::write(sandbox.paths.user_data_dir().join("save.txt"), b"saved").unwrap();
    let mut runtime = boot(None, Some(sandbox.vfs()));
    let checked = run(
        &mut runtime,
        "__settle('saveFileToDisk', { filePath: '/user/save.txt' });",
        "JSON.stringify(__results.saveFileToDisk)",
    );
    assert_eq!(
        checked,
        r#"{"ok":false,"res":{"errMsg":"saveFileToDisk:fail not supported"}}"#
    );
}
