//! What each of the platform's APIs reaches, measured by calling it.
//!
//! `contracts/runtime/minigame-api.json` lists the common mini-game platform's
//! API. For every name in it that migo publishes, this calls the function on a
//! real runtime and records two facts: the ops it dispatched (deno_core's op
//! metrics) and the host services it asked for (a device-services probe that
//! offers every service and notes each request). A factory's object is called
//! through too -- `createRewardedVideoAd` is what its `show` reaches.
//!
//! The result, `contracts/runtime/minigame-api-reach.json`, is the fact the
//! API x platform matrix is generated from: it is what decides that an API is
//! the engine's alone or needs a host, and which part of each platform's host
//! interface it needs. Measured rather than written down, because a table of
//! what an API needs is a claim, and this is what the API does.
//!
//! To change it on purpose:
//!   MIGO_UPDATE_API_REACH=1 cargo test -p migo-runtime-v8 --lib api_reach
//! and commit the file with the change.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use deno_core::serde_json::{self, Value, json};
use deno_core::{FastString, JsRuntime, PollEventLoopOptions, RuntimeOptions, v8};
use shared::services::{
    AccelerometerService, AdService, AudioPlatformService, AuthService, BatteryService,
    BluetoothService, CameraService, ClipboardService, CodecService, CommerceServices,
    CompassService, ConnectivityServices, DeviceMotionService, EcosystemService, FileService,
    GameLogService, GamepadService, GyroscopeService, ImageApiService, InteractionService,
    KeyboardService, LocationService, MediaServices, NavigateService, NetworkService,
    PaymentService, PermissionService, RecorderService, ScanCodeService, Scope, ScopeState,
    ScreenService, SensorServices, ServiceError, ShareService, SubpackageService,
    SystemInfoService, SystemUtilServices, VibrationService, VideoService, WindowService,
};

type Log = Arc<Mutex<Vec<String>>>;

/// Every host service, offered; asking for one is noted.
struct Probe {
    log: Log,
}

/// What a probed service answers: the trait's own defaults, which are the
/// "not supported" answers, so a call goes as far into the host as it would.
struct ProbeService;

macro_rules! probe_accessors {
    ($group:ident { $($accessor:ident: $service:ident),* $(,)? }) => {
        impl $group for Probe {
            $(fn $accessor(&self) -> Option<Arc<dyn $service>> {
                self.note(stringify!($accessor));
                Some(Arc::new(ProbeService) as Arc<dyn $service>)
            })*
        }
    };
}

probe_accessors!(SensorServices {
    battery: BatteryService,
    vibration: VibrationService,
    screen: ScreenService,
    device_motion: DeviceMotionService,
    gyroscope: GyroscopeService,
    compass: CompassService,
    accelerometer: AccelerometerService,
});
probe_accessors!(MediaServices {
    audio_platform: AudioPlatformService,
    recorder: RecorderService,
    camera: CameraService,
    image_api: ImageApiService,
    video: VideoService,
});
probe_accessors!(ConnectivityServices {
    network: NetworkService,
    bluetooth: BluetoothService,
    location: LocationService,
});
probe_accessors!(CommerceServices {
    game_log: GameLogService,
    ad: AdService,
    auth: AuthService,
    subpackage: SubpackageService,
    share: ShareService,
    payment: PaymentService,
});
probe_accessors!(SystemUtilServices {
    clipboard: ClipboardService,
    keyboard: KeyboardService,
    gamepad: GamepadService,
    interaction: InteractionService,
    system_info: SystemInfoService,
    permission: PermissionService,
    scan_code: ScanCodeService,
    navigate: NavigateService,
    window: WindowService,
    ecosystem: EcosystemService,
});

impl BatteryService for ProbeService {}
impl VibrationService for ProbeService {}
impl ScreenService for ProbeService {}
impl DeviceMotionService for ProbeService {}
impl GyroscopeService for ProbeService {}
impl CompassService for ProbeService {}
impl AccelerometerService for ProbeService {}
impl AudioPlatformService for ProbeService {}
impl RecorderService for ProbeService {}
impl CameraService for ProbeService {}
impl ImageApiService for ProbeService {}
impl NetworkService for ProbeService {}
impl BluetoothService for ProbeService {}
impl LocationService for ProbeService {}
impl GameLogService for ProbeService {}
impl AdService for ProbeService {}
impl AuthService for ProbeService {}
impl SubpackageService for ProbeService {}
impl ShareService for ProbeService {}
impl PaymentService for ProbeService {}
impl ClipboardService for ProbeService {}
impl KeyboardService for ProbeService {}
impl GamepadService for ProbeService {}
impl InteractionService for ProbeService {}
impl SystemInfoService for ProbeService {}
impl ScanCodeService for ProbeService {}
impl NavigateService for ProbeService {}
impl WindowService for ProbeService {}
impl EcosystemService for ProbeService {}

