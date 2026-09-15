//! Apple ANGLE presenter boundary, shared by macOS and iOS.
//!
//! Mirrors `platform/windows/presenter.rs`: a headless pbuffer path plus a
//! host-owned layer target, both injected through the same contract
//! (`EglProvider` / `EglSurfaceFactory` / `GraphicsPlatform`). One module for
//! both Apple platforms because the only thing that differs between them is the
//! name of the file ANGLE ships in, and that difference is four lines.
//!
//! # Why ANGLE rather than a system GL
//!
//! There is no GL framework on iOS. That is measured, not assumed:
//! `.github/workflows/apple-sdk.yml` asks rustc for the link line each Apple
//! slice needs, and macOS answers `-framework OpenGL` -- the legacy desktop GL
//! framework -- while iOS answers nothing at all. Skia is configured for its GL
//! backend, so on iOS there is nothing underneath it. ANGLE over Metal is what
//! fills that gap, and it is why "use the system GL for a macOS presenter
//! first" buys iOS nothing: it links a thing iOS does not have.
//!
//! # Which ANGLE backend
//!
//! Metal, and it is pinned in the ARTIFACT rather than selected here.
//! `contracts/artifact-manifest/apple-angle.lock.json` builds with
//! `angle_enable_metal=true angle_enable_gl=false angle_enable_vulkan=false`,
//! so `EGL_DEFAULT_DISPLAY` has one real backend to resolve to. Pinning at
//! build time rather than through `eglGetPlatformDisplayEXT` attributes keeps
//! this module free of a second, weaker copy of the decision -- and the
//! shared platform-display helper this crate uses on Linux passes an empty
//! attribute list anyway, which is the same reason the Windows presenter gives
//! for not pinning there.
//!
//! # Where ANGLE comes from
//!
//! From the process, not from a path this module guesses. An Apple application
//! that renders embeds ANGLE and links it -- on iOS that is not a preference,
//! Apple accepts an embedded framework bundle and rejects a bare dylib -- so by
//! the time any of this runs, `eglGetProcAddress` is already a symbol in the
//! image. `Library::this()` is that handle. Guessing a bundle-relative path
//! would be a second, worse copy of a layout Xcode already owns, and it would
//! be wrong in a different way for every host shape (an .app, a command-line
//! tool, a test bundle, an XCTest host).
//!
//! The bare-name fallback exists for the shapes with no bundle at all -- a
//! `cargo test` binary or the conformance player with `DYLD_LIBRARY_PATH` set --
//! and is the same delegation the Linux presenter makes when it opens its EGL
//! runtime by bare soname.

use std::{any::Any, ffi::c_void, ptr::NonNull, sync::Arc};

use graphics::egl_platform::{
    EglConcurrency, EglInstance, EglProvider, EglSurfaceFactory, GraphicsBackendId,
    GraphicsPlatform, PlatformIdentity, PreparedEglSurface, PreparedEglSurfaceRef,
};
use khronos_egl as egl;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    surface::Surface,
};

/// ANGLE's EGL entry point when it is not already in the process.
///
/// Two names because ANGLE builds a different product per Apple platform, and
/// that is upstream's decision rather than ours: `angle_shared_library` in
/// `gni/angle.gni` switches to `ios_framework_bundle` when `is_ios`, so an iOS
/// build produces `libEGL.framework` whose executable is `libEGL.framework/libEGL`
/// and a macOS build produces `libEGL.dylib`.
///
/// This is the FALLBACK, and its usefulness differs by platform, which is worth
/// stating rather than leaving to be discovered. Both products are built with an
/// `@rpath` install name, so a host that embeds and links ANGLE resolves it
/// through its own rpath and never reaches here -- `Library::this()` above finds
/// it already in the process. The bare-name `dlopen` below is for the shapes
/// with no bundle: a `cargo test` binary or the conformance player, with
/// `DYLD_LIBRARY_PATH` pointing at a build tree. On iOS there is no such search
/// path, and there is not meant to be: an iOS host links the framework.
///
/// On a non-Apple host this constant is compiled only under `cfg(test)`, where
/// nothing loads it -- the tests here exercise surface identity and factory
/// refusal, which need no EGL at all.
#[cfg(target_os = "macos")]
const APPLE_EGL_LIBRARY: &str = "libEGL.dylib";
#[cfg(not(target_os = "macos"))]
const APPLE_EGL_LIBRARY: &str = "libEGL.framework/libEGL";

