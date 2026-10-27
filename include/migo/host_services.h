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
#define MIGO_HOST_SERVICE_WINDOW 16U
#define MIGO_HOST_SERVICE_ECOSYSTEM 17U
#define MIGO_HOST_SERVICE_UPDATE 18U
#define MIGO_HOST_SERVICE_CAMERA 19U
#define MIGO_HOST_SERVICE_RECORDER 20U
#define MIGO_HOST_SERVICE_VIDEO 21U
#define MIGO_HOST_SERVICE_BLE_PERIPHERAL 22U

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
 *
 * CREATE's "adType" is "rewardedVideo", "interstitial", "banner", "custom",
 * "grid", "gameBanner", "gameIcon", "gamePortal" or "skipCard" -- the pass that
 * lets a player skip adverts, which the host sells: LOAD fetches the offer, SHOW
 * opens the purchase sheet, its "close" carries "result" ("close", "pay_cancel"
 * or "pay_success") and "use" ({"usedCount": passes spent today}) reports a pass
 * spent.
 */
#define MIGO_AD_CREATE 0U
#define MIGO_AD_LOAD 1U
#define MIGO_AD_SHOW 2U
#define MIGO_AD_HIDE 3U
#define MIGO_AD_UPDATE_STYLE 4U
#define MIGO_AD_DESTROY 5U
/* {"adId": <the advert's id>, "event": "load"|"error"|"show"|"hide"|"close"|
 * "resize"|"use", ...the event's own fields, e.g. "isEnded" on close}.
 *
 * The host's own cover advert -- the one it shows as the game launches, which
 * no content call creates -- is reported here too, with no adId, whenever its
 * state changes: {"event": "splashStatus", "status": "unknown"|"pending"|
 * "success"|"fail", "code": -1|1|2|3} (1 shown, 2 filtered out, 3 timed out).
 * getShowSplashAdStatus() answers the last report, and "unknown", -1 before
 * one. */
#define MIGO_AD_EVENT_LIFECYCLE 0U

/* Payment: two calls. */
#define MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT 0U
#define MIGO_PAYMENT_REQUEST_MIDAS_PAYMENT_GAME_ITEM 1U

/* Sign-in: four calls. login and getPhoneNumber succeed with {"code": "..."}. */
#define MIGO_AUTH_LOGIN 0U
#define MIGO_AUTH_CHECK_SESSION 1U
#define MIGO_AUTH_GET_USER_INFO 2U
#define MIGO_AUTH_GET_PHONE_NUMBER 3U

/*
 * Sharing: three calls, each naming at most one image -- a real path the engine
 * resolved through the sandbox, readable until the call completes (only
 * SHARE_APP_MESSAGE may name an http(s) URL instead). An option content did not
 * set is absent.
 *
 *   SHARE_APP_MESSAGE {"title"?, "imageUrl"?, "query"?, "imageUrlId"?,
 *                      "toCurrentGroup"?, "path"?} -- the share content asked for.
 *                      Without an image of content's own (no imageUrl or
 *                      imageUrlId), imageUrl is the screen as the engine took
 *                      it: the top 5:4 of the frame, as a JPEG.
 *   SHARE_MESSAGE_TO_FRIEND {"openId", "title"?, "imageUrl"?, "imageUrlId"?,
 *                            "query"?, "shareMessageToFriendScene"?} -- to one
 *                            friend; query and shareMessageToFriendScene are what
 *                            setMessageToFriendQuery set, for the friend's enter
 *                            options.
 *   SHOW_SHARE_IMAGE_MENU {"path", "needShowEntrance", "entrancePath"?} -- the
 *                          share sheet for one image.
 *   SHARE_GAME_RECORDING {"video", "title"?, "desc"?, "query"?, "path"?, "bgm"?,
 *                         "timeRange"?, "volume", "atempo", "audioMix"} -- the
 *                         game's last finished recording (getGameRecorder), for
 *                         its share button or operateGameRecorderVideo. video is
 *                         the engine's own file, readable for the rest of the
 *                         session; bgm a real path resolved through the
 *                         sandbox; timeRange [[from, to], ...] in ms of the
 *                         recording, to be cut to, at most 60 s in all; volume
 *                         0 to 1; atempo one of 0.3, 0.5, 1, 1.5, 2, 2.5, 3;
 *                         audioMix whether bgm mixes with the recording's own
 *                         sound. The editing is the host's. The result may carry
 *                         "shareToGameCenter" {"errCode", "errMsg"}.
 *
 * The share menu is the host's, and two commands keep it in step with content:
 *
 *   SET_SHARE_MENU {"menus": [...], "withShareTicket", ...} -- the menu's whole
 *                   state after each change: the share items shown
 *                   ("shareAppMessage", "shareTimeline"; none until content
 *                   shows them), and the updateShareMenu fields content set.
 *   MENU_SHARE_REPLY {"replyId", "menu", "content"} -- what to share for the
 *                   item the player picked, answering EVENT_MENU_SHARE
 *                   {"menu", "replyId"} ("shareAppMessage", "shareTimeline" or
 *                   "addToFavorites"). Every event is answered once; content is
 *                   null when nothing in the game answered, and the host shares
 *                   with its own defaults (the game's name, its icon) -- except
 *                   "shareAppMessage", which always has an image: one the game
 *                   gave, or the screen as the engine took it (then the only
 *                   field of an unanswered event's content). An image
 *                   in it is a real path resolved through the sandbox, readable
 *                   for the rest of the session, or an http(s) URL.
 */