/// Every scope granted, so a call gated on one goes on to what it is for.
impl PermissionService for ProbeService {
    fn scope_state(&self, _scope: Scope) -> ScopeState {
        ScopeState::Granted
    }
}

impl Probe {
    fn note(&self, accessor: &str) {
        self.log.lock().unwrap().push(format!("svc:{accessor}"));
    }
}

fn refused<T>() -> Result<T, ServiceError> {
    Err(ServiceError::not_supported("probe:fail not supported"))
}

impl VideoService for ProbeService {
    fn create(&self, _options_json: &str) -> Result<String, ServiceError> {
        refused()
    }
    fn play(&self, _video_id: u32) -> Result<(), ServiceError> {
        refused()
    }
    fn pause(&self, _video_id: u32) -> Result<(), ServiceError> {
        refused()
    }
    fn stop(&self, _video_id: u32) -> Result<(), ServiceError> {
        refused()
    }
    fn seek(&self, _video_id: u32, _position: f64) -> Result<(), ServiceError> {
        refused()
    }
    fn request_fullscreen(&self, _video_id: u32, _direction: i32) -> Result<(), ServiceError> {
        refused()
    }
    fn exit_fullscreen(&self, _video_id: u32) -> Result<(), ServiceError> {
        refused()
    }
    fn set_property(&self, _video_id: u32, _property_json: &str) -> Result<(), ServiceError> {
        refused()
    }
    fn destroy(&self, _video_id: u32) -> Result<(), ServiceError> {
        refused()
    }
}

impl CodecService for ProbeService {
    fn encode_gbk(&self, _data: &str) -> Result<Vec<u8>, ServiceError> {
        refused()
    }
    fn decode_gbk(&self, _data: &[u8]) -> Result<String, ServiceError> {
        refused()
    }
}

impl FileService for ProbeService {
    fn unzip(&self, _zip_path: &str, _dest_dir: &str) -> Result<usize, ServiceError> {
        refused()
    }
}

/// The engine's own plumbing, which any call may touch and none needs a host
/// for: microtasks, timers, the clock and the frame clock, callback ids, the
/// console -- and the releases the collector's finalizers make (an image, a
/// canvas, an audio buffer), which land in whichever call is running when it
/// collects, so a record that kept them would change from run to run.
const PLUMBING: &[&str] = &[
    "op_alloc_host_callback_id",
    "op_audio_release_buffer",
    "op_await_next_frame",
    "op_console",
    "op_destroy_canvas",
    "op_destroy_image",
    "op_immediate_has_ref_count",
    "op_now",
    "op_queue_microtask",
    "op_run_microtasks",
    "op_timer_cancel",
    "op_timer_is_backgrounded",
    "op_timer_queue",
];

