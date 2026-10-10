// showShareMenu / updateShareMenu / onShareAppMessage / offShareAppMessage /
// shareAppMessage / shareMessageToFriend / showShareImageMenu
//
//   - showShareMenu / updateShareMenu cache the menu configuration
//   - onShareAppMessage registers a callback invoked when shareAppMessage fires
//   - shareAppMessage, shareMessageToFriend and showShareImageMenu are requests
//     the host answers (Mode C), each through its own op and result hook; the
//     image each names reaches the host as the real file behind its sandbox
//     path (the ops resolve it), and an option content did not set is absent

import {
    op_share_app_message,
    op_share_message_to_friend,
    op_show_share_image_menu,
} from "ext:core/ops";
import {
    wrapAsync,
    createDeferredApi,
    createListenerGroup,
    invokeCallback,
    isCallbackStyle,
} from "ext:host_v8_base/02_async.js";

// The string options content set, under their own names: an option it left out
// is absent from the request rather than an empty string the host must tell
// apart from a real one -- and an empty image path is no file to resolve.
function _strings(target, opts, names) {
    for (let i = 0; i < names.length; i++) {
        const value = opts[names[i]];
        if (typeof value === 'string' && value.length > 0) target[names[i]] = value;
    }
    return target;
}

// ---- share menu state ------------------------------------------------------

let _shareMenuConfig = {
    withShareTicket: false,
    menus: ['shareAppMessage', 'shareTimeline'],
};

// ---- showShareMenu ---------------------------------------------------------

function showShareMenu(options) {
    return wrapAsync('showShareMenu', function () {
        const opts = options || {};
        if (opts.withShareTicket !== undefined) {
            _shareMenuConfig.withShareTicket = !!opts.withShareTicket;
        }
        if (Array.isArray(opts.menus)) {
            _shareMenuConfig.menus = opts.menus.slice();
        }
        return {};
    }, options);
}

// ---- hideShareMenu ---------------------------------------------------------

function hideShareMenu(options) {
    return wrapAsync('hideShareMenu', function () {
        _shareMenuConfig.menus = [];
        return {};
    }, options);
}

// ---- updateShareMenu -------------------------------------------------------

function updateShareMenu(options) {
    return wrapAsync('updateShareMenu', function () {
        const opts = options || {};
        if (opts.withShareTicket !== undefined) {
            _shareMenuConfig.withShareTicket = !!opts.withShareTicket;
        }
        if (opts.isUpdatableMessage !== undefined) {
            _shareMenuConfig.isUpdatableMessage = !!opts.isUpdatableMessage;
        }
        if (opts.activityId !== undefined) {
            _shareMenuConfig.activityId = opts.activityId;
        }
        if (opts.templateInfo !== undefined) {
            _shareMenuConfig.templateInfo = opts.templateInfo;
        }
        if (Array.isArray(opts.menus)) {
            _shareMenuConfig.menus = opts.menus.slice();
        }
        return {};
    }, options);
}

// ---- onShareAppMessage / offShareAppMessage --------------------------------

const _shareListeners = createListenerGroup('onShareAppMessage');

function onShareAppMessage(listener) {
    _shareListeners.on(listener);
}

function offShareAppMessage(listener) {
    _shareListeners.off(listener);
}

// ---- shareAppMessage (Mode C - host op) ------------------------------------

// Every share waits on the player -- a picker, a share sheet -- for as long as
// they take, so none times out here; the host answers or fails each one.
const _shareAppMessageApi = createDeferredApi('shareAppMessage', 0);

// What content asked to share, as it asked. The onShareAppMessage listeners
// answer the share menu, a different flow; they have no say over this call.
function shareAppMessage(options) {
    return _shareAppMessageApi.invoke(options, function (opts, requestId) {
        const request = _strings({ requestId: requestId }, opts,
            ['title', 'imageUrl', 'query', 'imageUrlId', 'path']);
        if (typeof opts.toCurrentGroup === 'boolean') request.toCurrentGroup = opts.toCurrentGroup;
        return op_share_app_message(JSON.stringify(request));
    });
}

function _internalOnShareAppMessageResult(resultJson) {
    _shareAppMessageApi.settle(resultJson);
}

// ---- onShareTimeline / offShareTimeline ------------------------------------

const _shareTimelineListeners = createListenerGroup('onShareTimeline');

function onShareTimeline(listener) {
    _shareTimelineListeners.on(listener);
}

function offShareTimeline(listener) {
    _shareTimelineListeners.off(listener);
}

// ---- host-side trigger (called from Rust when user taps native timeline share)

function _internalTriggerShareTimeline() {
    var shareData = { title: '', imageUrl: '', query: '' };
    var listeners = _shareTimelineListeners.snapshot();
    for (var i = 0; i < listeners.length; i++) {
        try {
            var override = listeners[i]();
            if (override && typeof override === 'object') {
                if (typeof override.title === 'string') shareData.title = override.title;
                if (typeof override.imageUrl === 'string') shareData.imageUrl = override.imageUrl;
                if (typeof override.query === 'string') shareData.query = override.query;
            }
        } catch (e) {
            console.error('onShareTimeline listener error:', e);
        }
    }
    return shareData;
}

