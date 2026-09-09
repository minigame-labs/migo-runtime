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

    /// How many owners the *native* handle has, when this surface holds it
    /// behind a reference count of its own.
    ///
    /// `None` means this platform does not track one -- **not** that the handle
    /// is unowned. A caller may only conclude something from `Some`.
    ///
    /// It exists because the `SurfaceRef` count and the native handle's count
    /// are two different numbers, and the release contract is written about the
    /// second one. On Apple the `CAMetalLayer` is kept alive by an
    /// `Arc<MetalLayerOwner>` that `attach` deliberately clones twice -- once
    /// into the surface, once into the resize target -- so a surface that is
    /// uniquely referenced can still be one of several owners of the layer. A
    /// guard that watched only the `SurfaceRef` count therefore stayed silent
    /// through a release that left the host's layer alive, which is the exact
    /// failure the phase's documentation entitles a host to rely on not
    /// happening.
    fn native_owner_count(&self) -> Option<usize> {
        None
    }
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
