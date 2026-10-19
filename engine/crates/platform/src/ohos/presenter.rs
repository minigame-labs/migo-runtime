//! OpenHarmony's system-EGL presenter boundary.
//!
//! `OhosSurfaceWrapper` remains the sole owner of the `OHNativeWindow`
//! reference. Prepared targets are non-owning and live only while the matching
//! `SurfaceLease` is retained by the render binding.
//!
//! This mirrors the Android presenter, and deliberately so: both platforms
//! reach the compositor through an opaque native window backed by a
//! producer/consumer buffer queue, and both expose it to EGL the same way. The
//! two places where a platform can differ here are the library name and the
//! calling convention of `eglCreateWindowSurface` -- see the note on that
//! function below, because getting the second one wrong is a mistake this
//! project has already made once.

use std::{any::Any, fmt, ptr::NonNull, sync::Arc};

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

use super::surface::{OHNativeWindow, OhosSurfaceWrapper};

const OHOS_EGL_LIBRARY: &str = "libEGL.so";

#[derive(Debug)]
struct OhosSystemEglBackend;
struct OhosProcessEglDomain;

#[derive(Debug, Default)]
pub struct OhosEglProvider;

impl EglProvider for OhosEglProvider {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<OhosSystemEglBackend>()
    }

    fn concurrency(&self) -> EglConcurrency {
        EglConcurrency::SharedContexts
    }

    fn platform_identity(&self) -> PlatformIdentity {
        PlatformIdentity::new::<OhosProcessEglDomain>(self.backend_id(), 0)
    }

    fn label(&self) -> &str {
        "ohos-system-egl"
    }

    fn load(&self) -> EngineResult<EglInstance> {
        let library = process_egl_library(OHOS_EGL_LIBRARY, || unsafe {
            libloading::Library::new(OHOS_EGL_LIBRARY)
        })
        .map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("load OpenHarmony system EGL failed")
                .with_detail(format!("{OHOS_EGL_LIBRARY}: {error}"))
        })?;
        unsafe { EglInstance::load_required_from(library) }.map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("resolve required OpenHarmony EGL symbols failed")
                .with_detail(format!("provider={}: {error:?}", self.label()))
        })
    }

    fn display(&self, egl: &EglInstance) -> EngineResult<egl::Display> {
        unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| {
            EngineError::new(ErrorCode::RenderInitializeError)
                .with_msg("OpenHarmony eglGetDisplay failed")
                .with_detail(format!("provider={}", self.label()))
        })
    }
}

#[derive(Debug, Default)]
pub struct OhosEglSurfaceFactory;

impl EglSurfaceFactory for OhosEglSurfaceFactory {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<OhosSystemEglBackend>()
    }

    fn platform_identity(&self) -> PlatformIdentity {
        PlatformIdentity::new::<OhosProcessEglDomain>(self.backend_id(), 0)
    }

    fn prepare(&self, surface: &dyn Surface) -> EngineResult<PreparedEglSurfaceRef> {
        let ohos = surface
            .as_any()
            .downcast_ref::<OhosSurfaceWrapper>()
            .ok_or_else(|| {
                EngineError::new(ErrorCode::Unsupported)
                    .with_msg("OpenHarmony presenter requires OhosSurfaceWrapper")
                    .with_detail(format!("surface={surface:?}"))
            })?;
        let handle = NonNull::new(ohos.native_handle()).ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("OpenHarmony presenter received a null OHNativeWindow")
        })?;
        Ok(Arc::new(OhosPreparedSurface { handle }))
    }
}

/// Non-owning, repeatable EGL target for one retained OpenHarmony Surface lease.
pub struct OhosPreparedSurface {
    handle: NonNull<OHNativeWindow>,
}

// Native window access is externally synchronized by the platform and EGL. The
// owning OhosSurfaceWrapper is Send + Sync and stays retained by SurfaceLease
// for the whole prepared-target lifetime.
unsafe impl Send for OhosPreparedSurface {}
unsafe impl Sync for OhosPreparedSurface {}

impl fmt::Debug for OhosPreparedSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OhosPreparedSurface")
            .field("handle", &format_args!("{:p}", self.handle.as_ptr()))
            .finish()
    }
}

impl PreparedEglSurface for OhosPreparedSurface {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<OhosSystemEglBackend>()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
        other
            .as_any()
            .downcast_ref::<OhosPreparedSurface>()
            .is_some_and(|other| self.handle == other.handle)
    }

    fn create_window_surface(
        &self,
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
    ) -> EngineResult<egl::Surface> {
        // The handle IS the native window, so it is passed by value the way
        // Android passes an ANativeWindow*. This is where the X11 and Wayland
        // backends differ from each other and from these two: X11's native
        // window is an XID, so the platform entry point there takes a pointer
        // *to* it, while a wl_egl_window* and an OHNativeWindow* are already
        // pointers. Passing the wrong one dereferences a handle as an address,
        // and eglCreateWindowSurface reports nothing more useful than failure.
        let native_window = self.handle.as_ptr() as egl::NativeWindowType;
        unsafe { egl.create_window_surface(display, config, native_window, None) }.map_err(
            |error| {
                EngineError::new(ErrorCode::RenderBackendError)
                    .with_msg("OpenHarmony eglCreateWindowSurface failed")
                    .with_detail(format!("{error:?}"))
            },
        )
    }
}

pub fn ohos_graphics_platform() -> EngineResult<GraphicsPlatform> {
    GraphicsPlatform::try_new(Arc::new(OhosEglProvider), Arc::new(OhosEglSurfaceFactory))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_identity_is_stable_for_ohos_process_egl() {
        assert_eq!(
            ohos_graphics_platform()
                .expect("first OpenHarmony platform")
                .platform_identity(),
            ohos_graphics_platform()
                .expect("second OpenHarmony platform")
                .platform_identity(),
        );
    }

    #[test]
    fn system_egl_supports_shared_context_threads() {
        assert_eq!(
            OhosEglProvider.concurrency(),
            EglConcurrency::SharedContexts
        );
    }
}