// ---- shareMessageToFriend ----------------------------------------------------

// A request the host answers: the host's relationship chain picks the friend,
// so success is the host's to report, not something this call can claim.
const _shareMessageToFriendApi = createDeferredApi('shareMessageToFriend', 0);

// onShareMessageToFriend is told how every shareMessageToFriend ended -- the
// call is made from the open data context, its outcome is the game's to hear --
// so the request is settled here first and content's own callbacks after.
const _shareToFriendListeners = createListenerGroup('onShareMessageToFriend');

function shareMessageToFriend(options) {
    const opts = options || {};
    const bare = Object.assign({}, opts);
    delete bare.success;
    delete bare.fail;
    delete bare.complete;
    const outcome = _shareMessageToFriendApi.invoke(bare, function (o, requestId) {
        if (typeof o.openId !== 'string' || o.openId.length === 0) {
            throw new Error('openId is required');
        }
        const request = _strings({ requestId: requestId }, o,
            ['openId', 'title', 'imageUrl', 'imageUrlId']);
        if (_messageToFriendQuery !== null) Object.assign(request, _messageToFriendQuery);
        return op_share_message_to_friend(JSON.stringify(request));
    }).then(function (res) {
        _shareToFriendListeners.trigger({ success: true, errMsg: res.errMsg });
        return res;
    }, function (res) {
        _shareToFriendListeners.trigger({ success: false, errMsg: res.errMsg });
        throw res;
    });
    if (!isCallbackStyle(opts)) return outcome;
    outcome.then(function (res) {
        invokeCallback('shareMessageToFriend', 'success', opts.success, res);
        invokeCallback('shareMessageToFriend', 'complete', opts.complete, res);
    }, function (res) {
        invokeCallback('shareMessageToFriend', 'fail', opts.fail, res);
        invokeCallback('shareMessageToFriend', 'complete', opts.complete, res);
    });
}

function _internalOnShareMessageToFriendResult(resultJson) {
    _shareMessageToFriendApi.settle(resultJson);
}

// ---- onShareMessageToFriend / offShareMessageToFriend ----------------------

function onShareMessageToFriend(listener) {
    _shareToFriendListeners.on(listener);
}

function offShareMessageToFriend(listener) {
    _shareToFriendListeners.off(listener);
}

// ---- setMessageToFriendQuery -----------------------------------------------

// What the next shareMessageToFriend carries for the friend it reaches, who
// reads it from their enter options' query.
let _messageToFriendQuery = null;

function setMessageToFriendQuery(options) {
    const opts = options || {};
    if (typeof opts.query !== 'string' || opts.query.length > 128) return false;
    const scene = opts.shareMessageToFriendScene;
    if (scene !== undefined && !(Number.isInteger(scene) && scene >= 0 && scene <= 50)) {
        return false;
    }
    _messageToFriendQuery = { query: opts.query };
    if (scene !== undefined) _messageToFriendQuery.shareMessageToFriendScene = scene;
    return true;
}

// ---- showShareImageMenu ------------------------------------------------------

// The share sheet is the host's UI; whether the player shared is its answer.
const _showShareImageMenuApi = createDeferredApi('showShareImageMenu', 0);

function showShareImageMenu(options) {
    return _showShareImageMenuApi.invoke(options, function (opts, requestId) {
        if (typeof opts.path !== 'string' || opts.path.length === 0) {
            throw new Error('path is required');
        }
        const request = _strings({ requestId: requestId }, opts, ['path', 'entrancePath']);
        request.needShowEntrance = !!opts.needShowEntrance;
        return op_show_share_image_menu(JSON.stringify(request));
    });
}

function _internalOnShowShareImageMenuResult(resultJson) {
    _showShareImageMenuApi.settle(resultJson);
}

// ---- host-side trigger (called from Rust when user taps native share) ------

function _internalTriggerShareAppMessage() {
    let shareData = { title: '', imageUrl: '', query: '' };
    const listeners = _shareListeners.snapshot();
    for (let i = 0; i < listeners.length; i++) {
        try {
            const override = listeners[i]();
            if (override && typeof override === 'object') {
                if (typeof override.title === 'string') shareData.title = override.title;
                if (typeof override.imageUrl === 'string') shareData.imageUrl = override.imageUrl;
                if (typeof override.query === 'string') shareData.query = override.query;
            }
        } catch (e) {
            console.error('onShareAppMessage listener error:', e);
        }
    }
    return shareData;
}

export {
    showShareMenu,
    hideShareMenu,
    updateShareMenu,
    onShareAppMessage,
    offShareAppMessage,
    shareAppMessage,
    _internalOnShareAppMessageResult,
    _internalTriggerShareAppMessage,
    onShareTimeline,
    offShareTimeline,
    _internalTriggerShareTimeline,
    shareMessageToFriend,
    _internalOnShareMessageToFriendResult,
    onShareMessageToFriend,
    offShareMessageToFriend,
    setMessageToFriendQuery,
    showShareImageMenu,
    _internalOnShowShareImageMenuResult,
};