/// Calls that get past content-side validation to the host, where `{}` would be
/// refused before reaching it: each is the smallest valid call, as a script.
/// `__probeCall(name, args)` also calls through the object a factory returns.
/// Paths name the files `boot` puts in the game's temporary directory.
const CALLS: &[(&str, &str)] = &[
    (
        "authorize",
        "__probeCall('authorize', [{ scope: 'scope.userLocation' }])",
    ),
    (
        "chooseMessageFile",
        "__probeCall('chooseMessageFile', [{ count: 1 }])",
    ),
    (
        "closeSocket",
        "migo.connectSocket({ url: 'wss://probe.invalid/' }); __probeCall('closeSocket', [{}])",
    ),
    (
        "closeBLEConnection",
        "__probeCall('closeBLEConnection', [{ deviceId: 'probe' }])",
    ),
    (
        "createBLEConnection",
        "__probeCall('createBLEConnection', [{ deviceId: 'probe' }])",
    ),
    (
        "getBLEDeviceCharacteristics",
        "__probeCall('getBLEDeviceCharacteristics', [{ deviceId: 'probe', serviceId: 'probe' }])",
    ),
    (
        "getBLEDeviceRSSI",
        "__probeCall('getBLEDeviceRSSI', [{ deviceId: 'probe' }])",
    ),
    (
        "getBLEDeviceServices",
        "__probeCall('getBLEDeviceServices', [{ deviceId: 'probe' }])",
    ),
    (
        "getBLEMTU",
        "__probeCall('getBLEMTU', [{ deviceId: 'probe' }])",
    ),
    (
        "notifyBLECharacteristicValueChange",
        "__probeCall('notifyBLECharacteristicValueChange', [{ deviceId: 'probe', serviceId: 'probe', characteristicId: 'probe', state: true }])",
    ),
    (
        "readBLECharacteristicValue",
        "__probeCall('readBLECharacteristicValue', [{ deviceId: 'probe', serviceId: 'probe', characteristicId: 'probe' }])",
    ),
    (
        "setBLEMTU",
        "__probeCall('setBLEMTU', [{ deviceId: 'probe', mtu: 512 }])",
    ),
    (
        "writeBLECharacteristicValue",
        "__probeCall('writeBLECharacteristicValue', [{ deviceId: 'probe', serviceId: 'probe', characteristicId: 'probe', value: new ArrayBuffer(1) }])",
    ),
    (
        "compressImage",
        "__probeCall('compressImage', [{ src: '/tmp/probe.png' }])",
    ),
    (
        "createBannerAd",
        "__probeCall('createBannerAd', [{ adUnitId: 'probe', style: { left: 0, top: 0, width: 300 } }])",
    ),
    (
        "createBufferURL",
        "__probeCall('createBufferURL', [new ArrayBuffer(1)])",
    ),
    (
        "createCustomAd",
        "__probeCall('createCustomAd', [{ adUnitId: 'probe', style: { left: 0, top: 0 } }])",
    ),
    (
        "createGridAd",
        "__probeCall('createGridAd', [{ adUnitId: 'probe', style: { left: 0, top: 0, width: 300 } }])",
    ),
    ("createImageData", "__probeCall('createImageData', [1, 1])"),
    (
        "createInterstitialAd",
        "__probeCall('createInterstitialAd', [{ adUnitId: 'probe' }])",
    ),
    (
        "createRewardedVideoAd",
        "__probeCall('createRewardedVideoAd', [{ adUnitId: 'probe' }])",
    ),
    (
        "createTCPSocket",
        "migo.createTCPSocket().connect({ address: 'probe.invalid', port: 1 })",
    ),
    (
        "decode",
        "__probeCall('decode', [{ data: new ArrayBuffer(1), format: 'utf8' }])",
    ),
    (
        "downloadFile",
        "__probeCall('downloadFile', [{ url: 'https://probe.invalid/' }])",
    ),
    (
        "encode",
        "__probeCall('encode', [{ data: 'a', format: 'utf8' }])",
    ),
    (
        "getConnectedBluetoothDevices",
        "__probeCall('getConnectedBluetoothDevices', [{ services: [] }])",
    ),
    (
        "getGameLogManager",
        "migo.getGameLogManager({}).log({ level: 'info', key: 'probe', value: 1 })",
    ),
    (
        "isBluetoothDevicePaired",
        "__probeCall('isBluetoothDevicePaired', [{ deviceId: 'probe' }])",
    ),
    ("loadFont", "__probeCall('loadFont', ['/tmp/probe.ttf'])"),
    (
        "makeBluetoothPair",
        "__probeCall('makeBluetoothPair', [{ deviceId: 'probe', pin: '0000' }])",
    ),
    (
        "navigateToMiniProgram",
        "__probeCall('navigateToMiniProgram', [{ appId: 'probe' }])",
    ),
    (
        "preDownloadSubpackage",
        "__probeCall('preDownloadSubpackage', [{ name: 'probe' }])",
    ),
    (
        "previewImage",
        "__probeCall('previewImage', [{ urls: ['/tmp/probe.png'] }])",
    ),
    (
        "previewMedia",
        "__probeCall('previewMedia', [{ sources: [{ url: '/tmp/probe.png' }] }])",
    ),
    ("reportEvent", "__probeCall('reportEvent', ['probe', {}])"),
    (
        "reportMonitor",
        "__probeCall('reportMonitor', ['probe', 1])",
    ),
    (
        "reportPerformance",
        "__probeCall('reportPerformance', [1001, 1])",
    ),
    (
        "reportScene",
        "__probeCall('reportScene', [{ sceneId: 1 }])",
    ),
    (
        "request",
        "__probeCall('request', [{ url: 'https://probe.invalid/' }])",
    ),
    (
        "saveFileToDisk",
        "__probeCall('saveFileToDisk', [{ filePath: '/tmp/probe.png' }])",
    ),
    (
        "saveImageToPhotosAlbum",
        "__probeCall('saveImageToPhotosAlbum', [{ filePath: '/tmp/probe.png' }])",
    ),
    (
        "sendSocketMessage",
        "migo.connectSocket({ url: 'wss://probe.invalid/' }); __probeCall('sendSocketMessage', [{ data: 'a' }])",
    ),
    (
        "setClipboardData",
        "__probeCall('setClipboardData', [{ data: 'probe' }])",
    ),
    ("setCursor", "__probeCall('setCursor', ['pointer'])"),
    (
        "setDeviceOrientation",
        "__probeCall('setDeviceOrientation', [{ value: 'portrait' }])",
    ),
    (
        "setEnableDebug",
        "__probeCall('setEnableDebug', [{ enableDebug: false }])",
    ),
    (
        "setKeepScreenOn",
        "__probeCall('setKeepScreenOn', [{ keepScreenOn: true }])",
    ),
    (
        "setMessageToFriendQuery",
        "__probeCall('setMessageToFriendQuery', [{ query: 'a=1' }])",
    ),
    (
        "setScreenBrightness",
        "__probeCall('setScreenBrightness', [{ value: 0.5 }])",
    ),
    (
        "setVisualEffectOnCapture",
        "__probeCall('setVisualEffectOnCapture', [{ visualEffect: 'hidden' }])",
    ),
    (
        "setWindowSize",
        "__probeCall('setWindowSize', [{ width: 800, height: 600 }])",
    ),
    (
        "shareMessageToFriend",
        "__probeCall('shareMessageToFriend', [{ openId: 'probe' }])",
    ),
    (
        "showShareImageMenu",
        "__probeCall('showShareImageMenu', [{ path: '/tmp/probe.png' }])",
    ),
    (
        "startBeaconDiscovery",
        "__probeCall('startBeaconDiscovery', [{ uuids: ['00000000-0000-0000-0000-000000000000'] }])",
    ),
    (
        "updateKeyboard",
        "__probeCall('updateKeyboard', [{ value: 'probe' }])",
    ),
    (
        "uploadFile",
        "__probeCall('uploadFile', [{ url: 'https://probe.invalid/', filePath: '/tmp/probe.png', name: 'file' }])",
    ),
];

