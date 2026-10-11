//! Decoding a video into frames content draws itself: `createVideoDecoder`.
//!
//! The platform decodes -- its codecs, behind [`VideoDecoderService`] -- and
//! the engine runs the decoding: one thread per decoder, paced by the frames'
//! presentation times or as fast as content takes them, a bounded buffer
//! between the two, and the decoder's events back to content. So a platform
//! supplies only what is its own: opening a source and turning it into RGBA
//! frames, one at a time, and moving to a position.

use crate::protocol::error::ServiceError;

/// When a decoded frame is due, in the source's own milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameTiming {
    /// Presentation time: when the frame is shown.
    pub pts_ms: f64,
    /// Decode time: when it was decoded, which a stream with reordered frames
    /// tells apart from the presentation time.
    pub dts_ms: f64,
}

/// One opened video, decoded frame by frame on the engine's decode thread.
pub trait VideoFrameSource: Send {
    /// The frames' width and height in pixels: fixed for the source.
    fn size(&self) -> (u32, u32);

    /// Decode the next frame, in presentation order, as straight RGBA8 into
    /// `rgba` -- `width * height * 4` bytes, row-major, top row first. `None`
    /// at the end of the stream.
    fn next_frame(&mut self, rgba: &mut [u8]) -> Result<Option<FrameTiming>, ServiceError>;

    /// Continue from the frame shown at `position_ms`: the next frame decoded
    /// is the one at or just before it.
    fn seek(&mut self, position_ms: f64) -> Result<(), ServiceError>;
}

/// The platform's video decoding.
pub trait VideoDecoderService: Send + Sync {
    /// Open `source` -- a real path the engine resolved from content's, or an
    /// http(s) URL -- for its video track. Called on the decode thread, so it
    /// may block for as long as opening takes.
    fn open(&self, source: &str) -> Result<Box<dyn VideoFrameSource>, ServiceError>;
}
