//! # Shared Types Library
//!
//! This crate provides shared types, utilities, and protocols used across the Migo engine.
//! It serves as the common foundation for inter-crate communication and shared abstractions.
//!
//! ## Module Overview
//!
//! - [`codec`]: Encoding/decoding utilities for various text formats (UTF-8, Base64, Hex, etc.)
//! - [`config`]: Engine initialization configuration options
//! - [`device`]: Device and system settings abstractions
//! - [`error`]: Unified error handling with [`ErrorCode`] and [`EngineError`](error::EngineError)
//! - [`op_state`]: Shared operational state for Deno extensions
//! - [`protocol`]: Inter-service communication protocols (Host, IO, Render, Audio commands)
//! - [`surface`]: Platform-agnostic surface and window abstractions
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use shared::prelude::*;
//!
//! // Create initialization options
//! let options = InitOptions::new("/tmp/app", 2.0);
//!
//! // Handle errors with ErrorCode
//! fn example_operation() -> shared::error::EngineResult<()> {
//!     shared::bail!(ErrorCode::NotFound, "resource not available");
//! }
//! ```
//!
//! ## Protocol Architecture
//!
//! The engine uses a message-passing architecture with typed commands:
//!
//! - `HostCommand`: Commands sent to the JS runtime thread
//! - `RenderCommand`: Graphics/rendering operations
//! - `io_cmd`: File system and image I/O type definitions
//! - `AudioCmd`: Audio playback and processing commands
//!
//! Each command type flows through dedicated channels, enabling concurrent
//! processing across specialized threads (main, render, audio, IO).

pub mod audio_channel;
pub mod audio_resources;
#[cfg(test)]
mod audio_resources_tests;
pub mod callback_id;
pub mod channel;
pub mod cjs_compat;
// Behind `codec`: it owns `base64` and the encoding sniffers, and the
// engine-free layer decodes no text.
#[cfg(feature = "codec")]
pub mod codec;
pub mod command_vec_pool;
pub mod config;
pub mod console_log;
pub mod css_font;
pub mod css_font_shorthand;
pub mod device;
pub mod error;
pub mod feature_policy;
pub mod frame_rate;
pub mod host_channel;
pub mod image_id;
pub mod js_escape;
pub mod log_level;
pub mod log_throttle;
// Behind `vfs`, not because it is a filesystem but because it holds
// `VirtualFS`, `MountTable` and `GamePaths` by value. See the manifest.
#[cfg(feature = "vfs")]
pub mod op_state;
pub mod payload_pool;
pub mod protocol;
pub mod raf_signal;
pub mod render_command_sender;
pub mod render_event;
pub mod render_exit;
pub mod services;
pub mod stats;
pub mod surface;
pub mod text_measurer;
pub mod text_texture_cache;
pub mod thread_priority;
#[cfg(feature = "vfs")]
pub mod vfs;

#[cfg(test)]
pub mod test_support;

// Section 7.3's steady-state allocation gate reads this. `#[cfg(test)]` scopes it
// to this crate's own test binary: a `#[global_allocator]` is unique per binary, so
// one declared unconditionally here would follow the library into every shipped
// cdylib. Deleting it does not make the gates pass silently -- each burst proves the
// allocator is installed before it trusts a zero count.
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

/// Frequently used re-exports for convenient imports.
///
/// # Example
///
/// ```rust,ignore
/// use shared::prelude::*;
///
/// let options = InitOptions::new("/tmp", 1.5);
/// let window = WindowInfo::default();
/// ```
pub mod prelude {
    pub use crate::config::InitOptions;
    pub use crate::device::SystemSettings;
    pub use crate::error::{ErrorCode, io_error_to_error_code};
    pub use crate::surface::{SafeArea, Surface, SurfaceRef, WindowInfo};
}

/// Top-level stable re-exports (keep minimal to avoid API churn).
pub use config::InitOptions;
pub use device::SystemSettings;
pub use error::{ErrorCode, io_error_to_error_code};
pub use surface::{SafeArea, Surface, SurfaceRef, WindowInfo};

/// Protocol types are intentionally namespaced.
/// Prefer `use shared::protocol::...` in downstream crates.
/// `frame_packet` stays re-exported here as an explicit protocol exception for this plan stage.
pub use protocol::{
    frame_packet::{FrameOp, FramePacket, FramePacketBuilder},
    host_cmd::HostCommand,
    render_cmd::RenderCommand,
};
