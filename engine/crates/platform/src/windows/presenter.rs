//! Windows ANGLE presenter boundary.
//!
//! Mirrors `platform/linux/presenter.rs` for `x86_64-pc-windows-msvc`: a
//! headless pbuffer path plus a host-owned `HWND` target, both injected through
//! the same contract (`EglProvider` / `EglSurfaceFactory` / `GraphicsPlatform`).
//!
//! # Why ANGLE rather than desktop GL
//!
//! Windows has no system EGL, and its desktop OpenGL is whatever the installed
//! driver happens to expose -- on many machines that is an OpenGL 1.1 software
//! path. ANGLE is what the browsers use for exactly this reason: it implements
//! GLES on top of D3D11, which is present and driver-supported everywhere the
//! engine targets. The engine already speaks EGL/GLES on Android and Linux, so
//! ANGLE also keeps the graphics layer on one API family instead of adding a
//! second backend.
//!
//! The host is responsible for `libEGL.dll` and `libGLESv2.dll` being loadable
//! (same directory as the executable, or on `PATH`). This crate does not ship or
//! locate them, exactly as it does not ship the system EGL it loads on Linux.

use std::{any::Any, ffi::c_void, ptr::NonNull, sync::Arc};

use graphics::egl_platform::{
    EglConcurrency, EglInstance, EglProvider, EglSurfaceFactory, GraphicsBackendId,
    GraphicsPlatform, PlatformIdentity, PreparedEglSurface, PreparedEglSurfaceRef,
    process_egl_library,
};
use khronos_egl as egl;
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    surface::Surface,
};

/// ANGLE's EGL entry point on Windows.
///
/// Loaded by plain name so the normal Windows search order applies and the host
/// decides which ANGLE it ships, the same delegation Linux makes by loading its
/// EGL runtime by bare soname rather than a pinned absolute path.
const WINDOWS_EGL_LIBRARY: &str = "libEGL.dll";

/// Backend identity for every surface and provider in this module.
///
/// Separate from Linux's marker so a surface prepared by one platform can never
/// be accepted by the other's factory.
struct WindowsAngleEglBackend;
struct WindowsAngleDeviceDomain;

/// EGL provider backed by ANGLE.
///
/// Unlike X11 -- where the display *is* the host's connection and must be
/// threaded through -- ANGLE resolves its display from `EGL_DEFAULT_DISPLAY`
/// and takes the window only when the surface is created. So there is one
/// provider for both the headless and the windowed case.
#[derive(Debug, Default)]
pub struct WindowsEglProvider;

impl WindowsEglProvider {
    pub fn new() -> Self {
        Self
    }
}

impl EglProvider for WindowsEglProvider {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<WindowsAngleEglBackend>()
    }

    fn concurrency(&self) -> EglConcurrency {
        EglConcurrency::SharedContexts
    }

    fn platform_identity(&self) -> PlatformIdentity {
        PlatformIdentity::new::<WindowsAngleDeviceDomain>(self.backend_id(), 0)
    }

    fn label(&self) -> &str {
        "windows-angle-egl"
    }

    fn load(&self) -> EngineResult<EglInstance> {
        let library = process_egl_library(WINDOWS_EGL_LIBRARY, || unsafe {
            libloading::Library::new(WINDOWS_EGL_LIBRARY)
        })
        .map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("load ANGLE libEGL.dll failed")
                .with_detail(format!("{WINDOWS_EGL_LIBRARY}: {error}"))
        })?;
        unsafe { EglInstance::load_required_from(library) }.map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("resolve required ANGLE EGL symbols failed")
                .with_detail(format!("provider={}: {error:?}", self.label()))
        })
    }

    fn display(&self, egl: &EglInstance) -> EngineResult<egl::Display> {
        // `EGL_DEFAULT_DISPLAY` gives ANGLE's own default backend, which is
        // D3D11 on Windows. Naming a backend explicitly is possible, but only
        // through `eglGetPlatformDisplayEXT` with `EGL_PLATFORM_ANGLE_ANGLE`
        // attributes, and the shared platform-display helper this crate uses on
        // Linux hardcodes an empty attribute list. Pinning a backend is
        // therefore a deliberate follow-up (it needs that helper to accept
        // attributes) rather than something to fake here with a second copy of
        // it -- and the default is the backend we would pin to anyway.
        unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| {
            EngineError::new(ErrorCode::RenderInitializeError)
                .with_msg("ANGLE eglGetDisplay failed")
                .with_detail(format!("provider={}", self.label()))
        })
    }
}

/// Headless render target: the presenter serves it from a pbuffer sized to
/// these dimensions, so no window and no compositor is involved.
#[derive(Debug)]
pub struct WindowsOffscreenSurface {
    width: u32,
    height: u32,
}

