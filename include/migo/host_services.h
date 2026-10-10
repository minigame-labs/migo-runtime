/*
 * Migo C ABI: the host-service channel.
 *
 * Some capabilities are never the engine's to implement. Showing an advert,
 * taking a payment, signing a player in, sharing, opening another mini-program:
 * each is the host app's own integration with a vendor SDK, a store or its own
 * backend. The channel lets content reach them through the host:
 *
 *   1. The host declares which services it supplies -- one bit per
 *      MIGO_HOST_SERVICE_* in MigoHostCallbacks.host_services -- and installs
 *      MigoHostCallbacks.on_host_service_call. A service it does not declare
 *      is never called, and content's API for it fails the way it does on a
 *      platform without it.
 *   2. Content's request arrives through the dispatcher as a
 *      MigoHostServiceCall: a service, a method within it and the request's
 *      options as a JSON object.
 *   3. A *call* (call_id != 0) is answered exactly once, from any thread, with
 *      migo_session_complete_host_service_call. A *command* (call_id == 0) is
 *      carried out and answered by nothing.
 *   4. A service with events of its own -- an advert loading, closing,
 *      resizing -- reports each with migo_session_post_host_service_event.
 *
 * Payloads are JSON because these option sets are open and vendor-shaped, and
 * are JSON on every other path into the engine; the small closed option sets
 * elsewhere in this ABI (the keyboard, vibration) stay typed. Every number here
 * is defined once, in contracts/runtime/host-services.json, append-only: the
 * request and result fields each method carries are documented there and in
 * the developer documentation.
 */
#ifndef MIGO_HOST_SERVICES_H_
#define MIGO_HOST_SERVICES_H_

#include <stddef.h> /* offsetof */
#include <migo/types.h>

MIGO_BEGIN_DECLS

/* Services. A service's number is also its bit in host_services. */
typedef uint32_t MigoHostService;
#define MIGO_HOST_SERVICE_AD 0U
#define MIGO_HOST_SERVICE_PAYMENT 1U
#define MIGO_HOST_SERVICE_AUTH 2U
#define MIGO_HOST_SERVICE_SHARE 3U
#define MIGO_HOST_SERVICE_NAVIGATE 4U
#define MIGO_HOST_SERVICE_SUBPACKAGE 5U
#define MIGO_HOST_SERVICE_PERMISSION 6U
#define MIGO_HOST_SERVICE_SETTING 7U
#define MIGO_HOST_SERVICE_INTERACTION 8U
#define MIGO_HOST_SERVICE_CLIPBOARD 9U
#define MIGO_HOST_SERVICE_SCAN_CODE 10U
#define MIGO_HOST_SERVICE_LOCATION 11U
#define MIGO_HOST_SERVICE_IMAGE 12U
#define MIGO_HOST_SERVICE_MOTION 13U
#define MIGO_HOST_SERVICE_SCREEN 14U
#define MIGO_HOST_SERVICE_BLUETOOTH 15U

/*
 * Ads. All six are commands addressed to the advert by the adId in their
 * payload; what happens to an advert is reported as MIGO_AD_EVENT_LIFECYCLE.
 * Content's load() and show() Promises are settled by those events: "load"
 * settles load(), "show" -- the SDK's exposure callback -- settles show() (a
 * full-screen ad's "close" counts as shown too), and "error" rejects both. An
 * ad with no close (banner, grid, custom, game banner) must report "show".
 * The host is authoritative for an incentivised video's reward: content grants
 * one only when the host's close event says the video was watched to the end
 * (isEnded), and the engine never reports that on the host's behalf.
 */
#define MIGO_AD_CREATE 0U
#define MIGO_AD_LOAD 1U
#define MIGO_AD_SHOW 2U
#define MIGO_AD_HIDE 3U
#define MIGO_AD_UPDATE_STYLE 4U
#define MIGO_AD_DESTROY 5U
/* {"adId": <the advert's id>, "event": "load"|"error"|"show"|"hide"|"close"|
 * "resize", ...the event's own fields, e.g. "isEnded" on close}. */
#define MIGO_AD_EVENT_LIFECYCLE 0U