#define MIGO_SHARE_SHARE_APP_MESSAGE 0U
#define MIGO_SHARE_SHARE_MESSAGE_TO_FRIEND 1U
#define MIGO_SHARE_SHOW_SHARE_IMAGE_MENU 2U
#define MIGO_SHARE_SET_SHARE_MENU 3U
#define MIGO_SHARE_MENU_SHARE_REPLY 4U
#define MIGO_SHARE_SHARE_GAME_RECORDING 5U
#define MIGO_SHARE_EVENT_MENU_SHARE 0U

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
 * Permission: content's authorize({scope}) asks the host, {"scope", "scopeId",
 * "desc"} -- the scope's name and its MIGO_SCOPE_* number, the one the decision
 * is reported with, and desc the reason the game declared, for an honest
 * prompt. A scope the platform does not define never reaches the host. Success
 * means granted; refusal is a failure with the reason "auth deny". Completing
 * the call does not record the decision: report it with
 * migo_session_set_scope_state, which is also how every gated capability is
 * checked.
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
 * Location, both gated on scope.userLocation before they reach the host.
 * GET_LOCATION {"altitude", "isHighAccuracy", "highAccuracyExpireTime"} answers
 * {"latitude", "longitude", "speed", "accuracy", "altitude", "verticalAccuracy",
 * "horizontalAccuracy"}; GET_FUZZY_LOCATION {} answers {"latitude",
 * "longitude"}, the host's coarse position. Answer in WGS-84: the engine converts
 * to GCJ-02 when content asked for it, and rounds a fuzzy position to 0.01
 * degrees itself.
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
 *   SAVE_FILE_TO_DISK {"filePath"} -> {} once saved: the platform's save dialog,
 *                  the file copied to where the player chose -- a desktop's.
 *
 * (* a file handed over.) A player who dismisses a picker or a save dialog is
 * the failure "cancel".
 */
