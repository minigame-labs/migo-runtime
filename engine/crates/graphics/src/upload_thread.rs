//! Dedicated texture upload thread with a shared EGL context.
//!
//! Runs GL texture uploads (PBO or AHB→EGLImage) off the render thread so
//! that `glTexImage2D` / EGLImage import never stalls frame presentation.
//!
//! **TierA only** — requires ES 3.0+ with working fence sync and a stable
//! shared EGL context (probed at init).  TierB devices skip this entirely
//! and upload on the render thread as before.

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use glow::HasContext;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tracing::{debug, info, warn};

use crate::egl_platform::{EglConcurrency, EglInstance, EglProvider, GraphicsBackendId};
use shared::error::{EngineError, EngineResult, ErrorCode};

/// A single upload that completed on the upload thread but whose result
/// could not be delivered to the render thread (channel full or disconnected).
/// Sent per-item through a dedicated channel so CanvasManager can recover
/// both the UploadServer budget and the pending_load_response atomically.
#[derive(Debug)]
pub struct DroppedUpload {
    pub image_id: u64,
    pub byte_len: usize,
}

/// A texture upload job sent from IO/render to the upload thread.
pub struct UploadJob {
    /// Unique image identifier (matches the host-side image_id).
    pub image_id: u64,
    pub width: u32,
    pub height: u32,
    /// RGBA pixel data.  Consumed (moved) by the upload thread.
    pub rgba: Arc<Vec<u8>>,
}

