//! Content's requests of the host app on the external session -- a toast, the
//! clipboard, a location, a Bluetooth connection: the host-service channel's
//! methods (`contracts/runtime/host-services.json`), each of which the embedded
//! runtime answers with one op, a scope check and one call of a device service.
//!
//! The producer sends each as a command under its op's own number, carrying
//! what the op would have been given. This host makes the same check, from the
//! same table ([`shared::services::op_scope`]), and the same call on the same
//! device services the embedded op reaches; what the host app answers reaches
//! content through the method's result hook, as it does embedded.
//!
//! The embedded op refuses by throwing, which fails content's call at once. A
//! command cannot throw, so a refusal here -- a scope not granted, no such
//! service, a request the service turns down -- is delivered to that same
//! hook, carrying the request's id and the reason the op would have thrown:
//! content's call fails with the words it fails with embedded instead of
//! waiting out its timeout. A method no hook answers is refused in the log, as
//! an embedded fire-and-forget op's failure is.

use std::sync::Arc;

use frame_wire::value::OwnedValue;
use futures::future::BoxFuture;
use migo_services::ServiceError;
use migo_services::host_paths::{self, Sandbox};
use shared::js_escape::hook_args_one;
use shared::services::{DeviceServices, ScopeState, op_scope};
use tracing::warn;

use super::host_events::ServiceEventSink;
use super::service_args::{exactly, i32_of, not_a, string};
use super::service_ops::id;

/// Why a channel op did not reach the host app, and which call it fails.
struct Refusal {
    request_id: Option<u64>,
    message: String,
}

impl From<ServiceError> for Refusal {
    /// A request that is not the shape its op takes: it names no call to fail.
    fn from(error: ServiceError) -> Self {
        Self {
            request_id: None,
            message: error.message,
        }
    }
}

/// Make one op's call: its scope checked, then its device service asked.
fn forward<S: ?Sized>(
    device: Option<&dyn DeviceServices>,
    op: &str,
    request_id: impl FnOnce() -> Option<u64>,
    service: impl FnOnce(&dyn DeviceServices) -> Option<Arc<S>>,
    call: impl FnOnce(&S) -> Result<(), shared::services::ServiceError>,
) -> Result<(), Refusal> {
    let outcome = match scope_refusal(device, op) {
        Some(message) => Err(message),
        None => match device.and_then(service) {
            Some(service) => call(&service).map_err(|error| error.message),
            None => Err("not supported".to_owned()),
        },
    };
    outcome.map_err(|message| Refusal {
        request_id: request_id(),
        message,
    })
}

/// The embedded ops' scope check: a gated op reaches the host only with its
/// scope granted, and is refused in the words they throw.
pub(crate) fn scope_refusal(device: Option<&dyn DeviceServices>, op: &str) -> Option<String> {
    let scope = op_scope(op)?;
    let state = device
        .and_then(|device| device.permission())
        .map_or(ScopeState::Unknown, |permission| {
            permission.scope_state(scope)
        });
    (state != ScopeState::Granted)
        .then(|| format!("auth deny: {} is not granted", scope.as_minigame_str()))
}

/// The id a JSON request's call waits under.
fn request_id_in(json: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("requestId")?
        .as_u64()
}

/// The table below, one row at a time: what each form of request is given and
/// how it is handed to its method.
macro_rules! forward_row {
    (none, $device:ident, $op:ident, $args:ident, $accessor:ident . $method:ident) => {{
        let [] = exactly(id::$op, $args)?;
        forward(
            $device,
            stringify!($op),
            || None,
            |device| device.$accessor(),
            |service| service.$method(),
        )
    }};
    (json, $device:ident, $op:ident, $args:ident, $accessor:ident . $method:ident) => {{
        let [request] = exactly(id::$op, $args)?;
        let request = string(id::$op, 0, request)?;
        forward(
            $device,
            stringify!($op),
            || request_id_in(&request),
            |device| device.$accessor(),
            |service| service.$method(&request),
        )
    }};
    (id, $device:ident, $op:ident, $args:ident, $accessor:ident . $method:ident) => {{
        let [request_id] = exactly(id::$op, $args)?;
        let request_id = i32_of(id::$op, 0, request_id)?;
        forward(
            $device,
            stringify!($op),
            || u64::try_from(request_id).ok(),
            |device| device.$accessor(),
            |service| service.$method(request_id),
        )
    }};
}

