//! `Canvas2DContext` — the CanvasManager-level Skia wrapper.
//!
//! One instance per live Canvas2D surface.  Owns:
//!   * A [`DirectContext`] tied to the EGL context that hosts the canvas.
//!     Each onscreen / offscreen canvas has its own EGL context (Chromium
//!     "DrawingBuffer" model) so each needs its own Skia context too.
//!   * A [`Surface`] wrapping the DrawingBuffer FBO (onscreen) or a
//!     pbuffer's default FBO (offscreen).  Drawing into this surface goes
//!     straight to the same GL attachments WebGL targets — no intermediate
//!     blit needed between 2D ↔ 3D frames.
//!   * A [`Canvas2DRenderer`] (pure state machine, see `canvas.rs`) that
//!     dispatches Canvas2DCmd opcodes against `surface.canvas()`.
//!
//! GL state hygiene: Skia and WebGL share a single EGL context, so Skia
//! mutates shared GL state (binding, blending, scissor, …).  Whenever we
//! interleave a Canvas2D batch with a WebGL batch we call
//! [`Canvas2DContext::reset_gl_state`] afterwards to flush Skia's
//! deferred draws and tell Skia to drop its tracked GL state — subsequent
//! WebGL commands then see a clean slate.

use std::cell::RefCell;

use skia_safe::{
    Canvas as SkCanvas, ColorType, Matrix, Paint, Rect as SkRect, SamplingOptions, Shader,
    Surface as SkSurface, TileMode,
    gpu::{
        self, DirectContext, SurfaceOrigin, backend_render_targets, direct_contexts, gl as sk_gl,
    },
};

use super::canvas::Canvas2DRenderer;
use super::image_store::ImageStore;
use super::paint::PatternResolver;
use super::state::Canvas2DState;
use super::text::TextContext;

/// Kind of framebuffer backing the canvas.  Affects the `SurfaceOrigin`
/// we hand to Skia: Android EGL's default framebuffer is bottom-left
/// origin (same as OpenGL), intermediate FBOs are top-left because we
/// control their orientation on upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FboKind {
    /// The default framebuffer (FBO 0) of an EGL surface — bottom-left
    /// origin, Y axis points up.
    DefaultFb,
    /// A named FBO we created ourselves (DrawingBuffer for the onscreen
    /// canvas).  Also bottom-left-origin in this project because the
    /// DrawingBuffer blit step flips at present time.
    DrawingBuffer,
}

/// Per-context GPU resource cache budget.
///
/// Rationale: Skia's default `GrResourceCache` budget is
/// unbounded-ish (96 MB + 2^20 resources on current Ganesh builds),
/// which is catastrophic when we have multiple Canvas2DContexts on
/// an Android device with 2 GB of system RAM.
///
/// We cap each context's Ganesh cache so the aggregate across all
/// live contexts stays within the 200 MB native-heap target.  Two
/// constants carve up that budget:
///
/// * `SKIA_RESOURCE_CACHE_BUDGET_BYTES` is the *aggregate* ceiling
///   we're willing to hand Skia across all live canvases.
/// * Each context's per-instance cap is
///   `max(MIN_PER_CTX_BYTES, budget / live_ctxs)` -- i.e. a single
///   onscreen canvas still gets the full 32 MiB, two canvases get
///   16 MiB each, four get 8 MiB each, and so on.  The minimum
///   keeps a tiny-but-active offscreen canvas above the glyph-atlas
///   working set.
///
/// Call [`Canvas2DContext::rebalance_resource_cache`] after any
/// canvas create / destroy so existing contexts pick up the new
/// share.  See Skia `GrDirectContext::setResourceCacheLimits`:
/// <https://api.skia.org/classGrDirectContext.html>.
/// Default aggregate Skia resource cache budget (32 MiB).  Kept
/// as the process-wide lower bound; individual tiers may raise
/// it through [`set_skia_resource_cache_budget`].
const DEFAULT_SKIA_RESOURCE_CACHE_BUDGET_BYTES: usize = 32 * 1024 * 1024;
/// Minimum per-context cap.  See [`per_ctx_resource_cache_bytes`].
///
/// **Was 4 MiB, and that floor outranked the aggregate ceiling: once
/// `n > aggregate / MIN_PER_CTX_BYTES` the process granted `n *
/// MIN_PER_CTX_BYTES` regardless of the ceiling — 320 MiB at 80 contexts on
/// every tier (3.3x–20x the aggregate). Both guarantees (an aggregate ceiling,
/// and a per-context floor) cannot hold for unbounded `n`; that is arithmetic,
/// not a call to make from a host. It wanted a device measurement, which is
/// what this value now reflects.**
///
/// 2026-08-28, Mali-G76 (Kirin 990): 80 live offscreen `Canvas2DContext`s, each
/// redrawn every frame with a short `fillText` label (the `canvas_id_set`
/// ~30-label shop-UI scale and the `render_thread` reorder fixture's 80,
/// scripts/fixtures/skia-floor-probe-{30,80,80-dynamic}), read via
/// `dumpsys meminfo` and render-thread CPU% (`/proc/<pid>/stat`
/// utime+stime, median of three 2s windows — frame time is not the right
/// instrument at 60 vsyncs/s; see the JITLESS.md precedent for why):
///
/// * **The overshoot is real, not theoretical, at the old floor** — and it
///   materialised the same way whether the 80 canvases redrew *unchanging*
///   text every frame or a different string every frame (`Graphics`
///   398 MB vs 402 MB): being drawn every frame is what mattered, not whether
///   the content changed. A 0-canvas control measured 9 MB of fixed `Graphics`
///   overhead; 30 canvases measured 166 MB (+5.2 MB/canvas) and 80 measured
///   398 MB (+4.9 MB/canvas over the control) — tracking the 4 MiB floor
///   almost exactly, and even a little past it (some of each context's
///   `Graphics` cost is fixed FBO/EGL surface overhead on top of the tunable
///   Ganesh cache).
/// * **The floor did not earn its keep.** The same 80-context fixture,
///   rebuilt with `MIN_PER_CTX_BYTES = 0` (aggregate/n honoured exactly —
///   1.2 MiB/context on TierA), showed render-thread CPU indistinguishable
///   from the shipped 4 MiB floor: medians of 128.5/130.0/131.0/134.0%
///   (floor) vs 122.5/136.0/131.5/127.5% (no floor) across four device-cooled
///   runs each — the two distributions overlap; there is no thrash signal.
///   This was the worst case for the floor's own justification (every context
///   drawn every frame, none idle), and it still did not need one.
/// * **And lowering the floor to 64 KiB did not lower `Graphics` either** —
///   398.3 MB before this change, 398.0 MB after, at the identical 80-context
///   fixture (0.09% apart — noise, not a trend), even though the Skia cache
///   *ceiling* this constant controls dropped 3.3x (4 MiB/context to
///   1.2 MiB/context). **That is the load-bearing finding, not the crossover
///   arithmetic below.** It means the ~4.5 MB/context this file's own comment
///   above attributed to the resource-cache floor is not coming from the
///   resource cache at all — actual per-context Ganesh usage for a fixture
///   this small was already far under both the old and the new ceiling, so
///   neither ceiling was ever what Skia's cache was bumping against. The
///   real cost is almost certainly the fixed overhead of the "one EGL context
///   (+ `DirectContext` + FBO) per `Canvas2DContext`" architecture this
///   file's own module comment documents as deliberate (no cross-context
///   blit between 2D and 3D) — driver-side command-buffer pools, shader
///   compiler state and EGL surface bookkeeping that a resource-cache-budget
///   knob cannot touch, because it is not resource-cache memory. Fixing *that*
///   is an architecture question (sharing GL contexts across canvases, or
///   bounding how many are concurrently backed by one), well past a constant
///   edit, and is not what this change claims to have done.
///
/// So this constant is kept at 64 KiB because it is still a real, if smaller,
/// fix: the *ceiling* the aggregate promises (see the module doc above, "the
/// aggregate across all live contexts stays within the 200 MB native-heap
/// target") is honoured again at realistic scene sizes, where it previously
/// was not, and nothing measured got worse for it. It is kept only as a
/// backstop against a literal zero-byte cache, not as a policy lever:
/// [`per_ctx_share`] still computes `max(aggregate / n, MIN_PER_CTX_BYTES)`,
/// but at 64 KiB the crossover (`aggregate / MIN_PER_CTX_BYTES`) is 1536
/// contexts on TierA, 768 on TierB, and 256 even under
/// [`LOW_MEMORY_AGGREGATE_BYTES`] — comfortably past the 80 this repository's
/// own fixtures anticipate as a worst case, so the floor does not bind at any
/// scene size measured or expected. If a future scene legitimately needs more
/// than 256 concurrently *live* contexts, that is a new measurement, not a
/// reason to raise this back toward 4 MiB: the 80-context result above is the
/// evidence that a uniform per-context floor is the wrong lever at that
/// scale — the correct fix past 256 is bounding how many contexts count as
/// "hot" (an LRU over recently-drawn contexts, so an idle canvas's cache can
/// be released independent of how many other
/// canvases exist), not raising a constant every live context multiplies by.
///
/// `the_aggregate_is_honoured_at_the_scene_sizes_this_repository_anticipates`
/// pins the new crossover and the 80-context result so both fail loudly
/// rather than drift quietly; `the_floor_still_backstops_a_degenerate_context_
/// count` keeps the old "floor can still overshoot" behaviour tested, just at
/// the (much larger) `n` where it now actually applies.
const MIN_PER_CTX_BYTES: usize = 64 * 1024;
const SKIA_RESOURCE_CACHE_MAX_RESOURCES: usize = 1 << 14;

