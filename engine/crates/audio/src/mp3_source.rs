//! MP3 through upstream minimp3's frame decoder (native/mp3.c), with the input
//! window, frame index and LAME/Xing gapless trimming of minimp3_ex's pull
//! reader (`mp3dec_ex`) reproduced here step for step, calling the same
//! minimp3 functions. Two things change, both about how much input a step
//! waits for: playback starts after one 16 KiB window instead of mp3dec_ex's
//! 128 KiB fill, and the frame index grows only as far as a seek needs instead
//! of scanning the whole stream on the first seek. Over HTTP that is starting
//! and seeking now rather than after much (or all) of the download. The PCM is
//! mp3dec_ex's, sample for sample: the tests keep it as the oracle.
//!
//! Like the other PCM sources it runs only on a stream's own decode thread.
use crate::pcm_source::PcmSource;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::ffi::{c_int, c_void};
use std::io::{self, Read, Seek, SeekFrom};
use symphonia_core::io::MediaSource;

#[repr(C)]
#[derive(Default)]
struct FrameInfo {
    frame_bytes: c_int,
    frame_offset: c_int,
    channels: c_int,
    hz: c_int,
    layer: c_int,
    bitrate_kbps: c_int,
}

unsafe extern "C" {
    fn migo_mp3_frame_new() -> *mut c_void;
    fn migo_mp3_frame_free(dec: *mut c_void);
    fn migo_mp3_frame_reset(dec: *mut c_void);
    fn migo_mp3_frame_decode(
        dec: *mut c_void,
        buf: *const u8,
        len: c_int,
        pcm: *mut i16,
        info: *mut FrameInfo,
    ) -> c_int;
    fn migo_mp3_find_frame(buf: *const u8, len: c_int, frame_size: *mut c_int) -> c_int;
    fn migo_mp3_frame_samples(hdr: *const u8) -> c_int;
    fn migo_mp3_frame_layer(hdr: *const u8) -> c_int;
    fn migo_mp3_frame_channels(hdr: *const u8) -> c_int;
    fn migo_mp3_frame_hz(hdr: *const u8) -> c_int;
    fn migo_mp3_vbr_tag(
        frame: *const u8,
        size: c_int,
        frames: *mut u32,
        delay: *mut c_int,
        padding: *mut c_int,
    ) -> c_int;
    fn migo_mp3_id3v2_size(buf: *const u8, len: usize) -> usize;
    fn migo_mp3_trim_id3v1(buf: *const u8, len: *mut usize);
    fn migo_mp3_main_data_bytes(frame: *const u8, size: c_int) -> c_int;
}

/// mp3dec_ex refills its input below this (MINIMP3_BUF_SIZE): ten
/// consecutive frames, all that frame sync looks ahead.
const WINDOW: usize = 16 * 1024;
/// MINIMP3_ID3_DETECT_SIZE.
const ID3_DETECT: usize = 10;
/// MINIMP3_PREDECODE_FRAMES and the Layer III bit reservoir a seek refills.
const PREDECODE_FRAMES: usize = 2;
const RESERVOIR_BYTES: i64 = 511;
/// Leading frames decoded to find where samples start (mp3dec_load_index).
const LEADING_FRAMES: usize = 256;
const MAX_SAMPLES_PER_FRAME: usize = 1152 * 2;
/// One read: about four frames of stereo.
const READ_SAMPLES: usize = 4 * 1152 * 2;

/// A frame as mp3dec_ex indexes it: its offset and size, and the interleaved
/// sample (encoder delay included) where its output begins.
#[derive(Clone, Copy)]
struct Frame {
    offset: u64,
    sample: u64,
    size: u32,
}

/// Bytes of the source from `base`, read in order, with mp3dec_ex's end of
/// stream: a trailing ID3v1/APE tag is dropped once the source ends.
struct Window {
    buf: Vec<u8>,
    head: usize,
    base: u64,
    eof: bool,
}

