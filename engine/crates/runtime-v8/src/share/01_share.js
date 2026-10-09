// showShareMenu / updateShareMenu / onShareAppMessage / offShareAppMessage /
// shareAppMessage / shareMessageToFriend / showShareImageMenu
//
//   - showShareMenu / updateShareMenu cache the menu configuration
//   - onShareAppMessage registers a callback invoked when shareAppMessage fires
//   - shareAppMessage, shareMessageToFriend and showShareImageMenu are requests
//     the host answers (Mode C), each through its own op and result hook

import {
    op_share_app_message,
    op_share_message_to_friend,
    op_show_share_image_menu,
} from "ext:core/ops";
import { wrapAsync, createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

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

const _shareAppMessageApi = createDeferredApi('shareAppMessage');

function shareAppMessage(options) {
    return _shareAppMessageApi.invoke(options, function (opts, requestId) {
        const shareData = {
            requestId: requestId,
            title: opts.title || '',
            imageUrl: opts.imageUrl || '',
            query: opts.query || '',
            imageUrlId: opts.imageUrlId || '',
        };

        // Invoke registered listeners to allow the game to customise share data
        const listeners = _shareListeners.snapshot();
        for (let i = 0; i < listeners.length; i++) {
            try {
                const override = listeners[i](shareData);
                if (override && typeof override === 'object') {
                    if (typeof override.title === 'string') shareData.title = override.title;
                    if (typeof override.imageUrl === 'string') shareData.imageUrl = override.imageUrl;
                    if (typeof override.query === 'string') shareData.query = override.query;
                }
            } catch (e) {
                console.error('onShareAppMessage listener error:', e);
            }
        }

        op_share_app_message(JSON.stringify(shareData));
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
const _shareMessageToFriendApi = createDeferredApi('shareMessageToFriend');

function shareMessageToFriend(options) {
    return _shareMessageToFriendApi.invoke(options, function (opts, requestId) {
        if (typeof opts.openId !== 'string' || opts.openId.length === 0) {
            throw new Error('openId is required');
        }
        op_share_message_to_friend(JSON.stringify({
            requestId: requestId,
            openId: opts.openId,
            title: opts.title || '',
            imageUrl: opts.imageUrl || '',
            imageUrlId: opts.imageUrlId || '',
        }));
    });
}

function _internalOnShareMessageToFriendResult(resultJson) {
    _shareMessageToFriendApi.settle(resultJson);
}

// ---- onShareMessageToFriend / offShareMessageToFriend ----------------------

const _shareToFriendListeners = createListenerGroup('onShareMessageToFriend');

function onShareMessageToFriend(listener) {
    _shareToFriendListeners.on(listener);
}

function offShareMessageToFriend(listener) {
    _shareToFriendListeners.off(listener);
}

function _internalTriggerShareMessageToFriend(data) {
    var parsed = data;
    if (typeof data === 'string') {
        try { parsed = JSON.parse(data); } catch (_) { parsed = {}; }
    }
    _shareToFriendListeners.trigger(parsed);
}

// ---- setMessageToFriendQuery -----------------------------------------------

let _messageToFriendQuery = '';

function setMessageToFriendQuery(options) {
    var opts = options || {};
    if (typeof opts.query === 'string') {
        _messageToFriendQuery = opts.query;
    }
}

// ---- showShareImageMenu ------------------------------------------------------

// The share sheet is the host's UI; whether the player shared is its answer.
const _showShareImageMenuApi = createDeferredApi('showShareImageMenu');

function showShareImageMenu(options) {
    return _showShareImageMenuApi.invoke(options, function (opts, requestId) {
        if (typeof opts.path !== 'string' || opts.path.length === 0) {
            throw new Error('path is required');
        }
        op_show_share_image_menu(JSON.stringify({
            requestId: requestId,
            path: opts.path,
            style: opts.style || '',
            needShowEntrance: !!opts.needShowEntrance,
            entrancePath: opts.entrancePath || '',
        }));
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
    _internalTriggerShareMessageToFriend,
    setMessageToFriendQuery,
    showShareImageMenu,
    _internalOnShowShareImageMenuResult,
};
