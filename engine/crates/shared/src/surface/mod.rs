//! # Surface Abstraction Module
//!
//! Provides platform-agnostic abstractions for rendering surfaces and window geometry.
//!
//! ## Overview
//!
//! A "surface" represents a drawable area where graphics can be rendered.
//! On Android, this wraps an `ANativeWindow`; on desktop platforms, it could
//! wrap a window handle from winit or similar libraries.
//!
//! ## Key Types
//!
//! - [`Surface`]: Trait for platform-specific surface implementations
//! - [`SurfaceRef`]: Thread-safe reference to a surface (`Arc<dyn Surface>`)
//! - [`WindowInfo`]: Logical window dimensions and pixel ratio
//! - [`SafeArea`]: Device safe area insets (notch, navigation bar, etc.)
//!
//! ## Platform Integration
//!
//! Each platform crate provides its own `Surface` implementation:
//!
//! ```rust,ignore
//! // Android implementation (in platform crate)
//! pub struct AndroidSurface {
//!     native_window: NonNull<ANativeWindow>,
//!     width: u32,
//!     height: u32,
//! }
//!
//! impl Surface for AndroidSurface {
//!     fn size(&self) -> (u32, u32) {
//!         (self.width, self.height)
//!     }
//!
//! }
//! ```

use std::{any::Any, sync::Arc};

mod attachment;
mod control;
mod geometry;
mod host_window;

pub use control::{
    SurfaceCandidateRevision, SurfaceControl, SurfaceControlAttachError, SurfaceControlInstallError,
};
pub use host_window::{HostWindowInfo, HostWindowMetrics, HostWindowState};

pub use attachment::{
    PreparedSurfaceRelease, PublicSurfaceGeneration, SurfaceGeneration, SurfaceGenerationError,
    SurfaceGenerationGate, SurfaceGenerationMismatch, SurfaceLease, SurfaceLivenessToken,
    SurfaceReleaseDisposition, SurfaceReleaseNotification, SurfaceReleaseObserver,
    SurfaceReleasePhase, SurfaceReleaseRegistrationError, SurfaceReleaseTransactionError,
    SurfaceResourceLease, release_retired_resource,
};
pub use geometry::{PixelRatio, SafeArea, SurfaceLossReason, WindowInfo};

/// A platform window/surface abstraction used by the renderer.
///
/// This trait provides the minimal interface needed to create an EGL/OpenGL
/// rendering context and draw to the screen.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to allow passing surfaces between
/// threads (e.g., from the platform layer to the render thread).
///
/// # Platform Implementation Notes
///
/// - **Android**: Wraps `ANativeWindow*` from the NDK
/// - **Desktop (future)**: Could wrap winit `Window` or raw X11/Wayland handles
pub trait Surface: std::fmt::Debug + Send + Sync {
    /// Type-erased platform payload access for the selected presenter.
    /// Downcasts are control-path only and must fail closed.
    fn as_any(&self) -> &dyn Any;

    /// Returns the physical size of the surface in pixels.
    ///
    /// This is the actual framebuffer size, not the logical (CSS pixel) size.
    /// Use this for `glViewport` and texture allocation.
    ///
    /// # Returns
    ///
    /// Tuple of `(width, height)` in physical pixels.
    fn size(&self) -> (u32, u32);
}

/// Thread-safe reference to a surface.
///
/// This type alias provides a convenient way to pass surfaces between
/// threads without ownership concerns. The underlying surface is
/// reference-counted and can be safely cloned.
///
/// # Example
///
/// ```rust,ignore
/// use shared::surface::SurfaceRef;
///
/// fn spawn_render_thread(surface: SurfaceRef) {
///     std::thread::spawn(move || {
///         let (width, height) = surface.size();
///         // Create EGL context with surface...
///     });
/// }
/// ```
pub type SurfaceRef = Arc<dyn Surface + Send + Sync + 'static>;
