import { op_show_keyboard, op_hide_keyboard, op_update_keyboard } from "ext:core/ops";
import { wrapAsync, createListenerGroup } from "ext:host_v8_base/02_async.js";
import { pageTime } from "ext:host_v8_touch/00_input_clock.js";

// ==================== Soft keyboard event listeners ====================

const _inputListeners = createListenerGroup('onKeyboardInput');
const _heightChangeListeners = createListenerGroup('onKeyboardHeightChange');
const _confirmListeners = createListenerGroup('onKeyboardConfirm');
const _completeListeners = createListenerGroup('onKeyboardComplete');

// ---- onKeyboardInput / offKeyboardInput ----

function onKeyboardInput(listener) {
    _inputListeners.on(listener);
}

function offKeyboardInput(listener) {
    _inputListeners.off(listener);
}

// ---- onKeyboardHeightChange / offKeyboardHeightChange ----

function onKeyboardHeightChange(listener) {
    _heightChangeListeners.on(listener);
}

function offKeyboardHeightChange(listener) {
    _heightChangeListeners.off(listener);
}

// ---- onKeyboardConfirm / offKeyboardConfirm ----

function onKeyboardConfirm(listener) {
    _confirmListeners.on(listener);
}

function offKeyboardConfirm(listener) {
    _confirmListeners.off(listener);
}

// ---- onKeyboardComplete / offKeyboardComplete ----

function onKeyboardComplete(listener) {
    _completeListeners.on(listener);
}

function offKeyboardComplete(listener) {
    _completeListeners.off(listener);
}

// ==================== Internal trigger functions (called from native) ====================

function _internalTriggerKeyboardInput(value) {
    _inputListeners.trigger({ value });
}

function _internalTriggerKeyboardHeightChange(height) {
    _heightChangeListeners.trigger({ height });
}

function _internalTriggerKeyboardConfirm(value) {
    _confirmListeners.trigger({ value });
}

function _internalTriggerKeyboardComplete(value) {
    _completeListeners.trigger({ value });
}

// ==================== PC keyboard event listeners ====================

const _keyDownListeners = createListenerGroup('onKeyDown');
const _keyUpListeners = createListenerGroup('onKeyUp');

function onKeyDown(listener) {
    _keyDownListeners.on(listener);
}

function offKeyDown(listener) {
    _keyDownListeners.off(listener);
}

function onKeyUp(listener) {
    _keyUpListeners.on(listener);
}

function offKeyUp(listener) {
    _keyUpListeners.off(listener);
}

// Modifier bits, matching the C ABI's MIGO_KEY_MODIFIER_* values. Content sees
// the DOM booleans instead of the mask, because that is what a KeyboardEvent
// carries and what an HTML5 game already reads.
const _MOD_CONTROL = 1;
const _MOD_SHIFT = 2;
const _MOD_ALT = 4;
const _MOD_META = 8;

// modifiers and repeat are appended AFTER timeStamp rather than placed beside
// key and code, for the same reason the C ABI appends struct fields: editing
// this file makes the V8 snapshot stale, and on-device validation deliberately
// runs a debug build against one. A snapshot holding the previous
// three-parameter function then still binds key/code/timeStamp correctly and
// reports no modifiers, instead of binding a modifier mask into timeStamp.
function _keyEventDetail(key, code, timeStamp, modifiers, repeat) {
    const mask = modifiers | 0;
    return {
        key,
        code,
        timeStamp: pageTime(timeStamp),
        ctrlKey: (mask & _MOD_CONTROL) !== 0,
        shiftKey: (mask & _MOD_SHIFT) !== 0,
        altKey: (mask & _MOD_ALT) !== 0,
        metaKey: (mask & _MOD_META) !== 0,
        repeat: repeat === true,
    };
}

function _internalTriggerKeyDown(key, code, timeStamp, modifiers, repeat) {
    _keyDownListeners.trigger(_keyEventDetail(key, code, timeStamp, modifiers, repeat));
}

function _internalTriggerKeyUp(key, code, timeStamp, modifiers, repeat) {
    _keyUpListeners.trigger(_keyEventDetail(key, code, timeStamp, modifiers, repeat));
}

// ==================== Async control APIs ====================

function showKeyboard(options = {}) {
    const { defaultValue, maxLength, multiple, confirmHold, confirmType, keyboardType } = options;
    return wrapAsync('showKeyboard', function () {
        op_show_keyboard(JSON.stringify({
            defaultValue: defaultValue !== undefined ? defaultValue : '',
            maxLength: maxLength !== undefined ? maxLength : 140,
            multiple: multiple !== undefined ? multiple : false,
            confirmHold: confirmHold !== undefined ? confirmHold : false,
            confirmType: confirmType !== undefined ? confirmType : 'done',
            keyboardType: keyboardType !== undefined ? keyboardType : 'text',
        }));
    }, options);
}

function hideKeyboard(options = {}) {
    return wrapAsync('hideKeyboard', function () {
        op_hide_keyboard();
    }, options);
}

function updateKeyboard(options = {}) {
    const { value } = options;
    return wrapAsync('updateKeyboard', function () {
        if (value === undefined) {
            throw new Error('value is required');
        }
        op_update_keyboard(value);
    }, options);
}

export {
    // Soft keyboard events
    onKeyboardInput,
    offKeyboardInput,
    onKeyboardHeightChange,
    offKeyboardHeightChange,
    onKeyboardConfirm,
    offKeyboardConfirm,
    onKeyboardComplete,
    offKeyboardComplete,
    // Internal triggers (called from native dispatch)
    _internalTriggerKeyboardInput,
    _internalTriggerKeyboardHeightChange,
    _internalTriggerKeyboardConfirm,
    _internalTriggerKeyboardComplete,
    // PC keyboard events
    onKeyDown,
    offKeyDown,
    onKeyUp,
    offKeyUp,
    _internalTriggerKeyDown,
    _internalTriggerKeyUp,
    // Control APIs
    showKeyboard,
    hideKeyboard,
    updateKeyboard,
};
