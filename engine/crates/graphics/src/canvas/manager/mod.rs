extern crate khronos_egl as egl;

use crate::backend::gl::state::Canvas2DState;
use crate::backend::gl::surface::Canvas2DContext;
use crate::dirty_region::damage_tracker::ResolvedDamage;
use crate::{
    BoundContext,
    canvas::{BackingSizeOwner, engine_default_backing},
    egl_platform::{EglProvider, PreparedEglSurfaceRef},
    surface_binding::{CandidateCleanup, InstallPhase, RecreateKind, SurfaceInstallFailure},
};
use glow::HasContext;

/// Local shim for the `NativeTextureFromRaw` pattern used in
/// `image.rs` — kept private to this module so callers inside
/// `manager/mod.rs` can reconstruct a `glow::NativeTexture` from
/// the raw GLuint stored in `StoredImage` without importing the
/// trait from an adjacent child module.
trait NativeTextureFromRawShim {
    fn try_from_raw(raw: u32) -> Option<glow::NativeTexture>;
}
impl NativeTextureFromRawShim for glow::NativeTexture {
    #[inline]
    fn try_from_raw(raw: u32) -> Option<glow::NativeTexture> {
        std::num::NonZeroU32::new(raw).map(glow::NativeTexture)
    }
}

/// Mirror of [`NativeTextureFromRawShim`] for `glow::NativeFramebuffer`.
/// Reconstructs a native framebuffer from an actual driver/resource GLuint.
/// Client framebuffer IDs must never be converted through this helper.
trait NativeFramebufferFromRawShim {
    fn try_from_raw(raw: u32) -> Option<glow::NativeFramebuffer>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EglSwapFailureClass {
    ContextLost,
    SurfaceLost,
    Other,
}

#[inline]
fn classify_egl_swap_failure(error: egl::Error) -> EglSwapFailureClass {
    match error {
        egl::Error::ContextLost => EglSwapFailureClass::ContextLost,
        egl::Error::BadCurrentSurface
        | egl::Error::BadSurface
        | egl::Error::BadNativeWindow
        | egl::Error::BadDisplay => EglSwapFailureClass::SurfaceLost,
        _ => EglSwapFailureClass::Other,
    }
}
impl NativeFramebufferFromRawShim for glow::NativeFramebuffer {
    #[inline]
    fn try_from_raw(raw: u32) -> Option<glow::NativeFramebuffer> {
        std::num::NonZeroU32::new(raw).map(glow::NativeFramebuffer)
    }
}
use shared::{
    error::{EngineResult, ErrorCode},
    protocol::{
        io_cmd::NormalizedImage,
        render_cmd::{
            BufferId, CanvasId, FramebufferId, ProgramId, RenderbufferId, ShaderId, TextureId,
        },
    },
    surface::PixelRatio,
};
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU32, Ordering},
};

mod context_2d_impl;
pub(crate) mod drawing_buffer;
mod egl_ops;
pub(crate) mod gl_object;
mod image;
mod pbo_upload;
mod types;

pub(crate) use types::{
    BlendEquation, BlendFactors, BufferMeta, CanvasGLState, CanvasInfo, CanvasStateTable,
    FramebufferMeta, MAX_UNIFORM_CACHE, ProgramMeta, QueryMeta, RenderbufferMeta, SamplerMeta,
    ScissorState, ShaderMeta, Shared2DContext, SyncMeta, TextureMeta, TransformFeedbackMeta,
    VaoMeta, VertexAttribPointerFp, ee,
};
use types::{CanvasEntry, EglContextHandle, SurfaceKind};

use self::image::ImageRegistry;

/// One entry in the [`CanvasManager::canvas2d_snapshots`] pool.
/// `tex` is owned by the EGL share group; deletion happens at
/// frame-end drain or on canvas-manager teardown.  Width/height are
/// retained so the upload path can size the destination glCopyTexImage2D
/// without a JS-side handshake.
///
/// `cache_key` is `Some` for snapshots whose JS-side originator
/// matched the cocos text pattern and whose key missed the text
/// texture cache: the drain path hands the texture off to this
/// session's text texture cache ([`CanvasManager::text_cache`])
/// instead of deleting it, so a subsequent identical fillText hits the
/// cache.  `None` for legacy snapshots (`getImageData` readback,
/// generic uploads).
#[derive(Clone)]
struct Canvas2DSnapshotEntry {
    tex: glow::NativeTexture,
    width: u32,
    height: u32,
    bytes: usize,
    cache_key: Option<Box<shared::text_texture_cache::TextCacheKey>>,
}

/// Bounded ownership for a window EGLSurface between native creation and
/// installation in `canvases`.  The slot is populated immediately after
/// `eglCreateWindowSurface` succeeds, before any later fallible operation.
///
/// `context` is optional because context creation is itself fallible.  Once a
/// context exists it remains owned here until the pair is moved atomically into
/// the onscreen `CanvasEntry`, or until cleanup proves the EGLSurface no longer
/// references the native window.
struct PendingOnscreenEgl {
    target: PreparedEglSurfaceRef,
    surface: egl::Surface,
    context: Option<egl::Context>,
    /// A preserved DrawingBuffer is ownership-paired with `context`. Moving
    /// both into this slot before the first make-current keeps a transient
    /// candidate failure from discarding the last presented frame.
    drawing_buffer: Option<drawing_buffer::DrawingBuffer>,
}

/// A budget-rejected upload still holding the caller's oneshot
/// `resp`, waiting for the next frame's upload budget to open up.
/// See [`CanvasManager::deferred_uploads`].
pub(crate) struct DeferredUpload {
    pub image_id: u32,
    /// The image's RGBA bytes are already decoded; holding an owned
    /// `NormalizedImage` rather than a borrow means the deferred
    /// queue can outlive any `load_image` op without lifetime
    /// plumbing.  Memory cost is bounded by the retry loop at the
    /// top of each frame emptying the queue as the budget opens.
    pub image: shared::protocol::io_cmd::NormalizedImage,
    pub resp: shared::protocol::render_cmd::RenderCmdResp<(u32, u32)>,
}

/// Soft cap on the deferred-upload queue.  A game that fires more
/// than this many concurrent loads in a single frame is either
/// bombarding the engine (benchmark / stress test) or leaking
/// requests; once the queue reaches this depth the handler falls
/// back to synchronous sync upload for the overflow rather than
/// letting the queue grow unbounded.  Chosen to comfortably cover
/// a first-screen burst (hundreds of assets) while still capping
/// worst-case residency.
pub(crate) const MAX_DEFERRED_UPLOADS: usize = 256;

/// RGBA bytes retained by deferred uploads.  The bound is intentionally
/// observable and conservative; its final value needs device ready-latency
/// and peak-residency data, so it is not a performance claim.
pub(crate) const MAX_DEFERRED_BYTES: usize = 128 * 1024 * 1024;

#[inline]
fn deferred_upload_fits(entry_count: usize, retained_bytes: usize, new_bytes: usize) -> bool {
    entry_count < MAX_DEFERRED_UPLOADS
        && retained_bytes
            .checked_add(new_bytes)
            .is_some_and(|total| total <= MAX_DEFERRED_BYTES)
}

