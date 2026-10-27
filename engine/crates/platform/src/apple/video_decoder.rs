//! `createVideoDecoder` on macOS: AVFoundation's asset reader.
//!
//! The reader decodes the video track to BGRA pixel buffers -- its decoders,
//! VideoToolbox's hardware where there is one, and its own colour handling --
//! and each buffer's rows become straight RGBA. A reader only reads forward,
//! so a seek starts a new one a little before the position and keeps the frame
//! shown there, as the Android decoder does.

use std::ffi::c_void;

use shared::{
    protocol::error::ServiceError,
    services::{FrameTiming, VideoDecoderService, VideoFrameSource},
};

use super::objc::{CFRelease, CMTime, Cf, Id, Owned, Pool, class, describe, sel, send};

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTimeRange {
    start: CMTime,
    duration: CMTime,
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: *const c_void;
    fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(buffer: *mut c_void) -> *const u8;
    fn CVPixelBufferGetBytesPerRow(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetWidth(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(buffer: *mut c_void) -> usize;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetImageBuffer(buffer: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetPresentationTimeStamp(buffer: *mut c_void) -> CMTime;
    fn CMTimeGetSeconds(time: CMTime) -> f64;
    fn CMTimeMakeWithSeconds(seconds: f64, timescale: i32) -> CMTime;
    static kCMTimePositiveInfinity: CMTime;
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeVideo: Id;
}

const BGRA: i32 = 0x4247_5241;
const READ_ONLY: u64 = 1;
const READER_COMPLETED: isize = 2;
/// How far before a seek's position the new reader starts, so the frame shown
/// there is among those it decodes.
const SEEK_LEAD_SECONDS: f64 = 2.0;

/// A sample buffer `copyNextSampleBuffer` handed over.
struct Sample(*mut c_void);

impl Drop for Sample {
    fn drop(&mut self) {
        // SAFETY: a copied buffer is ours to release, once.
        unsafe { CFRelease(self.0) }
    }
}

/// The engine's video decoding on macOS.
pub struct AppleVideoDecoder;

impl VideoDecoderService for AppleVideoDecoder {
    fn open(&self, source: &str) -> Result<Box<dyn VideoFrameSource>, ServiceError> {
        Ok(Box::new(Source::open(source)?))
    }
}

struct Source {
    asset: Owned,
    track: Owned,
    reader: Owned,
    output: Owned,
    width: u32,
    height: u32,
    seek_target: Option<f64>,
    /// Where the current seek's reader starts, in seconds.
    seek_from: f64,
    held: Option<(Sample, f64)>,
    pending: Option<(Sample, f64)>,
}

// SAFETY: the objects are used only from the one decode thread that owns the
// source; AVFoundation's readers may be driven from any one thread.
unsafe impl Send for Source {}

impl Source {
    fn open(source: &str) -> Result<Self, ServiceError> {
        let _pool = Pool::new();
        // SAFETY: each message is sent to a live object through its method's
        // prototype; what is kept past the pool is retained.
        unsafe {
            let string = Cf::string(source)
                .ok_or_else(|| ServiceError::invalid_param("the source is not text"))?;
            let remote = source.starts_with("http://") || source.starts_with("https://");
            let url: Id = if remote {
                send!(class(c"NSURL"), sel(c"URLWithString:"), string.id() => Id; Id)
            } else {
                send!(class(c"NSURL"), sel(c"fileURLWithPath:"), string.id() => Id; Id)
            };
            drop(string);
            if url.is_null() {
                return Err(ServiceError::invalid_param("the source is not a URL"));
            }
            let asset = send!(class(c"AVURLAsset"), sel(c"URLAssetWithURL:options:"), url => Id, std::ptr::null_mut() => Id; Id);
            let asset = Owned::retain(asset)
                .ok_or_else(|| ServiceError::system("the video could not be opened"))?;
            let tracks: Id =
                send!(asset.0, sel(c"tracksWithMediaType:"), AVMediaTypeVideo => Id; Id);
            let track: Id = if tracks.is_null() {
                tracks
            } else {
                send!(tracks, sel(c"firstObject") => Id)
            };
            let track = Owned::retain(track)
                .ok_or_else(|| ServiceError::not_supported("the source has no video track"))?;
            let (reader, output) = Self::reader(&asset, &track, None)?;
            let mut opened = Self {
                asset,
                track,
                reader,
                output,
                width: 0,
                height: 0,
                seek_target: None,
                seek_from: 0.0,
                held: None,
                pending: None,
            };
            // The size is the decoded frames': the first one says it, and is
            // kept for the first call.
            let first = opened
                .decode()?
                .ok_or_else(|| ServiceError::system("the video has no frames"))?;
            let buffer = CMSampleBufferGetImageBuffer(first.0.0);
            opened.width = CVPixelBufferGetWidth(buffer) as u32;
            opened.height = CVPixelBufferGetHeight(buffer) as u32;
            if opened.width == 0 || opened.height == 0 {
                return Err(ServiceError::system("the video has no size"));
            }
            opened.pending = Some(first);
            Ok(opened)
        }
    }

    /// A reader of `track`'s frames as BGRA, from `from` seconds or the start.
    unsafe fn reader(
        asset: &Owned,
        track: &Owned,
        from: Option<f64>,
    ) -> Result<(Owned, Owned), ServiceError> {
        // SAFETY: as in `open`.
        unsafe {
            let mut error: Id = std::ptr::null_mut();
            let reader = send!(class(c"AVAssetReader"), sel(c"assetReaderWithAsset:error:"),
                asset.0 => Id, &mut error as *mut Id => *mut Id; Id);
            let reader = Owned::retain(reader)
                .ok_or_else(|| ServiceError::system(describe("opening the video", error)))?;
            let format = Cf::i32(BGRA);
            let settings = Cf::dictionary(&[(kCVPixelBufferPixelFormatTypeKey, format.0)]);
            let output = send!(class(c"AVAssetReaderTrackOutput"), sel(c"assetReaderTrackOutputWithTrack:outputSettings:"),
                track.0 => Id, settings.id() => Id; Id);
            drop(settings);
            let output = Owned::retain(output)
                .ok_or_else(|| ServiceError::system("the video track cannot be read"))?;
            // The frames are only read here, never kept past the next one.
            send!(output.0, sel(c"setAlwaysCopiesSampleData:"), 0i8 => i8; ());
            send!(reader.0, sel(c"addOutput:"), output.0 => Id; ());
            if let Some(from) = from {
                let range = CMTimeRange {
                    start: CMTimeMakeWithSeconds(from.max(0.0), 600),
                    duration: kCMTimePositiveInfinity,
                };
                send!(reader.0, sel(c"setTimeRange:"), range => CMTimeRange; ());
            }
            if send!(reader.0, sel(c"startReading") => i8) == 0 {
                let error: Id = send!(reader.0, sel(c"error") => Id);
                return Err(ServiceError::system(describe("reading the video", error)));
            }
            Ok((reader, output))
        }
    }

    /// The next frame the reader decodes, with its presentation time in
    /// milliseconds; `None` at the end.
    fn decode(&mut self) -> Result<Option<(Sample, f64)>, ServiceError> {
        // SAFETY: as in `open`.
        unsafe {
            loop {
                let sample = send!(self.output.0, sel(c"copyNextSampleBuffer") => *mut c_void);
                if sample.is_null() {
                    let status = send!(self.reader.0, sel(c"status") => isize);
                    if status == READER_COMPLETED {
                        return Ok(None);
                    }
                    let error: Id = send!(self.reader.0, sel(c"error") => Id);
                    return Err(ServiceError::system(describe("decoding", error)));
                }
                let sample = Sample(sample);
                // A buffer with no image -- a marker -- is no frame.
                if CMSampleBufferGetImageBuffer(sample.0).is_null() {
                    continue;
                }
                let pts =
                    CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sample.0)) * 1000.0;
                return Ok(Some((sample, pts)));
            }
        }
    }

    fn emit(
        &self,
        (sample, pts): (Sample, f64),
        rgba: &mut [u8],
    ) -> Result<Option<FrameTiming>, ServiceError> {
        // SAFETY: the sample is live; its pixel buffer is locked for the read.
        unsafe {
            let buffer = CMSampleBufferGetImageBuffer(sample.0);
            if CVPixelBufferLockBaseAddress(buffer, READ_ONLY) != 0 {
                return Err(ServiceError::system("a frame could not be read"));
            }
            let base = CVPixelBufferGetBaseAddress(buffer);
            let stride = CVPixelBufferGetBytesPerRow(buffer);
            let width = CVPixelBufferGetWidth(buffer).min(self.width as usize);
            let height = CVPixelBufferGetHeight(buffer).min(self.height as usize);
            if !base.is_null() {
                bgra_to_rgba(
                    std::slice::from_raw_parts(base, stride * height),
                    stride,
                    width,
                    height,
                    self.width as usize,
                    rgba,
                );
            }
            CVPixelBufferUnlockBaseAddress(buffer, READ_ONLY);
            if base.is_null() {
                return Err(ServiceError::system("a frame has no pixels"));
            }
        }
        Ok(Some(FrameTiming {
            pts_ms: pts,
            dts_ms: pts,
        }))
    }
}

/// BGRA rows `stride` bytes apart into RGBA rows `out_width` pixels wide.
fn bgra_to_rgba(
    bgra: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    out_width: usize,
    rgba: &mut [u8],
) {
    for row in 0..height {
        let from = &bgra[row * stride..row * stride + width * 4];
        let to = &mut rgba[row * out_width * 4..row * out_width * 4 + width * 4];
        for (pixel, out) in from.chunks_exact(4).zip(to.chunks_exact_mut(4)) {
            out[0] = pixel[2];
            out[1] = pixel[1];
            out[2] = pixel[0];
            out[3] = pixel[3];
        }
    }
}

impl VideoFrameSource for Source {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn next_frame(&mut self, rgba: &mut [u8]) -> Result<Option<FrameTiming>, ServiceError> {
        let _pool = Pool::new();
        if let Some(pending) = self.pending.take() {
            return self.emit(pending, rgba);
        }
        loop {
            let Some((sample, pts)) = self.decode()? else {
                if self.seek_target.is_some() && self.held.is_none() && self.seek_from > 0.0 {
                    // A reader started past the last frame reads nothing:
                    // start further back until it reads one.
                    let lead = (self.seek_from * 2.0).max(SEEK_LEAD_SECONDS);
                    self.restart(self.seek_from - lead)?;
                    continue;
                }
                // A target past the last frame shows the last frame.
                self.seek_target = None;
                return match self.held.take() {
                    Some(held) => self.emit(held, rgba),
                    None => Ok(None),
                };
            };
            match self.seek_target {
                Some(target) if pts <= target => self.held = Some((sample, pts)),
                Some(_) => {
                    self.seek_target = None;
                    return match self.held.take() {
                        Some(held) => {
                            self.pending = Some((sample, pts));
                            self.emit(held, rgba)
                        }
                        None => self.emit((sample, pts), rgba),
                    };
                }
                None => return self.emit((sample, pts), rgba),
            }
        }
    }

    fn seek(&mut self, position_ms: f64) -> Result<(), ServiceError> {
        let _pool = Pool::new();
        self.held = None;
        self.pending = None;
        self.seek_target = Some(position_ms);
        self.restart(position_ms / 1000.0 - SEEK_LEAD_SECONDS)
    }
}

impl Source {
    /// A new reader from `from` seconds, or the start, in place of the current
    /// one.
    fn restart(&mut self, from: f64) -> Result<(), ServiceError> {
        let from = from.max(0.0);
        // SAFETY: the old reader is cancelled before it is let go; the asset
        // and track are live.
        let (reader, output) = unsafe {
            send!(self.reader.0, sel(c"cancelReading") => ());
            Self::reader(&self.asset, &self.track, Some(from))?
        };
        self.reader = reader;
        self.output = output;
        self.seek_from = from;
        Ok(())
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.held = None;
        self.pending = None;
        // SAFETY: the reader is live; cancelling stops its decoding at once.
        unsafe { send!(self.reader.0, sel(c"cancelReading") => ()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe's clip: ten 64 x 48 frames at ten a second, frame i flat
    /// (i * 25, 255 - i * 25, 128), BT.709 said in the stream.
    fn clip() -> String {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../scripts/fixtures/video-decoder-probe/clip.mp4"
        )
        .to_owned()
    }

    fn near(pixel: &[u8], expected: [i32; 3]) -> bool {
        pixel
            .iter()
            .zip(expected)
            .all(|(&value, want)| (i32::from(value) - want).abs() <= 12)
    }

    #[test]
    fn every_frame_arrives_in_order_with_its_time_and_colour() {
        let mut source = AppleVideoDecoder.open(&clip()).expect("open");
        assert_eq!(source.size(), (64, 48));
        let mut rgba = vec![0u8; 64 * 48 * 4];
        for i in 0..10 {
            let timing = source
                .next_frame(&mut rgba)
                .expect("decode")
                .expect("a frame");
            assert!(
                (timing.pts_ms - f64::from(i) * 100.0).abs() < 1.0,
                "frame {i} at {}",
                timing.pts_ms
            );
            let middle = (24 * 64 + 32) * 4;
            assert!(
                near(&rgba[middle..middle + 3], [i * 25, 255 - i * 25, 128]),
                "frame {i}: {:?}",
                &rgba[middle..middle + 4]
            );
            assert_eq!(rgba[middle + 3], 255);
        }
        assert!(source.next_frame(&mut rgba).expect("end").is_none());
    }

    #[test]
    fn a_seek_continues_from_the_frame_shown_there() {
        let mut source = AppleVideoDecoder.open(&clip()).expect("open");
        let mut rgba = vec![0u8; 64 * 48 * 4];
        source.next_frame(&mut rgba).expect("decode");
        source.seek(450.0).expect("seek");
        let timing = source
            .next_frame(&mut rgba)
            .expect("decode")
            .expect("a frame");
        assert!(
            (timing.pts_ms - 400.0).abs() < 1.0,
            "landed on {}",
            timing.pts_ms
        );
        let next = source
            .next_frame(&mut rgba)
            .expect("decode")
            .expect("a frame");
        assert!((next.pts_ms - 500.0).abs() < 1.0, "then {}", next.pts_ms);
        // Past the end shows the last frame.
        source.seek(5000.0).expect("seek");
        let last = source
            .next_frame(&mut rgba)
            .expect("decode")
            .expect("a frame");
        assert!(
            (last.pts_ms - 900.0).abs() < 1.0,
            "past the end: {}",
            last.pts_ms
        );
    }

    #[test]
    fn a_missing_file_fails_with_why() {
        assert!(AppleVideoDecoder.open("/nonexistent/clip.mp4").is_err());
    }

    #[test]
    fn bgra_rows_become_rgba_rows() {
        // Two pixels a row, rows eight bytes plus four of padding.
        let bgra = [
            1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0,
        ];
        let mut rgba = [0u8; 16];
        bgra_to_rgba(&bgra, 12, 2, 2, 2, &mut rgba);
        assert_eq!(
            rgba,
            [3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]
        );
    }
}