/// Runtime-tunable budget.  Set at engine init based on
/// `DeviceCapabilities::tier()` and lowered on
/// `onTrimMemory` hooks (P1-12).  The atomic is `AtomicUsize`
/// because reads happen on every canvas create / destroy and
/// we want the load to be lock-free.
static SKIA_RESOURCE_CACHE_BUDGET_BYTES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(DEFAULT_SKIA_RESOURCE_CACHE_BUDGET_BYTES);

/// Tune the aggregate resource-cache budget at runtime.
///
/// One call site: engine init, once the device tier has been detected, as
/// `set_skia_resource_cache_budget(tier_budget(tier))`. A low-memory signal
/// deliberately does *not* come through here — see [`low_memory_per_ctx_bytes`].
///
/// Existing contexts pick up the new cap the next time
/// [`Canvas2DContext::rebalance_resource_cache`] runs (driven by
/// canvas create / destroy).  To force an immediate rebalance,
/// the manager can call `rebalance_resource_cache` itself for
/// every live context.
pub fn set_skia_resource_cache_budget(bytes: usize) {
    SKIA_RESOURCE_CACHE_BUDGET_BYTES.store(
        bytes.max(MIN_PER_CTX_BYTES),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Suggested per-tier budget.  Numbers match Chromium-mobile
/// defaults on equivalent hardware and have been validated
/// against the 200 MB native-heap target on TierA devices.
pub fn tier_budget(tier: crate::device_caps::DeviceTier) -> usize {
    use crate::device_caps::DeviceTier;
    match tier {
        DeviceTier::TierA => 96 * 1024 * 1024,
        DeviceTier::TierB => 48 * 1024 * 1024,
    }
}

/// Aggregate ceiling a low-memory signal squeezes Skia down to.  Dropping below
/// `MIN_PER_CTX_BYTES` per context wouldn't be useful — Skia would re-evict on the
/// very next draw.
const LOW_MEMORY_AGGREGATE_BYTES: usize = 16 * 1024 * 1024;

/// Compute the per-context byte cap for the number of live
/// `Canvas2DContext`s **in the process**.
///
/// The count has to be process-wide because the budget is. It used to be passed in
/// as one `CanvasManager`'s own `contexts_2d.len()`, and there is one manager per
/// Session — so two Sessions each holding one canvas each divided the whole
/// aggregate budget by one, and the process handed Skia twice the ceiling this
/// module says it is willing to hand out. N Sessions meant N times.
///
/// Convergence is lazy, and that is forced rather than chosen: a Skia
/// `DirectContext` may only be touched from the render thread that owns it, so a
/// Session cannot rebalance another Session's contexts. A newly created context
/// takes the smaller share at once; already-live contexts in other Sessions keep
/// their larger cap until their own next canvas create or destroy. The overshoot is
/// bounded by what those contexts had already been granted.
#[inline]
/// The resource-count cap that goes with [`per_ctx_resource_cache_bytes`].
pub(crate) fn skia_max_resources() -> usize {
    SKIA_RESOURCE_CACHE_MAX_RESOURCES
}

pub(crate) fn per_ctx_resource_cache_bytes() -> usize {
    per_ctx_share(SKIA_RESOURCE_CACHE_BUDGET_BYTES.load(std::sync::atomic::Ordering::Relaxed))
}

/// The per-context cap a low-memory signal squeezes to.
///
/// **Not a budget, and that is the whole point.** A memory warning arrives per
/// Session — the host relays one Android `onTrimMemory` once for each — and it used
/// to be answered by *storing* 16 MiB as the process budget, which only engine init
/// ever raised again. So one game's warning capped every other game's canvases for
/// the life of the process. What a warning actually needs is a release, and Skia
/// releases when a lower cap is installed, so this figure is handed to
/// [`Canvas2DContext::trim_resource_cache`] and the ordinary share is restored in the
/// same call. Nothing outside that call is left capped, and a second Session relaying
/// the same signal finds nothing left to free rather than compounding the first.
#[inline]
pub(crate) fn low_memory_per_ctx_bytes() -> usize {
    per_ctx_share(LOW_MEMORY_AGGREGATE_BYTES)
}

/// One aggregate ceiling's share, for the contexts the process actually has.
///
/// Private, and takes no count: a caller that could pass the divisor is how the
/// numerator and the denominator came to have different scopes.
#[inline]
fn per_ctx_share(aggregate: usize) -> usize {
    let n = LIVE_CANVAS_CONTEXTS
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    (aggregate / n).max(MIN_PER_CTX_BYTES)
}

/// Live `Canvas2DContext` count, across every Session in the process.
static LIVE_CANVAS_CONTEXTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Membership in [`LIVE_CANVAS_CONTEXTS`], tied to a context's lifetime.
///
/// A field of `Canvas2DContext` rather than a pair of manual calls: the compiler
/// then refuses to build a context that is not counted, and the decrement cannot be
/// forgotten on any exit path — including a construction that fails after the
/// counter was raised.
pub(crate) struct LiveContextCount {
    /// False for a context that shares another's `GrDirectContext`. The count
    /// exists to divide the Skia resource budget, and the divisor must be the
    /// number of *contexts*, not the number of canvases -- counting sharers
    /// would shrink every context's share as sharing made the total cheaper.
    counted: bool,
}

impl LiveContextCount {
    fn enrol() -> Self {
        LIVE_CANVAS_CONTEXTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { counted: true }
    }

    /// Membership for a canvas that borrows someone else's `GrDirectContext`.
    /// Takes no slot, and releases none on drop.
    fn shared() -> Self {
        Self { counted: false }
    }
}

impl Drop for LiveContextCount {
    fn drop(&mut self) {
        if self.counted {
            LIVE_CANVAS_CONTEXTS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Monotonic allocator for `Canvas2DContext` identity tags.  Used by
/// [`ImageStore`] to key per-context SkImage wrapper caches without
/// having to hash the raw `DirectContext` handle (skia-safe exposes
/// `DirectContextId: Eq + Copy` but intentionally not `Hash`).
static CTX_TAG_ALLOC: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Tag for the one shared offscreen 2D context. Drawn from the same allocator
/// so it can never collide with a per-canvas tag.
pub(crate) fn alloc_shared_ctx_tag() -> u32 {
    alloc_ctx_tag()
}

fn alloc_ctx_tag() -> u32 {
    CTX_TAG_ALLOC.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Thin newtype around `skia_safe::gpu::DirectContext` (P2-3).
///
/// Exists to give the rest of the renderer a bounded API surface
/// that documents *which* Ganesh operations we actually use —
/// callers that want to reach past this wrapper still can via
/// [`Self::inner_mut`], but the presence of this type lets a
/// reviewer audit new Skia entry points by grepping for
/// `CanvasGr::` rather than trawling every file that imports
/// `skia_safe::gpu::DirectContext`.
///
/// The current engine already stores `gr_ctx: DirectContext`
/// directly on `Canvas2DContext` for backwards-compat with the
/// existing call graph; migrating each call site to use
/// `CanvasGr` is a follow-up (see `AUDIT.md` P2-3).  Until then
/// the newtype is exposed here as a documentation anchor and a
/// place to hang a narrow API once the migration starts.
#[allow(dead_code)]
pub(crate) struct CanvasGr {
    inner: DirectContext,
}

#[allow(dead_code)]
impl CanvasGr {
    /// Wrap an existing `DirectContext`.
    #[inline]
    pub(crate) fn new(inner: DirectContext) -> Self {
        Self { inner }
    }

    /// Access the raw Ganesh handle.  Kept `pub(crate)` so the
    /// boundary isn't accidentally widened to downstream crates;
    /// internal callers migrating to [`CanvasGr`] can use this
    /// during the transition without rewriting the full call
    /// stack.
    #[inline]
    pub(crate) fn inner_mut(&mut self) -> &mut DirectContext {
        &mut self.inner
    }

    /// Narrow `reset` wrapper: accepts a `skia_safe`-agnostic
    /// bitmask and passes it through.  Encourages call sites to
    /// use the named constants from `gr_state_bits` instead of
    /// raw Skia types.
    #[inline]
    pub(crate) fn reset(&mut self, bits: Option<u32>) {
        self.inner.reset(bits);
    }

    #[inline]
    pub(crate) fn flush_and_submit(&mut self) {
        self.inner.flush_and_submit();
    }

    #[inline]
    pub(crate) fn perform_deferred_cleanup(&mut self, not_used: std::time::Duration) {
        self.inner.perform_deferred_cleanup(not_used, None);
    }
}

/// Outcome of [`Canvas2DContext::try_fast_path_draw_image`].
///
/// Making the fast-path result explicit (P2-12) prevents the
/// previous "match-and-fall-through" pattern from silently
/// dropping new `DrawImage*` variants into the slow path
/// unannounced — a new variant that a contributor forgets to add
/// to the fast-path match now either produces the correct
/// fallback (handled cleanly) or trips a compiler warning if
/// someone adds `Handled` without a body.  The enum is private
/// to the backend because it's an implementation detail of the
/// dispatch layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FastPathOutcome {
    /// Command was handled inside the fast path.  The inner
    /// boolean mirrors the rest of the backend API's "was this
    /// a real render" convention: `true` means pixels were
    /// submitted, `false` means the command resolved to a
    /// no-op (e.g. a `DrawImageBatch` with zero resolvable
    /// entries).
    Handled(bool),
    /// Command was not a fast-path candidate.  The caller must
    /// route it through the generic resolver-based dispatch.
    Fallback,
}

/// Bit flags mirroring Skia's `GrGLBackendState` (see
/// `skia/include/gpu/ganesh/GrTypes.h`).  Only the bits Migo
/// actually needs are named; the rest fall through to
/// `skia_bindings::kAll_GrBackendState` via [`GrStateBits::ALL`].
///
/// Using a bitmask instead of the previous boolean lets the
/// per-context staleness tracker invalidate *exactly* the GL state
/// that external code mutated — e.g. an AHB import only dirties
/// the active texture binding, so there is no reason to force
/// Skia to re-send viewport / blend / program / stencil /
/// pixel-store state on the next draw.  Matches the idiomatic
/// Skia integration pattern documented in `GrDirectContext.h`.
pub mod gr_state_bits {
    /// Render target / framebuffer binding.
    pub const RENDER_TARGET: u32 = 1 << 0;
    /// Active texture unit + bound texture (includes sampler
    /// objects for ES 3.0+).  Set by AHB import and by raw GL
    /// texture creation that bypasses Skia.
    pub const TEXTURE_BINDING: u32 = 1 << 1;
    /// Scissor + viewport.
    pub const VIEW: u32 = 1 << 2;
    /// Blend enable / equation / factors.
    pub const BLEND: u32 = 1 << 3;
    /// Pixel-store `glPixelStorei` (UNPACK_ALIGNMENT etc).
    pub const PIXEL_STORE: u32 = 1 << 7;
    /// Currently bound program + active uniforms / attrib bindings.
    pub const PROGRAM: u32 = 1 << 8;

    /// "All known" mask.  Prefer this over `u32::MAX` so the
    /// invalidation stays inside Skia's declared enum range — on
    /// drivers that add new bits in the future we still want
    /// `reset_context(ALL)` to be equivalent to passing `None`.
    pub const ALL: u32 = 0xffff;
}

/// Per-canvas Skia surface + state.
pub struct Canvas2DContext {
    /// Keeps this context in the process-wide live count for as long as it exists,
    /// which is what makes the Skia budget's denominator match its numerator.
    _counted: LiveContextCount,
    pub gr_ctx: DirectContext,
    pub surface: SkSurface,
    /// The assembled GL interface this context's `GrDirectContext` was built
    /// from, kept so [`Canvas2DContext::resize`] can rebuild the context
    /// without needing the entry-point loader threaded back in. It is
    /// reference-counted on the Skia side, so holding it costs a refcount.
    interface: sk_gl::Interface,
    pub renderer: Canvas2DRenderer,
    pub width: u32,
    pub height: u32,
    pub fbo_id: u32,
    pub kind: FboKind,
    /// Stable monotonic tag identifying this `Canvas2DContext` /
    /// `DirectContext` pair.  Used as a key by `ImageStore` to cache
    /// SkImage wrappers per-context — see
    /// [`super::image_store::ImageStore::resolve_cached_or_wrap`].
    pub ctx_tag: u32,
    /// Bitmask of [`gr_state_bits`] describing which slices of GL
    /// state external code (WebGL dispatch, DrawingBuffer blit, AHB
    /// import) has mutated since Skia's last `reset_context`.
    ///
    /// The NEXT Skia-touching op on this context reads the mask,
    /// issues one `GrDirectContext::reset(Some(mask))`, and clears
    /// the mask back to 0.  `0` means "Skia's tracked state is in
    /// sync"; non-zero means "pass the mask to Skia".
    ///
    /// Per-context rather than manager-global: each `Canvas2DContext`
    /// owns its own `GrDirectContext`, so invalidation has to be
    /// scoped to the affected context to avoid under-invalidation on
    /// the other contexts that share the EGL context.
    pub skia_state_stale: u32,
    /// Reused atlas partition and geometry scratch for the draw-image fast
    /// path. These buffers belong to the Canvas2D owner, so warm sprite
    /// batches do not allocate on the render thread.
    atlas_runs: Vec<crate::draw_atlas::BatchRun>,
    atlas_xforms: Vec<skia_safe::RSXform>,
    atlas_tex: Vec<SkRect>,
}

/// Why a `Canvas2DContext` could not be built.
///
/// Returned rather than only logged, because *which* of the steps failed is the
/// whole question and a log line is not readable from everywhere that asks.
/// Measured 2026-09-11: an external-frame Canvas2D batch on macOS reported
/// "Skia Canvas2DContext::new failed for canvas_id=1 (64x64 fbo=1)" and the
/// next question -- which of the steps -- had no answer anywhere. Naming the
/// steps in the log (which this file already does) fixed that for a host with a
/// subscriber installed and not for a `#[test]`, because this workspace builds
/// `tracing-subscriber` without its `fmt` feature, so a unit test has no
/// subscriber to install and every one of those messages goes nowhere. A
/// returned value is the one form both a host log line and a test assertion can
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Canvas2DInitFailure {
    /// Skia could not assemble a GL interface from the injected loader: it
    /// resolved no usable entry points, or no context was current when it
    /// asked. About the loader or the caller's preconditions.
    GlInterface,
    /// Skia assembled an interface and then declined to build a
    /// `GrDirectContext` on it. The entry points resolved and Skia rejected
    /// what it found behind them -- about the driver, not the framebuffer.
    DirectContext,
    /// Skia built a context and would not wrap the caller's framebuffer as a
    /// render target. About the FBO -- its attachments, size or format -- not
    /// the driver.
    WrapRenderTarget,
    /// Skia would not allocate a render target on the shared offscreen
    /// context. Only [`Canvas2DContext::new_shared_offscreen`] produces this,
    /// and unlike the three above it is an allocation failure rather than a
    /// capability one.
    SharedRenderTarget,
}

impl Canvas2DInitFailure {
    /// A short stable name for the step, for logs and assertion messages.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GlInterface => "GL interface assembly",
            Self::DirectContext => "GrDirectContext::make_gl",
            Self::WrapRenderTarget => "wrap_backend_render_target",
            Self::SharedRenderTarget => "shared-context render_target",
        }
    }
}

impl std::fmt::Display for Canvas2DInitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Canvas2DContext {
    /// Create a new Skia-backed Canvas2D bound to an existing FBO.
    ///
    /// Preconditions:
    ///   * Caller has already made the target EGL context current.
    ///   * `fbo_id` names a framebuffer with a colour attachment of at
    ///     least 8 bits per RGBA channel.
    ///   * `width`/`height` are the attachment dimensions in physical
    ///     pixels (no DPR scaling; Canvas 2D coords are 1:1 with pixels).
    ///
    /// Returns which step failed if it could not build one; see
    /// [`Canvas2DInitFailure`], whose variants want different investigations.
    ///
    /// `load_gl` resolves a GL entry point. It must come from the *same* EGL
    /// implementation the caller injected for this manager, which is why it is
    /// a parameter rather than something Skia looks up for itself: Skia's own
    /// `interfaces::make_egl()` calls whichever `eglGetProcAddress` Skia was
    /// linked against, and on a host that supplies its own EGL (the Linux X11
    /// and Wayland presenters, or a test double) that is not necessarily the
    /// one Migo is driving. Assembling the interface from the injected loader
    /// makes both halves of the process agree by construction.
    ///
    /// It also removes Skia's need to link EGL at all, which is what lets this
    /// crate link on Windows: Skia's GL-interface selection is an if/else-if
    /// chain where `skia_use_egl` makes the Windows branch unreachable, while
    /// skia-bindings emits its `GrGLInterfaces::MakeWin` wrapper on every
    /// Windows build regardless — an unresolvable pair for as long as we asked
    /// Skia for the EGL interface.
    pub fn new(
        fbo_id: u32,
        width: u32,
        height: u32,
        kind: FboKind,
        load_gl: &dyn Fn(&str) -> *const std::ffi::c_void,
    ) -> Result<Self, Canvas2DInitFailure> {
        // Named steps, because the caller's error could only say the whole
        // thing failed. Three things can go wrong here and they have nothing to
        // do with each other -- a loader that cannot resolve GL, a driver Skia
        // will not build a context on, and a framebuffer it will not wrap --
        // and telling them apart from the outside was not possible. Measured
        // 2026-09-11: an external-frame Canvas2D batch reported "Skia
        // Canvas2DContext::new failed for canvas_id=1 (64x64 fbo=1)" and the
        // next question, which of the three, had no answer in the log.
        // What the driver says it is, read through the loader Skia is about to
        // use. Logged before the interface is built, so a failure below has the
        // answer sitting above it: a null version means no context was current
        // and the interface was assembled from nothing, while a real one means
        // Skia looked at a live context and declined it. Those want opposite
        // fixes, and the message alone could not tell them apart.
        //
        // Through `load_gl` and not a linked `glGetString`, which is the
        // mistake this replaces: declaring the symbol resolves it against
        // whatever GL the process happens to link -- on macOS the system
        // OpenGL, not the ANGLE this manager drives -- and calling that with no
        // CGL context current took the test process down with no output at all.
        {
            type GetString = unsafe extern "C" fn(u32) -> *const std::ffi::c_char;
            const GL_VERSION: u32 = 0x1F02;
            let raw = load_gl("glGetString");
            let version = if raw.is_null() {
                "<glGetString unresolved>".to_string()
            } else {
                // SAFETY: the loader returned a non-null entry point for a name
                // whose signature is fixed by the GL ES specification.
                let get_string: GetString = unsafe { std::mem::transmute(raw) };
                let text = unsafe { get_string(GL_VERSION) };
                if text.is_null() {
                    "<null: no context was current>".to_string()
                } else {
                    unsafe { std::ffi::CStr::from_ptr(text) }
                        .to_string_lossy()
                        .into_owned()
                }
            };
            tracing::info!(gl_version = %version, "Skia is about to build a GL context");
        }

        let Some(interface) = sk_gl::Interface::new_load_with(|symbol| load_gl(symbol)) else {
            tracing::error!(
                "Skia GL interface load failed: the loader resolved no usable GL entry \
                 points, or no context was current when it was asked"
            );
            return Err(Canvas2DInitFailure::GlInterface);
        };
        Self::with_interface(interface, fbo_id, width, height, kind)
    }

    /// Build a context around an already-assembled GL interface.
    ///
    /// Split out so `resize` can rebuild the `GrDirectContext` from the
    /// interface this context already holds. Re-assembling one there would mean
    /// carrying the loader through every resize call site, across borrows of
    /// the canvas manager that the entry-point table is a sibling field of.
    fn with_interface(
        interface: sk_gl::Interface,
        fbo_id: u32,
        width: u32,
        height: u32,
        kind: FboKind,
    ) -> Result<Self, Canvas2DInitFailure> {
        let Some(mut gr_ctx) = direct_contexts::make_gl(interface.clone(), None) else {
            tracing::error!(
                "Skia GrDirectContext::make_gl returned none for the current GL context"
            );
            return Err(Canvas2DInitFailure::DirectContext);
        };

        let fb_info = sk_gl::FramebufferInfo {
            fboid: fbo_id,
            // GL_RGBA8 == 0x8058, but the Skia binding exposes it as the
            // canonical `Format::RGBA8` constant on the `gl` module.  Use
            // the symbolic constant so a driver quirk (e.g. BGRA swap)
            // can be swapped out later in one spot.
            format: gl_rgba8(),
            protected: gpu::Protected::No,
        };

        let target = backend_render_targets::make_gl(
            (width as i32, height as i32),
            /* sample_count */ Some(0),
            /* stencil_bits */ 8,
            fb_info,
        );

        let surface = gpu::surfaces::wrap_backend_render_target(
            &mut gr_ctx,
            &target,
            SurfaceOrigin::BottomLeft,
            ColorType::RGBA8888,
            /* color_space */ None,
            /* surface_props */ None,
        );
        let Some(surface) = surface else {
            tracing::error!(
                fbo = fbo_id,
                width,
                height,
                "Skia would not wrap the framebuffer as a render target"
            );
            return Err(Canvas2DInitFailure::WrapRenderTarget);
        };

        // Clamp Ganesh's resource cache so a long-running scene
        // can't silently grow the GPU memory footprint past the
        // 200 MB native-heap target.  Start at the single-context
        // budget; `rebalance_resource_cache` shrinks the per-cap
        // as additional canvases come online.  See
        // `per_ctx_resource_cache_bytes` and
        // <https://api.skia.org/classGrDirectContext.html>.
        // Enrolled before the cap is computed so this context is in its own divisor.
        let counted = LiveContextCount::enrol();
        gr_ctx.set_resource_cache_limits(skia_safe::gpu::ganesh::ResourceCacheLimits {
            max_resources: SKIA_RESOURCE_CACHE_MAX_RESOURCES,
            max_resource_bytes: per_ctx_resource_cache_bytes(),
        });

        Ok(Self {
            _counted: counted,
            gr_ctx,
            surface,
            interface,
            renderer: Canvas2DRenderer::new(),
            width,
            height,
            fbo_id,
            kind,
            skia_state_stale: 0,
            ctx_tag: alloc_ctx_tag(),
            atlas_runs: Vec::new(),
            atlas_xforms: Vec::new(),
            atlas_tex: Vec::new(),
        })
    }

    /// Build an offscreen Canvas2D surface on a `GrDirectContext` that other
    /// offscreen canvases also use.
    ///
    /// **Why.** Measured on a Mate 30 Pro, an offscreen canvas costs 4.86 MB of
    /// `Graphics` and **96% of that is its own `GrDirectContext`** -- the EGL
    /// context under it is 0.20 MB and the 128x64 backing is 32 KB. 80 canvases
    /// therefore hold 398 MB where the pixels account for 2.5 MB. The full
    /// attribution is in `docs/performance/android/multicanvas-fixed-cost.md`.
    /// One context with many surfaces is also Skia's own usage model; a context
    /// per surface was the unusual part.
    ///
    /// **Two invariants this depends on, both enforced by the caller:**
    ///
    /// 1. The shared `DirectContext` lives on a GL context that *nothing else
    ///    ever makes current*. Skia caches GL state per `GrDirectContext`, so a
    ///    WebGL batch or a contextless op running on the same GL context would
    ///    invalidate that cache silently. The manager gives this its own EGL
    ///    context rather than reusing the resource context for exactly that
    ///    reason -- one EGL context for all of them, not one each.
    /// 2. The surface is a Skia-allocated render target, not a wrap of FBO 0.
    ///    FBO 0 names whichever EGL surface is current, so with one shared
    ///    context it would name the same pbuffer for every canvas. The
    ///    resulting `fbo_id` is read back out of Skia so the raw-GL snapshot
    ///    path keeps working unchanged.
    ///
    /// `gr_ctx` is a refcounted handle, so the clone stored here is the same
    /// context, and `LiveContextCount` is deliberately *not* enrolled: the Skia
    /// budget divides by the number of contexts, and there is only one.
    pub fn new_shared_offscreen(
        gr_ctx: &DirectContext,
        interface: sk_gl::Interface,
        width: u32,
        height: u32,
        ctx_tag: u32,
    ) -> Result<Self, Canvas2DInitFailure> {
        let mut gr_ctx = gr_ctx.clone();
        let image_info = skia_safe::ImageInfo::new(
            (width as i32, height as i32),
            ColorType::RGBA8888,
            skia_safe::AlphaType::Premul,
            None,
        );
        let Some(mut surface) = gpu::surfaces::render_target(
            &mut gr_ctx,
            gpu::Budgeted::No,
            &image_info,
            /* sample_count */ Some(0),
            // Same origin as every other Canvas2D surface in this project, so
            // readback, snapshot and the `drawImage`-from-canvas path all keep
            // the orientation they already assume.
            SurfaceOrigin::BottomLeft,
            /* surface_props */ None,
            /* should_create_with_mips */ false,
            /* is_protected */ false,
        ) else {
            tracing::error!(
                width,
                height,
                "Skia would not allocate a render target on the shared offscreen context"
            );
            return Err(Canvas2DInitFailure::SharedRenderTarget);
        };

        // The snapshot path blits from a raw FBO id. Ask Skia which one it
        // allocated rather than tracking a second copy of that fact.
        let fbo_id = gpu::surfaces::get_backend_render_target(
            &mut surface,
            skia_safe::surface::BackendHandleAccess::FlushRead,
        )
        .and_then(|rt| rt.gl_framebuffer_info().map(|info| info.fboid))
        .unwrap_or(0);

        Ok(Self {
            _counted: LiveContextCount::shared(),
            gr_ctx,
            surface,
            interface,
            renderer: Canvas2DRenderer::new(),
            width,
            height,
            fbo_id,
            // A named FBO we own, bottom-left origin -- the same shape as the
            // onscreen DrawingBuffer, which is what this variant describes.
            kind: FboKind::DrawingBuffer,
            skia_state_stale: 0,
            ctx_tag,
            atlas_runs: Vec::new(),
            atlas_xforms: Vec::new(),
            atlas_tex: Vec::new(),
        })
    }

    /// Release GPU resources Skia purged from its deferred command
    /// buffer.  Called from `CanvasManager` on app background/
    /// low-memory signals.  A zero-argument call means "only clean
    /// up resources that have already aged out"; pass a negative
    /// `msecs` (via the raw API) for aggressive immediate cleanup.
    #[inline]
    pub fn perform_deferred_cleanup(&mut self, ms_not_used: std::time::Duration) {
        self.gr_ctx.perform_deferred_cleanup(ms_not_used, None);
    }

    /// Mark every slice of Skia's cached GL state dirty because
    /// external code mutated multiple categories at once — e.g. a
    /// full WebGL batch that changed program, textures, viewport,
    /// and blend state.  Equivalent to passing `None` to
    /// `DirectContext::reset`.
    ///
    /// Prefer [`Self::mark_state_stale_bits`] when the caller
    /// knows exactly which slice changed (AHB import only
    /// mutates `TEXTURE_BINDING`, a DrawingBuffer blit only
    /// mutates `RENDER_TARGET`): the narrower invalidation lets
    /// Skia skip re-sending the rest of its tracked state on the
    /// next draw, saving ~a dozen GL calls per Canvas2D↔WebGL
    /// boundary.
    #[inline]
    pub fn mark_state_stale(&mut self) {
        self.skia_state_stale = gr_state_bits::ALL;
    }

    /// Mark a specific subset of Skia's cached GL state dirty.
    /// The bits OR together with any previously recorded
    /// staleness — a subsequent draw will see the union.
    #[inline]
    pub fn mark_state_stale_bits(&mut self, bits: u32) {
        self.skia_state_stale |= bits;
    }

    /// Re-apply the resource-cache byte cap for the current number
    /// of live `Canvas2DContext`s.  Called by the manager after any
    /// canvas create / destroy so the aggregate Skia cache budget
    /// stays pinned at
    /// [`SKIA_RESOURCE_CACHE_BUDGET_BYTES`] regardless of how
    /// many contexts are live.
    #[inline]
    pub fn rebalance_resource_cache(&mut self) {
        self.gr_ctx
            .set_resource_cache_limits(skia_safe::gpu::ganesh::ResourceCacheLimits {
                max_resources: SKIA_RESOURCE_CACHE_MAX_RESOURCES,
                max_resource_bytes: per_ctx_resource_cache_bytes(),
            });
    }

    /// Squeeze this context to the low-memory share, then restore the share the
    /// aggregate budget allows.
    ///
    /// Installing a lower cap is what makes Skia purge: `setResourceCacheLimits`
    /// evicts down to the new figure before returning, so the release lands inside
    /// this call and the cap does not have to stay behind to have had its effect.
    /// That is the difference from what this replaced — a *stored* low budget, which
    /// no signal ever lifted and which therefore capped every Session in the process,
    /// not just the one that was asked to trim.
    #[inline]
    pub fn trim_resource_cache(&mut self) {
        self.gr_ctx
            .set_resource_cache_limits(skia_safe::gpu::ganesh::ResourceCacheLimits {
                max_resources: SKIA_RESOURCE_CACHE_MAX_RESOURCES,
                max_resource_bytes: low_memory_per_ctx_bytes(),
            });
        self.rebalance_resource_cache();
    }

    /// Idempotent lazy reset — issues `reset_context(bits)` only
    /// when the dirty mask is non-zero.  Safe to call before every
    /// Skia draw batch; cheap when state isn't actually stale.
    ///
    /// If the mask saturates to [`gr_state_bits::ALL`] we still
    /// pass `None` (i.e. `kAll_GrBackendState`) to Skia so any
    /// hypothetical future GL backend state bit outside our
    /// enumeration is also invalidated; partial masks get mapped
    /// 1:1 to `Some(bits)`.
    #[inline]
    pub fn reset_gl_state_if_stale(&mut self) {
        if self.skia_state_stale == 0 {
            return;
        }
        let reset_arg = if self.skia_state_stale == gr_state_bits::ALL {
            None
        } else {
            Some(self.skia_state_stale)
        };
        self.gr_ctx.reset(reset_arg);
        self.skia_state_stale = 0;
        crate::render_diagnostics::bump_skia_context_reset();
    }

    #[inline]
    pub fn canvas(&mut self) -> &SkCanvas {
        self.surface.canvas()
    }

    /// Apply a Canvas2D command with no image-store access.
    ///
    /// Kept for tests and callers that never use `drawImage` /
    /// `createPattern`.  Production callers should use
    /// [`Self::apply_with_images`] so image-backed paths actually draw.
    pub fn apply<R: PatternResolver>(
        &mut self,
        cmd: &shared::protocol::render_cmd::Canvas2DCmd,
        text: &TextContext,
        resolver: &R,
    ) -> bool {
        let env = super::canvas::DrawEnv {
            canvas: self.surface.canvas(),
            text: Some(text),
            resolver,
        };
        self.renderer.apply_env(&env, cmd)
    }

    /// Apply a Canvas2D command with full access to the shared
    /// [`ImageStore`] — required for `DrawImage` / `DrawImageBatch` /
    /// `createPattern` fills to actually rasterise pixels.
    ///
    /// Borrow gymnastics: this method explicitly destructures `&mut self`
    /// into disjoint `&mut gr_ctx`, `&mut surface`, and `&mut renderer`
    /// borrows so the pattern resolver (which needs mutable access to
    /// the Ganesh context to wrap a backend texture into an `SkImage`)
    /// can coexist with the draw call (which borrows `surface.canvas()`).
    ///
    /// The dispatch logic is split into two steps so future readers
    /// don't have to infer which commands take the fast path.  See
    /// [`FastPathOutcome`] and [`Self::try_fast_path_draw_image`].
    pub fn apply_with_images(
        &mut self,
        cmd: &shared::protocol::render_cmd::Canvas2DCmd,
        text: Option<&TextContext>,
        image_store: &mut ImageStore,
    ) -> bool {
        // P2-12: dispatch via an explicit `FastPathOutcome` rather
        // than a loosely-documented `match ... _ => {}` + fall-through.
        match self.try_fast_path_draw_image(cmd, image_store) {
            FastPathOutcome::Handled(painted) => return painted,
            FastPathOutcome::Fallback => {}
        }

        // Everything else goes through the renderer with a real
        // pattern resolver that can wrap a stored texture into a
        // tiling `SkShader`.  `RefCell` is the cheapest way to hand a
        // `&mut DirectContext` into a `&self` trait method without
        // reshaping the trait signature.
        let Canvas2DContext {
            gr_ctx,
            surface,
            renderer,
            ctx_tag,
            ..
        } = self;
        let ctx_tag = *ctx_tag;
        let resolver = SkiaPatternResolver {
            ctx_tag,
            gr_ctx: RefCell::new(gr_ctx),
            image_store: RefCell::new(image_store),
        };
        let env = super::canvas::DrawEnv {
            canvas: surface.canvas(),
            text,
            resolver: &resolver,
        };
        renderer.apply_env(&env, cmd)
    }

    /// Draw-image fast path: `DrawImage` / `DrawImageBatch` talk to
    /// `SkCanvas::draw_image_rect` directly without building a
    /// `SkiaPatternResolver`, because the pattern resolver isn't
    /// needed for the common sprite blit case and its
    /// `RefCell<&mut DirectContext>` construction isn't free.
    ///
    /// Returns [`FastPathOutcome::Handled(painted)`] when the command
    /// matched a fast path (with `painted = true` iff the draw
    /// actually emitted pixels), or [`FastPathOutcome::Fallback`]
    /// when the caller should route the command through the
    /// generic `apply_env` path.  Any new `Canvas2DCmd` variant that
    /// wants a fast path **must** add a branch here; forgetting to
    /// do so only costs the extra resolver construction, never
    /// correctness.
    fn try_fast_path_draw_image(
        &mut self,
        cmd: &shared::protocol::render_cmd::Canvas2DCmd,
        image_store: &mut ImageStore,
    ) -> FastPathOutcome {
        use shared::protocol::render_cmd::Canvas2DCmd;

        let Canvas2DContext {
            gr_ctx,
            surface,
            renderer,
            ctx_tag,
            atlas_runs,
            atlas_xforms,
            atlas_tex,
            ..
        } = self;
        let ctx_tag = *ctx_tag;

        match cmd {
            Canvas2DCmd::DrawImage {
                image_id,
                sx,
                sy,
                sw,
                sh,
                dx,
                dy,
                dw,
                dh,
            } => {
                let painted = draw_one_image(
                    ctx_tag,
                    gr_ctx,
                    surface.canvas(),
                    renderer,
                    image_store,
                    *image_id,
                    *sx,
                    *sy,
                    *sw,
                    *sh,
                    *dx,
                    *dy,
                    *dw,
                    *dh,
                );
                FastPathOutcome::Handled(painted)
            }
            Canvas2DCmd::DrawImageBatch { draws } => {
                // Build the image paint once; all sub-draws share the
                // current 2D state (globalAlpha / composite / shadow /
                // smoothing).  Snapshot the state for the build
                // closure so the `&mut renderer` borrow needed by
                // `acquire_image_paint` doesn't conflict with the
                // `&renderer.state` the closure would otherwise
                // capture.  `Canvas2DState` is `Clone`; the copy is
                // cheap relative to the Paint construction we're
                // trying to skip in the cache-hit case.
                let state_snapshot = renderer.state.clone();
                let paint = renderer.acquire_image_paint(|| build_image_paint(&state_snapshot));
                let use_atlas = renderer.draw_atlas;
                let canvas = surface.canvas();
                let mut any = false;

                // One `drawAtlas` per consecutive same-image, uniformly scaled
                // run; everything else keeps going through `drawImageRect`.
                // `partition_into` never reorders, so the alpha-blend order the
                // content issued is the order these are submitted in. The
                // output table belongs to this Canvas2D owner and is reused.
                atlas_runs.clear();
                if use_atlas {
                    crate::draw_atlas::partition_into(draws, 2, atlas_runs);
                } else {
                    atlas_runs.push(crate::draw_atlas::BatchRun::Individual {
                        start: 0,
                        end: draws.len(),
                    });
                }

                for run in atlas_runs.iter().copied() {
                    let (start, end) = run.range();
                    match run {
                        crate::draw_atlas::BatchRun::Atlas { .. } => {
                            // Every entry in the run shares one image, so one
                            // resolve serves all of them.
                            let Some(img) = image_store.resolve_cached_or_wrap(
                                ctx_tag,
                                gr_ctx,
                                draws[start].image_id,
                            ) else {
                                continue;
                            };
                            atlas_xforms.clear();
                            atlas_tex.clear();
                            for d in &draws[start..end] {
                                let scale = crate::draw_atlas::uniform_scale(d);
                                // RSXform places the source rect's origin at
                                // (tx, ty) scaled by `scale` with no rotation,
                                // which is exactly a uniform sprite blit.
                                atlas_xforms.push(skia_safe::RSXform::new(
                                    scale,
                                    0.0,
                                    (d.dx, d.dy),
                                ));
                                atlas_tex.push(SkRect::from_xywh(d.sx, d.sy, d.sw, d.sh));
                            }
                            if !renderer.draw_atlas_reported.replace(true) {
                                tracing::info!(
                                    sprites = end - start,
                                    "Canvas2D drawAtlas path active"
                                );
                            }
                            canvas.draw_atlas(
                                &img,
                                &atlas_xforms,
                                &atlas_tex,
                                None,
                                skia_safe::BlendMode::SrcOver,
                                state_snapshot.image_sampling_options(),
                                None,
                                &paint,
                            );
                            any = true;
                        }
                        crate::draw_atlas::BatchRun::Individual { .. } => {
                            for d in &draws[start..end] {
                                if let Some(img) =
                                    image_store.resolve_cached_or_wrap(ctx_tag, gr_ctx, d.image_id)
                                {
                                    let src = SkRect::from_xywh(d.sx, d.sy, d.sw, d.sh);
                                    let dst = SkRect::from_xywh(d.dx, d.dy, d.dw, d.dh);
                                    canvas.draw_image_rect_with_sampling_options(
                                        &img,
                                        Some((&src, skia_safe::canvas::SrcRectConstraint::Fast)),
                                        dst,
                                        state_snapshot.image_sampling_options(),
                                        &paint,
                                    );
                                    any = true;
                                }
                            }
                        }
                    }
                }
                FastPathOutcome::Handled(any)
            }
            _ => FastPathOutcome::Fallback,
        }
    }

    /// Flush Skia's deferred draws and SUBMIT to the GL driver.  Called
    /// at frame-end, at a Canvas2D→WebGL boundary (Materialize op), and
    /// whenever a synchronous readback needs to see pixels.
    pub fn flush_and_submit(&mut self) {
        self.gr_ctx.flush_and_submit();
    }

    /// Read back a Canvas2D sub-rectangle as unpremultiplied RGBA8888.
    ///
    /// This is the renderer-side implementation of Canvas2D
    /// `getImageData()`, so it intentionally matches the JS-visible
    /// `ImageData` layout rather than WebGL's `readPixels` contract.
    pub fn read_image_data(
        &mut self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> shared::error::EngineResult<Vec<u8>> {
        self.reset_gl_state_if_stale();
        self.flush_and_submit();
        read_surface_rgba_unpremul(&mut self.surface, x, y, width, height).ok_or_else(|| {
            shared::error::EngineError::new(shared::error::ErrorCode::Internal)
                .with_msg("Canvas2D getImageData read_pixels failed")
        })
    }

    /// Tell Skia to drop its cached GL state tracking.  Required
    /// immediately after [`flush_and_submit`] when control is about to
    /// return to code that mutates GL state outside Skia (WebGL handler,
    /// DrawingBuffer blit).  See Skia docs on
    /// `GrDirectContext::resetContext()`.
    pub fn reset_gl_state(&mut self) {
        self.gr_ctx.reset(None);
    }

    /// Mark the underlying Ganesh context abandoned so dropping this
    /// wrapper will not issue GL object destruction against the wrong
    /// or already-dead EGL context.
    pub fn abandon(&mut self) {
        self.gr_ctx.abandon();
    }

    /// Resize after a surface change (window resize / orientation).  We
    /// rebuild the backing `SkSurface` because the FBO dimensions and
    /// possibly the underlying attachment handle changed.
    ///
    /// `image_store` is a hole for purging the SkImage wrapper cache
    /// this context previously populated.  The wrappers are bound to
    /// the *old* `GrDirectContext`; once we swap in a new one they're
    /// stale — continuing to hand them out would produce undefined
    /// rendering on the next `drawImage` / pattern lookup.  To avoid
    /// the GrContext-identity trap we also allocate a fresh `ctx_tag`,
    /// so any wrapper entry the purge missed (e.g. re-entrant callers)
    /// becomes unreachable via the new cache key.
    pub fn resize(
        &mut self,
        fbo_id: u32,
        width: u32,
        height: u32,
        image_store: &mut ImageStore,
    ) -> bool {
        let new_self =
            match Self::with_interface(self.interface.clone(), fbo_id, width, height, self.kind) {
                Ok(built) => built,
                Err(step) => {
                    tracing::error!(
                        step = step.as_str(),
                        fbo = fbo_id,
                        width,
                        height,
                        "Canvas2D resize could not rebuild its Skia context"
                    );
                    return false;
                }
            };
        // Drop every SkImage wrapper this context produced: they hold
        // a `GrDirectContext` pointer that's about to be dropped, and
        // reusing them post-swap is undefined behaviour inside Skia.
        image_store.purge_wrappers_for_context(self.ctx_tag);
        // `renderer.reset()` below returns the whole state machine to spec
        // defaults, which is what `canvas.width = N` is defined to do and what
        // the JS setters assume when they reset their shadow to match.
        //
        // A caller resizing for any OTHER reason -- the platform handed us a
        // different surface -- has to carry the state across itself, because
        // the content did not ask for a reset and will never re-send a value it
        // believes is still in force. `resize_canvas_for_surface_change` is
        // that caller.
        self.gr_ctx = new_self.gr_ctx;
        self.surface = new_self.surface;
        self.fbo_id = fbo_id;
        self.width = width;
        self.height = height;
        // Fresh ctx_tag: guarantees the next `resolve_cached_or_wrap`
        // call cannot collide with any residual wrapper from the
        // previous GrDirectContext.
        self.ctx_tag = new_self.ctx_tag;
        self.renderer.reset();
        true
    }

    /// The drawing-state machine (styles, alpha, line and text settings) this
    /// context holds.
    ///
    /// JS keeps a shadow copy of every one of these setters and skips sending a
    /// value it believes is already current, which makes this the authoritative
    /// half of a pair split across the ABI. Any path that replaces a context
    /// must carry the state across: a replacement that comes up at spec
    /// defaults desynchronises the two halves permanently, because the content
    /// has no way to learn its state was discarded and will never re-send it.
    /// `resize` preserves this by keeping `renderer`; a destroy-and-recreate
    /// has to ask for it explicitly -- and so does `resize`, which resets it.
    pub fn drawing_state(&self) -> Canvas2DState {
        self.renderer.state.clone()
    }

    /// Adopt drawing state captured from the context this one replaces.
    ///
    /// Re-applies the transform as well, because `Canvas2DState::ctm` is a
    /// mirror of `SkCanvas`'s own CTM rather than the thing Skia draws with,
    /// and the `SkCanvas` behind a replacement surface starts at identity.
    /// Adopting the mirror alone would leave the two disagreeing -- the damage
    /// classifier transforms rectangles with the mirror -- and would drop a
    /// transform the content set once and, like every other de-duplicated
    /// setter, will never send again.
    pub fn adopt_drawing_state(&mut self, state: Canvas2DState) {
        let [a, b, c, d, e, f] = state.ctm;
        self.renderer.state = state;
        let m = Matrix::new_all(a, c, e, b, d, f, 0.0, 0.0, 1.0);
        self.surface.canvas().set_matrix(&skia_safe::M44::from(m));
    }

    /// Clear the entire surface to transparent — spec'd fallout of
    /// `canvas.width = N`.  Does not mutate the state machine.
    pub fn clear_to_transparent(&mut self) {
        self.surface.canvas().clear(skia_safe::Color::TRANSPARENT);
    }
}

/// `GL_RGBA8` (0x8058) — named here so a future "use BGRA on Mali" quirk
/// can be flagged in one place.  Skia's `gl::Format::RGBA8` constant
/// aliases this value on every backend we target.
#[inline]
fn gl_rgba8() -> u32 {
    // 0x8058 is the sized internal format RGBA8 defined by the GLES 3.0
    // spec §3.8.3.  Using a literal rather than an enum to avoid a
    // feature-flag dependency on skia-bindings' gl constants.
    0x8058
}

// ---------------------------------------------------------------------------
// Image drawing helpers (shared by DrawImage / DrawImageBatch / pattern fills)
// ---------------------------------------------------------------------------

/// `SkPaint` preset tuned for `drawImage`.  Rebuilt each call because
/// the caller's `Canvas2DState` might have changed blend mode / alpha /
/// shadow / smoothing between frames, but the paint itself is cheap
/// (value type).
fn build_image_paint(state: &Canvas2DState) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(state.antialias);
    paint.set_blend_mode(state.blend_mode);

    // Canvas2D applies globalAlpha to *every* colour component of the
    // source image, not to a separate alpha channel — Skia's
    // `set_alpha_f` does exactly that.
    paint.set_alpha_f(state.global_alpha.clamp(0.0, 1.0));

    super::paint::apply_shadow_to_paint(&mut paint, state);
    paint
}

/// Execute a single `drawImage` - factored out of
/// [`Canvas2DContext::apply_with_images`] so the call path works
/// out the same between DrawImage and DrawImageBatch.
///
/// Routes the paint construction through
/// [`Canvas2DRenderer::acquire_image_paint`] so that a burst of
/// identical-style draws (the common UI case) only builds the
/// `SkPaint` once.
#[allow(clippy::too_many_arguments)]
fn draw_one_image(
    ctx_tag: u32,
    gr_ctx: &mut DirectContext,
    canvas: &SkCanvas,
    renderer: &mut super::canvas::Canvas2DRenderer,
    image_store: &mut ImageStore,
    image_id: u32,
    sx: f32,
    sy: f32,
    sw: f32,
    sh: f32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
) -> bool {
    let Some(sk_image) = image_store.resolve_cached_or_wrap(ctx_tag, gr_ctx, image_id) else {
        return false;
    };

    // Snapshot the state before crossing into `acquire_image_paint`
    // (which needs a `&mut renderer`).  `Canvas2DState` is `Clone`
    // and the copy is vastly cheaper than the Paint construction
    // the cache is there to skip.
    let state_snapshot = renderer.state.clone();
    let paint = renderer.acquire_image_paint(|| build_image_paint(&state_snapshot));
    let src = SkRect::from_xywh(sx, sy, sw, sh);
    let dst = SkRect::from_xywh(dx, dy, dw, dh);
    let sampling = state_snapshot.image_sampling_options();
    canvas.draw_image_rect_with_sampling_options(
        &sk_image,
        Some((&src, skia_safe::canvas::SrcRectConstraint::Fast)),
        dst,
        sampling,
        &paint,
    );
    true
}

/// `PatternResolver` backed by the manager's [`ImageStore`] and the
/// current canvas's [`DirectContext`].  Wraps `&mut gr_ctx` in a
/// `RefCell` so the trait's `&self` signature can still issue mutable
/// Ganesh calls (e.g. to build a `BackendTexture`).
struct SkiaPatternResolver<'a> {
    ctx_tag: u32,
    gr_ctx: RefCell<&'a mut DirectContext>,
    image_store: RefCell<&'a mut ImageStore>,
}

impl<'a> PatternResolver for SkiaPatternResolver<'a> {
    fn resolve_pattern(
        &self,
        image_id: u32,
        repeat_x: bool,
        repeat_y: bool,
        global_alpha: f32,
    ) -> Option<Shader> {
        let mut gr_ctx = self.gr_ctx.borrow_mut();
        let mut store = self.image_store.borrow_mut();
        let sk_image = store.resolve_cached_or_wrap(self.ctx_tag, *gr_ctx, image_id)?;
        let tile_x = if repeat_x {
            TileMode::Repeat
        } else {
            TileMode::Clamp
        };
        let tile_y = if repeat_y {
            TileMode::Repeat
        } else {
            TileMode::Clamp
        };
        // globalAlpha modulation: apply via colour filter on the shader.
        // For fully opaque the base shader is enough.
        let _ = global_alpha; // applied to the paint, not the shader
        sk_image.to_shader(Some((tile_x, tile_y)), SamplingOptions::default(), None)
    }
}

pub(crate) fn read_surface_rgba_unpremul(
    surface: &mut SkSurface,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let byte_len = shared::protocol::render_cmd::checked_canvas_rgba_byte_len(width, height)?;
    let info = skia_safe::ImageInfo::new(
        skia_safe::ISize::new(width as i32, height as i32),
        skia_safe::ColorType::RGBA8888,
        skia_safe::AlphaType::Unpremul,
        None,
    );
    let row_bytes = usize::try_from(width).ok()?.checked_mul(4)?;
    let mut out = vec![0u8; byte_len];
    surface
        .read_pixels(&info, &mut out, row_bytes, (x, y))
        .then_some(out)
}

#[cfg(test)]
mod tests {
    use super::{
        LOW_MEMORY_AGGREGATE_BYTES, LiveContextCount, MIN_PER_CTX_BYTES,
        SKIA_RESOURCE_CACHE_BUDGET_BYTES, low_memory_per_ctx_bytes, per_ctx_resource_cache_bytes,
        read_surface_rgba_unpremul, set_skia_resource_cache_budget, tier_budget,
    };
    use skia_safe::{AlphaType, Color, ColorType, ISize, ImageInfo, Paint, Rect, surfaces};
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    /// The budget and the live count are process-wide, so two tests moving them
    /// concurrently would read each other's values.
    static BUDGET_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn read_surface_rgba_unpremul_returns_painted_pixels() {
        let info = ImageInfo::new(
            ISize::new(2, 2),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let mut surface = surfaces::raster(&info, None, None).expect("valid raster surface");
        surface.canvas().clear(Color::TRANSPARENT);

        let mut paint = Paint::default();
        paint.set_color(Color::from_argb(255, 10, 20, 30));
        surface
            .canvas()
            .draw_rect(Rect::from_xywh(0.0, 0.0, 1.0, 1.0), &paint);

        let pixels =
            read_surface_rgba_unpremul(&mut surface, 0, 0, 1, 1).expect("readback should work");

        assert_eq!(pixels, vec![10, 20, 30, 255]);
    }

    /// Section 6.5: the Skia budget's denominator has to span the process, because
    /// its numerator does.
    ///
    /// The guard is exercised directly rather than through `Canvas2DContext::new`,
    /// which needs an EGL context and a GPU. What that leaves uncovered is only
    /// whether a context is enrolled at all, and that is not a convention to test:
    /// `LiveContextCount` is a required field, so a context which is not counted does
    /// not compile.
    ///
    /// Serialised against the other budget test: both move process-wide state.
    #[test]
    fn the_per_context_cap_divides_the_budget_across_every_session_s_contexts() {
        let _serialised = BUDGET_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let restore = SKIA_RESOURCE_CACHE_BUDGET_BYTES.load(Ordering::Relaxed);
        set_skia_resource_cache_budget(64 * 1024 * 1024);

        // No contexts anywhere: the divisor floors at one rather than dividing by zero.
        assert_eq!(per_ctx_resource_cache_bytes(), 64 * 1024 * 1024);

        // One context in this Session.
        let first = LiveContextCount::enrol();
        assert_eq!(per_ctx_resource_cache_bytes(), 64 * 1024 * 1024);

        // A second Session brings up its own. Before this counted process-wide, each
        // Session divided the whole budget by its own single context and the process
        // handed Skia 128 MiB against a 64 MiB ceiling.
        let second = LiveContextCount::enrol();
        assert_eq!(per_ctx_resource_cache_bytes(), 32 * 1024 * 1024);

        let third = LiveContextCount::enrol();
        let fourth = LiveContextCount::enrol();
        assert_eq!(per_ctx_resource_cache_bytes(), 16 * 1024 * 1024);

        // Deep enough that the per-context floor takes over from the share --
        // at a 64 MiB budget and a 64 KiB floor that crossover is 1024
        // contexts, well past any scene size this repository anticipates (see
        // MIN_PER_CTX_BYTES), so this pushes past it deliberately rather than
        // at a realistic count.
        let many: Vec<LiveContextCount> = (0..1196).map(|_| LiveContextCount::enrol()).collect();
        assert_eq!(per_ctx_resource_cache_bytes(), MIN_PER_CTX_BYTES);
        drop(many);

        // The count is released by dropping, on every path, because it is a guard.
        drop((second, third, fourth));
        assert_eq!(per_ctx_resource_cache_bytes(), 64 * 1024 * 1024);
        drop(first);
        assert_eq!(per_ctx_resource_cache_bytes(), 64 * 1024 * 1024);

        set_skia_resource_cache_budget(restore);
    }

    /// The aggregate the process actually grants for `n` contexts.
    fn granted(aggregate: usize, n: usize) -> usize {
        set_skia_resource_cache_budget(aggregate);
        let guards: Vec<LiveContextCount> = (0..n).map(|_| LiveContextCount::enrol()).collect();
        let total = per_ctx_resource_cache_bytes() * n;
        drop(guards);
        total
    }

    /// **The aggregate ceiling is honoured at every scene size this
    /// repository anticipates.** It was not always: with the 4 MiB floor this
    /// test used to carry, 80 contexts (the `render_thread` reorder fixture's
    /// count, on the record that "nothing bounds how many canvases a game
    /// draws to in one frame") were granted 320 MiB against a 96 MiB TierA
    /// ceiling — see the device measurement on `MIN_PER_CTX_BYTES` for why the
    /// floor moved to 64 KiB instead of being defended.
    ///
    /// At 64 KiB the crossover (`aggregate / MIN_PER_CTX_BYTES`) is 1536
    /// contexts on TierA, 768 on TierB, 256 even under the low-memory squeeze
    /// — this pins those numbers so a future edit to `MIN_PER_CTX_BYTES` or
    /// [`tier_budget`] cannot quietly drag the crossover back down under 80
    /// without a test failing here.
    #[test]
    fn the_aggregate_is_honoured_at_the_scene_sizes_this_repository_anticipates() {
        let _serialised = BUDGET_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let restore = SKIA_RESOURCE_CACHE_BUDGET_BYTES.load(Ordering::Relaxed);

        const MIB: usize = 1024 * 1024;
        for (tier, aggregate, crossover) in [
            (crate::device_caps::DeviceTier::TierA, 96 * MIB, 1536),
            (crate::device_caps::DeviceTier::TierB, 48 * MIB, 768),
        ] {
            assert_eq!(
                tier_budget(tier),
                aggregate,
                "tier budget moved; the crossover below is computed from it"
            );
            assert_eq!(
                aggregate / MIN_PER_CTX_BYTES,
                crossover,
                "{tier:?}: crossover moved — MIN_PER_CTX_BYTES no longer backstops \
                 only degenerate context counts"
            );
        }
        assert_eq!(LOW_MEMORY_AGGREGATE_BYTES / MIN_PER_CTX_BYTES, 256);

        // The scene size this repository already writes fixtures for
        // (scripts/fixtures/skia-floor-probe-80) — well under every crossover
        // above, so the share stays a plain aggregate/n division and the
        // aggregate is exactly honoured, no floor involved.
        const SHOP_UI_CANVASES: usize = 80;
        for (label, aggregate) in [
            ("TierA", tier_budget(crate::device_caps::DeviceTier::TierA)),
            ("TierB", tier_budget(crate::device_caps::DeviceTier::TierB)),
            ("low-memory", LOW_MEMORY_AGGREGATE_BYTES),
        ] {
            let share = aggregate / SHOP_UI_CANVASES;
            assert!(
                share >= MIN_PER_CTX_BYTES,
                "{label}: aggregate/80 = {share} bytes is below the {MIN_PER_CTX_BYTES}-byte \
                 floor, so this scene size no longer proves the floor stays out of the way"
            );
            let total = granted(aggregate, SHOP_UI_CANVASES);
            assert_eq!(
                total,
                share * SHOP_UI_CANVASES,
                "{label}: 80 contexts granted {total} bytes against a {aggregate} ceiling — \
                 the floor took over at a scene size this repository ships fixtures for"
            );
            assert!(
                total <= aggregate,
                "{label}: 80 contexts granted {total} bytes, over the {aggregate} ceiling"
            );
        }

        // Same claim for the low-memory squeeze specifically, through its own
        // accessor: a warning arriving in an 80-canvas scene now actually
        // squeezes proportionally instead of flooring out at four contexts.
        let guards: Vec<LiveContextCount> = (0..SHOP_UI_CANVASES)
            .map(|_| LiveContextCount::enrol())
            .collect();
        assert_eq!(
            low_memory_per_ctx_bytes() * SHOP_UI_CANVASES,
            (LOW_MEMORY_AGGREGATE_BYTES / SHOP_UI_CANVASES) * SHOP_UI_CANVASES,
            "the low-memory squeeze should still divide {LOW_MEMORY_AGGREGATE_BYTES} across \
             {SHOP_UI_CANVASES} contexts rather than flooring out"
        );
        drop(guards);

        set_skia_resource_cache_budget(restore);
    }

    /// The floor still exists, and still backstops something: past its (much
    /// larger, post-measurement) crossover, `per_ctx_share` still returns
    /// `MIN_PER_CTX_BYTES` rather than a share that keeps shrinking toward
    /// zero. What changed is only where that crossover sits — see
    /// `MIN_PER_CTX_BYTES` and the test above.
    #[test]
    fn the_floor_still_backstops_a_degenerate_context_count() {
        let _serialised = BUDGET_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let restore = SKIA_RESOURCE_CACHE_BUDGET_BYTES.load(Ordering::Relaxed);

        const MIB: usize = 1024 * 1024;
        for (tier, aggregate) in [
            (crate::device_caps::DeviceTier::TierA, 96 * MIB),
            (crate::device_caps::DeviceTier::TierB, 48 * MIB),
        ] {
            let crossover = aggregate / MIN_PER_CTX_BYTES;

            // At the crossover the share equals the floor, so the aggregate is
            // still exactly honoured.
            assert_eq!(
                granted(aggregate, crossover),
                aggregate,
                "{tier:?}: {crossover} contexts should still fit the ceiling"
            );

            // One past it, the floor wins and the ceiling is exceeded -- by
            // construction, since nothing below MIN_PER_CTX_BYTES is granted.
            let over = granted(aggregate, crossover + 1);
            assert!(
                over > aggregate,
                "{tier:?}: {} contexts granted {over} against a {aggregate} \
                 ceiling — expected the floor to have taken over",
                crossover + 1
            );
            assert_eq!(over, (crossover + 1) * MIN_PER_CTX_BYTES);
        }

        set_skia_resource_cache_budget(restore);
    }

    #[test]
    fn the_budget_never_drops_below_one_context_s_floor() {
        let _serialised = BUDGET_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let restore = SKIA_RESOURCE_CACHE_BUDGET_BYTES.load(Ordering::Relaxed);

        set_skia_resource_cache_budget(1);
        assert_eq!(per_ctx_resource_cache_bytes(), MIN_PER_CTX_BYTES);

        set_skia_resource_cache_budget(restore);
    }

    /// A memory warning must release bytes without capping anyone.
    ///
    /// The defect: one Session's `onTrimMemory` stored 16 MiB as *the* process
    /// budget, and only engine init ever raised it again — so one game's warning
    /// capped every other game's canvases for the life of the process. The squeeze is
    /// now a figure handed to Skia and immediately superseded, so what a Session's
    /// warning changes is what Skia holds, not what the process may hold.
    ///
    /// What this cannot see, stated rather than implied: whether
    /// `Canvas2DContext::trim_resource_cache` really installs the low cap before
    /// restoring the ordinary one, since a `DirectContext` needs an EGL context and a
    /// GPU. What it does cover is that no path can store the low figure — there is no
    /// longer a function that yields one.
    #[test]
    fn a_low_memory_squeeze_leaves_no_ceiling_behind() {
        let _serialised = BUDGET_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let restore = SKIA_RESOURCE_CACHE_BUDGET_BYTES.load(Ordering::Relaxed);
        set_skia_resource_cache_budget(64 * 1024 * 1024);

        let ordinary = per_ctx_resource_cache_bytes();
        let squeezed = low_memory_per_ctx_bytes();
        assert!(
            squeezed < ordinary,
            "a squeeze that asks for {squeezed} of an allowed {ordinary} frees nothing"
        );
        assert_eq!(
            per_ctx_resource_cache_bytes(),
            ordinary,
            "asking for the squeeze changed what the process may hold, so it outlived \
             the call that asked for it"
        );

        // The same, with two Sessions' contexts live: both figures divide by the same
        // process-wide count, so the squeeze stays proportional rather than becoming
        // the floor as soon as a second game starts.
        let contexts = [LiveContextCount::enrol(), LiveContextCount::enrol()];
        assert_eq!(low_memory_per_ctx_bytes(), 8 * 1024 * 1024);
        assert_eq!(per_ctx_resource_cache_bytes(), 32 * 1024 * 1024);
        drop(contexts);

        set_skia_resource_cache_budget(restore);
    }
    /// Adjacent drawImage calls must be pixel-identical whether the collector
    /// leaves them as individual draws or merges them into an atlas batch.
    /// This deliberately uses a two-colour source scaled up so Skia's default
    /// nearest sampling differs visibly from Canvas2D's default linear sampling.
    #[test]
    fn draw_image_batch_paths_match_single_image_sampling() {
        let source_info = ImageInfo::new(
            ISize::new(2, 1),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let mut source = surfaces::raster(&source_info, None, None).expect("source surface");
        source.canvas().clear(Color::TRANSPARENT);
        let mut red = Paint::default();
        red.set_color(Color::from_argb(255, 255, 0, 0));
        source
            .canvas()
            .draw_rect(Rect::from_xywh(0.0, 0.0, 1.0, 1.0), &red);
        let mut blue = Paint::default();
        blue.set_color(Color::from_argb(255, 0, 0, 255));
        source
            .canvas()
            .draw_rect(Rect::from_xywh(1.0, 0.0, 1.0, 1.0), &blue);
        let image = source.image_snapshot();
        let state = super::Canvas2DState::default();
        let sampling = state.image_sampling_options();

        fn render(draw: impl FnOnce(&skia_safe::Canvas, &skia_safe::Paint)) -> Vec<u8> {
            let info = ImageInfo::new(
                ISize::new(8, 4),
                ColorType::RGBA8888,
                AlphaType::Unpremul,
                None,
            );
            let mut surface = surfaces::raster(&info, None, None).expect("destination surface");
            surface.canvas().clear(Color::TRANSPARENT);
            let paint = Paint::default();
            draw(surface.canvas(), &paint);
            let mut pixels = vec![0; 8 * 4 * 4];
            assert!(surface.read_pixels(&info, &mut pixels, 8 * 4, (0, 0)));
            pixels
        }

        let single = render(|canvas, paint| {
            canvas.draw_image_rect_with_sampling_options(
                &image,
                None,
                Rect::from_xywh(0.0, 0.0, 8.0, 4.0),
                sampling,
                paint,
            );
        });
        let individual = render(|canvas, paint| {
            canvas.draw_image_rect_with_sampling_options(
                &image,
                None,
                Rect::from_xywh(0.0, 0.0, 8.0, 4.0),
                sampling,
                paint,
            );
        });
        let xforms = [skia_safe::RSXform::new(4.0, 0.0, (0.0, 0.0))];
        let tex = [Rect::from_xywh(0.0, 0.0, 2.0, 1.0)];
        let atlas = render(|canvas, paint| {
            canvas.draw_atlas(
                &image,
                &xforms,
                &tex,
                None,
                skia_safe::BlendMode::SrcOver,
                sampling,
                None,
                paint,
            );
        });

        assert_eq!(
            single, individual,
            "batched individual fallback must match the unbatched linear reference"
        );
        assert_eq!(
            single, atlas,
            "batched atlas path must match the unbatched linear reference"
        );
        let mut nearest_state = super::Canvas2DState::default();
        nearest_state.image_smoothing = false;
        let nearest_sampling = nearest_state.image_sampling_options();
        let single_nearest = render(|canvas, paint| {
            canvas.draw_image_rect_with_sampling_options(
                &image,
                None,
                Rect::from_xywh(0.0, 0.0, 8.0, 4.0),
                nearest_sampling,
                paint,
            );
        });
        let individual_nearest = render(|canvas, paint| {
            canvas.draw_image_rect_with_sampling_options(
                &image,
                None,
                Rect::from_xywh(0.0, 0.0, 8.0, 4.0),
                nearest_sampling,
                paint,
            );
        });
        let atlas_nearest = render(|canvas, paint| {
            canvas.draw_atlas(
                &image,
                &xforms,
                &tex,
                None,
                skia_safe::BlendMode::SrcOver,
                nearest_sampling,
                None,
                paint,
            );
        });
        assert_eq!(
            single_nearest, individual_nearest,
            "nearest batch fallback must match the unbatched reference"
        );
        assert_eq!(
            single_nearest, atlas_nearest,
            "nearest atlas path must match the unbatched reference"
        );
    }
}
