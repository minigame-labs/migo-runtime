//! The host-service channel's engine half.
//!
//! Content's ads, payments, sign-in, sharing and mini-program navigation reach a
//! C host as [`MigoHostServiceCall`]s through its dispatcher, and come back as a
//! completion (`migo_session_complete_host_service_call`) or a service event
//! (`migo_session_post_host_service_event`), which this module turns into the
//! host-bridge hook content is already waiting on -- the same hooks the Android
//! SDK's handlers answer through. See `include/migo/host_services.h` for the
//! host's side and `contracts/runtime/host-services.json` for the numbers.
//!
//! # A call id carries its own routing
//!
//! A call's id encodes the service, the method and content's request id, so a
//! completion needs no table to be routed: there is nothing to grow when a host
//! never answers, nothing to clean up when content goes away, and nothing to
//! lock on the way back. What that gives up is telling a completed call from one
//! never made -- and content does that already: request ids come from one
//! allocator per Host that a restart does not reset, a Session has one Host for
//! its life, and a result for an id content is not waiting on is discarded.
//! What a forged id could do is settle a request of the *same* method content
//! has outstanding, which is the host misreporting its own work, not an escape.

use std::sync::Arc;

use migo_capi_abi::{
    MIGO_ERROR_INTERNAL, MIGO_ERROR_INVALID_ARGUMENT, MIGO_OK, MigoResult,
    host_services::{
        HostServiceOutcome, MIGO_AD_CREATE, MIGO_AD_DESTROY, MIGO_AD_EVENT_LIFECYCLE, MIGO_AD_HIDE,
        MIGO_AD_LOAD, MIGO_AD_SHOW, MIGO_AD_UPDATE_STYLE, MIGO_AUTH_CHECK_SESSION,
        MIGO_AUTH_GET_PHONE_NUMBER, MIGO_AUTH_GET_USER_INFO, MIGO_AUTH_LOGIN,
        MIGO_BLUETOOTH_CLOSE_ADAPTER, MIGO_BLUETOOTH_CLOSE_BLE_CONNECTION,
        MIGO_BLUETOOTH_CREATE_BLE_CONNECTION, MIGO_BLUETOOTH_EVENT_ADAPTER_STATE_CHANGE,
        MIGO_BLUETOOTH_EVENT_BEACON_SERVICE_CHANGE, MIGO_BLUETOOTH_EVENT_BEACON_UPDATE,
        MIGO_BLUETOOTH_EVENT_BLE_CONNECTION_STATE_CHANGE, MIGO_BLUETOOTH_EVENT_BLE_MTU_CHANGE,
        MIGO_BLUETOOTH_EVENT_DEVICE_FOUND, MIGO_BLUETOOTH_GET_ADAPTER_STATE,
        MIGO_BLUETOOTH_GET_BEACONS, MIGO_BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS,
        MIGO_BLUETOOTH_GET_BLE_DEVICE_RSSI, MIGO_BLUETOOTH_GET_BLE_DEVICE_SERVICES,
        MIGO_BLUETOOTH_GET_BLE_MTU, MIGO_BLUETOOTH_GET_CONNECTED_DEVICES,
        MIGO_BLUETOOTH_GET_DEVICES, MIGO_BLUETOOTH_IS_DEVICE_PAIRED, MIGO_BLUETOOTH_MAKE_PAIR,
        MIGO_BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE, MIGO_BLUETOOTH_OPEN_ADAPTER,
        MIGO_BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE, MIGO_BLUETOOTH_SET_BLE_MTU,
        MIGO_BLUETOOTH_START_BEACON_DISCOVERY, MIGO_BLUETOOTH_START_DEVICES_DISCOVERY,
        MIGO_BLUETOOTH_STOP_BEACON_DISCOVERY, MIGO_BLUETOOTH_STOP_DEVICES_DISCOVERY,
        MIGO_BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE, MIGO_CLIPBOARD_GET_CLIPBOARD_DATA,
        MIGO_CLIPBOARD_SET_CLIPBOARD_DATA, MIGO_ECOSYSTEM_CALL, MIGO_ECOSYSTEM_EVENT_EVENT,
        MIGO_ECOSYSTEM_REPLY, MIGO_HOST_SERVICE_AD, MIGO_HOST_SERVICE_AUTH,
        MIGO_HOST_SERVICE_BLUETOOTH, MIGO_HOST_SERVICE_CLIPBOARD, MIGO_HOST_SERVICE_ECOSYSTEM,
        MIGO_HOST_SERVICE_IMAGE, MIGO_HOST_SERVICE_INTERACTION, MIGO_HOST_SERVICE_LOCATION,
        MIGO_HOST_SERVICE_MOTION, MIGO_HOST_SERVICE_NAVIGATE, MIGO_HOST_SERVICE_PAYMENT,
        MIGO_HOST_SERVICE_PERMISSION, MIGO_HOST_SERVICE_SCAN_CODE, MIGO_HOST_SERVICE_SCREEN,
        MIGO_HOST_SERVICE_SETTING, MIGO_HOST_SERVICE_SHARE, MIGO_HOST_SERVICE_SUBPACKAGE,
        MIGO_HOST_SERVICE_WINDOW, MIGO_IMAGE_CHOOSE_IMAGE, MIGO_IMAGE_CHOOSE_MEDIA,
        MIGO_IMAGE_CHOOSE_MESSAGE_FILE, MIGO_IMAGE_COMPRESS_IMAGE, MIGO_IMAGE_PREVIEW_IMAGE,
        MIGO_IMAGE_PREVIEW_MEDIA, MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM,
        MIGO_INTERACTION_HIDE_LOADING, MIGO_INTERACTION_HIDE_TOAST,
        MIGO_INTERACTION_SHOW_ACTION_SHEET, MIGO_INTERACTION_SHOW_LOADING,
        MIGO_INTERACTION_SHOW_MODAL, MIGO_INTERACTION_SHOW_TOAST, MIGO_LOCATION_GET_FUZZY_LOCATION,
        MIGO_LOCATION_GET_LOCATION, MIGO_MOTION_START_ACCELEROMETER, MIGO_MOTION_START_COMPASS,
        MIGO_MOTION_START_DEVICE_MOTION, MIGO_MOTION_START_GYROSCOPE,
        MIGO_MOTION_STOP_ACCELEROMETER, MIGO_MOTION_STOP_COMPASS, MIGO_MOTION_STOP_DEVICE_MOTION,
        MIGO_MOTION_STOP_GYROSCOPE, MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM,
        MIGO_NAVIGATE_NAVIGATE_TO_MINI_PROGRAM, MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION,
        MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT, MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM,
        MIGO_PERMISSION_REQUEST_SCOPE, MIGO_SCAN_CODE_SCAN_CODE,
        MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE, MIGO_SCREEN_EVENT_RECORDING_STATE_CHANGE,
        MIGO_SCREEN_EVENT_USER_CAPTURE_SCREEN, MIGO_SCREEN_GET_BRIGHTNESS,
        MIGO_SCREEN_GET_RECORDING_STATE, MIGO_SCREEN_SET_BRIGHTNESS,
        MIGO_SCREEN_SET_DEVICE_ORIENTATION, MIGO_SCREEN_SET_VISUAL_EFFECT_ON_CAPTURE,
        MIGO_SCREEN_START_CAPTURE_OBSERVER, MIGO_SCREEN_START_RECORDING_OBSERVER,
        MIGO_SCREEN_STOP_CAPTURE_OBSERVER, MIGO_SCREEN_STOP_RECORDING_OBSERVER,
        MIGO_SETTING_OPEN_APP_AUTHORIZE_SETTING, MIGO_SETTING_OPEN_SETTING,
        MIGO_SETTING_OPEN_SYSTEM_BLUETOOTH_SETTING, MIGO_SHARE_SHARE_APP_MESSAGE,
        MIGO_SUBPACKAGE_DOWNLOAD, MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE,
        MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE, MIGO_WINDOW_EXIT_POINTER_LOCK,
        MIGO_WINDOW_REQUEST_POINTER_LOCK, MIGO_WINDOW_SET_CURSOR, MIGO_WINDOW_SET_WINDOW_SIZE,
        MigoHostServiceResult, copy_bounded,
    },
};
use migo_core::services::{
    AccelerometerService, AdService, AuthService, BluetoothService, ClipboardService,
    CompassService, DeviceMotionService, EcosystemService, GyroscopeService, ImageApiService,
    InteractionService, LocationService, NavigateService, PaymentService, PermissionService,
    ScanCodeService, Scope, ScopeState, ScreenService, ShareService, SubpackageService,
    WindowService,
};
use serde_json::{Map, Value};
use shared::{
    js_escape::hook_args_one,
    protocol::error::ServiceError,
    protocol::host_cmd::HostCommand,
    services::{BLUETOOTH_RESULT_HOOKS, host_files::HostFiles},
};

use crate::{
    MigoSession, callbacks::Notifier, panic_barrier::guard, pin_session, settings::HostReports,
};

/// One request content makes that the host answers.
#[derive(Debug, PartialEq, Eq)]
struct Call {
    service: u32,
    method: u32,
    /// Where the outcome is delivered.
    hook: &'static str,
    /// The field a failure's numeric code travels in; `None` where the API
    /// defines none.
    error_code_field: Option<&'static str>,
    /// Where progress reported while the call is in flight is delivered;
    /// `None` for a call that reports none.
    progress_hook: Option<&'static str>,
    /// What happens to a result between the host and content.
    delivery: Delivery,
}

