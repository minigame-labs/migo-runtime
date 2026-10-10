import {
    op_ecosystem_available,
    op_ecosystem_call,
    op_ecosystem_reply,
    op_ecosystem_value,
} from "ext:core/ops";
import { createDeferredApi, createListenerGroup } from "ext:host_v8_base/02_async.js";

// The host's ecosystem: social, commerce and platform features a host app offers
// on top of the engine -- friends and groups, cloud storage, live channels, voice
// chat, handoff, subscriptions, privacy agreements. Their semantics are the
// host's, so every one crosses as the same request keyed by its name, and the
// host answers it -- or fails it as not supported. Nothing here invents an
// answer: an API that used to "succeed" while doing nothing (cloud storage that
// stored nothing, subscriptions every user "accepted") now asks the host.
//
// The lists below are closed, and the host-service contract
// (contracts/runtime/host-services.json, service "ecosystem") names the same
// ones -- its "calls" are ECOSYSTEM_CALLS and ECOSYSTEM_OBJECT_CALLS together,
// its "events" ECOSYSTEM_EVENTS and ECOSYSTEM_OBJECT_EVENTS: a host knows the
// whole set it may be asked for. What an object does crosses under the name the
// common platform documents it by, `Class.member`.

// Requests the host answers.
const ECOSYSTEM_CALLS = [
    'addCard', 'openCard',
    'authPrivateMessage',
    'checkHandoffEnabled', 'setHandoffQuery', 'startHandoff',
    'checkIsAddedToMyMiniProgram',
    'checkIsSupportFacialRecognition', 'requestFacialRecognition', 'requestFacialVerify',
    'enableOfflineModeDebug',
    'enterChatToolMode', 'exitChatTool', 'getChatToolInfo', 'openChatTool',
    'notifyGroupMembers', 'selectGroupMembers',
    'shareAppMessageToGroup', 'shareEmojiToGroup', 'shareImageToGroup', 'shareTextToGroup',
    'shareVideoToGroup',
    'getBackgroundFetchData', 'getBackgroundFetchToken', 'setBackgroundFetchToken',
    'getChannelsLiveInfo', 'getChannelsLiveNoticeInfo', 'openChannelsActivity', 'openChannelsEvent',
    'openChannelsLive', 'openChannelsUserProfile', 'reserveChannelsLive',
    'getExtConfig',
    'getFriendCloudStorage', 'getGroupCloudStorage', 'getUserCloudStorage', 'getUserCloudStorageKeys',
    'setUserCloudStorage', 'removeUserCloudStorage',
    'getUserInteractiveStorage', 'modifyFriendInteractiveStorage',
    'getFriendSendGiftStatus', 'sendGiftToFriend',
    'getGameClubData', 'getGameExptInfo',
    'getGroupEnterInfo', 'getGroupInfo', 'getGroupMembersInfo',
    'getPotentialFriendList', 'getRelationFriendList',
    'getShareInfo', 'getWeRunData',
    'joinVoIPChat', 'exitVoIPChat', 'updateVoIPChatMuteConfig',
    'openBusinessView', 'openCustomerServiceChat', 'openPhotoTopicView',
    'getPrivacySetting', 'openPrivacyContract', 'requirePrivacyAuthorize',
    'reportUserBehaviorBranchAnalytics',
    'requestMidasFriendPayment',
    'requestSubscribeMessage', 'requestSubscribeSystemMessage',
    'setMenuStyle', 'setStatusBarStyle',
    // The common platform names this one after its own app; it updates the app
    // the game runs in, so here it is named for that, and an adapter maps the
    // platform's name onto it.
    'updateHostApp',
];

