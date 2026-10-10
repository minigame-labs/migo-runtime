// The native buttons -- createUserInfoButton, createOpenSettingButton,
// createGameClubButton, createFeedbackButton -- and
// getMenuButtonBoundingClientRect.
//
// A native button sits above the game at the rect its style names, in the
// window's CSS pixels. The engine hit-tests it: a touch that begins on a shown
// button is the button's, and content does not see it (01_touch.js), as it
// would not see a touch on the common platform's native view; the touch ending
// on the button is a tap. What a tap does is real: the user-info button asks
// the host for the player's info and hands its answer to onTap, the setting
// button opens the settings, the game-club and feedback buttons ask the host to
// open its pages. A button is shown from creation until hide() or destroy(),
// and moving it is writing its style.

import { op_get_menu_button_rect } from "ext:core/ops";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import { _setTouchClaimer } from "ext:host_v8_touch/01_touch.js";
import { getUserInfo } from "ext:host_v8_system/13_login.js";
import { authorize, getSetting, openSetting } from "ext:host_v8_system/14_setting.js";
import { ecosystemObjectCall } from "ext:host_v8_system/20_ecosystem.js";

function _noop() {}

// ---- hit testing -------------------------------------------------------------

// Live buttons, in creation order: a later one is above an earlier one.
const _buttons = [];
// Touch id -> the button it began on, until the touch ends.
const _claimed = new Map();

function _buttonAt(x, y) {
    for (let i = _buttons.length - 1; i >= 0; i--) {
        if (_buttons[i]._contains(x, y)) return _buttons[i];
    }
    return null;
}

_setTouchClaimer({
    active() {
        return _buttons.length > 0 || _claimed.size > 0;
    },
    claims(type, touch, changed) {
        const id = touch.identifier;
        if (type === 'start' && changed && !_claimed.has(id)) {
            const button = _buttonAt(touch.clientX, touch.clientY);
            if (button !== null) _claimed.set(id, button);
        }
        const button = _claimed.get(id);
        if (button === undefined) return false;
        if (changed && (type === 'end' || type === 'cancel')) {
            _claimed.delete(id);
            // Lifted on the button it began on: a tap. Dispatched after the
            // touch event, as a view's click is.
            if (type === 'end' && button._contains(touch.clientX, touch.clientY)) {
                queueMicrotask(function () { button._tapped(); });
            }
        }
        return true;
    },
});

// ---- NativeButton --------------------------------------------------------------

const STYLE_DEFAULTS = {
    left: 0, top: 0, width: 0, height: 0,
    backgroundColor: '', borderColor: '', borderWidth: 0, borderRadius: 0,
    color: '#ffffff', textAlign: 'center', fontSize: 16, lineHeight: 0,
};

class NativeButton {
    #style;
    #visible = true;
    #destroyed = false;
    #tapListeners;

    constructor(label, options) {
        const opts = options || {};
        this.type = opts.type === 'image' ? 'image' : 'text';
        this.text = typeof opts.text === 'string' ? opts.text : '';
        this.image = typeof opts.image === 'string' ? opts.image : '';
        this.#style = Object.assign({}, STYLE_DEFAULTS, opts.style || {});
        this.#tapListeners = createListenerGroup(label + ' onTap');
        _buttons.push(this);
    }

    get style() { return this.#style; }

    show() {
        if (!this.#destroyed) this.#visible = true;
    }

    hide() {
        if (!this.#destroyed) this.#visible = false;
    }

    destroy() {
        if (this.#destroyed) return;
        this.#destroyed = true;
        this.#visible = false;
        this.#tapListeners.off();
        const index = _buttons.indexOf(this);
        if (index !== -1) _buttons.splice(index, 1);
    }

    onTap(listener) {
        if (!this.#destroyed) this.#tapListeners.on(listener);
    }

    offTap(listener) {
        this.#tapListeners.off(listener);
    }

    // Whether the window point (x, y) is on the shown button.
    _contains(x, y) {
        if (!this.#visible || this.#destroyed) return false;
        const style = this.#style;
        const left = Number(style.left);
        const top = Number(style.top);
        return x >= left && x < left + Number(style.width) &&
            y >= top && y < top + Number(style.height);
    }

    _tapped() {
        if (!this.#destroyed) this._act();
    }

    _emitTap(res) {
        if (!this.#destroyed) this.#tapListeners.trigger(res);
    }

    // What the button does when tapped; each kind overrides it.
    _act() {
        this._emitTap();
    }
}

// The player's info, as the host's getUserInfo answers -- or its failure, an
// `errMsg` content tells apart by `:fail`, as on the common platform. The tap
// is the player's consent gesture: when scope.userInfo is not yet granted the
// host is asked for it first, as the platform prompts on a first tap.
class UserInfoButton extends NativeButton {
    #withCredentials;
    #lang;

    constructor(options) {
        super('UserInfoButton', options);
        const opts = options || {};
        this.#withCredentials = opts.withCredentials !== false;
        this.#lang = typeof opts.lang === 'string' ? opts.lang : 'en';
    }

    _act() {
        const emit = (res) => this._emitTap(res);
        const fetch = () => getUserInfo({ withCredentials: this.#withCredentials, lang: this.#lang })
            .then(emit, emit);
        getSetting().then((setting) => {
            if (setting.authSetting['scope.userInfo'] === true) return fetch();
            return authorize({ scope: 'scope.userInfo' }).then(fetch, (refused) => {
                const reason = String(refused.errMsg).replace(/^authorize:fail ?/, '');
                emit({ errMsg: 'getUserInfo:fail ' + reason });
            });
        });
    }
}

class OpenSettingButton extends NativeButton {
    constructor(options) {
        super('OpenSettingButton', options);
    }

    _act() {
        this._emitTap();
        openSetting({}).then(undefined, _noop);
    }
}

class GameClubButton extends NativeButton {
    #openlink;
    #hasRedDot;

    constructor(options) {
        const opts = options || {};
        super('GameClubButton', Object.assign({ type: 'image' }, opts));
        this.icon = typeof opts.icon === 'string' ? opts.icon : 'green';
        this.#openlink = typeof opts.openlink === 'string' ? opts.openlink : '';
        this.#hasRedDot = opts.hasRedDot !== false;
    }

    // The host's game club, opened at the post or page `openlink` names.
    _act() {
        this._emitTap();
        const request = this.#openlink ? { openlink: this.#openlink, hasRedDot: this.#hasRedDot } : {};
        ecosystemObjectCall('GameClubButton.open', request).then(undefined, _noop);
    }
}

class FeedbackButton extends NativeButton {
    constructor(options) {
        super('FeedbackButton', options);
    }

    // The host's feedback page.
    _act() {
        this._emitTap();
        ecosystemObjectCall('FeedbackButton.open', {}).then(undefined, _noop);
    }
}

function createUserInfoButton(options) {
    return new UserInfoButton(options);
}

function createOpenSettingButton(options) {
    return new OpenSettingButton(options);
}

function createGameClubButton(options) {
    return new GameClubButton(options);
}

function createFeedbackButton(options) {
    return new FeedbackButton(options);
}

// ---- getMenuButtonBoundingClientRect --------------------------------------------

// The host's menu button, or -- with no host to ask -- none: an empty rect.
function getMenuButtonBoundingClientRect() {
    try {
        return JSON.parse(op_get_menu_button_rect());
    } catch (_) {
        return { width: 0, height: 0, top: 0, bottom: 0, left: 0, right: 0 };
    }
}

export {
    createUserInfoButton,
    createOpenSettingButton,
    createGameClubButton,
    createFeedbackButton,
    getMenuButtonBoundingClientRect,
};