impl Window {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(2 * WINDOW),
            head: 0,
            base: 0,
            eof: false,
        }
    }

    fn reset(&mut self, source: &mut dyn MediaSource, offset: u64) -> io::Result<()> {
        source.seek(SeekFrom::Start(offset))?;
        self.buf.clear();
        self.head = 0;
        self.base = offset;
        self.eof = false;
        Ok(())
    }

    fn data(&self) -> &[u8] {
        &self.buf[self.head..]
    }

    fn offset(&self) -> u64 {
        self.base + self.head as u64
    }

    fn consume(&mut self, n: usize) {
        self.head = (self.head + n).min(self.buf.len());
    }

    /// At least `want` bytes ahead, or the whole rest of the stream.
    fn fill(&mut self, source: &mut dyn MediaSource, want: usize) -> io::Result<()> {
        if self.eof || self.data().len() >= want {
            return Ok(());
        }
        if self.head > 0 {
            self.buf.drain(..self.head);
            self.base += self.head as u64;
            self.head = 0;
        }
        let target = want.max(2 * WINDOW);
        while self.buf.len() < want {
            let start = self.buf.len();
            // The index and seeks read the same source elsewhere: continue from
            // this window's own end.
            source.seek(SeekFrom::Start(self.base + start as u64))?;
            self.buf.resize(target, 0);
            let read = source.read(&mut self.buf[start..]);
            let n = match read {
                Ok(n) => n,
                Err(error) => {
                    self.buf.truncate(start);
                    return Err(error);
                }
            };
            self.buf.truncate(start + n);
            if n == 0 {
                self.eof = true;
                let mut len = self.buf.len();
                // SAFETY: the pointer and length name the window's own bytes.
                unsafe { migo_mp3_trim_id3v1(self.buf.as_ptr(), &mut len) };
                self.buf.truncate(len);
                break;
            }
        }
        Ok(())
    }
}

/// An owned minimp3 frame decoder (`mp3dec_t`).
struct FrameDecoder(*mut c_void);

impl FrameDecoder {
    fn new() -> EngineResult<Self> {
        // SAFETY: allocation and initialisation only.
        let dec = unsafe { migo_mp3_frame_new() };
        if dec.is_null() {
            return Err(EngineError::new(ErrorCode::OutOfMemory));
        }
        Ok(Self(dec))
    }

    fn reset(&mut self) {
        // SAFETY: a live decoder.
        unsafe { migo_mp3_frame_reset(self.0) };
    }

    /// Decode the next frame in `data` into `pcm`: its samples per channel.
    fn decode(&mut self, data: &[u8], pcm: &mut [i16]) -> (usize, FrameInfo) {
        assert!(pcm.len() >= MAX_SAMPLES_PER_FRAME);
        let mut info = FrameInfo::default();
        let len = c_int::try_from(data.len()).unwrap_or(c_int::MAX);
        // SAFETY: `data` holds `len` readable bytes and `pcm` room for a whole
        // frame; `self.0` is a live decoder.
        let samples = unsafe {
            migo_mp3_frame_decode(self.0, data.as_ptr(), len, pcm.as_mut_ptr(), &mut info)
        };
        (samples.max(0) as usize, info)
    }
}

impl Drop for FrameDecoder {
    fn drop(&mut self) {
        // SAFETY: allocated by migo_mp3_frame_new, freed once.
        unsafe { migo_mp3_frame_free(self.0) };
    }
}

// SAFETY: a decoder is owned by its source and only used through `&mut self`
// on the decode thread.
unsafe impl Send for FrameDecoder {}

/// Samples per channel of the frame whose header starts `hdr` (four bytes).
fn hdr_samples(hdr: &[u8]) -> u64 {
    if hdr.len() < 4 {
        return 0;
    }
    // SAFETY: four readable bytes.
    u64::from(unsafe { migo_mp3_frame_samples(hdr.as_ptr()) }.max(0) as u32)
}

