//! The game recorder's encoder on macOS and iOS: AVFoundation's asset writer.
//!
//! The writer's input takes pixel buffers from a pool it makes, IOSurface
//! backed, and its H.264 encoder is VideoToolbox's -- the hardware one where
//! the device has one. ANGLE draws each frame straight into a pool buffer's
//! IOSurface (`EGL_ANGLE_iosurface_client_buffer`), so a frame is never read
//! back to the CPU: the encoder thread lends a buffer, draws into it, and
//! appends it once the GPU has written it.

use std::{
    collections::HashMap,
    ffi::c_void,
    path::Path,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use graphics::game_recorder::{
    ClientBuffer, EncoderConfig, EncoderInput, VideoEncoder, VideoEncoderFactory,
};
use shared::error::{EngineError, EngineResult, ErrorCode};

use super::objc::{
    Block, BlockDescriptor, CMTime, Cf, Id, Owned, Pool, class, describe, sel, send,
};

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: *const c_void;
    static kCVPixelBufferWidthKey: *const c_void;
    static kCVPixelBufferHeightKey: *const c_void;
    static kCVPixelBufferIOSurfacePropertiesKey: *const c_void;
    static kCVPixelBufferMetalCompatibilityKey: *const c_void;
    fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: *const c_void,
        pool: *mut c_void,
        buffer: *mut *mut c_void,
    ) -> i32;
    fn CVPixelBufferGetIOSurface(buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferRelease(buffer: *mut c_void);
}

#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    fn IOSurfaceGetID(surface: *mut c_void) -> u32;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMTimeMake(value: i64, timescale: i32) -> CMTime;
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeVideo: Id;
    static AVFileTypeMPEG4: Id;
    static AVVideoCodecKey: *const c_void;
    static AVVideoCodecTypeH264: *const c_void;
    static AVVideoWidthKey: *const c_void;
    static AVVideoHeightKey: *const c_void;
    static AVVideoCompressionPropertiesKey: *const c_void;
    static AVVideoAverageBitRateKey: *const c_void;
    static AVVideoMaxKeyFrameIntervalKey: *const c_void;
    static AVVideoExpectedSourceFrameRateKey: *const c_void;
    static AVVideoProfileLevelKey: *const c_void;
    static AVVideoProfileLevelH264HighAutoLevel: *const c_void;
    static AVVideoColorPropertiesKey: *const c_void;
    static AVVideoColorPrimariesKey: *const c_void;
    static AVVideoColorPrimaries_ITU_R_709_2: *const c_void;
    static AVVideoTransferFunctionKey: *const c_void;
    static AVVideoTransferFunction_ITU_R_709_2: *const c_void;
    static AVVideoYCbCrMatrixKey: *const c_void;
    static AVVideoYCbCrMatrix_ITU_R_709_2: *const c_void;
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

/// `kCVPixelFormatType_32BGRA`: the pool's format, one ANGLE binds an IOSurface
/// of as a `GL_BGRA_EXT` texture.
const BGRA: i32 = 0x4247_5241;
/// `AVAssetWriterStatusCompleted`.
const WRITER_COMPLETED: isize = 2;
/// `AVAssetWriterStatusFailed`.
const WRITER_FAILED: isize = 3;
/// How long completing the file may take: it writes the index of what is
/// already encoded, so well past any recording's, and only a stuck writer
/// reaches it.
const FINISH_WITHIN: Duration = Duration::from_secs(30);

// EGL_ANGLE_iosurface_client_buffer.
const EGL_IOSURFACE_ANGLE: khronos_egl::Enum = 0x3454;
const EGL_IOSURFACE_PLANE_ANGLE: khronos_egl::Int = 0x345A;
const EGL_TEXTURE_TYPE_ANGLE: khronos_egl::Int = 0x345C;
const EGL_TEXTURE_INTERNAL_FORMAT_ANGLE: khronos_egl::Int = 0x345D;
const EGL_BIND_TO_TEXTURE_TARGET_ANGLE: khronos_egl::Int = 0x348D;
const GL_BGRA_EXT: khronos_egl::Int = 0x80E1;
const GL_UNSIGNED_BYTE: khronos_egl::Int = 0x1401;
const GL_TEXTURE_2D: u32 = 0x0DE1;

