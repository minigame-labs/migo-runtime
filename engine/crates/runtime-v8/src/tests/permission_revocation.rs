//! Permission denial must block capability use without trapping live resources.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use deno_core::{FastString, JsRuntime, RuntimeOptions};
use shared::{
    channel::ThreadWakeup,
    device::gpu_caps::GpuCaps,
    op_state::{AudioSender, HostOpState, NetworkPolicy},
    protocol::error::ServiceError,
    render_command_sender::CommandSender,
    services::{
        AuthService, BluetoothService, CameraService, CommerceServices, ConnectivityServices,
        DeviceServices, ImageApiService, MediaServices, PermissionService, RecorderService, Scope,
        ScopeState, SensorServices, SystemUtilServices,
    },
    vfs::{GamePaths, VirtualFS},
};

#[derive(Default)]
struct FakeCamera {
    protected_calls: AtomicUsize,
    cleanup_calls: AtomicUsize,
}

impl CameraService for FakeCamera {
    fn create(&self, _options_json: &str) -> Result<String, ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok("{}".to_string())
    }

    fn take_photo_async(&self, _request_id: u32, _options_json: &str) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn start_record(&self, _options_json: &str) -> Result<String, ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok("{}".to_string())
    }

    fn set_zoom(&self, _options_json: &str) -> Result<String, ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok("{}".to_string())
    }

    fn listen_frame_change(&self, _camera_id: u32) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn destroy(&self, _camera_id: u32) -> Result<(), ServiceError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop_record(&self, _options_json: &str) -> Result<String, ServiceError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok("{}".to_string())
    }

    fn close_frame_change(&self, _camera_id: u32) -> Result<(), ServiceError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct FakeRecorder {
    protected_calls: AtomicUsize,
    cleanup_calls: AtomicUsize,
}

impl RecorderService for FakeRecorder {
    fn start(&self, _options_json: &str) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn pause(&self) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn resume(&self) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<(), ServiceError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct FakeBluetooth {
    protected_calls: AtomicUsize,
    cleanup_calls: AtomicUsize,
}

impl BluetoothService for FakeBluetooth {
    fn open_adapter(&self, _options_json: &str) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn get_adapter_state(&self, _request_json: &str) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn start_devices_discovery(&self, _options_json: &str) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn create_ble_connection(&self, _options_json: &str) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn write_ble_characteristic_value(&self, _options_json: &str) -> Result<(), ServiceError> {
        self.protected_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn close_adapter(&self, _request_json: &str) -> Result<(), ServiceError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop_devices_discovery(&self, _request_json: &str) -> Result<(), ServiceError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn close_ble_connection(&self, _options_json: &str) -> Result<(), ServiceError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop_beacon_discovery(&self, _request_json: &str) -> Result<(), ServiceError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct FakeImageApi(AtomicUsize, std::sync::Mutex<Vec<String>>);

impl FakeImageApi {
    fn requests(&self) -> Vec<String> {
        self.1.lock().unwrap().clone()
    }
}

impl ImageApiService for FakeImageApi {
    fn save_image_to_photos_album(&self, request_json: &str) -> Result<(), ServiceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        self.1.lock().unwrap().push(request_json.to_string());
        Ok(())
    }

    fn preview_image(&self, request_json: &str) -> Result<(), ServiceError> {
        self.1.lock().unwrap().push(request_json.to_string());
        Ok(())
    }
}

#[derive(Default)]
struct FakeAuth(AtomicUsize);

impl AuthService for FakeAuth {
    fn get_user_info(&self, _options_json: &str) -> Result<(), ServiceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct MutablePermissions(AtomicBool);

impl PermissionService for MutablePermissions {
    fn scope_state(&self, _scope: Scope) -> ScopeState {
        if self.0.load(Ordering::SeqCst) {
            ScopeState::Granted
        } else {
            ScopeState::Denied
        }
    }
}

struct Bundle {
    permissions: Arc<MutablePermissions>,
    camera: Arc<FakeCamera>,
    recorder: Arc<FakeRecorder>,
    bluetooth: Arc<FakeBluetooth>,
    image_api: Arc<FakeImageApi>,
    auth: Arc<FakeAuth>,
}

impl Bundle {
    fn new(granted: bool) -> Arc<Self> {
        Arc::new(Self {
            permissions: Arc::new(MutablePermissions(AtomicBool::new(granted))),
            camera: Arc::new(FakeCamera::default()),
            recorder: Arc::new(FakeRecorder::default()),
            bluetooth: Arc::new(FakeBluetooth::default()),
            image_api: Arc::new(FakeImageApi::default()),
            auth: Arc::new(FakeAuth::default()),
        })
    }
}

impl SensorServices for Bundle {}

impl MediaServices for Bundle {
    fn recorder(&self) -> Option<Arc<dyn RecorderService>> {
        Some(self.recorder.clone())
    }