/// Construct the upload budget with the platform-aware API identity.  Keeping
/// this in one helper prevents a context-recovery path from drifting from the
/// initialisation path.  The profile values still need device measurements;
/// this only removes the false Android API-0 classification on non-Android.
fn upload_server_for_device(
    caps: &crate::device_caps::DeviceCapabilities,
) -> crate::upload_server::UploadServer {
    let profile = caps.render_profile_for_platform(crate::device_caps::android_api_level());
    let mut server = crate::upload_server::UploadServer::new(
        profile.max_upload_jobs_per_frame,
        profile.max_upload_bytes_per_frame,
    );
    server.set_frame_budget(
        profile.max_upload_jobs_per_frame,
        profile.max_upload_bytes_per_frame,
    );
    server
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AsyncUploadRejectAction {
    SyncFallback,
    DeferRetry,
}

fn should_latch_default_fbo_readback(needs_default_fbo_readback: bool) -> bool {
    !needs_default_fbo_readback
}

/// Gather one frame's worth of upload completions into a reusable buffer:
/// whatever was deferred from earlier frames, then whatever `fill` produces.
///
/// **Both vectors keep their allocations.** The shape this replaced —
/// `let mut v = Vec::new(); fill(&mut v); let mut d = mem::take(&mut pending);
/// d.extend(v); for x in d { … pending.push(x) }` — gave one back and bought
/// another every frame that had an upload in flight, because consuming `d` by
/// value frees its buffer while `mem::take` has already reduced `pending` to
/// capacity zero. During a level load or texture streaming that is a `malloc`
/// and a `free` per frame on the render thread.
///
/// `Vec::append` is what makes it free: it moves the elements out of `pending`
/// and leaves it empty *with its capacity intact*, so the caller's re-defer
/// pushes land in the buffer that is already there. `fill` drains straight into
/// the scratch, which removes the intermediate vector entirely.
///
/// Element order is preserved — deferred first, newly arrived after — because
/// the caller's fence check is order-sensitive in one direction: an upload
/// deferred N frames ago is the one most likely to be ready now, and putting it
/// first keeps the common case at the front of the scan.
///
/// **The Canvas2D snapshot pool's two caps only work as a pair, so their
/// relationship is a build error rather than a test.**
///
/// Guards `CanvasManager::MAX_LIVE_CANVAS2D_SNAPSHOTS` (1024 entries) and
/// `MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES` (64 MiB, tied to the sync-readback
/// ceiling). Three things have to stay true:
///
/// 1. For the shape the pool exists to cache — a 200x40 RGBA8 text strip,
///    32,000 bytes — the *count* cap binds first. If the byte cap started
///    binding, a text-heavy frame would be limited by GPU bytes instead of entry
///    count and the 1024 figure would stop meaning anything.
/// 2. For a large capture — 1024x1024 RGBA is 4 MiB — the *byte* cap binds
///    first, which is its only job. Sixteen of those reach 64 MiB while the
///    count cap is still a thousand entries away.
/// 3. The count cap comfortably exceeds the worst case it was sized for: ~200
///    text sprites in one Cocos shop frame, each taking a snapshot. Four times
///    over, so a scene twice as heavy still has headroom.
///
/// **A free `const`, deliberately.** This first lived as an associated const
/// inside `impl CanvasManager`, which compiles but enforces nothing: an unused
/// associated const's initializer is never evaluated, so breaking a cap left the
/// build green. Reverse-validating the assertion is what surfaced that — the
/// form matters as much as the predicate.
const _: () = {
    const TEXT_STRIP: usize = 200 * 40 * 4;
    const LARGE_CAPTURE: usize = 1024 * 1024 * 4;
    const OBSERVED_SPRITES_PER_FRAME: usize = 200;

    assert!(
        CanvasManager::MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES / TEXT_STRIP
            > CanvasManager::MAX_LIVE_CANVAS2D_SNAPSHOTS,
        "the byte cap now binds before the count cap for a typical text strip, \
         so a text-heavy frame is limited by GPU bytes rather than entry count"
    );
    assert!(
        CanvasManager::MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES / LARGE_CAPTURE
            < CanvasManager::MAX_LIVE_CANVAS2D_SNAPSHOTS,
        "the byte cap no longer bounds large captures before the count cap, \
         which was its only job"
    );
    assert!(
        CanvasManager::MAX_LIVE_CANVAS2D_SNAPSHOTS >= OBSERVED_SPRITES_PER_FRAME * 4,
        "the count cap no longer comfortably exceeds the ~200 text sprites per \
         frame it was sized for"
    );
};

/// Whether a `glClientWaitSync` status means the GPU has passed the fence.
///
/// The four possible returns are `ALREADY_SIGNALED` (done before we asked),
/// `CONDITION_SATISFIED` (done while we waited), `TIMEOUT_EXPIRED` (not done)
/// and `WAIT_FAILED` (the driver refused to answer). Only the first two are a
/// yes, and the last two are the same decision at every call site even though
/// they mean different things — one is a slow GPU, the other is a broken one,
/// and neither permits proceeding as though the work landed.
///
/// **One spelling for three fences,** which is the point of hoisting it here.
/// `drain_upload_completed` polls upload fences with a zero timeout,
/// `pbo_upload::PboPool::acquire` polls DMA fences the same way, and
/// `snapshot_canvas2d_region` waits on a pre-blit fence with a real timeout.
/// Each had its own copy of the comparison; a fourth would eventually have
/// admitted `TIMEOUT_EXPIRED` by writing `!=` where it meant `==`, and the
/// symptom — reusing a buffer the GPU is still reading, or blitting pre-draw
/// tiles — produces no GL error.
///
/// A free function so it can be tested without a GL context, like
/// [`stage_upload_drain`] below.
#[inline]
pub(super) fn fence_signalled(status: u32) -> bool {
    status == glow::ALREADY_SIGNALED || status == glow::CONDITION_SATISFIED
}

#[inline]
fn fence_failed_permanently(status: u32) -> bool {
    status == glow::WAIT_FAILED
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotFenceStatus {
    Signalled,
    Timeout,
    Failed,
}

#[inline]
fn classify_snapshot_fence(status: u32) -> SnapshotFenceStatus {
    if fence_signalled(status) {
        SnapshotFenceStatus::Signalled
    } else if status == glow::TIMEOUT_EXPIRED {
        SnapshotFenceStatus::Timeout
    } else {
        SnapshotFenceStatus::Failed
    }
}

/// A free function rather than a method so the allocation claim can be gated
/// without a GL context: [`CanvasManager::drain_upload_completed`] calls
/// `glClientWaitSync`, and reaching it needs a live context.
fn stage_upload_drain<T>(
    scratch: &mut Vec<T>,
    pending: &mut Vec<T>,
    fill: impl FnOnce(&mut Vec<T>),
) -> Vec<T> {
    let mut staged = std::mem::take(scratch);
    staged.clear();
    staged.append(pending);
    fill(&mut staged);
    staged
}

fn decide_async_upload_reject_action(
    upload_thread_healthy: bool,
    upload_server: Option<&crate::upload_server::UploadServer>,
    bytes: usize,
) -> AsyncUploadRejectAction {
    let can_fit_later = upload_server
        .map(|server| server.can_ever_fit_bytes(bytes))
        .unwrap_or(false);

    if upload_thread_healthy && can_fit_later {
        AsyncUploadRejectAction::DeferRetry
    } else {
        AsyncUploadRejectAction::SyncFallback
    }
}

#[allow(private_interfaces)]
pub(crate) struct CanvasManager {
    egl_provider: std::sync::Arc<dyn EglProvider>,
    egl: egl_ops::EglRuntime,
    /// Shared so the render thread can reuse this table instead of building a
    /// second one. `glow::Context::from_loader_function` resolves the whole
    /// GLES dispatch table -- 709 symbols on this device -- and that measured
    /// 41-50 ms. It was being paid twice per session: once here, blocking the
    /// host thread's GPU join, and once again in `RenderThread` right after,
    /// delaying the first frame by the same amount. Both contexts are built on
    /// the render thread from EGL contexts in one share group, so one table
    /// serves both.
    gl: std::sync::Arc<glow::Context>,
    display: egl::Display,
    config: egl::Config,
    /// See `EglInitResult::surfaceless`.
    surfaceless: bool,

    /// This session's text texture cache.  Taken once at construction
    /// from `text_texture_cache::text_cache_for_host(host_id)`, the same
    /// handle the session's `CanvasOpState` holds.
    ///
    /// It must be per session rather than per process because the ids it
    /// stores are GL texture names minted in *this* manager's EGL
    /// context (created above with no share list linking it to any other
    /// manager). A name reachable from another session would be either
    /// meaningless there or, worse, deletable by it.
    text_cache: shared::text_texture_cache::SharedTextCache,

    pub(super) dpi: PixelRatio,

    resource: EglContextHandle,

    /// One EGL context and one `GrDirectContext` shared by every offscreen
    /// Canvas2D surface, created on first use.
    ///
    /// Its own EGL context rather than the resource context, because Skia
    /// caches GL state per `GrDirectContext`: anything else making that GL
    /// context current -- a WebGL batch, a contextless shader compile --
    /// invalidates the cache with no signal. Nothing but offscreen 2D drawing
    /// ever binds this one, which makes that invariant local and checkable.
    ///
    /// See `Canvas2DContext::new_shared_offscreen` and
    /// `docs/performance/android/multicanvas-fixed-cost.md`.
    shared_2d: Option<Shared2DContext>,

    // current binding
    pub(super) bound: BoundContext,

    // canvases
    pub(super) canvases: HashMap<CanvasId, CanvasEntry>,
    next_canvas_id: AtomicU32,

    // 2D
    /// See [`crate::canvas_keyed`]: reached twice per Canvas2D command, once to
    /// classify the draw's damage and once to run the command.
    pub(super) contexts_2d: crate::canvas_keyed::CanvasKeyed<Canvas2DContext>,
    pub(super) dirty_2d: HashSet<CanvasId>,

    // Image registry
    image_registry: ImageRegistry,

    /// Render-owner scratch for image retention tables. It is cleared and
    /// returned after each packet or standalone batch, so warm sprite runs do
    /// not repeatedly allocate past the inline SmallVec capacity.
    pub(crate) retained_image_scratch: smallvec::SmallVec<[u32; 16]>,

    /// Per-canvas FBO used as the read source for `glCopyTexImage2D`
    /// when handling `GLCmd::TexImage2DFromShared`.  Lazy-created on
    /// first use because most canvases never trigger the WebGL
    /// `texImage2D(image)` path; one FBO per canvas because FBO
    /// names live in their owning context's namespace even when
    /// textures are shared via EGL share lists.  Deleted in
    /// `destroy_canvas` / `destroy_all` alongside other per-canvas
    /// GL objects.
    image_copy_fbos: HashMap<CanvasId, glow::NativeFramebuffer>,

    /// Pool of GL textures that mirror Canvas2D regions captured by
    /// [`shared::protocol::render_cmd::Canvas2DCmd::GetImageDataSnapshot`].
    /// Keyed by an opaque `snapshot_id` JS holds onto.  Drained at
    /// frame-end (`drain_canvas2d_snapshots`) so cocos's
    /// `getImageData(text)`→`texImage2D` pattern stays GPU-only and
    /// never builds up across frames.
    ///
    /// Bounded two ways, and **at the cap a new capture is refused, not traded
    /// for an old one** — this said "oldest evicted first", which the code has
    /// never done and must not. Every live entry is waiting for a
    /// `TexImage2DFromSnapshot` that JS has already committed to issuing;
    /// evicting one to make room turns a bounded pool into a missing texture.
    /// Refusing returns `0`, which JS reads as "fall back to the legacy CPU
    /// readback" — slower for that one label, correct for all of them.
    ///
    /// The two bounds are `MAX_LIVE_CANVAS2D_SNAPSHOTS` (1024 entries) and
    /// `MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES` (64 MiB). Which one binds depends on
    /// snapshot size: at the typical 200x40 text strip (31.25 KiB) the byte cap
    /// would allow ~2048, so the count cap binds first; for large captures the
    /// byte cap binds first. Both are checked before any GPU work.
    canvas2d_snapshots: HashMap<u32, Canvas2DSnapshotEntry>,
    /// Aggregate RGBA bytes retained by the live snapshot textures.
    canvas2d_snapshot_bytes: usize,
    /// Insertion order tracker for the per-frame drain.
    /// `VecDeque` for O(1) push/pop on both ends.  Snapshot ids are
    /// allocated JS-side and arrive on the wire with the capture
    /// command, so the manager only tracks insertion order, not
    /// allocation.
    canvas2d_snapshot_order: std::collections::VecDeque<u32>,
    /// Lazy per-render-thread temp FBO used as the DRAW target when
    /// blitting from a Canvas2D surface into a freshly allocated
    /// snapshot texture.  One global FBO is enough because we
    /// detach the colour attachment immediately after each blit.
    canvas2d_snapshot_blit_fbo: Option<glow::NativeFramebuffer>,
    /// Lazy per-render-thread temp FBO used as the READ source when
    /// uploading a snapshot texture into a destination texture via
    /// `glCopyTexImage2D`.  Same one-FBO-many-attachments idiom as
    /// [`Self::canvas2d_snapshot_blit_fbo`].
    canvas2d_snapshot_read_fbo: Option<glow::NativeFramebuffer>,

    /// Last eglSwapInterval value to avoid redundant driver calls per frame.
    /// Initialized to -1 (sentinel) so the first swap forces an actual EGL call.
    /// Reset to -1 whenever the EGL surface is destroyed/recreated, because the
    /// new surface may not inherit the previous interval on all drivers.
    /// The sole eglSwapInterval call site is in `swap_buffers_no_restore()`,
    /// guarded by `interval != self.last_swap_interval`.
    last_swap_interval: i32,

    /// Set to true when EGL reports CONTEXT_LOST during swap_buffers.
    /// The next `create_onscreen()` call will perform a full resource rebuild.
    context_lost: bool,

    /// Set only for EGL failures that prove the native presentation target is
    /// no longer usable. The render thread retires the matching generation;
    /// allocation/access failures remain retryable and do not masquerade as a
    /// host Surface loss.
    surface_unavailable: bool,

    /// One-shot: force the next `create_onscreen()` to fully tear down and
    /// recreate the onscreen EGL surface, bypassing the same-window fast paths
    /// (skip-recreate and fast-resize). Set by the render thread when a
    /// surfaceDestroyed occurred (epoch advanced), because the previous
    /// EGLSurface is bound to an abandoned ANativeWindow even if Android hands
    /// back an equal window pointer/size. Consumed (cleared) on read.
    force_onscreen_recreate: bool,

    /// What the next onscreen surface install owes the content: the 2D drawing
    /// state of the context that was torn down, if there was one.
    ///
    /// Recorded by the teardown rather than read at install time, because on
    /// Android those are two separate events. `surfaceDestroyed` takes the
    /// onscreen 2D context away when the app is backgrounded; `surfaceCreated`
    /// arrives whenever the user comes back, which may be much later. An
    /// install that asks "did this canvas have a 2D context?" by looking at
    /// `contexts_2d` is asking after the answer has already been destroyed --
    /// it reads `false`, skips the rebuild, and every later `Canvas2DBatch`
    /// fails with `2d context not found` while the game runs on at full speed,
    /// painting into nothing.
    ///
    /// Content never re-requests the context: it holds the object it got from
    /// `getContext('2d')` and has no idea the surface went away. So nothing
    /// else will ever ask for the rebuild.
    onscreen_2d_restore: Option<Canvas2DState>,

    /// The backing-store size the content chose for the onscreen canvas with
    /// `canvas.width`/`height`, if it has chosen one.
    ///
    /// `None` means the size is the engine's own, derived from the surface, and
    /// therefore has to be re-derived whenever the surface changes; that is the
    /// whole rule, and holding the *size* rather than a flag is what makes it
    /// survive a full context loss, where the buffer that carried the number is
    /// gone. See [`crate::canvas::BackingSizeOwner`] for the two halves of the
    /// rule and where the JS one lives.
    ///
    /// Here rather than on the entry because it has to outlive a surface
    /// teardown: `create_onscreen` destroys and reinserts the entry on every
    /// recreate, so state stored there would reset itself exactly when it is
    /// needed. Same reason [`Self::onscreen_2d_restore`] lives here, and the
    /// onscreen canvas is the singleton id 1 this path already assumes.
    ///
    /// A session restart does not clear it, because nothing tells the render
    /// thread a restart happened -- and the replacement content inherits the
    /// installed backing size through `op_get_canvas_info` anyway, so what this
    /// remembers still describes the buffer that is actually installed.
    onscreen_content_backing: Option<(u32, u32)>,

    /// Debug one-shot: when set (via `WEBGL_lose_context.loseContext()` ->
    /// `GLCmd::DebugLoseContext`), the next `check_graphics_reset_status()`
    /// poll reports a reset and consumes the flag, driving the exact same
    /// detection -> teardown -> recovery pipeline as a real GPU reset. This is
    /// how context-loss recovery is verified on devices that cannot be made to
    /// trigger a real EGL_CONTEXT_LOST on demand.
    simulated_reset: bool,

    /// Prepared platform target paired with the installed onscreen EGLSurface.
    /// It is non-owning; the render binding's current SurfaceLease owns the
    /// underlying native resource and is dropped after CanvasManager.
    installed_surface: Option<PreparedEglSurfaceRef>,

    /// The frame rate content has asked for, kept here because it is a property
    /// of the *window* that must survive the window being replaced. A fresh
    /// native target has no rate requested, so a surface recreate would otherwise
    /// silently drop the request and leave the display in whichever mode it
    /// happened to be in -- the failure this exists to make unrepresentable, by
    /// re-asserting the request at the one place an install commits.
    frame_rate_request: u32,

    /// At most one partially-created onscreen EGL target.  This is a control
    /// path only; it adds no work to drawing or presentation.
    pending_onscreen: Option<PendingOnscreenEgl>,

    /// The canvases a context-loss teardown destroyed and the rebuild still owes
    /// the game, parked here so a failed attempt can resume instead of losing
    /// them.
    ///
    /// **Why a field and not a local.** `tear_down_share_group` returns a
    /// [`ShareGroupRestorePlan`] precisely so the obligation to re-create those
    /// canvases is explicit — its doc says a teardown not followed by a matching
    /// restore "silently strands live JS handles". But `#[must_use]` only catches
    /// *ignoring* the value, not dropping it on an early return, and
    /// `try_recover_context` had two `?` between the teardown and the restore
    /// (`create_pbuffer_context`, `bind_resource`) plus more inside the restore
    /// itself. Any of them fired and the plan went out of scope with the local.
    ///
    /// The next attempt would then tear down an already-empty registry, get an
    /// empty plan, and the canvases from the first teardown would be stranded for
    /// the life of the process — JS registers a canvas exactly once, so there is
    /// no lazy rebuild to fall back on. A game whose EGL recovery hit a transient
    /// failure mid-rebuild would keep running with ids that resolve to nothing.
    ///
    /// Parking it makes a retry resume the *original* debt.
    /// `register_offscreen` already returns `Ok(())` for an id it holds, so
    /// re-running a partially applied plan is idempotent.
    ///
    /// Mirrors [`Self::pending_onscreen`], which exists for the same reason one
    /// step earlier in the same function.
    pending_share_group_restore: Option<ShareGroupRestorePlan>,

    /// Makes explicit shutdown and the `Drop` fallback share one idempotent
    /// teardown path.
    teardown_complete: bool,
    /// True only after either every native window EGLSurface was explicitly
    /// destroyed or final `eglTerminate` succeeded. Prepared platform targets
    /// must not be dropped without this proof.
    native_release_confirmed: bool,

    // GL object registries
    pub(crate) programs: HashMap<ProgramId, ProgramMeta>,
    /// Programs whose `glLinkProgram` has been issued but whose `LINK_STATUS`
    /// has not been read. Insertion-ordered so a drain reads them in the order
    /// the content linked them, which is also the order the driver is most
    /// likely to have finished them in.
    pub(crate) pending_links: Vec<ProgramId>,
    pub(crate) shaders: HashMap<ShaderId, ShaderMeta>,
    pub(crate) buffers: HashMap<BufferId, BufferMeta>,
    pub(crate) textures: HashMap<TextureId, TextureMeta>,
    pub(crate) framebuffers: HashMap<FramebufferId, FramebufferMeta>,
    pub(crate) renderbuffers: HashMap<RenderbufferId, RenderbufferMeta>,
    pub(crate) gl_state: CanvasStateTable,
    /// Ordered, authoritative accounting for WebGL-created texture and
    /// renderbuffer storage. This excludes Canvas2D/internal renderer objects.
    pub(crate) webgl_gpu_budget: crate::webgl_gpu_budget::WebGlGpuBudget,

    // WebGL 2 registries.  Kept separate from the WebGL 1 set because
    // lookup paths are distinct (VAOs are bound by the state tracker,
    // samplers are per-texture-unit, sync handles are polled from
    // synchronous reply ops).  The dispatchers in renderergl own the
    // actual GL calls; manager just holds the handle tables.
    pub(crate) vaos: HashMap<shared::protocol::render_cmd::VaoId, VaoMeta>,
    pub(crate) samplers: HashMap<shared::protocol::render_cmd::SamplerId, SamplerMeta>,
    pub(crate) syncs: HashMap<shared::protocol::render_cmd::SyncId, SyncMeta>,
    /// WebGL 2 Query objects (occlusion, timer, etc.).
    pub(crate) queries: HashMap<u32, QueryMeta>,
    /// WebGL 2 Transform Feedback objects.
    pub(crate) transform_feedbacks: HashMap<u32, TransformFeedbackMeta>,

    /// Texture atlas for small Canvas2D `drawImage` sources.
    /// Lazy-initialised on first use to keep the idle memory
    /// footprint unchanged (a single 2048x2048 page is 16 MiB).
    ///
    /// Enabling path: [`Self::maybe_atlas_small_image`] allocates a
    /// region and returns the adjusted origin.  The call site can
    /// then reuse the atlas page texture + UV offset in
    /// `StoredImage` so `drawImage` of many small sprites (icons,
    /// HUD) avoids per-draw texture bind churn.  The Skia backend
    /// still wraps the atlas page into an `SkImage`; the draw
    /// path offsets its source rect by the atlas region.
    ///
    /// P1-13: gated behind the `experimental_atlas` cargo feature
    /// until the profile-validated cutover lands.  The field is
    /// kept at the struct level (rather than behind `cfg!`) so
    /// the hot-path fast check (`atlas.is_some()`) still compiles
    /// to a simple `None` discriminant test without needing a
    /// branch on a feature flag.
    pub(crate) atlas: Option<crate::atlas::AtlasManager>,

    // NOTE: WebGL → Canvas2D invalidation is now tracked per-context
    // via `Canvas2DContext::skia_state_stale` (see backend/gl/surface.rs).
    // The old manager-global `skia_needs_reset` flag was removed after
    // it was observed to over-/mis-invalidate in multi-canvas scenes.
    /// Runtime device capabilities, detected once at init.
    pub(crate) device_caps: crate::device_caps::DeviceCapabilities,

    /// Cross-thread image capability publication. A runtime AHB import failure
    /// clears its one-way AHB bit so later IO jobs skip decode-to-AHB.
    gpu_caps: std::sync::Arc<shared::device::gpu_caps::GpuCaps>,

    /// GLES major version negotiated during EGL init (3 = ES 3.0+, 2 = ES 2.0).
    /// Used when creating shared contexts (offscreen canvas, upload thread).
    gles_major: u32,

    /// Whether the EGL implementation supports
    /// `EGL_EXT_create_context_robustness`.  When true, every context
    /// the manager creates asks for `LOSE_CONTEXT_ON_RESET_EXT` so the
    /// GL driver reports resets synchronously (R-3).
    has_robust_context: bool,

    /// Resolved `glGetGraphicsResetStatusKHR` entry point (R-3).
    /// `None` on drivers without `GL_KHR_robustness` — the render
    /// thread then has to fall back to detecting loss at
    /// `eglSwapBuffers` time, same as the legacy path.
    #[allow(improper_ctypes_definitions)]
    pub(crate) gl_get_graphics_reset_status_fn: Option<unsafe extern "C" fn() -> u32>,

    /// Preserved EGL context from the last destroyed onscreen canvas.
    /// Reused on the next `create_onscreen()` to avoid losing GL state
    /// (textures, shaders, buffers) across Android surface destroy/recreate cycles.
    preserved_ctx: Option<egl::Context>,

    /// Preserved onscreen DrawingBuffer paired with [`Self::preserved_ctx`].
    ///
    /// Android `SurfaceView` destruction invalidates the window EGLSurface, not
    /// the share-group GL objects.  Industry renderers (Chromium's
    /// DrawingBuffer, Flutter's surface/picture split, Slint's Skia surface
    /// layer) keep the offscreen backbuffer independent from the platform
    /// surface so returning from background can immediately blit the last
    /// rendered frame while the JS/game loop schedules its next redraw.
    ///
    /// Before this field we destroyed the DrawingBuffer on
    /// `surfaceDestroyed()` but preserved the EGL context.  On resume,
    /// `create_onscreen()` then allocated a brand-new empty FBO, forced
    /// `dirty = true`, and blitted black to the new window surface.  If Cocos
    /// had not yet produced a new RAF frame (as seen in hxddd logs), the screen
    /// remained black even though textures/programs survived.  Preserving the
    /// DrawingBuffer closes that lifecycle gap.
    preserved_drawing_buffer: Option<drawing_buffer::DrawingBuffer>,

    /// Set to true when a game reads pixels from the onscreen default framebuffer
    /// (readPixels on canvas_id=1 with default FBO bound). Once set, DrawingBuffer
    /// bypass is permanently disabled so the DrawingBuffer preserves content across
    /// swaps and readback returns valid data. One-way latch — never cleared.
    needs_default_fbo_readback: bool,

    /// Per-frame upload budget gating (device-tier aware).
    /// Gates submission to the upload thread with bandwidth and job-count limits.
    upload_server: Option<crate::upload_server::UploadServer>,

    /// Dedicated texture upload thread (TierA only).
    /// None on TierB devices or if shared-context probe failed.
    pub(crate) upload_thread: Option<crate::upload_thread::UploadThreadHandle>,

    /// Best-effort shader binary cache.
    /// Saves compiled GL program binaries to disk; loads on next run.
    /// None if GL_NUM_PROGRAM_BINARY_FORMATS == 0.
    pub(crate) shader_cache: Option<crate::shader_cache::ShaderCache>,

    /// Uploads whose GPU fence has not yet signaled.
    /// Re-checked each frame in `drain_upload_completed()`.
    pending_uploads: Vec<crate::upload_thread::CompletedUpload>,
    /// Working buffer for [`Self::drain_upload_completed`], held across frames
    /// so neither it nor `pending_uploads` ever hands its allocation back.
    /// See [`stage_upload_drain`].
    upload_drain_scratch: Vec<crate::upload_thread::CompletedUpload>,
    /// Working buffer for the dropped-upload half of the same drain.
    upload_dropped_scratch: Vec<crate::upload_thread::DroppedUpload>,

    /// Deferred LoadImage responses — sent only after the async upload
    /// thread has completed and the texture is registered.
    /// Key: image_id, Value: the oneshot response sender.
    pending_load_responses: HashMap<u64, shared::protocol::render_cmd::RenderCmdResp<(u32, u32)>>,

    /// Image IDs whose DestroyImage arrived while the upload was still in
    /// flight.  `drain_upload_completed` uses this to delete the orphaned
    /// GL texture/fence instead of registering them.
    cancelled_uploads: HashSet<u64>,

    /// Uploads that the per-frame budget rejected.  Previously the
    /// handler fell back to a synchronous `load_shared_image` on
    /// the render thread -- exactly the frame spike we're trying
    /// to prevent.  Instead we queue the work here, retry at the
    /// top of each frame via `try_drain_deferred_uploads`, and keep
    /// the oneshot `resp` alive so `Image.onload` fires in order
    /// when the upload eventually lands.
    ///
    /// VecDeque for FIFO ordering; a capacity cap is enforced at
    /// insert time to bound worst-case memory when a pathological
    /// game fires a thousand concurrent loads.
    deferred_uploads: std::collections::VecDeque<DeferredUpload>,

    /// Total RGBA payload retained by `deferred_uploads`.
    deferred_upload_bytes: usize,

    /// `eglSetDamageRegionKHR` function pointer (None if EGL_KHR_partial_update
    /// is not supported). Called before `eglSwapBuffers` to inform the
    /// compositor which region changed — saves power on OLED screens.
    #[allow(improper_ctypes_definitions)]
    egl_set_damage_region_fn: Option<
        unsafe extern "C" fn(egl::Display, egl::Surface, *const egl::Int, egl::Int) -> egl::Boolean,
    >,

    /// `eglSwapBuffersWithDamageKHR`/`EXT` function pointer, or `None` when
    /// neither extension is present. This is the *other* half of partial
    /// presentation: `eglSetDamageRegionKHR` above tells the driver which tiles
    /// to load, this tells the compositor which region it has to recomposite.
    /// Having only the first is half the chain, which is what this engine
    /// shipped until now.
    #[allow(improper_ctypes_definitions)]
    egl_swap_buffers_with_damage_fn: Option<
        unsafe extern "C" fn(egl::Display, egl::Surface, *const egl::Int, egl::Int) -> egl::Boolean,
    >,

    /// Set once a driver rejects a swap-with-damage call. The plain swap is
    /// always a correct substitute, and a driver that refused once will refuse
    /// again, so the retry is not worth a syscall per frame for the rest of the
    /// session.
    swap_with_damage_rejected: bool,

    /// Unified per-frame damage accumulator for mixed Canvas2D + WebGL frames.
    /// Fed by Canvas2D batches, GL draw/clear, and readback paths.
    /// Resolved at swap time to determine partial vs full surface update.
    pub(crate) damage: crate::damage_effect::FrameDamageAccumulator,

    /// History of recent successfully-presented current-frame damage regions.
    /// Unioned with the queried buffer age to compute the exact repair region.
    damage_history: crate::present_damage::PresentDamageHistory,

    /// The present/blit plan for the frame-in-flight, keyed by canvas id.
    /// [`declare_frame_damage`] builds and caches it (declaring the `repair`
    /// region to the compositor before any FBO 0 write); [`swap_buffers_no_restore`]
    /// consumes it so the blit and history use the exact same `repair`/`current`
    /// regions. A missing or mismatched entry is recomputed safely before
    /// touching FBO 0; it never reuses another canvas/frame's plan.
    pending_present_plan: Option<(CanvasId, crate::present_damage::PresentDamagePlan)>,

    /// Whether the selected EGL window-surface config is single-sample
    /// (`EGL_SAMPLE_BUFFERS == 0 && EGL_SAMPLES == 0`). Queried once at init;
    /// a multisampled config — or a failed query — disables the partial blit
    /// because identity-coordinate rect blits are only valid single-sample.
    dest_single_sample: bool,

    /// `EGL_EXT_buffer_age` is independently advertised (never inferred from
    /// `EGL_KHR_partial_update`). Governs whether a rejected/absent
    /// `eglSetDamageRegionKHR` declaration may keep a partial repair: EXT
    /// guarantees the aged back-buffer contents, KHR-only does not.
    has_ext_buffer_age: bool,

    /// Count of `glClientWaitSync` calls issued by
    /// [`Self::snapshot_canvas2d_region_with_id`] since the last
    /// [`Self::take_snapshot_fence_waits`].  Read and reset at present time
    /// so the render thread can attribute each actual GPU wait to
    /// [`crate::render_thread::PresentPassCounters::note_snapshot_wait`] once
    /// per fence rather than once per present.
    snapshot_fence_waits: u32,
}

impl CanvasManager {
    /// This session's text texture cache.  The render loop uses it for
    /// the memory-pressure trim and the font-generation bump, both of
    /// which must stay scoped to this session.
    pub(crate) fn text_cache(&self) -> &shared::text_texture_cache::SharedTextCache {
        &self.text_cache
    }

    /// The GL dispatch table this manager built, for the render thread to reuse.
    ///
    /// See the `gl` field: building a second one costs 41-50 ms of
    /// `eglGetProcAddress` on the path to first frame, for a table identical to
    /// this one.
    pub(crate) fn gl_shared(&self) -> std::sync::Arc<glow::Context> {
        std::sync::Arc::clone(&self.gl)
    }

    /// Resolve a GL entry point from the exact EGL implementation injected for
    /// this manager. Used only while constructing render-thread dispatch tables.
    pub(crate) fn gl_proc_address(&self, symbol: &str) -> *const std::ffi::c_void {
        self.egl
            .get_proc_address(symbol)
            .map(|function| function as *const std::ffi::c_void)
            .unwrap_or(std::ptr::null())
    }

    pub(crate) fn new_with_resource(
        egl_provider: std::sync::Arc<dyn EglProvider>,
        dpi: f32,
        cache_dir: Option<&std::path::Path>,
        gpu_caps: std::sync::Arc<shared::device::gpu_caps::GpuCaps>,
        // This session's text texture cache handle, resolved from the
        // host id by the render thread.  The same handle the session's
        // `CanvasOpState` holds, so the JS and render sides of the cache
        // protocol agree; distinct from every other session's, so the GL
        // texture names this manager mints stay inside its own context.
        text_cache: shared::text_texture_cache::SharedTextCache,
    ) -> EngineResult<Self> {
        let dpi = PixelRatio::new(dpi).ok_or_else(|| {
            ee(
                ErrorCode::InvalidArgument,
                "CanvasManager requires a finite positive pixel ratio",
            )
        })?;
        let init = egl_ops::init_egl(egl_provider.as_ref())?;
        let mut egl = init.egl;
        let display = init.display;
        let config = init.config;
        let surfaceless = init.surfaceless;
        let gles_major = init.gles_major;
        let has_robust_context = init.has_robust_context;

        // Create resource context + pbuffer.
        let (resource_ctx, resource_surf) = egl_ops::create_pbuffer_context(
            &egl,
            display,
            config,
            None,
            16,
            16,
            gles_major,
            has_robust_context,
            surfaceless,
        )?;
        // From this point onward EglRuntime owns the share-group root. Any
        // later constructor error/panic destroys it and terminates the display.
        egl.track_resource(resource_ctx, resource_surf);
        let resource = EglContextHandle {
            ctx: resource_ctx,
            surf: resource_surf,
        };

        // Make resource current once.
        egl.make_current(display, resource.surf, resource.surf, Some(resource.ctx))
            .map_err(|e| {
                ee(
                    ErrorCode::RenderBackendError,
                    format!("eglMakeCurrent(resource) failed: {e:?}"),
                )
            })?;

        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                egl.get_proc_address(s)
                    .map(|f| f as *const std::ffi::c_void)
                    .unwrap_or(std::ptr::null())
            })
        };

        let egl_extensions = egl
            .query_string(Some(display), egl::EXTENSIONS)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut device_caps =
            crate::device_caps::DeviceCapabilities::detect(&gl, &egl_extensions, gles_major);

        // Resolve the AHardwareBuffer → EGLImage → GL texture import
        // function pointers exactly once, while the resource EGL context is
        // still current (so `eglGetProcAddress` can return driver pointers
        // for both EGL and GL extension entry points).  Without this call
        // every `import_ahb_as_texture` invocation would fail with
        // "AHB import functions not resolved" and force a costly AHB readback.
        // Downgrade `ahb_available` if the driver doesn't actually
        // expose all four required entry points, which lets the upload
        // pipeline skip the AHB path entirely and go through the PBO
        // fallback instead of hitting the same error on every image.
        if device_caps.ahb_available {
            // `eglGetProcAddress` returns `extern "system" fn()`; on every
            // platform Migo targets this is ABI-identical to the
            // `unsafe extern "C" fn()` the import helper expects, so the
            // transmute below is just a calling-convention re-tagging.
            // Kept `unsafe` for documentation because the compiler cannot
            // verify the invariant.
            match crate::texture_import::ensure_import_fns(&|s| {
                egl.get_proc_address(s)
                    .map(|f| unsafe { std::mem::transmute::<_, unsafe extern "C" fn()>(f) })
            }) {
                Ok(()) => {}
                Err(missing) => {
                    tracing::warn!("AHB advertised but {missing}; disabling AHB upload path");
                    device_caps.ahb_available = false;
                }
            }
        }

        // P1-12: set the Skia resource-cache budget from the
        // detected device tier before any `Canvas2DContext` is
        // created.  The per-context cap is derived lazily in
        // `per_ctx_resource_cache_bytes`, so updating the global
        // here propagates automatically to every subsequent
        // canvas creation.  A low-memory signal does not come
        // through here: it squeezes and restores within one call
        // — see [`CanvasManager::on_trim_memory`].
        crate::backend::gl::surface::set_skia_resource_cache_budget(
            crate::backend::gl::surface::tier_budget(device_caps.tier()),
        );
        tracing::info!(
            "DeviceCapabilities: GLES {:?}, tier={:?}, pbo={}, fence={}, compute={}, ahb={}, buffer_age={}, partial_update={}",
            device_caps.gles_version,
            device_caps.tier(),
            device_caps.has_pbo,
            device_caps.has_fence_sync,
            device_caps.has_compute,
            device_caps.ahb_available,
            device_caps.has_buffer_age,
            device_caps.has_partial_update,
        );

        // Initialize shader binary cache (best-effort, no-op if unsupported).
        let shader_cache = cache_dir.and_then(|dir| {
            let cache = crate::shader_cache::ShaderCache::new(&gl, dir);
            if cache.is_supported() {
                Some(cache)
            } else {
                None
            }
        });

        // Spawn upload thread on TierA devices (shared GL context for async texture upload).
        let upload_thread = if device_caps.tier() == crate::device_caps::DeviceTier::TierA {
            crate::upload_thread::UploadThreadHandle::try_spawn(
                std::sync::Arc::clone(&egl_provider),
                &egl,
                display,
                config,
                resource.ctx,
                gles_major,
                has_robust_context,
                surfaceless,
            )
        } else {
            None
        };
        // Budget gating: only when upload thread is live.
        let upload_server = if upload_thread.is_some() {
            Some(upload_server_for_device(&device_caps))
        } else {
            None
        };

        // Probe EGL_KHR_partial_update (power saving on OLED screens).
        let egl_set_damage_region_fn = if egl_extensions.contains("EGL_KHR_partial_update") {
            let fname = std::ffi::CStr::from_bytes_with_nul(b"eglSetDamageRegionKHR\0").unwrap();
            egl.get_proc_address(fname.to_str().unwrap())
                .map(|ptr| unsafe { std::mem::transmute(ptr) })
        } else {
            None
        };
        if egl_set_damage_region_fn.is_some() {
            tracing::info!("EGL_KHR_partial_update available");
        }

        // Surface damage: the compositor-facing half. KHR and EXT are the same
        // entry point under two names and drivers ship one or the other, so both
        // are probed rather than assuming the newer one.
        let egl_swap_buffers_with_damage_fn = [
            (
                "EGL_KHR_swap_buffers_with_damage",
                "eglSwapBuffersWithDamageKHR",
            ),
            (
                "EGL_EXT_swap_buffers_with_damage",
                "eglSwapBuffersWithDamageEXT",
            ),
        ]
        .iter()
        .find_map(|(extension, symbol)| {
            if !egl_extensions.contains(extension) {
                return None;
            }
            egl.get_proc_address(symbol).map(|ptr| {
                tracing::info!("{} available", extension);
                unsafe {
                    std::mem::transmute::<
                        extern "system" fn(),
                        unsafe extern "C" fn(
                            egl::Display,
                            egl::Surface,
                            *const egl::Int,
                            egl::Int,
                        ) -> egl::Boolean,
                    >(ptr)
                }
            })
        });

        // Query the selected EGL config's multisample state once. The partial
        // DrawingBuffer→surface blit uses identity source/dest coordinates,
        // which is only valid for a single-sample destination; a multisampled
        // window surface — or a failed query — forces the legacy full blit.
        const EGL_SAMPLE_BUFFERS: egl::Int = 0x3032;
        const EGL_SAMPLES: egl::Int = 0x3031;
        let dest_single_sample = match (
            egl.get_config_attrib(display, config, EGL_SAMPLE_BUFFERS),
            egl.get_config_attrib(display, config, EGL_SAMPLES),
        ) {
            (Ok(sample_buffers), Ok(samples)) => sample_buffers == 0 && samples == 0,
            _ => false,
        };
        // Cache whether EGL_EXT_buffer_age is *independently* advertised (as
        // opposed to age support supplied only by EGL_KHR_partial_update) before
        // `device_caps` is moved into `Self`.
        let has_ext_buffer_age = device_caps.has_ext_buffer_age;

        // Probe `GL_KHR_robustness::glGetGraphicsResetStatusKHR` (R-3).
        // Only resolved when both the EGL extension for robust contexts
        // and the GL extension are advertised — otherwise calling
        // `glGetGraphicsResetStatus` is a no-op on some drivers and a
        // hard crash on others.  The render loop uses the resolved
        // pointer to poll context health at frame boundaries instead
        // of waiting for `eglSwapBuffers` to fail.
        let gl_get_graphics_reset_status_fn: Option<unsafe extern "C" fn() -> u32> =
            if has_robust_context {
                let gl_exts = unsafe {
                    let ptr = gl.get_parameter_string(glow::EXTENSIONS);
                    ptr
                };
                if gl_exts.contains("GL_KHR_robustness") || gl_exts.contains("GL_EXT_robustness") {
                    // Prefer the KHR-suffixed symbol when present.
                    let primary = egl.get_proc_address("glGetGraphicsResetStatusKHR");
                    let fallback = egl.get_proc_address("glGetGraphicsResetStatusEXT");
                    let fn_ptr = primary.or(fallback);
                    if let Some(p) = fn_ptr {
                        tracing::info!("GL_KHR_robustness::glGetGraphicsResetStatus resolved");
                        Some(unsafe { std::mem::transmute::<_, unsafe extern "C" fn() -> u32>(p) })
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

        // Pre-allocate with reasonable capacities to reduce rehashing.
        // Most games use a small number of canvases and GL objects.
        Ok(Self {
            egl_provider,
            egl,
            gl: std::sync::Arc::new(gl),
            display,
            config,
            surfaceless,
            text_cache,
            dpi,
            resource,
            bound: BoundContext::Resource,
            canvases: HashMap::with_capacity(4),
            next_canvas_id: AtomicU32::new(2), // 1 is reserved for onscreen
            contexts_2d: crate::canvas_keyed::CanvasKeyed::default(),
            dirty_2d: HashSet::with_capacity(4),
            image_registry: ImageRegistry::new(),
            retained_image_scratch: smallvec::SmallVec::new(),
            image_copy_fbos: HashMap::with_capacity(4),
            canvas2d_snapshots: HashMap::with_capacity(8),
            canvas2d_snapshot_bytes: 0,
            canvas2d_snapshot_order: std::collections::VecDeque::with_capacity(8),
            canvas2d_snapshot_blit_fbo: None,
            canvas2d_snapshot_read_fbo: None,
            last_swap_interval: -1, // force first eglSwapInterval call
            context_lost: false,
            surface_unavailable: false,
            force_onscreen_recreate: false,
            onscreen_2d_restore: None,
            // A canvas nobody has sized yet is the engine's to derive.
            onscreen_content_backing: None,
            simulated_reset: false,
            installed_surface: None,
            frame_rate_request: shared::frame_rate::DEFAULT_FPS,
            pending_onscreen: None,
            pending_share_group_restore: None,
            teardown_complete: false,
            native_release_confirmed: false,
            shared_2d: None,
            programs: HashMap::with_capacity(16),
            pending_links: Vec::new(),
            shaders: HashMap::with_capacity(32),
            buffers: HashMap::with_capacity(32),
            textures: HashMap::with_capacity(64),
            framebuffers: HashMap::with_capacity(8),
            renderbuffers: HashMap::with_capacity(8),
            gl_state: CanvasStateTable::default(),
            webgl_gpu_budget: crate::webgl_gpu_budget::WebGlGpuBudget::new(),
            vaos: HashMap::with_capacity(16),
            samplers: HashMap::with_capacity(8),
            syncs: HashMap::with_capacity(4),
            queries: HashMap::with_capacity(4),
            transform_feedbacks: HashMap::with_capacity(2),
            atlas: None,
            device_caps,
            deferred_upload_bytes: 0,
            gpu_caps,
            gles_major,
            has_robust_context,
            gl_get_graphics_reset_status_fn,
            preserved_ctx: None,
            preserved_drawing_buffer: None,
            needs_default_fbo_readback: false,
            snapshot_fence_waits: 0,
            upload_server,
            upload_thread,
            shader_cache,
            pending_uploads: Vec::new(),
            upload_drain_scratch: Vec::new(),
            upload_dropped_scratch: Vec::new(),
            pending_load_responses: HashMap::new(),
            deferred_uploads: std::collections::VecDeque::new(),
            cancelled_uploads: HashSet::new(),
            egl_set_damage_region_fn,
            egl_swap_buffers_with_damage_fn,
            swap_with_damage_rejected: false,
            damage: crate::damage_effect::FrameDamageAccumulator::new(),
            pending_present_plan: None,
            damage_history: crate::present_damage::PresentDamageHistory::new(),
            dest_single_sample,
            has_ext_buffer_age,
        })
    }

    fn new_canvas_id(&self) -> CanvasId {
        let id = self.next_canvas_id.fetch_add(1, Ordering::Relaxed);
        CanvasId::from(id)
    }

    /// Publish the immutable startup capabilities after the render thread has
    /// also resolved its initial surface. Keeping this separate from resource
    /// context construction prevents a successful caps snapshot from racing a
    /// subsequent initial-surface failure.
    pub(crate) fn publish_gpu_caps(&self) {
        self.gpu_caps.set(
            self.device_caps.compressed_format_support.etc2,
            self.device_caps.compressed_format_support.astc,
            self.device_caps.ahb_available,
        );
    }

    // ==================== Canvas Lifecycle ====================

    /// Create an offscreen (pbuffer) canvas.
    ///
    /// `w` and `h` are in **physical (buffer) pixels** — the same unit JS
    /// `canvas.width`/`canvas.height` uses, matching browser semantics.
    pub(crate) fn create_offscreen(&mut self, w: u32, h: u32) -> EngineResult<CanvasId> {
        let id = self.new_canvas_id();
        self.register_offscreen(id, w, h)?;
        Ok(id)
    }

    /// Same as `create_offscreen` but uses the supplied `id` instead
    /// of allocating one.  Used by the fire-and-forget JS path
    /// (`CanvasCmd::RegisterOffscreen`) where JS owns the id range.
    /// Idempotent: if `id` already exists this is a no-op.
    pub(crate) fn register_offscreen(&mut self, id: CanvasId, w: u32, h: u32) -> EngineResult<()> {
        if shared::protocol::render_cmd::checked_canvas_pixel_count(w, h).is_none() {
            return Err(ee(
                ErrorCode::InvalidArgument,
                format!("offscreen canvas {w}x{h} exceeds the surface pixel cap"),
            ));
        }
        if self.canvases.contains_key(&id) {
            return Ok(());
        }

        let share = Some(self.resource.ctx);
        let (ctx, surf) = egl_ops::create_pbuffer_context(
            &self.egl,
            self.display,
            self.config,
            share,
            w,
            h,
            self.gles_major,
            self.has_robust_context,
            self.surfaceless,
        )?;

        let info = CanvasInfo {
            id,
            width: w,
            height: h,
            is_onscreen: false,
        };

        self.canvases.insert(
            id,
            CanvasEntry {
                info,
                physical_width: w,
                physical_height: h,
                kind: SurfaceKind::Pbuffer,
                ctx: EglContextHandle { ctx, surf },
                drawing_buffer: None,
                bypass_drawing_buffer: false,
                applied_default_framebuffer: None,
            },
        );
        // Offscreen canvas created → bypass no longer valid.
        self.evaluate_bypass();

        Ok(())
    }

    /// Whether this canvas's Canvas2D surface lives on the shared context.
    ///
    /// Answered from the context that exists, not from the feature flag: a
    /// canvas created while sharing was on must keep being bound to the shared
    /// context even if the flag is read differently later, or its draws would
    /// go to a context that does not own its surface.
    pub(crate) fn canvas_uses_shared_2d(&self, id: CanvasId) -> bool {
        let Some(shared) = self.shared_2d.as_ref() else {
            return false;
        };
        self.contexts_2d
            .get(&id)
            .is_some_and(|ctx| ctx.ctx_tag == shared.ctx_tag)
    }

    /// Make the shared offscreen-2D EGL context current, creating it and its
    /// `GrDirectContext` on first use.
    ///
    /// Returns the pieces `Canvas2DContext::new_shared_offscreen` needs. The
    /// pbuffer is 1x1: nothing renders into it, it exists only because
    /// `eglMakeCurrent` needs a draw surface unless the driver advertises
    /// `EGL_KHR_surfaceless_context`. Every canvas on this context draws into
    /// its own Skia render target, so FBO 0 here is never a target -- which is
    /// what makes one context safe to share, and is exactly the property the
    /// per-canvas pbuffers do *not* have (see `register_offscreen`).
    pub(crate) fn bind_shared_2d_context(
        &mut self,
        serving: CanvasId,
    ) -> EngineResult<(
        skia_safe::gpu::DirectContext,
        skia_safe::gpu::gl::Interface,
        u32,
    )> {
        if self.shared_2d.is_none() {
            let (ctx, surf) = egl_ops::create_pbuffer_context(
                &self.egl,
                self.display,
                self.config,
                Some(self.resource.ctx),
                1,
                1,
                self.gles_major,
                self.has_robust_context,
                self.surfaceless,
            )?;
            self.egl
                .make_current(self.display, surf, surf, Some(ctx))
                .map_err(|e| {
                    ee(
                        ErrorCode::RenderBackendError,
                        format!("shared 2D context make_current failed: {e:?}"),
                    )
                })?;
            self.bound = BoundContext::Shared2D(serving);
            let interface = {
                let load_gl = |symbol: &str| self.gl_proc_address(symbol);
                skia_safe::gpu::gl::Interface::new_load_with(load_gl).ok_or_else(|| {
                    ee(
                        ErrorCode::RenderBackendError,
                        "shared 2D context: GL interface assembly failed",
                    )
                })?
            };
            let gr_ctx = skia_safe::gpu::direct_contexts::make_gl(interface.clone(), None)
                .ok_or_else(|| {
                    ee(
                        ErrorCode::RenderBackendError,
                        "shared 2D context: GrDirectContext creation failed",
                    )
                })?;
            // The shared context is one context serving many surfaces, so it
            // takes a whole context's share of the Skia budget -- not a
            // per-canvas slice, and not Skia's own default, which is what it
            // got until this call existed. `LiveContextCount` deliberately does
            // not count canvases that share it, so the divisor stays the number
            // of contexts.
            let mut gr_ctx = gr_ctx;
            gr_ctx.set_resource_cache_limits(skia_safe::gpu::ganesh::ResourceCacheLimits {
                max_resources: crate::backend::gl::surface::skia_max_resources(),
                max_resource_bytes: crate::backend::gl::surface::per_ctx_resource_cache_bytes(),
            });
            let ctx_tag = crate::backend::gl::surface::alloc_shared_ctx_tag();
            tracing::info!("Canvas2D shared offscreen GrDirectContext created");
            self.shared_2d = Some(Shared2DContext {
                egl: EglContextHandle { ctx, surf },
                gr_ctx,
                interface,
                ctx_tag,
            });
        }

        let shared = self
            .shared_2d
            .as_ref()
            .expect("created on the line above if absent");
        let (ctx, surf) = (shared.egl.ctx, shared.egl.surf);
        let out = (
            shared.gr_ctx.clone(),
            shared.interface.clone(),
            shared.ctx_tag,
        );
        // Only the id changes when the shared context is already current: the
        // expensive part is `eglMakeCurrent`, and switching between two
        // canvases that share a context does not need one.
        if !matches!(self.bound, BoundContext::Shared2D(_)) {
            self.egl
                .make_current(self.display, surf, surf, Some(ctx))
                .map_err(|e| {
                    ee(
                        ErrorCode::RenderBackendError,
                        format!("shared 2D context make_current failed: {e:?}"),
                    )
                })?;
        }
        self.bound = BoundContext::Shared2D(serving);
        Ok(out)
    }

    /// Record that a program's link was issued and its status not yet read.
    ///
    /// Idempotent: a program re-linked before its previous link was drained is
    /// still exactly one pending entry, and the status read that eventually
    /// happens describes the most recent link -- which is the only one whose
    /// binary is still installed.
    pub(crate) fn mark_link_pending(&mut self, program_id: ProgramId) {
        if let Some(meta) = self.programs.get_mut(&program_id) {
            if !meta.link_pending {
                meta.link_pending = true;
                self.pending_links.push(program_id);
            }
        }
    }

    /// Drop a program from the pending queue without reading its status.
    ///
    /// Used when the program is deleted: the handle is gone, so there is
    /// nothing to query and nothing worth caching.
    pub(crate) fn forget_pending_link(&mut self, program_id: ProgramId) {
        if let Some(meta) = self.programs.get_mut(&program_id) {
            meta.link_pending = false;
        }
        self.pending_links.retain(|id| *id != program_id);
    }

    /// Force the next `create_onscreen()` to fully recreate the onscreen EGL
    /// surface, bypassing the same-window fast paths. Call before recreating
    /// after a surfaceDestroyed (the old EGLSurface is bound to an abandoned
    /// ANativeWindow even if the new window pointer/size compare equal).
    pub(crate) fn force_next_onscreen_recreate(&mut self) {
        self.force_onscreen_recreate = true;
    }

    /// Record the rate content is presenting at and ask the display to serve it.
    ///
    /// Filtered on change because the ask is a display transaction and the
    /// startup path legitimately re-sends the rate that is already in effect
    /// (the host applies its configured target to a thread already defaulting to
    /// it).
    pub(crate) fn set_frame_rate_request(&mut self, fps: u32) {
        if self.frame_rate_request == fps {
            return;
        }
        self.frame_rate_request = fps;
        if let Some(installed) = self.installed_surface.as_ref() {
            installed.request_frame_rate(fps);
        }
    }

    /// Explicitly detach the installed window EGLSurface.
    ///
    /// Success is the only point at which the caller may drop the matching
    /// `SurfaceResourceLease`.  Ordinary EGL failure leaves the platform target
    /// installed and returns an error; `destroy_all` retains it through final
    /// `eglTerminate` as the shutdown fallback.
    pub(crate) fn release_onscreen(&mut self) -> EngineResult<()> {
        let id = CanvasId::from(1u32);

        if self.pending_onscreen.is_some()
            && self.cleanup_pending_onscreen() != CandidateCleanup::Released
        {
            return Err(ee(
                ErrorCode::RenderBackendError,
                "partial onscreen EGL target remains referenced during release",
            ));
        }

        if self.canvases.contains_key(&id) {
            return self.destroy_onscreen_internal(id);
        }

        if self.installed_surface.is_some() {
            return Err(ee(
                ErrorCode::RenderBackendError,
                "installed native target has no owned onscreen EGL canvas",
            ));
        }

        Ok(())
    }

    /// Create or recreate the onscreen canvas (id=1).
    ///
    /// `surface_size`: expected physical dimensions from the SurfaceRef.
    /// When provided, skip the destroy-recreate cycle if the same native
    /// window is already active AND its dimensions match. This avoids
    /// "already connected" (EGL_BAD_ALLOC) errors on some Android drivers
    /// while still handling surface resizes (e.g. status bar hide/show).
    /// Pass `None` for initial creation and context recovery.
    pub(crate) fn create_onscreen(
        &mut self,
        target: PreparedEglSurfaceRef,
        recreate_kind: RecreateKind,
        surface_size: Option<(u32, u32)>,
        had_usable_previous: bool,
        pixel_ratio: Option<PixelRatio>,
    ) -> Result<(), SurfaceInstallFailure> {
        // The candidate ratio follows the same transaction as the native
        // Surface. It may influence a fresh default backing store, but does not
        // become observable render state until installation succeeds.
        let effective_pixel_ratio = pixel_ratio.unwrap_or(self.dpi);
        // A prior failed cleanup may still own a window EGLSurface.  Retry its
        // cleanup before touching the new candidate, but never infer release
        // from the new operation's error code.
        if self.pending_onscreen.is_some()
            && self.cleanup_pending_onscreen() != CandidateCleanup::Released
        {
            return Err(SurfaceInstallFailure::from_phase(
                ee(
                    ErrorCode::RenderBackendError,
                    "previous partial onscreen EGL target is still retained",
                ),
                false,
                InstallPhase::PreviousInvalidated,
                CandidateCleanup::NotRequired,
            ));
        }

        if let Some((exp_w, exp_h)) = surface_size {
            tracing::info!(
                target = ?target,
                "CanvasManager::create_onscreen begin: expected={}x{}",
                exp_w,
                exp_h
            );
        } else {
            tracing::info!(
                target = ?target,
                "CanvasManager::create_onscreen begin: expected=<none>"
            );
        }

        let id = CanvasId::from(1u32);

        // Consume the force flag: when set (a surfaceDestroyed occurred), skip
        // both same-window fast paths below and fall through to a full teardown +
        // recreate, since the existing EGLSurface is bound to an abandoned window
        // even if `window`/size compare equal.
        let force_recreate = std::mem::take(&mut self.force_onscreen_recreate);
        let native_equivalent = self
            .installed_surface
            .as_ref()
            .is_some_and(|installed| installed.same_native_surface(target.as_ref()));
        let installed_size = self
            .canvases
            .get(&id)
            .map(|entry| (entry.physical_width, entry.physical_height));
        let install_policy = crate::canvas::classify_surface_install(
            recreate_kind,
            native_equivalent,
            installed_size,
            surface_size,
            self.contexts_2d.contains_key(&id),
            self.onscreen_2d_restore.is_some(),
            force_recreate,
        );

        // Same native window + same physical dimensions → skip destroy-recreate.
        if install_policy == crate::canvas::InstallPolicy::Skip {
            if let Some((exp_w, exp_h)) = surface_size {
                if let Some(entry) = self.canvases.get(&id) {
                    if matches!(entry.kind, SurfaceKind::Window)
                        && entry.physical_width == exp_w
                        && entry.physical_height == exp_h
                    {
                        tracing::info!(
                            "CanvasManager::create_onscreen skip recreate: window unchanged and size matched {}x{}",
                            exp_w,
                            exp_h
                        );
                        // EGL still references the installed target. The
                        // equivalent candidate is deliberately discarded; it
                        // must never replace native ownership under a live
                        // EGLSurface.
                        self.dpi = effective_pixel_ratio;
                        self.surface_unavailable = false;
                        return Ok(());
                    }
                }
            }
        }

        // R-5: same ANativeWindow but physical dimensions changed
        // (status bar hide/show, keyboard open/close, orientation
        // where the window handle is preserved).  Fast-path via
        // `resize_canvas`, which keeps the EGL context, Skia
        // DirectContext, every uploaded texture, and every
        // compiled program alive — the only work is rebuilding
        // the onscreen `SkSurface` and the DrawingBuffer FBO at
        // the new dimensions.  The previous path destroyed the
        // entire EGL surface + context (50-100 ms stall) on every
        // orientation change even when it wasn't needed.
        //
        // Skipped when:
        //   * surface_size is `None` (first create / recovery);
        //   * the window pointer changed (genuine surface recreate
        //     — EGLSurface is bound to the ANativeWindow that's
        //     gone);
        //   * there is no existing 2D context (first frame).
        if install_policy == crate::canvas::InstallPolicy::FastResize {
            if let Some((exp_w, exp_h)) = surface_size {
                if let Some(entry) = self.canvases.get(&id) {
                    if matches!(entry.kind, SurfaceKind::Window)
                        && self.contexts_2d.contains_key(&id)
                    {
                        let installed = self.installed_surface.as_ref().ok_or_else(|| {
                            SurfaceInstallFailure::from_phase(
                                ee(
                                    ErrorCode::RenderBackendError,
                                    "window canvas has no installed platform target",
                                ),
                                had_usable_previous,
                                InstallPhase::BeforePreviousInvalidation,
                                CandidateCleanup::NotRequired,
                            )
                        })?;
                        installed
                            .reconfigure_from(target.as_ref())
                            .map_err(|error| {
                                SurfaceInstallFailure::from_phase(
                                    error,
                                    had_usable_previous,
                                    InstallPhase::BeforePreviousInvalidation,
                                    CandidateCleanup::NotRequired,
                                )
                            })?;
                        tracing::info!(
                            "CanvasManager::create_onscreen fast resize {}x{} -> {}x{} (same native surface)",
                            entry.physical_width,
                            entry.physical_height,
                            exp_w,
                            exp_h,
                        );
                        let physical_w = exp_w.max(1);
                        let physical_h = exp_h.max(1);
                        // One rule, the same one the recreate path and a fresh
                        // install answer: a backing store the content chose is
                        // the content's and does not move because the window
                        // did (what a browser does, and what the JS half
                        // already promises by refusing to adopt a new size for
                        // such a canvas), while the engine's own default is
                        // re-derived from the surface it is meant to describe.
                        //
                        // This used to scale the buffer by the ratio it had to
                        // the OLD surface, which preserved neither: a canvas
                        // fixed at 960x640 (Phaser `Scale.NONE`) came back as
                        // some third size while `canvas.width` still read 960,
                        // so the content drew in coordinates its own buffer no
                        // longer had -- into a corner of it.
                        let (backing_w, backing_h) = self
                            .onscreen_backing_size((physical_w, physical_h), effective_pixel_ratio);
                        self.resize_canvas_for_surface_change(id, backing_w, backing_h)
                            .map_err(|error| {
                                SurfaceInstallFailure::from_phase(
                                    error,
                                    had_usable_previous,
                                    InstallPhase::PreviousInvalidated,
                                    CandidateCleanup::NotRequired,
                                )
                            })?;
                        if let Some(entry) = self.canvases.get_mut(&id) {
                            entry.physical_width = physical_w;
                            entry.physical_height = physical_h;
                        }
                        // `resize_canvas` may have returned early when the
                        // backing store already matched the new surface. The
                        // surface boundary still invalidates the old plan.
                        self.damage_history.clear();
                        self.pending_present_plan = None;
                        self.damage
                            .add(crate::damage_effect::DamageEffect::FullSurface);
                        self.evaluate_bypass();
                        // Keep `installed`: EGL references this exact platform
                        // object, which was reconfigured in place above. The
                        // candidate is discarded on return.
                        self.dpi = effective_pixel_ratio;
                        self.surface_unavailable = false;
                        return Ok(());
                    }
                }
            }
        }

        // Validate the client API while the previous presentation is still
        // untouched.  This is the last failure point allowed to report the
        // previous presentation as usable.
        self.egl.bind_api(egl::OPENGL_ES_API).map_err(|e| {
            SurfaceInstallFailure::from_phase(
                ee(
                    ErrorCode::RenderBackendError,
                    format!("eglBindAPI failed: {e:?}"),
                ),
                had_usable_previous,
                InstallPhase::BeforePreviousInvalidation,
                CandidateCleanup::NotRequired,
            )
        })?;

        self.context_lost = false;

        // Track whether a 2D context existed before destruction, so we can
        // re-initialize it after the new EGL context is created. This is
        // needed for Android resume: the surface is a different native window
        // but the game's JS code still expects canvas_id=1 to work.
        // The drawing state of that context, carried across the rebuild.
        //
        // JS shadows every Canvas2D state setter and skips sending a value it
        // believes is already current, so this state is the authoritative half
        // of a pair whose other half lives in the content. A context rebuilt at
        // spec defaults desynchronises them permanently: the content goes on
        // thinking its fillStyle is set, never re-sends it, and every later draw
        // paints with the default. That is opaque black on an opaque black
        // buffer -- a Canvas2D game returned from the background to a black
        // screen while JS drew every frame, the context was healthy, and every
        // boundary reported success. `Canvas2DContext::resize` preserves this
        // state for the same reason; the destroy-and-recreate path must too.

        if let Some(_entry) = self.canvases.get(&id) {
            // Destroy and recreate the EGL surface when the ANativeWindow
            // changed. On many Android drivers, eglQuerySurface returns the
            // creation-time dimensions and does NOT reflect later window
            // resizes (e.g. navigation bar hide/show). Reusing the old
            // surface leads to buffer size mismatches that SurfaceFlinger
            // rejects ("rejecting buffer"), causing flicker.
            self.destroy_onscreen_internal(id).map_err(|error| {
                SurfaceInstallFailure::from_phase(
                    error,
                    had_usable_previous,
                    InstallPhase::PreviousInvalidated,
                    CandidateCleanup::NotRequired,
                )
            })?;
        }

        // A newly created window surface may not inherit the previous swap
        // interval state on all drivers, so force the next swap to reapply it.
        self.last_swap_interval = -1;
        // New surface = new back buffers, old damage history is invalid.
        self.damage_history.clear();
        // The pending plan targets the abandoned surface — drop it too.
        self.pending_present_plan = None;

        let surf = target
            .create_window_surface(&self.egl, self.display, self.config)
            .map_err(|error| {
                SurfaceInstallFailure::from_phase(
                    error,
                    had_usable_previous,
                    InstallPhase::PreviousInvalidated,
                    CandidateCleanup::NotRequired,
                )
            })?;
        debug_assert!(self.pending_onscreen.is_none());
        self.pending_onscreen = Some(PendingOnscreenEgl {
            target,
            surface: surf,
            context: None,
            drawing_buffer: None,
        });

        // R-3: apply robust-context attribs when the driver supports
        // them so the onscreen context signals resets via
        // `glGetGraphicsResetStatus` instead of waiting for the
        // swap-buffers detection path.
        let ctx_attribs = egl_ops::build_ctx_attribs(self.gles_major, self.has_robust_context);
        let (ctx, preserved_drawing_buffer) = if let Some(preserved) = self.preserved_ctx.take() {
            tracing::info!(
                canvas_id = %id,
                has_robust = self.has_robust_context,
                "Reusing preserved EGL context for onscreen canvas"
            );
            // Context and DrawingBuffer are one recovery unit. Stage both
            // before the first fallible make-current so every cleanup path
            // can restore the exact pair for the next retry.
            (preserved, self.preserved_drawing_buffer.take())
        } else {
            debug_assert!(
                self.preserved_drawing_buffer.is_none(),
                "a preserved DrawingBuffer must never outlive its context"
            );
            tracing::info!(
                canvas_id = %id,
                has_robust = self.has_robust_context,
                gles_major = self.gles_major,
                "Creating fresh EGL context for onscreen canvas"
            );
            let context = match self.egl.create_context(
                self.display,
                self.config,
                Some(self.resource.ctx),
                &ctx_attribs,
            ) {
                Ok(context) => context,
                Err(e) => {
                    let error = ee(
                        ErrorCode::RenderBackendError,
                        format!("eglCreateContext(onscreen) failed: {e:?}"),
                    );
                    let cleanup = self.cleanup_pending_onscreen();
                    return Err(SurfaceInstallFailure::from_phase(
                        error,
                        had_usable_previous,
                        InstallPhase::CandidateReferenced,
                        cleanup,
                    ));
                }
            };
            (context, None)
        };
        let pending = self
            .pending_onscreen
            .as_mut()
            .expect("window EGLSurface must be staged before its context");
        pending.context = Some(ctx);
        pending.drawing_buffer = preserved_drawing_buffer;

        // Query EGL for diagnostics only. Some Android stacks report a rotated
        // size here (e.g. 1080x2340 while the Java SurfaceHolder reports
        // 2340x1080). For onscreen we trust the size from updateSurface when
        // provided, because JS canvas metrics and viewport logic must align with
        // SurfaceHolder dimensions.
        let queried_w = self
            .egl
            .query_surface(self.display, surf, egl::WIDTH)
            .unwrap_or(0)
            .max(1) as u32;
        let queried_h = self
            .egl
            .query_surface(self.display, surf, egl::HEIGHT)
            .unwrap_or(0)
            .max(1) as u32;

        let (physical_w, physical_h) = if let Some((exp_w, exp_h)) = surface_size {
            if exp_w != queried_w || exp_h != queried_h {
                tracing::warn!(
                    "CanvasManager::create_onscreen size mismatch: expected={}x{}, egl_surface={}x{}; using expected size",
                    exp_w,
                    exp_h,
                    queried_w,
                    queried_h
                );
            }
            (exp_w.max(1), exp_h.max(1))
        } else {
            (queried_w, queried_h)
        };

        // JS canvas.width/height report physical (buffer) pixels — matching
        // browser semantics.  Games that want crisp rendering multiply by
        // devicePixelRatio themselves; the engine must NOT scale again.
        let info = CanvasInfo {
            id,
            width: physical_w,
            height: physical_h,
            is_onscreen: true,
        };

        let pending = self
            .pending_onscreen
            .take()
            .expect("window EGL target must stay owned until CanvasEntry install");
        let installed_ctx = pending
            .context
            .expect("onscreen EGL context must exist before CanvasEntry install");
        self.canvases.insert(
            id,
            CanvasEntry {
                info,
                kind: SurfaceKind::Window,
                physical_width: physical_w,
                physical_height: physical_h,
                ctx: EglContextHandle {
                    ctx: installed_ctx,
                    // An onscreen canvas always has a real window surface; the
                    // Option exists for the surfaceless offscreen ones.
                    surf: Some(pending.surface),
                },
                // A preserved buffer moves with its context before the first
                // make-current. A fresh buffer is initialized below.
                drawing_buffer: pending.drawing_buffer,
                bypass_drawing_buffer: false, // evaluated after DrawingBuffer creation
                // A fresh context's default framebuffer is real FBO 0 until an
                // install or `create` points it somewhere; both record it there.
                applied_default_framebuffer: None,
            },
        );
        // A fresh native target carries no frame-rate request, so it is asserted
        // here rather than by each caller: this is the one place an install
        // commits, so every path through it -- first attach, resize, surface
        // recreate, context recovery -- inherits the request instead of
        // remembering to re-send it.
        let installed = self.installed_surface.insert(pending.target);
        installed.request_frame_rate(self.frame_rate_request);

        // Make current so GL calls work.
        if let Err(error) = self.make_current_needed(id) {
            let cleanup = self.cleanup_failed_onscreen_install(id);
            return Err(SurfaceInstallFailure::from_phase(
                error,
                had_usable_previous,
                InstallPhase::CandidateReferenced,
                cleanup,
            ));
        }

        // Attach the DrawingBuffer (intermediate FBO) for the onscreen canvas.
        // WebGL renders to this FBO; it gets blitted to the window surface on
        // swap.  If the previous Android surface was destroyed while the EGL
        // context was preserved, the DrawingBuffer GL objects are still valid
        // and should be reused.  Reusing them preserves the last frame and
        // avoids a black resume frame before Cocos schedules a new RAF draw.
        let (target_w, target_h) =
            self.onscreen_backing_size((physical_w, physical_h), effective_pixel_ratio);
        let mut staged_drawing_buffer = self
            .canvases
            .get_mut(&id)
            .and_then(|entry| entry.drawing_buffer.take());
        // The preserved buffer keeps its GL objects, and its *size* answers the
        // same question a fresh one does: a size the content chose is the
        // content's, and the engine's own default describes the surface it was
        // derived from -- this one. Keeping it unconditionally is what left a
        // canvas nobody sized describing the surface the app was suspended on,
        // upscaled by the blit, while `windowWidth` reported the real one.
        //
        // Reuse without a resize is still the common case (a resume at the same
        // size), and that is the one that preserves the last frame and avoids a
        // black resume frame; a surface that came back a different size has no
        // frame worth preserving anyway.
        if let Some(db) = staged_drawing_buffer.as_mut() {
            if (db.width, db.height) != (target_w, target_h) {
                match drawing_buffer::resize(&self.gl, db, target_w, target_h) {
                    Ok(()) => tracing::info!(
                        canvas_id = %id,
                        width = target_w,
                        height = target_h,
                        surface_width = physical_w,
                        surface_height = physical_h,
                        "Resized preserved DrawingBuffer to follow the recreated surface"
                    ),
                    Err(error) => {
                        // `resize` reallocates both attachments before it checks
                        // completeness, so a failure leaves the GL objects at the
                        // new size while the struct still reports the old one --
                        // and then every reader below (the entry, the viewport, JS
                        // `canvas.width`, the blit) describes storage that is not
                        // there. Discard it and let a fresh buffer be built at the
                        // size that was wanted: the preserved frame is the only
                        // loss, which is what a fresh buffer costs anyway.
                        tracing::error!(
                            canvas_id = %id,
                            "preserved DrawingBuffer resize to {target_w}x{target_h} failed, \
                             rebuilding it instead of reusing a buffer of unknown size: {error}"
                        );
                        if let Some(corrupted) = staged_drawing_buffer.take() {
                            drawing_buffer::destroy(&self.gl, corrupted);
                        }
                    }
                }
            } else {
                tracing::info!(
                    canvas_id = %id,
                    width = db.width,
                    height = db.height,
                    surface_width = physical_w,
                    surface_height = physical_h,
                    "Reusing preserved DrawingBuffer for onscreen canvas"
                );
            }
        }
        let drawing_buffer = if let Some(db) = staged_drawing_buffer {
            unsafe {
                self.gl.bind_framebuffer(
                    glow::FRAMEBUFFER,
                    default_framebuffer_of(
                        self.canvases
                            .get(&id)
                            .map_or(false, |e| e.bypass_drawing_buffer),
                        Some(db.fbo),
                    ),
                );
            }
            // The install re-points the driver, so the shadow has to say so. A
            // resume carries the previous surface's `gl_state` forward, and it can
            // still name a framebuffer the content bound before the surface went
            // away.
            crate::backend::gl::state_tracker::record_default_framebuffer_bind(
                self.gl_state.entry(id).or_default(),
            );
            Some(db)
        } else {
            None
        };
        // The onscreen canvas backing store == the DrawingBuffer. Track its
        // actual size in the entry so bypass evaluation (db vs surface) and JS
        // canvas.width/height stay consistent.
        let (backing_w, backing_h) = if let Some(db) = drawing_buffer {
            let (dbw, dbh) = (db.width, db.height);
            if let Some(entry) = self.canvases.get_mut(&id) {
                entry.info.width = dbw;
                entry.info.height = dbh;
                entry.drawing_buffer = Some(db);
            }
            (dbw, dbh)
        } else {
            match drawing_buffer::create(&self.gl, target_w, target_h) {
                Ok(db) => {
                    if let Some(entry) = self.canvases.get_mut(&id) {
                        entry.info.width = target_w;
                        entry.info.height = target_h;
                        entry.drawing_buffer = Some(db);
                    }
                    (target_w, target_h)
                }
                Err(e) => {
                    tracing::error!(
                        "DrawingBuffer creation failed, rendering direct to surface: {e}"
                    );
                    // Fallback: render directly to the window surface at its
                    // physical size (legacy behaviour; no scaling blit).
                    (physical_w, physical_h)
                }
            }
        };
        self.evaluate_bypass();

        // Reset default viewport/state for the newly created onscreen context.
        // Context recreation invalidates old GL state tracking. The default
        // framebuffer viewport covers the DrawingBuffer (backing store), not
        // the surface — the blit handles buffer -> surface scaling.
        unsafe {
            self.gl.viewport(0, 0, backing_w as i32, backing_h as i32);
        }
        self.gl_state.insert(
            id,
            CanvasGLState {
                current_program: None,
                viewport: Some((0, 0, backing_w as i32, backing_h as i32)),
                ..Default::default()
            },
        );

        // Resize any pre-existing Skia surface against the new DrawingBuffer
        // / FBO dimensions.  If the onscreen 2D context was torn down by the
        // destroy path above, re-initialise it here so JS targeting
        // canvas_id=1 keeps working through an Android surface recreate.
        let new_fbo = self
            .canvases
            .get(&id)
            .and_then(|e| e.drawing_buffer.as_ref())
            .map(|db| db.fbo.0.get())
            .unwrap_or(0);
        // Split-borrow: `contexts_2d` and `image_registry` are
        // disjoint fields, so the compiler lets us hand a mutable
        // reference to the image store into the resize path — the
        // context uses it to purge stale SkImage wrappers tied to
        // its outgoing GrDirectContext.
        let resized_ok = {
            let image_store = self.image_registry.store_mut();
            self.contexts_2d
                .get_mut(&id)
                .map(|ctx2d| ctx2d.resize(new_fbo, physical_w, physical_h, image_store))
                .unwrap_or(true)
        };
        if !resized_ok {
            // Same obligation as a teardown: this drops the context the content
            // still believes it holds.
            self.stash_onscreen_2d_restore(id);
            if let Some(tag) = self.drop_2d_context(id, true) {
                self.image_registry
                    .store_mut()
                    .purge_wrappers_for_context(tag);
            }
        }
        // Discharge whatever a teardown recorded -- this call's, or one from an
        // earlier `surfaceDestroyed` that has been waiting for the app to come
        // back to the foreground.
        let retained_2d_state = self.onscreen_2d_restore.take();
        if retained_2d_state.is_some() && (!self.contexts_2d.contains_key(&id) || !resized_ok) {
            if let Err(error) = context_2d_impl::init_skia_for_canvas(self, id) {
                let cleanup = self.cleanup_failed_onscreen_install(id);
                return Err(SurfaceInstallFailure::from_phase(
                    error,
                    had_usable_previous,
                    InstallPhase::CandidateReferenced,
                    cleanup,
                ));
            }
            // The fresh context starts at spec defaults; the content believes
            // its own values are still in force and will not re-send them.
            if let (Some(state), Some(ctx)) = (retained_2d_state, self.contexts_2d.get_mut(&id)) {
                ctx.adopt_drawing_state(state);
            }
        }

        self.dpi = effective_pixel_ratio;
        self.surface_unavailable = false;
        Ok(())
    }

    /// Release a partially-created candidate window surface.  `Released` is
    /// returned only after the resource context is current and EGL confirms
    /// destruction of the window surface.  A failed cleanup restores the slot
    /// verbatim so the matching Surface lease must remain retained.
    fn cleanup_pending_onscreen(&mut self) -> CandidateCleanup {
        let Some(mut pending) = self.pending_onscreen.take() else {
            return CandidateCleanup::NotRequired;
        };

        if let Err(error) = self.egl.make_current(
            self.display,
            self.resource.surf,
            self.resource.surf,
            Some(self.resource.ctx),
        ) {
            tracing::error!(
                target = ?pending.target,
                ?error,
                "cannot unbind partial onscreen EGLSurface; retaining target"
            );
            self.pending_onscreen = Some(pending);
            return CandidateCleanup::Failed;
        }
        self.bound = BoundContext::Resource;

        if let Err(error) = self.egl.destroy_surface(self.display, pending.surface) {
            tracing::error!(
                target = ?pending.target,
                ?error,
                "eglDestroySurface failed for partial onscreen target; retaining target"
            );
            self.pending_onscreen = Some(pending);
            return CandidateCleanup::Failed;
        }

        // The context does not reference the native window. Preserve it for a
        // retry after the window surface has been conclusively destroyed; this
        // also keeps the expensive share-group state alive across a transient
        // make-current/Skia failure.
        if let Some(context) = pending.context.take() {
            if self.preserved_ctx.is_none() {
                self.preserved_ctx = Some(context);
                debug_assert!(self.preserved_drawing_buffer.is_none());
                self.preserved_drawing_buffer = pending.drawing_buffer.take();
            } else if self.preserved_ctx == Some(context) {
                // Defensive merge for an idempotent retry. This state should
                // not normally occur on the single render thread.
                if self.preserved_drawing_buffer.is_none() {
                    self.preserved_drawing_buffer = pending.drawing_buffer.take();
                }
            } else {
                // Never replace a preserved context that may be paired with a
                // preserved DrawingBuffer. The rejected candidate context is
                // unreferenced after its window surface was destroyed.
                if let Some(db) = pending.drawing_buffer.take() {
                    if self
                        .egl
                        .make_current(
                            self.display,
                            self.resource.surf,
                            self.resource.surf,
                            Some(context),
                        )
                        .is_ok()
                    {
                        drawing_buffer::destroy(&self.gl, db);
                    }
                    let _ = self.egl.make_current(
                        self.display,
                        self.resource.surf,
                        self.resource.surf,
                        Some(self.resource.ctx),
                    );
                    self.bound = BoundContext::Resource;
                }
                let _ = self.egl.destroy_context(self.display, context);
            }
        }
        CandidateCleanup::Released
    }

    fn cleanup_failed_onscreen_install(&mut self, id: CanvasId) -> CandidateCleanup {
        match self.destroy_onscreen_internal(id) {
            Ok(()) => CandidateCleanup::Released,
            Err(error) => {
                tracing::error!(
                    canvas_id = %id,
                    %error,
                    "failed to detach rejected onscreen EGL target; retaining target"
                );
                CandidateCleanup::Failed
            }
        }
    }

    /// Rebuild a canvas's 2D context, carrying its drawing state across.
    ///
    /// The state is the load-bearing half. JS de-duplicates every setter
    /// against a shadow (`if (this._fillStyle === value) return;`) that nothing
    /// clears, and Canvas2D has no context-loss event for content to react to,
    /// because browsers restore 2D contexts transparently and no engine listens
    /// for one. A context rebuilt at spec defaults is therefore permanently
    /// desynchronised from the content, silently: fills paint opaque black and
    /// no layer reports an error.
    ///
    /// Every drop-and-re-create pair goes through here so the sequence exists
    /// once. `Canvas2DContext::resize` already preserves the state on its happy
    /// path; this is the same promise for the path where that resize fails.
    fn rebuild_2d_context_preserving_state(&mut self, id: CanvasId) -> EngineResult<()> {
        let state = self.contexts_2d.get(&id).map(|ctx| ctx.drawing_state());
        if let Some(tag) = self.drop_2d_context(id, true) {
            self.image_registry
                .store_mut()
                .purge_wrappers_for_context(tag);
        }
        context_2d_impl::init_skia_for_canvas(self, id)?;
        if let (Some(state), Some(ctx)) = (state, self.contexts_2d.get_mut(&id)) {
            ctx.adopt_drawing_state(state);
        }
        Ok(())
    }

    /// The backing store the onscreen canvas must have on a surface of
    /// `surface` physical pixels.
    ///
    /// The one place the ownership rule is applied, so the three installs that
    /// have to agree — a fresh create, a same-surface resize, and a
    /// destroy-and-recreate — cannot drift apart. That drift is the defect this
    /// exists to remove: the recreate path kept the preserved buffer at the size
    /// derived from the surface the app was suspended on, so a canvas the content
    /// never sized came back from a rotation still describing a portrait window.
    fn onscreen_backing_size(&self, surface: (u32, u32), pixel_ratio: PixelRatio) -> (u32, u32) {
        self.onscreen_content_backing
            .unwrap_or_else(|| engine_default_backing(surface, pixel_ratio.get()))
    }

    /// Resize a canvas because the *surface* moved, not because the content
    /// asked it to.
    ///
    /// The distinction decides what happens to the 2D drawing state, and the
    /// two answers are opposites:
    ///
    ///   * `canvas.width = N` — the spec resets the context, and the JS setters
    ///     reset their shadow to match. `resize_canvas` doing the same is
    ///     correct.
    ///   * the platform hands us a different surface — the content did not ask
    ///     for anything and nothing about its context was reset, so the state
    ///     has to survive.
    ///
    /// `resize_canvas` cannot tell the two apart, and its rebuild comes up at
    /// spec defaults either way. Left alone here, a content that set its fill
    /// style once at start-up would draw in opaque black from the first system
    /// bar hide onwards: the JS setters de-duplicate against a shadow no
    /// surface change clears, so it believes the value is already in force and
    /// will never send it again. Same invariant as `ShareGroupRestorePlan` and
    /// `stash_onscreen_2d_restore`, reached by a third route.
    fn resize_canvas_for_surface_change(
        &mut self,
        id: CanvasId,
        width: u32,
        height: u32,
    ) -> EngineResult<()> {
        let state = self.contexts_2d.get(&id).map(|ctx| ctx.drawing_state());
        self.resize_canvas(id, Some(width), Some(height), BackingSizeOwner::Engine)?;
        if let (Some(state), Some(ctx)) = (state, self.contexts_2d.get_mut(&id)) {
            ctx.adopt_drawing_state(state);
        }
        Ok(())
    }

    /// Record what a future surface install owes this canvas.
    ///
    /// Only ever sets, never clears: the obligation outlives any number of
    /// teardowns and is discharged by the install that rebuilds the context.
    fn stash_onscreen_2d_restore(&mut self, id: CanvasId) {
        if let Some(ctx) = self.contexts_2d.get(&id) {
            self.onscreen_2d_restore = Some(ctx.drawing_state());
        }
    }

    fn destroy_onscreen_internal(&mut self, id: CanvasId) -> EngineResult<()> {
        // Before anything is dropped: whoever installs the next surface has to
        // know this canvas had a 2D context, and with which drawing state.
        self.stash_onscreen_2d_restore(id);
        if let Some(mut entry) = self.canvases.remove(&id) {
            let skia_ctx_current = self
                .egl
                .make_current(
                    self.display,
                    entry.ctx.surf,
                    entry.ctx.surf,
                    Some(entry.ctx.ctx),
                )
                .is_ok();

            // Capture the context's `ctx_tag` BEFORE dropping it so we
            // can purge matching SkImage wrappers from the shared
            // image store.  Without this step those wrappers hold
            // `GrDirectContext` pointers that are about to be
            // destroyed; the entries would never be reclaimed
            // (sk_image_cache has no LRU / size cap) and the
            // dangling pointers would be a correctness landmine if
            // any future caller hit the cache for the recycled tag.
            let ctx_tag = self.drop_2d_context(id, skia_ctx_current);
            if let Some(tag) = ctx_tag {
                self.image_registry
                    .store_mut()
                    .purge_wrappers_for_context(tag);
            }
            // Rebalance Skia resource-cache caps now that one
            // fewer context is sharing the aggregate budget.
            for ctx in self.contexts_2d.values_mut() {
                ctx.rebalance_resource_cache();
            }
            self.dirty_2d.remove(&id);
            self.gl_state.remove(&id);

            self.image_registry.remove_canvas_images(id);

            // Switch to the resource (pbuffer) context so the ANativeWindow is
            // properly disconnected before we destroy the onscreen surface.
            if let Err(error) = self.egl.make_current(
                self.display,
                self.resource.surf,
                self.resource.surf,
                Some(self.resource.ctx),
            ) {
                self.canvases.insert(id, entry);
                self.evaluate_bypass();
                return Err(ee(
                    ErrorCode::RenderBackendError,
                    format!("eglMakeCurrent(resource) before onscreen detach failed: {error:?}"),
                ));
            }
            self.bound = BoundContext::Resource;
            // An onscreen canvas always has a window surface, so this is not a
            // conditional teardown -- it is the same unconditional one, written
            // to keep the destroy-then-check ordering that
            // `onscreen_detach_checks_egl_destroy_before_releasing_native_ownership`
            // pins. Releasing native ownership before the destroy is checked is
            // the bug that guard exists for.
            if let Some(onscreen_surf) = entry.ctx.surf {
                if let Err(error) = self.egl.destroy_surface(self.display, onscreen_surf) {
                    self.canvases.insert(id, entry);
                    self.evaluate_bypass();
                    return Err(ee(
                        ErrorCode::RenderBackendError,
                        format!("eglDestroySurface(onscreen) failed: {error:?}"),
                    ));
                }
            }
            self.installed_surface = None;
            // Preserve the context for reuse on the next create_onscreen().
            // This avoids losing GL state (textures, shaders) across
            // Android surface destroy/recreate cycles (pause/resume).
            //
            // Also preserve the DrawingBuffer when the context is preserved:
            // it is an offscreen FBO/texture owned by the context/share group,
            // not by the outgoing ANativeWindow.  Dropping it here created a
            // fresh empty black FBO on resume; if JS didn't redraw immediately
            // the user saw a black screen.  Keeping it matches Chromium-style
            // DrawingBuffer lifecycle: surface loss detaches presentation, not
            // the retained drawing target.
            let old_preserved_ctx = self.preserved_ctx.take();
            if let Some(old_db) = self.preserved_drawing_buffer.take() {
                if let Some(old_ctx) = old_preserved_ctx {
                    // Delete the old DrawingBuffer while its owning context is
                    // current, then switch away before destroying that context.
                    let _ = self.egl.make_current(
                        self.display,
                        self.resource.surf,
                        self.resource.surf,
                        Some(old_ctx),
                    );
                    drawing_buffer::destroy(&self.gl, old_db);
                    let _ = self.egl.make_current(
                        self.display,
                        self.resource.surf,
                        self.resource.surf,
                        Some(self.resource.ctx),
                    );
                    let _ = self.egl.destroy_context(self.display, old_ctx);
                } else {
                    // Should not happen: DrawingBuffer and context are paired.
                    // Destroy against the current resource context as a best
                    // effort so we do not leak the GL objects.
                    drawing_buffer::destroy(&self.gl, old_db);
                }
            } else if let Some(old_ctx) = old_preserved_ctx {
                let _ = self.egl.destroy_context(self.display, old_ctx);
            }
            let preserved_db = entry.drawing_buffer.take();
            self.preserved_ctx = Some(entry.ctx.ctx);
            if let Some(db) = preserved_db {
                tracing::info!(
                    width = db.width,
                    height = db.height,
                    "Preserved DrawingBuffer across onscreen surface destruction"
                );
                self.preserved_drawing_buffer = Some(db);
            }

            self.last_swap_interval = -1;
            self.damage_history.clear();
            self.pending_present_plan = None;
        }
        // Onscreen destroyed → bypass must be off until re-created.
        self.evaluate_bypass();
        Ok(())
    }

    /// Returns true if EGL reported context loss.
    /// The render thread should attempt recovery on the next frame.
    #[inline]
    pub(crate) fn is_context_lost(&self) -> bool {
        self.context_lost
    }

    #[inline]
    pub(crate) fn is_surface_unavailable(&self) -> bool {
        self.surface_unavailable
    }

    /// Cold-path readiness for rebuilding a lost share group. Platform
    /// preparation is never repeated here: the installed target is the stable
    /// non-owning descriptor paired with RenderSurfaceBinding's live lease.
    #[inline]
    pub(crate) fn is_surface_recovery_ready(&self) -> bool {
        self.installed_surface.is_some()
    }

    /// Low-memory signal handler (P1-12).  Called by the host
    /// runtime in response to an Android `onTrimMemory`
    /// notification or any equivalent platform signal.  Squeezes
    /// every live `Canvas2DContext` to
    /// [`crate::backend::gl::surface::low_memory_per_ctx_bytes`],
    /// which is what makes Skia release, and restores the ordinary
    /// share in the same call.  Also runs a deferred-resource purge
    /// so Skia can drop atlas / glyph entries the squeeze freed.
    ///
    /// The signal arrives per Session — the host relays one
    /// `onTrimMemory` once for each — so what it must not do is
    /// leave anything behind: this used to store 16 MiB as *the*
    /// process budget, which only engine init raised again, so one
    /// game's warning capped every other game's canvases for the
    /// life of the process.
    pub(crate) fn on_trim_memory(&mut self) {
        for ctx in self.contexts_2d.values_mut() {
            ctx.trim_resource_cache();
        }
        self.perform_deferred_cleanup_all(std::time::Duration::from_millis(200));
    }

    /// Purge only Skia resources older than `unused_age` in every live 2D
    /// context. The render thread calls this after a coalesced cadence decision
    /// or an explicit memory-pressure edge; it never schedules work itself.
    pub(crate) fn perform_deferred_cleanup_all(&mut self, unused_age: std::time::Duration) {
        for ctx in self.contexts_2d.values_mut() {
            ctx.perform_deferred_cleanup(unused_age);
        }
    }

    /// Poll `glGetGraphicsResetStatus` (R-3).  Returns `true` when
    /// the driver reports a reset, in which case the caller should
    /// treat the GL context as dead and trigger recovery.  A `None`
    /// function pointer (driver without `GL_KHR_robustness`) falls
    /// through to `false` so legacy behaviour is preserved.
    ///
    /// Glazed-over `u32` return matches the raw
    /// `glGetGraphicsResetStatus` ABI — `GL_NO_ERROR = 0` means
    /// healthy, anything else (`GL_GUILTY_CONTEXT_RESET = 0x8253`,
    /// `GL_INNOCENT_CONTEXT_RESET = 0x8254`,
    /// `GL_UNKNOWN_CONTEXT_RESET = 0x8255`) indicates loss.  The
    /// probing itself is cheap: a single driver function pointer
    /// call per frame.
    pub(crate) fn check_graphics_reset_status(&mut self) -> bool {
        // Debug one-shot injection (WEBGL_lose_context): report a reset once so
        // the real detection -> recovery pipeline runs on demand. Consumed here
        // so it fires for exactly one frame, mirroring a real driver reset.
        if self.simulated_reset {
            self.simulated_reset = false;
            tracing::warn!("Simulated GL context reset (WEBGL_lose_context.loseContext)");
            self.context_lost = true;
            return true;
        }
        let Some(poll) = self.gl_get_graphics_reset_status_fn else {
            return false;
        };
        // SAFETY: `poll` was resolved via `eglGetProcAddress` under
        // a GL context of the same share group.  `glGetGraphicsResetStatus`
        // reads driver-internal state only; no preconditions on the
        // currently-bound context or thread beyond "a GL context is
        // current", which is guaranteed by the render-thread loop
        // calling this helper after `make_current`.
        let status = unsafe { poll() };
        if status != 0 {
            tracing::warn!("GL_KHR_robustness reports context reset: status=0x{status:04X}");
            self.context_lost = true;
            true
        } else {
            false
        }
    }

    /// Arm a one-shot simulated context reset (debug trigger for
    /// `WEBGL_lose_context.loseContext()`). The next
    /// `check_graphics_reset_status()` poll consumes it and drives the real
    /// loss -> recovery path. No-op-safe to call repeatedly.
    pub(crate) fn request_simulated_reset(&mut self) {
        self.simulated_reset = true;
    }

    /// Mark every live Skia `DirectContext` as abandoned.  After
    /// this call Skia will neither flush nor `glDelete*` any
    /// resource — subsequent `Drop` runs become no-ops and can't
    /// crash the driver.  Called when the render loop detects
    /// EGL context loss and knows the underlying GL objects are
    /// already invalid.  Mirrors Slint's `Drop for OpenGLSurface`
    /// pattern
    /// (`internal/renderers/skia/opengl_surface.rs:515-525`)
    /// which calls `GrDirectContext::abandon()` when
    /// `make_current` fails.  Without this, dropping a
    /// `Canvas2DContext` on a lost EGL context produces
    /// `GL_INVALID_OPERATION` storms in logcat and on some
    /// Adreno / Mali drivers crashes the renderer thread.
    /// Total number of canvases (onscreen + offscreen) currently
    /// tracked by the manager.  Cheap counter read, mainly used
    /// by lifecycle diagnostic logs (`Pause` / `Resume`) to
    /// correlate field reports with the amount of GPU state the
    /// engine is holding at the time.
    #[inline]
    pub(crate) fn canvas_count(&self) -> usize {
        self.canvases.len()
    }

    pub(crate) fn abandon_all_2d_contexts(&mut self) {
        for ctx in self.contexts_2d.values_mut() {
            ctx.abandon();
        }
    }

    /// Fail every responder the manager is holding on behalf of a
    /// JS-side sync op with `ErrorCode::ContextLost`.
    ///
    /// Called by the render thread the moment `swap_buffers` sets
    /// `context_lost = true`.  Without this sweep, a call like
    /// `Image.src = ...` whose `LoadImage` response is still sitting
    /// in `pending_load_responses` would time out the full 10 s
    /// `COMMAND_TIMEOUT_MS` before the host learns the context
    /// vanished — with the sweep it fails immediately and the JS
    /// game code can choose to retry on the next surface.  The
    /// guarantee is "every responder we track is honoured exactly
    /// once" — Drop-safety of `RenderCmdResp` turns any responder
    /// we might miss into a structured `Internal` error instead of
    /// a silent disconnect.
    pub(crate) fn fail_pending_sync_responders(&mut self, reason: &str) -> usize {
        use shared::error::EngineError;

        let mut drained = 0usize;
        let take_pending = std::mem::take(&mut self.pending_load_responses);
        for (image_id, resp) in take_pending {
            drained += 1;
            let err = EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("image upload aborted")
                .with_detail(format!("image_id={image_id}: {reason}"));
            resp.err(err);
        }
        let take_deferred = std::mem::take(&mut self.deferred_uploads);
        for pending in take_deferred {
            drained += 1;
            let err = EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("deferred upload aborted")
                .with_detail(format!("image_id={}: {reason}", pending.image_id));
            pending.resp.err(err);
        }

        // Drop in-flight async uploads too. Their textures + fences were
        // created on the now-lost context: the fences can never signal, so
        // `drain_upload_completed` would re-queue each entry forever
        // (leaking the GL texture + fence) and their upload budget would
        // never be released, eventually wedging all future uploads. Delete
        // the (already-dead) GL objects best-effort and, crucially, release
        // the byte budget locally.
        let take_uploads = std::mem::take(&mut self.pending_uploads);
        for c in take_uploads {
            unsafe {
                self.gl.delete_texture(c.texture);
                self.gl.delete_sync(c.fence);
            }
            if let Some(ref mut server) = self.upload_server {
                server.finish_job_bytes(c.byte_len);
            }
            self.cancelled_uploads.remove(&c.image_id);
            drained += 1;
        }
        drained
    }

    /// Attempt to recover from EGL context loss by re-creating the
    /// onscreen surface using the last known window handle.
    /// Returns Ok(true) if recovery succeeded, Ok(false) if no window
    /// handle is available, or Err on failure.
    ///
    /// A real `EGL_CONTEXT_LOST` / GPU reset invalidates the *entire share
    /// group* — the resource context, every canvas context, the upload-thread
    /// context, and every GL object created through them. Recovery therefore
    /// tears the whole share group down and rebuilds it from scratch, rather
    /// than reusing the (dead) preserved onscreen context:
    ///
    ///   1. Drop all GL-object bookkeeping WITHOUT calling `glDelete` — the
    ///      handles name objects in a context that no longer exists, so a
    ///      delete is at best a no-op and at worst a driver error. EGL-level
    ///      `destroy_context` / `destroy_surface` on the (still-allocated but
    ///      lost) handles is safe and reclaims the EGL objects.
    ///   2. Rebuild the resource pbuffer context, respawn the upload thread
    ///      (shares the new resource context), and recreate the onscreen
    ///      canvas with a fresh context (`preserved_ctx` is cleared so
    ///      `create_onscreen` cannot reuse the dead one).
    ///   3. Re-create every other canvas JS still holds an id for — the
    ///      onscreen 2D (Skia) context and all offscreen canvases — from the
    ///      [`ShareGroupRestorePlan`] the teardown returned. These have no
    ///      lazy-rebuild path: JS registers a canvas exactly once, so anything
    ///      recovery drops is stranded for the life of the process. (The atlas
    ///      and image registry do rebuild lazily.)
    ///   4. Probe the rebuilt context with a trivial clear + `glGetError` +
    ///      `glGetGraphicsResetStatus`. Only a passing probe returns `Ok(true)`
    ///      so the caller can honestly report `ContextRecovered { success }`.
    ///
    /// The game's own WebGL resources (programs/buffers/textures) are gone; the
    /// game rebuilds them in its `webglcontextrestored` handler. Until then
    /// `isContextLost()` stays true and the caller keeps the JS-visible flag
    /// set (see `render_thread` / `host`).
    pub(crate) fn try_recover_context(&mut self) -> EngineResult<bool> {
        if !self.context_lost {
            return Ok(false);
        }
        // A previous recovery attempt may have failed after creating a window
        // EGLSurface. Retry its proof-producing cleanup before touching the
        // share group. Failed cleanup retains the target and defers recovery.
        if self.pending_onscreen.is_some()
            && self.cleanup_pending_onscreen() != CandidateCleanup::Released
        {
            tracing::warn!("Cannot recover EGL context: partial window target is still retained");
            return Ok(false);
        }
        let Some(target) = self.installed_surface.clone() else {
            tracing::warn!("Cannot recover EGL context: no prepared surface target available");
            return Ok(false);
        };
        tracing::warn!("EGL context loss: tearing down and rebuilding the share group");

        let onscreen_id = CanvasId::from(1u32);

        // ---- Phase 1: hard teardown of the dead share group ----
        // Records the JS-visible canvases the teardown just destroyed; Phase 2
        // owes their re-creation.
        //
        // Parked on `self` rather than held in a local. Everything below can
        // fail, and a plan that goes out of scope with a local strands every
        // canvas it recorded — see `pending_share_group_restore`. A retry
        // resumes the debt recorded here instead of tearing down an
        // already-empty registry.
        if self.pending_share_group_restore.is_none() {
            let plan = self.tear_down_share_group();
            self.pending_share_group_restore = Some(plan);
        } else {
            tracing::warn!(
                "EGL recovery: resuming the share-group restore a previous attempt \
                 could not settle"
            );
        }

        // ---- Phase 2: rebuild the share group ----
        let (resource_ctx, resource_surf) = egl_ops::create_pbuffer_context(
            &self.egl,
            self.display,
            self.config,
            None,
            16,
            16,
            self.gles_major,
            self.has_robust_context,
            self.surfaceless,
        )?;
        self.egl.track_resource(resource_ctx, resource_surf);
        self.resource = EglContextHandle {
            ctx: resource_ctx,
            surf: resource_surf,
        };
        self.bind_resource()?;

        // Respawn the async upload thread on TierA (shares the new resource ctx).
        if self.device_caps.tier() == crate::device_caps::DeviceTier::TierA {
            self.upload_thread = crate::upload_thread::UploadThreadHandle::try_spawn(
                std::sync::Arc::clone(&self.egl_provider),
                &self.egl,
                self.display,
                self.config,
                self.resource.ctx,
                self.gles_major,
                self.has_robust_context,
                self.surfaceless,
            );
            if self.upload_thread.is_some() {
                self.upload_server = Some(upload_server_for_device(&self.device_caps));
            }
        }
        // Recreate every canvas the teardown destroyed and probe, all inside one
        // fallible block. `context_lost` must NOT be cleared until the ENTIRE
        // sequence succeeds: `create_onscreen` clears it internally (line ~788)
        // and any later `?` failure or a failed probe would otherwise leave the
        // manager's flag `false` while the render thread's shared atomic is still
        // `true` — a split-brain that removes the stable basis for the next
        // recovery retry. So on ANY failure (Err or a failed probe) we force
        // `context_lost = true`; only full success leaves it `false`.
        let rebuilt = (|| -> EngineResult<bool> {
            // `canvases` is empty and `preserved_ctx` is None, so
            // `create_onscreen` builds a fresh onscreen context.
            self.create_onscreen(target, RecreateKind::SameGeneration, None, false, None)
                .map_err(|failure| failure.error)?;
            // Settle the teardown's debt: the onscreen 2D context plus every
            // offscreen canvas whose id JS still holds.
            //
            // Taken out to satisfy the borrow, and put straight back if the
            // restore fails: `restore_share_group` uses `?` per canvas, so a
            // failure part-way leaves the untouched tail owed. Dropping the plan
            // here would strand exactly those.
            let plan = self
                .pending_share_group_restore
                .take()
                .expect("Phase 1 parks a plan before any fallible step");
            match self.restore_share_group(&plan, onscreen_id) {
                Ok(()) => {}
                Err(e) => {
                    self.pending_share_group_restore = Some(plan);
                    return Err(e);
                }
            }
            // ---- Phase 3: probe the rebuilt context for real usability ----
            Ok(self.probe_context_usable(onscreen_id))
        })();

        match rebuilt {
            Ok(true) => {
                self.context_lost = false;
                tracing::info!("EGL context share group rebuilt and probed OK");
                Ok(true)
            }
            Ok(false) => {
                self.context_lost = true;
                tracing::error!("EGL recovery: probe draw failed, context still unusable");
                Ok(false)
            }
            Err(e) => {
                self.context_lost = true;
                tracing::error!("EGL recovery failed during rebuild: {e}");
                Err(e)
            }
        }
    }

    /// Destroy the dead share group and report what the rebuild owes the game.
    ///
    /// No `glDelete` — the objects live in a context that is gone. Drop the
    /// Rust-side handles and release EGL objects (`destroy_*` is safe on a
    /// lost-but-allocated context).
    ///
    /// The returned [`ShareGroupRestorePlan`] is the whole point of the split:
    /// canvas identity is owned by JS and survives the GPU state, so a teardown
    /// that is not followed by a matching restore silently strands live JS
    /// handles. Returning the plan makes that obligation explicit at the type
    /// level instead of leaving it to whoever edits the recovery path next.
    fn tear_down_share_group(&mut self) -> ShareGroupRestorePlan {
        // Snapshot before anything is dropped: the live registry is the single
        // source of truth for canvas identity and current size.
        let plan = plan_share_group_restore(
            self.canvases.iter().map(|(id, entry)| {
                (
                    *id,
                    matches!(entry.kind, SurfaceKind::Pbuffer),
                    entry.physical_width,
                    entry.physical_height,
                )
            }),
            |id| self.contexts_2d.get(&id).map(|ctx| ctx.drawing_state()),
        );

        // Skia contexts: abandoned on the loss path; drop every one so they
        // rebuild against the new share group. `abandon()` makes Drop a no-op.
        for (_id, mut ctx) in self.contexts_2d.drain().collect::<Vec<_>>() {
            ctx.abandon();
        }
        self.dirty_2d.clear();

        // Every canvas's EGL surface + context (onscreen + offscreen).
        for (_id, entry) in std::mem::take(&mut self.canvases) {
            if let Some(surf) = entry.ctx.surf {
                self.egl.destroy_surface(self.display, surf).ok();
            }
            self.egl.destroy_context(self.display, entry.ctx.ctx).ok();
        }
        // Preserved onscreen ctx / DrawingBuffer from a prior resume are dead.
        if let Some(c) = self.preserved_ctx.take() {
            self.egl.destroy_context(self.display, c).ok();
        }
        self.preserved_drawing_buffer.take();

        // Upload thread shares the dead resource context — stop it (Drop joins
        // the thread; it does no GL in Drop) before we destroy that context.
        drop(self.upload_thread.take());
        self.upload_server.take();
        self.pending_uploads.clear();
        self.cancelled_uploads.clear();

        // Resource (root) context + surface.
        if let Some((resource_ctx, resource_surf)) = self.egl.untrack_resource() {
            self.egl.make_current(self.display, None, None, None).ok();
            if let Some(resource_surf) = resource_surf {
                self.egl.destroy_surface(self.display, resource_surf).ok();
            }
            self.egl.destroy_context(self.display, resource_ctx).ok();
        }

        // Drop all GL-object bookkeeping (invalid handles; no glDelete).
        self.programs.clear();
        self.shaders.clear();
        self.buffers.clear();
        self.textures.clear();
        self.framebuffers.clear();
        self.renderbuffers.clear();
        self.webgl_gpu_budget.clear();
        self.vaos.clear();
        self.queries.clear();
        self.transform_feedbacks.clear();
        self.image_copy_fbos.clear();
        self.gl_state.clear();
        self.atlas = None;
        self.image_registry = ImageRegistry::new();
        self.damage_history.clear();
        self.pending_present_plan = None;
        self.last_swap_interval = -1;

        plan
    }

    /// Re-create the canvases a [`ShareGroupRestorePlan`] recorded, into the
    /// share group Phase 2 has just rebuilt.
    ///
    /// The onscreen canvas is already back (it needs the window target, so
    /// `create_onscreen` owns it); this restores its Canvas2D context plus
    /// every offscreen canvas. Their pixels are gone — same as a browser after
    /// a GPU reset — but the ids resolve again, which is what keeps the game
    /// running.
    ///
    /// Fails loudly: a partially populated share group is not a recovered one,
    /// so the caller's all-or-nothing contract keeps `context_lost` set and the
    /// render thread retries the whole recovery.
    fn restore_share_group(
        &mut self,
        plan: &ShareGroupRestorePlan,
        onscreen_id: CanvasId,
    ) -> EngineResult<()> {
        // A rebuilt context starts at spec defaults, and the content will not
        // re-send what it believes is still in force -- the JS setters
        // de-duplicate against a shadow no GPU reset clears. Restoring the
        // state is as load-bearing as restoring the context.
        if let Some(state) = plan.onscreen_2d.clone() {
            context_2d_impl::init_skia_for_canvas(self, onscreen_id)?;
            if let Some(ctx) = self.contexts_2d.get_mut(&onscreen_id) {
                ctx.adopt_drawing_state(state);
            }
        }
        for spec in &plan.offscreen {
            self.register_offscreen(spec.id, spec.width, spec.height)?;
            if let Some(state) = spec.state_2d.clone() {
                context_2d_impl::init_skia_for_canvas(self, spec.id)?;
                if let Some(ctx) = self.contexts_2d.get_mut(&spec.id) {
                    ctx.adopt_drawing_state(state);
                }
            }
        }
        if !plan.offscreen.is_empty() {
            tracing::info!(
                "EGL recovery re-registered {} offscreen canvas(es)",
                plan.offscreen.len()
            );
        }
        Ok(())
    }

    /// Probe whether a freshly rebuilt onscreen context is actually usable:
    /// bind it, drain stale errors, issue a trivial clear, and check both
    /// `glGetError` and (when available) `glGetGraphicsResetStatus`. Returns
    /// `false` on any failure so recovery can report an honest `success`.
    fn probe_context_usable(&mut self, id: CanvasId) -> bool {
        if self.make_current_needed(id).is_err() {
            return false;
        }
        unsafe {
            for _ in 0..8 {
                if self.gl.get_error() == glow::NO_ERROR {
                    break;
                }
            }
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
            if self.gl.get_error() != glow::NO_ERROR {
                return false;
            }
        }
        if let Some(poll) = self.gl_get_graphics_reset_status_fn {
            if unsafe { poll() } != 0 {
                return false;
            }
        }
        true
    }

    pub(crate) fn destroy_canvas(&mut self, id: CanvasId) -> EngineResult<()> {
        // The content threw this canvas away; a later surface install owes it
        // nothing. Leaving the obligation would rebuild a context for a canvas
        // that no longer exists.
        if id == CanvasId::from(1u32) {
            self.onscreen_2d_restore = None;
        }
        shared::ensure!(
            id != 1,
            ErrorCode::InvalidArgument,
            "cannot destroy onscreen canvas"
        );

        if let Some(entry) = self.canvases.remove(&id) {
            let saved_bound = self.bound;
            let skia_ctx_current = self
                .egl
                .make_current(
                    self.display,
                    entry.ctx.surf,
                    entry.ctx.surf,
                    Some(entry.ctx.ctx),
                )
                .is_ok();
            if skia_ctx_current {
                self.bound = BoundContext::Canvas(id);
            }

            // Same SkImage-wrapper purge pattern as the onscreen
            // destroy path: capture ctx_tag before removing.
            let ctx_tag = self.drop_2d_context(id, skia_ctx_current);
            if skia_ctx_current {
                let _ = self.bind_resource();
            }
            if let Some(surf) = entry.ctx.surf {
                self.egl.destroy_surface(self.display, surf).ok();
            }
            self.egl.destroy_context(self.display, entry.ctx.ctx).ok();
            if let Some(tag) = ctx_tag {
                self.image_registry
                    .store_mut()
                    .purge_wrappers_for_context(tag);
            }
            // Rebalance Skia caches now that the denominator changed.
            for ctx in self.contexts_2d.values_mut() {
                ctx.rebalance_resource_cache();
            }
            self.dirty_2d.remove(&id);
            self.gl_state.remove(&id);

            // This offscreen WebGL context is actually gone (unlike an
            // onscreen surface detach, which preserves its context). Delete
            // its share-group objects and return resident-byte charges.
            let texture_ids = self
                .textures
                .iter()
                .filter_map(|(texture_id, meta)| {
                    (meta.owner_canvas == Some(id)).then_some(*texture_id)
                })
                .collect::<Vec<_>>();
            for texture_id in texture_ids {
                if let Some(meta) = self.textures.remove(&texture_id) {
                    if let Some(handle) = meta.gl_handle {
                        unsafe { self.gl.delete_texture(handle) };
                    }
                }
                self.webgl_gpu_budget.delete_texture(texture_id);
            }
            let renderbuffer_ids = self
                .renderbuffers
                .iter()
                .filter_map(|(renderbuffer_id, meta)| {
                    (meta.owner_canvas == Some(id)).then_some(*renderbuffer_id)
                })
                .collect::<Vec<_>>();
            for renderbuffer_id in renderbuffer_ids {
                if let Some(meta) = self.renderbuffers.remove(&renderbuffer_id) {
                    if let Some(handle) = meta.gl_handle {
                        unsafe { self.gl.delete_renderbuffer(handle) };
                    }
                }
                self.webgl_gpu_budget.delete_renderbuffer(renderbuffer_id);
            }
            self.webgl_gpu_budget.release_context(id);

            // Release the GPU-copy FBO for this canvas.  FBO names
            // are context-local, so it would be unusable after the
            // owning canvas is gone.
            if let Some(fbo) = self.image_copy_fbos.remove(&id) {
                unsafe { self.gl.delete_framebuffer(fbo) };
            }

            self.image_registry.remove_canvas_images(id);
            let _ = self.restore_bound(saved_bound);
        }
        // Canvas destroyed → re-evaluate (may re-enable bypass).
        self.evaluate_bypass();
        Ok(())
    }

    /// Idempotent owner teardown used by every explicit exit and by `Drop`.
    ///
    /// Order is intentional: the upload worker may still use the shared
    /// display; Skia/GL objects need a live context; window/offscreen EGL
    /// objects must go before the resource context; display termination is the
    /// final authority that releases any driver-retained native window.
    ///
    /// Returns `true` only when native-window release is proven, either because
    /// every referencing EGLSurface was explicitly destroyed or because final
    /// display termination succeeded. A false result has already quarantined
    /// the prepared native targets and must make the outer render owner retain
    /// its corresponding [`crate::surface_binding::RenderSurfaceBinding`]
    /// leases as well.
    #[must_use]
    pub(crate) fn destroy_all(&mut self) -> bool {
        if self.teardown_complete {
            return self.native_release_confirmed;
        }
        self.teardown_complete = true;

        drop(self.upload_thread.take());
        self.upload_server.take();

        // destroy_canvas refuses id=1 (onscreen), so handle it separately.
        let onscreen_id = CanvasId::from(1u32);
        if self.canvases.contains_key(&onscreen_id) {
            let _ = self.destroy_onscreen_internal(onscreen_id);
        }
        let ids: Vec<CanvasId> = self.canvases.keys().copied().collect();
        for id in ids {
            let _ = self.destroy_canvas(id);
        }

        // Delete GL objects (best effort). Need a current context.
        let has_current_context = self.ensure_any_canvas_current().is_ok();

        unsafe {
            for (_id, p) in self.programs.drain() {
                if let Some(h) = p.gl_handle {
                    self.gl.delete_program(h);
                }
            }
            for (_id, s) in self.shaders.drain() {
                if let Some(h) = s.gl_handle {
                    self.gl.delete_shader(h);
                }
            }
            for (_id, b) in self.buffers.drain() {
                if let Some(h) = b.gl_handle {
                    self.gl.delete_buffer(h);
                }
            }
            for (_id, t) in self.textures.drain() {
                if let Some(h) = t.gl_handle {
                    self.gl.delete_texture(h);
                }
            }
            for (_id, f) in self.framebuffers.drain() {
                if let Some(h) = f.gl_handle {
                    self.gl.delete_framebuffer(h);
                }
            }
            for (_id, fbo) in self.image_copy_fbos.drain() {
                self.gl.delete_framebuffer(fbo);
            }
            if has_current_context {
                for (_id, entry) in self.canvas2d_snapshots.drain() {
                    self.gl.delete_texture(entry.tex);
                }
                self.canvas2d_snapshot_order.clear();
                self.canvas2d_snapshot_bytes = 0;
            }
            if let Some(fbo) = self.canvas2d_snapshot_blit_fbo.take() {
                self.gl.delete_framebuffer(fbo);
            }
            if let Some(fbo) = self.canvas2d_snapshot_read_fbo.take() {
                self.gl.delete_framebuffer(fbo);
            }
            for (_id, r) in self.renderbuffers.drain() {
                if let Some(h) = r.gl_handle {
                    self.gl.delete_renderbuffer(h);
                }
            }
        }
        self.webgl_gpu_budget.clear();

        // Images
        self.image_registry.destroy_all(&self.gl);

        // A failed install can leave a candidate surface outside `canvases`.
        // Try the ordinary proof-producing cleanup first; even if the driver
        // rejects it, final `eglTerminate` below releases the display's native
        // references before the render binding clears its leases.
        let _ = self.cleanup_pending_onscreen();

        if let Some(db) = self.preserved_drawing_buffer.take() {
            if let Some(ctx) = self.preserved_ctx {
                let _ = self.egl.make_current(
                    self.display,
                    self.resource.surf,
                    self.resource.surf,
                    Some(ctx),
                );
            }
            drawing_buffer::destroy(&self.gl, db);
        }
        if let Some(ctx) = self.preserved_ctx.take() {
            let _ = self.egl.make_current(
                self.display,
                self.resource.surf,
                self.resource.surf,
                Some(self.resource.ctx),
            );
            let _ = self.egl.destroy_context(self.display, ctx);
        }

        // Destroy the root pbuffer/context and terminate the display through
        // the same idempotent owner used by constructor-error and Drop paths.
        let windows_explicitly_released =
            self.pending_onscreen.is_none() && self.installed_surface.is_none();
        let display_terminated = self.egl.shutdown();
        if display_terminated {
            // If no context was available above, EGL termination is the
            // teardown proof that reclaims the retained snapshot textures.
            self.canvas2d_snapshots.clear();
            self.canvas2d_snapshot_order.clear();
            self.canvas2d_snapshot_bytes = 0;
        }
        self.native_release_confirmed = windows_explicitly_released || display_terminated;
        if self.native_release_confirmed {
            self.pending_onscreen = None;
            self.installed_surface = None;
        } else {
            tracing::error!(
                "EGL teardown produced no native-release proof; quarantining platform targets"
            );
            self.quarantine_prepared_native_targets();
        }
        self.native_release_confirmed
    }

    /// Fail-safe terminal ownership sink used only when EGL cannot prove it no
    /// longer references a platform target. Intentionally leaking a bounded
    /// target prevents a host-side use-after-free; this is never a recovery or
    /// frame path.
    fn quarantine_prepared_native_targets(&mut self) {
        if let Some(pending) = self.pending_onscreen.take() {
            std::mem::forget(pending);
        }
        if let Some(installed) = self.installed_surface.take() {
            std::mem::forget(installed);
        }
    }

    // ==================== Context Binding ====================

    /// Make the resource context current.
    ///
    /// **Unconditionally, unlike [`Self::make_current_needed`], and that
    /// asymmetry is load-bearing.** The obvious symmetry fix — an
    /// `if self.bound == BoundContext::Resource { return Ok(()) }` guard —
    /// introduces a use-after-destroy.
    ///
    /// The caller that forbids it is context recreation after a GPU reset
    /// (`recreate_contexts_after_loss`, the `bind_resource()` a few dozen lines
    /// below the new `self.resource = …`): at that point `self.bound` can still
    /// read `Resource` from *before* the recreation, naming the context that was
    /// just destroyed. The shadow is stale by construction there, so a guard
    /// that trusted it would skip the `eglMakeCurrent` and leave the thread
    /// current on a dead context — every GL call after it undefined.
    ///
    /// The cost of not guarding is a redundant `eglMakeCurrent` on the paths
    /// where the shadow *is* accurate. Those are the canvas-destroy sweep and
    /// `restore_bound` with a saved `Resource`, neither of which is the
    /// steady-state frame path: a rendering frame is bound to a canvas, and
    /// `restore_bound` reaches the canvas arm, which does short-circuit through
    /// `make_current_needed`. So the redundancy is off the hot path and the
    /// hazard it would trade for is not.
    pub(super) fn bind_resource(&mut self) -> EngineResult<()> {
        self.egl
            .make_current(
                self.display,
                self.resource.surf,
                self.resource.surf,
                Some(self.resource.ctx),
            )
            .map_err(|e| {
                ee(
                    ErrorCode::RenderBackendError,
                    format!("eglMakeCurrent(resource) failed: {e:?}"),
                )
            })?;
        self.bound = BoundContext::Resource;
        Ok(())
    }

    /// Returns true if the currently bound context is the onscreen canvas (id=1).
    #[allow(dead_code)]
    pub(crate) fn is_onscreen_bound(&self) -> bool {
        self.bound == BoundContext::Canvas(CanvasId::from(1u32))
    }

    /// Returns the DrawingBuffer FBO for the given canvas, or None (= real FBO 0)
    /// if the canvas has no DrawingBuffer, or if bypass mode is active.
    pub(crate) fn get_drawing_buffer_fbo(
        &self,
        canvas_id: CanvasId,
    ) -> Option<glow::NativeFramebuffer> {
        self.canvases.get(&canvas_id).and_then(|entry| {
            default_framebuffer_of(
                entry.bypass_drawing_buffer,
                entry.drawing_buffer.as_ref().map(|db| db.fbo),
            )
        })
    }

    /// Returns true if the framebuffer bound to `target` is the DrawingBuffer
    /// FBO for the given canvas.
    ///
    /// WebGL spec forbids modifying the default framebuffer attachments.
    /// Use this to guard `framebufferTexture2D` / `framebufferRenderbuffer`.
    ///
    /// `target` must be one of `FRAMEBUFFER`, `DRAW_FRAMEBUFFER`, or
    /// `READ_FRAMEBUFFER`; the correct binding point is queried accordingly.
    pub(crate) fn is_drawing_buffer_bound(
        &self,
        canvas_id: CanvasId,
        gl: &glow::Context,
        target: u32,
    ) -> bool {
        if let Some(db_fbo) = self.get_drawing_buffer_fbo(canvas_id) {
            let query = match target {
                glow::READ_FRAMEBUFFER => glow::READ_FRAMEBUFFER_BINDING,
                // DRAW_FRAMEBUFFER and FRAMEBUFFER both use the draw binding.
                _ => glow::DRAW_FRAMEBUFFER_BINDING,
            };
            let bound_fbo = unsafe { gl.get_parameter_i32(query) } as u32;
            bound_fbo == db_fbo.0.get()
        } else {
            false
        }
    }

    /// Drain completed texture uploads from the upload thread and register
    /// the resulting textures in the image registry for rendering.
    ///
    /// Textures whose GPU fence has not yet signaled are deferred to the
    /// next frame (stored in `pending_uploads`).
    ///
    /// Called once per frame from the render thread (non-blocking).
    /// Returns the number of dropped upload recoveries processed this call.
    ///
    /// Allocation-free in steady state; see [`stage_upload_drain`].
    pub(crate) fn drain_upload_completed(&mut self) -> u32 {
        let upload = match self.upload_thread.as_mut() {
            Some(u) => u,
            None => return 0,
        };

        // Both halves stage into scratch buffers that outlive the frame. See
        // `stage_upload_drain` for why the obvious `Vec::new()` + `for x in v`
        // shape was an allocation round trip per frame.
        let mut deferred = stage_upload_drain(
            &mut self.upload_drain_scratch,
            &mut self.pending_uploads,
            |sink| upload.drain_completed(sink),
        );

        // Process uploads that completed but whose results could not be
        // delivered (result channel full/disconnected).  Each item carries
        // image_id + byte_len for consistent per-item recovery.
        let mut dropped = std::mem::take(&mut self.upload_dropped_scratch);
        dropped.clear();
        upload.drain_dropped(&mut dropped);
        let dropped_recovery_count = dropped.len() as u32;
        for d in &dropped {
            // 1. Recover UploadServer budget.
            if let Some(ref mut server) = self.upload_server {
                server.recover_dropped(d);
            }
            // 2. Resolve the pending response with an error so callers
            //    don't wait forever.
            if let Some(resp) = self.pending_load_responses.remove(&d.image_id) {
                resp.send(Err(shared::error::EngineError::from_detail(
                    shared::error::ErrorCode::Cancelled,
                    format!(
                        "image {} upload completed but result channel was full",
                        d.image_id,
                    ),
                )));
            }
            // 3. Clean stale cancelled_uploads entry (if DestroyImage arrived
            //    while the upload was in flight and then the result was dropped).
            self.cancelled_uploads.remove(&d.image_id);
        }

        // Deferred first, newly completed after — the order `stage_upload_drain`
        // preserves and the order this loop has always seen.
        for c in deferred.drain(..) {
            // Non-blocking fence check (timeout = 0).
            let status = unsafe { self.gl.client_wait_sync(c.fence, 0, 0) };

            if fence_signalled(status) {
                unsafe { self.gl.delete_sync(c.fence) };

                if let Some(server) = &mut self.upload_server {
                    server.finish_job_bytes(c.byte_len);
                }

                if self.cancelled_uploads.remove(&c.image_id) {
                    unsafe { self.gl.delete_texture(c.texture) };
                    tracing::debug!(
                        "Upload thread: discarded cancelled upload image_id={}",
                        c.image_id
                    );
                    continue;
                }

                let info = crate::backend::gl::image_store::GpuImageInfo::rgba8_unpremul(
                    c.width, c.height,
                );
                self.image_registry
                    .register_shared_texture(c.image_id as u32, c.texture, info);

                if let Some(resp) = self.pending_load_responses.remove(&c.image_id) {
                    resp.send(Ok((c.width, c.height)));
                }

                tracing::trace!(
                    "Upload thread: texture registered image_id={} {}x{}",
                    c.image_id,
                    c.width,
                    c.height
                );
            } else if fence_failed_permanently(status) {
                // WAIT_FAILED is a known-permanent driver refusal, not a
                // slow GPU. Retrying it every frame retains the texture and
                // upload budget forever.
                unsafe {
                    self.gl.delete_sync(c.fence);
                    self.gl.delete_texture(c.texture);
                }
                if let Some(server) = &mut self.upload_server {
                    server.finish_job_bytes(c.byte_len);
                }
                self.cancelled_uploads.remove(&c.image_id);
                if let Some(resp) = self.pending_load_responses.remove(&c.image_id) {
                    resp.send(Err(shared::error::EngineError::from_detail(
                        shared::error::ErrorCode::RenderBackendError,
                        format!("image {} upload fence failed", c.image_id),
                    )));
                }
            } else {
                // TIMEOUT_EXPIRED is normal GPU lifetime: defer until a later
                // frame rather than deleting a texture whose DMA may continue.
                self.pending_uploads.push(c);
            }
        }
        // Both scratch buffers go home empty, keeping their capacity for the
        // next frame.
        self.upload_drain_scratch = deferred;
        self.upload_dropped_scratch = dropped;
        dropped_recovery_count
    }

    /// Reset per-frame upload budget counters.  Called at the render thread's
    /// frame boundary (after draining completions, before signaling RAF).
    pub(crate) fn reset_frame_upload_budget(&mut self) {
        if let Some(ref mut server) = self.upload_server {
            server.reset_frame_budget();
        }
    }

    /// Read and reset per-frame upload budget rejections since last call.
    pub(crate) fn take_upload_frame_rejections(&mut self) -> u32 {
        match self.upload_server {
            Some(ref mut server) => {
                let n = server.frame_rejections();
                // frame_rejections is reset by reset_frame_budget, but we
                // read it before that reset fires (called in the same
                // present_frame_and_signal_raf closure).
                n
            }
            None => 0,
        }
    }

    /// Read and reset the number of snapshot fence waits since the last
    /// present-pass accounting point.
    pub(crate) fn take_snapshot_fence_waits(&mut self) -> u32 {
        let waits = self.snapshot_fence_waits;
        self.snapshot_fence_waits = 0;
        waits
    }

    /// Preserve the first nonempty read of the onscreen default framebuffer.
    /// The caller has made the onscreen context current and validated storage.
    /// A failed snapshot leaves the mode and client binding shadow unchanged.
    pub(crate) fn signal_default_fbo_readback(&mut self) -> EngineResult<()> {
        if !should_latch_default_fbo_readback(self.needs_default_fbo_readback) {
            return Ok(());
        }
        let onscreen_id = CanvasId::from(1u32);
        debug_assert_eq!(self.bound, BoundContext::Canvas(onscreen_id));
        if !reads_from_default_framebuffer(
            &self.gl,
            self.canvases
                .get(&onscreen_id)
                .is_some_and(|entry| entry.drawing_buffer.is_some()),
            self.gl_state.get(&onscreen_id),
            self.get_drawing_buffer_fbo(onscreen_id),
        ) {
            return Ok(());
        }
        if let Some(entry) = self.canvases.get(&onscreen_id) {
            if entry.bypass_drawing_buffer {
                if let Some(db) = entry.drawing_buffer.as_ref() {
                    // A DrawingBuffer exists only after create() successfully
                    // probes split bindings and glBlitFramebuffer on this driver.
                    if !drawing_buffer::blit_from_surface(
                        &self.gl,
                        db,
                        entry.physical_width,
                        entry.physical_height,
                    ) {
                        return Err(ee(
                            ErrorCode::RenderBackendError,
                            "default framebuffer snapshot failed",
                        ));
                    }
                }
            }
        }
        self.needs_default_fbo_readback = true;
        self.evaluate_bypass();
        Ok(())
    }

    pub(crate) fn evaluate_bypass(&mut self) {
        let onscreen_id = CanvasId::from(1u32);
        // Bypass requires: single canvas, has DrawingBuffer, no default-FBO
        // readback, and the onscreen canvas is NOT a Canvas2D canvas.
        //
        // Bypass is a WebGL-only optimization: in bypass mode WebGL's default
        // framebuffer is redirected to the real FBO 0 (see
        // `get_drawing_buffer_fbo`), so `swap_buffers_no_restore` can skip the
        // DrawingBuffer→window blit. Skia/Canvas2D has no such redirect — its
        // onscreen surface always targets the DrawingBuffer FBO
        // (`init_skia_for_canvas`), so skipping the blit would leave every 2D
        // draw stranded in the DrawingBuffer and never presented (black
        // screen). Canvas2D also requires preserved content across swaps
        // (the canvas is not implicitly cleared each frame), which only the
        // DrawingBuffer provides. So whenever the onscreen canvas has a 2D
        // context, bypass must stay off. `init_skia_for_canvas` re-runs this
        // check when an onscreen 2D context is created.
        // Bypass is only safe when the DrawingBuffer is exactly the surface
        // size: the swap-time blit scales db→surface, but bypass renders
        // straight to FBO 0 with no scaling. A game that shrinks its canvas
        // below the surface (Phaser Scale.NONE) must go through the blit so it
        // fills the window instead of landing in a corner. `false` when there
        // is no DrawingBuffer, which also (correctly) disables bypass.
        let onscreen_db_matches_surface = self.canvases.get(&onscreen_id).map_or(false, |e| {
            e.drawing_buffer.as_ref().map_or(false, |db| {
                db.width == e.physical_width && db.height == e.physical_height
            })
        });
        let canvas_count = self.canvases.len();
        let onscreen_has_2d_context = self.contexts_2d.contains_key(&onscreen_id);
        let needs_default_fbo_readback = self.needs_default_fbo_readback;
        let can_bypass = can_bypass_drawing_buffer(
            canvas_count,
            needs_default_fbo_readback,
            onscreen_has_2d_context,
            onscreen_db_matches_surface,
        );

        let mut mode_changed = false;
        if let Some(entry) = self.canvases.get_mut(&onscreen_id) {
            if entry.bypass_drawing_buffer != can_bypass {
                // The four inputs travel with the verdict: a presented frame
                // either copies or it does not, and "why not" is otherwise only
                // recoverable by re-deriving them from a log that does not carry
                // them. This is the instrument Section 7.3's "asserted where the
                // platform allows observation" needs on a host with no device.
                tracing::info!(
                    canvas_count,
                    needs_default_fbo_readback,
                    onscreen_has_2d_context,
                    onscreen_db_matches_surface,
                    "DrawingBuffer bypass: {} → {}",
                    entry.bypass_drawing_buffer,
                    can_bypass,
                );
                entry.bypass_drawing_buffer = can_bypass;
                mode_changed = true;
            }
        }
        if mode_changed {
            // Direct-FBO frames and DrawingBuffer frames do not share a
            // trustworthy repair history. Start the new presentation mode at
            // a full boundary and discard any plan built for the old mode.
            self.damage_history.clear();
            self.pending_present_plan = None;
            self.damage
                .add(crate::damage_effect::DamageEffect::FullSurface);
        }

        self.apply_default_framebuffer_mapping(onscreen_id);
    }

    fn apply_default_framebuffer_mapping(&mut self, id: CanvasId) {
        if let Some(entry) = self.canvases.get_mut(&id) {
            apply_default_framebuffer(
                &self.gl,
                self.bound == BoundContext::Canvas(id),
                &mut entry.applied_default_framebuffer,
                default_framebuffer_of(
                    entry.bypass_drawing_buffer,
                    entry.drawing_buffer.as_ref().map(|db| db.fbo),
                ),
            );
        }
    }

    /// Delete one GL object, from the context that owns it when its kind has one.
    ///
    /// The pairing is the point. `glDelete*` for a container object — a framebuffer,
    /// vertex array, query or transform feedback — issued from another context of the
    /// share group either frees that context's object of the same name or silently
    /// frees nothing, and this decision used to be taken independently at each of the
    /// eleven delete sites. See [`gl_object`] for the sharing rule and for what the
    /// two sites that took it wrongly actually did.
    ///
    /// A missing owner canvas is not a reason to delete from somewhere else: a
    /// container object cannot outlive the context that holds it, so if the canvas is
    /// gone the object already is. That replaces a fallback which deleted from
    /// whatever context happened to be current.
    pub(crate) fn delete_gl_object(&mut self, object: gl_object::GlObject) -> EngineResult<()> {
        match object.owning_context() {
            Some(owner) => {
                if !self.canvases.contains_key(&owner) {
                    return Ok(());
                }
                self.make_current_needed(owner)?;
            }
            None => {
                self.ensure_any_canvas_current()?;
            }
        }
        object.delete(&self.gl);
        Ok(())
    }

    pub(crate) fn make_current_needed(&mut self, id: CanvasId) -> EngineResult<()> {
        // A canvas whose 2D surface lives on the shared context must have that
        // context current, not its own: its pixels are in a Skia render target
        // that only exists there. Checked before the fast path below, because
        // `BoundContext::Canvas(id)` is not the right binding for such a canvas
        // even when it names the right id.
        if self.canvas_uses_shared_2d(id) {
            self.bind_shared_2d_context(id)?;
            return Ok(());
        }
        if self.bound == BoundContext::Canvas(id) {
            return Ok(());
        }
        let entry = self
            .canvases
            .get(&id)
            .ok_or_else(|| ee(ErrorCode::NotFound, format!("canvas not found: {id:?}")))?;

        self.egl
            .make_current(
                self.display,
                entry.ctx.surf,
                entry.ctx.surf,
                Some(entry.ctx.ctx),
            )
            .map_err(|e| {
                ee(
                    ErrorCode::RenderBackendError,
                    format!("eglMakeCurrent(canvas) failed: {e:?}"),
                )
            })?;
        self.bound = BoundContext::Canvas(id);

        // EGL preserves this context's client bindings. Only a pending change
        // to the native meaning of its default framebuffer needs applying.
        // Custom READ/DRAW bindings remain exactly as the context left them.
        self.apply_default_framebuffer_mapping(id);

        Ok(())
    }

    /// Re-point `id` at its WebGL default framebuffer and tell the dedup shadow.
    ///
    /// For the sites that genuinely destroyed the binding rather than merely left
    /// it: the swap-time blit binds `READ=DrawingBuffer, DRAW=0`, and a surface
    /// install starts from whatever the fresh context had. The shadow record is
    /// half of the operation, not bookkeeping after it — a driver re-point the
    /// shadow does not know about is exactly what put a render-to-texture pass on
    /// the screen.
    fn bind_default_framebuffer(&mut self, id: CanvasId) {
        let Some(target) = self.get_drawing_buffer_fbo(id) else {
            return;
        };
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target));
        }
        crate::backend::gl::state_tracker::record_default_framebuffer_bind(
            self.gl_state.entry(id).or_default(),
        );
    }

    pub(crate) fn ensure_any_canvas_current(&mut self) -> EngineResult<CanvasId> {
        match self.bound {
            BoundContext::Canvas(id) => Ok(id),
            // The shared 2D context belongs to no canvas, so "any canvas" has
            // to be chosen the same way it is from the resource context.
            BoundContext::Shared2D(id) => Ok(id),
            BoundContext::Resource => {
                // Prefer onscreen(1) if exists
                let onscreen = CanvasId::from(1u32);
                if self.canvases.contains_key(&onscreen) {
                    self.make_current_needed(onscreen)?;
                    return Ok(onscreen);
                }
                // else pick any canvas
                if let Some((&id, _)) = self.canvases.iter().next() {
                    self.make_current_needed(id)?;
                    return Ok(id);
                }
                // else bind resource
                self.bind_resource()?;
                Ok(CanvasId::from(0u32))
            }
        }
    }

    pub(crate) fn current_canvas_id(&self) -> Option<CanvasId> {
        match self.bound {
            BoundContext::Canvas(id) => Some(id),
            BoundContext::Shared2D(id) => Some(id),
            BoundContext::Resource => None,
        }
    }

    fn restore_bound(&mut self, saved: BoundContext) -> EngineResult<()> {
        match saved {
            BoundContext::Resource => self.bind_resource(),
            BoundContext::Shared2D(id) => self.bind_shared_2d_context(id).map(|_| ()),
            BoundContext::Canvas(id) => {
                if self.canvases.contains_key(&id) {
                    self.make_current_needed(id)
                } else {
                    self.bind_resource()
                }
            }
        }
    }

    /// Remove a Canvas2DContext and drop it under a known-good GL context
    /// when possible. If the matching EGL context is not current, abandon
    /// the Skia context first so Drop doesn't issue GL destruction calls
    /// against the wrong or already-destroyed driver context.
    fn drop_2d_context(&mut self, id: CanvasId, ctx_is_current: bool) -> Option<u32> {
        let mut ctx = self.contexts_2d.remove(&id)?;
        let ctx_tag = ctx.ctx_tag;
        if !ctx_is_current {
            tracing::warn!(
                "dropping Canvas2DContext for canvas {id:?} without its EGL context current; abandoning Skia context first"
            );
            ctx.abandon();
        }
        drop(ctx);
        Some(ctx_tag)
    }

    // ==================== Canvas Operations ====================

    /// Resize a canvas buffer.
    ///
    /// `w` and `h` are in **physical (buffer) pixels** — the same unit JS
    /// `canvas.width`/`canvas.height` uses.  No DPR scaling is applied.
    ///
    /// `owner` says who is asking, and only [`BackingSizeOwner::Content`]
    /// promotes the onscreen canvas to a content-owned size. It is an argument
    /// rather than something the callers remember to record separately because
    /// every call site then has to answer it, and the surface-driven caller
    /// answering it wrongly is precisely the bug: it would take the canvas away
    /// from the content that owns it.
    pub(crate) fn resize_canvas(
        &mut self,
        id: CanvasId,
        w: Option<u32>,
        h: Option<u32>,
        owner: BackingSizeOwner,
    ) -> EngineResult<()> {
        let (old_w, old_h, kind, ctx_handle, old_surf) = {
            let entry = self.canvases.get(&id).ok_or_else(|| {
                ee(
                    ErrorCode::NotFound,
                    format!("resize_canvas: canvas not found: {id:?}"),
                )
            })?;

            // The baseline for the *unset* dimension must be the CURRENT canvas
            // buffer size, not the fixed EGL surface size.  For a window canvas
            // that current size is the DrawingBuffer's; `physical_width/height`
            // is the (immutable) surface size.  JS sets `canvas.width` and
            // `canvas.height` in two separate ops (Pixi does), so if we used the
            // surface size here, setting one dimension would reset the other to
            // the surface size — corrupting the canvas (content renders into a
            // corner of an over-sized DrawingBuffer instead of filling it).
            let (cur_w, cur_h) = match (entry.kind, entry.drawing_buffer.as_ref()) {
                (SurfaceKind::Window, Some(db)) => (db.width, db.height),
                _ => (entry.physical_width, entry.physical_height),
            };
            (cur_w, cur_h, entry.kind, entry.ctx.ctx, entry.ctx.surf)
        };

        let new_w = w.unwrap_or(old_w);
        let new_h = h.unwrap_or(old_h);

        if shared::protocol::render_cmd::checked_canvas_pixel_count(new_w, new_h).is_none() {
            return Err(ee(
                ErrorCode::InvalidArgument,
                format!("canvas resize {new_w}x{new_h} exceeds the surface pixel cap"),
            ));
        }

        // Recorded before the no-op return below, because assigning the size it
        // already has still claims the canvas -- the JS setter sets its half of
        // this on every assignment, and a canvas the two halves disagree about
        // is one whose size the engine would move under content that thinks it
        // owns it.
        if owner == BackingSizeOwner::Content && matches!(kind, SurfaceKind::Window) {
            self.onscreen_content_backing = Some((new_w, new_h));
        }

        if new_w == old_w && new_h == old_h {
            return Ok(());
        }

        // Window surfaces: the EGL surface is controlled by Android SurfaceView.
        // Resize only the DrawingBuffer so canvas.width/height reflects what JS
        // set, and WebGL renders at that resolution. The blit in swap_buffers
        // scales to the actual surface dimensions.
        if matches!(kind, SurfaceKind::Window) {
            self.make_current_needed(id)?;
            if let Some(entry) = self.canvases.get_mut(&id) {
                if let Some(ref mut db) = entry.drawing_buffer {
                    drawing_buffer::resize(&self.gl, db, new_w, new_h)?;
                }
                entry.info.width = new_w;
                entry.info.height = new_h;
            }

            let new_fbo = self
                .canvases
                .get(&id)
                .and_then(|e| e.drawing_buffer.as_ref())
                .map(|db| db.fbo.0.get())
                .unwrap_or(0);
            let resized_ok = {
                let image_store = self.image_registry.store_mut();
                self.contexts_2d
                    .get_mut(&id)
                    .map(|ctx2d| ctx2d.resize(new_fbo, new_w, new_h, image_store))
                    .unwrap_or(true)
            };
            if !resized_ok {
                self.rebuild_2d_context_preserving_state(id)?;
            }

            // WebGL default framebuffer viewport resets after drawing buffer resize.
            unsafe {
                self.gl.viewport(0, 0, new_w as i32, new_h as i32);
            }
            self.gl_state.entry(id).or_default().viewport =
                Some((0, 0, new_w as i32, new_h as i32));

            // `drawing_buffer::resize` reallocates its attachments, so neither
            // the old buffer-age history nor a plan referencing the old
            // storage remains valid. Poison this frame to guarantee the first
            // present after resize is a full repair.
            if id == CanvasId::from(1u32) {
                self.damage_history.clear();
                self.pending_present_plan = None;
                self.damage
                    .add(crate::damage_effect::DamageEffect::FullSurface);
            }

            // The DrawingBuffer size just changed, which affects bypass
            // eligibility: bypass is only safe when db == surface (see
            // `can_bypass_drawing_buffer`). A game shrinking its onscreen canvas
            // below the surface (Phaser Scale.NONE) must fall back to the
            // scaling blit so it fills the window instead of a corner.
            if id == CanvasId::from(1u32) {
                self.evaluate_bypass();
            }

            return Ok(());
        }

        let saved_bound = self.bound;
        let was_current = matches!(saved_bound, BoundContext::Canvas(cur) if cur == id);

        if was_current {
            self.egl
                .make_current(self.display, None, None, None)
                .map_err(|e| {
                    ee(
                        ErrorCode::RenderBackendError,
                        format!("resize_canvas: make_current(None) failed: {e:?}"),
                    )
                })?;
        }

        // destroy old surface
        // A surfaceless share group has no pbuffer to swap: the offscreen canvas
        // renders into an FBO whose size is what actually changes here, and the
        // context is current against EGL_NO_SURFACE either way. Destroying and
        // recreating nothing is exactly right; only the recorded metrics move.
        if let Some(old_surf) = old_surf {
            self.egl
                .destroy_surface(self.display, old_surf)
                .map_err(|e| {
                    ee(
                        ErrorCode::RenderBackendError,
                        format!("resize_canvas: destroy_surface failed: {e:?}"),
                    )
                })?;
        }

        // create new surface
        let new_surf = match kind {
            SurfaceKind::Window => {
                return Err(ee(
                    ErrorCode::InvalidOperation,
                    "window DrawingBuffer resize must not recreate its platform EGLSurface",
                ));
            }
            SurfaceKind::Pbuffer if self.surfaceless => None,
            SurfaceKind::Pbuffer => {
                let pbuf_attribs = [
                    egl::WIDTH as i32,
                    new_w as i32,
                    egl::HEIGHT as i32,
                    new_h as i32,
                    egl::NONE as i32,
                ];
                Some(
                    self.egl
                        .create_pbuffer_surface(self.display, self.config, &pbuf_attribs)
                        .map_err(|e| {
                            ee(
                                ErrorCode::RenderBackendError,
                                format!("resize_canvas: create_pbuffer_surface failed: {e:?}"),
                            )
                        })?,
                )
            }
        };

        self.egl
            .make_current(self.display, new_surf, new_surf, Some(ctx_handle))
            .map_err(|e| {
                ee(
                    ErrorCode::RenderBackendError,
                    format!("resize_canvas: make_current(resized surf) failed: {e:?}"),
                )
            })?;
        self.bound = BoundContext::Canvas(id);
        let _ = was_current;

        // Keep canvas metrics aligned with JS-requested dimensions. On some
        // devices EGL surface queries may return rotated values for window
        // surfaces, which breaks viewport/layout logic in games.
        let (actual_w, actual_h) = (new_w, new_h);

        {
            let entry = self.canvases.get_mut(&id).ok_or_else(|| {
                ee(
                    ErrorCode::NotFound,
                    format!("resize_canvas: canvas not found after egl ops: {id:?}"),
                )
            })?;

            entry.physical_width = actual_w;
            entry.physical_height = actual_h;

            entry.ctx.surf = new_surf;
            entry.info.width = actual_w;
            entry.info.height = actual_h;
        }

        // Offscreen pbuffer: Skia renders into FBO 0 directly.
        let resized_ok = {
            let image_store = self.image_registry.store_mut();
            self.contexts_2d
                .get_mut(&id)
                .map(|ctx2d| ctx2d.resize(0, actual_w, actual_h, image_store))
                .unwrap_or(true)
        };
        if !resized_ok {
            self.rebuild_2d_context_preserving_state(id)?;
        }

        if !was_current {
            self.restore_bound(saved_bound)?;
        }

        Ok(())
    }

    /// Declare the damage region for the current back buffer BEFORE rendering
    /// to the onscreen surface (Skia flush_and_submit, DrawingBuffer blit).
    ///
    /// Per EGL_KHR_partial_update spec, this must be called before any rendering
    /// to the main framebuffer so the driver can skip loading unchanged tiles.
    /// The declared region is the age-expanded historical damage — what this
    /// buffer needs to "catch up" on since it was last presented.  The app will
    /// render at least this region (and typically the full game frame on top).
    ///
    /// Called from the render thread right before `flush_dirty_2d_contexts()`.
    /// Build the present/blit plan for `id` and declare its repair region to the
    /// compositor (when partial). Shared by [`Self::declare_frame_damage`] and the
    /// [`Self::swap_buffers_no_restore`] cache-miss fallback so both paths agree on
    /// the exact `repair`/`current` regions. For a non-bypassed surface this
    /// helper makes `id` current before querying age or declaring damage, as
    /// required by `EGL_KHR_partial_update`.
    fn prepare_present_plan(&mut self, id: CanvasId) -> crate::present_damage::PresentDamagePlan {
        use crate::present_damage::{
            DamageRegion, PresentDamagePlan, build_present_plan, repair_after_declaration_failure,
        };

        let (surface_w, surface_h, surf, db_matches, bypass) = match self.canvases.get(&id) {
            Some(e) => {
                // A partial identity blit requires the DrawingBuffer to exactly
                // match the surface. When it differs, the swap-time blit *scales*
                // db -> surface: it rewrites the whole surface every frame and the
                // game's damage rects (DrawingBuffer/game coordinates) no longer
                // map 1:1 onto surface pixels, so partial repair must stay full.
                let db_matches = e.drawing_buffer.as_ref().map_or(false, |db| {
                    db.width == e.physical_width && db.height == e.physical_height
                });
                // Buffer age and partial repair are properties of a window
                // surface. A canvas without one is offscreen and never reaches
                // here, but a full plan is the safe answer if it ever did.
                let Some(surf) = e.ctx.surf else {
                    return PresentDamagePlan {
                        current: DamageRegion::FullSurface,
                        repair: DamageRegion::FullSurface,
                    };
                };
                (
                    e.physical_width,
                    e.physical_height,
                    surf,
                    db_matches,
                    e.bypass_drawing_buffer,
                )
            }
            // No canvas entry: nothing to blit — a full/full plan is safe.
            None => {
                return PresentDamagePlan {
                    current: DamageRegion::FullSurface,
                    repair: DamageRegion::FullSurface,
                };
            }
        };

        // Current-frame surface damage as discrete lower-left rects.
        let current = self.current_damage_region(surface_w, surface_h);

        // Bypass renders directly to FBO 0 *before* this declaration point, so a
        // restricted region here would violate EGL_KHR_partial_update ordering.
        // The direct-to-FBO-0 path also skips the DrawingBuffer blit entirely.
        if bypass {
            return PresentDamagePlan {
                current,
                repair: DamageRegion::FullSurface,
            };
        }

        // EGL_BUFFER_AGE_KHR and eglSetDamageRegionKHR apply to the current
        // draw surface. Switching contexts binds the persistent DrawingBuffer,
        // not FBO 0, so this still precedes every write to the window surface.
        if let Err(err) = self.make_current_needed(id) {
            tracing::warn!(
                "present damage: failed to make canvas {id:?} current before buffer-age query: {err}"
            );
            return PresentDamagePlan {
                current,
                repair: DamageRegion::FullSurface,
            };
        }

        // Query buffer age every eligible frame; a query error maps to 0 (full).
        const EGL_BUFFER_AGE_KHR: egl::Int = 0x313D;
        let buffer_age = if self.device_caps.has_buffer_age {
            self.egl
                .query_surface(self.display, surf, EGL_BUFFER_AGE_KHR)
                .unwrap_or(0)
        } else {
            0
        };

        let plan = build_present_plan(
            current,
            &self.damage_history,
            self.device_caps.has_buffer_age,
            buffer_age,
            db_matches,
            self.dest_single_sample,
        );

        // Declare the exact repair rectangles before any FBO 0 write. A full
        // repair leaves the compositor's default full damage region untouched.
        match &plan.repair {
            DamageRegion::FullSurface => plan,
            DamageRegion::Partial(_) => {
                if self.declare_repair_region(surf, &plan.repair) {
                    plan
                } else {
                    // Declaration unavailable/rejected: keep the partial repair
                    // only when EGL_EXT_buffer_age independently guarantees the
                    // aged back-buffer contents; otherwise fall back to full
                    // before the blit touches FBO 0.
                    repair_after_declaration_failure(plan, self.has_ext_buffer_age)
                }
            }
        }
    }

    /// Resolve the current-frame accumulator into a bounded [`DamageRegion`] of
    /// discrete lower-left rects, failing closed to `FullSurface`.
    fn current_damage_region(
        &self,
        surface_w: u32,
        surface_h: u32,
    ) -> crate::present_damage::DamageRegion {
        use crate::present_damage::{DamageRect, DamageRegion};
        match self
            .damage
            .resolve_rects((surface_w as i32, surface_h as i32))
        {
            None => DamageRegion::FullSurface,
            Some(rects) => {
                let mut region: Option<DamageRegion> = None;
                for (x, y, w, h) in rects {
                    let Some(rect) = DamageRect::new(x, y, w, h) else {
                        return DamageRegion::FullSurface;
                    };
                    region = Some(match region {
                        None => DamageRegion::from_rect(rect),
                        Some(acc) => acc.union(DamageRegion::from_rect(rect)),
                    });
                }
                region.unwrap_or(DamageRegion::FullSurface)
            }
        }
    }

    /// Flatten up to four repair rectangles into a fixed stack array and issue
    /// one `eglSetDamageRegionKHR` call. Returns `true` only when the driver
    /// accepted the declaration. A missing function pointer (KHR unavailable) or
    /// an empty/full region counts as a declaration failure so the caller can
    /// apply the EXT-vs-KHR fallback.
    fn declare_repair_region(
        &self,
        surf: egl::Surface,
        repair: &crate::present_damage::DamageRegion,
    ) -> bool {
        let set_damage = match self.egl_set_damage_region_fn {
            Some(f) => f,
            None => return false,
        };
        let rects = match repair.rects() {
            Some(r) if !r.is_empty() => r,
            _ => return false,
        };
        // 4 rects * 4 ints = 16 ints; stack allocation, no heap.
        let mut flat: [egl::Int; 16] = [0; 16];
        let n = rects.len().min(4);
        for (i, r) in rects.iter().take(n).enumerate() {
            let (x, y, width, height) = r.xywh();
            flat[i * 4] = x;
            flat[i * 4 + 1] = y;
            flat[i * 4 + 2] = width;
            flat[i * 4 + 3] = height;
        }
        let ret = unsafe { set_damage(self.display, surf, flat.as_ptr(), n as egl::Int) };
        ret == egl::TRUE
    }

    /// Present with surface damage when the driver offers it.
    ///
    /// Returns `true` only when the frame was actually presented by
    /// `eglSwapBuffersWithDamage`; every other path returns `false` so the
    /// caller performs an ordinary `eglSwapBuffers`. A rejection is permanent
    /// for the session: the plain swap is always a correct substitute, and a
    /// driver that refused once will refuse again.
    ///
    /// Which frames qualify is decided by
    /// [`crate::present_damage::surface_damage_to_submit`], which host tests
    /// cover; what is left here is the EGL call itself.
    fn try_swap_with_damage(
        &mut self,
        surf: egl::Surface,
        plan: &crate::present_damage::PresentDamagePlan,
    ) -> bool {
        let Some(rects) = crate::present_damage::surface_damage_to_submit(
            plan,
            self.egl_swap_buffers_with_damage_fn.is_some(),
            self.swap_with_damage_rejected,
        ) else {
            return false;
        };
        let Some(swap_with_damage) = self.egl_swap_buffers_with_damage_fn else {
            return false;
        };

        // 4 rects * 4 ints, matching the declaration path's bound: stack only.
        let mut flat: [egl::Int; 16] = [0; 16];
        let n = rects.len().min(4);
        for (i, r) in rects.iter().take(n).enumerate() {
            let (x, y, width, height) = r.xywh();
            flat[i * 4] = x;
            flat[i * 4 + 1] = y;
            flat[i * 4 + 2] = width;
            flat[i * 4 + 3] = height;
        }

        let ok = unsafe { swap_with_damage(self.display, surf, flat.as_ptr(), n as egl::Int) };
        if ok == egl::TRUE {
            return true;
        }

        // EGL_FALSE means the swap did not happen, so the caller still has to
        // present this frame -- it falls through to the plain swap, which also
        // re-surfaces a context/surface loss through the normal classification.
        self.swap_with_damage_rejected = true;
        tracing::warn!(
            "eglSwapBuffersWithDamage rejected {} rect(s); falling back to eglSwapBuffers for \
             the rest of the session",
            n
        );
        false
    }

    /// Damage-region declaration point.
    ///
    /// Builds and caches the frame's [`PresentDamagePlan`], declaring the exact
    /// buffer-age repair region to the compositor before any FBO 0 rendering
    /// (`flush_dirty_2d_contexts`, DrawingBuffer blit). Per EGL_KHR_partial_update
    /// the declaration must precede GL draws to the main framebuffer so the driver
    /// can skip loading unchanged tiles.
    ///
    /// Called from the render thread right before `flush_dirty_2d_contexts()`.
    pub(crate) fn declare_frame_damage(&mut self, id: CanvasId) {
        let plan = self.prepare_present_plan(id);
        self.pending_present_plan = Some((id, plan));
    }

    pub(crate) fn swap_buffers_no_restore(
        &mut self,
        id: CanvasId,
        wait_for_vsync: bool,
    ) -> EngineResult<ResolvedDamage> {
        self.make_current_needed(id)?;

        // Consume the plan declared earlier this frame. A missing/mismatched
        // entry recomputes safely with the same plan-preparation helper before
        // any FBO 0 write; it never reuses another canvas/frame's plan.
        //
        // P1-8: a mismatch means declare ↔ swap targeted different canvases.
        // Debug builds panic; release builds recompute and log a warning.
        let plan = match self.pending_present_plan.take() {
            Some((cached_id, plan)) if cached_id == id => plan,
            Some((cached_id, _)) => {
                debug_assert!(
                    false,
                    "present-plan/swap guard violated: declared on canvas_id={cached_id:?} but swapping canvas_id={id:?}"
                );
                tracing::warn!(
                    "present-plan guard: declared on {cached_id:?} but swap targets {id:?}; recomputing before swap"
                );
                self.prepare_present_plan(id)
            }
            None => self.prepare_present_plan(id),
        };

        let entry = self
            .canvases
            .get(&id)
            .ok_or_else(|| ee(ErrorCode::NotFound, format!("canvas not found: {id:?}")))?;

        // Blit DrawingBuffer to the real window surface before swap, driven by the
        // plan's `repair` region (never `current`). When bypass is active, WebGL
        // already rendered to FBO 0 — skip the blit.
        let mut blit_succeeded = true;
        if !entry.bypass_drawing_buffer {
            if let Some(ref db) = entry.drawing_buffer {
                let db_matches =
                    db.width == entry.physical_width && db.height == entry.physical_height;
                let blit = crate::present_damage::blit_plan(
                    &plan.repair,
                    db_matches,
                    self.dest_single_sample,
                );
                blit_succeeded = drawing_buffer::blit_to_surface(
                    &self.gl,
                    db,
                    entry.physical_width,
                    entry.physical_height,
                    &blit,
                );
            }
        }

        // One-shot offscreen capture for the headless dev player: read the final
        // frame from FBO 0 after the blit and before eglSwapBuffers, while the
        // context is current and the back buffer is still valid. No-op (single
        // atomic load) unless a capture was explicitly requested.
        crate::frame_capture::capture_default_fbo(
            &self.gl,
            entry.physical_width,
            entry.physical_height,
        );

        // Only call eglSwapInterval when the value actually changes
        let interval = if wait_for_vsync { 1 } else { 0 };
        if interval != self.last_swap_interval {
            let _ = self.egl.swap_interval(self.display, interval);
            self.last_swap_interval = interval;
        }

        // Only a window surface is ever presented, and only a window canvas
        // reaches here. An offscreen one has nothing to swap; FullSurface is
        // the conservative answer, since a caller reading it repairs everything
        // rather than trusting a region no present ever wrote.
        let Some(entry_surf) = entry.ctx.surf else {
            return Ok(ResolvedDamage::FullSurface);
        };
        // Surface damage first: tell the compositor what actually changed since
        // the frame it last showed, so it can leave the rest of the screen
        // composited as-is. `swap_damage_region` is the named choice of which
        // of the plan's two regions that is -- submitting `repair` here would
        // look identical on screen and quietly give the saving back.
        let swapped_with_damage = self.try_swap_with_damage(entry_surf, &plan);

        let swap = if swapped_with_damage {
            Ok(())
        } else {
            self.egl.swap_buffers(self.display, entry_surf)
        }
        .map_err(|e| {
            match classify_egl_swap_failure(e) {
                EglSwapFailureClass::ContextLost => {
                    tracing::warn!("EGL context lost detected during swap_buffers");
                    self.context_lost = true;
                }
                EglSwapFailureClass::SurfaceLost => {
                    tracing::warn!(?e, "EGL native Surface became unavailable during swap");
                    self.surface_unavailable = true;
                }
                EglSwapFailureClass::Other => {}
            }
            ee(
                ErrorCode::RenderBackendError,
                format!("eglSwapBuffers failed: {e:?}"),
            )
        });

        let commit = commit_present_outcome(
            &mut self.damage,
            &mut self.damage_history,
            swap.is_ok(),
            blit_succeeded,
            &plan,
        );
        // The failed-swap path returns here, and the commit above is what leaves
        // this frame's damage in the accumulator for the retry to repair.
        swap?;

        // The blit bound READ=DrawingBuffer / DRAW=0 and so destroyed whatever
        // the content had on `FRAMEBUFFER`; re-point it at this canvas's default
        // framebuffer and record that in the shadow, or the content's next bind of
        // its own FBO is deduped against a claim the blit already invalidated.
        // Under bypass there was no blit, nothing was destroyed, and the
        // resolver's answer (real FBO 0) is what is already bound.
        self.bind_default_framebuffer(id);

        Ok(match commit {
            PresentCommit::Presented(resolved) => resolved,
            PresentCommit::PresentedPartial => ResolvedDamage::FullSurface,
            // `swap?` above returns on exactly this arm. Answered rather than
            // `unreachable!()`d so that reordering the two degrades to the
            // conservative region instead of panicking on the present path.
            PresentCommit::Retry => ResolvedDamage::FullSurface,
        })
    }

    /// Collapse a [`crate::present_damage::DamageRegion`] to a single-AABB
    /// [`ResolvedDamage`] for swap-stats reporting of current-frame surface damage.
    fn region_to_resolved(region: &crate::present_damage::DamageRegion) -> ResolvedDamage {
        let Some(rect) = region.bounding_rect() else {
            return ResolvedDamage::FullSurface;
        };
        let (x, y, width, height) = rect.xywh();
        ResolvedDamage::Partial {
            x,
            y,
            width,
            height,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn get_info(&self, id: CanvasId) -> EngineResult<CanvasInfo> {
        let entry = self
            .canvases
            .get(&id)
            .ok_or_else(|| ee(ErrorCode::NotFound, format!("canvas not found: {id:?}")))?;
        Ok(entry.info.clone())
    }

    /// Return the buffer-pixel size of a canvas.
    /// This is what JS `canvas.width`/`canvas.height` reports (physical pixels,
    /// matching browser semantics).
    /// For the onscreen canvas with a DrawingBuffer, returns the DrawingBuffer
    /// dimensions (which may differ from the EGL surface dimensions).
    pub(crate) fn get_canvas_size(&self, id: CanvasId) -> EngineResult<(u32, u32)> {
        let entry = self
            .canvases
            .get(&id)
            .ok_or_else(|| ee(ErrorCode::NotFound, format!("canvas not found: {id:?}")))?;
        if let Some(ref db) = entry.drawing_buffer {
            Ok((db.width, db.height))
        } else {
            Ok((entry.physical_width, entry.physical_height))
        }
    }

    // ==================== 2D Context Management ====================

    pub(crate) fn init_skia_for_canvas(&mut self, canvas_id: CanvasId) -> EngineResult<()> {
        context_2d_impl::init_skia_for_canvas(self, canvas_id)
    }

    pub(crate) fn get_2d_context_mut(
        &mut self,
        canvas_id: CanvasId,
    ) -> EngineResult<&mut Canvas2DContext> {
        self.contexts_2d.get_mut(&canvas_id).ok_or_else(|| {
            ee(
                ErrorCode::NotFound,
                format!("2d context not found: canvas_id={canvas_id:?}"),
            )
        })
    }

    /// Split-borrow accessor: returns `(&mut 2d_ctx, &ImageStore)` so a
    /// single `drawImage` dispatch can wrap a stored GL texture into an
    /// `SkImage` (needs `&mut GrDirectContext`, owned by 2d_ctx) while
    /// also looking up the texture metadata (needs `&ImageStore`).
    ///
    /// Returns `NotFound` if the canvas has no 2D context yet.
    pub(crate) fn split_2d_and_images(
        &mut self,
        canvas_id: CanvasId,
    ) -> EngineResult<(
        &mut Canvas2DContext,
        &mut crate::backend::gl::image_store::ImageStore,
    )> {
        let ctx = self.contexts_2d.get_mut(&canvas_id).ok_or_else(|| {
            ee(
                ErrorCode::NotFound,
                format!("2d context not found: canvas_id={canvas_id:?}"),
            )
        })?;
        Ok((ctx, self.image_registry.store_mut()))
    }

    pub(crate) fn mark_2d_dirty(&mut self, canvas_id: CanvasId) {
        self.dirty_2d.insert(canvas_id);
    }

    /// Remove a canvas from the dirty-2D set after an explicit Materialize
    /// flush, preventing double-flush at present time.
    pub(crate) fn clear_2d_dirty(&mut self, canvas_id: CanvasId) {
        self.dirty_2d.remove(&canvas_id);
    }

    #[allow(dead_code)]
    pub(crate) fn mark_current_frame_requires_full_redraw(&mut self) {
        self.damage
            .add(crate::damage_effect::DamageEffect::FullSurface);
    }

    #[allow(dead_code)]
    pub(crate) fn stage_current_frame_partial_damage(&mut self, rect: [i32; 4], is_onscreen: bool) {
        if !is_onscreen {
            return;
        }
        let [x, y, width, height] = rect;
        if width <= 0 || height <= 0 {
            return;
        }
        self.damage
            .add(crate::damage_effect::DamageEffect::OnscreenRect {
                x,
                y,
                width,
                height,
            });
    }

    /// Feed a DamageEffect directly into the per-frame accumulator.
    pub(crate) fn add_damage(&mut self, effect: crate::damage_effect::DamageEffect) {
        self.damage.add(effect);
    }

    #[allow(dead_code)]
    pub(crate) fn pending_dirty_2d_count(&self) -> usize {
        self.dirty_2d.len()
    }

    #[allow(dead_code)]
    /// Save current GL state and set a safe baseline for Canvas2D / Skia
    /// text atlas uploads.
    /// Mark every live 2D context's Skia cache as stale.  Called
    /// only from fall-back paths where we can't identify the
    /// touched canvas (empty-canvas-id GL batches, panic recovery);
    /// normal WebGL batch dispatch uses [`mark_2d_context_stale`]
    /// for narrower invalidation.
    ///
    /// Unused today: every current caller of the fall-back paths
    /// described above reaches for the narrower
    /// [`Self::mark_all_2d_contexts_stale_bits`] instead. Kept as
    /// the documented full-invalidation escape hatch for a
    /// fall-back path that can't identify which bits are dirty.
    pub(crate) fn mark_all_2d_contexts_stale(&mut self) {
        for ctx in self.contexts_2d.values_mut() {
            ctx.mark_state_stale();
        }
    }

    /// Narrow-scope variant of [`mark_all_2d_contexts_stale`]: only
    /// invalidate the caller-declared state bits across every live
    /// 2D context.  Used by shared-context ops (AHB import, PBO
    /// upload) that mutate a *specific* subset of GL state — e.g.
    /// AHB import only touches the texture binding on the active
    /// unit, so there's no reason to force every Canvas2D context
    /// to re-send its entire tracked GL state before the next
    /// draw.
    pub(crate) fn mark_all_2d_contexts_stale_bits(&mut self, bits: u32) {
        for ctx in self.contexts_2d.values_mut() {
            ctx.mark_state_stale_bits(bits);
        }
    }

    /// Mark a SPECIFIC 2D context's Skia cache as stale.  Silent
    /// no-op when the canvas id has no 2D context (e.g. a
    /// WebGL-only canvas was the one targeted by the GL batch).
    /// The per-context `reset_gl_state_if_stale()` picks this up
    /// lazily on the next Skia draw for that canvas only.
    pub(crate) fn mark_2d_context_stale(&mut self, canvas_id: CanvasId) {
        if let Some(ctx) = self.contexts_2d.get_mut(&canvas_id) {
            ctx.mark_state_stale();
        }
    }

    #[allow(dead_code)]
    /// Narrow-scope variant of [`mark_2d_context_stale`]: the caller
    /// declares which slice of Skia's GL state needs invalidation.
    pub(crate) fn mark_2d_context_stale_bits(&mut self, canvas_id: CanvasId, bits: u32) {
        if let Some(ctx) = self.contexts_2d.get_mut(&canvas_id) {
            ctx.mark_state_stale_bits(bits);
        }
    }

    /// Begin a Skia-side GL scope for `canvas_id`.  Restores the 5
    /// raw-GL bindings Skia requires (active-tex, PBO, alignment) on
    /// drop and invalidates the per-canvas dedup shadow so WebGL
    /// can't serve stale cached state.  Canonical entry point for
    /// the Materialize / Present boundary.
    pub(crate) fn begin_canvas2d_gl_scope_for(
        &mut self,
        canvas_id: CanvasId,
    ) -> context_2d_impl::Canvas2DGlScopeGuard {
        let gl_ptr: *const glow::Context = &*self.gl;
        let shadow = self.gl_state.entry(canvas_id).or_default() as *mut _;
        // SAFETY: `self.gl` is never mutated; `gl_state` is borrowed
        // only through this pointer until the guard drops.  The
        // guard drops before any other method returns a borrow of
        // `self`.
        unsafe { context_2d_impl::begin_canvas2d_gl_scope(&*gl_ptr, Some(&mut *shadow)) }
    }

    pub(crate) fn flush_dirty_2d_contexts(&mut self) -> EngineResult<Vec<CanvasId>> {
        context_2d_impl::flush_dirty_2d_contexts(self)
    }

    // ==================== Image Management ====================

    pub(crate) fn load_shared_image(
        &mut self,
        image_id: u32,
        image: NormalizedImage,
    ) -> EngineResult<(u32, u32)> {
        self.ensure_any_canvas_current()?;
        let display_ptr = self.display.as_ptr() as *const std::ffi::c_void;
        let result = self.image_registry.load_shared_image(
            &self.gl,
            image_id,
            image,
            &self.device_caps,
            &self.gpu_caps,
            display_ptr,
        )?;
        // PBO / glTexImage2D uploads bind, parameterise, and
        // (optionally) use a PBO buffer behind Skia's back.
        // Declare only the bits we actually touched so the next
        // Canvas2D draw on any live context re-sends that GL
        // slice — not the whole tracked state.  See
        // `backend/gl/surface.rs::gr_state_bits`.
        self.mark_all_2d_contexts_stale_bits(
            crate::backend::gl::surface::gr_state_bits::TEXTURE_BINDING
                | crate::backend::gl::surface::gr_state_bits::PIXEL_STORE,
        );
        Ok(result)
    }

    /// GPU-side `glTexImage2D(image)`: copy from a previously uploaded
    /// shared image's GL texture into the destination texture
    /// currently bound to `target` on `canvas_id`.  Replaces the slow
    /// path of round-tripping CPU-side RGBA bytes back through the
    /// render thread for WebGL `texImage2D(image)` calls.
    ///
    /// The destination texture is whichever the caller bound via
    /// `gl.bindTexture(target, my_dst)` before issuing the WebGL
    /// call — same semantics as the regular `TexImage2D` path.
    /// Returns silently (with a `tracing::warn`) on lookup miss or
    /// FBO completeness failure so a stale alias never panics the
    /// render thread.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tex_image_2d_from_shared(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        internalformat: i32,
        source_shared_id: u32,
        src_width: i32,
        src_height: i32,
    ) -> EngineResult<()> {
        self.make_current_needed(canvas_id)?;

        let stored = match self.image_registry.get_shared_texture(source_shared_id) {
            Some(s) => s,
            None => {
                tracing::warn!(
                    "TexImage2DFromShared: source shared_id {} not found",
                    source_shared_id
                );
                return Ok(());
            }
        };

        let src_tex = match <glow::NativeTexture as NativeTextureFromRawShim>::try_from_raw(
            stored.gl_texture,
        ) {
            Some(t) => t,
            None => {
                tracing::warn!(
                    "TexImage2DFromShared: source texture handle is 0 for shared_id {}",
                    source_shared_id
                );
                return Ok(());
            }
        };

        let (sx, sy) = stored
            .atlas_origin
            .map(|(x, y)| (x as i32, y as i32))
            .unwrap_or((0, 0));

        let copy_fbo = self.ensure_image_copy_fbo(canvas_id)?;
        let status = crate::backend::gl::texture_copy::copy_texture(
            &self.gl,
            copy_fbo,
            src_tex,
            crate::backend::gl::texture_copy::TextureCopy::Image {
                target,
                level,
                internal_format: internalformat as u32,
                source_x: sx,
                source_y: sy,
                width: src_width,
                height: src_height,
            },
        );
        if status != glow::FRAMEBUFFER_COMPLETE {
            tracing::warn!("TexImage2DFromShared: read FBO incomplete: 0x{status:X}");
        }

        self.mark_all_2d_contexts_stale_bits(
            crate::backend::gl::surface::gr_state_bits::TEXTURE_BINDING,
        );
        Ok(())
    }

    fn ensure_image_copy_fbo(
        &mut self,
        canvas_id: CanvasId,
    ) -> EngineResult<glow::NativeFramebuffer> {
        if let Some(fbo) = self.image_copy_fbos.get(&canvas_id).copied() {
            return Ok(fbo);
        }
        let fbo = unsafe {
            self.gl.create_framebuffer().map_err(|e| {
                shared::error::EngineError::new(ErrorCode::Internal)
                    .with_msg("create_framebuffer failed for image copy FBO")
                    .with_detail(e)
            })?
        };
        self.image_copy_fbos.insert(canvas_id, fbo);
        Ok(fbo)
    }

    // -----------------------------------------------------------------
    // Canvas2D zero-readback snapshot pool (cocos text-rendering fast path)
    // -----------------------------------------------------------------
    //
    // hxddd 商城 spammed `getImageData(text)` → `texImage2D(data)` per
    // sprite, blocking V8 36-65 ms each call.  The snapshot pool keeps
    // the bytes on the GPU: `getImageData` produces a snapshot texture
    // (FBO blit with Y-flip to match CPU-path orientation), and
    // `texImage2D` consumes it via the existing `image_copy_fbos`
    // FBO + glCopyTexImage2D primitive.  See
    // `Canvas2DCmd::GetImageDataSnapshot` for the protocol.
    //
    // Tradeoff: Skia renders premultiplied alpha; our snapshot
    // reads the FB as-is, so the resulting texture is premul.  The
    // legacy CPU path explicitly converted to unpremul during
    // `read_pixels`.  For pure-black anti-aliased text (cocos's
    // dominant case: prices, button labels, item names) the bytes
    // are byte-identical because `0 * alpha == 0`.  Coloured AA
    // glyphs render with marginally darker fringes — acceptable for
    // the >10× perf win.  Games that need bit-exact unpremul bytes
    // can read `imageData.data` to fall back to the
    // legacy `read_image_data` synchronous path.

    /// Maximum live snapshot textures kept in the pool at any time.
    /// Frame-end drain releases everything from the prior frame, so
    /// this only matters within a single frame: getImageData →
    /// texImage2D pairs all queue snapshots that accumulate until
    /// frame-end execution.  Cocos商城 has been observed with ~200
    /// text sprites per frame (each price/button/item label takes
    /// one snapshot), so the cap must comfortably exceed that.
    ///
    /// Sizing: 1024 snapshots * 32 KiB (typical text strip = 200x40
    /// RGBA8) ≈ 32 MiB peak — high enough that the JS-side per-frame
    /// budget (`MAX_LIVE_CANVAS2D_SNAPSHOTS_JS`, currently 512 in
    /// `02_2d_context.js`) reaches its limit first and falls back to
    /// the legacy CPU readback well before the render-side cap is
    /// hit.  Render-side cap remains as a memory ceiling: when the
    /// JS-side budget is somehow bypassed (different runtime, old
    /// cached JS), we silently drop the snapshot rather than
    /// blowing past the 200 MB native-heap target.
    const MAX_LIVE_CANVAS2D_SNAPSHOTS: usize = 1024;
    /// Hard aggregate RGBA footprint for snapshots retained until frame-end.
    /// Reuse the synchronous-pixel ceiling so one temporary optimization pool
    /// cannot consume more GPU storage than the largest readback we otherwise
    /// permit. Typical text snapshots are much smaller than this budget.
    const MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES: usize =
        shared::protocol::render_cmd::MAX_SYNC_READBACK_BYTES;

    // The relationship these two caps must keep is enforced at compile time by
    // `SNAPSHOT_CAP_RELATIONS` at module scope — a free `const`, because an
    // unused *associated* const's initializer is not evaluated and an assertion
    // placed here proved nothing (verified by breaking a cap and watching the
    // build succeed).

    /// Capture a Canvas2D sub-rectangle into a GL texture using a
    /// caller-supplied `snapshot_id`.  Powers the fire-and-forget
    /// hot path where JS pre-allocated the id from a process-local
    /// counter — there is intentionally no sync wrapper; supporting
    /// both an internally-allocated and externally-allocated id
    /// risked counter-collision in the shared pool HashMap.
    ///
    /// The blit applies a vertical mirror so the resulting texture's
    /// GL row 0 (== bottom in GL coords) holds the JS top row of the
    /// captured region — matching the on-wire layout WebGL produces
    /// when uploading an unflipped `ImageData` (CPU path).  Without
    /// the mirror, every cocos text sprite would render upside-down.
    ///
    /// Returns `0` on failure (caller has already committed to the
    /// id; the consuming `TexImage2DFromSnapshot` will detect the
    /// missing pool entry and warn).
    pub(crate) fn snapshot_canvas2d_region_with_id(
        &mut self,
        canvas_id: CanvasId,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        snapshot_id: u32,
    ) -> EngineResult<u32> {
        if width == 0 || height == 0 {
            return Ok(0);
        }
        // `0` is the JS sentinel for "no snapshot", so an entry under that key
        // could never be looked up. Rejected here rather than after the capture:
        // this used to be checked just before the pool insert, by which point a
        // texture had been created, an FBO blit issued and the surrounding GL
        // state saved and restored — all of it for a result guaranteed to be
        // thrown away. `op_capture_canvas2d_snapshot` screens this JS-side too,
        // so reaching it means a different runtime or stale cached JS; the point
        // of defence in depth is to cost nothing when it holds.
        if snapshot_id == 0 {
            return Ok(0);
        }
        let snapshot_bytes =
            match shared::protocol::render_cmd::checked_canvas_rgba_byte_len(width, height) {
                Some(bytes) => bytes,
                None => {
                    return Err(ee(
                        ErrorCode::InvalidArgument,
                        format!(
                            "canvas snapshot {width}x{height} exceeds the synchronous RGBA cap"
                        ),
                    ));
                }
            };
        if self.canvas2d_snapshots.contains_key(&snapshot_id) {
            return Ok(0);
        }
        let Some(next_snapshot_bytes) = self.canvas2d_snapshot_bytes.checked_add(snapshot_bytes)
        else {
            return Ok(0);
        };
        if next_snapshot_bytes > Self::MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES {
            return Ok(0);
        }
        let (w_i32, h_i32) = (width as i32, height as i32);
        let Some((surface_width, surface_height)) = self
            .contexts_2d
            .get(&canvas_id)
            .map(|ctx| (ctx.width as i32, ctx.height as i32))
        else {
            return Ok(0);
        };
        let Some(right) = x.checked_add(w_i32) else {
            return Ok(0);
        };
        let Some(bottom) = y.checked_add(h_i32) else {
            return Ok(0);
        };
        if x < 0 || y < 0 || right > surface_width || bottom > surface_height {
            return Ok(0);
        }
        // GLES 3.0+ required for glBlitFramebuffer.  On GLES 2 we
        // bail; JS-side falls back to op_get_image_data + texImage2D.
        if self.gles_major < 3 {
            return Ok(0);
        }
        // Pool already at cap → return 0 so JS falls back to the
        // legacy CPU path.  Eviction-and-overwrite would corrupt an
        // in-flight snapshot whose texImage2DFromSnapshot is still
        // pending in the frame collector.
        if self.canvas2d_snapshot_order.len() >= Self::MAX_LIVE_CANVAS2D_SNAPSHOTS {
            return Ok(0);
        }
        self.make_current_needed(canvas_id)?;

        // Flush the source Canvas2D batch so the FB content is up to
        // date before we sample it.  Mirrors what `read_image_data`
        // does for the legacy path.
        let surface_height = match self.contexts_2d.get_mut(&canvas_id) {
            Some(ctx) => {
                ctx.reset_gl_state_if_stale();
                ctx.flush_and_submit();
                ctx.height as i32
            }
            None => return Ok(0),
        };

        // Force the GPU to actually complete Skia's submitted draws
        // before the subsequent `glBlitFramebuffer` samples this
        // canvas's FBO.  `flush_and_submit` only enqueues commands;
        // on Mali tile-based GPUs (Kirin 980 / HUAWEI EMUI) we have
        // observed the blit reading pre-draw tile contents when no
        // explicit sync is inserted — symptom: cocos text labels
        // intermittently upload as blank textures.
        //
        // `glFenceSync` + `glClientWaitSync` is the targeted form of
        // `glFinish`: it only waits for the commands queued before
        // this fence (Skia's `flushAndSubmit`), not for anything
        // submitted later or on other contexts.  `SYNC_FLUSH_COMMANDS_BIT`
        // gives `glFlush` semantics for free so the fence is
        // guaranteed to make it to the GPU.  Mirrors the upload-thread
        // pattern in `upload_thread.rs`.
        unsafe {
            if let Ok(fence) = self.gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) {
                let waited =
                    self.gl
                        .client_wait_sync(fence, glow::SYNC_FLUSH_COMMANDS_BIT, i32::MAX);
                self.snapshot_fence_waits = self.snapshot_fence_waits.saturating_add(1);
                let status = classify_snapshot_fence(waited);
                if status != SnapshotFenceStatus::Signalled {
                    let (message, cause) = match status {
                        SnapshotFenceStatus::Timeout => (
                            "snapshot_canvas2d_region: pre-blit fence timed out; stale copy suppressed",
                            "timed out",
                        ),
                        SnapshotFenceStatus::Failed => (
                            "snapshot_canvas2d_region: pre-blit fence failed; stale copy suppressed",
                            "failed",
                        ),
                        SnapshotFenceStatus::Signalled => unreachable!(),
                    };
                    tracing::warn!(
                        canvas_id = ?canvas_id,
                        "snapshot_canvas2d_region: pre-blit fence {cause} \
                         (0x{waited:04X}); no snapshot was published"
                    );
                    self.gl.delete_sync(fence);
                    return Err(ee(ErrorCode::RenderBackendError, message));
                }
                self.gl.delete_sync(fence);
            }
        }

        let src_fbo_raw = match self.contexts_2d.get(&canvas_id) {
            Some(ctx) => ctx.fbo_id,
            None => return Ok(0),
        };
        let src_fbo =
            <glow::NativeFramebuffer as NativeFramebufferFromRawShim>::try_from_raw(src_fbo_raw);
        // src_fbo == None means "default framebuffer"; pass `None` to
        // bind FBO 0 explicitly.

        // Allocate the destination texture before touching FBO state
        // so we can roll back cleanly on alloc failure.
        let dest_tex = unsafe {
            self.gl.create_texture().map_err(|e| {
                shared::error::EngineError::new(ErrorCode::Internal)
                    .with_msg("snapshot_canvas2d_region: create_texture failed")
                    .with_detail(e)
            })?
        };

        // Save GL state we're about to mutate so the surrounding
        // WebGL/Canvas2D batch sees no observable change.
        let prev_active_texture = unsafe { self.gl.get_parameter_i32(glow::ACTIVE_TEXTURE) };
        let prev_tex_2d = unsafe { self.gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) };
        let prev_read_fbo = unsafe { self.gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING) };
        let prev_draw_fbo = unsafe { self.gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) };
        // `glBlitFramebuffer` writes to the DRAW framebuffer *through the
        // scissor test*, and Skia leaves its own scissor behind after painting:
        // a fill that does not span the full surface produces a box with a
        // non-zero origin. This blit writes to (0,0) of a fresh destination
        // texture, so such a box rejects every pixel and the snapshot comes
        // back fully transparent -- with no GL error, because a fully
        // scissored blit is not an error.
        //
        // The consequence is not cosmetic: `getImageData` is backed by these
        // snapshots, so content that builds a sprite texture from a readback
        // (Migo's own bunnymark does), hit-tests against one, or runs an image
        // effect on one silently gets an empty buffer. Measured: a fill
        // covering a quarter of the surface was enough.
        //
        // Same reasoning, same fix as the DrawingBuffer present blit in
        // `drawing_buffer.rs` -- a snapshot is system-level work, not part of
        // the content's draw state, so it must not inherit the content's clip.
        let prev_scissor_enabled = unsafe { self.gl.is_enabled(glow::SCISSOR_TEST) };
        if prev_scissor_enabled {
            unsafe { self.gl.disable(glow::SCISSOR_TEST) };
        }

        // Allocate storage on the destination texture.
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(dest_tex));
            // Match CPU-path texture parameters: linear filter, clamp.
            // Cocos sets these explicitly post-upload, so the values
            // are mostly cosmetic — but a complete texture must have
            // a min-filter that doesn't require mipmaps.
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            // Use sized internal format GL_RGBA8 (0x8058) so the
            // FBO attachment is guaranteed color-renderable across
            // GLES 3 drivers.  Unsized GL_RGBA is GLES 2 style and
            // some Mali / Adreni drivers reject it as a colour-
            // attachment source for glBlitFramebuffer / glCopy.
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                w_i32,
                h_i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
        }

        // Lazy-init the temp FBOs.  Read FBO is not used in this
        // function but the snapshot upload path needs it; create it
        // here too so destroy_all has a single deletion site.
        let blit_fbo = match self.canvas2d_snapshot_blit_fbo {
            Some(f) => f,
            None => {
                let f = unsafe {
                    self.gl.create_framebuffer().map_err(|e| {
                        shared::error::EngineError::new(ErrorCode::Internal)
                            .with_msg("snapshot blit FBO alloc failed")
                            .with_detail(e)
                    })?
                };
                self.canvas2d_snapshot_blit_fbo = Some(f);
                f
            }
        };

        unsafe {
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, src_fbo);
            self.gl
                .bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(blit_fbo));
            self.gl.framebuffer_texture_2d(
                glow::DRAW_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(dest_tex),
                0,
            );
            let status = self.gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                tracing::warn!(
                    "snapshot_canvas2d_region: draw FBO incomplete: 0x{:X}",
                    status
                );
                self.gl.framebuffer_texture_2d(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    None,
                    0,
                );
                self.gl.delete_texture(dest_tex);
                self.restore_state_after_snapshot(
                    prev_active_texture,
                    prev_tex_2d,
                    prev_read_fbo,
                    prev_draw_fbo,
                    prev_scissor_enabled,
                );
                return Ok(0);
            }
            // Mirror Y: srcY0 = top edge in GL coords (== JS y_min),
            // srcY1 = bottom edge.  GL spec: src(srcX0, srcY0) maps to
            // dst(dstX0, dstY0).  With dstY0=0 (GL bottom of dst tex)
            // and srcY0 = surface_h - y (the top of the JS region in
            // GL coords), the destination texture's GL row 0 ends up
            // holding the JS top row — exactly the layout the
            // downstream `glCopyTexImage2D` upload needs to land on
            // the same GL coords as the legacy CPU path's
            // `texImage2D(unpremul_bytes)`.
            let src_y_top = surface_height - y;
            let src_y_bot = surface_height - y - h_i32;
            self.gl.blit_framebuffer(
                x,
                src_y_top,
                x + w_i32,
                src_y_bot,
                0,
                0,
                w_i32,
                h_i32,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            // Detach so the texture lifetime isn't pinned by the FBO
            // beyond the blit.
            self.gl.framebuffer_texture_2d(
                glow::DRAW_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                None,
                0,
            );
        }

        self.restore_state_after_snapshot(
            prev_active_texture,
            prev_tex_2d,
            prev_read_fbo,
            prev_draw_fbo,
            prev_scissor_enabled,
        );

        // We mutated ACTIVE_TEXTURE/TEXTURE_BINDING_2D and the FBO
        // bindings out from under Skia.  Tell every live Canvas2D
        // context that those slices are stale so the next 2D draw
        // re-syncs Skia's tracking.
        self.mark_all_2d_contexts_stale_bits(
            crate::backend::gl::surface::gr_state_bits::TEXTURE_BINDING
                | crate::backend::gl::surface::gr_state_bits::RENDER_TARGET,
        );

        self.canvas2d_snapshots.insert(
            snapshot_id,
            Canvas2DSnapshotEntry {
                tex: dest_tex,
                width,
                height,
                bytes: snapshot_bytes,
                cache_key: None,
            },
        );
        self.canvas2d_snapshot_order.push_back(snapshot_id);
        self.canvas2d_snapshot_bytes = next_snapshot_bytes;
        Ok(snapshot_id)
    }

    /// Tag a previously-captured snapshot with a text-cache key so
    /// the next `drain_canvas2d_snapshots` hands its GL texture off
    /// to this session's text texture cache instead of deleting it.
    /// Called by the dispatcher after a `CaptureSnapshot { cache_key:
    /// Some(_), .. }` succeeded.  No-op when the snapshot is absent
    /// (capture failed earlier in the same packet).
    pub(crate) fn mark_snapshot_for_text_cache(
        &mut self,
        snapshot_id: u32,
        key: Box<shared::text_texture_cache::TextCacheKey>,
    ) {
        if let Some(entry) = self.canvas2d_snapshots.get_mut(&snapshot_id) {
            entry.cache_key = Some(key);
        }
    }

    fn remove_canvas2d_snapshot(&mut self, snapshot_id: u32) -> Option<Canvas2DSnapshotEntry> {
        let entry = self.canvas2d_snapshots.remove(&snapshot_id)?;
        self.canvas2d_snapshot_order.retain(|&id| id != snapshot_id);
        self.canvas2d_snapshot_bytes = match self.canvas2d_snapshot_bytes.checked_sub(entry.bytes) {
            Some(remaining) => remaining,
            None => {
                // An accounting bug must fail closed in production: keeping the
                // budget exhausted is safer than opening an unbounded allocation
                // path, and unlike a panic it still lets the render thread tear
                // down the already-created resources.
                tracing::error!(
                    live_bytes = self.canvas2d_snapshot_bytes,
                    removed_bytes = entry.bytes,
                    "Canvas2D snapshot byte accounting underflow"
                );
                Self::MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES
            }
        };
        Some(entry)
    }

    /// Helper used by [`Self::snapshot_canvas2d_region`] to roll
    /// back the GL state we mutated for the blit.
    fn restore_state_after_snapshot(
        &self,
        prev_active_texture: i32,
        prev_tex_2d: i32,
        prev_read_fbo: i32,
        prev_draw_fbo: i32,
        prev_scissor_enabled: bool,
    ) {
        unsafe {
            // Restored here rather than at the blit so it happens on every exit
            // path, including the early returns that abandon the snapshot.
            if prev_scissor_enabled {
                self.gl.enable(glow::SCISSOR_TEST);
            }
            // ACTIVE_TEXTURE first because the texture binding is
            // unit-scoped.
            if prev_active_texture as u32 >= glow::TEXTURE0 {
                self.gl.active_texture(prev_active_texture as u32);
            }
            let prev_tex =
                <glow::NativeTexture as NativeTextureFromRawShim>::try_from_raw(prev_tex_2d as u32);
            self.gl.bind_texture(glow::TEXTURE_2D, prev_tex);
            let prev_read = <glow::NativeFramebuffer as NativeFramebufferFromRawShim>::try_from_raw(
                prev_read_fbo as u32,
            );
            let prev_draw = <glow::NativeFramebuffer as NativeFramebufferFromRawShim>::try_from_raw(
                prev_draw_fbo as u32,
            );
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, prev_read);
            self.gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, prev_draw);
        }
    }

    /// Upload a previously captured snapshot texture into the
    /// destination texture currently bound to `target` on
    /// `canvas_id`.  Mirrors [`Self::tex_image_2d_from_shared`] but
    /// pulls from the snapshot pool.
    pub(crate) fn tex_image_2d_from_canvas2d_snapshot(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        internalformat: i32,
        snapshot_id: u32,
    ) -> EngineResult<()> {
        let entry = match self.canvas2d_snapshots.get(&snapshot_id) {
            Some(e) => e.clone(),
            None => {
                tracing::warn!(
                    "TexImage2DFromSnapshot: snapshot_id {} not in pool (frame drain race?)",
                    snapshot_id
                );
                return Ok(());
            }
        };
        self.make_current_needed(canvas_id)?;

        // Reuse the per-canvas image_copy_fbo as the READ framebuffer
        // — exactly the same primitive `tex_image_2d_from_shared`
        // uses, so the same driver paths are exercised.
        let copy_fbo = self.ensure_image_copy_fbo(canvas_id)?;
        let status = crate::backend::gl::texture_copy::copy_texture(
            &self.gl,
            copy_fbo,
            entry.tex,
            crate::backend::gl::texture_copy::TextureCopy::Image {
                target,
                level,
                internal_format: internalformat as u32,
                source_x: 0,
                source_y: 0,
                width: entry.width as i32,
                height: entry.height as i32,
            },
        );
        if status != glow::FRAMEBUFFER_COMPLETE {
            tracing::warn!("TexImage2DFromSnapshot: read FBO incomplete: 0x{status:X}");
        }

        self.mark_all_2d_contexts_stale_bits(
            crate::backend::gl::surface::gr_state_bits::TEXTURE_BINDING,
        );
        Ok(())
    }

    /// Text texture cache hit path: copy from the cached source
    /// texture (lives in this session's cache, [`Self::text_cache`])
    /// into the destination texture currently bound to `target` on
    /// `canvas_id`.  Mirrors `tex_image_2d_from_canvas2d_snapshot`'s
    /// FBO + `glCopyTexImage2D` shape — same correctness story, just
    /// the source texture comes from the session text cache instead
    /// of the per-frame snapshot pool.  Because the cache is per
    /// session, the name it returns was minted in this manager's own
    /// EGL context.
    ///
    /// Always unpins `key` on return (success OR error path), so a
    /// JS-side pin acquired at fillText time is balanced exactly
    /// once.  A miss (cache evicted between JS lookup + render
    /// execution despite the pin) returns `Ok(false)` to signal the
    /// caller "we did nothing"; the caller has no way to recover
    /// (the original fillText was suppressed) so it warns.  The pin
    /// guarantees this should not happen in practice.
    pub(crate) fn tex_image_2d_from_text_cache(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        internalformat: i32,
        key: &shared::text_texture_cache::TextCacheKey,
    ) -> EngineResult<bool> {
        let (src_tex_raw, width, height) = {
            let mut cache = self.text_cache.lock();
            let lookup = cache.get(key);
            // Always unpin once, regardless of hit/miss — the JS-side
            // pin is balanced by this render-thread call.  Doing it
            // BEFORE we drop the lock keeps the bookkeeping atomic
            // with the lookup.
            cache.unpin(key);
            match lookup {
                Some(entry) => (entry.texture_id, entry.width, entry.height),
                None => return Ok(false),
            }
        };

        let src_tex =
            match <glow::NativeTexture as NativeTextureFromRawShim>::try_from_raw(src_tex_raw) {
                Some(t) => t,
                None => return Ok(false),
            };

        self.make_current_needed(canvas_id)?;
        let copy_fbo = self.ensure_image_copy_fbo(canvas_id)?;
        let status = crate::backend::gl::texture_copy::copy_texture(
            &self.gl,
            copy_fbo,
            src_tex,
            crate::backend::gl::texture_copy::TextureCopy::Image {
                target,
                level,
                internal_format: internalformat as u32,
                source_x: 0,
                source_y: 0,
                width: width as i32,
                height: height as i32,
            },
        );
        if status != glow::FRAMEBUFFER_COMPLETE {
            tracing::warn!("TexImage2DFromTextCache: read FBO incomplete: 0x{status:X}");
        }

        self.mark_all_2d_contexts_stale_bits(
            crate::backend::gl::surface::gr_state_bits::TEXTURE_BINDING,
        );
        Ok(true)
    }

    /// Reserved snapshot id for the direct `tex_image_2d_from_canvas2d`
    /// path: the entry is captured + consumed + freed within a single
    /// render-thread call, so the slot is never observable to anyone
    /// else.  Sentinel chosen at the very top of the u32 range so JS-
    /// allocated ids (which start at 1 and increment) effectively can
    /// never reach it.
    const DIRECT_CANVAS2D_RESERVED_ID: u32 = u32::MAX;

    /// Direct GPU->GPU upload from a 2D canvas's framebuffer to the
    /// WebGL texture currently bound to `target` on `canvas_id`.
    /// Combines `snapshot_canvas2d_region_with_id` and
    /// `tex_image_2d_from_canvas2d_snapshot` so the cocos
    /// `gl.texImage2D(target, ..., HTMLCanvasElement)` pattern never
    /// has to round-trip through `getImageData` + a sync readback --
    /// previously ~50ms V8 stall per label, ~20 labels per cocos popup
    /// open.
    pub(crate) fn tex_image_2d_from_canvas2d_direct(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        internalformat: i32,
        canvas_2d_id: CanvasId,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> EngineResult<()> {
        let id = Self::DIRECT_CANVAS2D_RESERVED_ID;

        // Defence: free any leftover entry from a prior direct call
        // that errored before the post-upload cleanup ran.
        if let Some(entry) = self.remove_canvas2d_snapshot(id) {
            unsafe {
                self.gl.delete_texture(entry.tex);
            }
        }

        // Capture into the reserved slot.  Returns 0 on failure (pool
        // full, GLES 2, zero area, or FBO-incomplete) -- in which case
        // we silently drop the upload, mirroring the pre-existing
        // `texImage2D` fallback contract.
        let captured =
            self.snapshot_canvas2d_region_with_id(canvas_2d_id, x, y, width, height, id)?;
        if captured == 0 {
            return Ok(());
        }

        // Upload into the texture currently bound on `canvas_id`.
        self.tex_image_2d_from_canvas2d_snapshot(canvas_id, target, level, internalformat, id)?;

        // Free immediately.  The drain at frame end would clean it up
        // anyway, but we'd rather not pin the slot for a whole frame.
        if let Some(entry) = self.remove_canvas2d_snapshot(id) {
            unsafe {
                self.gl.delete_texture(entry.tex);
            }
        }
        Ok(())
    }

    /// Sub-region variant of `tex_image_2d_from_canvas2d_direct`.
    /// Mirrors `tex_sub_image_2d_from_canvas2d_snapshot` for the cocos
    /// text-atlas pattern that streams glyph cells in via
    /// `gl.texSubImage2D(..., HTMLCanvasElement)`.
    pub(crate) fn tex_sub_image_2d_from_canvas2d_direct(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        canvas_2d_id: CanvasId,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> EngineResult<()> {
        let id = Self::DIRECT_CANVAS2D_RESERVED_ID;

        if let Some(entry) = self.remove_canvas2d_snapshot(id) {
            unsafe {
                self.gl.delete_texture(entry.tex);
            }
        }

        let captured =
            self.snapshot_canvas2d_region_with_id(canvas_2d_id, x, y, width, height, id)?;
        if captured == 0 {
            return Ok(());
        }

        self.tex_sub_image_2d_from_canvas2d_snapshot(
            canvas_id, target, level, xoffset, yoffset, id,
        )?;

        if let Some(entry) = self.remove_canvas2d_snapshot(id) {
            unsafe {
                self.gl.delete_texture(entry.tex);
            }
        }
        Ok(())
    }

    /// Sub-region variant of `tex_image_2d_from_canvas2d_snapshot`.
    /// Uses `glCopyTexSubImage2D` to copy the entire snapshot texture
    /// into the destination texture currently bound to `target` on
    /// `canvas_id`, anchored at (`xoffset`, `yoffset`).  Required for
    /// cocos-style text atlases that pre-allocate via `texImage2D` and
    /// stream glyphs in via `texSubImage2D`.
    pub(crate) fn tex_sub_image_2d_from_canvas2d_snapshot(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        snapshot_id: u32,
    ) -> EngineResult<()> {
        let entry = match self.canvas2d_snapshots.get(&snapshot_id) {
            Some(e) => e.clone(),
            None => {
                tracing::warn!(
                    "TexSubImage2DFromSnapshot: snapshot_id {} not in pool (frame drain race?)",
                    snapshot_id
                );
                return Ok(());
            }
        };
        self.make_current_needed(canvas_id)?;

        let copy_fbo = self.ensure_image_copy_fbo(canvas_id)?;
        let status = crate::backend::gl::texture_copy::copy_texture(
            &self.gl,
            copy_fbo,
            entry.tex,
            crate::backend::gl::texture_copy::TextureCopy::SubImage {
                target,
                level,
                xoffset,
                yoffset,
                width: entry.width as i32,
                height: entry.height as i32,
            },
        );
        if status != glow::FRAMEBUFFER_COMPLETE {
            tracing::warn!("TexSubImage2DFromSnapshot: read FBO incomplete: 0x{status:X}");
        }

        self.mark_all_2d_contexts_stale_bits(
            crate::backend::gl::surface::gr_state_bits::TEXTURE_BINDING,
        );
        Ok(())
    }

    /// Sync CPU readback of a snapshot texture, used by
    /// lazy `ImageData.data` getter. Layout matches the
    /// legacy CPU path: top-down RGBA8 rows, length `w * h * 4`.
    /// Empty `Vec` for a missing snapshot or incomplete FBO; allocation and
    /// context errors are returned to the caller.
    pub(crate) fn read_canvas2d_snapshot_pixels(
        &mut self,
        snapshot_id: u32,
    ) -> EngineResult<Vec<u8>> {
        let entry = match self.canvas2d_snapshots.get(&snapshot_id) {
            Some(e) => e.clone(),
            None => return Ok(Vec::new()),
        };
        let readback = crate::backend::gl::readback::Rgba8Readback::new(entry.width, entry.height)?;
        // Need any current GL context to issue commands.  Hop on
        // whichever canvas is convenient — the snapshot tex is
        // shared across the EGL share group.
        self.ensure_any_canvas_current()?;

        let read_fbo = match self.canvas2d_snapshot_read_fbo {
            Some(f) => f,
            None => {
                let f = unsafe {
                    self.gl.create_framebuffer().map_err(|e| {
                        shared::error::EngineError::new(ErrorCode::Internal)
                            .with_msg("snapshot read FBO alloc failed")
                            .with_detail(e)
                    })?
                };
                self.canvas2d_snapshot_read_fbo = Some(f);
                f
            }
        };

        let prev_read_fbo =
            unsafe { self.gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING) as u32 };

        let out = unsafe {
            self.gl
                .bind_framebuffer(glow::READ_FRAMEBUFFER, Some(read_fbo));
            self.gl.framebuffer_texture_2d(
                glow::READ_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(entry.tex),
                0,
            );
            let status = self.gl.check_framebuffer_status(glow::READ_FRAMEBUFFER);
            let out = if status == glow::FRAMEBUFFER_COMPLETE {
                readback.read(&self.gl)
            } else {
                tracing::warn!(
                    "read_canvas2d_snapshot_pixels: FBO incomplete: 0x{:X}",
                    status
                );
                Vec::new()
            };
            self.gl.framebuffer_texture_2d(
                glow::READ_FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                None,
                0,
            );
            let prev = <glow::NativeFramebuffer as NativeFramebufferFromRawShim>::try_from_raw(
                prev_read_fbo,
            );
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, prev);
            out
        };
        Ok(out)
    }

    /// Drain every live snapshot.  Called at frame-end so the
    /// `getImageData` → `texImage2D` pattern within a frame stays
    /// GPU-only without leaking textures across frames.
    ///
    /// Snapshots tagged with `cache_key` (the cocos text miss path)
    /// have their texture transferred to this session's text texture
    /// cache instead of being deleted, so a subsequent identical
    /// fillText resolves through `TexImage2DFromTextCache` without a
    /// re-render.  The cache's own LRU may evict an older entry to make
    /// room — any returned victim texture ids are deleted here as part
    /// of the same drain, and they can only ever be this session's own
    /// names because the cache is per session.
    pub(crate) fn drain_canvas2d_snapshots(&mut self) {
        if self.canvas2d_snapshots.is_empty() {
            return;
        }
        // Need a current GL context for `glDeleteTextures`.
        if self.ensure_any_canvas_current().is_err() {
            // No live canvas yet: retain the entries and their texture handles.
            // A later canvas recreation can make a context current and delete
            // them. Clearing the map here would leak the textures until EGL
            // teardown and would also lose the aggregate-byte accounting.
            return;
        }

        // First pass: split into (delete now) and (hand off to text
        // cache).  Doing the cache inserts after we've collected all
        // entries means we touch the text cache mutex only once even
        // when many entries are being moved.
        let drained: Vec<(u32, Canvas2DSnapshotEntry)> = self.canvas2d_snapshots.drain().collect();
        self.canvas2d_snapshot_order.clear();
        self.canvas2d_snapshot_bytes = 0;

        let mut to_delete: Vec<glow::NativeTexture> = Vec::new();
        let mut to_cache: Vec<Canvas2DSnapshotEntry> = Vec::new();
        for (_id, entry) in drained {
            if entry.cache_key.is_some() {
                to_cache.push(entry);
            } else {
                to_delete.push(entry.tex);
            }
        }

        if !to_cache.is_empty() {
            let mut cache = self.text_cache.lock();
            for entry in to_cache {
                let key = entry.cache_key.expect("cache_key checked above");
                let cached = shared::text_texture_cache::CachedTextEntry {
                    texture_id: entry.tex.0.get(),
                    width: entry.width,
                    height: entry.height,
                    size_bytes: entry.bytes,
                };
                let evicted_ids = cache.insert(*key, cached);
                for raw in evicted_ids {
                    if let Some(t) =
                        <glow::NativeTexture as NativeTextureFromRawShim>::try_from_raw(raw)
                    {
                        to_delete.push(t);
                    }
                }
            }
            // Publish gauges with the post-insert state.
            let stats = cache.stats();
            drop(cache);
            crate::render_diagnostics::set_text_cache_gauges(
                stats.size_bytes as u32,
                stats.entries as u32,
            );
        }

        unsafe {
            for tex in to_delete {
                self.gl.delete_texture(tex);
            }
        }
    }

    /// Zero-copy AHB upload path. See
    /// [`crate::canvas::manager::image::ImageRegistry::load_ahb_image`]
    /// for the fallback semantics.
    pub(crate) fn load_ahb_image(
        &mut self,
        image_id: u32,
        ahb_image: shared::protocol::io_cmd::AhbImage,
    ) -> EngineResult<(u32, u32)> {
        self.ensure_any_canvas_current()?;
        let display_ptr = self.display.as_ptr() as *const std::ffi::c_void;
        let result = self.image_registry.load_ahb_image(
            &self.gl,
            image_id,
            ahb_image,
            &self.device_caps,
            &self.gpu_caps,
            display_ptr,
        )?;
        // AHB → EGLImage → `glEGLImageTargetTexture2DOES` mutates the active
        // GL_TEXTURE_2D binding on texture unit 0 out from under Skia. An AHB
        // failure can also fall through to PBO/synchronous upload, which
        // changes pixel-store state. Declare both slices stale: without this
        // the next Canvas2D draw may sample the wrong texture or reuse an
        // incorrect unpack alignment. See Skia's
        // `AHardwareBufferGL.cpp::GrAHardwareBufferUtils` which
        // does the same `resetContext(kTextureBinding)` dance.
        self.mark_all_2d_contexts_stale_bits(
            crate::backend::gl::surface::gr_state_bits::TEXTURE_BINDING
                | crate::backend::gl::surface::gr_state_bits::PIXEL_STORE,
        );
        Ok(result)
    }

    /// Upload a compressed texture (KTX2/ETC2/ASTC) directly to the GPU.
    ///
    /// Bypasses RGBA decode and PBO upload — calls `glCompressedTexImage2D`
    /// directly. Falls back to the standard RGBA path if the GPU doesn't
    /// support the compressed format.
    pub(crate) fn load_compressed_image(
        &mut self,
        image_id: u32,
        compressed: &shared::protocol::io_cmd::CompressedImage,
    ) -> EngineResult<(u32, u32)> {
        use shared::error::EngineError;

        // Sink-side pixel cap: the io layer already rejects oversized KTX2, but
        // this is the last line before glCompressedTexImage2D allocates a GPU
        // texture, so guard here too against any future producer that bypasses
        // the io cap. Uses the shared single-source-of-truth constant so io and
        // graphics can't drift.
        let cap = shared::protocol::io_cmd::MAX_IMAGE_PIXELS;
        let px = (compressed.width as u64).saturating_mul(compressed.height as u64);
        if px > cap {
            return Err(
                EngineError::new(ErrorCode::OutOfMemory).with_detail(format!(
                    "compressed texture {}x{} ({} px) exceeds cap ({} px); refusing GPU upload",
                    compressed.width, compressed.height, px, cap
                )),
            );
        }

        self.ensure_any_canvas_current()?;

        let format =
            crate::compressed_upload::CompressedFormat::from_vk_format(compressed.vk_format)
                .ok_or_else(|| {
                    EngineError::new(ErrorCode::Unsupported).with_detail(format!(
                        "unsupported compressed format: {}",
                        compressed.vk_format
                    ))
                })?;

        if !self
            .device_caps
            .compressed_format_support
            .is_supported(format)
        {
            tracing::warn!(
                "GPU does not support {}, image_id={}",
                format.label(),
                image_id,
            );
            return Err(EngineError::new(ErrorCode::Unsupported)
                .with_detail(format!("GPU does not support {}", format.label())));
        }

        // Reject a malformed KTX2 whose level byte lengths don't match its
        // declared dimensions before the driver would fail the upload with
        // GL_INVALID_VALUE (glCompressedTexImage2D requires an exact size).
        // Checked over the whole chain: one that fails partway through uploading
        // leaves a texture that samples garbage rather than reporting an error.
        let levels: Vec<&[u8]> = compressed.levels().collect();
        if let Err(reason) = crate::compressed_upload::validate_mip_chain(
            format,
            compressed.width,
            compressed.height,
            &levels,
        ) {
            return Err(
                EngineError::new(ErrorCode::InvalidArgument).with_detail(format!(
                    "compressed KTX2 {} {}x{} with {} level(s): {}",
                    format.label(),
                    compressed.width,
                    compressed.height,
                    levels.len(),
                    reason
                )),
            );
        }

        let texture = crate::compressed_upload::upload_compressed_texture(
            &self.gl,
            format,
            compressed.width,
            compressed.height,
            &levels,
            self.device_caps.has_pbo,
        )
        .ok_or_else(|| {
            EngineError::new(ErrorCode::Unsupported).with_detail("glCompressedTexImage2D failed")
        })?;

        let info = crate::backend::gl::image_store::GpuImageInfo::rgba8_unpremul(
            compressed.width,
            compressed.height,
        );
        self.image_registry
            .register_shared_texture(image_id, texture, info);

        tracing::debug!(
            "compressed texture uploaded: image_id={} {}x{} {}",
            image_id,
            compressed.width,
            compressed.height,
            format.label(),
        );

        Ok((compressed.width, compressed.height))
    }

    /// Submit a texture upload to the upload thread (async, non-blocking).
    ///
    /// On success, the `resp` is stored and will be sent from
    /// `drain_upload_completed()` once the GPU fence signals and the
    /// texture is actually registered in the image registry.
    ///
    /// Returns `Err(resp)` if the upload thread is unavailable or degraded,
    /// giving the resp back so the caller can fall back to sync upload.
    pub(crate) fn submit_async_upload(
        &mut self,
        image_id: u32,
        image: &NormalizedImage,
        resp: shared::protocol::render_cmd::RenderCmdResp<(u32, u32)>,
    ) -> Result<(), shared::protocol::render_cmd::RenderCmdResp<(u32, u32)>> {
        let upload = match self.upload_thread.as_ref() {
            Some(u) if !u.is_degraded() => u,
            _ => return Err(resp),
        };
        let job = crate::upload_thread::UploadJob {
            image_id: image_id as u64,
            width: image.width,
            height: image.height,
            rgba: image.rgba.clone(),
        };

        // Budget gate: reject if per-frame upload budget is exhausted.
        if let Some(ref mut server) = self.upload_server {
            if !server.try_acquire_job(&job) {
                tracing::debug!(
                    "Upload budget exhausted: rejecting image_id={} ({} bytes), queue_depth={}",
                    image_id,
                    job.byte_len(),
                    server.queue_depth(),
                );
                return Err(resp);
            }
        }

        if upload.submit(job) {
            self.pending_load_responses.insert(image_id as u64, resp);
            Ok(())
        } else {
            // Channel full — release the budget we just acquired.
            if let Some(ref mut server) = self.upload_server {
                let release_job = crate::upload_thread::UploadJob {
                    image_id: image_id as u64,
                    width: image.width,
                    height: image.height,
                    rgba: image.rgba.clone(),
                };
                server.finish_job(&release_job);
            }
            Err(resp)
        }
    }

    /// Defer a budget-rejected upload for retry on the next frame.
    /// Returns the `resp` back (as `Err`) if the queue is full, so the
    /// caller can choose between synchronous upload (last-resort
    /// fallback) and dropping the request.
    ///
    /// The queue is FIFO to preserve request ordering so
    /// `Image.onload` fires in the order `load_image` ops were issued.
    pub(crate) fn defer_upload(
        &mut self,
        image_id: u32,
        image: shared::protocol::io_cmd::NormalizedImage,
        resp: shared::protocol::render_cmd::RenderCmdResp<(u32, u32)>,
    ) -> Result<
        (),
        (
            shared::protocol::io_cmd::NormalizedImage,
            shared::protocol::render_cmd::RenderCmdResp<(u32, u32)>,
        ),
    > {
        let bytes = image.rgba.len();
        if !deferred_upload_fits(
            self.deferred_uploads.len(),
            self.deferred_upload_bytes,
            bytes,
        ) {
            return Err((image, resp));
        }
        self.deferred_upload_bytes += bytes;
        self.deferred_uploads.push_back(DeferredUpload {
            image_id,
            image,
            resp,
        });
        Ok(())
    }

    /// Retry budget-rejected uploads from the head of the deferred
    /// queue.  Call at the top of each frame (before draw dispatch)
    /// so the budget window — which resets on every vsync tick — can
    /// absorb the backlog first instead of the frame's new draws.
    ///
    /// Stops on the first budget rejection: the per-frame budget is
    /// monotonic within a frame, so once it rejects a small-ish
    /// upload nothing larger will fit either.  Leaves the rest of the
    /// queue intact for the next frame.
    /// Current depth of the deferred-upload queue.  Surfaced to
    /// `DebugStats.deferred_uploads` so the overlay shows when
    /// asset ingestion is outpacing the per-frame upload budget.
    pub(crate) fn deferred_uploads_len(&self) -> usize {
        self.deferred_uploads.len()
    }

    /// R1 on-demand vsync source: whether any upload still needs a frame to
    /// make progress. Covers fence-pending completed uploads, budget-deferred
    /// retries, and in-flight jobs submitted to the upload thread but not yet
    /// drained. The render thread treats this as demand so an async upload that
    /// completes while the frame clock is idle still gets one frame to poll its
    /// fence and fire `Image.onload`, rather than waiting forever for a vsync.
    pub(crate) fn has_outstanding_upload_work(&self) -> bool {
        crate::frame_scheduler::outstanding_upload_work(
            self.pending_uploads.len(),
            self.deferred_uploads.len(),
            self.upload_server.as_ref().map_or(0, |s| s.queue_depth()),
        )
    }

    /// Current size of the shared `SkImage` wrapper cache.
    /// Surfaced to `DebugStats.sk_image_wrappers`.
    pub(crate) fn image_wrapper_cache_len(&self) -> usize {
        self.image_registry.wrapper_cache_len()
    }

    #[allow(dead_code)]
    /// Try to pack a small RGBA image into the shared atlas.
    ///
    /// Lazy-initialises [`Self::atlas`] on the first call.  Returns
    /// an `AtlasEntry` on success, or `None` when the image is too
    /// large for the current atlas layout, the allocator is out of
    /// space, or no GL context is current.  A current GL context is
    /// required because the upload issues `glTexSubImage2D` on the
    /// atlas page.
    ///
    /// The typical call site is `load_shared_image` for small
    /// sprites (icons, HUD tiles): when this returns `Some`, the
    /// caller should store an atlas-aware [`StoredImage`] (setting
    /// `atlas_origin` + `atlas_page_size`) and skip the dedicated
    /// per-image GL texture path.  Existing Skia `drawImage` code
    /// then wraps the atlas page and offsets its source rect.
    ///
    /// # Safety
    ///
    /// Caller must have a current GL context on the calling thread.
    /// The atlas module's `upload` is `unsafe` for the same reason;
    /// this wrapper forwards the obligation.
    pub(crate) unsafe fn atlas_upload_small(
        &mut self,
        width: u16,
        height: u16,
        rgba: &[u8],
    ) -> Option<crate::atlas::AtlasEntry> {
        let atlas = self
            .atlas
            .get_or_insert_with(crate::atlas::AtlasManager::new);
        unsafe { atlas.upload(&self.gl, width, height, rgba) }
    }

    pub(crate) fn try_drain_deferred_uploads(&mut self) {
        while self.deferred_uploads.front().is_some() {
            // Snapshot the image data + resp via a take that puts them
            // back if submit fails — avoids cloning the RGBA Arc.
            // front.image holds an `Arc<Vec<u8>>` internally, so
            // `take` + re-insert is still cheap.
            let Some(pending) = self.deferred_uploads.pop_front() else {
                break;
            };
            let bytes = pending.image.rgba.len();
            match self.submit_async_upload(pending.image_id, &pending.image, pending.resp) {
                Ok(()) => {
                    self.deferred_upload_bytes = self.deferred_upload_bytes.saturating_sub(bytes);
                    continue;
                }
                Err(resp) => match self.async_upload_reject_action(bytes) {
                    AsyncUploadRejectAction::SyncFallback => {
                        self.deferred_upload_bytes =
                            self.deferred_upload_bytes.saturating_sub(bytes);
                        let res = self.load_shared_image(pending.image_id, pending.image);
                        let _ = resp.send(res);
                        continue;
                    }
                    AsyncUploadRejectAction::DeferRetry => {
                        // Budget still exhausted; keep the request and its
                        // bytes counted at the head for the next frame.
                        self.deferred_uploads.push_front(DeferredUpload {
                            image_id: pending.image_id,
                            image: pending.image,
                            resp,
                        });
                        break;
                    }
                },
            }
        }
    }

    /// Total RGBA bytes retained by deferred uploads.
    pub(crate) fn deferred_upload_bytes(&self) -> usize {
        self.deferred_upload_bytes
    }

    #[allow(dead_code)]
    /// Number of uploads currently waiting for budget.  Exposed so
    /// the debug overlay / metrics probe the backlog depth.
    pub(crate) fn deferred_upload_depth(&self) -> usize {
        self.deferred_uploads.len()
    }

    /// Probe whether the upload thread is usable (not degraded).
    /// Handlers use this to decide between `defer_upload` (healthy
    /// thread, temporary budget squeeze) and `load_shared_image`
    /// (permanent degradation, sync fallback is the only path).
    pub(crate) fn upload_thread_healthy(&self) -> bool {
        self.upload_thread
            .as_ref()
            .map(|u| !u.is_degraded())
            .unwrap_or(false)
    }

    pub(crate) fn async_upload_reject_action(&self, bytes: usize) -> AsyncUploadRejectAction {
        decide_async_upload_reject_action(
            self.upload_thread_healthy(),
            self.upload_server.as_ref(),
            bytes,
        )
    }

    /// Cancel a pending async upload for the given image_id.
    ///
    /// Sends a `Cancelled` error on the deferred response (if still pending)
    /// and marks the image_id so that `drain_upload_completed` will delete
    /// the orphaned GL texture instead of registering it.
    pub(crate) fn cancel_pending_load(&mut self, image_id: u32) {
        let id64 = image_id as u64;
        if let Some(resp) = self.pending_load_responses.remove(&id64) {
            use shared::error::{EngineError, ErrorCode};
            resp.send(Err(EngineError::from_detail(
                ErrorCode::Cancelled,
                format!("image {} destroyed before upload completed", image_id),
            )));
            // Only mark as cancelled when there was actually a pending
            // async upload — otherwise cancelled_uploads grows unbounded
            // and risks discarding a future upload if the ID is reused.
            self.cancelled_uploads.insert(id64);
        }
    }

    /// Set `GL_PROGRAM_BINARY_RETRIEVABLE_HINT` on a program.
    ///
    /// glow does not wrap `glProgramParameteri`, so we resolve the
    /// function pointer via EGL at call time.  No-op if the shader
    /// cache is disabled or the function is unavailable.
    pub(crate) fn set_program_binary_hint(&self, program: glow::NativeProgram) {
        if self.shader_cache.is_none() {
            return;
        }
        const GL_PROGRAM_BINARY_RETRIEVABLE_HINT: u32 = 0x8257;
        if let Some(fn_ptr) = self.egl.get_proc_address("glProgramParameteri") {
            type GlProgramParameteriFn = unsafe extern "system" fn(u32, u32, i32);
            unsafe {
                let f: GlProgramParameteriFn = std::mem::transmute(fn_ptr);
                f(program.0.get(), GL_PROGRAM_BINARY_RETRIEVABLE_HINT, 1);
            }
        }
    }

    /// `glClientWaitSync` with a full 64-bit timeout.
    ///
    /// glow 0.17's `HasContext::client_wait_sync` signature takes an
    /// `i32` timeout (which it internally casts to `u64`), so using
    /// it would silently clamp any timeout above `i32::MAX` ns
    /// (~2.147 s).  The WebGL 2 spec mandates the full
    /// `GLuint64 timeout` range, so we resolve the raw symbol via
    /// EGL and call it directly.  Returns one of `ALREADY_SIGNALED`,
    /// `TIMEOUT_EXPIRED`, `CONDITION_SATISFIED`, or `WAIT_FAILED`.
    pub(crate) fn client_wait_sync_u64(
        &self,
        sync: *const std::ffi::c_void,
        flags: u32,
        timeout_ns: u64,
    ) -> u32 {
        type GlClientWaitSyncFn =
            unsafe extern "system" fn(*const std::ffi::c_void, u32, u64) -> u32;
        static FN_PTR: std::sync::OnceLock<Option<GlClientWaitSyncFn>> = std::sync::OnceLock::new();
        let resolved = *FN_PTR.get_or_init(|| {
            self.egl
                .get_proc_address("glClientWaitSync")
                .map(|p| unsafe { std::mem::transmute::<_, GlClientWaitSyncFn>(p) })
        });
        match resolved {
            Some(f) => unsafe { f(sync, flags, timeout_ns) },
            None => glow::WAIT_FAILED,
        }
    }

    pub(crate) fn destroy_shared_image(&mut self, image_id: u32) -> EngineResult<()> {
        self.ensure_any_canvas_current()?;
        self.image_registry.destroy_shared_image(&self.gl, image_id)
    }

    /// F-1: Pin an image id so a concurrent `DestroyImage` can't
    /// glDeleteTextures the underlying texture while a queued
    /// `DrawImage` / `DrawImageBatch` command still references
    /// it.  Call once per referenced id when a `FramePacket`
    /// arrives on the render thread; pair with
    /// [`release_in_flight_image`] exactly once per retain at
    /// Present barrier (or on packet abort).
    ///
    /// Cheap: one `HashMap` entry update per id per frame.
    #[inline]
    pub(crate) fn retain_in_flight_image(&mut self, image_id: u32) {
        self.image_registry.store_mut().retain_in_flight(image_id);
    }

    /// F-1: Companion to [`retain_in_flight_image`].  Returns the
    /// `StoredImage` that the caller should `glDeleteTextures`
    /// when the release caused the refcount to hit zero AND a
    /// destroy had been requested while the image was in flight.
    /// Returns `None` otherwise (more references outstanding, or
    /// no destroy was ever requested).
    #[inline]
    #[must_use = "if Some(entry), glDeleteTextures(entry.gl_texture) is required"]
    pub(crate) fn release_in_flight_image(
        &mut self,
        image_id: u32,
    ) -> Option<crate::backend::gl::image_store::StoredImage> {
        self.image_registry.store_mut().release_in_flight(image_id)
    }

    /// F-1: Flush deferred deletions.  Called at the post-frame
    /// Present barrier: walks the `pending_delete` map for every
    /// id whose in-flight refcount has dropped to zero and
    /// deletes its GL texture.  Idempotent and cheap when the
    /// map is empty (the common case).
    pub(crate) fn drain_pending_image_deletions(&mut self) {
        let store = self.image_registry.store_mut();
        for entry in store.take_unreferenced_pending_delete() {
            if let Some(tex) =
                <glow::NativeTexture as NativeTextureFromRawShim>::try_from_raw(entry.gl_texture)
            {
                unsafe { self.gl.delete_texture(tex) };
            }
        }
    }

    /// Access the PBO pool for WebGL texture uploads.
    /// Returns None if no images have been loaded yet (pool not initialized).
    pub(crate) fn pbo_pool_mut(&mut self) -> Option<&mut pbo_upload::PboPool> {
        self.image_registry.ensure_pbo_pool_public(&self.gl);
        self.image_registry.pbo_pool_mut()
    }

    // ==================== GL Object Helpers ====================

    pub(crate) fn check_owner(
        &self,
        owner: Option<CanvasId>,
        canvas: CanvasId,
        what: &str,
    ) -> EngineResult<()> {
        if owner == Some(canvas) {
            return Ok(());
        }
        Err(ee(
            ErrorCode::InvalidOperation,
            format!("{what} belongs to another WebGL context (owner={owner:?}, canvas={canvas:?})"),
        ))
    }
}