/* Payment: two calls. */
#define MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT 0U
#define MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM 1U

/* Sign-in: four calls. login and getPhoneNumber succeed with {"code": "..."}. */
#define MIGO_AUTH_LOGIN 0U
#define MIGO_AUTH_CHECK_SESSION 1U
#define MIGO_AUTH_GET_USER_INFO 2U
#define MIGO_AUTH_GET_PHONE_NUMBER 3U

/* Sharing: one call. */
#define MIGO_SHARE_SHARE_APP_MESSAGE 0U

/* Navigation: one call and two commands. */
#define MIGO_NAVIGATE_NAVIGATE_TO_MINI_PROGRAM 0U
#define MIGO_NAVIGATE_NAVIGATE_BACK_MINI_PROGRAM 1U
#define MIGO_NAVIGATE_OPEN_CUSTOMER_SERVICE_CONVERSATION 2U

/*
 * Subpackages: one call, for both loadSubpackage and preDownloadSubpackage. The
 * request is {"name", "root"}; report progress while downloading with
 * migo_session_update_host_service_call ({"progress", "totalBytesWritten",
 * "totalBytesExpectedToWrite"}), and succeed with {"zipPath": "<absolute path
 * of the downloaded zip>"} -- the engine installs from it, and the path never
 * reaches content.
 */
#define MIGO_SUBPACKAGE_DOWNLOAD 0U

/*
 * Permission: content's authorize({scope}) asks the host, {"scope", "desc"} --
 * desc is the reason the game declared, for an honest prompt. Success means
 * granted; refusal is a failure with the reason "auth deny". Completing the call
 * does not record the decision: report it with migo_session_set_scope_state,
 * which is also how every gated capability is checked.
 */
#define MIGO_PERMISSION_REQUEST_SCOPE 0U

/* Settings pages content can send the player to: three calls. */
#define MIGO_SETTING_OPEN_SETTING 0U
#define MIGO_SETTING_OPEN_SYSTEM_BLUETOOTH_SETTING 1U
#define MIGO_SETTING_OPEN_APP_AUTHORIZE_SETTING 2U

/*
 * Native UI: toasts and loading indicators are commands; a modal and an action
 * sheet are calls. A modal answers {"confirm", "cancel"} -- with "content", the
 * input's text, when the request was "editable" -- and an action sheet
 * {"tapIndex"}, or fails with the reason "cancel" when the player dismissed it.
 */
#define MIGO_INTERACTION_SHOW_TOAST 0U
#define MIGO_INTERACTION_HIDE_TOAST 1U
#define MIGO_INTERACTION_SHOW_MODAL 2U
#define MIGO_INTERACTION_SHOW_LOADING 3U
#define MIGO_INTERACTION_HIDE_LOADING 4U
#define MIGO_INTERACTION_SHOW_ACTION_SHEET 5U

/* The clipboard: setClipboardData ({"data"}) and getClipboardData (answers {"data"}). */
#define MIGO_CLIPBOARD_SET_CLIPBOARD_DATA 0U
#define MIGO_CLIPBOARD_GET_CLIPBOARD_DATA 1U

/* scanCode: {"onlyFromCamera", "scanType"}; answers {"result", "scanType", ...}. */
#define MIGO_SCAN_CODE_SCAN_CODE 0U

/*
 * Location, both gated on scope.userLocation before they reach the host: answers
 * {"latitude", "longitude", "accuracy", ...} as getLocation and getFuzzyLocation
 * define them.
 */
#define MIGO_LOCATION_GET_LOCATION 0U
#define MIGO_LOCATION_GET_FUZZY_LOCATION 1U

