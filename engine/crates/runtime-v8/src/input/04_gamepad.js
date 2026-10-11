import { op_gamepad_rumble } from "ext:core/ops";
import { domException } from "ext:host_v8_base/06_dom_exception.js";
import { _internalObserveVisibility } from "ext:host_v8_lifecycle/01_lifecycle.js";
import { pageTime } from "ext:host_v8_touch/00_input_clock.js";
import { clearTimeout, setTimeout } from "ext:host_v8_web/02_timers.js";
// The gamepad API: getGamepads(), onGamepadConnected / onGamepadDisconnected
// and their off*, as the common mini-game platform has them -- and its pads
// follow the browser standard, the W3C Gamepad API, which content running in a
// WebView already reads through navigator.getGamepads().
//
// The API is POLLED, not evented: content calls getGamepads() every frame and
// reads whatever is current. So state lives here and the host pushes updates
// into it, rather than each sample being dispatched to a listener that most
// content would not have.
//
// Each pad also has the standard's vibrationActuator. The engine times its
// effects -- start delay, end, preemption -- and settles their promises; the
// host is only told what to set the motors to, and for how long.

// Capture the intrinsics before game code can replace Array/Object methods.
// Native input triggers run after untrusted content has started.
const SafeArray = Array;
const ObjectDefineProperty = Object.defineProperty;
const ObjectDefineProperties = Object.defineProperties;
const ObjectFreeze = Object.freeze;
const ArraySlice = Function.prototype.call.bind(Array.prototype.slice);
const ArrayIndexOf = Function.prototype.call.bind(Array.prototype.indexOf);
const ArrayPush = Function.prototype.call.bind(Array.prototype.push);
const ArraySplice = Function.prototype.call.bind(Array.prototype.splice);
const ConsoleError = Function.prototype.call.bind(console.error, console);
const SafePromise = Promise;
const PromiseResolve = Promise.resolve.bind(Promise);
const PromiseReject = Promise.reject.bind(Promise);
const SafeTypeError = TypeError;
const SafeString = String;
const NumberIsFinite = Number.isFinite;
const MathMin = Math.min;
const MathTrunc = Math.trunc;

const _gamepads = [];
const _gamepadStates = [];
const _connectedListeners = [];
const _disconnectedListeners = [];

// The Web API's slot semantics: an unavailable pad leaves an explicit `null`
// slot rather than shifting the ones after it, because content holds on to an
// index. Sparse array holes are not equivalent: indexed reads expose them as
// `undefined`, which violates the Gamepad API contract.
function getGamepads() {
    return ArraySlice(_gamepads);
}

// ==================== Haptics ====================
//
// The W3C Gamepad API's GamepadHapticActuator. Which effects a pad plays is
// the host's to say when it announces the pad (a GAMEPAD_HAPTICS_* bit each);
// an actuator with none still exists and rejects every effect, as the
// standard has it.

const HAPTICS_DUAL_RUMBLE = 1;
const HAPTICS_TRIGGER_RUMBLE = 2;
const HAPTICS_KNOWN = HAPTICS_DUAL_RUMBLE | HAPTICS_TRIGGER_RUMBLE;
// The longest an effect plays, its delay included: the standard's recommended
// limit, so an effect content forgets to stop cannot run on. The engine's
// GAMEPAD_EFFECT_MAX_MS, which refuses anything longer.
const EFFECT_MAX_MS = 5000;

// Whether the game is in the background. The standard rejects effects then
// and preempts the one playing when it goes there.
let _hidden = false;

function _effectBit(type) {
    return type === 'dual-rumble' ? HAPTICS_DUAL_RUMBLE : HAPTICS_TRIGGER_RUMBLE;
}

// WebIDL `unsigned long long` without [EnforceRange]: non-finite is 0, and a
// negative wraps modulo 2^64 -- past any limit, which is what it then meets.
function _unsignedLongLong(value) {
    if (value === undefined) return 0;
    const number = +value;
    if (!NumberIsFinite(number)) return 0;
    const whole = MathTrunc(number);
    return whole < 0 ? 18446744073709551616 + whole : whole;
}

// WebIDL `double`: a non-finite value is a TypeError.
function _double(value, member) {
    if (value === undefined) return 0;
    const number = +value;
    if (!NumberIsFinite(number)) {
        throw new SafeTypeError("Failed to read the '" + member +
            "' property from 'GamepadEffectParameters': The provided double value is non-finite.");
    }
    return number;
}