/// A row's hook, if it names one.
macro_rules! hook_of {
    () => {
        None
    };
    ($hook:literal) => {
        Some($hook)
    };
}

macro_rules! channel {
    ($($op:ident($form:ident) => $accessor:ident . $method:ident $(, $hook:literal)?;)*) => {
        /// Whether `op` is one of the channel's.
        pub(crate) fn is_channel(op: u32) -> bool {
            matches!(op, $(id::$op)|*)
        }

        fn call(
            device: Option<&dyn DeviceServices>,
            op: u32,
            args: Vec<OwnedValue>,
        ) -> Result<(), Refusal> {
            match op {
                $(id::$op => forward_row!($form, device, $op, args, $accessor.$method),)*
                _ => Err(Refusal { request_id: None, message: "not a channel op".to_owned() }),
            }
        }

        /// The hook a method's result reaches content through, for a method
        /// that has one.
        fn hook(op: u32) -> Option<&'static str> {
            match op {
                $(id::$op => hook_of!($($hook)?),)*
                _ => None,
            }
        }

        /// Every op of the channel's, by name.
        #[cfg(test)]
        const OPS: &[&str] = &[$(stringify!($op)),*];
    };
}

// What the embedded op of each method takes and calls (runtime-v8's `device`,
// `media`, `share`, `system`, `ui` and `update` modules), and the hook its
// result reaches content through.
channel! {
    op_ad_create(json) => ad.create_ad;
    op_ad_destroy(json) => ad.destroy_ad;
    op_ad_hide(json) => ad.hide_ad;
    op_ad_load(json) => ad.load_ad;
    op_ad_show(json) => ad.show_ad;
    op_ad_update_style(json) => ad.update_ad_style;
    op_apply_update(none) => update.apply_update;
    op_authorize(json) => permission.request_scope, "_internalOnAuthorizeResult";
    op_ble_peripheral_add_service(json) => ble_peripheral.add_service,
        "_internalOnBLEPeripheralAddServiceResult";
    op_ble_peripheral_remove_service(json) => ble_peripheral.remove_service,
        "_internalOnBLEPeripheralRemoveServiceResult";
    op_ble_peripheral_start_advertising(json) => ble_peripheral.start_advertising,
        "_internalOnBLEPeripheralStartAdvertisingResult";
    op_ble_peripheral_stop_advertising(json) => ble_peripheral.stop_advertising,
        "_internalOnBLEPeripheralStopAdvertisingResult";
    op_ble_peripheral_write_characteristic_value(json) => ble_peripheral.write_characteristic_value,
        "_internalOnBLEPeripheralWriteCharacteristicValueResult";
    op_check_session(json) => auth.check_session, "_internalOnCheckSessionResult";
    op_choose_image(json) => image_api.choose_image, "_internalOnChooseImageResult";
    op_choose_media(json) => image_api.choose_media, "_internalOnChooseMediaResult";
    op_choose_message_file(json) => image_api.choose_message_file,
        "_internalOnChooseMessageFileResult";
    op_create_ble_peripheral_server(json) => ble_peripheral.create_server,
        "_internalOnCreateBLEPeripheralServerResult";
    op_close_ble_connection(json) => bluetooth.close_ble_connection,
        "_internalOnCloseBLEConnectionResult";
    op_close_bluetooth_adapter(json) => bluetooth.close_adapter,
        "_internalOnCloseBluetoothAdapterResult";
    op_create_ble_connection(json) => bluetooth.create_ble_connection,
        "_internalOnCreateBLEConnectionResult";
    op_ecosystem_reply(json) => ecosystem.reply;
    op_exit_pointer_lock(none) => window.exit_pointer_lock;
    op_get_beacons(json) => bluetooth.get_beacons, "_internalOnGetBeaconsResult";
    op_get_ble_device_characteristics(json) => bluetooth.get_ble_device_characteristics,
        "_internalOnGetBLEDeviceCharacteristicsResult";
    op_get_ble_device_rssi(json) => bluetooth.get_ble_device_rssi,
        "_internalOnGetBLEDeviceRSSIResult";
    op_get_ble_device_services(json) => bluetooth.get_ble_device_services,
        "_internalOnGetBLEDeviceServicesResult";
    op_get_ble_mtu(json) => bluetooth.get_ble_mtu, "_internalOnGetBLEMTUResult";
    op_get_bluetooth_adapter_state(json) => bluetooth.get_adapter_state,
        "_internalOnGetBluetoothAdapterStateResult";
    op_get_bluetooth_devices(json) => bluetooth.get_devices, "_internalOnGetBluetoothDevicesResult";
    op_get_clipboard_data(json) => clipboard.get_data, "_internalOnGetClipboardDataResult";
    op_get_connected_bluetooth_devices(json) => bluetooth.get_connected_devices,
        "_internalOnGetConnectedBluetoothDevicesResult";
    op_get_fuzzy_location(json) => location.get_fuzzy_location, "_internalOnFuzzyLocationResult";
    op_get_location(json) => location.get_location, "_internalOnLocationResult";
    op_get_phone_number(json) => auth.get_phone_number, "_internalOnGetPhoneNumberResult";
    op_get_screen_brightness(json) => screen.get_brightness, "_internalOnGetScreenBrightnessResult";
    op_set_device_orientation(json) => screen.set_orientation,
        "_internalOnSetDeviceOrientationResult";
    op_get_screen_recording_state(json) => screen.get_screen_recording_state,
        "_internalOnGetScreenRecordingStateResult";
    op_get_user_info(json) => auth.get_user_info, "_internalOnGetUserInfoResult";
    op_hide_loading(none) => interaction.hide_loading;
    op_hide_toast(none) => interaction.hide_toast;
    op_is_bluetooth_device_paired(json) => bluetooth.is_device_paired,
        "_internalOnIsBluetoothDevicePairedResult";
    op_login(json) => auth.login, "_internalOnLoginResult";
    op_make_bluetooth_pair(json) => bluetooth.make_pair, "_internalOnMakeBluetoothPairResult";
    op_navigate_back_mini_program(json) => navigate.navigate_back_mini_program;
    op_navigate_to_mini_program(json) => navigate.navigate_to_mini_program,
        "_internalOnNavigateToMiniProgramResult";
    op_notify_ble_characteristic_value_change(json) =>
        bluetooth.notify_ble_characteristic_value_change,
        "_internalOnNotifyBLECharacteristicValueChangeResult";
    op_open_app_authorize_setting(id) => system_info.open_app_authorize_setting,
        "_internalOnOpenAppAuthorizeSettingFinished";
    op_open_bluetooth_adapter(json) => bluetooth.open_adapter,
        "_internalOnOpenBluetoothAdapterResult";
    op_open_customer_service_conversation(json) => navigate.open_customer_service_conversation;
    op_open_setting(json) => system_info.open_setting, "_internalOnOpenSettingResult";
    op_open_system_bluetooth_setting(id) => system_info.open_bluetooth_settings,
        "_internalOnOpenBluetoothSettingResult";
    op_read_ble_characteristic_value(json) => bluetooth.read_ble_characteristic_value,
        "_internalOnReadBLECharacteristicValueResult";
    op_request_midas_payment(json) => payment.request_midas_payment,
        "_internalOnMidasPaymentResult";
    op_request_midas_payment_game_item(json) => payment.request_midas_payment_game_item,
        "_internalOnMidasPaymentGameItemResult";
    op_request_pointer_lock(none) => window.request_pointer_lock;
    op_scan_code(json) => scan_code.scan_code, "_internalOnScanCodeResult";
    op_set_ble_mtu(json) => bluetooth.set_ble_mtu, "_internalOnSetBLEMTUResult";
    op_set_clipboard_data(json) => clipboard.set_data, "_internalOnSetClipboardDataResult";
    op_set_screen_brightness(json) => screen.set_brightness, "_internalOnSetScreenBrightnessResult";
    op_set_visual_effect_on_capture(json) => screen.set_visual_effect_on_capture,
        "_internalOnSetVisualEffectOnCaptureResult";
    op_set_window_size(json) => window.set_window_size, "_internalOnSetWindowSizeResult";
    op_share_set_menu(json) => share.set_share_menu;
    op_show_action_sheet(json) => interaction.show_action_sheet, "_internalOnActionSheetResult";
    op_show_loading(json) => interaction.show_loading;
    op_show_modal(json) => interaction.show_modal, "_internalOnModalResult";
    op_show_toast(json) => interaction.show_toast;
    op_start_accelerometer(json) => accelerometer.start, "_internalOnStartAccelerometerResult";
    op_start_beacon_discovery(json) => bluetooth.start_beacon_discovery,
        "_internalOnStartBeaconDiscoveryResult";
    op_start_bluetooth_devices_discovery(json) => bluetooth.start_devices_discovery,
        "_internalOnStartBluetoothDevicesDiscoveryResult";
    op_start_capture_screen(none) => screen.start_capture_screen;
    op_start_compass(json) => compass.start, "_internalOnStartCompassResult";
    op_start_device_motion(json) => device_motion.start,
        "_internalOnStartDeviceMotionListeningResult";
    op_start_gyroscope(json) => gyroscope.start, "_internalOnStartGyroscopeResult";
    op_start_screen_recording_observer(none) => screen.start_screen_recording_observer;
    op_stop_accelerometer(none) => accelerometer.stop;
    op_stop_beacon_discovery(json) => bluetooth.stop_beacon_discovery,
        "_internalOnStopBeaconDiscoveryResult";
    op_stop_bluetooth_devices_discovery(json) => bluetooth.stop_devices_discovery,
        "_internalOnStopBluetoothDevicesDiscoveryResult";
    op_stop_capture_screen(none) => screen.stop_capture_screen;
    op_stop_compass(none) => compass.stop;
    op_stop_device_motion(none) => device_motion.stop;
    op_stop_gyroscope(none) => gyroscope.stop;
    op_stop_screen_recording_observer(none) => screen.stop_screen_recording_observer;
    op_video_destroy(json) => video.destroy;
    op_video_exit_full_screen(json) => video.exit_full_screen,
        "_internalOnVideoExitFullScreenResult";
    op_video_pause(json) => video.pause, "_internalOnVideoPauseResult";
    op_video_play(json) => video.play, "_internalOnVideoPlayResult";
    op_video_request_full_screen(json) => video.request_full_screen,
        "_internalOnVideoRequestFullScreenResult";
    op_video_seek(json) => video.seek, "_internalOnVideoSeekResult";
    op_video_stop(json) => video.stop, "_internalOnVideoStopResult";
    op_write_ble_characteristic_value(json) => bluetooth.write_ble_characteristic_value,
        "_internalOnWriteBLECharacteristicValueResult";
}

