/**
 * Canvas 2D Context - Command Batching Implementation
 *
 * All draw commands within a RAF frame are batched and sent as a single
 * message to the render thread, significantly reducing IPC overhead.
 */

import {
    flushRenderCommandStream,
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
    encode2dFill,
    encode2dStroke,
    encode2dClip,
    encode2dFillRect,
    encode2dStrokeRect,
    encode2dClearRect,
    encode2dSave,
    encode2dRestore,
    encode2dSetTransform,
    encode2dResetTransform,
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
    op_tex_image_2d_from_snapshot,
    // Text methods
    op_fill_text,
    op_stroke_text,
    // Style setters
    op_set_fill_style,
    op_set_stroke_style,
    op_set_font,
    op_set_text_align,
    op_set_text_baseline,
    op_set_text_direction,
    // Image methods
    op_draw_image,
    op_draw_image_batch,
    // Compositing + gradient + dash
    op_set_line_dash,
    op_set_fill_style_gradient,
    op_set_stroke_style_gradient,
    op_set_fill_style_pattern,
    op_set_stroke_style_pattern,
    op_set_shadow_color,
} from "ext:core/ops";

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

// Gradient object returned by createLinearGradient / createRadialGradient.
// Collects color stops and sends them to the render thread when assigned
// to fillStyle.
const MAX_CANVAS_SURFACE_PIXELS = 8192 * 8192;
const MAX_SYNC_CANVAS_RGBA_BYTES = 64 * 1024 * 1024;
const MAX_DRAW_IMAGE_BATCH_ENTRIES = 65_536;