// GamepadEffectParameters, its members read in the dictionary's (sorted) order.
function _effectParameters(params) {
    if (params === undefined || params === null) params = {};
    else if (typeof params !== 'object' && typeof params !== 'function') {
        throw new SafeTypeError("The provided value is not of type 'GamepadEffectParameters'.");
    }
    const duration = _unsignedLongLong(params.duration);
    const leftTrigger = _double(params.leftTrigger, 'leftTrigger');
    const rightTrigger = _double(params.rightTrigger, 'rightTrigger');
    const startDelay = _unsignedLongLong(params.startDelay);
    const strongMagnitude = _double(params.strongMagnitude, 'strongMagnitude');
    const weakMagnitude = _double(params.weakMagnitude, 'weakMagnitude');
    return {
        duration: duration,
        leftTrigger: leftTrigger,
        rightTrigger: rightTrigger,
        startDelay: startDelay,
        strongMagnitude: strongMagnitude,
        weakMagnitude: weakMagnitude,
    };
}

function _isMagnitude(value) {
    return value >= 0 && value <= 1;
}

// Set the motors -- or, for a silent instruction, stop them if they run. False
// when the host would not take it.
function _drive(actuator, strong, weak, left, right, duration) {
    const audible = duration > 0 && (strong > 0 || weak > 0 || left > 0 || right > 0);
    if (!audible && !actuator.audible) return true;
    try {
        if (audible) {
            op_gamepad_rumble(actuator.index, strong, weak, left, right, duration);
        } else {
            op_gamepad_rumble(actuator.index, 0, 0, 0, 0, 0);
        }
    } catch (_) {
        actuator.audible = false;
        return false;
    }
    actuator.audible = audible;
    return true;
}

// End the effect playing, if any, settling its promise with `result`.
function _settle(actuator, result) {
    const resolve = actuator.resolve;
    if (resolve === null) return;
    actuator.resolve = null;
    if (actuator.startTimer !== 0) {
        clearTimeout(actuator.startTimer);
        actuator.startTimer = 0;
    }
    if (actuator.endTimer !== 0) {
        clearTimeout(actuator.endTimer);
        actuator.endTimer = 0;
    }
    resolve(result);
}

// Stop what is playing and say it was preempted.
function _preempt(actuator) {
    if (actuator.resolve === null) return;
    if (actuator.connected) _drive(actuator, 0, 0, 0, 0, 0);
    _settle(actuator, 'preempted');
}

function _playEffect(actuator, type, params) {
    let effect;
    try {
        type = SafeString(type);
        if (type !== 'dual-rumble' && type !== 'trigger-rumble') {
            throw new SafeTypeError("The provided value '" + type +
                "' is not a valid enum value of type GamepadHapticEffectType.");
        }
        effect = _effectParameters(params);
    } catch (e) {
        return PromiseReject(e);
    }
    const trigger = type === 'trigger-rumble';
    if (!_isMagnitude(effect.strongMagnitude) || !_isMagnitude(effect.weakMagnitude)
        || (trigger && (!_isMagnitude(effect.leftTrigger) || !_isMagnitude(effect.rightTrigger)))) {
        return PromiseReject(new SafeTypeError(
            'Failed to execute \'playEffect\': The effect parameters are not valid for ' + type + '.'));
    }
    if (_hidden) {
        return PromiseReject(domException(
            'playEffect: the game is in the background.', 'InvalidStateError'));
    }
    // The effect playing is preempted whatever happens next. Its motors are
    // left to the next instruction, which replaces it on the pad.
    _settle(actuator, 'preempted');
    // A pad that went away plays nothing, whatever it once declared.
    if (!actuator.connected || (actuator.haptics & _effectBit(type)) === 0) {
        _drive(actuator, 0, 0, 0, 0, 0);
        return PromiseReject(domException(
            'playEffect: this gamepad does not play ' + type + ' effects.', 'NotSupportedError'));
    }

    const delay = MathMin(effect.startDelay, EFFECT_MAX_MS);
    const duration = MathMin(effect.duration, EFFECT_MAX_MS - delay);
    const strong = effect.strongMagnitude;
    const weak = effect.weakMagnitude;
    const left = trigger ? effect.leftTrigger : 0;
    const right = trigger ? effect.rightTrigger : 0;

    if (delay === 0 && !_drive(actuator, strong, weak, left, right, duration)) {
        return PromiseReject(domException(
            'playEffect: the host did not take the effect.', 'OperationError'));
    }
    let resolveEffect;
    let rejectEffect;
    const promise = new SafePromise(function (resolve, reject) {
        resolveEffect = resolve;
        rejectEffect = reject;
    });
    actuator.resolve = resolveEffect;
    if (delay !== 0) {
        // Nothing plays during the delay.
        _drive(actuator, 0, 0, 0, 0, 0);
        actuator.startTimer = setTimeout(function () {
            actuator.startTimer = 0;
            if (!_drive(actuator, strong, weak, left, right, duration)) {
                actuator.resolve = null;
                clearTimeout(actuator.endTimer);
                actuator.endTimer = 0;
                rejectEffect(domException(
                    'playEffect: the host did not take the effect.', 'OperationError'));
            }
        }, delay);
    }
    actuator.endTimer = setTimeout(function () {
        actuator.endTimer = 0;
        // The host stops the motors itself when the duration runs out.
        actuator.audible = false;
        _settle(actuator, 'complete');
    }, delay + duration);
    return promise;
}

