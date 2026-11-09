//! The test oracle for `mp3_source`: upstream minimp3's own pull reader
//! (`mp3dec_ex`, native/mp3.c), whose output the frame-level source must
//! reproduce sample for sample. Formerly the product MP3 source:
//! callback IO over a blocking source, LAME/Xing gapless trimming and
//! sample-exact seeking. Like the other PCM sources it runs only on a stream's
//! own decode thread.
//!
//! Opened without a whole-stream scan, so playback starts at once: the length
//! (and with it seek/loop control) is known when the stream carries a LAME,
//! Xing or VBRI tag, as before. The first seek builds minimp3's frame index.
use crate::pcm_source::PcmSource;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::ffi::{c_int, c_void};
use std::io::{self, Read, Seek, SeekFrom};
use symphonia_core::io::MediaSource;

#[repr(C)]
struct Io {
    read: unsafe extern "C" fn(*mut c_void, usize, *mut c_void) -> usize,
    read_data: *mut c_void,
    seek: unsafe extern "C" fn(u64, *mut c_void) -> c_int,
    seek_data: *mut c_void,
}

unsafe extern "C" {
    fn migo_mp3_open(io: *mut Io, flags: c_int, error: *mut c_int) -> *mut c_void;
    fn migo_mp3_close(dec: *mut c_void);
    fn migo_mp3_info(
        dec: *const c_void,
        hz: *mut c_int,
        channels: *mut c_int,
        samples: *mut u64,
        tagged: *mut c_int,
    );
    fn migo_mp3_seek(dec: *mut c_void, position: u64) -> c_int;
    fn migo_mp3_read(dec: *mut c_void, buffer: *mut i16, samples: usize) -> usize;
    fn migo_mp3_last_error(dec: *const c_void) -> c_int;
}

const MP3D_SEEK_TO_SAMPLE: c_int = 1;
const MP3D_DO_NOT_SCAN: c_int = 2;
const MP3D_E_MEMORY: c_int = -2;
const MP3D_E_IOERROR: c_int = -3;
/// One read: four 1152-frame MP3 frames of stereo.
const READ_SAMPLES: usize = 4 * 1152 * 2;

/// The callbacks' view of the source. IO errors (including cancellation,
/// which sources report as `Interrupted`) are kept for the Rust caller: minimp3
/// itself only sees a short read.
struct Reader {
    source: Box<dyn MediaSource>,
    error: Option<io::Error>,
}

unsafe extern "C" fn read_callback(buf: *mut c_void, size: usize, user: *mut c_void) -> usize {
    // SAFETY: `user` is the Reader this decoder was opened with; it outlives
    // the decoder and is used by no one else while minimp3 runs.
    let reader = unsafe { &mut *(user as *mut Reader) };
    if reader.error.is_some() || size == 0 {
        return 0;
    }
    // SAFETY: minimp3 hands a writable buffer of `size` bytes.
    let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, size) };
    let mut filled = 0;
    while filled < size {
        match reader.source.read(&mut out[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) => {
                reader.error = Some(error);
                break;
            }
        }
    }
    filled
}

unsafe extern "C" fn seek_callback(position: u64, user: *mut c_void) -> c_int {
    // SAFETY: as in read_callback.
    let reader = unsafe { &mut *(user as *mut Reader) };
    if reader.error.is_some() {
        return -1;
    }
    match reader.source.seek(SeekFrom::Start(position)) {
        Ok(_) => 0,
        Err(error) => {
            reader.error = Some(error);
            -1
        }
    }
}

pub(crate) struct Mp3Ex {
    dec: *mut c_void,
    /// Pinned: minimp3 keeps pointers to both.
    io: Box<Io>,
    reader: Box<Reader>,
    rate: u32,
    channels: u32,
    frames: Option<u64>,
    position: u64,
    buffer: Vec<i16>,
}

// SAFETY: the decoder state and the reader are owned by this value and only
// touched through `&mut self`; nothing is shared with another thread.
unsafe impl Send for Mp3Ex {}

impl Drop for Mp3Ex {
    fn drop(&mut self) {
        // SAFETY: `dec` came from migo_mp3_open and is closed exactly once.
        unsafe { migo_mp3_close(self.dec) };
    }
}

/// A failure while reading or decoding an opened stream.
fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, detail.into())
}

/// Bytes that are not a playable MP3 stream (as the other readers report it).
fn malformed(detail: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, detail.into())
}

fn source_error(error: io::Error) -> EngineError {
    if error.kind() == io::ErrorKind::Interrupted {
        EngineError::from_detail(ErrorCode::Cancelled, "audio source cancelled")
    } else {
        invalid(format!("MP3 source: {error}"))
    }
}

impl Mp3Ex {
    pub(crate) fn open(source: Box<dyn MediaSource>) -> EngineResult<Self> {
        let mut reader = Box::new(Reader {
            source,
            error: None,
        });
        let user = &mut *reader as *mut Reader as *mut c_void;
        let mut io = Box::new(Io {
            read: read_callback,
            read_data: user,
            seek: seek_callback,
            seek_data: user,
        });
        let dec = start(&mut io, &mut reader)?;
        let mut source = Self {
            dec,
            io,
            reader,
            rate: 0,
            channels: 0,
            frames: None,
            position: 0,
            buffer: vec![0; READ_SAMPLES],
        };
        let (rate, channels, frames) = source.format()?;
        source.rate = rate;
        source.channels = channels;
        source.frames = frames;
        Ok(source)
    }