/// The texture target ANGLE binds an IOSurface to: a 2D texture, the
/// `EGL_BIND_TO_TEXTURE_TARGET_ANGLE` every config of the engine's ANGLE
/// (its Metal back end) publishes, on macOS as on iOS. ANGLE's desktop-GL back
/// end would publish rectangle textures; the engine does not ship it.
const SURFACE_TARGET: (khronos_egl::Int, u32) = (khronos_egl::TEXTURE_2D, GL_TEXTURE_2D);

/// The engine's video encoding on Apple platforms.
#[derive(Debug)]
pub struct AppleVideoEncoderFactory;

impl VideoEncoderFactory for AppleVideoEncoderFactory {
    fn available(&self) -> bool {
        // AVAssetWriter and its H.264 encoder are part of every macOS and iOS
        // the engine ships to.
        true
    }

    fn create(&self, config: &EncoderConfig, path: &Path) -> EngineResult<Box<dyn VideoEncoder>> {
        Ok(Box::new(AppleVideoEncoder::new(config, path)?))
    }
}

struct AppleVideoEncoder {
    writer: Owned,
    input: Owned,
    adaptor: Owned,
    width: i32,
    height: i32,
    /// The pool's buffers lent and not yet appended, by IOSurface ID.
    lent: HashMap<u64, *mut c_void>,
    /// The first appended frame's time, the recording's zero.
    first_pts_ns: Option<i64>,
}

// SAFETY: the writer and its buffers are driven from the one encoder thread
// that owns the encoder; AVFoundation's writer may be used from any one thread.
unsafe impl Send for AppleVideoEncoder {}

fn failure(what: impl Into<String>) -> EngineError {
    EngineError::new(ErrorCode::RenderBackendError).with_msg(what.into())
}