/// Backend identity for every surface and provider in this module.
///
/// Separate from the Windows and Linux markers so a surface prepared by one
/// platform can never be accepted by another's factory.
struct AppleAngleEglBackend;
struct AppleAngleDeviceDomain;

/// EGL provider backed by ANGLE-Metal.
///
/// Like Windows and unlike X11, the display is not the host's connection: ANGLE
/// resolves it from `EGL_DEFAULT_DISPLAY` and takes the layer only when the
/// surface is created. So one provider serves both the headless and the
/// onscreen case.
#[derive(Debug, Default)]
pub struct AppleEglProvider;

impl AppleEglProvider {
    pub fn new() -> Self {
        Self
    }
}

impl EglProvider for AppleEglProvider {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<AppleAngleEglBackend>()
    }

    fn concurrency(&self) -> EglConcurrency {
        // The same answer Windows gives for the same implementation: ANGLE
        // supports shared contexts, and the upload thread needs one. This is a
        // statement about ANGLE, not an inference from the driver -- which is
        // why it is a constant here rather than a runtime probe.
        EglConcurrency::SharedContexts
    }

    fn platform_identity(&self) -> PlatformIdentity {
        PlatformIdentity::new::<AppleAngleDeviceDomain>(self.backend_id(), 0)
    }

    fn label(&self) -> &str {
        "apple-angle-egl"
    }

    fn load(&self) -> EngineResult<EglInstance> {
        // The process first. See the module header: on any Apple host that
        // renders, ANGLE is already linked, and asking dyld for a path would be
        // a guess about a bundle layout Xcode owns.
        let this = libloading::os::unix::Library::this();
        if unsafe { this.get::<*const c_void>(b"eglGetProcAddress\0") }.is_ok() {
            let library: libloading::Library = this.into();
            return unsafe { EglInstance::load_required_from(library) }.map_err(|error| {
                EngineError::new(ErrorCode::RenderBackendError)
                    .with_msg("resolve ANGLE EGL symbols from the process failed")
                    .with_detail(format!("provider={}: {error:?}", self.label()))
            });
        }

        let library = unsafe { libloading::Library::new(APPLE_EGL_LIBRARY) }.map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg(
                    "load ANGLE failed: not linked into the process and not on the loader path",
                )
                .with_detail(format!("{APPLE_EGL_LIBRARY}: {error}"))
        })?;
        unsafe { EglInstance::load_required_from(library) }.map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("resolve required ANGLE EGL symbols failed")
                .with_detail(format!("provider={}: {error:?}", self.label()))
        })
    }

    fn display(&self, egl: &EglInstance) -> EngineResult<egl::Display> {
        unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| {
            EngineError::new(ErrorCode::RenderInitializeError)
                .with_msg("ANGLE eglGetDisplay failed")
                .with_detail(format!("provider={}", self.label()))
        })
    }
}

/// Headless render target: the presenter serves it from a pbuffer sized to
/// these dimensions, so no layer and no compositor is involved.
#[derive(Debug)]
pub struct AppleOffscreenSurface {
    width: u32,
    height: u32,
}

impl AppleOffscreenSurface {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl Surface for AppleOffscreenSurface {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Shared ownership of one native `CAMetalLayer` retain.
///
/// Surface payloads, prepared EGL targets and resize targets share this owner.
/// The final owner releases the object after native rendering has retired.
#[derive(Clone, Debug)]
pub struct RetainedMetalLayer(Arc<MetalLayerOwner>);

#[derive(Debug)]
struct MetalLayerOwner {
    layer: NonNull<c_void>,
    release: unsafe fn(NonNull<c_void>),
}

// SAFETY: Objective-C retain/release may run on either lifecycle thread. All
// drawing and host layout still follow the existing CAMetalLayer/EGL contract;
// this owner exposes no Rust reference or mutable access to the object.
unsafe impl Send for MetalLayerOwner {}
unsafe impl Sync for MetalLayerOwner {}

impl Drop for MetalLayerOwner {
    fn drop(&mut self) {
        // SAFETY: construction acquired exactly one reference, and Arc runs
        // this destructor only after every surface/target owner has retired.
        unsafe { (self.release)(self.layer) };
    }
}

#[cfg(target_vendor = "apple")]
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_retain(object: *mut c_void) -> *mut c_void;
    fn objc_release(object: *mut c_void);
}

impl RetainedMetalLayer {
    /// Acquire ownership before a surface can be published to another thread.
    ///
    /// # Safety
    /// `layer` must point to a live `CAMetalLayer` for this call. The host must
    /// continue to synchronize layout and native rendering while attached.
    pub unsafe fn retain(layer: NonNull<c_void>) -> Self {
        #[cfg(target_vendor = "apple")]
        {
            unsafe fn retain(layer: NonNull<c_void>) {
                unsafe { objc_retain(layer.as_ptr()) };
            }
            unsafe fn release(layer: NonNull<c_void>) {
                unsafe { objc_release(layer.as_ptr()) };
            }
            unsafe { Self::with_refcount(layer, retain, release) }
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = layer;
            panic!("native CAMetalLayer ownership requires an Apple target")
        }
    }

