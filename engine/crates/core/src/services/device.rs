//! Device service traits re-exported from shared.
//!
//! See [`shared::services`] for the actual trait definitions.

pub use shared::services::{
    AccelerometerService, AdService, AudioPlatformService, AuthService, BatteryService,
    BlePeripheralService, BluetoothService, CameraService, ClipboardService, CodecService,
    CommerceServices, CompassService, ConnectivityServices, DeviceMotionService, DeviceServices,
    EcosystemService, FileService, GAMEPAD_EFFECT_MAX_MS, GameLogService, GamepadRumble,
    GamepadService, GyroscopeService, ImageApiService, InteractionService, KeyboardService,
    LocationService, MediaServices, NavigateService, NetworkService, PaymentService,
    PermissionService, RecorderService, ScanCodeService, Scope, ScopeState, ScreenService,
    SensorServices, ServiceError, ServiceErrorCode, ShareService, SubpackageService,
    SystemInfoService, SystemUtilServices, UpdateService, VibrationService, VideoDecoderService,
    VideoFrameSource, VideoService, WindowService,
};
