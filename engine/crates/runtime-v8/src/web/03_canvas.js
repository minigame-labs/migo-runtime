import { primordials } from "ext:core/mod.js";
import { op_create_offscreen_canvas, op_get_canvas_info, op_resize_canvas, op_destroy_canvas } from "ext:core/ops";
import {
    WebGLRenderingContext,
    WebGL2RenderingContext,
    _bumpCapabilityGeneration,
} from "ext:host_v8_webgl/02_webgl_context.js";
import { CanvasRenderingContext2D } from "ext:host_v8_webgl/02_2d_context.js";
import {
    flushRenderCommandStream,
    discardRenderCommandStream,
} from "ext:host_v8_webgl/00_render_command_stream.js";
const { SafeFinalizationRegistry } = primordials;

const registry = new SafeFinalizationRegistry((rid) => {
    // Flush any pending GL/2D collector commands before the synchronous destroy
    // so they arrive at the render thread BEFORE the DestroyCanvas command
    // (design S8 ordering: pending work for canvas N must precede its teardown).
    flushRenderCommandStream();
    op_destroy_canvas(rid);
});

let _mainCanvas = null;
let _isFirstCreate = true;

class Canvas {
    constructor(rid) {
        this._rid = rid;
        this._offscreen = rid !== 1;
        // Has the content picked the backing-store size itself?
        //
        // Until it does, the main canvas tracks the surface: the platform can
        // hand us a different one after the content is already running (Android
        // delivers `surfaceCreated` at one size and `surfaceChanged` at another
        // when the system bars hide), and a canvas still reporting the old size
        // leaves the content drawing into part of the window.
        //
        // Once the content assigns `width` or `height` it owns the backing
        // store and this must never be overwritten -- a DPR-naive engine
        // (Phaser's `Scale.NONE`, vanilla 2D at resolution 1) chooses a fixed
        // backing on purpose, and one that moved under it would be the same
        // defect in the other direction. That is also what a browser does: an
        // explicitly sized canvas does not resize because the window did.
        this._sizedByContent = false;
        flushRenderCommandStream();
        const info = op_get_canvas_info(rid);
        this._width = info['0'];
        this._height = info['1'];
        // Engines treat the canvas as a DOM element and set CSS on it (e.g.
        // Phaser's ScaleManager writes `canvas.style.marginLeft` to center it).
        // Absorb those writes on a plain object.
        this.style = {};

        registry.register(this, rid);
    }

    get width() {
        return this._width;
    }
    get height() {
        return this._height;
    }
    set width(v) {
        // Flush pending GL stream before resize so GL commands encoded before this
        // resize arrive at the render thread before the ResizeCanvas command.
        flushRenderCommandStream();
        this._sizedByContent = true;
        op_resize_canvas(this._rid, v, undefined);
        this._width = v;
        if (this._context && this._context._resetShadowState) {
            this._context._resetShadowState();
        }
    }
    set height(v) {
        // Flush pending GL stream before resize (same ordering invariant as width setter).
        flushRenderCommandStream();
        this._sizedByContent = true;
        op_resize_canvas(this._rid, undefined, v);
        this._height = v;
        if (this._context && this._context._resetShadowState) {
            this._context._resetShadowState();
        }
    }