// Requests content makes through an object rather than a function of their
// own -- the call that creates it (getMiniReportManager) or its methods -- so
// none is published as a global; the objects are in 21_ecosystem_objects.js.
const ECOSYSTEM_OBJECT_CALLS = [
    'GameServerManager.broadcastInRoom', 'GameServerManager.cancelMatch',
    'GameServerManager.changeSeat', 'GameServerManager.createRoom', 'GameServerManager.endGame',
    'GameServerManager.endStateService', 'GameServerManager.getFriendsStateData',
    'GameServerManager.getJoinVoIPChatSignature', 'GameServerManager.getLastRoomInfo',
    'GameServerManager.getLostFrames', 'GameServerManager.getRoomInfo',
    'GameServerManager.inviteFriend', 'GameServerManager.joinRoom',
    'GameServerManager.kickoutMember', 'GameServerManager.login', 'GameServerManager.logout',
    'GameServerManager.memberLeaveRoom', 'GameServerManager.ownerLeaveRoom',
    'GameServerManager.reconnect', 'GameServerManager.restart', 'GameServerManager.setState',
    'GameServerManager.startGame', 'GameServerManager.startMatch',
    'GameServerManager.startStateService', 'GameServerManager.updateReadyStatus',
    'GameServerManager.uploadFrame',
    'RankManager.abort', 'RankManager.createChallenge', 'RankManager.getScore',
    'RankManager.middleUpdate', 'RankManager.update',
    'getMiniReportManager', 'MiniReportManager.report',
    'ScenePerformanceManager.setData',
    'StoreGift.getOrderInfo', 'StoreGift.open',
    'UserCryptoManager.getLatestUserKey',
];

// Events the host posts, as `{"name", "data", "replyId"?}`. An event with a
// replyId is always answered, as `{"replyId", "data", "done"}`, and its last
// reply has done true: `return` -- once, with the listener's returned object (a
// `promise` in it is awaited for up to two seconds, as the common platform does
// for onCopyUrl); `resolve` -- the one registered listener is handed a resolve
// function, and each call to it is a reply until one agrees or disagrees; any
// other event, or one with no listener to answer -- once, with data null.
const ECOSYSTEM_EVENTS = {
    onBackgroundFetchData: null,
    onCopyUrl: 'return',
    onHandoff: 'return',
    onInteractiveStorageModified: null,
    onNeedPrivacyAuthorization: 'resolve',
    onOfficialComponentsInfoChange: null,
    onOfflineModeStateChange: null,
    onVoIPChatInterrupted: null,
    onVoIPChatMembersChanged: null,
    onVoIPChatSpeakersChanged: null,
    onVoIPChatStateChanged: null,
};

// Events of the objects, posted as the others are; none is answered.
const ECOSYSTEM_OBJECT_EVENTS = [
    'GameServerManager.onBeKickedOut', 'GameServerManager.onBroadcast',
    'GameServerManager.onDisconnect', 'GameServerManager.onGameEnd',
    'GameServerManager.onGameStart', 'GameServerManager.onInvite',
    'GameServerManager.onLockStepError', 'GameServerManager.onLogout',
    'GameServerManager.onMatch', 'GameServerManager.onRoomInfoChange',
    'GameServerManager.onStateUpdate', 'GameServerManager.onSyncFrame',
    'RankManager.onChallengeStart',
];

// Synchronous getters, answered from what the host last reported for each.
const ECOSYSTEM_VALUES = [
    'getExptInfoSync', 'getExtConfigSync', 'getOfficialComponentsInfo', 'isChatTool',
    'StoreGift.isSupported',
];

// The true answer, for a host that offers no ecosystem at all: with no privacy
// agreement there is nothing to authorize, with no ext configuration it is
// empty, and nothing can have been added to a list that does not exist. Every
// other request fails as not supported there.
const NO_HOST_ANSWERS = {
    checkIsAddedToMyMiniProgram: { added: false },
    getExtConfig: { extConfig: {} },
    getPrivacySetting: { needAuthorization: false, privacyContractName: '' },
    requirePrivacyAuthorize: {},
};

// ---- requests ----------------------------------------------------------------

const _apis = new Map();
const _pending = new Map();

function _deferred(name) {
    let api = _apis.get(name);
    if (api === undefined) {
        // Most wait on a person -- a picker, a payment, a live room -- so none
        // times out here; the host answers or fails each one. An object's
        // member reports under its own name (`createRoom:ok`), as the common
        // platform's objects do.
        api = createDeferredApi(name.slice(name.indexOf('.') + 1), 0);
        _apis.set(name, api);
    }
    return api;
}