/// Calls `migo[name]` with `args`, and every method of an object it returns
/// with `{}`; promises are observed so a rejection is not an error here.
const CALL: &str = r#"
globalThis.__probeCall = function (name, args) {
    const settle = function (value) {
        if (value && typeof value.then === 'function') value.then(function () {}, function () {});
        return value;
    };
    let result;
    try {
        result = settle(migo[name].apply(migo, args));
    } catch (_) {
        return;
    }
    if (result === null || typeof result !== 'object' || typeof result.then === 'function') return;
    const called = new Set();
    for (let o = result; o && o !== Object.prototype; o = Object.getPrototypeOf(o)) {
        for (const key of Object.getOwnPropertyNames(o)) {
            if (key === 'constructor' || called.has(key)) continue;
            called.add(key);
            const descriptor = Object.getOwnPropertyDescriptor(o, key);
            if (!descriptor || typeof descriptor.value !== 'function') continue;
            const argument = /^(on|off)[A-Z]/.test(key) ? function () {} : {};
            try { settle(descriptor.value.call(result, argument)); } catch (_) {}
        }
    }
};
"#;

fn contract(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../contracts/runtime")
        .join(name)
}

fn eval_string(runtime: &mut JsRuntime, source: String) -> String {
    let value = runtime
        .execute_script("<api-reach>", FastString::from(source))
        .expect("probe script");
    deno_core::scope!(scope, runtime);
    v8::Local::new(scope, value).to_rust_string_lossy(scope)
}