#define MIGO_IMAGE_SAVE_IMAGE_TO_PHOTOS_ALBUM 0U
#define MIGO_IMAGE_PREVIEW_IMAGE 1U
#define MIGO_IMAGE_PREVIEW_MEDIA 2U
#define MIGO_IMAGE_COMPRESS_IMAGE 3U
#define MIGO_IMAGE_CHOOSE_IMAGE 4U
#define MIGO_IMAGE_CHOOSE_MESSAGE_FILE 5U
#define MIGO_IMAGE_CHOOSE_MEDIA 6U
#define MIGO_IMAGE_SAVE_FILE_TO_DISK 7U

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
 * RECORDING_STATE_CHANGE {"state"}. DEVICE_ORIENTATION_CHANGE {"value":
 * "portrait" | "landscape" | "landscapeReverse"} is posted whenever the screen
 * the game is shown on turns, whether content asked or not -- on a desktop,
 * the game's window crossing between taller and wider -- and needs no
 * observer.
 * SET_ENABLE_DEBUG {"enableDebug"} is content's setEnableDebug: the host shows
 * or hides its debugging aids -- the platform's vConsole, a panel with what
 * content wrote to its console (migo_session_copy_console_logs) -- and reports
 * MIGO_HOST_APP_INFO_FLAG_DEBUG to match from then on; content is told the new
 * state at once, and its console is recorded while the aids are on.
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
#define MIGO_SCREEN_SET_ENABLE_DEBUG 9U
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
 * The desktop window. SET_WINDOW_SIZE is a call {"width", "height"} (pixels) ->
 * {}. SET_CURSOR is a command: {"keyword"} -- a CSS cursor keyword, "default"
 * restoring the system cursor -- or {"path", "x", "y"}, a real path to the image
 * (ico, cur, or whatever the platform reads) and its hotspot. REQUEST_ and
 * EXIT_POINTER_LOCK are commands; report the outcome as POINTER_LOCK_CHANGE
 * {"locked"} -- a host may refuse a request that did not follow a user action,
 * and the player may release the lock -- and while locked keep reporting mouse
 * MOVE positions that accumulate the raw motion (they may leave the surface), so
 * content's movementX/movementY keep moving while the cursor stands still.
 * WINDOW_STATE_CHANGE {"state": "minimize" | "normalize" | "maximize"} reports
 * each change. A mobile host declares none of this.
 */
#define MIGO_WINDOW_SET_WINDOW_SIZE 0U
#define MIGO_WINDOW_SET_CURSOR 1U
#define MIGO_WINDOW_REQUEST_POINTER_LOCK 2U
#define MIGO_WINDOW_EXIT_POINTER_LOCK 3U
#define MIGO_WINDOW_EVENT_WINDOW_STATE_CHANGE 0U
#define MIGO_WINDOW_EVENT_POINTER_LOCK_CHANGE 1U

/*
 * The host's own ecosystem: friends and groups, cloud storage, live channels,
 * voice chat, handoff, subscriptions, privacy agreements -- features whose
 * meaning is the host's. They cross by content API name -- an object's member
 * as Class.member, the name the platform documents it by
 * ("GameServerManager.createRoom", "RankManager.onChallengeStart") -- and the
 * contract (contracts/runtime/host-services.json, service "ecosystem", "names")
 * lists every name that may arrive. A frame-sync action list that is binary
 * crosses as lower-case hex with "binary": true beside it, both ways.
 * An object content can make more than one of names the one each request is
 * for: every PageManager request carries the "pageId" of its manager, and so
 * must every "PageManager.on" event, {"pageId", "eventName", "data"} -- an
 * event the opened page sends, by a name its channel documents. "PageManager.
 * destroy" releases the page; nothing arrives for that pageId after it.
 *
 *   CALL {"api", "options"} -> the API's result, or a failure with the code the
 *        API defines (MIGO_HOST_SERVICE_RESULT_FLAG_ERROR_CODE). A path among an
 *        API's options (shareImageToGroup's imagePath, ...) is already a real path
 *        resolved through the sandbox, readable until the call completes.
 *   EVENT {"name", "data", "replyId"?} -- name is one of the contract's events
 *        (onCopyUrl, onVoIPChatStateChanged, ...). An event with a replyId is
 *        always answered by REPLY, a command {"replyId", "data", "done"}: what an
 *        onCopyUrl or onHandoff listener returned, or each resolve() of the
 *        onNeedPrivacyAuthorization listener; data null when no listener
 *        answered. The last reply for a replyId has done true; drop it then.
 *
 * The synchronous getters (getExtConfigSync, isChatTool, ...) read the values
 * reported through migo_session_set_ecosystem_value.
 */
#define MIGO_ECOSYSTEM_CALL 0U
#define MIGO_ECOSYSTEM_REPLY 1U
#define MIGO_ECOSYSTEM_EVENT_EVENT 0U

/*
 * Updates of the content package, which are the host's from start to finish:
 * it checks for a new version every time the game starts (a warm start
 * included), downloads what it finds, and restarts the game on it when content
 * asks. Each step is reported as EVENT_STATE -- {"event": "checkForUpdate",
 * "hasUpdate": true|false} for every check, then {"event": "updateReady"} or
 * {"event": "updateFailed"} for the download a positive check started -- and
 * content's UpdateManager hears it, a listener registered after the step
 * included. APPLY_UPDATE {} is content asking for the restart, which it does
 * only after updateReady.
 *
 * A host that declares this service is one that checks: content waits for its
 * report. One that does not has no update, and content is told so at once.
 */
