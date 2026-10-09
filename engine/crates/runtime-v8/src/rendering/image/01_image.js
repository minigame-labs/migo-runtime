import { primordials } from "ext:core/mod.js";
import {
    op_create_image,
    op_load_image,
    op_load_image_subrect,
    op_destroy_image,
    op_preload_images,
    op_clear_image_cache,
    op_get_image_cache_stats
} from "ext:core/ops";
import { createCallbackEvent, createListenerGroup, errorToString } from "ext:host_v8_base/02_async.js";

const { SafeFinalizationRegistry } = primordials;

const registry = new SafeFinalizationRegistry((rid) => {
    try {
        op_destroy_image(rid);
    } catch (_) { }
});

// The 2D context's test for its image arguments, set by each class (see their static blocks).
let isImage;
let isImageBitmap;

class Image {
    constructor(width, height) {
        this._src = "";
        this.width = 0;
        this.height = 0;
        this.naturalWidth = 0;
        this.naturalHeight = 0;
        this.complete = false;

        // Target decode dimensions: when set via constructor, the decoder
        // will resize the image to fit within these bounds (preserving
        // aspect ratio). This avoids decoding a 4096x4096 atlas at full
        // resolution when only a 512x512 version is needed.
        this._targetWidth = (typeof width === 'number' && width > 0) ? (width | 0) : 0;
        this._targetHeight = (typeof height === 'number' && height > 0) ? (height | 0) : 0;

        this._onload = null;
        this._onerror = null;
        this.#listeners = {
            load: createListenerGroup('Image load', true),
            error: createListenerGroup('Image error', true),
        };

        this._loaded = false;
        this._error = null;

        // "caller image id" (alias). Rust cache uses this for alias/ref tracking.
        this._rid = op_create_image();

        // "shared image id" (the actual underlying shared resource id)
        // If not loaded yet, fall back to _rid.
        this._shared_img_id = this._rid;

        // prevent out-of-order loads from stomping state
        this._load_seq = 0;

        // token used for unregister if needed
        this._finalize_token = {};
        registry.register(this, this._rid, this._finalize_token);
    }

    // For drawImage: prefer shared id if available.
    get rid() {
        return this._shared_img_id ?? this._rid;
    }

    static {
        // Whether a value is one of these, by a field only the constructor gives: what a 2D context takes as an image.
        isImage = (value) => value !== null && typeof value === 'object' && #listeners in value;
    }

    get loaded() {
        return this._loaded;
    }

    get error() {
        return this._error;
    }

    get src() {
        return this._src;
    }

    set src(url) {
        this._src = String(url ?? "");
        this._startLoad(this._src);
    }

    set onload(fn) {
        this._onload = typeof fn === "function" ? fn : null;
    }

    set onerror(fn) {
        this._onerror = typeof fn === "function" ? fn : null;
    }

    #listeners;

    addEventListener(type, fn) {
        const group = this.#listeners[type];
        if (!group) return;
        group.on(fn);
    }

    removeEventListener(type, fn) {
        const group = this.#listeners[type];
        if (!group) return;
        group.off(fn);
    }

    #fireListeners(type, arg) {
        const group = this.#listeners[type];
        if (!group) return;
        group.trigger(arg, this);
    }

    _startLoad(url) {
        const seq = ++this._load_seq;

        // reset observable state like browsers do (roughly)
        this._loaded = false;
        this._error = null;
        this.width = 0;
        this.height = 0;
        this.naturalWidth = 0;
        this.naturalHeight = 0;
        this.complete = false;

        // Empty src: treat as error (browsers treat as a request to current document; for your runtime we error)
        if (!url) {
            const err = new TypeError("Image.src is empty");
            this._error = err;
            this.complete = false;
            const ev = createCallbackEvent("error", this, { error: err });
            this._onerror && this._onerror.call(this, ev);
            this.#fireListeners('error', ev);
            return;
        }

        op_load_image(this._rid, url, this._targetWidth, this._targetHeight)
            .then((dim) => {
                // out-of-order: ignore if a newer src has been set
                if (seq !== this._load_seq) return;

                const sharedId = dim[0];
                const w = dim[1][0];
                const h = dim[1][1];

                this._shared_img_id = sharedId;
                this.width = w;
                this.height = h;
                this.naturalWidth = w;
                this.naturalHeight = h;

                this._loaded = true;
                this._error = null;
                this.complete = true;
                const ev = createCallbackEvent("load", this);

                if (this._onload) {
                    try {
                        this._onload.call(this, ev);
                    } catch (e) {
                        try {
                            console.error(`Image onload error: ${errorToString(e)}`);
                        } catch (_) {}
                    }
                }
                this.#fireListeners('load', ev);
            })
            .catch((err) => {
                if (seq !== this._load_seq) return;

                this._shared_img_id = this._rid; // fall back
                this._loaded = false;
                this._error = err;
                this.complete = false;
                const ev = createCallbackEvent("error", this, { error: err });

                if (this._onerror) {
                    try {
                        this._onerror.call(this, ev);
                    } catch (e) {
                        try {
                            console.error(`Image onerror error: ${errorToString(e)}`);
                        } catch (_) {}
                    }
                }
                this.#fireListeners('error', ev);
            });
    }
}