/*
 * Images and media, all calls. A path in a request is a real path the engine
 * resolved from the game's sandbox -- or, for the two previews, an http(s) URL --
 * readable until the call is completed: a host that hands it to a viewer still
 * showing it after that opens or copies it first. A file a result names is
 * handed over when the call is completed: the engine moves it into the session's
 * /tmp (a rename on the same volume) and content sees only that path. Name only
 * files the host owns and will not touch again -- copy a file dialog's answer,
 * which is the user's original, first.
 *
 *   SAVE_IMAGE_TO_PHOTOS_ALBUM {"filePath"}                         -> {}
 *   PREVIEW_IMAGE  {"urls", "current", "showmenu", "referrerPolicy"} -> {} once shown
 *   PREVIEW_MEDIA  {"sources": [{"url", "type", "poster"}], "current", ...} -> {} once shown
 *   COMPRESS_IMAGE {"src", "quality", "compressedWidth", "compressedHeight"}
 *                  -> {"tempFilePath"*}
 *   CHOOSE_IMAGE   {"count", "sizeType", "sourceType"}
 *                  -> {"tempFilePaths"*: [...], "tempFiles": [{"path"*, "size"}]}
 *   CHOOSE_MESSAGE_FILE {"count", "type", "extension"}
 *                  -> {"tempFiles": [{"path"*, "size", "name", "type", "time"}]}
 *   CHOOSE_MEDIA   {"count", "mediaType", "sourceType", "maxDuration", "sizeType", "camera"}
 *                  -> {"type", "tempFiles": [{"tempFilePath"*, "thumbTempFilePath"*, "size",
 *                      "duration", "width", "height", "fileType"}]}
 *
 * (* a file handed over.) A player who dismisses a picker is the failure "cancel".
 */
#define MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM 0U
#define MIGO_IMAGE_PREVIEW_IMAGE 1U
#define MIGO_IMAGE_PREVIEW_MEDIA 2U
#define MIGO_IMAGE_COMPRESS_IMAGE 3U
#define MIGO_IMAGE_CHOOSE_IMAGE 4U
#define MIGO_IMAGE_CHOOSE_MESSAGE_FILE 5U
#define MIGO_IMAGE_CHOOSE_MEDIA 6U

/*
 * Motion sensors. Each START is a call {"interval"} -- "game" (20 ms), "ui"
 * (60 ms) or "normal" (200 ms); the compass takes none -- answered {} once the
 * sensor runs, or failed when the device has none. Each STOP is a command.
 * Readings go to migo_session_post_sensor_sample, typed: they come up to fifty
 * times a second.
 */
#define MIGO_MOTION_START_ACCELEROMETER 0U
#define MIGO_MOTION_STOP_ACCELEROMETER 1U
#define MIGO_MOTION_START_GYROSCOPE 2U
#define MIGO_MOTION_STOP_GYROSCOPE 3U
#define MIGO_MOTION_START_COMPASS 4U
#define MIGO_MOTION_STOP_COMPASS 5U
#define MIGO_MOTION_START_DEVICE_MOTION 6U
#define MIGO_MOTION_STOP_DEVICE_MOTION 7U

/*
 * The screen. Calls: GET_BRIGHTNESS -> {"value"} (0..1); SET_BRIGHTNESS
 * {"value"}; SET_DEVICE_ORIENTATION {"value": "portrait" | "landscape"};
 * GET_RECORDING_STATE -> {"state": "on" | "off"}, failed where the platform
 * cannot tell; SET_VISUAL_EFFECT_ON_CAPTURE {"visualEffect": "none" | "hidden"},
 * hidden keeping the game out of screenshots and recordings. The observer
 * commands run while content listens, and report through the events:
 * USER_CAPTURE_SCREEN {} when the player takes a screenshot,
 * RECORDING_STATE_CHANGE {"state"}, and DEVICE_ORIENTATION_CHANGE {"value":
 * "portrait" | "landscape" | "landscapeReverse"} whenever the device turns.
 * Keeping the screen on stays MigoHostCallbacks.on_keep_screen_on.
 */
#define MIGO_SCREEN_GET_BRIGHTNESS 0U
#define MIGO_SCREEN_SET_BRIGHTNESS 1U
#define MIGO_SCREEN_SET_DEVICE_ORIENTATION 2U
#define MIGO_SCREEN_START_CAPTURE_OBSERVER 3U
#define MIGO_SCREEN_STOP_CAPTURE_OBSERVER 4U
#define MIGO_SCREEN_GET_RECORDING_STATE 5U
#define MIGO_SCREEN_START_RECORDING_OBSERVER 6U
#define MIGO_SCREEN_STOP_RECORDING_OBSERVER 7U
#define MIGO_SCREEN_SET_VISUAL_EFFECT_ON_CAPTURE 8U
#define MIGO_SCREEN_EVENT_USER_CAPTURE_SCREEN 0U
#define MIGO_SCREEN_EVENT_DEVICE_ORIENTATION_CHANGE 1U
#define MIGO_SCREEN_EVENT_RECORDING_STATE_CHANGE 2U