#define MIGO_UPDATE_APPLY_UPDATE 0U
#define MIGO_UPDATE_EVENT_STATE 0U

/*
 * The camera: createCamera's preview over the game, its frames, photos, video
 * and zoom. Every request names its camera by the cameraId content gave CREATE,
 * and a call is answered once what it asked has happened. scope.camera is the
 * engine's: no request that opens or uses a camera reaches the host before it is
 * granted. Requests and answers, in order:
 *
 *   CREATE {"cameraId", "x", "y", "width", "height", "devicePosition", "flash", "size"}
 *       -> {} once the preview shows at x, y, width x height in content's
 *       coordinates (the logical pixels its touches arrive in, from the game
 *       surface's top left), covering it, over the game; devicePosition "back" or
 *       "front", flash "auto", "on" or "off", and frames of size "small",
 *       "medium" or "large" -- the reference sets no pixel counts; Android's are
 *       its camera's nearest to 320 x 240, 1280 x 720 and 1920 x 1080.
 *   DESTROY {"cameraId"}, a command: the camera closed and its preview gone; its
 *       calls still unanswered fail ("camera destroyed"). It follows a CREATE
 *       that failed too, so a camera that did not open is let go the one way
 *       every camera is.
 *   SET {"cameraId", any of "x", "y", "width", "height", "devicePosition",
 *       "flash", "size"}, a command: what content changed on it.
 *   TAKE_PHOTO {"cameraId", "quality"} -> {"tempImagePath", "width", "height"},
 *       quality "high", "normal" or "low"; the file is handed over.
 *   START_RECORD {"cameraId"} -> {} once it records.
 *   STOP_RECORD {"cameraId", "compressed"} -> {"tempThumbPath", "tempVideoPath"}
 *       once the video and its cover are written; the files are handed over.
 *   SET_ZOOM {"cameraId", "zoom"} -> {"zoom"}: the zoom the camera took, from 1
 *       to its largest, to a tenth.
 *   LISTEN_FRAME_CHANGE {"cameraId"}, a command: from now until
 *       CLOSE_FRAME_CHANGE {"cameraId"}, each frame goes to
 *       migo_session_post_camera_frame.
 *
 * What happens to a camera on its own is reported with {"cameraId"}: EVENT_STOP
 * when it stops -- the App went to the background, another App took the device
 * -- and EVENT_AUTH_CANCEL when the player refuses the system's permission,
 * which fails CREATE too.
 */
#define MIGO_CAMERA_CREATE 0U
#define MIGO_CAMERA_DESTROY 1U
#define MIGO_CAMERA_SET 2U
#define MIGO_CAMERA_TAKE_PHOTO 3U
#define MIGO_CAMERA_START_RECORD 4U
#define MIGO_CAMERA_STOP_RECORD 5U
#define MIGO_CAMERA_SET_ZOOM 6U
#define MIGO_CAMERA_LISTEN_FRAME_CHANGE 7U
#define MIGO_CAMERA_CLOSE_FRAME_CHANGE 8U
#define MIGO_CAMERA_EVENT_STOP 0U
#define MIGO_CAMERA_EVENT_AUTH_CANCEL 1U

/*
 * MIGO_HOST_SERVICE_RECORDER: getRecorderManager's microphone, one recording at
 * a time. Every method is a command -- the reference's recorder is heard through
 * events -- and scope.record is the engine's: no START reaches the host before
 * it is granted. Each request carries the recording's "recordingId", and every
 * event and frame the host reports carries it back: content drops what is not
 * of the recording it holds.
 *
 *   START {"recordingId", "duration", "sampleRate", "numberOfChannels",
 *       "encodeBitRate", "format", "frameSize"?, "audioSource", "audioMode"}:
 *       record for at most duration ms (up to 600000), format "mp3", "aac",
 *       "wav" or "PCM"; with frameSize (KB), hand over frames of about that size
 *       with migo_session_post_recorder_frame as they are recorded. A recording
 *       already under way stops first, with no STOP. Reported: EVENT_START once
 *       it records, or EVENT_ERROR.
 *   PAUSE, RESUME {"recordingId"}: EVENT_PAUSE, EVENT_RESUME once done; a
 *       request in the wrong state is ignored.
 *   STOP {"recordingId"}: EVENT_STOP once the file is written.
 *
 * Events, each {"recordingId", ...}: START, PAUSE, RESUME; STOP with
 * {"tempFilePath", "duration" (ms), "fileSize" (bytes)} -- also when the
 * duration runs out -- the file handed over as a call result's are; ERROR with
 * {"errMsg"}; INTERRUPTION_BEGIN and INTERRUPTION_END when the system takes the
 * microphone away and gives it back.
 */
