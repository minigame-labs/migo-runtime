extern crate khronos_egl as egl;

use egl::EGL1_4;
use shared::error::{EngineResult, ErrorCode};

use crate::egl_platform::{EglInstance, EglProvider};

use super::types::ee;

/// Terminates an initialized display if configuration/probing returns early.
/// The guard borrows the instance so it cannot outlive the exact dispatch
/// table that initialized the display.
struct InitializedDisplayGuard<'a> {
    egl: &'a EglInstance,
    display: egl::Display,
    armed: bool,
}

impl<'a> InitializedDisplayGuard<'a> {
    fn new(egl: &'a EglInstance, display: egl::Display) -> Self {
        Self {
            egl,
            display,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InitializedDisplayGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.egl.terminate(self.display);
        }
    }
}

/// Owns one initialized EGLDisplay and the resource context that roots its
/// share group.  It is installed in CanvasManager immediately after init so
/// every constructor error and panic has the same best-effort EGL fallback.
pub(super) struct EglRuntime {
    instance: EglInstance,
    display: egl::Display,
    resource: Option<(egl::Context, Option<egl::Surface>)>,
    initialized: bool,
    termination_confirmed: bool,
    /// See [`SurfaceLedger`]. `RefCell` and not a lock: an `EglRuntime` belongs
    /// to the canvas manager, which is owned by one thread, and EGL would reject
    /// a second thread making its contexts current anyway.
    #[cfg(debug_assertions)]
    ledger: std::cell::RefCell<SurfaceLedger>,
}

impl EglRuntime {
    fn new(instance: EglInstance, display: egl::Display) -> Self {
        Self {
            instance,
            display,
            resource: None,
            initialized: true,
            termination_confirmed: false,
            #[cfg(debug_assertions)]
            ledger: std::cell::RefCell::new(SurfaceLedger::default()),
        }
    }

    // ---------------------------------------------------------------------
    // The three EGL calls that move surface ownership.
    //
    // These shadow the `Deref` to the instance rather than being called
    // through it, so every existing `self.egl.make_current(..)` is ledgered
    // without a call site changing. That is deliberate: a bookkeeping scheme
    // somebody has to remember to call is a scheme that is right until the
    // next call site.
    // ---------------------------------------------------------------------

    pub(super) fn make_current(
        &self,
        display: egl::Display,
        draw: Option<egl::Surface>,
        read: Option<egl::Surface>,
        context: Option<egl::Context>,
    ) -> Result<(), egl::Error> {
        let result = self.instance.make_current(display, draw, read, context);
        // Recorded only on success. A rejected eglMakeCurrent changes nothing
        // inside ANGLE, and recording the intent would make the ledger diverge
        // in exactly the situation -- a surface the driver will not accept --
        // where it is about to be read.
        #[cfg(debug_assertions)]
        if result.is_ok() {
            self.ledger.borrow_mut().made_current(
                draw.map(|s| s.as_ptr() as usize),
                read.map(|s| s.as_ptr() as usize),
                context.map(|c| c.as_ptr() as usize),
            );
        }
        result
    }

    pub(super) fn destroy_surface(
        &self,
        display: egl::Display,
        surface: egl::Surface,
    ) -> Result<(), egl::Error> {
        #[cfg(debug_assertions)]
        {
            let held = self.ledger.borrow().references(surface.as_ptr() as usize);
            if held > 0 {
                tracing::error!(
                    surface = format_args!("{:p}", surface.as_ptr()),
                    references = held,
                    "eglDestroySurface while the surface is still current: ANGLE defers the \
                     destroy above a zero reference count, so this call returns success and \
                     the native window stays owned until eglTerminate"
                );
            }
        }
        let result = self.instance.destroy_surface(display, surface);
        #[cfg(debug_assertions)]
        if result.is_ok() {
            self.ledger
                .borrow_mut()
                .surface_destroyed(surface.as_ptr() as usize);
        }
        result
    }

    pub(super) fn destroy_context(
        &self,
        display: egl::Display,
        context: egl::Context,
    ) -> Result<(), egl::Error> {
        let result = self.instance.destroy_context(display, context);
        #[cfg(debug_assertions)]
        if result.is_ok() {
            self.ledger
                .borrow_mut()
                .context_destroyed(context.as_ptr() as usize);
        }
        result
    }

    pub(super) fn track_resource(&mut self, context: egl::Context, surface: Option<egl::Surface>) {
        assert!(
            self.resource.is_none(),
            "EGL resource owner cannot track two share-group roots"
        );
        self.resource = Some((context, surface));
    }