/*
 * Bluetooth: the adapter, scanning, pairing, iBeacons and BLE GATT as a central.
 * Every method is a call, answered when the operation has happened -- a
 * connection made (and its services discovered), a write acknowledged, an RSSI
 * read -- not when it was issued. A failure carries the common mini-game
 * platform's code as its error code (MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE):
 * 10000 adapter not opened, 10001 Bluetooth unavailable, 10002 no such device,
 * 10003 connection failed, 10004 no such service, 10005 no such characteristic,
 * 10006 not connected, 10007 not supported by the characteristic, 10008 system
 * error, 10012 timed out, 10013 invalid data; -1 already connected; iBeacons
 * 11000-11006. Binary values (a write's "value", a pairing "pin", advertising and
 * service data) are lower-case hex strings. Requests and answers, in order:
 *
 *   OPEN_ADAPTER {"mode"} -> {}            CLOSE_ADAPTER -> {}
 *   GET_ADAPTER_STATE -> {"available", "discovering"}
 *   START_DEVICES_DISCOVERY {"services", "allowDuplicatesKey", "interval", "powerLevel"} -> {}
 *   STOP_DEVICES_DISCOVERY -> {}           GET_DEVICES -> {"devices"}
 *   GET_CONNECTED_DEVICES {"services"} -> {"devices": [{"name", "deviceId"}]}
 *   MAKE_PAIR {"deviceId", "pin", "timeout"} -> {} once paired
 *   IS_DEVICE_PAIRED {"deviceId"} -> {} when paired, failed when not
 *   START_BEACON_DISCOVERY {"uuids", "ignoreBluetoothAvailable"} -> {}
 *   STOP_BEACON_DISCOVERY -> {}
 *   GET_BEACONS -> {"beacons": [{"uuid", "major", "minor", "proximity", "accuracy", "rssi"}]}
 *   CREATE_BLE_CONNECTION {"deviceId", "timeout"} -> {}    CLOSE_BLE_CONNECTION {"deviceId"} -> {}
 *   GET_BLE_DEVICE_SERVICES {"deviceId"} -> {"services": [{"uuid", "isPrimary"}]}
 *   GET_BLE_DEVICE_CHARACTERISTICS {"deviceId", "serviceId"}
 *       -> {"characteristics": [{"uuid", "properties": {"read", "write", "notify",
 *           "indicate", "writeNoResponse", "writeDefault"}}]}
 *   READ_BLE_CHARACTERISTIC_VALUE {"deviceId", "serviceId", "characteristicId"} -> {}
 *       once issued; the value arrives through migo_session_post_ble_characteristic_value
 *   WRITE_BLE_CHARACTERISTIC_VALUE {..., "value", "writeType"} -> {}
 *   NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE {..., "state", "type"} -> {}
 *   GET_BLE_DEVICE_RSSI {"deviceId"} -> {"RSSI"}
 *   SET_BLE_MTU {"deviceId", "mtu"} -> {"mtu"}   GET_BLE_MTU {"deviceId", "writeType"} -> {"mtu"}
 *
 * Events: ADAPTER_STATE_CHANGE {"available", "discovering"}; DEVICE_FOUND
 * {"devices": [{"deviceId", "name", "RSSI", "advertisData", "advertisServiceUUIDs",
 * "localName", "serviceData": {uuid: hex}}]} -- advertisData being the
 * manufacturer-specific segment, company identifier first; BLE_CONNECTION_STATE_CHANGE
 * {"deviceId", "connected"}; BLE_MTU_CHANGE {"deviceId", "mtu"}; BEACON_UPDATE
 * {"beacons"}; BEACON_SERVICE_CHANGE {"available", "discovering"}.
 */
