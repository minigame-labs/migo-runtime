//! `createVideoDecoder` on Linux: GStreamer's decoders.
//!
//! GStreamer is loaded, not linked: a Linux system without it still runs every
//! game, and only this API answers that it cannot. Whether it is there is a
//! `dlopen`, cheap enough for content's thread; initialising it -- which may
//! scan the plugins -- happens when the first decoder opens, on the decode
//! thread.
//!
//! A source is played by `playbin` with its video only -- no audio is decoded
//! -- into `videoconvert`, which turns the frames into RGBA as the stream's
//! colorimetry says, and an `appsink` the decode thread pulls from: decoding
//! goes as fast as the engine takes frames, two at most waiting.
//!
//! A seek goes to the keyframe before the position, and the frames are decoded
//! from there until the one shown at it, as on the other platforms: GStreamer's
//! own accurate seek would hand that frame out stamped with the position
//! instead of its own time.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};

use super::gst::{
    GST_CLOCK_TIME_NONE, GST_MAP_READ, GST_MESSAGE_ERROR, GST_STATE_CHANGE_FAILURE, GST_STATE_NULL,
    GST_STATE_PLAYING, Gst, GstClockTime, GstMapInfo, GstMessage,
};

use shared::protocol::error::ServiceError;
use shared::services::{FrameTiming, VideoDecoderService, VideoFrameSource};

const GST_FORMAT_TIME: c_int = 3;
const GST_SEEK_FLAG_FLUSH: c_int = 1 << 0;
const GST_SEEK_FLAG_KEY_UNIT: c_int = 1 << 2;
const GST_SEEK_FLAG_SNAP_BEFORE: c_int = 1 << 5;
const GST_BUS_DROP: c_int = 0;
const GST_BUS_PASS: c_int = 1;
/// playbin's flags: the video, deinterlaced when it is interlaced.
const GST_PLAY_FLAGS: c_uint = 0x1 | 0x200;
/// How long a wait for a frame goes before the pipeline is asked whether it
/// failed: a failed pipeline stops sending, and never says so to the sink.
const PULL_WAIT_NS: GstClockTime = 100_000_000;

/// GStreamer's decoders, when the system has GStreamer.
pub struct GstVideoDecoder;

impl GstVideoDecoder {
    /// The decoder, or `None` when GStreamer cannot be loaded.
    pub fn available() -> Option<Self> {
        Gst::loaded().map(|_| GstVideoDecoder)
    }
}

impl VideoDecoderService for GstVideoDecoder {
    fn open(&self, source: &str) -> Result<Box<dyn VideoFrameSource>, ServiceError> {
        Ok(Box::new(Source::open(Gst::initialised()?, source)?))
    }
}

/// The bus's handler, given GStreamer's entry points: an error is queued for
/// the decode thread, anything else dropped -- and a message dropped is the
/// handler's to release.
unsafe extern "C" fn errors_only(
    _bus: *mut c_void,
    message: *mut GstMessage,
    gst: *mut c_void,
) -> c_int {
    // SAFETY: GStreamer hands the handler a live message, whose reference it
    // owns when it drops it; `gst` is the process's `Gst`.
    unsafe {
        if (*message).kind == GST_MESSAGE_ERROR {
            return GST_BUS_PASS;
        }
        ((*gst.cast::<Gst>()).mini_object_unref)(message.cast());
    }
    GST_BUS_DROP
}

/// One source playing into the sink the decode thread pulls from.
struct Source {
    gst: &'static Gst,
    pipeline: *mut c_void,
    sink: *mut c_void,
    width: u32,
    height: u32,
    /// A frame to hand out first: the first one, pulled to learn the size, or
    /// the one after the frame a seek landed on.
    pending: Option<Sample>,
    /// Where a seek asked to continue from, until the frame shown there is
    /// found.
    seek_target: Option<f64>,
    /// During a seek, the latest frame at or before the target.
    held: Option<Sample>,
}

// SAFETY: the pipeline is driven from the one decode thread that owns the
// source; GStreamer's objects may be used from any thread.
unsafe impl Send for Source {}

/// A pulled sample, released with it.
struct Sample {
    gst: &'static Gst,
    sample: *mut c_void,
    /// Where in the video the frame is shown.
    position_ms: f64,
}

impl Sample {
    /// `sample`, taken over, with where in the video it is: its timestamp
    /// through the segment it played in.
    fn new(gst: &'static Gst, sample: *mut c_void) -> Result<Self, ServiceError> {
        let mut taken = Sample {
            gst,
            sample,
            position_ms: 0.0,
        };
        // SAFETY: the sample's buffer and segment live with it.
        let position = unsafe {
            let buffer = (gst.sample_get_buffer)(sample);
            let segment = (gst.sample_get_segment)(sample);
            if buffer.is_null() || (*buffer).pts == GST_CLOCK_TIME_NONE {
                GST_CLOCK_TIME_NONE
            } else if segment.is_null() {
                (*buffer).pts
            } else {
                (gst.segment_to_stream_time)(segment, GST_FORMAT_TIME, (*buffer).pts)
            }
        };
        if position == GST_CLOCK_TIME_NONE {
            return Err(ServiceError::system("a frame has no time"));
        }
        taken.position_ms = position as f64 / 1_000_000.0;
        Ok(taken)
    }