    get clientWidth() {
        return this._width;
    }
    get clientHeight() {
        return this._height;
    }
    get offsetWidth() {
        return this._width;
    }
    get offsetHeight() {
        return this._height;
    }
    // Engines treat the canvas as a DOM element. Provide the minimal element
    // surface their scale/DOM managers touch (Phaser's ScaleManager measures
    // via getBoundingClientRect and reads/writes attributes; `style` is set in
    // the constructor). The canvas fills the surface at the origin.
    getBoundingClientRect() {
        const w = this._width, h = this._height;
        return { x: 0, y: 0, top: 0, left: 0, right: w, bottom: h, width: w, height: h };
    }
    setAttribute(name, value) {
        if (name === "width") this.width = value;
        else if (name === "height") this.height = value;
        else this["_attr_" + name] = value;
    }
    getAttribute(name) {
        if (name === "width") return this._width;
        if (name === "height") return this._height;
        const v = this["_attr_" + name];
        return v === undefined ? null : v;
    }
    removeAttribute(name) {
        delete this["_attr_" + name];
    }
    // Minimal EventTarget so engines can register canvas-level listeners
    // (notably `webglcontextlost` / `webglcontextrestored`; also touch). The
    // adapter's canvas shim only installs a no-op `addEventListener` when the
    // native canvas lacks one, so providing a real implementation here makes
    // engine listeners actually fire.
    addEventListener(type, listener) {
        if (typeof listener !== "function") return;
        if (!this._listeners) this._listeners = { __proto__: null };
        (this._listeners[type] || (this._listeners[type] = [])).push(listener);
    }
    removeEventListener(type, listener) {
        const arr = this._listeners && this._listeners[type];
        if (!arr) return;
        const i = arr.indexOf(listener);
        if (i !== -1) arr.splice(i, 1);
    }
    dispatchEvent(event) {
        const arr = this._listeners && this._listeners[event.type];
        if (arr) {
            // Copy so a listener removing itself mid-dispatch is safe.
            const copy = arr.slice();
            for (let i = 0; i < copy.length; i++) {
                try { copy[i].call(this, event); } catch (_e) { /* DOM swallows */ }
            }
        }
        // Also honor an `on<type>` property (e.g. canvas.onwebglcontextlost).
        const on = this["on" + event.type];
        if (typeof on === "function") {
            try { on.call(this, event); } catch (_e) { /* swallow */ }
        }
        return !event.defaultPrevented;
    }
    getContext(contextType, options) {
        if (this._context) { return this._context; }
        if (contextType === 'webgl2') {
            this._context = new WebGL2RenderingContext(this, options);
        } else if (contextType === 'webgl' || contextType === 'experimental-webgl') {
            this._context = new WebGLRenderingContext(this, options);
        } else {
            this._context = new CanvasRenderingContext2D(this);
        }
        return this._context;
    }
}

// Game API: first call returns the main canvas, subsequent calls return offscreen.
const createCanvas = () => {
    if (_isFirstCreate) {
        _isFirstCreate = false;
        return getMainCanvas();
    }
    return createOffscreenCanvas(1, 1);
};

// SDK internal: always creates an offscreen canvas, never touches rid 1.
const createOffscreenCanvas = (width, height) => {
    flushRenderCommandStream();
    const rid = op_create_offscreen_canvas(width || 1, height || 1);
    return new Canvas(rid);
};

// Returns the main canvas (rid 1). Lazily wraps rid 1 on first access.
const getMainCanvas = () => {
    if (!_mainCanvas) {
        _mainCanvas = new Canvas(1);
    }
    return _mainCanvas;
};

// The surface changed. Re-read what the render thread now backs the main
// canvas with, so `canvas.width`/`height` keep describing the pixels the
// content actually draws into.
//
// Without this the canvas reports whatever the surface measured at the instant
// the content first asked for it, forever. On Android that instant is often the
// short-lived pre-system-bar-hide surface, and every game that fills its canvas
// ends up with a dead band along the edge the surface grew into -- with nothing
// in the rendering path wrong, so no rendering test catches it.
//
// Deliberately does NOT create the main canvas: content that never asked for
// one must not acquire it because the window moved.
//
// The 2D drawing state is left alone on purpose. The content did not assign
// `canvas.width`, so nothing about its context was reset -- and the JS setters
// de-duplicate against a shadow, so clearing that shadow here (or letting the
// render side clear its half) would leave the two halves permanently
// disagreeing about a value the content will never send again.
const adoptMainCanvasSurfaceSize = () => {
    const canvas = _mainCanvas;
    if (!canvas || canvas._sizedByContent) return;
    let info;
    try {
        info = op_get_canvas_info(canvas._rid);
    } catch (e) {
        // A surface that cannot be measured is not a reason to stop delivering
        // the resize; keep the last known size rather than reporting zeroes.
        console.error("adopt surface size: canvas info read failed:", e);
        return;
    }
    canvas._width = info['0'];
    canvas._height = info['1'];
};

