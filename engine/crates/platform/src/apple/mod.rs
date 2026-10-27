//! macOS and iOS platform support.

// The Objective-C runtime and CoreFoundation, as the modules below use them.
#[cfg(target_vendor = "apple")]
pub(crate) mod objc;
// AVFoundation's asset writer: recording the game, on macOS and iOS alike.
#[cfg(target_vendor = "apple")]
pub mod game_recorder;
pub mod presenter;
// AVFoundation's: macOS only, where the V8 lane decodes in process.
#[cfg(target_os = "macos")]
pub mod video_decoder;