    /// The frame's width and height, as its caps say.
    fn size(&self) -> Option<(usize, usize)> {
        // SAFETY: the sample's caps and their structure live with it.
        unsafe {
            let caps = (self.gst.sample_get_caps)(self.sample);
            if caps.is_null() {
                return None;
            }
            let structure = (self.gst.caps_get_structure)(caps, 0);
            let (mut width, mut height) = (0, 0);
            if structure.is_null()
                || (self.gst.structure_get_int)(structure, c"width".as_ptr(), &mut width) == 0
                || (self.gst.structure_get_int)(structure, c"height".as_ptr(), &mut height) == 0
                || width <= 0
                || height <= 0
            {
                return None;
            }
            Some((width as usize, height as usize))
        }
    }
}

impl Drop for Sample {
    fn drop(&mut self) {
        // SAFETY: the sample is ours: pulled, and unreferenced once.
        unsafe { (self.gst.mini_object_unref)(self.sample) };
    }
}

impl Source {
    fn open(gst: &'static Gst, source: &str) -> Result<Self, ServiceError> {
        let remote = source.starts_with("http://") || source.starts_with("https://");
        let text = CString::new(source)
            .map_err(|_| ServiceError::invalid_param("the source is not text"))?;
        // SAFETY: GStreamer is initialised; every object made here is either
        // owned by the pipeline or released before returning.
        unsafe {
            let uri = if remote {
                text
            } else {
                let mut error = std::ptr::null_mut();
                let made = (gst.filename_to_uri)(text.as_ptr(), &mut error);
                if made.is_null() {
                    let why = gst.take_error(error).unwrap_or_default();
                    return Err(ServiceError::invalid_param(format!(
                        "the source is not a path: {why}"
                    )));
                }
                let owned = CStr::from_ptr(made).to_owned();
                (gst.free)(made.cast());
                owned
            };
            let pipeline = (gst.element_factory_make)(c"playbin".as_ptr(), std::ptr::null());
            if pipeline.is_null() {
                return Err(ServiceError::not_supported(
                    "GStreamer has no playbin (its base plugins)",
                ));
            }
            (gst.object_ref_sink)(pipeline);
            // Nothing reads the pipeline's messages but its errors: the rest
            // are dropped as they are posted rather than queued for no one.
            let bus = (gst.element_get_bus)(pipeline);
            if !bus.is_null() {
                (gst.bus_set_sync_handler)(
                    bus,
                    Some(errors_only),
                    std::ptr::from_ref(gst).cast_mut().cast(),
                    std::ptr::null_mut(),
                );
                (gst.object_unref)(bus);
            }
            let mut error = std::ptr::null_mut();
            let output = (gst.parse_bin_from_description)(
                c"videoconvert ! video/x-raw,format=RGBA ! appsink name=frames sync=false max-buffers=2"
                    .as_ptr(),
                1,
                &mut error,
            );
            if output.is_null() {
                (gst.object_unref)(pipeline);
                let why = gst.take_error(error).unwrap_or_default();
                return Err(ServiceError::not_supported(format!(
                    "GStreamer cannot convert video: {why}"
                )));
            }
            let sink = (gst.bin_get_by_name)(output, c"frames".as_ptr());
            // The pipeline takes the output bin, which keeps the sink.
            (gst.object_set)(
                pipeline,
                c"uri".as_ptr(),
                uri.as_ptr(),
                c"flags".as_ptr(),
                GST_PLAY_FLAGS,
                c"video-sink".as_ptr(),
                output,
                std::ptr::null::<c_char>(),
            );
            let mut opened = Self {
                gst,
                pipeline,
                sink,
                width: 0,
                height: 0,
                pending: None,
                seek_target: None,
                held: None,
            };
            if (gst.element_set_state)(pipeline, GST_STATE_PLAYING) == GST_STATE_CHANGE_FAILURE {
                return Err(ServiceError::system(
                    opened.failure("the video could not be opened"),
                ));
            }
            // The size is the decoded frames': the first one says it, and is
            // kept for the first call.
            let first = opened
                .pull()?
                .ok_or_else(|| ServiceError::system("the video has no frames"))?;
            let (width, height) = first
                .size()
                .ok_or_else(|| ServiceError::system("the video has no size"))?;
            opened.width = width as u32;
            opened.height = height as u32;
            opened.pending = Some(first);
            Ok(opened)
        }
    }