impl Drop for CanvasManager {
    fn drop(&mut self) {
        // Destructors must not double-panic if an EGL/GL wrapper encounters a
        // broken driver while the render thread is already unwinding. The
        // EglRuntime field remains an independent final terminate fallback.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = self.destroy_all();
        }));
        if result.is_err() || !self.native_release_confirmed {
            self.quarantine_prepared_native_targets();
        }
    }
}

/// Pure decision for whether the onscreen canvas may bypass its DrawingBuffer
/// (render straight to FBO 0 and skip the DrawingBuffer→window blit at swap).
///
/// Bypass is a WebGL-only optimization. It is safe ONLY when every draw to the
/// onscreen canvas lands in FBO 0 — which holds for WebGL (its default
/// framebuffer is redirected to FBO 0 in bypass mode) but NOT for Skia/Canvas2D
/// (whose onscreen surface always targets the DrawingBuffer FBO). It also
/// requires no default-FBO readback (which needs preserved content) and exactly
/// one canvas.
///
/// Crucially, bypass also requires the DrawingBuffer to be exactly the surface
/// size (`onscreen_db_matches_surface`). The DrawingBuffer→window blit *scales*
/// (`glBlitFramebuffer` src=db → dst=surface), so a game that sizes its canvas
/// below the surface (e.g. Phaser `Scale.NONE` at a fixed 960x640 on a 2340x1080
/// screen) is upscaled to fill the window by the blit path. Bypass has no such
/// scaling — WebGL renders straight into FBO 0 at the game's viewport, landing a
/// 960x640 image in the bottom-left corner of the surface with the rest black.
/// So whenever the DrawingBuffer differs from the surface, bypass must be off.
/// (`onscreen_db_matches_surface` is false when there is no DrawingBuffer at all,
/// which also correctly disables bypass.)
///
/// Extracted as a pure fn so the conditions are unit-testable without a live GL
/// context.
/// Whether the onscreen canvas may render straight to the window and skip the
/// per-frame DrawingBuffer→surface blit.
///
/// **Why a single canvas is required — and the reason recorded here was wrong.**
/// The argument used to be that bypass makes `get_drawing_buffer_fbo` return `None`,
/// so the onscreen canvas's default framebuffer is *real* FBO 0; that real FBO 0 is
/// whichever EGL draw surface is current; and that since offscreen canvases are
/// [`SurfaceKind::Pbuffer`] with surfaces of their own, "FBO 0" stops naming the
/// window as soon as a second canvas exists.
///
/// The middle step does not hold, because FBO 0 follows the current *surface of the
/// current context* and every canvas here owns a context of its own. `create_onscreen`
/// calls `eglCreateContext` and `register_offscreen` calls `create_pbuffer_context`,
/// each sharing only the resource context's *objects*, and
/// [`Self::make_current_needed`] takes the context and the surface from one
/// [`EglContextHandle`]. So a pbuffer is only ever current *with its own canvas's
/// context*, in which the onscreen canvas cannot be drawn at all — and inside the
/// onscreen context real FBO 0 is the window however many canvases exist.
///
/// What both modes really require is the same: a command that draws to a canvas runs
/// with that canvas current. `handle_command` establishes that per command. Bypass
/// does not weaken it; it changes what going wrong looks like, from a framebuffer name
/// that means nothing in the current context to a silently wrong surface.
///
/// So this condition is not a correctness condition. It makes a *shared* precondition
/// vacuous by leaving no other context to be current, and the cost of that is paid by
/// every real bundle. Measured on the Linux host: two live canvases, drawn to in both
/// orders, with the frame deliberately ending on the pbuffer, present the onscreen
/// clear and nothing else — `scripts/fixtures/bypass-multi-probe`, 240 frames, one
/// distinct colour. It is still not widened here, because bypass has never run in
/// steady state on any device (this condition is why), and Android, Windows and
/// HarmonyOS presentation is unmeasured — see ledger 0.57 and 0.65.
///
/// **The consequence is measured, not hypothetical.** A single offscreen canvas
/// anywhere in the scene disables bypass for the whole run, and that is the
/// ordinary case rather than the exotic one: the bunnymark bundle is pure WebGL on
/// its onscreen canvas, never reads it back, and matches the surface exactly — and
/// it still presents its entire 60 fps steady state through the blit, because
/// `canvas_count` is 2. At its 720×1280 that is about 3.7 MB read and 3.7 MB
/// written per frame, some 440 MB/s of bandwidth, for a copy whose only purpose is
/// to keep FBO 0 unambiguous.
///
/// So Section 7.3's "no redundant presentation copy" is **not** met for ordinary
/// content, and the reason is this condition rather than a missing optimisation.
fn can_bypass_drawing_buffer(
    canvas_count: usize,
    needs_default_fbo_readback: bool,
    onscreen_has_2d_context: bool,
    onscreen_db_matches_surface: bool,
) -> bool {
    canvas_count == 1
        && !needs_default_fbo_readback
        && !onscreen_has_2d_context
        && onscreen_db_matches_surface
}