    pub(super) fn untrack_resource(&mut self) -> Option<(egl::Context, Option<egl::Surface>)> {
        self.resource.take()
    }

    /// Idempotent final display teardown. Callers must release every window
    /// and offscreen surface first; this method owns only the root pbuffer and
    /// the final `eglTerminate` authority.
    ///
    /// Returns `true` only when the driver confirms `eglTerminate`. A caller
    /// may independently prove that a native window is releasable by having
    /// successfully destroyed every EGLSurface that referenced it, but must
    /// not infer that proof from this runtime merely becoming disarmed.
    #[must_use]
    pub(super) fn shutdown(&mut self) -> bool {
        if !self.initialized {
            return self.termination_confirmed;
        }

        // Disarm before calling driver code, and isolate each wrapper call.
        // khronos-egl normally returns Result, but a non-conforming driver that
        // reports EGL_FALSE without an EGL error can still trigger an internal
        // unwrap. Drop must never double-panic during render-thread unwinding.
        self.initialized = false;
        let resource = self.resource.take();
        let instance = &self.instance;
        let display = self.display;
        let ignore_driver_panic = |operation: &mut dyn FnMut()| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
        };

        ignore_driver_panic(&mut || {
            let _ = instance.make_current(display, None, None, None);
        });
        if let Some((context, surface)) = resource {
            if let Some(surface) = surface {
                ignore_driver_panic(&mut || {
                    let _ = instance.destroy_surface(display, surface);
                });
            }
            ignore_driver_panic(&mut || {
                let _ = instance.destroy_context(display, context);
            });
        }
        let termination =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| instance.terminate(display)));
        self.termination_confirmed = matches!(termination, Ok(Ok(())));
        if !self.termination_confirmed {
            match termination {
                Ok(Err(error)) => tracing::error!(
                    ?error,
                    "eglTerminate failed; native Surface ownership cannot be released"
                ),
                Err(_) => tracing::error!(
                    "eglTerminate panicked; native Surface ownership cannot be released"
                ),
                Ok(Ok(())) => unreachable!(),
            }
        }
        self.termination_confirmed
    }
}

impl std::ops::Deref for EglRuntime {
    type Target = EglInstance;

    fn deref(&self) -> &Self::Target {
        &self.instance
    }
}

impl Drop for EglRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Destroys a newly-created context if pbuffer creation (or unwinding between
/// the two EGL calls) does not complete the pair.
struct ContextCleanupGuard<'a> {
    egl: &'a EglInstance,
    display: egl::Display,
    context: Option<egl::Context>,
}

impl<'a> ContextCleanupGuard<'a> {
    fn new(egl: &'a EglInstance, display: egl::Display, context: egl::Context) -> Self {
        Self {
            egl,
            display,
            context: Some(context),
        }
    }

    fn disarm(mut self) -> egl::Context {
        self.context
            .take()
            .expect("pbuffer context cleanup guard must be armed")
    }
}

impl Drop for ContextCleanupGuard<'_> {
    fn drop(&mut self) {
        if let Some(context) = self.context.take() {
            let _ = self.egl.destroy_context(self.display, context);
        }
    }
}

// ---- EGL_EXT_create_context_robustness ----
//
// Lets Migo request a GL ES context that the driver observes
// periodically for resets.  Paired with `GL_KHR_robustness` it
// turns a slow "invisible corruption → caught only at next
// eglSwapBuffers" failure into "every GL call starts returning
// GL_CONTEXT_LOST and glGetGraphicsResetStatus reports the
// reason" — giving the render thread a chance to tear down and
// rebuild before the user sees a black frame.  See wgpu-hal's
// GLES backend for the same pattern
// (`wgpu-hal/src/gles/egl.rs:511-567`).
const EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT: egl::Int = 0x3138;
const EGL_LOSE_CONTEXT_ON_RESET_EXT: egl::Int = 0x31BF;

/// EGL initialization result
pub(super) struct EglInitResult {
    pub egl: EglRuntime,
    pub display: egl::Display,
    pub config: egl::Config,
    /// Whether the share group's offscreen contexts have no surface at all.
    ///
    /// False everywhere a config supports both window and pbuffer surfaces,
    /// which is every platform that shipped before Wayland. True only on a
    /// driver that publishes no pbuffer config -- Mesa's Wayland platform
    /// publishes thirty configs and not one of them is pbuffer-capable -- where
    /// the offscreen contexts are made current against EGL_NO_SURFACE instead.
    pub surfaceless: bool,
    /// The GLES version actually negotiated (3 = ES 3.0+, 2 = ES 2.0 fallback).
    pub gles_major: u32,
    /// Whether the driver supports `EGL_EXT_create_context_robustness` so
    /// subsequent `create_context` calls can add the reset-notification
    /// strategy attribute (R-3).  Cached here rather than re-queried
    /// per context to avoid the extra `eglQueryString` on every canvas
    /// create.
    pub has_robust_context: bool,
}

