//! GStreamer, as the engine uses it on Linux: loaded, not linked. A system
//! without it still runs every game, and only what needs it -- decoding a video
//! (`createVideoDecoder`), recording the game (`getGameRecorder`) -- answers
//! that it cannot. Whether it is there is a `dlopen`, cheap enough for any
//! thread; initialising it, which may scan the plugins, happens on first use.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use libloading::Library;
use shared::protocol::error::ServiceError;

pub(super) type Gboolean = c_int;
pub(super) type GstClockTime = u64;

pub(super) const GST_CLOCK_TIME_NONE: GstClockTime = u64::MAX;
pub(super) const GST_STATE_NULL: c_int = 1;
pub(super) const GST_STATE_PLAYING: c_int = 4;
pub(super) const GST_STATE_CHANGE_FAILURE: c_int = 0;
pub(super) const GST_MAP_READ: c_int = 1;
pub(super) const GST_MAP_WRITE: c_int = 2;
pub(super) const GST_MESSAGE_EOS: c_uint = 1 << 0;
pub(super) const GST_MESSAGE_ERROR: c_uint = 1 << 1;

#[repr(C)]
pub(super) struct GError {
    pub(super) domain: u32,
    pub(super) code: c_int,
    pub(super) message: *const c_char,
}

#[repr(C)]
pub(super) struct GstMiniObject {
    pub(super) kind: usize,
    pub(super) refcount: c_int,
    pub(super) lockstate: c_int,
    pub(super) flags: c_uint,
    pub(super) copy: *mut c_void,
    pub(super) dispose: *mut c_void,
    pub(super) free: *mut c_void,
    pub(super) priv_uint: c_uint,
    pub(super) priv_pointer: *mut c_void,
}

/// GstBuffer's public head, which its timestamp is read from: the C API reads
/// it through a macro on this field.
#[repr(C)]
pub(super) struct GstBuffer {
    pub(super) mini_object: GstMiniObject,
    pub(super) pool: *mut c_void,
    pub(super) pts: GstClockTime,
    pub(super) dts: GstClockTime,
    pub(super) duration: GstClockTime,
}

/// GstMessage's public head, which its type is read from: the C API reads it
/// through a macro on this field.
#[repr(C)]
pub(super) struct GstMessage {
    pub(super) mini_object: GstMiniObject,
    pub(super) kind: c_uint,
}

#[repr(C)]
pub(super) struct GstMapInfo {
    pub(super) memory: *mut c_void,
    pub(super) flags: c_int,
    pub(super) data: *const u8,
    pub(super) size: usize,
    pub(super) maxsize: usize,
    pub(super) user_data: [*mut c_void; 4],
    pub(super) reserved: [*mut c_void; 4],
}

/// The GStreamer entry points used, resolved once.
pub(super) struct Gst {
    pub(super) init_check:
        unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut GError) -> Gboolean,
    pub(super) element_factory_make:
        unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void,
    pub(super) parse_bin_from_description:
        unsafe extern "C" fn(*const c_char, Gboolean, *mut *mut GError) -> *mut c_void,
    pub(super) bin_get_by_name: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    pub(super) filename_to_uri:
        unsafe extern "C" fn(*const c_char, *mut *mut GError) -> *mut c_char,
    pub(super) element_set_state: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    pub(super) element_seek_simple:
        unsafe extern "C" fn(*mut c_void, c_int, c_int, i64) -> Gboolean,
    pub(super) element_get_bus: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub(super) bus_set_sync_handler: unsafe extern "C" fn(
        *mut c_void,
        Option<unsafe extern "C" fn(*mut c_void, *mut GstMessage, *mut c_void) -> c_int>,
        *mut c_void,
        *mut c_void,
    ),
    pub(super) bus_pop_filtered: unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void,
    pub(super) message_parse_error:
        unsafe extern "C" fn(*mut c_void, *mut *mut GError, *mut *mut c_char),
    pub(super) mini_object_unref: unsafe extern "C" fn(*mut c_void),
    pub(super) object_ref_sink: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub(super) object_unref: unsafe extern "C" fn(*mut c_void),
    pub(super) sample_get_buffer: unsafe extern "C" fn(*mut c_void) -> *mut GstBuffer,
    pub(super) sample_get_caps: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub(super) sample_get_segment: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub(super) segment_to_stream_time: unsafe extern "C" fn(*const c_void, c_int, u64) -> u64,
    pub(super) caps_get_structure: unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void,
    pub(super) structure_get_int:
        unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> Gboolean,
    pub(super) buffer_map: unsafe extern "C" fn(*mut GstBuffer, *mut GstMapInfo, c_int) -> Gboolean,
    pub(super) buffer_unmap: unsafe extern "C" fn(*mut GstBuffer, *mut GstMapInfo),
    pub(super) app_sink_try_pull_sample:
        unsafe extern "C" fn(*mut c_void, GstClockTime) -> *mut c_void,
    pub(super) app_sink_is_eos: unsafe extern "C" fn(*mut c_void) -> Gboolean,
    pub(super) parse_launch: unsafe extern "C" fn(*const c_char, *mut *mut GError) -> *mut c_void,
    pub(super) element_factory_find: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    pub(super) buffer_new_allocate:
        unsafe extern "C" fn(*mut c_void, usize, *mut c_void) -> *mut GstBuffer,
    pub(super) bus_timed_pop_filtered:
        unsafe extern "C" fn(*mut c_void, GstClockTime, c_uint) -> *mut GstMessage,
    pub(super) app_src_push_buffer: unsafe extern "C" fn(*mut c_void, *mut GstBuffer) -> c_int,
    pub(super) app_src_end_of_stream: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub(super) object_set: unsafe extern "C" fn(*mut c_void, *const c_char, ...),
    pub(super) error_free: unsafe extern "C" fn(*mut GError),
    pub(super) free: unsafe extern "C" fn(*mut c_void),
    // Held for as long as the entry points are: for the process.
    pub(super) _libraries: [Library; 4],
}

// SAFETY: the entry points are plain functions, safe to call from any thread;
// GStreamer serialises what it must itself.
unsafe impl Send for Gst {}
unsafe impl Sync for Gst {}

impl Gst {
    /// GStreamer, loaded; `None` when the system has none.
    pub(super) fn loaded() -> Option<&'static Gst> {
        static LOADED: OnceLock<Option<Gst>> = OnceLock::new();
        LOADED.get_or_init(Gst::load).as_ref()
    }

    /// GStreamer, loaded and initialised, or why not.
    pub(super) fn initialised() -> Result<&'static Gst, ServiceError> {
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
            parse_launch: symbol!(gst, "gst_parse_launch"),
            element_factory_find: symbol!(gst, "gst_element_factory_find"),
            buffer_new_allocate: symbol!(gst, "gst_buffer_new_allocate"),
            bus_timed_pop_filtered: symbol!(gst, "gst_bus_timed_pop_filtered"),
            app_src_push_buffer: symbol!(app, "gst_app_src_push_buffer"),
            app_src_end_of_stream: symbol!(app, "gst_app_src_end_of_stream"),
            object_set: symbol!(gobject, "g_object_set"),
            error_free: symbol!(glib, "g_error_free"),
            free: symbol!(glib, "g_free"),
            _libraries: [gst, app, gobject, glib],
        })
    }

    /// The text of `error`, which is freed.
    pub(super) unsafe fn take_error(&self, error: *mut GError) -> Option<String> {
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