#[cfg(feature = "profile-full")]
#[test]
fn api_reach_matches_the_committed_record() {
    let reference: Value = serde_json::from_str(
        &std::fs::read_to_string(contract("minigame-api.json")).expect("the API reference"),
    )
    .expect("the API reference is JSON");
    let names: Vec<String> = reference["apis"]
        .as_object()
        .expect("apis")
        .keys()
        .cloned()
        .collect();

    // Some ops spawn onto the runtime's reactor as they are dispatched, so every
    // call runs inside it, not only the polling.
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _inside = reactor.enter();

    let log: Log = Arc::default();
    let metrics_log = Arc::clone(&log);
    // A real sandbox, so a call naming a file gets as far as resolving it.
    let sandbox = super::support::Sandbox::new("api-reach");
    let temp = sandbox.paths.temp_dir();
    std::fs::write(temp.join("probe.png"), [0x89, b'P', b'N', b'G']).expect("probe image");
    std::fs::write(temp.join("probe.ttf"), [0, 1, 0, 0]).expect("probe font");
    let mut state = super::support::test_host_state();
    state.vfs = Some(sandbox.vfs());
    state.device_services = Some(Arc::new(Probe {
        log: Arc::clone(&log),
    }));
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: crate::main_extensions(state),
        op_metrics_factory_fn: Some(Box::new(move |_, _, decl| {
            let log = Arc::clone(&metrics_log);
            let name = decl.name;
            Some(Rc::new(move |_, event, _| {
                if event == deno_core::OpMetricsEvent::Dispatched {
                    log.lock().unwrap().push(format!("op:{name}"));
                }
            }))
        })),
        ..Default::default()
    });
    crate::harden_global_scope(&mut runtime);
    runtime
        .execute_script("<api-reach>", FastString::from_static(CALL))
        .expect("probe helper");
    let calls: BTreeMap<&str, &str> = CALLS.iter().copied().collect();

    let mut apis = serde_json::Map::new();
    for name in &names {
        let kind = eval_string(&mut runtime, format!("typeof migo[{name:?}]"));
        if kind == "undefined" {
            apis.insert(name.clone(), Value::Null);
            continue;
        }
        log.lock().unwrap().clear();
        if kind == "function" {
            let call = match calls.get(name.as_str()) {
                Some(call) => (*call).to_owned(),
                None if name.starts_with("on") || name.starts_with("off") => {
                    format!("__probeCall({name:?}, [function () {{}}])")
                }
                None => format!("__probeCall({name:?}, [{{}}])"),
            };
            eval_string(&mut runtime, format!("try {{ {call} }} catch (_) {{}}; ''"));
            reactor.block_on(async {
                let _ = tokio::time::timeout(
                    Duration::from_millis(20),
                    runtime.run_event_loop(PollEventLoopOptions::default()),
                )
                .await;
            });
        }
        let mut ops = BTreeSet::new();
        let mut services = BTreeSet::new();
        for entry in log.lock().unwrap().drain(..) {
            if let Some(op) = entry.strip_prefix("op:") {
                if !PLUMBING.contains(&op) {
                    ops.insert(op.to_owned());
                }
            } else if let Some(service) = entry.strip_prefix("svc:") {
                services.insert(service.to_owned());
            }
        }
        apis.insert(name.clone(), json!({ "ops": ops, "services": services }));
    }

    let rendered = render(&apis);
    let path = contract("minigame-api-reach.json");
    if std::env::var("MIGO_UPDATE_API_REACH").is_ok() {
        std::fs::write(&path, rendered).expect("write the reach record");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == rendered,
        "what the platform's APIs reach changed; review it and rerun with MIGO_UPDATE_API_REACH=1:\n{}",
        first_difference(&committed, &rendered)
    );
}

/// One API per line, so a change reads as the APIs it changed.
fn render(apis: &serde_json::Map<String, Value>) -> String {
    let mut out = String::from(
        "{\n  \"schema_version\": 1,\n  \"$comment\": [\n    \
         \"What each of the platform's APIs reaches when called: the ops it dispatched and the host services it asked for.\",\n    \
         \"null: migo does not publish it. Measured by engine/crates/runtime-v8/src/tests/api_reach.rs; regenerate with\",\n    \
         \"MIGO_UPDATE_API_REACH=1 cargo test -p migo-runtime-v8 --lib api_reach, and review the diff.\"\n  ],\n  \"apis\": {\n",
    );
    let count = apis.len();
    for (index, (name, reach)) in apis.iter().enumerate() {
        let separator = if index + 1 == count { "" } else { "," };
        out.push_str(&format!("    {}: {}{}\n", json!(name), reach, separator));
    }
    out.push_str("  }\n}\n");
    out
}

fn first_difference(committed: &str, rendered: &str) -> String {
    let mut differences = Vec::new();
    let committed: BTreeSet<&str> = committed.lines().collect();
    let rendered_lines: BTreeSet<&str> = rendered.lines().collect();
    for line in committed.difference(&rendered_lines).take(10) {
        differences.push(format!("- {line}"));
    }
    for line in rendered_lines.difference(&committed).take(10) {
        differences.push(format!("+ {line}"));
    }
    differences.join("\n")
}
