//! Per-manager image registry: texture uploads + lifetime tracking.
//!
//! This is a thin wrapper around
//! [`crate::backend::gl::image_store::ImageStore`] that plugs into the
//! manager-level upload pipeline (PBO async, AHB, compressed).  It owns
//! *only* raw GL textures; `SkImage`s are constructed on demand by
//! callers that have access to a `GrDirectContext`.
//!
//! The femtovg-era per-canvas replica bookkeeping
//! (`fv_images: HashMap<image_id, HashMap<CanvasId, …>>`) has been
//! removed: Skia can wrap a shared texture into a context-specific
//! `SkImage` without copying, so per-canvas replicas were busywork.

use glow::HasContext;
use shared::{
    error::EngineResult,
    protocol::{
        io_cmd::{AhbImage, NormalizedImage},
        render_cmd::CanvasId,
    },
};

use crate::backend::gl::image_store::{GpuImageInfo, ImageStore, StoredImage};

use super::pbo_upload::{self, PboPool};

pub(super) struct ImageRegistry {
    store: ImageStore,
    pbo_pool: Option<PboPool>,
    use_pbo: bool,
    /// AHB owners kept alive while their derived GL texture is in
    /// the store.  Dropped on `destroy_shared_image`.  Android only
    /// — the zero-copy path only fires there; off-Android we run a
    /// mock AHB that downgrades to RGBA before upload, so nothing
    /// to retain.
    #[cfg(target_os = "android")]
    alive_ahbs: std::collections::HashMap<u32, shared::protocol::ahb::OwnedAhb>,
}

impl ImageRegistry {
    pub fn new() -> Self {
        Self {
            store: ImageStore::new(),
            pbo_pool: None,
            use_pbo: true,
            #[cfg(target_os = "android")]
            alive_ahbs: std::collections::HashMap::new(),
        }
    }

    /// Initialize PBO pool on first use (once a GL context is current).
    fn ensure_pbo_pool(&mut self, gl: &glow::Context) {
        if self.pbo_pool.is_none() {
            let pool = PboPool::new(gl, PboPool::DEFAULT_POOL_SIZE);
            self.use_pbo = pool.is_pbo_supported();
            self.pbo_pool = Some(pool);
            tracing::debug!("PBO upload initialized: enabled={}", self.use_pbo);
        }
    }

    pub fn load_shared_image(
        &mut self,
        gl: &glow::Context,
        image_id: u32,
        image: NormalizedImage,
        device_caps: &crate::device_caps::DeviceCapabilities,
        gpu_caps: &shared::device::gpu_caps::GpuCaps,
        egl_display_ptr: *const std::ffi::c_void,
    ) -> EngineResult<(u32, u32)> {
        self.ensure_pbo_pool(gl);

        let result = pbo_upload::upload_texture_tiered(
            gl,
            &image,
            self.use_pbo,
            self.pbo_pool.as_mut(),
            device_caps,
            gpu_caps,
            egl_display_ptr,
        )?;

        self.store.insert(
            image_id,
            StoredImage {
                gl_texture: result.texture.0.get(),
                info: GpuImageInfo::rgba8_unpremul(result.width, result.height),
                atlas_origin: None,
                atlas_page_size: 0,
            },
        );
        Ok((result.width, result.height))
    }

