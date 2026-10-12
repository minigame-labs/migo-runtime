//! macOS and iOS platform support.

pub mod presenter;
// AVFoundation's: macOS only, where the V8 lane decodes in process.
#[cfg(target_os = "macos")]
pub mod video_decoder;
