//! The host-service channel: capabilities a C host supplies to content.
//!
//! Ads, payment, login, sharing and mini-program navigation are the host app's
//! own integrations -- a vendor SDK, a store, a backend -- and never the
//! engine's. The channel carries them in three moves: the engine hands the host
//! a [`MigoHostServiceCall`] through its dispatcher, the host completes a call
//! with a [`MigoHostServiceResult`], and the host reports a service's own events
//! (an advert closing, say) with an event number and a JSON payload.
//!
//! The numbers are defined once, in `contracts/runtime/host-services.json`, and
//! the constants here and in `include/migo/host_services.h` are held to it.

use std::{
    mem::{offset_of, size_of},
    os::raw::c_char,
};

use crate::{
    AbiStruct, MIGO_ERROR_INVALID_ARGUMENT, MigoResult, VersionedHeader, copy_utf8,
    copy_utf8_with_length, copy_versioned,
    validate::{validate_flags, validate_reserved},
};

/// `void (*)(void *user_data, MigoSession *session, const MigoHostServiceCall *call)`.
pub type MigoOnHostServiceCallFn =
    unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const MigoHostServiceCall);

// ---- Services. A service's number is also its bit in `host_services`. ----

pub const MIGO_HOST_SERVICE_AD: u32 = 0;
pub const MIGO_HOST_SERVICE_PAYMENT: u32 = 1;
pub const MIGO_HOST_SERVICE_AUTH: u32 = 2;
pub const MIGO_HOST_SERVICE_SHARE: u32 = 3;
pub const MIGO_HOST_SERVICE_NAVIGATE: u32 = 4;
pub const MIGO_HOST_SERVICE_SUBPACKAGE: u32 = 5;
pub const MIGO_HOST_SERVICE_PERMISSION: u32 = 6;
pub const MIGO_HOST_SERVICE_SETTING: u32 = 7;
pub const MIGO_HOST_SERVICE_INTERACTION: u32 = 8;
pub const MIGO_HOST_SERVICE_CLIPBOARD: u32 = 9;
pub const MIGO_HOST_SERVICE_SCAN_CODE: u32 = 10;
pub const MIGO_HOST_SERVICE_LOCATION: u32 = 11;
pub const MIGO_HOST_SERVICE_IMAGE: u32 = 12;
pub const MIGO_HOST_SERVICE_MOTION: u32 = 13;
pub const MIGO_HOST_SERVICE_SCREEN: u32 = 14;
pub const MIGO_HOST_SERVICE_BLUETOOTH: u32 = 15;
pub const MIGO_HOST_SERVICE_WINDOW: u32 = 16;
pub const MIGO_HOST_SERVICE_ECOSYSTEM: u32 = 17;
pub const MIGO_HOST_SERVICE_UPDATE: u32 = 18;
pub const MIGO_HOST_SERVICE_CAMERA: u32 = 19;
pub const MIGO_HOST_SERVICE_RECORDER: u32 = 20;
pub const MIGO_HOST_SERVICE_VIDEO: u32 = 21;

/// Every service this library knows. A host declaring a bit outside it was
/// built against a newer header than the library it runs with.
pub const MIGO_HOST_SERVICES_KNOWN: u64 = (1 << MIGO_HOST_SERVICE_AD)
    | (1 << MIGO_HOST_SERVICE_PAYMENT)
    | (1 << MIGO_HOST_SERVICE_AUTH)
    | (1 << MIGO_HOST_SERVICE_SHARE)
    | (1 << MIGO_HOST_SERVICE_NAVIGATE)
    | (1 << MIGO_HOST_SERVICE_SUBPACKAGE)
    | (1 << MIGO_HOST_SERVICE_PERMISSION)
    | (1 << MIGO_HOST_SERVICE_SETTING)
    | (1 << MIGO_HOST_SERVICE_INTERACTION)
    | (1 << MIGO_HOST_SERVICE_CLIPBOARD)
    | (1 << MIGO_HOST_SERVICE_SCAN_CODE)
    | (1 << MIGO_HOST_SERVICE_LOCATION)
    | (1 << MIGO_HOST_SERVICE_IMAGE)
    | (1 << MIGO_HOST_SERVICE_MOTION)
    | (1 << MIGO_HOST_SERVICE_SCREEN)
    | (1 << MIGO_HOST_SERVICE_BLUETOOTH)
    | (1 << MIGO_HOST_SERVICE_WINDOW)
    | (1 << MIGO_HOST_SERVICE_ECOSYSTEM)
    | (1 << MIGO_HOST_SERVICE_UPDATE)
    | (1 << MIGO_HOST_SERVICE_CAMERA)
    | (1 << MIGO_HOST_SERVICE_RECORDER)
    | (1 << MIGO_HOST_SERVICE_VIDEO);

// ---- Methods, numbered per service. ----

pub const MIGO_AD_CREATE: u32 = 0;
pub const MIGO_AD_LOAD: u32 = 1;
pub const MIGO_AD_SHOW: u32 = 2;
pub const MIGO_AD_HIDE: u32 = 3;
pub const MIGO_AD_UPDATE_STYLE: u32 = 4;
pub const MIGO_AD_DESTROY: u32 = 5;

pub const MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT: u32 = 0;
pub const MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM: u32 = 1;

pub const MIGO_AUTH_LOGIN: u32 = 0;
pub const MIGO_AUTH_CHECK_SESSION: u32 = 1;
pub const MIGO_AUTH_GET_USER_INFO: u32 = 2;
pub const MIGO_AUTH_GET_PHONE_NUMBER: u32 = 3;

pub const MIGO_SHARE_SHARE_APP_MESSAGE: u32 = 0;
pub const MIGO_SHARE_SHARE_MESSAGE_TO_FRIEND: u32 = 1;
pub const MIGO_SHARE_SHOW_SHARE_IMAGE_MENU: u32 = 2;
pub const MIGO_SHARE_SET_SHARE_MENU: u32 = 3;
pub const MIGO_SHARE_MENU_SHARE_REPLY: u32 = 4;
pub const MIGO_SHARE_EVENT_MENU_SHARE: u32 = 0;

pub const MIGO_NAVIGATE_NAVIGATE_TO_MINI_PROGRAM: u32 = 0;
pub const MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM: u32 = 1;
pub const MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION: u32 = 2;

pub const MIGO_SUBPACKAGE_DOWNLOAD: u32 = 0;

pub const MIGO_PERMISSION_REQUEST_SCOPE: u32 = 0;

pub const MIGO_SETTING_OPEN_SETTING: u32 = 0;
pub const MIGO_SETTING_OPEN_SYSTEM_BLUETOOTH_SETTING: u32 = 1;
pub const MIGO_SETTING_OPEN_APP_AUTHORIZE_SETTING: u32 = 2;

pub const MIGO_INTERACTION_SHOW_TOAST: u32 = 0;
pub const MIGO_INTERACTION_HIDE_TOAST: u32 = 1;
pub const MIGO_INTERACTION_SHOW_MODAL: u32 = 2;
pub const MIGO_INTERACTION_SHOW_LOADING: u32 = 3;
pub const MIGO_INTERACTION_HIDE_LOADING: u32 = 4;
pub const MIGO_INTERACTION_SHOW_ACTION_SHEET: u32 = 5;

pub const MIGO_CLIPBOARD_SET_CLIPBOARD_DATA: u32 = 0;
pub const MIGO_CLIPBOARD_GET_CLIPBOARD_DATA: u32 = 1;

pub const MIGO_SCAN_CODE_SCAN_CODE: u32 = 0;

pub const MIGO_LOCATION_GET_LOCATION: u32 = 0;
pub const MIGO_LOCATION_GET_FUZZY_LOCATION: u32 = 1;