/// The framebuffer name that *is* a canvas's WebGL default framebuffer: the
/// DrawingBuffer's FBO normally, and real FBO 0 (`None`) under bypass — because
/// bypass is *defined* as "the WebGL default framebuffer is the window's".
///
/// **Single-sourced because two sites derived it independently and dropped the
/// bypass term.** `make_current_needed` and the surface-recreate DrawingBuffer
/// reuse each bound `drawing_buffer.map(|db| db.fbo)` directly, so under bypass
/// they re-pointed the default framebuffer at a buffer that bypass has just
/// stopped blitting. Nothing then presents the frame. The post-swap restore
/// carried the bypass term in a bespoke `if !bypass` and was the only site that
/// had it, which is why the disagreement read as a comment rather than a bug.
fn default_framebuffer_of(
    bypass_drawing_buffer: bool,
    drawing_buffer_fbo: Option<glow::NativeFramebuffer>,
) -> Option<glow::NativeFramebuffer> {
    if bypass_drawing_buffer {
        None
    } else {
        drawing_buffer_fbo
    }
}

/// What one present attempt did to the frame's damage bookkeeping.
#[derive(Debug, PartialEq, Eq)]
enum PresentCommit {
    /// The swap failed. This frame's damage stays in the accumulator so the
    /// retry repairs everything the lost frame still owed, and history is
    /// untouched because nothing reached the surface.
    Retry,
    /// Swap and blit both complete: history records the frame's own damage, and
    /// a later present with buffer age 2 can repair from it.
    Presented(ResolvedDamage),
    /// The swap advanced EGL's buffer sequence, but at least one repair write
    /// failed, so the back buffer is only partly defined. History is poisoned so
    /// the next present repairs everything — a same-frame full retry is illegal
    /// once the sequence has advanced.
    PresentedPartial,
}

