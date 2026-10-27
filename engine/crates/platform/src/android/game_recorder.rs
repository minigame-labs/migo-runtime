//! `getGameRecorder` on Android: the NDK's H.264 encoder, fed through its own
//! input surface, and its muxer writing the MP4.
//!
//! The encoder thread draws each frame into the surface (`graphics::
//! game_recorder`); this drains what the encoder made into the muxer, which
//! starts once the encoder has said what its stream is.

use std::{
    ffi::{CStr, c_char, c_void},
    fs::File,
    os::fd::AsRawFd,
    path::Path,
    ptr,
    time::{Duration, Instant},
};

use graphics::game_recorder::{EncoderConfig, EncoderInput, VideoEncoder, VideoEncoderFactory};
use shared::error::{EngineError, EngineResult, ErrorCode};

use crate::android::{
    media_ndk::{
        AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG, AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
        AMEDIACODEC_CONFIGURE_FLAG_ENCODE, AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED,
        AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED, AMEDIACODEC_INFO_TRY_AGAIN_LATER, AMediaCodec,
        AMediaCodec_configure, AMediaCodec_createEncoderByType, AMediaCodec_createInputSurface,
        AMediaCodec_delete, AMediaCodec_dequeueOutputBuffer, AMediaCodec_getOutputBuffer,
        AMediaCodec_getOutputFormat, AMediaCodec_releaseOutputBuffer,
        AMediaCodec_signalEndOfInputStream, AMediaCodec_start, AMediaCodec_stop,
        AMediaCodecBufferInfo, AMediaFormat, AMediaFormat_delete, AMediaFormat_new,
        AMediaFormat_setFloat, AMediaFormat_setInt32, AMediaFormat_setString,
    },
    surface::{ANativeWindow, ANativeWindow_release},
};

#[repr(C)]
struct AMediaMuxer(c_void);