pub const MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM: u32 = 0;
pub const MIGO_IMAGE_PREVIEW_IMAGE: u32 = 1;
pub const MIGO_IMAGE_PREVIEW_MEDIA: u32 = 2;
pub const MIGO_IMAGE_COMPRESS_IMAGE: u32 = 3;
pub const MIGO_IMAGE_CHOOSE_IMAGE: u32 = 4;
pub const MIGO_IMAGE_CHOOSE_MESSAGE_FILE: u32 = 5;
pub const MIGO_IMAGE_CHOOSE_MEDIA: u32 = 6;
pub const MIGO_IMAGE_SAVE_FILE_TO_DISK: u32 = 7;

pub const MIGO_MOTION_START_ACCELEROMETER: u32 = 0;
pub const MIGO_MOTION_STOP_ACCELEROMETER: u32 = 1;
pub const MIGO_MOTION_START_GYROSCOPE: u32 = 2;
pub const MIGO_MOTION_STOP_GYROSCOPE: u32 = 3;
pub const MIGO_MOTION_START_COMPASS: u32 = 4;
pub const MIGO_MOTION_STOP_COMPASS: u32 = 5;
pub const MIGO_MOTION_START_DEVICE_MOTION: u32 = 6;
pub const MIGO_MOTION_STOP_DEVICE_MOTION: u32 = 7;

pub const MIGO_SCREEN_GET_BRIGHTNESS: u32 = 0;
pub const MIGO_SCREEN_SET_BRIGHTNESS: u32 = 1;
pub const MIGO_SCREEN_SET_DEVICE_ORIENTATION: u32 = 2;
pub const MIGO_SCREEN_START_CAPTURE_OBSERVER: u32 = 3;
pub const MIGO_SCREEN_STOP_CAPTURE_OBSERVER: u32 = 4;
pub const MIGO_SCREEN_GET_RECORDING_STATE: u32 = 5;
pub const MIGO_SCREEN_START_RECORDING_OBSERVER: u32 = 6;
pub const MIGO_SCREEN_STOP_RECORDING_OBSERVER: u32 = 7;
pub const MIGO_SCREEN_SET_VISUAL_EFFECT_ON_CAPTURE: u32 = 8;
pub const MIGO_SCREEN_SET_ENABLE_DEBUG: u32 = 9;
pub const MIGO_SCREEN_EVENT_USER_CAPTURE_SCREEN: u32 = 0;
pub const MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE: u32 = 1;
pub const MIGO_SCREEN_EVENT_RECORDING_STATE_CHANGE: u32 = 2;

pub const MIGO_BLUETOOTH_OPEN_ADAPTER: u32 = 0;
pub const MIGO_BLUETOOTH_CLOSE_ADAPTER: u32 = 1;
pub const MIGO_BLUETOOTH_GET_ADAPTER_STATE: u32 = 2;
pub const MIGO_BLUETOOTH_START_DEVICES_DISCOVERY: u32 = 3;
pub const MIGO_BLUETOOTH_STOP_DEVICES_DISCOVERY: u32 = 4;
pub const MIGO_BLUETOOTH_GET_DEVICES: u32 = 5;
pub const MIGO_BLUETOOTH_GET_CONNECTED_DEVICES: u32 = 6;
pub const MIGO_BLUETOOTH_MAKE_PAIR: u32 = 7;
pub const MIGO_BLUETOOTH_IS_DEVICE_PAIRED: u32 = 8;
pub const MIGO_BLUETOOTH_START_BEACON_DISCOVERY: u32 = 9;
pub const MIGO_BLUETOOTH_STOP_BEACON_DISCOVERY: u32 = 10;
pub const MIGO_BLUETOOTH_GET_BEACONS: u32 = 11;
pub const MIGO_BLUETOOTH_CREATE_BLE_CONNECTION: u32 = 12;
pub const MIGO_BLUETOOTH_CLOSE_BLE_CONNECTION: u32 = 13;
pub const MIGO_BLUETOOTH_GET_BLE_DEVICE_SERVICES: u32 = 14;
pub const MIGO_BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS: u32 = 15;
pub const MIGO_BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE: u32 = 16;
pub const MIGO_BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE: u32 = 17;
pub const MIGO_BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE: u32 = 18;
pub const MIGO_BLUETOOTH_GET_BLE_DEVICE_RSSI: u32 = 19;
pub const MIGO_BLUETOOTH_SET_BLE_MTU: u32 = 20;
pub const MIGO_BLUETOOTH_GET_BLE_MTU: u32 = 21;
pub const MIGO_BLUETOOTH_EVENT_ADAPTER_STATE_CHANGE: u32 = 0;
pub const MIGO_BLUETOOTH_EVENT_DEVICE_FOUND: u32 = 1;
pub const MIGO_BLUETOOTH_EVENT_BLE_CONNECTION_STATE_CHANGE: u32 = 2;
pub const MIGO_BLUETOOTH_EVENT_BLE_MTU_CHANGE: u32 = 3;
pub const MIGO_BLUETOOTH_EVENT_BEACON_UPDATE: u32 = 4;
pub const MIGO_BLUETOOTH_EVENT_BEACON_SERVICE_CHANGE: u32 = 5;

pub const MIGO_WINDOW_SET_WINDOW_SIZE: u32 = 0;
pub const MIGO_WINDOW_SET_CURSOR: u32 = 1;
pub const MIGO_WINDOW_REQUEST_POINTER_LOCK: u32 = 2;
pub const MIGO_WINDOW_EXIT_POINTER_LOCK: u32 = 3;
pub const MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE: u32 = 0;
pub const MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE: u32 = 1;

pub const MIGO_ECOSYSTEM_CALL: u32 = 0;
pub const MIGO_ECOSYSTEM_REPLY: u32 = 1;
pub const MIGO_ECOSYSTEM_EVENT_EVENT: u32 = 0;

pub const MIGO_UPDATE_APPLY_UPDATE: u32 = 0;
pub const MIGO_UPDATE_EVENT_STATE: u32 = 0;

pub const MIGO_CAMERA_CREATE: u32 = 0;
pub const MIGO_CAMERA_DESTROY: u32 = 1;
pub const MIGO_CAMERA_SET: u32 = 2;
pub const MIGO_CAMERA_TAKE_PHOTO: u32 = 3;
pub const MIGO_CAMERA_START_RECORD: u32 = 4;
pub const MIGO_CAMERA_STOP_RECORD: u32 = 5;
pub const MIGO_CAMERA_SET_ZOOM: u32 = 6;
pub const MIGO_CAMERA_LISTEN_FRAME_CHANGE: u32 = 7;
pub const MIGO_CAMERA_CLOSE_FRAME_CHANGE: u32 = 8;
pub const MIGO_CAMERA_EVENT_STOP: u32 = 0;
pub const MIGO_CAMERA_EVENT_AUTH_CANCEL: u32 = 1;

pub const MIGO_RECORDER_START: u32 = 0;
pub const MIGO_RECORDER_PAUSE: u32 = 1;
pub const MIGO_RECORDER_RESUME: u32 = 2;
pub const MIGO_RECORDER_STOP: u32 = 3;
pub const MIGO_RECORDER_EVENT_START: u32 = 0;
pub const MIGO_RECORDER_EVENT_PAUSE: u32 = 1;
pub const MIGO_RECORDER_EVENT_RESUME: u32 = 2;
pub const MIGO_RECORDER_EVENT_STOP: u32 = 3;
pub const MIGO_RECORDER_EVENT_ERROR: u32 = 4;
pub const MIGO_RECORDER_EVENT_INTERRUPTION_BEGIN: u32 = 5;
pub const MIGO_RECORDER_EVENT_INTERRUPTION_END: u32 = 6;