#define MIGO_RECORDER_START 0U
#define MIGO_RECORDER_PAUSE 1U
#define MIGO_RECORDER_RESUME 2U
#define MIGO_RECORDER_STOP 3U
#define MIGO_RECORDER_EVENT_START 0U
#define MIGO_RECORDER_EVENT_PAUSE 1U
#define MIGO_RECORDER_EVENT_RESUME 2U
#define MIGO_RECORDER_EVENT_STOP 3U
#define MIGO_RECORDER_EVENT_ERROR 4U
#define MIGO_RECORDER_EVENT_INTERRUPTION_BEGIN 5U
#define MIGO_RECORDER_EVENT_INTERRUPTION_END 6U

/*
 * MIGO_HOST_SERVICE_VIDEO: createVideo's players, each shown where content put
 * it -- x, y, width x height in content's coordinates (the logical pixels its
 * touches arrive in, from the game surface's top left) -- over the game, or
 * under it when underGameView. Every request carries the player's "videoId",
 * which content takes from the Host id space, and every event names it back: an
 * event for a player content no longer holds is dropped. A call is answered once
 * what it asked has happened, as the reference's promises settle.
 *
 *   CREATE {"videoId", "x", "y", "width", "height", "src", "poster",
 *       "initialTime", "playbackRate", "live", "objectFit", "controls",
 *       "showProgress", "showProgressInControlMode", "backgroundColor",
 *       "autoplay", "loop", "muted", "obeyMuteSwitch", "enableProgressGesture",
 *       "enablePlayGesture", "showCenterPlayBtn", "underGameView",
 *       "autoPauseIfNavigate", "autoPauseIfOpenNative"}, a command: src and
 *       poster are a file the host can open or a URL the engine allowed.
 *   DESTROY {"videoId"}, a command: the player gone; its calls still
 *       unanswered fail ("video destroyed").
 *   SET {"videoId", any of CREATE's options}, a command: what content changed.
 *   PLAY, PAUSE, STOP {"videoId"} -> {} once playing, paused, or stopped back
 *       at the start.
 *   SEEK {"videoId", "time"} -> {} once at time seconds.
 *   REQUEST_FULL_SCREEN {"videoId", "direction"} -> {} once it fills the
 *       screen, direction 0, 90 (counterclockwise) or -90 (clockwise).
 *   EXIT_FULL_SCREEN {"videoId"} -> {} once back where content put it.
 *
 * Events, each {"videoId", ...}: PLAY, PAUSE, ENDED, WAITING; TIME_UPDATE with
 * {"position", "duration"} in seconds, several a second while it plays; ERROR
 * with {"errMsg"}; PROGRESS with {"buffered" (percent), "duration"}. TIME_UPDATE
 * and PROGRESS are readings the next one supersedes: refused with
 * MIGO_ERROR_WOULD_BLOCK, and dropped, when content falls behind.
 */
#define MIGO_VIDEO_CREATE 0U
#define MIGO_VIDEO_DESTROY 1U
#define MIGO_VIDEO_SET 2U
#define MIGO_VIDEO_PLAY 3U
#define MIGO_VIDEO_PAUSE 4U
#define MIGO_VIDEO_STOP 5U
#define MIGO_VIDEO_SEEK 6U
#define MIGO_VIDEO_REQUEST_FULL_SCREEN 7U
#define MIGO_VIDEO_EXIT_FULL_SCREEN 8U
#define MIGO_VIDEO_EVENT_PLAY 0U
#define MIGO_VIDEO_EVENT_PAUSE 1U
#define MIGO_VIDEO_EVENT_ENDED 2U
#define MIGO_VIDEO_EVENT_TIME_UPDATE 3U
#define MIGO_VIDEO_EVENT_ERROR 4U
#define MIGO_VIDEO_EVENT_WAITING 5U
#define MIGO_VIDEO_EVENT_PROGRESS 6U