// Host-driven WebGL context-loss lifecycle. When the render thread rebuilds the
// GL share group after a real GPU reset (or a MIGO_debug_gpu_reset
// simulation), it drives these events so the engine can drop and rebuild its
// own GL resources: `webglcontextlost` then, once the fresh context is ready,
// `webglcontextrestored`. Dispatched on the main (onscreen) canvas, which is
// where engines register their listeners. Invoked from the host via
// `_internalTriggerWebglContextEvent` (see 98_global_scope_window.js).
//
// NON-SPEC RECOVERY MODEL: unlike browsers, Migo recovery is MANDATORY and
// automatic -- the render thread always tears down and rebuilds the share group
// and only reports success after a probe. Recovery is therefore NOT gated on
// the listener calling `preventDefault()`. The event is still exposed as
// `cancelable` for engine compatibility, and `preventDefault()` is honored as a
// signal (returned to the host) rather than as the trigger for restoration.
// Engines should treat all GL resources as invalid on `webglcontextlost` and
// rebuild them on `webglcontextrestored`; `gl.isContextLost()` reflects the
// render-thread-owned authoritative state at all times.
//
// Returns `true` if a listener called `preventDefault()`, else `false`.
const dispatchWebglContextEvent = (type) => {
    // On context loss: discard (do NOT submit) any pending GL stream commands.
    // Dead-context commands must never reach the render thread; submitting them
    // would corrupt ordering and potentially trigger use-after-free on the render
    // side. Discard before any early return and before dispatching to game listeners
    // so the stream is clean regardless of what listeners do (design S9).
    if (type === "webglcontextlost") {
        discardRenderCommandStream();
    }
    // The capability shadow in the WebGL facade describes a context that is
    // going away, and then a different one that comes back at its GL initial
    // state. Bump on both edges: on loss so a query between the two events does
    // not answer from the dead context's state, and on restore so an `enable`
    // a listener issued while the context was lost -- whose command was
    // discarded above and never reached a driver -- does not survive into the
    // fresh one.
    _bumpCapabilityGeneration();
    if (!_mainCanvas) return false;
    let prevented = false;
    const event = {
        type,
        statusMessage: "",
        bubbles: false,
        cancelable: type === "webglcontextlost",
        get defaultPrevented() { return prevented; },
        preventDefault() { if (type === "webglcontextlost") prevented = true; },
    };
    _mainCanvas.dispatchEvent(event);
    return prevented;
};

// The host's surface-change ingress.
//
// Deliberately here, in the always-compiled canvas module, rather than beside
// `migo.onWindowResize`. Adopting the surface size is not a connectivity feature:
// a Slim build has no window-info service and still has a surface, and a canvas
// that keeps the size the window had before a rotation is a rendering defect,
// not a missing API. It used to live in the `system` extension, which
// `api-connectivity` gates out, so no Slim build followed its surface at all.
//
// The optional half - reading window geometry and telling content - registers
// itself through `setWindowResizeReporter` when it is compiled in.
let _windowResizeReporter = null;

const setWindowResizeReporter = (reporter) => {
    _windowResizeReporter = typeof reporter === "function" ? reporter : null;
};

const handleSurfaceResized = () => {
    // First, and unconditionally. The main canvas is what the content draws
    // into, so a listener that reads `canvas.width` must not be handed the size
    // the surface had a moment ago. Ahead of the reporter for the same reason a
    // host with no window-info service still has a surface: whether content
    // learns the new geometry must not decide whether the canvas has it.
    adoptMainCanvasSurfaceSize();
    if (_windowResizeReporter === null) return;
    try {
        _windowResizeReporter();
    } catch (e) {
        console.error("window resize reporter failed:", e);
    }
};

export {
    createCanvas,
    createOffscreenCanvas,
    getMainCanvas,
    adoptMainCanvasSurfaceSize,
    setWindowResizeReporter,
    handleSurfaceResized,
    dispatchWebglContextEvent,
};
