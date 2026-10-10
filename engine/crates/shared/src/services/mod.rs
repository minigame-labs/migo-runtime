//! Platform service traits for cross-platform device abstractions.
//!
//! These traits define the interface for device capabilities. Each platform
//! implements these traits, and ops in runtime-v8 call them through HostOpState.
//!
//! # Error Convention
//!
//! Methods return `Result<T, ServiceError>` where `ServiceError` carries a
//! typed error code and a human-readable message:
//! - `Err(ServiceError::not_supported("vibrateShort:fail not supported"))` - Feature not supported
//! - `Err(ServiceError::system("vibrateShort:fail system error"))` - Runtime error
//! - `Err(ServiceError::invalid_param("vibrateShort:fail invalid type"))` - Bad input

mod ad;
mod auth;
mod camera;
mod clipboard;
mod codec;
mod device;
mod file;
mod game_log;
pub mod host_files;
mod image_api;
mod interaction;
mod location;
mod navigate;
mod network;
mod payment;
mod permission;
mod scan_code;
mod share;
mod subpackage;
mod system_info;
mod video;

pub use ad::AdService;
pub use auth::AuthService;
pub use camera::CameraService;
pub use clipboard::ClipboardService;
pub use codec::CodecService;
pub use device::{
    AccelerometerService, AudioPlatformService, BLUETOOTH_RESULT_HOOKS, BatteryService,
    BluetoothService, CommerceServices, CompassService, ConnectivityServices, DeviceMotionService,
    DeviceServices, GyroscopeService, KeyboardService, MediaServices, RecorderService,
    ScreenService, SensorServices, SystemUtilServices, VibrationService,
};
pub use file::FileService;
pub use game_log::GameLogService;
pub use image_api::ImageApiService;
pub use interaction::InteractionService;
pub use location::LocationService;
pub use navigate::NavigateService;
pub use network::NetworkService;
pub use payment::PaymentService;
pub use permission::{PermissionService, Scope, ScopeState};
pub use scan_code::ScanCodeService;
pub use share::ShareService;
pub use subpackage::{
    SubpackageService, forget_downloaded_zips, intercept_download_result, take_downloaded_zip,
};
pub use system_info::{
    SystemInfoService, default_device_info_json, host_os_name, host_platform_name,
};
pub use video::VideoService;

pub use crate::protocol::error::{ServiceError, ServiceErrorCode};