const createImage = (width, height) => new Image(width, height);

// The bitmaps `createImageBitmap` makes; content cannot construct one (`new ImageBitmap()` is a TypeError, as in a
// browser), which would otherwise name any image the renderer holds.
const _IMAGE_BITMAP = Symbol('ImageBitmap');

/**
 * ImageBitmap - a loaded, decoded image tied to a GPU texture.
 *
 * Browser parity:
 *   - usable anywhere an Image/HTMLCanvasElement can be passed
 *     (drawImage, texImage2D, texSubImage2D)
 *   - exposes `width` / `height` for the final drawn size
 *   - `close()` releases the GPU texture deterministically;
 *     finalization-registry is a safety net in case JS drops the
 *     ref without calling close().
 *
 * Internally this reuses the same `_shared_img_id` alias mechanism
 * as Image so the Rust-side upload pool dedupes across identical
 * `(src, resizeW, resizeH)` keys -- e.g. a scroll list that builds
 * 200 ImageBitmaps from one loaded Image at a fixed thumbnail size
 * ends up sharing a single GPU texture.
 */
class ImageBitmap {
    #bitmap = true;

    // (rid, sharedId, width, height), after the token.
    constructor() {
        if (arguments[0] !== _IMAGE_BITMAP) throw new TypeError("Illegal constructor");
        const [, rid, sharedId, width, height] = arguments;
        this._rid = rid;
        this._shared_img_id = sharedId;
        this.width = width;
        this.height = height;
        this._closed = false;
        this._finalize_token = {};
        registry.register(this, this._rid, this._finalize_token);
    }

    get rid() {
        return this._closed ? 0 : (this._shared_img_id ?? this._rid);
    }

    static {
        isImageBitmap = (value) => value !== null && typeof value === 'object' && #bitmap in value;
    }

    close() {
        if (this._closed) return;
        this._closed = true;
        try { op_destroy_image(this._rid); } catch (_) { }
        registry.unregister(this._finalize_token);
        this.width = 0;
        this.height = 0;
    }
}

/**
 * createImageBitmap(source[, options])
 * createImageBitmap(source, sx, sy, sw, sh[, options])
 *
 * Returns a Promise that resolves to an `ImageBitmap`.  The MVP
 * scope is:
 *
 * 1. `source` is an `Image` (or another `ImageBitmap`) produced by
 *    this runtime.  Blob/ArrayBuffer/ImageData inputs are not yet
 *    supported; call sites in cocos-style games overwhelmingly
 *    pass an Image.
 * 2. Optional `resizeWidth` / `resizeHeight` produce a bitmap at
 *    the requested dimensions.  This replays the source's `src`
 *    through `op_load_image` with target dims, which hits the
 *    `(path, gen, tw, th)` LRU slot added in the image-cache
 *    refactor -- so repeated calls with the same target dims are
 *    O(1) after the first decode.
 * 3. The 5-argument (sx, sy, sw, sh) sub-rect form is accepted
 *    syntactically but behaves as a full-image bitmap for now.
 *    Sub-rect extraction is planned with a host-side crop op.
 */