/// What happens to a result between the host and content.
#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    /// Delivered as the host completed it.
    Verbatim,
    /// A downloaded subpackage's `zipPath`: a host path, which content must
    /// not name -- the install ingests whatever file it names, so a path
    /// content could choose would make any zip the process can read installable
    /// as game code. It is recorded for the install and stripped from the result
    /// (`shared::services::intercept_download_result`), exactly as the Android
    /// SDK's path does.
    SubpackageZip,
    /// Fields naming files the host hands over (the contract's `files`): each is
    /// moved into the session's `/tmp` and the field rewritten to the sandbox
    /// path, and what the engine copied out for the request is released
    /// (`shared::services::host_files::deliver_result`).
    HostFiles(&'static HostFiles),
}

const REQUEST_MIDAS_PAYMENT: Call = Call {
    service: MIGO_HOST_SERVICE_PAYMENT,
    method: MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT,
    hook: "_internalOnMidasPaymentResult",
    error_code_field: Some("errCode"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const REQUEST_MIDAS_PAYMENT_GAME_ITEM: Call = Call {
    service: MIGO_HOST_SERVICE_PAYMENT,
    method: MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM,
    hook: "_internalOnMidasPaymentGameItemResult",
    error_code_field: Some("errCode"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const LOGIN: Call = Call {
    service: MIGO_HOST_SERVICE_AUTH,
    method: MIGO_AUTH_LOGIN,
    hook: "_internalOnLoginResult",
    error_code_field: Some("errno"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const CHECK_SESSION: Call = Call {
    service: MIGO_HOST_SERVICE_AUTH,
    method: MIGO_AUTH_CHECK_SESSION,
    hook: "_internalOnCheckSessionResult",
    error_code_field: Some("errno"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const GET_USER_INFO: Call = Call {
    service: MIGO_HOST_SERVICE_AUTH,
    method: MIGO_AUTH_GET_USER_INFO,
    hook: "_internalOnGetUserInfoResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const GET_PHONE_NUMBER: Call = Call {
    service: MIGO_HOST_SERVICE_AUTH,
    method: MIGO_AUTH_GET_PHONE_NUMBER,
    hook: "_internalOnGetPhoneNumberResult",
    error_code_field: Some("errno"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const SHARE_APP_MESSAGE: Call = Call {
    service: MIGO_HOST_SERVICE_SHARE,
    method: MIGO_SHARE_SHARE_APP_MESSAGE,
    hook: "_internalOnShareAppMessageResult",
    error_code_field: Some("errCode"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const NAVIGATE_TO_MINI_PROGRAM: Call = Call {
    service: MIGO_HOST_SERVICE_NAVIGATE,
    method: MIGO_NAVIGATE_NAVIGATE_TO_MINI_PROGRAM,
    hook: "_internalOnNavigateToMiniProgramResult",
    error_code_field: Some("errCode"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};

const SUBPACKAGE_DOWNLOAD: Call = Call {
    service: MIGO_HOST_SERVICE_SUBPACKAGE,
    method: MIGO_SUBPACKAGE_DOWNLOAD,
    hook: "_internalOnSubpackageResult",
    error_code_field: None,
    progress_hook: Some("_internalOnSubpackageProgress"),
    delivery: Delivery::SubpackageZip,
};
const REQUEST_SCOPE: Call = Call {
    service: MIGO_HOST_SERVICE_PERMISSION,
    method: MIGO_PERMISSION_REQUEST_SCOPE,
    hook: "_internalOnAuthorizeResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const OPEN_SETTING: Call = Call {
    service: MIGO_HOST_SERVICE_SETTING,
    method: MIGO_SETTING_OPEN_SETTING,
    hook: "_internalOnOpenSettingResult",
    error_code_field: Some("errCode"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const OPEN_SYSTEM_BLUETOOTH_SETTING: Call = Call {
    service: MIGO_HOST_SERVICE_SETTING,
    method: MIGO_SETTING_OPEN_SYSTEM_BLUETOOTH_SETTING,
    hook: "_internalOnOpenBluetoothSettingResult",
    error_code_field: Some("errCode"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const OPEN_APP_AUTHORIZE_SETTING: Call = Call {
    service: MIGO_HOST_SERVICE_SETTING,
    method: MIGO_SETTING_OPEN_APP_AUTHORIZE_SETTING,
    hook: "_internalOnOpenAppAuthorizeSettingFinished",
    error_code_field: Some("errCode"),
    progress_hook: None,
    delivery: Delivery::Verbatim,
};

const SHOW_MODAL: Call = Call {
    service: MIGO_HOST_SERVICE_INTERACTION,
    method: MIGO_INTERACTION_SHOW_MODAL,
    hook: "_internalOnModalResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const SHOW_ACTION_SHEET: Call = Call {
    service: MIGO_HOST_SERVICE_INTERACTION,
    method: MIGO_INTERACTION_SHOW_ACTION_SHEET,
    hook: "_internalOnActionSheetResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const SET_CLIPBOARD_DATA: Call = Call {
    service: MIGO_HOST_SERVICE_CLIPBOARD,
    method: MIGO_CLIPBOARD_SET_CLIPBOARD_DATA,
    hook: "_internalOnSetClipboardDataResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const GET_CLIPBOARD_DATA: Call = Call {
    service: MIGO_HOST_SERVICE_CLIPBOARD,
    method: MIGO_CLIPBOARD_GET_CLIPBOARD_DATA,
    hook: "_internalOnGetClipboardDataResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const SCAN_CODE: Call = Call {
    service: MIGO_HOST_SERVICE_SCAN_CODE,
    method: MIGO_SCAN_CODE_SCAN_CODE,
    hook: "_internalOnScanCodeResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const GET_LOCATION: Call = Call {
    service: MIGO_HOST_SERVICE_LOCATION,
    method: MIGO_LOCATION_GET_LOCATION,
    hook: "_internalOnLocationResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const GET_FUZZY_LOCATION: Call = Call {
    service: MIGO_HOST_SERVICE_LOCATION,
    method: MIGO_LOCATION_GET_FUZZY_LOCATION,
    hook: "_internalOnFuzzyLocationResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};

const SAVE_IMAGE_TO_PHOTOS_ALBUM: Call = Call {
    service: MIGO_HOST_SERVICE_IMAGE,
    method: MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM,
    hook: "_internalOnSaveImageToPhotosAlbumResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::HostFiles(&HostFiles::NONE),
};
const PREVIEW_IMAGE: Call = Call {
    service: MIGO_HOST_SERVICE_IMAGE,
    method: MIGO_IMAGE_PREVIEW_IMAGE,
    hook: "_internalOnPreviewImageResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::HostFiles(&HostFiles::NONE),
};
const PREVIEW_MEDIA: Call = Call {
    service: MIGO_HOST_SERVICE_IMAGE,
    method: MIGO_IMAGE_PREVIEW_MEDIA,
    hook: "_internalOnPreviewMediaResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::HostFiles(&HostFiles::NONE),
};
const COMPRESS_IMAGE: Call = Call {
    service: MIGO_HOST_SERVICE_IMAGE,
    method: MIGO_IMAGE_COMPRESS_IMAGE,
    hook: "_internalOnCompressImageResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::HostFiles(&HostFiles::COMPRESS_IMAGE),
};
const CHOOSE_IMAGE: Call = Call {
    service: MIGO_HOST_SERVICE_IMAGE,
    method: MIGO_IMAGE_CHOOSE_IMAGE,
    hook: "_internalOnChooseImageResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::HostFiles(&HostFiles::CHOOSE_IMAGE),
};
const CHOOSE_MESSAGE_FILE: Call = Call {
    service: MIGO_HOST_SERVICE_IMAGE,
    method: MIGO_IMAGE_CHOOSE_MESSAGE_FILE,
    hook: "_internalOnChooseMessageFileResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::HostFiles(&HostFiles::CHOOSE_MESSAGE_FILE),
};
const CHOOSE_MEDIA: Call = Call {
    service: MIGO_HOST_SERVICE_IMAGE,
    method: MIGO_IMAGE_CHOOSE_MEDIA,
    hook: "_internalOnChooseMediaResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::HostFiles(&HostFiles::CHOOSE_MEDIA),
};

const START_ACCELEROMETER: Call = Call {
    service: MIGO_HOST_SERVICE_MOTION,
    method: MIGO_MOTION_START_ACCELEROMETER,
    hook: "_internalOnStartAccelerometerResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const START_GYROSCOPE: Call = Call {
    service: MIGO_HOST_SERVICE_MOTION,
    method: MIGO_MOTION_START_GYROSCOPE,
    hook: "_internalOnStartGyroscopeResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const START_COMPASS: Call = Call {
    service: MIGO_HOST_SERVICE_MOTION,
    method: MIGO_MOTION_START_COMPASS,
    hook: "_internalOnStartCompassResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const START_DEVICE_MOTION: Call = Call {
    service: MIGO_HOST_SERVICE_MOTION,
    method: MIGO_MOTION_START_DEVICE_MOTION,
    hook: "_internalOnStartDeviceMotionListeningResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const GET_SCREEN_BRIGHTNESS: Call = Call {
    service: MIGO_HOST_SERVICE_SCREEN,
    method: MIGO_SCREEN_GET_BRIGHTNESS,
    hook: "_internalOnGetScreenBrightnessResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const SET_SCREEN_BRIGHTNESS: Call = Call {
    service: MIGO_HOST_SERVICE_SCREEN,
    method: MIGO_SCREEN_SET_BRIGHTNESS,
    hook: "_internalOnSetScreenBrightnessResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const SET_DEVICE_ORIENTATION: Call = Call {
    service: MIGO_HOST_SERVICE_SCREEN,
    method: MIGO_SCREEN_SET_DEVICE_ORIENTATION,
    hook: "_internalOnSetDeviceOrientationResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const GET_SCREEN_RECORDING_STATE: Call = Call {
    service: MIGO_HOST_SERVICE_SCREEN,
    method: MIGO_SCREEN_GET_RECORDING_STATE,
    hook: "_internalOnGetScreenRecordingStateResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};
const SET_VISUAL_EFFECT_ON_CAPTURE: Call = Call {
    service: MIGO_HOST_SERVICE_SCREEN,
    method: MIGO_SCREEN_SET_VISUAL_EFFECT_ON_CAPTURE,
    hook: "_internalOnSetVisualEffectOnCaptureResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};

/// A Bluetooth request: answered through the hook the one shared table names
/// for its method, with failures coded in `errCode`.
const fn bluetooth_call(method: u32) -> Call {
    Call {
        service: MIGO_HOST_SERVICE_BLUETOOTH,
        method,
        hook: BLUETOOTH_RESULT_HOOKS[method as usize],
        error_code_field: Some("errCode"),
        progress_hook: None,
        delivery: Delivery::Verbatim,
    }
}

const BLUETOOTH_OPEN_ADAPTER: Call = bluetooth_call(MIGO_BLUETOOTH_OPEN_ADAPTER);
const BLUETOOTH_CLOSE_ADAPTER: Call = bluetooth_call(MIGO_BLUETOOTH_CLOSE_ADAPTER);
const BLUETOOTH_GET_ADAPTER_STATE: Call = bluetooth_call(MIGO_BLUETOOTH_GET_ADAPTER_STATE);
const BLUETOOTH_START_DEVICES_DISCOVERY: Call =
    bluetooth_call(MIGO_BLUETOOTH_START_DEVICES_DISCOVERY);
const BLUETOOTH_STOP_DEVICES_DISCOVERY: Call =
    bluetooth_call(MIGO_BLUETOOTH_STOP_DEVICES_DISCOVERY);
const BLUETOOTH_GET_DEVICES: Call = bluetooth_call(MIGO_BLUETOOTH_GET_DEVICES);
const BLUETOOTH_GET_CONNECTED_DEVICES: Call = bluetooth_call(MIGO_BLUETOOTH_GET_CONNECTED_DEVICES);
const BLUETOOTH_MAKE_PAIR: Call = bluetooth_call(MIGO_BLUETOOTH_MAKE_PAIR);
const BLUETOOTH_IS_DEVICE_PAIRED: Call = bluetooth_call(MIGO_BLUETOOTH_IS_DEVICE_PAIRED);
const BLUETOOTH_START_BEACON_DISCOVERY: Call =
    bluetooth_call(MIGO_BLUETOOTH_START_BEACON_DISCOVERY);
const BLUETOOTH_STOP_BEACON_DISCOVERY: Call = bluetooth_call(MIGO_BLUETOOTH_STOP_BEACON_DISCOVERY);
const BLUETOOTH_GET_BEACONS: Call = bluetooth_call(MIGO_BLUETOOTH_GET_BEACONS);
const BLUETOOTH_CREATE_BLE_CONNECTION: Call = bluetooth_call(MIGO_BLUETOOTH_CREATE_BLE_CONNECTION);
const BLUETOOTH_CLOSE_BLE_CONNECTION: Call = bluetooth_call(MIGO_BLUETOOTH_CLOSE_BLE_CONNECTION);
const BLUETOOTH_GET_BLE_DEVICE_SERVICES: Call =
    bluetooth_call(MIGO_BLUETOOTH_GET_BLE_DEVICE_SERVICES);
const BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS: Call =
    bluetooth_call(MIGO_BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS);
const BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE: Call =
    bluetooth_call(MIGO_BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE);
const BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE: Call =
    bluetooth_call(MIGO_BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE);
const BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE: Call =
    bluetooth_call(MIGO_BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE);
const BLUETOOTH_GET_BLE_DEVICE_RSSI: Call = bluetooth_call(MIGO_BLUETOOTH_GET_BLE_DEVICE_RSSI);
const BLUETOOTH_SET_BLE_MTU: Call = bluetooth_call(MIGO_BLUETOOTH_SET_BLE_MTU);
const BLUETOOTH_GET_BLE_MTU: Call = bluetooth_call(MIGO_BLUETOOTH_GET_BLE_MTU);

const SET_WINDOW_SIZE: Call = Call {
    service: MIGO_HOST_SERVICE_WINDOW,
    method: MIGO_WINDOW_SET_WINDOW_SIZE,
    hook: "_internalOnSetWindowSizeResult",
    error_code_field: None,
    progress_hook: None,
    delivery: Delivery::Verbatim,
};

const ECOSYSTEM_CALL: Call = Call {
    service: MIGO_HOST_SERVICE_ECOSYSTEM,
    method: MIGO_ECOSYSTEM_CALL,
    hook: "_internalOnEcosystemResult",
    error_code_field: Some("errCode"),
    progress_hook: None,
    // Names no file the host hands over, but releases what was copied out of the
    // package for the request (shareImageToGroup's image, ...).
    delivery: Delivery::HostFiles(&HostFiles::NONE),
};

const CALLS: &[&Call] = &[
    &REQUEST_MIDAS_PAYMENT,
    &REQUEST_MIDAS_PAYMENT_GAME_ITEM,
    &LOGIN,
    &CHECK_SESSION,
    &GET_USER_INFO,
    &GET_PHONE_NUMBER,
    &SHARE_APP_MESSAGE,
    &NAVIGATE_TO_MINI_PROGRAM,
    &SUBPACKAGE_DOWNLOAD,
    &REQUEST_SCOPE,
    &OPEN_SETTING,
    &OPEN_SYSTEM_BLUETOOTH_SETTING,
    &OPEN_APP_AUTHORIZE_SETTING,
    &SHOW_MODAL,
    &SHOW_ACTION_SHEET,
    &SET_CLIPBOARD_DATA,
    &GET_CLIPBOARD_DATA,
    &SCAN_CODE,
    &GET_LOCATION,
    &GET_FUZZY_LOCATION,
    &SAVE_IMAGE_TO_PHOTOS_ALBUM,
    &PREVIEW_IMAGE,
    &PREVIEW_MEDIA,
    &COMPRESS_IMAGE,
    &CHOOSE_IMAGE,
    &CHOOSE_MESSAGE_FILE,
    &CHOOSE_MEDIA,
    &START_ACCELEROMETER,
    &START_GYROSCOPE,
    &START_COMPASS,
    &START_DEVICE_MOTION,
    &GET_SCREEN_BRIGHTNESS,
    &SET_SCREEN_BRIGHTNESS,
    &SET_DEVICE_ORIENTATION,
    &GET_SCREEN_RECORDING_STATE,
    &SET_VISUAL_EFFECT_ON_CAPTURE,
    &BLUETOOTH_OPEN_ADAPTER,
    &BLUETOOTH_CLOSE_ADAPTER,
    &BLUETOOTH_GET_ADAPTER_STATE,
    &BLUETOOTH_START_DEVICES_DISCOVERY,
    &BLUETOOTH_STOP_DEVICES_DISCOVERY,
    &BLUETOOTH_GET_DEVICES,
    &BLUETOOTH_GET_CONNECTED_DEVICES,
    &BLUETOOTH_MAKE_PAIR,
    &BLUETOOTH_IS_DEVICE_PAIRED,
    &BLUETOOTH_START_BEACON_DISCOVERY,
    &BLUETOOTH_STOP_BEACON_DISCOVERY,
    &BLUETOOTH_GET_BEACONS,
    &BLUETOOTH_CREATE_BLE_CONNECTION,
    &BLUETOOTH_CLOSE_BLE_CONNECTION,
    &BLUETOOTH_GET_BLE_DEVICE_SERVICES,
    &BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS,
    &BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE,
    &BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE,
    &BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE,
    &BLUETOOTH_GET_BLE_DEVICE_RSSI,
    &BLUETOOTH_SET_BLE_MTU,
    &BLUETOOTH_GET_BLE_MTU,
    &SET_WINDOW_SIZE,
    &ECOSYSTEM_CALL,
];

/// Fire-and-forget requests, as `(service, method)`. Listed so the contract test
/// can hold the whole method set to the contract, not only the calls; a command
/// needs no routing back, so nothing else reads it.
#[cfg(test)]
const COMMANDS: &[(u32, u32)] = &[
    (MIGO_HOST_SERVICE_AD, MIGO_AD_CREATE),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_LOAD),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_SHOW),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_HIDE),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_UPDATE_STYLE),
    (MIGO_HOST_SERVICE_AD, MIGO_AD_DESTROY),
    (
        MIGO_HOST_SERVICE_NAVIGATE,
        MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM,
    ),
    (
        MIGO_HOST_SERVICE_NAVIGATE,
        MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION,
    ),
    (MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_TOAST),
    (MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_HIDE_TOAST),
    (MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_SHOW_LOADING),
    (MIGO_HOST_SERVICE_INTERACTION, MIGO_INTERACTION_HIDE_LOADING),
    (MIGO_HOST_SERVICE_MOTION, MIGO_MOTION_STOP_ACCELEROMETER),
    (MIGO_HOST_SERVICE_MOTION, MIGO_MOTION_STOP_GYROSCOPE),
    (MIGO_HOST_SERVICE_MOTION, MIGO_MOTION_STOP_COMPASS),
    (MIGO_HOST_SERVICE_MOTION, MIGO_MOTION_STOP_DEVICE_MOTION),
    (MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_START_CAPTURE_OBSERVER),
    (MIGO_HOST_SERVICE_SCREEN, MIGO_SCREEN_STOP_CAPTURE_OBSERVER),
    (
        MIGO_HOST_SERVICE_SCREEN,
        MIGO_SCREEN_START_RECORDING_OBSERVER,
    ),
    (
        MIGO_HOST_SERVICE_SCREEN,
        MIGO_SCREEN_STOP_RECORDING_OBSERVER,
    ),
    (MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_CURSOR),
    (MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_REQUEST_POINTER_LOCK),
    (MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_EXIT_POINTER_LOCK),
    (MIGO_HOST_SERVICE_ECOSYSTEM, MIGO_ECOSYSTEM_REPLY),
];

/// A service's own events, as `(service, event, hook)`.
const EVENTS: &[(u32, u32, &str)] = &[
    (
        MIGO_HOST_SERVICE_AD,
        MIGO_AD_EVENT_LIFECYCLE,
        "_internalOnAdEvent",
    ),
    (
        MIGO_HOST_SERVICE_SCREEN,
        MIGO_SCREEN_EVENT_USER_CAPTURE_SCREEN,
        "_internalTriggerUserCaptureScreen",
    ),
    (
        MIGO_HOST_SERVICE_SCREEN,
        MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE,
        "_internalOnDeviceOrientationEvent",
    ),
    (
        MIGO_HOST_SERVICE_SCREEN,
        MIGO_SCREEN_EVENT_RECORDING_STATE_CHANGE,
        "_internalOnScreenRecordingStateEvent",
    ),
    (
        MIGO_HOST_SERVICE_BLUETOOTH,
        MIGO_BLUETOOTH_EVENT_ADAPTER_STATE_CHANGE,
        "_internalOnBluetoothAdapterStateEvent",
    ),
    (
        MIGO_HOST_SERVICE_BLUETOOTH,
        MIGO_BLUETOOTH_EVENT_DEVICE_FOUND,
        "_internalOnBluetoothDeviceFoundEvent",
    ),
    (
        MIGO_HOST_SERVICE_BLUETOOTH,
        MIGO_BLUETOOTH_EVENT_BLE_CONNECTION_STATE_CHANGE,
        "_internalOnBLEConnectionStateEvent",
    ),
    (
        MIGO_HOST_SERVICE_BLUETOOTH,
        MIGO_BLUETOOTH_EVENT_BLE_MTU_CHANGE,
        "_internalOnBLEMTUEvent",
    ),
    (
        MIGO_HOST_SERVICE_BLUETOOTH,
        MIGO_BLUETOOTH_EVENT_BEACON_UPDATE,
        "_internalOnBeaconUpdateEvent",
    ),
    (
        MIGO_HOST_SERVICE_BLUETOOTH,
        MIGO_BLUETOOTH_EVENT_BEACON_SERVICE_CHANGE,
        "_internalOnBeaconServiceEvent",
    ),
    (
        MIGO_HOST_SERVICE_WINDOW,
        MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE,
        "_internalOnWindowStateEvent",
    ),
    (
        MIGO_HOST_SERVICE_WINDOW,
        MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE,
        "_internalOnPointerLockEvent",
    ),
    (
        MIGO_HOST_SERVICE_ECOSYSTEM,
        MIGO_ECOSYSTEM_EVENT_EVENT,
        "_internalOnEcosystemEvent",
    ),
];

fn event_hook(service: u32, event: u32) -> Option<&'static str> {
    EVENTS
        .iter()
        .find(|(s, e, _)| *s == service && *e == event)
        .map(|(_, _, hook)| *hook)
}

// ---- call ids ---------------------------------------------------------------

const METHOD_SHIFT: u32 = 32;
const SERVICE_SHIFT: u32 = 48;
/// Content's request ids, as `parseHostCallbackId` in `02_async.js` accepts them.
const MAX_REQUEST_ID: u64 = i32::MAX as u64;

fn encode_call_id(call: &Call, request_id: u32) -> u64 {
    debug_assert!(request_id != 0 && u64::from(request_id) <= MAX_REQUEST_ID);
    (u64::from(call.service) << SERVICE_SHIFT)
        | (u64::from(call.method) << METHOD_SHIFT)
        | u64::from(request_id)
}

fn decode_call_id(call_id: u64) -> Option<(&'static Call, u32)> {
    let request_id = call_id & 0xffff_ffff;
    if request_id == 0 || request_id > MAX_REQUEST_ID {
        return None;
    }
    let method = ((call_id >> METHOD_SHIFT) & 0xffff) as u32;
    let service = (call_id >> SERVICE_SHIFT) as u32;
    let call = CALLS
        .iter()
        .copied()
        .find(|call| call.service == service && call.method == method)?;
    Some((call, request_id as u32))
}

// ---- outbound ---------------------------------------------------------------

/// Split content's request into its id and the options the host is given.
///
/// The id is content's bookkeeping, not part of the request, and the host never
/// needs it: it answers by call id. A request without a valid one is an engine
/// defect -- every call site allocates one -- so it is refused, not guessed.
fn split_request(options_json: &str) -> Option<(u32, String)> {
    let Value::Object(mut options) = serde_json::from_str::<Value>(options_json).ok()? else {
        return None;
    };
    let request_id = options.remove("requestId")?.as_u64()?;
    if request_id == 0 || request_id > MAX_REQUEST_ID {
        return None;
    }
    Some((request_id as u32, Value::Object(options).to_string()))
}

/// Content's requests of the host's services, carried to a C host that declared
/// it supplies them.
pub(crate) struct CapiHostServices {
    notifier: Arc<Notifier>,
    /// The host's standing permission decisions, which the permission service
    /// answers from.
    reports: Arc<HostReports>,
}

impl CapiHostServices {
    pub(crate) fn new(notifier: Arc<Notifier>, reports: Arc<HostReports>) -> Arc<Self> {
        Arc::new(Self { notifier, reports })
    }

    /// Whether the host declared `service`.
    #[inline]
    pub(crate) fn supplies(&self, service: u32) -> bool {
        self.notifier.supplies_host_service(service)
    }

    fn command(&self, service: u32, method: u32, options_json: &str) -> Result<(), ServiceError> {
        if self
            .notifier
            .host_service_call(service, method, 0, options_json.to_owned())
        {
            Ok(())
        } else {
            Err(ServiceError::not_supported(
                "host dispatcher refused the request",
            ))
        }
    }

    fn call(&self, call: &Call, options_json: &str) -> Result<(), ServiceError> {
        let Some((request_id, payload)) = split_request(options_json) else {
            return Err(ServiceError::invalid_param("malformed request"));
        };
        self.post_call(call, request_id, payload)
    }

    /// A call whose request id content passed as an argument rather than in
    /// its options, with nothing else to say.
    fn call_by_id(&self, call: &Call, request_id: i32) -> Result<(), ServiceError> {
        let Ok(request_id) = u32::try_from(request_id) else {
            return Err(ServiceError::invalid_param("malformed request"));
        };
        if request_id == 0 {
            return Err(ServiceError::invalid_param("malformed request"));
        }
        self.post_call(call, request_id, "{}".to_owned())
    }

    fn post_call(&self, call: &Call, request_id: u32, payload: String) -> Result<(), ServiceError> {
        if self.notifier.host_service_call(
            call.service,
            call.method,
            encode_call_id(call, request_id),
            payload,
        ) {
            Ok(())
        } else {
            Err(ServiceError::not_supported(
                "host dispatcher refused the request",
            ))
        }
    }
}

impl SubpackageService for CapiHostServices {
    fn download_subpackage(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&SUBPACKAGE_DOWNLOAD, options_json)
    }
}

impl PermissionService for CapiHostServices {
    fn scope_state(&self, scope: Scope) -> ScopeState {
        self.reports.scope_state(scope)
    }

    fn request_scope(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&REQUEST_SCOPE, request_json)
    }
}

/// The settings pages content can send the player to, for [`crate::settings`]'s
/// system-information service to forward.
impl CapiHostServices {
    pub(crate) fn open_setting(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&OPEN_SETTING, options_json)
    }

    pub(crate) fn open_system_bluetooth_setting(
        &self,
        request_id: i32,
    ) -> Result<(), ServiceError> {
        self.call_by_id(&OPEN_SYSTEM_BLUETOOTH_SETTING, request_id)
    }

    pub(crate) fn open_app_authorize_setting(&self, request_id: i32) -> Result<(), ServiceError> {
        self.call_by_id(&OPEN_APP_AUTHORIZE_SETTING, request_id)
    }
}

impl InteractionService for CapiHostServices {
    fn show_toast(&self, json: &str) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_INTERACTION,
            MIGO_INTERACTION_SHOW_TOAST,
            json,
        )
    }
    fn hide_toast(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_INTERACTION,
            MIGO_INTERACTION_HIDE_TOAST,
            "{}",
        )
    }
    fn show_modal(&self, json: &str) -> Result<(), ServiceError> {
        self.call(&SHOW_MODAL, json)
    }
    fn show_loading(&self, json: &str) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_INTERACTION,
            MIGO_INTERACTION_SHOW_LOADING,
            json,
        )
    }
    fn hide_loading(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_INTERACTION,
            MIGO_INTERACTION_HIDE_LOADING,
            "{}",
        )
    }
    fn show_action_sheet(&self, json: &str) -> Result<(), ServiceError> {
        self.call(&SHOW_ACTION_SHEET, json)
    }
}

impl ClipboardService for CapiHostServices {
    fn set_data(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&SET_CLIPBOARD_DATA, request_json)
    }
    fn get_data(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_CLIPBOARD_DATA, request_json)
    }
}

impl ScanCodeService for CapiHostServices {
    fn scan_code(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&SCAN_CODE, options_json)
    }
}

impl LocationService for CapiHostServices {
    fn get_location(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_LOCATION, options_json)
    }
    fn get_fuzzy_location(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_FUZZY_LOCATION, options_json)
    }
}

// Paths in these requests were resolved from the sandbox by the ops that make
// them, and the files their results name are taken over on delivery.
impl ImageApiService for CapiHostServices {
    fn save_image_to_photos_album(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&SAVE_IMAGE_TO_PHOTOS_ALBUM, request_json)
    }
    fn preview_image(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&PREVIEW_IMAGE, request_json)
    }
    fn preview_media(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&PREVIEW_MEDIA, request_json)
    }
    fn compress_image(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&COMPRESS_IMAGE, request_json)
    }
    fn choose_image(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&CHOOSE_IMAGE, request_json)
    }
    fn choose_message_file(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&CHOOSE_MESSAGE_FILE, request_json)
    }
    fn choose_media(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&CHOOSE_MEDIA, request_json)
    }
}

