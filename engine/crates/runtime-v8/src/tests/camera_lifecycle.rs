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

    fn listen_frame_change(&self, request_json: &str) -> Result<(), ServiceError> {
        self.record("listen_frame_change", request_json)
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
    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
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

/// A frame reaches content when it is announced: content takes the newest one
/// waiting -- the one place its pixels cross -- and hands it to its listeners;
/// a frame replaced before it was taken is never seen, and an announcement with
/// nothing waiting hands over nothing.
#[test]
fn an_announced_frame_is_taken_and_handed_to_the_listeners() {
    use shared::op_state::HostOpState;
    use shared::protocol::camera_frame::{
        CameraFrameEntry, CameraFramePush, push_camera_frame, take_camera_frame,
    };

    let v8_executor = crate::tests::support::v8_test_executor();
    let _v8_enter = v8_executor.as_ref().map(|executor| executor.enter());
    let camera = Arc::new(FakeCamera::default());
    let mut runtime = boot(camera.clone());
    run(
        &mut runtime,
        "globalThis.__frames = []; \
         globalThis.__camera = migo.createCamera({}); \
         __camera.onCameraFrame((frame) => { \
             __frames.push([frame.width, frame.height, frame.data instanceof ArrayBuffer, \
                 Array.from(new Uint8Array(frame.data))]); }); \
         __camera.listenFrameChange();",
        "0",
    );
    let requests = camera.requests();
    let create: serde_json::Value =
        serde_json::from_str(requests[0].strip_prefix("create ").unwrap()).unwrap();
    let camera_id = u32::try_from(create["cameraId"].as_u64().unwrap()).unwrap();
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("listen_frame_change ")),
        "{requests:?}"
    );
    let host_id = runtime.op_state().borrow().borrow::<HostOpState>().id;
    let announce = |runtime: &mut JsRuntime| {
        run(
            runtime,
            &format!(
                "globalThis[Symbol.for('Migo.hostBridge')]._internalOnCameraFrameReady({camera_id});"
            ),
            "JSON.stringify(__frames)",
        )
    };
    let frame = |fill: u8| CameraFrameEntry {
        data: vec![fill; 8],
        width: 2,
        height: 1,
    };

    // One frame, announced: taken and handed over, its size beside its pixels.
    let first = push_camera_frame(host_id, camera_id, frame(7));
    assert!(matches!(first, CameraFramePush::Notify(_)));
    assert_eq!(announce(&mut runtime), "[[2,1,true,[7,7,7,7,7,7,7,7]]]");
    drop(first);
    assert!(
        take_camera_frame(host_id, camera_id).is_none(),
        "taken once"
    );

    // Two before content came to take one: the newer is the one it sees, and
    // only the first of them was an announcement.
    let announced = push_camera_frame(host_id, camera_id, frame(1));
    assert!(matches!(announced, CameraFramePush::Notify(_)));
    assert!(matches!(
        push_camera_frame(host_id, camera_id, frame(2)),
        CameraFramePush::Superseded
    ));
    assert_eq!(
        announce(&mut runtime),
        "[[2,1,true,[7,7,7,7,7,7,7,7]],[2,1,true,[2,2,2,2,2,2,2,2]]]"
    );
    drop(announced);

    // Announced with nothing waiting: nothing is handed over.
    assert_eq!(
        announce(&mut runtime),
        "[[2,1,true,[7,7,7,7,7,7,7,7]],[2,1,true,[2,2,2,2,2,2,2,2]]]"
    );
}
