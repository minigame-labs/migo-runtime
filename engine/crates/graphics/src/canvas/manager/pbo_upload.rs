//! PBO (Pixel Buffer Object) Async Texture Upload
//!
//! This module provides optimized texture uploading using PBOs for OpenGL ES 3.0+.
//! PBOs allow async DMA transfers from CPU to GPU memory, reducing CPU stalls.
//!
//! Design considerations:
//! - Requires OpenGL ES 3.0 (PBO + `glFenceSync`); the engine's minSdk is
//!   API 26, which guarantees ES 3.0 on conforming devices. The PBO/fence
//!   entry points themselves are ES 3.0, not API 21 — an earlier version of
//!   this note said "API 21 compatible", which was misleading.
//! - Automatic fallback to synchronous upload on ES 2.0 / TierB drivers
//! - Memory-efficient: reuses PBO buffers when possible

use glow::HasContext;
use shared::{
    error::{EngineResult, ErrorCode},
    protocol::io_cmd::NormalizedImage,
};
use tracing::{debug, info, trace, warn};

use super::types::ee;

/// Configuration for PBO uploads
#[allow(dead_code)]
pub struct PboConfig {
    /// Enable PBO usage (requires ES 3.0+)
    pub enabled: bool,
    /// Maximum PBO buffer size in bytes (for reuse pool)
    pub max_buffer_size: usize,
}

impl Default for PboConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // 4MB max buffer size - enough for a 1024x1024 RGBA image
            max_buffer_size: 4 * 1024 * 1024,
        }
    }
}

/// PBO upload result
pub struct PboUploadResult {
    pub texture: glow::NativeTexture,
    pub width: u32,
    pub height: u32,
    /// Which upload path was actually taken.  Callers may accumulate
    /// per-path counters following the `PresentPassCounters` pattern
    /// to make path selection observable during a device run.
    pub path_taken: TextureUploadPath,
}

#[inline]
fn should_try_ahb_upload(
    device_available: bool,
    session_enabled: bool,
    display_available: bool,
) -> bool {
    device_available && session_enabled && display_available
}

/// Check if OpenGL ES 3.0+ is available (required for PBO)
///
/// Returns true if PBOs can be used
pub fn check_pbo_support(gl: &glow::Context) -> bool {
    unsafe {
        let version_str = gl.get_parameter_string(glow::VERSION);
        // OpenGL ES 3.x pattern
        if version_str.contains("OpenGL ES 3") {
            debug!("PBO support detected: {}", version_str);
            return true;
        }
        // Desktop OpenGL 3.0+ also supports PBO
        if version_str.starts_with("3.") || version_str.starts_with("4.") {
            debug!("PBO support detected (Desktop): {}", version_str);
            return true;
        }
        debug!("No PBO support: {}", version_str);
        false
    }
}