/// Commit one present attempt's damage bookkeeping.
///
/// **Split out of `swap_buffers_no_restore` because the ordering is the property
/// and the EGL calls around it are not.** Taking the swap outcome as data
/// instead of leaving it as an early `?` return is what makes both branches
/// reachable without a window surface — the same move `run_frame_phases` used for
/// the frame phases. Until now the two properties were asserted by searching
/// this function's *source text* for `.swap_buffers(`, `blit_succeeded` and
/// `self.damage.reset()` and checking the offsets were in order, which cannot
/// fail on any behavioural change that leaves the text intact, on the one path
/// where being wrong means a frame of stale pixels.
fn commit_present_outcome(
    damage: &mut crate::damage_effect::FrameDamageAccumulator,
    history: &mut crate::present_damage::PresentDamageHistory,
    swap_succeeded: bool,
    blit_succeeded: bool,
    plan: &crate::present_damage::PresentDamagePlan,
) -> PresentCommit {
    if !swap_succeeded {
        return PresentCommit::Retry;
    }
    // Reset only at a frame boundary that actually happened.
    damage.reset();
    if blit_succeeded {
        // `current`, never `repair`. `repair` is age-expanded — it covers what
        // this frame had to rewrite to make an N-frames-old back buffer whole,
        // which is a superset of what this frame's content changed. Recording it
        // would make every later repair grow off the previous one's expansion.
        history.push(plan.current.clone());
        PresentCommit::Presented(CanvasManager::region_to_resolved(&plan.current))
    } else {
        history.clear();
        history.push(crate::present_damage::DamageRegion::FullSurface);
        damage.add(crate::damage_effect::DamageEffect::FullSurface);
        PresentCommit::PresentedPartial
    }
}