/// The next frame mp3dec_iterate_cb would find: its offset and size, or the
/// bytes to skip when the window holds only junk.
enum Found {
    Frame(usize, usize),
    Junk(usize),
    End,
}

fn find(data: &[u8]) -> Found {
    let mut size = 0;
    let len = c_int::try_from(data.len()).unwrap_or(c_int::MAX);
    // SAFETY: `data` holds `len` readable bytes.
    let i = unsafe { migo_mp3_find_frame(data.as_ptr(), len, &mut size) }.max(0) as usize;
    match (i, size) {
        (_, size) if size > 0 => Found::Frame(i, size as usize),
        (0, _) => Found::End,
        (i, _) => Found::Junk(i),
    }
}

/// The frame index, built as mp3dec_load_index builds it, but only as far as
/// it is asked to reach.
struct Index {
    frames: Vec<Frame>,
    window: Window,
    /// Leading frames are decoded until one yields samples (a first frame can
    /// lack its bit reservoir), with a decoder of its own as in mp3dec_ex.
    leading: FrameDecoder,
    leading_done: bool,
    samples: u64,
    started: bool,
    complete: bool,
}

pub(crate) struct Minimp3 {
    source: Box<dyn MediaSource>,
    dec: FrameDecoder,
    window: Window,
    index: Index,
    rate: u32,
    channels: u32,
    layer: c_int,
    frames: Option<u64>,
    /// Interleaved: encoder delay, samples still to drop, the presentation
    /// length, and samples output so far (mp3dec_ex's cur_sample).
    start_delay: u64,
    to_skip: u64,
    detected: Option<u64>,
    cur_sample: u64,
    start_offset: u64,
    failed: Option<EngineError>,
    pcm: Vec<i16>,
    scratch: Vec<i16>,
}

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

/// Start `window` at `at`, past a leading ID3v2 tag as mp3dec_iterate_cb skips it.
fn start_after_id3v2(source: &mut dyn MediaSource, window: &mut Window, at: u64) -> io::Result<()> {
    window.reset(source, at)?;
    window.fill(source, ID3_DETECT)?;
    let data = window.data();
    if data.len() < ID3_DETECT {
        return Ok(());
    }
    // SAFETY: at least ID3_DETECT readable bytes.
    let size = unsafe { migo_mp3_id3v2_size(data.as_ptr(), ID3_DETECT) } as u64;
    if size > 0 {
        window.reset(source, at + size)?;
    }
    Ok(())
}

impl Minimp3 {
    pub(crate) fn open(mut source: Box<dyn MediaSource>) -> EngineResult<Self> {
        let mut window = Window::new();
        start_after_id3v2(source.as_mut(), &mut window, 0).map_err(source_error)?;
        // The first frame, as mp3dec_load_index meets it at open.
        let (offset, frame) = loop {
            window.fill(source.as_mut(), WINDOW).map_err(source_error)?;
            match find(window.data()) {
                Found::Frame(i, size) => {
                    let data = window.data();
                    break (window.offset() + i as u64, data[i..i + size].to_vec());
                }
                Found::Junk(i) => window.consume(i),
                Found::End => return Err(malformed("not a decodable MP3 stream")),
            }
        };
        // SAFETY: `frame` is a located frame, at least a header long.
        let (hz, channels, layer) = unsafe {
            (
                migo_mp3_frame_hz(frame.as_ptr()),
                migo_mp3_frame_channels(frame.as_ptr()),
                migo_mp3_frame_layer(frame.as_ptr()),
            )
        };
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
        let ch = u64::from(channels);
        let mut start_offset = offset;
        let (mut start_delay, mut detected) = (0, None);
        if layer == 3 {
            let (mut frames, mut delay, mut padding) = (0u32, 0, 0);
            // SAFETY: `frame` holds the whole frame.
            let tag = unsafe {
                migo_mp3_vbr_tag(
                    frame.as_ptr(),
                    frame.len() as c_int,
                    &mut frames,
                    &mut delay,
                    &mut padding,
                )
            };
            if tag != 0 {
                start_offset = offset + frame.len() as u64;
            }
            if tag > 0 {
                let padding = i64::from(padding) * ch as i64;
                start_delay = (i64::from(delay) * ch as i64).max(0) as u64;
                let mut samples = hdr_samples(&frame) * ch * u64::from(frames);
                if samples >= start_delay {
                    samples -= start_delay;
                }
                if padding > 0 && samples >= padding as u64 {
                    samples -= padding as u64;
                }
                detected = Some(samples);
            }
        }
        window
            .reset(source.as_mut(), start_offset)
            .map_err(source_error)?;
        Ok(Self {
            source,
            dec: FrameDecoder::new()?,
            window,
            index: Index {
                frames: Vec::new(),
                window: Window::new(),
                leading: FrameDecoder::new()?,
                leading_done: false,
                samples: 0,
                started: false,
                complete: false,
            },
            rate,
            channels,
            layer,
            frames: detected.filter(|n| *n > 0).map(|n| n / ch),
            start_delay,
            to_skip: start_delay,
            detected,
            cur_sample: 0,
            start_offset,
            failed: None,
            pcm: vec![0; MAX_SAMPLES_PER_FRAME],
            scratch: vec![0; MAX_SAMPLES_PER_FRAME],
        })
    }

