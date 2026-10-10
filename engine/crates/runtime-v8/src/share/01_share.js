// Sharing.
//
//   - shareAppMessage, shareMessageToFriend and showShareImageMenu are requests
//     the host answers (Mode C), each through its own op and result hook; the
//     image each names reaches the host as the real file behind its sandbox
//     path (the ops resolve it), and an option content did not set is absent
//   - the share menu is the host's: showShareMenu / hideShareMenu /
//     updateShareMenu keep its state here and tell the host each change, and
//     when the player picks "share", "share to moments" or "add to favorites"
//     the host asks content for what to share -- the onShareAppMessage,
//     onShareTimeline or onAddToFavorites listener's answer -- and is always
//     answered, null when nothing listens

import {
    op_share_app_message,
    op_share_menu_reply,
    op_share_message_to_friend,
    op_share_set_menu,
    op_show_share_image_menu,
} from "ext:core/ops";
import {
    wrapAsync,
    createDeferredApi,
    createListenerGroup,
    isCallbackStyle,
    withCallbacks,
    withoutCallbacks,
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

// ---- the share menu --------------------------------------------------------

// What the host's menu offers. A mini game's share items start hidden and
// content turns them on with showShareMenu.
const SHARE_MENUS = ['shareAppMessage', 'shareTimeline'];
// updateShareMenu's fields, kept as content set them.
const UPDATE_FIELDS = [
    'isUpdatableMessage', 'activityId', 'toDoActivityId', 'templateInfo', 'isPrivateMessage',
    'participant', 'useForChatTool', 'chooseType',
];

const _shareMenu = { menus: [], withShareTicket: false };

// The host keeps its menu in step with this state, whole, after each change.
// With no host there is no menu, and the state is still content's to read.
function _menuChanged() {
    try {
        op_share_set_menu(JSON.stringify(_shareMenu));
    } catch (_) {
        // No share service: nothing shows a menu.
    }
}

function _menusOf(opts, fallback) {
    const menus = Array.isArray(opts.menus) ? opts.menus : fallback;
    return menus.filter(function (menu) { return SHARE_MENUS.includes(menu); });
}

function showShareMenu(options) {
    return wrapAsync('showShareMenu', function () {
        const opts = options || {};
        const shown = _menusOf(opts, ['shareAppMessage']);
        // Moments sharing is offered only beside sharing to a friend.
        if (shown.includes('shareTimeline')) shown.push('shareAppMessage');
        _shareMenu.menus = SHARE_MENUS.filter(function (menu) {
            return _shareMenu.menus.includes(menu) || shown.includes(menu);
        });
        if (typeof opts.withShareTicket === 'boolean') _shareMenu.withShareTicket = opts.withShareTicket;
        _menuChanged();
        return {};
    }, options);
}

function hideShareMenu(options) {
    return wrapAsync('hideShareMenu', function () {
        const hidden = _menusOf(options || {}, SHARE_MENUS);
        // Without sharing to a friend there is no moments sharing either.
        if (hidden.includes('shareAppMessage')) hidden.push('shareTimeline');
        _shareMenu.menus = _shareMenu.menus.filter(function (menu) { return !hidden.includes(menu); });
        _menuChanged();
        return {};
    }, options);
}

function updateShareMenu(options) {
    return wrapAsync('updateShareMenu', function () {
        const opts = options || {};
        if (typeof opts.withShareTicket === 'boolean') _shareMenu.withShareTicket = opts.withShareTicket;
        for (let i = 0; i < UPDATE_FIELDS.length; i++) {
            if (opts[UPDATE_FIELDS[i]] !== undefined) _shareMenu[UPDATE_FIELDS[i]] = opts[UPDATE_FIELDS[i]];
        }
        _menuChanged();
        return {};
    }, options);
}

// ---- what the menu shares --------------------------------------------------

const _shareListeners = createListenerGroup('onShareAppMessage');
const _shareTimelineListeners = createListenerGroup('onShareTimeline');
const _addToFavoritesListeners = createListenerGroup('onAddToFavorites');

function onShareAppMessage(listener) { _shareListeners.on(listener); }
function offShareAppMessage(listener) { _shareListeners.off(listener); }
function onShareTimeline(listener) { _shareTimelineListeners.on(listener); }
function offShareTimeline(listener) { _shareTimelineListeners.off(listener); }
function onAddToFavorites(listener) { _addToFavoritesListeners.on(listener); }
function offAddToFavorites(listener) { _addToFavoritesListeners.off(listener); }

// Each menu item: whose listeners answer it, and the fields its answer has.
const MENU_SHARES = {
    shareAppMessage: {
        listeners: _shareListeners,
        strings: ['title', 'imageUrl', 'query', 'imageUrlId', 'path'],
        booleans: ['toCurrentGroup'],
    },
    shareTimeline: {
        listeners: _shareTimelineListeners,
        strings: ['title', 'imageUrl', 'imageUrlId', 'imagePreviewUrl', 'imagePreviewUrlId', 'query', 'path'],
        booleans: [],
    },
    addToFavorites: {
        listeners: _addToFavoritesListeners,
        strings: ['title', 'query', 'imageUrl'],
        booleans: ['disableForward'],
    },
};

// How long a share waits on the promise an onShareAppMessage answer carries,
// as the common platform does, before it shares the answer's own fields.
const SHARE_PROMISE_WAIT_MS = 3000;

function _content(share, answer) {
    if (answer === null || typeof answer !== 'object') return null;
    const content = _strings({}, answer, share.strings);
    for (let i = 0; i < share.booleans.length; i++) {
        if (typeof answer[share.booleans[i]] === 'boolean') {
            content[share.booleans[i]] = answer[share.booleans[i]];
        }
    }
    return content;
}

function _replyMenuShare(replyId, menu, content) {
    op_share_menu_reply(JSON.stringify({ replyId: replyId, menu: menu, content: content }))
        .then(undefined, function (e) { console.error('share menu reply not delivered:', e); });
}

// The host's {"menu", "replyId"}: the player picked a share item. The last
// listener that answers with an object decides, as one registration does on
// the common platform.
function _internalOnShareMenuEvent(eventJson) {
    let event;
    try { event = JSON.parse(eventJson); } catch (_) { return; }
    if (event === null || typeof event !== 'object' || typeof event.replyId !== 'number') return;
    const menu = event.menu;
    const share = Object.prototype.hasOwnProperty.call(MENU_SHARES, menu) ? MENU_SHARES[menu] : null;
    if (share === null) {
        _replyMenuShare(event.replyId, menu, null);
        return;
    }
    let answer = null;
    const listeners = share.listeners.snapshot();
    for (let i = 0; i < listeners.length; i++) {
        try {
            const value = listeners[i]();
            if (value !== null && typeof value === 'object') answer = value;
        } catch (e) {
            console.error('on' + menu.charAt(0).toUpperCase() + menu.slice(1) + ' listener error:', e);
        }
    }
    const promise = menu === 'shareAppMessage' && answer !== null ? answer.promise : undefined;
    if (promise === undefined || promise === null || typeof promise.then !== 'function') {
        _replyMenuShare(event.replyId, menu, _content(share, answer));
        return;
    }
    let replied = false;
    const reply = function (content) {
        if (replied) return;
        replied = true;
        _replyMenuShare(event.replyId, menu, content);
    };
    const timer = setTimeout(function () { reply(_content(share, answer)); }, SHARE_PROMISE_WAIT_MS);
    promise.then(function (resolved) {
        clearTimeout(timer);
        reply(_content(share, resolved !== null && typeof resolved === 'object' ? resolved : answer));
    }, function () {
        clearTimeout(timer);
        reply(_content(share, answer));
    });
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
    const outcome = _shareMessageToFriendApi.invoke(withoutCallbacks(opts), function (o, requestId) {
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
    withCallbacks('shareMessageToFriend', outcome, opts);
    // A callback-style call returns nothing, as every other one does.
    return isCallbackStyle(opts) ? undefined : outcome;
}

function _internalOnShareMessageToFriendResult(resultJson) {
    _shareMessageToFriendApi.settle(resultJson);
}

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

export {
    showShareMenu,
    hideShareMenu,
    updateShareMenu,
    onShareAppMessage,
    offShareAppMessage,
    onShareTimeline,
    offShareTimeline,
    onAddToFavorites,
    offAddToFavorites,
    _internalOnShareMenuEvent,
    shareAppMessage,
    _internalOnShareAppMessageResult,
    shareMessageToFriend,
    _internalOnShareMessageToFriendResult,
    onShareMessageToFriend,
    offShareMessageToFriend,
    setMessageToFriendQuery,
    showShareImageMenu,
    _internalOnShowShareImageMenuResult,
};