/// Extended version that additionally knows whether the driver supports
/// `glTexStorage2D`.  When `true`, we allocate the texture immutably up
/// front (one driver-side layout decision instead of per-level guessing)
/// and use `glTexSubImage2D` to populate; measurably faster on Mali /
/// Adreno / PowerVR in micro-benchmarks (~10-25% on first-frame upload
/// cost for mid-sized sprite atlases).  Falls back to the traditional
/// `glTexImage2D` path on allocation failure.
pub fn upload_texture_with_pbo_ext(
    gl: &glow::Context,
    image: &NormalizedImage,
    use_pbo: bool,
    pool: Option<&mut PboPool>,
    has_tex_storage: bool,
) -> EngineResult<PboUploadResult> {
    let start = std::time::Instant::now();

    // What the caller's context had bound, so it can have it back. On the upload
    // thread nobody is watching; on the render thread the WebGL dedup shadow still
    // names the content's texture and alignment, and binding *zero* here instead of
    // restoring made the content's next identical `bindTexture` / `pixelStorei`
    // look redundant — dropped, leaving the upload or sample after it pointed at no
    // texture. `compressed_upload.rs` already restores for exactly this reason;
    // these two upload paths were the ones that did not.
    let saved = unsafe {
        (
            gl.get_parameter_i32(glow::TEXTURE_BINDING_2D),
            gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
        )
    };

    // Create texture
    let tex = unsafe {
        gl.create_texture().map_err(|e| {
            ee(
                ErrorCode::RenderBackendError,
                format!("create_texture failed: {e:?}"),
            )
        })?
    };

    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));

        // Set texture parameters
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
    }

    // Choose the best upload path.  See `TextureUploadPath` for the decision
    // table; this is a one-line wrapper to keep the business logic testable.
    let path = TextureUploadPath::select(use_pbo, has_tex_storage, !image.rgba.is_empty());
    match path {
        TextureUploadPath::Ahb => unreachable!("AHB is selected by upload_texture_tiered"),
        TextureUploadPath::PboImmutable => {
            upload_immutable_with_pbo(gl, image, pool)?;
        }
        TextureUploadPath::PboMutable => {
            upload_with_pbo_pooled(gl, image, pool)?;
        }
        TextureUploadPath::Synchronous => {
            upload_sync_internal(gl, tex, image)?;
        }
    }

    unsafe {
        let (saved_texture, saved_alignment) = saved;
        gl.bind_texture(
            glow::TEXTURE_2D,
            std::num::NonZeroU32::new(saved_texture as u32).map(glow::NativeTexture),
        );
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, saved_alignment);
    }

    trace!(
        "Texture uploaded: {}x{} in {:.2?} (path={:?})",
        image.width,
        image.height,
        start.elapsed(),
        path,
    );

    Ok(PboUploadResult {
        texture: tex,
        width: image.width,
        height: image.height,
        path_taken: path,
    })
}

/// Classification of which texture-upload path the tiered uploader chooses.
/// Factored out so the decision logic has unit tests independent of any GL
/// driver; `select` covers only the PBO/synchronous subtable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureUploadPath {
    /// AHardwareBuffer → EGLImage → GL texture (zero `glTexImage2D`).
    /// Requires: `device_caps.ahb_available`, the session circuit-breaker
    /// `gpu_caps.snapshot().ahb`, and a non-null EGL display pointer.
    Ahb,
    /// `glTexStorage2D` + `glTexSubImage2D` via a PBO.  Fastest PBO path.
    PboImmutable,
    /// `glTexImage2D` via a PBO.  Async DMA transfer, driver chooses layout.
    PboMutable,
    /// `glTexImage2D` with the pixel slice directly.  Synchronous, fallback.
    Synchronous,
}

impl TextureUploadPath {
    /// Pure-function decision table.
    ///
    /// * `has_data = false` forces the synchronous path because `glTexStorage2D`
    ///   + `glTexSubImage2D(NULL)` leaves the texture uninitialised, which
    ///   would create a visible "black frame" for games that allocate then
    ///   uploading later.  The synchronous path with a zero-length slice
    ///   at least lets the driver skip the copy.
    pub fn select(use_pbo: bool, has_tex_storage: bool, has_data: bool) -> Self {
        if !has_data {
            return Self::Synchronous;
        }
        if use_pbo && has_tex_storage {
            return Self::PboImmutable;
        }
        if use_pbo {
            return Self::PboMutable;
        }
        Self::Synchronous
    }

    /// Select the complete tiered upload path without touching a GL context.
    ///
    /// AHB is selected before the PBO table because it imports the decoded
    /// buffer directly and therefore does not use any `glTexImage2D` upload.
    /// The three AHB booleans mirror `upload_texture_tiered`'s runtime gates;
    /// keeping this table pure makes the API-26 boundary and fallback choice
    /// host-testable without pretending to have an Android EGL driver.
    pub fn select_tiered(
        ahb_available: bool,
        session_ahb_enabled: bool,
        display_available: bool,
        use_pbo: bool,
        has_tex_storage: bool,
        has_data: bool,
    ) -> Self {
        // AHB allocation also requires initialized pixel contents.  Keep the
        // empty-data invariant shared with `select`: an uninitialised AHB
        // would turn an allocation-only request into a black texture.
        if !has_data {
            return Self::Synchronous;
        }
        if should_try_ahb_upload(ahb_available, session_ahb_enabled, display_available) {
            return Self::Ahb;
        }
        Self::select(use_pbo, has_tex_storage, has_data)
    }
}

