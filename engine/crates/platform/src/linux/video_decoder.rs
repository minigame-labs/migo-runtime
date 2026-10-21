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
use std::sync::OnceLock;

use libloading::Library;
use shared::protocol::error::ServiceError;
use shared::services::{FrameTiming, VideoDecoderService, VideoFrameSource};

type Gboolean = c_int;
type GstClockTime = u64;

const GST_CLOCK_TIME_NONE: GstClockTime = u64::MAX;
const GST_STATE_NULL: c_int = 1;
const GST_STATE_PLAYING: c_int = 4;
const GST_STATE_CHANGE_FAILURE: c_int = 0;
const GST_FORMAT_TIME: c_int = 3;
const GST_SEEK_FLAG_FLUSH: c_int = 1 << 0;
const GST_SEEK_FLAG_KEY_UNIT: c_int = 1 << 2;
const GST_SEEK_FLAG_SNAP_BEFORE: c_int = 1 << 5;
const GST_MAP_READ: c_int = 1;
const GST_MESSAGE_ERROR: c_uint = 1 << 1;
const GST_BUS_DROP: c_int = 0;
const GST_BUS_PASS: c_int = 1;
/// playbin's flags: the video, deinterlaced when it is interlaced.
const GST_PLAY_FLAGS: c_uint = 0x1 | 0x200;
/// How long a wait for a frame goes before the pipeline is asked whether it
/// failed: a failed pipeline stops sending, and never says so to the sink.
const PULL_WAIT_NS: GstClockTime = 100_000_000;

#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *const c_char,
}

#[repr(C)]
struct GstMiniObject {
    kind: usize,
    refcount: c_int,
    lockstate: c_int,
    flags: c_uint,
    copy: *mut c_void,
    dispose: *mut c_void,
    free: *mut c_void,
    priv_uint: c_uint,
    priv_pointer: *mut c_void,
}

/// GstBuffer's public head, which its timestamp is read from: the C API reads
/// it through a macro on this field.
#[repr(C)]
struct GstBuffer {
    mini_object: GstMiniObject,
    pool: *mut c_void,
    pts: GstClockTime,
    dts: GstClockTime,
    duration: GstClockTime,
}

/// GstMessage's public head, which its type is read from: the C API reads it
/// through a macro on this field.
#[repr(C)]
struct GstMessage {
    mini_object: GstMiniObject,
    kind: c_uint,
}

#[repr(C)]
struct GstMapInfo {
    memory: *mut c_void,
    flags: c_int,
    data: *const u8,
    size: usize,
    maxsize: usize,
    user_data: [*mut c_void; 4],
    reserved: [*mut c_void; 4],
}

/// The GStreamer entry points used, resolved once.
struct Gst {
    init_check:
        unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> Gboolean,
    element_factory_make: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void,
    parse_bin_from_description:
        unsafe extern "C" fn(*const c_char, Gboolean, *mut *mut GError) -> *mut c_void,
    bin_get_by_name: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    filename_to_uri: unsafe extern "C" fn(*const c_char, *mut *mut GError) -> *mut c_char,
    element_set_state: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    element_seek_simple: unsafe extern "C" fn(*mut c_void, c_int, c_int, i64) -> Gboolean,
    element_get_bus: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    bus_set_sync_handler: unsafe extern "C" fn(
        *mut c_void,
        Option<unsafe extern "C" fn(*mut c_void, *mut GstMessage, *mut c_void) -> c_int>,
        *mut c_void,
        *mut c_void,
    ),
    bus_pop_filtered: unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void,
    message_parse_error: unsafe extern "C" fn(*mut c_void, *mut *mut GError, *mut *mut c_char),
    mini_object_unref: unsafe extern "C" fn(*mut c_void),
    object_ref_sink: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    object_unref: unsafe extern "C" fn(*mut c_void),
    sample_get_buffer: unsafe extern "C" fn(*mut c_void) -> *mut GstBuffer,
    sample_get_caps: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    sample_get_segment: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    segment_to_stream_time: unsafe extern "C" fn(*const c_void, c_int, u64) -> u64,
    caps_get_structure: unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void,
    structure_get_int: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> Gboolean,
    buffer_map: unsafe extern "C" fn(*mut GstBuffer, *mut GstMapInfo, c_int) -> Gboolean,
    buffer_unmap: unsafe extern "C" fn(*mut GstBuffer, *mut GstMapInfo),
    app_sink_try_pull_sample: unsafe extern "C" fn(*mut c_void, GstClockTime) -> *mut c_void,
    app_sink_is_eos: unsafe extern "C" fn(*mut c_void) -> Gboolean,
    object_set: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    error_free: unsafe extern "C" fn(*mut GError),
    free: unsafe extern "C" fn(*mut c_void),
    // Held for as long as the entry points are: for the process.
    _libraries: [Library; 4],
}

// SAFETY: the entry points are plain functions, safe to call from any thread;
// GStreamer serialises what it must itself.
unsafe impl Send for Gst {}
unsafe impl Sync for Gst {}