/*
 * MIGO_HOST_SERVICE_BLE_PERIPHERAL: createBLEPeripheralServer's GATT servers --
 * this device as a Bluetooth LE peripheral. Every request carries the server's
 * "serverId", which content takes from the Host id space, and every event names
 * it back. Every method is a call, answered once what it asked has happened, or
 * with {"error", "errCode"} in the central role's Bluetooth codes (10000 the
 * adapter is not open, 10001 Bluetooth is not available, ...). Binary values
 * cross as lower-case hex. The host closes a session's servers with it.
 *
 *   CREATE_SERVER {"serverId"} -> {} once it serves.
 *   ADD_SERVICE {"serverId", "service": {"uuid", "characteristics": [{"uuid",
 *       "properties": {"write", "writeNoResponse", "read", "notify",
 *       "indicate"}, "permission": {"readable", "writeable",
 *       "readEncryptionRequired", "writeEncryptionRequired"}, "value",
 *       "descriptors": [{"uuid", "permission": {"write", "read"},
 *       "value"}]}]}} -> {} once served; every flag is present.
 *   REMOVE_SERVICE {"serverId", "serviceId"} -> {} once removed.
 *   START_ADVERTISING {"serverId", "advertiseRequest": {"connectable",
 *       "deviceName"?, "serviceUuids"?, "manufacturerData"?:
 *       [{"manufacturerId" ("0x" and hex), "manufacturerSpecificData"}],
 *       "beacon"?: {"uuid", "major", "minor", "measuredPower"?}}, "powerLevel"
 *       ("low", "medium", "high")} -> {} once advertising; it replaces what
 *       this server advertised. A beacon comes without the other three.
 *   STOP_ADVERTISING {"serverId"} -> {} once stopped.
 *   WRITE_CHARACTERISTIC_VALUE {"serverId", "serviceId", "characteristicId",
 *       "value", "needNotify", "callbackId"?} -> {} once set, and, with
 *       needNotify, sent to the centrals subscribed to it; with callbackId it
 *       is also the response to that read or write request, if one waits.
 *
 * Events, each {"serverId", ...}: CHARACTERISTIC_READ_REQUEST with
 * {"serviceId", "characteristicId", "callbackId"} and
 * CHARACTERISTIC_WRITE_REQUEST with those and "value" -- callbackId a number
 * the host makes, which content answers with WRITE_CHARACTERISTIC_VALUE. A
 * request content does not answer gets no response, as the reference says; the
 * host itself answers only what content never sees: the rest of a long read,
 * from the value content gave; a long write's parts until it is executed, then
 * reported whole; descriptors, from their values. CHARACTERISTIC_SUBSCRIBED
 * and CHARACTERISTIC_UNSUBSCRIBED with {"serviceId", "characteristicId"} as a
 * central turns its notifications or indications on or off.
 * CONNECTION_STATE_CHANGED with {"deviceId", "connected"} as a central
 * connects or disconnects.
 */
#define MIGO_BLE_PERIPHERAL_CREATE_SERVER 0U
#define MIGO_BLE_PERIPHERAL_ADD_SERVICE 1U
#define MIGO_BLE_PERIPHERAL_REMOVE_SERVICE 2U
#define MIGO_BLE_PERIPHERAL_START_ADVERTISING 3U
#define MIGO_BLE_PERIPHERAL_STOP_ADVERTISING 4U
#define MIGO_BLE_PERIPHERAL_WRITE_CHARACTERISTIC_VALUE 5U
#define MIGO_BLE_PERIPHERAL_EVENT_CHARACTERISTIC_READ_REQUEST 0U
#define MIGO_BLE_PERIPHERAL_EVENT_CHARACTERISTIC_WRITE_REQUEST 1U
#define MIGO_BLE_PERIPHERAL_EVENT_CHARACTERISTIC_SUBSCRIBED 2U
#define MIGO_BLE_PERIPHERAL_EVENT_CHARACTERISTIC_UNSUBSCRIBED 3U
#define MIGO_BLE_PERIPHERAL_EVENT_CONNECTION_STATE_CHANGED 4U

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