function checkedImageDataDimensions(width, height) {
    // These parameters are WebIDL `long`s. Bitwise conversion implements the
    // required signed 32-bit conversion, including NaN/Infinity -> 0.
    width |= 0;
    height |= 0;
    if (width === 0 || height === 0) {
        throw new DOMException(
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

class CanvasGradient {
    constructor(type, canvasId, x0, y0, r0, x1, y1, r1) {
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
    addColorStop(offset, color) {
        var off = Number(offset);
        if (!Number.isFinite(off) || off < 0 || off > 1) {
            throw new RangeError("Failed to execute 'addColorStop': offset must be between 0 and 1");
        }
        if (typeof color !== 'string') {
            throw new TypeError("Failed to execute 'addColorStop': color must be a string");
        }
        var parsed = _parseColorToRGBA(color);
        this._stops.push({ offset: off, r: parsed[0], g: parsed[1], b: parsed[2], a: parsed[3] });
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

class CanvasPattern {
    constructor(canvasId, imageRid, repetition) {
        this._canvasId = canvasId;
        this._imageRid = imageRid;
        var rep = repetition == null ? 'repeat' : String(repetition);
        if (rep !== 'repeat' && rep !== 'repeat-x' && rep !== 'repeat-y' && rep !== 'no-repeat') {
            throw new TypeError("Failed to execute 'createPattern': invalid repetition value");
        }
        this._repeatX = rep === 'repeat' || rep === 'repeat-x';
        this._repeatY = rep === 'repeat' || rep === 'repeat-y';
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

// Minimal color string to [r,g,b,a] parser.
function _parseColorToRGBA(color) {
    if (typeof color !== 'string') return [0, 0, 0, 255];
    color = color.trim();
    // rgba(r,g,b,a) or rgb(r,g,b)
    var m = color.match(/^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+))?\s*\)$/);
    if (m) {
        var a = m[4] !== undefined ? Math.round(parseFloat(m[4]) * 255) : 255;
        return [parseInt(m[1]), parseInt(m[2]), parseInt(m[3]), a];
    }
    // #RRGGBB, #RGB, #RRGGBBAA, #RGBA
    if (color[0] === '#') {
        var hex = color.slice(1);
        if (hex.length === 3) hex = hex[0]+hex[0]+hex[1]+hex[1]+hex[2]+hex[2];
        if (hex.length === 4) hex = hex[0]+hex[0]+hex[1]+hex[1]+hex[2]+hex[2]+hex[3]+hex[3];
        var n = parseInt(hex.substring(0, 6), 16);
        var alpha = hex.length === 8 ? parseInt(hex.substring(6, 8), 16) : 255;
        return [(n >> 16) & 255, (n >> 8) & 255, n & 255, alpha];
    }
    // Named colors
    var named = _NAMED_COLORS[color.toLowerCase()];
    if (named) return named.slice();
    return [0, 0, 0, 255];
}

// The colour forms this encoder is willing to answer for itself.
//
// `parse_color_string` on the Rust side is the authority, and it stays the
// authority: this returns `null` for anything it is not certain it would agree
// with, and the caller falls back to the op that runs the Rust parser. So the
// contract is one-sided and checkable -- **this may abstain, it may not
// disagree** -- which is what makes a second parser tolerable here at all. The
// CSS *font* parser was deleted from this file for the opposite reason: it was
// two implementations both claiming to be right, and they drifted.
//
// Abstaining is not a slow path in any sense that matters. It costs exactly
// what every colour assignment cost before this existed: one stream flush and
// one op.
//
// `canvas2d_colour_agreement` in the Rust tests runs a corpus through the whole
// path -- this parser, the wire encoding, the decoder, and the Rust parser on
// the fallback -- and requires the resulting `Color` to be bit-identical either
// way.
function _strictColorToRGBA(color) {
    if (typeof color !== 'string') return null;
    const s = color.trim();
    if (s.length === 0) return null;

    if (s.charCodeAt(0) === 35 /* # */) {
        const hex = s.slice(1);
        for (let i = 0; i < hex.length; i++) {
            const c = hex.charCodeAt(i);
            const isHex = (c >= 48 && c <= 57) || (c >= 65 && c <= 70) || (c >= 97 && c <= 102);
            if (!isHex) return null;
        }
        // Short forms double each digit, exactly as `Color::hex` does.
        if (hex.length === 3 || hex.length === 4) {
            const r = parseInt(hex[0], 16), g = parseInt(hex[1], 16), b = parseInt(hex[2], 16);
            const a = hex.length === 4 ? parseInt(hex[3], 16) : 15;
            return [(r << 4) | r, (g << 4) | g, (b << 4) | b, (a << 4) | a];
        }
        if (hex.length === 6 || hex.length === 8) {
            const r = parseInt(hex.substring(0, 2), 16);
            const g = parseInt(hex.substring(2, 4), 16);
            const b = parseInt(hex.substring(4, 6), 16);
            const a = hex.length === 8 ? parseInt(hex.substring(6, 8), 16) : 255;
            return [r, g, b, a];
        }
        return null;
    }

    const lower = s.toLowerCase();
    if (lower.charCodeAt(0) === 114 /* r */ && s.charCodeAt(s.length - 1) === 41 /* ) */) {
        let inner = null, wantsAlpha = false;
        if (lower.startsWith('rgba(')) {
            inner = s.substring(5, s.length - 1);
            wantsAlpha = true;
        } else if (lower.startsWith('rgb(')) {
            inner = s.substring(4, s.length - 1);
        } else {
            return null;
        }
        const parts = inner.split(',');
        if (parts.length !== (wantsAlpha ? 4 : 3)) return null;
        const out = [0, 0, 0, 255];
        for (let i = 0; i < 3; i++) {
            const channel = _strictU8(parts[i].trim());
            if (channel === null) return null;
            out[i] = channel;
        }
        if (wantsAlpha) {
            const alpha = _strictAlpha(parts[3].trim());
            if (alpha === null) return null;
            out[3] = alpha;
        }
        return out;
    }

    // Named colours are answered only on a hit. Both tables read an unknown name
    // as black, so a miss answered here would agree today -- but only while the
    // tables are identical, and `the_named_colours_agree_name_for_name` is what
    // holds that, not this. Abstaining costs a miss path nothing and keeps this
    // function's own rule intact: it answers what it knows and guesses nothing.
    if (lower.length > 24) return null;
    const named = _NAMED_COLORS[lower];
    return named === undefined ? null : named;
}

// A channel exactly as Rust's `str::parse::<u8>` would take it: optional `+`,
// decimal digits, nothing else, and in range. Anything Rust would reject lands
// on `unwrap_or(0)` there, which this abstains from guessing at.
function _strictU8(text) {
    let i = 0;
    if (text.charCodeAt(0) === 43 /* + */) i = 1;
    if (i === text.length || text.length - i > 3) return null;
    let value = 0;
    for (; i < text.length; i++) {
        const digit = text.charCodeAt(i) - 48;
        if (digit < 0 || digit > 9) return null;
        value = value * 10 + digit;
    }
    return value > 255 ? null : value;
}

// The alpha channel, mirroring `parse::<f32>().unwrap_or(1.0).clamp(0,1) * 255.0
// as u8`. The arithmetic runs at `f32` through `Math.fround` because the Rust
// side does: at `f64` the product can land on the other side of an integer and
// truncate one lower.
//
// Only plain decimal literals are accepted. Exponents, infinities and NaN are
// left to the Rust parser rather than reimplemented.
function _strictAlpha(text) {
    let i = 0;
    if (text.charCodeAt(0) === 43 /* + */) i = 1;
    let digits = 0, dots = 0;
    for (let j = i; j < text.length; j++) {
        const c = text.charCodeAt(j);
        if (c === 46 /* . */) { dots++; if (dots > 1) return null; continue; }
        if (c < 48 || c > 57) return null;
        digits++;
    }
    if (digits === 0) return null;
    let value = Math.fround(parseFloat(text));
    if (!(value >= 0)) value = 0;
    if (value > 1) value = 1;
    return Math.trunc(Math.fround(value * 255));
}

// G-2: CSS `font` parsing used to live here as `_parseCssFont`
// and in Rust as a separate implementation.  Both parsers could
// subtly drift (different weight ladders, different unit
// conversions), producing silent "measureText disagrees with
// fillText" bugs.  The JS-side parser has been removed; the
// authoritative implementation is now
// `shared::css_font::parse_css_font` on the Rust side.  The JS
// layer only needs to pass the raw `this._font` string through
// to `op_measure_text_flat`, which parses it once through the
// `SharedTextMeasurer::measure_css` trait.

class CanvasRenderingContext2D {
    constructor(canvas) {
        this._canvas = canvas;
        this._canvasId = canvas._rid;

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
        this._font = '10px sans-serif';
        this._textAlign = 'start';
        this._textBaseline = 'alphabetic';

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

    // ==================== Text texture cache ====================

    // Build the cache-key argument tuple for the current 2D state, or
    // return null when the state is outside the cacheable whitelist
    // (anything that moves / recolours / blends the glyph run beyond
    // the keyed fields).  Font identity is carried entirely by the
    // raw `font` string -- JS deliberately does not re-parse CSS font
    // (see the G-2 note above), so size/weight/italic stay 0/false in
    // the key; the render-thread resolves the string authoritatively
    // and identical strings always render identically within a
    // process generation.
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
        const rgba = _parseColorToRGBA(this._fillStyle);
        const color = (((rgba[0] & 255) << 24)
            | ((rgba[1] & 255) << 16)
            | ((rgba[2] & 255) << 8)
            | (rgba[3] & 255)) >>> 0;
        return {
            text: String(text),
            fontRequest: this._font,
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

    // Called by WebGL `texImage2D(target, ..., canvasElement)` when the
    // source canvas's 2D context has pending text-cache state.  This is
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
    _consumeTextCacheForTexImage(glCanvasId, target, level, internalformat) {
        if (this._tcState === 0 || this._tcKey === null) return false;
        flushRenderCommandStream();
        const k = this._tcKey;
        const isHit = this._tcState === 2;
        this._tcState = 0;
        this._tcKey = null;
        if (isHit) {
            op_tex_image_2d_from_text_cache(
                glCanvasId, target, level, internalformat,
                k.text, k.fontRequest, k.fontSize, k.fontWeight,
                k.italic, k.fillColor, k.textAlign, k.textBaseline,
                k.canvasW, k.canvasH,
            );
            return true;
        }
        if (_migoSnapshotFrameCount >= MAX_LIVE_CANVAS2D_SNAPSHOTS_JS) {
            // Snapshot budget exhausted this frame: can't record.
            // Fall back to the normal direct path (text already
            // painted, so the canvas is correct).
            return false;
        }
        const snapId = _migoNextSnapshotId();
        _migoSnapshotFrameCount++;
        op_capture_canvas2d_snapshot_for_cache(
            this._canvasId, 0, 0, k.canvasW, k.canvasH, snapId,
            k.text, k.fontRequest, k.fontSize, k.fontWeight,
            k.italic, k.fillColor, k.textAlign, k.textBaseline,
            k.canvasW, k.canvasH,
        );
        op_tex_image_2d_from_snapshot(
            glCanvasId, target, level, internalformat,
            0, 0, snapId,
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

    moveTo(x, y) {
        encode2dMoveTo(this._canvasId, x, y);
    }

    lineTo(x, y) {
        encode2dLineTo(this._canvasId, x, y);
    }

    quadraticCurveTo(cpx, cpy, x, y) {
        encode2dQuadraticCurveTo(this._canvasId, cpx, cpy, x, y);
    }

    bezierCurveTo(cp1x, cp1y, cp2x, cp2y, x, y) {
        encode2dBezierCurveTo(this._canvasId, cp1x, cp1y, cp2x, cp2y, x, y);
    }

    arc(x, y, radius, startAngle, endAngle, counterclockwise = false) {
        encode2dArc(this._canvasId, x, y, radius, startAngle, endAngle, counterclockwise);
    }

    arcTo(x1, y1, x2, y2, radius) {
        encode2dArcTo(this._canvasId, x1, y1, x2, y2, radius);
    }

    rect(x, y, width, height) {
        encode2dRect(this._canvasId, x, y, width, height);
    }

    ellipse(x, y, radiusX, radiusY, rotation, startAngle, endAngle, counterclockwise = false) {
        encode2dEllipse(this._canvasId, x, y, radiusX, radiusY, rotation, startAngle, endAngle, counterclockwise);
    }

    // ==================== Drawing Methods ====================

    fill(pathOrFillRule) {
        this._abandonPendingTextCache();
        encode2dFill(this._canvasId);
    }

    stroke(path) {
        this._abandonPendingTextCache();
        encode2dStroke(this._canvasId);
    }

    clip(pathOrFillRule) {
        encode2dClip(this._canvasId);
    }

    // ==================== Rectangle Methods ====================

    fillRect(x, y, width, height) {
        this._abandonPendingTextCache();
        encode2dFillRect(this._canvasId, x, y, width, height);
    }

    strokeRect(x, y, width, height) {
        this._abandonPendingTextCache();
        encode2dStrokeRect(this._canvasId, x, y, width, height);
    }

    clearRect(x, y, width, height) {
        this._abandonPendingTextCache();
        encode2dClearRect(this._canvasId, x, y, width, height);
    }

    // ==================== Text Methods ====================

    fillText(text, x, y, maxWidth = Infinity) {
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
        op_fill_text(this._canvasId, String(text), x, y, maxWidth);
    }

    strokeText(text, x, y, maxWidth = Infinity) {
        this._abandonPendingTextCache();
        this._barrier();
        op_stroke_text(this._canvasId, String(text), x, y, maxWidth);
    }

    measureText(text) {
        const s = String(text);
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
        // Cache key: `${font}\x1f${text}`.  `\x1f` is an ASCII
        // unit-separator that cannot appear in a valid CSS font
        // shorthand or in any canvas-drawable text snippet we
        // care about, so no key collisions.  Epoch-invalidated
        // against the global `__migoFontEpoch` set by
        // `op_load_font`; see `registerFontFamily` in the bundled
        // loader.
        const epoch = (globalThis.__migoFontEpoch | 0);
        if (this._measureCacheEpoch !== epoch) {
            this._measureCacheEpoch = epoch;
            this._measureCache = new Map();
        }
        const key = this._font + '\x1f' + s;
        const hit = this._measureCache.get(key);
        if (hit !== undefined) return hit;
        // R-7: prefer the flat-buffer op so we skip serde_v8's
        // 12-field V8 object construction on the hot measure
        // path.  Layout is fixed little-endian f32 at the offsets
        // documented on `op_measure_text_flat`; the Float32Array
        // view is zero-copy.  Keep the old serde op as fallback
        // for older snapshots -- the engine exposes both.
        //
        // G-2: pass the raw CSS font string; Rust-side
        // `SharedTextMeasurer::measure_css` parses it through the
        // shared `css_font::parse_css_font` implementation, which
        // is also what the render-thread `SetFont` handler uses
        // so the two sides can't disagree.
        flushRenderCommandStream();
        const buf = op_measure_text_flat(this._canvasId, s, this._font);
        const f = new Float32Array(buf.buffer, buf.byteOffset, 12);
        const metrics = {
            width: f[0],
            actualBoundingBoxLeft: f[1],
            actualBoundingBoxRight: f[2],
            emHeightAscent: f[3],
            emHeightDescent: f[4],
            alphabeticBaseline: f[5],
            fontBoundingBoxDescent: f[6],
            actualBoundingBoxAscent: f[7],
            actualBoundingBoxDescent: f[8],
            fontBoundingBoxAscent: f[9],
            hangingBaseline: f[10],
            ideographicBaseline: f[11],
        };
        // Cap the cache at 256 entries to match the render-side
        // LRU budget; oldest-inserted drops when full.  `Map`
        // iteration follows insertion order so this is O(1).
        if (this._measureCache.size >= 256) {
            const first = this._measureCache.keys().next().value;
            if (first !== undefined) this._measureCache.delete(first);
        }
        this._measureCache.set(key, metrics);
        return metrics;
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
        if (this._fillStyle === value) return;
        this._fillStyle = value;
        if (value instanceof CanvasGradient) {
            value._apply();
        } else if (value instanceof CanvasPattern) {
            value._applyFill();
        } else {
            const rgba = _strictColorToRGBA(value);
            if (rgba === null) {
                this._barrier();
                op_set_fill_style(this._canvasId, String(value));
            } else {
                encode2dSetFillStyle(
                    this._canvasId,
                    rgba[0] / 255, rgba[1] / 255, rgba[2] / 255, rgba[3] / 255,
                );
            }
        }
    }

    get strokeStyle() { return this._strokeStyle; }
    set strokeStyle(value) {
        if (this._strokeStyle === value) return;
        this._strokeStyle = value;
        if (value instanceof CanvasGradient) {
            value._applyStroke();
        } else if (value instanceof CanvasPattern) {
            value._applyStroke();
        } else {
            const rgba = _strictColorToRGBA(value);
            if (rgba === null) {
                this._barrier();
                op_set_stroke_style(this._canvasId, String(value));
            } else {
                encode2dSetStrokeStyle(
                    this._canvasId,
                    rgba[0] / 255, rgba[1] / 255, rgba[2] / 255, rgba[3] / 255,
                );
            }
        }
    }

    get lineWidth() { return this._lineWidth; }
    set lineWidth(value) {
        if (this._lineWidth === value) return;
        this._lineWidth = value;
        encode2dSetLineWidth(this._canvasId, value);
    }

    get lineCap() { return this._lineCap; }
    set lineCap(value) {
        if (this._lineCap === value) return;
        this._lineCap = value;
        encode2dSetLineCap(this._canvasId, LINE_CAP_MAP[value] ?? 0);
    }

    get lineJoin() { return this._lineJoin; }
    set lineJoin(value) {
        if (this._lineJoin === value) return;
        this._lineJoin = value;
        encode2dSetLineJoin(this._canvasId, LINE_JOIN_MAP[value] ?? 0);
    }

    get miterLimit() { return this._miterLimit; }
    set miterLimit(value) {
        if (this._miterLimit === value) return;
        this._miterLimit = value;
        encode2dSetMiterLimit(this._canvasId, value);
    }

    get globalAlpha() { return this._globalAlpha; }
    set globalAlpha(value) {
        const clamped = Math.max(0, Math.min(1, value));
        if (this._globalAlpha === clamped) return;
        this._globalAlpha = clamped;
        encode2dSetGlobalAlpha(this._canvasId, this._globalAlpha);
    }

    get font() { return this._font; }
    set font(value) {
        if (this._font === value) return;
        // G-2: no JS-side parsing needed.  `op_set_font` parses on
        // the Rust side via `shared::css_font_shorthand::
        // parse_font_shorthand`, the same function the render
        // thread uses for `Canvas2DCmd::SetFont`.  One parser, one
        // source of truth.
        //
        // It also answers whether the value was a font at all.
        // WHATWG makes an unparseable assignment a no-op, and this
        // is where that has to be decided: `_font` is the string
        // `measureText` is later measured from, so accepting one
        // the render thread will reject is how the same `ctx.font`
        // comes to measure at one size and paint at another.
        this._barrier();
        if (!op_set_font(this._canvasId, value)) return;
        this._font = value;
    }

    get textAlign() { return this._textAlign; }
    set textAlign(value) {
        if (this._textAlign === value) return;
        this._textAlign = value;
        this._barrier();
        op_set_text_align(this._canvasId, TEXT_ALIGN_MAP[value] ?? 0);
    }

    get textBaseline() { return this._textBaseline; }
    set textBaseline(value) {
        if (this._textBaseline === value) return;
        this._textBaseline = value;
        this._barrier();
        op_set_text_baseline(this._canvasId, TEXT_BASELINE_MAP[value] ?? 3);
    }

    get direction() { return this._direction || 'inherit'; }
    set direction(value) {
        if (this._direction === value) return;
        this._direction = value;
        this._barrier();
        op_set_text_direction(this._canvasId, TEXT_DIRECTION_MAP[value] ?? 0);
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
        this._font = '10px sans-serif';
        this._textAlign = 'start';
        this._textBaseline = 'alphabetic';
        this._direction = undefined;
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
        const m = this._tm;
        m[4] += m[0] * x + m[2] * y;
        m[5] += m[1] * x + m[3] * y;
        encode2dTranslate(this._canvasId, x, y);
    }

    rotate(angle) {
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
        this._tm[0] *= x; this._tm[1] *= x;
        this._tm[2] *= y; this._tm[3] *= y;
        encode2dScale(this._canvasId, x, y);
    }

    setTransform(a, b, c, d, e, f) {
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

    drawImage(image, ...args) {
        this._abandonPendingTextCache();
        if (!image || !image.loaded) return;

        this._barrier();

        let sx, sy, sw, sh, dx, dy, dw, dh;

        if (args.length === 2) {
            [dx, dy] = args;
            sx = sy = 0;
            sw = image.width;
            sh = image.height;
            dw = sw;
            dh = sh;
        } else if (args.length === 4) {
            [dx, dy, dw, dh] = args;
            sx = sy = 0;
            sw = image.width;
            sh = image.height;
        } else if (args.length === 8) {
            [sx, sy, sw, sh, dx, dy, dw, dh] = args;
        } else {
            return;
        }

        op_draw_image(this._canvasId, image.rid, sx, sy, sw, sh, dx, dy, dw, dh);
    }

    drawImageBatch(draws) {
        if (!Array.isArray(draws) || draws.length === 0) return;
        if (draws.length > MAX_DRAW_IMAGE_BATCH_ENTRIES) {
            throw new RangeError("drawImageBatch exceeds the implementation limit");
        }

        this._barrier();

        const validDraws = draws.filter(d => d.image && d.image.loaded);
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

    getImageData(sx, sy, sw, sh) {
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
        let x = sx | 0;
        let y = sy | 0;
        const rawWidth = sw | 0;
        const rawHeight = sh | 0;
        const dimensions = checkedImageDataDimensions(rawWidth, rawHeight);
        const w = dimensions.width;
        const h = dimensions.height;
        // Negative source dimensions grow the rectangle in the opposite
        // direction; the returned pixels themselves are never flipped.
        if (rawWidth < 0) x += rawWidth;
        if (rawHeight < 0) y += rawHeight;
        const snapshotInBounds = x >= 0 && y >= 0
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
            if (fullCanvas && this._tcState === 2) {
                // HIT: no snapshot -- hand back a cache-marked
                // ImageData.  The pin is now owned by the upcoming
                // texImage2D (it unpins after the GPU copy).
                this._tcState = 0;
                this._tcKey = null;
                return _migoMakeTextCacheImageData(this, k, w, h);
            }
            if (fullCanvas && snapshotInBounds && this._tcState === 1
                    && w > 0 && h > 0
                    && _migoSnapshotFrameCount < MAX_LIVE_CANVAS2D_SNAPSHOTS_JS) {
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
                _migoSnapshotFrameCount++;
                this._tcState = 0;
                this._tcKey = null;
                return _migoMakeSnapshotImageData(snapshotId, w, h);
            }
            // Pattern didn't hold (partial read or budget exhausted):
            // abandon -- commits the suppressed fillText if it was a
            // hit -- then fall through to the legacy path below.
            this._abandonPendingTextCache();
        }

        if (snapshotInBounds && w > 0 && h > 0
                && _migoSnapshotFrameCount < MAX_LIVE_CANVAS2D_SNAPSHOTS_JS) {
            const snapshotId = _migoNextSnapshotId();
            op_capture_canvas2d_snapshot(this._canvasId, x, y, w, h, snapshotId);
            _migoSnapshotFrameCount++;
            return _migoMakeSnapshotImageData(snapshotId, w, h);
        }
        // Fall back to legacy CPU path (zero-area, GLES 2, or budget
        // exhausted).  Behaviour preserved bit-exactly.
        const data = op_get_image_data(this._canvasId, x, y, w, h);
        return { width: w, height: h, data: new Uint8ClampedArray(data) };
    }

    createImageData(sw, sh) {
        const dimensions = checkedImageDataDimensions(sw, sh);
        return {
            width: dimensions.width,
            height: dimensions.height,
            data: new Uint8ClampedArray(dimensions.bytes),
        };
    }

    putImageData(imageData, dx, dy) {
        // Not implemented
    }

    // ==================== Compositing ====================
    get globalCompositeOperation() { return this._compositeOp || 'source-over'; }
    set globalCompositeOperation(value) {
        if (this._compositeOp === value) return;
        var idx = _COMPOSITE_OPS.indexOf(value);
        if (idx !== -1) {
            this._compositeOp = value;
            encode2dSetCompositeOperation(this._canvasId, idx);
        }
    }

    // ==================== Shadows ====================
    get shadowBlur() { return this._shadowBlur || 0; }
    set shadowBlur(value) {
        const v = +value || 0;
        if (this._shadowBlur === v) return;
        this._shadowBlur = v;
        encode2dSetShadowBlur(this._canvasId, this._shadowBlur);
    }
    get shadowColor() { return this._shadowColor || 'rgba(0,0,0,0)'; }
    set shadowColor(value) {
        if (this._shadowColor === value) return;
        this._shadowColor = value;
        const rgba = _strictColorToRGBA(value);
        if (rgba === null) {
            this._barrier();
            op_set_shadow_color(this._canvasId, String(value));
        } else {
            encode2dSetShadowColor(
                this._canvasId,
                rgba[0] / 255, rgba[1] / 255, rgba[2] / 255, rgba[3] / 255,
            );
        }
    }
    get shadowOffsetX() { return this._shadowOffsetX || 0; }
    set shadowOffsetX(value) {
        const v = +value || 0;
        if (this._shadowOffsetX === v) return;
        this._shadowOffsetX = v;
        encode2dSetShadowOffsetX(this._canvasId, this._shadowOffsetX);
    }
    get shadowOffsetY() { return this._shadowOffsetY || 0; }
    set shadowOffsetY(value) {
        const v = +value || 0;
        if (this._shadowOffsetY === v) return;
        this._shadowOffsetY = v;
        encode2dSetShadowOffsetY(this._canvasId, this._shadowOffsetY);
    }

    // ==================== Gradient ====================
    createLinearGradient(x0, y0, x1, y1) {
        return new CanvasGradient('linear', this._canvasId, x0, y0, 0, x1, y1, 0);
    }
    createRadialGradient(x0, y0, r0, x1, y1, r1) {
        return new CanvasGradient('radial', this._canvasId, x0, y0, r0, x1, y1, r1);
    }
    createConicGradient(startAngle, cx, cy) {
        return new CanvasGradient('conic', this._canvasId, cx, cy, 0, startAngle, 0, 0);
    }

    // ==================== Line Dash ====================
    // Handled by the Rust render thread via Skia's `SkPathEffect::dash`
    // (see engine/crates/graphics/backend/gl/paint.rs).  Odd-length dash
    // arrays are doubled on the render side, matching the Canvas 2D spec.
    setLineDash(segments) {
        if (!Array.isArray(segments)) return;
        this._lineDash = segments.slice();
        this._barrier();
        var buf = new Float32Array(segments);
        op_set_line_dash(this._canvasId, new Uint8Array(buf.buffer));
    }
    getLineDash() { return this._lineDash ? this._lineDash.slice() : []; }
    get lineDashOffset() { return this._lineDashOffset || 0; }
    set lineDashOffset(value) {
        this._lineDashOffset = +value || 0;
        encode2dSetLineDashOffset(this._canvasId, this._lineDashOffset);
    }

    // ==================== Other stubs ====================
    isPointInPath() { return false; }
    isPointInStroke() { return false; }
    createPattern(image, repetition) {
        if (!image || !image.loaded) return null;
        return new CanvasPattern(this._canvasId, image.rid, repetition);
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
let _migoSnapshotFrameCount = 0;

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
function _migoMakeSnapshotImageData(snapshotId, w, h) {
    const placeholder = new Uint8ClampedArray(w * h * 4);
    let _populated = false;
    const imageData = { width: w, height: h };
    Object.defineProperty(imageData, '__migo_snapshot_id__', {
        value: snapshotId,
        enumerable: false,
        writable: true,
        configurable: true,
    });
    Object.defineProperty(imageData, 'data', {
        get() {
            if (!_populated) {
                _populated = true;
                const sid = imageData.__migo_snapshot_id__ | 0;
                if (sid !== 0) {
                    flushRenderCommandStream();
                    const real = op_force_readback_snapshot(sid);
                    if (real && real.length === placeholder.length) {
                        placeholder.set(real);
                    }
                    // JS now owns the buffer; subsequent uploads must
                    // pick up any in-place mutations from this point on.
                    imageData.__migo_snapshot_id__ = 0;
                }
            }
            return placeholder;
        },
        enumerable: true,
        configurable: true,
    });
    return imageData;
}

// Synthetic ImageData for a text texture cache HIT.  No snapshot was
// captured (the offscreen fillText was suppressed entirely); the
// downstream texImage2D detects `__migo_text_cache_key__` and routes
// to `op_tex_image_2d_from_text_cache`, which copies the cached
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
    const placeholder = new Uint8ClampedArray(w * h * 4);
    let _populated = false;
    const imageData = { width: w, height: h };
    Object.defineProperty(imageData, '__migo_text_cache_key__', {
        value: k,
        enumerable: false,
        writable: true,
        configurable: true,
    });
    Object.defineProperty(imageData, 'data', {
        get() {
            if (!_populated) {
                _populated = true;
                const key = imageData.__migo_text_cache_key__;
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
                    imageData.__migo_text_cache_key__ = null;
                }
            }
            return placeholder;
        },
        enumerable: true,
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
});

// The native test/host side may need to terminate a synthetic frame without
// evaluating source that names an internal. `99_main.js` moves every
// `_internal*` hook onto the private, handle-retained host bridge before any
// game script can run.
globalThis._internalFrameEnd = frameEndAll;

export { CanvasRenderingContext2D, CanvasGradient, frameEndAll };
