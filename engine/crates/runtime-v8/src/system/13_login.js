import {
    op_login,
    op_check_session,
    op_get_user_info,
    op_get_phone_number,
} from "ext:core/ops";
import { allocateHostCallbackId, parseHostCallbackId, invokeCallback, failMessage } from "ext:host_v8_base/02_async.js";

const noop = () => {};

const _pendingLogin = new Map();
const _pendingCheckSession = new Map();
const _pendingUserInfo = new Map();
const _pendingPhoneNumber = new Map();

function _safeErrorMessage(error) {
    if (!error) return "unknown error";
    if (typeof error === "string") return error;
    if (typeof error.message === "string" && error.message.length > 0) {
        return error.message;
    }
    try {
        return String(error);
    } catch (_) {
        return "unknown error";
    }
}

function _parseResultJson(resultJson) {
    if (typeof resultJson !== "string" || resultJson.length === 0) {
        return {};
    }
    try {
        const parsed = JSON.parse(resultJson);
        return parsed && typeof parsed === "object" ? parsed : {};
    } catch (_) {
        return {};
    }
}

function _toErrMsg(apiName, error) {
    const text = typeof error === "string" ? error.trim() : "";
    if (!text) {
        return `${apiName}:fail unknown error`;
    }
    if (text.startsWith(`${apiName}:fail`)) {
        return text;
    }
    return `${apiName}:fail ${text}`;
}

function _normalizeLang(value) {
    if (value === "zh_CN" || value === "zh_TW" || value === "en") {
        return value;
    }
    return "en";
}

function login(options = {}) {
    const opts = options && typeof options === "object" ? options : {};
    const success = typeof opts.success === "function" ? opts.success : noop;
    const fail = typeof opts.fail === "function" ? opts.fail : noop;
    const complete = typeof opts.complete === "function" ? opts.complete : noop;

    const timeout = Number.isFinite(opts.timeout) && opts.timeout > 0
        ? Math.floor(opts.timeout)
        : undefined;

    // Returns a Promise as well as taking callbacks, like every other
    // API on this surface -- and like `checkSession` and `getUserProfile`
    // in this very file. Without one, `await migo.login()` resolved
    // immediately on `undefined` and content carried on before the call
    // had finished. Settlement is folded into the stored callbacks, which
    // is `getUserProfile`'s shape here and leaves the settle functions
    // untouched.
    return new Promise(function (resolve, reject) {
        let requestId;
        try {
            requestId = allocateHostCallbackId();
            _pendingLogin.set(requestId, {
                success: function (res) {
                    invokeCallback("login", "success", success, res);
                    resolve(res);
                },
                fail: function (res) {
                    invokeCallback("login", "fail", fail, res);
                    reject(res);
                },
                complete,
            });
            op_login(JSON.stringify({ requestId, timeout }));
        } catch (error) {
            _pendingLogin.delete(requestId);
            const res = { errMsg: failMessage('login', _safeErrorMessage(error)) };
            queueMicrotask(function () {
                invokeCallback("login", "fail", fail, res);
                invokeCallback("login", "complete", complete, res);
                reject(res);
            });
        }
    });
}

function checkSession(options = {}) {
    const opts = options && typeof options === "object" ? options : {};
    const success = typeof opts.success === "function" ? opts.success : noop;
    const fail = typeof opts.fail === "function" ? opts.fail : noop;
    const complete = typeof opts.complete === "function" ? opts.complete : noop;

    return new Promise(function (resolve, reject) {
        let requestId;
        try {
            requestId = allocateHostCallbackId();
            _pendingCheckSession.set(requestId, {
                success,
                fail,
                complete,
                resolve,
                reject,
            });
            op_check_session(JSON.stringify({ requestId }));
        } catch (error) {
            _pendingCheckSession.delete(requestId);
            const res = { errMsg: failMessage('checkSession', _safeErrorMessage(error)) };
            queueMicrotask(function () {
                fail(res);
                complete(res);
                reject(res);
            });
        }
    });
}