/*
 * What the host app says about itself: what getAppBaseInfo() and
 * getDeviceBenchmarkInfo() describe beyond what the engine knows. The engine
 * knows the rest -- the API level it implements (SDKVersion), its own version,
 * the content's id -- and a host that reports nothing is answered with
 * "unknown" where the platform has a word for it and the standard where it has
 * one, never with a value made up to look plausible.
 *
 * app_id_utf8   the host app's own id (bundle identifier, package name):
 *               getAppBaseInfo().host.appId. NULL or empty for none, and then
 *               host has no appId. At most MIGO_HOST_APP_ID_MAX_BYTES.
 * language_utf8 the host app's language, a BCP 47 tag ("zh-Hans-CN", "en-US");
 *               content is told "zh_CN", "en_US". NULL or empty for the
 *               system's, which the engine reads itself on macOS, iOS,
 *               Windows and Linux. At most MIGO_LANGUAGE_TAG_MAX_BYTES.
 * font_scale    the host app's text scale, 1 standard (fontSizeScaleFactor;
 *               fontSizeSetting is 16 px times it). 0 for not reported, which
 *               is 1. At most MIGO_FONT_SCALE_MAX.
 * benchmark_level  the host's own score for the device, -1 for unknown, 1 and
 *               up higher faster (-2 and 0: the device cannot run games).
 * model_level   MIGO_DEVICE_MODEL_LEVEL_*.
 * flags         MIGO_HOST_APP_INFO_FLAG_DEBUG: the host's debugging aids are
 *               on (getAppBaseInfo().enableDebug).
 *
 * A zeroed record but for benchmark_level = -1 says nothing.
 */
#define MIGO_HOST_APP_INFO_FLAG_DEBUG (1U << 0)

#define MIGO_DEVICE_MODEL_LEVEL_UNKNOWN 0U
#define MIGO_DEVICE_MODEL_LEVEL_HIGH 1U
#define MIGO_DEVICE_MODEL_LEVEL_MEDIUM 2U
#define MIGO_DEVICE_MODEL_LEVEL_LOW 3U

#define MIGO_HOST_APP_ID_MAX_BYTES 256U
#define MIGO_LANGUAGE_TAG_MAX_BYTES 64U
#define MIGO_FONT_SCALE_MAX 8.0f

typedef struct MigoHostAppInfo {
    uint32_t struct_size;
    uint32_t abi_version;
    const char *app_id_utf8;
    const char *language_utf8;
    float font_scale;
    int32_t benchmark_level;
    uint32_t model_level;
    uint32_t flags;
} MigoHostAppInfo;

