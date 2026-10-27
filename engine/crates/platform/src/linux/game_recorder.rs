//! The game recorder's encoder on Linux: GStreamer, with the system's H.264
//! encoder.
//!
//! Each frame is read back by the encoder thread and pushed into an `appsrc`,
//! through `videoconvert` to I420 in BT.709 -- the game draws sRGB -- an H.264
//! encoder and `mp4mux`. The encoder is the first of these the system has:
//! VA-API's (`vah264enc`, then `vaapih264enc`), x264's (`x264enc`), OpenH264's
//! (`openh264enc`). All but x264 hand out a byte stream the muxer takes only
//! through `h264parse` (the bad plugins), so they need it there too. A system
//! with none cannot record, and says so.
//!
//! The source blocks while the encoder is behind: the encoder thread waits,
//! the render thread's textures stay taken, and frames are not taken until it
//! catches up -- the game itself is never held.

use std::{
    ffi::{CString, c_char, c_void},
    path::Path,
    sync::OnceLock,
};

use graphics::game_recorder::{EncoderConfig, FrameInput, VideoEncoder, VideoEncoderFactory};
use shared::error::{EngineError, EngineResult, ErrorCode};

use super::gst::{
    GST_MAP_WRITE, GST_MESSAGE_EOS, GST_MESSAGE_ERROR, GST_STATE_CHANGE_FAILURE, GST_STATE_NULL,
    GST_STATE_PLAYING, Gst, GstMapInfo, GstMessage,
};

/// How long completing the file may take: it writes what is already encoded
/// and the file's index, so only a stuck pipeline reaches it.
const FINISH_WITHIN_NS: u64 = 30_000_000_000;
/// Frames the source holds before a push waits for the encoder.
const QUEUED_FRAMES: usize = 3;

/// An H.264 encoder element, and how it is told the recording's settings.
struct Element {
    name: &'static str,
    settings: fn(&EncoderConfig) -> String,
    /// Whether its stream reaches the muxer only through `h264parse`.
    needs_parser: bool,
}

/// The encoders tried, in order: the hardware's, then software.
const ELEMENTS: &[Element] = &[
    Element {
        name: "vah264enc",
        settings: |c| {
            format!(
                "bitrate={} key-int-max={}",
                c.bitrate_bps / 1000,
                c.key_frame_interval
            )
        },
        needs_parser: true,
    },
    Element {
        name: "vaapih264enc",
        settings: |c| {
            format!(
                "bitrate={} keyframe-period={}",
                c.bitrate_bps / 1000,
                c.key_frame_interval
            )
        },
        needs_parser: true,
    },
    Element {
        name: "x264enc",
        settings: |c| {
            format!(
                "bitrate={} key-int-max={} speed-preset=veryfast tune=zerolatency",
                c.bitrate_bps / 1000,
                c.key_frame_interval
            )
        },
        needs_parser: false,
    },
    Element {
        name: "openh264enc",
        settings: |c| {
            format!(
                "bitrate={} gop-size={}",
                c.bitrate_bps, c.key_frame_interval
            )
        },
        needs_parser: true,
    },
];

/// What the system records with, looked for once: initialising GStreamer may
/// scan its plugins.
struct Chosen {
    gst: &'static Gst,
    encoder: &'static Element,
    /// Whether `h264parse` is there, between the encoder and the muxer.
    parser: bool,
}

fn chosen() -> Option<&'static Chosen> {
    static CHOSEN: OnceLock<Option<Chosen>> = OnceLock::new();
    CHOSEN
        .get_or_init(|| {
            let gst = Gst::initialised().ok()?;
            // Muxing is base's and good's: a system without them records nothing.
            let has = |name: &str| {
                let name = CString::new(name).expect("element names have no NUL");
                // SAFETY: looking a factory up by name; the reference it returns is
                // ours, released at once.
                unsafe {
                    let factory = (gst.element_factory_find)(name.as_ptr());
                    if factory.is_null() {
                        return false;
                    }
                    (gst.object_unref)(factory);
                    true
                }
            };
            let base = ["appsrc", "videoconvert", "mp4mux", "filesink"];
            if !base.iter().all(|name| has(name)) {
                return None;
            }
            let parser = has("h264parse");
            ELEMENTS
                .iter()
                .find(|element| (parser || !element.needs_parser) && has(element.name))
                .map(|encoder| Chosen {
                    gst,
                    encoder,
                    parser,
                })
        })
        .as_ref()
}