/// Internal: Upload through an immutable `glTexStorage2D` allocation +
/// `glTexSubImage2D` filled via a PBO.  Fastest path on GLES 3.0+.
///
/// Caller must have already `glBindTexture`-d the destination.
fn upload_immutable_with_pbo(
    gl: &glow::Context,
    image: &NormalizedImage,
    mut pool: Option<&mut PboPool>,
) -> EngineResult<()> {
    let data_size = image.rgba.len();

    // Allocate the texture storage immutably (one big driver-side
    // decision on layout, tiling, compression …).
    // `GL_RGBA8` is 0x8058 (matches backend::gl::surface).
    unsafe {
        gl.tex_storage_2d(
            glow::TEXTURE_2D,
            /* levels */ 1,
            /* internal_format */ 0x8058,
            image.width as i32,
            image.height as i32,
        );
    }

    // Acquire or create a PBO, fill it, and copy into the texture via
    // TexSubImage2D (equivalent to the classic path but against an
    // already-allocated texture).
    let pbo = match pool.as_mut().and_then(|p| p.acquire(gl, data_size)) {
        Some(pbo) => pbo,
        None => unsafe {
            gl.create_buffer().map_err(|e| {
                ee(
                    ErrorCode::RenderBackendError,
                    format!("create_buffer (PBO) failed: {e:?}"),
                )
            })?
        },
    };

    unsafe {
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));
        gl.buffer_data_u8_slice(glow::PIXEL_UNPACK_BUFFER, &image.rgba, glow::STREAM_DRAW);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        gl.tex_sub_image_2d(
            glow::TEXTURE_2D,
            /* level */ 0,
            /* xoffset */ 0,
            /* yoffset */ 0,
            image.width as i32,
            image.height as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::BufferOffset(0),
        );
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
    }

    if let Some(p) = pool {
        p.release(gl, pbo, data_size);
    } else {
        unsafe { gl.delete_buffer(pbo) };
    }

    Ok(())
}

/// Internal: Upload using PBO with optional pool reuse
fn upload_with_pbo_pooled(
    gl: &glow::Context,
    image: &NormalizedImage,
    mut pool: Option<&mut PboPool>,
) -> EngineResult<()> {
    let data_size = image.rgba.len();

    // Try to acquire PBO from pool, otherwise create one
    let pbo = match pool.as_mut().and_then(|p| p.acquire(gl, data_size)) {
        Some(pbo) => pbo,
        None => unsafe {
            gl.create_buffer().map_err(|e| {
                ee(
                    ErrorCode::RenderBackendError,
                    format!("create_buffer (PBO) failed: {e:?}"),
                )
            })?
        },
    };

    unsafe {
        // Bind PBO as PIXEL_UNPACK_BUFFER
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));

        // Allocate and fill PBO with image data
        // Using STREAM_DRAW hint for upload-once data
        gl.buffer_data_u8_slice(glow::PIXEL_UNPACK_BUFFER, &image.rgba, glow::STREAM_DRAW);

        // Set pixel store alignment (RGBA is 4-byte aligned)
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);

        // Upload from PBO to texture (async DMA transfer)
        // When PBO is bound, the last parameter is interpreted as an offset into the PBO
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            image.width as i32,
            image.height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::BufferOffset(0),
        );

        // Unbind PBO
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
    }

    // Return PBO to pool (pool handles "full" case by deleting), or delete directly
    if let Some(p) = pool {
        p.release(gl, pbo, data_size);
    } else {
        unsafe { gl.delete_buffer(pbo) };
    }

    Ok(())
}

/// Internal: Synchronous upload (fallback for ES 2.0)
fn upload_sync_internal(
    gl: &glow::Context,
    _tex: glow::NativeTexture,
    image: &NormalizedImage,
) -> EngineResult<()> {
    unsafe {
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);

        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            image.width as i32,
            image.height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&image.rgba)),
        );

        Ok(())
    }
}