function getUserInfo(options = {}) {
    const opts = options && typeof options === "object" ? options : {};
    const success = typeof opts.success === "function" ? opts.success : noop;
    const fail = typeof opts.fail === "function" ? opts.fail : noop;
    const complete = typeof opts.complete === "function" ? opts.complete : noop;

    const withCredentials = opts.withCredentials === true;
    const lang = _normalizeLang(opts.lang);

    // Returns a Promise as well as taking callbacks, like every other
    // API on this surface -- and like `checkSession` and `getUserProfile`
    // in this very file. Without one, `await migo.getUserInfo()` resolved
    // immediately on `undefined` and content carried on before the call
    // had finished. Settlement is folded into the stored callbacks, which
    // is `getUserProfile`'s shape here and leaves the settle functions
    // untouched.
    return new Promise(function (resolve, reject) {
        let requestId;
        try {
            requestId = allocateHostCallbackId();
            _pendingUserInfo.set(requestId, {
                success: function (res) {
                    invokeCallback("getUserInfo", "success", success, res);
                    resolve(res);
                },
                fail: function (res) {
                    invokeCallback("getUserInfo", "fail", fail, res);
                    reject(res);
                },
                complete,
            });
            op_get_user_info(JSON.stringify({ requestId, withCredentials, lang }));
        } catch (error) {
            _pendingUserInfo.delete(requestId);
            const res = { errMsg: `getUserInfo:fail ${_safeErrorMessage(error)}` };
            queueMicrotask(function () {
                invokeCallback("getUserInfo", "fail", fail, res);
                invokeCallback("getUserInfo", "complete", complete, res);
                reject(res);
            });
        }
    });
}

function getPhoneNumber(options = {}) {
    const opts = options && typeof options === "object" ? options : {};
    const success = typeof opts.success === "function" ? opts.success : noop;
    const fail = typeof opts.fail === "function" ? opts.fail : noop;
    const complete = typeof opts.complete === "function" ? opts.complete : noop;

    const isRealtime = opts.isRealtime === true;
    const phoneNumberNoQuotaToast = opts.phoneNumberNoQuotaToast !== false;

    // Returns a Promise as well as taking callbacks, like every other
    // API on this surface -- and like `checkSession` and `getUserProfile`
    // in this very file. Without one, `await migo.getPhoneNumber()` resolved
    // immediately on `undefined` and content carried on before the call
    // had finished. Settlement is folded into the stored callbacks, which
    // is `getUserProfile`'s shape here and leaves the settle functions
    // untouched.
    return new Promise(function (resolve, reject) {
        let requestId;
        try {
            requestId = allocateHostCallbackId();
            _pendingPhoneNumber.set(requestId, {
                success: function (res) {
                    invokeCallback("getPhoneNumber", "success", success, res);
                    resolve(res);
                },
                fail: function (res) {
                    invokeCallback("getPhoneNumber", "fail", fail, res);
                    reject(res);
                },
                complete,
            });
            op_get_phone_number(JSON.stringify({
                requestId,
                isRealtime,
                phoneNumberNoQuotaToast,
            }));
        } catch (error) {
            _pendingPhoneNumber.delete(requestId);
            const res = { errMsg: `getPhoneNumber:fail ${_safeErrorMessage(error)}` };
            queueMicrotask(function () {
                invokeCallback("getPhoneNumber", "fail", fail, res);
                invokeCallback("getPhoneNumber", "complete", complete, res);
                reject(res);
            });
        }
    });
}

function _internalOnLoginResult(resultJson) {
    const result = _parseResultJson(resultJson);
    const requestId = parseHostCallbackId(result.requestId);
    if (requestId === null) {
        return;
    }

    const pending = _pendingLogin.get(requestId);
    if (!pending) {
        return;
    }
    _pendingLogin.delete(requestId);

    if (typeof result.error === "string" && result.error.length > 0) {
        const res = { errMsg: _toErrMsg("login", result.error) };
        if (result.errno !== undefined) {
            res.errno = result.errno;
        }
        invokeCallback('login', 'fail', pending.fail, res);
        invokeCallback('login', 'complete', pending.complete, res);
        return;
    }

    if (typeof result.code !== "string" || result.code.length === 0) {
        const res = { errMsg: "login:fail invalid code" };
        invokeCallback('login', 'fail', pending.fail, res);
        invokeCallback('login', 'complete', pending.complete, res);
        return;
    }

    const res = {
        errMsg: "login:ok",
        code: result.code,
    };
    invokeCallback('login', 'success', pending.success, res);
    invokeCallback('login', 'complete', pending.complete, res);
}

function _internalOnCheckSessionResult(resultJson) {
    const result = _parseResultJson(resultJson);
    const requestId = parseHostCallbackId(result.requestId);
    if (requestId === null) {
        return;
    }

    const pending = _pendingCheckSession.get(requestId);
    if (!pending) {
        return;
    }
    _pendingCheckSession.delete(requestId);

    if (typeof result.error === "string" && result.error.length > 0) {
        const res = { errMsg: _toErrMsg("checkSession", result.error) };
        if (result.errno !== undefined) {
            res.errno = result.errno;
        }
        invokeCallback('checkSession', 'fail', pending.fail, res);
        invokeCallback('checkSession', 'complete', pending.complete, res);
        pending.reject(res);
        return;
    }

    const res = { errMsg: "checkSession:ok" };
    invokeCallback('checkSession', 'success', pending.success, res);
    invokeCallback('checkSession', 'complete', pending.complete, res);
    pending.resolve(res);
}

