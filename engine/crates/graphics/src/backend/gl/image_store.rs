//! Skia-friendly image registry.
//!
//! Replaces the femtovg `ImageId` bookkeeping with a thin
//! `image_id → (GL texture, dimensions)` map.  Pattern / drawImage paths
//! wrap a texture into an `SkImage` *on demand* via
//! [`resolve_as_sk_image`], because an `SkImage` is tied to a specific
//! [`DirectContext`] and canvases don't share one.
//!
//! Off-screen canvases that use the same texture would each create their
//! own transient `SkImage` on first reference.  This is cheap — Skia's
//! [`images::borrow_from_backend_texture`] does no upload, just a
//! metadata wrap.
//!
//! [`images::borrow_from_backend_texture`]: https://skia.org/docs/user/api/gpu/

use std::collections::HashMap;

use skia_safe::{
    AlphaType, ColorType, Image as SkImage,
    gpu::{self, Mipmapped, SurfaceOrigin, backend_textures, gl::TextureInfo},
};

/// Metadata we retain for every uploaded image.  Kept deliberately minimal;
/// everything else (shaders, sub-images, …) is rebuilt on demand.
#[derive(Clone, Copy, Debug)]
pub struct GpuImageInfo {
    pub width: u32,
    pub height: u32,
    /// Colour type used when wrapping the texture into an `SkImage`.
    /// Separated from `has_alpha` to accommodate future premultiplied-BGRA
    /// uploads (e.g. AHB external textures on Mali).
    pub color_type: ColorType,
    pub alpha_type: AlphaType,
}

impl GpuImageInfo {
    pub fn rgba8_unpremul(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            color_type: ColorType::RGBA8888,
            alpha_type: AlphaType::Unpremul,
        }
    }
}

/// One entry in the image store.  `texture` is a raw GLuint held by
/// the engine (uploaded via PBO or AHB from the `upload_thread`).
/// Ownership is exclusive: when the refcount hits zero (driven by
/// the JS-side cache) we call `glDeleteTextures` and drop the entry.
///
/// When [`Self::atlas_origin`] is `Some`, the `gl_texture` is an
/// atlas page shared with other small images; `info.width/height`
/// still describe the *logical* image size (used by JS), while the
/// atlas page's full dimensions are given by `atlas_page_size`.
/// Draw sites offset their source rect by `atlas_origin` and treat
/// the Skia `SkImage` wrapper's dims as `atlas_page_size`.
#[derive(Clone, Copy, Debug)]
pub struct StoredImage {
    /// Raw GL texture name (GLuint).
    pub gl_texture: u32,
    pub info: GpuImageInfo,
    /// `(x, y)` of this image within the atlas page, in pixels.
    /// `None` when the texture is a dedicated per-image allocation
    /// (the legacy path).
    pub atlas_origin: Option<(u16, u16)>,
    /// Full dimensions of the atlas page texture (always square in
    /// our layout).  Only meaningful when `atlas_origin.is_some()`.
    pub atlas_page_size: u16,
}

impl StoredImage {
    /// Construct a stored image backed by its own dedicated GL
    /// texture (no atlas).  Shortcut so call sites don't have to
    /// spell out the atlas fields when they're not in use.
    #[inline]
    pub fn dedicated(gl_texture: u32, info: GpuImageInfo) -> Self {
        Self {
            gl_texture,
            info,
            atlas_origin: None,
            atlas_page_size: 0,
        }
    }
}

/// Shared image registry.  Lives on the render thread — not `Send` because
/// `glow::Context` is `!Send` on most platforms and adjacent bookkeeping
/// would cross that boundary.
///
/// `sk_image_cache` memoises the `backend_textures::make_gl` +
/// `borrow_texture_from` pair.  Each produced `SkImage` is bound to a
/// specific `DirectContext`, so the key is `(image_id,
/// DirectContextId.id())`; a second canvas (different DirectContext)
/// will lazily populate its own entry on first draw.  `SkImage` is
/// an `RCHandle`, so cloning it on cache hit is a refcount bump.
///
/// `in_flight` is the image lifetime refcount (P1-6): any
/// `FramePacket` that still carries a `DrawImage { image_id }` holds
/// a +1 reference, and `destroy_shared_image` subtracts 1 only once
/// the packet has been executed.  When JS calls
/// `destroyImage(id)` while the image is still in flight, the
/// store marks the entry "pending delete" and defers the GL
/// `glDeleteTextures` call until the refcount hits zero — without
/// this, a future `drawImage(id)` race between JS and render thread
/// could sample a freed GL name (undefined behaviour).
#[derive(Default)]
pub struct ImageStore {
    entries: HashMap<u32, StoredImage>,
    sk_image_cache: HashMap<(u32, u32), SkImage>,
    /// Per-image-id in-flight counter.  Incremented when a command
    /// carrying the id is queued into the render thread, decremented
    /// when that command finishes executing.  A zero count means no
    /// queued reference holds the texture alive — safe to actually
    /// delete.  Not all code paths pin entries (tests, offline
    /// tooling) so a missing entry is treated as zero.
    in_flight: HashMap<u32, u32>,
    /// Image ids whose destroy was requested while in flight.  The
    /// actual GL texture deletion happens in
    /// `drain_pending_deletions` once `in_flight[id]` hits zero.
    pending_delete: HashMap<u32, StoredImage>,
}

