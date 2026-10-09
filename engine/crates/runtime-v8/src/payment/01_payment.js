// checkIsSupportMidasPayment / requestMidasPayment /
// requestMidasPaymentGameItem
//
// checkIsSupportMidasPayment: Mode B (sync, returns host JSON).
// requestMidasPayment/requestMidasPaymentGameItem: Mode C (async,
// host payment flow, result via EvalScript callback).
//
// Uses Map-based request tracking (same pattern as 13_login.js) to
// support concurrent payment requests without callback overwrite.

import {
    op_check_is_support_midas_payment,
    op_request_midas_payment,
    op_request_midas_payment_game_item,
} from "ext:core/ops";
import { wrapAsync } from "ext:host_v8_base/02_async.js";

import {
    allocateHostCallbackId,
    parseHostCallbackId,
    invokeCallback,
    failMessage,
    createSettlement,
} from "ext:host_v8_base/02_async.js";

const noop = function () {};

const _pendingMidas = new Map();
const _pendingMidasGameItem = new Map();

function _parseResult(resultJson) {
    if (typeof resultJson !== 'string' || resultJson.length === 0) return {};
    try {
        var p = JSON.parse(resultJson);
        return (p && typeof p === 'object') ? p : {};
    } catch (_) {
        return {};
    }
}

// ---- checkIsSupportMidasPayment ---------------------------------------------

function checkIsSupportMidasPayment(options) {
    return wrapAsync('checkIsSupportMidasPayment', function () {
        var resultJson = op_check_is_support_midas_payment(JSON.stringify({}));
        return JSON.parse(resultJson);
    }, options);
}

// ---- requestMidasPayment ---------------------------------------------------

function requestMidasPayment(options) {
    var opts = (options && typeof options === 'object') ? options : {};
    var success = typeof opts.success === 'function' ? opts.success : noop;
    var fail = typeof opts.fail === 'function' ? opts.fail : noop;
    var complete = typeof opts.complete === 'function' ? opts.complete : noop;

    const settlement = createSettlement(opts);
    const resolve = settlement.resolve;
    const reject = settlement.reject;
    (function () {
        var requestId;
        try {
            requestId = allocateHostCallbackId();
            _pendingMidas.set(requestId, { success: success, fail: fail, complete: complete, resolve: resolve, reject: reject });
            op_request_midas_payment(JSON.stringify({
                requestId: requestId,
                mode: opts.mode || 'game',
                env: opts.env !== undefined ? opts.env : 0,
                offerId: opts.offerId || '',
                currencyType: opts.currencyType || 'CNY',
                platform: opts.platform || '',
                buyQuantity: opts.buyQuantity || 0,
                zoneId: opts.zoneId || 1,
                outTradeNo: opts.outTradeNo || '',
            }));
        } catch (e) {
            _pendingMidas.delete(requestId);
            var res = { errMsg: failMessage('requestMidasPayment', e), errCode: -1 };
            queueMicrotask(function () { fail(res); complete(res); reject(res); });
        }
    })();
    return settlement.promise;
}

function _settleMidas(requestId, result) {
    var pending = _pendingMidas.get(requestId);
    if (!pending) return;
    _pendingMidas.delete(requestId);

    if (result.error) {
        var res = { errMsg: failMessage('requestMidasPayment', result.error) };
        if (result.errCode !== undefined) res.errCode = result.errCode;
        invokeCallback('requestMidasPayment', 'fail', pending.fail, res);
        invokeCallback('requestMidasPayment', 'complete', pending.complete, res);
        pending.reject(res);
    } else {
        var res = { errMsg: 'requestMidasPayment:ok' };
        invokeCallback('requestMidasPayment', 'success', pending.success, res);
        invokeCallback('requestMidasPayment', 'complete', pending.complete, res);
        pending.resolve(res);
    }
}

function _internalOnMidasPaymentResult(resultJson) {
    var result = _parseResult(resultJson);
    if (result !== null && typeof result === 'object' && 'requestId' in result) {
        var requestId = parseHostCallbackId(result.requestId);
        // Present and not an id: discard. `Number()` alone read `true` as the
        // id 1 and `1.5` as a lookup, and either could settle a purchase the
        // result does not belong to.
        if (requestId !== null) _settleMidas(requestId, result);
    }
    // No id names no purchase: settling the oldest pending one with it would
    // settle somebody else's. Every host path stamps the id.
}

// ---- requestMidasPaymentGameItem -------------------------------------------

function requestMidasPaymentGameItem(options) {
    var opts = (options && typeof options === 'object') ? options : {};
    var success = typeof opts.success === 'function' ? opts.success : noop;
    var fail = typeof opts.fail === 'function' ? opts.fail : noop;
    var complete = typeof opts.complete === 'function' ? opts.complete : noop;

    const settlement = createSettlement(opts);
    const resolve = settlement.resolve;
    const reject = settlement.reject;
    (function () {
        var requestId;
        try {
            requestId = allocateHostCallbackId();
            _pendingMidasGameItem.set(requestId, { success: success, fail: fail, complete: complete, resolve: resolve, reject: reject });
            op_request_midas_payment_game_item(JSON.stringify({
                requestId: requestId,
                signData: opts.signData || '',
                paySig: opts.paySig || '',
                signature: opts.signature || '',
            }));
        } catch (e) {
            _pendingMidasGameItem.delete(requestId);
            var res = { errMsg: failMessage('requestMidasPaymentGameItem', e), errCode: -1 };
            queueMicrotask(function () { fail(res); complete(res); reject(res); });
        }
    })();
    return settlement.promise;
}

function _settleMidasGameItem(requestId, result) {
    var pending = _pendingMidasGameItem.get(requestId);
    if (!pending) return;
    _pendingMidasGameItem.delete(requestId);

    if (result.error) {
        var res = { errMsg: failMessage('requestMidasPaymentGameItem', result.error) };
        if (result.errCode !== undefined) res.errCode = result.errCode;
        invokeCallback('requestMidasPaymentGameItem', 'fail', pending.fail, res);
        invokeCallback('requestMidasPaymentGameItem', 'complete', pending.complete, res);
        pending.reject(res);
    } else {
        var res = { errMsg: 'requestMidasPaymentGameItem:ok' };
        invokeCallback('requestMidasPaymentGameItem', 'success', pending.success, res);
        invokeCallback('requestMidasPaymentGameItem', 'complete', pending.complete, res);
        pending.resolve(res);
    }
}

function _internalOnMidasPaymentGameItemResult(resultJson) {
    var result = _parseResult(resultJson);
    if (result !== null && typeof result === 'object' && 'requestId' in result) {
        var requestId = parseHostCallbackId(result.requestId);
        // Present and not an id: discard. `Number()` alone read `true` as the
        // id 1 and `1.5` as a lookup, and either could settle a purchase the
        // result does not belong to.
        if (requestId !== null) _settleMidasGameItem(requestId, result);
    }
    // No id names no purchase: settling the oldest pending one with it would
    // settle somebody else's. Every host path stamps the id.
}

export {
    checkIsSupportMidasPayment,
    requestMidasPayment,
    requestMidasPaymentGameItem,
    _internalOnMidasPaymentResult,
    _internalOnMidasPaymentGameItemResult,
};