fn failure(what: impl Into<String>) -> EngineError {
    EngineError::new(ErrorCode::RenderBackendError).with_msg(what.into())
}

/// The engine's video encoding on Linux.
#[derive(Debug)]
pub struct GstVideoEncoderFactory;

impl GstVideoEncoderFactory {
    /// The factory, its encoder looked for on a thread of its own from now:
    /// initialising GStreamer may scan its plugins, which content's
    /// `isFrameSupported()` -- answered on the render thread while content
    /// waits -- should not be the one to pay for.
    pub fn probing() -> Self {
        if Gst::loaded().is_some() {
            let probe = std::thread::Builder::new()
                .name("Migo-GstEncoders".into())
                .spawn(|| {
                    chosen();
                });
            if let Err(e) = probe {
                tracing::warn!("game recorder: the encoder probe could not start: {e}");
            }
        }
        Self
    }
}

impl VideoEncoderFactory for GstVideoEncoderFactory {
    fn available(&self) -> bool {
        // Without GStreamer there is nothing to initialise: a `dlopen` answers.
        Gst::loaded().is_some() && chosen().is_some()
    }

    fn input(&self) -> FrameInput {
        // Read back by the render thread: an X11 display has no second
        // context to draw into an encoder from.
        FrameInput::Memory
    }

    fn create(&self, config: &EncoderConfig, path: &Path) -> EngineResult<Box<dyn VideoEncoder>> {
        let chosen = chosen().ok_or_else(|| failure("no H.264 encoder"))?;
        Ok(Box::new(GstVideoEncoder::new(chosen, config, path)?))
    }
}

/// `text` as a quoted value of a pipeline description.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

struct GstVideoEncoder {
    gst: &'static Gst,
    pipeline: *mut c_void,
    source: *mut c_void,
    bus: *mut c_void,
    /// One frame's bytes, and its length in time.
    frame_bytes: usize,
    frame_ns: u64,
    /// The first frame's time, the recording's zero.
    first_pts_ns: Option<i64>,
}

// SAFETY: the pipeline is driven from the one encoder thread that owns the
// encoder; GStreamer's objects may be used from any thread.
unsafe impl Send for GstVideoEncoder {}

impl GstVideoEncoder {
    fn new(chosen: &Chosen, config: &EncoderConfig, path: &Path) -> EngineResult<Self> {
        let Chosen {
            gst,
            encoder: element,
            parser,
        } = *chosen;
        let frame_bytes = config.width as usize * config.height as usize * 4;
        let description = format!(
            "appsrc name=frames is-live=true block=true format=time max-bytes={max} \
             caps=video/x-raw,format=RGBA,width={w},height={h},framerate={fps}/1 \
             ! videoconvert ! video/x-raw,format=I420,colorimetry=bt709 \
             ! {encoder} {settings} {parse}! mp4mux ! filesink location={location}",
            max = frame_bytes * QUEUED_FRAMES,
            w = config.width,
            h = config.height,
            fps = config.fps,
            encoder = element.name,
            settings = (element.settings)(config),
            parse = if parser { "! h264parse " } else { "" },
            location = quoted(&path.to_string_lossy()),
        );
        let description =
            CString::new(description).map_err(|_| failure("the recording's path has a NUL"))?;
        // SAFETY: GStreamer is initialised; each object is held by the
        // encoder and released in `close`, once.
        unsafe {
            let mut error = std::ptr::null_mut();
            let pipeline = (gst.parse_launch)(description.as_ptr(), &mut error);
            let why = gst.take_error(error);
            if pipeline.is_null() {
                return Err(failure(format!(
                    "the recording's pipeline: {}",
                    why.unwrap_or_default()
                )));
            }
            let pipeline = (gst.object_ref_sink)(pipeline);
            let source = (gst.bin_get_by_name)(pipeline, c"frames".as_ptr());
            let bus = (gst.element_get_bus)(pipeline);
            let mut encoder = Self {
                gst,
                pipeline,
                source,
                bus,
                frame_bytes,
                frame_ns: 1_000_000_000 / u64::from(config.fps.max(1)),
                first_pts_ns: None,
            };
            if source.is_null() || bus.is_null() {
                encoder.close();
                return Err(failure("the recording's pipeline has no source"));
            }
            if (gst.element_set_state)(pipeline, GST_STATE_PLAYING) == GST_STATE_CHANGE_FAILURE {
                let why = encoder
                    .error()
                    .unwrap_or_else(|| "it would not start".into());
                encoder.close();
                return Err(failure(format!("the recording's pipeline: {why}")));
            }
            Ok(encoder)
        }
    }

