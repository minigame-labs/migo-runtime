//! The NDK's media codec and format, as the engine's decoder
//! (`video_decoder`) and encoder (`game_recorder`) use them.

use std::ffi::{c_char, c_void};

use crate::android::surface::ANativeWindow;

#[repr(C)]
pub(crate) struct AMediaFormat(c_void);
#[repr(C)]
pub(crate) struct AMediaCodec(c_void);

#[repr(C)]
#[derive(Default)]
pub(crate) struct AMediaCodecBufferInfo {
    pub(crate) offset: i32,
    pub(crate) size: i32,
    pub(crate) presentation_time_us: i64,
    pub(crate) flags: u32,
}

/// `off_t`, which is 32 bits on the 32-bit ABIs.
#[cfg(target_pointer_width = "64")]
pub(crate) type OffT = i64;
#[cfg(not(target_pointer_width = "64"))]
pub(crate) type OffT = i32;

pub(crate) const AMEDIACODEC_CONFIGURE_FLAG_ENCODE: u32 = 1;
pub(crate) const AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
pub(crate) const AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
pub(crate) const AMEDIACODEC_INFO_TRY_AGAIN_LATER: isize = -1;
pub(crate) const AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
pub(crate) const AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;

#[link(name = "mediandk")]
unsafe extern "C" {
    pub(crate) fn AMediaFormat_new() -> *mut AMediaFormat;
    pub(crate) fn AMediaFormat_delete(format: *mut AMediaFormat) -> i32;
    pub(crate) fn AMediaFormat_getString(
        format: *mut AMediaFormat,
        name: *const c_char,
        out: *mut *const c_char,
    ) -> bool;
    pub(crate) fn AMediaFormat_getInt32(
        format: *mut AMediaFormat,
        name: *const c_char,
        out: *mut i32,
    ) -> bool;
    pub(crate) fn AMediaFormat_setString(
        format: *mut AMediaFormat,
        name: *const c_char,
        value: *const c_char,
    );
    pub(crate) fn AMediaFormat_setInt32(format: *mut AMediaFormat, name: *const c_char, value: i32);
    pub(crate) fn AMediaFormat_setFloat(format: *mut AMediaFormat, name: *const c_char, value: f32);

    pub(crate) fn AMediaCodec_createDecoderByType(mime: *const c_char) -> *mut AMediaCodec;
    pub(crate) fn AMediaCodec_createEncoderByType(mime: *const c_char) -> *mut AMediaCodec;
    pub(crate) fn AMediaCodec_configure(
        codec: *mut AMediaCodec,
        format: *const AMediaFormat,
        surface: *mut ANativeWindow,
        crypto: *mut c_void,
        flags: u32,
    ) -> i32;
    /// API 26.
    pub(crate) fn AMediaCodec_createInputSurface(
        codec: *mut AMediaCodec,
        surface: *mut *mut ANativeWindow,
    ) -> i32;
    /// API 26.
    pub(crate) fn AMediaCodec_signalEndOfInputStream(codec: *mut AMediaCodec) -> i32;
    pub(crate) fn AMediaCodec_start(codec: *mut AMediaCodec) -> i32;
    pub(crate) fn AMediaCodec_stop(codec: *mut AMediaCodec) -> i32;
    pub(crate) fn AMediaCodec_delete(codec: *mut AMediaCodec) -> i32;
    pub(crate) fn AMediaCodec_flush(codec: *mut AMediaCodec) -> i32;
    pub(crate) fn AMediaCodec_dequeueInputBuffer(codec: *mut AMediaCodec, timeout_us: i64)
    -> isize;
    pub(crate) fn AMediaCodec_getInputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        size: *mut usize,
    ) -> *mut u8;
    pub(crate) fn AMediaCodec_queueInputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        offset: OffT,
        size: usize,
        time_us: u64,
        flags: u32,
    ) -> i32;
    pub(crate) fn AMediaCodec_dequeueOutputBuffer(
        codec: *mut AMediaCodec,
        info: *mut AMediaCodecBufferInfo,
        timeout_us: i64,
    ) -> isize;
    pub(crate) fn AMediaCodec_getOutputFormat(codec: *mut AMediaCodec) -> *mut AMediaFormat;
    pub(crate) fn AMediaCodec_getOutputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        size: *mut usize,
    ) -> *mut u8;
    pub(crate) fn AMediaCodec_releaseOutputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        render: bool,
    ) -> i32;
}
