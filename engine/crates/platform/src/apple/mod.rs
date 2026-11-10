//! macOS and iOS platform support.

// The Objective-C runtime and CoreFoundation, as the modules below use them.
#[cfg(target_vendor = "apple")]
pub(crate) mod objc;
// AVFoundation's asset writer: recording the game, on macOS and iOS alike.
#[cfg(target_vendor = "apple")]
pub mod game_recorder;
pub mod presenter;
// AVFoundation's asset reader: decoding a video for content, on macOS and iOS
// alike -- in process with the V8 lane, on the host for Performance+'s.
#[cfg(target_vendor = "apple")]
pub mod video_decoder;