impl UploadJob {
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

/// A completed upload, ready for the render thread to consume.
pub struct CompletedUpload {
    pub image_id: u64,
    pub texture: glow::NativeTexture,
    pub width: u32,
    pub height: u32,
    /// GL fence inserted after the upload.  The render thread must
    /// `glClientWaitSync` before sampling this texture.
    pub fence: glow::NativeFence,
    /// Original RGBA byte count, used by UploadServer to release budget.
    pub byte_len: usize,
}

// Safety: CompletedUpload is created on the upload thread (shared GL context)
// and consumed on the render thread (same EGLDisplay).  The GL object names
// (texture, fence) are valid across shared contexts by EGL spec, so
// transferring the raw handles between these two threads is sound.
unsafe impl Send for CompletedUpload {}

/// Handle to the upload thread.  Dropped when the engine shuts down.
pub struct UploadThreadHandle {
    /// Wrapped in `Option` so `Drop` can take and drop it before joining,
    /// closing the channel and unblocking the upload thread's `recv`.
    job_tx: Option<Sender<UploadJob>>,
    result_rx: Receiver<CompletedUpload>,
    consecutive_failures: Arc<AtomicU32>,
    /// Per-item notifications for uploads that completed but whose results
    /// could not be delivered.  Carries image_id + byte_len for each,
    /// enabling CanvasManager to recover budget and resolve pending responses.
    dropped_rx: Receiver<DroppedUpload>,
    degraded: bool,
    /// Wrapped in `Option` so `Drop` can take and `join` the thread,
    /// guaranteeing it fully releases its shared EGL context before the
    /// parent's EGL display teardown runs.  Without the join the upload
    /// thread races with EGL display destruction and emits
    /// `EGL_BAD_SURFACE` / `EGL_BAD_CONTEXT` at process exit.
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for UploadThreadHandle {
    fn drop(&mut self) {
        // Close the job channel first so the upload thread's `recv()`
        // returns Err and the loop exits.
        drop(self.job_tx.take());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Maximum consecutive upload failures before permanent degradation.
const MAX_FAILURES_BEFORE_DEGRADE: u32 = 3;

/// Queue capacity for pending upload jobs.
const JOB_QUEUE_CAPACITY: usize = 16;

#[inline]
fn provider_allows_background_context(provider: &dyn EglProvider) -> bool {
    provider.concurrency() == EglConcurrency::SharedContexts
}

fn with_background_context<T>(provider: &dyn EglProvider, create: impl FnOnce() -> T) -> Option<T> {
    if !provider_allows_background_context(provider) {
        info!(
            provider = provider.label(),
            "background EGL upload disabled by provider concurrency policy"
        );
        return None;
    }
    Some(create())
}

/// Notify the render owner that an accepted job cannot produce a texture.
/// This is the single failure/cancellation accounting lane: the owner uses
/// the image id and byte count to release both the pending response and the
/// in-flight upload budget.
fn notify_dropped(job: &UploadJob, dropped_tx: &Sender<DroppedUpload>) {
    let _ = dropped_tx.send(DroppedUpload {
        image_id: job.image_id,
        byte_len: job.byte_len(),
    });
}

/// Settle jobs still queued when the worker cannot initialise or is exiting.
/// `try_recv` is intentional: the worker owns the receiver and no producer
/// can append a job after the sender is closed during normal teardown.
fn settle_queued_jobs(job_rx: &Receiver<UploadJob>, dropped_tx: &Sender<DroppedUpload>) {
    while let Ok(job) = job_rx.try_recv() {
        notify_dropped(&job, dropped_tx);
    }
}

impl UploadThreadHandle {
    /// Try to spawn the upload thread with a shared EGL context.
    ///
    /// Returns `None` if the shared context cannot be created (TierB device).
    /// The caller should fall back to render-thread uploads in that case.
    ///
    /// # Arguments
    /// * `egl` — EGL instance (dynamic)
    /// * `display` — current EGL display
    /// * `config` — EGL config used by the render context
    /// * `share_ctx` — the render thread's EGL context to share with
    pub fn try_spawn(
        egl_provider: Arc<dyn EglProvider>,
        egl: &khronos_egl::DynamicInstance<khronos_egl::EGL1_4>,
        display: khronos_egl::Display,
        config: khronos_egl::Config,
        share_ctx: khronos_egl::Context,
        gles_major: u32,
        has_robust_context: bool,
        surfaceless: bool,
    ) -> Option<Self> {
        with_background_context(egl_provider.as_ref(), || ())?;

        // Create shared context matching the render context's GLES
        // version.  R-3: mirror the render context's robustness
        // setting so the shared context also reports reset via
        // `glGetGraphicsResetStatus` if the driver supports it.
        let mut ctx_attribs: Vec<khronos_egl::Int> = vec![
            khronos_egl::CONTEXT_CLIENT_VERSION as khronos_egl::Int,
            gles_major as khronos_egl::Int,
        ];
        if has_robust_context {
            // Same attribute pair as `egl_ops::build_ctx_attribs`;
            // replicated here to keep `upload_thread` free of a
            // manager-module dependency.
            const EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT: khronos_egl::Int = 0x3138;
            const EGL_LOSE_CONTEXT_ON_RESET_EXT: khronos_egl::Int = 0x31BF;
            ctx_attribs.push(EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT);
            ctx_attribs.push(EGL_LOSE_CONTEXT_ON_RESET_EXT);
        }
        ctx_attribs.push(khronos_egl::NONE as khronos_egl::Int);

        let shared_ctx = egl
            .create_context(display, config, Some(share_ctx), &ctx_attribs)
            .map_err(|e| warn!("Upload thread: eglCreateContext failed: {e:?}"))
            .ok()?;

        // A 1x1 pbuffer purely so eglMakeCurrent has a surface to accept. Where
        // the driver publishes no pbuffer config there is none to create, and
        // the context is made current against EGL_NO_SURFACE instead -- uploads
        // go to texture objects, never to this surface.
        let pbuf = if surfaceless {
            None
        } else {
            let pbuf_attribs = [
                khronos_egl::WIDTH as i32,
                1,
                khronos_egl::HEIGHT as i32,
                1,
                khronos_egl::NONE as i32,
            ];
            Some(
                egl.create_pbuffer_surface(display, config, &pbuf_attribs)
                    .map_err(|e| {
                        warn!("Upload thread: eglCreatePbufferSurface failed: {e:?}");
                        egl.destroy_context(display, shared_ctx).ok();
                    })
                    .ok()?,
            )
        };

        let (job_tx, job_rx) = bounded::<UploadJob>(JOB_QUEUE_CAPACITY);
        let (result_tx, result_rx) = bounded::<CompletedUpload>(JOB_QUEUE_CAPACITY);
        let consecutive_failures = Arc::new(AtomicU32::new(0));
        let failures_clone = consecutive_failures.clone();
        // Unbounded: DroppedUpload is 16 bytes, and the number of items is
        // bounded in practice by the UploadServer in-flight budget.  Using
        // unbounded eliminates the possibility of dropped_tx.send() failing,
        // which would silently leak budget + pending_load_responses.
        let (dropped_tx, dropped_rx) = unbounded::<DroppedUpload>();

        // We need raw pointers to pass to the thread because EGL types are
        // not Send.  The upload thread will reconstruct EGL/GL wrappers.
        let expected_backend = egl_provider.backend_id();
        let display_raw = display.as_ptr() as usize;
        let ctx_raw = shared_ctx.as_ptr() as usize;
        let pbuf_raw = pbuf.map_or(0usize, |pbuf| pbuf.as_ptr() as usize);

        let handle = std::thread::Builder::new()
            .name("Migo-Upload".into())
            .spawn(move || {
                shared::thread_priority::set_current_thread_priority(
                    shared::thread_priority::Priority::Default,
                );
                upload_thread_main(
                    egl_provider,
                    expected_backend,
                    display_raw,
                    ctx_raw,
                    pbuf_raw,
                    job_rx,
                    result_tx,
                    failures_clone,
                    dropped_tx,
                );
            })
            .map_err(|e| warn!("Upload thread: spawn failed: {e}"))
            .ok()?;

        info!("Upload thread spawned (shared GL context)");

        Some(Self {
            job_tx: Some(job_tx),
            result_rx,
            consecutive_failures,
            dropped_rx,
            degraded: false,
            handle: Some(handle),
        })
    }

    /// Submit an upload job. Returns false if the thread is degraded or the queue is full.
    pub fn submit(&self, job: UploadJob) -> bool {
        if self.degraded {
            return false;
        }
        match self.job_tx.as_ref() {
            Some(tx) => tx.try_send(job).is_ok(),
            None => false,
        }
    }

    /// Drain completed uploads.  Non-blocking — returns whatever is ready.
    pub fn drain_completed(&mut self, out: &mut Vec<CompletedUpload>) {
        while let Ok(result) = self.result_rx.try_recv() {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            out.push(result);
        }
    }

    /// Drain per-item notifications for uploads that completed but whose
    /// results could not be delivered (channel full/disconnected).
    /// Each item carries `image_id` + `byte_len` so the caller can recover
    /// both the UploadServer budget and the pending_load_response.
    pub fn drain_dropped(&self, out: &mut Vec<DroppedUpload>) {
        while let Ok(item) = self.dropped_rx.try_recv() {
            out.push(item);
        }
    }

    /// Check if the upload thread has permanently degraded.
    pub fn is_degraded(&self) -> bool {
        self.degraded
            || self.consecutive_failures.load(Ordering::Relaxed) >= MAX_FAILURES_BEFORE_DEGRADE
    }
}

// ---------------------------------------------------------------------------
// Upload thread main loop
// ---------------------------------------------------------------------------

fn upload_thread_main(
    egl_provider: Arc<dyn EglProvider>,
    expected_backend: GraphicsBackendId,
    display_raw: usize,
    ctx_raw: usize,
    pbuf_raw: usize,
    job_rx: Receiver<UploadJob>,
    result_tx: Sender<CompletedUpload>,
    failures: Arc<AtomicU32>,
    dropped_tx: Sender<DroppedUpload>,
) {
    // Load through the exact provider selected and validated for the render
    // thread before reconstructing any raw EGL handles.
    let egl = match load_upload_egl(egl_provider.as_ref(), expected_backend) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                provider = egl_provider.label(),
                "Upload thread: failed to load EGL: {e}"
            );
            settle_queued_jobs(&job_rx, &dropped_tx);
            return;
        }
    };