impl WindowsOffscreenSurface {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl Surface for WindowsOffscreenSurface {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Onscreen render target wrapping a window the **host** owns.
///
/// The host creates, sizes and destroys the `HWND`; the engine only renders
/// into it and never runs its message loop. That is the same ownership rule the
/// X11 and Wayland targets follow, and it is what keeps the SDK from owning a
/// top-level window.
#[derive(Debug)]
pub struct WindowsHwndSurface {
    hwnd: NonNull<c_void>,
    width: u32,
    height: u32,
}

// SAFETY: the handle is an opaque token handed to EGL and never dereferenced
// here. The render thread creates the surface from it while the host services
// the window on its own thread, which is sound because the host guarantees
// (documented on `windows_hwnd_graphics_platform`) that the window outlives the
// attachment.
unsafe impl Send for WindowsHwndSurface {}
unsafe impl Sync for WindowsHwndSurface {}

impl WindowsHwndSurface {
    /// # Safety
    ///
    /// `hwnd` must be a live window handle that stays valid until the engine
    /// reports the surface released.
    pub unsafe fn new(hwnd: NonNull<c_void>, width: u32, height: u32) -> Self {
        Self {
            hwnd,
            width,
            height,
        }
    }
}

impl Surface for WindowsHwndSurface {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy)]
enum WindowsSurfaceTarget {
    Offscreen,
    Hwnd,
}

#[derive(Debug)]
pub struct WindowsEglSurfaceFactory {
    target: WindowsSurfaceTarget,
}

impl WindowsEglSurfaceFactory {
    fn offscreen() -> Self {
        Self {
            target: WindowsSurfaceTarget::Offscreen,
        }
    }

    fn hwnd() -> Self {
        Self {
            target: WindowsSurfaceTarget::Hwnd,
        }
    }
}

impl EglSurfaceFactory for WindowsEglSurfaceFactory {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<WindowsAngleEglBackend>()
    }

    fn platform_identity(&self) -> PlatformIdentity {
        PlatformIdentity::new::<WindowsAngleDeviceDomain>(self.backend_id(), 0)
    }

    fn prepare(&self, surface: &dyn Surface) -> EngineResult<PreparedEglSurfaceRef> {
        let any = surface.as_any();
        match self.target {
            WindowsSurfaceTarget::Offscreen => {
                if let Some(offscreen) = any.downcast_ref::<WindowsOffscreenSurface>() {
                    return Ok(Arc::new(WindowsPreparedSurface::Offscreen {
                        width: offscreen.width,
                        height: offscreen.height,
                    }));
                }
            }
            WindowsSurfaceTarget::Hwnd => {
                if let Some(window) = any.downcast_ref::<WindowsHwndSurface>() {
                    return Ok(Arc::new(WindowsPreparedSurface::Hwnd {
                        hwnd: window.hwnd,
                        width: window.width,
                        height: window.height,
                    }));
                }
            }
        }
        // Refusing an unexpected surface type is the point: a factory built for
        // one target must not silently render into another's.
        Err(EngineError::new(ErrorCode::RenderBackendError)
            .with_msg("Windows EGL surface factory received an unsupported surface")
            .with_detail(format!("target={:?}", self.target)))
    }
}

#[derive(Debug)]
pub enum WindowsPreparedSurface {
    Offscreen {
        width: u32,
        height: u32,
    },
    Hwnd {
        hwnd: NonNull<c_void>,
        width: u32,
        height: u32,
    },
}

// SAFETY: see `WindowsHwndSurface` -- the handle is only ever passed to EGL.
unsafe impl Send for WindowsPreparedSurface {}
unsafe impl Sync for WindowsPreparedSurface {}