/// Initialize EGL exclusively through the platform-selected provider.
pub(super) fn init_egl(provider: &dyn EglProvider) -> EngineResult<EglInitResult> {
    let egl = provider.load()?;
    let display = provider.display(&egl)?;

    egl.initialize(display).map_err(|_| {
        ee(
            ErrorCode::RenderInitializeError,
            format!(
                "initialize failed: 0x{:x}",
                egl.get_error().map(|e| e as u32).unwrap_or(0)
            ),
        )
    })?;
    let mut initialized_display = InitializedDisplayGuard::new(&egl, display);

    // EGL_OPENGL_ES3_BIT_KHR — request configs that support ES 3.0.
    // Defined by EGL_KHR_create_context, widely available on Android 5.0+.
    const OPENGL_ES3_BIT: egl::Int = 0x0040;

    // Try ES 3.0-capable configs first, then fall back to ES 2.0.
    // Within each GLES level, try depth/stencil variants in preference order.
    struct ConfigCandidate {
        depth: egl::Int,
        stencil: egl::Int,
        renderable: egl::Int,
        gles_major: u32,
    }

    let candidates = [
        // ES 3.0 — primary: D16+S8
        ConfigCandidate {
            depth: 16,
            stencil: 8,
            renderable: OPENGL_ES3_BIT,
            gles_major: 3,
        },
        // ES 3.0 — fallback: D24+S8 (some drivers only offer D24S8 packed)
        ConfigCandidate {
            depth: 24,
            stencil: 8,
            renderable: OPENGL_ES3_BIT,
            gles_major: 3,
        },
        // ES 3.0 — fallback: D16 no stencil
        ConfigCandidate {
            depth: 16,
            stencil: 0,
            renderable: OPENGL_ES3_BIT,
            gles_major: 3,
        },
        // ES 2.0 — primary: D16+S8
        ConfigCandidate {
            depth: 16,
            stencil: 8,
            renderable: egl::OPENGL_ES2_BIT,
            gles_major: 2,
        },
        // ES 2.0 — fallback: D24+S8
        ConfigCandidate {
            depth: 24,
            stencil: 8,
            renderable: egl::OPENGL_ES2_BIT,
            gles_major: 2,
        },
        // ES 2.0 — fallback: D16 no stencil
        ConfigCandidate {
            depth: 16,
            stencil: 0,
            renderable: egl::OPENGL_ES2_BIT,
            gles_major: 2,
        },
    ];

    let pick = |surface_type: egl::Int, c: &ConfigCandidate| {
        let attrs = [
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::DEPTH_SIZE,
            c.depth,
            egl::STENCIL_SIZE,
            c.stencil,
            egl::SURFACE_TYPE,
            surface_type,
            egl::RENDERABLE_TYPE,
            c.renderable,
            egl::NONE,
        ];
        egl.choose_first_config(display, &attrs).ok().flatten()
    };

    // A config that serves both surface types is still asked for first, and
    // found on every platform that shipped before Wayland -- so those keep
    // selecting exactly the config they selected before this fallback existed.
    let mut config = None;
    let mut gles_major = 2u32;
    let mut surfaceless = false;
    for c in &candidates {
        if let Some(cfg) = pick(egl::WINDOW_BIT | egl::PBUFFER_BIT, c) {
            config = Some(cfg);
            gles_major = c.gles_major;
            break;
        }
    }

    if config.is_none() {
        // No pbuffer config anywhere. The offscreen contexts can still exist
        // without a surface, but only if the driver says so -- making a context
        // current against EGL_NO_SURFACE without the extension is an error, and
        // a clear refusal here beats one at the first canvas.
        let extensions = egl
            .query_string(Some(display), egl::EXTENSIONS)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if extensions.contains("EGL_KHR_surfaceless_context") {
            for c in &candidates {
                if let Some(cfg) = pick(egl::WINDOW_BIT, c) {
                    tracing::info!(
                        "no pbuffer-capable EGL config; offscreen contexts will be surfaceless"
                    );
                    config = Some(cfg);
                    gles_major = c.gles_major;
                    surfaceless = true;
                    break;
                }
            }
        }
    }
    let config = config.ok_or_else(|| {
        // Reached when no candidate offers a window config at all, or offers
        // one without a pbuffer while the driver also lacks
        // EGL_KHR_surfaceless_context.
        //
        // Historical note, because the shape of the fallback above is otherwise
        // hard to justify: on Wayland every candidate above asks for
        // WINDOW_BIT | PBUFFER_BIT, and Mesa's Wayland platform offers no
        // pbuffer config at all. Enumerated against a live compositor: 30
        // configs, 30 window-capable, 0 pbuffer-capable.
        //
        // Two fixes were tried against that fact and neither works:
        //
        //  * Relaxing the mask to WINDOW_BIT gets a window and then breaks the
        //    resource context, the upload thread and every offscreen canvas,
        //    which build their surfaces from this same config. Offscreen
        //    canvases are not optional -- bunnymark makes its sprite textures
        //    with `createCanvas`.
        //  * Choosing two configs, one window and one pbuffer, cannot help
        //    either: with zero pbuffer configs published there is no second one
        //    to choose. That fallback would be dead code on the only platform
        //    it was written for.
        //
        // The fix, implemented above, is to stop needing a pbuffer:
        // `EGL_KHR_surfaceless_context` lets the resource, upload and offscreen
        // contexts be made current against EGL_NO_SURFACE. Everything they draw
        // already goes to an FBO, so the pbuffer was never a render target --
        // only something for eglMakeCurrent to accept.
        ee(
            ErrorCode::RenderChooseConfigError,
            "all EGL config candidates failed",
        )
    })?;

    tracing::info!("EGL config selected: GLES {gles_major}.0");

    // Probe `EGL_EXT_create_context_robustness` once at init.  The
    // robustness context attribute is harmless on drivers that
    // advertise the extension but a hard-error on those that
    // don't — keep the toggle here so `create_pbuffer_context` /
    // `create_onscreen` can decide whether to include it.
    let extensions = egl
        .query_string(Some(display), egl::EXTENSIONS)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let has_robust_context = extensions.contains("EGL_EXT_create_context_robustness");
    if has_robust_context {
        tracing::info!(
            "EGL_EXT_create_context_robustness available; GL reset notifications enabled"
        );
    }

    initialized_display.disarm();
    drop(initialized_display);

    Ok(EglInitResult {
        egl: EglRuntime::new(egl, display),
        display,
        config,
        surfaceless,
        gles_major,
        has_robust_context,
    })
}