    /// The next decoded frame; `None` at the end.
    fn pull(&mut self) -> Result<Option<Sample>, ServiceError> {
        loop {
            // SAFETY: the sink is the pipeline's, alive with it.
            unsafe {
                let sample = (self.gst.app_sink_try_pull_sample)(self.sink, PULL_WAIT_NS);
                if !sample.is_null() {
                    return Sample::new(self.gst, sample).map(Some);
                }
                if (self.gst.app_sink_is_eos)(self.sink) != 0 {
                    return Ok(None);
                }
            }
            if let Some(why) = self.error() {
                return Err(ServiceError::system(format!("decoding failed: {why}")));
            }
        }
    }

    /// GStreamer's reason, when the pipeline failed.
    fn error(&self) -> Option<String> {
        // SAFETY: the bus is the pipeline's; a message popped is ours to read
        // and release.
        unsafe {
            let bus = (self.gst.element_get_bus)(self.pipeline);
            if bus.is_null() {
                return None;
            }
            let message = (self.gst.bus_pop_filtered)(bus, GST_MESSAGE_ERROR);
            (self.gst.object_unref)(bus);
            if message.is_null() {
                return None;
            }
            let mut error = std::ptr::null_mut();
            let mut debug = std::ptr::null_mut();
            (self.gst.message_parse_error)(message, &mut error, &mut debug);
            (self.gst.free)(debug.cast());
            (self.gst.mini_object_unref)(message);
            Some(
                self.gst
                    .take_error(error)
                    .unwrap_or_else(|| "no reason given".to_owned()),
            )
        }
    }

    /// `what` failed, with GStreamer's reason when it posted one.
    fn failure(&self, what: &str) -> String {
        match self.error() {
            Some(why) => format!("{what}: {why}"),
            None => what.to_owned(),
        }
    }

    fn emit(&self, sample: Sample, rgba: &mut [u8]) -> Result<Option<FrameTiming>, ServiceError> {
        // A stream whose size changes is cut or left short to the first size.
        let (width, height) = sample
            .size()
            .ok_or_else(|| ServiceError::system("a frame has no size"))?;
        let out_row = self.width as usize * 4;
        let row = width * 4;
        let copied = row.min(out_row);
        // SAFETY: the sample's buffer lives with it, mapped for the read.
        unsafe {
            let buffer = (self.gst.sample_get_buffer)(sample.sample);
            if buffer.is_null() {
                return Err(ServiceError::system("a frame has no pixels"));
            }
            let mut map: GstMapInfo = std::mem::zeroed();
            if (self.gst.buffer_map)(buffer, &mut map, GST_MAP_READ) == 0 {
                return Err(ServiceError::system("a frame could not be read"));
            }
            // The sink takes no layout but the caps' own: one plane of RGBA,
            // rows `width * 4` bytes apart.
            if map.data.is_null() || map.size < row * height {
                (self.gst.buffer_unmap)(buffer, &mut map);
                return Err(ServiceError::system("a frame is smaller than its size"));
            }
            let pixels = std::slice::from_raw_parts(map.data, row * height);
            for y in 0..height.min(self.height as usize) {
                rgba[y * out_row..y * out_row + copied]
                    .copy_from_slice(&pixels[y * row..y * row + copied]);
            }
            (self.gst.buffer_unmap)(buffer, &mut map);
        }
        Ok(Some(FrameTiming {
            pts_ms: sample.position_ms,
            dts_ms: sample.position_ms,
        }))
    }
}

impl VideoFrameSource for Source {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn next_frame(&mut self, rgba: &mut [u8]) -> Result<Option<FrameTiming>, ServiceError> {
        if let Some(pending) = self.pending.take() {
            return self.emit(pending, rgba);
        }
        loop {
            let Some(sample) = self.pull()? else {
                // A target past the last frame shows the last frame.
                self.seek_target = None;
                return match self.held.take() {
                    Some(held) => self.emit(held, rgba),
                    None => Ok(None),
                };
            };
            match self.seek_target {
                Some(target) if sample.position_ms <= target => self.held = Some(sample),
                Some(_) => {
                    self.seek_target = None;
                    return match self.held.take() {
                        Some(held) => {
                            self.pending = Some(sample);
                            self.emit(held, rgba)
                        }
                        None => self.emit(sample, rgba),
                    };
                }
                None => return self.emit(sample, rgba),
            }
        }
    }