function _reset(actuator) {
    if (_hidden) {
        return PromiseReject(domException(
            'reset: the game is in the background.', 'InvalidStateError'));
    }
    _preempt(actuator);
    return PromiseResolve('complete');
}

function _makeActuator(index, haptics) {
    const effects = [];
    if (haptics & HAPTICS_DUAL_RUMBLE) ArrayPush(effects, 'dual-rumble');
    if (haptics & HAPTICS_TRIGGER_RUMBLE) ArrayPush(effects, 'trigger-rumble');
    const actuator = {
        index: index,
        haptics: haptics,
        connected: true,
        // Whether the host was last told to run the motors and their time is
        // not yet up: only then does stopping need an instruction.
        audible: false,
        resolve: null,
        startTimer: 0,
        endTimer: 0,
    };
    const view = ObjectFreeze(ObjectDefineProperties({}, {
        effects: { value: ObjectFreeze(effects), enumerable: true },
        playEffect: {
            value: function playEffect(type, params) {
                return _playEffect(actuator, type, params);
            },
        },
        reset: {
            value: function reset() {
                return _reset(actuator);
            },
        },
    }));
    return { actuator: actuator, view: view };
}

_internalObserveVisibility(function (visible) {
    _hidden = !visible;
    if (visible) return;
    for (let i = 0; i < _gamepadStates.length; i++) {
        const state = _gamepadStates[i];
        if (state) _preempt(state.actuator);
    }
});

function _makeGamepad(index, id, mapping, axisCount, buttonCount, haptics) {
    const actuator = _makeActuator(index, haptics);
    const state = {
        connected: true,
        timestamp: 0,
        axes: new SafeArray(axisCount),
        buttons: new SafeArray(buttonCount),
        actuator: actuator.actuator,
    };
    const axes = new SafeArray(axisCount);
    for (let i = 0; i < axisCount; i++) {
        state.axes[i] = 0;
        ObjectDefineProperty(axes, i, {
            get: function () { return state.axes[i]; },
            enumerable: true,
            configurable: false,
        });
    }
    const buttons = new SafeArray(buttonCount);
    for (let i = 0; i < buttonCount; i++) {
        const buttonState = { pressed: false, touched: false, value: 0 };
        state.buttons[i] = buttonState;
        buttons[i] = ObjectFreeze(ObjectDefineProperties({}, {
            pressed: {
                get: function () { return buttonState.pressed; },
                enumerable: true,
            },
            touched: {
                get: function () { return buttonState.touched; },
                enumerable: true,
            },
            value: {
                get: function () { return buttonState.value; },
                enumerable: true,
            },
        }));
    }
    ObjectFreeze(axes);
    ObjectFreeze(buttons);
    const view = ObjectFreeze(ObjectDefineProperties({}, {
        id: { value: id, enumerable: true },
        index: { value: index, enumerable: true },
        connected: {
            get: function () { return state.connected; },
            enumerable: true,
        },
        mapping: { value: mapping, enumerable: true },
        timestamp: {
            get: function () { return state.timestamp; },
            enumerable: true,
        },
        axes: { value: axes, enumerable: true },
        buttons: { value: buttons, enumerable: true },
        vibrationActuator: { value: actuator.view, enumerable: true },
    }));
    return { state: state, view: view };
}