/// Build the `eglCreateContext` attribute list for a GLES
/// context, optionally appending the robustness / reset
/// notification attributes when the driver supports them (R-3).
pub(super) fn build_ctx_attribs(gles_major: u32, has_robust_context: bool) -> Vec<egl::Int> {
    let mut attribs: Vec<egl::Int> = vec![
        egl::CONTEXT_CLIENT_VERSION as egl::Int,
        gles_major as egl::Int,
    ];
    if has_robust_context {
        // Asking for LoseContextOnReset means the driver will
        // transition the context to LOST status on a reset, and
        // `glGetGraphicsResetStatus` returns a non-zero reason
        // that the render loop can act on.  An implementation
        // that doesn't support robustness at all would have
        // failed the `has_robust_context` probe; drivers that
        // advertise it but don't honour the strategy simply
        // ignore the attribute, which is the desired fallback
        // behaviour.
        attribs.push(EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT);
        attribs.push(EGL_LOSE_CONTEXT_ON_RESET_EXT);
    }
    attribs.push(egl::NONE as egl::Int);
    attribs
}

/// Create a pbuffer context with optional share context.
///
/// `gles_major` should match the version negotiated by `init_egl` so all
/// contexts in the share group use the same GLES level.
pub(super) fn create_pbuffer_context(
    egl: &egl::DynamicInstance<EGL1_4>,
    display: egl::Display,
    config: egl::Config,
    share: Option<egl::Context>,
    width: u32,
    height: u32,
    gles_major: u32,
    has_robust_context: bool,
    surfaceless: bool,
) -> EngineResult<(egl::Context, Option<egl::Surface>)> {
    let ctx_attribs = build_ctx_attribs(gles_major, has_robust_context);

    egl.bind_api(egl::OPENGL_ES_API).map_err(|e| {
        ee(
            ErrorCode::RenderBackendError,
            format!("eglBindAPI failed: {e:?}"),
        )
    })?;

    let ctx = egl
        .create_context(display, config, share, &ctx_attribs)
        .map_err(|e| {
            ee(
                ErrorCode::RenderBackendError,
                format!("eglCreateContext failed: {e:?}"),
            )
        })?;
    let context_cleanup = ContextCleanupGuard::new(egl, display, ctx);

    // Surfaceless means exactly that: no pbuffer is created, and the caller
    // makes this context current against EGL_NO_SURFACE. Everything drawn
    // through it already goes to an FBO, so the pbuffer was never a render
    // target -- only something for eglMakeCurrent to accept.
    let surf = if surfaceless {
        None
    } else {
        let pbuf_attribs = [
            egl::WIDTH as i32,
            width as i32,
            egl::HEIGHT as i32,
            height as i32,
            egl::NONE as i32,
        ];
        Some(
            egl.create_pbuffer_surface(display, config, &pbuf_attribs)
                .map_err(|e| {
                    ee(
                        ErrorCode::RenderBackendError,
                        format!("eglCreatePbufferSurface failed: {e:?}"),
                    )
                })?,
        )
    };
    let ctx = context_cleanup.disarm();

    Ok((ctx, surf))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use crate::egl_platform::{EglConcurrency, EglInstance, EglProvider, GraphicsBackendId};
    use shared::error::{EngineError, EngineResult, ErrorCode};

    use super::init_egl;

    const EGL_OPS_SOURCE: &str = include_str!("egl_ops.rs");

    #[derive(Debug)]
    struct InjectedFailureProvider {
        loads: Arc<AtomicUsize>,
    }

    impl EglProvider for InjectedFailureProvider {
        fn backend_id(&self) -> GraphicsBackendId {
            GraphicsBackendId::of::<Self>()
        }

        fn concurrency(&self) -> EglConcurrency {
            EglConcurrency::SharedContexts
        }

        fn platform_identity(&self) -> crate::egl_platform::PlatformIdentity {
            crate::egl_platform::PlatformIdentity::new::<Self>(self.backend_id(), 0)
        }

        fn label(&self) -> &str {
            "sentinel-injected-egl"
        }

        fn load(&self) -> EngineResult<EglInstance> {
            self.loads.fetch_add(1, Ordering::Relaxed);
            Err(EngineError::new(ErrorCode::RenderInitializeError)
                .with_msg("sentinel provider load failure"))
        }

        fn display(&self, _egl: &EglInstance) -> EngineResult<khronos_egl::Display> {
            panic!("display must not be requested after provider load failure")
        }
    }

    #[test]
    fn injected_egl_provider_is_the_only_loader_used_by_init() {
        let loads = Arc::new(AtomicUsize::new(0));
        let provider = InjectedFailureProvider {
            loads: Arc::clone(&loads),
        };

        let error = init_egl(&provider).err().expect("provider must fail");
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert_eq!(error.msg, "sentinel provider load failure");
    }

    #[test]
    fn initialized_display_and_partial_pbuffer_creation_have_raii_guards() {
        assert!(EGL_OPS_SOURCE.contains("struct InitializedDisplayGuard"));
        assert!(EGL_OPS_SOURCE.contains("struct ContextCleanupGuard"));

        let init = EGL_OPS_SOURCE
            .split("pub(super) fn init_egl")
            .nth(1)
            .expect("init_egl must exist");
        assert!(init.contains("InitializedDisplayGuard::new"));

        let pbuffer = EGL_OPS_SOURCE
            .split("pub(super) fn create_pbuffer_context")
            .nth(1)
            .expect("create_pbuffer_context must exist");
        assert!(pbuffer.contains("ContextCleanupGuard::new"));
    }
}