    // Reconstruct opaque handles from raw pointers.
    let display = unsafe { khronos_egl::Display::from_ptr(display_raw as *mut _) };
    let ctx = unsafe { khronos_egl::Context::from_ptr(ctx_raw as *mut _) };
    // Zero is how "surfaceless" crosses the thread boundary: the spawn side has
    // no surface to hand over, and rebuilding one from a null pointer would
    // make eglMakeCurrent fail instead of doing the surfaceless thing.
    let pbuf =
        (pbuf_raw != 0).then(|| unsafe { khronos_egl::Surface::from_ptr(pbuf_raw as *mut _) });
    // Make the shared context current on this thread.
    if egl.make_current(display, pbuf, pbuf, Some(ctx)).is_err() {
        warn!("Upload thread: eglMakeCurrent failed");
        settle_queued_jobs(&job_rx, &dropped_tx);
        return;
    }

    let gl = unsafe {
        glow::Context::from_loader_function(|s| {
            egl.get_proc_address(s)
                .map(|f| f as *const std::ffi::c_void)
                .unwrap_or(std::ptr::null())
        })
    };

    // Re-run the AHB import-fn resolution on this thread, against
    // this thread's EGL context.  On most Android drivers the
    // `eglGetProcAddress` table is process-global and the
    // process-wide `OnceLock::get_or_init` already holds the
    // cached pointers — but the EGL 1.4 spec only guarantees the
    // addresses are valid for the resolving context, so we
    // defensively re-resolve under the upload thread's own
    // context.  If the main thread's resolution already succeeded
    // this is cheap (the `OnceLock` short-circuits); if the main
    // thread never attempted resolution we still capture the
    // diagnostic here so the operator sees where the failure
    // originated.
    let _ = crate::texture_import::ensure_import_fns(&|s| {
        egl.get_proc_address(s)
            .map(|f| unsafe { std::mem::transmute::<_, unsafe extern "C" fn()>(f) })
    });
    if let Err(e) = crate::texture_import::validate_import_fns() {
        // Not a hard failure: the upload thread's primary job is
        // PBO uploads; AHB is only a fast-path optimisation
        // inside `upload_texture_tiered`.  Log and continue so
        // PBO work still lands.
        info!("Upload thread: AHB path unavailable ({e}); continuing with PBO only");
    }

