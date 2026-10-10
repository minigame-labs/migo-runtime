import { pageTime } from "ext:host_v8_touch/00_input_clock.js";
const TOUCH_STRIDE = 20; // bytes
const FLAG_CHANGED = 1; // pointer is in changedTouches
const FLAG_REMOVED = 2; // pointer left the surface (end/cancel) -> excluded from touches

// typeCode from native
const TYPE_MAP = Object.freeze({
  0: 'start',
  1: 'move',
  2: 'end',
  3: 'cancel',
});

const TOUCH_LISTENERS = {
  start: new Set(),
  move: new Set(),
  end: new Set(),
  cancel: new Set(),
};

function addListener(type, fn) {
  if (typeof fn !== 'function') return;
  const set = TOUCH_LISTENERS[type];
  if (set) set.add(fn);
}

function removeListener(type, fn) {
  const set = TOUCH_LISTENERS[type];
  if (!set) return;
  if (typeof fn === 'function') {
    set.delete(fn);
  } else {
    set.clear();
  }
}

export const onTouchStart = (fn) => addListener('start', fn);
export const onTouchMove = (fn) => addListener('move', fn);
export const onTouchEnd = (fn) => addListener('end', fn);
export const onTouchCancel = (fn) => addListener('cancel', fn);

export const offTouchStart = (fn) => removeListener('start', fn);
export const offTouchMove = (fn) => removeListener('move', fn);
export const offTouchEnd = (fn) => removeListener('end', fn);
export const offTouchCancel = (fn) => removeListener('cancel', fn);

let _queue = [];
let _scheduled = false;

// A native control above the game -- a user-info button -- owns the touches
// that begin on it, as the common platform's native views do: content does not
// see them. The claimer is asked about every touch while it is `active()`;
// `claims(type, touch, changed)` answers whether the touch is the control's.
let _claimer = null;

/** Install the one claimer (ui/02_buttons.js). */
export function _setTouchClaimer(claimer) {
  _claimer = claimer;
}

/**
 * Called from native (Rust / V8 binding)
 *
 * @param {number} typeCode
 * @param {ArrayBuffer} buffer - TouchPoint[]
 * @param {number} count
 * @param {number} timeStamp
 */
export function _internalEnqueueRawTouchEvent(typeCode, buffer, count, timeStamp) {
  _queue.push({ typeCode, buffer, count, timeStamp: pageTime(timeStamp) });

  if (!_scheduled) {
    _scheduled = true;
    Promise.resolve().then(_drain);
  }
}

function _makeTouch(id, x, y, pressure) {
  // Keep shape stable for JS engines
  return {
    identifier: id,
    clientX: x,
    clientY: y,
    pageX: x,
    pageY: y,
    force: pressure,
  };
}

function _drain() {
  _scheduled = false;

  // Swap queue to avoid re-entrancy issues
  const batch = _queue;
  _queue = [];

  for (let idx = 0; idx < batch.length; idx++) {
    const ev = batch[idx];

    const type = TYPE_MAP[ev.typeCode] || 'move';
    const listeners = TOUCH_LISTENERS[type];
    const claiming = _claimer !== null && _claimer.active();
    if ((!listeners || listeners.size === 0) && !claiming) continue;

    // Snapshot listeners: a handler that adds/removes listeners mid-dispatch
    // must not change who receives the current event (stable event semantics).
    const fns = Array.from(listeners);

    const count = ev.count | 0;
    if (count <= 0 || !(ev.buffer instanceof ArrayBuffer)) {
      const event = {
        type,
        touches: [],
        changedTouches: [],
        timeStamp: ev.timeStamp,
      };

      for (let li = 0; li < fns.length; li++) {
        try {
          fns[li](event);
        } catch (e) {
          // The game's own bug: the listeners after it still hear the event, and the
          // error is reported, not swallowed -- a handler that throws on every touch
          // is otherwise a game that "ignores input" with no clue why.
          console.error('Error in touch listener:', e);
        }
      }
      continue;
    }

    const view = new DataView(ev.buffer);
    const touches = [];
    const changedTouches = [];

    for (let i = 0; i < count; i++) {
      const base = i * TOUCH_STRIDE;

      const id = view.getUint32(base, true);
      const x = view.getFloat32(base + 4, true);
      const y = view.getFloat32(base + 8, true);
      const pressure = view.getFloat32(base + 12, true);
      const flags = view.getUint32(base + 16, true);

      const touch = _makeTouch(id, x, y, pressure);
      if (claiming && _claimer.claims(type, touch, (flags & FLAG_CHANGED) !== 0)) continue;
      // `touches` = points still on the surface; a lifted/cancelled pointer
      // (FLAG_REMOVED) appears only in `changedTouches`.
      if (!(flags & FLAG_REMOVED)) touches.push(touch);
      if (flags & FLAG_CHANGED) changedTouches.push(touch);
    }
    // Every changed touch was a control's: content has no event to hear.
    if (claiming && changedTouches.length === 0) continue;
    if (!listeners || listeners.size === 0) continue;

    const event = {
      type,
      touches,
      changedTouches,
      timeStamp: ev.timeStamp,
    };

    for (let li = 0; li < fns.length; li++) {
      try {
        fns[li](event);
      } catch (e) {
        // The game's own bug: the listeners after it still hear the event, and the
        // error is reported, not swallowed -- a handler that throws on every touch
        // is otherwise a game that "ignores input" with no clue why.
        console.error('Error in touch listener:', e);
      }
    }
  }
}