/// Reusable PBO pool for frequent uploads.
///
/// This reduces allocation overhead for games that frequently load/unload textures.
/// Each returned PBO carries a fence sync so that on Adreno GPUs (and others)
/// we do not re-use a PBO whose previous DMA transfer has not completed.
/// Whether a `glClientWaitSync` status means the previous DMA has finished.
///
/// Taken with a zero timeout here, so the only outcomes are "done"
/// (`ALREADY_SIGNALED`), "done while we asked" (`CONDITION_SATISFIED`), "not
/// done" (`TIMEOUT_EXPIRED`) and "the driver refused to answer"
/// (`WAIT_FAILED`). The last two are the same decision: do not reuse.
///
/// **One spelling, three fences.** This was a local copy of the comparison
/// `CanvasManager::drain_upload_completed` makes on upload fences, with a
/// comment noting that two spellings of "is the GPU done" would be two chances
/// to get one wrong. A third then appeared on the pre-blit snapshot fence, so it
/// is now [`super::fence_signalled`] and the copies are gone.
use super::fence_signalled as dma_complete;

/// Index of the first pool entry that can be reused *without waiting* for a
/// request of `size`, given `(entry_size, dma_complete)` per entry.
///
/// `None` means every candidate is either too small or still in flight, and the
/// caller should take a fresh buffer name rather than block. Size is a hard
/// requirement; readiness is what makes waiting unnecessary.
#[inline]
fn first_reusable(entries: &[(usize, bool)], size: usize) -> Option<usize> {
    entries
        .iter()
        .position(|(entry_size, ready)| *entry_size >= size && *ready)
}

pub struct PboPool {
    /// Available PBOs: (buffer, capacity, optional fence from last upload).
    available: Vec<(glow::NativeBuffer, usize, Option<glow::NativeFence>)>,
    /// Total driver storage reserved by idle entries in `available`.
    reserved_bytes: usize,
    /// Storage currently owned by an acquired PBO until `release`.
    in_flight_bytes: usize,
    /// Maximum pool size
    max_pool_size: usize,
    /// PBO support flag
    pbo_supported: bool,
    /// Whether GL fence sync is available (ES 3.0+, same as PBO).
    fence_supported: bool,
}

impl PboPool {
    /// Default pool size (number of PBOs to keep).
    pub const DEFAULT_POOL_SIZE: usize = 4;

    pub fn new(gl: &glow::Context, max_pool_size: usize) -> Self {
        let pbo_supported = check_pbo_support(gl);
        // Fence sync requires the same ES 3.0 context as PBOs.
        let fence_supported = pbo_supported;
        Self {
            available: Vec::with_capacity(max_pool_size),
            reserved_bytes: 0,
            in_flight_bytes: 0,
            max_pool_size: max_pool_size.max(1).min(Self::DEFAULT_POOL_SIZE * 2),
            pbo_supported,
            fence_supported,
        }
    }

    /// Bytes reserved by idle PBO entries (not just entry count).
    pub fn reserved_bytes(&self) -> usize {
        self.reserved_bytes
    }

    /// Bytes held by PBOs currently acquired by an upload.
    pub fn in_flight_bytes(&self) -> usize {
        self.in_flight_bytes
    }

    /// Check if PBOs are supported
    pub fn is_pbo_supported(&self) -> bool {
        self.pbo_supported
    }

