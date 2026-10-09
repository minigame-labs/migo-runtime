// setClipboardData / getClipboardData
//
// Requests the host answers (Mode C): the clipboard is the host's, and a host
// that owns it on another thread -- every C ABI host does -- cannot answer a
// synchronous read. Both are asynchronous APIs in the first place.

import { op_set_clipboard_data, op_get_clipboard_data } from "ext:core/ops";
import { createDeferredApi } from "ext:host_v8_base/02_async.js";

const _setClipboardDataApi = createDeferredApi('setClipboardData');
const _getClipboardDataApi = createDeferredApi('getClipboardData');

function setClipboardData(options) {
    return _setClipboardDataApi.invoke(options, function (opts, requestId) {
        if (typeof opts.data !== 'string') {
            throw new Error('data must be a string');
        }
        op_set_clipboard_data(JSON.stringify({ requestId: requestId, data: opts.data }));
    });
}

function getClipboardData(options) {
    return _getClipboardDataApi.invoke(options, function (_opts, requestId) {
        op_get_clipboard_data(JSON.stringify({ requestId: requestId }));
    });
}

function _internalOnSetClipboardDataResult(resultJson) {
    _setClipboardDataApi.settle(resultJson);
}

function _internalOnGetClipboardDataResult(resultJson) {
    _getClipboardDataApi.settle(resultJson);
}

export {
    setClipboardData,
    getClipboardData,
    _internalOnSetClipboardDataResult,
    _internalOnGetClipboardDataResult,
};