/// A mirror of ANGLE's surface-reference rule, kept on our side of the call.
///
/// WHY IT EXISTS. `eglDestroySurface` on a surface that is still current does
/// not destroy it. ANGLE's `Surface::onDestroy` sets `mDestroyed` and returns
/// success while `mRefCount > 0`, and the surface -- on Apple, together with the
/// `CAMetalLayer` it retains -- then lives until `eglTerminate`. There is no
/// error, no log and no return value that says so: a retirement announces the
/// native window released while ANGLE is still holding it. That defect was found
/// from the far end, as a layer outliving the retirement that announced it, and
/// cost a session of bisection to get back to the surface.
///
/// WHAT IT MIRRORS. ANGLE's own rule, read at the revision this repository pins
/// (`contracts/artifact-manifest/apple-angle.lock.json`, `52f59428`):
///
///   * `Context::setDefaultFramebuffer(draw, read)` calls `draw->makeCurrent`,
///     and `read->makeCurrent` only when `read != draw`. Each is one `addRef`.
///   * `Context::unsetDefaultFramebuffer` is the exact inverse, and
///     `Context::makeCurrent` runs it *before* binding the new surfaces -- so
///     re-binding a context balances itself.
///   * `Display::makeCurrent` unsets the previous context's surfaces only when
///     the context changes, which is what makes the previous rule necessary.
///   * `Surface::onDestroy` destroys only at `mRefCount == 0`.
///
/// So a surface's count is the number of (context, slot) bindings naming it, and
/// the question a retirement has to answer -- did we leave one behind? -- is
/// answerable here, at the call, instead of inferred later from a live layer.
///
/// SCOPE. One ledger per `EglRuntime`, and an `EglRuntime` is used from the
/// thread that owns the canvas manager. The upload thread loads its own EGL
/// instance and binds only its own pbuffer, so it cannot name a surface this
/// ledger tracks; nothing else calls `eglMakeCurrent` on this display.
///
/// COST. Debug builds only. `make_current` runs a few hundred times a frame on a
/// busy scene -- a map lookup per call is not a price a shipping build should pay
/// for a check whose job is to establish an invariant, not to guard one.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
#[derive(Default)]
pub(super) struct SurfaceLedger {
    /// The context current on this thread and what it has bound.
    ///
    /// One entry and not a map of contexts: EGL allows one current context per
    /// thread, and ANGLE releases the outgoing context's surfaces on every
    /// switch, so a context that is not current holds nothing. The first
    /// version of this did keep a map, and the branch that would have used a
    /// second entry turned out to be unreachable -- a model that cannot be
    /// wrong because it cannot be exercised. This one can.
    current: Option<Binding>,
    /// How many bindings name each surface. Entries at zero are removed, so
    /// `counts.get(s)` answering `None` and answering `Some(0)` are the same
    /// statement and only one of them can be written.
    counts: std::collections::HashMap<usize, u32>,
}