    unsafe fn with_refcount(
        layer: NonNull<c_void>,
        retain: unsafe fn(NonNull<c_void>),
        release: unsafe fn(NonNull<c_void>),
    ) -> Self {
        unsafe { retain(layer) };
        Self(Arc::new(MetalLayerOwner { layer, release }))
    }

    /// Inject native refcount operations for portable lifecycle tests.
    ///
    /// # Safety
    /// Both callbacks must implement a balanced, thread-safe retain/release
    /// pair for `layer`, and their backing storage must outlive the final owner.
    #[cfg(any(test, feature = "test-support"))]
    pub unsafe fn retain_for_test(
        layer: NonNull<c_void>,
        retain: unsafe fn(NonNull<c_void>),
        release: unsafe fn(NonNull<c_void>),
    ) -> Self {
        unsafe { Self::with_refcount(layer, retain, release) }
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.0.layer.as_ptr()
    }

    /// How many `RetainedMetalLayer` values share this one native retain.
    ///
    /// One means this value's drop is the `objc_release`. More means it is not,
    /// and whoever is reasoning about when the host's layer goes has to account
    /// for the others first.
    pub fn owner_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }
}

/// Onscreen render target retaining a `CAMetalLayer` the host creates.
///
/// The host creates, sizes and positions the layer. The engine retains it for
/// the attachment and asynchronous native retirement, without owning a window.
///
/// A layer and not a view, on both platforms, because that is what the public
/// headers already decided: `include/migo/platform/ios.h` says the layer path
/// "stays the authoritative one for the renderer" and that the Host Kit creates
/// the `CAMetalLayer` backing a view. A `UIView`'s own layer is a `CALayer`, and
/// ANGLE's Metal backend needs a `CAMetalLayer`, so resolving a view here would
/// mean either creating a layer the host does not know about or accepting one
/// that cannot be drawn to.
#[derive(Debug)]
pub struct AppleMetalLayerSurface {
    layer: RetainedMetalLayer,
    width: u32,
    height: u32,
}

impl AppleMetalLayerSurface {
    /// # Safety
    ///
    /// `layer` must be a live `CAMetalLayer` for this call. The surface acquires
    /// a native reference before returning; layout remains host-controlled.
    pub unsafe fn new(layer: NonNull<c_void>, width: u32, height: u32) -> Self {
        Self::from_retained_layer(unsafe { RetainedMetalLayer::retain(layer) }, width, height)
    }

    pub fn from_retained_layer(layer: RetainedMetalLayer, width: u32, height: u32) -> Self {
        Self {
            layer,
            width,
            height,
        }
    }
}

impl Surface for AppleMetalLayerSurface {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn native_owner_count(&self) -> Option<usize> {
        Some(self.layer.owner_count())
    }
}

#[derive(Debug, Clone, Copy)]
enum AppleSurfaceTarget {
    Offscreen,
    MetalLayer,
}

#[derive(Debug)]
pub struct AppleEglSurfaceFactory {
    target: AppleSurfaceTarget,
}

impl AppleEglSurfaceFactory {
    fn offscreen() -> Self {
        Self {
            target: AppleSurfaceTarget::Offscreen,
        }
    }

    fn metal_layer() -> Self {
        Self {
            target: AppleSurfaceTarget::MetalLayer,
        }
    }
}

impl EglSurfaceFactory for AppleEglSurfaceFactory {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<AppleAngleEglBackend>()
    }

    fn platform_identity(&self) -> PlatformIdentity {
        PlatformIdentity::new::<AppleAngleDeviceDomain>(self.backend_id(), 0)
    }

    fn prepare(&self, surface: &dyn Surface) -> EngineResult<PreparedEglSurfaceRef> {
        let any = surface.as_any();
        match self.target {
            AppleSurfaceTarget::Offscreen => {
                if let Some(offscreen) = any.downcast_ref::<AppleOffscreenSurface>() {
                    return Ok(Arc::new(ApplePreparedSurface::Offscreen {
                        width: offscreen.width,
                        height: offscreen.height,
                    }));
                }
            }
            AppleSurfaceTarget::MetalLayer => {
                if let Some(layer) = any.downcast_ref::<AppleMetalLayerSurface>() {
                    return Ok(Arc::new(ApplePreparedSurface::MetalLayer {
                        layer: layer.layer.clone(),
                        width: layer.width,
                        height: layer.height,
                    }));
                }
            }
        }
        // Refusing an unexpected surface type is the point: a factory built for
        // one target must not silently render into another's.
        Err(EngineError::new(ErrorCode::RenderBackendError)
            .with_msg("Apple EGL surface factory received an unsupported surface")
            .with_detail(format!("target={:?}", self.target)))
    }
}