/// Apply a pending default-FBO mapping only in its owning context. What is
/// installed and what is wanted are tracked separately so events while away
/// cannot lose the transition, and the installed value is the framebuffer
/// itself rather than the mode that chose it: a DrawingBuffer rebuilt under an
/// unchanged mode is a new default framebuffer, and a mode flag cannot say so.
/// Native bindings are authoritative here: a client ID is not a GL name, and
/// an invalidated dedup shadow does not tell us which target is default.
pub(crate) fn apply_default_framebuffer(
    gl: &glow::Context,
    context_is_current: bool,
    applied: &mut Option<glow::NativeFramebuffer>,
    desired: Option<glow::NativeFramebuffer>,
) {
    if !context_is_current || *applied == desired {
        return;
    }
    // A nonzero DrawingBuffer has already passed the split-binding/blit
    // capability probe in create(). No GLES3 query runs without one.
    unsafe {
        let read = gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING);
        let draw = gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING);
        let target = match (read == *applied, draw == *applied) {
            (true, true) => Some(glow::FRAMEBUFFER),
            (true, false) => Some(glow::READ_FRAMEBUFFER),
            (false, true) => Some(glow::DRAW_FRAMEBUFFER),
            (false, false) => None,
        };
        if let Some(target) = target {
            gl.bind_framebuffer(target, desired);
            crate::render_diagnostics::bump_state_change();
        }
    }
    *applied = desired;
}