#define MIGO_BLUETOOTH_OPEN_ADAPTER 0U
#define MIGO_BLUETOOTH_CLOSE_ADAPTER 1U
#define MIGO_BLUETOOTH_GET_ADAPTER_STATE 2U
#define MIGO_BLUETOOTH_START_DEVICES_DISCOVERY 3U
#define MIGO_BLUETOOTH_STOP_DEVICES_DISCOVERY 4U
#define MIGO_BLUETOOTH_GET_DEVICES 5U
#define MIGO_BLUETOOTH_GET_CONNECTED_DEVICES 6U
#define MIGO_BLUETOOTH_MAKE_PAIR 7U
#define MIGO_BLUETOOTH_IS_DEVICE_PAIRED 8U
#define MIGO_BLUETOOTH_START_BEACON_DISCOVERY 9U
#define MIGO_BLUETOOTH_STOP_BEACON_DISCOVERY 10U
#define MIGO_BLUETOOTH_GET_BEACONS 11U
#define MIGO_BLUETOOTH_CREATE_BLE_CONNECTION 12U
#define MIGO_BLUETOOTH_CLOSE_BLE_CONNECTION 13U
#define MIGO_BLUETOOTH_GET_BLE_DEVICE_SERVICES 14U
#define MIGO_BLUETOOTH_GET_BLE_DEVICE_CHARACTERISTICS 15U
#define MIGO_BLUETOOTH_READ_BLE_CHARACTERISTIC_VALUE 16U
#define MIGO_BLUETOOTH_WRITE_BLE_CHARACTERISTIC_VALUE 17U
#define MIGO_BLUETOOTH_NOTIFY_BLE_CHARACTERISTIC_VALUE_CHANGE 18U
#define MIGO_BLUETOOTH_GET_BLE_DEVICE_RSSI 19U
#define MIGO_BLUETOOTH_SET_BLE_MTU 20U
#define MIGO_BLUETOOTH_GET_BLE_MTU 21U
#define MIGO_BLUETOOTH_EVENT_ADAPTER_STATE_CHANGE 0U
#define MIGO_BLUETOOTH_EVENT_DEVICE_FOUND 1U
#define MIGO_BLUETOOTH_EVENT_BLE_CONNECTION_STATE_CHANGE 2U
#define MIGO_BLUETOOTH_EVENT_BLE_MTU_CHANGE 3U
#define MIGO_BLUETOOTH_EVENT_BEACON_UPDATE 4U
#define MIGO_BLUETOOTH_EVENT_BEACON_SERVICE_CHANGE 5U

/*
 * Content's permission scopes, in migo.getSetting()'s order. The host decides
 * each; until it reports one, content's capability calls that need it fail with
 * "auth deny", and getSetting omits it -- nobody has decided, which content
 * tells apart from a refusal.
 */
typedef uint32_t MigoScope;
#define MIGO_SCOPE_USER_INFO 0U
#define MIGO_SCOPE_USER_LOCATION 1U
#define MIGO_SCOPE_USER_LOCATION_BACKGROUND 2U
#define MIGO_SCOPE_ADDRESS 3U
#define MIGO_SCOPE_INVOICE_TITLE 4U
#define MIGO_SCOPE_INVOICE 5U
#define MIGO_SCOPE_WERUN 6U
#define MIGO_SCOPE_RECORD 7U
#define MIGO_SCOPE_WRITE_PHOTOS_ALBUM 8U
#define MIGO_SCOPE_CAMERA 9U
#define MIGO_SCOPE_BLUETOOTH 10U
#define MIGO_SCOPE_ADD_PHONE_CONTACT 11U
#define MIGO_SCOPE_ADD_PHONE_CALENDAR 12U
#define MIGO_SCOPE_FRIEND_INTERACTION 13U
#define MIGO_SCOPE_GAME_CLUB_DATA 14U

typedef uint32_t MigoScopeState;
#define MIGO_SCOPE_STATE_UNKNOWN 0U
#define MIGO_SCOPE_STATE_GRANTED 1U
#define MIGO_SCOPE_STATE_DENIED 2U

