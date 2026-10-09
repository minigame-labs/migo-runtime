import { op_open_app_authorize_setting } from "ext:core/ops";
import { createDeferredApi } from "ext:host_v8_base/02_async.js";

// Correlated by request id like every other deferred API. This settled by
// `shift()` on an array, so two calls in flight -- or one left pending across a
// runtime restart -- answered each other. Timeout disabled: the user is in the
// system settings app and takes as long as they take.
const _authSettingApi = createDeferredApi('openAppAuthorizeSetting', 0);

function openAppAuthorizeSetting(options) {
    return _authSettingApi.invoke(options || {}, function (_o, requestId) {
        op_open_app_authorize_setting(requestId);
    });
}

// The result in the shape every host result takes: `{requestId}` on success,
// `{requestId, error}` with the host's reason on failure.
function _internalOnOpenAppAuthorizeSettingFinished(resultJson) {
    _authSettingApi.settle(resultJson);
}

export { openAppAuthorizeSetting, _internalOnOpenAppAuthorizeSettingFinished };
