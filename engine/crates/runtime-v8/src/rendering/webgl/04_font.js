// From ext:core/mod.js, as every other engine module takes it: the global
// `Deno` is deno_core's bootstrap object, deleted before content runs and absent
// on a runtime that is not deno_core.
import { core } from "ext:core/mod.js";

const { ops } = core;

// Line height is a pure function of (family, size, bold, italic). The only
// thing that can change the answer is `loadFont` registering a new face, and
// that already announces itself through `__migoFontEpoch` -- the same counter
// measureText's per-canvas cache compares against. One epoch, two caches, so
// there is no second invalidation rule to keep in step with the first.
//
// It was worth caching because of where it is called from: the synchronous
// surface contract records this one as per-frame in label-heavy UI, sharing
// measureText's four-millisecond deadline class. Every one of those frames was
// stalling the JavaScript thread to be told a constant.
const _lineHeightCache = new Map();
let _lineHeightCacheEpoch = -1;

// Bounded, because the key space belongs to content: a game that measures a
// thousand sizes must not be able to grow this without limit.
//
// Over the bound, the oldest entry goes rather than the whole table. A Map
// iterates in insertion order, so evicting one is a `keys().next()` and a
// delete, and it costs nothing on the hit path. Clearing everything instead
// would be cheaper still to write and would turn a workload that cycles over
// slightly more than the bound into a permanent 0% hit rate -- the one case
// where a cache is pure overhead.
const _LINE_HEIGHT_CACHE_MAX = 256;

/**
 * loadFont(path, family?)
 *
 * Loads a custom font file and returns the canonical font family name.
 * When `family` is provided it becomes the preferred CSS family alias.
 * The runtime also registers compatibility aliases from the font file
 * stem and the font's internal family name when available.
 *
 * @param {string} path - Path to the font file (relative to game code directory).
 * @param {string} [family] - Optional CSS family alias to register explicitly.
 * @returns {string} Font family name, or empty string on failure.
 */
const loadFont = (path, family) => {
    if (typeof path !== 'string' || path.length === 0) {
        console.error('loadFont: path must be a non-empty string');
        return '';
    }
    if (family !== undefined && typeof family !== 'string') {
        console.error('loadFont: family must be a string when provided');
        return '';
    }
    console.info('migo.loadFont called: path=' + path + (family ? ', family=' + family : ''));
    const name = ops.op_load_font(path, family);
    console.info('migo.loadFont result: path=' + path + ', family=' + (name || 'null'));
    // R-10: bump the global font epoch so every per-canvas JS
    // measureText cache invalidates its stored metrics on the
    // next access.  Cheap monotonic counter; compared against
    // `this._measureCacheEpoch` in `CanvasRenderingContext2D.
    // measureText`.  No cross-thread synchronisation needed:
    // V8's single-threaded execution model guarantees the JS
    // measureText caller either sees the new epoch (cache miss
    // -> refetch) or the old one (cache hit on stale metrics),
    // and because any measurement depending on the newly-loaded
    // font has to run after `loadFont` returns to JS, the
    // happens-before is preserved by the microtask boundary.
    globalThis.__migoFontEpoch = (globalThis.__migoFontEpoch | 0) + 1;
    return name;
};

/**
 * getTextLineHeight(object)
 *
 * Returns the line height of text rendered with the given font configuration.
 *
 * @param {object} object
 * @param {string} [object.fontStyle='normal'] - Font style ('normal' or 'italic').
 * @param {string} [object.fontWeight='normal'] - Font weight ('normal' or 'bold').
 * @param {number} [object.fontSize=16] - Font size in pixels.
 * @param {string} [object.fontFamily='sans-serif'] - Font family name.
 * @param {string} [object.text=''] - Not used for measurement, kept for API compat.
 * @returns {number} Line height in pixels.
 */
const getTextLineHeight = (object) => {
    const obj = object || {};
    const fontStyle = obj.fontStyle || 'normal';
    const fontWeight = obj.fontWeight || 'normal';
    const fontSize = typeof obj.fontSize === 'number' ? obj.fontSize : 16;
    const fontFamily = obj.fontFamily || 'sans-serif';

    const bold = fontWeight === 'bold' || fontWeight === '700';
    const italic = fontStyle === 'italic';

    const epoch = globalThis.__migoFontEpoch | 0;
    if (epoch !== _lineHeightCacheEpoch) {
        _lineHeightCache.clear();
        _lineHeightCacheEpoch = epoch;
    }

    const key = fontFamily + '|' + fontSize + '|' + (bold ? 1 : 0) + (italic ? 1 : 0);
    let height = _lineHeightCache.get(key);
    if (height === undefined) {
        height = ops.op_get_text_line_height(fontFamily, fontSize, bold, italic);
        if (_lineHeightCache.size >= _LINE_HEIGHT_CACHE_MAX) {
            _lineHeightCache.delete(_lineHeightCache.keys().next().value);
        }
        _lineHeightCache.set(key, height);
    }
    return height;
};

export { loadFont, getTextLineHeight };
