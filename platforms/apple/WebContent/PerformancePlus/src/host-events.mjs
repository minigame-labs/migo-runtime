// Host events: what the host tells content that no request asked for.
//
// The host's input -- touches, keys, the soft keyboard, IME, gamepads -- is a
// call of a function on the engine's host bridge, with the arguments the
// embedded runtime passes it (engine/crates/runtime-v8/src/js_bindings.rs). On
// this lane the host sends each as a service EVENT record naming the function
// by the number below (contracts/runtime/host-events.json, checked entry for
// entry by scripts/gen-performance-plus-engine.py; append only), and this
// makes the same call on the same engine JavaScript.
//
// The functions are taken from the bridge once, when the engine has loaded and
// before content runs -- as the embedded bindings resolve them once -- so
// content replacing a bridge function afterwards changes nothing here. Until
// then an event has nowhere to go and is dropped, which is what the embedded
// runtime does with input that arrives before its bindings exist.

export const HOST_EVENT = Object.freeze({
  _internalEnqueueRawTouchEvent: 1,
  _internalTriggerFocusChanged: 2,
  _internalTriggerKeyboardInput: 3,
  _internalTriggerKeyboardHeightChange: 4,
  _internalTriggerKeyboardConfirm: 5,
  _internalTriggerKeyboardComplete: 6,
  _internalTriggerCompositionStart: 7,
  _internalTriggerCompositionUpdate: 8,
  _internalTriggerCompositionEnd: 9,
  _internalTriggerGamepadConnected: 10,
  _internalTriggerGamepadDisconnected: 11,
  _internalTriggerGamepadState: 12,
  _internalTriggerKeyDown: 13,
  _internalTriggerKeyUp: 14,
  _internalTriggerMouseDown: 15,
  _internalTriggerMouseMove: 16,
  _internalTriggerMouseUp: 17,
  _internalTriggerWheel: 18,
});

/** event number → the bridge function it calls, once the engine is bound. */
let hooks = null;

/**
 * Take the bridge's functions. Call after the engine has loaded and before
 * content runs. A name the bridge does not have is left out and its events
 * dropped, which a stale engine build would do in process too.
 */
export function bindHostEvents(bridge) {
  const bound = new Map();
  for (const [name, event] of Object.entries(HOST_EVENT)) {
    const hook = bridge?.[name];
    if (typeof hook === "function") bound.set(event, hook);
  }
  hooks = bound;
  return bound.size;
}

/**
 * Deliver one event: the bridge function it names, called with its values as
 * the embedded runtime passes them -- bytes as their ArrayBuffer.
 *
 * What the call throws ends with the call, as it does in process, where the
 * binding discards a hook's exception: the events after it in the same
 * message are still delivered, and the stream is not the listener's to break.
 */
export function dispatchHostEvent(event, values) {
  const hook = hooks?.get(event);
  if (hook === undefined) return false;
  const args = values.map((value) => (value instanceof Uint8Array ? value.buffer : value));
  try {
    hook(...args);
  } catch {
    // Discarded, as the embedded binding discards it.
  }
  return true;
}
