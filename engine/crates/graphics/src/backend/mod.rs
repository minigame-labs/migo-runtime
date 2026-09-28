//! Backend abstraction layer for the Migo renderer.
//!
//! Today only the GL backend exists.  The module is structured as if a second
//! backend (Vulkan/Graphite) could be added later; no `RenderBackend` trait
//! or runtime selection exists yet.
//!
//! # Layering
//!
//! ```text
//! RenderThread  ─────────── drives frame lifecycle
//!     │
//!     │ FramePacket (Canvas2DCmd + GLCmd)
//!     ▼
//! gl::Canvas2DRenderer   — Canvas2D → SkCanvas
//! renderergl::RendererGL  — GLCmd    → glow + StateTracker
//! ```
//!
//! Both sub-backends share the same `CanvasManager` (EGL contexts, onscreen
//! DrawingBuffer FBO, image registry) so 2D and 3D draw into the same
//! framebuffer without intermediate blits.

pub mod gl;
