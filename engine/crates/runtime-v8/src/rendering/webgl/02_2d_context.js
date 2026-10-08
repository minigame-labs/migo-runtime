/**
 * Canvas 2D Context - Command Batching Implementation
 *
 * All draw commands within a RAF frame are batched and sent as a single
 * message to the render thread, significantly reducing IPC overhead.
 */

import {
    flushRenderCommandStream,
    uploadTexImageSource,
    TEX_SOURCE_CALL_IMAGE_2D,
    TEX_SOURCE_SNAPSHOT,
    TEX_SOURCE_NO_PIXELS,
    encode2dBeginPath,
    encode2dClosePath,
    encode2dMoveTo,
    encode2dLineTo,
    encode2dQuadraticCurveTo,
    encode2dBezierCurveTo,
    encode2dArc,
    encode2dArcTo,
    encode2dRect,
    encode2dEllipse,
    encode2dRoundRect,
    encode2dFillPath,
    encode2dStrokePath,
    encode2dClipPath,
    encode2dFill,
    encode2dStroke,
    encode2dClip,
    encode2dFillEvenOdd,
    encode2dDrawCanvas,
    encode2dCaptureImage,
    encode2dClipEvenOdd,
    encode2dFillRect,
    encode2dStrokeRect,
    encode2dClearRect,
    encode2dSave,
    encode2dRestore,
    encode2dSetTransform,
    encode2dResetTransform,
    encode2dReset,
    encode2dTranslate,
    encode2dRotate,
    encode2dScale,
    encode2dSetLineWidth,
    encode2dSetGlobalAlpha,
    encode2dSetMiterLimit,
    encode2dSetLineDashOffset,
    encode2dSetShadowBlur,
    encode2dSetShadowOffsetX,
    encode2dSetShadowOffsetY,
    encode2dSetLineCap,
    encode2dSetLineJoin,
    encode2dSetCompositeOperation,
    encode2dSetImageSmoothing,
    encode2dSetFillStyle,
    encode2dSetStrokeStyle,
    encode2dSetShadowColor,
} from "./00_render_command_stream.js";

import {
    op_create_context_2d,
    op_measure_text_flat,
    // Frame lifecycle
    op_frame_end_unified,
    op_get_image_data,
    op_capture_canvas2d_snapshot,
    op_capture_canvas2d_snapshot_for_cache,
    op_force_readback_snapshot,
    // Text texture cache
    op_text_cache_peek_pin,
    op_text_cache_unpin,
    op_tex_image_2d_from_text_cache,
    // Text methods
    op_fill_text,
    op_stroke_text,
    // Style setters
    op_set_font,
    op_set_text_align,
    op_set_text_baseline,
    op_set_text_direction,
    // Image methods
    op_draw_image,
    op_draw_image_batch,
    op_create_image,
    op_destroy_image,
    // Compositing + gradient + dash
    op_set_line_dash,
    op_set_fill_style_gradient,
    op_set_stroke_style_gradient,
    op_set_fill_style_pattern,
    op_set_stroke_style_pattern,
    op_put_image_data,
    // Path2D
    op_canvas2d_draw_path,
    op_canvas2d_hit_test,
    // Context loss, which the 2D context shares with every other on the GPU
    op_gl_is_context_lost,
} from "ext:core/ops";
import { domException } from "ext:host_v8_base/06_dom_exception.js";
import { isImage, isImageBitmap } from "ext:host_v8_image/01_image.js";
import { primordials } from "ext:core/mod.js";

// Line cap constants
const LINE_CAP_MAP = { 'butt': 0, 'round': 1, 'square': 2 };
// Line join constants
const LINE_JOIN_MAP = { 'miter': 0, 'round': 1, 'bevel': 2 };
// Text align constants
const TEXT_ALIGN_MAP = { 'start': 0, 'end': 1, 'left': 2, 'right': 3, 'center': 4 };
// Text baseline constants
const TEXT_BASELINE_MAP = {
    'top': 0, 'hanging': 1, 'middle': 2,
    'alphabetic': 3, 'ideographic': 4, 'bottom': 5,
};
// Text direction constants - match protocol::TextDirection order.
// Canvas 2D spec accepts "inherit" | "ltr" | "rtl"; unknown values
// are treated as "inherit" (browser-compatible no-op).
const TEXT_DIRECTION_MAP = { 'inherit': 0, 'ltr': 1, 'rtl': 2 };

// The exception the specification names for a rejected argument: the adapter's `DOMException` when
// one was installed, the engine's own otherwise (see base/06_dom_exception.js). A missing global
// turned `getImageData(0, 0, 0, 1)` into a ReferenceError instead of an IndexSizeError.
// Canvas 2D methods that take numbers return without doing anything when any of
// them is NaN or infinite, and a transform or a path that took one would poison
// every later draw (one `translate(NaN, 0)` blanks the canvas). `n - n` is 0
// for every finite number and NaN for NaN, Infinity and anything that is not a
// number, in one subtraction.
function _fin1(a) { return a - a === 0; }
function _fin2(a, b) { return a - a === 0 && b - b === 0; }
function _fin4(a, b, c, d) { return a - a === 0 && b - b === 0 && c - c === 0 && d - d === 0; }
function _fin6(a, b, c, d, e, f) {
    return a - a === 0 && b - b === 0 && c - c === 0 && d - d === 0 && e - e === 0 && f - f === 0;
}

// Composite operation names indexed to stable u8 opcodes consumed by the
// Rust render thread.  The first 11 entries preserve the legacy numbering
// (so pre-existing bytecode keeps the same behaviour); entries 11..25 are
// the advanced / non-separable modes added with the Skia migration.
// See engine/crates/graphics/backend/gl/blend_mode.rs for the canonical
// table.
const _COMPOSITE_OPS = [
    'source-over', 'source-in', 'source-out', 'source-atop',
    'destination-over', 'destination-in', 'destination-out', 'destination-atop',
    'lighter', 'copy', 'xor',
    'multiply', 'screen', 'overlay', 'darken', 'lighten',
    'color-dodge', 'color-burn', 'hard-light', 'soft-light',
    'difference', 'exclusion',
    'hue', 'saturation', 'color', 'luminosity',
];

// The canvas copies `createPattern(canvas)` made are the renderer's image-store entries; one goes when the
// pattern that holds it is collected.
const {
    SafeFinalizationRegistry,
    ArrayBuffer,
    ArrayIsArray,
    ArrayPrototypePush,
    Float32Array,
    Float64Array,
    MathAbs,
    MathFround,
    ReflectApply,
    StringPrototypeCharCodeAt,
    StringPrototypeSlice,
    SymbolIterator,
    TypedArrayPrototypeGetLength,
    TypedArrayPrototypeGetSymbolToStringTag,
    TypedArrayPrototypeSet,
    TypedArrayPrototypeSubarray,
    Uint32Array,
} = primordials;
const _patternCopies = new SafeFinalizationRegistry((imageId) => {
    try {
        // Whatever the stream still holds that names this copy goes first: a destroy that overtook the pattern's
        // last fill would draw with an image the renderer no longer has.
        flushRenderCommandStream();
        op_destroy_image(imageId);
    } catch (_) { }
});

// Gradient object returned by createLinearGradient / createRadialGradient.
// Collects color stops and sends them to the render thread when assigned
// to fillStyle.
const MAX_CANVAS_SURFACE_PIXELS = 8192 * 8192;
const MAX_SYNC_CANVAS_RGBA_BYTES = 64 * 1024 * 1024;
const MAX_DRAW_IMAGE_BATCH_ENTRIES = 65_536;

// The size of an image the engine makes for content: `width` and `height` in pixels, already integers, non-zero. A
// negative one counts from the other corner, so its magnitude is the size. Past the implementation limit -- what a
// synchronous readback may hold -- it is a RangeError, as an allocation that fails is.
function checkedImageDataDimensions(width, height) {
    if (width === 0 || height === 0) {
        throw domException(
            "ImageData width and height must be non-zero",
            "IndexSizeError",
        );
    }
    width = Math.abs(width);
    height = Math.abs(height);
    const pixels = width * height;
    const bytes = pixels * 4;
    if (!Number.isSafeInteger(pixels) || pixels > MAX_CANVAS_SURFACE_PIXELS
            || !Number.isSafeInteger(bytes) || bytes > MAX_SYNC_CANVAS_RGBA_BYTES) {
        throw new RangeError("Canvas ImageData dimensions exceed the implementation limit");
    }
    return { width, height, bytes };
}

// WebIDL's `[EnforceRange] long`: a number that is finite and, truncated, a signed 32-bit integer -- anything else is a
// TypeError, a Symbol or a BigInt included (`+` throws for both).
function _enforceRangeLong(value, context) {
    const x = +value;
    if (x - x !== 0) throw new TypeError(`${context}: Value is not of type 'long'.`);
    const n = Math.trunc(x);
    if (n < -2147483648 || n > 2147483647) {
        throw new TypeError(`${context}: Value is outside the 'long' value range.`);
    }
    return n === 0 ? 0 : n;
}

// WebIDL's `unsigned long`: ToNumber, then modulo 2^32 with NaN and the infinities 0.
function _unsignedLong(value) {
    return (+value) >>> 0;
}

// An `ImageDataSettings` dictionary's colour space, or `undefined` when it names none. `undefined` and `null` are the
// empty dictionary; any other value that is not an object is not a dictionary.
function _imageDataColorSpace(settings, context) {
    if (settings === undefined || settings === null) return undefined;
    if (typeof settings !== 'object' && typeof settings !== 'function') {
        throw new TypeError(`${context}: The provided value is not of type 'ImageDataSettings'.`);
    }
    const colorSpace = settings.colorSpace;
    if (colorSpace === undefined) return undefined;
    const name = `${colorSpace}`;
    if (name !== 'srgb' && name !== 'display-p3') {
        throw new TypeError(`${context}: The provided value '${name}' is not a valid enum value of type PredefinedColorSpace.`);
    }
    return name;
}

// `getContext("2d", settings)`'s CanvasRenderingContext2DSettings, converted as WebIDL converts a dictionary: `undefined`
// and `null` are the empty one, any other value that is not an object is not one, and the members are read in their
// order -- alpha, colorSpace, colorType, desynchronized, willReadFrequently -- a keyword that is not one of the enum's a
// TypeError. Only `willReadFrequently` is kept: it is a hint, and the one this context can report as asked. A canvas
// here is sRGB, 8 bits a channel and has alpha, whatever is asked, and `getContextAttributes` says what it is.
function _contextSettings(settings) {
    const context = "Failed to execute 'getContext' on 'HTMLCanvasElement'";
    if (settings === undefined || settings === null) return { willReadFrequently: false };
    if (typeof settings !== 'object' && typeof settings !== 'function') {
        throw new TypeError(`${context}: The provided value is not of type 'CanvasRenderingContext2DSettings'.`);
    }
    void !!settings.alpha;
    const colorSpace = settings.colorSpace;
    if (colorSpace !== undefined) {
        const name = _domString(colorSpace);
        if (name !== 'srgb' && name !== 'display-p3') {
            throw new TypeError(`${context}: The provided value '${name}' is not a valid enum value of type PredefinedColorSpace.`);
        }
    }
    const colorType = settings.colorType;
    if (colorType !== undefined) {
        const name = _domString(colorType);
        if (name !== 'unorm8' && name !== 'float16') {
            throw new TypeError(`${context}: The provided value '${name}' is not a valid enum value of type CanvasColorType.`);
        }
    }
    void !!settings.desynchronized;
    return { willReadFrequently: !!settings.willReadFrequently };
}

// ---- Display P3 ----
//
// A canvas here is sRGB. An `ImageData` in Display P3 is read from it, and written to it, converted: the two share the
// sRGB transfer function and differ in primaries, so a byte is decoded to linear light, the three channels are mapped
// through the 3x3 matrix between the primaries, clipped to the gamut written to, and encoded again. Alpha is not a
// colour and is not touched, and the bytes are unpremultiplied, as an ImageData's are.
const _SRGB_TO_LINEAR = new Float64Array(256);
for (let i = 0; i < 256; i++) {
    const c = i / 255;
    _SRGB_TO_LINEAR[i] = c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}
function _linearToByte(l) {
    if (!(l > 0)) return 0;
    if (l >= 1) return 255;
    return Math.round((l <= 0.0031308 ? 12.92 * l : 1.055 * l ** (1 / 2.4) - 0.055) * 255);
}
// Linear sRGB to linear Display P3, and back.
const _SRGB_TO_P3 = [0.8224621, 0.177538, 0, 0.0331941, 0.9668058, 0, 0.0170827, 0.0723974, 0.9105199];
const _P3_TO_SRGB = [1.2249401, -0.2249404, 0, -0.0420569, 1.0420571, 0, -0.0196376, -0.0786361, 1.0982735];
function _convertPrimaries(bytes, m) {
    for (let i = 0; i < bytes.length; i += 4) {
        const r = _SRGB_TO_LINEAR[bytes[i]];
        const g = _SRGB_TO_LINEAR[bytes[i + 1]];
        const b = _SRGB_TO_LINEAR[bytes[i + 2]];
        bytes[i] = _linearToByte(m[0] * r + m[1] * g + m[2] * b);
        bytes[i + 1] = _linearToByte(m[3] * r + m[4] * g + m[5] * b);
        bytes[i + 2] = _linearToByte(m[6] * r + m[7] * g + m[8] * b);
    }
    return bytes;
}

// The interface members WebIDL defines on a prototype are enumerable, and an instance's class string is the
// interface's name; a JavaScript class makes neither so. Applied to each interface this file exposes.
function _exposeInterface(cls, name) {
    const proto = cls.prototype;
    for (const key of Object.getOwnPropertyNames(proto)) {
        if (key === 'constructor' || key.charCodeAt(0) === 95 /* _ */) continue;
        const descriptor = Object.getOwnPropertyDescriptor(proto, key);
        descriptor.enumerable = true;
        Object.defineProperty(proto, key, descriptor);
    }
    Object.defineProperty(proto, Symbol.toStringTag, { value: name, configurable: true });
}

// ==================== ImageData ====================
//
// The pixels `getImageData` reads and `putImageData` writes, and what `new ImageData` makes: a width, a height, the
// RGBA bytes (unpremultiplied, row by row) and the colour space they are in. A class of its own because content
// constructs it and tests for it -- PixiJS's extract and Egret's render textures call `new ImageData`, three.js asks
// `image instanceof ImageData` -- and because `putImageData` takes nothing else: its argument is checked for being one,
// not for looking like one.
//
// The engine's own (a readback, a blank image) are built through `_IMAGE_DATA`, which content cannot name. Two of them
// hold no bytes yet: a `getImageData` the renderer answered with a snapshot, and one answered from the text cache. Their
// `data` is read back the first time it is asked for, by the function they were made with (see
// `_migoMakeSnapshotImageData`).
const _IMAGE_DATA = Symbol('ImageData');
let _imageDataFields;
class ImageData {
    #width;
    #height;
    #data;
    #colorSpace;
    #materialize = null;

    // (sw, sh, settings) or (data, sw, sh, settings): two parameters as WebIDL counts them, the rest from `arguments`.
    constructor(source, sw) {
        const argc = arguments.length;
        if (source === _IMAGE_DATA) {
            this.#width = sw;
            this.#height = arguments[2];
            this.#data = arguments[3];
            this.#colorSpace = arguments[4];
            this.#materialize = arguments[5] ?? null;
            return;
        }
        const context = "Failed to construct 'ImageData'";
        if (argc < 2) throw new TypeError(`${context}: 2 arguments required, but only ${argc} present.`);
        if (ArrayBuffer.isView(source) && TypedArrayPrototypeGetSymbolToStringTag(source) === 'Uint8ClampedArray') {
            const width = _unsignedLong(sw);
            const height = arguments[2] === undefined ? undefined : _unsignedLong(arguments[2]);
            const colorSpace = _imageDataColorSpace(arguments[3], context);
            const length = TypedArrayPrototypeGetLength(source);
            if (length === 0) throw domException(`${context}: The input data has zero elements.`, 'InvalidStateError');
            if (length % 4 !== 0) {
                throw domException(`${context}: The input data length is not a multiple of 4.`, 'InvalidStateError');
            }
            if (width === 0) throw domException(`${context}: The source width is zero or not a number.`, 'IndexSizeError');
            const pixels = length / 4;
            if (pixels % width !== 0) {
                throw domException(`${context}: The input data length is not a multiple of (4 * width).`, 'IndexSizeError');
            }
            if (height !== undefined && height * width !== pixels) {
                throw domException(
                    `${context}: The input data length is not equal to (4 * width * height).`, 'IndexSizeError');
            }
            // The array is the image's, not a copy of it.
            this.#width = width;
            this.#height = pixels / width;
            this.#data = source;
            this.#colorSpace = colorSpace ?? 'srgb';
            return;
        }
        const width = _unsignedLong(source);
        const height = _unsignedLong(sw);
        const colorSpace = _imageDataColorSpace(arguments[2], context);
        if (width === 0) throw domException(`${context}: The source width is zero or not a number.`, 'IndexSizeError');
        if (height === 0) throw domException(`${context}: The source height is zero or not a number.`, 'IndexSizeError');
        const dimensions = checkedImageDataDimensions(width, height);
        this.#width = width;
        this.#height = height;
        this.#data = new Uint8ClampedArray(dimensions.bytes);
        this.#colorSpace = colorSpace ?? 'srgb';
    }