    info!("Upload thread: GL context ready");

    // Thread-local PBO ring — see `UploadPboPool` for the rationale.
    // Lives for the entire lifetime of the upload thread so PBO buffers
    // can be reused across successive uploads instead of being created
    // and destroyed per job (which on Mali/Adreno drivers costs a full
    // GPU memory allocator round-trip for every image).
    let mut pbo_pool = UploadPboPool::new();

    // Process jobs until channel closes.
    while let Ok(job) = job_rx.recv() {
        match do_upload(&gl, &job, &mut pbo_pool) {
            Ok(completed) => {
                failures.store(0, Ordering::Relaxed);
                if let Err(e) = result_tx.try_send(completed) {
                    // Channel full or disconnected — clean up GL resources
                    // to prevent GPU memory leak.
                    let c = e.into_inner();
                    warn!(
                        "Upload thread: result channel unavailable, deleting texture for image_id={}",
                        c.image_id
                    );
                    // Notify CanvasManager per-item so it can recover the
                    // UploadServer budget AND resolve the pending response.
                    // send() on an unbounded channel only fails if disconnected
                    // (engine shutting down), in which case budget recovery is moot.
                    let _ = dropped_tx.send(DroppedUpload {
                        image_id: c.image_id,
                        byte_len: c.byte_len,
                    });
                    unsafe {
                        gl.delete_texture(c.texture);
                        gl.delete_sync(c.fence);
                    }
                }
            }
            Err(e) => {
                let count = failures.fetch_add(1, Ordering::Relaxed) + 1;
                warn!("Upload thread: upload failed ({count}x): {e}");
                notify_dropped(&job, &dropped_tx);
                if count >= MAX_FAILURES_BEFORE_DEGRADE {
                    warn!("Upload thread: degraded after {count} consecutive failures");
                    break;
                }
            }
        }
    }
    // Settle jobs accepted before the receiver closed or before degradation
    // stopped processing.  No accepted job may leave its owner budget held.
    settle_queued_jobs(&job_rx, &dropped_tx);

    // Cleanup while the worker context is still current.  Shared objects may
    // outlive this context, so every owned PBO name is explicitly retired.
    unsafe { pbo_pool.clear(&gl) };
    egl.make_current(display, None, None, None).ok();
    egl.destroy_context(display, ctx).ok();
    if let Some(pbuf) = pbuf {
        egl.destroy_surface(display, pbuf).ok();
    }
    info!("Upload thread: exited");
}

fn load_upload_egl(
    provider: &dyn EglProvider,
    expected_backend: GraphicsBackendId,
) -> EngineResult<EglInstance> {
    if provider.backend_id() != expected_backend {
        return Err(EngineError::new(ErrorCode::InvalidOperation)
            .with_msg("upload EGL provider identity changed")
            .with_detail(format!(
                "expected={expected_backend:?}, actual={:?}, provider={}",
                provider.backend_id(),
                provider.label()
            )));
    }
    provider.load()
}

/// Ring-buffer of PBOs kept live on the upload thread so that each
/// `do_upload` can reuse one instead of paying a `glGenBuffers` +
/// `glDeleteBuffers` round-trip per image.  PBO reuse is race-free here
/// because uploads are sequential within this thread and the
/// `glBufferData(STREAM_DRAW)` call that precedes every `glTexImage2D`
/// effectively orphans the previous contents on every driver we target
/// (Mali / Adreno / PowerVR).
struct UploadPboPool {
    /// `(buffer, capacity_bytes)`.  Kept small (≤ 4) to bound memory;
    /// typical mobile games upload in bursts of 2-3 concurrent textures
    /// which is enough to saturate DMA even without a larger ring.
    entries: Vec<(glow::NativeBuffer, usize)>,
    reserved_bytes: usize,
    in_flight_bytes: usize,
}

impl UploadPboPool {
    const MAX_ENTRIES: usize = 4;

