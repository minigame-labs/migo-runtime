// getSetting / authorize / openSetting
//
// Maintains an in-memory authSetting map. getSetting and authorize are pure JS.
// openSetting delegates to the host via op (Mode C) so the native settings UI
// can be shown; falls back to returning current state if no host op available.

import { op_open_setting, op_get_auth_setting, op_authorize } from "ext:core/ops";
import { wrapAsync, createDeferredApi, isCallbackStyle } from "ext:host_v8_base/02_async.js";

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

const _authorizeApi = createDeferredApi('authorize');

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

// ---- openSetting (Mode C - host op) ---------------------------------------

const _openSettingApi = createDeferredApi('openSetting');

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
