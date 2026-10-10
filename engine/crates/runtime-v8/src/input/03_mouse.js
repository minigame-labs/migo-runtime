// Mouse and Wheel event listeners (PC-only)
//
// These are part of the common mini-game surface: mini-games run on PC clients of that platform, where the
// mouse is the primary input. A host delivers them through the C ABI's pointer
// and wheel entry points; the engine never synthesizes them from touch, nor
// touch from them. Which streams a host sends is its own decision -- an Android
// host sends touch, a desktop host sends the mouse, and a desktop host serving
// phone-first content may send both.
//
// Coordinates are CSS pixels, the same logical space touch uses.

import { createListenerGroup } from "ext:host_v8_base/02_async.js";
import { pageTime } from "ext:host_v8_touch/00_input_clock.js";

const _mouseDown = createListenerGroup('onMouseDown');
const _mouseMove = createListenerGroup('onMouseMove');
const _mouseUp = createListenerGroup('onMouseUp');
const _wheel = createListenerGroup('onWheel');

// ---- Mouse Down ----

function onMouseDown(listener) {
    _mouseDown.on(listener);
}

function offMouseDown(listener) {
    _mouseDown.off(listener);
}

function _internalTriggerMouseDown(x, y, button, timeStamp) {
    _mouseDown.trigger({ x: x, y: y, button: button, timeStamp: pageTime(timeStamp) });
}

// ---- Mouse Move ----

function onMouseMove(listener) {
    _mouseMove.on(listener);
}

function offMouseMove(listener) {
    _mouseMove.off(listener);
}

// The common mini-game platform's onMouseMove carries movementX/movementY, the offset from the previous
// move. It is derived here rather than in each host: every host already sends
// consecutive positions, so computing it once keeps Android, Linux and Windows
// from arriving at three different answers. Pointer lock -- where the cursor
// stands still but movement must still be reported -- keeps that true: while
// locked, a host reports positions that go on accumulating the motion (they may
// leave the surface), so the same subtraction answers.
//
// The position is not reset when the pointer leaves and re-enters. DOM
// movementX is defined as the difference from the previous event's position and
// browsers do report that jump, so clearing it here would be this runtime
// inventing a behaviour rather than matching the platform content expects.
let _lastMoveX = null;
let _lastMoveY = null;

function _internalTriggerMouseMove(x, y, button, timeStamp) {
    const movementX = _lastMoveX === null ? 0 : x - _lastMoveX;
    const movementY = _lastMoveY === null ? 0 : y - _lastMoveY;
    _lastMoveX = x;
    _lastMoveY = y;
    _mouseMove.trigger({
        x: x,
        y: y,
        button: button,
        movementX: movementX,
        movementY: movementY,
        timeStamp: pageTime(timeStamp),
    });
}

// ---- Mouse Up ----

function onMouseUp(listener) {
    _mouseUp.on(listener);
}

function offMouseUp(listener) {
    _mouseUp.off(listener);
}

function _internalTriggerMouseUp(x, y, button, timeStamp) {
    _mouseUp.trigger({ x: x, y: y, button: button, timeStamp: pageTime(timeStamp) });
}

// ---- Wheel ----

function onWheel(listener) {
    _wheel.on(listener);
}

function offWheel(listener) {
    _wheel.off(listener);
}

// deltaMode says what unit the deltas are in, matching DOM WheelEvent:
// 0 = pixel, 1 = line, 2 = page. It is carried rather than normalized to pixels
// because converting a line-based delta needs the content's own line height.
//
// It is appended AFTER timeStamp rather than placed next to the deltas it
// describes, for the same reason the C ABI appends struct fields: an embedded-JS
// change makes the V8 snapshot stale, and on-device validation deliberately runs
// a debug AAR against one. A snapshot holding the previous four-parameter
// function then binds every argument it does have correctly and simply misses
// this one, instead of binding deltaMode into timeStamp and handing content a
// timestamp that is really a unit code.
function _internalTriggerWheel(deltaX, deltaY, deltaZ, timeStamp, deltaMode) {
    _wheel.trigger({
        deltaX: deltaX,
        deltaY: deltaY,
        deltaZ: deltaZ,
        deltaMode: deltaMode,
        timeStamp: pageTime(timeStamp),
    });
}

export {
    onMouseDown, offMouseDown, _internalTriggerMouseDown,
    onMouseMove, offMouseMove, _internalTriggerMouseMove,
    onMouseUp, offMouseUp, _internalTriggerMouseUp,
    onWheel, offWheel, _internalTriggerWheel,
};
