import { createListenerGroup } from "ext:host_v8_base/02_async.js";

function createDefaultLaunchOptions() {
    return {
        scene: 1001,
        query: {},
        shareTicket: "",
        referrerInfo: {
            appId: "",
            extraData: {},
            chatType: 0,
        },
    };
}

function deepCloneValue(val) {
    if (val === null || typeof val !== "object") return val;
    if (Array.isArray(val)) {
        const arr = [];
        for (let i = 0; i < val.length; i++) arr[i] = deepCloneValue(val[i]);
        return arr;
    }
    const out = {};
    const keys = Object.keys(val);
    for (let i = 0; i < keys.length; i++) {
        out[keys[i]] = deepCloneValue(val[keys[i]]);
    }
    return out;
}

function parseQueryString(queryString) {
    const query = {};
    if (typeof queryString !== "string" || queryString.length === 0) {
        return query;
    }

    const source = queryString.startsWith("?") ? queryString.slice(1) : queryString;
    if (!source) {
        return query;
    }

    const pairs = source.split("&");
    for (let i = 0; i < pairs.length; i++) {
        const pair = pairs[i];
        if (!pair) continue;

        const index = pair.indexOf("=");
        const keyPart = index >= 0 ? pair.slice(0, index) : pair;
        const valuePart = index >= 0 ? pair.slice(index + 1) : "";

        let key = keyPart;
        let value = valuePart;

        try {
            key = decodeURIComponent(keyPart);
        } catch (_) {
            key = keyPart;
        }

        try {
            value = decodeURIComponent(valuePart);
        } catch (_) {
            value = valuePart;
        }

        if (!key) continue;
        query[key] = value;
    }

    return query;
}

function deepCloneOptions(opts) {
    return deepCloneValue(opts);
}

class AppLifecycleManager {
    constructor() {
        this.onShowListeners = createListenerGroup('onShow');
        this.onHideListeners = createListenerGroup('onHide');
        this.isVisible = false;
        // Cold launch params -- set once on first onShow, never overwritten
        this.launchOptions = null;
        // Most recent enter params -- updated on every onShow
        this.enterOptions = null;
        this._firstShow = true;
    }

    _normalizeLaunchOptions(raw) {
        const normalized = createDefaultLaunchOptions();
        if (!raw || typeof raw !== "object") {
            return normalized;
        }

        if (Number.isFinite(raw.scene)) {
            normalized.scene = Math.trunc(raw.scene);
        }

        if (typeof raw.shareTicket === "string") {
            normalized.shareTicket = raw.shareTicket;
        }

        if (typeof raw.query === "string") {
            normalized.query = parseQueryString(raw.query);
        } else if (raw.query && typeof raw.query === "object") {
            normalized.query = deepCloneValue(raw.query);
        }

        const referrer = raw.referrerInfo;
        if (referrer && typeof referrer === "object") {
            if (typeof referrer.appId === "string") {
                normalized.referrerInfo.appId = referrer.appId;
            }

            if (referrer.extraData && typeof referrer.extraData === "object") {
                normalized.referrerInfo.extraData = deepCloneValue(referrer.extraData);
            }

            if (Number.isFinite(referrer.chatType)) {
                normalized.referrerInfo.chatType = Math.trunc(referrer.chatType);
            }
        }

        return normalized;
    }

    onShow(listener) {
        this.onShowListeners.on(listener);
    }

    onHide(listener) {
        this.onHideListeners.on(listener);
    }

    offShow(listener) {
        this.onShowListeners.off(listener);
    }

    offHide(listener) {
        this.onHideListeners.off(listener);
    }

    _triggerShow(options) {
        this.isVisible = true;

        const normalized = this._normalizeLaunchOptions(options);

        // First onShow = cold launch: freeze launch options
        if (this._firstShow) {
            this.launchOptions = normalized;
            this._firstShow = false;
        }

        // Always update enter options (tracks most recent enter)
        this.enterOptions = normalized;

        // Pass a separate clone to listeners so they cannot mutate internal state
        const listenerOptions = deepCloneOptions(normalized);
        this.onShowListeners.trigger(listenerOptions);
    }

    _triggerHide() {
        this.isVisible = false;
        this.onHideListeners.trigger();
    }

    getLaunchOptionsSync() {
        return deepCloneOptions(this.launchOptions || createDefaultLaunchOptions());
    }

    getEnterOptionsSync() {
        return deepCloneOptions(this.enterOptions || this.launchOptions || createDefaultLaunchOptions());
    }
}

const lifecycleManager = new AppLifecycleManager();

function onShow(listener) {
    lifecycleManager.onShow(listener);
}

function onHide(listener) {
    lifecycleManager.onHide(listener);
}

function offShow(listener) {
    lifecycleManager.offShow(listener);
}

function offHide(listener) {
    lifecycleManager.offHide(listener);
}

function getLaunchOptionsSync() {
    return lifecycleManager.getLaunchOptionsSync();
}

function getEnterOptionsSync() {
    return lifecycleManager.getEnterOptionsSync();
}

function _internalTriggerOnShow(option) {
    lifecycleManager._triggerShow(option);
}

function _internalTriggerOnHide() {
    lifecycleManager._triggerHide();
}

// Focus is independent of visibility: a desktop window can lose keyboard
// focus while remaining visible and rendering. Core retains the level and a
// single profile callback. An HTML5 prelude maps this to focus/blur; the common mini-game platform leaves
// the adapter unset because it has no matching public API.
let _focused = false;
let _focusAdapter = null;

function _internalTriggerFocusChanged(focused) {
    _focused = focused === true;
    if (_focusAdapter !== null) {
        try {
            _focusAdapter(_focused);
        } catch (e) {
            console.error('focus adapter threw: ' + e);
        }
    }
}

function _internalInstallFocusAdapter(adapter) {
    if (adapter !== null && typeof adapter !== 'function') {
        throw new TypeError('focus adapter must be a function or null');
    }
    _focusAdapter = adapter;
    if (_focusAdapter !== null) _focusAdapter(_focused);
}

function _internalGetFocusState() {
    return _focused;
}

export {
    onShow,
    onHide,
    offShow,
    offHide,
    getLaunchOptionsSync,
    getEnterOptionsSync,
    _internalTriggerOnHide,
    _internalTriggerOnShow,
    _internalTriggerFocusChanged,
    _internalInstallFocusAdapter,
    _internalGetFocusState,
};