/// Forward one of the channel's ops to the host app, or refuse it as the
/// embedded op would: through its result hook when it has one, so content's
/// call fails now, or in the log when nothing answers it.
pub(crate) fn command(
    device: Option<&dyn DeviceServices>,
    op: u32,
    args: Vec<OwnedValue>,
    sink: &ServiceEventSink<'_>,
) {
    let Err(refusal) = call(device, op, args) else {
        return;
    };
    match (hook(op), refusal.request_id) {
        (Some(hook), Some(request_id)) => {
            let result = serde_json::json!({ "requestId": request_id, "error": refusal.message });
            sink.host_hook(hook, &hook_args_one(result.to_string()));
        }
        _ => warn!(
            "service command {} refused: {}",
            super::service_ops::name_of(op).unwrap_or("?"),
            refusal.message
        ),
    }
}

// ---------------------------------------------------------------------------
// Requests that name content's files
// ---------------------------------------------------------------------------
//
// A request that names one of content's files -- an image to preview, a file
// to save, a picture to share -- is not forwarded as content wrote it: each
// path is resolved through the game's sandbox into a real file the host app
// can open (`migo_services::host_paths`, which the embedded ops export through
// too), and a package entry with no file of its own is copied out first. That
// takes the io scheduler, so these are awaited requests, as their embedded ops
// are: a refusal -- a scope not granted, no such service, a path that is not
// content's file -- rejects the call, and content's facade fails it in the
// embedded op's words. What the host app answers reaches content through the
// method's hook, as for every other channel method.

