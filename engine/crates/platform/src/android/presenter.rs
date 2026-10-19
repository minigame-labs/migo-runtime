//! Android's system-EGL presenter boundary.
//!
//! `AndroidSurfaceWrapper` remains the sole owner of the ANativeWindow strong
//! reference. Prepared targets are non-owning and live only while the matching
//! `SurfaceLease` is retained by the render binding.

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

use super::surface::{ANativeWindow, AndroidSurfaceWrapper};

const ANDROID_EGL_LIBRARY: &str = "libEGL.so";

#[derive(Debug)]
struct AndroidSystemEglBackend;
struct AndroidProcessEglDomain;

#[derive(Debug, Default)]
pub struct AndroidEglProvider;

impl EglProvider for AndroidEglProvider {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<AndroidSystemEglBackend>()
    }

    fn concurrency(&self) -> EglConcurrency {
        EglConcurrency::SharedContexts
    }

    fn platform_identity(&self) -> PlatformIdentity {
        PlatformIdentity::new::<AndroidProcessEglDomain>(self.backend_id(), 0)
    }

    fn label(&self) -> &str {
        "android-system-egl"
    }

    fn load(&self) -> EngineResult<EglInstance> {
        let library = process_egl_library(ANDROID_EGL_LIBRARY, || unsafe {
            libloading::Library::new(ANDROID_EGL_LIBRARY)
        })
        .map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("load Android system EGL failed")
                .with_detail(format!("{ANDROID_EGL_LIBRARY}: {error}"))
        })?;
        unsafe { EglInstance::load_required_from(library) }.map_err(|error| {
            EngineError::new(ErrorCode::RenderBackendError)
                .with_msg("resolve required Android EGL symbols failed")
                .with_detail(format!("provider={}: {error:?}", self.label()))
        })
    }

    fn display(&self, egl: &EglInstance) -> EngineResult<egl::Display> {
        unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| {
            EngineError::new(ErrorCode::RenderInitializeError)
                .with_msg("Android eglGetDisplay failed")
                .with_detail(format!("provider={}", self.label()))
        })
    }
}

#[derive(Debug, Default)]
pub struct AndroidEglSurfaceFactory;

impl EglSurfaceFactory for AndroidEglSurfaceFactory {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<AndroidSystemEglBackend>()
    }

    fn platform_identity(&self) -> PlatformIdentity {
        PlatformIdentity::new::<AndroidProcessEglDomain>(self.backend_id(), 0)
    }

    fn prepare(&self, surface: &dyn Surface) -> EngineResult<PreparedEglSurfaceRef> {
        let android = surface
            .as_any()
            .downcast_ref::<AndroidSurfaceWrapper>()
            .ok_or_else(|| {
                EngineError::new(ErrorCode::Unsupported)
                    .with_msg("Android presenter requires AndroidSurfaceWrapper")
                    .with_detail(format!("surface={surface:?}"))
            })?;
        let handle = NonNull::new(android.native_handle()).ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("Android presenter received a null ANativeWindow")
        })?;
        Ok(Arc::new(AndroidPreparedSurface { handle }))
    }
}

/// Non-owning, repeatable EGL target for one retained Android Surface lease.
pub struct AndroidPreparedSurface {
    handle: NonNull<ANativeWindow>,
}

// ANativeWindow access is externally synchronized by Android and EGL. The
// matching owning AndroidSurfaceWrapper is Send + Sync and remains retained by
// SurfaceLease for the entire prepared-target lifetime.
unsafe impl Send for AndroidPreparedSurface {}
unsafe impl Sync for AndroidPreparedSurface {}

impl fmt::Debug for AndroidPreparedSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AndroidPreparedSurface")
            .field("handle", &format_args!("{:p}", self.handle.as_ptr()))
            .finish()
    }
}

impl PreparedEglSurface for AndroidPreparedSurface {
    fn backend_id(&self) -> GraphicsBackendId {
        GraphicsBackendId::of::<AndroidSystemEglBackend>()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same_native_surface(&self, other: &dyn PreparedEglSurface) -> bool {
        other
            .as_any()
            .downcast_ref::<AndroidPreparedSurface>()
            .is_some_and(|other| self.handle == other.handle)
    }

    fn create_window_surface(
        &self,
        egl: &EglInstance,
        display: egl::Display,
        config: egl::Config,
    ) -> EngineResult<egl::Surface> {
        let native_window = self.handle.as_ptr() as egl::NativeWindowType;
        unsafe { egl.create_window_surface(display, config, native_window, None) }.map_err(
            |error| {
                EngineError::new(ErrorCode::RenderBackendError)
                    .with_msg("Android eglCreateWindowSurface failed")
                    .with_detail(format!("{error:?}"))
            },
        )
    }

    fn request_frame_rate(&self, fps: u32) {
        super::surface::request_frame_rate(self.handle.as_ptr(), fps);
    }
}

pub fn android_graphics_platform() -> EngineResult<GraphicsPlatform> {
    GraphicsPlatform::try_new(
        Arc::new(AndroidEglProvider),
        Arc::new(AndroidEglSurfaceFactory),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_identity_is_stable_for_android_process_egl() {
        assert_eq!(
            android_graphics_platform()
                .expect("first Android platform")
                .platform_identity(),
            android_graphics_platform()
                .expect("second Android platform")
                .platform_identity(),
        );
    }
}
