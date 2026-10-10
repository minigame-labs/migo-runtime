import {
    createListenerGroup,
    isCallbackStyle,
    withCallbacks,
    withoutCallbacks,
} from "ext:host_v8_base/02_async.js";
import { bytesToHex, hexToBytes } from "ext:host_v8_system/00_host_binary.js";
import {
    ecosystemAvailable,
    ecosystemObjectCall,
    ecosystemValue,
    onEcosystemObjectEvent,
} from "ext:host_v8_system/20_ecosystem.js";

// The objects whose work is the host's ecosystem: the frame-sync game service
// (GameServerManager), the challenge board (RankManager), level and scene
// reporting (MiniReportManager, ScenePerformanceManager) and gifts
// (StoreGift). Each method is a request named `Class.method`, and each event
// one named the same way (20_ecosystem.js); what an object keeps for itself --
// the data an invitation carries, the scene info every report is merged with --
// is kept here.

function _noop() {}

// What a call that settles on a promise owes content when it fails before the
// request leaves: the same failure, by callback or by promise.
function _failNow(member, reason, options) {
    const failed = Promise.reject({ errMsg: member + ':fail ' + reason });
    withCallbacks(member, failed, options || {});
    if (isCallbackStyle(options)) {
        failed.then(undefined, _noop);
        return undefined;
    }
    return failed;
}

// A request that returns its promise whether or not content passed callbacks,
// as the frame-sync service's methods do; `shape` makes the result content's.
function _promised(name, options, shape) {
    const opts = options || {};
    let outcome = ecosystemObjectCall(name, withoutCallbacks(opts));
    if (shape !== undefined) outcome = outcome.then(shape);
    return withCallbacks(name.slice(name.indexOf('.') + 1), outcome, opts);
}

// The listener group behind one event and its on/off pair.
function _event(name, shape) {
    const group = createListenerGroup(name.slice(name.indexOf('.') + 1));
    onEcosystemObjectEvent(name, function (data) {
        group.trigger(shape === undefined ? data : shape(data));
    });
    return group;
}

// ---- GameServerManager ---------------------------------------------------------
//
// Frame actions are all strings or all binary, as the service is configured
// (lockStepOption.dataType). Binary actions cross as hex with `binary: true`
// beside the list, and a frame the host marks so reaches content as
// ArrayBuffers.

function _actionsOut(actionList) {
    if (!Array.isArray(actionList)) throw new TypeError('actionList must be an array');
    if (actionList.length === 0 || typeof actionList[0] === 'string') {
        for (let i = 0; i < actionList.length; i++) {
            if (typeof actionList[i] !== 'string') {
                throw new TypeError('actionList mixes strings and binary actions');
            }
        }
        return { actionList: actionList.slice(), binary: false };
    }
    const hex = new Array(actionList.length);
    for (let i = 0; i < actionList.length; i++) {
        hex[i] = bytesToHex(actionList[i]);
        if (hex[i] === null) throw new TypeError('actionList mixes strings and binary actions');
    }
    return { actionList: hex, binary: true };
}

function _frameIn(frame) {
    if (frame === null || typeof frame !== 'object') return frame;
    const binary = frame.binary === true;
    delete frame.binary;
    if (binary && Array.isArray(frame.actionList)) {
        frame.actionList = frame.actionList.map(function (action) {
            return hexToBytes(action) || new ArrayBuffer(0);
        });
    }
    return frame;
}

const GAME_SERVER_PROMISED = [
    'broadcastInRoom', 'cancelMatch', 'changeSeat', 'createRoom', 'endGame', 'endStateService',
    'getJoinVoIPChatSignature', 'getLastRoomInfo', 'getRoomInfo', 'joinRoom', 'kickoutMember',
    'login', 'logout', 'memberLeaveRoom', 'ownerLeaveRoom', 'reconnect', 'restart', 'setState',
    'startMatch', 'startStateService', 'updateReadyStatus',
];

const GAME_SERVER_EVENTS = [
    'BeKickedOut', 'Broadcast', 'Disconnect', 'GameEnd', 'GameStart', 'Invite', 'LockStepError',
    'Logout', 'Match', 'RoomInfoChange', 'StateUpdate', 'SyncFrame',
];