    fn camera(&self) -> Option<Arc<dyn CameraService>> {
        Some(self.camera.clone())
    }

    fn image_api(&self) -> Option<Arc<dyn ImageApiService>> {
        Some(self.image_api.clone())
    }
}

impl ConnectivityServices for Bundle {
    fn bluetooth(&self) -> Option<Arc<dyn BluetoothService>> {
        Some(self.bluetooth.clone())
    }
}

impl CommerceServices for Bundle {
    fn auth(&self) -> Option<Arc<dyn AuthService>> {
        Some(self.auth.clone())
    }
}

impl SystemUtilServices for Bundle {
    fn permission(&self) -> Option<Arc<dyn PermissionService>> {
        Some(self.permissions.clone())
    }
}

/// A game's sandbox in a scratch directory, removed when dropped.
struct Sandbox {
    root: PathBuf,
    paths: GamePaths,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-permission-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "sandboxed", 1)
            .expect("game paths");
        paths.ensure_directories().expect("sandbox directories");
        Self { root, paths }
    }

    fn vfs(&self) -> Arc<VirtualFS> {
        Arc::new(VirtualFS::from_game_paths(&self.paths))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn host_state(bundle: Arc<Bundle>) -> HostOpState {
    host_state_in(bundle, None)
}

fn host_state_in(bundle: Arc<Bundle>, vfs: Option<Arc<VirtualFS>>) -> HostOpState {
    let (render_tx, _render_rx) = CommandSender::new();
    let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);
    HostOpState {
        callback_ids: std::sync::Arc::new(shared::callback_id::CallbackIdAllocator::default()),
        runtime_generation: 1,
        id: 1,
        app_cache_dir: PathBuf::from("/tmp/cache"),
        app_files_dir: PathBuf::from("/tmp/files"),
        code_dir: None,
        game_paths: None,
        vfs,
        mount_table: None,
        render_tx,
        text_measurer: None,
        audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
        host_tx,
        device_services: Some(bundle as Arc<dyn DeviceServices>),
        raf_rx: None,
        raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
        request_vsync: None,
        sub_packages: Vec::new(),
        workers_path: None,
        network_policy: NetworkPolicy::default(),
        backgrounded: Arc::new(false.into()),
        timer_backgrounded: Arc::new(false.into()),
        webgl_context_created: Arc::new(false.into()),
        context_lost: Arc::new(shared::op_state::ContextLostState::default()),
        code_signing_enabled: false,
        gpu_caps: GpuCaps::new(),
    }
}

fn boot(bundle: Arc<Bundle>) -> JsRuntime {
    boot_with(host_state(bundle))
}

fn boot_with(state: HostOpState) -> JsRuntime {
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: crate::main_extensions(state),
        ..Default::default()
    });
    crate::harden_global_scope(&mut runtime);
    runtime
}

/// Run `source` where the runtime's timers can be armed: a request that reaches
/// the host starts its timeout, and the web timer needs a reactor to exist.
fn run(runtime: &mut JsRuntime, source: &'static str) {
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime for timers");
    let _context = reactor.enter();
    runtime
        .execute_script(
            "<test:permission-revocation>",
            FastString::from_static(source),
        )
        .expect("permission behavior script");
}