function _internalOnGetUserInfoResult(resultJson) {
    const result = _parseResultJson(resultJson);
    const requestId = parseHostCallbackId(result.requestId);
    if (requestId === null) {
        return;
    }

    const pending = _pendingUserInfo.get(requestId);
    if (!pending) {
        return;
    }
    _pendingUserInfo.delete(requestId);

    if (typeof result.error === "string" && result.error.length > 0) {
        const res = { errMsg: _toErrMsg("getUserInfo", result.error) };
        invokeCallback('getUserInfo', 'fail', pending.fail, res);
        invokeCallback('getUserInfo', 'complete', pending.complete, res);
        return;
    }

    if (!result.userInfo || typeof result.userInfo !== "object") {
        const res = { errMsg: "getUserInfo:fail invalid userInfo" };
        invokeCallback('getUserInfo', 'fail', pending.fail, res);
        invokeCallback('getUserInfo', 'complete', pending.complete, res);
        return;
    }

    const res = {
        errMsg: "getUserInfo:ok",
        userInfo: result.userInfo,
    };

    if (typeof result.rawData === "string") {
        res.rawData = result.rawData;
    }
    if (typeof result.signature === "string") {
        res.signature = result.signature;
    }
    if (typeof result.encryptedData === "string") {
        res.encryptedData = result.encryptedData;
    }
    if (typeof result.iv === "string") {
        res.iv = result.iv;
    }
    if (typeof result.cloudID === "string") {
        res.cloudID = result.cloudID;
    }

    invokeCallback('getUserInfo', 'success', pending.success, res);
    invokeCallback('getUserInfo', 'complete', pending.complete, res);
}

function _internalOnGetPhoneNumberResult(resultJson) {
    const result = _parseResultJson(resultJson);
    const requestId = parseHostCallbackId(result.requestId);
    if (requestId === null) {
        return;
    }

    const pending = _pendingPhoneNumber.get(requestId);
    if (!pending) {
        return;
    }
    _pendingPhoneNumber.delete(requestId);

    if (typeof result.error === "string" && result.error.length > 0) {
        const res = { errMsg: _toErrMsg("getPhoneNumber", result.error) };
        if (result.errno !== undefined) {
            res.errno = result.errno;
        }
        invokeCallback('getPhoneNumber', 'fail', pending.fail, res);
        invokeCallback('getPhoneNumber', 'complete', pending.complete, res);
        return;
    }

    if (typeof result.code !== "string" || result.code.length === 0) {
        const res = { errMsg: "getPhoneNumber:fail invalid code" };
        invokeCallback('getPhoneNumber', 'fail', pending.fail, res);
        invokeCallback('getPhoneNumber', 'complete', pending.complete, res);
        return;
    }

    const res = {
        errMsg: "getPhoneNumber:ok",
        code: result.code,
    };
    invokeCallback('getPhoneNumber', 'success', pending.success, res);
    invokeCallback('getPhoneNumber', 'complete', pending.complete, res);
}

// getUserProfile - deprecated but still used by some games.
// Delegates to getUserInfo internally.
function getUserProfile(options = {}) {
    const opts = options && typeof options === "object" ? options : {};
    const success = typeof opts.success === "function" ? opts.success : noop;
    const fail = typeof opts.fail === "function" ? opts.fail : noop;
    const complete = typeof opts.complete === "function" ? opts.complete : noop;

    const lang = _normalizeLang(opts.lang);
    const desc = typeof opts.desc === "string" ? opts.desc : "";

    return new Promise(function (resolve, reject) {
        let requestId;
        try {
            requestId = allocateHostCallbackId();
            _pendingUserInfo.set(requestId, {
                // The caller's callback runs first, but it must not decide
                // whether this promise settles: a `success` that threw here
                // skipped `resolve` and left `await getUserProfile()` pending
                // forever.
                success: function (res) {
                    invokeCallback('getUserProfile', 'success', success, res);
                    resolve(res);
                },
                fail: function (res) {
                    invokeCallback('getUserProfile', 'fail', fail, res);
                    reject(res);
                },
                complete,
            });
            op_get_user_info(JSON.stringify({
                requestId,
                withCredentials: false,
                lang,
                desc,
            }));
        } catch (error) {
            _pendingUserInfo.delete(requestId);
            const res = { errMsg: failMessage('getUserProfile', _safeErrorMessage(error)) };
            queueMicrotask(function () {
                fail(res);
                complete(res);
                reject(res);
            });
        }
    });
}

export {
    login,
    checkSession,
    getUserInfo,
    getUserProfile,
    getPhoneNumber,
    _internalOnLoginResult,
    _internalOnCheckSessionResult,
    _internalOnGetUserInfoResult,
    _internalOnGetPhoneNumberResult,
};