impl AccelerometerService for CapiHostServices {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&START_ACCELEROMETER, request_json)
    }
    fn stop(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_MOTION,
            MIGO_MOTION_STOP_ACCELEROMETER,
            "{}",
        )
    }
}

impl GyroscopeService for CapiHostServices {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&START_GYROSCOPE, request_json)
    }
    fn stop(&self) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_MOTION, MIGO_MOTION_STOP_GYROSCOPE, "{}")
    }
}

impl CompassService for CapiHostServices {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&START_COMPASS, request_json)
    }
    fn stop(&self) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_MOTION, MIGO_MOTION_STOP_COMPASS, "{}")
    }
}

impl DeviceMotionService for CapiHostServices {
    fn start(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&START_DEVICE_MOTION, request_json)
    }
    fn stop(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_MOTION,
            MIGO_MOTION_STOP_DEVICE_MOTION,
            "{}",
        )
    }
}

// Keeping the screen on is the typed callback, not this channel
// (`host_kit::CapiScreen` joins the two).
impl ScreenService for CapiHostServices {
    fn get_brightness(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_SCREEN_BRIGHTNESS, request_json)
    }
    fn set_brightness(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&SET_SCREEN_BRIGHTNESS, request_json)
    }
    fn set_orientation(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&SET_DEVICE_ORIENTATION, request_json)
    }
    fn start_capture_screen(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_SCREEN,
            MIGO_SCREEN_START_CAPTURE_OBSERVER,
            "{}",
        )
    }
    fn stop_capture_screen(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_SCREEN,
            MIGO_SCREEN_STOP_CAPTURE_OBSERVER,
            "{}",
        )
    }
    fn get_screen_recording_state(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_SCREEN_RECORDING_STATE, request_json)
    }
    fn start_screen_recording_observer(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_SCREEN,
            MIGO_SCREEN_START_RECORDING_OBSERVER,
            "{}",
        )
    }
    fn stop_screen_recording_observer(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_SCREEN,
            MIGO_SCREEN_STOP_RECORDING_OBSERVER,
            "{}",
        )
    }
    fn set_visual_effect_on_capture(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&SET_VISUAL_EFFECT_ON_CAPTURE, request_json)
    }
}