function _call(name, options) {
    const api = _deferred(name);
    const answer = NO_HOST_ANSWERS[name];
    if (answer !== undefined && !op_ecosystem_available()) {
        return api.invoke(options, function (_opts, requestId) {
            const result = JSON.parse(JSON.stringify(answer));
            result.requestId = requestId;
            queueMicrotask(function () { api.settleParsed(result); });
        });
    }
    return api.invoke(options, function (opts, requestId) {
        _pending.set(requestId, api);
        // Functions -- the callbacks -- do not survive JSON, which is the point.
        // The op resolves any file the request names before it leaves, so it may
        // settle later; a refusal fails the request as a throw would.
        return op_ecosystem_call(name, JSON.stringify({ requestId: requestId, api: name, options: opts }))
            .then(undefined, function (e) {
                _pending.delete(requestId);
                throw e;
            });
    });
}

/**
 * The request behind an object's call; `name` is one of ECOSYSTEM_OBJECT_CALLS.
 * Settles as any other: `options`' callbacks, or the returned promise.
 */
function ecosystemObjectCall(name, options) {
    if (!ECOSYSTEM_OBJECT_CALLS.includes(name)) throw new TypeError('not an ecosystem call: ' + name);
    return _call(name, options);
}

// The handler of each object event, which its object installs once.
const _objectEventHandlers = new Map();

/** `handler(data)` receives every `name` event; `name` is one of ECOSYSTEM_OBJECT_EVENTS. */
function onEcosystemObjectEvent(name, handler) {
    if (!ECOSYSTEM_OBJECT_EVENTS.includes(name)) throw new TypeError('not an ecosystem event: ' + name);
    _objectEventHandlers.set(name, handler);
}

/** Whether a host answers ecosystem requests at all. */
function ecosystemAvailable() {
    return op_ecosystem_available();
}

function _internalOnEcosystemResult(resultJson) {
    let parsed;
    try { parsed = JSON.parse(resultJson); } catch (_) { return; }
    if (parsed === null || typeof parsed !== 'object') return;
    const api = _pending.get(parsed.requestId);
    if (api === undefined) return;
    _pending.delete(parsed.requestId);
    api.settleParsed(parsed);
}

// ---- events ------------------------------------------------------------------

const _listeners = {};
// onNeedPrivacyAuthorization registers by overwriting: only the last listener
// is the one asked.
const _resolveListeners = {};

function _reply(replyId, data, done) {
    try {
        op_ecosystem_reply(JSON.stringify({
            replyId: replyId, data: data === undefined ? null : data, done: done,
        }));
    } catch (e) {
        console.error('ecosystem reply not delivered:', e);
    }
}

function _answerReturned(replyId, returned) {
    const promise = returned && returned.promise;
    const settled = Object.assign({}, returned);
    delete settled.promise;
    if (promise === undefined || promise === null || typeof promise.then !== 'function') {
        _reply(replyId, settled, true);
        return;
    }
    let answered = false;
    const timer = setTimeout(function () {
        if (!answered) { answered = true; _reply(replyId, settled, true); }
    }, 2000);
    promise.then(function (value) {
        if (answered) return;
        answered = true;
        clearTimeout(timer);
        _reply(replyId, Object.assign(settled, value || {}), true);
    }, function () {
        if (answered) return;
        answered = true;
        clearTimeout(timer);
        _reply(replyId, settled, true);
    });
}

