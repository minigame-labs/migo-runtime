// navigateToMiniProgram / openCustomerServiceConversation
//
// navigateToMiniProgram: Mode C (async, host navigates, result via EvalScript).
// openCustomerServiceConversation: Mode A (sync, host opens conversation).

import { op_navigate_to_mini_program, op_navigate_back_mini_program, op_open_customer_service_conversation } from "ext:core/ops";
import { wrapAsync, createDeferredApi } from "ext:host_v8_base/02_async.js";

// ---- navigateToMiniProgram (Mode C) ----------------------------------------

const _navigateApi = createDeferredApi('navigateToMiniProgram');

function navigateToMiniProgram(options) {
    return _navigateApi.invoke(options, function (opts, requestId) {
        if (typeof opts.appId !== 'string' || opts.appId.length === 0) {
            throw new Error('appId is required');
        }
        op_navigate_to_mini_program(JSON.stringify({
            requestId: requestId,
            appId: opts.appId,
            path: opts.path || '',
            extraData: opts.extraData || {},
            envVersion: opts.envVersion || 'release',
        }));
    });
}

function _internalOnNavigateToMiniProgramResult(resultJson) {
    _navigateApi.settle(resultJson);
}

// ---- navigateBackMiniProgram (Mode A) ----------------------------------------

function navigateBackMiniProgram(options) {
    return wrapAsync('navigateBackMiniProgram', function () {
        var opts = options || {};
        op_navigate_back_mini_program(JSON.stringify({
            extraData: opts.extraData || {},
        }));
    }, options);
}

// ---- openCustomerServiceConversation (Mode A) ------------------------------

function openCustomerServiceConversation(options) {
    return wrapAsync('openCustomerServiceConversation', function () {
        var opts = options || {};
        op_open_customer_service_conversation(JSON.stringify({
            sessionFrom: opts.sessionFrom || '',
            showMessageCard: !!opts.showMessageCard,
            sendMessageTitle: opts.sendMessageTitle || '',
            sendMessagePath: opts.sendMessagePath || '',
            sendMessageImg: opts.sendMessageImg || '',
        }));
    }, options);
}

// ---- openBusinessView (Mode A stub) ----------------------------------------

function openBusinessView(options) {
    return wrapAsync('openBusinessView', function () {
        throw new Error('not supported');
    }, options);
}

export {
    navigateToMiniProgram,
    navigateBackMiniProgram,
    _internalOnNavigateToMiniProgramResult,
    openCustomerServiceConversation,
    openBusinessView,
};