/* The system switches migo.getSystemSetting() reports. Unreported reads as off. */
typedef uint32_t MigoSystemSettingFlags;
#define MIGO_SYSTEM_SETTING_FLAG_NONE 0U
#define MIGO_SYSTEM_SETTING_FLAG_BLUETOOTH_ENABLED (1U << 0)
#define MIGO_SYSTEM_SETTING_FLAG_LOCATION_ENABLED (1U << 1)
#define MIGO_SYSTEM_SETTING_FLAG_WIFI_ENABLED (1U << 2)

/* One OS authorisation of the host app, as migo.getAppAuthorizeSetting() words it. */
#define MIGO_AUTHORIZATION_NOT_DETERMINED 0U
#define MIGO_AUTHORIZATION_AUTHORIZED 1U
#define MIGO_AUTHORIZATION_DENIED 2U

/*
 * What the operating system has granted the host app itself -- not content's
 * scopes, which are the host's own decisions. Each field is a
 * MIGO_AUTHORIZATION_*; location_reduced_accuracy is 1 when location is granted
 * only approximately.
 */
typedef struct MigoAppAuthorizeSetting {
    uint32_t struct_size;
    uint32_t abi_version;
    uint8_t album;
    uint8_t bluetooth;
    uint8_t camera;
    uint8_t location;
    uint8_t microphone;
    uint8_t notification;
    uint8_t notification_alert;
    uint8_t notification_badge;
    uint8_t notification_sound;
    uint8_t phone_calendar;
    uint8_t location_reduced_accuracy;
    uint8_t reserved0;
} MigoAppAuthorizeSetting;