impl PreparedEglSurface for WindowsPreparedSurface {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<WindowsAngleEglBackend>()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
        let Some(other) = other.as_any().downcast_ref::<WindowsPreparedSurface>() else {
            return false;
        };
        match (self, other) {
            // Identity for a window is the window, not its size: a resized
            // window is still the same native surface, and treating it as a new
            // one would retire an attachment the host never replaced.
            (Self::Hwnd { hwnd: a, .. }, Self::Hwnd { hwnd: b, .. }) => a == b,
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
        match *self {
            Self::Offscreen { width, height } => {
                let attributes = [
                    egl::WIDTH,
                    width as egl::Int,
                    egl::HEIGHT,
                    height as egl::Int,
                    egl::NONE,
                ];
                egl.create_pbuffer_surface(display, config, &attributes)
                    .map_err(|error| {
                        EngineError::new(ErrorCode::RenderBackendError)
                            .with_msg("ANGLE eglCreatePbufferSurface failed")
                            .with_detail(format!("{error:?}"))
                    })
            }
            Self::Hwnd { hwnd, .. } => {
                // EGL 1.4's `eglCreateWindowSurface` takes the native window
                // **by value**, and on Windows `EGLNativeWindowType` is the
                // `HWND` itself. This is the opposite of the EGL 1.5/EXT
                // platform call, which takes a *pointer to* the native window --
                // the distinction that makes the X11 path pass `&xid` while
                // Wayland passes its `wl_egl_window*` directly. Passing the
                // wrong one here would have EGL dereference a window handle.
                unsafe {
                    egl.create_window_surface(
                        display,
                        config,
                        hwnd.as_ptr() as egl::NativeWindowType,
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

/// Headless Windows graphics platform: ANGLE + a pbuffer surface factory.
pub fn windows_graphics_platform() -> EngineResult<GraphicsPlatform> {
    GraphicsPlatform::try_new(
        Arc::new(WindowsEglProvider::new()),
        Arc::new(WindowsEglSurfaceFactory::offscreen()),
    )
}

/// Onscreen Windows graphics platform rendering into a host-owned window.
///
/// The caller keeps ownership of the `HWND`: it must stay valid for as long as
/// the attachment lives, and the host keeps running its own message loop. The
/// engine never creates, resizes or destroys the window.
pub fn windows_hwnd_graphics_platform() -> EngineResult<GraphicsPlatform> {
    GraphicsPlatform::try_new(
        Arc::new(WindowsEglProvider::new()),
        Arc::new(WindowsEglSurfaceFactory::hwnd()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hwnd(value: usize) -> NonNull<c_void> {
        NonNull::new(value as *mut c_void).expect("test handle must be non-null")
    }

    #[test]
    fn platform_identity_is_stable_for_windows_angle_device() {
        assert_eq!(
            windows_graphics_platform()
                .expect("offscreen ANGLE platform")
                .platform_identity(),
            windows_hwnd_graphics_platform()
                .expect("HWND ANGLE platform")
                .platform_identity(),
        );
    }

    #[test]
    fn an_offscreen_factory_refuses_a_window_surface() {
        let factory = WindowsEglSurfaceFactory::offscreen();
        let window = unsafe { WindowsHwndSurface::new(hwnd(0x1234), 800, 600) };
        assert!(
            factory.prepare(&window).is_err(),
            "a pbuffer factory must not silently accept a window"
        );
    }

    #[test]
    fn a_window_factory_refuses_an_offscreen_surface() {
        let factory = WindowsEglSurfaceFactory::hwnd();
        let offscreen = WindowsOffscreenSurface::new(800, 600);
        assert!(factory.prepare(&offscreen).is_err());
    }

    /// A pbuffer and a window are never the same native surface, whatever their
    /// sizes.
    ///
    /// The mirror of `apple::presenter`'s
    /// `offscreen_and_layer_targets_are_never_the_same_surface` and of
    /// `linux::presenter`'s X11 one, and it was the only one of the three
    /// missing. `same_native_surface`'s `_ => false` arm -- the arm that answers
    /// a headless target compared against an onscreen one -- was asserted by
    /// nothing: flipping it to `true` left all six presenter tests green.
    ///
    /// What that arm being wrong costs: a move between headless and onscreen
    /// would read as "the surface did not change", and the engine would go on
    /// rendering into the one it already had.
    #[test]
    fn offscreen_and_window_targets_are_never_the_same_surface() {
        let offscreen = WindowsEglSurfaceFactory::offscreen()
            .prepare(&WindowsOffscreenSurface::new(800, 600))
            .expect("prepare offscreen");
        let onscreen = WindowsEglSurfaceFactory::hwnd()
            .prepare(&unsafe { WindowsHwndSurface::new(hwnd(0x1234), 800, 600) })
            .expect("prepare window");

        assert!(!offscreen.same_native_surface(onscreen.as_ref()));
        assert!(!onscreen.same_native_surface(offscreen.as_ref()));
    }

    /// A resized window is still the same native surface. Reporting otherwise
    /// would retire an attachment the host never replaced.
    #[test]
    fn window_identity_is_the_handle_not_the_size() {
        let factory = WindowsEglSurfaceFactory::hwnd();
        let before = factory
            .prepare(&unsafe { WindowsHwndSurface::new(hwnd(0x1234), 800, 600) })
            .expect("prepare");
        let after = factory
            .prepare(&unsafe { WindowsHwndSurface::new(hwnd(0x1234), 1024, 768) })
            .expect("prepare");
        let other = factory
            .prepare(&unsafe { WindowsHwndSurface::new(hwnd(0x5678), 800, 600) })
            .expect("prepare");

        assert!(before.same_native_surface(after.as_ref()));
        assert!(!before.same_native_surface(other.as_ref()));
    }

    #[test]
    fn offscreen_identity_follows_the_size_it_was_allocated_for() {
        let factory = WindowsEglSurfaceFactory::offscreen();
        let a = factory
            .prepare(&WindowsOffscreenSurface::new(800, 600))
            .expect("prepare");
        let b = factory
            .prepare(&WindowsOffscreenSurface::new(800, 600))
            .expect("prepare");
        let c = factory
            .prepare(&WindowsOffscreenSurface::new(1024, 768))
            .expect("prepare");

        assert!(a.same_native_surface(b.as_ref()));
        assert!(!a.same_native_surface(c.as_ref()));
    }

    /// The two platforms must not accept each other's prepared surfaces.
    #[test]
    fn the_windows_backend_has_its_own_identity() {
        let offscreen = WindowsEglSurfaceFactory::offscreen();
        assert_eq!(
            offscreen.backend_id(),
            GraphicsBackendId::of::<WindowsAngleEglBackend>()
        );
        assert_eq!(
            WindowsEglProvider::new().backend_id(),
            offscreen.backend_id()
        );
    }
}