    /// Get a PBO of at least the specified size.
    ///
    /// If the PBO carries a fence from a previous upload, waits (with a short
    /// timeout) for the DMA transfer to complete before returning it.  This
    /// prevents GPU stalls on Adreno and similar drivers where reusing a PBO
    /// whose transfer is still in flight causes the driver to block.
    ///
    /// # Why this probes instead of waiting
    ///
    /// This used to `glClientWaitSync` for up to 5 ms on the render thread, and
    /// on timeout delete the buffer and create a fresh one. **The wait was
    /// strictly dominated, and settling that needs no hardware.**
    ///
    /// The two things that decide it:
    ///
    /// 1. **Every write to a PBO from this pool is a full respecify.** All four
    ///    sites — [`upload_pbo_mutable`], [`upload_pbo_immutable`], and the two
    ///    WebGL paths in `renderergl::handler` — call
    ///    `buffer_data_u8_slice(PIXEL_UNPACK_BUFFER, .., STREAM_DRAW)` over the
    ///    whole buffer; none uses `buffer_sub_data` or a mapping. A conforming
    ///    GLES 3.0 driver orphans the previous storage on that call (§2.9),
    ///    which is why `STREAM_DRAW` plus a full `glBufferData` is the
    ///    canonical stall-free streaming idiom. This engine's other PBO pool
    ///    relies on exactly that, and keeps no fences at all — see
    ///    [`crate::upload_thread`].
    ///
    /// 2. **The old timeout path already did the safe thing, 5 ms late.** It
    ///    fell back to a fresh buffer name, whose storage the caller's
    ///    `glBufferData` allocates. A fresh buffer cannot be in flight, so it
    ///    is safe whether or not the driver honours the orphan.
    ///
    /// Put together: if the driver orphans, the wait was pure waste; if it
    /// stalls instead — the Adreno behaviour the previous comment described —
    /// the answer is still not to wait, because the fresh buffer that the
    /// timeout eventually produced was available immediately. Either way there
    /// is no state of the world in which blocking the render thread helps.
    ///
    /// So the fence survives, demoted to a zero-timeout *probe*: it tells us
    /// whether a warm buffer can be reused right now, and when none can we take
    /// a fresh name instead of waiting for one. Worst case is now one
    /// `glGenBuffers` rather than 5 ms of a 16.67 ms budget, and the two pools
    /// no longer implement opposite policies for the same access pattern.
    ///
    /// The decision rests on two predicates, both pure and both exhaustively
    /// tested without a GL context: [`dma_complete`] and [`first_reusable`].
    pub fn acquire(&mut self, gl: &glow::Context, size: usize) -> Option<glow::NativeBuffer> {
        if !self.pbo_supported {
            return None;
        }

        // Probe, never wait.  A fresh name is safe when every warm candidate
        // is still in flight, so the render thread never blocks on DMA.
        let view: smallvec::SmallVec<[(usize, bool); 8]> = self
            .available
            .iter()
            .map(|(_, entry_size, fence)| {
                let ready = match fence {
                    None => true,
                    Some(f) => dma_complete(unsafe { gl.client_wait_sync(*f, 0, 0) }),
                };
                (*entry_size, ready)
            })
            .collect();

        if let Some(idx) = first_reusable(&view, size) {
            let (pbo, entry_size, fence) = self.available.remove(idx);
            self.reserved_bytes = self.reserved_bytes.saturating_sub(entry_size);
            // Track requested transfer bytes so release(size) balances even
            // when a larger historical PBO is reused.
            self.in_flight_bytes = self.in_flight_bytes.saturating_add(size);
            if let Some(f) = fence {
                unsafe { gl.delete_sync(f) };
            }
            return Some(pbo);
        }

        let pbo = unsafe { gl.create_buffer().ok() };
        if pbo.is_some() {
            self.in_flight_bytes = self.in_flight_bytes.saturating_add(size);
        }
        pbo
    }