/// A refusal of the embedded op's, with its words.
fn refused(message: impl Into<String>) -> ServiceError {
    ServiceError::generic(message)
}

/// The scope check, then the service: what the embedded op makes sure of
/// before it exports anything.
fn reach<S: ?Sized>(
    device: Option<&dyn DeviceServices>,
    op: &str,
    unsupported: &str,
    service: impl FnOnce(&dyn DeviceServices) -> Option<Arc<S>>,
) -> Result<Arc<S>, ServiceError> {
    if let Some(message) = scope_refusal(device, op) {
        return Err(refused(message));
    }
    device.and_then(service).ok_or_else(|| refused(unsupported))
}

/// One row of the table below, by the shape of what its op is given.
macro_rules! file_request_row {
    (files $files:ident, $sandbox:ident, $paths:ident, $device:ident, $op:ident, $args:ident,
        $accessor:ident . $method:ident, $unsupported:literal) => {{
        let [request] = exactly(id::$op, $args)?;
        let request = string(id::$op, 0, request)?;
        let service = reach(
            $device.as_deref(),
            stringify!($op),
            $unsupported,
            |device| device.$accessor(),
        )?;
        Box::pin(async move {
            let request = host_paths::export_paths(&$sandbox, &request, host_paths::$files).await?;
            service
                .$method(&request)
                .map(|()| OwnedValue::Null)
                .map_err(|error| refused(error.message))
        })
    }};
    (ecosystem, $sandbox:ident, $paths:ident, $device:ident, $op:ident, $args:ident,
        $accessor:ident . $method:ident, $unsupported:literal) => {{
        let [api, request] = exactly(id::$op, $args)?;
        let (api, request) = (string(id::$op, 0, api)?, string(id::$op, 1, request)?);
        let service = reach(
            $device.as_deref(),
            stringify!($op),
            $unsupported,
            |device| device.$accessor(),
        )?;
        Box::pin(async move {
            // Only a request that names files is parsed here.
            let request = match host_paths::ecosystem_files(&api) {
                Some(files) => host_paths::export_paths(&$sandbox, &request, files).await?,
                None => request,
            };
            service
                .$method(&request)
                .map(|()| OwnedValue::Null)
                .map_err(|error| refused(error.message))
        })
    }};
    (menu_reply, $sandbox:ident, $paths:ident, $device:ident, $op:ident, $args:ident,
        $accessor:ident . $method:ident, $unsupported:literal) => {{
        let [reply] = exactly(id::$op, $args)?;
        let reply = string(id::$op, 0, reply)?;
        let service = reach(
            $device.as_deref(),
            stringify!($op),
            $unsupported,
            |device| device.$accessor(),
        )?;
        Box::pin(async move {
            let mut reply: serde_json::Value = serde_json::from_str(&reply)
                .map_err(|e| refused(format!("malformed reply: {e}")))?;
            host_paths::export_menu_share(&$sandbox, &mut reply).await;
            service
                .$method(&reply.to_string())
                .map(|()| OwnedValue::Null)
                .map_err(|error| refused(error.message))
        })
    }};
    (recording, $sandbox:ident, $paths:ident, $device:ident, $op:ident, $args:ident,
        $accessor:ident . $method:ident, $unsupported:literal) => {{
        let [request] = exactly(id::$op, $args)?;
        let request = string(id::$op, 0, request)?;
        let service = reach(
            $device.as_deref(),
            stringify!($op),
            $unsupported,
            |device| device.$accessor(),
        )?;
        Box::pin(async move {
            // The game's last recording, with its music resolved through the
            // sandbox: refused, before the host is asked, without either.
            let request =
                migo_services::game_recorder::share_request(&$sandbox, $paths.as_deref(), &request)
                    .await?;
            service
                .$method(&request)
                .map(|()| OwnedValue::Null)
                .map_err(|error| refused(error.message))
        })
    }};
}