const AMEDIAMUXER_OUTPUT_FORMAT_MPEG_4: i32 = 0;
/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface`.
const COLOR_FORMAT_SURFACE: i32 = 0x7F00_0789;
/// `MediaFormat.COLOR_STANDARD_BT709`, `COLOR_TRANSFER_SDR_VIDEO` and
/// `COLOR_RANGE_LIMITED`: the game draws sRGB, which is BT.709, and the stream
/// says so -- untagged, every player guesses the matrix, and one that guesses
/// BT.601 shows the game's red as a darker red.
const COLOR_STANDARD_BT709: i32 = 1;
const COLOR_TRANSFER_SDR_VIDEO: i32 = 3;
const COLOR_RANGE_LIMITED: i32 = 2;
const MIME: &CStr = c"video/avc";
/// How long the last of the stream may take to come out once input has ended.
const FINISH: Duration = Duration::from_secs(3);

#[link(name = "mediandk")]
unsafe extern "C" {
    fn AMediaMuxer_new(fd: i32, format: i32) -> *mut AMediaMuxer;
    fn AMediaMuxer_delete(muxer: *mut AMediaMuxer) -> i32;
    fn AMediaMuxer_addTrack(muxer: *mut AMediaMuxer, format: *const AMediaFormat) -> isize;
    fn AMediaMuxer_start(muxer: *mut AMediaMuxer) -> i32;
    fn AMediaMuxer_stop(muxer: *mut AMediaMuxer) -> i32;
    fn AMediaMuxer_writeSampleData(
        muxer: *mut AMediaMuxer,
        track: usize,
        data: *const u8,
        info: *const AMediaCodecBufferInfo,
    ) -> i32;
}

fn key(name: &CStr) -> *const c_char {
    name.as_ptr()
}

fn failure(what: &'static str, status: impl std::fmt::Display) -> EngineError {
    EngineError::new(ErrorCode::RenderBackendError)
        .with_msg(what)
        .with_detail(status.to_string())
}

/// The engine's encoders on Android: every device the SDK runs on (API 26 and
/// later) has an H.264 encoder that takes a surface.
#[derive(Debug)]
pub struct AndroidVideoEncoderFactory;

impl VideoEncoderFactory for AndroidVideoEncoderFactory {
    fn available(&self) -> bool {
        true
    }

    fn create(&self, config: &EncoderConfig, path: &Path) -> EngineResult<Box<dyn VideoEncoder>> {
        Ok(Box::new(AndroidVideoEncoder::new(config, path)?))
    }
}

struct AndroidVideoEncoder {
    codec: *mut AMediaCodec,
    window: *mut ANativeWindow,
    muxer: *mut AMediaMuxer,
    /// The muxer writes into this; it stays open while the muxer lives.
    _file: File,
    track: Option<usize>,
}

// SAFETY: the codec, its window and the muxer are used from the encoder thread
// alone once created; the NDK objects are not tied to the thread that made them.
unsafe impl Send for AndroidVideoEncoder {}

impl AndroidVideoEncoder {
    fn new(config: &EncoderConfig, path: &Path) -> EngineResult<Self> {
        let file = File::create(path).map_err(|e| failure("the recording's file", e))?;
        unsafe {
            let codec = AMediaCodec_createEncoderByType(MIME.as_ptr());
            if codec.is_null() {
                return Err(failure("an H.264 encoder", "none"));
            }
            let format = AMediaFormat_new();
            AMediaFormat_setString(format, key(c"mime"), MIME.as_ptr());
            AMediaFormat_setInt32(format, key(c"width"), config.width as i32);
            AMediaFormat_setInt32(format, key(c"height"), config.height as i32);
            AMediaFormat_setInt32(format, key(c"color-format"), COLOR_FORMAT_SURFACE);
            AMediaFormat_setInt32(format, key(c"color-standard"), COLOR_STANDARD_BT709);
            AMediaFormat_setInt32(format, key(c"color-transfer"), COLOR_TRANSFER_SDR_VIDEO);
            AMediaFormat_setInt32(format, key(c"color-range"), COLOR_RANGE_LIMITED);
            AMediaFormat_setInt32(format, key(c"bitrate"), config.bitrate_bps as i32);
            AMediaFormat_setInt32(format, key(c"frame-rate"), config.fps as i32);
            // In seconds; fractional from API 25.
            let interval = config.key_frame_interval as f32 / config.fps.max(1) as f32;
            AMediaFormat_setFloat(format, key(c"i-frame-interval"), interval);
            let configured = AMediaCodec_configure(
                codec,
                format,
                ptr::null_mut(),
                ptr::null_mut(),
                AMEDIACODEC_CONFIGURE_FLAG_ENCODE,
            );
            AMediaFormat_delete(format);
            if configured != 0 {
                AMediaCodec_delete(codec);
                return Err(failure("configuring the encoder", configured));
            }
            let mut window = ptr::null_mut();
            let surfaced = AMediaCodec_createInputSurface(codec, &mut window);
            if surfaced != 0 || window.is_null() {
                AMediaCodec_delete(codec);
                return Err(failure("the encoder's input surface", surfaced));
            }
            let started = AMediaCodec_start(codec);
            if started != 0 {
                ANativeWindow_release(window);
                AMediaCodec_delete(codec);
                return Err(failure("starting the encoder", started));
            }
            let muxer = AMediaMuxer_new(file.as_raw_fd(), AMEDIAMUXER_OUTPUT_FORMAT_MPEG_4);
            if muxer.is_null() {
                AMediaCodec_stop(codec);
                ANativeWindow_release(window);
                AMediaCodec_delete(codec);
                return Err(failure("the recording's muxer", "none"));
            }
            Ok(Self {
                codec,
                window,
                muxer,
                _file: file,
                track: None,
            })
        }
    }

    /// Write out what the encoder has made, waiting up to `timeout_us` for each
    /// buffer. `true` once the end of the stream has come out.
    fn drain_for(&mut self, timeout_us: i64) -> EngineResult<bool> {
        loop {
            let mut info = AMediaCodecBufferInfo::default();
            let index =
                unsafe { AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, timeout_us) };
            match index {
                AMEDIACODEC_INFO_TRY_AGAIN_LATER => return Ok(false),
                AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED => continue,
                AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED => {
                    // What the stream is, once: the muxer starts with it.
                    unsafe {
                        let format = AMediaCodec_getOutputFormat(self.codec);
                        let track = AMediaMuxer_addTrack(self.muxer, format);
                        AMediaFormat_delete(format);
                        if track < 0 {
                            return Err(failure("the recording's track", track));
                        }
                        let started = AMediaMuxer_start(self.muxer);
                        if started != 0 {
                            return Err(failure("starting the muxer", started));
                        }
                        self.track = Some(track as usize);
                    }
                }
                index if index < 0 => return Err(failure("the encoder's output", index)),
                index => {
                    let index = index as usize;
                    let end = info.flags & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0;
                    let config = info.flags & AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG != 0;
                    let written = unsafe {
                        let mut size = 0usize;
                        let data = AMediaCodec_getOutputBuffer(self.codec, index, &mut size);
                        // The codec's configuration reached the muxer in its
                        // format; an empty buffer carries only a flag.
                        let result = match self.track {
                            Some(track) if !config && info.size > 0 && !data.is_null() => {
                                AMediaMuxer_writeSampleData(
                                    self.muxer,
                                    track,
                                    data.add(info.offset as usize),
                                    &info,
                                )
                            }
                            _ => 0,
                        };
                        AMediaCodec_releaseOutputBuffer(self.codec, index, false);
                        result
                    };
                    if written != 0 {
                        return Err(failure("writing the recording", written));
                    }
                    if end {
                        return Ok(true);
                    }
                }
            }
        }
    }

    fn release(&mut self) {
        unsafe {
            if !self.muxer.is_null() {
                AMediaMuxer_delete(self.muxer);
                self.muxer = ptr::null_mut();
            }
            if !self.codec.is_null() {
                AMediaCodec_stop(self.codec);
                AMediaCodec_delete(self.codec);
                self.codec = ptr::null_mut();
            }
            if !self.window.is_null() {
                ANativeWindow_release(self.window);
                self.window = ptr::null_mut();
            }
        }
    }
}

impl VideoEncoder for AndroidVideoEncoder {
    fn input(&self) -> EncoderInput {
        EncoderInput::Window(self.window.cast())
    }

    fn drain(&mut self) -> EngineResult<()> {
        self.drain_for(0).map(|_| ())
    }

    fn finish(mut self: Box<Self>) -> EngineResult<()> {
        let ended = unsafe { AMediaCodec_signalEndOfInputStream(self.codec) };
        if ended != 0 {
            return Err(failure("ending the encoder's input", ended));
        }
        let deadline = Instant::now() + FINISH;
        while !self.drain_for(10_000)? {
            if Instant::now() >= deadline {
                return Err(failure("the end of the recording", "never came out"));
            }
        }
        if self.track.is_none() {
            return Err(failure("the recording", "no frame was encoded"));
        }
        let stopped = unsafe { AMediaMuxer_stop(self.muxer) };
        if stopped != 0 {
            return Err(failure("completing the recording", stopped));
        }
        Ok(())
    }

    fn discard(self: Box<Self>) {}
}

impl Drop for AndroidVideoEncoder {
    fn drop(&mut self) {
        self.release();
    }
}