#[cfg_attr(not(debug_assertions), allow(dead_code))]
struct Binding {
    context: usize,
    draw: Option<usize>,
    read: Option<usize>,
}

impl SurfaceLedger {
    /// Record an `eglMakeCurrent` that the driver accepted.
    ///
    /// Called after the call succeeds, never before: a rejected
    /// `eglMakeCurrent` changes nothing in ANGLE, and recording the intent
    /// would make this ledger diverge in exactly the situation -- a failing
    /// surface -- where it is being read.
    pub(super) fn made_current(
        &mut self,
        draw: Option<usize>,
        read: Option<usize>,
        context: Option<usize>,
    ) {
        // The outgoing context loses its surfaces either way: when the context
        // changes `Display::makeCurrent` unsets it, and when it does not
        // `Context::makeCurrent` unsets the same context before rebinding it.
        // One release covers both.
        self.release_current();
        let Some(context) = context else {
            return;
        };
        if let Some(draw) = draw {
            self.retain(draw);
        }
        // ANGLE takes the read surface only when it differs from the draw
        // surface, and an onscreen canvas passes the same surface for both.
        if read != draw
            && let Some(read) = read
        {
            self.retain(read);
        }
        self.current = Some(Binding {
            context,
            draw,
            read,
        });
    }

    /// Record an `eglDestroyContext`. A destroyed context releases what it held.
    ///
    /// Only the current one can be holding anything, and destroying some other
    /// context has to leave the current binding alone -- a retirement destroys
    /// the context it preserved from the *previous* cycle while the live one is
    /// current, and releasing on that call would report the live surface as
    /// free.
    pub(super) fn context_destroyed(&mut self, context: usize) {
        if self.current.as_ref().is_some_and(|b| b.context == context) {
            self.release_current();
        }
    }

    /// How many bindings still name `surface`.
    ///
    /// Non-zero at `eglDestroySurface` means ANGLE will defer the destroy, and
    /// the native window behind it stays owned.
    pub(super) fn references(&self, surface: usize) -> u32 {
        self.counts.get(&surface).copied().unwrap_or(0)
    }

    /// Forget a surface after the driver has destroyed it, so a handle the
    /// allocator reuses does not inherit the old surface's history.
    pub(super) fn surface_destroyed(&mut self, surface: usize) {
        self.counts.remove(&surface);
    }

