// getAppBaseInfo, getAccountInfoSync and getDeviceBenchmarkInfo.
//
// What they answer is composed natively (shared::services::app_info): what the
// engine knows of itself -- the API level it implements, its own version, the
// content's id -- what the host reports about itself and the content it runs,
// and the system's language when the host names none. A host that says nothing
// is told "unknown" where the platform has a word for it and the standard where
// it has one; nothing here is a constant standing in for a fact.

import { op_get_app_info } from "ext:core/ops";
import { wrapAsync } from "ext:host_v8_base/02_async.js";

/** The composed facts, for the APIs here and getSystemInfoSync's copies of them. */
function appInfo() {
    return op_get_app_info();
}

function getAppBaseInfo() {
    const info = op_get_app_info();
    // `theme` is left out: the platform documents it as unavailable to mini
    // games. `host.appId` is present only when the host names itself, and
    // `PCKernelVersion` only on a desktop, as the platform has it.
    const base = {
        SDKVersion: info.SDKVersion,
        enableDebug: info.enableDebug,
        host: info.hostAppId === null ? {} : { appId: info.hostAppId },
        language: info.language,
        version: info.version,
        fontSizeScaleFactor: info.fontSizeScaleFactor,
        fontSizeSetting: info.fontSizeSetting,
    };
    if (info.PCKernelVersion !== undefined) base.PCKernelVersion = info.PCKernelVersion;
    return base;
}

function getAccountInfoSync() {
    const info = op_get_app_info();
    // `plugin` is the platform's only for a call made inside a plugin, which
    // content here never is.
    return {
        miniProgram: {
            appId: info.appId,
            envVersion: info.envVersion,
            version: info.contentVersion,
        },
    };
}

// Answered as every asynchronous call is; the platform offers it no Promise
// style, so its Promise's rejection is not reported as unhandled.
function getDeviceBenchmarkInfo(options) {
    return wrapAsync('getDeviceBenchmarkInfo', function () {
        const info = op_get_app_info();
        return { benchmarkLevel: info.benchmarkLevel, modelLevel: info.modelLevel };
    }, options);
}

export {
    appInfo,
    getAppBaseInfo,
    getAccountInfoSync,
    getDeviceBenchmarkInfo,
};