    /// Return a PBO to the pool, inserting a fence so the next `acquire`
    /// can probe the in-flight DMA without blocking.
    pub fn release(&mut self, gl: &glow::Context, pbo: glow::NativeBuffer, size: usize) {
        self.in_flight_bytes = self.in_flight_bytes.saturating_sub(size);
        if self.available.len() < self.max_pool_size {
            let fence = if self.fence_supported {
                unsafe { gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0).ok() }
            } else {
                None
            };
            self.available.push((pbo, size, fence));
            self.reserved_bytes = self.reserved_bytes.saturating_add(size);
            self.available.sort_by_key(|(_, s, _)| *s);
        } else {
            unsafe { gl.delete_buffer(pbo) };
        }
    }

    /// Delete all idle names and reset the byte ledger.
    pub fn clear(&mut self, gl: &glow::Context) {
        for (pbo, _, fence) in self.available.drain(..) {
            unsafe {
                if let Some(f) = fence {
                    gl.delete_sync(f);
                }
                gl.delete_buffer(pbo);
            }
        }
        self.reserved_bytes = 0;
        self.in_flight_bytes = 0;
    }
}
impl Drop for PboPool {
    fn drop(&mut self) {
        if !self.available.is_empty() || self.reserved_bytes != 0 || self.in_flight_bytes != 0 {
            warn!(
                "PboPool dropped with {} available PBOs, {} reserved bytes, {} in-flight bytes",
                self.available.len(),
                self.reserved_bytes,
                self.in_flight_bytes,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// AHardwareBuffer import path (API 26+, TierA)
// ---------------------------------------------------------------------------

/// Upload a decoded image using the best available path for the device tier.
///
/// - **TierA + API 26+**: AHardwareBuffer → EGLImage → GL texture (zero `glTexImage2D`).
/// - **TierA / TierB**: PBO async upload (current path).
/// - **ES 2.0 fallback**: synchronous `glTexImage2D`.
///
/// `egl_display_ptr` is the raw `EGLDisplay` pointer, needed only for AHB import.
/// Pass `std::ptr::null()` if AHB is not available.
pub fn upload_texture_tiered(
    gl: &glow::Context,
    image: &NormalizedImage,
    use_pbo: bool,
    pool: Option<&mut PboPool>,
    device_caps: &crate::device_caps::DeviceCapabilities,
    gpu_caps: &shared::device::gpu_caps::GpuCaps,
    egl_display_ptr: *const std::ffi::c_void,
) -> EngineResult<PboUploadResult> {
    let selected_path = TextureUploadPath::select_tiered(
        device_caps.ahb_available,
        gpu_caps.snapshot().ahb,
        !egl_display_ptr.is_null(),
        use_pbo,
        device_caps.gles_version >= (3, 0),
        !image.rgba.is_empty(),
    );

    // AHB path: decode RGBA → AHardwareBuffer → EGLImage → GL texture.
    #[cfg(target_os = "android")]
    if selected_path == TextureUploadPath::Ahb {
        match try_ahb_upload(gl, image, egl_display_ptr) {
            Ok(result) => return Ok(result),
            Err(e) => {
                if gpu_caps.disable_ahb() {
                    shared::stats::io_metrics_global().record_ahb_fallback(
                        shared::stats::AhbFallbackReason::HardwareBufferUnavailable,
                    );
                    shared::warn_once!(
                        "Legacy RGBA-to-AHB upload failed; disabling AHB for this host session ({e})"
                    );
                }
                debug!("AHB upload failed, falling back to PBO: {e}");
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    let _ = selected_path; // selection is still unit-tested off-Android

    // Fallback: immutable PBO, mutable PBO, or synchronous upload.
    let has_tex_storage = device_caps.gles_version >= (3, 0);
    let result = upload_texture_with_pbo_ext(gl, image, use_pbo, pool, has_tex_storage)?;
    info!(
        "Texture upload path taken: {:?} ({}x{})",
        result.path_taken, result.width, result.height,
    );
    Ok(result)
}

#[cfg(target_os = "android")]
fn try_ahb_upload(
    gl: &glow::Context,
    image: &NormalizedImage,
    egl_display_ptr: *const std::ffi::c_void,
) -> Result<PboUploadResult, String> {
    use shared::protocol::ahb::{AhbDesc, OwnedAhb, write_rgba_into_ahb};

    // Legacy RGBA path: allocate an AHB here and memcpy the RGBA
    // payload in. Kept so `upload_texture_tiered` still has a useful
    // AHB fallback for non-Android decoders or cached-RGBA hits.  The
    // zero-memcpy path — decoder writes straight into AHB — lives in
    // [`upload_ahb_image`]; `CanvasCmd::LoadImage` routes to it when
    // the caller has a [`DecodedImage::HardwareBuffer`] already.
    let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(image.width, image.height))
        .map_err(|e| format!("AHB allocate: {e}"))?;
    write_rgba_into_ahb(&ahb, &image.rgba).map_err(|e| format!("AHB write: {e}"))?;

    let result = unsafe {
        crate::texture_import::import_ahb_as_texture(
            gl,
            ahb.raw(),
            egl_display_ptr,
            image.width,
            image.height,
        )
    }
    .map_err(|e| format!("{e}"))?;

    info!(
        "Texture upload path selected: Ahb ({}x{})",
        result.width, result.height
    );
    Ok(PboUploadResult {
        texture: result.texture,
        width: result.width,
        height: result.height,
        path_taken: TextureUploadPath::Ahb,
    })
    // ahb dropped here → AHardwareBuffer_release (EGLImage holds its own ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Path selection is a pure function — exhaustive decision-table tests
    // below run without any GL context.  Full-stack upload tests require a
    // GL context and live in the on-device integration suite.

    // ── PBO reuse: probe, never wait ─────────────────────────────────────
    //
    // `acquire` used to block the render thread for up to 5 ms on a fence and,
    // on timeout, throw the buffer away and create a fresh one. The wait was
    // strictly dominated — see `PboPool::acquire` — and these pin the two
    // predicates the replacement rests on.

    /// Every status `glClientWaitSync` can return, enumerated, for the one
    /// predicate all three fence sites now share.
    ///
    /// `TIMEOUT_EXPIRED` and `WAIT_FAILED` are the same decision — the GPU has
    /// not passed the fence — but for different reasons, and each site pays a
    /// different price for admitting either:
    ///
    /// * `PboPool::acquire` would hand out a buffer whose DMA is still reading
    ///   it, so the next upload overwrites pixels in flight.
    /// * `drain_upload_completed` would register a texture whose upload has not
    ///   landed, so a draw samples undefined contents.
    /// * `snapshot_canvas2d_region` would blit pre-draw tiles, which is the
    ///   blank-text-label defect its fence exists to prevent.
    ///
    /// None of the three produces a GL error, which is why the predicate is
    /// asserted here rather than trusted.
    #[test]
    fn only_a_signalled_fence_means_the_gpu_is_done() {
        assert!(dma_complete(glow::ALREADY_SIGNALED));
        assert!(dma_complete(glow::CONDITION_SATISFIED));
        assert!(
            !dma_complete(glow::TIMEOUT_EXPIRED),
            "an unfinished fence was reported as passed"
        );
        assert!(
            !dma_complete(glow::WAIT_FAILED),
            "a driver that refused to answer was read as a yes"
        );

        // The four are distinct values, so the predicate is discriminating
        // rather than accidentally right about a pair that happens to collide.
        let all = [
            glow::ALREADY_SIGNALED,
            glow::CONDITION_SATISFIED,
            glow::TIMEOUT_EXPIRED,
            glow::WAIT_FAILED,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two wait statuses share a value");
            }
        }

        // And nothing outside the enumerated set is admitted — a driver
        // returning something undocumented must not read as success.
        for bogus in [0u32, 1, 0xFFFF_FFFF] {
            if !all.contains(&bogus) {
                assert!(
                    !dma_complete(bogus),
                    "undocumented status 0x{bogus:08X} was read as a passed fence"
                );
            }
        }
    }

    #[test]
    fn an_empty_pool_has_nothing_to_reuse() {
        assert_eq!(first_reusable(&[], 1024), None);
    }

    #[test]
    fn a_ready_entry_of_sufficient_size_is_reused() {
        assert_eq!(first_reusable(&[(2048, true)], 1024), Some(0));
        assert_eq!(first_reusable(&[(1024, true)], 1024), Some(0));
    }

    /// Size is a hard requirement: a too-small buffer would make the caller's
    /// `glBufferData` reallocate anyway, which is what taking a fresh name
    /// already does without disturbing the pool.
    #[test]
    fn an_entry_smaller_than_the_request_is_never_reused() {
        assert_eq!(first_reusable(&[(512, true)], 1024), None);
    }

    /// The whole point: an in-flight entry is skipped rather than waited on.
    #[test]
    fn an_entry_still_in_flight_is_skipped_not_waited_for() {
        assert_eq!(
            first_reusable(&[(4096, false)], 1024),
            None,
            "an in-flight buffer was selected, so `acquire` would hand out a \
             buffer whose previous DMA has not finished"
        );
    }

    /// Scanning past an in-flight entry is what keeps a warm pool useful: one
    /// slow upload must not force every later acquire onto a fresh buffer.
    #[test]
    fn the_scan_looks_past_an_in_flight_entry_to_a_ready_one() {
        assert_eq!(
            first_reusable(&[(4096, false), (2048, true)], 1024),
            Some(1)
        );
    }

    /// And past a too-small one, in either order — a pool sorted by size puts
    /// the small entries first.
    #[test]
    fn the_scan_looks_past_a_too_small_entry_to_a_ready_one() {
        assert_eq!(first_reusable(&[(256, true), (2048, true)], 1024), Some(1));
        assert_eq!(
            first_reusable(&[(256, true), (512, false), (2048, true)], 1024),
            Some(2)
        );
    }

    /// A pool where nothing qualifies must report so rather than settle for a
    /// buffer that fails either requirement.
    #[test]
    fn a_pool_with_no_qualifying_entry_reports_none() {
        assert_eq!(
            first_reusable(&[(256, true), (4096, false), (512, true)], 1024),
            None
        );
    }

    /// Ties go to the earliest entry. `release` keeps the pool sorted by size,
    /// so first-fit is also best-fit, and taking the smallest sufficient buffer
    /// leaves the large ones for the requests that need them.
    #[test]
    fn first_fit_over_a_size_sorted_pool_is_best_fit() {
        assert_eq!(
            first_reusable(&[(1024, true), (2048, true), (4096, true)], 1024),
            Some(0)
        );
        assert_eq!(
            first_reusable(&[(1024, true), (2048, true), (4096, true)], 2000),
            Some(1)
        );
    }

    #[test]
    fn legacy_ahb_upload_obeys_the_session_circuit_breaker() {
        assert!(should_try_ahb_upload(true, true, true));
        assert!(!should_try_ahb_upload(false, true, true));
        assert!(!should_try_ahb_upload(true, false, true));
        assert!(!should_try_ahb_upload(true, true, false));
    }

    #[test]
    fn select_path_prefers_immutable_when_everything_available() {
        assert_eq!(
            TextureUploadPath::select(true, true, true),
            TextureUploadPath::PboImmutable
        );
    }

    #[test]
    fn select_path_falls_back_to_mutable_pbo_when_no_tex_storage() {
        assert_eq!(
            TextureUploadPath::select(true, false, true),
            TextureUploadPath::PboMutable
        );
    }

    #[test]
    fn select_path_uses_sync_when_pbo_unavailable() {
        assert_eq!(
            TextureUploadPath::select(false, true, true),
            TextureUploadPath::Synchronous
        );
        assert_eq!(
            TextureUploadPath::select(false, false, true),
            TextureUploadPath::Synchronous
        );
    }

    #[test]
    fn select_path_uses_sync_when_data_is_empty() {
        // Even on the fanciest GLES 3.0 driver: no data ⇒ no DMA, so
        // synchronous is both correct and allocation-free.
        for use_pbo in [false, true] {
            for has_ts in [false, true] {
                assert_eq!(
                    TextureUploadPath::select(use_pbo, has_ts, false),
                    TextureUploadPath::Synchronous,
                    "use_pbo={use_pbo} has_ts={has_ts}",
                );
            }
        }
    }

    #[test]
    fn select_tiered_uses_ahb_at_api_26_capability_boundary() {
        // `ahb_available` is the detected capability from
        // `DeviceCapabilities::detect`: API 26+ plus both EGL/GL image
        // extensions.  Host tests represent API 26 and API 25 by the
        // resulting capability bit, without fabricating an Android API query.
        assert_eq!(
            TextureUploadPath::select_tiered(true, true, true, true, true, true),
            TextureUploadPath::Ahb,
            "API 26+ AHB capability must win over the PBO table"
        );
        assert_eq!(
            TextureUploadPath::select_tiered(false, true, true, true, true, true),
            TextureUploadPath::PboImmutable,
            "API 25 / unavailable AHB must use the PBO fallback"
        );
        assert_eq!(
            TextureUploadPath::select_tiered(false, true, true, true, false, true),
            TextureUploadPath::PboMutable,
            "without immutable storage the fallback must use mutable PBO upload"
        );
    }

    #[test]
    fn select_tiered_uses_sync_for_empty_data_even_when_ahb_is_available() {
        assert_eq!(
            TextureUploadPath::select_tiered(true, true, true, true, true, false),
            TextureUploadPath::Synchronous,
            "an empty image must not take an uninitialised AHB upload path"
        );
    }

    #[test]
    fn select_path_is_deterministic_across_identical_inputs() {
        // Simple sanity check: same inputs always yield the same path,
        // i.e. the function is pure.
        for _ in 0..10 {
            assert_eq!(
                TextureUploadPath::select(true, true, true),
                TextureUploadPath::PboImmutable
            );
        }
    }
}