    /// The exact inverse of what `made_current` retained, which is the exact
    /// inverse ANGLE's `unsetDefaultFramebuffer` performs.
    fn release_current(&mut self) {
        let Some(binding) = self.current.take() else {
            return;
        };
        if let Some(draw) = binding.draw {
            self.release(draw);
        }
        if binding.read != binding.draw
            && let Some(read) = binding.read
        {
            self.release(read);
        }
    }

    fn retain(&mut self, surface: usize) {
        *self.counts.entry(surface).or_insert(0) += 1;
    }

    fn release(&mut self, surface: usize) {
        if let Some(count) = self.counts.get_mut(&surface) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.counts.remove(&surface);
            }
        }
    }
}

#[cfg(test)]
mod surface_ledger_tests {
    use super::SurfaceLedger;

    const CTX_A: usize = 0xA;
    const CTX_B: usize = 0xB;
    const WINDOW: usize = 0x100;
    const PBUFFER: usize = 0x200;

    #[test]
    fn a_bound_surface_is_referenced_and_switching_away_releases_it() {
        let mut ledger = SurfaceLedger::default();
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_A));
        assert_eq!(ledger.references(WINDOW), 1);

        // This is the sequence a retirement runs: switch to the resource
        // context, then destroy the window surface.
        ledger.made_current(Some(PBUFFER), Some(PBUFFER), Some(CTX_B));
        assert_eq!(
            ledger.references(WINDOW),
            0,
            "switching the context away has to release the window surface, or a \
             retirement that is correct would be reported as a leak"
        );
        assert_eq!(ledger.references(PBUFFER), 1);
    }

    #[test]
    fn rebinding_the_same_context_does_not_accumulate() {
        // The failure this pins: if a redundant eglMakeCurrent added a reference
        // without releasing the old one, a frame loop would push the count up
        // without bound and every destroy would look deferred.
        let mut ledger = SurfaceLedger::default();
        for _ in 0..64 {
            ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_A));
        }
        assert_eq!(ledger.references(WINDOW), 1);
    }

    #[test]
    fn separate_read_and_draw_surfaces_are_counted_separately() {
        let mut ledger = SurfaceLedger::default();
        ledger.made_current(Some(WINDOW), Some(PBUFFER), Some(CTX_A));
        assert_eq!(ledger.references(WINDOW), 1);
        assert_eq!(ledger.references(PBUFFER), 1);
        ledger.made_current(None, None, None);
        assert_eq!(ledger.references(WINDOW), 0);
        assert_eq!(ledger.references(PBUFFER), 0);
    }

    #[test]
    fn one_surface_in_both_slots_is_one_reference() {
        // ANGLE's setDefaultFramebuffer takes the read surface only when it
        // differs from the draw surface. Counting it twice would report a
        // correct retirement as a leak on every onscreen canvas we have.
        let mut ledger = SurfaceLedger::default();
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_A));
        assert_eq!(ledger.references(WINDOW), 1);
    }

    #[test]
    fn a_second_context_taking_the_surface_does_not_double_count() {
        let mut ledger = SurfaceLedger::default();
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_A));
        // A second context binding the same surface: legal, and the case where
        // switching only the outgoing context away is not enough.
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_B));
        assert_eq!(
            ledger.references(WINDOW),
            1,
            "CTX_A gave up the surface when the context changed"
        );
        ledger.made_current(Some(PBUFFER), Some(PBUFFER), Some(CTX_B));
        assert_eq!(ledger.references(WINDOW), 0);
    }

    #[test]
    fn a_preserved_context_keeps_holding_what_it_bound() {
        // The shape the Apple defect is suspected to have: the retirement
        // preserves the onscreen context instead of destroying it. If the
        // preserved context were still current, the surface would not be
        // released -- and this is the assertion that would catch it.
        let mut ledger = SurfaceLedger::default();
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_A));
        // No switch away: destroy is attempted with CTX_A still current.
        assert_eq!(ledger.references(WINDOW), 1);
        // Destroying the context is the other way to let go.
        ledger.context_destroyed(CTX_A);
        assert_eq!(ledger.references(WINDOW), 0);
    }

    #[test]
    fn destroying_a_context_that_is_not_current_leaves_the_live_binding_alone() {
        // The retirement path destroys the context it preserved from the
        // previous cycle while the live context is current. Releasing on that
        // call would report the live surface as free and hide a real leak.
        let mut ledger = SurfaceLedger::default();
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_A));
        ledger.context_destroyed(CTX_B);
        assert_eq!(ledger.references(WINDOW), 1);
    }

    #[test]
    fn a_reused_surface_handle_does_not_inherit_the_old_count() {
        let mut ledger = SurfaceLedger::default();
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_A));
        ledger.surface_destroyed(WINDOW);
        assert_eq!(ledger.references(WINDOW), 0);
        // The allocator hands the same address back for a new surface.
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_B));
        assert_eq!(ledger.references(WINDOW), 1);
    }

    #[test]
    fn releasing_the_current_context_releases_its_surfaces() {
        let mut ledger = SurfaceLedger::default();
        ledger.made_current(Some(WINDOW), Some(WINDOW), Some(CTX_A));
        ledger.made_current(None, None, None);
        assert_eq!(ledger.references(WINDOW), 0);
    }
}