MIGO_STATIC_ASSERT(offsetof(MigoHostAppInfo, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
#if MIGO_LP64
MIGO_STATIC_ASSERT(sizeof(MigoHostAppInfo) == 40, "MigoHostAppInfo LP64 size changed");
MIGO_STATIC_ASSERT(offsetof(MigoHostAppInfo, font_scale) == 24, "MigoHostAppInfo.font_scale moved");
#endif

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

/* How a frame's four-byte pixels are laid out. */
typedef uint32_t MigoPixelFormat;
#define MIGO_PIXEL_FORMAT_RGBA8 0U
#define MIGO_PIXEL_FORMAT_BGRA8 1U

/* One frame of a camera content listens to (MIGO_CAMERA_LISTEN_FRAME_CHANGE):
 * width x height pixels of format, rows top first, row_stride bytes apart (at
 * least width * 4). Alpha is not read: content's frames are opaque. */
typedef struct MigoCameraFrame {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t camera_id;
    uint32_t width;
    uint32_t height;
    uint32_t row_stride;
    MigoPixelFormat format;
    uint32_t reserved0;
    const uint8_t *pixels;
} MigoCameraFrame;

MIGO_STATIC_ASSERT(offsetof(MigoCameraFrame, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(offsetof(MigoCameraFrame, pixels) == 32, "MigoCameraFrame layout changed");

/* One frame of a recording whose START asked for frames: length bytes of the
 * recording's format (at most 1 MiB); is_last_frame 1 for the frame that ends
 * the recording, which may be empty, else 0. */
typedef struct MigoRecorderFrame {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t recording_id;
    uint32_t is_last_frame;
    const uint8_t *data;
    uint32_t length;
    uint32_t reserved0;
} MigoRecorderFrame;

MIGO_STATIC_ASSERT(offsetof(MigoRecorderFrame, struct_size) == 0,
                   "every versioned struct must begin with struct_size");
MIGO_STATIC_ASSERT(offsetof(MigoRecorderFrame, data) == 16, "MigoRecorderFrame layout changed");

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
 * when the event was accepted or there is no running content to deliver it to;
 * MIGO_ERROR_WOULD_BLOCK when the event is a reading the next one supersedes
 * (MIGO_VIDEO_EVENT_TIME_UPDATE, MIGO_VIDEO_EVENT_PROGRESS) and content has
 * fallen behind -- it is dropped; any other event always arrives; and
 * MIGO_ERROR_INVALID_ARGUMENT for a service the host did not declare, an event
 * that service does not define, or a payload that is not a JSON object.
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
 * Report what the host app says about itself (MigoHostAppInfo), from any
 * thread, at start and whenever it changes -- the player changing the text
 * size, say. Each report replaces the last; strings are copied. Returns
 * MIGO_ERROR_INVALID_ARGUMENT for a NULL or malformed record, a string that is
 * not UTF-8 or longer than its bound, a language that does not begin with a
 * language subtag, a font_scale that is negative, not finite or past
 * MIGO_FONT_SCALE_MAX, a benchmark_level below -2, a model_level or a flag this
 * header does not define. May be called before a Surface is attached; the
 * report is kept.
 */
MIGO_API MigoResult MIGO_CALL migo_session_set_host_app_info(MigoSession *session,
                                                             const MigoHostAppInfo *info);

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

/*
 * Deliver one frame of a camera content listens to, from any thread, while a
 * Surface is attached. Typed because a camera sends thirty a second: the pixels
 * are read into content's RGBA as the call copies them, so the record is only
 * borrowed for the call, and only the latest frame waits -- one that arrives
 * before content took the last replaces it. Returns MIGO_ERROR_INVALID_ARGUMENT
 * for a NULL or malformed record, a format this header does not define, a width
 * or height of 0 or over 8192, a row_stride below width * 4, a frame whose RGBA
 * is over 64 MiB, or when the host did not declare MIGO_HOST_SERVICE_CAMERA;
 * MIGO_ERROR_INVALID_STATE with no Surface attached.
 */
MIGO_API MigoResult MIGO_CALL migo_session_post_camera_frame(MigoSession *session,
                                                             const MigoCameraFrame *frame);

/*
 * Deliver one frame of a recording, from any thread, while a Surface is
 * attached. Typed because a microphone sends many a second: the bytes are copied
 * as the call reads them, so the record is only borrowed for the call. Frames
 * share a byte budget content drains; a frame over it is refused with
 * MIGO_ERROR_WOULD_BLOCK and dropped, the last frame never is. Returns
 * MIGO_ERROR_INVALID_ARGUMENT for a NULL or malformed record, a recording_id of
 * 0, an is_last_frame other than 0 or 1, a length over 1 MiB or data NULL with
 * a length, or when the host did not declare MIGO_HOST_SERVICE_RECORDER;
 * MIGO_ERROR_INVALID_STATE with no Surface attached.
 */
MIGO_API MigoResult MIGO_CALL migo_session_post_recorder_frame(MigoSession *session,
                                                               const MigoRecorderFrame *frame);

/*
 * Report the value a synchronous ecosystem getter answers with -- name is one of
 * the contract's "values" (getExtConfigSync, isChatTool, ...), json what the API
 * returns -- from any thread, whenever it changes. An empty json withdraws it,
 * and the getter answers as for a host with none. Returns
 * MIGO_ERROR_INVALID_ARGUMENT for a name that is not an API name (letters, or
 * an object's member as Class.member; at most 64), json that is not JSON or larger than
 * MIGO_HOST_SERVICE_PAYLOAD_MAX_BYTES, or when the host did not declare
 * MIGO_HOST_SERVICE_ECOSYSTEM. May be called before a Surface is attached.
 */
MIGO_API MigoResult MIGO_CALL migo_session_set_ecosystem_value(MigoSession *session,
                                                               const char *name_utf8,
                                                               uint32_t name_length,
                                                               const char *json_utf8,
                                                               uint32_t json_length);

MIGO_END_DECLS

#endif /* MIGO_HOST_SERVICES_H_ */