impl BluetoothService for CapiHostServices {
    fn open_adapter(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_OPEN_ADAPTER, request_json)
    }
    fn close_adapter(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_CLOSE_ADAPTER, request_json)
    }
    fn get_adapter_state(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_GET_ADAPTER_STATE, request_json)
    }
    fn start_devices_discovery(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_START_DEVICES_DISCOVERY, request_json)
    }
    fn stop_devices_discovery(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_STOP_DEVICES_DISCOVERY, request_json)
    }
    fn get_devices(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_GET_DEVICES, request_json)
    }
    fn get_connected_devices(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_GET_CONNECTED_DEVICES, request_json)
    }
    fn make_pair(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_MAKE_PAIR, request_json)
    }
    fn is_device_paired(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_IS_DEVICE_PAIRED, request_json)
    }
    fn start_beacon_discovery(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_START_BEACON_DISCOVERY, request_json)
    }
    fn stop_beacon_discovery(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_STOP_BEACON_DISCOVERY, request_json)
    }
    fn get_beacons(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_GET_BEACONS, request_json)
    }
    fn create_ble_connection(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_CREATE_BLE_CONNECTION, request_json)
    }
    fn close_ble_connection(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_CLOSE_BLE_CONNECTION, request_json)
    }
    fn get_ble_device_services(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_GET_BLE_DEVICE_SERVICES, request_json)
    }
    fn get_ble_device_characteristics(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS, request_json)
    }
    fn read_ble_characteristic_value(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE, request_json)
    }
    fn write_ble_characteristic_value(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE, request_json)
    }
    fn notify_ble_characteristic_value_change(
        &self,
        request_json: &str,
    ) -> Result<(), ServiceError> {
        self.call(
            &BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE,
            request_json,
        )
    }
    fn get_ble_device_rssi(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_GET_BLE_DEVICE_RSSI, request_json)
    }
    fn set_ble_mtu(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_SET_BLE_MTU, request_json)
    }
    fn get_ble_mtu(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&BLUETOOTH_GET_BLE_MTU, request_json)
    }
}