#[cfg(test)]
mod surface_ledger_interposition {
    /// The ledger only sees what goes through `EglRuntime`'s own methods.
    ///
    /// `EglRuntime` derefs to the EGL instance, so deleting one of those methods
    /// leaves every call site compiling and calling the driver directly -- and
    /// the ledger simply goes quiet. A ledger that reports nothing and a ledger
    /// that is not being called are the same output, which is the failure this
    /// project has already been bitten by often enough to name: a guard that
    /// covers only the side it was designed for.
    const SOURCE: &str = include_str!("egl_ops.rs");

    fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} is not in this file"));
        let rest = &source[start..];
        let open = rest.find('{').expect("a function has a body");
        let mut depth = 0usize;
        for (index, byte) in rest.as_bytes().iter().enumerate().skip(open) {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &rest[open..=index];
                    }
                }
                _ => {}
            }
        }
        panic!("{signature} has an unbalanced body");
    }

    #[test]
    fn every_ownership_moving_egl_call_is_ledgered() {
        // Named per method rather than as "mentions the ledger". The weaker
        // version passed a mutation that removed the only thing this whole
        // facility is for -- destroy_surface still wrote to the ledger while no
        // longer reading it, so the surface it was about to leak went
        // unreported. A guard that covers only the side it was designed for is
        // this project's most repeated defect; this one names both sides.
        for (signature, required) in [
            (
                "pub(super) fn make_current(",
                &[
                    "self.instance.make_current",
                    "made_current(",
                    "result.is_ok()",
                ][..],
            ),
            (
                "pub(super) fn destroy_surface(",
                &[
                    "self.instance.destroy_surface",
                    "references(",
                    "tracing::error!",
                    "surface_destroyed(",
                ][..],
            ),
            (
                "pub(super) fn destroy_context(",
                &["self.instance.destroy_context", "context_destroyed("][..],
            ),
        ] {
            let body = function_body(SOURCE, signature);
            for fragment in required {
                assert!(
                    body.contains(fragment),
                    "{signature} no longer contains `{fragment}`, so it forwards to EGL \
                     without the bookkeeping that makes a deferred destroy visible"
                );
            }
        }
    }

    #[test]
    fn a_surface_still_referenced_is_reported_before_it_is_destroyed() {
        // Order matters: ANGLE's eglDestroySurface returns success either way,
        // so asking afterwards would ask about a count the call has already
        // changed and report nothing.
        //
        // What this cannot see is reachability: a check compiled out behind a
        // constant false still reads as present and in order. Presence and
        // order are what a source guard can hold, and they are what the drift
        // it is aimed at -- someone tidying the bookkeeping into one block
        // after the call -- actually violates.
        let body = function_body(SOURCE, "pub(super) fn destroy_surface(");
        let ask = body.find("references(").expect("asks the ledger");
        let report = body.find("tracing::error!").expect("reports");
        let destroy = body
            .find("self.instance.destroy_surface")
            .expect("forwards to the driver");
        assert!(
            ask < report && report < destroy,
            "the count has to be read and reported before the driver call, not after"
        );
    }

    #[test]
    fn the_ledger_is_recorded_after_the_driver_agrees() {
        // Recording the intent rather than the outcome is the version of this
        // that is wrong exactly when it is being read: a surface the driver
        // refuses is the interesting one.
        let body = function_body(SOURCE, "pub(super) fn make_current(");
        let call = body.find("self.instance.make_current").expect("forwards");
        let record = body.find("self.ledger").expect("records");
        assert!(
            call < record,
            "the ledger has to be written after the driver call, from its result"
        );
        assert!(
            body.contains("result.is_ok()"),
            "a rejected eglMakeCurrent must not be recorded as a binding"
        );
    }
}