// The data every invitation carries, which the invited player's onInvite reads.
let _inviteData = '';

const _gameServerManager = {};
for (let i = 0; i < GAME_SERVER_PROMISED.length; i++) {
    const member = GAME_SERVER_PROMISED[i];
    _gameServerManager[member] = function (options) {
        return _promised('GameServerManager.' + member, options);
    };
}
// Settled by callback or by promise, as most calls are.
_gameServerManager.getFriendsStateData = function (options) {
    return ecosystemObjectCall('GameServerManager.getFriendsStateData', options);
};
_gameServerManager.startGame = function (options) {
    return ecosystemObjectCall('GameServerManager.startGame', options);
};
_gameServerManager.getLostFrames = function (options) {
    return _promised('GameServerManager.getLostFrames', options, function (res) {
        if (res.data && Array.isArray(res.data.frameList)) res.data.frameList.forEach(_frameIn);
        return res;
    });
};
_gameServerManager.uploadFrame = function (options) {
    const opts = options || {};
    let actions;
    try {
        actions = _actionsOut(opts.actionList);
    } catch (e) {
        return withCallbacks('uploadFrame', Promise.reject({ errMsg: 'uploadFrame:fail ' + e.message }), opts);
    }
    const outcome = ecosystemObjectCall('GameServerManager.uploadFrame',
        Object.assign(withoutCallbacks(opts), actions));
    return withCallbacks('uploadFrame', outcome, opts);
};
// No callback and no result: the invitation is the host's to deliver.
_gameServerManager.inviteFriend = function (options) {
    const opts = options || {};
    if (typeof opts.openId !== 'string' || opts.openId.length === 0) return;
    ecosystemObjectCall('GameServerManager.inviteFriend', { openId: opts.openId, data: _inviteData })
        .then(undefined, _noop);
};
_gameServerManager.setInviteData = function (data) {
    if (typeof data !== 'string') return false;
    _inviteData = data;
    return true;
};
for (let i = 0; i < GAME_SERVER_EVENTS.length; i++) {
    const event = GAME_SERVER_EVENTS[i];
    const group = _event('GameServerManager.on' + event, event === 'SyncFrame' ? _frameIn : undefined);
    _gameServerManager['on' + event] = function (listener) { group.on(listener); };
    _gameServerManager['off' + event] = function (listener) { group.off(listener); };
}

/** The one game service manager. */
function getGameServerManager() {
    return _gameServerManager;
}

// ---- RankManager ---------------------------------------------------------------
//
// A challenge the player accepted before the game listened is held, and handed
// to the first listener: content is told to listen early, and the platform
// waits for it to.

const _challengeListeners = [];
let _heldChallenge = null;

function _challengeStarted(data) {
    if (_challengeListeners.length === 0) {
        _heldChallenge = data;
        return;
    }
    const listeners = _challengeListeners.slice();
    for (let i = 0; i < listeners.length; i++) {
        try {
            listeners[i](data);
        } catch (e) {
            console.error('onChallengeStart listener error:', e);
        }
    }
}
onEcosystemObjectEvent('RankManager.onChallengeStart', _challengeStarted);

const _rankManager = {
    onChallengeStart(listener) {
        if (typeof listener !== 'function') return;
        _challengeListeners.push(listener);
        if (_heldChallenge !== null) {
            const held = _heldChallenge;
            _heldChallenge = null;
            _challengeStarted(held);
        }
    },
    offChallengeStart(listener) {
        if (typeof listener !== 'function') {
            _challengeListeners.length = 0;
            return;
        }
        const index = _challengeListeners.indexOf(listener);
        if (index !== -1) _challengeListeners.splice(index, 1);
    },
};
for (const member of ['abort', 'createChallenge', 'getScore', 'middleUpdate', 'update']) {
    _rankManager[member] = function (options) {
        return ecosystemObjectCall('RankManager.' + member, options);
    };
}

/** The one challenge-board manager. */
function getRankManager() {
    return _rankManager;
}

// ---- MiniReportManager / ScenePerformanceManager ---------------------------------

// A plain JSON copy of `value`, or null when it is not an object JSON can carry.
function _jsonObject(value) {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) return null;
    try {
        return JSON.parse(JSON.stringify(value));
    } catch (_) {
        return null;
    }
}