impl WindowService for CapiHostServices {
    fn set_window_size(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&SET_WINDOW_SIZE, request_json)
    }
    fn set_cursor(&self, json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_WINDOW, MIGO_WINDOW_SET_CURSOR, json)
    }
    fn request_pointer_lock(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_WINDOW,
            MIGO_WINDOW_REQUEST_POINTER_LOCK,
            "{}",
        )
    }
    fn exit_pointer_lock(&self) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_WINDOW,
            MIGO_WINDOW_EXIT_POINTER_LOCK,
            "{}",
        )
    }
}

impl EcosystemService for CapiHostServices {
    fn call(&self, request_json: &str) -> Result<(), ServiceError> {
        self.call(&ECOSYSTEM_CALL, request_json)
    }
    fn reply(&self, json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_ECOSYSTEM, MIGO_ECOSYSTEM_REPLY, json)
    }
    fn value(&self, name: &str) -> Option<String> {
        self.reports.ecosystem_value(name)
    }
}

impl AdService for CapiHostServices {
    fn create_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_CREATE, request_json)
    }
    fn load_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_LOAD, request_json)
    }
    fn show_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_SHOW, request_json)
    }
    fn hide_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_HIDE, request_json)
    }
    fn update_ad_style(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_UPDATE_STYLE, request_json)
    }
    fn destroy_ad(&self, request_json: &str) -> Result<(), ServiceError> {
        self.command(MIGO_HOST_SERVICE_AD, MIGO_AD_DESTROY, request_json)
    }
}

impl PaymentService for CapiHostServices {
    /// A host that declared the payment service is one that can take payments;
    /// one that cannot right now -- no store account, a region without it --
    /// says so by failing the payment, which is what content handles anyway.
    fn check_is_support_midas_payment(&self, _options_json: &str) -> Result<String, ServiceError> {
        Ok(r#"{"data":{"allow_pay":true}}"#.to_owned())
    }
    fn request_midas_payment(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&REQUEST_MIDAS_PAYMENT, options_json)
    }
    fn request_midas_payment_game_item(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&REQUEST_MIDAS_PAYMENT_GAME_ITEM, options_json)
    }
}

impl AuthService for CapiHostServices {
    fn login(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&LOGIN, options_json)
    }
    fn check_session(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&CHECK_SESSION, options_json)
    }
    fn get_user_info(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_USER_INFO, options_json)
    }
    fn get_phone_number(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&GET_PHONE_NUMBER, options_json)
    }
}

impl ShareService for CapiHostServices {
    fn share_app_message(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&SHARE_APP_MESSAGE, options_json)
    }
}

impl NavigateService for CapiHostServices {
    fn navigate_to_mini_program(&self, options_json: &str) -> Result<(), ServiceError> {
        self.call(&NAVIGATE_TO_MINI_PROGRAM, options_json)
    }
    fn navigate_back_mini_program(&self, options_json: &str) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_NAVIGATE,
            MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM,
            options_json,
        )
    }
    fn open_customer_service_conversation(&self, options_json: &str) -> Result<(), ServiceError> {
        self.command(
            MIGO_HOST_SERVICE_NAVIGATE,
            MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION,
            options_json,
        )
    }
}

// ---- inbound ----------------------------------------------------------------

/// Names content's result hooks write themselves, which a host's result must
/// not supply: `error` would turn a success into a failure, `requestId` would
/// redirect it, and `errMsg` is composed by content from the outcome.
const RESERVED_RESULT_FIELDS: [&str; 3] = ["requestId", "error", "errMsg"];

/// The JSON a call's hook receives, in the shape the Android SDK's results
/// already take: the request id, then either the result's fields or `error`
/// with the API's code field.
///
/// `error` is the host's reason and nothing more. Content composes the `errMsg`
/// (`<api>:fail <reason>`) because only content knows which API it was: one
/// host method can serve several -- a subpackage download is both
/// `loadSubpackage` and `preDownloadSubpackage` -- and a name chosen here would
/// be wrong for all but one.
fn completion_json(
    call: &Call,
    request_id: u32,
    outcome: HostServiceOutcome,
) -> Result<String, MigoResult> {
    let mut fields = Map::new();
    match outcome {
        HostServiceOutcome::Ok { payload_json } => {
            if !payload_json.is_empty() {
                let Ok(Value::Object(result)) = serde_json::from_str::<Value>(&payload_json) else {
                    return Err(MIGO_ERROR_INVALID_ARGUMENT);
                };
                if RESERVED_RESULT_FIELDS
                    .iter()
                    .any(|field| result.contains_key(*field))
                {
                    return Err(MIGO_ERROR_INVALID_ARGUMENT);
                }
                fields = result;
            }
            // A download that succeeded has a file to install; without one
            // the install would fail later, far from the host's mistake.
            if call.delivery == Delivery::SubpackageZip
                && !fields
                    .get("zipPath")
                    .and_then(Value::as_str)
                    .is_some_and(|path| !path.is_empty())
            {
                return Err(MIGO_ERROR_INVALID_ARGUMENT);
            }
            fields.insert("requestId".into(), request_id.into());
        }
        HostServiceOutcome::Fail {
            error_code,
            message,
        } => {
            // Never empty: the ABI refuses a failure without a reason, and an
            // empty `error` is what content's settlers read as success.
            debug_assert!(!message.is_empty());
            fields.insert("requestId".into(), request_id.into());
            fields.insert("error".into(), message.into());
            if let (Some(field), Some(code)) = (call.error_code_field, error_code) {
                fields.insert(field.into(), code.into());
            }
        }
    }
    Ok(Value::Object(fields).to_string())
}