#[test]
fn denied_camera_can_release_but_cannot_acquire_or_use() {
    let bundle = Bundle::new(true);
    let mut runtime = boot(bundle.clone());
    run(&mut runtime, "globalThis.__camera = migo.createCamera({});");
    bundle.camera.protected_calls.store(0, Ordering::SeqCst);
    bundle.permissions.0.store(false, Ordering::SeqCst);

    run(
        &mut runtime,
        "try { __camera.takePhoto({ fail() {} }); } catch (_) {} \
         __camera.startRecord({ fail() {} }); \
         __camera.setZoom({ zoom: 2, fail() {} }); \
         __camera.listenFrameChange(); \
         __camera.stopRecord({ fail() {} }); \
         __camera.closeFrameChange(); \
         __camera.destroy();",
    );

    assert_eq!(bundle.camera.cleanup_calls.load(Ordering::SeqCst), 3);
    assert_eq!(bundle.camera.protected_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn denied_recorder_can_stop_but_not_start_pause_or_resume() {
    let bundle = Bundle::new(true);
    let mut runtime = boot(bundle.clone());
    run(
        &mut runtime,
        "globalThis.__recorder = migo.getRecorderManager(); __recorder.start();",
    );
    bundle.recorder.protected_calls.store(0, Ordering::SeqCst);
    bundle.permissions.0.store(false, Ordering::SeqCst);
    run(
        &mut runtime,
        "try { __recorder.start(); } catch (_) {} \
         try { __recorder.pause(); } catch (_) {} \
         try { __recorder.resume(); } catch (_) {} \
         try { __recorder.stop(); } catch (_) {}",
    );

    assert_eq!(bundle.recorder.cleanup_calls.load(Ordering::SeqCst), 1);
    assert_eq!(bundle.recorder.protected_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn denied_bluetooth_can_close_and_stop_but_not_acquire_query_or_write() {
    let bundle = Bundle::new(true);
    let mut runtime = boot(bundle.clone());
    run(&mut runtime, "migo.openBluetoothAdapter({ fail() {} });");
    bundle.bluetooth.protected_calls.store(0, Ordering::SeqCst);
    bundle.permissions.0.store(false, Ordering::SeqCst);
    run(
        &mut runtime,
        "migo.closeBluetoothAdapter({ fail() {} }); \
         migo.stopBluetoothDevicesDiscovery({ fail() {} }); \
         migo.closeBLEConnection({ deviceId: 'device', fail() {} }); \
         migo.stopBeaconDiscovery({ fail() {} }); \
         migo.openBluetoothAdapter({ fail() {} }); \
         migo.getBluetoothAdapterState({ fail() {} }); \
         migo.startBluetoothDevicesDiscovery({ fail() {} }); \
         migo.createBLEConnection({ deviceId: 'device', fail() {} }); \
         migo.writeBLECharacteristicValue({ deviceId: 'device', serviceId: 'service', \
             characteristicId: 'characteristic', value: new ArrayBuffer(1), fail() {} });",
    );

    assert_eq!(bundle.bluetooth.cleanup_calls.load(Ordering::SeqCst), 4);
    assert_eq!(bundle.bluetooth.protected_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn album_write_and_shared_user_info_op_require_their_scopes() {
    let sandbox = Sandbox::new("album");
    std::fs::write(sandbox.paths.temp_dir().join("image.png"), b"png").unwrap();

    let denied = Bundle::new(false);
    let mut denied_runtime = boot_with(host_state_in(denied.clone(), Some(sandbox.vfs())));
    run(
        &mut denied_runtime,
        "migo.saveImageToPhotosAlbum({ filePath: '/tmp/image.png', fail() {} }); \
         migo.getUserInfo({ fail() {} }); \
         migo.getUserProfile({ desc: 'profile', fail() {} });",
    );
    assert_eq!(denied.image_api.0.load(Ordering::SeqCst), 0);
    assert_eq!(denied.auth.0.load(Ordering::SeqCst), 0);

    let granted = Bundle::new(true);
    let mut granted_runtime = boot_with(host_state_in(granted.clone(), Some(sandbox.vfs())));
    run(
        &mut granted_runtime,
        "migo.saveImageToPhotosAlbum({ filePath: '/tmp/image.png', fail() {} }); \
         migo.getUserInfo({ fail() {} }); \
         migo.getUserProfile({ desc: 'profile', fail() {} });",
    );
    assert_eq!(granted.image_api.0.load(Ordering::SeqCst), 1);
    assert_eq!(granted.auth.0.load(Ordering::SeqCst), 2);
}

/// A path content names reaches the host as the real file behind it -- and only a
/// file the game can itself read. The host used to be handed content's string
/// verbatim, so any path the host process could read went into the album.
#[test]
fn a_content_path_reaches_the_host_as_the_sandbox_file_and_nothing_else_does() {
    let sandbox = Sandbox::new("paths");
    let image = sandbox.paths.user_data_dir().join("shot.png");
    std::fs::write(&image, b"png").unwrap();
    let bundle = Bundle::new(true);
    let mut runtime = boot_with(host_state_in(bundle.clone(), Some(sandbox.vfs())));

    run(
        &mut runtime,
        "migo.saveImageToPhotosAlbum({ filePath: '/user/shot.png', fail() {} }); \
         migo.saveImageToPhotosAlbum({ filePath: '/etc/hosts', fail() {} }); \
         migo.saveImageToPhotosAlbum({ filePath: '/user/../../outside.png', fail() {} }); \
         migo.saveImageToPhotosAlbum({ filePath: 'https://example.com/a.png', fail() {} }); \
         migo.previewImage({ urls: ['https://example.com/a.png', '/user/shot.png'], \
                             current: '/user/shot.png', fail() {} });",
    );

    let requests = bundle.image_api.requests();
    assert_eq!(
        requests.len(),
        2,
        "only the sandbox file and the preview leave: {requests:?}"
    );
    let saved: deno_core::serde_json::Value =
        deno_core::serde_json::from_str(&requests[0]).unwrap();
    let real = std::fs::canonicalize(&image).unwrap();
    assert_eq!(
        std::fs::canonicalize(saved["filePath"].as_str().unwrap()).unwrap(),
        real
    );
    let previewed: deno_core::serde_json::Value =
        deno_core::serde_json::from_str(&requests[1]).unwrap();
    assert_eq!(
        previewed["urls"][0], "https://example.com/a.png",
        "a viewer takes a URL"
    );
    assert_eq!(previewed["urls"][1], previewed["current"]);
    assert_eq!(
        std::fs::canonicalize(previewed["current"].as_str().unwrap()).unwrap(),
        real
    );
}