function _internalOnEcosystemEvent(eventJson) {
    let event;
    try { event = JSON.parse(eventJson); } catch (_) { return; }
    if (event === null || typeof event !== 'object' || typeof event.name !== 'string') return;
    const replyId = typeof event.replyId === 'number' ? event.replyId : undefined;
    const objectHandler = _objectEventHandlers.get(event.name);
    if (objectHandler !== undefined) {
        try {
            objectHandler(event.data === undefined ? {} : event.data);
        } catch (e) {
            console.error(event.name + ' listener error:', e);
        }
        if (replyId !== undefined) _reply(replyId, null, true);
        return;
    }
    if (!Object.prototype.hasOwnProperty.call(ECOSYSTEM_EVENTS, event.name)) {
        // No such event: nothing will answer, and a host must not wait for it.
        if (replyId !== undefined) _reply(replyId, null, true);
        return;
    }
    const kind = ECOSYSTEM_EVENTS[event.name];
    const data = event.data === undefined ? {} : event.data;

    if (kind === 'resolve') {
        const listener = _resolveListeners[event.name];
        if (typeof listener !== 'function') {
            // Nobody will resolve: say so, and the host uses its own prompt.
            if (replyId !== undefined) _reply(replyId, null, true);
            return;
        }
        let finished = false;
        const resolve = function (result) {
            if (finished || replyId === undefined) return;
            const choice = result !== null && typeof result === 'object' ? result.event : undefined;
            finished = choice === 'agree' || choice === 'disagree';
            _reply(replyId, result, finished);
        };
        try { listener(resolve, data); } catch (e) { console.error(event.name + ' listener error:', e); }
        return;
    }

    const group = _listeners[event.name];
    if (kind !== 'return' || replyId === undefined) {
        group.trigger(data);
        if (replyId !== undefined) _reply(replyId, null, true);
        return;
    }
    // The last listener that returns an object decides, as one registration
    // per page does on the common platform.
    let returned = {};
    const listeners = group.snapshot();
    for (let i = 0; i < listeners.length; i++) {
        try {
            const value = listeners[i](data);
            if (value !== null && typeof value === 'object') returned = value;
        } catch (e) {
            console.error(event.name + ' listener error:', e);
        }
    }
    _answerReturned(replyId, returned);
}

// ---- synchronous getters -------------------------------------------------------

/** The JSON the host reported for the getter `name`, parsed; `fallback` when none. */
function ecosystemValue(name, fallback) {
    return _value(name, fallback);
}

function _value(name, fallback) {
    const json = op_ecosystem_value(name);
    if (json === null || json === undefined) return fallback;
    try { return JSON.parse(json); } catch (_) { return fallback; }
}

function getExptInfoSync(keys) {
    const all = _value('getExptInfoSync', {});
    if (!Array.isArray(keys)) return all !== null && typeof all === 'object' ? all : {};
    const picked = {};
    for (let i = 0; i < keys.length; i++) {
        if (all && Object.prototype.hasOwnProperty.call(all, keys[i])) picked[keys[i]] = all[keys[i]];
    }
    return picked;
}

function getExtConfigSync() {
    const config = _value('getExtConfigSync', {});
    return config !== null && typeof config === 'object' ? config : {};
}

function getOfficialComponentsInfo() {
    const info = _value('getOfficialComponentsInfo', {});
    return info !== null && typeof info === 'object' ? info : {};
}

function isChatTool() {
    return _value('isChatTool', false) === true;
}

// ---- the API content sees ------------------------------------------------------

const ecosystemApis = {};
for (let i = 0; i < ECOSYSTEM_CALLS.length; i++) {
    const name = ECOSYSTEM_CALLS[i];
    ecosystemApis[name] = { [name](options) { return _call(name, options); } }[name];
}
for (const name of Object.keys(ECOSYSTEM_EVENTS)) {
    const off = 'off' + name.slice(2);
    if (ECOSYSTEM_EVENTS[name] === 'resolve') {
        ecosystemApis[name] = { [name](listener) {
            if (typeof listener === 'function') _resolveListeners[name] = listener;
        } }[name];
        ecosystemApis[off] = { [off](listener) {
            if (listener === undefined || _resolveListeners[name] === listener) delete _resolveListeners[name];
        } }[off];
        continue;
    }
    const group = createListenerGroup(name);
    _listeners[name] = group;
    ecosystemApis[name] = { [name](listener) { group.on(listener); } }[name];
    ecosystemApis[off] = { [off](listener) { group.off(listener); } }[off];
}
ecosystemApis.getExptInfoSync = getExptInfoSync;
ecosystemApis.getExtConfigSync = getExtConfigSync;
ecosystemApis.getOfficialComponentsInfo = getOfficialComponentsInfo;
ecosystemApis.isChatTool = isChatTool;

export {
    ECOSYSTEM_CALLS,
    ECOSYSTEM_OBJECT_CALLS,
    ECOSYSTEM_EVENTS,
    ECOSYSTEM_OBJECT_EVENTS,
    ECOSYSTEM_VALUES,
    ecosystemApis,
    ecosystemAvailable,
    ecosystemObjectCall,
    ecosystemValue,
    onEcosystemObjectEvent,
    _internalOnEcosystemResult,
    _internalOnEcosystemEvent,
};