#[derive(Debug)]
pub enum ApplePreparedSurface {
    Offscreen {
        width: u32,
        height: u32,
    },
    MetalLayer {
        layer: RetainedMetalLayer,
        width: u32,
        height: u32,
    },
}

impl PreparedEglSurface for ApplePreparedSurface {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<AppleAngleEglBackend>()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
        let Some(other) = other.as_any().downcast_ref::<ApplePreparedSurface>() else {
            return false;
        };
        match (self, other) {
            // Identity for a layer is the layer, not its size: a resized layer
            // is still the same native surface, and treating it as a new one
            // would retire an attachment the host never replaced.
            (Self::MetalLayer { layer: a, .. }, Self::MetalLayer { layer: b, .. }) => {
                a.as_ptr() == b.as_ptr()
            }
            (
                Self::Offscreen {
                    width: aw,
                    height: ah,
                },
                Self::Offscreen {
                    width: bw,
                    height: bh,
                },
            ) => aw == bw && ah == bh,
            _ => false,
        }
    }

    fn create_window_surface(
        &self,
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
    ) -> EngineResult<egl::Surface> {
        match self {
            Self::Offscreen { width, height } => {
                let attributes = [
                    egl::WIDTH,
                    *width as egl::Int,
                    egl::HEIGHT,
                    *height as egl::Int,
                    egl::NONE,
                ];
                egl.create_pbuffer_surface(display, config, &attributes)
                    .map_err(|error| {
                        EngineError::new(ErrorCode::RenderBackendError)
                            .with_msg("ANGLE eglCreatePbufferSurface failed")
                            .with_detail(format!("{error:?}"))
                    })
            }
            Self::MetalLayer { layer, .. } => {
                // EGL 1.4's `eglCreateWindowSurface` takes the native window
                // **by value**, and ANGLE's Metal backend defines
                // `EGLNativeWindowType` as the `CAMetalLayer *` itself -- the
                // same shape as Windows passing an `HWND`, and the opposite of
                // the EGL 1.5/EXT platform call that takes a *pointer to* the
                // native window, which is why the X11 path passes `&xid`.
                // Passing the wrong one here would have EGL dereference an
                // Objective-C object pointer.
                unsafe {
                    egl.create_window_surface(
                        display,
                        config,
                        layer.as_ptr() as egl::NativeWindowType,
                        None,
                    )
                }
                .map_err(|error| {
                    EngineError::new(ErrorCode::RenderBackendError)
                        .with_msg("ANGLE eglCreateWindowSurface failed")
                        .with_detail(format!("{error:?}"))
                })
            }
        }
    }
}

/// Headless Apple graphics platform: ANGLE-Metal plus a pbuffer surface factory.
pub fn apple_graphics_platform() -> EngineResult<GraphicsPlatform> {
    GraphicsPlatform::try_new(
        Arc::new(AppleEglProvider::new()),
        Arc::new(AppleEglSurfaceFactory::offscreen()),
    )
}