pub const MIGO_VIDEO_CREATE: u32 = 0;
pub const MIGO_VIDEO_DESTROY: u32 = 1;
pub const MIGO_VIDEO_SET: u32 = 2;
pub const MIGO_VIDEO_PLAY: u32 = 3;
pub const MIGO_VIDEO_PAUSE: u32 = 4;
pub const MIGO_VIDEO_STOP: u32 = 5;
pub const MIGO_VIDEO_SEEK: u32 = 6;
pub const MIGO_VIDEO_REQUEST_FULL_SCREEN: u32 = 7;
pub const MIGO_VIDEO_EXIT_FULL_SCREEN: u32 = 8;
pub const MIGO_VIDEO_EVENT_PLAY: u32 = 0;
pub const MIGO_VIDEO_EVENT_PAUSE: u32 = 1;
pub const MIGO_VIDEO_EVENT_ENDED: u32 = 2;
pub const MIGO_VIDEO_EVENT_TIME_UPDATE: u32 = 3;
pub const MIGO_VIDEO_EVENT_ERROR: u32 = 4;
pub const MIGO_VIDEO_EVENT_WAITING: u32 = 5;
pub const MIGO_VIDEO_EVENT_PROGRESS: u32 = 6;

// ---- Events, numbered per service. ----

pub const MIGO_AD_EVENT_LIFECYCLE: u32 = 0;

// ---- Scopes: what content may be granted, in `migo.getSetting()`'s order. ----

pub const MIGO_SCOPE_USER_INFO: u32 = 0;
pub const MIGO_SCOPE_USER_LOCATION: u32 = 1;
pub const MIGO_SCOPE_USER_LOCATION_BACKGROUND: u32 = 2;
pub const MIGO_SCOPE_ADDRESS: u32 = 3;
pub const MIGO_SCOPE_INVOICE_TITLE: u32 = 4;
pub const MIGO_SCOPE_INVOICE: u32 = 5;
pub const MIGO_SCOPE_WERUN: u32 = 6;
pub const MIGO_SCOPE_RECORD: u32 = 7;
pub const MIGO_SCOPE_WRITE_PHOTOS_ALBUM: u32 = 8;
pub const MIGO_SCOPE_CAMERA: u32 = 9;
pub const MIGO_SCOPE_BLUETOOTH: u32 = 10;
pub const MIGO_SCOPE_ADD_PHONE_CONTACT: u32 = 11;
pub const MIGO_SCOPE_ADD_PHONE_CALENDAR: u32 = 12;
pub const MIGO_SCOPE_FRIEND_INTERACTION: u32 = 13;
pub const MIGO_SCOPE_GAME_CLUB_DATA: u32 = 14;
/// One past the last scope.
pub const MIGO_SCOPE_COUNT: u32 = 15;

/// A scope nobody has decided yet, which content may still ask about.
pub const MIGO_SCOPE_STATE_UNKNOWN: u32 = 0;
pub const MIGO_SCOPE_STATE_GRANTED: u32 = 1;
pub const MIGO_SCOPE_STATE_DENIED: u32 = 2;

// ---- The system switches `migo.getSystemSetting()` reports. ----

pub const MIGO_SYSTEM_SETTING_FLAG_NONE: u32 = 0;
pub const MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED: u32 = 1 << 0;
pub const MIGO_SYSTEM_SETTING_FLAG_LOCATION_ENABLED: u32 = 1 << 1;
pub const MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED: u32 = 1 << 2;
pub const MIGO_SYSTEM_SETTING_FLAGS_KNOWN: u32 = MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED
    | MIGO_SYSTEM_SETTING_FLAG_LOCATION_ENABLED
    | MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED;

// ---- The app's OS-level authorisations `migo.getAppAuthorizeSetting()` reports. ----

pub const MIGO_AUTHORIZATION_NOT_DETERMINED: u8 = 0;
pub const MIGO_AUTHORIZATION_AUTHORIZED: u8 = 1;
pub const MIGO_AUTHORIZATION_DENIED: u8 = 2;

/// What the operating system has granted the host app itself, which is what
/// `migo.getAppAuthorizeSetting()` describes -- not content's scopes, which are
/// the host's own decisions.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigoAppAuthorizeSetting {
    pub header: VersionedHeader,
    pub album: u8,
    pub bluetooth: u8,
    pub camera: u8,
    pub location: u8,
    pub microphone: u8,
    pub notification: u8,
    pub notification_alert: u8,
    pub notification_badge: u8,
    pub notification_sound: u8,
    pub phone_calendar: u8,
    /// 1 when location is granted only approximately, else 0.
    pub location_reduced_accuracy: u8,
    pub reserved0: u8,
}

// SAFETY: integers only; v1 requires the complete record.
unsafe impl AbiStruct for MigoAppAuthorizeSetting {}

// ---- What the host app says about itself: `migo.getAppBaseInfo()`, `migo.getDeviceBenchmarkInfo()`. ----

/// The host's debugging aids are on: `getAppBaseInfo().enableDebug`.
pub const MIGO_HOST_APP_INFO_FLAG_DEBUG: u32 = 1 << 0;
const MIGO_HOST_APP_INFO_FLAGS_KNOWN: u32 = MIGO_HOST_APP_INFO_FLAG_DEBUG;

pub const MIGO_DEVICE_MODEL_LEVEL_UNKNOWN: u32 = 0;
pub const MIGO_DEVICE_MODEL_LEVEL_HIGH: u32 = 1;
pub const MIGO_DEVICE_MODEL_LEVEL_MEDIUM: u32 = 2;
pub const MIGO_DEVICE_MODEL_LEVEL_LOW: u32 = 3;

/// The longest host app id a report may carry, in bytes.
pub const MIGO_HOST_APP_ID_MAX_BYTES: usize = 256;
/// The longest language tag a report may carry, in bytes.
pub const MIGO_LANGUAGE_TAG_MAX_BYTES: usize = 64;
/// The largest text scale a report may carry: past every platform's largest
/// accessibility size, and short of a font size no layout could hold.
pub const MIGO_FONT_SCALE_MAX: f32 = 8.0;

/// What the host app says about itself, which is what `migo.getAppBaseInfo()`
/// and `migo.getDeviceBenchmarkInfo()` describe beyond what the engine knows.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoHostAppInfo {
    pub header: VersionedHeader,
    /// The host app's own id -- bundle identifier, package name -- or null.
    pub app_id_utf8: *const c_char,
    /// The host app's language as a BCP 47 tag, or null for the system's.
    pub language_utf8: *const c_char,
    /// The host app's text scale, 1 standard; 0 for not reported.
    pub font_scale: f32,
    /// The host's score for the device: -1 unknown, 1 and up higher faster.
    pub benchmark_level: i32,
    /// `MIGO_DEVICE_MODEL_LEVEL_*`.
    pub model_level: u32,
    /// `MIGO_HOST_APP_INFO_FLAG_*`.
    pub flags: u32,
}

// SAFETY: scalars and pointers only, all zero-valid; v1 requires the complete
// record.
unsafe impl AbiStruct for MigoHostAppInfo {}

/// A host app report, copied and checked: everything the engine keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValidatedHostAppInfo {
    pub app_id: Option<String>,
    pub language: Option<String>,
    pub font_scale: Option<f32>,
    pub benchmark_level: i32,
    pub model_level: u32,
    pub debug: bool,
}

