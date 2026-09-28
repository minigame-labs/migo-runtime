// The W3C Gamepad API.
//
// Mainstream mini-game platforms have no gamepad API, so this follows the
// standard Web API used by HTML5 content: navigator.getGamepads() and
// gamepadconnected. Inventing a Migo-shaped API instead would make existing
// HTML5 games not work for no reason.
//
// The API is POLLED, not evented: content calls getGamepads() every frame and
// reads whatever is current. So state lives here and the host pushes updates
// into it, rather than each sample being dispatched to a listener that most
// content would not have.

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

function _makeGamepad(index, id, mapping, axisCount, buttonCount) {
    const state = {
        connected: true,
        timestamp: 0,
        axes: new SafeArray(axisCount),
        buttons: new SafeArray(buttonCount),
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

function _internalTriggerGamepadConnected(index, id, mapping, axisCount, buttonCount) {
    const record = _makeGamepad(index, id, mapping, axisCount, buttonCount);
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
    state.timestamp = timestampMs;
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