    /// Extend the index (mp3dec_iterate_cb with mp3dec_load_index from the
    /// first audio frame) until a frame begins at or after `position`, or it
    /// holds the whole stream.
    fn index_through(&mut self, position: u64) -> EngineResult<()> {
        let ch = u64::from(self.channels);
        let index = &mut self.index;
        let source = self.source.as_mut();
        if !index.started {
            start_after_id3v2(source, &mut index.window, self.start_offset)
                .map_err(source_error)?;
            index.started = true;
        }
        while !index.complete && index.frames.last().is_none_or(|f| f.sample < position) {
            index.window.fill(source, WINDOW).map_err(source_error)?;
            let (i, size) = match find(index.window.data()) {
                Found::Frame(i, size) => (i, size),
                Found::Junk(i) => {
                    index.window.consume(i);
                    continue;
                }
                Found::End => {
                    index.complete = true;
                    break;
                }
            };
            index.frames.push(Frame {
                offset: index.window.offset() + i as u64,
                sample: index.samples,
                size: size as u32,
            });
            let data = &index.window.data()[i..];
            if !index.leading_done && index.frames.len() < LEADING_FRAMES {
                let (decoded, info) = index.leading.decode(data, &mut self.scratch);
                index.samples += decoded as u64 * u64::from(info.channels.max(0) as u32);
                index.leading_done = decoded != 0;
            } else {
                index.samples += hdr_samples(data) * ch;
            }
            index.window.consume(i + size);
        }
        Ok(())
    }