impl MigoHostAppInfo {
    /// Copy and validate a caller-owned report. The language's form is the
    /// caller's to check further: this only bounds it.
    ///
    /// # Safety
    /// `info` must be null or readable for its announced byte count, and each
    /// non-null string NUL-terminated and readable for this call.
    pub unsafe fn parse(info: *const Self) -> Result<ValidatedHostAppInfo, MigoResult> {
        // SAFETY: forwarded from this function's contract.
        let raw = unsafe { copy_versioned::<Self>(info.cast::<VersionedHeader>()) }?;
        if raw.flags & !MIGO_HOST_APP_INFO_FLAGS_KNOWN != 0
            || raw.model_level > MIGO_DEVICE_MODEL_LEVEL_LOW
            || raw.benchmark_level < -2
            || !(raw.font_scale == 0.0
                || (raw.font_scale.is_finite()
                    && raw.font_scale > 0.0
                    && raw.font_scale <= MIGO_FONT_SCALE_MAX))
        {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        // SAFETY: the copied pointers keep their call-duration validity.
        let app_id = unsafe { optional_utf8(raw.app_id_utf8, MIGO_HOST_APP_ID_MAX_BYTES) }?;
        let language = unsafe { optional_utf8(raw.language_utf8, MIGO_LANGUAGE_TAG_MAX_BYTES) }?;
        Ok(ValidatedHostAppInfo {
            app_id,
            language,
            font_scale: (raw.font_scale != 0.0).then_some(raw.font_scale),
            benchmark_level: raw.benchmark_level,
            model_level: raw.model_level,
            debug: raw.flags & MIGO_HOST_APP_INFO_FLAG_DEBUG != 0,
        })
    }
}

/// A NUL-terminated UTF-8 string no longer than `max` bytes; null and empty are
/// both absent.
///
/// # Safety
/// `text` must be null or NUL-terminated and readable.
unsafe fn optional_utf8(text: *const c_char, max: usize) -> Result<Option<String>, MigoResult> {
    if text.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded from this function's contract.
    let text = unsafe { copy_utf8(text) }?;
    if text.len() > max {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }
    Ok(Some(text).filter(|text| !text.is_empty()))
}

pub const MIGO_SENSOR_ACCELEROMETER: u32 = 0;
pub const MIGO_SENSOR_GYROSCOPE: u32 = 1;
pub const MIGO_SENSOR_DEVICE_MOTION: u32 = 2;
pub const MIGO_SENSOR_COMPASS: u32 = 3;

pub const MIGO_COMPASS_ACCURACY_UNKNOWN: u32 = 0;
pub const MIGO_COMPASS_ACCURACY_HIGH: u32 = 1;
pub const MIGO_COMPASS_ACCURACY_MEDIUM: u32 = 2;
pub const MIGO_COMPASS_ACCURACY_LOW: u32 = 3;
pub const MIGO_COMPASS_ACCURACY_NO_CONTACT: u32 = 4;
pub const MIGO_COMPASS_ACCURACY_UNRELIABLE: u32 = 5;

/// One reading from a sensor content started through the motion service.
///
/// Typed rather than a JSON event: readings come up to fifty times a second per
/// sensor, and each would otherwise be formatted by the host and parsed twice.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MigoSensorSample {
    pub header: VersionedHeader,
    /// A `MIGO_SENSOR_*`.
    pub kind: u32,
    /// A `MIGO_COMPASS_ACCURACY_*` for a compass reading, else 0.
    pub compass_accuracy: u32,
    /// The reading; see each `MIGO_SENSOR_*` for what the three values are.
    pub values: [f64; 3],
}

// SAFETY: integers and floats only; v1 requires the complete record.
unsafe impl AbiStruct for MigoSensorSample {}

/// The longest device, service or characteristic id a value may name.
pub const MIGO_BLE_ID_MAX_BYTES: u32 = 256;
/// The longest attribute value BLE carries.
pub const MIGO_BLE_VALUE_MAX_BYTES: u32 = 512;

/// One characteristic value a connected peripheral sent.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoBleCharacteristicValue {
    pub header: VersionedHeader,
    pub device_id_utf8: *const c_char,
    pub service_id_utf8: *const c_char,
    pub characteristic_id_utf8: *const c_char,
    pub value: *const u8,
    pub device_id_length: u32,
    pub service_id_length: u32,
    pub characteristic_id_length: u32,
    pub value_length: u32,
}

// SAFETY: pointers and integers, all zero-valid; v1 requires the complete record.
unsafe impl AbiStruct for MigoBleCharacteristicValue {}

/// A validated [`MigoBleCharacteristicValue`], borrowing the caller's memory for
/// the call: the value path copies once, into a pooled slot, and nowhere else.
#[derive(Debug, PartialEq, Eq)]
pub struct BleCharacteristicValueView<'a> {
    pub device_id: &'a str,
    pub service_id: &'a str,
    pub characteristic_id: &'a str,
    pub value: &'a [u8],
}

impl MigoBleCharacteristicValue {
    /// Validate a caller-owned record and borrow what it names.
    ///
    /// # Safety
    /// `record` must be null or readable for its announced byte count, and each
    /// range it announces readable for the call.
    pub unsafe fn parse<'a>(
        record: *const Self,
    ) -> Result<BleCharacteristicValueView<'a>, MigoResult> {
        // SAFETY: forwarded from this function's contract.
        let raw = unsafe { copy_versioned::<Self>(record.cast::<VersionedHeader>()) }?;
        // SAFETY: each range is the caller's to make readable.
        unsafe {
            Ok(BleCharacteristicValueView {
                device_id: borrow_id(raw.device_id_utf8, raw.device_id_length)?,
                service_id: borrow_id(raw.service_id_utf8, raw.service_id_length)?,
                characteristic_id: borrow_id(
                    raw.characteristic_id_utf8,
                    raw.characteristic_id_length,
                )?,
                value: borrow_value(raw.value, raw.value_length)?,
            })
        }
    }
}

/// # Safety
/// `text` must be readable for `length` bytes when it is not null.
unsafe fn borrow_id<'a>(text: *const c_char, length: u32) -> Result<&'a str, MigoResult> {
    if text.is_null() || length == 0 || length > MIGO_BLE_ID_MAX_BYTES {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }
    // SAFETY: the caller guarantees `length` readable bytes at `text`.
    let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length as usize) };
    std::str::from_utf8(bytes).map_err(|_| MIGO_ERROR_INVALID_ARGUMENT)
}

/// # Safety
/// `value` must be readable for `length` bytes when it is not null.
unsafe fn borrow_value<'a>(value: *const u8, length: u32) -> Result<&'a [u8], MigoResult> {
    if length > MIGO_BLE_VALUE_MAX_BYTES || (value.is_null() && length != 0) {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }
    if length == 0 {
        return Ok(&[]);
    }
    // SAFETY: the caller guarantees `length` readable bytes at `value`.
    Ok(unsafe { std::slice::from_raw_parts(value, length as usize) })
}

pub const MIGO_PIXEL_FORMAT_RGBA8: u32 = 0;
pub const MIGO_PIXEL_FORMAT_BGRA8: u32 = 1;
/// The widest or tallest camera frame a host may hand over.
const CAMERA_FRAME_MAX_DIMENSION: u32 = 8192;

/// One frame of a camera content listens to.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoCameraFrame {
    pub header: VersionedHeader,
    pub camera_id: u32,
    pub width: u32,
    pub height: u32,
    pub row_stride: u32,
    pub format: u32,
    pub reserved0: u32,
    pub pixels: *const u8,
}

// SAFETY: integers and a pointer, all zero-valid; v1 requires the complete record.
unsafe impl AbiStruct for MigoCameraFrame {}