macro_rules! file_requests {
    ($($op:ident($form:ident $($files:ident)?) => $accessor:ident . $method:ident,
        $unsupported:literal;)*) => {
        /// Whether `op` is a channel request that names content's files.
        pub(crate) fn names_files(op: u32) -> bool {
            matches!(op, $(id::$op)|*)
        }

        /// Export the paths `op`'s request names, then make its call; the
        /// answer is the host's acceptance, or the refusal.
        pub(crate) fn request_with_files(
            sandbox: Sandbox,
            paths: Option<Arc<shared::vfs::GamePaths>>,
            device: Option<Arc<dyn DeviceServices>>,
            op: u32,
            args: Vec<OwnedValue>,
        ) -> Result<BoxFuture<'static, Result<OwnedValue, ServiceError>>, ServiceError> {
            Ok(match op {
                $(id::$op => file_request_row!(
                    $form $($files)?, sandbox, paths, device, $op, args, $accessor.$method, $unsupported
                ),)*
                other => return Err(not_a(other, "file request")),
            })
        }

        /// Every op of the table's, by name.
        #[cfg(test)]
        const FILE_OPS: &[&str] = &[$(stringify!($op)),*];
    };
}

// What the embedded op of each method takes, which of its fields are files
// (`migo_services::host_paths`), what it calls, and the words it refuses with
// when the platform has no such service.
file_requests! {
    op_compress_image(files COMPRESS_IMAGE) => image_api.compress_image,
        "compressImage:fail not supported";
    op_ecosystem_call(ecosystem) => ecosystem.call, "not supported";
    op_preview_image(files PREVIEW_IMAGE) => image_api.preview_image,
        "previewImage:fail not supported";
    op_preview_media(files PREVIEW_MEDIA) => image_api.preview_media,
        "previewMedia:fail not supported";
    op_save_file_to_disk(files SAVE_FILE_TO_DISK) => image_api.save_file_to_disk,
        "saveFileToDisk:fail not supported";
    op_save_image_to_photos_album(files SAVE_IMAGE_TO_PHOTOS_ALBUM) =>
        image_api.save_image_to_photos_album, "saveImageToPhotosAlbum:fail not supported";
    op_share_app_message(files SHARE_APP_MESSAGE) => share.share_app_message,
        "shareAppMessage:fail not supported";
    op_share_game_recording(recording) => share.share_game_recording,
        "operateGameRecorderVideo:fail not supported";
    op_share_menu_reply(menu_reply) => share.menu_share_reply, "share menu: not supported";
    op_share_message_to_friend(files SHARE_MESSAGE_TO_FRIEND) => share.share_message_to_friend,
        "shareMessageToFriend:fail not supported";
    op_show_share_image_menu(files SHOW_SHARE_IMAGE_MENU) => share.show_share_image_menu,
        "showShareImageMenu:fail not supported";
}

#[cfg(test)]
#[path = "service_channel_tests.rs"]
mod tests;
