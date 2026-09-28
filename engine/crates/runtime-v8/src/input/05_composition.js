// IME composition, as the DOM CompositionEvent.
//
// Mainstream mini-game platforms have no composition API, so this follows the
// standard Web composition events used by HTML5 content. Composition is the
// in-progress state of IME input: typing pinyin
// shows a preedit string before any of it is committed. It is distinct from the
// soft keyboard's onKeyboardInput, which reports text that has already been
// committed.
//
// A game drawing its own text field needs both -- the preedit to show what is
// being typed, and the committed value to store -- which is why this is a
// separate listener group rather than more keyboard events.

import { createListenerGroup } from "ext:host_v8_base/02_async.js";

const _startListeners = createListenerGroup('onCompositionStart');
const _updateListeners = createListenerGroup('onCompositionUpdate');
const _endListeners = createListenerGroup('onCompositionEnd');

function onCompositionStart(listener) {
    _startListeners.on(listener);
}

function offCompositionStart(listener) {
    _startListeners.off(listener);
}

function onCompositionUpdate(listener) {
    _updateListeners.on(listener);
}

function offCompositionUpdate(listener) {
    _updateListeners.off(listener);
}

function onCompositionEnd(listener) {
    _endListeners.on(listener);
}

function offCompositionEnd(listener) {
    _endListeners.off(listener);
}

// ==================== Internal triggers (called from native) ====================

// `data` is the whole current preedit string, never a delta: content that only
// received what changed could not reconstruct the rest.
function _internalTriggerCompositionStart(data) {
    _startListeners.trigger({ type: 'compositionstart', data: data });
}

function _internalTriggerCompositionUpdate(data) {
    _updateListeners.trigger({ type: 'compositionupdate', data: data });
}

// On end, `data` is the committed text -- empty when the user cancelled.
function _internalTriggerCompositionEnd(data) {
    _endListeners.trigger({ type: 'compositionend', data: data });
}

export {
    onCompositionStart,
    offCompositionStart,
    onCompositionUpdate,
    offCompositionUpdate,
    onCompositionEnd,
    offCompositionEnd,
    _internalTriggerCompositionStart,
    _internalTriggerCompositionUpdate,
    _internalTriggerCompositionEnd,
};
