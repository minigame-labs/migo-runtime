import { op_scan_code } from "ext:core/ops";
import { createDeferredApi } from "ext:host_v8_base/02_async.js";

// Timeout disabled: the player aims the camera, or gives up, when they do.
const _scanCodeApi = createDeferredApi('scanCode', 0);

function scanCode(options = {}) {
    return _scanCodeApi.invoke(options, function (opts, requestId) {
        op_scan_code(JSON.stringify({
            requestId: requestId,
            onlyFromCamera: opts.onlyFromCamera === true,
            scanType: Array.isArray(opts.scanType) ? opts.scanType : ['barCode', 'qrCode'],
        }));
    });
}

function _internalOnScanCodeResult(resultJson) {
    _scanCodeApi.settle(resultJson);
}

export { scanCode, _internalOnScanCodeResult };
