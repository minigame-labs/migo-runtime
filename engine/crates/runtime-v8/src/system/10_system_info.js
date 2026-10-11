import { op_get_window_info, op_get_device_info, op_get_system_settings, op_get_app_authorization_setting } from "ext:core/ops";
import { promisify } from "ext:host_v8_base/02_async.js";
import { appInfo } from "ext:host_v8_system/07_app_info.js";

function authToBool(val) {
    if (typeof val === 'boolean') return val;
    return val === 'authorized';
}

function getSystemInfoSync() {
    var result = {
        brand: '',
        model: 'unknown',
        pixelRatio: 1,
        // Alias of pixelRatio. The common mini-game platform's getSystemInfoSync returns BOTH
        // `pixelRatio` and `devicePixelRatio`; browser-style engines
        // (Cocos/Laya/...) read `devicePixelRatio` to size the canvas
        // backing store (`canvas.width = screenWidth * devicePixelRatio`).
        // Omitting it makes those engines fall back to dpr=1, rendering
        // the whole scene into 1/dpr of the surface (horizontal squish).
        devicePixelRatio: 1,
        screenWidth: 0,
        screenHeight: 0,
        windowWidth: 0,
        windowHeight: 0,
        statusBarHeight: 0,
        system: '',
        platform: 'android',
        albumAuthorized: false,
        cameraAuthorized: false,
        locationAuthorized: false,
        microphoneAuthorized: false,
        notificationAuthorized: false,
        notificationAlertAuthorized: false,
        notificationBadgeAuthorized: false,
        notificationSoundAuthorized: false,
        phoneCalendarAuthorized: false,
        bluetoothEnabled: false,
        locationEnabled: false,
        wifiEnabled: false,
        safeArea: { left: 0, right: 0, top: 0, bottom: 0, width: 0, height: 0 },
        locationReducedAccuracy: false,
        deviceOrientation: 'portrait',
        errMsg: 'getSystemInfo:ok'
    };

    // The same facts getAppBaseInfo and getDeviceBenchmarkInfo answer with, from
    // the same place, so no two APIs disagree about them.
    var app = appInfo();
    result.language = app.language;
    result.version = app.version;
    result.SDKVersion = app.SDKVersion;
    result.fontSizeSetting = app.fontSizeSetting;
    result.benchmarkLevel = app.benchmarkLevel;
    result.host = app.hostAppId === null ? {} : { appId: app.hostAppId };
    result.enableDebug = app.enableDebug;

    try {
        var deviceJson = op_get_device_info();
        var device = JSON.parse(deviceJson);
        result.brand = device.brand || '';
        result.model = device.model || 'unknown';
        result.system = device.system || '';
        result.platform = device.platform || 'android';
    } catch (e) { /* use defaults */ }

    try {
        var winJson = op_get_window_info();
        var win = JSON.parse(winJson);
        result.pixelRatio = win.pixel_ratio || 1;
        result.devicePixelRatio = result.pixelRatio;
        result.screenWidth = win.screen_width || 0;
        result.screenHeight = win.screen_height || 0;
        result.windowWidth = win.window_width || 0;
        result.windowHeight = win.window_height || 0;
        result.statusBarHeight = win.status_bar_height || 0;
        if (win.safe_area) {
            var safeLeft = win.safe_area.left || 0;
            var safeTop = win.safe_area.top || 0;
            var safeRight = (win.screen_width || 0) - (win.safe_area.right || 0);
            var safeBottom = (win.screen_height || 0) - (win.safe_area.bottom || 0);
            result.safeArea = {
                left: safeLeft,
                top: safeTop,
                right: safeRight,
                bottom: safeBottom,
                width: safeRight - safeLeft,
                height: safeBottom - safeTop
            };
        }
    } catch (e) { /* use defaults */ }

    try {
        var settingsJson = op_get_system_settings();
        var settings = JSON.parse(settingsJson);
        result.bluetoothEnabled = !!settings.bluetooth_enabled;
        result.locationEnabled = !!settings.location_enabled;
        result.wifiEnabled = !!settings.wifi_enabled;
        result.deviceOrientation = settings.orientation || 'portrait';
    } catch (e) { /* use defaults */ }

    try {
        var authJson = op_get_app_authorization_setting();
        var auth = JSON.parse(authJson);
        result.albumAuthorized = authToBool(auth.albumAuthorized);
        result.cameraAuthorized = authToBool(auth.cameraAuthorized);
        result.locationAuthorized = authToBool(auth.locationAuthorized);
        result.microphoneAuthorized = authToBool(auth.microphoneAuthorized);
        result.notificationAuthorized = authToBool(auth.notificationAuthorized);
        result.notificationAlertAuthorized = authToBool(auth.notificationAlertAuthorized);
        result.notificationBadgeAuthorized = authToBool(auth.notificationBadgeAuthorized);
        result.notificationSoundAuthorized = authToBool(auth.notificationSoundAuthorized);
        result.phoneCalendarAuthorized = authToBool(auth.phoneCalendarAuthorized);
        if (auth.locationReducedAccuracy !== undefined) {
            result.locationReducedAccuracy = !!auth.locationReducedAccuracy;
        }
    } catch (e) { /* use defaults */ }

    return result;
}

var getSystemInfo = promisify('getSystemInfo', function () { return getSystemInfoSync(); });
var getSystemInfoAsync = promisify('getSystemInfoAsync', function () { return getSystemInfoSync(); });

export { getSystemInfo, getSystemInfoSync, getSystemInfoAsync };
