import { wrapAsync } from "ext:host_v8_base/02_async.js";

// Cached appId -- set by host via _internalSetAppId(id) at startup.
var _appId = "";

function _internalSetAppId(appId) {
    if (typeof appId === "string") _appId = appId;
}

function getAppBaseInfo() {
    const appInfo = {
        SDKVersion: "4.0.0",
        enableDebug: false,
        host: {
            appId: "com.minigame.host",
        },
        language: "zh_CN",
        version: "1.0.0",
        theme: "light",
        fontSizeScaleFactor: 1.0,
        fontSizeSetting: 16
    };

    return appInfo;
}

function getAccountInfoSync() {
    return {
        miniProgram: {
            appId: _appId,
            envVersion: "release",
            version: "1.0.0",
        },
        plugin: {},
    };
}

function checkIsAddedToMyMiniProgram(options) {
    return wrapAsync('checkIsAddedToMyMiniProgram', function () {
        return { added: false };
    }, options);
}

export {
    getAppBaseInfo,
    getAccountInfoSync,
    checkIsAddedToMyMiniProgram,
    _internalSetAppId,
};
