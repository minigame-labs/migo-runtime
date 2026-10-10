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
    AbiStruct, MIGO_ERROR_INVALID_ARGUMENT, MigoResult, VersionedHeader, copy_utf8_with_length,
    copy_versioned,
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
    | (1 << MIGO_HOST_SERVICE_SCREEN);

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
pub const MIGO_SCREEN_EVENT_USER_CAPTURE_SCREEN: u32 = 0;
pub const MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE: u32 = 1;
pub const MIGO_SCREEN_EVENT_RECORDING_STATE_CHANGE: u32 = 2;

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
const _: () = assert!(size_of::<MigoSensorSample>() == 40);
const _: () = assert!(offset_of!(MigoSensorSample, kind) == 8);
const _: () = assert!(offset_of!(MigoSensorSample, compass_accuracy) == 12);
const _: () = assert!(offset_of!(MigoSensorSample, values) == 16);
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