impl Gst {
    /// GStreamer, loaded; `None` when the system has none.
    fn loaded() -> Option<&'static Gst> {
        static LOADED: OnceLock<Option<Gst>> = OnceLock::new();
        LOADED.get_or_init(Gst::load).as_ref()
    }

    /// GStreamer, loaded and initialised, or why not.
    fn initialised() -> Result<&'static Gst, ServiceError> {
        static INITIALISED: OnceLock<Result<(), String>> = OnceLock::new();
        let gst = Gst::loaded()
            .ok_or_else(|| ServiceError::not_supported("GStreamer is not installed"))?;
        INITIALISED
            .get_or_init(|| {
                let mut error = std::ptr::null_mut();
                // SAFETY: GStreamer may be initialised from any thread, more
                // than once; the error, if any, is ours.
                let done = unsafe {
                    (gst.init_check)(std::ptr::null_mut(), std::ptr::null_mut(), &mut error)
                };
                // SAFETY: as above.
                let why = unsafe { gst.take_error(error) };
                if done != 0 {
                    Ok(())
                } else {
                    Err(why.unwrap_or_else(|| "GStreamer could not start".to_owned()))
                }
            })
            .as_ref()
            .map(|()| gst)
            .map_err(|why| ServiceError::not_supported(why.clone()))
    }

    fn load() -> Option<Gst> {
        // SAFETY: loading the system's GStreamer, GObject and GLib runs their
        // constructors, which have no preconditions.
        let (gst, app, gobject, glib) = unsafe {
            (
                Library::new("libgstreamer-1.0.so.0").ok()?,
                Library::new("libgstapp-1.0.so.0").ok()?,
                Library::new("libgobject-2.0.so.0").ok()?,
                Library::new("libglib-2.0.so.0").ok()?,
            )
        };
        macro_rules! symbol {
            ($library:expr, $name:literal) => {
                // SAFETY: the symbol is the C function of that name, whose
                // prototype the field it is stored in declares.
                *unsafe { $library.get(concat!($name, "\0").as_bytes()) }.ok()?
            };
        }
        Some(Gst {
            init_check: symbol!(gst, "gst_init_check"),
            element_factory_make: symbol!(gst, "gst_element_factory_make"),
            parse_bin_from_description: symbol!(gst, "gst_parse_bin_from_description"),
            bin_get_by_name: symbol!(gst, "gst_bin_get_by_name"),
            filename_to_uri: symbol!(gst, "gst_filename_to_uri"),
            element_set_state: symbol!(gst, "gst_element_set_state"),
            element_seek_simple: symbol!(gst, "gst_element_seek_simple"),
            element_get_bus: symbol!(gst, "gst_element_get_bus"),
            bus_set_sync_handler: symbol!(gst, "gst_bus_set_sync_handler"),
            bus_pop_filtered: symbol!(gst, "gst_bus_pop_filtered"),
            message_parse_error: symbol!(gst, "gst_message_parse_error"),
            mini_object_unref: symbol!(gst, "gst_mini_object_unref"),
            object_ref_sink: symbol!(gst, "gst_object_ref_sink"),
            object_unref: symbol!(gst, "gst_object_unref"),
            sample_get_buffer: symbol!(gst, "gst_sample_get_buffer"),
            sample_get_caps: symbol!(gst, "gst_sample_get_caps"),
            sample_get_segment: symbol!(gst, "gst_sample_get_segment"),
            segment_to_stream_time: symbol!(gst, "gst_segment_to_stream_time"),
            caps_get_structure: symbol!(gst, "gst_caps_get_structure"),
            structure_get_int: symbol!(gst, "gst_structure_get_int"),
            buffer_map: symbol!(gst, "gst_buffer_map"),
            buffer_unmap: symbol!(gst, "gst_buffer_unmap"),
            app_sink_try_pull_sample: symbol!(app, "gst_app_sink_try_pull_sample"),
            app_sink_is_eos: symbol!(app, "gst_app_sink_is_eos"),
            object_set: symbol!(gobject, "g_object_set"),
            error_free: symbol!(glib, "g_error_free"),
            free: symbol!(glib, "g_free"),
            _libraries: [gst, app, gobject, glib],
        })
    }

    /// The text of `error`, which is freed.
    unsafe fn take_error(&self, error: *mut GError) -> Option<String> {
        if error.is_null() {
            return None;
        }
        // SAFETY: a GError GStreamer filled, its message NUL-terminated.
        unsafe {
            let message = (!(*error).message.is_null()).then(|| {
                CStr::from_ptr((*error).message)
                    .to_string_lossy()
                    .into_owned()
            });
            (self.error_free)(error);
            message
        }
    }
}

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

/// The bus's handler: an error is queued for the decode thread, anything else
/// dropped.
unsafe extern "C" fn errors_only(
    _bus: *mut c_void,
    message: *mut GstMessage,
    _data: *mut c_void,
) -> c_int {
    // SAFETY: GStreamer hands the handler a live message.
    if unsafe { (*message).kind } == GST_MESSAGE_ERROR {
        GST_BUS_PASS
    } else {
        GST_BUS_DROP
    }
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
                    std::ptr::null_mut(),
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
