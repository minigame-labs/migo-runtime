//! Private ownership of minimp3_ex presentation; no Rust tag parser or trim DSP.
use super::{FrameBoundary, MAX_SAMPLES_PER_FRAME, Mp3FrameDecoder, Mp3Step, ffi};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{
    ffi::{c_int, c_void},
    ptr::NonNull,
};

unsafe extern "C" {
    fn migo_mp3_presentation_new() -> *mut c_void;
    fn migo_mp3_presentation_free(p: *mut c_void);
    fn migo_mp3_presentation_begin(
        p: *mut c_void,
        frame: *const u8,
        size: c_int,
        free_bytes: c_int,
        info: *const ffi::mp3dec_frame_info_t,
        limit: u64,
        expected: *mut u64,
    ) -> c_int;
    fn migo_mp3_presentation_begin_stream(
        p: *mut c_void,
        frame: *const u8,
        size: c_int,
        free_bytes: c_int,
        info: *const ffi::mp3dec_frame_info_t,
        expected: *mut u64,
    ) -> c_int;
    fn migo_mp3_presentation_decode(
        p: *mut c_void,
        decoder: *mut ffi::mp3dec_t,
        input: *const u8,
        size: c_int,
        pcm: *mut i16,
        info: *mut ffi::mp3dec_frame_info_t,
        skip: *mut c_int,
    ) -> c_int;
    fn migo_mp3_presentation_finish(p: *const c_void) -> c_int;
}

pub(crate) struct Presentation {
    native: NonNull<c_void>,
    pub(crate) expected: Option<usize>,
    format: (c_int, c_int, c_int),
}
// SAFETY: this uniquely owned native state retains no input/IO pointers or
// seek index. Only mutable methods decode; moving it cannot alias codec state.
unsafe impl Send for Presentation {}

impl Presentation {
    pub(super) fn new(frame: &[u8], boundary: &FrameBoundary) -> EngineResult<(Self, bool)> {
        let mut owner = Self::allocate(boundary)?;
        let skip = owner.begin(frame, boundary, false)?;
        Ok((owner, skip))
    }

    pub(crate) fn allocate(boundary: &FrameBoundary) -> EngineResult<Self> {
        #[cfg(test)]
        super::trace_native(0);
        // SAFETY: the constructor returns one exclusively owned C allocation.
        let native = NonNull::new(unsafe { migo_mp3_presentation_new() }).ok_or_else(|| {
            EngineError::from_detail(ErrorCode::OutOfMemory, "MP3 presentation allocation failed")
        })?;
        Ok(Self {
            native,
            expected: None,
            format: (
                boundary.info.hz,
                boundary.info.channels,
                boundary.info.layer,
            ),
        })
    }

    pub(crate) fn begin(
        &mut self,
        frame: &[u8],
        boundary: &FrameBoundary,
        streaming: bool,
    ) -> EngineResult<bool> {
        let mut expected = 0;
        #[cfg(test)]
        super::trace_native(frame.len());
        // SAFETY: native is owned here, frame is the complete bounded frame
        // measured by minimp3; info/free size come from that same dry probe.
        let status = unsafe {
            if streaming {
                migo_mp3_presentation_begin_stream(
                    self.native.as_ptr(),
                    frame.as_ptr(),
                    frame.len() as c_int,
                    boundary.free_format_bytes,
                    &boundary.info,
                    &mut expected,
                )
            } else {
                migo_mp3_presentation_begin(
                    self.native.as_ptr(),
                    frame.as_ptr(),
                    frame.len() as c_int,
                    boundary.free_format_bytes,
                    &boundary.info,
                    crate::limits::MAX_AUDIO_PCM_SAMPLES,
                    &mut expected,
                )
            }
        };
        if status < 0 {
            return Err(invalid("invalid MP3 presentation metadata or PCM quota"));
        }
        self.expected = if expected == 0 {
            None
        } else {
            Some(
                usize::try_from(expected)
                    .map_err(|_| invalid("MP3 presentation overflows sample coordinates"))?,
            )
        };
        Ok(status == 1)
    }

    pub(crate) fn format(&self) -> (u32, u32) {
        (self.format.0 as u32, self.format.1 as u32)
    }

    pub(crate) fn accepts(&self, info: &ffi::mp3dec_frame_info_t) -> bool {
        self.format == (info.hz, info.channels, info.layer)
    }

    pub(crate) fn finish(&self) -> EngineResult<()> {
        #[cfg(test)]
        super::trace_native(0);
        // SAFETY: native remains live and owned through final validation.
        if unsafe { migo_mp3_presentation_finish(self.native.as_ptr()) } < 0 {
            return Err(invalid("MP3 ended before its declared presentation"));
        }
        Ok(())
    }
}
impl Drop for Presentation {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner; C has no index or borrowed input to free.
        unsafe { migo_mp3_presentation_free(self.native.as_ptr()) };
    }
}

impl Mp3FrameDecoder {
    pub(crate) fn decode_presented<'a>(
        &'a mut self,
        input: &[u8],
        owner: &mut Presentation,
    ) -> EngineResult<Mp3Step<'a>> {
        // SAFETY: frame info is a C POD containing only integer fields.
        let mut info = unsafe { std::mem::zeroed() };
        let mut skip = 0;
        #[cfg(test)]
        super::trace_native(input.len());
        // SAFETY: decoder/owner are uniquely borrowed, input is bounded by the
        // cursor, and pcm is the codec's documented fixed maximum buffer.
        let count = unsafe {
            migo_mp3_presentation_decode(
                owner.native.as_ptr(),
                &mut *self.state,
                input.as_ptr(),
                input.len() as c_int,
                self.pcm.as_mut_ptr(),
                &mut info,
                &mut skip,
            )
        };
        if count < 0 || skip < 0 || skip as usize + count as usize > MAX_SAMPLES_PER_FRAME {
            return Err(invalid(
                "MP3 source format changed or presentation decode failed",
            ));
        }
        Ok(self.frame_step(input.len(), info, count as usize, skip as usize, true))
    }
}
fn invalid(message: &'static str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, message)
}