/// A validated [`MigoCameraFrame`], borrowing the caller's pixels for the call.
#[derive(Debug, PartialEq, Eq)]
pub struct CameraFrameView<'a> {
    pub camera_id: u32,
    pub width: u32,
    pub height: u32,
    pub row_stride: usize,
    /// True for `MIGO_PIXEL_FORMAT_BGRA8`, false for `RGBA8`.
    pub bgra: bool,
    /// Every byte the rows reach: `row_stride` per row but the last, which is
    /// `width * 4`.
    pub pixels: &'a [u8],
}

impl MigoCameraFrame {
    /// Validate a caller-owned frame and borrow its pixels.
    ///
    /// # Safety
    /// `record` must be null or readable for its announced byte count, and the
    /// rows it announces readable for the call.
    pub unsafe fn parse<'a>(record: *const Self) -> Result<CameraFrameView<'a>, MigoResult> {
        // SAFETY: forwarded from this function's contract.
        let raw = unsafe { copy_versioned::<Self>(record.cast::<VersionedHeader>()) }?;
        let bgra = match raw.format {
            MIGO_PIXEL_FORMAT_RGBA8 => false,
            MIGO_PIXEL_FORMAT_BGRA8 => true,
            _ => return Err(MIGO_ERROR_INVALID_ARGUMENT),
        };
        if raw.width == 0
            || raw.height == 0
            || raw.width > CAMERA_FRAME_MAX_DIMENSION
            || raw.height > CAMERA_FRAME_MAX_DIMENSION
            || raw.pixels.is_null()
            || raw.reserved0 != 0
        {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        let row = raw.width as usize * 4;
        let row_stride = raw.row_stride as usize;
        if row_stride < row {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        let reach = (raw.height as usize - 1)
            .checked_mul(row_stride)
            .and_then(|start| start.checked_add(row))
            .ok_or(MIGO_ERROR_INVALID_ARGUMENT)?;
        Ok(CameraFrameView {
            camera_id: raw.camera_id,
            width: raw.width,
            height: raw.height,
            row_stride,
            bgra,
            // SAFETY: the caller guarantees the rows it announced are readable.
            pixels: unsafe { std::slice::from_raw_parts(raw.pixels, reach) },
        })
    }
}

/// The largest recording frame a host may hand over: content asks for frames of
/// at most 1024 KB.
const RECORDER_FRAME_MAX_BYTES: u32 = 1 << 20;

/// One frame of a recording whose options asked for frames.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoRecorderFrame {
    pub header: VersionedHeader,
    pub recording_id: u32,
    pub is_last_frame: u32,
    pub data: *const u8,
    pub length: u32,
    pub reserved0: u32,
}

// SAFETY: integers and a pointer, all zero-valid; v1 requires the complete record.
unsafe impl AbiStruct for MigoRecorderFrame {}

/// A validated [`MigoRecorderFrame`], borrowing the caller's bytes for the call.
#[derive(Debug, PartialEq, Eq)]
pub struct RecorderFrameView<'a> {
    pub recording_id: u32,
    pub is_last_frame: bool,
    pub data: &'a [u8],
}

impl MigoRecorderFrame {
    /// Validate a caller-owned frame and borrow its bytes.
    ///
    /// # Safety
    /// `record` must be null or readable for its announced byte count, and
    /// `data` readable for `length` bytes for the call.
    pub unsafe fn parse<'a>(record: *const Self) -> Result<RecorderFrameView<'a>, MigoResult> {
        // SAFETY: forwarded from this function's contract.
        let raw = unsafe { copy_versioned::<Self>(record.cast::<VersionedHeader>()) }?;
        let is_last_frame = match raw.is_last_frame {
            0 => false,
            1 => true,
            _ => return Err(MIGO_ERROR_INVALID_ARGUMENT),
        };
        if raw.recording_id == 0
            || raw.length > RECORDER_FRAME_MAX_BYTES
            || (raw.data.is_null() && raw.length != 0)
            || raw.reserved0 != 0
        {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        let data = if raw.length == 0 {
            &[][..]
        } else {
            // SAFETY: the caller guarantees `length` readable bytes at `data`.
            unsafe { std::slice::from_raw_parts(raw.data, raw.length as usize) }
        };
        Ok(RecorderFrameView {
            recording_id: raw.recording_id,
            is_last_frame,
            data,
        })
    }
}

impl MigoSensorSample {
    /// Copy and validate a caller-owned reading.
    ///
    /// # Safety
    /// `sample` must be null or readable for its announced byte count.
    pub unsafe fn parse(sample: *const Self) -> Result<Self, MigoResult> {
        // SAFETY: forwarded from this function's contract.
        let raw = unsafe { copy_versioned::<Self>(sample.cast::<VersionedHeader>()) }?;
        let accuracy_ok = if raw.kind == MIGO_SENSOR_COMPASS {
            raw.compass_accuracy <= MIGO_COMPASS_ACCURACY_UNRELIABLE
                && raw.values[1] == 0.0
                && raw.values[2] == 0.0
        } else {
            raw.compass_accuracy == 0
        };
        if raw.kind > MIGO_SENSOR_COMPASS
            || !accuracy_ok
            || raw.values.iter().any(|value| !value.is_finite())
        {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        Ok(raw)
    }
}

impl MigoAppAuthorizeSetting {
    /// Copy and validate a caller-owned report.
    ///
    /// # Safety
    /// `setting` must be null or readable for its announced byte count.
    pub unsafe fn parse(setting: *const Self) -> Result<Self, MigoResult> {
        // SAFETY: forwarded from this function's contract.
        let raw = unsafe { copy_versioned::<Self>(setting.cast::<VersionedHeader>()) }?;
        validate_reserved(u64::from(raw.reserved0))?;
        let states = [
            raw.album,
            raw.bluetooth,
            raw.camera,
            raw.location,
            raw.microphone,
            raw.notification,
            raw.notification_alert,
            raw.notification_badge,
            raw.notification_sound,
            raw.phone_calendar,
        ];
        if states
            .iter()
            .any(|state| *state > MIGO_AUTHORIZATION_DENIED)
            || raw.location_reduced_accuracy > 1
        {
            return Err(MIGO_ERROR_INVALID_ARGUMENT);
        }
        Ok(raw)
    }
}

// ---- Completion. ----

pub const MIGO_HOST_SERVICE_STATUS_OK: u32 = 0;
pub const MIGO_HOST_SERVICE_STATUS_FAIL: u32 = 1;

/// `error_code` carries a value. Without it a failure has no numeric code.
pub const MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE: u32 = 1 << 0;
const MIGO_HOST_SERVICE_RESULT_FLAGS_KNOWN: u32 = MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE;

/// The largest payload or message either direction carries, in bytes.
///
/// Generous for what crosses here -- a user profile, an advert's size, a
/// share's title -- and small enough that a host passing the wrong length
/// cannot make the engine copy an arbitrary amount of its memory.
pub const MIGO_HOST_SERVICE_PAYLOAD_MAX_BYTES: u32 = 1 << 20;

/// A request content made of the host. Written by the library, borrowed by the
/// host for the duration of the callback.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoHostServiceCall {
    pub header: VersionedHeader,
    /// Nonzero for a call the host must complete exactly once; zero for a
    /// command, which has nothing to complete. Opaque to the host.
    pub call_id: u64,
    pub service: u32,
    pub method: u32,
    /// The request's options, a JSON object, length-delimited UTF-8.
    pub payload_json_utf8: *const c_char,
    pub payload_length: u32,
    pub reserved0: u32,
}

/// The host's completion of one call. Read by the library during
/// `migo_session_complete_host_service_call` and not retained.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoHostServiceResult {
    pub header: VersionedHeader,
    pub status: u32,
    pub flags: u32,
    pub error_code: i32,
    /// A failure's reason, without the API's name: content sees
    /// `<api>:fail <message>`. Required on failure, empty on success.
    pub message_length: u32,
    pub message_utf8: *const c_char,
    /// A success's result, a JSON object, or empty for one with no fields.
    /// Must be empty on failure.
    pub payload_json_utf8: *const c_char,
    pub payload_length: u32,
    pub reserved0: u32,
}