    fn seek(&mut self, position_ms: f64) -> Result<(), ServiceError> {
        self.pending = None;
        self.held = None;
        self.seek_target = Some(position_ms);
        let position = (position_ms.max(0.0) * 1_000_000.0) as i64;
        // SAFETY: the pipeline is alive; a flushing seek empties the sink
        // before it returns.
        let sought = unsafe {
            (self.gst.element_seek_simple)(
                self.pipeline,
                GST_FORMAT_TIME,
                GST_SEEK_FLAG_FLUSH | GST_SEEK_FLAG_KEY_UNIT | GST_SEEK_FLAG_SNAP_BEFORE,
                position,
            )
        };
        if sought == 0 {
            return Err(ServiceError::system(
                self.failure("the video could not seek"),
            ));
        }
        Ok(())
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.pending = None;
        self.held = None;
        // SAFETY: stopped before it is let go: its threads end with it.
        unsafe {
            (self.gst.element_set_state)(self.pipeline, GST_STATE_NULL);
            if !self.sink.is_null() {
                (self.gst.object_unref)(self.sink);
            }
            (self.gst.object_unref)(self.pipeline);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::gst::Gboolean;

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

    fn decoder() -> GstVideoDecoder {
        GstVideoDecoder::available()
            .expect("GStreamer with an H.264 decoder: its base, good and libav plugins")
    }

    #[test]
    fn every_frame_arrives_in_order_with_its_time_and_colour() {
        let mut source = decoder().open(&clip()).expect("open");
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
        let mut source = decoder().open(&clip()).expect("open");
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
        let middle = (24 * 64 + 32) * 4;
        assert!(
            near(&rgba[middle..middle + 3], [125, 130, 128]),
            "{:?}",
            &rgba[middle..middle + 4]
        );
        // Past the last frame: the last frame, then the end.
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
        assert!(source.next_frame(&mut rgba).expect("end").is_none());
        // Back to the start after the end: the stream plays again from there.
        while source.next_frame(&mut rgba).expect("decode").is_some() {}
        source.seek(0.0).expect("seek");
        let again = source
            .next_frame(&mut rgba)
            .expect("decode")
            .expect("a frame");
        assert!(again.pts_ms.abs() < 1.0, "again at {}", again.pts_ms);
    }

    #[test]
    fn nothing_but_errors_waits_on_the_bus() {
        let gst = Gst::initialised().expect("GStreamer");
        let mut source = Source::open(gst, &clip()).expect("open");
        let mut rgba = vec![0u8; 64 * 48 * 4];
        while source.next_frame(&mut rgba).expect("decode").is_some() {}
        source.seek(300.0).expect("seek");
        source.next_frame(&mut rgba).expect("decode");
        // SAFETY: the function of that name, with its prototype; the bus is
        // the pipeline's, released after.
        unsafe {
            let have_pending: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> Gboolean> =
                gst._libraries[0]
                    .get(b"gst_bus_have_pending\0")
                    .expect("gst_bus_have_pending");
            let bus = (gst.element_get_bus)(source.pipeline);
            let pending = have_pending(bus);
            (gst.object_unref)(bus);
            assert_eq!(pending, 0, "messages wait on the bus for no one");
        }
    }

    #[test]
    fn a_message_dropped_is_released() {
        let gst = Gst::initialised().expect("GStreamer");
        let source = Source::open(gst, &clip()).expect("open");
        // SAFETY: the functions of those names, with their prototypes; the
        // message is the test's own reference and the bus's, the bus the
        // pipeline's, released after.
        unsafe {
            let library = &gst._libraries[0];
            let structure: libloading::Symbol<unsafe extern "C" fn(*const c_char) -> *mut c_void> =
                library.get(b"gst_structure_new_empty\0").expect("symbol");
            let application: libloading::Symbol<
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut GstMessage,
            > = library
                .get(b"gst_message_new_application\0")
                .expect("symbol");
            let reference: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> *mut c_void> =
                library.get(b"gst_mini_object_ref\0").expect("symbol");
            let post: libloading::Symbol<
                unsafe extern "C" fn(*mut c_void, *mut GstMessage) -> Gboolean,
            > = library.get(b"gst_bus_post\0").expect("symbol");
            let message = application(std::ptr::null_mut(), structure(c"probe".as_ptr()));
            reference(message.cast());
            let bus = (gst.element_get_bus)(source.pipeline);
            assert_ne!(post(bus, message), 0);
            (gst.object_unref)(bus);
            // The bus took one reference and its handler, dropping the
            // message, released it: the test's is the only one left.
            assert_eq!((*message).mini_object.refcount, 1);
            (gst.mini_object_unref)(message.cast());
        }
    }

    #[test]
    fn what_is_no_video_is_refused_with_why() {
        let Err(missing) = decoder().open("/nonexistent/clip.mp4") else {
            panic!("a missing file opened");
        };
        // With GStreamer's reason after the colon.
        let text = format!("{missing:?}");
        assert!(text.contains("could not be opened: "), "{text}");
        let text = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        assert!(decoder().open(text).is_err());
    }
}