impl AppleVideoEncoder {
    fn new(config: &EncoderConfig, path: &Path) -> EngineResult<Self> {
        let _pool = Pool::new();
        // The writer refuses a file that exists.
        let _ = std::fs::remove_file(path);
        let path = Cf::string(&path.to_string_lossy())
            .ok_or_else(|| failure("the recording's path is not text"))?;
        // SAFETY: each message is sent to a live object through its method's
        // prototype; what is kept past the pool is retained.
        unsafe {
            let url: Id = send!(class(c"NSURL"), sel(c"fileURLWithPath:"), path.id() => Id; Id);
            let mut error: Id = std::ptr::null_mut();
            let writer = send!(class(c"AVAssetWriter"), sel(c"assetWriterWithURL:fileType:error:"),
                url => Id, AVFileTypeMPEG4 => Id, &mut error as *mut Id => *mut Id; Id);
            let writer = Owned::retain(writer)
                .ok_or_else(|| failure(describe("making the recording's file", error)))?;

            let bitrate = Cf::i64(i64::from(config.bitrate_bps));
            let key_frames = Cf::i32(config.key_frame_interval as i32);
            let fps = Cf::i32(config.fps as i32);
            let compression = Cf::dictionary(&[
                (AVVideoAverageBitRateKey, bitrate.0),
                (AVVideoMaxKeyFrameIntervalKey, key_frames.0),
                (AVVideoExpectedSourceFrameRateKey, fps.0),
                (AVVideoProfileLevelKey, AVVideoProfileLevelH264HighAutoLevel),
            ]);
            // The game draws sRGB, which is BT.709: converted with its matrix,
            // and said in the stream -- untagged, every player guesses, and one
            // that guesses BT.601 shows the game's red as a darker red.
            let colour = Cf::dictionary(&[
                (AVVideoColorPrimariesKey, AVVideoColorPrimaries_ITU_R_709_2),
                (
                    AVVideoTransferFunctionKey,
                    AVVideoTransferFunction_ITU_R_709_2,
                ),
                (AVVideoYCbCrMatrixKey, AVVideoYCbCrMatrix_ITU_R_709_2),
            ]);
            let width = Cf::i32(config.width as i32);
            let height = Cf::i32(config.height as i32);
            let settings = Cf::dictionary(&[
                (AVVideoCodecKey, AVVideoCodecTypeH264),
                (AVVideoWidthKey, width.0),
                (AVVideoHeightKey, height.0),
                (AVVideoCompressionPropertiesKey, compression.0),
                (AVVideoColorPropertiesKey, colour.0),
            ]);
            let input = send!(class(c"AVAssetWriterInput"), sel(c"assetWriterInputWithMediaType:outputSettings:"),
                AVMediaTypeVideo => Id, settings.id() => Id; Id);
            let input = Owned::retain(input)
                .ok_or_else(|| failure("the encoder does not take these settings"))?;
            // Frames come as the game draws them: the writer never holds the
            // encoder thread waiting to interleave.
            send!(input.0, sel(c"setExpectsMediaDataInRealTime:"), 1i8 => i8; ());

            let format = Cf::i32(BGRA);
            let surface = Cf::dictionary(&[]);
            let buffers = Cf::dictionary(&[
                (kCVPixelBufferPixelFormatTypeKey, format.0),
                (kCVPixelBufferWidthKey, width.0),
                (kCVPixelBufferHeightKey, height.0),
                (kCVPixelBufferIOSurfacePropertiesKey, surface.0),
                (
                    kCVPixelBufferMetalCompatibilityKey,
                    super::objc::kCFBooleanTrue,
                ),
            ]);
            let adaptor = send!(class(c"AVAssetWriterInputPixelBufferAdaptor"),
                sel(c"assetWriterInputPixelBufferAdaptorWithAssetWriterInput:sourcePixelBufferAttributes:"),
                input.0 => Id, buffers.id() => Id; Id);
            let adaptor = Owned::retain(adaptor)
                .ok_or_else(|| failure("the encoder's input takes no pixel buffers"))?;

            if send!(writer.0, sel(c"canAddInput:"), input.0 => Id; i8) == 0 {
                return Err(failure(
                    "the recording's file takes no video of these settings",
                ));
            }
            send!(writer.0, sel(c"addInput:"), input.0 => Id; ());
            if send!(writer.0, sel(c"startWriting") => i8) == 0 {
                let error: Id = send!(writer.0, sel(c"error") => Id);
                return Err(failure(describe("starting the recording", error)));
            }
            send!(writer.0, sel(c"startSessionAtSourceTime:"), CMTimeMake(0, 1) => CMTime; ());
            Ok(Self {
                writer,
                input,
                adaptor,
                width: config.width as i32,
                height: config.height as i32,
                lent: HashMap::new(),
                first_pts_ns: None,
            })
        }
    }

    /// The writer's failure, if it has failed.
    fn failed(&self, what: &str) -> Option<EngineError> {
        // SAFETY: messages to the live writer.
        unsafe {
            if send!(self.writer.0, sel(c"status") => isize) != WRITER_FAILED {
                return None;
            }
            let error: Id = send!(self.writer.0, sel(c"error") => Id);
            Some(failure(describe(what, error)))
        }
    }

    fn release_lent(&mut self) {
        for (_, buffer) in self.lent.drain() {
            // SAFETY: a buffer the pool made for this encoder, released once.
            unsafe { CVPixelBufferRelease(buffer) };
        }
    }
}

impl VideoEncoder for AppleVideoEncoder {
    fn input(&self) -> EncoderInput {
        EncoderInput::ClientBuffers
    }