async function createImageBitmap(source, ...args) {
    if (source == null || typeof source !== 'object') {
        throw new TypeError('createImageBitmap: invalid source');
    }

    // Parse positional args per WHATWG:
    //   createImageBitmap(source[, options])
    //   createImageBitmap(source, sx, sy, sw, sh[, options])
    let opts = null;
    let subrect = null;  // { sx, sy, sw, sh } or null
    if (args.length === 1 && args[0] && typeof args[0] === 'object') {
        opts = args[0];
    } else if (args.length >= 4) {
        // WHATWG HTML spec `createImageBitmap(source, sx, sy, sw, sh[, options])`:
        //   - If sw or sh is zero: reject with InvalidStateError.
        //   - If sw is negative: let sx = sx + sw; sw = -sw. (same for sh)
        // `| 0` coerces each arg to a signed 32-bit integer which
        // preserves the sign so the negative-normalisation below
        // produces the canonical (sx, sw >= 0) box.
        let sx = args[0] | 0;
        let sy = args[1] | 0;
        let sw = args[2] | 0;
        let sh = args[3] | 0;
        if (sw === 0 || sh === 0) {
            throw new RangeError(
                'createImageBitmap: sw and sh must be non-zero'
            );
        }
        if (sw < 0) { sx += sw; sw = -sw; }
        if (sh < 0) { sy += sh; sh = -sh; }
        subrect = { sx, sy, sw, sh };
        if (args.length >= 5 && args[4] && typeof args[4] === 'object') {
            opts = args[4];
        }
    }

    // Await source readiness.  Images that have already settled
    // (loaded or errored) resolve synchronously through the
    // listener-group trigger-on-late-subscribe semantics.
    if ('complete' in source && !source.complete && !source._error) {
        await new Promise((resolve, reject) => {
            const onLoad = () => { cleanup(); resolve(); };
            const onErr = (ev) => { cleanup(); reject(ev && ev.error ? ev.error : new Error('image load failed')); };
            const cleanup = () => {
                try { source.removeEventListener('load', onLoad); } catch (_) { }
                try { source.removeEventListener('error', onErr); } catch (_) { }
            };
            try { source.addEventListener('load', onLoad); } catch (_) { }
            try { source.addEventListener('error', onErr); } catch (_) { }
        });
    }
    if (source._error) {
        throw source._error;
    }

    const sharedId = source._shared_img_id ?? source.rid;
    if (!sharedId) {
        throw new TypeError('createImageBitmap: source has no backing texture');
    }

    const rw = (opts && opts.resizeWidth | 0) || 0;
    const rh = (opts && opts.resizeHeight | 0) || 0;

    // Sub-rect path: delegate to a dedicated op that crops the
    // decoded RGBA on the Rust side and uploads as a new texture.
    // This is the only mode that handles out-of-bounds sx/sy/sw/sh
    // with the spec-mandated transparent-black fill.
    // After the parsing block above `subrect.sw`/`subrect.sh` are
    // guaranteed positive; sw === 0 | sh === 0 already threw.
    if (subrect) {
        if (!source._src) {
            throw new Error(
                'createImageBitmap: sub-rect requires a source with src'
            );
        }
        const rid2 = op_create_image();
        const final_w = rw > 0 ? rw : subrect.sw;
        const final_h = rh > 0 ? rh : subrect.sh;
        const dim = await op_load_image_subrect(
            rid2,
            source._src,
            subrect.sx,
            subrect.sy,
            subrect.sw,
            subrect.sh,
            final_w,
            final_h,
        );
        return new ImageBitmap(_IMAGE_BITMAP, rid2, dim[0], dim[1][0], dim[1][1]);
    }

    const wantsResize = rw > 0 && rh > 0 && (rw !== source.width || rh !== source.height);

    // Fast path: no resize requested -> share the source's texture
    // via a fresh alias id.  The Rust cache treats this as an
    // alias-only `op_load_image` (same src+dims hits
    // BeginLoadResult::AlreadyLoaded) and bumps the refcount so
    // bitmap.close() decrements independently of the source Image.
    if (!wantsResize) {
        if (!source._src) {
            // No src => can't re-alias via cache; still return a
            // bitmap sharing the id directly.  close() will decref
            // the shared texture; caller accepts this trade-off.
            return new ImageBitmap(_IMAGE_BITMAP, sharedId, sharedId, source.width, source.height);
        }
        const rid2 = op_create_image();
        const dim = await op_load_image(rid2, source._src, 0, 0);
        return new ImageBitmap(_IMAGE_BITMAP, rid2, dim[0], dim[1][0], dim[1][1]);
    }

    // Resize path: replay src with target dims.  Same cache
    // machinery handles dedup and eviction.
    if (!source._src) {
        throw new Error('createImageBitmap: resize requires a source with src');
    }
    const rid2 = op_create_image();
    const dim = await op_load_image(rid2, source._src, rw, rh);
    return new ImageBitmap(_IMAGE_BITMAP, rid2, dim[0], dim[1][0], dim[1][1]);
}