impl ImageStore {
    pub fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(32),
            sk_image_cache: HashMap::with_capacity(32),
            in_flight: HashMap::with_capacity(32),
            pending_delete: HashMap::with_capacity(8),
        }
    }

    /// Reserve a fresh image id.  Always `> 0`.
    ///
    /// Delegates to the process-global counter in
    /// `shared::image_id` so the JS thread can allocate ids without
    /// a render-thread round-trip — see `op_create_image` in
    /// `runtime-v8/src/rendering/image/mod.rs` for the JS-side caller.
    pub fn generate_id(&self) -> u32 {
        shared::image_id::next_image_id()
    }

    /// Current size of the `SkImage` wrapper cache.  Exposed for
    /// `DebugStats.sk_image_wrappers` so the debug overlay shows
    /// cache multiplication (image_count * live_gr_contexts).
    #[inline]
    pub fn wrapper_cache_len(&self) -> usize {
        self.sk_image_cache.len()
    }

    /// Images currently queued into unexecuted frame packets —
    /// exposed for diagnostics; a rising value without a matching
    /// draw-call throughput increase indicates frame-packet
    /// backlog.
    #[inline]
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Estimated bytes retained by live image payloads.  ImageStore currently
    /// accepts RGBA8 uploads, so the estimate is the logical dimensions times
    /// four; wrapper metadata is reported separately.
    #[inline]
    pub fn live_byte_estimate(&self) -> u64 {
        self.entries.values().map(Self::payload_bytes).sum()
    }

    /// Estimated payload bytes held in the deferred-delete tombstone map.
    #[inline]
    pub fn pending_delete_byte_estimate(&self) -> u64 {
        self.pending_delete.values().map(Self::payload_bytes).sum()
    }

    /// Attributed payload bytes for cached SkImage wrappers.  A wrapper borrows
    /// the underlying texture; this reports the referenced payload once per
    /// cached `(image_id, context_tag)` wrapper to expose cache multiplication.
    #[inline]
    pub fn wrapper_byte_estimate(&self) -> u64 {
        self.sk_image_cache
            .keys()
            .filter_map(|(id, _)| self.entries.get(id))
            .map(Self::payload_bytes)
            .sum()
    }

    #[inline]
    fn payload_bytes(entry: &StoredImage) -> u64 {
        u64::from(entry.info.width)
            .saturating_mul(u64::from(entry.info.height))
            .saturating_mul(4)
    }

    /// F-1: drain every `pending_delete` entry whose in-flight
    /// refcount has already reached zero.  Returns the freed
    /// entries so the caller can issue `glDeleteTextures` under
    /// the correct GL context.  Idempotent when no deletions are
    /// pending.  Used at the post-frame Present barrier so
    /// Skia's deferred command buffer is guaranteed to have
    /// submitted all references to the texture before it is
    /// freed.
    pub fn take_unreferenced_pending_delete(&mut self) -> Vec<StoredImage> {
        if self.pending_delete.is_empty() {
            return Vec::new();
        }
        let ready_ids: Vec<u32> = self
            .pending_delete
            .keys()
            .filter(|id| !self.in_flight.contains_key(id))
            .copied()
            .collect();
        let mut out = Vec::with_capacity(ready_ids.len());
        for id in ready_ids {
            if let Some(entry) = self.pending_delete.remove(&id) {
                out.push(entry);
            }
        }
        out
    }

    /// Mark `image_id` as queued into a render-thread command.  Must
    /// be paired 1:1 with [`Self::release_in_flight`] when the
    /// command executes.
    #[inline]
    pub fn retain_in_flight(&mut self, image_id: u32) {
        *self.in_flight.entry(image_id).or_insert(0) += 1;
    }

    /// Decrement the in-flight refcount.  When it hits zero and
    /// a destroy was previously deferred, the (removed) entry is
    /// returned so the caller can issue `glDeleteTextures` on its
    /// raw name.  Returning the entry rather than calling `gl`
    /// here keeps the store `!Send`-free and avoids wiring a
    /// `glow::Context` reference through every release site.
    #[must_use = "GL texture must be deleted by the caller when returned"]
    pub fn release_in_flight(&mut self, image_id: u32) -> Option<StoredImage> {
        if let Some(count) = self.in_flight.get_mut(&image_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.in_flight.remove(&image_id);
            }
        }
        if !self.in_flight.contains_key(&image_id) {
            self.pending_delete.remove(&image_id)
        } else {
            None
        }
    }

    pub fn insert(&mut self, image_id: u32, entry: StoredImage) {
        self.entries.insert(image_id, entry);
    }

    /// Remove an entry by id.  If the image is still in flight the
    /// entry is moved into `pending_delete` and `None` is returned
    /// — the actual GL texture deletion must wait until the final
    /// in-flight reference is released.  Otherwise returns the
    /// entry so the caller can delete the GL texture immediately.
    pub fn remove(&mut self, image_id: u32) -> Option<StoredImage> {
        // Evict every SkImage wrapper that pointed at this image id
        // so a future insert with the same id doesn't accidentally
        // resolve to a stale backend texture.  HashMap iteration
        // requires a Vec detour because we're mutating in place.
        let stale_keys: Vec<(u32, u32)> = self
            .sk_image_cache
            .keys()
            .filter(|(id, _)| *id == image_id)
            .copied()
            .collect();
        for k in stale_keys {
            self.sk_image_cache.remove(&k);
        }
        let entry = self.entries.remove(&image_id)?;
        if self.in_flight.contains_key(&image_id) {
            // Still referenced by an unexecuted command; defer the
            // GL deletion.  The render thread's per-frame packet
            // completion path calls `release_in_flight`, which
            // eventually returns this entry to the caller for
            // glDeleteTextures.
            self.pending_delete.insert(image_id, entry);
            None
        } else {
            Some(entry)
        }
    }

    pub fn get(&self, image_id: u32) -> Option<&StoredImage> {
        self.entries.get(&image_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&u32, &StoredImage)> {
        self.entries.iter()
    }

    /// Wrap an existing GL texture into an `SkImage` owned by `gr_ctx`,
    /// bypassing the wrapper cache.  Kept for diagnostic use and tests;
    /// production code should use [`Self::resolve_cached_or_wrap`] so
    /// repeated `drawImage` of the same texture reuses the `SkImage`
    /// handle and skips the `backend_textures::make_gl` +
    /// `borrow_texture_from` pair.
    ///
    /// The returned image borrows the texture -- it does NOT own it, and
    /// the caller must keep the underlying GL texture alive for the
    /// lifetime of the image (release on `DestroyImage`).
    pub fn resolve_as_sk_image(
        gr_ctx: &mut gpu::DirectContext,
        entry: &StoredImage,
    ) -> Option<SkImage> {
        let tex_info = TextureInfo {
            target: gles_texture_2d(),
            id: entry.gl_texture,
            format: gl_rgba8_size(),
            protected: gpu::Protected::No,
        };
        // SAFETY: `gl_texture` is a live GLuint owned by the render thread
        // (populated via `pbo_upload` or AHB on a shared GL context).  The
        // caller guarantees (via DestroyImage reference-counting on the
        // JS side) that the texture outlives any `SkImage` we hand out.
        let backend_tex = unsafe {
            backend_textures::make_gl(
                (entry.info.width as i32, entry.info.height as i32),
                Mipmapped::No,
                tex_info,
                "migo.image_store",
            )
        };
        gpu::images::borrow_texture_from(
            gr_ctx,
            &backend_tex,
            SurfaceOrigin::TopLeft,
            entry.info.color_type,
            entry.info.alpha_type,
            None,
        )
    }

    /// Fetch the `SkImage` wrapper for `image_id` against `gr_ctx`,
    /// building + caching it on first use.  Repeated calls at the
    /// same `(image_id, gr_ctx)` pair hand back a refcounted clone
    /// of the prior wrapper without touching the Ganesh backend
    /// texture machinery -- the performance-critical case for games
    /// that call `drawImage(sameSprite, ...)` hundreds of times per
    /// frame.
    ///
    /// Returns `None` when the `image_id` isn't in the store or the
    /// Ganesh wrap fails (OOM / driver issue).  Cache entries are
    /// scoped by `DirectContext::id()` so two Canvas2DContexts can
    /// independently cache their own wrappers without collisions.
    pub fn resolve_cached_or_wrap(
        &mut self,
        ctx_tag: u32,
        gr_ctx: &mut gpu::DirectContext,
        image_id: u32,
    ) -> Option<SkImage> {
        let key = (image_id, ctx_tag);
        if let Some(hit) = self.sk_image_cache.get(&key) {
            crate::render_diagnostics::hit_sk_image_wrapper();
            return Some(hit.clone());
        }
        crate::render_diagnostics::miss_sk_image_wrapper();
        let entry = self.entries.get(&image_id)?.clone();
        let wrapped = Self::resolve_as_sk_image(gr_ctx, &entry)?;
        self.sk_image_cache.insert(key, wrapped.clone());
        Some(wrapped)
    }

    /// Drop every SkImage wrapper whose DirectContext has been torn down, so a
    /// dead `GrDirectContext` is not kept alive by a stale wrapper.
    ///
    /// Five call sites: `Canvas2DContext::release_gpu_resources` in
    /// `backend/gl/surface.rs`, and four teardown paths in
    /// `canvas/manager/mod.rs` (context loss, share-group rebuild, canvas
    /// here was stale and actively misleading: an attribute saying "nothing
    /// calls this" on a function five paths depend on is an invitation to delete
    /// it, and deleting it would leak a `GrDirectContext` plus its resource
    /// cache for every destroyed canvas.
    ///
    /// Not a dangling-pointer risk either way — `SkImage_Ganesh` holds its
    /// context by refcount, so a stale wrapper pins a dead context rather than
    /// outliving it. What skipping this costs is memory, not correctness.
    pub fn purge_wrappers_for_context(&mut self, ctx_id: u32) {
        self.sk_image_cache.retain(|(_, c), _| *c != ctx_id);
    }
}

