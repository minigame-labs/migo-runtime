// The native buttons, drawn.
//
// On the platform whose API they are, createUserInfoButton and its kind are
// native views above the game. Here the engine draws them, into a canvas of its
// own that content has no handle on, and names that canvas the frame's overlay
// (OP2D_SET_OVERLAY): the renderer composites it over the window at present
// time, so the buttons are in what the player sees and never in the content's
// canvas -- getImageData, readPixels and a screenshot of the game do not see
// them, as they would not see a view above it.
//
// The canvas covers only the buttons -- the box around the shown ones, in the
// window's physical pixels -- so a button costs its own pixels, not a screen's.
// Any change (a button shown, hidden, moved, restyled, its image loaded, the
// window resized) asks for one repaint, made in the engine's next frame, so a
// change shows even when content runs no loop of its own and many changes in
// one frame cost one paint.

import { createOffscreenCanvas } from "ext:host_v8_web/03_canvas.js";
import { encode2dSetOverlay } from "ext:host_v8_webgl/00_render_command_stream.js";
import { requestAnimationFrame } from "ext:host_v8_webgl/03_raf.js";
import { getWindowInfo } from "ext:host_v8_system/03_window_info.js";
import { _internalObserveWindowResize } from "ext:host_v8_system/12_window_resize.js";

const MathFloor = Math.floor;
const MathCeil = Math.ceil;
const MathMax = Math.max;
const MathMin = Math.min;
const ConsoleError = Function.prototype.call.bind(console.error, console);

// The live buttons, in creation order (ui/02_buttons.js owns the list).
let _buttons = null;
let _canvas = null;
let _context = null;
// The canvas's size as this module last set it: reading it back from the
// canvas would be a round trip to the renderer on every repaint.
let _width = 0;
let _height = 0;
// Whether the renderer is presenting the canvas now.
let _presented = false;
let _scheduled = false;

/** Draw from `buttons`, the live list of buttons in creation order. */
function attachButtonLayer(buttons) {
    _buttons = buttons;
}

/** Ask for a repaint in the engine's next frame. */
function invalidateButtonLayer() {
    if (_scheduled || _buttons === null) return;
    _scheduled = true;
    requestAnimationFrame(_paint);
}

_internalObserveWindowResize(invalidateButtonLayer);

function _paint() {
    _scheduled = false;
    try {
        _paintNow();
    } catch (e) {
        try {
            ConsoleError("native buttons: painting failed: " + e);
        } catch (_) {}
    }
}

function _withdraw() {
    if (_presented) {
        encode2dSetOverlay(_context._canvasId, 0, 0, false);
        _presented = false;
    }
    // With no button left the canvas goes; its destruction is the canvas's own.
    if (_buttons.length === 0) {
        _canvas = null;
        _context = null;
        _width = 0;
        _height = 0;
    }
}

function _paintNow() {
    const shown = [];
    let left = Infinity;
    let top = Infinity;
    let right = -Infinity;
    let bottom = -Infinity;
    for (let i = 0; i < _buttons.length; i++) {
        const bounds = _buttons[i]._paintBounds();
        if (bounds === null) continue;
        shown.push(_buttons[i]);
        left = MathMin(left, bounds.left);
        top = MathMin(top, bounds.top);
        right = MathMax(right, bounds.left + bounds.width);
        bottom = MathMax(bottom, bounds.top + bounds.height);
    }
    if (shown.length === 0) {
        _withdraw();
        return;
    }
    // The box in the window's physical pixels, kept to the window: what is off
    // it is never seen and would only cost memory.
    const info = getWindowInfo();
    const ratio = info.pixelRatio > 0 ? info.pixelRatio : 1;
    const x0 = MathMax(0, MathFloor(left * ratio));
    const y0 = MathMax(0, MathFloor(top * ratio));
    const x1 = MathMin(MathCeil(info.windowWidth * ratio), MathCeil(right * ratio));
    const y1 = MathMin(MathCeil(info.windowHeight * ratio), MathCeil(bottom * ratio));
    if (!(x1 > x0 && y1 > y0)) {
        _withdraw();
        return;
    }
    const width = x1 - x0;
    const height = y1 - y0;
    if (_canvas === null) {
        _canvas = createOffscreenCanvas(width, height);
        _context = _canvas.getContext("2d");
        _width = width;
        _height = height;
    } else if (_width !== width || _height !== height) {
        // A new size is a new bitmap and a fresh drawing state.
        _canvas.width = width;
        _canvas.height = height;
        _width = width;
        _height = height;
    } else {
        _context.setTransform(1, 0, 0, 1, 0, 0);
        _context.clearRect(0, 0, width, height);
    }
    // Buttons are styled in CSS pixels of the window; the canvas's origin is the
    // box's corner.
    _context.setTransform(ratio, 0, 0, ratio, -x0, -y0);
    for (let i = 0; i < shown.length; i++) {
        _context.save();
        try {
            shown[i]._paint(_context);
        } catch (e) {
            try {
                ConsoleError("native buttons: a button failed to paint: " + e);
            } catch (_) {}
        }
        _context.restore();
    }
    encode2dSetOverlay(_context._canvasId, x0, y0, true);
    _presented = true;
}

/**
 * How wide `text` is in `font`, as this layer draws it: for a button that lays
 * itself out around its text. Measured on the layer's own canvas, made now if
 * no button has been drawn yet -- the one canvas every button is drawn on.
 */
function measureButtonText(font, text) {
    if (_context === null) {
        _canvas = createOffscreenCanvas(1, 1);
        _context = _canvas.getContext("2d");
        _width = 1;
        _height = 1;
    }
    _context.font = font;
    return _context.measureText(text).width;
}

export { attachButtonLayer, invalidateButtonLayer, measureButtonText };