/**
 * ImagePreloader - Preload multiple images in parallel for better performance
 *
 * Usage:
 *   const preloader = new ImagePreloader();
 *   const results = await preloader.preload(["img1.png", "img2.png"]);
 *   // After preloading, images load instantly from cache
 *   const img = createImage();
 *   img.src = "img1.png"; // instant load from cache
 */
class ImagePreloader {
    constructor() {
        this._results = new Map();
    }

    /**
     * Preload multiple images in parallel
     * @param {string[]} paths - Array of image paths to preload
     * @returns {Promise<PreloadResult[]>} Array of results
     */
    async preload(paths) {
        if (!Array.isArray(paths) || paths.length === 0) {
            return [];
        }

        const results = await op_preload_images(paths);

        // Store results for later query
        const output = results.map(([path, success, width, height, errorMsg]) => {
            const result = {
                path,
                success,
                width: success ? width : 0,
                height: success ? height : 0,
                error: success ? null : errorMsg
            };
            this._results.set(path, result);
            return result;
        });

        return output;
    }

    /**
     * Check if a path was preloaded successfully
     * @param {string} path
     * @returns {boolean}
     */
    isLoaded(path) {
        const result = this._results.get(path);
        return result?.success ?? false;
    }

    /**
     * Get preload result for a path
     * @param {string} path
     * @returns {PreloadResult|null}
     */
    getResult(path) {
        return this._results.get(path) || null;
    }

    /**
     * Get all preload results
     * @returns {Map<string, PreloadResult>}
     */
    getAllResults() {
        return new Map(this._results);
    }

    /**
     * Clear preload results (does not clear the actual cache)
     */
    clearResults() {
        this._results.clear();
    }
}

/**
 * ImageCache - Utility for managing the image cache
 */
const ImageCache = {
    /**
     * Clear the entire image cache
     * Useful for memory management in resource-constrained environments
     */
    async clear() {
        await op_clear_image_cache();
    },

    /**
     * Get cache statistics
     * @returns {Promise<CacheStats>}
     */
    async getStats() {
        return await op_get_image_cache_stats();
    }
};

// ------------------------------------------------------------------
// Global prefetch helper
// ------------------------------------------------------------------
//
// Exposed as `migo.prefetchImage(urls)`. Decodes one or more images
// ahead of time so a subsequent `Image.src = path` or `drawImage`
// can complete without re-running decode.  Under the hood this just
// forwards to a process-singleton ImagePreloader: the decode path
// and cache semantics are identical, so callers that want the full
// PreloadResult can reuse the results array.
//
// Accepts either a single string or an array. Always resolves to an
// array so callers don't have to branch.
const _prefetchImageSharedPreloader = new ImagePreloader();

function prefetchImage(urls) {
    if (urls == null) return Promise.resolve([]);
    if (typeof urls === 'string') {
        return _prefetchImageSharedPreloader.preload([urls]);
    }
    if (Array.isArray(urls)) {
        // Filter non-strings defensively -- op_preload_images would
        // otherwise reject the whole batch on first bad entry.
        const clean = urls.filter(u => typeof u === 'string' && u.length > 0);
        return _prefetchImageSharedPreloader.preload(clean);
    }
    return Promise.resolve([]);
}

export {
    createImage,
    ImagePreloader,
    ImageCache,
    ImageBitmap,
    createImageBitmap,
    isImage,
    isImageBitmap,
    prefetchImage,
};
