//! A camera's life as the host sees it: a camera that did not open is let go
//! the one way every camera is -- the host hears DESTROY -- and content's handle
//! to it refuses what follows without reaching the host.

use std::sync::{Arc, Mutex};

use deno_core::{JsRuntime, serde_json};
use shared::{
    protocol::error::ServiceError,
    services::{
        CameraService, CommerceServices, ConnectivityServices, DeviceServices, MediaServices,
        PermissionService, Scope, ScopeState, SensorServices, SystemUtilServices,
    },
};

use super::support::{boot_with_services, run_to_idle as run};

/// What the host was asked, in order: `method request`.
#[derive(Default)]
struct FakeCamera(Mutex<Vec<String>>);

impl FakeCamera {
    fn record(&self, method: &str, request_json: &str) -> Result<(), ServiceError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("{method} {request_json}"));
        Ok(())
    }

    fn requests(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl CameraService for FakeCamera {
    fn create(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("create", request_json)
    }

    fn destroy(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("destroy", request_json)
    }

    fn take_photo(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("take_photo", request_json)
    }
}

struct Granted;

impl PermissionService for Granted {
    fn scope_state(&self, _scope: Scope) -> ScopeState {
        ScopeState::Granted
    }
}

struct Host(Arc<FakeCamera>);

impl SensorServices for Host {}
impl ConnectivityServices for Host {}
impl CommerceServices for Host {}
impl SystemUtilServices for Host {
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        Some(Arc::new(Granted))
    }
}
impl MediaServices for Host {
    fn camera(&self) -> Option<Arc<dyn CameraService>> {
        Some(self.0.clone())
    }
}

fn boot(camera: Arc<FakeCamera>) -> JsRuntime {
    boot_with_services(
        Some(Arc::new(Host(camera)) as Arc<dyn DeviceServices>),
        None,
    )
}

#[test]
fn a_camera_that_did_not_open_is_destroyed_and_refuses_what_follows() {
    let camera = Arc::new(FakeCamera::default());
    let mut runtime = boot(camera.clone());
    run(
        &mut runtime,
        "globalThis.__results = {}; \
         globalThis.__camera = migo.createCamera({ \
             fail: (res) => { __results.fail = res; }, \
             complete: () => { __results.complete = true; } });",
        "0",
    );
    let requests = camera.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let create: serde_json::Value =
        serde_json::from_str(requests[0].strip_prefix("create ").unwrap()).unwrap();

    let checked = run(
        &mut runtime,
        &format!(
            "globalThis[Symbol.for('Migo.hostBridge')]._internalDispatch('_internalOnCreateCameraResult', \
             JSON.stringify([JSON.stringify({{ requestId: {}, error: 'no back camera' }})])); \
             __camera.takePhoto('normal').catch((res) => {{ __results.photo = res; }});",
            create["requestId"]
        ),
        "JSON.stringify([__results.fail, __results.complete, __results.photo])",
    );
    assert_eq!(
        checked,
        r#"[{"errMsg":"createCamera:fail no back camera"},true,{"errMsg":"camera.takePhoto:fail camera destroyed"}]"#
    );
    let requests = camera.requests();
    assert_eq!(
        requests[1..],
        [format!(r#"destroy {{"cameraId":{}}}"#, create["cameraId"])],
        "the host lets go of the camera it did not open, and hears nothing more: {requests:?}"
    );
}