const _miniReportManager = {
    report(param) {
        const opts = param || {};
        if (typeof opts.eventID !== 'string' || opts.eventID.length === 0) {
            return _failNow('report', 'eventID is required', opts);
        }
        const fields = withoutCallbacks(opts);
        for (const key of Object.keys(fields)) {
            const value = fields[key];
            const ok = typeof value === 'string' || typeof value === 'boolean' ||
                (typeof value === 'number' && Number.isFinite(value));
            if (!ok && key !== '_timeout') return _failNow('report', 'invalid value for ' + key, opts);
        }
        return ecosystemObjectCall('MiniReportManager.report', opts);
    },
};

/**
 * The level-report manager. The host is told which events will be reported,
 * and content's callbacks hear whether it took them; the manager is returned
 * either way.
 */
function getMiniReportManager(param) {
    const opts = param || {};
    const eventList = Array.isArray(opts.eventList)
        ? opts.eventList.filter(function (id) { return typeof id === 'string'; })
        : [];
    const registered = ecosystemObjectCall('getMiniReportManager',
        { eventList: eventList, debug: opts.debug === true });
    registered.then(undefined, _noop);
    withCallbacks('getMiniReportManager', registered, opts);
    return _miniReportManager;
}

class ScenePerformanceManager {
    constructor(commonInfo) {
        this._commonInfo = commonInfo;
    }

    getCommonInfo() {
        return JSON.parse(JSON.stringify(this._commonInfo));
    }

    // Replaces what was set before, as a whole.
    setCommonInfo(params) {
        const info = _jsonObject(params);
        if (info !== null) this._commonInfo = info;
    }

    // Reported with the common info merged in; the scene's own fields win.
    setData(param) {
        const opts = param || {};
        if (typeof opts.sceneId !== 'number' || !Number.isFinite(opts.sceneId)) {
            return _failNow('setData', 'sceneId is required', opts);
        }
        const sceneData = Object.assign({}, this._commonInfo, _jsonObject(opts.sceneData) || {});
        return ecosystemObjectCall('ScenePerformanceManager.setData',
            Object.assign({}, opts, { sceneData: sceneData }));
    }
}

/**
 * A scene-performance manager over `commonInfo`. It reports to the host, so it
 * is ready -- content's callbacks hear success -- only when there is one.
 */
function getScenePerformanceManager(param) {
    const opts = param || {};
    const manager = new ScenePerformanceManager(_jsonObject(opts.commonInfo) || {});
    const ready = ecosystemAvailable()
        ? Promise.resolve({ errMsg: 'getScenePerformanceManager:ok' })
        : Promise.reject({ errMsg: 'getScenePerformanceManager:fail not supported' });
    ready.then(undefined, _noop);
    withCallbacks('getScenePerformanceManager', ready, opts);
    return manager;
}

// ---- StoreGift -------------------------------------------------------------------

// The common platform's code for "this environment cannot open gifts".
const GIFT_UNSUPPORTED = -1005;

class StoreGift {
    constructor(order) {
        this._order = order;
    }

    // Whether the host reported that it opens gifts.
    isSupported() {
        return ecosystemValue('StoreGift.isSupported', false) === true;
    }

    // Answers when the gift UI opened, not when the gift was taken.
    open() {
        if (!ecosystemAvailable()) {
            return Promise.reject({ errCode: GIFT_UNSUPPORTED, errMsg: 'open:fail not supported' });
        }
        return ecosystemObjectCall('StoreGift.open', Object.assign({}, this._order))
            .then(function () { return { errCode: 0, errMsg: 'ok' }; });
    }

    getOrderInfo() {
        return ecosystemObjectCall('StoreGift.getOrderInfo', Object.assign({}, this._order));
    }
}

/** A gift for the order `presentOrderId`, received by `openid`. */
function createStoreGift(options) {
    const opts = options || {};
    const order = {};
    if (typeof opts.presentOrderId === 'string') order.presentOrderId = opts.presentOrderId;
    if (typeof opts.openid === 'string') order.openid = opts.openid;
    return new StoreGift(order);
}

export {
    createStoreGift,
    getGameServerManager,
    getMiniReportManager,
    getRankManager,
    getScenePerformanceManager,
};