// SAFETY: integers and nullable pointers only; v1 requires the complete record.
unsafe impl AbiStruct for MigoHostServiceResult {}

/// A completion the library has copied out of host memory and checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostServiceOutcome {
    /// The JSON text as the host wrote it; empty when it gave none. Whether it
    /// is an object is the engine's check, which owns a JSON parser.
    Ok { payload_json: String },
    Fail {
        error_code: Option<i32>,
        message: String,
    },
}

impl MigoHostServiceResult {
    /// Copy and validate a caller-owned completion.
    ///
    /// # Safety
    /// `result` must be null or readable for its announced byte count, and each
    /// string pointer readable for its length.
    pub unsafe fn parse(result: *const Self) -> Result<HostServiceOutcome, MigoResult> {
        // SAFETY: forwarded from this function's contract.
        let raw = unsafe { copy_versioned::<Self>(result.cast::<VersionedHeader>()) }?;
        validate_reserved(u64::from(raw.reserved0))?;
        validate_flags(
            u64::from(raw.flags),
            u64::from(MIGO_HOST_SERVICE_RESULT_FLAGS_KNOWN),
        )?;
        match raw.status {
            MIGO_HOST_SERVICE_STATUS_OK => {
                // A success with a reason or a code is a host that has confused
                // the two statuses; reading either would guess which it meant.
                if raw.flags != 0 || raw.message_length != 0 {
                    return Err(MIGO_ERROR_INVALID_ARGUMENT);
                }
                // SAFETY: forwarded from this function's contract.
                let payload_json =
                    unsafe { copy_bounded(raw.payload_json_utf8, raw.payload_length) }?;
                Ok(HostServiceOutcome::Ok { payload_json })
            }
            MIGO_HOST_SERVICE_STATUS_FAIL => {
                // A failure must say why: it is the only text content gets, and
                // an empty reason is what content's settlers read as success.
                if raw.payload_length != 0 || raw.message_length == 0 {
                    return Err(MIGO_ERROR_INVALID_ARGUMENT);
                }
                // SAFETY: forwarded from this function's contract.
                let message = unsafe { copy_bounded(raw.message_utf8, raw.message_length) }?;
                let error_code = (raw.flags & MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE != 0)
                    .then_some(raw.error_code);
                Ok(HostServiceOutcome::Fail {
                    error_code,
                    message,
                })
            }
            _ => Err(MIGO_ERROR_INVALID_ARGUMENT),
        }
    }
}

/// Copy a length-delimited UTF-8 payload no longer than the channel's limit.
///
/// # Safety
/// `value` must be null with `length == 0`, or readable for `length` bytes.
pub unsafe fn copy_bounded(value: *const c_char, length: u32) -> Result<String, MigoResult> {
    if length > MIGO_HOST_SERVICE_PAYLOAD_MAX_BYTES {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }
    // SAFETY: forwarded from this function's contract.
    unsafe { copy_utf8_with_length(value, length) }
}

const _: () = assert!(size_of::<MigoAppAuthorizeSetting>() == 20);
const _: () = assert!(offset_of!(MigoHostAppInfo, app_id_utf8) == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoHostAppInfo>() == 40);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostAppInfo, font_scale) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostAppInfo, flags) == 36);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<MigoHostAppInfo>() == 32);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostAppInfo, font_scale) == 16);
const _: () = assert!(size_of::<MigoSensorSample>() == 40);
const _: () = assert!(offset_of!(MigoSensorSample, kind) == 8);
const _: () = assert!(offset_of!(MigoSensorSample, compass_accuracy) == 12);
const _: () = assert!(offset_of!(MigoSensorSample, values) == 16);
const _: () = assert!(offset_of!(MigoBleCharacteristicValue, device_id_utf8) == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoBleCharacteristicValue>() == 56);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoBleCharacteristicValue, device_id_length) == 40);
const _: () = assert!(offset_of!(MigoCameraFrame, camera_id) == 8);
const _: () = assert!(offset_of!(MigoCameraFrame, format) == 24);
const _: () = assert!(offset_of!(MigoCameraFrame, pixels) == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoCameraFrame>() == 40);
const _: () = assert!(offset_of!(MigoRecorderFrame, recording_id) == 8);
const _: () = assert!(offset_of!(MigoRecorderFrame, is_last_frame) == 12);
const _: () = assert!(offset_of!(MigoRecorderFrame, data) == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoRecorderFrame, length) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoRecorderFrame>() == 32);
const _: () = assert!(offset_of!(MigoAppAuthorizeSetting, album) == 8);
const _: () = assert!(offset_of!(MigoAppAuthorizeSetting, location_reduced_accuracy) == 18);
const _: () = assert!(offset_of!(MigoHostServiceCall, header) == 0);
const _: () = assert!(offset_of!(MigoHostServiceCall, call_id) == 8);
const _: () = assert!(offset_of!(MigoHostServiceCall, service) == 16);
const _: () = assert!(offset_of!(MigoHostServiceCall, method) == 20);
const _: () = assert!(offset_of!(MigoHostServiceCall, payload_json_utf8) == 24);
const _: () = assert!(offset_of!(MigoHostServiceResult, header) == 0);
const _: () = assert!(offset_of!(MigoHostServiceResult, status) == 8);
const _: () = assert!(offset_of!(MigoHostServiceResult, flags) == 12);
const _: () = assert!(offset_of!(MigoHostServiceResult, error_code) == 16);
const _: () = assert!(offset_of!(MigoHostServiceResult, message_length) == 20);
const _: () = assert!(offset_of!(MigoHostServiceResult, message_utf8) == 24);

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoHostServiceCall>() == 40);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostServiceCall, payload_length) == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoHostServiceResult>() == 48);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostServiceResult, payload_json_utf8) == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoHostServiceResult, payload_length) == 40);