    /// Zero-copy upload: the decoder already wrote into an AHB, so
    /// there is no CPU-side RGBA `Vec` to stage. We hand the AHB
    /// straight to `eglCreateImageKHR` → `glEGLImageTargetTexture2DOES`.
    ///
    /// Falls back to [`Self::load_shared_image`] in two cases:
    ///   1. Device doesn't advertise AHB+`GL_OES_EGL_image` support
    ///      (shouldn't happen on API 26+ Android, but we don't trust
    ///      vendor drivers blindly).
    ///   2. EGL display pointer is null — e.g. test harness, headless
    ///      renderer.
    ///
    /// Both fallbacks go through `into_rgba()` which locks the AHB
    /// for CPU read and copies once into a plain `NormalizedImage`.
    /// Normal capability rejection is prevented before decode; an unexpected
    /// runtime import rejection pays this round trip once and then disables AHB
    /// for the host session.
    pub fn load_ahb_image(
        &mut self,
        gl: &glow::Context,
        image_id: u32,
        ahb_image: AhbImage,
        device_caps: &crate::device_caps::DeviceCapabilities,
        gpu_caps: &shared::device::gpu_caps::GpuCaps,
        egl_display_ptr: *const std::ffi::c_void,
    ) -> EngineResult<(u32, u32)> {
        // Guard conditions: if the device lacks AHB support or we
        // have no display, downgrade and re-enter the legacy path.
        if !device_caps.ahb_available || !gpu_caps.snapshot().ahb || egl_display_ptr.is_null() {
            let rgba =
                shared::protocol::io_cmd::DecodedImage::HardwareBuffer(ahb_image).into_rgba()?;
            return self.load_shared_image(
                gl,
                image_id,
                rgba,
                device_caps,
                gpu_caps,
                egl_display_ptr,
            );
        }

        // Zero-copy GPU import. The `OwnedAhb` held by `AhbImage`
        // lives on a refcount; `texture_import` creates its own
        // EGLImage ref while we retain the Rust-side owner, so the
        // AHB stays alive until Drop.
        #[cfg(target_os = "android")]
        {
            let import_result = unsafe {
                crate::texture_import::import_ahb_as_texture(
                    gl,
                    ahb_image.ahb.raw(),
                    egl_display_ptr,
                    ahb_image.width,
                    ahb_image.height,
                )
            };

            let result = match import_result {
                Ok(r) => r,
                Err(e) => {
                    // Graceful degradation: the zero-copy path
                    // failed (driver quirk, missing proc address,
                    // EGLImage creation refused, …) so fall
                    // through to the CPU-staged RGBA → PBO path.
                    //
                    // Before the fix this returned `Err`, which
                    // caused `op_load_image` to resolve with an
                    // error and left `image_id` without a GL
                    // texture — rendering as black in
                    // `drawImage`.  That matched the "images go
                    // black" symptom in the P0 audit report.
                    //
                    // Once-only warn.  A driver that rejects
                    // AHB once will reject it every image; spamming
                    // the log at 30+ images per screen turned the
                    // logcat into a 10kB/s firehose of identical
                    // messages.  Downgrade subsequent entries to
                    // `debug!` so the event is still captured at
                    // higher verbosity without the CPU / log-size
                    // tax at info level.
                    if gpu_caps.disable_ahb() {
                        shared::stats::io_metrics_global().record_ahb_fallback(
                            shared::stats::AhbFallbackReason::HardwareBufferUnavailable,
                        );
                    }
                    shared::warn_once!(
                        "AHB EGLImage import failed for image_id={image_id}; falling back to RGBA+PBO ({e}). \
                         AHB decode is disabled for this host session."
                    );
                    tracing::debug!(
                        image_id,
                        error = %e,
                        "AHB fallback (debug-level; warn was fired once earlier)",
                    );
                    let rgba = shared::protocol::io_cmd::DecodedImage::HardwareBuffer(ahb_image)
                        .into_rgba()?;
                    return self.load_shared_image(
                        gl,
                        image_id,
                        rgba,
                        device_caps,
                        gpu_caps,
                        egl_display_ptr,
                    );
                }
            };

            self.store.insert(
                image_id,
                StoredImage {
                    gl_texture: result.texture.0.get(),
                    info: GpuImageInfo::rgba8_unpremul(result.width, result.height),
                    atlas_origin: None,
                    atlas_page_size: 0,
                },
            );
            // Keep the AHB alive for at least as long as the GL
            // texture lifetime. `import_ahb_as_texture` already
            // stores an eglImage handle inside the driver, but the
            // underlying AHB must not be released while that EGLImage
            // exists.  Stash the owner here; destruction path drops
            // it when the image_id is evicted.
            self.retain_ahb_for(image_id, ahb_image.ahb);
            Ok((result.width, result.height))
        }
        #[cfg(not(target_os = "android"))]
        {
            // Non-Android hosts do not have a real EGL/AHB import path;
            // the AHB is a mock `Vec<u8>` that we downgrade to CPU
            // RGBA just like the fallback above.
            let rgba =
                shared::protocol::io_cmd::DecodedImage::HardwareBuffer(ahb_image).into_rgba()?;
            self.load_shared_image(gl, image_id, rgba, device_caps, gpu_caps, egl_display_ptr)
        }
    }