// ==================== Events ====================

function _addGamepadListener(group, listener) {
    if (typeof listener === 'function' && ArrayIndexOf(group, listener) === -1) {
        ArrayPush(group, listener);
    }
}

function _removeGamepadListener(group, listener) {
    const at = ArrayIndexOf(group, listener);
    if (at !== -1) ArraySplice(group, at, 1);
}

function _fireGamepadEvent(group, type, gamepad) {
    const event = { type: type, gamepad: gamepad };
    // DOM-style dispatch uses the listener set captured at dispatch start:
    // removing oneself cannot skip the next listener in the same event.
    const listeners = ArraySlice(group);
    for (let i = 0; i < listeners.length; i++) {
        // One listener throwing must not stop the others, and must not leave
        // the pad half-registered: content commonly adds a listener per system.
        try {
            listeners[i](event);
        } catch (e) {
            // Logging is diagnostic, not part of event delivery. Capture the
            // original function before content runs and contain even a broken
            // logger so it cannot suppress later listeners.
            try {
                ConsoleError('[gamepad] ' + type + ' listener threw: ' + e);
            } catch (_) {}
        }
    }
}

function onGamepadConnected(listener) {
    _addGamepadListener(_connectedListeners, listener);
}

function offGamepadConnected(listener) {
    _removeGamepadListener(_connectedListeners, listener);
}

function onGamepadDisconnected(listener) {
    _addGamepadListener(_disconnectedListeners, listener);
}

function offGamepadDisconnected(listener) {
    _removeGamepadListener(_disconnectedListeners, listener);
}

// ==================== Internal triggers (called from native) ====================

function _internalTriggerGamepadConnected(index, id, mapping, axisCount, buttonCount, haptics) {
    const record = _makeGamepad(index, id, mapping, axisCount, buttonCount,
        (haptics >>> 0) & HAPTICS_KNOWN);
    // Stable host indices need not arrive in order. Materialise lower empty
    // slots as null on this connection-only cold path; per-frame sampling does
    // no length checks or allocation.
    while (_gamepads.length <= index) {
        ArrayPush(_gamepads, null);
        ArrayPush(_gamepadStates, null);
    }
    _gamepadStates[index] = record.state;
    _gamepads[index] = record.view;
    _fireGamepadEvent(_connectedListeners, 'gamepadconnected', record.view);
}

function _internalTriggerGamepadDisconnected(index) {
    const gamepad = _gamepads[index];
    const state = _gamepadStates[index];
    if (!gamepad || !state) return;
    state.connected = false;
    // The pad is gone, so are its motors: its effect ends unplayed.
    state.actuator.connected = false;
    state.actuator.audible = false;
    _settle(state.actuator, 'preempted');
    // Emptied rather than removed: content that stored the index must see a
    // null slot, not the pad that happens to be plugged in next.
    _gamepads[index] = null;
    _gamepadStates[index] = null;
    _fireGamepadEvent(_disconnectedListeners, 'gamepaddisconnected', gamepad);
}

// One flat array per sample, laid out as
//   [axisCount, buttonCount, ...axes, ...(pressed, touched, value) per button]
// so a frame's worth of state crosses the boundary as a single value rather
// than as several nested ones.
function _internalTriggerGamepadState(index, timestampMs, packed) {
    const state = _gamepadStates[index];
    // A state update for a slot nothing is connected to is not an error: a pad
    // can be unplugged between the host sampling it and this running.
    if (!state) return;

    const axisCount = packed[0];
    const buttonCount = packed[1];
    let at = 2;
    for (let i = 0; i < axisCount; i++) {
        state.axes[i] = packed[at++];
    }
    for (let i = 0; i < buttonCount; i++) {
        const button = state.buttons[i];
        if (button) {
            button.pressed = packed[at] !== 0;
            button.touched = packed[at + 1] !== 0;
            button.value = packed[at + 2];
        }
        at += 3;
    }
    state.timestamp = pageTime(timestampMs);
}

export {
    getGamepads,
    onGamepadConnected,
    offGamepadConnected,
    onGamepadDisconnected,
    offGamepadDisconnected,
    _internalTriggerGamepadConnected,
    _internalTriggerGamepadDisconnected,
    _internalTriggerGamepadState,
};