    /// The error the pipeline posted, if it posted one.
    fn error(&self) -> Option<String> {
        // SAFETY: the encoder's live bus; a message popped is ours.
        unsafe {
            let message = (self.gst.bus_pop_filtered)(self.bus, GST_MESSAGE_ERROR);
            (!message.is_null()).then(|| self.describe(message.cast()))
        }
    }

    /// What `message`, an error, says; it is released.
    unsafe fn describe(&self, message: *mut GstMessage) -> String {
        // SAFETY: a message the bus handed over, parsed and released once.
        unsafe {
            let mut error = std::ptr::null_mut();
            let mut debug: *mut c_char = std::ptr::null_mut();
            (self.gst.message_parse_error)(message.cast(), &mut error, &mut debug);
            if !debug.is_null() {
                (self.gst.free)(debug.cast());
            }
            (self.gst.mini_object_unref)(message.cast());
            self.gst
                .take_error(error)
                .unwrap_or_else(|| "the pipeline failed".into())
        }
    }

    /// Stop the pipeline and release what the encoder holds.
    fn close(&mut self) {
        // SAFETY: the encoder's objects, released once and forgotten.
        unsafe {
            if !self.pipeline.is_null() {
                (self.gst.element_set_state)(self.pipeline, GST_STATE_NULL);
            }
            for object in [&mut self.source, &mut self.bus, &mut self.pipeline] {
                if !object.is_null() {
                    (self.gst.object_unref)(*object);
                    *object = std::ptr::null_mut();
                }
            }
        }
    }
}

impl VideoEncoder for GstVideoEncoder {
    fn submit_pixels(&mut self, rgba: &[u8], pts_ns: i64) -> EngineResult<()> {
        if rgba.len() != self.frame_bytes {
            return Err(failure("a frame of the wrong size"));
        }
        let first = *self.first_pts_ns.get_or_insert(pts_ns);
        // SAFETY: a buffer GStreamer allocated for the frame, written while
        // mapped, and handed to the source, which takes it.
        let pushed = unsafe {
            let buffer = (self.gst.buffer_new_allocate)(
                std::ptr::null_mut(),
                rgba.len(),
                std::ptr::null_mut(),
            );
            if buffer.is_null() {
                return Err(failure("no memory for a frame"));
            }
            let mut map = std::mem::zeroed::<GstMapInfo>();
            if (self.gst.buffer_map)(buffer, &mut map, GST_MAP_WRITE) == 0 {
                (self.gst.mini_object_unref)(buffer.cast());
                return Err(failure("a frame's memory could not be written"));
            }
            std::ptr::copy_nonoverlapping(rgba.as_ptr(), map.data.cast_mut(), rgba.len());
            (self.gst.buffer_unmap)(buffer, &mut map);
            (*buffer).pts = (pts_ns - first).max(0) as u64;
            (*buffer).duration = self.frame_ns;
            (self.gst.app_src_push_buffer)(self.source, buffer)
        };
        if pushed == 0 {
            Ok(())
        } else {
            Err(failure(self.error().unwrap_or_else(|| {
                format!("the encoder refused a frame ({pushed})")
            })))
        }
    }