/// readPixels consumes READ on GLES3, independently of the draw target. Known
/// client bindings avoid a driver query; unknown state must use native truth.
fn reads_from_default_framebuffer(
    gl: &glow::Context,
    split_bindings_probed: bool,
    state: Option<&CanvasGLState>,
    default: Option<glow::NativeFramebuffer>,
) -> bool {
    // The requested EGL version may be lower than the returned GL version.
    // DrawingBuffer creation can also establish split support on an extension
    // stack; use the same capability contract as the mapping and blit paths.
    let (target, binding) = if gl.version().major >= 3 || split_bindings_probed {
        (glow::READ_FRAMEBUFFER, glow::READ_FRAMEBUFFER_BINDING)
    } else {
        (glow::FRAMEBUFFER, glow::FRAMEBUFFER_BINDING)
    };
    if let Some(bound) = state.and_then(|s| s.bound_framebuffer.get(target)) {
        return bound.is_none();
    }
    unsafe { gl.get_parameter_framebuffer(binding) == default }
}

/// One offscreen canvas that must be re-created after the EGL share group is
/// rebuilt, captured before the dead group is torn down.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct OffscreenRestore {
    pub id: CanvasId,
    pub width: u32,
    pub height: u32,
    /// The 2D drawing state of the context this canvas had, if it had one.
    ///
    /// JS holds the context object forever and never re-issues
    /// `getContext('2d')`, so recovery must re-initialise it rather than wait
    /// for a call that will not come -- and it must restore the state, because
    /// the JS setters de-duplicate against a shadow that a GPU reset does not
    /// clear. A context rebuilt at spec defaults means every value the content
    /// set once and never re-sent is silently wrong from then on.
    pub state_2d: Option<Canvas2DState>,
}

/// The JS-visible canvas state a share-group teardown destroyed, which the
/// rebuild must re-create before recovery may report success.
///
/// JS owns canvas identity: it allocates offscreen ids itself and posts
/// `RegisterOffscreen` exactly once, when the canvas is created
/// (`op_create_offscreen_canvas` is fire-and-forget). Nothing re-registers a
/// canvas later, and JS keeps its `Canvas`/`CanvasRenderingContext2D` objects
/// across a context loss, so any canvas dropped by recovery strands a live JS
/// handle: every later op on it fails `NotFound` and the game silently stops
/// drawing. Identity therefore outlives the GPU state, and the teardown hands
/// this plan to the rebuild rather than discarding it.
#[derive(Debug, Clone, PartialEq, Default)]
#[must_use = "a torn-down share group must be restored or JS-visible canvases silently vanish"]
pub(super) struct ShareGroupRestorePlan {
    /// The onscreen canvas's 2D drawing state, if it had a context. The canvas
    /// itself is rebuilt by `create_onscreen` (it needs the window target), but
    /// its 2D context is not.
    pub onscreen_2d: Option<Canvas2DState>,
    /// Offscreen canvases, ordered by id.
    pub offscreen: Vec<OffscreenRestore>,
}

/// Build the restore plan from the live canvas registry, which is the single
/// source of truth for canvas identity and current size (`resize_canvas` keeps
/// `physical_width`/`physical_height` in step with what JS asked for).
///
/// Pure so the policy — which canvases carry over, and what has to be rebuilt
/// for each — is testable without an EGL display.
fn plan_share_group_restore(
    canvases: impl Iterator<Item = (CanvasId, bool, u32, u32)>,
    state_2d: impl Fn(CanvasId) -> Option<Canvas2DState>,
) -> ShareGroupRestorePlan {
    let onscreen_id = CanvasId::from(1u32);
    let mut offscreen: Vec<OffscreenRestore> = canvases
        .filter(|(id, is_pbuffer, _, _)| *is_pbuffer && *id != onscreen_id)
        .map(|(id, _, width, height)| OffscreenRestore {
            id,
            width,
            height,
            state_2d: state_2d(id),
        })
        .collect();
    // The caller iterates a HashMap: sort so recovery order — and the failure
    // it reports when one canvas cannot be rebuilt — is reproducible.
    offscreen.sort_unstable_by_key(|spec| spec.id);
    ShareGroupRestorePlan {
        onscreen_2d: state_2d(onscreen_id),
        offscreen,
    }
}

/// Source guards for the context-recovery contract. `CanvasManager` cannot be
/// constructed without an EGL display, so the wiring that pairs the teardown
/// with its restore is asserted against the source itself — the same technique
/// `present_damage` and `surface_binding` use for their EGL-bound invariants.
#[cfg(test)]
mod recovery_source_guards {
    const MGR: &str = include_str!("mod.rs");

    fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .expect("function signature must exist");
        let source = &source[start..];
        let open = source.find('{').expect("function body must open");
        let mut depth = 0usize;
        for (offset, ch) in source[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &source[open + 1..open + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("function body must close");
    }

    /// A fresh native target carries no frame-rate request, so the install has to
    /// re-assert it. Making that the *installer's* job rather than each caller's
    /// is what keeps a new install path -- first attach, resize, surface
    /// recreate, context recovery -- from silently leaving the display in
    /// whichever mode it was in.
    ///
    /// Structural because installing a target needs a real EGL display, which no
    /// host test has: the same reason `startup_ordering` checks this manager's
    /// construction order by reading its source.
    #[test]
    fn every_onscreen_install_re_asserts_the_frame_rate_request() {
        // The scan stops at this module, because `include_str!` includes the
        // assertions below and they would match themselves.
        let code = MGR
            .split_once("mod recovery_source_guards")
            .expect("this module bounds the production half of the file")
            .0;
        assert!(
            !code.contains("installed_surface = Some("),
            "an install that assigns the field directly bypasses the request"
        );
        let commits: Vec<_> = code
            .match_indices("self.installed_surface.insert(")
            .collect();
        assert_eq!(
            commits.len(),
            1,
            "one commit point, or checking the first one proves nothing about the rest"
        );
        let after_commit: String = code[commits[0].0..].lines().take(3).collect();
        assert!(
            after_commit.contains("request_frame_rate(self.frame_rate_request)"),
            "the commit point must ask the display for the stored rate, got: {after_commit}"
        );
    }

    #[test]
    fn teardown_captures_the_restore_plan_before_dropping_canvases() {
        let body = function_body(MGR, "fn tear_down_share_group");
        let plan = body
            .find("plan_share_group_restore(")
            .expect("teardown must capture what the rebuild owes the game");
        let drop_canvases = body
            .find("std::mem::take(&mut self.canvases)")
            .expect("teardown must drop the canvas registry");
        assert!(
            plan < drop_canvases,
            "the restore plan must be captured while the canvas registry is still populated"
        );
    }

    #[test]
    fn recovery_restores_the_canvases_the_teardown_destroyed() {
        let body = function_body(MGR, "pub(crate) fn try_recover_context");
        let teardown = body
            .find("self.tear_down_share_group()")
            .expect("recovery must tear down the dead share group");
        let onscreen = body
            .find("self.create_onscreen(")
            .expect("recovery must rebuild the onscreen canvas");
        let restore = body
            .find("self.restore_share_group(")
            .expect("recovery must restore the canvases the teardown destroyed");
        assert!(
            teardown < onscreen && onscreen < restore,
            "order must be teardown -> onscreen rebuild -> restore of the remaining canvases"
        );
        let probe = body
            .find("self.probe_context_usable(")
            .expect("recovery must probe the rebuilt context");
        assert!(
            restore < probe,
            "the probe must run against a fully restored share group"
        );
    }

    /// **A recovery attempt that fails part-way must not lose the canvases the
    /// teardown recorded**, because JS registers a canvas exactly once and
    /// anything dropped here is stranded for the life of the process.
    ///
    /// `tear_down_share_group` returns a `#[must_use]`
    /// `ShareGroupRestorePlan` to make that obligation explicit, and it did not
    /// hold: `#[must_use]` catches ignoring a value, not dropping one on an early
    /// return, and the plan was a local with two `?` between its creation and its
    /// use (`create_pbuffer_context`, `bind_resource`) plus more inside
    /// `restore_share_group`. Any of those firing lost every canvas the plan
    /// carried, and the next attempt would tear down an already-empty registry
    /// and record nothing.
    ///
    /// So the plan is parked on `self.pending_share_group_restore` before the
    /// first fallible step and cleared only when a restore fully succeeds. This
    /// asserts that shape, since reaching the path needs EGL and a GPU reset.
    #[test]
    fn a_failed_recovery_hands_its_restore_debt_to_the_next_attempt() {
        let body = function_body(MGR, "pub(crate) fn try_recover_context");

        // The plan must be parked, not bound to a local that an early return
        // would drop.
        let park = body
            .find("self.pending_share_group_restore = Some(plan)")
            .expect(
                "the restore plan must be parked on `self` before the fallible \
                 rebuild, or an early return strands every canvas it recorded",
            );
        let teardown = body
            .find("self.tear_down_share_group()")
            .expect("recovery must tear down the dead share group");
        assert!(
            teardown < park,
            "the plan must be parked immediately after the teardown that produced it"
        );

        // Everything that can fail must come after the park.
        for fallible in ["create_pbuffer_context(", "self.bind_resource()?"] {
            let at = body
                .find(fallible)
                .unwrap_or_else(|| panic!("{fallible} is no longer in the recovery path"));
            assert!(
                park < at,
                "{fallible} runs before the restore plan is parked, so its `?` \
                 would drop the plan"
            );
        }

        // A retry must resume the parked plan rather than tearing down again —
        // the registry is empty by then, so a second teardown records nothing.
        assert!(
            body.contains("if self.pending_share_group_restore.is_none()"),
            "recovery must reuse a parked plan instead of unconditionally tearing \
             down; a second teardown of an empty registry yields an empty plan"
        );

        // And a failed restore must give the plan back, because
        // `restore_share_group` uses `?` per canvas and leaves the tail owed.
        let restore_at = body
            .find("self.restore_share_group(")
            .expect("recovery must restore the recorded canvases");
        let give_back = body[restore_at..]
            .find("self.pending_share_group_restore = Some(plan)")
            .expect(
                "a failed `restore_share_group` must hand the plan back, or the \
                 canvases it had not reached yet are stranded",
            );
        assert!(
            give_back > 0,
            "the plan must be returned after the restore call, not before"
        );
    }

    /// Every 2D context recovery rebuilds must also get its drawing state back.
    ///
    /// The JS setters de-duplicate against a shadow (`if (this._fillStyle ===
    /// value) return;`) that a GPU reset does not clear, and Canvas2D has no
    /// context-loss event for content to react to -- browsers restore 2D
    /// contexts transparently, so no engine listens for one. A context rebuilt
    /// at spec defaults is therefore permanent: every value the content set
    /// once and never re-sent is wrong from then on, fills paint opaque black,
    /// and nothing anywhere reports an error.
    ///
    /// The onscreen path learned this as #48. Recovery is the same invariant on
    /// a different trigger, and it covers the offscreen canvases too, which #48
    /// never had to think about.
    #[test]
    fn recovery_gives_every_rebuilt_2d_context_its_drawing_state_back() {
        let body = function_body(MGR, "fn restore_share_group");
        let inits = body.matches("init_skia_for_canvas").count();
        let adopts = body.matches("adopt_drawing_state").count();
        assert!(
            inits > 0,
            "recovery must re-create the 2D contexts the teardown dropped"
        );
        assert_eq!(
            adopts, inits,
            "every re-created 2D context must adopt the state it had ({inits} rebuilt, \
             {adopts} restored) -- a context rebuilt at spec defaults desynchronises \
             from the content permanently"
        );

        // The plan has to carry the state, or there would be nothing to adopt.
        let plan = function_body(MGR, "fn plan_share_group_restore");
        assert!(
            plan.contains("state_2d"),
            "the restore plan must carry the drawing state, not merely a flag"
        );
    }

    /// No path may drop a 2D context and re-create it without carrying the
    /// drawing state -- whatever the trigger is.
    ///
    /// This invariant has now been broken three times, each on a different
    /// trigger and each silent: surface recreate (#48), a background round trip
    /// where the teardown and the install are separate events, and GPU
    /// context-loss recovery. The per-site guards each covered the site they
    /// were written for; this one covers the shape.
    ///
    /// `rebuild_2d_context_preserving_state` is the single sequence point, so
    /// the check is that nothing else pairs a drop with a re-create.
    #[test]
    fn no_path_re_creates_a_2d_context_without_carrying_its_state() {
        let helper = function_body(MGR, "fn rebuild_2d_context_preserving_state");
        for needle in ["drawing_state()", "drop_2d_context", "init_skia_for_canvas"] {
            assert!(
                helper.contains(needle),
                "the sequence point must still {needle}"
            );
        }
        assert!(
            helper.find("drawing_state()") < helper.find("drop_2d_context"),
            "the state must be captured before the context it belongs to is dropped"
        );
        assert!(
            helper.find("init_skia_for_canvas") < helper.find("adopt_drawing_state"),
            "the state must be adopted by the context that replaces the old one"
        );

        // Any other function that drops a 2D context and re-creates it in the
        // same body has bypassed the sequence point. `create_onscreen` is
        // excluded: it defers the re-create to the obligation it records, which
        // `onscreen_surface_recreate_carries_the_2d_drawing_state_across` pins,
        // and `destroy_onscreen_internal` only ever drops.
        let allowed = [
            "fn rebuild_2d_context_preserving_state",
            "pub(crate) fn create_onscreen",
        ];
        // Only the production half. `MGR` is this file, so a scan of the whole
        // of it matches the test module -- including this test's own allow-list
        // literals, which name both sides of the pair it looks for.
        let production = MGR
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .expect("mod.rs must have a test module boundary to cut at");
        let mut offenders = Vec::new();
        for (index, _) in production.match_indices("fn ") {
            let tail = &production[index..];
            let Some(name_end) = tail.find(['(', '<']) else {
                continue;
            };
            let signature = &tail[..name_end];
            if allowed.iter().any(|a| a.ends_with(signature)) {
                continue;
            }
            let body_end = tail.find("\n    }").unwrap_or(tail.len());
            let body = &tail[..body_end];
            if body.contains("drop_2d_context") && body.contains("init_skia_for_canvas") {
                offenders.push(signature.trim().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "these drop and re-create a 2D context without going through \
             `rebuild_2d_context_preserving_state`, so the content keeps drawing \
             with state the render side no longer has: {offenders:?}"
        );
    }

    /// Every `glBlitFramebuffer` in the crate must neutralise the scissor test.
    ///
    /// A blit writes to the DRAW framebuffer *through* the scissor, and both
    /// Skia and game code leave their own boxes enabled. A blit that inherits
    /// one is silently clipped -- and a fully clipped blit is not a GL error,
    /// so nothing anywhere reports it. It cost this crate two separate bugs:
    /// the present blit landing the game in a corner of the window, and
    /// `getImageData` returning a fully transparent buffer whenever the
    /// preceding fill left a box with a non-zero origin.
    ///
    /// Asserted over every call site rather than the two that were fixed,
    /// because the next blit added here will be written by someone who has not
    /// read either bug.
    #[test]
    fn every_blit_neutralises_the_scissor_test() {
        let sources = [
            ("canvas/manager/mod.rs", MGR),
            (
                "canvas/manager/drawing_buffer.rs",
                include_str!("drawing_buffer.rs"),
            ),
            (
                "renderergl/handler.rs",
                include_str!("../../renderergl/handler.rs"),
            ),
        ];
        let mut offenders = Vec::new();
        for (name, source) in sources {
            let production = source.split_once("#[cfg(test)]").map_or(source, |(a, _)| a);
            let blits = production.matches("blit_framebuffer(").count();
            if blits == 0 {
                continue;
            }
            // `is_enabled(SCISSOR_TEST)` + `disable(SCISSOR_TEST)` is the
            // established idiom; a file that blits without both has a call site
            // running under whatever clip the caller happened to leave.
            let reads = production.matches("is_enabled(glow::SCISSOR_TEST)").count();
            let disables = production.matches("disable(glow::SCISSOR_TEST)").count();
            if reads == 0 || disables == 0 {
                offenders.push(format!(
                    "{name}: {blits} blit(s), {reads} scissor read(s), {disables} disable(s)"
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "these blit without neutralising the scissor test, so the copy is \
             silently clipped by whatever box the caller left enabled -- with no \
             GL error to notice: {offenders:?}"
        );
    }

    /// A half-restored share group must never report a recovered context.
    ///
    /// Asserts the property rather than one spelling of it. The first version
    /// searched for the literal `self.restore_share_group(&restore, onscreen_id)?`
    /// and went red when the call grew a `match` so a failed restore could hand
    /// its plan back — a change that strengthened the very guarantee this test
    /// exists for. What matters is that a failure leaves the block before the
    /// flag is cleared, however the failure is spelled.
    #[test]
    fn offscreen_restore_failure_keeps_the_context_lost() {
        let body = function_body(MGR, "pub(crate) fn try_recover_context");
        let restore = body
            .find("self.restore_share_group(")
            .expect("recovery must restore the canvases the teardown destroyed");
        let clears_flag = body
            .find("self.context_lost = false;")
            .expect("successful recovery must clear the lost flag");
        assert!(
            restore < clears_flag,
            "a half-restored share group must never report a recovered context"
        );

        // The failure has to leave the fallible block. Either spelling does:
        // a `?` on the call, or an explicit early `return Err`.
        let after = &body[restore..];
        let propagates = after
            .split("self.context_lost")
            .next()
            .is_some_and(|between| between.contains("return Err(e)") || between.contains(")?;"));
        assert!(
            propagates,
            "a failed restore no longer leaves the all-or-nothing block, so a \
             half-restored share group could reach `context_lost = false`"
        );
    }

    #[test]
    fn canvas_snapshot_creation_checks_an_aggregate_byte_budget() {
        let body = function_body(MGR, "pub(crate) fn snapshot_canvas2d_region_with_id");
        assert!(
            body.contains("MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES"),
            "snapshot creation must enforce an aggregate GPU byte budget"
        );
        assert!(
            body.contains("checked_add"),
            "aggregate snapshot bytes must use checked arithmetic"
        );
    }

    #[test]
    fn canvas_snapshot_drain_defers_when_no_context_is_current() {
        let body = function_body(MGR, "pub(crate) fn drain_canvas2d_snapshots");
        assert!(
            body.contains("ensure_any_canvas_current().is_err()"),
            "drain must detect that no context is available"
        );
        assert!(
            !body.contains("canvas2d_snapshots.clear()"),
            "drain must retain texture handles until a context can delete them"
        );
        let teardown = function_body(MGR, "pub(crate) fn destroy_all");
        let shutdown = teardown
            .find("let display_terminated = self.egl.shutdown()")
            .expect("teardown must reach EGL shutdown");
        let clear = teardown
            .find("self.canvas2d_snapshots.clear()")
            .expect("teardown must clear retained handles only after EGL shutdown");
        assert!(
            shutdown < clear,
            "EGL teardown must precede dropping handles when no context was current"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::damage_effect::{DamageEffect, FrameDamageAccumulator};

    /// **At the cap a snapshot is refused, never traded for an older one.**
    ///
    /// The field doc used to promise "oldest evicted first", which the code has
    /// never done and must not: every live entry is waiting for a
    /// `TexImage2DFromSnapshot` that JS already committed to issuing, so
    /// evicting one to admit another turns a bounded pool into a missing
    /// texture. Refusing returns `0`, which JS reads as "fall back to the legacy
    /// CPU readback" — slower for one label, correct for all of them. A reader
    /// who trusted the old doc would have "fixed" the code toward the eviction
    /// it described.
    ///
    /// The caps' arithmetic is not checked here: it is all compile-time
    /// constants, so it lives next to them as `const _: () = assert!(…)` and a
    /// mis-relation fails the build rather than the suite.
    ///
    /// What this pins instead is the refusal itself, at the level a host can
    /// reach it. `snapshot_canvas2d_region_with_id` needs an EGL context, so
    /// the assertion is on the decision function's shape rather than on a live
    /// capture — stated plainly rather than dressed up as an end-to-end test.
    #[test]
    fn a_full_snapshot_pool_refuses_rather_than_evicting() {
        // The two rejection sites, read out of the source. Crude, and chosen
        // deliberately over a mock: the alternative is a fake GL context whose
        // agreement with a driver is its own open question, and what is in doubt
        // here is not GL behaviour but whether this function still *refuses*.
        const SRC: &str = include_str!("mod.rs");
        let body_start = SRC
            .find("pub(crate) fn snapshot_canvas2d_region_with_id")
            .expect("the capture entry point");
        let body = &SRC[body_start..body_start + 4000];

        assert!(
            body.contains("canvas2d_snapshot_order.len() >= Self::MAX_LIVE_CANVAS2D_SNAPSHOTS"),
            "the count cap is no longer consulted in the capture path"
        );
        assert!(
            body.contains("next_snapshot_bytes > Self::MAX_LIVE_CANVAS2D_SNAPSHOT_BYTES"),
            "the byte cap is no longer consulted in the capture path"
        );
        for evicting in ["pop_front", "remove(&evict", "evict"] {
            assert!(
                !body.contains(evicting),
                "the capture path now contains {evicting:?}. If the pool has \
                 started evicting to make room, every evicted entry is a \
                 `TexImage2DFromSnapshot` that will find nothing — see this \
                 test's doc for why refusing is the only safe answer at the cap"
            );
        }
    }

    // ── Upload drain staging ─────────────────────────────────────────────

    /// One frame of the drain: some completions arrive, some are deferred to
    /// the next frame. Modelled the way `drain_upload_completed` runs it, with
    /// the fence verdict standing in for the real `glClientWaitSync`.
    fn one_drain_frame(
        scratch: &mut Vec<u32>,
        pending: &mut Vec<u32>,
        arriving: &[u32],
        ready: impl Fn(u32) -> bool,
    ) -> Vec<u32> {
        let mut staged =
            stage_upload_drain(scratch, pending, |sink| sink.extend_from_slice(arriving));
        let mut registered = Vec::new();
        for item in staged.drain(..) {
            if ready(item) {
                registered.push(item);
            } else {
                pending.push(item);
            }
        }
        *scratch = staged;
        registered
    }

    /// Section 7.3 on the once-per-frame upload drain.
    ///
    /// **This is the assertion the previous shape could not have passed.** It
    /// consumed the staging vector by value — freeing its buffer — after
    /// `mem::take` had already reduced `pending_uploads` to capacity zero, so
    /// every frame with an upload still in flight bought a fresh buffer for the
    /// re-defer. A level load runs that for as many frames as it takes.
    ///
    /// The fixture keeps one item permanently unready, which is what makes the
    /// re-defer happen on every measured iteration.
    #[test]
    fn a_steady_state_upload_drain_never_reaches_the_heap() {
        let mut scratch = Vec::new();
        let mut pending = Vec::new();
        // Item 0 never becomes ready; items 1..4 always do.
        let arriving = [1u32, 2, 3, 4];
        let ready = |item: u32| item != 0;
        // Seed the deferred item and let both buffers reach their steady size.
        pending.push(0);
        for _ in 0..4 {
            one_drain_frame(&mut scratch, &mut pending, &arriving, ready);
        }

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "canvas manager: per-frame upload completion drain",
                warmup: 4,
                measured: 64,
            },
            |_| {
                let mut staged = stage_upload_drain(&mut scratch, &mut pending, |sink| {
                    sink.extend_from_slice(&arriving)
                });
                let mut n = 0u32;
                for item in staged.drain(..) {
                    if ready(item) {
                        n += 1;
                    } else {
                        pending.push(item);
                    }
                }
                scratch = staged;
                n
            },
        );
        assert_eq!(pending, vec![0], "the unready item must stay deferred");
    }

    /// Order is load-bearing: an upload deferred from an earlier frame is the
    /// one most likely to be ready now, so it has to stay ahead of the
    /// completions that only just arrived.
    #[test]
    fn staging_keeps_deferred_uploads_ahead_of_newly_arrived_ones() {
        let mut scratch = Vec::new();
        let mut pending = vec![10u32, 11];
        let staged = stage_upload_drain(&mut scratch, &mut pending, |sink| {
            sink.extend_from_slice(&[20, 21])
        });
        assert_eq!(staged, vec![10, 11, 20, 21]);
        assert!(
            pending.is_empty(),
            "the deferred list must be emptied, or its items are staged twice"
        );
    }

    /// The scratch comes back reusable, not carrying last frame's contents into
    /// this one — which would register the same upload twice.
    #[test]
    fn staging_starts_from_an_empty_buffer_even_if_the_scratch_was_dirty() {
        let mut scratch = vec![99u32, 98];
        let mut pending = vec![1u32];
        let staged = stage_upload_drain(&mut scratch, &mut pending, |sink| sink.push(2));
        assert_eq!(
            staged,
            vec![1, 2],
            "a dirty scratch leaked last frame's uploads into this frame"
        );
    }

    #[test]
    fn egl_swap_failures_distinguish_context_surface_and_retryable_errors() {
        assert_eq!(
            classify_egl_swap_failure(egl::Error::ContextLost),
            EglSwapFailureClass::ContextLost
        );
        for error in [
            egl::Error::BadCurrentSurface,
            egl::Error::BadSurface,
            egl::Error::BadNativeWindow,
            egl::Error::BadDisplay,
        ] {
            assert_eq!(
                classify_egl_swap_failure(error),
                EglSwapFailureClass::SurfaceLost
            );
        }
        assert_eq!(
            classify_egl_swap_failure(egl::Error::BadAlloc),
            EglSwapFailureClass::Other
        );
    }

    // ---- Share-group restore planning (context-loss recovery) ----

    /// `(id, is_pbuffer, width, height)` as the teardown reads them out of the
    /// live canvas registry.
    const ONSCREEN: (CanvasId, bool, u32, u32) = (1, false, 720, 1280);

    /// A drawing state distinguishable from the spec defaults a rebuilt
    /// context starts at, so a test cannot pass by accident.
    fn marked_state() -> Canvas2DState {
        let mut state = Canvas2DState::default();
        state.global_alpha = 0.25;
        state
    }

    #[test]
    fn restore_plan_carries_offscreen_canvases_at_their_current_size() {
        // Regression: recovery used to rebuild only the onscreen canvas, so a
        // game holding an offscreen canvas id (Pixi allocates two at startup)
        // hit `NotFound` on every later op and stopped rendering.
        let plan =
            plan_share_group_restore([ONSCREEN, (16777217, true, 256, 128)].into_iter(), |_| None);
        assert_eq!(
            plan.offscreen,
            vec![OffscreenRestore {
                id: 16777217,
                width: 256,
                height: 128,
                state_2d: None,
            }]
        );
    }

    #[test]
    fn restore_plan_excludes_the_onscreen_canvas() {
        // `create_onscreen` owns the onscreen canvas because it needs the
        // window target; re-registering it as a pbuffer would fight that.
        let plan = plan_share_group_restore([ONSCREEN].into_iter(), |_| None);
        assert!(plan.offscreen.is_empty());
        assert!(plan.onscreen_2d.is_none());
    }

    #[test]
    fn restore_plan_records_which_canvases_had_2d_contexts() {
        // JS holds its 2D context objects across the loss and never re-issues
        // `getContext('2d')`, so recovery must re-init Skia for both the
        // onscreen canvas and any offscreen one that had a context.
        let plan = plan_share_group_restore(
            [ONSCREEN, (16777216, true, 1, 1), (16777217, true, 1, 1)].into_iter(),
            |id| (id == 1 || id == 16777217).then(marked_state),
        );
        assert!(plan.onscreen_2d.is_some());
        assert_eq!(
            plan.offscreen
                .iter()
                .map(|spec| (spec.id, spec.state_2d.is_some()))
                .collect::<Vec<_>>(),
            vec![(16777216, false), (16777217, true)]
        );
    }

    /// The plan carries the drawing *state*, not merely the fact that there was
    /// one.
    ///
    /// A context rebuilt at spec defaults desynchronises from the content for
    /// good: the JS setters de-duplicate against a shadow (`if (this._fillStyle
    /// === value) return;`) that no GPU reset clears, so every value set once
    /// and never re-sent is silently wrong from then on -- fills paint opaque
    /// black, `globalAlpha` snaps back to 1. Recording a bool would rebuild the
    /// context and still leave the content drawing with someone else's state.
    #[test]
    fn restore_plan_carries_the_drawing_state_not_just_a_flag() {
        let plan = plan_share_group_restore([ONSCREEN, (16777217, true, 1, 1)].into_iter(), |_| {
            Some(marked_state())
        });
        assert_eq!(
            plan.onscreen_2d.as_ref(),
            Some(&marked_state()),
            "the onscreen state must survive the teardown"
        );
        assert_eq!(
            plan.offscreen[0].state_2d.as_ref(),
            Some(&marked_state()),
            "offscreen canvases keep their state too -- they have the same JS shadow"
        );
    }

    #[test]
    fn restore_plan_is_ordered_by_id() {
        // The registry is a HashMap: without sorting, recovery order (and the
        // canvas blamed when a rebuild fails) would vary run to run.
        let plan = plan_share_group_restore(
            [
                (16777219, true, 1, 1),
                ONSCREEN,
                (16777217, true, 1, 1),
                (16777218, true, 1, 1),
            ]
            .into_iter(),
            |_| None,
        );
        assert_eq!(
            plan.offscreen.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![16777217, 16777218, 16777219]
        );
    }

    #[test]
    fn bypass_ok_for_single_webgl_onscreen_canvas() {
        // The canonical bypass case: one onscreen canvas whose DrawingBuffer
        // matches the surface, no 2D context (WebGL), no readback → bypass is
        // safe.
        assert!(can_bypass_drawing_buffer(1, false, false, true));
    }

    #[test]
    fn onscreen_canvas2d_context_disables_bypass() {
        // Regression: a single onscreen Canvas2D canvas. Skia renders into the
        // DrawingBuffer FBO; if bypass skipped the blit those pixels would
        // never reach the window (black screen). Bypass MUST be off.
        assert!(!can_bypass_drawing_buffer(1, false, true, true));
    }

    #[test]
    fn onscreen_db_smaller_than_surface_disables_bypass() {
        // Regression: a game (Phaser Scale.NONE) shrinks its onscreen canvas
        // below the surface, so the DrawingBuffer no longer matches the surface.
        // Bypass renders straight to FBO 0 with no scaling, landing the small
        // image in the corner; the DrawingBuffer→surface blit upscales it to
        // fill the window. Bypass MUST be off whenever db != surface (here also
        // false when no DrawingBuffer exists).
        assert!(!can_bypass_drawing_buffer(1, false, false, false));
    }

    /// A second canvas keeps the onscreen canvas on the DrawingBuffer blit.
    ///
    /// **The name this test used to carry asserted a reason that is false**, and it is
    /// worth recording rather than quietly correcting: it said FBO 0 stops naming the
    /// window once a pbuffer exists. FBO 0 follows the current surface *of the current
    /// context*, and every canvas here owns its own context paired with its own
    /// surface, so a pbuffer is only ever current with the canvas that owns it. See
    /// [`can_bypass_drawing_buffer`] for the whole argument and for what the condition
    /// is actually doing.
    ///
    /// So what this pins is that the condition is still in force — which is deliberate
    /// while bypass has never run in steady state on any device — and not why.
    ///
    /// **Split from the readback case deliberately.** The two were one test, so
    /// deleting either condition failed it and neither was individually pinned —
    /// the aggregate-assertion shape this plan warns about. One test per condition
    /// means a mutant names the guard it broke.
    #[test]
    fn a_second_canvas_keeps_the_onscreen_canvas_on_the_drawing_buffer() {
        assert!(!can_bypass_drawing_buffer(2, false, false, true));
    }

    /// A latched default-FBO readback means content has to survive
    /// `eglSwapBuffers`, and under bypass the window's back buffer is undefined
    /// afterwards per the EGL spec. Only the DrawingBuffer preserves it.
    #[test]
    fn a_latched_default_fbo_readback_disables_bypass() {
        assert!(!can_bypass_drawing_buffer(1, true, false, true));
    }

    // ---- What the default framebuffer resolves to, and who re-points it ----
    //
    // Bypass was measured presenting *nothing*: `scripts/fixtures/bypass-probe`
    // holds all four bypass conditions for a whole run, painted 180 frames of
    // rgba(51,204,102,255), and the captured frame was (0,0,0,0) everywhere,
    // while bunnymark — identical path except `canvas_count=2` — captured its
    // real scene. The frames were landing in the DrawingBuffer, which bypass
    // has by definition stopped blitting. Two causes, both here: the meaning of
    // "the default framebuffer" changed with nothing re-pointing the binding,
    // and two of the three sites that do re-point it derived the target without
    // the bypass term.

    fn fbo(n: u32) -> glow::NativeFramebuffer {
        glow::NativeFramebuffer(std::num::NonZeroU32::new(n).expect("non-zero fbo name"))
    }

    /// Bypass *is* the statement "this canvas's WebGL default framebuffer is the
    /// window's". A resolver that returned the DrawingBuffer here would send
    /// every draw into a buffer nothing blits.
    #[test]
    fn bypass_resolves_the_default_framebuffer_to_the_window() {
        assert_eq!(default_framebuffer_of(true, Some(fbo(7))), None);
    }

    /// The Chromium DrawingBuffer pattern off the bypass path: WebGL's "default
    /// framebuffer" is our FBO, and the swap-time blit is what presents it.
    #[test]
    fn without_bypass_the_default_framebuffer_is_the_drawing_buffer() {
        assert_eq!(default_framebuffer_of(false, Some(fbo(7))), Some(fbo(7)));
    }

    #[test]
    fn default_framebuffer_remaps_only_default_bindings_in_both_directions() {
        use crate::backend::gl::readback_test_gl as fixture;
        for installed_is_zero in [false, true] {
            let old = if installed_is_zero { 0 } else { 7 };
            let new = if installed_is_zero { 7 } else { 0 };
            let applied_fbo = (!installed_is_zero).then(|| fbo(7));
            let desired = installed_is_zero.then(|| fbo(7));
            for (read, draw) in [(old, old), (old, 9), (8, old), (8, 9)] {
                let gl = fixture::context();
                fixture::set_bindings(fixture::Bindings {
                    read_framebuffer: read,
                    draw_framebuffer: draw,
                    ..Default::default()
                });
                let mut applied = applied_fbo;
                apply_default_framebuffer(&gl, true, &mut applied, desired);
                assert_eq!(
                    fixture::bindings().read_framebuffer,
                    if read == old { new } else { read }
                );
                assert_eq!(
                    fixture::bindings().draw_framebuffer,
                    if draw == old { new } else { draw }
                );
                assert_eq!(applied, desired);
                assert_eq!(
                    fixture::mutations(),
                    usize::from(read == old || draw == old)
                );
            }
        }
    }

    /// A DrawingBuffer rebuilt while bypass never changed is a new default
    /// framebuffer. The mode flag this replaced could not express that, so the
    /// remap was skipped and the content kept drawing into the deleted name.
    #[test]
    fn a_rebuilt_drawing_buffer_remaps_even_though_the_mode_is_unchanged() {
        use crate::backend::gl::readback_test_gl as fixture;
        let gl = fixture::context();
        fixture::set_bindings(fixture::Bindings {
            read_framebuffer: 7,
            draw_framebuffer: 7,
            ..Default::default()
        });
        let mut applied = Some(fbo(7));
        apply_default_framebuffer(&gl, true, &mut applied, Some(fbo(11)));
        assert_eq!(applied, Some(fbo(11)));
        assert_eq!(fixture::bindings().read_framebuffer, 11);
        assert_eq!(fixture::bindings().draw_framebuffer, 11);
        assert_eq!(fixture::mutations(), 1);
    }

    #[test]
    fn default_framebuffer_change_waits_for_its_context_and_then_applies_once() {
        use crate::backend::gl::readback_test_gl as fixture;
        let gl = fixture::context();
        let mut applied = None;
        apply_default_framebuffer(&gl, false, &mut applied, Some(fbo(7)));
        assert_eq!(applied, None);
        assert_eq!(fixture::mutations(), 0);
        // Return to the owning context, with READ default and DRAW custom.
        fixture::set_bindings(fixture::Bindings {
            draw_framebuffer: 0x8000_0009,
            ..Default::default()
        });
        apply_default_framebuffer(&gl, true, &mut applied, Some(fbo(7)));
        assert_eq!(applied, Some(fbo(7)));
        assert_eq!(fixture::bindings().read_framebuffer, 7);
        assert_eq!(fixture::bindings().draw_framebuffer, 0x8000_0009);
        assert_eq!(fixture::mutations(), 1);
        apply_default_framebuffer(&gl, true, &mut applied, Some(fbo(7)));
        assert_eq!(fixture::mutations(), 1);
    }

    #[test]
    fn default_framebuffer_changes_that_cancel_while_away_need_no_bind() {
        use crate::backend::gl::readback_test_gl as fixture;
        let gl = fixture::context();
        let mut applied = None;
        apply_default_framebuffer(&gl, false, &mut applied, Some(fbo(7)));
        apply_default_framebuffer(&gl, false, &mut applied, None);
        apply_default_framebuffer(&gl, true, &mut applied, None);
        assert_eq!(applied, None);
        assert_eq!(fixture::mutations(), 0);
    }

    #[test]
    fn default_read_source_uses_read_binding_and_native_unknown_state() {
        use crate::backend::gl::{readback_test_gl as fixture, state_tracker as st};
        for (gl, probed) in [
            (fixture::context as fn() -> glow::Context, false),
            (fixture::context_gles2_framebuffer_blit, true),
        ] {
            let gl = gl();
            let mut state = CanvasGLState::default();
            st::update_bind_framebuffer(&mut state, glow::READ_FRAMEBUFFER, None);
            st::update_bind_framebuffer(&mut state, glow::DRAW_FRAMEBUFFER, Some(9001));
            assert!(reads_from_default_framebuffer(
                &gl,
                probed,
                Some(&state),
                Some(fbo(7))
            ));
            st::update_bind_framebuffer(&mut state, glow::FRAMEBUFFER, None);
            st::update_bind_framebuffer(&mut state, glow::READ_FRAMEBUFFER, Some(9002));
            assert!(!reads_from_default_framebuffer(
                &gl,
                probed,
                Some(&state),
                None
            ));
            state.bound_framebuffer.forget_all();
            fixture::set_bindings(fixture::Bindings {
                read_framebuffer: 7,
                draw_framebuffer: 9,
                ..Default::default()
            });
            assert!(reads_from_default_framebuffer(
                &gl,
                probed,
                Some(&state),
                Some(fbo(7))
            ));
            assert!(!reads_from_default_framebuffer(&gl, probed, None, None));
            assert_eq!(fixture::unsupported_calls(), 0);
        }
    }

    #[test]
    fn default_mapping_without_a_drawing_buffer_needs_no_split_bindings() {
        use crate::backend::gl::readback_test_gl as fixture;
        let gl = fixture::context_gles2(fixture::Gles2Caps::default());
        let mut applied = None;
        apply_default_framebuffer(&gl, true, &mut applied, None);
        assert!(reads_from_default_framebuffer(&gl, false, None, None));
        assert_eq!(fixture::mutations(), 0);
        assert_eq!(fixture::unsupported_calls(), 0);
    }

    // ---- Present bookkeeping across swap and blit outcomes ----
    //
    // These replace two guards in `present_damage.rs` that read the *source
    // text* of `swap_buffers_no_restore` and asserted the byte offsets of
    // `.swap_buffers(`, `self.damage.reset()` and `blit_succeeded` were in
    // order. That shape cannot fail on a behavioural change which leaves the
    // text intact, and it sat on the presentation path. Passing the swap outcome
    // in as data makes both branches reachable with no window surface.

    use crate::present_damage::{DamageRegion, PresentDamageHistory, PresentDamagePlan};

    /// A plan whose two regions are *distinguishable*, so a test can tell which
    /// one the commit recorded. Both were the same value in the earlier
    /// source-text guard, which is part of why "history records current, never
    /// repair" could only be checked by reading the call.
    fn plan_with_distinct_regions() -> PresentDamagePlan {
        PresentDamagePlan {
            current: DamageRegion::from_rect(
                crate::present_damage::DamageRect::new(0, 0, 360, 640).expect("non-empty rect"),
            ),
            repair: DamageRegion::FullSurface,
        }
    }

    /// The frame's damage is what the *next* present owes the compositor. A swap
    /// that failed presented nothing, so resetting the accumulator would drop
    /// the debt and the retry would declare a region smaller than what is
    /// actually stale — leaving the previous image on screen inside the part
    /// nobody declared.
    #[test]
    fn a_failed_swap_keeps_the_frames_damage_for_the_retry() {
        let mut damage = FrameDamageAccumulator::new();
        let mut history = PresentDamageHistory::new();
        damage.add(DamageEffect::OnscreenRect {
            x: 0,
            y: 0,
            width: 360,
            height: 640,
        });

        let commit = commit_present_outcome(
            &mut damage,
            &mut history,
            false,
            true,
            &plan_with_distinct_regions(),
        );

        assert_eq!(commit, PresentCommit::Retry);
        assert!(
            damage.has_damage(),
            "a failed swap must leave the accumulated damage in place for the retry"
        );
        assert_eq!(
            history.len(),
            0,
            "nothing reached the surface, so nothing may enter the buffer-age history"
        );
    }

    /// The control for the test above. Without it, "damage survived a failed
    /// swap" is satisfied by an accumulator that never resets at all — every
    /// frame would then repair the whole surface forever and the buffer-age
    /// machinery would be decoration.
    #[test]
    fn a_successful_swap_resets_the_frames_damage() {
        let mut damage = FrameDamageAccumulator::new();
        let mut history = PresentDamageHistory::new();
        damage.add(DamageEffect::OnscreenRect {
            x: 0,
            y: 0,
            width: 360,
            height: 640,
        });

        let commit = commit_present_outcome(
            &mut damage,
            &mut history,
            true,
            true,
            &plan_with_distinct_regions(),
        );

        assert!(matches!(commit, PresentCommit::Presented(_)));
        assert!(
            !damage.has_damage(),
            "a presented frame starts the next one owing nothing"
        );
        assert_eq!(history.len(), 1, "a presented frame is repairable from");
    }

    /// `repair` is age-expanded: it covers what this frame had to rewrite to make
    /// an N-frames-old back buffer whole, which is a superset of what the content
    /// changed. Recording it would make each later repair grow off the previous
    /// one's expansion until every frame was full.
    #[test]
    fn history_records_the_frames_own_damage_and_never_the_age_expanded_repair() {
        let mut damage = FrameDamageAccumulator::new();
        let mut history = PresentDamageHistory::new();
        let plan = plan_with_distinct_regions();

        commit_present_outcome(&mut damage, &mut history, true, true, &plan);

        // Age 2 unions `current` with the newest history entry. The plan's
        // `repair` is FullSurface here, so recording it would answer FullSurface
        // and recording `current` answers the rect — which is what tells the two
        // apart without an accessor into the ring buffer.
        assert_eq!(
            history.resolve_with_age(&plan.current, 2),
            plan.current,
            "the history entry must be the current-frame region, not the repair region"
        );
    }

    /// A partial blit means the back buffer is only partly defined, but the swap
    /// already advanced EGL's buffer sequence, so the frame cannot be retried in
    /// place. The recovery is to poison history: the entry a later
    /// buffer-age-2 present repairs from must be `FullSurface`, not the region
    /// this frame *intended* to write.
    #[test]
    fn a_failed_blit_makes_the_history_unusable_as_a_repair_source() {
        let mut damage = FrameDamageAccumulator::new();
        let mut history = PresentDamageHistory::new();
        let plan = plan_with_distinct_regions();
        history.push(plan.current.clone());

        let commit = commit_present_outcome(&mut damage, &mut history, true, false, &plan);

        assert_eq!(commit, PresentCommit::PresentedPartial);
        assert_eq!(
            history.resolve_with_age(&plan.current, 2),
            DamageRegion::FullSurface,
            "a partially-written frame must be unusable as a repair source"
        );
    }

    /// Separate from the history poison because they are separate obligations and
    /// separate defects. Poisoning history only fixes what a *later* present
    /// repairs *from*; the very next present also has to repair everything, and
    /// that debt lives in the accumulator. A commit that did one and not the
    /// other would leave a real hole, so bundling them into one assertion would
    /// leave whichever half broke unnamed.
    #[test]
    fn a_failed_blit_leaves_the_next_present_owing_the_whole_surface() {
        let mut damage = FrameDamageAccumulator::new();
        let mut history = PresentDamageHistory::new();

        commit_present_outcome(
            &mut damage,
            &mut history,
            true,
            false,
            &plan_with_distinct_regions(),
        );

        assert!(
            damage.is_full_surface(),
            "the next present must repair everything, not the region this frame meant to write"
        );
    }

    // ---- Unified DamageEffect accumulator integration tests ----
    // These verify that the CanvasManager methods correctly feed the accumulator,
    // testing the same scenarios as the previous stage_partial_damage / resolve_staged_damage
    // tests but through the unified model.

    #[test]
    fn canvas2d_rect_resolves_to_partial_damage() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 300,
            height: 400,
        });
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 10,
                y: 20,
                width: 300,
                height: 400
            }
        );
    }

    #[test]
    fn canvas2d_plus_gl_viewport_unions_to_partial() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::OnscreenRect {
            x: 200,
            y: 300,
            width: 150,
            height: 100,
        });
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 10,
                y: 20,
                width: 340,
                height: 380
            }
        );
    }

    #[test]
    fn untracked_gl_clear_forces_full_surface() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::FullSurface);
        assert_eq!(acc.resolve((1080, 1920)), ResolvedDamage::FullSurface);
    }

    #[test]
    fn offscreen_gl_produces_no_damage() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::NoDamage); // offscreen GL
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 10,
                y: 20,
                width: 100,
                height: 50
            }
        );
    }

    #[test]
    fn full_surface_after_partial_rects_poisons_accumulator() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::OnscreenRect {
            x: 200,
            y: 300,
            width: 150,
            height: 100,
        });
        acc.add(DamageEffect::FullSurface);
        assert_eq!(acc.resolve((1080, 1920)), ResolvedDamage::FullSurface);
    }

    #[test]
    fn multiple_mixed_batches_union_correctly() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 0,
            y: 0,
            width: 50,
            height: 50,
        });
        acc.add(DamageEffect::OnscreenRect {
            x: 100,
            y: 100,
            width: 60,
            height: 40,
        });
        acc.add(DamageEffect::OnscreenRect {
            x: 30,
            y: 20,
            width: 80,
            height: 60,
        });
        acc.add(DamageEffect::OnscreenRect {
            x: 200,
            y: 0,
            width: 50,
            height: 200,
        });
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 0,
                y: 0,
                width: 250,
                height: 200
            }
        );
    }

    #[test]
    fn scissor_bounded_clear_unions_with_canvas2d() {
        let mut acc = FrameDamageAccumulator::new();
        // Canvas2D rect
        acc.add(DamageEffect::OnscreenRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        });
        // Scissor-bounded clear (produced by damage_for_clear when scissor is active)
        acc.add(DamageEffect::OnscreenRect {
            x: 200,
            y: 200,
            width: 50,
            height: 50,
        });
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 0,
                y: 0,
                width: 250,
                height: 250
            }
        );
    }

    // ---- GL state tracking tests ----

    #[test]
    fn gl_state_defaults_correctly() {
        use super::ScissorState;
        let state = CanvasGLState::default();
        assert!(state.draws_to_default_fbo);
        assert_eq!(state.scissor, ScissorState::Disabled);
        assert_eq!(state.last_scissor_rect, None);
    }

    #[test]
    fn gl_state_tracks_fbo_and_scissor() {
        use super::ScissorState;
        let mut state = CanvasGLState::default();

        // Bind user FBO
        state.draws_to_default_fbo = false;
        assert!(!state.draws_to_default_fbo);

        // Set scissor rect + enable
        state.last_scissor_rect = Some((10, 20, 100, 50));
        state.scissor = ScissorState::Enabled {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        };
        assert!(matches!(state.scissor, ScissorState::Enabled { .. }));

        // Disable scissor
        state.scissor = ScissorState::Disabled;
        assert_eq!(state.scissor, ScissorState::Disabled);
        // last_scissor_rect retained
        assert_eq!(state.last_scissor_rect, Some((10, 20, 100, 50)));
    }

    #[test]
    fn handler_rejection_sync_fallbacks_when_image_can_never_fit() {
        let mut server = crate::upload_server::UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 512 * 1024);

        assert_eq!(
            decide_async_upload_reject_action(true, Some(&server), 600 * 1024),
            AsyncUploadRejectAction::SyncFallback
        );
    }

    #[test]
    fn deferred_retry_rejection_stays_deferred_when_budget_pressure_is_temporary() {
        let mut server = crate::upload_server::UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 512 * 1024);

        assert_eq!(
            decide_async_upload_reject_action(true, Some(&server), 256 * 1024),
            AsyncUploadRejectAction::DeferRetry
        );
    }

    #[test]
    fn handler_rejection_sync_fallbacks_when_upload_thread_is_degraded() {
        let mut server = crate::upload_server::UploadServer::new(4, 4 * 1024 * 1024);
        server.set_frame_budget(2, 512 * 1024);

        assert_eq!(
            decide_async_upload_reject_action(false, Some(&server), 256 * 1024),
            AsyncUploadRejectAction::SyncFallback
        );
    }

    #[test]
    fn handler_rejection_sync_fallbacks_without_upload_server() {
        assert_eq!(
            decide_async_upload_reject_action(true, None, 256 * 1024),
            AsyncUploadRejectAction::SyncFallback
        );
    }

    #[test]
    fn default_fbo_readback_latch_only_on_first_signal() {
        assert!(should_latch_default_fbo_readback(false));
        assert!(!should_latch_default_fbo_readback(true));
    }

    // ---- DeferredUpload queue semantics (P13) ------------------------
    //
    // These tests cover the queue cap + FIFO ordering without needing
    // an EGL-backed CanvasManager: we exercise the naked VecDeque
    // contract the handler relies on.

    #[test]
    fn deferred_upload_queue_is_fifo() {
        use shared::protocol::io_cmd::NormalizedImage;
        // Three distinct image_ids pushed in order; popping from the
        // front yields them in the same order.  Matches the
        // `Image.onload` spec requirement that loads fire in issue
        // order even when a budget squeeze defers them.
        let mut q: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
        q.push_back(1);
        q.push_back(2);
        q.push_back(3);
        assert_eq!(q.pop_front(), Some(1));
        assert_eq!(q.pop_front(), Some(2));
        assert_eq!(q.pop_front(), Some(3));
        // NormalizedImage reference just pinned so a refactor that
        // changes the struct triggers this test too.
        let _ = NormalizedImage::new(1, 1, vec![0, 0, 0, 0]);
    }

    #[test]
    fn max_deferred_uploads_constant_is_reasonable() {
        // Bound is a meaningful soft cap, not accidentally 0 or
        // astronomically large.  If a future tuning change lands
        // here the assert is an early warning.
        assert!(MAX_DEFERRED_UPLOADS >= 32);
        assert!(MAX_DEFERRED_UPLOADS <= 4096);
    }

    #[test]
    fn wait_failed_is_permanent_but_timeout_is_deferred() {
        assert!(fence_failed_permanently(glow::WAIT_FAILED));
        assert!(!fence_failed_permanently(glow::TIMEOUT_EXPIRED));
        assert!(!fence_failed_permanently(glow::ALREADY_SIGNALED));
    }

    #[test]
    fn deferred_upload_byte_budget_rejects_before_entry_cap() {
        assert!(deferred_upload_fits(
            MAX_DEFERRED_UPLOADS - 1,
            MAX_DEFERRED_BYTES - 4,
            4
        ));
        assert!(!deferred_upload_fits(
            MAX_DEFERRED_UPLOADS - 1,
            MAX_DEFERRED_BYTES - 4,
            5,
        ));
        assert!(!deferred_upload_fits(0, MAX_DEFERRED_BYTES, 1));
    }

    #[test]
    fn snapshot_fence_status_has_explicit_timeout_and_failure_results() {
        assert_eq!(
            classify_snapshot_fence(glow::ALREADY_SIGNALED),
            SnapshotFenceStatus::Signalled
        );
        assert_eq!(
            classify_snapshot_fence(glow::TIMEOUT_EXPIRED),
            SnapshotFenceStatus::Timeout
        );
        assert_eq!(
            classify_snapshot_fence(glow::WAIT_FAILED),
            SnapshotFenceStatus::Failed
        );
    }
}
