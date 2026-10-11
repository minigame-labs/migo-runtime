// The native buttons -- createUserInfoButton, createOpenSettingButton,
// createGameClubButton, createFeedbackButton -- and
// getMenuButtonBoundingClientRect.
//
// A native button sits above the game at the rect its style names, in the
// window's CSS pixels. The engine draws it (05_button_layer.js) and hit-tests
// it: a touch or a mouse press that begins on a shown button is the button's,
// and content does not see it (01_touch.js, 03_mouse.js), as it would not see
// one on the common platform's native view. A button is pressed while any
// press that began on it lasts, and taps once when the last of them lifts on
// it -- one tap for one click, also from a host that delivers a mouse click as
// both a press of the mouse and a touch. What a tap does is real: the user-info button asks
// the host for the player's info and hands its answer to onTap, the setting
// button opens the settings, the game-club and feedback buttons ask the host to
// open its pages. A button is shown from creation until hide() or destroy(),
// and moving it is writing its style.

import { op_get_menu_button_rect } from "ext:core/ops";
import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import { _setTouchClaimer } from "ext:host_v8_touch/01_touch.js";
import { _setMouseClaimer } from "ext:host_v8_touch/03_mouse.js";
import { createImage } from "ext:host_v8_image/01_image.js";
import { attachButtonLayer, invalidateButtonLayer } from "ext:host_v8_ui/05_button_layer.js";
import { getUserInfo } from "ext:host_v8_system/13_login.js";
import { authorize, getSetting, openSetting } from "ext:host_v8_system/14_setting.js";
import { ecosystemObjectCall } from "ext:host_v8_system/20_ecosystem.js";

function _noop() {}

// ---- hit testing -------------------------------------------------------------

// Live buttons, in creation order: a later one is above an earlier one.
const _buttons = [];
attachButtonLayer(_buttons);
// Touch id -> the button it began on, until the touch ends.
const _claimed = new Map();
// Mouse button -> the native button its press began on, until it is released.
const _mouseClaimed = new Map();

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
            if (button !== null) {
                _claimed.set(id, button);
                button._pressBegan();
            }
        }
        const button = _claimed.get(id);
        if (button === undefined) return false;
        if (changed && (type === 'end' || type === 'cancel')) {
            _claimed.delete(id);
            button._pressEnded(type === 'end' && button._contains(touch.clientX, touch.clientY));
        }
        return true;
    },
});

// The mouse, as a desktop host delivers it. A press of the primary button on a
// shown button presses it; any press that began on one, and the pointer moving
// over one, are the button's and not content's -- as over a native view.
_setMouseClaimer({
    active() {
        return _buttons.length > 0 || _mouseClaimed.size > 0;
    },
    down(x, y, mouseButton) {
        const button = _buttonAt(x, y);
        if (button === null) return false;
        if (!_mouseClaimed.has(mouseButton)) {
            _mouseClaimed.set(mouseButton, button);
            if (mouseButton === 0) button._pressBegan();
        }
        return true;
    },
    move(x, y) {
        return _mouseClaimed.size > 0 || _buttonAt(x, y) !== null;
    },
    up(x, y, mouseButton) {
        const button = _mouseClaimed.get(mouseButton);
        if (button === undefined) return _buttonAt(x, y) !== null;
        _mouseClaimed.delete(mouseButton);
        if (mouseButton === 0) button._pressEnded(button._contains(x, y));
        return true;
    },
});

// ---- NativeButton --------------------------------------------------------------

const STYLE_DEFAULTS = {
    left: 0, top: 0, width: 0, height: 0,
    backgroundColor: '', borderColor: '', borderWidth: 0, borderRadius: 0,
    color: '#ffffff', textAlign: 'center', fontSize: 16, lineHeight: 0,
};