    /// Hold a reference to an AHB keyed by image_id so it isn't
    /// released while the GL texture derived from it is live. The
    /// EGLImage inside the driver keeps its own handle to the AHB,
    /// but from the Rust side we must guarantee the `OwnedAhb`
    /// wrapper doesn't drop first — which would decrement the Rust
    /// refcount and potentially release the buffer.
    #[cfg(target_os = "android")]
    fn retain_ahb_for(&mut self, image_id: u32, ahb: shared::protocol::ahb::OwnedAhb) {
        self.alive_ahbs.insert(image_id, ahb);
    }

    /// Destroy a shared image — deletes its GL texture (if present) and
    /// removes it from the registry.
    pub fn destroy_shared_image(&mut self, gl: &glow::Context, image_id: u32) -> EngineResult<()> {
        if let Some(entry) = self.store.remove(image_id) {
            // SAFETY: we just removed the entry so no other live reference
            // to this texture exists in the registry.  Raw GL deletion.
            if let Some(tex) = glow::NativeTexture::try_from_raw(entry.gl_texture) {
                unsafe { gl.delete_texture(tex) };
            }
        }
        // Drop any retained AHB owner for this image_id. The EGLImage
        // held by the driver was torn down with the GL texture above,
        // so releasing our `OwnedAhb` here is the last refcount.
        #[cfg(target_os = "android")]
        {
            self.alive_ahbs.remove(&image_id);
        }
        Ok(())
    }

    /// Register a pre-uploaded texture (invoked by the upload thread).
    pub fn register_shared_texture(
        &mut self,
        image_id: u32,
        texture: glow::NativeTexture,
        info: GpuImageInfo,
    ) {
        self.store.insert(
            image_id,
            StoredImage {
                gl_texture: texture.0.get(),
                info,
                atlas_origin: None,
                atlas_page_size: 0,
            },
        );
    }

    pub fn get_shared_texture(&self, image_id: u32) -> Option<StoredImage> {
        self.store.get(image_id).copied()
    }

    /// Current `SkImage` wrapper cache size.  Snapshotted into
    /// `DebugStats.sk_image_wrappers` so the overlay visualises
    /// per-`GrDirectContext` duplication.
    #[inline]
    pub fn wrapper_cache_len(&self) -> usize {
        self.store.wrapper_cache_len()
    }

    /// Mutable variant of [`Self::store`] for call sites that need to
    /// populate/update the SkImage wrapper cache (per-DirectContext).
    pub fn store_mut(&mut self) -> &mut ImageStore {
        &mut self.store
    }

    /// Initialize the PBO pool (public entry for CanvasManager).
    pub fn ensure_pbo_pool_public(&mut self, gl: &glow::Context) {
        self.ensure_pbo_pool(gl);
    }

    /// Mutable access to the PBO pool for WebGL texture uploads.
    pub fn pbo_pool_mut(&mut self) -> Option<&mut PboPool> {
        self.pbo_pool.as_mut()
    }

    /// No-op: Skia-era registry keeps no per-canvas state.  Kept for
    /// source-compatibility with older call sites (which will be deleted
    /// once the transition is complete).
    pub fn remove_canvas_images(&mut self, _canvas_id: CanvasId) {}

    /// Destroy all registered textures.  Called on manager shutdown.
    pub fn destroy_all(&mut self, gl: &glow::Context) {
        // Snapshot ids first so we can mutate the store inside the loop.
        let ids: Vec<u32> = self.store.iter().map(|(k, _)| *k).collect();
        for id in ids {
            if let Some(entry) = self.store.remove(id) {
                if let Some(tex) = glow::NativeTexture::try_from_raw(entry.gl_texture) {
                    unsafe { gl.delete_texture(tex) };
                }
            }
        }
        if let Some(ref mut pool) = self.pbo_pool {
            pool.clear(gl);
        }
    }
}

/// Extension-method compatibility: glow's `NativeTexture` does not have a
/// `try_from_raw` in 0.17, so we roll one here.  The texture was created
/// by `glGenTextures`, so any non-zero `u32` round-trips safely.
trait NativeTextureFromRaw {
    fn try_from_raw(raw: u32) -> Option<glow::NativeTexture>;
}
impl NativeTextureFromRaw for glow::NativeTexture {
    #[inline]
    fn try_from_raw(raw: u32) -> Option<glow::NativeTexture> {
        std::num::NonZeroU32::new(raw).map(glow::NativeTexture)
    }
}