/// GLES2 / GLES3 enum value for `GL_TEXTURE_2D`.
#[inline]
fn gles_texture_2d() -> u32 {
    0x0DE1
}

/// GLES3 sized internal format `GL_RGBA8` (`0x8058`).  Matches the
/// Chromium-style RGBA8 attachments we create in `drawing_buffer.rs`.
#[inline]
fn gl_rgba8_size() -> u32 {
    0x8058
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_id_is_monotonic_nonzero() {
        let s = ImageStore::new();
        let a = s.generate_id();
        let b = s.generate_id();
        assert_ne!(a, 0);
        assert!(b > a);
    }

    #[test]
    fn insert_then_get_roundtrip() {
        let mut s = ImageStore::new();
        let id = s.generate_id();
        s.insert(
            id,
            StoredImage::dedicated(42, GpuImageInfo::rgba8_unpremul(64, 64)),
        );
        let got = s.get(id).expect("missing");
        assert_eq!(got.gl_texture, 42);
        assert_eq!(got.info.width, 64);
    }

    #[test]
    fn remove_returns_entry_once() {
        let mut s = ImageStore::new();
        let id = s.generate_id();
        s.insert(
            id,
            StoredImage::dedicated(1, GpuImageInfo::rgba8_unpremul(1, 1)),
        );
        assert!(s.remove(id).is_some());
        assert!(s.remove(id).is_none());
        assert!(s.get(id).is_none());
    }

    #[test]
    fn remove_evicts_sk_image_cache_entries_for_that_id() {
        // Regression: re-inserting the same image id with a fresh
        // backing GL texture must NOT serve a stale SkImage wrapper
        // from before the first remove().  Without the eviction in
        // `ImageStore::remove`, a `drawImage` after the re-insert
        // would render the old texture content.
        let mut s = ImageStore::new();
        let id = s.generate_id();
        s.insert(
            id,
            StoredImage::dedicated(1, GpuImageInfo::rgba8_unpremul(4, 4)),
        );
        // Populate the cache directly (can't call resolve_cached_or_wrap
        // without a live DirectContext in pure-Rust tests).
        // The key shape is what ::remove invalidates by.
        s.sk_image_cache.insert(
            (id, 0xAAAA_AAAA),
            // Invalid placeholder SkImage - this test only checks
            // the map eviction semantics, never touches the image.
            // We drop the entry via `remove`; the placeholder is
            // immediately forgotten.
            {
                // A trivial 1x1 raster SkImage works as a placeholder.
                let info =
                    skia_safe::ImageInfo::new((1, 1), ColorType::RGBA8888, AlphaType::Premul, None);
                let mut surf = skia_safe::surfaces::raster(&info, None, None).unwrap();
                surf.image_snapshot()
            },
        );
        assert_eq!(s.sk_image_cache.len(), 1);
        let _ = s.remove(id);
        assert_eq!(s.sk_image_cache.len(), 0, "remove must drop wrappers");
    }

    #[test]
    fn purge_wrappers_for_context_is_scoped() {
        // Build a two-context shape: ctx_tag A has two images, ctx B
        // has one.  Purging A's wrappers leaves B's untouched.
        let mut s = ImageStore::new();
        let info = skia_safe::ImageInfo::new((1, 1), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut mk_img = || {
            skia_safe::surfaces::raster(&info, None, None)
                .unwrap()
                .image_snapshot()
        };
        s.sk_image_cache.insert((1, 100), mk_img());
        s.sk_image_cache.insert((2, 100), mk_img());
        s.sk_image_cache.insert((3, 200), mk_img());
        s.purge_wrappers_for_context(100);
        assert_eq!(s.sk_image_cache.len(), 1);
        assert!(s.sk_image_cache.contains_key(&(3, 200)));
    }

    /// Simulate the `Canvas2DContext::resize` flow: a wrapper keyed
    /// on `(image_id, ctx_tag_old)` must no longer match lookups
    /// after we rotate `ctx_tag_old → ctx_tag_new` and purge the
    /// old tag.  Without the purge, a bug in the `(image_id, tag)`
    /// match would silently serve a wrapper bound to a destroyed
    /// `GrDirectContext`; this test locks the invariant.
    #[test]
    fn purge_then_new_ctx_tag_isolates_pre_resize_wrappers() {
        let mut s = ImageStore::new();
        let info = skia_safe::ImageInfo::new((1, 1), ColorType::RGBA8888, AlphaType::Premul, None);
        let mut mk_img = || {
            skia_safe::surfaces::raster(&info, None, None)
                .unwrap()
                .image_snapshot()
        };
        // Pre-resize: ctx_tag = 42 uses image_id = 7.
        s.sk_image_cache.insert((7, 42), mk_img());
        assert!(s.sk_image_cache.contains_key(&(7, 42)));

        // Resize flow: purge old tag, then install new tag's wrapper.
        s.purge_wrappers_for_context(42);
        assert!(s.sk_image_cache.is_empty());

        s.sk_image_cache.insert((7, 43), mk_img());
        // Old key is gone; new key is reachable.  A re-resize
        // (42 → 43 → 44) can continue the pattern without ever
        // colliding with stale wrappers.
        assert!(!s.sk_image_cache.contains_key(&(7, 42)));
        assert!(s.sk_image_cache.contains_key(&(7, 43)));
    }

    #[test]
    fn rgba8_unpremul_factory_has_expected_fields() {
        let info = GpuImageInfo::rgba8_unpremul(10, 20);
        assert_eq!(info.width, 10);
        assert_eq!(info.height, 20);
        assert_eq!(info.color_type, ColorType::RGBA8888);
        assert_eq!(info.alpha_type, AlphaType::Unpremul);
    }
    /// P2-MEM: repeated create/delete must return both metadata and retained
    /// payload accounting to baseline.  The second remove remains idempotent.
    #[test]
    fn repeated_create_delete_returns_retained_bytes_to_baseline() {
        let mut store = ImageStore::new();
        assert_eq!(store.live_byte_estimate(), 0);
        assert_eq!(store.pending_delete_byte_estimate(), 0);
        assert_eq!(store.wrapper_byte_estimate(), 0);
        for _texture in 1..=32 {
            let id = store.generate_id();
            store.insert(
                id,
                StoredImage::dedicated(1, GpuImageInfo::rgba8_unpremul(8, 4)),
            );
            assert_eq!(store.live_byte_estimate(), 1 * 8 * 4 * 4);
            assert!(store.remove(id).is_some());
            assert!(store.remove(id).is_none());
            assert_eq!(store.live_byte_estimate(), 0);
            assert_eq!(store.pending_delete_byte_estimate(), 0);
            assert_eq!(store.wrapper_byte_estimate(), 0);
            assert_eq!(store.entries.len(), 0);
        }
    }
}