MIGO_STATIC_ASSERT(offsetof(MigoAppAuthorizeSetting, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(sizeof(MigoAppAuthorizeSetting) == 20,
                   "MigoAppAuthorizeSetting size changed");

/* Which sensor a MigoSensorSample comes from, and what its values are. */
typedef uint32_t MigoSensorKind;
/* values = x, y, z in g, gravity included: x right, y up the screen, z out of it,
 * so a device lying face up reads z = +1. Convert a platform that reports m/s^2
 * or the opposite sign. */
#define MIGO_SENSOR_ACCELEROMETER 0U
/* values = x, y, z angular velocity in rad/s about the same axes. */
#define MIGO_SENSOR_GYROSCOPE 1U
/* values = alpha (0..360), beta (-180..180), gamma (-90..90), in degrees. */
#define MIGO_SENSOR_DEVICE_MOTION 2U
/* values[0] = heading in degrees from magnetic north (0..360); the others 0. */
#define MIGO_SENSOR_COMPASS 3U

/* How far a compass heading can be trusted. Zero is "unknown". */
typedef uint32_t MigoCompassAccuracy;
#define MIGO_COMPASS_ACCURACY_UNKNOWN 0U
#define MIGO_COMPASS_ACCURACY_HIGH 1U
#define MIGO_COMPASS_ACCURACY_MEDIUM 2U
#define MIGO_COMPASS_ACCURACY_LOW 3U
#define MIGO_COMPASS_ACCURACY_NO_CONTACT 4U
#define MIGO_COMPASS_ACCURACY_UNRELIABLE 5U

/* One reading from a sensor content started through MIGO_HOST_SERVICE_MOTION.
 * compass_accuracy is a MIGO_COMPASS_ACCURACY_* for a compass reading and 0
 * otherwise. */
typedef struct MigoSensorSample {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoSensorKind kind;
    MigoCompassAccuracy compass_accuracy;
    double values[3];
} MigoSensorSample;

MIGO_STATIC_ASSERT(offsetof(MigoSensorSample, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(offsetof(MigoSensorSample, values) == 16, "MigoSensorSample layout changed");
MIGO_STATIC_ASSERT(sizeof(MigoSensorSample) == 40, "MigoSensorSample size changed");

/* One characteristic value a connected peripheral sent -- a notification, an
 * indication or the answer to READ_BLE_CHARACTERISTIC_VALUE. The ids are UTF-8 as
 * the requests named them; value is the raw bytes (at most 512). */
typedef struct MigoBleCharacteristicValue {
    uint32_t struct_size;
    uint32_t abi_version;
    const char *device_id_utf8;
    const char *service_id_utf8;
    const char *characteristic_id_utf8;
    const uint8_t *value;
    uint32_t device_id_length;
    uint32_t service_id_length;
    uint32_t characteristic_id_length;
    uint32_t value_length;
} MigoBleCharacteristicValue;

MIGO_STATIC_ASSERT(offsetof(MigoBleCharacteristicValue, struct_size) == 0,
                   "every versioned struct must begin with struct_size");

/*
 * The largest payload or message either direction carries, in bytes. A longer
 * one is refused with MIGO_ERROR_INVALID_ARGUMENT before any byte is read.
 */
#define MIGO_HOST_SERVICE_PAYLOAD_MAX_BYTES (1U << 20)

/*
 * A request content made of the host. Written by the library; every pointer is
 * borrowed for the duration of the callback only. call_id is opaque: the host
 * hands it back unchanged and never interprets it.
 */
typedef struct MigoHostServiceCall {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t call_id;
    MigoHostService service;
    uint32_t method;
    const char *payload_json_utf8;
    uint32_t payload_length;
    uint32_t reserved0;
} MigoHostServiceCall;

typedef void(MIGO_CALL *MigoOnHostServiceCallFn)(void *user_data, MigoSession *session,
                                                 const MigoHostServiceCall *call);

typedef uint32_t MigoHostServiceStatus;
#define MIGO_HOST_SERVICE_STATUS_OK 0U
#define MIGO_HOST_SERVICE_STATUS_FAIL 1U

typedef uint32_t MigoHostServiceResultFlags;
#define MIGO_HOST_SERVICE_RESULT_FLAG_NONE 0U
/* error_code carries a value; without the flag a failure has no numeric code. */
#define MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE (1U << 0)

/*
 * A call's outcome. On MIGO_HOST_SERVICE_STATUS_OK, payload is the result as a
 * JSON object (or empty for a result with no fields) and the message is empty
 * with no flags. On MIGO_HOST_SERVICE_STATUS_FAIL, message is the reason --
 * required, and without the API's name: content sees "<api>:fail <message>",
 * composed by content because one method can serve several APIs -- error_code
 * is the API's numeric code when flagged, and payload is empty. Strings are
 * length-delimited UTF-8, read during the call and not retained.
 */
typedef struct MigoHostServiceResult {
    uint32_t struct_size;
    uint32_t abi_version;
    MigoHostServiceStatus status;
    MigoHostServiceResultFlags flags;
    int32_t error_code;
    uint32_t message_length;
    const char *message_utf8;
    const char *payload_json_utf8;
    uint32_t payload_length;
    uint32_t reserved0;
} MigoHostServiceResult;

MIGO_STATIC_ASSERT(offsetof(MigoHostServiceCall, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(offsetof(MigoHostServiceCall, call_id) == 8,
                   "MigoHostServiceCall.call_id is naturally aligned");
MIGO_STATIC_ASSERT(offsetof(MigoHostServiceResult, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoHostServiceCall) == 40, "MigoHostServiceCall LP64 size changed");
MIGO_STATIC_ASSERT(sizeof(MigoHostServiceResult) == 48,
                   "MigoHostServiceResult LP64 size changed");
#endif

/*
 * Complete one call, exactly once, from any thread. Returns MIGO_OK when the
 * outcome was accepted -- including for a call whose content has gone, since
 * no one is left to tell and that is not the host's error -- and
 * MIGO_ERROR_INVALID_ARGUMENT for a NULL argument, a call_id that does not name
 * a call of a service this host declared, a malformed result (a failure without
 * a reason among them), or a success payload that is not a JSON object. A result must not name a field the
 * library writes itself ("requestId", "error", "errMsg").
 */
MIGO_API MigoResult MIGO_CALL migo_session_complete_host_service_call(
    MigoSession *session, uint64_t call_id, const MigoHostServiceResult *result);

/*
 * Report progress on a call still in flight, from any thread: a JSON object of
 * the method's progress fields (see MIGO_SUBPACKAGE_DOWNLOAD). Returns MIGO_OK
 * when accepted or there is no running content to tell, and
 * MIGO_ERROR_INVALID_ARGUMENT for a call_id that does not name a call of a
 * declared service, a method that reports no progress, or a payload that is not
 * a JSON object.
 */
MIGO_API MigoResult MIGO_CALL migo_session_update_host_service_call(
    MigoSession *session, uint64_t call_id, const char *payload_json_utf8,
    uint32_t payload_length);

/*
 * Report one of a declared service's own events, from any thread. payload is a
 * JSON object, length-delimited UTF-8, read during the call. Returns MIGO_OK
 * when the event was accepted or there is no running content to deliver it to,
 * and MIGO_ERROR_INVALID_ARGUMENT for a service the host did not declare, an
 * event that service does not define, or a payload that is not a JSON object.
 */
MIGO_API MigoResult MIGO_CALL migo_session_post_host_service_event(
    MigoSession *session, MigoHostService service, uint32_t event,
    const char *payload_json_utf8, uint32_t payload_length);

/*
 * Record the host's standing decision for one scope, from any thread, whenever
 * it changes -- including a revocation made in system settings while content
 * runs; the next gated call reads it. Needs MIGO_HOST_SERVICE_PERMISSION
 * declared, because without that service every scope is denied and a grant
 * would be read by nobody. Returns MIGO_ERROR_INVALID_ARGUMENT for an unknown
 * scope or state, or when the permission service is not declared. Revoking
 * stops new use; it does not tear down a camera or connection already open.
 */
MIGO_API MigoResult MIGO_CALL migo_session_set_scope_state(MigoSession *session,
                                                           MigoScope scope,
                                                           MigoScopeState state);

/*
 * Report the system switches, from any thread, when they change. The device
 * orientation getSystemSetting() also reports is the attached window's shape and
 * needs no report. Returns MIGO_ERROR_INVALID_ARGUMENT for a flag this header
 * does not define. May be called before a Surface is attached; the report is
 * kept.
 */
MIGO_API MigoResult MIGO_CALL migo_session_set_system_settings(MigoSession *session,
                                                               MigoSystemSettingFlags flags);

/*
 * Report the host app's OS authorisations, from any thread, when they change.
 * Until the first report every one reads as "not determined". Returns
 * MIGO_ERROR_INVALID_ARGUMENT for a NULL or malformed record or a value outside
 * MIGO_AUTHORIZATION_*. May be called before a Surface is attached; the report
 * is kept.
 */
MIGO_API MigoResult MIGO_CALL migo_session_set_app_authorize_setting(
    MigoSession *session, const MigoAppAuthorizeSetting *setting);

/*
 * Deliver one sensor reading, from any thread, while a Surface is attached.
 * Readings are dropped rather than queued without bound when content falls
 * behind. Returns MIGO_ERROR_INVALID_ARGUMENT for a NULL or malformed sample, a
 * kind or accuracy this header does not define, a value that is not finite, or
 * when the host did not declare MIGO_HOST_SERVICE_MOTION; MIGO_ERROR_INVALID_STATE
 * with no Surface attached.
 */
MIGO_API MigoResult MIGO_CALL migo_session_post_sensor_sample(MigoSession *session,
                                                              const MigoSensorSample *sample);

/*
 * Deliver one characteristic value, from any thread, while a Surface is
 * attached. Typed because a peripheral may notify a hundred times a second; the
 * bytes are copied into a pooled slot, so the record is only borrowed for the
 * call. Dropped rather than queued without bound when content falls behind.
 * Returns MIGO_ERROR_INVALID_ARGUMENT for a NULL or malformed record, an id that
 * is empty, longer than 256 bytes or not UTF-8, a value longer than 512 bytes, or
 * when the host did not declare MIGO_HOST_SERVICE_BLUETOOTH;
 * MIGO_ERROR_INVALID_STATE with no Surface attached.
 */
MIGO_API MigoResult MIGO_CALL migo_session_post_ble_characteristic_value(
    MigoSession *session, const MigoBleCharacteristicValue *value);

MIGO_END_DECLS

#endif /* MIGO_HOST_SERVICES_H_ */