    /// The decoder's format and tagged length, validated.
    fn format(&self) -> EngineResult<(u32, u32, Option<u64>)> {
        let (mut hz, mut channels, mut samples, mut tagged) = (0, 0, 0u64, 0);
        // SAFETY: `dec` is a live decoder.
        unsafe { migo_mp3_info(self.dec, &mut hz, &mut channels, &mut samples, &mut tagged) };
        let rate = u32::try_from(hz)
            .ok()
            .filter(|r| {
                (crate::limits::MIN_SAMPLE_RATE..=crate::limits::MAX_SAMPLE_RATE).contains(r)
            })
            .ok_or_else(|| malformed("unsupported MP3 sample rate"))?;
        let channels = u32::try_from(channels)
            .ok()
            .filter(|c| (1..=2).contains(c))
            .ok_or_else(|| malformed("unsupported MP3 channel count"))?;
        let frames = (tagged != 0 && samples > 0).then(|| samples / u64::from(channels));
        Ok((rate, channels, frames))
    }

    /// Back to the first playable sample by opening again. minimp3 reaches
    /// sample 0 behind an encoder delay through its seek index, which scans
    /// the whole stream (over HTTP: waits for its whole download); opening
    /// reads only the head, so restarts and loop points stay immediate.
    fn restart(&mut self) -> EngineResult<()> {
        self.reader.error = None;
        let dec = start(&mut self.io, &mut self.reader)?;
        let old = std::mem::replace(&mut self.dec, dec);
        // SAFETY: `old` came from migo_mp3_open and is no longer referenced.
        unsafe { migo_mp3_close(old) };
        if self.format()? != (self.rate, self.channels, self.frames) {
            return Err(invalid("MP3 stream changed between passes"));
        }
        self.position = 0;
        Ok(())
    }

    fn failure(&mut self) -> EngineError {
        if let Some(error) = self.reader.error.take() {
            return source_error(error);
        }
        // SAFETY: `dec` is a live decoder.
        match unsafe { migo_mp3_last_error(self.dec) } {
            MP3D_E_MEMORY => EngineError::new(ErrorCode::OutOfMemory),
            MP3D_E_IOERROR => invalid("MP3 source IO failed"),
            code => invalid(format!("MP3 decode failed ({code})")),
        }
    }
}

/// Open a decoder over `io` (positioned by minimp3 itself): no whole-stream
/// scan, so playback starts after the first input buffer.
fn start(io: &mut Io, reader: &mut Reader) -> EngineResult<*mut c_void> {
    let mut error = 0;
    // SAFETY: `io` and `reader` are boxed by the caller and outlive the
    // decoder, so the pointers minimp3 keeps stay valid until migo_mp3_close.
    let dec = unsafe { migo_mp3_open(io, MP3D_SEEK_TO_SAMPLE | MP3D_DO_NOT_SCAN, &mut error) };
    if dec.is_null() {
        return Err(match (reader.error.take(), error) {
            (Some(error), _) => source_error(error),
            (None, MP3D_E_MEMORY) => EngineError::new(ErrorCode::OutOfMemory),
            (None, code) => malformed(format!("not a decodable MP3 stream ({code})")),
        });
    }
    Ok(dec)
}

impl PcmSource for Mp3Ex {
    fn rate(&self) -> u32 {
        self.rate
    }
    fn channels(&self) -> u32 {
        self.channels
    }
    fn frames(&self) -> Option<u64> {
        self.frames
    }

    fn read(&mut self, out: &mut Vec<f32>) -> EngineResult<bool> {
        let channels = self.channels as usize;
        if self.frames.is_some_and(|n| self.position >= n) {
            return Ok(false);
        }
        let want = self.buffer.len() / channels * channels;
        // SAFETY: the buffer holds `want` samples; `dec` is a live decoder.
        let got = unsafe { migo_mp3_read(self.dec, self.buffer.as_mut_ptr(), want) };
        if self.reader.error.is_some() {
            // The source failed (or was interrupted) during this read: report
            // it now, not on some later call. minimp3 took the short read as
            // the end of its input, so what it decoded is dropped and the
            // position stays; the owner seeks back before reading again.
            return Err(self.failure());
        }
        if got == 0 {
            // SAFETY: `dec` is a live decoder.
            let failed =
                self.reader.error.is_some() || unsafe { migo_mp3_last_error(self.dec) } != 0;
            return if failed {
                Err(self.failure())
            } else {
                Ok(false)
            };
        }
        let mut frames = (got / channels) as u64;
        if let Some(n) = self.frames {
            frames = frames.min(n - self.position);
        }
        let samples = frames as usize * channels;
        out.extend(
            self.buffer[..samples]
                .iter()
                .map(|&s| f32::from(s) / 32768.0),
        );
        self.position += frames;
        Ok(true)
    }

    fn seek(&mut self, frame: u64) -> EngineResult<()> {
        if self.frames.is_some_and(|n| frame > n) {
            return Err(invalid("seek beyond the MP3 presentation"));
        }
        if frame == 0 {
            return self.restart();
        }
        // SAFETY: `dec` is a live decoder.
        let result = unsafe { migo_mp3_seek(self.dec, frame * u64::from(self.channels)) };
        if result != 0 || self.reader.error.is_some() {
            return Err(self.failure());
        }
        self.position = frame;
        Ok(())
    }
}