/// The JSON a call's progress hook receives: the host's fields and the request
/// id.
fn progress_json(request_id: u32, payload_json: &str) -> Result<String, MigoResult> {
    let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(payload_json) else {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    };
    if RESERVED_RESULT_FIELDS
        .iter()
        .any(|field| fields.contains_key(*field))
    {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }
    fields.insert("requestId".into(), request_id.into());
    Ok(Value::Object(fields).to_string())
}

/// Hand `json` to content's `hook`, if `service` is one this host declared and
/// there is content to tell.
///
/// `delivery` is applied with the Host's id in hand, because what it keeps is
/// keyed by the Host the content runs in.
fn deliver(
    session: &MigoSession,
    service: u32,
    hook: &'static str,
    json: String,
    delivery: &Delivery,
) -> MigoResult {
    let Ok(state) = session.state.lock() else {
        return MIGO_ERROR_INTERNAL;
    };
    let declared = state
        .callbacks
        .as_ref()
        .is_some_and(|callbacks| callbacks.supplies_host_service(service));
    if !declared {
        return MIGO_ERROR_INVALID_ARGUMENT;
    }
    // No Host yet means no content has run, so none is waiting; one that has
    // gone with its Session cannot be reached here at all.
    let Some(host) = state.host.as_ref().map(crate::SessionEngine::id) else {
        return MIGO_OK;
    };
    drop(state);
    let json = match delivery {
        Delivery::Verbatim => json,
        Delivery::SubpackageZip => shared::services::intercept_download_result(host, &json),
        Delivery::HostFiles(files) => {
            shared::services::host_files::deliver_result(host, files, &json)
        }
    };
    if let Err(error) = migo_core::send_reliable_command_to_host(
        host,
        HostCommand::InvokeHostHook {
            hook,
            args_json: hook_args_one(json),
        },
    ) {
        tracing::warn!("host service result for {hook} not delivered: {error}");
    }
    MIGO_OK
}

/// # Safety
/// `session` must be a live session handle and `result` null or readable for
/// its announced size, with each string readable for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_complete_host_service_call(
    session: *mut MigoSession,
    call_id: u64,
    result: *const MigoHostServiceResult,
) -> MigoResult {
    guard("migo_session_complete_host_service_call", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Some((call, request_id)) = decode_call_id(call_id) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let outcome = match unsafe { MigoHostServiceResult::parse(result) } {
            Ok(outcome) => outcome,
            Err(error) => return error,
        };
        match completion_json(call, request_id, outcome) {
            Ok(json) => deliver(&session, call.service, call.hook, json, &call.delivery),
            Err(error) => error,
        }
    })
}

/// # Safety
/// `session` must be a live session handle and `payload_json_utf8` null with a
/// zero length or readable for `payload_length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_update_host_service_call(
    session: *mut MigoSession,
    call_id: u64,
    payload_json_utf8: *const std::os::raw::c_char,
    payload_length: u32,
) -> MigoResult {
    guard("migo_session_update_host_service_call", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Some((call, request_id)) = decode_call_id(call_id) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let Some(hook) = call.progress_hook else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let payload = match unsafe { copy_bounded(payload_json_utf8, payload_length) } {
            Ok(payload) => payload,
            Err(error) => return error,
        };
        match progress_json(request_id, &payload) {
            Ok(json) => deliver(&session, call.service, hook, json, &Delivery::Verbatim),
            Err(error) => error,
        }
    })
}

