import {
    op_show_toast,
    op_hide_toast,
    op_show_modal,
    op_show_loading,
    op_hide_loading,
    op_show_action_sheet,
} from "ext:core/ops";
import { wrapAsync, createDeferredApi } from "ext:host_v8_base/02_async.js";

// ==================== Toast (Mode A) ====================

function showToast(options) {
    return wrapAsync('showToast', function () {
        var opts = options || {};
        op_show_toast(JSON.stringify({
            title: opts.title || '',
            icon: opts.icon || 'success',
            duration: opts.duration !== undefined ? opts.duration : 1500,
            mask: !!opts.mask,
        }));
    }, options);
}

function hideToast(options) {
    return wrapAsync('hideToast', function () {
        op_hide_toast();
    }, options);
}

// ==================== Modal (correlated by request id) ====================

// The same `createDeferredApi` every other deferred API uses, rather than a
// fourth hand-rolled pending registry. This one settled by `shift()` on an
// array, so with two dialogs queued the first result answered whichever call
// arrived first -- and a host that answers out of order, or a runtime restart
// between them, made that the wrong one. Timeout disabled: a modal legitimately
// waits for a person, and rejecting one on a clock is not this change's job.
const _modalApi = createDeferredApi('showModal', 0);

function showModal(options) {
    var opts = options || {};
    return _modalApi.invoke(opts, function (o, requestId) {
        op_show_modal(JSON.stringify({
            requestId: requestId,
            title: o.title || '',
            content: o.content || '',
            showCancel: o.showCancel !== false,
            cancelText: o.cancelText || '\u53d6\u6d88',
            confirmText: o.confirmText || '\u786e\u5b9a',
            cancelColor: o.cancelColor || '#000000',
            confirmColor: o.confirmColor || '#576B95',
            // An input in place of the content, its text the answer's `content`.
            editable: o.editable === true,
            placeholderText: typeof o.placeholderText === 'string' ? o.placeholderText : '',
        }));
    });
}

// `{requestId, confirm, cancel, content?}` -- content when the modal was
// editable -- or `{requestId, error}`, in the shape every host result takes.
function _internalOnModalResult(resultJson) {
    _modalApi.settle(resultJson);
}

// ==================== Loading (Mode A) ====================

function showLoading(options) {
    return wrapAsync('showLoading', function () {
        var opts = options || {};
        op_show_loading(JSON.stringify({
            title: opts.title || '',
            mask: !!opts.mask,
        }));
    }, options);
}

function hideLoading(options) {
    return wrapAsync('hideLoading', function () {
        op_hide_loading();
    }, options);
}

// ==================== Action Sheet (correlated by request id) ====================

const _actionSheetApi = createDeferredApi('showActionSheet', 0);

function showActionSheet(options) {
    var opts = options || {};
    return _actionSheetApi.invoke(opts, function (o, requestId) {
        op_show_action_sheet(JSON.stringify({
            requestId: requestId,
            alertText: o.alertText || '',
            itemList: o.itemList || [],
            itemColor: o.itemColor || '#000000',
        }));
    });
}

// `{requestId, tapIndex}`, or `{requestId, error}` -- "cancel" when the
// player dismissed the sheet.
function _internalOnActionSheetResult(resultJson) {
    _actionSheetApi.settle(resultJson);
}

export {
    showToast, hideToast,
    showModal, _internalOnModalResult,
    showLoading, hideLoading,
    showActionSheet, _internalOnActionSheetResult,
};