// The call record ends on a 4-byte field after a u64, so its ILP32 size is the
// target's u64 alignment's business: 40 where u64 aligns to 8 (ARMv7, MSVC
// x86), 36 where it aligns to 4 (i386 System V). It is library-written, so the
// host only ever reads the fields, never the tail padding.
#[cfg(target_pointer_width = "32")]
const _: () =
    assert!(size_of::<MigoHostServiceCall>() == if align_of::<u64>() == 8 { 40 } else { 36 });
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostServiceCall, payload_length) == 28);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<MigoHostServiceResult>() == 40);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostServiceResult, payload_json_utf8) == 28);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(offset_of!(MigoHostServiceResult, payload_length) == 32);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_UNSUPPORTED_ABI};

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

    fn parse(value: &MigoHostServiceResult) -> Result<HostServiceOutcome, MigoResult> {
        unsafe { MigoHostServiceResult::parse(value) }
    }

    fn frame(
        pixels: &[u8],
        width: u32,
        height: u32,
        row_stride: u32,
        format: u32,
    ) -> MigoCameraFrame {
        MigoCameraFrame {
            header: VersionedHeader {
                struct_size: size_of::<MigoCameraFrame>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            camera_id: 7,
            width,
            height,
            row_stride,
            format,
            reserved0: 0,
            pixels: pixels.as_ptr(),
        }
    }

    #[test]
    fn a_camera_frame_borrows_the_rows_it_announces() {
        // 2 x 2, rows 12 bytes apart: the last row ends 20 bytes in.
        let pixels = [0u8; 24];
        let view =
            unsafe { MigoCameraFrame::parse(&frame(&pixels, 2, 2, 12, MIGO_PIXEL_FORMAT_BGRA8)) }
                .expect("a whole frame");
        assert_eq!(
            (
                view.camera_id,
                view.width,
                view.height,
                view.row_stride,
                view.bgra
            ),
            (7, 2, 2, 12, true)
        );
        assert_eq!(
            view.pixels.len(),
            20,
            "the last row is width * 4, not a stride"
        );
    }

    #[test]
    fn a_camera_frame_out_of_shape_is_refused() {
        let pixels = [0u8; 64];
        let refused = |record: MigoCameraFrame| unsafe { MigoCameraFrame::parse(&record) }.err();
        assert_eq!(
            refused(frame(&pixels, 2, 2, 8, 2)),
            Some(MIGO_ERROR_INVALID_ARGUMENT),
            "a format"
        );
        assert_eq!(
            refused(frame(&pixels, 2, 2, 7, 0)),
            Some(MIGO_ERROR_INVALID_ARGUMENT),
            "a stride"
        );
        assert_eq!(
            refused(frame(&pixels, 0, 2, 8, 0)),
            Some(MIGO_ERROR_INVALID_ARGUMENT),
            "no width"
        );
        assert_eq!(
            refused(frame(&pixels, 8193, 1, 32772, 0)),
            Some(MIGO_ERROR_INVALID_ARGUMENT),
            "too wide"
        );
        let mut unnamed = frame(&pixels, 2, 2, 8, 0);
        unnamed.pixels = std::ptr::null();
        assert_eq!(
            refused(unnamed),
            Some(MIGO_ERROR_INVALID_ARGUMENT),
            "no pixels"
        );
        assert_eq!(
            unsafe { MigoCameraFrame::parse(std::ptr::null()) }.err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    fn recorder_frame(data: &[u8], recording_id: u32, is_last_frame: u32) -> MigoRecorderFrame {
        MigoRecorderFrame {
            header: VersionedHeader {
                struct_size: size_of::<MigoRecorderFrame>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            recording_id,
            is_last_frame,
            data: if data.is_empty() {
                std::ptr::null()
            } else {
                data.as_ptr()
            },
            length: data.len() as u32,
            reserved0: 0,
        }
    }

    #[test]
    fn a_recorder_frame_borrows_its_bytes() {
        let bytes = [1u8, 2, 3, 4];
        let view = unsafe { MigoRecorderFrame::parse(&recorder_frame(&bytes, 9, 0)) }.unwrap();
        assert_eq!(
            view,
            RecorderFrameView {
                recording_id: 9,
                is_last_frame: false,
                data: &bytes,
            }
        );
        // The last frame of a recording may be empty.
        let last = unsafe { MigoRecorderFrame::parse(&recorder_frame(&[], 9, 1)) }.unwrap();
        assert!(last.is_last_frame && last.data.is_empty());
    }

    #[test]
    fn a_malformed_recorder_frame_is_refused() {
        let bytes = [0u8; 4];
        let refused =
            |record: MigoRecorderFrame| unsafe { MigoRecorderFrame::parse(&record) }.err();
        let invalid = Some(MIGO_ERROR_INVALID_ARGUMENT);
        assert_eq!(
            refused(recorder_frame(&bytes, 0, 0)),
            invalid,
            "no recording"
        );
        assert_eq!(
            refused(recorder_frame(&bytes, 9, 2)),
            invalid,
            "a flag that is not one"
        );
        let mut huge = recorder_frame(&bytes, 9, 0);
        huge.length = RECORDER_FRAME_MAX_BYTES + 1;
        assert_eq!(refused(huge), invalid, "over 1 MiB");
        let mut unnamed = recorder_frame(&bytes, 9, 0);
        unnamed.data = std::ptr::null();
        assert_eq!(refused(unnamed), invalid, "bytes announced, none given");
        let mut reserved = recorder_frame(&bytes, 9, 0);
        reserved.reserved0 = 1;
        assert_eq!(refused(reserved), invalid, "reserved");
        assert_eq!(
            unsafe { MigoRecorderFrame::parse(std::ptr::null()) }.err(),
            invalid
        );
    }

    #[test]
    fn a_success_carries_its_payload() {
        let payload = br#"{"code":"abc"}"#;
        let mut ok = result(MIGO_HOST_SERVICE_STATUS_OK);
        ok.payload_json_utf8 = payload.as_ptr().cast();
        ok.payload_length = payload.len() as u32;
        assert_eq!(
            parse(&ok),
            Ok(HostServiceOutcome::Ok {
                payload_json: r#"{"code":"abc"}"#.into()
            })
        );
        assert_eq!(
            parse(&result(MIGO_HOST_SERVICE_STATUS_OK)),
            Ok(HostServiceOutcome::Ok {
                payload_json: String::new()
            })
        );
    }

    #[test]
    fn a_failure_carries_its_reason_and_only_a_flagged_code() {
        let reason = b"cancel";
        let mut fail = result(MIGO_HOST_SERVICE_STATUS_FAIL);
        fail.message_utf8 = reason.as_ptr().cast();
        fail.message_length = reason.len() as u32;
        fail.error_code = -2;
        assert_eq!(
            parse(&fail),
            Ok(HostServiceOutcome::Fail {
                error_code: None,
                message: "cancel".into()
            }),
            "a code the host did not flag is not one it meant to send"
        );
        fail.flags = MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE;
        assert_eq!(
            parse(&fail),
            Ok(HostServiceOutcome::Fail {
                error_code: Some(-2),
                message: "cancel".into()
            })
        );
    }

    #[test]
    fn mixed_statuses_and_unknown_bits_are_refused() {
        let text = b"x";
        let mut ok_with_reason = result(MIGO_HOST_SERVICE_STATUS_OK);
        ok_with_reason.message_utf8 = text.as_ptr().cast();
        ok_with_reason.message_length = 1;
        assert_eq!(parse(&ok_with_reason), Err(MIGO_ERROR_INVALID_ARGUMENT));

        let mut ok_with_code = result(MIGO_HOST_SERVICE_STATUS_OK);
        ok_with_code.flags = MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE;
        assert_eq!(parse(&ok_with_code), Err(MIGO_ERROR_INVALID_ARGUMENT));

        let mut fail_with_payload = result(MIGO_HOST_SERVICE_STATUS_FAIL);
        fail_with_payload.message_utf8 = text.as_ptr().cast();
        fail_with_payload.message_length = 1;
        fail_with_payload.payload_json_utf8 = text.as_ptr().cast();
        fail_with_payload.payload_length = 1;
        assert_eq!(parse(&fail_with_payload), Err(MIGO_ERROR_INVALID_ARGUMENT));

        assert_eq!(
            parse(&result(MIGO_HOST_SERVICE_STATUS_FAIL)),
            Err(MIGO_ERROR_INVALID_ARGUMENT),
            "a failure without a reason"
        );

        assert_eq!(parse(&result(2)), Err(MIGO_ERROR_INVALID_ARGUMENT));

        let mut unknown_flag = result(MIGO_HOST_SERVICE_STATUS_FAIL);
        unknown_flag.message_utf8 = text.as_ptr().cast();
        unknown_flag.message_length = 1;
        unknown_flag.flags = 1 << 1;
        assert_eq!(parse(&unknown_flag), Err(MIGO_ERROR_INVALID_ARGUMENT));

        let mut reserved = result(MIGO_HOST_SERVICE_STATUS_OK);
        reserved.reserved0 = 1;
        assert_eq!(parse(&reserved), Err(MIGO_ERROR_INVALID_ARGUMENT));
    }

    #[test]
    fn strings_must_be_utf8_bounded_and_present_when_long() {
        let mut missing = result(MIGO_HOST_SERVICE_STATUS_OK);
        missing.payload_length = 3;
        assert_eq!(parse(&missing), Err(MIGO_ERROR_INVALID_ARGUMENT));

        let bad = [0xffu8, 0xfe];
        let mut not_utf8 = result(MIGO_HOST_SERVICE_STATUS_FAIL);
        not_utf8.message_utf8 = bad.as_ptr().cast();
        not_utf8.message_length = 2;
        assert_eq!(parse(&not_utf8), Err(MIGO_ERROR_INVALID_ARGUMENT));

        // Refused on the length alone, before any byte is read.
        let mut oversized = result(MIGO_HOST_SERVICE_STATUS_OK);
        oversized.payload_json_utf8 = bad.as_ptr().cast();
        oversized.payload_length = MIGO_HOST_SERVICE_PAYLOAD_MAX_BYTES + 1;
        assert_eq!(parse(&oversized), Err(MIGO_ERROR_INVALID_ARGUMENT));
    }

    fn host_app_info() -> MigoHostAppInfo {
        MigoHostAppInfo {
            header: VersionedHeader {
                struct_size: size_of::<MigoHostAppInfo>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            app_id_utf8: std::ptr::null(),
            language_utf8: std::ptr::null(),
            font_scale: 0.0,
            benchmark_level: -1,
            model_level: MIGO_DEVICE_MODEL_LEVEL_UNKNOWN,
            flags: 0,
        }
    }

    #[test]
    fn a_host_app_report_that_says_nothing_is_all_absent() {
        assert_eq!(
            unsafe { MigoHostAppInfo::parse(&host_app_info()) },
            Ok(ValidatedHostAppInfo {
                benchmark_level: -1,
                ..ValidatedHostAppInfo::default()
            })
        );
    }

    #[test]
    fn a_host_app_report_is_copied() {
        let id = std::ffi::CString::new("com.example.host").unwrap();
        let language = std::ffi::CString::new("zh-Hans-CN").unwrap();
        let mut report = host_app_info();
        report.app_id_utf8 = id.as_ptr();
        report.language_utf8 = language.as_ptr();
        report.font_scale = 1.25;
        report.benchmark_level = 30;
        report.model_level = MIGO_DEVICE_MODEL_LEVEL_HIGH;
        report.flags = MIGO_HOST_APP_INFO_FLAG_DEBUG;
        assert_eq!(
            unsafe { MigoHostAppInfo::parse(&report) },
            Ok(ValidatedHostAppInfo {
                app_id: Some("com.example.host".into()),
                language: Some("zh-Hans-CN".into()),
                font_scale: Some(1.25),
                benchmark_level: 30,
                model_level: MIGO_DEVICE_MODEL_LEVEL_HIGH,
                debug: true,
            })
        );
        // Empty strings are absent, like null ones.
        let empty = std::ffi::CString::new("").unwrap();
        report.app_id_utf8 = empty.as_ptr();
        report.language_utf8 = empty.as_ptr();
        let parsed = unsafe { MigoHostAppInfo::parse(&report) }.unwrap();
        assert_eq!((parsed.app_id, parsed.language), (None, None));
    }

    #[test]
    fn a_host_app_report_holds_only_values_in_range() {
        let long_id = std::ffi::CString::new("a".repeat(MIGO_HOST_APP_ID_MAX_BYTES + 1)).unwrap();
        let long_tag = std::ffi::CString::new("a".repeat(MIGO_LANGUAGE_TAG_MAX_BYTES + 1)).unwrap();
        let cases: [(&str, fn(&mut MigoHostAppInfo)); 7] = [
            ("an unknown flag", |r| r.flags = 1 << 1),
            ("a model level past low", |r| {
                r.model_level = MIGO_DEVICE_MODEL_LEVEL_LOW + 1
            }),
            ("a benchmark level below -2", |r| r.benchmark_level = -3),
            ("a negative text scale", |r| r.font_scale = -1.0),
            ("a text scale that is not a number", |r| {
                r.font_scale = f32::NAN
            }),
            ("an infinite text scale", |r| r.font_scale = f32::INFINITY),
            ("a text scale past the largest", |r| {
                r.font_scale = MIGO_FONT_SCALE_MAX * 2.0
            }),
        ];
        for (what, inject) in cases {
            let mut report = host_app_info();
            inject(&mut report);
            assert_eq!(
                unsafe { MigoHostAppInfo::parse(&report) },
                Err(MIGO_ERROR_INVALID_ARGUMENT),
                "{what}"
            );
        }
        let mut report = host_app_info();
        report.app_id_utf8 = long_id.as_ptr();
        assert_eq!(
            unsafe { MigoHostAppInfo::parse(&report) },
            Err(MIGO_ERROR_INVALID_ARGUMENT)
        );
        let mut report = host_app_info();
        report.language_utf8 = long_tag.as_ptr();
        assert_eq!(
            unsafe { MigoHostAppInfo::parse(&report) },
            Err(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    fn authorization() -> MigoAppAuthorizeSetting {
        MigoAppAuthorizeSetting {
            header: VersionedHeader {
                struct_size: size_of::<MigoAppAuthorizeSetting>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            album: 0,
            bluetooth: 0,
            camera: 0,
            location: 0,
            microphone: 0,
            notification: 0,
            notification_alert: 0,
            notification_badge: 0,
            notification_sound: 0,
            phone_calendar: 0,
            location_reduced_accuracy: 0,
            reserved0: 0,
        }
    }

    #[test]
    fn an_app_authorization_report_holds_only_known_states() {
        let mut report = authorization();
        report.camera = MIGO_AUTHORIZATION_AUTHORIZED;
        report.location = MIGO_AUTHORIZATION_DENIED;
        report.location_reduced_accuracy = 1;
        assert_eq!(
            unsafe { MigoAppAuthorizeSetting::parse(&report) },
            Ok(report)
        );

        let mut unknown_state = authorization();
        unknown_state.microphone = 3;
        assert_eq!(
            unsafe { MigoAppAuthorizeSetting::parse(&unknown_state) },
            Err(MIGO_ERROR_INVALID_ARGUMENT)
        );
        let mut not_a_bool = authorization();
        not_a_bool.location_reduced_accuracy = 2;
        assert_eq!(
            unsafe { MigoAppAuthorizeSetting::parse(&not_a_bool) },
            Err(MIGO_ERROR_INVALID_ARGUMENT)
        );
        let mut reserved = authorization();
        reserved.reserved0 = 1;
        assert_eq!(
            unsafe { MigoAppAuthorizeSetting::parse(&reserved) },
            Err(MIGO_ERROR_INVALID_ARGUMENT)
        );
        assert_eq!(
            unsafe { MigoAppAuthorizeSetting::parse(std::ptr::null()) },
            Err(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    #[test]
    fn the_record_is_versioned_like_every_other() {
        assert_eq!(
            unsafe { MigoHostServiceResult::parse(std::ptr::null()) },
            Err(MIGO_ERROR_INVALID_ARGUMENT)
        );
        let mut newer = result(MIGO_HOST_SERVICE_STATUS_OK);
        newer.header.struct_size += 8;
        assert_eq!(parse(&newer), Err(MIGO_ERROR_UNSUPPORTED_ABI));
        let mut truncated = result(MIGO_HOST_SERVICE_STATUS_OK);
        truncated.header.struct_size = 16;
        assert_eq!(parse(&truncated), Err(MIGO_ERROR_INVALID_ARGUMENT));
    }
}