/// Onscreen Apple graphics platform rendering into a host-owned `CAMetalLayer`.
///
/// The caller creates the layer and drives its layout and display link. Surface
/// wrappers retain it until native retirement completes; the engine never
/// creates or resizes it.
pub fn apple_metal_layer_graphics_platform() -> EngineResult<GraphicsPlatform> {
    GraphicsPlatform::try_new(
        Arc::new(AppleEglProvider::new()),
        Arc::new(AppleEglSurfaceFactory::metal_layer()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct LayerRefcounts {
        retains: AtomicUsize,
        releases: AtomicUsize,
    }

    unsafe fn retain_counted_layer(layer: NonNull<c_void>) {
        let counts = unsafe { layer.cast::<LayerRefcounts>().as_ref() };
        counts.retains.fetch_add(1, Ordering::SeqCst);
    }

    unsafe fn release_counted_layer(layer: NonNull<c_void>) {
        let counts = unsafe { layer.cast::<LayerRefcounts>().as_ref() };
        counts.releases.fetch_add(1, Ordering::SeqCst);
    }

    // The native refcount seam lets Linux exercise the same ownership path as
    // Apple, without treating identity-only test pointers as Objective-C objects.
    unsafe fn counted_surface(counts: &LayerRefcounts) -> AppleMetalLayerSurface {
        let pointer = NonNull::from(counts).cast();
        let layer = unsafe {
            RetainedMetalLayer::retain_for_test(
                pointer,
                retain_counted_layer,
                release_counted_layer,
            )
        };
        AppleMetalLayerSurface::from_retained_layer(layer, 800, 600)
    }

    #[test]
    fn constructing_a_layer_surface_retains_before_it_can_be_published() {
        let counts = LayerRefcounts::default();
        let surface = unsafe { counted_surface(&counts) };
        assert_eq!(counts.retains.load(Ordering::SeqCst), 1);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 0);
        drop(surface);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
    }

    // What `native_owner_count` is for, stated as the case that motivated it.
    //
    // Three things clone the layer's owner on the attach path: the surface, the
    // resize target the attachment keeps, and the prepared EGL surface the canvas
    // manager installs. Only the first is reachable through the `SurfaceRef`, so
    // a release check that counts `SurfaceRef`s sees one owner where there are
    // three -- and reports nothing while the host's `CAMetalLayer` is still
    // reachable from the renderer.
    //
    // The prepared surface is the interesting one because it is the owner that
    // outlives its `SurfaceRef` by design: `prepare` exists so the render thread
    // can hold the native target without holding the attachment.
    #[test]
    fn the_layer_owner_count_sees_owners_the_surface_reference_count_cannot() {
        let counts = LayerRefcounts::default();
        let surface = unsafe { counted_surface(&counts) };
        assert_eq!(
            surface.native_owner_count(),
            Some(1),
            "a surface that nothing has prepared is the only owner of its layer"
        );

        let prepared = AppleEglSurfaceFactory::metal_layer()
            .prepare(&surface)
            .expect("prepare retained layer");
        assert_eq!(
            surface.native_owner_count(),
            Some(2),
            "preparing an EGL target clones the layer's owner. This is the count the release \
             boundary has to read: the surface is still a single Arc, so a check that reads \
             Arc::strong_count on the SurfaceRef is looking at a 1 here"
        );
        assert_eq!(
            counts.releases.load(Ordering::SeqCst),
            0,
            "and nothing has been released yet, which is what makes the second owner matter"
        );

        drop(prepared);
        assert_eq!(
            surface.native_owner_count(),
            Some(1),
            "retiring the prepared target hands ownership back"
        );
        drop(surface);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn prepared_surface_keeps_the_layer_until_its_final_reference_is_retired() {
        let counts = LayerRefcounts::default();
        let surface = unsafe { counted_surface(&counts) };
        let prepared = AppleEglSurfaceFactory::metal_layer()
            .prepare(&surface)
            .expect("prepare retained layer");
        let retiring = prepared.clone();
        drop(surface);
        drop(prepared);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 0);
        drop(retiring);
        assert_eq!(counts.retains.load(Ordering::SeqCst), 1);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn release_notification_follows_the_final_native_layer_release() {
        use shared::surface::{SurfaceGenerationGate, SurfaceLease, SurfaceReleasePhase};
        let counts = Arc::new(LayerRefcounts::default());
        let gate = Arc::new(SurfaceGenerationGate::new());
        let lease = SurfaceLease::new(
            Arc::new(unsafe { counted_surface(&counts) }),
            gate.attach_or_update().expect("attach generation"),
        );
        let prepared = apple_metal_layer_graphics_platform()
            .expect("graphics platform")
            .prepare_surface_for_lease(&lease)
            .expect("prepare resource-bound layer");
        let pending = Arc::new(AtomicUsize::new(0));
        let notified = Arc::new(AtomicUsize::new(0));
        let notification_counts = counts.clone();
        let notification_calls = notified.clone();
        let release = lease
            .prepare_release(
                pending.clone(),
                Some(Box::new(move |_| {
                    assert_eq!(notification_counts.releases.load(Ordering::SeqCst), 1);
                    notification_calls.fetch_add(1, Ordering::SeqCst);
                })),
            )
            .expect("prepare release")
            .commit();
        gate.retire_current();
        drop(lease);
        assert_eq!(release.phase(), SurfaceReleasePhase::Pending);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 0);

        std::thread::spawn(move || drop(prepared))
            .join()
            .expect("retirement thread");
        assert_eq!(release.phase(), SurfaceReleasePhase::Released);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert_eq!(pending.load(Ordering::SeqCst), 0);
    }

    fn layer(value: usize) -> NonNull<c_void> {
        NonNull::new(value as *mut c_void).expect("test handle must be non-null")
    }

    fn fake_surface(value: usize, width: u32, height: u32) -> AppleMetalLayerSurface {
        unsafe fn no_refcount(_: NonNull<c_void>) {}
        let layer =
            unsafe { RetainedMetalLayer::retain_for_test(layer(value), no_refcount, no_refcount) };
        AppleMetalLayerSurface::from_retained_layer(layer, width, height)
    }

    /// Both targets report back the size they were handed, in that order.
    ///
    /// `Surface::size` is what the engine reads to size its viewport and its
    /// canvases; nothing else in this module consults it, because the pbuffer and
    /// the window surface are built from the prepared surface's own fields. So a
    /// transposed or dropped dimension here is invisible to every other test in
    /// this file, and the mirror of this test on Linux
    /// (`offscreen_surface_reports_its_size`) is the reason that was noticed.
    ///
    /// Deliberately not square, and the two targets deliberately given different
    /// sizes: with 256x256 a transposition asserts nothing, and with one shared
    /// size a test could pass while both wrappers read the same field.
    #[test]
    fn both_targets_report_the_size_they_were_given() {
        assert_eq!(AppleOffscreenSurface::new(320, 240).size(), (320, 240));
        assert_eq!(fake_surface(0x1234, 1024, 768).size(), (1024, 768));
    }

    #[test]
    fn platform_identity_is_stable_for_apple_angle_device() {
        assert_eq!(
            apple_graphics_platform()
                .expect("offscreen ANGLE platform")
                .platform_identity(),
            apple_metal_layer_graphics_platform()
                .expect("CAMetalLayer ANGLE platform")
                .platform_identity(),
        );
    }

    #[test]
    fn an_offscreen_factory_refuses_a_layer_surface() {
        let factory = AppleEglSurfaceFactory::offscreen();
        let surface = fake_surface(0x1234, 800, 600);
        assert!(
            factory.prepare(&surface).is_err(),
            "a pbuffer factory must not silently render into a layer"
        );
    }

    #[test]
    fn a_layer_factory_refuses_an_offscreen_surface() {
        let factory = AppleEglSurfaceFactory::metal_layer();
        let offscreen = AppleOffscreenSurface::new(800, 600);
        assert!(factory.prepare(&offscreen).is_err());
    }

    /// A resized layer is still the same native surface. Reporting otherwise
    /// would retire an attachment the host never replaced.
    #[test]
    fn layer_identity_is_the_layer_not_the_size() {
        let factory = AppleEglSurfaceFactory::metal_layer();
        let before = factory
            .prepare(&fake_surface(0x1234, 800, 600))
            .expect("prepare");
        let after = factory
            .prepare(&fake_surface(0x1234, 1024, 768))
            .expect("prepare");
        let other = factory
            .prepare(&fake_surface(0x5678, 800, 600))
            .expect("prepare");

        assert!(before.same_native_surface(after.as_ref()));
        assert!(!before.same_native_surface(other.as_ref()));
    }

    #[test]
    fn offscreen_identity_follows_the_size_it_was_allocated_for() {
        let factory = AppleEglSurfaceFactory::offscreen();
        let a = factory
            .prepare(&AppleOffscreenSurface::new(800, 600))
            .expect("prepare");
        let b = factory
            .prepare(&AppleOffscreenSurface::new(800, 600))
            .expect("prepare");
        let c = factory
            .prepare(&AppleOffscreenSurface::new(1024, 768))
            .expect("prepare");

        assert!(a.same_native_surface(b.as_ref()));
        assert!(!a.same_native_surface(c.as_ref()));
    }

    /// The platforms must not accept each other's prepared surfaces.
    #[test]
    fn the_apple_backend_has_its_own_identity() {
        let offscreen = AppleEglSurfaceFactory::offscreen();
        assert_eq!(
            offscreen.backend_id(),
            GraphicsBackendId::of::<AppleAngleEglBackend>()
        );
        assert_eq!(AppleEglProvider::new().backend_id(), offscreen.backend_id());
    }

    /// A pbuffer and a layer are never the same native surface, whatever their
    /// sizes.
    ///
    /// The mirror of `linux::presenter`'s
    /// `offscreen_and_x11_targets_are_never_the_same_surface`, and it is needed
    /// here for a sharper reason than symmetry: the two Apple graphics platforms
    /// deliberately share one `PlatformIdentity` -- the test above asserts exactly
    /// that -- so identity cannot tell a headless target from an onscreen one.
    /// This comparison is the only thing that can. Reporting them equal would let
    /// a move between headless and onscreen read as "the surface did not change",
    /// and the engine would go on rendering into the one it already had.
    ///
    /// Added because mutation testing found the `_ => false` arm of
    /// `same_native_surface` asserted by nothing: flipping it to `true` left all
    /// six of the inherited tests green.
    #[test]
    fn offscreen_and_layer_targets_are_never_the_same_surface() {
        let offscreen = AppleEglSurfaceFactory::offscreen()
            .prepare(&AppleOffscreenSurface::new(800, 600))
            .expect("prepare offscreen");
        let onscreen = AppleEglSurfaceFactory::metal_layer()
            .prepare(&fake_surface(0x1234, 800, 600))
            .expect("prepare layer");

        assert!(!offscreen.same_native_surface(onscreen.as_ref()));
        assert!(!onscreen.same_native_surface(offscreen.as_ref()));
    }

    /// Serialises the tests that initialise ANGLE's EGL display.
    ///
    /// `eglGetDisplay(EGL_DEFAULT_DISPLAY)` returns the same display to every
    /// caller in the process, and `eglTerminate` un-initialises it for all of
    /// them -- it is not refcounted against `eglInitialize`. So two tests that
    /// each initialise, work, and terminate are not independent: whichever
    /// terminates first pulls the display out from under the other, and the
    /// loser fails with `NotInitialized` on whatever EGL call it happened to be
    /// making.
    ///
    /// Measured 2026-09-11, and it is a race rather than a rule: the whole
    /// binary passed on the macOS lane and failed on the iOS Simulator lane in
    /// the same run, at `eglChooseConfig: NotInitialized`. `cargo test` runs
    /// these on separate threads by default, so which one wins is scheduling.
    ///
    /// `parking_lot::Mutex` because a panicking test must not poison the lock
    /// and turn one real failure into a second, fictional one in the other test.
    #[cfg(target_vendor = "apple")]
    static EGL_DISPLAY: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// ANGLE is really present, really loads under the name this module chose,
    /// and really answers with a usable display.
    ///
    /// Every other test in this module runs on any host and touches no EGL. This
    /// one is the opposite, and it is the only thing that makes
    /// `APPLE_EGL_LIBRARY` more than a string literal no code path reads --
    /// which is the class of claim that has already cost this project a device
    /// trip. `scripts/test-apple-egl-loader-name-contract.sh` proves the NAME is
    /// the one the recipe produces; only this proves a file answers to it.
    ///
    /// The three calls are `init_egl`'s, in its order, because the question is
    /// whether the render thread's own bring-up would succeed rather than whether
    /// these symbols resolve. `eglInitialize` is the one that needs a backend to
    /// exist: the lock file pins `angle_enable_metal=true` with every other
    /// backend off, so an archive built without Metal gets this far and fails
    /// here.
    ///
    /// NOT skipped when ANGLE is absent. A test that passes when the thing under
    /// test is missing is the gate shape this repository keeps being bitten by,
    /// so the failure names the script that installs it instead. The macOS leg of
    /// `.github/workflows/apple-sdk.yml` runs `scripts/fetch-apple-angle.sh` and
    /// puts the unpacked directory on `DYLD_LIBRARY_PATH`.
    #[test]
    #[cfg(target_vendor = "apple")]
    fn angle_loads_under_its_pinned_name_and_answers_with_a_display() {
        let _serialised = EGL_DISPLAY.lock();
        let provider = AppleEglProvider::new();
        let egl = provider.load().unwrap_or_else(|error| {
            panic!(
                "loading ANGLE failed: {error:?}\nThis build looks for {APPLE_EGL_LIBRARY:?}. \
                 Run `bash scripts/fetch-apple-angle.sh` and put the unpacked directory on \
                 DYLD_LIBRARY_PATH."
            )
        });
        let display = provider
            .display(&egl)
            .expect("ANGLE must resolve EGL_DEFAULT_DISPLAY");
        let (major, minor) = egl
            .initialize(display)
            .expect("eglInitialize must succeed: the pinned archive is built with Metal only");
        assert!(
            major > 1 || (major == 1 && minor >= 4),
            "the engine needs EGL 1.4 or better, ANGLE reported {major}.{minor}"
        );
        egl.terminate(display).expect("eglTerminate");
    }

    /// Skia builds a GL context on this platform's ANGLE, or it does not.
    ///
    /// Nothing on any host answered this before. `graphics/tests/common/harness.rs`
    /// has `with_gl_surface` behind `#[cfg(any())]` -- never compiled, its own
    /// comment saying "deliberately unimplemented until Phase 6" -- so every 2D
    /// golden in that crate runs on Skia's CPU raster backend and says nothing
    /// about GL. Skia-on-GL plainly works in production on Android; whether it
    /// works on ANGLE here was an open question with no test anywhere.
    ///
    /// It was written to tell two shapes of one failure apart -- was the
    /// external-frame lane's Canvas2D broken by something about that lane's
    /// context, or was Skia-on-ANGLE not working here at all -- and it answered:
    /// anywhere on macOS, on a bare pbuffer with nothing else involved. The
    /// cause was a GN argument rather than a driver, and it is fixed; see
    /// `scripts/apple-skia-gl-env.sh` for the mechanism.
    ///
    /// So its job now is to keep that fixed. It is the only thing in this
    /// repository that would notice the correction going away -- nothing else
    /// asks Skia for a GL context on a host, and WebGL never goes through Skia,
    /// so the symptom of losing it is every 2D surface silently failing to build
    /// while every lane stays green.
    ///
    /// Skipped rather than failed where ANGLE cannot load: a machine without it
    /// cannot answer the question, and a test that failed there would be
    /// reporting the machine.
    #[test]
    #[cfg(target_vendor = "apple")]
    fn skia_builds_a_gl_context_on_this_platforms_angle() {
        let _serialised = EGL_DISPLAY.lock();
        let provider = AppleEglProvider::new();
        let Ok(egl) = provider.load() else {
            eprintln!("SKIP: ANGLE did not load on this machine; nothing to ask");
            return;
        };
        let display = provider.display(&egl).expect("eglGetDisplay");
        egl.initialize(display).expect("eglInitialize");

        // The same shape the canvas manager asks for, so the answer is about
        // Skia and ANGLE rather than about an unusual config.
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
            24,
            egl::STENCIL_SIZE,
            8,
            egl::SURFACE_TYPE,
            egl::PBUFFER_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES3_BIT,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &attrs)
            .expect("eglChooseConfig")
            .expect("an ES3 pbuffer config");

        let pbuffer = egl
            .create_pbuffer_surface(
                display,
                config,
                &[egl::WIDTH, 64, egl::HEIGHT, 64, egl::NONE],
            )
            .expect("eglCreatePbufferSurface");
        let context = egl
            .create_context(
                display,
                config,
                None,
                &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
            )
            .expect("eglCreateContext");
        egl.make_current(display, Some(pbuffer), Some(pbuffer), Some(context))
            .expect("eglMakeCurrent");

        // And the question. `FboKind::DefaultFb` with fbo 0 is the pbuffer's own
        // framebuffer -- the simplest thing Skia could be asked to wrap.
        let built = graphics::backend::gl::surface::Canvas2DContext::new(
            0,
            64,
            64,
            graphics::backend::gl::surface::FboKind::DefaultFb,
            &|symbol: &str| {
                egl.get_proc_address(symbol)
                    .map(|f| f as *const std::ffi::c_void)
                    .unwrap_or(std::ptr::null())
            },
        );
        // The step, not just the verdict. Skia's own messages travel by
        // `tracing`, and a `#[test]` has no subscriber to receive them -- this
        // workspace builds `tracing-subscriber` without its `fmt` feature -- so
        // a bare `is_some()` would report the same "no" for a loader that
        // resolved nothing and for a driver Skia declined. Those want opposite
        // investigations; see `Canvas2DInitFailure`.
        let failed_at = built.err();

        let _ = egl.make_current(display, None, None, None);
        let _ = egl.destroy_context(display, context);
        let _ = egl.destroy_surface(display, pbuffer);
        let _ = egl.terminate(display);

        assert!(
            failed_at.is_none(),
            "Skia would not build a Canvas2D context on an ANGLE ES 3.0 pbuffer on this \
             machine; it stopped at {}. On macOS this is what losing \
             `skia_gl_standard=\"\"` looks like: Skia's macOS default assumes desktop GL \
             at compile time and ANGLE is ES, so `make_gl` rejects a context that is \
             perfectly good. Check that this command sourced scripts/apple-skia-gl-env.sh \
             and that the Skia it linked was built rather than downloaded -- a downloaded \
             one carries key.txt where a built one carries args.gn.",
            failed_at.expect("checked on the line above")
        );
    }
}