    fn lend(&mut self) -> EngineResult<Option<ClientBuffer>> {
        let _pool = Pool::new();
        if let Some(error) = self.failed("encoding the recording") {
            return Err(error);
        }
        // SAFETY: messages to the live input and adaptor; the pool's buffer is
        // owned by this encoder until it is appended or released.
        unsafe {
            if send!(self.input.0, sel(c"isReadyForMoreMediaData") => i8) == 0 {
                return Ok(None);
            }
            let pool: *mut c_void = send!(self.adaptor.0, sel(c"pixelBufferPool") => *mut c_void);
            if pool.is_null() {
                return Err(failure("the encoder has no pixel buffers"));
            }
            let mut buffer: *mut c_void = std::ptr::null_mut();
            let status = CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut buffer);
            if status != 0 || buffer.is_null() {
                return Err(failure(format!(
                    "the encoder lent no pixel buffer ({status})"
                )));
            }
            let surface = CVPixelBufferGetIOSurface(buffer);
            if surface.is_null() {
                CVPixelBufferRelease(buffer);
                return Err(failure("the encoder's pixel buffer has no IOSurface"));
            }
            let key = u64::from(IOSurfaceGetID(surface));
            if let Some(stale) = self.lent.insert(key, buffer) {
                CVPixelBufferRelease(stale);
            }
            let (egl_target, gl_target) = SURFACE_TARGET;
            Ok(Some(ClientBuffer {
                buffer_type: EGL_IOSURFACE_ANGLE,
                handle: surface,
                key,
                attribs: vec![
                    khronos_egl::WIDTH,
                    self.width,
                    khronos_egl::HEIGHT,
                    self.height,
                    EGL_IOSURFACE_PLANE_ANGLE,
                    0,
                    khronos_egl::TEXTURE_TARGET,
                    egl_target,
                    EGL_TEXTURE_INTERNAL_FORMAT_ANGLE,
                    GL_BGRA_EXT,
                    khronos_egl::TEXTURE_FORMAT,
                    khronos_egl::TEXTURE_RGBA,
                    EGL_TEXTURE_TYPE_ANGLE,
                    GL_UNSIGNED_BYTE,
                ],
                config_attribs: vec![
                    khronos_egl::RED_SIZE,
                    8,
                    khronos_egl::GREEN_SIZE,
                    8,
                    khronos_egl::BLUE_SIZE,
                    8,
                    khronos_egl::ALPHA_SIZE,
                    8,
                    khronos_egl::SURFACE_TYPE,
                    khronos_egl::PBUFFER_BIT,
                    EGL_BIND_TO_TEXTURE_TARGET_ANGLE,
                    egl_target,
                ],
                texture_target: gl_target,
                // An IOSurface's first row is the picture's top.
                top_row_first: true,
            }))
        }
    }

    fn submit(&mut self, buffer: ClientBuffer, pts_ns: i64) -> EngineResult<()> {
        let _pool = Pool::new();
        let Some(pixels) = self.lent.remove(&buffer.key) else {
            return Err(failure(
                "a frame was handed back in a buffer the encoder did not lend",
            ));
        };
        let first = *self.first_pts_ns.get_or_insert(pts_ns);
        // SAFETY: the buffer this encoder lent, appended to its live adaptor
        // and released once.
        let appended = unsafe {
            let time = CMTimeMake((pts_ns - first).max(0), 1_000_000_000);
            let appended = send!(self.adaptor.0, sel(c"appendPixelBuffer:withPresentationTime:"),
                pixels => *mut c_void, time => CMTime; i8);
            CVPixelBufferRelease(pixels);
            appended != 0
        };
        if appended {
            Ok(())
        } else {
            Err(self
                .failed("encoding a frame")
                .unwrap_or_else(|| failure("the encoder refused a frame")))
        }
    }

    fn drain(&mut self) -> EngineResult<()> {
        // The writer writes what it has encoded on its own: what is asked here
        // is whether it still can.
        match self.failed("writing the recording") {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn finish(mut self: Box<Self>) -> EngineResult<()> {
        self.release_lent();
        let _pool = Pool::new();
        let done = Arc::new(Done::default());
        let block = Block::new(Arc::into_raw(Arc::clone(&done)), finished, &FINISHED);
        // SAFETY: messages to the live input and writer; the writer copies the
        // block and calls it once, and the copy owns the reference it carries.
        unsafe {
            send!(self.input.0, sel(c"markAsFinished") => ());
            send!(self.writer.0, sel(c"finishWritingWithCompletionHandler:"),
                &raw const block as *const c_void => *const c_void; ());
        }
        if !done.wait(FINISH_WITHIN) {
            return Err(failure("completing the recording's file took too long"));
        }
        // SAFETY: a message to the live writer.
        let status = unsafe { send!(self.writer.0, sel(c"status") => isize) };
        if status == WRITER_COMPLETED {
            Ok(())
        } else {
            Err(self
                .failed("completing the recording's file")
                .unwrap_or_else(|| failure("the recording's file was not completed")))
        }
    }

    fn discard(mut self: Box<Self>) {
        self.release_lent();
        let _pool = Pool::new();
        // SAFETY: a message to the live writer.
        unsafe { send!(self.writer.0, sel(c"cancelWriting") => ()) };
    }
}

impl Drop for AppleVideoEncoder {
    fn drop(&mut self) {
        self.release_lent();
    }
}

/// Whether the writer has called its completion handler.
#[derive(Default)]
struct Done {
    finished: Mutex<bool>,
    signal: Condvar,
}

impl Done {
    fn wait(&self, within: Duration) -> bool {
        let finished = self.finished.lock().unwrap_or_else(|e| e.into_inner());
        let (finished, _) = self
            .signal
            .wait_timeout_while(finished, within, |finished| !*finished)
            .unwrap_or_else(|e| e.into_inner());
        *finished
    }
}

static FINISHED: BlockDescriptor = Block::<*const Done>::descriptor();

/// The completion handler: the block's reference to `Done` is taken back and
/// released after it signals.
unsafe extern "C" fn finished(block: *const Block<*const Done>) {
    // SAFETY: the block carries a pointer from `Arc::into_raw`, and the writer
    // calls it once.
    let done = unsafe { Arc::from_raw((*block).context) };
    *done.finished.lock().unwrap_or_else(|e| e.into_inner()) = true;
    done.signal.notify_all();
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::apple::video_decoder::AppleVideoDecoder;
    use shared::services::VideoDecoderService;

    #[link(name = "IOSurface", kind = "framework")]
    unsafe extern "C" {
        fn IOSurfaceLock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceUnlock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceGetBaseAddress(surface: *mut c_void) -> *mut u8;
        fn IOSurfaceGetBytesPerRow(surface: *mut c_void) -> usize;
    }

    const WIDTH: usize = 64;
    const HEIGHT: usize = 48;

    /// Paint a lent buffer as the GPU would: its top half `top`, its bottom
    /// half `bottom`, BGRA rows top row first.
    fn paint(buffer: &ClientBuffer, top: [u8; 3], bottom: [u8; 3]) {
        // SAFETY: the IOSurface of a buffer the encoder lent, locked for the
        // writes and within its rows.
        unsafe {
            assert_eq!(IOSurfaceLock(buffer.handle, 0, std::ptr::null_mut()), 0);
            let base = IOSurfaceGetBaseAddress(buffer.handle);
            let stride = IOSurfaceGetBytesPerRow(buffer.handle);
            for y in 0..HEIGHT {
                let [r, g, b] = if y < HEIGHT / 2 { top } else { bottom };
                let row = std::slice::from_raw_parts_mut(base.add(y * stride), WIDTH * 4);
                for pixel in row.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[b, g, r, 255]);
                }
            }
            assert_eq!(IOSurfaceUnlock(buffer.handle, 0, std::ptr::null_mut()), 0);
        }
    }

    fn near(pixel: &[u8], expected: [i32; 3]) -> bool {
        pixel
            .iter()
            .zip(expected)
            .all(|(&value, want)| (i32::from(value) - want).abs() <= 12)
    }

    #[test]
    fn the_frames_drawn_into_its_buffers_are_the_video() {
        let dir = std::env::temp_dir().join(format!("migo-apple-recorder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("replay.mp4.part");
        let config = EncoderConfig {
            width: WIDTH as u32,
            height: HEIGHT as u32,
            fps: 10,
            bitrate_bps: 1_000_000,
            key_frame_interval: 10,
        };
        let mut encoder = AppleVideoEncoderFactory
            .create(&config, &part)
            .expect("an encoder");
        assert!(matches!(encoder.input(), EncoderInput::ClientBuffers));
        // A recording's frames are stamped with the clock's time, not from zero.
        let base_ns = 5_000_000_000i64;
        for i in 0..10i64 {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let buffer = loop {
                if let Some(buffer) = encoder.lend().expect("lend") {
                    break buffer;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the encoder took no frame {i}"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(buffer.buffer_type, EGL_IOSURFACE_ANGLE);
            assert!(buffer.top_row_first);
            if i < 5 {
                paint(&buffer, [255, 0, 0], [0, 0, 255]);
            } else {
                paint(&buffer, [0, 255, 0], [0, 255, 0]);
            }
            encoder
                .submit(buffer, base_ns + i * 100_000_000)
                .expect("submit");
        }
        encoder.finish().expect("the file completes");
        let video = dir.join("replay.mp4");
        std::fs::rename(&part, &video).unwrap();

        let mut source = AppleVideoDecoder
            .open(&video.to_string_lossy())
            .expect("the recording opens");
        assert_eq!(source.size(), (WIDTH as u32, HEIGHT as u32));
        let mut rgba = vec![0u8; WIDTH * HEIGHT * 4];
        let at = |x: usize, y: usize| (y * WIDTH + x) * 4;
        let mut frames = 0;
        while let Some(timing) = source.next_frame(&mut rgba).expect("decode") {
            if frames == 0 {
                assert!(
                    timing.pts_ms.abs() < 1.0,
                    "the video starts at its first frame: {}",
                    timing.pts_ms
                );
                let (top, bottom) = (at(32, 4), at(32, HEIGHT - 4));
                assert!(
                    near(&rgba[top..top + 3], [255, 0, 0]),
                    "top {:?}",
                    &rgba[top..top + 4]
                );
                assert!(
                    near(&rgba[bottom..bottom + 3], [0, 0, 255]),
                    "bottom {:?}",
                    &rgba[bottom..bottom + 4]
                );
            }
            frames += 1;
        }
        assert_eq!(frames, 10);
        let middle = at(32, 24);
        assert!(
            near(&rgba[middle..middle + 3], [0, 255, 0]),
            "last {:?}",
            &rgba[middle..middle + 4]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_discarded_recording_completes_no_file() {
        let dir = std::env::temp_dir().join(format!(
            "migo-apple-recorder-discard-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("replay.mp4.part");
        let config = EncoderConfig {
            width: WIDTH as u32,
            height: HEIGHT as u32,
            fps: 10,
            bitrate_bps: 1_000_000,
            key_frame_interval: 10,
        };
        let mut encoder = AppleVideoEncoderFactory
            .create(&config, &part)
            .expect("an encoder");
        let buffer = encoder.lend().expect("lend").expect("a buffer");
        paint(&buffer, [255, 0, 0], [255, 0, 0]);
        encoder.submit(buffer, 0).expect("submit");
        // A buffer lent and never handed back is the encoder's to release.
        let _kept = encoder.lend().expect("lend");
        encoder.discard();
        assert!(
            AppleVideoDecoder.open(&part.to_string_lossy()).is_err(),
            "nothing playable is left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