/// # Safety
/// `session` must be a live session handle and `payload_json_utf8` null with a
/// zero length or readable for `payload_length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_post_host_service_event(
    session: *mut MigoSession,
    service: u32,
    event: u32,
    payload_json_utf8: *const std::os::raw::c_char,
    payload_length: u32,
) -> MigoResult {
    guard("migo_session_post_host_service_event", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Some(hook) = event_hook(service, event) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let payload = match unsafe { copy_bounded(payload_json_utf8, payload_length) } {
            Ok(payload) => payload,
            Err(error) => return error,
        };
        // Checked here, where the host can be told, rather than in content,
        // which can only drop it.
        if !matches!(
            serde_json::from_str::<Value>(&payload),
            Ok(Value::Object(_))
        ) {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        deliver(&session, service, hook, payload, &Delivery::Verbatim)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        callbacks::{MigoHostCallbacks, MigoTaskFn},
        test_support::{callback_session_pin, with_session},
    };
    use migo_capi_abi::{
        MIGO_ABI_VERSION_CURRENT, VersionedHeader,
        host_services::{
            MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE, MIGO_HOST_SERVICE_STATUS_FAIL,
            MIGO_HOST_SERVICE_STATUS_OK, MIGO_HOST_SERVICES_KNOWN, MigoHostServiceCall,
        },
    };
    use std::{ffi::c_void, sync::Mutex};

    // ---- the contract ----

    const CONTRACT: &str = include_str!("../../../../contracts/runtime/host-services.json");

    fn abi_service(name: &str) -> u32 {
        match name {
            "ad" => MIGO_HOST_SERVICE_AD,
            "payment" => MIGO_HOST_SERVICE_PAYMENT,
            "auth" => MIGO_HOST_SERVICE_AUTH,
            "share" => MIGO_HOST_SERVICE_SHARE,
            "navigate" => MIGO_HOST_SERVICE_NAVIGATE,
            "subpackage" => MIGO_HOST_SERVICE_SUBPACKAGE,
            "permission" => MIGO_HOST_SERVICE_PERMISSION,
            "setting" => MIGO_HOST_SERVICE_SETTING,
            "interaction" => MIGO_HOST_SERVICE_INTERACTION,
            "clipboard" => MIGO_HOST_SERVICE_CLIPBOARD,
            "scan_code" => MIGO_HOST_SERVICE_SCAN_CODE,
            "location" => MIGO_HOST_SERVICE_LOCATION,
            "image" => MIGO_HOST_SERVICE_IMAGE,
            "motion" => MIGO_HOST_SERVICE_MOTION,
            "screen" => MIGO_HOST_SERVICE_SCREEN,
            "bluetooth" => MIGO_HOST_SERVICE_BLUETOOTH,
            "window" => MIGO_HOST_SERVICE_WINDOW,
            "ecosystem" => MIGO_HOST_SERVICE_ECOSYSTEM,
            other => panic!("the contract names a service the ABI has no constant for: {other}"),
        }
    }

    /// The Rust table, the ABI's constants and the contract say the same thing,
    /// in both directions: nothing the contract lists is missing here, and
    /// nothing here is missing from the contract.
    #[test]
    fn the_table_is_the_contract() {
        let contract: Value = serde_json::from_str(CONTRACT).expect("contract parses");
        let services = contract["services"].as_object().expect("services");
        let mut declared_bits = 0u64;
        let (mut calls_seen, mut commands_seen, mut events_seen) = (0, 0, 0);
        for (name, spec) in services {
            let service = abi_service(name);
            assert_eq!(spec["id"].as_u64(), Some(u64::from(service)), "{name}'s id");
            declared_bits |= 1 << service;
            for (method_name, method) in spec["methods"].as_object().expect("methods") {
                let id = method["id"].as_u64().expect("method id") as u32;
                match method["kind"].as_str() {
                    Some("call") => {
                        let call = CALLS
                            .iter()
                            .find(|call| call.service == service && call.method == id)
                            .unwrap_or_else(|| panic!("{name}.{method_name} is not in CALLS"));
                        assert!(
                            method.get("api").is_none(),
                            "{method_name}: content composes errMsg, so a call names no api"
                        );
                        assert_eq!(
                            Some(call.hook),
                            method["hook"].as_str(),
                            "{method_name} hook"
                        );
                        assert_eq!(
                            call.error_code_field,
                            method["error_code_field"].as_str(),
                            "{method_name} error_code_field"
                        );
                        assert_eq!(
                            call.progress_hook,
                            method.get("progress_hook").and_then(Value::as_str),
                            "{method_name} progress_hook"
                        );
                        let listed = |key: &str| -> Vec<String> {
                            method
                                .get(key)
                                .and_then(Value::as_array)
                                .map(|fields| {
                                    fields
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(str::to_owned)
                                        .collect()
                                })
                                .unwrap_or_default()
                        };
                        let (withheld, files): (Vec<String>, Vec<String>) = match call.delivery {
                            Delivery::Verbatim => (vec![], vec![]),
                            Delivery::SubpackageZip => (vec!["zipPath".into()], vec![]),
                            Delivery::HostFiles(files) => (vec![], files.describe()),
                        };
                        assert_eq!(listed("withheld"), withheld, "{method_name} withheld");
                        assert_eq!(listed("files"), files, "{method_name} files");
                        calls_seen += 1;
                    }
                    Some("command") => {
                        assert!(
                            COMMANDS.contains(&(service, id)),
                            "{name}.{method_name} is not in COMMANDS"
                        );
                        commands_seen += 1;
                    }
                    other => panic!("{name}.{method_name} has kind {other:?}"),
                }
            }
            for (event_name, event) in spec["events"].as_object().expect("events") {
                let id = event["id"].as_u64().expect("event id") as u32;
                assert_eq!(
                    event_hook(service, id),
                    event["hook"].as_str(),
                    "{name}.{event_name}"
                );
                events_seen += 1;
            }
        }
        assert_eq!(
            declared_bits, MIGO_HOST_SERVICES_KNOWN,
            "the known service set"
        );
        assert_eq!(
            calls_seen,
            CALLS.len(),
            "CALLS has an entry the contract does not"
        );
        assert_eq!(
            commands_seen,
            COMMANDS.len(),
            "COMMANDS has one the contract does not"
        );
        assert_eq!(
            events_seen,
            EVENTS.len(),
            "EVENTS has one the contract does not"
        );
    }

    // ---- ids and payloads ----

    #[test]
    fn a_call_id_carries_its_route_and_nothing_else_decodes() {
        for call in CALLS {
            let id = encode_call_id(call, 7);
            assert_ne!(id, 0, "a call is never mistaken for a command");
            assert_eq!(decode_call_id(id), Some((*call, 7)));
            assert_eq!(
                decode_call_id(encode_call_id(call, i32::MAX as u32)).map(|(_, r)| r),
                Some(i32::MAX as u32)
            );
        }
        assert_eq!(decode_call_id(0), None);
        let login = encode_call_id(&LOGIN, 1);
        assert_eq!(decode_call_id(login & !0xffff_ffff), None, "no request id");
        assert_eq!(
            decode_call_id(login | 0x8000_0000),
            None,
            "beyond content's ids"
        );
        // A command's route is not a call's: the ad service has no calls.
        assert_eq!(
            decode_call_id((u64::from(MIGO_HOST_SERVICE_AD) << SERVICE_SHIFT) | 1),
            None
        );
        assert_eq!(
            decode_call_id(login | (0x7f << METHOD_SHIFT)),
            None,
            "no such method"
        );
    }

    #[test]
    fn the_host_sees_the_request_without_content_s_id() {
        assert_eq!(
            split_request(r#"{"requestId":12,"timeout":500}"#),
            Some((12, r#"{"timeout":500}"#.to_owned()))
        );
        assert_eq!(split_request(r#"{"timeout":500}"#), None);
        assert_eq!(split_request(r#"{"requestId":0}"#), None);
        assert_eq!(split_request(r#"{"requestId":"12"}"#), None);
        assert_eq!(split_request(r#"{"requestId":2147483648}"#), None);
        assert_eq!(split_request("[12]"), None);
    }

    fn ok(payload: &str) -> HostServiceOutcome {
        HostServiceOutcome::Ok {
            payload_json: payload.to_owned(),
        }
    }

    fn fail(code: Option<i32>, message: &str) -> HostServiceOutcome {
        HostServiceOutcome::Fail {
            error_code: code,
            message: message.to_owned(),
        }
    }

    fn parsed(json: Result<String, MigoResult>) -> Value {
        serde_json::from_str(&json.expect("accepted")).expect("json")
    }

    #[test]
    fn a_success_is_the_host_s_fields_and_the_request_id() {
        assert_eq!(
            parsed(completion_json(&LOGIN, 3, ok(r#"{"code":"abc"}"#))),
            serde_json::json!({"requestId": 3, "code": "abc"})
        );
        assert_eq!(
            parsed(completion_json(&SHARE_APP_MESSAGE, 4, ok(""))),
            serde_json::json!({"requestId": 4})
        );
    }

    #[test]
    fn a_success_that_is_not_an_object_or_names_a_reserved_field_is_refused() {
        for payload in [
            "[1]",
            "\"x\"",
            "{",
            r#"{"error":"x"}"#,
            r#"{"requestId":9}"#,
            r#"{"errMsg":"x"}"#,
        ] {
            assert_eq!(
                completion_json(&LOGIN, 1, ok(payload)),
                Err(MIGO_ERROR_INVALID_ARGUMENT),
                "{payload}"
            );
        }
    }

    #[test]
    fn a_failure_is_the_host_s_reason_and_the_api_s_own_code_field() {
        assert_eq!(
            parsed(completion_json(&LOGIN, 5, fail(Some(1), "timeout"))),
            serde_json::json!({"requestId": 5, "error": "timeout", "errno": 1})
        );
        assert_eq!(
            parsed(completion_json(
                &REQUEST_MIDAS_PAYMENT,
                6,
                fail(Some(-2), "cancel")
            )),
            serde_json::json!({"requestId": 6, "error": "cancel", "errCode": -2})
        );
        // getUserInfo defines no code, so a host's is not invented a field for.
        assert_eq!(
            parsed(completion_json(&GET_USER_INFO, 7, fail(Some(9), "deny"))),
            serde_json::json!({"requestId": 7, "error": "deny"})
        );
        assert_eq!(
            parsed(completion_json(
                &NAVIGATE_TO_MINI_PROGRAM,
                8,
                fail(None, "no such app")
            )),
            serde_json::json!({"requestId": 8, "error": "no such app"})
        );
    }

    #[test]
    fn a_downloaded_subpackage_must_name_its_zip() {
        for payload in ["", "{}", r#"{"zipPath":""}"#, r#"{"zipPath":7}"#] {
            assert_eq!(
                completion_json(&SUBPACKAGE_DOWNLOAD, 3, ok(payload)),
                Err(MIGO_ERROR_INVALID_ARGUMENT),
                "{payload}"
            );
        }
        // Kept in the JSON here; `deliver` strips it with the Host's id in
        // hand, through the same interceptor the Android SDK's path uses.
        assert_eq!(
            parsed(completion_json(
                &SUBPACKAGE_DOWNLOAD,
                3,
                ok(r#"{"zipPath":"/tmp/stage1.zip"}"#)
            )),
            serde_json::json!({"requestId": 3, "zipPath": "/tmp/stage1.zip"})
        );
        let delivered = shared::services::intercept_download_result(
            i32::MAX - 7,
            r#"{"requestId":3,"zipPath":"/tmp/stage1.zip"}"#,
        );
        assert_eq!(
            serde_json::from_str::<Value>(&delivered).unwrap(),
            serde_json::json!({"requestId": 3})
        );
        assert_eq!(
            shared::services::take_downloaded_zip(i32::MAX - 7, 3),
            Some(std::path::PathBuf::from("/tmp/stage1.zip"))
        );
    }

    #[test]
    fn progress_is_the_host_s_fields_and_the_request_id() {
        assert_eq!(
            serde_json::from_str::<Value>(&progress_json(9, r#"{"progress":50}"#).unwrap())
                .unwrap(),
            serde_json::json!({"requestId": 9, "progress": 50})
        );
        for payload in ["", "[]", r#"{"requestId":1}"#, r#"{"error":"x"}"#] {
            assert_eq!(
                progress_json(9, payload),
                Err(MIGO_ERROR_INVALID_ARGUMENT),
                "{payload}"
            );
        }
    }

    // ---- through the dispatcher ----

    #[derive(Default)]
    struct Heard(Mutex<Vec<(u64, u32, u32, String)>>);

    unsafe extern "C" fn inline_dispatch(
        _dispatcher: *mut c_void,
        task: MigoTaskFn,
        context: *mut c_void,
    ) -> MigoResult {
        unsafe { task(context) };
        MIGO_OK
    }

    unsafe extern "C" fn rejecting_dispatch(
        _dispatcher: *mut c_void,
        _task: MigoTaskFn,
        _context: *mut c_void,
    ) -> MigoResult {
        migo_capi_abi::MIGO_ERROR_DISPATCH_REJECTED
    }

    unsafe extern "C" fn on_call(
        user: *mut c_void,
        _session: *mut c_void,
        call: *const MigoHostServiceCall,
    ) {
        let call = unsafe { &*call };
        assert_eq!(
            call.header.struct_size as usize,
            size_of::<MigoHostServiceCall>()
        );
        assert_eq!(call.header.abi_version, MIGO_ABI_VERSION_CURRENT);
        let bytes = unsafe {
            std::slice::from_raw_parts(
                call.payload_json_utf8.cast::<u8>(),
                call.payload_length as usize,
            )
        };
        let heard = unsafe { &*(user as *const Heard) };
        heard.0.lock().unwrap().push((
            call.call_id,
            call.service,
            call.method,
            std::str::from_utf8(bytes).unwrap().to_owned(),
        ));
    }

    fn services_with(
        heard: &Heard,
        dispatch: migo_capi_abi::callbacks::MigoDispatchFn,
    ) -> (Arc<MigoSession>, Arc<CapiHostServices>) {
        let mut raw = MigoHostCallbacks::empty();
        raw.user_data = heard as *const Heard as *mut c_void;
        raw.dispatch = Some(dispatch);
        raw.on_host_service_call = Some(on_call);
        raw.host_services = MIGO_HOST_SERVICES_KNOWN;
        let session = callback_session_pin();
        let notifier = Arc::new(Notifier::new(
            raw.validate().expect("valid").expect("a dispatcher"),
            Arc::downgrade(&session),
        ));
        let reports = Arc::clone(&session.host_reports);
        (session, CapiHostServices::new(notifier, reports))
    }

    #[test]
    fn calls_and_commands_reach_the_host_with_their_numbers() {
        let heard = Heard::default();
        let (_session, services) = services_with(&heard, inline_dispatch);
        AuthService::login(&*services, r#"{"requestId":41,"timeout":0}"#).unwrap();
        AdService::show_ad(&*services, r#"{"adId":3}"#).unwrap();
        NavigateService::navigate_back_mini_program(&*services, r#"{"extraData":{}}"#).unwrap();
        let heard = heard.0.lock().unwrap();
        assert_eq!(
            *heard,
            vec![
                (
                    encode_call_id(&LOGIN, 41),
                    MIGO_HOST_SERVICE_AUTH,
                    MIGO_AUTH_LOGIN,
                    r#"{"timeout":0}"#.to_owned()
                ),
                (
                    0,
                    MIGO_HOST_SERVICE_AD,
                    MIGO_AD_SHOW,
                    r#"{"adId":3}"#.to_owned()
                ),
                (
                    0,
                    MIGO_HOST_SERVICE_NAVIGATE,
                    MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM,
                    r#"{"extraData":{}}"#.to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_refusing_dispatcher_fails_content_s_call_rather_than_stranding_it() {
        let heard = Heard::default();
        let (_session, services) = services_with(&heard, rejecting_dispatch);
        let error = AuthService::login(&*services, r#"{"requestId":1}"#).unwrap_err();
        assert!(
            error.to_string().contains("host dispatcher refused"),
            "{error}"
        );
        assert!(AdService::create_ad(&*services, r#"{"adId":1}"#).is_err());
        assert!(heard.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_settings_page_is_a_call_named_by_content_s_request_id() {
        let heard = Heard::default();
        let (_session, services) = services_with(&heard, inline_dispatch);
        services.open_app_authorize_setting(17).unwrap();
        assert!(
            services.open_system_bluetooth_setting(0).is_err(),
            "no request id"
        );
        assert!(
            services.open_system_bluetooth_setting(-4).is_err(),
            "no request id"
        );
        let heard = heard.0.lock().unwrap();
        assert_eq!(
            *heard,
            vec![(
                encode_call_id(&OPEN_APP_AUTHORIZE_SETTING, 17),
                MIGO_HOST_SERVICE_SETTING,
                MIGO_SETTING_OPEN_APP_AUTHORIZE_SETTING,
                "{}".to_owned()
            )]
        );
    }

    #[test]
    fn permission_answers_from_the_host_s_standing_decisions() {
        let heard = Heard::default();
        let (session, services) = services_with(&heard, inline_dispatch);
        assert_eq!(
            PermissionService::scope_state(&*services, Scope::Camera),
            ScopeState::Unknown
        );
        // What `migo_session_set_scope_state` writes, read through the service.
        session
            .host_reports
            .record_scope_for_test(Scope::Camera, ScopeState::Granted);
        assert_eq!(
            PermissionService::scope_state(&*services, Scope::Camera),
            ScopeState::Granted
        );
        PermissionService::request_scope(
            &*services,
            r#"{"requestId":5,"scope":"scope.record","desc":""}"#,
        )
        .unwrap();
        assert_eq!(
            heard.0.lock().unwrap()[0],
            (
                encode_call_id(&REQUEST_SCOPE, 5),
                MIGO_HOST_SERVICE_PERMISSION,
                MIGO_PERMISSION_REQUEST_SCOPE,
                r#"{"desc":"","scope":"scope.record"}"#.to_owned()
            )
        );
    }

    #[test]
    fn a_malformed_request_is_an_engine_defect_not_a_host_call() {
        let heard = Heard::default();
        let (_session, services) = services_with(&heard, inline_dispatch);
        assert!(AuthService::login(&*services, r#"{"timeout":0}"#).is_err());
        assert!(heard.0.lock().unwrap().is_empty());
    }

    // ---- the entry points ----

    fn result(status: u32) -> MigoHostServiceResult {
        MigoHostServiceResult {
            header: VersionedHeader {
                struct_size: size_of::<MigoHostServiceResult>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            status,
            flags: 0,
            error_code: 0,
            message_length: 0,
            message_utf8: std::ptr::null(),
            payload_json_utf8: std::ptr::null(),
            payload_length: 0,
            reserved0: 0,
        }
    }

    unsafe extern "C" fn noop_call(_: *mut c_void, _: *mut c_void, _: *const MigoHostServiceCall) {}

    fn install(session: *mut MigoSession, services: u64) {
        let mut raw = MigoHostCallbacks::empty();
        raw.dispatch = Some(inline_dispatch);
        raw.on_host_service_call = Some(noop_call);
        raw.host_services = services;
        assert_eq!(
            unsafe { crate::migo_session_set_host_callbacks(session, &raw) },
            MIGO_OK
        );
    }

    #[test]
    fn completions_are_accepted_for_declared_services_and_refused_otherwise() {
        with_session("host-service-complete", |session| {
            install(session, 1 << MIGO_HOST_SERVICE_AUTH);
            let payload = br#"{"code":"abc"}"#;
            let mut success = result(MIGO_HOST_SERVICE_STATUS_OK);
            success.payload_json_utf8 = payload.as_ptr().cast();
            success.payload_length = payload.len() as u32;
            // No Host has started, so nobody is waiting: accepted, not an error.
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 1),
                        &success,
                    )
                },
                MIGO_OK
            );
            // Payment was never declared, so no call of it was ever made.
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&REQUEST_MIDAS_PAYMENT, 1),
                        &result(MIGO_HOST_SERVICE_STATUS_OK),
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            for bad_id in [0, 1, u64::MAX] {
                assert_eq!(
                    unsafe { migo_session_complete_host_service_call(session, bad_id, &success) },
                    MIGO_ERROR_INVALID_ARGUMENT
                );
            }
            let mut failure = result(MIGO_HOST_SERVICE_STATUS_FAIL);
            failure.flags = MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE;
            failure.error_code = 4;
            // A failure must say why.
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 2),
                        &failure,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            let reason = b"session expired";
            failure.message_utf8 = reason.as_ptr().cast();
            failure.message_length = reason.len() as u32;
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 2),
                        &failure,
                    )
                },
                MIGO_OK
            );
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 3),
                        std::ptr::null(),
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            let not_an_object = b"[]";
            let mut wrong = result(MIGO_HOST_SERVICE_STATUS_OK);
            wrong.payload_json_utf8 = not_an_object.as_ptr().cast();
            wrong.payload_length = 2;
            assert_eq!(
                unsafe {
                    migo_session_complete_host_service_call(
                        session,
                        encode_call_id(&LOGIN, 4),
                        &wrong,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn progress_reaches_only_a_call_that_reports_it() {
        with_session("host-service-progress", |session| {
            install(
                session,
                1 << MIGO_HOST_SERVICE_SUBPACKAGE | 1 << MIGO_HOST_SERVICE_AUTH,
            );
            let progress =
                br#"{"progress":50,"totalBytesWritten":1,"totalBytesExpectedToWrite":2}"#;
            let update = |call_id, bytes: &[u8]| unsafe {
                migo_session_update_host_service_call(
                    session,
                    call_id,
                    bytes.as_ptr().cast(),
                    bytes.len() as u32,
                )
            };
            assert_eq!(
                update(encode_call_id(&SUBPACKAGE_DOWNLOAD, 1), progress),
                MIGO_OK
            );
            assert_eq!(
                update(encode_call_id(&LOGIN, 1), progress),
                MIGO_ERROR_INVALID_ARGUMENT,
                "login reports no progress"
            );
            assert_eq!(
                update(encode_call_id(&SUBPACKAGE_DOWNLOAD, 1), b"[]"),
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(update(0, progress), MIGO_ERROR_INVALID_ARGUMENT);
        });
    }

    #[test]
    fn events_are_checked_against_the_service_and_its_event_set() {
        with_session("host-service-event", |session| {
            install(session, 1 << MIGO_HOST_SERVICE_AD);
            let event = br#"{"adId":1,"event":"load"}"#;
            let post = |service, id, bytes: &[u8]| unsafe {
                migo_session_post_host_service_event(
                    session,
                    service,
                    id,
                    bytes.as_ptr().cast(),
                    bytes.len() as u32,
                )
            };
            assert_eq!(
                post(MIGO_HOST_SERVICE_AD, MIGO_AD_EVENT_LIFECYCLE, event),
                MIGO_OK
            );
            assert_eq!(
                post(MIGO_HOST_SERVICE_AD, 1, event),
                MIGO_ERROR_INVALID_ARGUMENT,
                "an event the ad service does not define"
            );
            assert_eq!(
                post(MIGO_HOST_SERVICE_AUTH, MIGO_AD_EVENT_LIFECYCLE, event),
                MIGO_ERROR_INVALID_ARGUMENT,
                "a service with no such event"
            );
            assert_eq!(
                post(MIGO_HOST_SERVICE_AD, MIGO_AD_EVENT_LIFECYCLE, b"[1]"),
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(
                post(MIGO_HOST_SERVICE_AD, MIGO_AD_EVENT_LIFECYCLE, &[0xff]),
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(
                unsafe {
                    migo_session_post_host_service_event(
                        std::ptr::null_mut(),
                        MIGO_HOST_SERVICE_AD,
                        0,
                        event.as_ptr().cast(),
                        event.len() as u32,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn an_undeclared_service_s_event_is_refused() {
        with_session("host-service-undeclared", |session| {
            install(session, 1 << MIGO_HOST_SERVICE_AUTH);
            let event = br#"{"adId":1,"event":"close","isEnded":true}"#;
            assert_eq!(
                unsafe {
                    migo_session_post_host_service_event(
                        session,
                        MIGO_HOST_SERVICE_AD,
                        MIGO_AD_EVENT_LIFECYCLE,
                        event.as_ptr().cast(),
                        event.len() as u32,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }
}