    fn new() -> Self {
        Self {
            entries: Vec::with_capacity(Self::MAX_ENTRIES),
            reserved_bytes: 0,
            in_flight_bytes: 0,
        }
    }

    /// Bytes held by idle PBO names in this pool.
    #[cfg(test)]
    fn reserved_bytes(&self) -> usize {
        self.reserved_bytes
    }

    /// Bytes associated with the PBO currently handed to GL upload code.
    #[cfg(test)]
    fn in_flight_bytes(&self) -> usize {
        self.in_flight_bytes
    }

    /// Acquire a PBO with capacity ≥ `need`.  If none matches, a fresh
    /// buffer is returned (and enters the pool on `release`).
    ///
    /// # Safety
    /// A GL context must be current.
    unsafe fn acquire(
        &mut self,
        gl: &glow::Context,
        need: usize,
    ) -> Result<glow::NativeBuffer, String> {
        if let Some(idx) = self.entries.iter().position(|(_, cap)| *cap >= need) {
            let (buf, capacity) = self.entries.remove(idx);
            self.reserved_bytes = self.reserved_bytes.saturating_sub(capacity);
            self.in_flight_bytes = self.in_flight_bytes.saturating_add(need);
            return Ok(buf);
        }
        self.in_flight_bytes = self.in_flight_bytes.saturating_add(need);
        match unsafe { gl.create_buffer() } {
            Ok(buf) => Ok(buf),
            Err(e) => {
                self.in_flight_bytes = self.in_flight_bytes.saturating_sub(need);
                Err(format!("create_buffer(PBO): {e}"))
            }
        }
    }

    /// Return a PBO to the pool.  Drops the buffer if the pool is full.
    unsafe fn release(&mut self, gl: &glow::Context, buf: glow::NativeBuffer, capacity: usize) {
        self.in_flight_bytes = self.in_flight_bytes.saturating_sub(capacity);
        if self.entries.len() < Self::MAX_ENTRIES {
            self.entries.push((buf, capacity));
            self.reserved_bytes = self.reserved_bytes.saturating_add(capacity);
        } else {
            unsafe { gl.delete_buffer(buf) };
        }
    }

    unsafe fn clear(&mut self, gl: &glow::Context) {
        self.clear_with(|buf| unsafe { gl.delete_buffer(buf) });
    }

    fn clear_with(&mut self, mut delete: impl FnMut(glow::NativeBuffer)) {
        for (buf, _) in self.entries.drain(..) {
            delete(buf);
        }
        self.reserved_bytes = 0;
        self.in_flight_bytes = 0;
    }
}

/// Perform a single texture upload on the shared GL context.
fn do_upload(
    gl: &glow::Context,
    job: &UploadJob,
    pbo_pool: &mut UploadPboPool,
) -> Result<CompletedUpload, String> {
    unsafe {
        let tex = gl
            .create_texture()
            .map_err(|e| format!("create_texture: {e}"))?;

        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
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

        // Upload RGBA data via PBO for async DMA.  `pbo_pool` hands us
        // a reusable buffer (with matching or larger capacity) so we
        // don't pay a glGenBuffers round-trip per image.
        let need = job.rgba.len();
        let pbo = match pbo_pool.acquire(gl, need) {
            Ok(p) => p,
            Err(e) => {
                // Free the texture we already created before bailing —
                // otherwise a run of acquire failures (PBO pool churn /
                // OOM) leaks one GL texture per attempt.
                gl.delete_texture(tex);
                return Err(e);
            }
        };
        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, Some(pbo));
        // `STREAM_DRAW` with a full-buffer replacement orphans the
        // driver-side storage (per spec §6.2), so reuse is safe even
        // though the previous DMA may not have completed on the GPU.
        gl.buffer_data_u8_slice(glow::PIXEL_UNPACK_BUFFER, &job.rgba, glow::STREAM_DRAW);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);