// The style a button is drawn and hit-tested by. Each property is live: writing
// one moves or restyles the button in the engine's next frame.
function _liveStyle(given) {
    const values = Object.assign({}, STYLE_DEFAULTS, given || {});
    const style = {};
    const keys = Object.keys(values);
    for (let i = 0; i < keys.length; i++) {
        const key = keys[i];
        Object.defineProperty(style, key, {
            enumerable: true,
            get() { return values[key]; },
            set(value) {
                values[key] = value;
                invalidateButtonLayer();
            },
        });
    }
    return style;
}

// A finite number from a style value, or `fallback`.
function _number(value, fallback) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
}

class NativeButton {
    #type;
    #text;
    #image;
    #picture = null;
    #style;
    #visible = true;
    #destroyed = false;
    #presses = 0;
    #tapListeners;

    constructor(label, options) {
        const opts = options || {};
        this.#type = opts.type === 'image' ? 'image' : 'text';
        this.#text = typeof opts.text === 'string' ? opts.text : '';
        this.#image = '';
        this.#style = _liveStyle(opts.style);
        this.#tapListeners = createListenerGroup(label + ' onTap');
        _buttons.push(this);
        this.image = typeof opts.image === 'string' ? opts.image : '';
        invalidateButtonLayer();
    }

    get style() { return this.#style; }

    get type() { return this.#type; }
    set type(value) {
        this.#type = value === 'image' ? 'image' : 'text';
        invalidateButtonLayer();
    }

    get text() { return this.#text; }
    set text(value) {
        this.#text = value === undefined || value === null ? '' : String(value);
        invalidateButtonLayer();
    }

    // The picture an image button shows, loaded by the engine like any image;
    // the button is drawn again once it arrives.
    get image() { return this.#image; }
    set image(value) {
        const path = value === undefined || value === null ? '' : String(value);
        if (path === this.#image) return;
        this.#image = path;
        this.#picture = null;
        if (path !== '') {
            const picture = createImage();
            const self = this;
            picture.onload = function () {
                if (self.#picture === picture) invalidateButtonLayer();
            };
            picture.src = path;
            this.#picture = picture;
        }
        invalidateButtonLayer();
    }

    show() {
        if (!this.#destroyed && !this.#visible) {
            this.#visible = true;
            invalidateButtonLayer();
        }
    }

    hide() {
        if (!this.#destroyed && this.#visible) {
            this.#visible = false;
            invalidateButtonLayer();
        }
    }

    destroy() {
        if (this.#destroyed) return;
        this.#destroyed = true;
        this.#visible = false;
        this.#picture = null;
        this.#tapListeners.off();
        const index = _buttons.indexOf(this);
        if (index !== -1) _buttons.splice(index, 1);
        invalidateButtonLayer();
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

    // A press that began on the button; `_pressEnded` when it lifts or is
    // cancelled. The last to lift taps, if it lifted on the button -- after the
    // event that lifted it, as a view's click follows the pointer's release.
    _pressBegan() {
        this.#presses++;
    }

    _pressEnded(onButton) {
        if (this.#presses > 0) this.#presses--;
        if (this.#presses === 0 && onButton) {
            const self = this;
            queueMicrotask(function () { self._tapped(); });
        }
    }

    _tapped() {
        if (!this.#destroyed) this._act();
    }

    // Where the button is drawn, in the window's CSS pixels, or null when
    // nothing of it is.
    _paintBounds() {
        if (!this.#visible || this.#destroyed) return null;
        const style = this.#style;
        const width = _number(style.width, 0);
        const height = _number(style.height, 0);
        if (!(width > 0 && height > 0)) return null;
        return { left: _number(style.left, 0), top: _number(style.top, 0), width: width, height: height };
    }

    // Draw the button into the layer's context, whose transform maps the
    // window's CSS pixels: background, the picture or text, then the border,
    // inside the rect.
    _paint(context) {
        const bounds = this._paintBounds();
        if (bounds === null) return;
        const style = this.#style;
        const { left, top, width, height } = bounds;
        const border = Math.max(0, _number(style.borderWidth, 0));
        const radius = Math.max(0, Math.min(_number(style.borderRadius, 0), width / 2, height / 2));
        const background = style.backgroundColor;
        if (typeof background === 'string' && background !== '') {
            context.fillStyle = background;
            context.beginPath();
            context.roundRect(left, top, width, height, radius);
            context.fill();
        }
        if (this.#type === 'image') {
            context.save();
            context.beginPath();
            context.roundRect(left, top, width, height, radius);
            context.clip();
            this._paintPicture(context, bounds, this.#picture);
            context.restore();
        } else if (this.#text !== '') {
            const fontSize = Math.max(1, _number(style.fontSize, 16));
            const lineHeight = _number(style.lineHeight, 0);
            const align = style.textAlign === 'left' || style.textAlign === 'right' ? style.textAlign : 'center';
            context.font = fontSize + 'px sans-serif';
            context.fillStyle = typeof style.color === 'string' && style.color !== '' ? style.color : '#ffffff';
            context.textAlign = align;
            context.textBaseline = 'middle';
            const x = align === 'left' ? left + border : align === 'right' ? left + width - border : left + width / 2;
            // One line, centred in its line box: the first `lineHeight` of the
            // button when one is set, else the whole of it.
            const y = top + (lineHeight > 0 ? Math.min(lineHeight, height) : height) / 2;
            context.fillText(this.#text, x, y);
        }
        const borderColor = style.borderColor;
        if (border > 0 && typeof borderColor === 'string' && borderColor !== '') {
            context.lineWidth = border;
            context.strokeStyle = borderColor;
            context.beginPath();
            context.roundRect(left + border / 2, top + border / 2,
                Math.max(0, width - border), Math.max(0, height - border), Math.max(0, radius - border / 2));
            context.stroke();
        }
    }

    // An image button's picture, stretched over the button once it has loaded.
    _paintPicture(context, bounds, picture) {
        if (picture !== null && picture.complete && picture.width > 0) {
            context.drawImage(picture, bounds.left, bounds.top, bounds.width, bounds.height);
        }
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
    #icon = 'green';
    #openlink;
    #hasRedDot;

    constructor(options) {
        const opts = options || {};
        super('GameClubButton', Object.assign({ type: 'image' }, opts));
        this.icon = opts.icon;
        this.#openlink = typeof opts.openlink === 'string' ? opts.openlink : '';
        this.#hasRedDot = opts.hasRedDot !== false;
    }

    get icon() { return this.#icon; }
    set icon(value) {
        this.#icon = typeof value === 'string' && value !== '' ? value : 'green';
        invalidateButtonLayer();
    }

    // Without a picture of its own, a neutral mark of two people in the tone
    // `icon` names; the platform's own artwork is not the engine's to draw.
    _paintPicture(context, bounds, picture) {
        if (picture !== null) {
            super._paintPicture(context, bounds, picture);
            return;
        }
        const tones = {
            green: ['#2ba245', '#ffffff'],
            white: ['rgba(0,0,0,0)', '#ffffff'],
            dark: ['#2c2c2c', '#ffffff'],
            light: ['#ffffff', '#2ba245'],
        };
        const [disc, mark] = tones[this.#icon] || tones.green;
        const size = Math.min(bounds.width, bounds.height);
        const cx = bounds.left + bounds.width / 2;
        const cy = bounds.top + bounds.height / 2;
        context.fillStyle = disc;
        context.beginPath();
        context.arc(cx, cy, size / 2, 0, Math.PI * 2);
        context.fill();
        context.fillStyle = mark;
        for (const [dx, scale] of [[-0.16, 0.85], [0.14, 1]]) {
            const x = cx + dx * size;
            const head = 0.1 * size * scale;
            context.beginPath();
            context.arc(x, cy - 0.08 * size * scale, head, 0, Math.PI * 2);
            context.fill();
            context.beginPath();
            context.ellipse(x, cy + 0.2 * size * scale, head * 1.9, head * 1.4, 0, Math.PI, 0);
            context.fill();
        }
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
