// getSetting / authorize / openSetting
//
// Maintains an in-memory authSetting map. getSetting and authorize are pure JS.
// openSetting delegates to the host via op (Mode C) so the native settings UI
// can be shown; falls back to returning current state if no host op available.

import { op_open_setting, op_get_auth_setting, op_authorize } from "ext:core/ops";
import {
    wrapAsync,
    createDeferredApi,
    isCallbackStyle,
    errorMessage,
    setScopeAsker,
} from "ext:host_v8_base/02_async.js";

// ---- authorisation state ---------------------------------------------------
//
// The host owns this. There is no local map seeded with defaults: the previous
// one initialised every scope to `true`, so `migo.getSetting()` told content it
// held permissions nobody had granted, and a game that checked before acting
// was misled precisely because it checked.
//
// Nothing is cached here either. The host may revise a decision at any time --
// the user can revoke in system settings while the game runs -- and a cache
// would answer from a snapshot of when the game started. `op_get_auth_setting`
// reads the host's current answer.

function _cloneAuthSetting() {
    try {
        return JSON.parse(op_get_auth_setting());
    } catch (_) {
        // A malformed reply is not evidence of a grant.
        return {};
    }
}

// ---- getSetting ------------------------------------------------------------

function getSetting(options) {
    return wrapAsync('getSetting', function () {
        return { authSetting: _cloneAuthSetting() };
    }, options);
}

// ---- authorize -------------------------------------------------------------

// ---- authorize (Mode C - host decides) -------------------------------------
//
// Asks the host, which may put the question to the user. The previous version
// set its own map entry and returned success: an API whose entire purpose is to
// obtain consent, obtaining none.

// Timeout disabled: the player answers the host's question when they do.
const _authorizeApi = createDeferredApi('authorize', 0);

function authorize(options) {
    const opts = options || {};
    const scope = opts.scope;
    if (typeof scope !== 'string' || scope.length === 0) {
        return wrapAsync('authorize', function () {
            throw new Error('scope is required');
        }, options);
    }
    return _authorizeApi.invoke(options, function (o, requestId) {
        op_authorize(JSON.stringify({
            requestId: requestId,
            scope: scope,
            // The reason text the game declared in game.json. A host cannot
            // write an honest prompt without it; empty when none was declared.
            desc: typeof o.desc === 'string' ? o.desc : '',
        }));
    });
}

function _internalOnAuthorizeResult(resultJson) {
    _authorizeApi.settle(resultJson);
}

// ---- asking as a call needs ------------------------------------------------
//
// A call that needs a scope nobody has decided asks for it as authorize does
// (base/02_async.js, setScopeAsker): authorize is how content asks in advance.
// scope.userInfo is not asked this way -- in a mini-game it is asked through a
// user-info button, and authorize does not prompt for it. One ask per scope at a
// time: calls made while the player is asked share the answer.

const _SCOPE_REFUSAL = /^auth deny: (scope\.[A-Za-z]+) is not granted$/;
const _asking = new Map();

function _askForScope(error) {
    const refusal = _SCOPE_REFUSAL.exec(errorMessage(error));
    if (refusal === null) return null;
    const scope = refusal[1];
    if (scope === 'scope.userInfo') return null;
    const pending = _asking.get(scope);
    if (pending !== undefined) return pending;
    // Decided, and so refused: asked no more, as on the platform.
    if (Object.prototype.hasOwnProperty.call(_cloneAuthSetting(), scope)) return null;
    let decided;
    const asking = new Promise(function (resolve) { decided = resolve; });
    // Held before asking: an authorize that fails at once completes at once.
    _asking.set(scope, asking);
    authorize({
        scope: scope,
        complete: function () {
            _asking.delete(scope);
            decided();
        },
    });
    return asking;
}

setScopeAsker(_askForScope);

// ---- openSetting (Mode C - host op) ---------------------------------------

// Timeout disabled: the player is in the settings page as long as they like.
const _openSettingApi = createDeferredApi('openSetting', 0);

function openSetting(options) {
    return _openSettingApi.invoke(options, function (opts, requestId) {
        op_open_setting(JSON.stringify({ requestId: requestId }));
    });
}

function _internalOnOpenSettingResult(resultJson) {
    var parsed;
    try { parsed = JSON.parse(resultJson); } catch (_) { parsed = {}; }
    if (parsed === null || typeof parsed !== 'object') parsed = {};
    // openSetting answers with the settings as the player left them. Read
    // from the host's standing decisions -- what getSetting reads -- rather
    // than from the reply, so the two cannot disagree; a host that reports
    // each decision as it changes has already recorded what was toggled.
    if (!parsed.error) parsed.authSetting = _cloneAuthSetting();
    _openSettingApi.settleParsed(parsed);
}

// ---- host-side helpers -----------------------------------------------------

// Called from Rust / EvalScript to update a specific scope's auth state.
//   _internalUpdateAuthSetting('scope.userLocation', false)
// Retained for the host-bridge surface, but it no longer stores anything:
// permission state lives with the host and `getSetting` reads it there. It used
// to write a local map that `getSetting` returned, which made the state
// writable by anything that could reach the bridge -- and the bridge holder is
// reachable from content (`Symbol.for` uses the global registry). A permission
// answer content can write is not a permission answer.
function _internalUpdateAuthSetting(_scope, _authorized) {}

// The privacy, subscription and private-message APIs are the host's to answer:
// see 20_ecosystem.js.

function checkUserLocation(options) {
    return wrapAsync('checkUserLocation', function () {
        var allowed = !!_cloneAuthSetting()['scope.userLocation'];
        return {
            authSetting: {
                'scope.userLocation': allowed,
            },
            hasLocationPer: allowed,
        };
    }, options);
}

// Asks for the permission, as `authorize` does: the host decides, and may put the question to the user. This used
// to set its own map entry to true and answer success, which is the self-grant the host-owns-it design removed
// (the map went, this line was left behind, and every call threw a ReferenceError instead).
function getWritePhotosAlbum(options) {
    var opts = options || {};
    // `authorize` answers `authorize:ok` / `authorize:fail ...`: the caller asked for this API.
    var named = function (res) {
        if (res && typeof res.errMsg === 'string') {
            var colon = res.errMsg.indexOf(':');
            return Object.assign({}, res, { errMsg: 'getWritePhotosAlbum' + res.errMsg.slice(colon) });
        }
        return res;
    };
    var wrap = function (cb) {
        return typeof cb === 'function' ? function (res) { cb(named(res)); } : undefined;
    };
    if (isCallbackStyle(opts)) {
        authorize({
            scope: 'scope.writePhotosAlbum',
            success: wrap(opts.success),
            fail: wrap(opts.fail),
            complete: wrap(opts.complete),
        });
        return undefined;
    }
    return authorize({ scope: 'scope.writePhotosAlbum' })
        .then(named, function (res) { throw named(res); });
}

function checkWritePhotosAlbum(options) {
    return wrapAsync('checkWritePhotosAlbum', function () {
        var allowed = !!_cloneAuthSetting()['scope.writePhotosAlbum'];
        return {
            authSetting: {
                'scope.writePhotosAlbum': allowed,
            },
            hascheckWritePhotosAlbum: allowed,
        };
    }, options);
}

export {
    getSetting,
    authorize,
    openSetting,
    _internalOnOpenSettingResult,
    _internalOnAuthorizeResult,
    _internalUpdateAuthSetting,
    checkUserLocation,
    getWritePhotosAlbum,
    checkWritePhotosAlbum,
};