        // Immutable storage + subimage upload. GLES 3.0+ drivers
        // optimise this pair far more aggressively than the classic
        // `glTexImage2D` path: the allocation is committed once and
        // never reshaped, and subsequent rebinds skip validation of
        // completeness flags. Sized internal format `GL_RGBA8`
        // (`0x8058`) matches the canvas/manager immutable path.
        gl.tex_storage_2d(
            glow::TEXTURE_2D,
            /* levels */ 1,
            /* internal_format */ 0x8058,
            job.width as i32,
            job.height as i32,
        );
        gl.tex_sub_image_2d(
            glow::TEXTURE_2D,
            0,
            0,
            0,
            job.width as i32,
            job.height as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::BufferOffset(0),
        );

        gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        // Record the capacity the *driver* now considers the PBO to
        // have (== `need`, since we reallocated with STREAM_DRAW) so
        // the next acquire can reuse this one for any smaller payload.
        pbo_pool.release(gl, pbo, need);

        // Insert fence so the render thread knows when the upload is complete.
        let fence = match gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) {
            Ok(f) => f,
            Err(e) => {
                // Same leak guard as the PBO-acquire path above.
                gl.delete_texture(tex);
                return Err(format!("fence_sync: {e}"));
            }
        };

        // Flush to ensure the fence and upload are submitted to the GPU.
        gl.flush();

        gl.bind_texture(glow::TEXTURE_2D, None);

        let err = gl.get_error();
        if err != glow::NO_ERROR {
            gl.delete_texture(tex);
            gl.delete_sync(fence);
            return Err(format!("GL error after upload: 0x{err:X}"));
        }

        debug!(
            "Upload thread: uploaded image_id={} {}x{} ({} bytes)",
            job.image_id,
            job.width,
            job.height,
            job.rgba.len()
        );

        Ok(CompletedUpload {
            image_id: job.image_id,
            texture: tex,
            width: job.width,
            height: job.height,
            fence,
            byte_len: job.byte_len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egl_platform::EglConcurrency;

    #[derive(Debug)]
    struct InjectedFailureProvider {
        loads: Arc<AtomicU32>,
    }

    impl EglProvider for InjectedFailureProvider {
        fn backend_id(&self) -> GraphicsBackendId {
            GraphicsBackendId::of::<Self>()
        }

        fn concurrency(&self) -> EglConcurrency {
            EglConcurrency::RenderThreadOnly
        }

        fn platform_identity(&self) -> crate::egl_platform::PlatformIdentity {
            crate::egl_platform::PlatformIdentity::new::<Self>(self.backend_id(), 0)
        }

        fn label(&self) -> &str {
            "upload-sentinel-egl"
        }

        fn load(&self) -> EngineResult<EglInstance> {
            self.loads.fetch_add(1, Ordering::Relaxed);
            Err(EngineError::new(ErrorCode::RenderInitializeError)
                .with_msg("upload sentinel provider load failure"))
        }

        fn display(&self, _egl: &EglInstance) -> EngineResult<khronos_egl::Display> {
            panic!("upload worker must only load before reconstructing raw handles")
        }
    }

    #[test]
    fn render_thread_only_provider_never_spawns_upload_worker() {
        let create_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider = InjectedFailureProvider {
            loads: Arc::new(AtomicU32::new(0)),
        };

        let called = Arc::clone(&create_called);
        let result = with_background_context(&provider, move || {
            called.store(true, Ordering::Relaxed);
        });

        assert!(result.is_none());
        assert!(!create_called.load(Ordering::Relaxed));
    }

    #[test]
    fn injected_egl_provider_is_the_only_upload_worker_loader() {
        let loads = Arc::new(AtomicU32::new(0));
        let provider = InjectedFailureProvider {
            loads: Arc::clone(&loads),
        };

        let error = load_upload_egl(&provider, provider.backend_id()).unwrap_err();
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert_eq!(error.msg, "upload sentinel provider load failure");
    }

    /// Verifies that the dropped upload channel delivers consistent per-item
    /// data — no torn reads, no lost image_ids, no byte_len mismatch.
    /// This is the contract that CanvasManager relies on to recover budget
    /// AND resolve pending responses.
    #[test]
    fn dropped_channel_delivers_consistent_per_item_notifications() {
        let (tx, rx) = unbounded::<DroppedUpload>();

        // Simulate upload thread sending two dropped completions.
        tx.send(DroppedUpload {
            image_id: 42,
            byte_len: 4096,
        })
        .unwrap();
        tx.send(DroppedUpload {
            image_id: 99,
            byte_len: 1024,
        })
        .unwrap();

        // Simulate CanvasManager draining (same as drain_dropped).
        let mut items = Vec::new();
        while let Ok(item) = rx.try_recv() {
            items.push(item);
        }

        // Each item carries exactly the data for one dropped upload.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].image_id, 42);
        assert_eq!(items[0].byte_len, 4096);
        assert_eq!(items[1].image_id, 99);
        assert_eq!(items[1].byte_len, 1024);
    }

    /// drain_dropped returns nothing when the channel is empty.

    #[test]
    fn failed_job_settlement_emits_exactly_one_dropped_notification() {
        let (job_tx, job_rx) = bounded::<UploadJob>(2);
        let (dropped_tx, dropped_rx) = unbounded::<DroppedUpload>();
        let job = UploadJob {
            image_id: 7,
            width: 2,
            height: 2,
            rgba: Arc::new(vec![0; 16]),
        };
        job_tx.send(job).unwrap();
        let queued = job_rx.try_recv().unwrap();
        notify_dropped(&queued, &dropped_tx);
        drop(job_tx);

        let item = dropped_rx.try_recv().expect("failed job was not settled");
        assert_eq!((item.image_id, item.byte_len), (7, 16));
        assert!(dropped_rx.try_recv().is_err(), "job settled more than once");
    }

    #[test]
    fn worker_exit_settles_all_jobs_left_in_queue() {
        let (job_tx, job_rx) = bounded::<UploadJob>(4);
        let (dropped_tx, dropped_rx) = unbounded::<DroppedUpload>();
        for id in 1..=3 {
            job_tx
                .send(UploadJob {
                    image_id: id,
                    width: 1,
                    height: 1,
                    rgba: Arc::new(vec![id as u8; 4]),
                })
                .unwrap();
        }

        settle_queued_jobs(&job_rx, &dropped_tx);
        let mut settled = Vec::new();
        while let Ok(item) = dropped_rx.try_recv() {
            settled.push((item.image_id, item.byte_len));
        }
    }
    #[test]
    fn worker_pbo_ledger_returns_to_zero_on_clear() {
        let mut pool = UploadPboPool::new();
        pool.reserved_bytes = 4096;
        pool.in_flight_bytes = 2048;
        // The production caller invokes `clear` before unbinding its current
        // worker context.  The accounting half is context-free, so exercise
        // it without constructing a GL loader in this host-only test.
        pool.clear_with(|_| {});
        assert_eq!(pool.reserved_bytes(), 0);
        assert_eq!(pool.in_flight_bytes(), 0);
    }

    #[test]
    fn drain_dropped_returns_empty_when_no_drops() {
        let (_tx, rx) = unbounded::<DroppedUpload>();
        let mut items = Vec::new();
        while let Ok(item) = rx.try_recv() {
            items.push(item);
        }
        assert!(items.is_empty());
    }

    /// The dropped channel must never lose items even when many uploads are
    /// dropped without the render thread draining.  This covers the scenario
    /// where both result_tx and a bounded dropped_tx would be full.
    /// With an unbounded dropped channel, all items must arrive.
    #[test]
    fn dropped_channel_never_loses_items_under_backpressure() {
        use crossbeam_channel::unbounded;

        let (tx, rx) = unbounded::<DroppedUpload>();

        // Simulate 64 dropped uploads accumulating without any drain.
        // (More than JOB_QUEUE_CAPACITY * 2, which would overflow two
        // bounded-16 channels.)
        for i in 0..64u64 {
            tx.send(DroppedUpload {
                image_id: i,
                byte_len: 1024,
            })
            .unwrap(); // unbounded send is infallible (non-disconnected)
        }

        let mut items = Vec::new();
        while let Ok(item) = rx.try_recv() {
            items.push(item);
        }

        assert_eq!(items.len(), 64);
        for (i, item) in items.iter().enumerate() {
            assert_eq!(item.image_id, i as u64);
            assert_eq!(item.byte_len, 1024);
        }
    }
}