    fn drain(&mut self) -> EngineResult<()> {
        // The pipeline writes what it encodes on its own: what is asked here is
        // whether it still can.
        match self.error() {
            Some(why) => Err(failure(why)),
            None => Ok(()),
        }
    }

    fn finish(mut self: Box<Self>) -> EngineResult<()> {
        // SAFETY: the encoder's live source and bus; a message popped is ours.
        let outcome = unsafe {
            (self.gst.app_src_end_of_stream)(self.source);
            let message = (self.gst.bus_timed_pop_filtered)(
                self.bus,
                FINISH_WITHIN_NS,
                GST_MESSAGE_EOS | GST_MESSAGE_ERROR,
            );
            if message.is_null() {
                Err(failure("completing the recording's file took too long"))
            } else if (*message).kind == GST_MESSAGE_ERROR {
                Err(failure(self.describe(message)))
            } else {
                (self.gst.mini_object_unref)(message.cast());
                Ok(())
            }
        };
        self.close();
        outcome
    }

    fn discard(mut self: Box<Self>) {
        self.close();
    }
}

impl Drop for GstVideoEncoder {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::video_decoder::GstVideoDecoder;
    use shared::services::VideoDecoderService;

    const WIDTH: usize = 64;
    const HEIGHT: usize = 48;

    /// A frame as the encoder thread hands it over: RGBA, top row first, its
    /// top half `top` and its bottom half `bottom`.
    fn frame(top: [u8; 3], bottom: [u8; 3]) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(WIDTH * HEIGHT * 4);
        for y in 0..HEIGHT {
            let [r, g, b] = if y < HEIGHT / 2 { top } else { bottom };
            for _ in 0..WIDTH {
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        rgba
    }

    fn near(pixel: &[u8], expected: [i32; 3]) -> bool {
        pixel
            .iter()
            .zip(expected)
            .all(|(&value, want)| (i32::from(value) - want).abs() <= 16)
    }

    #[test]
    fn the_frames_handed_to_it_are_the_video() {
        let factory = GstVideoEncoderFactory;
        assert!(
            factory.available(),
            "GStreamer with an H.264 encoder: its base and good plugins, and x264enc (ugly) or \
             openh264enc (bad)"
        );
        let dir = std::env::temp_dir().join(format!("migo-gst-recorder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("replay.mp4.part");
        let config = EncoderConfig {
            width: WIDTH as u32,
            height: HEIGHT as u32,
            fps: 10,
            bitrate_bps: 1_000_000,
            key_frame_interval: 10,
        };
        let mut encoder = factory.create(&config, &part).expect("an encoder");
        assert_eq!(factory.input(), FrameInput::Memory);
        // A recording's frames are stamped with the clock's time, not from zero.
        let base_ns = 5_000_000_000i64;
        for i in 0..10i64 {
            let rgba = if i < 5 {
                frame([255, 0, 0], [0, 0, 255])
            } else {
                frame([0, 255, 0], [0, 255, 0])
            };
            encoder
                .submit_pixels(&rgba, base_ns + i * 100_000_000)
                .expect("a frame");
            encoder.drain().expect("still encoding");
        }
        encoder.finish().expect("the file completes");
        let video = dir.join("replay.mp4");
        std::fs::rename(&part, &video).unwrap();

        let mut source = GstVideoDecoder::available()
            .expect("GStreamer")
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
    fn a_frame_of_the_wrong_size_is_refused() {
        let factory = GstVideoEncoderFactory;
        assert!(factory.available(), "GStreamer with an H.264 encoder");
        let dir =
            std::env::temp_dir().join(format!("migo-gst-recorder-size-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = EncoderConfig {
            width: WIDTH as u32,
            height: HEIGHT as u32,
            fps: 10,
            bitrate_bps: 1_000_000,
            key_frame_interval: 10,
        };
        let mut encoder = factory
            .create(&config, &dir.join("replay.mp4.part"))
            .expect("an encoder");
        assert!(encoder.submit_pixels(&[0; 16], 0).is_err());
        encoder.discard();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_is_quoted_whole() {
        assert_eq!(quoted(r#"/tmp/a "b"\c"#), r#""/tmp/a \"b\"\\c""#);
    }
}