    get width() { return this.#width; }
    get height() { return this.#height; }
    get data() {
        const materialize = this.#materialize;
        if (materialize !== null) {
            this.#materialize = null;
            materialize(this, this.#data);
        }
        return this.#data;
    }
    get colorSpace() { return this.#colorSpace; }

    static {
        // What `putImageData` and `createImageData(imagedata)` read of one, by its own fields, so a prototype content
        // replaced is not asked: `[width, height, data, colorSpace]`, or `null` for a value that is not an ImageData.
        _imageDataFields = (value) => {
            if (value === null || typeof value !== 'object' || !(#data in value)) return null;
            const materialize = value.#materialize;
            if (materialize !== null) {
                value.#materialize = null;
                materialize(value, value.#data);
            }
            return [value.#width, value.#height, value.#data, value.#colorSpace];
        };
    }
}
_exposeInterface(ImageData, 'ImageData');

// ==================== TextMetrics ====================
//
// `measureText`'s answer: the twelve numbers `op_measure_text_flat` returns, read through getters, so a measurement is
// read-only as it is in a browser. A new object each call, over numbers that may be shared: the measure cache keeps the
// numbers, not the object content was handed.
const _TEXT_METRICS = Symbol('TextMetrics');
class TextMetrics {
    #m;
    constructor() {
        if (arguments[0] !== _TEXT_METRICS) throw new TypeError("Illegal constructor");
        this.#m = arguments[1];
    }
    get width() { return this.#m[0]; }
    get actualBoundingBoxLeft() { return this.#m[1]; }
    get actualBoundingBoxRight() { return this.#m[2]; }
    get fontBoundingBoxAscent() { return this.#m[9]; }
    get fontBoundingBoxDescent() { return this.#m[6]; }
    get actualBoundingBoxAscent() { return this.#m[7]; }
    get actualBoundingBoxDescent() { return this.#m[8]; }
    get emHeightAscent() { return this.#m[3]; }
    get emHeightDescent() { return this.#m[4]; }
    get hangingBaseline() { return this.#m[10]; }
    get alphabeticBaseline() { return this.#m[5]; }
    get ideographicBaseline() { return this.#m[11]; }
}
_exposeInterface(TextMetrics, 'TextMetrics');

// The constructions only this file makes: a gradient or a pattern comes from the context, never from `new`.
const _CANVAS_STYLE = Symbol('canvas style');

class CanvasGradient {
    // (type, canvasId, x0, y0, r0, x1, y1, r1), after the token: no parameters, as WebIDL counts them.
    constructor() {
        if (arguments[0] !== _CANVAS_STYLE) throw new TypeError("Illegal constructor");
        const [, type, canvasId, x0, y0, r0, x1, y1, r1] = arguments;
        this._type = type;
        this._canvasId = canvasId;
        this._x0 = x0;
        this._y0 = y0;
        this._r0 = r0;
        this._x1 = x1;
        this._y1 = y1;
        this._r1 = r1;
        this._stops = [];
    }
    // `addColorStop(offset, color)`: a `double` and a DOMString, then the specification's order -- an offset outside
    // 0..1 is an IndexSizeError, a colour that does not parse a SyntaxError.
    addColorStop(offset, color) {
        _requireArguments(arguments.length, 2, 'addColorStop', 'CanvasGradient');
        const off = _double(offset, 'addColorStop', 'CanvasGradient');
        const text = _domString(color);
        if (off < 0 || off > 1) {
            throw domException("Failed to execute 'addColorStop' on 'CanvasGradient': The provided value (" + off +
                ") is outside the range (0.0, 1.0).", "IndexSizeError");
        }
        const entry = _cssColour(text);
        if (entry === null) {
            throw domException("Failed to execute 'addColorStop' on 'CanvasGradient': The value provided ('" + text +
                "') could not be parsed as a color.", "SyntaxError");
        }
        const parsed = entry.rgba;
        this._stops.push({ offset: off, r: parsed[0], g: parsed[1], b: parsed[2], a: parsed[3] });
        // Stable: stops at one offset keep the order they were added in.
        this._stops.sort(function (a, b) { return a.offset - b.offset; });
    }
    // Called internally when this gradient is assigned to fillStyle.
    _apply() {
        if (this._stops.length < 2) return;
        flushRenderCommandStream();
        op_set_fill_style_gradient(
            this._canvasId,
            this._type === 'radial' ? 1 : this._type === 'conic' ? 2 : 0,
            this._x0, this._y0, this._r0,
            this._x1, this._y1, this._r1,
            JSON.stringify(this._stops)
        );
    }
    // Called internally when this gradient is assigned to strokeStyle.
    _applyStroke() {
        if (this._stops.length < 2) return;
        flushRenderCommandStream();
        op_set_stroke_style_gradient(
            this._canvasId,
            this._type === 'radial' ? 1 : this._type === 'conic' ? 2 : 0,
            this._x0, this._y0, this._r0,
            this._x1, this._y1, this._r1,
            JSON.stringify(this._stops)
        );
    }
}
_exposeInterface(CanvasGradient, 'CanvasGradient');

class CanvasPattern {
    // (canvasId, imageRid, repetition), after the token.
    constructor() {
        if (arguments[0] !== _CANVAS_STYLE) throw new TypeError("Illegal constructor");
        // The repetition is one of the four names: `createPattern` checked it.
        const [, canvasId, imageRid, repetition] = arguments;
        this._canvasId = canvasId;
        this._imageRid = imageRid;
        this._repeatX = repetition === 'repeat' || repetition === 'repeat-x';
        this._repeatY = repetition === 'repeat' || repetition === 'repeat-y';
    }
    _applyFill() {
        flushRenderCommandStream();
        op_set_fill_style_pattern(this._canvasId, this._imageRid, this._repeatX, this._repeatY);
    }
    _applyStroke() {
        flushRenderCommandStream();
        op_set_stroke_style_pattern(this._canvasId, this._imageRid, this._repeatX, this._repeatY);
    }
}
_exposeInterface(CanvasPattern, 'CanvasPattern');

// ==================== Path2D ====================
//
// A `Path2D` is the CanvasPath calls content made on it, kept as the segments the renderer builds a path from
// (`frame_wire::canvas2d::path2d`): each an opcode word and its arguments, floats as `f32` bits and flags as 0 or 1. It
// travels with each use -- a fill, a stroke, a clip, a hit test -- and the renderer builds it with the code that builds
// the current default path, so the two cannot differ. Its arguments are checked as the context's are: a call with an
// argument that is not finite adds nothing, a negative radius throws.

// `frame_wire::canvas2d::path2d`'s opcodes and bounds, held to it by render_stream_js_agreement.rs.
const SEG_MOVE_TO = 1;
const SEG_LINE_TO = 2;
const SEG_QUADRATIC_CURVE_TO = 3;
const SEG_BEZIER_CURVE_TO = 4;
const SEG_ARC = 5;
const SEG_ARC_TO = 6;
const SEG_ELLIPSE = 7;
const SEG_RECT = 8;
const SEG_ROUND_RECT = 9;
const SEG_CLOSE_PATH = 10;
const SEG_SVG_ARC_TO = 11;
const SEG_ADD_PATH = 12;
const MAX_PATH_WORDS = 262144;
const MAX_PATH_NESTING = 32;

// `isPointInPath` / `isPointInStroke`'s flags, `frame_wire::sync::canvas2d_hit_test`'s.
const HIT_TEST_STROKE = 1;
const HIT_TEST_EVEN_ODD = 2;
const HIT_TEST_PATH = 4;

// The largest finite `f32`. A coordinate past it would be an infinity on the wire, which the reader refuses, so it is
// clamped to the largest the wire carries -- what a browser's float path does with it.
const F32_MAX = 3.4028234663852886e38;
function _f32Clamp(v) { return v > F32_MAX ? F32_MAX : v < -F32_MAX ? -F32_MAX : v; }

const _NO_SEGMENTS = new Uint32Array(0);

// WebIDL's "n arguments required" for a method whose required arguments were not all passed.
function _requireArguments(given, required, method, owner) {
    if (given < required) {
        throw new TypeError("Failed to execute '" + method + "' on '" + owner + "': " + required +
            " argument" + (required === 1 ? "" : "s") + " required, but only " + given + " present.");
    }
}

// An overloaded call with a count no overload takes (`drawImage` with four arguments).
function _arityError(given, arities, method, owner) {
    return new TypeError("Failed to execute '" + method + "' on '" + owner + "': Valid arities are: [" + arities +
        "], but " + given + " arguments provided.");
}

// WebIDL's DOMString: a string is itself, anything else is converted -- and a Symbol refuses, with a TypeError.
function _domString(value) {
    return typeof value === 'string' ? value : `${value}`;
}

// WebIDL's `double`: a finite number, or a TypeError. The factories take these (the gradients, `addColorStop`) and throw
// where the drawing calls, which take `unrestricted double`, draw nothing.
function _double(value, method, owner) {
    const x = +value;
    if (x - x !== 0) {
        throw new TypeError("Failed to execute '" + method + "' on '" + owner + "': The provided double value is non-finite.");
    }
    return x;
}

// WebIDL's `sequence<unrestricted double>`: an object content can iterate, each item a number. A string is not an
// object, and an object with no iterator is not a sequence.
function _numberSequence(value, method, owner) {
    if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
        throw new TypeError("Failed to execute '" + method + "' on '" + owner +
            "': The provided value cannot be converted to a sequence.");
    }
    const out = [];
    for (const item of value) ArrayPrototypePush(out, +item);
    return out;
}

// ---- Image arguments ----
//
// `drawImage` and `createPattern` take a CanvasImageSource: here an image, an ImageBitmap or a canvas, each the engine's
// own (the image classes by their private fields, a canvas by its renderer id). Anything else -- `null`, an ImageData,
// an object with the right names -- is a TypeError, as the WebIDL union has it. What is then done with one is the
// specification's "check the usability of the image argument": a broken image or a closed bitmap is an
// InvalidStateError, as is a canvas with no pixels; an image not yet loaded draws nothing.
const _SOURCE_IMAGE = 1;    // an image or a bitmap: its renderer id is drawn
const _SOURCE_CANVAS = 2;   // a canvas: the renderer copies it in stream order

function _imageSourceKind(image, method) {
    if (isImage(image) || isImageBitmap(image)) return _SOURCE_IMAGE;
    if (image !== null && typeof image === 'object' && typeof image.getContext === 'function'
            && typeof image._rid === 'number') {
        return _SOURCE_CANVAS;
    }
    throw new TypeError("Failed to execute '" + method + "' on 'CanvasRenderingContext2D': The provided value is not " +
        "of type '(HTMLCanvasElement or HTMLImageElement or ImageBitmap)'.");
}

// Whether the image can be drawn now: `false` for an image still loading, a throw for one that never will be.
function _imageUsable(image, kind, method) {
    const context = "Failed to execute '" + method + "' on 'CanvasRenderingContext2D': ";
    if (kind === _SOURCE_CANVAS) {
        if (image.width === 0 || image.height === 0) {
            throw domException(context + "The image argument is a canvas element with a width or height of 0.",
                "InvalidStateError");
        }
        return true;
    }
    if (isImageBitmap(image)) {
        if (image.rid === 0) throw domException(context + "The image source is detached.", "InvalidStateError");
        return true;
    }
    if (image.error) {
        throw domException(context + "The HTMLImageElement provided is in the 'broken' state.", "InvalidStateError");
    }
    return image.loaded === true;
}

function _negativeRadius(value) {
    return domException("The radius provided (" + value + ") is negative.", "IndexSizeError");
}

// A `Path2D` argument, or WebIDL's TypeError for anything else.
function _path2DArgument(value, method) {
    if (_isPath2D(value)) return value;
    throw new TypeError("Failed to execute '" + method + "' on 'CanvasRenderingContext2D': parameter 1 is not of type 'Path2D'.");
}

// `CanvasFillRule`: undefined is the default, "nonzero"; anything else is converted to a string and must be one of the
// two names.
function _fillRule(value, method) {
    if (value === undefined || value === 'nonzero') return false;
    if (value === 'evenodd') return true;
    const name = `${value}`;
    if (name === 'nonzero') return false;
    if (name === 'evenodd') return true;
    throw new TypeError("Failed to execute '" + method + "' on 'CanvasRenderingContext2D': The provided value '" +
        name + "' is not a valid enum value of type CanvasFillRule.");
}

// `roundRect`'s radii as WebIDL converts `(unrestricted double or DOMPointInit or sequence<(unrestricted double or
// DOMPointInit)>)`, before any of them is checked: a number, a point `{x, y}`, or a list of those.
function _radiusOf(value) {
    if (value === undefined || value === null || typeof value === 'object' || typeof value === 'function') {
        return _domPointInit(value);
    }
    return +value;
}

// A dictionary member: absent, or converted to a number.
const _member = (value) => (value === undefined ? undefined : +value);
const _sameValueZero = (a, b) => a === b || (a !== a && b !== b);

// A `DOMPointInit`, its members read and converted in WebIDL's order -- w, x, y, z -- of which the radii use x and y.
function _domPointInit(value) {
    if (value === undefined || value === null) return { x: 0, y: 0 };
    _member(value.w);
    const x = _member(value.x) ?? 0;
    const y = _member(value.y) ?? 0;
    _member(value.z);
    return { x, y };
}

function _convertRadii(radii) {
    if (radii === null || (typeof radii !== 'object' && typeof radii !== 'function')) return _radiusOf(radii);
    const method = radii[SymbolIterator];
    if (method === undefined || method === null) return _domPointInit(radii);
    if (typeof method !== 'function') throw new TypeError("The provided radii are not iterable.");
    const iterator = ReflectApply(method, radii, []);
    if (iterator === null || (typeof iterator !== 'object' && typeof iterator !== 'function')) {
        throw new TypeError("The radii's iterator is not an object.");
    }
    const next = iterator.next;
    const list = [];
    for (;;) {
        const step = ReflectApply(next, iterator, []);
        if (step === null || (typeof step !== 'object' && typeof step !== 'function')) {
            throw new TypeError("The radii's iterator result is not an object.");
        }
        if (step.done) return list;
        ArrayPrototypePush(list, _radiusOf(step.value));
    }
}

// Which radius each corner takes -- top left, top right, bottom right, bottom left -- for a list of one to four.
const _CORNERS = [null, [0, 0, 0, 0], [0, 1, 0, 1], [0, 1, 2, 1], [0, 1, 2, 3]];

// The corners' `rx ry` into `out` from converted radii, by the specification's steps in its order: a list of other than
// one to four is a RangeError; then, radius by radius, one that is not finite makes the call do nothing (false) and a
// negative one is a RangeError.
function _cornerRadii(radii, out, method) {
    if (typeof radii === 'number') {
        if (radii - radii !== 0) return false;
        if (radii < 0) throw new RangeError("Failed to execute '" + method + "': Radius value " + radii + " is negative.");
        for (let i = 0; i < 8; i++) out[i] = radii;
        return true;
    }
    const list = ArrayIsArray(radii) ? radii : [radii];
    const size = list.length;
    if (size < 1 || size > 4) {
        throw new RangeError("Failed to execute '" + method + "': " + size +
            " radii provided. Between one and four radii are necessary.");
    }
    for (let i = 0; i < size; i++) {
        const radius = list[i];
        const x = typeof radius === 'number' ? radius : radius.x;
        const y = typeof radius === 'number' ? radius : radius.y;
        if (!_fin2(x, y)) return false;
        if (x < 0 || y < 0) {
            throw new RangeError("Failed to execute '" + method + "': Radius value " + (x < 0 ? x : y) + " is negative.");
        }
    }
    const corners = _CORNERS[size];
    for (let corner = 0; corner < 4; corner++) {
        const radius = list[corners[corner]];
        out[corner * 2] = typeof radius === 'number' ? radius : radius.x;
        out[corner * 2 + 1] = typeof radius === 'number' ? radius : radius.y;
    }
    return true;
}

// A `DOMMatrix2DInit`, validated and fixed up as the specification's "create a DOMMatrix from the 2D dictionary" does,
// into `out` as `a b c d e f`. Its members are read in WebIDL's order.
// `setTransform(transform)`'s matrix, read into the same six numbers every call.
const _setTransformMatrix = [1, 0, 0, 1, 0, 0];

function _matrix2DInit(init, out, method, owner) {
    if (init === undefined || init === null) {
        out[0] = 1; out[1] = 0; out[2] = 0; out[3] = 1; out[4] = 0; out[5] = 0;
        return;
    }
    if (typeof init !== 'object' && typeof init !== 'function') {
        throw new TypeError("Failed to execute '" + method + "' on '" + owner +
            "': The provided value is not of type 'DOMMatrix2DInit'.");
    }
    const a = _member(init.a), b = _member(init.b), c = _member(init.c);
    const d = _member(init.d), e = _member(init.e), f = _member(init.f);
    const m11 = _member(init.m11), m12 = _member(init.m12), m21 = _member(init.m21);
    const m22 = _member(init.m22), m41 = _member(init.m41), m42 = _member(init.m42);
    if ((a !== undefined && m11 !== undefined && !_sameValueZero(a, m11)) ||
        (b !== undefined && m12 !== undefined && !_sameValueZero(b, m12)) ||
        (c !== undefined && m21 !== undefined && !_sameValueZero(c, m21)) ||
        (d !== undefined && m22 !== undefined && !_sameValueZero(d, m22)) ||
        (e !== undefined && m41 !== undefined && !_sameValueZero(e, m41)) ||
        (f !== undefined && m42 !== undefined && !_sameValueZero(f, m42))) {
        throw new TypeError("Failed to execute '" + method + "' on '" + owner +
            "': Property mismatch on matrix initialization.");
    }
    out[0] = m11 ?? a ?? 1;
    out[1] = m12 ?? b ?? 0;
    out[2] = m21 ?? c ?? 0;
    out[3] = m22 ?? d ?? 1;
    out[4] = m41 ?? e ?? 0;
    out[5] = m42 ?? f ?? 0;
}

// SVG 2 path data (https://www.w3.org/TR/SVG2/paths.html#PathDataBNF), scanned a token at a time.
class _SvgPathScanner {
    constructor(text) {
        this.text = text;
        this.at = 0;
        this.end = text.length;
    }
    skipSpace() {
        const text = this.text;
        while (this.at < this.end) {
            const c = StringPrototypeCharCodeAt(text, this.at);
            if (c !== 0x20 && c !== 0x09 && c !== 0x0A && c !== 0x0C && c !== 0x0D) return;
            this.at++;
        }
    }
    // `comma_wsp?`: space, then at most one comma and the space after it. True when there was a comma.
    skipSeparator() {
        this.skipSpace();
        if (this.at < this.end && StringPrototypeCharCodeAt(this.text, this.at) === 0x2C) {
            this.at++;
            this.skipSpace();
            return true;
        }
        return false;
    }
    peek() {
        return this.at < this.end ? StringPrototypeCharCodeAt(this.text, this.at) : -1;
    }
    // A number, or NaN where there is none -- a digit must follow a point and an exponent's sign -- or where it is one
    // no `f32` holds.
    number() {
        const text = this.text;
        const start = this.at;
        let at = start;
        let c = StringPrototypeCharCodeAt(text, at);
        if (c === 0x2B || c === 0x2D) c = StringPrototypeCharCodeAt(text, ++at);
        let digits = 0;
        while (c >= 0x30 && c <= 0x39) { digits++; c = StringPrototypeCharCodeAt(text, ++at); }
        if (c === 0x2E) {
            c = StringPrototypeCharCodeAt(text, ++at);
            let fraction = 0;
            while (c >= 0x30 && c <= 0x39) { fraction++; c = StringPrototypeCharCodeAt(text, ++at); }
            if (fraction === 0) return NaN;
            digits += fraction;
        }
        if (digits === 0) return NaN;
        if (c === 0x45 || c === 0x65) {
            c = StringPrototypeCharCodeAt(text, ++at);
            if (c === 0x2B || c === 0x2D) c = StringPrototypeCharCodeAt(text, ++at);
            if (!(c >= 0x30 && c <= 0x39)) return NaN;
            while (c >= 0x30 && c <= 0x39) c = StringPrototypeCharCodeAt(text, ++at);
        }
        const value = +StringPrototypeSlice(text, start, at);
        if (!(MathAbs(MathFround(value)) <= F32_MAX)) return NaN;
        this.at = at;
        return value;
    }
    // An arc's flag: one character, 0 or 1; -1 for anything else.
    flag() {
        const c = this.peek();
        if (c !== 0x30 && c !== 0x31) return -1;
        this.at++;
        return c - 0x30;
    }
}

// Arguments per command, by the upper-case letter's code; a letter with none here is not a command.
const _SVG_ARGUMENTS = { 0x4D: 2, 0x4C: 2, 0x48: 1, 0x56: 1, 0x43: 6, 0x53: 4, 0x51: 4, 0x54: 2, 0x41: 7, 0x5A: 0 };
const _svgArguments = new Float64Array(7);
const _startsNumber = (c) => (c >= 0x30 && c <= 0x39) || c === 0x2B || c === 0x2D || c === 0x2E;

// `roundRect`'s corners, written by `_cornerRadii` and read straight after by one encoder.
const _roundRectRadii = new Float64Array(8);

// The friends of Path2D: the brand check and the segments, for the context that draws with one.
let _isPath2D = null;
let _segmentsOf = null;
const _addPathMatrix = new Float64Array(6);

class Path2D {
    // The segments in a buffer that grows by doubling: `#words` and `#floats` are the same bytes, `#length` words of
    // them used.
    #words = null;
    #floats = null;
    #length = 0;
    // How deep the paths added to this one nest.
    #depth = 0;
    // Exactly the used words, made when a draw asks and dropped when a segment is added: a path kept and drawn every
    // frame costs no allocation per draw.
    #view = null;

    static #friends = (
        _isPath2D = (value) => value !== null && typeof value === 'object' && #words in value,
        _segmentsOf = (path) => path.#segments(),
        0
    );

    // `new Path2D()`, `new Path2D(path)` -- a copy -- or `new Path2D(d)`, SVG path data, as far as its first error.
    constructor(path = undefined) {
        if (path === undefined) return;
        if (_isPath2D(path)) {
            const count = path.#length;
            if (count === 0) return;
            const at = this.#reserve(count);
            TypedArrayPrototypeSet(this.#words, TypedArrayPrototypeSubarray(path.#words, 0, count), at);
            this.#depth = path.#depth;
            return;
        }
        this.#parse(`${path}`);
    }

    // Room for `count` more words; where they start. Past the bound a record carries, a RangeError: a path that long
    // could not be drawn.
    #reserve(count) {
        const at = this.#length;
        const need = at + count;
        if (need > MAX_PATH_WORDS) {
            throw new RangeError("The Path2D would exceed the implementation limit of " + MAX_PATH_WORDS + " words.");
        }
        if (this.#words === null || need > TypedArrayPrototypeGetLength(this.#words)) {
            let capacity = this.#words === null ? 64 : TypedArrayPrototypeGetLength(this.#words) * 2;
            while (capacity < need) capacity *= 2;
            if (capacity > MAX_PATH_WORDS) capacity = MAX_PATH_WORDS;
            const buffer = new ArrayBuffer(capacity * 4);
            const words = new Uint32Array(buffer);
            if (this.#words !== null) TypedArrayPrototypeSet(words, TypedArrayPrototypeSubarray(this.#words, 0, at), 0);
            this.#words = words;
            this.#floats = new Float32Array(buffer);
        }
        this.#length = need;
        this.#view = null;
        return at;
    }

    #segments() {
        if (this.#view === null) {
            this.#view = this.#words === null ? _NO_SEGMENTS : TypedArrayPrototypeSubarray(this.#words, 0, this.#length);
        }
        return this.#view;
    }

    #segment2(op, x, y) {
        const at = this.#reserve(3);
        this.#words[at] = op;
        const f = this.#floats;
        f[at + 1] = _f32Clamp(x);
        f[at + 2] = _f32Clamp(y);
    }

    #segment4(op, a, b, c, d) {
        const at = this.#reserve(5);
        this.#words[at] = op;
        const f = this.#floats;
        f[at + 1] = _f32Clamp(a);
        f[at + 2] = _f32Clamp(b);
        f[at + 3] = _f32Clamp(c);
        f[at + 4] = _f32Clamp(d);
    }

    #segment6(op, a, b, c, d, e, g) {
        const at = this.#reserve(7);
        this.#words[at] = op;
        const f = this.#floats;
        f[at + 1] = _f32Clamp(a);
        f[at + 2] = _f32Clamp(b);
        f[at + 3] = _f32Clamp(c);
        f[at + 4] = _f32Clamp(d);
        f[at + 5] = _f32Clamp(e);
        f[at + 6] = _f32Clamp(g);
    }

    // SVG path data, read up to the command that holds its first error: that command and everything after it are not
    // part of the path, as an SVG path element draws.
    #parse(text) {
        const scan = new _SvgPathScanner(text);
        const a = _svgArguments;
        let command = 0;
        // The current point, the current subpath's start, and the control point an S or a T reflects.
        let cx = 0, cy = 0, sx = 0, sy = 0, px = 0, py = 0;
        let previous = 0;
        for (;;) {
            scan.skipSpace();
            const c = scan.peek();
            if (c < 0) return;
            if (c < 0x80 && _SVG_ARGUMENTS[c & ~0x20] !== undefined) {
                command = c;
                scan.at++;
            } else if (command !== 0 && (command & ~0x20) !== 0x5A && _startsNumber(c)) {
                // Arguments with no letter repeat the command before them; a moveto's repeat as linetos.
                if (command === 0x4D) command = 0x4C;
                else if (command === 0x6D) command = 0x6C;
            } else {
                return;
            }
            const upper = command & ~0x20;
            // The path begins with a moveto, or it has no segments.
            if (previous === 0 && upper !== 0x4D) return;
            const relative = command !== upper;
            const ox = relative ? cx : 0;
            const oy = relative ? cy : 0;
            const count = _SVG_ARGUMENTS[upper];
            for (let i = 0; i < count; i++) {
                if (i === 0) scan.skipSpace(); else scan.skipSeparator();
                if (upper === 0x41 && (i === 3 || i === 4)) {
                    const flag = scan.flag();
                    if (flag < 0) return;
                    a[i] = flag;
                } else {
                    const value = scan.number();
                    if (value !== value) return;
                    a[i] = value;
                }
            }
            switch (upper) {
                case 0x4D: // M
                    cx = sx = ox + a[0];
                    cy = sy = oy + a[1];
                    this.#segment2(SEG_MOVE_TO, cx, cy);
                    break;
                case 0x4C: // L
                    cx = ox + a[0];
                    cy = oy + a[1];
                    this.#segment2(SEG_LINE_TO, cx, cy);
                    break;
                case 0x48: // H
                    cx = ox + a[0];
                    this.#segment2(SEG_LINE_TO, cx, cy);
                    break;
                case 0x56: // V
                    cy = oy + a[0];
                    this.#segment2(SEG_LINE_TO, cx, cy);
                    break;
                case 0x43: // C
                case 0x53: { // S
                    let x1, y1, k = 0;
                    if (upper === 0x43) {
                        x1 = ox + a[0];
                        y1 = oy + a[1];
                        k = 2;
                    } else if (previous === 0x43 || previous === 0x53) {
                        x1 = 2 * cx - px;
                        y1 = 2 * cy - py;
                    } else {
                        x1 = cx;
                        y1 = cy;
                    }
                    px = ox + a[k];
                    py = oy + a[k + 1];
                    cx = ox + a[k + 2];
                    cy = oy + a[k + 3];
                    this.#segment6(SEG_BEZIER_CURVE_TO, x1, y1, px, py, cx, cy);
                    break;
                }
                case 0x51: // Q
                case 0x54: { // T
                    let k = 0;
                    if (upper === 0x51) {
                        px = ox + a[0];
                        py = oy + a[1];
                        k = 2;
                    } else if (previous === 0x51 || previous === 0x54) {
                        px = 2 * cx - px;
                        py = 2 * cy - py;
                    } else {
                        px = cx;
                        py = cy;
                    }
                    cx = ox + a[k];
                    cy = oy + a[k + 1];
                    this.#segment4(SEG_QUADRATIC_CURVE_TO, px, py, cx, cy);
                    break;
                }
                case 0x41: { // A
                    cx = ox + a[5];
                    cy = oy + a[6];
                    const at = this.#reserve(8);
                    this.#words[at] = SEG_SVG_ARC_TO;
                    const f = this.#floats;
                    f[at + 1] = _f32Clamp(a[0]);
                    f[at + 2] = _f32Clamp(a[1]);
                    f[at + 3] = _f32Clamp(a[2]);
                    this.#words[at + 4] = a[3];
                    this.#words[at + 5] = a[4];
                    f[at + 6] = _f32Clamp(cx);
                    f[at + 7] = _f32Clamp(cy);
                    break;
                }
                default: { // Z
                    const at = this.#reserve(1);
                    this.#words[at] = SEG_CLOSE_PATH;
                    cx = sx;
                    cy = sy;
                }
            }
            previous = upper;
            // A comma after a command's arguments promises another set of them.
            if (upper !== 0x5A && scan.skipSeparator() && !_startsNumber(scan.peek())) return;
        }
    }

    // ---- CanvasPath ----

    closePath() {
        const at = this.#reserve(1);
        this.#words[at] = SEG_CLOSE_PATH;
    }

    moveTo(x, y) {
        _requireArguments(arguments.length, 2, 'moveTo', 'Path2D');
        x = +x; y = +y;
        if (!_fin2(x, y)) return;
        this.#segment2(SEG_MOVE_TO, x, y);
    }

    lineTo(x, y) {
        _requireArguments(arguments.length, 2, 'lineTo', 'Path2D');
        x = +x; y = +y;
        if (!_fin2(x, y)) return;
        this.#segment2(SEG_LINE_TO, x, y);
    }

    quadraticCurveTo(cpx, cpy, x, y) {
        _requireArguments(arguments.length, 4, 'quadraticCurveTo', 'Path2D');
        cpx = +cpx; cpy = +cpy; x = +x; y = +y;
        if (!_fin4(cpx, cpy, x, y)) return;
        this.#segment4(SEG_QUADRATIC_CURVE_TO, cpx, cpy, x, y);
    }

    bezierCurveTo(cp1x, cp1y, cp2x, cp2y, x, y) {
        _requireArguments(arguments.length, 6, 'bezierCurveTo', 'Path2D');
        cp1x = +cp1x; cp1y = +cp1y; cp2x = +cp2x; cp2y = +cp2y; x = +x; y = +y;
        if (!_fin6(cp1x, cp1y, cp2x, cp2y, x, y)) return;
        this.#segment6(SEG_BEZIER_CURVE_TO, cp1x, cp1y, cp2x, cp2y, x, y);
    }

    arcTo(x1, y1, x2, y2, radius) {
        _requireArguments(arguments.length, 5, 'arcTo', 'Path2D');
        x1 = +x1; y1 = +y1; x2 = +x2; y2 = +y2; radius = +radius;
        if (!_fin6(x1, y1, x2, y2, radius, 0)) return;
        if (radius < 0) throw _negativeRadius(radius);
        const at = this.#reserve(6);
        this.#words[at] = SEG_ARC_TO;
        const f = this.#floats;
        f[at + 1] = _f32Clamp(x1);
        f[at + 2] = _f32Clamp(y1);
        f[at + 3] = _f32Clamp(x2);
        f[at + 4] = _f32Clamp(y2);
        f[at + 5] = _f32Clamp(radius);
    }

    rect(x, y, w, h) {
        _requireArguments(arguments.length, 4, 'rect', 'Path2D');
        x = +x; y = +y; w = +w; h = +h;
        if (!_fin4(x, y, w, h)) return;
        this.#segment4(SEG_RECT, x, y, w, h);
    }

    roundRect(x, y, w, h, radii = 0) {
        _requireArguments(arguments.length, 4, 'roundRect', 'Path2D');
        x = +x; y = +y; w = +w; h = +h;
        const converted = typeof radii === 'number' ? radii : _convertRadii(radii);
        if (!_fin4(x, y, w, h)) return;
        if (!_cornerRadii(converted, _roundRectRadii, 'roundRect')) return;
        const at = this.#reserve(13);
        this.#words[at] = SEG_ROUND_RECT;
        const f = this.#floats;
        f[at + 1] = _f32Clamp(x);
        f[at + 2] = _f32Clamp(y);
        f[at + 3] = _f32Clamp(w);
        f[at + 4] = _f32Clamp(h);
        for (let i = 0; i < 8; i++) f[at + 5 + i] = _f32Clamp(_roundRectRadii[i]);
    }

    arc(x, y, radius, startAngle, endAngle, counterclockwise = false) {
        _requireArguments(arguments.length, 5, 'arc', 'Path2D');
        x = +x; y = +y; radius = +radius; startAngle = +startAngle; endAngle = +endAngle;
        const ccw = !!counterclockwise;
        if (!_fin6(x, y, radius, startAngle, endAngle, 0)) return;
        if (radius < 0) throw _negativeRadius(radius);
        const at = this.#reserve(7);
        this.#words[at] = SEG_ARC;
        const f = this.#floats;
        f[at + 1] = _f32Clamp(x);
        f[at + 2] = _f32Clamp(y);
        f[at + 3] = _f32Clamp(radius);
        f[at + 4] = _f32Clamp(startAngle);
        f[at + 5] = _f32Clamp(endAngle);
        this.#words[at + 6] = ccw ? 1 : 0;
    }

    ellipse(x, y, radiusX, radiusY, rotation, startAngle, endAngle, counterclockwise = false) {
        _requireArguments(arguments.length, 7, 'ellipse', 'Path2D');
        x = +x; y = +y; radiusX = +radiusX; radiusY = +radiusY;
        rotation = +rotation; startAngle = +startAngle; endAngle = +endAngle;
        const ccw = !!counterclockwise;
        if (!_fin6(x, y, radiusX, radiusY, rotation, startAngle) || !_fin1(endAngle)) return;
        if (radiusX < 0 || radiusY < 0) throw _negativeRadius(radiusX < 0 ? radiusX : radiusY);
        const at = this.#reserve(9);
        this.#words[at] = SEG_ELLIPSE;
        const f = this.#floats;
        f[at + 1] = _f32Clamp(x);
        f[at + 2] = _f32Clamp(y);
        f[at + 3] = _f32Clamp(radiusX);
        f[at + 4] = _f32Clamp(radiusY);
        f[at + 5] = _f32Clamp(rotation);
        f[at + 6] = _f32Clamp(startAngle);
        f[at + 7] = _f32Clamp(endAngle);
        this.#words[at + 8] = ccw ? 1 : 0;
    }

    // `addPath(path, transform)`: a copy of `path`'s segments, taken now, under the transform. A transform with a
    // member that is not finite adds nothing; an empty path adds nothing.
    addPath(path, transform = undefined) {
        _requireArguments(arguments.length, 1, 'addPath', 'Path2D');
        if (!_isPath2D(path)) {
            throw new TypeError("Failed to execute 'addPath' on 'Path2D': parameter 1 is not of type 'Path2D'.");
        }
        const m = _addPathMatrix;
        _matrix2DInit(transform, m, 'addPath', 'Path2D');
        if (!_fin6(m[0], m[1], m[2], m[3], m[4], m[5])) return;
        const count = path.#length;
        if (count === 0) return;
        const depth = path.#depth + 1;
        if (depth > MAX_PATH_NESTING) {
            throw new RangeError("The Path2D would nest paths deeper than the implementation limit of " +
                MAX_PATH_NESTING + ".");
        }
        const at = this.#reserve(8 + count);
        this.#words[at] = SEG_ADD_PATH;
        const f = this.#floats;
        for (let i = 0; i < 6; i++) f[at + 1 + i] = _f32Clamp(m[i]);
        this.#words[at + 7] = count;
        // After the reserve: a path added to itself has its words in the buffer the reserve may have replaced.
        TypedArrayPrototypeSet(this.#words, TypedArrayPrototypeSubarray(path.#words, 0, count), at + 8);
        if (depth > this.#depth) this.#depth = depth;
    }
}

// Full CSS named color table, synced with Rust NAMED_COLORS. Values are [r,g,b,a].
const _NAMED_COLORS = {
    'transparent': [0,0,0,0],
    'aliceblue': [240,248,255,255], 'antiquewhite': [250,235,215,255],
    'aqua': [0,255,255,255], 'aquamarine': [127,255,212,255],
    'azure': [240,255,255,255], 'beige': [245,245,220,255],
    'bisque': [255,228,196,255], 'black': [0,0,0,255],
    'blanchedalmond': [255,235,205,255], 'blue': [0,0,255,255],
    'blueviolet': [138,43,226,255], 'brown': [165,42,42,255],
    'burlywood': [222,184,135,255], 'cadetblue': [95,158,160,255],
    'chartreuse': [127,255,0,255], 'chocolate': [210,105,30,255],
    'coral': [255,127,80,255], 'cornflowerblue': [100,149,237,255],
    'cornsilk': [255,248,220,255], 'crimson': [220,20,60,255],
    'cyan': [0,255,255,255], 'darkblue': [0,0,139,255],
    'darkcyan': [0,139,139,255], 'darkgoldenrod': [184,134,11,255],
    'darkgray': [169,169,169,255], 'darkgreen': [0,100,0,255],
    'darkgrey': [169,169,169,255], 'darkkhaki': [189,183,107,255],
    'darkmagenta': [139,0,139,255], 'darkolivegreen': [85,107,47,255],
    'darkorange': [255,140,0,255], 'darkorchid': [153,50,204,255],
    'darkred': [139,0,0,255], 'darksalmon': [233,150,122,255],
    'darkseagreen': [143,188,143,255], 'darkslateblue': [72,61,139,255],
    'darkslategray': [47,79,79,255], 'darkslategrey': [47,79,79,255],
    'darkturquoise': [0,206,209,255], 'darkviolet': [148,0,211,255],
    'deeppink': [255,20,147,255], 'deepskyblue': [0,191,255,255],
    'dimgray': [105,105,105,255], 'dimgrey': [105,105,105,255],
    'dodgerblue': [30,144,255,255], 'firebrick': [178,34,34,255],
    'floralwhite': [255,250,240,255], 'forestgreen': [34,139,34,255],
    'fuchsia': [255,0,255,255], 'gainsboro': [220,220,220,255],
    'ghostwhite': [248,248,255,255], 'gold': [255,215,0,255],
    'goldenrod': [218,165,32,255], 'gray': [128,128,128,255],
    'green': [0,128,0,255], 'greenyellow': [173,255,47,255],
    'grey': [128,128,128,255], 'honeydew': [240,255,240,255],
    'hotpink': [255,105,180,255], 'indianred': [205,92,92,255],
    'indigo': [75,0,130,255], 'ivory': [255,255,240,255],
    'khaki': [240,230,140,255], 'lavender': [230,230,250,255],
    'lavenderblush': [255,240,245,255], 'lawngreen': [124,252,0,255],
    'lemonchiffon': [255,250,205,255], 'lightblue': [173,216,230,255],
    'lightcoral': [240,128,128,255], 'lightcyan': [224,255,255,255],
    'lightgoldenrodyellow': [250,250,210,255], 'lightgray': [211,211,211,255],
    'lightgreen': [144,238,144,255], 'lightgrey': [211,211,211,255],
    'lightpink': [255,182,193,255], 'lightsalmon': [255,160,122,255],
    'lightseagreen': [32,178,170,255], 'lightskyblue': [135,206,250,255],
    'lightslategray': [119,136,153,255], 'lightslategrey': [119,136,153,255],
    'lightsteelblue': [176,196,222,255], 'lightyellow': [255,255,224,255],
    'lime': [0,255,0,255], 'limegreen': [50,205,50,255],
    'linen': [250,240,230,255], 'magenta': [255,0,255,255],
    'maroon': [128,0,0,255], 'mediumaquamarine': [102,205,170,255],
    'mediumblue': [0,0,205,255], 'mediumorchid': [186,85,211,255],
    'mediumpurple': [147,112,219,255], 'mediumseagreen': [60,179,113,255],
    'mediumslateblue': [123,104,238,255], 'mediumspringgreen': [0,250,154,255],
    'mediumturquoise': [72,209,204,255], 'mediumvioletred': [199,21,133,255],
    'midnightblue': [25,25,112,255], 'mintcream': [245,255,250,255],
    'mistyrose': [255,228,225,255], 'moccasin': [255,228,181,255],
    'navajowhite': [255,222,173,255], 'navy': [0,0,128,255],
    'oldlace': [253,245,230,255], 'olive': [128,128,0,255],
    'olivedrab': [107,142,35,255], 'orange': [255,165,0,255],
    'orangered': [255,69,0,255], 'orchid': [218,112,214,255],
    'palegoldenrod': [238,232,170,255], 'palegreen': [152,251,152,255],
    'paleturquoise': [175,238,238,255], 'palevioletred': [219,112,147,255],
    'papayawhip': [255,239,213,255], 'peachpuff': [255,218,185,255],
    'peru': [205,133,63,255], 'pink': [255,192,203,255],
    'plum': [221,160,221,255], 'powderblue': [176,224,230,255],
    'purple': [128,0,128,255], 'rebeccapurple': [102,51,153,255],
    'red': [255,0,0,255], 'rosybrown': [188,143,143,255],
    'royalblue': [65,105,225,255], 'saddlebrown': [139,69,19,255],
    'salmon': [250,128,114,255], 'sandybrown': [244,164,96,255],
    'seagreen': [46,139,87,255], 'seashell': [255,245,238,255],
    'sienna': [160,82,45,255], 'silver': [192,192,192,255],
    'skyblue': [135,206,235,255], 'slateblue': [106,90,205,255],
    'slategray': [112,128,144,255], 'slategrey': [112,128,144,255],
    'snow': [255,250,250,255], 'springgreen': [0,255,127,255],
    'steelblue': [70,130,180,255], 'tan': [210,180,140,255],
    'teal': [0,128,128,255], 'thistle': [216,191,216,255],
    'tomato': [255,99,71,255], 'turquoise': [64,224,208,255],
    'violet': [238,130,238,255], 'wheat': [245,222,179,255],
    'white': [255,255,255,255], 'whitesmoke': [245,245,245,255],
    'yellow': [255,255,0,255], 'yellowgreen': [154,205,50,255],
};

// ---- CSS colour strings ----
//
// The one parser. A colour string is read here, in the facade, and the renderer is only ever sent the colour: the
// stream records carry r, g, b, a, never text. It has to be here because the answer is needed synchronously -- an
// invalid string is ignored and leaves the previous colour (which the renderer would have to be asked about), and
// `fillStyle` reads back the serialised colour, not the string that was assigned.
//
// The syntax is CSS Color 4 as a canvas takes it: `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa`, every named colour,
// `transparent`, `currentcolor` (the canvas's own colour: black), `rgb()`/`rgba()` and `hsl()`/`hsla()` in the legacy
// comma form and the modern space form (`rgb(10 20 30 / 50%)`, numbers or percentages, `none`), and `hwb()`. `lab()`,
// `lch()`, `oklab()`, `oklch()`, `color()` and `color-mix()` are not read: they are invalid here, so ignored.
//
// There used to be a second reader in Rust (`parse_color_string`, reached through `op_set_fill_style`) and a third in the
// producer, kept in step by a corpus test. They read a narrower language (no hsl, no percentages, no decimals, an invalid
// string was black rather than ignored), so the strings the specification tests assign were handled one way here and
// another there. Now there is one.

// Named colours that are not a plain table lookup: `currentcolor` is the canvas's text colour, black until a page says
// otherwise.
const _CSS_NUMBER = /^[+-]?(?:\d+(?:\.\d+)?|\.\d+)(?:[eE][+-]?\d+)?$/;

// A component token: a number, a percentage, an angle, or `none`. `null` for anything else.
function _cssToken(text) {
    if (text === 'none') return { kind: 'none', v: 0 };
    if (text.charCodeAt(text.length - 1) === 37 /* % */) {
        const body = text.slice(0, -1);
        return _CSS_NUMBER.test(body) ? { kind: 'pct', v: parseFloat(body) } : null;
    }
    if (_CSS_NUMBER.test(text)) return { kind: 'num', v: parseFloat(text) };
    const unit = /(deg|grad|rad|turn)$/.exec(text);
    if (unit !== null) {
        const body = text.slice(0, text.length - unit[1].length);
        if (!_CSS_NUMBER.test(body)) return null;
        const n = parseFloat(body);
        const degrees = unit[1] === 'deg' ? n : unit[1] === 'grad' ? n * 0.9 : unit[1] === 'rad' ? n * 180 / Math.PI : n * 360;
        return { kind: 'angle', v: degrees };
    }
    return null;
}

const _clamp = (v, lo, hi) => (v < lo ? lo : v > hi ? hi : v);

// The alpha byte of an alpha token: a number 0..1 or a percentage; `none` is 0.
function _cssAlphaByte(token) {
    if (token === null) return -1;
    if (token.kind === 'num') return Math.round(_clamp(token.v, 0, 1) * 255);
    if (token.kind === 'pct') return Math.round(_clamp(token.v / 100, 0, 1) * 255);
    if (token.kind === 'none') return 0;
    return -1;
}

// The three components and the optional alpha of a function's arguments, in either syntax. Legacy is comma-separated
// and takes no `none` and no `/`; modern is space-separated with an optional `/ alpha`. `{ parts, alpha }` or `null`.
function _cssArguments(inner) {
    const body = inner.trim();
    if (body.indexOf(',') !== -1) {
        if (body.indexOf('/') !== -1) return null;
        const pieces = body.split(',').map((t) => t.trim().toLowerCase());
        if (pieces.length !== 3 && pieces.length !== 4) return null;
        const tokens = pieces.map(_cssToken);
        if (tokens.some((t) => t === null || t.kind === 'none')) return null;
        return { parts: tokens.slice(0, 3), alpha: pieces.length === 4 ? tokens[3] : undefined, legacy: true };
    }
    let colour = body, alphaText = null;
    const slash = body.indexOf('/');
    if (slash !== -1) {
        colour = body.slice(0, slash);
        alphaText = body.slice(slash + 1).trim().toLowerCase();
        if (alphaText.length === 0 || /\s/.test(alphaText)) return null;
    }
    const pieces = colour.trim().toLowerCase().split(/\s+/);
    if (pieces.length !== 3) return null;
    const tokens = pieces.map(_cssToken);
    if (tokens.some((t) => t === null)) return null;
    const alpha = alphaText === null ? undefined : _cssToken(alphaText);
    if (alphaText !== null && alpha === null) return null;
    return { parts: tokens, alpha, legacy: false };
}

const _channelByte = (token, scale) => Math.round(_clamp(token.kind === 'pct' ? token.v * scale / 100 : token.v, 0, scale));

// `rgb()` / `rgba()`.
function _cssRgb(inner) {
    const args = _cssArguments(inner);
    if (args === null) return null;
    const [r, g, b] = args.parts;
    for (const t of args.parts) if (t.kind === 'angle') return null;
    // The legacy form takes numbers or percentages throughout, not a mix.
    if (args.legacy && !(r.kind === g.kind && g.kind === b.kind)) return null;
    let a = 255;
    if (args.alpha !== undefined) {
        a = _cssAlphaByte(args.alpha);
        if (a < 0) return null;
    }
    return [_channelByte(r, 255), _channelByte(g, 255), _channelByte(b, 255), a];
}

// A hue in degrees from a token: a number is degrees, and `none` is 0.
function _cssHue(token) {
    if (token.kind === 'num' || token.kind === 'angle') return ((token.v % 360) + 360) % 360;
    if (token.kind === 'none') return 0;
    return null;
}

// The RGB, each 0..1, of a hue at full saturation and half lightness.
function _hueToRgb(h) {
    const f = (n) => {
        const k = (n + h / 30) % 12;
        return 0.5 - 0.5 * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    };
    return [f(0), f(8), f(4)];
}

// `hsl()` / `hsla()`.
function _cssHsl(inner) {
    const args = _cssArguments(inner);
    if (args === null) return null;
    const [ht, st, lt] = args.parts;
    const h = _cssHue(ht);
    if (h === null) return null;
    // Saturation and lightness are percentages; the modern form also takes a bare number as one.
    for (const t of [st, lt]) {
        if (t.kind === 'angle') return null;
        if (args.legacy && t.kind !== 'pct') return null;
    }
    if (args.legacy && ht.kind === 'pct') return null;
    const s = _clamp(st.v, 0, 100) / 100;
    const l = _clamp(lt.v, 0, 100) / 100;
    let a = 255;
    if (args.alpha !== undefined) {
        a = _cssAlphaByte(args.alpha);
        if (a < 0) return null;
    }
    const m = s * Math.min(l, 1 - l);
    const channel = (n) => {
        const k = (n + h / 30) % 12;
        return l - m * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    };
    return [Math.round(channel(0) * 255), Math.round(channel(8) * 255), Math.round(channel(4) * 255), a];
}

// `hwb()`.
function _cssHwb(inner) {
    const args = _cssArguments(inner);
    if (args === null || args.legacy) return null;
    const [ht, wt, bt] = args.parts;
    const h = _cssHue(ht);
    if (h === null || wt.kind === 'angle' || bt.kind === 'angle') return null;
    let w = _clamp(wt.v, 0, 100) / 100;
    let k = _clamp(bt.v, 0, 100) / 100;
    let a = 255;
    if (args.alpha !== undefined) {
        a = _cssAlphaByte(args.alpha);
        if (a < 0) return null;
    }
    if (w + k >= 1) {
        const grey = Math.round(w / (w + k) * 255);
        return [grey, grey, grey, a];
    }
    const base = _hueToRgb(h);
    return [
        Math.round((base[0] * (1 - w - k) + w) * 255),
        Math.round((base[1] * (1 - w - k) + w) * 255),
        Math.round((base[2] * (1 - w - k) + w) * 255),
        a,
    ];
}

// A colour string as [r, g, b, a] bytes, or `null` when it is not a colour this canvas reads.
function _parseCssColor(input) {
    const s = input.trim();
    if (s.length === 0) return null;

    if (s.charCodeAt(0) === 35 /* # */) {
        const hex = s.slice(1);
        for (let i = 0; i < hex.length; i++) {
            const c = hex.charCodeAt(i);
            if (!((c >= 48 && c <= 57) || (c >= 65 && c <= 70) || (c >= 97 && c <= 102))) return null;
        }
        if (hex.length === 3 || hex.length === 4) {
            const r = parseInt(hex[0], 16), g = parseInt(hex[1], 16), b = parseInt(hex[2], 16);
            const a = hex.length === 4 ? parseInt(hex[3], 16) : 15;
            return [(r << 4) | r, (g << 4) | g, (b << 4) | b, (a << 4) | a];
        }
        if (hex.length === 6 || hex.length === 8) {
            return [
                parseInt(hex.substring(0, 2), 16), parseInt(hex.substring(2, 4), 16), parseInt(hex.substring(4, 6), 16),
                hex.length === 8 ? parseInt(hex.substring(6, 8), 16) : 255,
            ];
        }
        return null;
    }

    const open = s.indexOf('(');
    if (open === -1) {
        const lower = s.toLowerCase();
        if (lower === 'currentcolor') return [0, 0, 0, 255];
        const named = _NAMED_COLORS[lower];
        return named === undefined ? null : named.slice();
    }
    if (s.charCodeAt(s.length - 1) !== 41 /* ) */) return null;
    const name = s.slice(0, open).trim().toLowerCase();
    const inner = s.slice(open + 1, s.length - 1);
    if (name === 'rgb' || name === 'rgba') return _cssRgb(inner);
    if (name === 'hsl' || name === 'hsla') return _cssHsl(inner);
    if (name === 'hwb') return _cssHwb(inner);
    return null;
}

const _hex2 = (n) => (n < 16 ? '0' : '') + n.toString(16);

// "Serialization of a color": `#rrggbb` when opaque, otherwise `rgba(r, g, b, a)` with the alpha as the shortest
// decimal of two places that is the same byte, three when two cannot say it.
function _serializeCssColor(rgba) {
    const a = rgba[3];
    if (a === 255) return '#' + _hex2(rgba[0]) + _hex2(rgba[1]) + _hex2(rgba[2]);
    let alpha = Math.round(a / 255 * 100) / 100;
    if (Math.round(alpha * 255) !== a) alpha = Math.round(a / 255 * 1000) / 1000;
    return 'rgba(' + rgba[0] + ', ' + rgba[1] + ', ' + rgba[2] + ', ' + alpha + ')';
}

// Assignments repeat the same few strings (a scene sets a colour per shape), so each string is read once.
// `{ text, rgba }`, or `null` for a string that is not a colour.
const _colourCache = new Map();
const _COLOUR_CACHE_LIMIT = 512;
function _cssColour(raw) {
    let entry = _colourCache.get(raw);
    if (entry !== undefined) return entry;
    const rgba = _parseCssColor(raw);
    entry = rgba === null ? null : { text: _serializeCssColor(rgba), rgba };
    if (_colourCache.size >= _COLOUR_CACHE_LIMIT) _colourCache.clear();
    _colourCache.set(raw, entry);
    return entry;
}

// The colour of a style as a text-cache key wants it: bytes, black for a gradient, a pattern or anything unread.
function _styleRgba(style) {
    const entry = typeof style === 'string' ? _cssColour(style) : null;
    return entry === null ? [0, 0, 0, 255] : entry.rgba;
}

// ---- CSS font shorthand ----
//
// The one parser, for the reason colours have one: `ctx.font = s` has to answer at once whether `s` is a font -- an
// invalid shorthand is ignored and keeps the previous font -- and `font` reads back the serialised font, not the string
// that was assigned. The renderer and the measurer are only ever sent the font this reads, never text: a size in CSS
// pixels, a weight, whether it slants, and the family list.
//
// There used to be three: `shared::css_font_shorthand`, which the op and the renderer ran; a lenient `shared::css_font`
// the measurer fell back to; and a port of the first in the Performance+ producer, held to it by a corpus. All three
// read a language of their own -- `20px` with no family was a font, `0px` was not, `em` and `%` were 16px -- and the
// getter returned the string as assigned.
//
// The grammar is CSS Fonts 4's `font` as a canvas takes it: up to four of a font-style (`italic`, or `oblique` and an
// optional angle), `small-caps`, a font-weight (`bold`, `bolder`, `lighter`, or 1 to 1000) and a font-stretch keyword,
// each at most once and `normal` standing for any of them; a size; an optional `/` line-height, read and dropped; and a
// list of families, each a string, a sequence of identifiers or a generic keyword. A system font keyword alone is the
// platform's UI face at the default 16px. Tokens are CSS Syntax 3's, escapes, comments and an unterminated final
// string included.
//
// A canvas here has no element and no style, so relative sizes resolve against the canvas default font, 10px, as a
// browser resolves them for a canvas it is not rendering: `em`, `rem` and `%` against 10px, `larger` and `smaller` a
// factor of 1.2 from it. `ex`, `ch` and `ic` take CSS Values' values for a font whose metrics cannot be read, which is
// the case here: 0.5em, 0.5em and 1em. Math functions, `var()`, and viewport, container, line-height and `cap` units
// are not read: a shorthand with one is invalid here, so ignored. A size past 10000px is 10000px, as in Chrome.
//
// The serialisation is Chrome's: style, weight, `small-caps`, size, families. `italic` for a style that slants
// (`oblique` with no angle or a positive one); a weight of 400 is left out, 700 is `bold`, any other the integer; the
// size in up to six significant digits; a family as an identifier when it is one and no keyword, else as a string. A
// stretch keyword is read and, as in Chrome, neither serialised nor drawn; `small-caps` is serialised and not drawn,
// for the renderer has no small-capitals synthesis. A font's fields are those of its serialisation -- the size is
// rounded to what the text says -- so two assignments that read back alike draw and measure alike.

// The canvas default font's size, which relative sizes resolve against.
const _FONT_BASE_PX = 10;
const _FONT_SIZE_LIMIT = 10000;
const _FONT_GENERIC = new Set([
    'serif', 'sans-serif', 'cursive', 'fantasy', 'monospace', 'system-ui', 'emoji', 'math', 'fangsong',
    'ui-serif', 'ui-sans-serif', 'ui-monospace', 'ui-rounded',
]);
// The CSS-wide keywords and `default`: never a family name unless quoted.
const _FONT_RESERVED = new Set(['inherit', 'initial', 'unset', 'revert', 'revert-layer', 'default']);
const _FONT_SYSTEM = new Set(['caption', 'icon', 'menu', 'message-box', 'small-caption', 'status-bar']);
const _FONT_STRETCH = new Set([
    'ultra-condensed', 'extra-condensed', 'condensed', 'semi-condensed',
    'semi-expanded', 'expanded', 'extra-expanded', 'ultra-expanded',
]);
const _FONT_ABSOLUTE_SIZE = new Map([
    ['xx-small', 9], ['x-small', 10], ['small', 13], ['medium', 16],
    ['large', 18], ['x-large', 24], ['xx-large', 32], ['xxx-large', 48],
]);
// CSS pixels per unit.
const _FONT_LENGTH = new Map([
    ['px', 1], ['pt', 4 / 3], ['pc', 16], ['in', 96], ['cm', 96 / 2.54], ['mm', 96 / 25.4], ['q', 96 / 101.6],
    ['em', _FONT_BASE_PX], ['rem', _FONT_BASE_PX], ['ic', _FONT_BASE_PX], ['ric', _FONT_BASE_PX],
    ['ex', _FONT_BASE_PX / 2], ['rex', _FONT_BASE_PX / 2], ['ch', _FONT_BASE_PX / 2], ['rch', _FONT_BASE_PX / 2],
]);
// Degrees per unit.
const _FONT_ANGLE = new Map([['deg', 1], ['grad', 0.9], ['rad', 180 / Math.PI], ['turn', 360]]);
const _FONT_COMMA = { type: ',' };
const _FONT_SLASH = { type: '/' };

function _asciiLower(text) {
    return text.replace(/[A-Z]+/g, (upper) => upper.toLowerCase());
}

function _isCssDigit(c) { return c >= 0x30 && c <= 0x39; }
function _isCssHex(c) { return _isCssDigit(c) || (c >= 0x41 && c <= 0x46) || (c >= 0x61 && c <= 0x66); }
function _isCssIdentStart(c) { return (c >= 0x41 && c <= 0x5a) || (c >= 0x61 && c <= 0x7a) || c === 0x5f || c >= 0x80; }
function _isCssIdentChar(c) { return _isCssIdentStart(c) || _isCssDigit(c) || c === 0x2d; }
function _isCssSpace(c) { return c === 0x20 || c === 0x09 || c === 0x0a; }

// The shorthand's tokens, whitespace and comments dropped (nothing in the grammar depends on them once the tokens are
// cut), or `null` for input with a token no font has: a function, a bad string, `!`, `;`, a bracket.
function _fontTokens(input) {
    // CSS Syntax's preprocessing: one newline, and U+FFFD for NUL and for a lone surrogate.
    const s = input.replace(/\r\n?|\f/g, '\n')
        .replace(/\u0000|[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, '\uFFFD');
    const n = s.length;
    const at = (k) => s.charCodeAt(k); // NaN past the end, which no class test accepts
    const validEscape = (k) => at(k) === 0x5c && at(k + 1) !== 0x0a;
    const startsIdent = (k) => at(k) === 0x2d
        ? _isCssIdentStart(at(k + 1)) || at(k + 1) === 0x2d || validEscape(k + 1)
        : _isCssIdentStart(at(k)) || validEscape(k);
    const startsNumber = (k) => {
        const c = at(k) === 0x2b || at(k) === 0x2d ? at(++k) : at(k);
        return _isCssDigit(c) || (c === 0x2e && _isCssDigit(at(k + 1)));
    };
    let i = 0;
    // An escape, from the code point after its backslash.
    const escape = () => {
        if (i >= n) return '\uFFFD';
        if (_isCssHex(at(i))) {
            const begin = i;
            while (i - begin < 6 && _isCssHex(at(i))) i++;
            const cp = parseInt(s.slice(begin, i), 16);
            if (_isCssSpace(at(i))) i++;
            return cp === 0 || (cp >= 0xd800 && cp <= 0xdfff) || cp > 0x10ffff ? '\uFFFD' : String.fromCodePoint(cp);
        }
        const cp = s.codePointAt(i);
        i += cp > 0xffff ? 2 : 1;
        return String.fromCodePoint(cp);
    };
    const name = () => {
        let out = '';
        for (;;) {
            if (_isCssIdentChar(at(i))) out += s[i++];
            else if (validEscape(i)) { i++; out += escape(); }
            else return out;
        }
    };
    const tokens = [];
    while (i < n) {
        const c = at(i);
        if (_isCssSpace(c)) { i++; continue; }
        if (c === 0x2f && at(i + 1) === 0x2a) {
            const end = s.indexOf('*/', i + 2);
            i = end < 0 ? n : end + 2;
            continue;
        }
        if (c === 0x22 || c === 0x27) {
            i++;
            let out = '';
            // The end of the input ends a string too: a parse error, and still a string.
            while (i < n) {
                const d = at(i);
                if (d === c) { i++; break; }
                if (d === 0x0a) return null;
                if (d !== 0x5c) { out += s[i++]; continue; }
                i++;
                if (i >= n) continue;
                if (at(i) === 0x0a) { i++; continue; }
                out += escape();
            }
            tokens.push({ type: 'string', value: out });
            continue;
        }
        if (startsNumber(i)) {
            const begin = i;
            if (at(i) === 0x2b || at(i) === 0x2d) i++;
            while (_isCssDigit(at(i))) i++;
            if (at(i) === 0x2e && _isCssDigit(at(i + 1))) {
                i += 2;
                while (_isCssDigit(at(i))) i++;
            }
            if ((at(i) === 0x45 || at(i) === 0x65) && (_isCssDigit(at(i + 1))
                    || ((at(i + 1) === 0x2b || at(i + 1) === 0x2d) && _isCssDigit(at(i + 2))))) {
                i += _isCssDigit(at(i + 1)) ? 1 : 2;
                while (_isCssDigit(at(i))) i++;
            }
            const value = +s.slice(begin, i);
            if (startsIdent(i)) tokens.push({ type: 'dimension', value, unit: _asciiLower(name()) });
            else if (at(i) === 0x25) { i++; tokens.push({ type: 'percentage', value }); }
            else tokens.push({ type: 'number', value });
            continue;
        }
        if (startsIdent(i)) {
            const value = name();
            if (at(i) === 0x28) return null;
            tokens.push({ type: 'ident', value });
            continue;
        }
        if (c === 0x2c) { tokens.push(_FONT_COMMA); i++; continue; }
        if (c === 0x2f) { tokens.push(_FONT_SLASH); i++; continue; }
        return null;
    }
    return tokens;
}

// `{ size, weight, italic, smallCaps, families: [{ generic, name }] }`, or `null` for a string that is not a font.
function _parseCssFont(input) {
    const tokens = _fontTokens(input);
    if (tokens === null || tokens.length === 0) return null;
    if (tokens.length === 1 && tokens[0].type === 'ident' && _FONT_SYSTEM.has(_asciiLower(tokens[0].value))) {
        return { size: 16, weight: 400, italic: false, smallCaps: false, families: [{ generic: true, name: 'system-ui' }] };
    }
    let i = 0;
    // `null` until given; `normal` gives none of them and still counts as one of the four.
    let slant = null, smallCaps = null, weight = null, stretch = null;
    for (let given = 0; given < 4 && i < tokens.length; given++) {
        const t = tokens[i];
        if (t.type === 'number') {
            if (weight !== null || !(t.value >= 1 && t.value <= 1000)) break;
            weight = t.value;
            i++;
            continue;
        }
        if (t.type !== 'ident') break;
        const k = _asciiLower(t.value);
        if (k === 'normal') {
            i++;
        } else if (k === 'italic' || k === 'oblique') {
            if (slant !== null) break;
            slant = true;
            i++;
            const angle = k === 'oblique' ? tokens[i] : undefined;
            if (angle !== undefined && angle.type === 'dimension' && _FONT_ANGLE.has(angle.unit)) {
                const degrees = angle.value * _FONT_ANGLE.get(angle.unit);
                if (!(degrees >= -90 && degrees <= 90)) return null;
                slant = degrees > 0;
                i++;
            }
        } else if (k === 'small-caps') {
            if (smallCaps !== null) break;
            smallCaps = true;
            i++;
        } else if (k === 'bold' || k === 'bolder' || k === 'lighter') {
            // `bolder` and `lighter` are relative to the inherited weight, the default font's 400.
            if (weight !== null) break;
            weight = k === 'lighter' ? 100 : 700;
            i++;
        } else if (_FONT_STRETCH.has(k)) {
            if (stretch !== null) break;
            stretch = k;
            i++;
        } else {
            break;
        }
    }

    const t = tokens[i++];
    let size;
    if (t === undefined) {
        return null;
    } else if (t.type === 'dimension') {
        const pxPerUnit = _FONT_LENGTH.get(t.unit);
        if (pxPerUnit === undefined || !(t.value >= 0)) return null;
        size = t.value * pxPerUnit;
    } else if (t.type === 'percentage') {
        if (!(t.value >= 0)) return null;
        size = t.value / 100 * _FONT_BASE_PX;
    } else if (t.type === 'number') {
        if (t.value !== 0) return null;
        size = 0;
    } else if (t.type === 'ident') {
        const k = _asciiLower(t.value);
        size = k === 'larger' ? _FONT_BASE_PX * 1.2 : k === 'smaller' ? _FONT_BASE_PX / 1.2 : _FONT_ABSOLUTE_SIZE.get(k);
        if (size === undefined) return null;
    } else {
        return null;
    }

    if (tokens[i] === _FONT_SLASH) {
        const h = tokens[i + 1];
        if (h === undefined) return null;
        const lineHeight = (h.type === 'ident' && _asciiLower(h.value) === 'normal')
            || ((h.type === 'number' || h.type === 'percentage') && h.value >= 0)
            || (h.type === 'dimension' && _FONT_LENGTH.has(h.unit) && h.value >= 0);
        if (!lineHeight) return null;
        i += 2;
    }

    const families = [];
    for (;;) {
        const f = tokens[i];
        if (f === undefined) return null;
        if (f.type === 'string') {
            families.push({ generic: false, name: f.value });
            i++;
        } else if (f.type === 'ident') {
            let end = i + 1;
            while (end < tokens.length && tokens[end].type === 'ident') end++;
            const head = _asciiLower(f.value);
            // A CSS-wide keyword alone is that keyword, never a name; a generic keyword is a family of its own,
            // never the start of one.
            if (end - i === 1 && _FONT_RESERVED.has(head)) return null;
            if (_FONT_GENERIC.has(head)) {
                if (end - i > 1) return null;
                families.push({ generic: true, name: head });
            } else {
                let familyName = f.value;
                for (let k = i + 1; k < end; k++) familyName += ' ' + tokens[k].value;
                families.push({ generic: false, name: familyName });
            }
            i = end;
        } else {
            return null;
        }
        if (i === tokens.length) break;
        if (tokens[i] !== _FONT_COMMA) return null;
        i++;
    }

    return {
        // Rounded to the digits the serialisation keeps, so the font is a function of its text.
        size: +Math.min(size, _FONT_SIZE_LIMIT).toPrecision(6),
        weight: weight === null ? 400 : Math.trunc(weight),
        italic: slant === true,
        smallCaps: smallCaps === true,
        families,
    };
}

// A family name as CSS writes it: bare when it reads back as the same single identifier, else a string.
const _CSS_IDENT = /^-?[A-Za-z_\u0080-\uFFFF][\w\-\u0080-\uFFFF]*$/;
function _serializeFontFamily(family) {
    if (family.generic) return family.name;
    const lower = _asciiLower(family.name);
    if (_CSS_IDENT.test(family.name) && !_FONT_GENERIC.has(lower) && !_FONT_RESERVED.has(lower)) return family.name;
    return '"' + family.name.replace(/["\\]/g, '\\$&')
        .replace(/[\u0001-\u001f\u007f]/g, (c) => '\\' + c.charCodeAt(0).toString(16) + ' ') + '"';
}

function _serializeCssFont(font) {
    let text = font.italic ? 'italic ' : '';
    if (font.weight !== 400) text += (font.weight === 700 ? 'bold' : String(font.weight)) + ' ';
    if (font.smallCaps) text += 'small-caps ';
    text += font.size + 'px ';
    for (let i = 0; i < font.families.length; i++) {
        text += (i === 0 ? '' : ', ') + _serializeFontFamily(font.families[i]);
    }
    return text;
}

// Assignments repeat the same few strings (a label sets its font before every draw), so each string is read once.
// `{ text, size, weight, italic, families }`, `families` the names joined by NUL -- which no name holds, CSS having
// replaced it -- as the ops and the record take them; or `null` for a string that is not a font.
const _fontCache = new Map();
const _FONT_CACHE_LIMIT = 256;
function _cssFont(raw) {
    let entry = _fontCache.get(raw);
    if (entry !== undefined) return entry;
    const font = _parseCssFont(raw);
    entry = font === null ? null : Object.freeze({
        text: _serializeCssFont(font),
        size: font.size,
        weight: font.weight,
        italic: font.italic,
        families: font.families.map((family) => family.name).join('\u0000'),
    });
    if (_fontCache.size >= _FONT_CACHE_LIMIT) _fontCache.clear();
    _fontCache.set(raw, entry);
    return entry;
}

const _DEFAULT_FONT = _cssFont('10px sans-serif');

class CanvasRenderingContext2D {
    // `settings`: `getContext("2d", settings)`'s dictionary, read when the context is made.
    constructor(canvas, settings = undefined) {
        this._canvas = canvas;
        this._canvasId = canvas._rid;
        this._willReadFrequently = _contextSettings(settings).willReadFrequently;

        // Create native 2D context on the render thread
        flushRenderCommandStream();
        this._ctxId = op_create_context_2d(this._canvasId);
        if (this._ctxId < 0) { console.error("Failed to create 2d context"); }

        // Shadow state (for JS-side queries)
        this._fillStyle = '#000000';
        this._strokeStyle = '#000000';
        this._lineWidth = 1;
        this._lineCap = 'butt';
        this._lineJoin = 'miter';
        this._miterLimit = 10;
        this._globalAlpha = 1;
        this._font = _DEFAULT_FONT;
        this._textAlign = 'start';
        this._textBaseline = 'alphabetic';
        this._direction = 'inherit';
        this._imageSmoothing = true;
        this._imageSmoothingQuality = 'low';

        // Current transform matrix [a, b, c, d, e, f] for getTransform/transform
        this._tm = [1, 0, 0, 1, 0, 0];

        // State stack for save/restore
        this._stateStack = [];

        // Text texture cache state machine:
        //   0 = none, 1 = pending record (cache miss; op_fill_text was
        //       issued, the matching getImageData tags the snapshot),
        //   2 = pending hit (op_fill_text suppressed; the matching
        //       getImageData returns a cache-marked ImageData).
        // `_tcKey` holds the args tuple from `_buildTextCacheArgs`
        // plus `_x/_y/_mw` of the suppressed fillText for the
        // abandon/recover path.
        this._tcState = 0;
        this._tcKey = null;
    }

    get canvas() { return this._canvas; }

    // `reset()`: the context's default state again -- the bitmap transparent black, the state stack and the clips gone,
    // every attribute and the transform at its default, the current path empty. What assigning the canvas's size does,
    // without the surface being made again: a context reset every frame would otherwise reallocate it every frame.
    reset() {
        this._abandonPendingTextCache();
        encode2dReset(this._canvasId);
        this._resetShadowState();
    }

    // Whether the GPU the canvas draws with is lost and not yet restored. A 2D canvas shares it with every other context,
    // so it is lost when they are. Barriered: draws already encoded and still sitting in the stream buffer must reach
    // the collector before this host-local read, or this call would overtake them.
    isContextLost() {
        this._barrier();
        return op_gl_is_context_lost();
    }

    // What this context is: sRGB with alpha, 8 bits a channel, never desynchronized -- whatever `getContext` asked for,
    // which is why these are not echoed back -- and the `willReadFrequently` hint as it was given.
    getContextAttributes() {
        return {
            alpha: true,
            colorSpace: 'srgb',
            colorType: 'unorm8',
            desynchronized: false,
            willReadFrequently: this._willReadFrequently,
        };
    }

    // ==================== Text texture cache ====================

    // Build the cache-key argument tuple for the current 2D state, or
    // return null when the state is outside the cacheable whitelist
    // (anything that moves / recolours / blends the glyph run beyond
    // the keyed fields).  Font identity is carried entirely by the
    // serialised `font`, which is a function of the font drawn, so
    // size/weight/italic stay 0/false in the key.
    _buildTextCacheArgs(text) {
        const tm = this._tm;
        if (tm[0] !== 1 || tm[1] !== 0 || tm[2] !== 0
                || tm[3] !== 1 || tm[4] !== 0 || tm[5] !== 0) {
            return null;
        }
        if ((this._shadowBlur || 0) !== 0) return null;
        if ((this._shadowOffsetX || 0) !== 0) return null;
        if ((this._shadowOffsetY || 0) !== 0) return null;
        const ga = this._globalAlpha == null ? 1 : this._globalAlpha;
        if (ga !== 1) return null;
        const comp = this._compositeOp || 'source-over';
        if (comp !== 'source-over') return null;
        const cw = this._canvas.width | 0;
        const ch = this._canvas.height | 0;
        if (cw <= 0 || ch <= 0) return null;
        const rgba = _styleRgba(this._fillStyle);
        const color = (((rgba[0] & 255) << 24)
            | ((rgba[1] & 255) << 16)
            | ((rgba[2] & 255) << 8)
            | (rgba[3] & 255)) >>> 0;
        return {
            text: String(text),
            fontRequest: this._font.text,
            fontSize: 0,
            fontWeight: 0,
            italic: false,
            fillColor: color,
            textAlign: TEXT_ALIGN_MAP[this._textAlign] ?? 0,
            textBaseline: TEXT_BASELINE_MAP[this._textBaseline] ?? 3,
            canvasW: cw,
            canvasH: ch,
        };
    }

    // Drop any pending cache state.  For a suppressed hit (state 2)
    // this restores correctness: unpin the cache entry and actually
    // render the text we previously skipped, so the canvas is not
    // silently blank.  For a pending record (state 1) op_fill_text
    // was already issued; nothing to undo, just forget the key so the
    // next getImageData doesn't tag a snapshot that no longer matches
    // the canvas content.
    _abandonPendingTextCache() {
        if (this._tcState === 0) return;
        const k = this._tcKey;
        if (this._tcState === 2 && k) {
            this._barrier();
            op_text_cache_unpin(
                k.text, k.fontRequest, k.fontSize, k.fontWeight,
                k.italic, k.fillColor, k.textAlign, k.textBaseline,
                k.canvasW, k.canvasH,
            );
            op_fill_text(
                this._canvasId,
                k.text,
                k._x || 0,
                k._y || 0,
                k._mw == null ? Infinity : k._mw,
            );
        }
        this._tcState = 0;
        this._tcKey = null;
    }

    // Called by WebGL `texImage2D(target, level, internalformat, format, type, canvasElement)` -- the whole canvas,
    // nothing of it skipped -- when the source canvas's 2D context has pending text-cache state.  This is
    // cocos's actual upload path (NOT getImageData), so the
    // hit/record decision has to happen here.  Returns true when it
    // fully issued the upload (caller skips the normal direct path).
    //
    //   HIT  (suppressed fillText): copy the cached texture straight
    //         into the bound WebGL dest; the 2D canvas was never
    //         painted.  Render thread unpins.
    //   MISS (fillText was let through): snapshot the just-painted 2D
    //         canvas tagged for cache record, then upload that
    //         snapshot into the WebGL dest.  Frame-end drain transfers
    //         the snapshot texture into the cache so the next identical
    //         fillText hits.
    _consumeTextCacheForTexImage(glCanvasId, target, level, internalformat, format, type) {
        if (this._tcState === 0 || this._tcKey === null) return false;
        flushRenderCommandStream();
        const k = this._tcKey;
        const isHit = this._tcState === 2;
        this._tcState = 0;
        this._tcKey = null;
        if (isHit) {
            op_tex_image_2d_from_text_cache(
                glCanvasId, target, level, internalformat, format, type,
                k.text, k.fontRequest, k.fontSize, k.fontWeight,
                k.italic, k.fillColor, k.textAlign, k.textBaseline,
                k.canvasW, k.canvasH,
            );
            return true;
        }
        if (!_migoSnapshotBudgetAllows(k.canvasW, k.canvasH)) {
            // Snapshot budget exhausted this frame: can't record.
            // Fall back to the normal direct path (text already
            // painted, so the canvas is correct).
            return false;
        }
        const snapId = _migoNextSnapshotId();
        _migoSnapshotCharge(k.canvasW, k.canvasH);
        op_capture_canvas2d_snapshot_for_cache(
            this._canvasId, 0, 0, k.canvasW, k.canvasH, snapId,
            k.text, k.fontRequest, k.fontSize, k.fontWeight,
            k.italic, k.fillColor, k.textAlign, k.textBaseline,
            k.canvasW, k.canvasH,
        );
        uploadTexImageSource(
            glCanvasId, TEX_SOURCE_CALL_IMAGE_2D, target, level, internalformat, 0, 0, 0,
            k.canvasW, k.canvasH, 1, format, type, 0, TEX_SOURCE_SNAPSHOT, snapId, k.canvasW, k.canvasH,
            TEX_SOURCE_NO_PIXELS,
        );
        return true;
    }

    // ==================== Frame Lifecycle ====================

    // The ordering barrier, for the commands that still cross as ops.
    //
    // 2D and GL records share one buffer, so between two *encoded* commands
    // there is nothing to do -- the order in the buffer is the order. What still
    // needs saying is the boundary between an encoded command and one that goes
    // straight to the collector: text, gradients, patterns, dash arrays, images
    // and the synchronous reads. Those carry strings or variable-length arrays
    // and have no record shape, so they arrive by op, and an op that overtakes
    // the records encoded before it draws the frame out of order.
    //
    // Empty-buffer cost is one comparison. `every_op_in_the_2d_facade_is_ordered`
    // checks that every op call in this file has one of these in front of it,
    // because a barrier that has to be remembered is one that gets forgotten.
    _barrier() {
        flushRenderCommandStream();
    }

    // ==================== Path Methods ====================

    beginPath() {
        encode2dBeginPath(this._canvasId);
    }

    closePath() {
        encode2dClosePath(this._canvasId);
    }

    // The CanvasPath calls take their arguments as Path2D's do (WebIDL's `unrestricted double`, converted in order,
    // a Symbol or a BigInt a TypeError, a missing one a TypeError); a call with one that is not finite adds nothing.
    moveTo(x, y) {
        _requireArguments(arguments.length, 2, 'moveTo', 'CanvasRenderingContext2D');
        x = +x; y = +y;
        if (!_fin2(x, y)) return;
        encode2dMoveTo(this._canvasId, x, y);
    }

    lineTo(x, y) {
        _requireArguments(arguments.length, 2, 'lineTo', 'CanvasRenderingContext2D');
        x = +x; y = +y;
        if (!_fin2(x, y)) return;
        encode2dLineTo(this._canvasId, x, y);
    }

    quadraticCurveTo(cpx, cpy, x, y) {
        _requireArguments(arguments.length, 4, 'quadraticCurveTo', 'CanvasRenderingContext2D');
        cpx = +cpx; cpy = +cpy; x = +x; y = +y;
        if (!_fin4(cpx, cpy, x, y)) return;
        encode2dQuadraticCurveTo(this._canvasId, cpx, cpy, x, y);
    }

    bezierCurveTo(cp1x, cp1y, cp2x, cp2y, x, y) {
        _requireArguments(arguments.length, 6, 'bezierCurveTo', 'CanvasRenderingContext2D');
        cp1x = +cp1x; cp1y = +cp1y; cp2x = +cp2x; cp2y = +cp2y; x = +x; y = +y;
        if (!_fin6(cp1x, cp1y, cp2x, cp2y, x, y)) return;
        encode2dBezierCurveTo(this._canvasId, cp1x, cp1y, cp2x, cp2y, x, y);
    }

    // A negative radius is an error the content is told about; any non-finite argument is a silent no-op, and is
    // checked first.
    arc(x, y, radius, startAngle, endAngle, counterclockwise = false) {
        _requireArguments(arguments.length, 5, 'arc', 'CanvasRenderingContext2D');
        x = +x; y = +y; radius = +radius; startAngle = +startAngle; endAngle = +endAngle;
        const ccw = !!counterclockwise;
        if (!_fin6(x, y, radius, startAngle, endAngle, 0)) return;
        if (radius < 0) throw _negativeRadius(radius);
        encode2dArc(this._canvasId, x, y, radius, startAngle, endAngle, ccw);
    }

    arcTo(x1, y1, x2, y2, radius) {
        _requireArguments(arguments.length, 5, 'arcTo', 'CanvasRenderingContext2D');
        x1 = +x1; y1 = +y1; x2 = +x2; y2 = +y2; radius = +radius;
        if (!_fin6(x1, y1, x2, y2, radius, 0)) return;
        if (radius < 0) throw _negativeRadius(radius);
        encode2dArcTo(this._canvasId, x1, y1, x2, y2, radius);
    }

    rect(x, y, width, height) {
        _requireArguments(arguments.length, 4, 'rect', 'CanvasRenderingContext2D');
        x = +x; y = +y; width = +width; height = +height;
        if (!_fin4(x, y, width, height)) return;
        encode2dRect(this._canvasId, x, y, width, height);
    }

    // `roundRect(x, y, w, h, radii)`: the radii are converted, then the rectangle checked, then the radii -- the
    // specification's order, which decides whether a call with a bad rectangle and bad radii throws.
    roundRect(x, y, w, h, radii = 0) {
        _requireArguments(arguments.length, 4, 'roundRect', 'CanvasRenderingContext2D');
        x = +x; y = +y; w = +w; h = +h;
        const converted = typeof radii === 'number' ? radii : _convertRadii(radii);
        if (!_fin4(x, y, w, h)) return;
        if (!_cornerRadii(converted, _roundRectRadii, 'roundRect')) return;
        encode2dRoundRect(this._canvasId, x, y, w, h, _roundRectRadii);
    }

    ellipse(x, y, radiusX, radiusY, rotation, startAngle, endAngle, counterclockwise = false) {
        _requireArguments(arguments.length, 7, 'ellipse', 'CanvasRenderingContext2D');
        x = +x; y = +y; radiusX = +radiusX; radiusY = +radiusY; rotation = +rotation;
        startAngle = +startAngle; endAngle = +endAngle;
        const ccw = !!counterclockwise;
        if (!_fin6(x, y, radiusX, radiusY, rotation, startAngle) || !_fin1(endAngle)) return;
        if (radiusX < 0 || radiusY < 0) throw _negativeRadius(radiusX < 0 ? radiusX : radiusY);
        encode2dEllipse(this._canvasId, x, y, radiusX, radiusY, rotation, startAngle, endAngle, ccw);
    }

    // ==================== Drawing Methods ====================

    // `fill(fillRule)` and `fill(path, fillRule)`, told apart as WebIDL tells overloads apart: two arguments, or a
    // `Path2D` first, is the second. A fill rule that is not one of the two names is a TypeError.
    fill(pathOrFillRule = undefined, fillRule = undefined) {
        let path = null;
        let evenOdd;
        if (arguments.length >= 2 || _isPath2D(pathOrFillRule)) {
            path = _path2DArgument(pathOrFillRule, 'fill');
            evenOdd = _fillRule(fillRule, 'fill');
        } else {
            evenOdd = _fillRule(pathOrFillRule, 'fill');
        }
        this._abandonPendingTextCache();
        if (path === null) {
            if (evenOdd) encode2dFillEvenOdd(this._canvasId);
            else encode2dFill(this._canvasId);
            return;
        }
        const segments = _segmentsOf(path);
        if (TypedArrayPrototypeGetLength(segments) === 0) return;
        if (!encode2dFillPath(this._canvasId, evenOdd, segments)) this._drawPathByOp(0, evenOdd, segments);
    }

    // `stroke()` and `stroke(path)`: an argument, even undefined, is the path.
    stroke(path = undefined) {
        const given = arguments.length !== 0 ? _path2DArgument(path, 'stroke') : null;
        this._abandonPendingTextCache();
        if (given === null) {
            encode2dStroke(this._canvasId);
            return;
        }
        const segments = _segmentsOf(given);
        if (TypedArrayPrototypeGetLength(segments) === 0) return;
        if (!encode2dStrokePath(this._canvasId, segments)) this._drawPathByOp(1, false, segments);
    }

    // `clip(fillRule)` and `clip(path, fillRule)`, as `fill`. An empty path is still a clip: it clips everything.
    clip(pathOrFillRule = undefined, fillRule = undefined) {
        let path = null;
        let evenOdd;
        if (arguments.length >= 2 || _isPath2D(pathOrFillRule)) {
            path = _path2DArgument(pathOrFillRule, 'clip');
            evenOdd = _fillRule(fillRule, 'clip');
        } else {
            evenOdd = _fillRule(pathOrFillRule, 'clip');
        }
        if (path === null) {
            if (evenOdd) encode2dClipEvenOdd(this._canvasId);
            else encode2dClip(this._canvasId);
            return;
        }
        const segments = _segmentsOf(path);
        if (!encode2dClipPath(this._canvasId, evenOdd, segments)) this._drawPathByOp(2, evenOdd, segments);
    }

    // A `Path2D` longer than a stream buffer holds, drawn by op behind what the stream holds. `kind` is 0 fill, 1 stroke,
    // 2 clip.
    _drawPathByOp(kind, evenOdd, segments) {
        this._barrier();
        op_canvas2d_draw_path(this._canvasId, kind, evenOdd, segments);
    }

    // `isPointInPath(x, y, fillRule)` and `isPointInPath(path, x, y, fillRule)`: three arguments are the second form
    // when the first is a `Path2D`, four always are. The point is in canvas coordinates, not through the transform.
    isPointInPath(a, b, c = undefined, d = undefined) {
        const given = arguments.length;
        _requireArguments(given, 2, 'isPointInPath', 'CanvasRenderingContext2D');
        let path = null;
        let x, y, rule;
        if (given === 2 || (given === 3 && !_isPath2D(a))) {
            x = +a; y = +b; rule = c;
        } else {
            path = _path2DArgument(a, 'isPointInPath');
            x = +b; y = +c; rule = d;
        }
        return this._hitTest(path, x, y, false, _fillRule(rule, 'isPointInPath'));
    }

    // `isPointInStroke(x, y)` and `isPointInStroke(path, x, y)`: under the current line styles and transform.
    isPointInStroke(a, b, c = undefined) {
        const given = arguments.length;
        _requireArguments(given, 2, 'isPointInStroke', 'CanvasRenderingContext2D');
        if (given === 2) return this._hitTest(null, +a, +b, true, false);
        const path = _path2DArgument(a, 'isPointInStroke');
        return this._hitTest(path, +b, +c, true, false);
    }

    // Asked of the renderer, which holds the path, the transform and the line styles, after everything recorded before.
    // A point that is not finite, or a `Path2D` with nothing in it, is outside without asking.
    _hitTest(path, x, y, stroke, evenOdd) {
        if (!_fin2(x, y)) return false;
        let segments = _NO_SEGMENTS;
        let flags = stroke ? HIT_TEST_STROKE : evenOdd ? HIT_TEST_EVEN_ODD : 0;
        if (path !== null) {
            segments = _segmentsOf(path);
            if (TypedArrayPrototypeGetLength(segments) === 0) return false;
            flags |= HIT_TEST_PATH;
        }
        this._barrier();
        return op_canvas2d_hit_test(this._canvasId, flags, x, y, segments);
    }

    // ==================== Rectangle Methods ====================

    fillRect(x, y, width, height) {
        _requireArguments(arguments.length, 4, 'fillRect', 'CanvasRenderingContext2D');
        x = +x; y = +y; width = +width; height = +height;
        this._abandonPendingTextCache();
        encode2dFillRect(this._canvasId, x, y, width, height);
    }

    strokeRect(x, y, width, height) {
        _requireArguments(arguments.length, 4, 'strokeRect', 'CanvasRenderingContext2D');
        x = +x; y = +y; width = +width; height = +height;
        this._abandonPendingTextCache();
        encode2dStrokeRect(this._canvasId, x, y, width, height);
    }

    clearRect(x, y, width, height) {
        _requireArguments(arguments.length, 4, 'clearRect', 'CanvasRenderingContext2D');
        x = +x; y = +y; width = +width; height = +height;
        this._abandonPendingTextCache();
        encode2dClearRect(this._canvasId, x, y, width, height);
    }

    // ==================== Text Methods ====================

    // `fillText(text, x, y, maxWidth)`: a DOMString and `unrestricted double`s, `maxWidth` optional.
    fillText(text, x, y, maxWidth = Infinity) {
        _requireArguments(arguments.length, 3, 'fillText', 'CanvasRenderingContext2D');
        text = _domString(text); x = +x; y = +y; maxWidth = +maxWidth;
        // Non-finite position: nothing is drawn. A maxWidth that is NaN or not positive draws nothing either
        // (Infinity, the default, is "no limit").
        if (!_fin2(x, y) || maxWidth !== maxWidth || !(maxWidth > 0)) return;
        // A second fillText before the prior pending entry was
        // consumed means the cocos single-label pattern doesn't
        // hold; abandon (and commit) the prior one first.
        this._abandonPendingTextCache();

        const args = this._buildTextCacheArgs(text);
        if (args !== null) {
            this._barrier();
            const hit = op_text_cache_peek_pin(
                args.text, args.fontRequest, args.fontSize, args.fontWeight,
                args.italic, args.fillColor, args.textAlign, args.textBaseline,
                args.canvasW, args.canvasH,
            );
            args._x = x;
            args._y = y;
            args._mw = maxWidth;
            if (hit === 1) {
                // HIT: suppress the Skia paint entirely.  The matching
                // full-canvas getImageData returns a cache-marked
                // ImageData; texImage2D copies the cached texture.
                this._tcState = 2;
                this._tcKey = args;
                return;
            }
            // MISS: render normally + remember the key so the
            // matching getImageData tags the snapshot for record.
            this._tcState = 1;
            this._tcKey = args;
        }
        this._barrier();
        op_fill_text(this._canvasId, text, x, y, maxWidth);
    }

    strokeText(text, x, y, maxWidth = Infinity) {
        _requireArguments(arguments.length, 3, 'strokeText', 'CanvasRenderingContext2D');
        text = _domString(text); x = +x; y = +y; maxWidth = +maxWidth;
        if (!_fin2(x, y) || maxWidth !== maxWidth || !(maxWidth > 0)) return;
        this._abandonPendingTextCache();
        this._barrier();
        op_stroke_text(this._canvasId, text, x, y, maxWidth);
    }

    measureText(text) {
        _requireArguments(arguments.length, 1, 'measureText', 'CanvasRenderingContext2D');
        const s = _domString(text);
        // R-10 + F-2: JS-side measure cache in front of the
        // native op.  Cross-thread RPC into the render thread
        // costs 30-50 us round-trip even on the cache-hit
        // path; `op_measure_text_flat` (R-7 / F-2) drops that
        // to ~5-10 us when the shared measurer is installed,
        // and ~10 us otherwise.  Most UI code calls
        // `measureText` with a repeating set of strings per
        // frame (labels, digit sprites, button captions) --
        // caching locally turns the hot case into a `Map.get`,
        // ~100 ns.
        //
        // Cache key: `${font}\x1f${text}`, the font serialised.  `\x1f`
        // is a control character a serialised font escapes, so no
        // key collisions.  Epoch-invalidated
        // against the global `__migoFontEpoch` set by
        // `op_load_font`; see `registerFontFamily` in the bundled
        // loader.
        const epoch = (globalThis.__migoFontEpoch | 0);
        if (this._measureCacheEpoch !== epoch) {
            this._measureCacheEpoch = epoch;
            this._measureCache = new Map();
        }
        const font = this._font;
        const key = font.text + '\x1f' + s;
        const hit = this._measureCache.get(key);
        if (hit !== undefined) return new TextMetrics(_TEXT_METRICS, hit);
        // R-7: a flat buffer, not a V8 object built field by field
        // on the hot measure path.  Layout is fixed little-endian f32
        // at the offsets documented on `op_measure_text_flat`; the
        // Float32Array view is zero-copy.
        //
        // The font crosses as `op_set_font` sends it, so a measurement
        // and the `fillText` after it resolve the same face.
        flushRenderCommandStream();
        const buf = op_measure_text_flat(this._canvasId, s, font.size, font.weight, font.italic, font.families);
        const metrics = new Float32Array(buf.buffer, buf.byteOffset, 12);
        // Cap the cache at 256 entries to match the render-side
        // LRU budget; oldest-inserted drops when full.  `Map`
        // iteration follows insertion order so this is O(1).
        if (this._measureCache.size >= 256) {
            const first = this._measureCache.keys().next().value;
            if (first !== undefined) this._measureCache.delete(first);
        }
        this._measureCache.set(key, metrics);
        return new TextMetrics(_TEXT_METRICS, metrics);
    }

    // ==================== Style Properties ====================

    // ==================== State setter dedup ====================
    //
    // Every setter below short-circuits when the incoming value is
    // strict-equal to the shadow copy: string (colour / composite),
    // number, or gradient / pattern object identity.  Animation
    // loops that assign the same `ctx.fillStyle` or same
    // `globalAlpha` on every frame stop pushing redundant
    // `SetFillStyle` / `SetGlobalAlpha` commands into the IPC
    // queue -- eliminates 30-70% of Canvas2D command volume on
    // UI-heavy scenes where setter assignment is not hoisted.

    get fillStyle() { return this._fillStyle; }
    set fillStyle(value) {
        if (value instanceof CanvasGradient) {
            if (this._fillStyle === value) return;
            this._fillStyle = value;
            value._apply();
        } else if (value instanceof CanvasPattern) {
            if (this._fillStyle === value) return;
            this._fillStyle = value;
            value._applyFill();
        } else {
            // A string that is not a colour leaves the style as it was; what reads back is the serialised colour.
            const entry = _cssColour(_domString(value));
            if (entry === null || entry.text === this._fillStyle) return;
            this._fillStyle = entry.text;
            const rgba = entry.rgba;
            encode2dSetFillStyle(this._canvasId, rgba[0] / 255, rgba[1] / 255, rgba[2] / 255, rgba[3] / 255);
        }
    }

    get strokeStyle() { return this._strokeStyle; }
    set strokeStyle(value) {
        if (value instanceof CanvasGradient) {
            if (this._strokeStyle === value) return;
            this._strokeStyle = value;
            value._applyStroke();
        } else if (value instanceof CanvasPattern) {
            if (this._strokeStyle === value) return;
            this._strokeStyle = value;
            value._applyStroke();
        } else {
            const entry = _cssColour(_domString(value));
            if (entry === null || entry.text === this._strokeStyle) return;
            this._strokeStyle = entry.text;
            const rgba = entry.rgba;
            encode2dSetStrokeStyle(this._canvasId, rgba[0] / 255, rgba[1] / 255, rgba[2] / 255, rgba[3] / 255);
        }
    }

    // Each of these keeps its previous value when assigned something the specification rejects: a number that is
    // not finite (and, where the attribute has a range, outside it) or a keyword it does not define. Storing the
    // rejected value instead -- as these did -- made `lineWidth = NaN` read NaN and draw nothing, and `lineCap =
    // "banana"` read back "banana" while drawing butt caps.
    get lineWidth() { return this._lineWidth; }
    set lineWidth(value) {
        const v = +value;
        if (!(v > 0) || v === Infinity) return;
        if (this._lineWidth === v) return;
        this._lineWidth = v;
        encode2dSetLineWidth(this._canvasId, v);
    }

    // The keyword attributes are WebIDL enums: the value is converted to a string (a Symbol is a TypeError), and one
    // that is not a keyword is ignored.
    get lineCap() { return this._lineCap; }
    set lineCap(value) {
        value = _domString(value);
        if (this._lineCap === value) return;
        if (!Object.prototype.hasOwnProperty.call(LINE_CAP_MAP, value)) return;
        this._lineCap = value;
        encode2dSetLineCap(this._canvasId, LINE_CAP_MAP[value]);
    }

    get lineJoin() { return this._lineJoin; }
    set lineJoin(value) {
        value = _domString(value);
        if (this._lineJoin === value) return;
        if (!Object.prototype.hasOwnProperty.call(LINE_JOIN_MAP, value)) return;
        this._lineJoin = value;
        encode2dSetLineJoin(this._canvasId, LINE_JOIN_MAP[value]);
    }

    get miterLimit() { return this._miterLimit; }
    set miterLimit(value) {
        const v = +value;
        if (!(v > 0) || v === Infinity) return;
        if (this._miterLimit === v) return;
        this._miterLimit = v;
        encode2dSetMiterLimit(this._canvasId, v);
    }

    // Outside 0..1 (or NaN) is ignored, not clamped: `globalAlpha = 2` leaves the previous value, as in a browser.
    get globalAlpha() { return this._globalAlpha; }
    set globalAlpha(value) {
        const v = +value;
        if (!(v >= 0 && v <= 1)) return;
        if (this._globalAlpha === v) return;
        this._globalAlpha = v;
        encode2dSetGlobalAlpha(this._canvasId, v);
    }

    get font() { return this._font.text; }
    set font(value) {
        // A string that is not a font leaves the font as it was; what reads back is the serialised font.
        const font = _cssFont(_domString(value));
        if (font === null || font.text === this._font.text) return;
        this._font = font;
        this._barrier();
        op_set_font(this._canvasId, font.size, font.weight, font.italic, font.families);
    }

    get textAlign() { return this._textAlign; }
    set textAlign(value) {
        value = _domString(value);
        if (this._textAlign === value) return;
        if (!Object.prototype.hasOwnProperty.call(TEXT_ALIGN_MAP, value)) return;
        this._textAlign = value;
        this._barrier();
        op_set_text_align(this._canvasId, TEXT_ALIGN_MAP[value]);
    }

    get textBaseline() { return this._textBaseline; }
    set textBaseline(value) {
        value = _domString(value);
        if (this._textBaseline === value) return;
        if (!Object.prototype.hasOwnProperty.call(TEXT_BASELINE_MAP, value)) return;
        this._textBaseline = value;
        this._barrier();
        op_set_text_baseline(this._canvasId, TEXT_BASELINE_MAP[value]);
    }

    get direction() { return this._direction; }
    set direction(value) {
        value = _domString(value);
        if (this._direction === value) return;
        if (!Object.prototype.hasOwnProperty.call(TEXT_DIRECTION_MAP, value)) return;
        this._direction = value;
        this._barrier();
        op_set_text_direction(this._canvasId, TEXT_DIRECTION_MAP[value]);
    }

    // ==================== State Methods ====================

    // Called by Canvas.set width/height after op_resize_canvas.  The Rust
    // render thread resets Canvas2DState to defaults on every canvas resize
    // (via Canvas2DRenderer::reset), so the JS shadow state must match or
    // the early-return guards in property setters will suppress the
    // corresponding op_set_* calls, leaving the render thread in its
    // post-reset default state while JS thinks the old values are still live.
    _resetShadowState() {
        this._fillStyle = '#000000';
        this._strokeStyle = '#000000';
        this._lineWidth = 1;
        this._lineCap = 'butt';
        this._lineJoin = 'miter';
        this._miterLimit = 10;
        this._globalAlpha = 1;
        this._font = _DEFAULT_FONT;
        this._textAlign = 'start';
        this._textBaseline = 'alphabetic';
        this._imageSmoothing = true;
        this._imageSmoothingQuality = 'low';
        this._direction = 'inherit';
        this._tm = [1, 0, 0, 1, 0, 0];
        this._stateStack = [];
        this._compositeOp = null;
        this._lineDash = null;
        this._lineDashOffset = null;
        this._shadowBlur = null;
        this._shadowColor = null;
        this._shadowOffsetX = null;
        this._shadowOffsetY = null;
    }

    save() {
        this._stateStack.push({
            fillStyle: this._fillStyle,
            strokeStyle: this._strokeStyle,
            lineWidth: this._lineWidth,
            lineCap: this._lineCap,
            lineJoin: this._lineJoin,
            miterLimit: this._miterLimit,
            globalAlpha: this._globalAlpha,
            font: this._font,
            textAlign: this._textAlign,
            textBaseline: this._textBaseline,
            // Drawing state like the rest: the renderer's `restore()` gives it back, so this side must too, or a setter
            // after a restore compares against the inner value and sends nothing.
            direction: this._direction,
            imageSmoothing: this._imageSmoothing,
            imageSmoothingQuality: this._imageSmoothingQuality,
            tm: this._tm.slice(),
            compositeOp: this._compositeOp,
            lineDash: this._lineDash ? this._lineDash.slice() : null,
            lineDashOffset: this._lineDashOffset,
            shadowBlur: this._shadowBlur,
            shadowColor: this._shadowColor,
            shadowOffsetX: this._shadowOffsetX,
            shadowOffsetY: this._shadowOffsetY,
        });
        encode2dSave(this._canvasId);
    }

    restore() {
        if (this._stateStack.length > 0) {
            const state = this._stateStack.pop();
            Object.assign(this, {
                _fillStyle: state.fillStyle,
                _strokeStyle: state.strokeStyle,
                _lineWidth: state.lineWidth,
                _lineCap: state.lineCap,
                _lineJoin: state.lineJoin,
                _miterLimit: state.miterLimit,
                _globalAlpha: state.globalAlpha,
                _font: state.font,
                _textAlign: state.textAlign,
                _textBaseline: state.textBaseline,
                _direction: state.direction,
                _imageSmoothing: state.imageSmoothing,
                _imageSmoothingQuality: state.imageSmoothingQuality,
                _tm: state.tm,
                _compositeOp: state.compositeOp,
                _lineDash: state.lineDash,
                _lineDashOffset: state.lineDashOffset,
                _shadowBlur: state.shadowBlur,
                _shadowColor: state.shadowColor,
                _shadowOffsetX: state.shadowOffsetX,
                _shadowOffsetY: state.shadowOffsetY,
            });
        }
        encode2dRestore(this._canvasId);
    }

    // ==================== Transform Methods ====================

    translate(x, y) {
        _requireArguments(arguments.length, 2, 'translate', 'CanvasRenderingContext2D');
        x = +x; y = +y;
        if (!_fin2(x, y)) return;
        const m = this._tm;
        m[4] += m[0] * x + m[2] * y;
        m[5] += m[1] * x + m[3] * y;
        encode2dTranslate(this._canvasId, x, y);
    }

    rotate(angle) {
        _requireArguments(arguments.length, 1, 'rotate', 'CanvasRenderingContext2D');
        angle = +angle;
        if (!_fin1(angle)) return;
        const cos = Math.cos(angle), sin = Math.sin(angle);
        const m = this._tm;
        const a = m[0], b = m[1], c = m[2], d = m[3];
        m[0] = a * cos + c * sin;
        m[1] = b * cos + d * sin;
        m[2] = a * -sin + c * cos;
        m[3] = b * -sin + d * cos;
        encode2dRotate(this._canvasId, angle);
    }

    scale(x, y) {
        _requireArguments(arguments.length, 2, 'scale', 'CanvasRenderingContext2D');
        x = +x; y = +y;
        if (!_fin2(x, y)) return;
        this._tm[0] *= x; this._tm[1] *= x;
        this._tm[2] *= y; this._tm[3] *= y;
        encode2dScale(this._canvasId, x, y);
    }

    // `setTransform(a, b, c, d, e, f)` and `setTransform(transform)`, the second taking a DOMMatrix2DInit (a DOMMatrix
    // is one) or nothing, which is the identity. A matrix with a value that is not finite changes nothing.
    setTransform(a, b, c, d, e, f) {
        const given = arguments.length;
        if (given > 1 && given < 6) throw _arityError(given, '0, 1, 6', 'setTransform', 'CanvasRenderingContext2D');
        if (given <= 1) {
            const m = _setTransformMatrix;
            _matrix2DInit(a, m, 'setTransform', 'CanvasRenderingContext2D');
            [a, b, c, d, e, f] = m;
        } else {
            a = +a; b = +b; c = +c; d = +d; e = +e; f = +f;
        }
        if (!_fin6(a, b, c, d, e, f)) return;
        this._tm[0] = a; this._tm[1] = b;
        this._tm[2] = c; this._tm[3] = d;
        this._tm[4] = e; this._tm[5] = f;
        encode2dSetTransform(this._canvasId, a, b, c, d, e, f);
    }

    resetTransform() {
        this._tm[0] = 1; this._tm[1] = 0;
        this._tm[2] = 0; this._tm[3] = 1;
        this._tm[4] = 0; this._tm[5] = 0;
        encode2dResetTransform(this._canvasId);
    }

    transform(a, b, c, d, e, f) {
        _requireArguments(arguments.length, 6, 'transform', 'CanvasRenderingContext2D');
        a = +a; b = +b; c = +c; d = +d; e = +e; f = +f;
        if (!_fin6(a, b, c, d, e, f)) return;
        // Multiply current matrix: CTM = CTM * [a b c d e f]
        const m = this._tm;
        const a0 = m[0], b0 = m[1], c0 = m[2], d0 = m[3], e0 = m[4], f0 = m[5];
        m[0] = a0 * a + c0 * b;
        m[1] = b0 * a + d0 * b;
        m[2] = a0 * c + c0 * d;
        m[3] = b0 * c + d0 * d;
        m[4] = a0 * e + c0 * f + e0;
        m[5] = b0 * e + d0 * f + f0;
        encode2dSetTransform(this._canvasId, m[0], m[1], m[2], m[3], m[4], m[5]);
    }

    getTransform() {
        const m = this._tm;
        return { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] };
    }

    // ==================== Image Methods ====================

    // `drawImage(image, dx, dy)`, `(image, dx, dy, dw, dh)` and `(image, sx, sy, sw, sh, dx, dy, dw, dh)`: three, five or
    // nine arguments (more than nine are the nine), the image a CanvasImageSource (see `_imageSourceKind`) and the rest
    // `unrestricted double`s, converted before the image's usability is checked. An argument that is not finite draws
    // nothing; so does an image still loading.
    drawImage(image, a1, a2, a3, a4, a5, a6, a7, a8) {
        const given = arguments.length;
        _requireArguments(given, 3, 'drawImage', 'CanvasRenderingContext2D');
        if (given !== 3 && given !== 5 && given < 9) throw _arityError(given, '3, 5, 9', 'drawImage', 'CanvasRenderingContext2D');
        const kind = _imageSourceKind(image, 'drawImage');
        a1 = +a1; a2 = +a2;
        if (given >= 5) { a3 = +a3; a4 = +a4; }
        if (given >= 9) { a5 = +a5; a6 = +a6; a7 = +a7; a8 = +a8; }
        if (!_imageUsable(image, kind, 'drawImage')) return;
        // The image's own size, which the shorter forms draw whole: a canvas's or a bitmap's pixels, an image's natural
        // size (its `width` is content's to change and does not resize what is drawn).
        const natural = isImage(image);
        const w = natural ? image.naturalWidth : image.width;
        const h = natural ? image.naturalHeight : image.height;
        let sx = 0, sy = 0, sw = w, sh = h, dx, dy, dw = w, dh = h;
        if (given === 3) {
            dx = a1; dy = a2;
        } else if (given === 5) {
            dx = a1; dy = a2; dw = a3; dh = a4;
        } else {
            sx = a1; sy = a2; sw = a3; sh = a4; dx = a5; dy = a6; dw = a7; dh = a8;
        }
        if (!_fin4(sx, sy, sw, sh) || !_fin4(dx, dy, dw, dh)) return;
        this._abandonPendingTextCache();
        if (kind === _SOURCE_CANVAS) {
            // A canvas the renderer cannot copy as it is (a WebGL one) names the 2D canvas that holds what it shows.
            const source = typeof image._imageSourceCanvas === 'function' ? image._imageSourceCanvas() : image;
            encode2dDrawCanvas(this._canvasId, source._rid, sx, sy, sw, sh, dx, dy, dw, dh);
            return;
        }
        this._barrier();
        op_draw_image(this._canvasId, image.rid, sx, sy, sw, sh, dx, dy, dw, dh);
    }

    drawImageBatch(draws) {
        if (!Array.isArray(draws) || draws.length === 0) return;
        if (draws.length > MAX_DRAW_IMAGE_BATCH_ENTRIES) {
            throw new RangeError("drawImageBatch exceeds the implementation limit");
        }

        this._barrier();

        // An image that has loaded, or a bitmap still open; anything else draws nothing.
        const validDraws = draws.filter(d => d !== null && typeof d === 'object'
            && (isImage(d.image) ? d.image.loaded === true : isImageBitmap(d.image) && d.image.rid !== 0));
        if (validDraws.length === 0) return;

        const buffer = new Float32Array(validDraws.length * 9);
        // The id's own bits, not a float: shared image ids live above 2^30,
        // where consecutive f32 values are 128 apart, so a float would name
        // another image -- or none.
        const ids = new Uint32Array(buffer.buffer);
        let offset = 0;

        for (const d of validDraws) {
            ids[offset++] = d.image.rid;
            buffer[offset++] = d.sx ?? -1;
            buffer[offset++] = d.sy ?? -1;
            buffer[offset++] = d.sw ?? -1;
            buffer[offset++] = d.sh ?? -1;
            buffer[offset++] = d.dx;
            buffer[offset++] = d.dy;
            buffer[offset++] = d.dw ?? -1;
            buffer[offset++] = d.dh ?? -1;
        }

        op_draw_image_batch(this._canvasId, new Uint8Array(buffer.buffer));
    }

    // `getImageData(sx, sy, sw, sh, settings)`: four `[EnforceRange] long`s, and the colour space the pixels are wanted
    // in -- the canvas's, sRGB, unless the settings name Display P3, which is converted to on the CPU and so takes none
    // of the snapshot paths below.
    getImageData(sx, sy, sw, sh, settings) {
        const context = "Failed to execute 'getImageData' on 'CanvasRenderingContext2D'";
        if (arguments.length < 4) {
            throw new TypeError(`${context}: 4 arguments required, but only ${arguments.length} present.`);
        }
        let x = _enforceRangeLong(sx, context);
        let y = _enforceRangeLong(sy, context);
        const rawWidth = _enforceRangeLong(sw, context);
        const rawHeight = _enforceRangeLong(sh, context);
        const colorSpace = _imageDataColorSpace(settings, context) ?? 'srgb';
        const srgb = colorSpace === 'srgb';
        // Zero-readback fast path.  Two layers of optimisation:
        //
        // 1. Snapshot id is allocated JS-side from a process-local
        //    counter so the call NEVER blocks on a render-thread
        //    round-trip -- the previous sync `op_get_image_data_snapshot`
        //    still cost 5-15ms per call when the render thread had a
        //    backlog (head-of-line stall).
        // 2. Capture op rides the existing frame collector, so it
        //    lands in the same FramePacket as the surrounding
        //    canvas2D draws (correct ordering vs the prior fillText)
        //    and the downstream texImage2DFromSnapshot GL op.
        //
        // Per-frame budget: the render-side pool caps at 1024 live
        // snapshots; we cap JS-side at 512 with a fall-back to the
        // legacy CPU path so a pathological scene (thousands of
        // distinct text sprites in one frame) stays correct rather
        // than silently dropping snapshots.  Counter resets on
        // frame-end (see the module-private frame-end hook registry below).
        const dimensions = checkedImageDataDimensions(rawWidth, rawHeight);
        const w = dimensions.width;
        const h = dimensions.height;
        // Negative source dimensions grow the rectangle in the opposite
        // direction; the returned pixels themselves are never flipped.
        if (rawWidth < 0) x += rawWidth;
        if (rawHeight < 0) y += rawHeight;
        const snapshotInBounds = srgb && x >= 0 && y >= 0
            && x + w <= this._canvas.width
            && y + h <= this._canvas.height;
        flushRenderCommandStream();

        // Text texture cache: only a full-canvas read participates
        // (cocos always does getImageData(0, 0, canvas.w, canvas.h)
        // right after the single fillText).
        if (this._tcState !== 0 && this._tcKey !== null) {
            const k = this._tcKey;
            const fullCanvas =
                x === 0 && y === 0 && w === k.canvasW && h === k.canvasH;
            if (fullCanvas && srgb && this._tcState === 2) {
                // HIT: no snapshot -- hand back a cache-marked
                // ImageData.  The pin is now owned by the upcoming
                // texImage2D (it unpins after the GPU copy).
                this._tcState = 0;
                this._tcKey = null;
                return _migoMakeTextCacheImageData(this, k, w, h);
            }
            if (fullCanvas && snapshotInBounds && this._tcState === 1
                    && w > 0 && h > 0
                    && _migoSnapshotBudgetAllows(w, h)) {
                // MISS: capture + record.  for_cache op tags the
                // snapshot so the render thread transfers its texture
                // into the cache at frame-end drain.
                const snapshotId = _migoNextSnapshotId();
                op_capture_canvas2d_snapshot_for_cache(
                    this._canvasId, x, y, w, h, snapshotId,
                    k.text, k.fontRequest, k.fontSize, k.fontWeight,
                    k.italic, k.fillColor, k.textAlign, k.textBaseline,
                    k.canvasW, k.canvasH,
                );
                _migoSnapshotCharge(w, h);
                this._tcState = 0;
                this._tcKey = null;
                return _migoMakeSnapshotImageData(snapshotId, w, h);
            }
            // Pattern didn't hold (partial read or budget exhausted):
            // abandon -- commits the suppressed fillText if it was a
            // hit -- then fall through to the legacy path below.
            this._abandonPendingTextCache();
        }

        if (snapshotInBounds && w > 0 && h > 0 && _migoSnapshotBudgetAllows(w, h)) {
            const snapshotId = _migoNextSnapshotId();
            op_capture_canvas2d_snapshot(this._canvasId, x, y, w, h, snapshotId);
            _migoSnapshotCharge(w, h);
            return _migoMakeSnapshotImageData(snapshotId, w, h);
        }
        // Fall back to legacy CPU path (zero-area, GLES 2, budget
        // exhausted, or a colour space to convert to).
        const data = new Uint8ClampedArray(op_get_image_data(this._canvasId, x, y, w, h));
        if (!srgb) _convertPrimaries(data, _SRGB_TO_P3);
        return new ImageData(_IMAGE_DATA, w, h, data, colorSpace);
    }

    // `createImageData(sw, sh, settings)` is a blank image of that size -- two `[EnforceRange] long`s, a negative one
    // counting from the other corner -- in the canvas's colour space or the one the settings name;
    // `createImageData(imagedata)` is a blank image of that one's size and colour space.
    createImageData(sw, sh, settings) {
        const context = "Failed to execute 'createImageData' on 'CanvasRenderingContext2D'";
        const argc = arguments.length;
        if (argc === 0) throw new TypeError(`${context}: 1 argument required, but only 0 present.`);
        if (argc === 1) {
            const fields = _imageDataFields(sw);
            if (fields === null) throw new TypeError(`${context}: parameter 1 is not of type 'ImageData'.`);
            const dimensions = checkedImageDataDimensions(fields[0], fields[1]);
            return new ImageData(_IMAGE_DATA, dimensions.width, dimensions.height,
                new Uint8ClampedArray(dimensions.bytes), fields[3]);
        }
        const width = _enforceRangeLong(sw, context);
        const height = _enforceRangeLong(sh, context);
        const colorSpace = _imageDataColorSpace(settings, context) ?? 'srgb';
        const dimensions = checkedImageDataDimensions(width, height);
        return new ImageData(_IMAGE_DATA, dimensions.width, dimensions.height,
            new Uint8ClampedArray(dimensions.bytes), colorSpace);
    }

    // `putImageData(imageData, dx, dy)` and `putImageData(imageData, dx, dy, dirtyX, dirtyY, dirtyWidth, dirtyHeight)`:
    // the pixels of `imageData` -- or only its dirty rectangle -- replace the canvas's at (dx, dy). The call ignores the
    // transform, the clip, `globalAlpha`, the composite operation and the shadow (HTML Standard, "putImageData"). The
    // algorithm below is the specification's; the arguments are an ImageData and WebIDL `[EnforceRange] long`s, and
    // pixels in Display P3 are converted to the canvas's sRGB on the way.
    putImageData(imageData, dx, dy, dirtyX, dirtyY, dirtyWidth, dirtyHeight) {
        const context = "Failed to execute 'putImageData' on 'CanvasRenderingContext2D'";
        const argc = arguments.length;
        // WebIDL's overloads take three arguments or seven; one past seven is ignored, a count between is neither.
        if (argc < 3) throw new TypeError(`${context}: 3 arguments required, but only ${argc} present.`);
        if (argc > 3 && argc < 7) {
            throw new TypeError(`${context}: Valid arities are: [3, 7], but ${argc} arguments provided.`);
        }
        const fields = _imageDataFields(imageData);
        if (fields === null) throw new TypeError(`${context}: parameter 1 is not of type 'ImageData'.`);
        dx = _enforceRangeLong(dx, context);
        dy = _enforceRangeLong(dy, context);
        const [width, height, data, colorSpace] = fields;
        let x = 0;
        let y = 0;
        let w = width;
        let h = height;
        if (argc >= 7) {
            x = _enforceRangeLong(dirtyX, context);
            y = _enforceRangeLong(dirtyY, context);
            w = _enforceRangeLong(dirtyWidth, context);
            h = _enforceRangeLong(dirtyHeight, context);
            // A negative width or height names the rectangle from its other corner.
            if (w < 0) { x += w; w = -w; }
            if (h < 0) { y += h; h = -h; }
            // Only what lies inside the ImageData is taken.
            if (x < 0) { w += x; x = 0; }
            if (y < 0) { h += y; y = 0; }
            if (x + w > width) w = width - x;
            if (y + h > height) h = height - y;
        }
        // A buffer content transferred away holds nothing to put.
        if (TypedArrayPrototypeGetLength(data) === 0) {
            throw domException(`${context}: The source data has been detached.`, 'InvalidStateError');
        }
        if (w <= 0 || h <= 0) return;
        // Where it lands, and the part of that inside the canvas: pixels past the edge are not sent.
        let destX = dx + x;
        let destY = dy + y;
        const canvasW = this._canvas.width | 0;
        const canvasH = this._canvas.height | 0;
        let srcX = x;
        let srcY = y;
        if (destX < 0) { srcX -= destX; w += destX; destX = 0; }
        if (destY < 0) { srcY -= destY; h += destY; destY = 0; }
        if (destX + w > canvasW) w = canvasW - destX;
        if (destY + h > canvasH) h = canvasH - destY;
        if (w <= 0 || h <= 0) return;
        // The rows of the rectangle, contiguous: the ImageData's own bytes when the rectangle is whole rows, a copy of the
        // rows otherwise.
        let pixels;
        if (w === width && colorSpace === 'srgb') {
            pixels = new Uint8Array(data.buffer, data.byteOffset + srcY * width * 4, w * h * 4);
        } else {
            pixels = new Uint8Array(w * h * 4);
            const rowBytes = w * 4;
            for (let row = 0; row < h; row++) {
                const from = ((srcY + row) * width + srcX) * 4;
                pixels.set(new Uint8Array(data.buffer, data.byteOffset + from, rowBytes), row * rowBytes);
            }
            if (colorSpace !== 'srgb') _convertPrimaries(pixels, _P3_TO_SRGB);
        }
        // Not a stream record: the bytes go in the frame as a command, behind everything the stream already holds.
        this._barrier();
        op_put_image_data(this._canvasId, destX, destY, w, h, pixels);
    }

    // ==================== Image smoothing ====================
    //
    // `false` samples a scaled `drawImage` at the nearest texel; `true` (the default) samples it bilinearly. Drawing state:
    // `save()` keeps it and `restore()` gives it back, on this side and, through the record, on the renderer's.
    //
    // Until this existed the name was an ordinary property on the object and nothing read it, so pixel art was blurred
    // on every platform -- which is what a conformance run on a Mac with a 2x display finally showed.
    get imageSmoothingEnabled() { return this._imageSmoothing; }
    set imageSmoothingEnabled(value) {
        const enabled = !!value;
        if (this._imageSmoothing === enabled) return;
        this._imageSmoothing = enabled;
        encode2dSetImageSmoothing(this._canvasId, enabled);
    }

    // A hint the specification lets an implementation ignore: `low`, `medium` and `high` all draw the same here, and a
    // value that is not one of the three is ignored, as it is in a browser.
    get imageSmoothingQuality() { return this._imageSmoothingQuality; }
    set imageSmoothingQuality(value) {
        value = _domString(value);
        if (value === 'low' || value === 'medium' || value === 'high') this._imageSmoothingQuality = value;
    }

    // ==================== Compositing ====================
    get globalCompositeOperation() { return this._compositeOp || 'source-over'; }
    set globalCompositeOperation(value) {
        value = _domString(value);
        if (this._compositeOp === value) return;
        var idx = _COMPOSITE_OPS.indexOf(value);
        if (idx !== -1) {
            this._compositeOp = value;
            encode2dSetCompositeOperation(this._canvasId, idx);
        }
    }

    // ==================== Shadows ====================
    // A negative blur and a non-finite blur or offset are ignored, like every other attribute with a domain.
    get shadowBlur() { return this._shadowBlur || 0; }
    set shadowBlur(value) {
        const v = +value;
        if (!(v >= 0) || v === Infinity) return;
        if (this._shadowBlur === v) return;
        this._shadowBlur = v;
        encode2dSetShadowBlur(this._canvasId, v);
    }
    get shadowColor() { return this._shadowColor || 'rgba(0, 0, 0, 0)'; }
    set shadowColor(value) {
        const entry = _cssColour(_domString(value));
        if (entry === null || entry.text === (this._shadowColor || 'rgba(0, 0, 0, 0)')) return;
        this._shadowColor = entry.text;
        const rgba = entry.rgba;
        encode2dSetShadowColor(this._canvasId, rgba[0] / 255, rgba[1] / 255, rgba[2] / 255, rgba[3] / 255);
    }
    get shadowOffsetX() { return this._shadowOffsetX || 0; }
    set shadowOffsetX(value) {
        const v = +value;
        if (v - v !== 0) return;
        if (this._shadowOffsetX === v) return;
        this._shadowOffsetX = v;
        encode2dSetShadowOffsetX(this._canvasId, v);
    }
    get shadowOffsetY() { return this._shadowOffsetY || 0; }
    set shadowOffsetY(value) {
        const v = +value;
        if (v - v !== 0) return;
        if (this._shadowOffsetY === v) return;
        this._shadowOffsetY = v;
        encode2dSetShadowOffsetY(this._canvasId, v);
    }

    // ==================== Gradient ====================
    // The factories take WebIDL `double`s: an argument that is not finite is a TypeError, as is a missing one, and a
    // negative radius an IndexSizeError.
    createLinearGradient(x0, y0, x1, y1) {
        const method = 'createLinearGradient';
        _requireArguments(arguments.length, 4, method, 'CanvasRenderingContext2D');
        x0 = _double(x0, method, 'CanvasRenderingContext2D');
        y0 = _double(y0, method, 'CanvasRenderingContext2D');
        x1 = _double(x1, method, 'CanvasRenderingContext2D');
        y1 = _double(y1, method, 'CanvasRenderingContext2D');
        return new CanvasGradient(_CANVAS_STYLE, 'linear', this._canvasId, x0, y0, 0, x1, y1, 0);
    }
    createRadialGradient(x0, y0, r0, x1, y1, r1) {
        const method = 'createRadialGradient';
        _requireArguments(arguments.length, 6, method, 'CanvasRenderingContext2D');
        x0 = _double(x0, method, 'CanvasRenderingContext2D');
        y0 = _double(y0, method, 'CanvasRenderingContext2D');
        r0 = _double(r0, method, 'CanvasRenderingContext2D');
        x1 = _double(x1, method, 'CanvasRenderingContext2D');
        y1 = _double(y1, method, 'CanvasRenderingContext2D');
        r1 = _double(r1, method, 'CanvasRenderingContext2D');
        if (r0 < 0 || r1 < 0) {
            throw domException("Failed to execute 'createRadialGradient' on 'CanvasRenderingContext2D': The " +
                (r0 < 0 ? "r0" : "r1") + " provided is less than 0.", "IndexSizeError");
        }
        return new CanvasGradient(_CANVAS_STYLE, 'radial', this._canvasId, x0, y0, r0, x1, y1, r1);
    }
    createConicGradient(startAngle, cx, cy) {
        const method = 'createConicGradient';
        _requireArguments(arguments.length, 3, method, 'CanvasRenderingContext2D');
        startAngle = _double(startAngle, method, 'CanvasRenderingContext2D');
        cx = _double(cx, method, 'CanvasRenderingContext2D');
        cy = _double(cy, method, 'CanvasRenderingContext2D');
        return new CanvasGradient(_CANVAS_STYLE, 'conic', this._canvasId, cx, cy, 0, startAngle, 0, 0);
    }

    // ==================== Line Dash ====================
    // Handled by the Rust render thread via Skia's `SkPathEffect::dash`
    // (see engine/crates/graphics/backend/gl/paint.rs).  Odd-length dash
    // arrays are doubled on the render side, matching the Canvas 2D spec.
    // A list with a negative or non-finite entry is rejected whole; an odd-length list is stored (and reported by
    // `getLineDash`) repeated to an even one, so `[5]` is `[5, 5]`.
    setLineDash(segments) {
        _requireArguments(arguments.length, 1, 'setLineDash', 'CanvasRenderingContext2D');
        const list = _numberSequence(segments, 'setLineDash', 'CanvasRenderingContext2D');
        for (let i = 0; i < list.length; i++) {
            const v = list[i];
            if (!(v >= 0) || v === Infinity) return;
        }
        if (list.length % 2 === 1) {
            for (let i = 0, n = list.length; i < n; i++) list.push(list[i]);
        }
        this._lineDash = list;
        this._barrier();
        var buf = new Float32Array(list);
        op_set_line_dash(this._canvasId, new Uint8Array(buf.buffer));
    }
    getLineDash() { return this._lineDash ? this._lineDash.slice() : []; }
    get lineDashOffset() { return this._lineDashOffset || 0; }
    set lineDashOffset(value) {
        const v = +value;
        if (v - v !== 0) return;
        this._lineDashOffset = v;
        encode2dSetLineDashOffset(this._canvasId, v);
    }

    // `createPattern(image, repetition)`: `image` is an image, an ImageBitmap or a canvas, and a pattern from a canvas
    // is the canvas as it is now -- what it draws afterwards does not reach the pattern. In the specification's order:
    // the image's usability first (an image still loading is a null pattern, whatever the repetition), then the
    // repetition -- `null` and "" are "repeat", anything else not one of the four names a SyntaxError.
    createPattern(image, repetition) {
        _requireArguments(arguments.length, 2, 'createPattern', 'CanvasRenderingContext2D');
        const kind = _imageSourceKind(image, 'createPattern');
        let rep = repetition === null ? '' : _domString(repetition);
        if (!_imageUsable(image, kind, 'createPattern')) return null;
        if (rep === '') rep = 'repeat';
        if (rep !== 'repeat' && rep !== 'repeat-x' && rep !== 'repeat-y' && rep !== 'no-repeat') {
            throw domException("Failed to execute 'createPattern' on 'CanvasRenderingContext2D': The provided " +
                "repetition value '" + rep + "' is not a valid repetition.", "SyntaxError");
        }
        if (kind === _SOURCE_IMAGE) return new CanvasPattern(_CANVAS_STYLE, this._canvasId, image.rid, rep);
        // A copy that belongs to the pattern: the canvas is selected for the record so it runs in the canvas's own
        // stream, after everything drawn to it so far. The id is allocated after what the stream holds, as every op in
        // this file is.
        flushRenderCommandStream();
        const imageId = op_create_image();
        encode2dCaptureImage(image._rid, imageId);
        const pattern = new CanvasPattern(_CANVAS_STYLE, this._canvasId, imageId, rep);
        _patternCopies.register(pattern, imageId);
        return pattern;
    }
}

// JS-side per-frame snapshot budget + id allocator (see
// `getImageData` above).  Both are process-globals so multiple
// CanvasRenderingContext2D instances share the same id namespace
// (matches the render-side pool, which is keyed by global id).
//
// MAX_LIVE_CANVAS2D_SNAPSHOTS_JS must stay strictly less than the
// render-side `MAX_LIVE_CANVAS2D_SNAPSHOTS` (currently 1024) so we
// fall back to the legacy CPU path before the render-side pool
// silently drops snapshots.
const MAX_LIVE_CANVAS2D_SNAPSHOTS_JS = 512;
// The render side refuses a capture past 64 MiB of live snapshots, and a refused capture reaches
// nobody: the op is fire-and-forget, so the ImageData built around it would read as zeros. This
// is the same bound counted here, at half, because the render pool is drained a frame after the
// frame that filled it and the two frames can overlap. Past it the read is taken eagerly, as a
// browser takes every one. The bound is held to the Rust constant by a test in `graphics`.
const MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES_JS = 32 * 1024 * 1024;
let _migoSnapshotFrameCount = 0;
let _migoSnapshotFrameBytes = 0;

function _migoSnapshotBudgetAllows(w, h) {
    return _migoSnapshotFrameCount < MAX_LIVE_CANVAS2D_SNAPSHOTS_JS
        && _migoSnapshotFrameBytes + w * h * 4 <= MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES_JS;
}

function _migoSnapshotCharge(w, h) {
    _migoSnapshotFrameCount++;
    _migoSnapshotFrameBytes += w * h * 4;
}

// The snapshot-backed ImageData this frame has made. Held strongly until the frame ends, so the
// end-of-frame pass can reach the ones nobody has read.
const _migoFrameSnapshotImageData = [];

let _migoSnapshotIdCounter = 0;
function _migoNextSnapshotId() {
    _migoSnapshotIdCounter = (_migoSnapshotIdCounter + 1) >>> 0;
    if (_migoSnapshotIdCounter === 0) {
        // u32 wrap; skip 0 (reserved sentinel).
        _migoSnapshotIdCounter = 1;
    }
    return _migoSnapshotIdCounter;
}

// Build a synthetic ImageData whose `.data` is materialized lazily on
// first access via a forced readback.  Critical for compatibility:
// engines (notably cocos) often inspect the byte buffer between
// getImageData and texImage2D -- e.g. an "is the region empty?" check
// reading `imageData.data[3]` to skip transparent labels.  With a raw
// zero-filled placeholder that probe always reads 0, so the engine
// silently skips the upload and the label is missing on screen.
//
// On first `.data` read we fire the snapshot readback once, populate
// the placeholder in place, and clear `__migo_snapshot_id__` so the
// downstream texImage2D / texSubImage2D paths route through the
// legacy bytes path with whatever is now in (and may have been
// modified in) the buffer.  When JS never touches `.data`, the
// snapshot fast path stays in effect and no readback occurs.
//
// The render pool holds a snapshot for the frame it was captured in and no longer, so a `.data`
// first read in a later frame would find nothing and read zeros -- a stored `ImageData` used for
// hit-testing was all transparent a frame later. So the frame-end pass below reads back every
// one that is neither read nor spent by then: the readback a browser does at the call, taken
// when it can no longer be avoided. `texImage2D(imageData)` within the frame, which is the
// pattern this exists for, spends the snapshot and costs nothing.
function _migoMakeSnapshotImageData(snapshotId, w, h) {
    const imageData = new ImageData(_IMAGE_DATA, w, h, new Uint8ClampedArray(w * h * 4), 'srgb', _readBackSnapshot);
    Object.defineProperty(imageData, '__migo_snapshot_id__', {
        value: snapshotId,
        enumerable: false,
        writable: true,
        configurable: true,
    });
    // Set by a `texImage2D`/`texSubImage2D` that took this snapshot: the render side has consumed
    // it, there is nothing left to read back, and the end-of-frame pass must leave it alone.
    Object.defineProperty(imageData, '__migo_snapshot_spent__', {
        value: false,
        enumerable: false,
        writable: true,
        configurable: true,
    });
    _migoFrameSnapshotImageData.push(imageData);
    return imageData;
}

// A snapshot `ImageData`'s bytes, on the first read of its `data`: the renderer's readback, unless a `texImage2D` has
// already taken the snapshot and left nothing to read.
function _readBackSnapshot(imageData, placeholder) {
    const sid = imageData.__migo_snapshot_id__ | 0;
    if (sid === 0) return;
    flushRenderCommandStream();
    const real = op_force_readback_snapshot(sid);
    if (real && real.length === placeholder.length) {
        placeholder.set(real);
    }
    // JS now owns the buffer; subsequent uploads must
    // pick up any in-place mutations from this point on.
    imageData.__migo_snapshot_id__ = 0;
}

// Synthetic ImageData for a text texture cache HIT.  No snapshot was
// captured (the offscreen fillText was suppressed entirely); the
// downstream texImage2D detects `__migo_text_cache_key__` and routes
// to `op_tex_image_2d_from_text_cache`, which uploads from the cached
// texture and unpins the entry.
//
// `.data` fallback: a game that actually inspects the bytes of the
// returned ImageData (cocos's "is this label empty?" probe) forces a
// correctness recovery -- we have no GPU readback for cached text
// textures, so re-render the text into the canvas, snapshot+read it
// back the legacy way, unpin the cache, and clear the marker so
// texImage2D routes the bytes path.  The target cocos game does NOT
// read `.data` for these labels (otherwise the pre-existing snapshot
// fast path wouldn't help either), so this stays cold in practice.
function _migoMakeTextCacheImageData(ctx, k, w, h) {
    // The bytes on the first read of `data`: the text drawn after all, and read back (see above).
    const imageData = new ImageData(_IMAGE_DATA, w, h, new Uint8ClampedArray(w * h * 4), 'srgb', (self, placeholder) => {
        const key = self.__migo_text_cache_key__;
        if (key) {
            ctx._barrier();
            op_text_cache_unpin(
                key.text, key.fontRequest, key.fontSize, key.fontWeight,
                key.italic, key.fillColor, key.textAlign, key.textBaseline,
                key.canvasW, key.canvasH,
            );
            op_fill_text(
                ctx._canvasId,
                key.text,
                key._x || 0,
                key._y || 0,
                key._mw == null ? Infinity : key._mw,
            );
            const sid = _migoNextSnapshotId();
            op_capture_canvas2d_snapshot(ctx._canvasId, 0, 0, w, h, sid);
            const real = op_force_readback_snapshot(sid);
            if (real && real.length === placeholder.length) {
                placeholder.set(real);
            }
            self.__migo_text_cache_key__ = null;
        }
    });
    Object.defineProperty(imageData, '__migo_text_cache_key__', {
        value: k,
        enumerable: false,
        writable: true,
        configurable: true,
    });
    return imageData;
}

// Frame-end callback registry. The unified frame-end op builds a single
// interleaved FramePacket from both Canvas2D and GL segments, with
// Materialize barriers at 2D->GL transitions.
const frameEndHooks = [];
function frameEndAll() {
    for (let i = 0; i < frameEndHooks.length; i++) {
        frameEndHooks[i]();
    }
}
// First, while the frame's snapshots are still the renderer's: read back the `ImageData` that
// nobody has read and nothing has consumed (see `_migoMakeSnapshotImageData`).
frameEndHooks.push(() => {
    const held = _migoFrameSnapshotImageData;
    for (let i = 0; i < held.length; i++) {
        const imageData = held[i];
        if (imageData.__migo_snapshot_id__ !== 0 && !imageData.__migo_snapshot_spent__) {
            // A readback that fails (the session is going away) must not take the frame with it.
            try {
                void imageData.data;
            } catch (e) {
                console.error("getImageData: reading a held ImageData at frame end failed:", e);
            }
        }
    }
    held.length = 0;
});
frameEndHooks.push(() => {
    // Flush any pending GL stream BEFORE building the frame packet so that
    // GL commands encoded in the JS buffer arrive at the render thread in the
    // same FramePacket as the Canvas2D work (design S8 ordering invariant).
    flushRenderCommandStream();
    op_frame_end_unified();
});
// Reset the per-frame snapshot budget AFTER op_frame_end_unified
// has flushed the FramePacket (so the capture ops we counted
// against this frame's budget are dispatched to the render thread
// before we clear the count).  The render-thread pool drains in
// the same present_and_raf step, so the next frame starts clean
// on both sides.
frameEndHooks.push(() => {
    _migoSnapshotFrameCount = 0;
    _migoSnapshotFrameBytes = 0;
});

// The native test/host side may need to terminate a synthetic frame without
// evaluating source that names an internal. `99_main.js` moves every
// `_internal*` hook onto the private, handle-retained host bridge before any
// game script can run.
globalThis._internalFrameEnd = frameEndAll;

export { CanvasRenderingContext2D, CanvasGradient, CanvasPattern, ImageData, Path2D, TextMetrics, frameEndAll };