    /// Bytes of one indexed frame.
    fn frame_bytes(&mut self, frame: Frame) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; frame.size as usize];
        self.source.seek(SeekFrom::Start(frame.offset))?;
        self.source.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// mp3dec_ex_seek: decode from the frame holding `position` (interleaved,
    /// encoder delay included), refilling the bit reservoir before it.
    fn reposition(&mut self, position: u64) -> EngineResult<()> {
        let mut target = (self.start_offset, 0);
        if position != 0 {
            self.index_through(position)?;
        }
        if position != 0 && !self.index.frames.is_empty() {
            // The last frame starting before `position`, or one starting at it.
            let frames = &self.index.frames;
            let mut i = frames.partition_point(|f| f.sample < position);
            if i == frames.len() || frames[i].sample != position {
                i = i.saturating_sub(1);
            }
            if i > 0 {
                i -= i.min(PREDECODE_FRAMES);
                if self.layer == 3 {
                    let mut to_fill = RESERVOIR_BYTES;
                    while i > 0 && to_fill > 0 {
                        let previous = self.index.frames[i - 1];
                        let bytes = self.frame_bytes(previous).map_err(source_error)?;
                        i -= 1;
                        // SAFETY: `bytes` holds the whole frame.
                        let main = unsafe {
                            migo_mp3_main_data_bytes(bytes.as_ptr(), bytes.len() as c_int)
                        };
                        if main < 0 {
                            break;
                        }
                        to_fill -= to_fill.min(i64::from(main));
                    }
                }
            }
            let offset = self.index.frames[i].offset;
            let mut to_skip = position.saturating_sub(self.index.frames[i].sample);
            // Frames before the first that decodes count only their length.
            while i + 1 < self.index.frames.len()
                && self.index.frames[i].sample == 0
                && self.index.frames[i + 1].sample == 0
            {
                let frame = self.index.frames[i];
                let bytes = self.frame_bytes(frame).map_err(source_error)?;
                to_skip += hdr_samples(&bytes) * u64::from(self.channels);
                i += 1;
            }
            target = (offset, to_skip);
        }
        self.window
            .reset(self.source.as_mut(), target.0)
            .map_err(source_error)?;
        self.dec.reset();
        self.to_skip = target.1;
        self.failed = None;
        Ok(())
    }

    fn fail(&mut self, error: EngineError) -> EngineError {
        self.failed = Some(error.clone());
        error
    }

    /// mp3dec_ex_read_frame: the next frame's output, as a range of `pcm`
    /// (empty at the end of the presentation).
    fn next_frame(&mut self) -> EngineResult<std::ops::Range<usize>> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if self.detected.is_some_and(|n| self.cur_sample >= n) {
            return Ok(0..0);
        }
        loop {
            self.window
                .fill(self.source.as_mut(), WINDOW)
                .map_err(source_error)?;
            let data = self.window.data();
            if data.is_empty() {
                return Ok(0..0);
            }
            // A frame that decodes to nothing still counts toward the delay,
            // by the length its window start declares.
            let silent = hdr_samples(data);
            let (samples, info) = self.dec.decode(data, &mut self.pcm);
            self.window.consume(info.frame_bytes.max(0) as usize);
            if info.hz as u32 != self.rate || info.layer != self.layer {
                return Err(self.fail(invalid("MP3 decode failed (-5)")));
            }
            let frame_channels = u64::from(info.channels.max(0) as u32);
            if samples == 0 {
                self.to_skip -= self.to_skip.min(silent * frame_channels);
                continue;
            }
            let total = samples * frame_channels as usize;
            let skip = (self.to_skip as usize).min(total);
            self.to_skip -= skip as u64;
            if skip != total && info.channels as u32 != self.channels {
                return Err(self.fail(invalid("MP3 decode failed (-5)")));
            }
            if skip == total {
                continue;
            }
            let mut end = total;
            if let Some(n) = self.detected
                && self.cur_sample + (end - skip) as u64 >= n
            {
                end = skip + (n - self.cur_sample) as usize;
            }
            self.cur_sample += (end - skip) as u64;
            return Ok(skip..end);
        }
    }
}

impl PcmSource for Minimp3 {
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
        let start = out.len();
        while out.len() - start < READ_SAMPLES {
            let range = self.next_frame()?;
            if range.is_empty() {
                break;
            }
            out.extend(self.pcm[range].iter().map(|&s| f32::from(s) / 32768.0));
        }
        Ok(out.len() > start)
    }

    fn seek(&mut self, frame: u64) -> EngineResult<()> {
        if self.frames.is_some_and(|n| frame > n) {
            return Err(invalid("seek beyond the MP3 presentation"));
        }
        let cur_sample = frame * u64::from(self.channels);
        self.reposition(cur_sample + self.start_delay)?;
        self.cur_sample = cur_sample;
        Ok(())
    }
}

#[cfg(test)]
#[path = "mp3_source_tests.rs"]
mod tests;
