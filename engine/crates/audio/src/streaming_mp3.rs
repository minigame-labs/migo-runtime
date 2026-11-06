//! Incremental raw MP3 frames and persistent sinc, one native/DSP operation per
//! CPU turn. Fixed PCM batching keeps four transport slots useful after SRC.
use crate::decoder::mp3::{
    FrameBoundary, MAX_SAMPLES_PER_FRAME, Mp3FrameDecoder, Mp3Step, Presentation,
    STREAM_LOOKAHEAD_BYTES, append_as_f32, id3v2_tag_length, id3v2_tag_size, next_sync_candidate,
};
use crate::resample_job::bandlimited;
use crate::streaming_sinc::{STEP_SAMPLES, Stream};
use crate::streaming_transport::{Decoder, Progress, Step};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};

const RECEIVE_BYTES: usize = 64 * 1024;
const INPUT_BYTES: usize = 4096;
const NATIVE_BYTES: usize = 8192;
const SCAN_BYTES: usize = 4096;
const FRAME_BYTES: usize = 2304;
const PCM_BATCH_SAMPLES: usize = 4096;

enum Flush {
    Search,
    Hint,
    Full,
    Prefix(usize),
    Allocate(FrameBoundary),
    Begin(FrameBoundary),
    Decode(FrameBoundary),
    Done,
}
struct Window {
    frames: u64,
    local: u64,
    completed: u64,
    cycles: Option<u64>,
    skip: u64,
    presentation: crate::streaming::StreamPresentation,
}
struct Owner {
    frames: Mp3FrameDecoder,
    probe: Mp3FrameDecoder,
    buffer: Vec<u8>,
    encoded_head: usize,
    encoded_origin: u64,
    keep_encoded: bool,
    target: u32,
    source_format: (u32, u32),
    pipeline: Option<Stream>,
    scratch: Vec<f32>,
    head: usize,
    pending_dsp: bool,
    output: Vec<f32>,
    eof: bool,
    dsp_finished: bool,
    want_input: bool,
    previous: Option<usize>,
    flush: Flush,
    candidates: usize,
    presentation: Option<Presentation>,
    started: bool,
    validated: bool,
    tag_remaining: usize,
    window: Option<Window>,
    media_frames: Option<u64>,
    history: Option<u64>,
    grid: u64,
    source_pending: bool,
}

pub(crate) struct Mp3StreamDecoder {
    owner: Option<Owner>,
    error: Option<ErrorCode>,
    format: (u32, u32),
    clock: Option<crate::streaming::StreamTimeline>,
    retained: bool,
}
impl Mp3StreamDecoder {
    pub(crate) fn new(target: u32) -> Self {
        Self {
            owner: Some(Owner {
                frames: Mp3FrameDecoder::new(),
                probe: Mp3FrameDecoder::new(),
                buffer: Vec::with_capacity(RECEIVE_BYTES),
                encoded_head: 0,
                encoded_origin: 0,
                keep_encoded: false,
                target,
                source_format: (0, 0),
                pipeline: None,
                scratch: Vec::with_capacity(MAX_SAMPLES_PER_FRAME),
                head: 0,
                pending_dsp: false,
                output: Vec::with_capacity(2 * STEP_SAMPLES),
                eof: false,
                dsp_finished: false,
                want_input: true,
                previous: None,
                flush: Flush::Search,
                candidates: 0,
                presentation: None,
                started: false,
                validated: false,
                tag_remaining: 0,
                window: None,
                media_frames: None,
                history: None,
                grid: 1,
                source_pending: false,
            }),
            error: None,
            format: (0, 0),
            clock: None,
            retained: false,
        }
    }
    fn retire(&mut self, code: ErrorCode) {
        self.owner = None;
        self.error = Some(code);
    }
    fn advance(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        let Some(owner) = &mut self.owner else {
            if !input.is_empty() {
                return Err(invalid("MP3 input after EOF"));
            }
            return Ok(Step {
                consumed: 0,
                progress: Progress::Finished,
                sample_rate: self.format.0,
                channels: self.format.1,
            });
        };
        if owner.source_pending {
            return Ok(Step {
                consumed: 0,
                progress: Progress::NeedSource,
                sample_rate: self.format.0,
                channels: self.format.1,
            });
        }
        let (consumed, mut progress) = owner.advance(input, eof, cancel, self.retained)?;
        // Bounded metadata/bootstrap work may need another turn with the same
        // loan. Ask the source for input only after consuming its offered suffix.
        if progress == Progress::NeedInput && consumed < input.len() {
            progress = Progress::Yield;
        }
        if owner.source_format.0 != 0 {
            self.format = (owner.target, owner.source_format.1);
        }
        if let Some(frames) = owner.media_frames {
            let clock = crate::streaming::StreamTimeline::new(
                frames,
                owner.source_format.0,
                owner.target,
                owner.source_format.1,
            )?;
            if self.clock.is_some_and(|old| old != clock) {
                return Err(invalid("MP3 source presentation changed"));
            }
            self.clock = Some(clock);
        }
        owner.publish(pcm)?;
        let progress = if owner.dsp_finished && owner.output.is_empty() {
            Progress::Finished
        } else {
            progress
        };
        let preparing = self.retained && owner.history.is_none();
        if progress == Progress::Finished && !self.retained {
            self.owner = None;
        }
        Ok(Step {
            consumed,
            progress,
            sample_rate: if preparing { 0 } else { self.format.0 },
            channels: if preparing { 0 } else { self.format.1 },
        })
    }

    #[cfg(test)]
    pub(crate) fn buffered_len(&self) -> usize {
        self.owner.as_ref().map_or(0, |owner| owner.buffer.len())
    }
    #[cfg(test)]
    pub(crate) fn push_data(&mut self, data: &[u8]) {
        let owner = self.owner.as_mut().unwrap();
        assert!(owner.buffer.len() + data.len() <= RECEIVE_BYTES);
        owner.buffer.extend_from_slice(data);
    }
    #[cfg(test)]
    pub(crate) fn decode_into(&mut self, out: &mut Vec<f32>) -> (u32, u32) {
        self.drain(false, out)
    }
    #[cfg(test)]
    pub(crate) fn flush_into(&mut self, out: &mut Vec<f32>) -> (u32, u32) {
        self.drain(true, out)
    }
    #[cfg(test)]
    fn drain(&mut self, eof: bool, out: &mut Vec<f32>) -> (u32, u32) {
        for _ in 0..1_000_000 {
            out.reserve(STEP_SAMPLES);
            let step = self.step(&[], eof, out, &AtomicBool::new(false)).unwrap();
            if matches!(step.progress, Progress::NeedInput | Progress::Finished) {
                return (step.sample_rate, step.channels);
            }
        }
        panic!("MP3 drain did not finish");
    }
}
#[path = "streaming_mp3_control.rs"]
mod control;
impl Decoder for Mp3StreamDecoder {
    fn timeline(&self) -> EngineResult<Option<crate::streaming::StreamTimeline>> {
        Ok(self.clock)
    }
    fn presentation(&self) -> EngineResult<Option<crate::streaming::StreamPresentation>> {
        Ok(self
            .owner
            .as_ref()
            .and_then(|o| o.window.as_ref().map(|w| w.presentation))
            .or_else(|| self.clock.map(crate::streaming::StreamPresentation::single)))
    }
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if cancel.load(Ordering::Acquire) {
            self.retire(ErrorCode::Cancelled);
            pcm.clear();
            return Ok(Step {
                consumed: 0,
                progress: Progress::Cancelled,
                sample_rate: 0,
                channels: 0,
            });
        }
        if let Some(code) = self.error {
            pcm.clear();
            return Err(EngineError::from_detail(code, "MP3 stream retired"));
        }
        let result = self.advance(input, eof, pcm, cancel);
        if let Err(error) = &result {
            self.retire(error.code);
            pcm.clear();
        }
        result
    }
}
impl Owner {
    fn advance(
        &mut self,
        input: &[u8],
        eof: bool,
        cancel: &AtomicBool,
        retained: bool,
    ) -> EngineResult<(usize, Progress)> {
        if self.target == 0 || ((eof || self.eof) && !input.is_empty()) {
            return Err(invalid("MP3 requires a valid target and separate EOF"));
        }
        self.eof |= eof;
        self.keep_encoded = retained;
        if self.dsp_finished {
            return Ok((0, Progress::Yield));
        }
        if retained
            && self.history.is_none()
            && let Some(pipeline) = &mut self.pipeline
        {
            if pipeline.is_preparing() {
                pipeline.step(&[], false, &mut self.output, cancel)?;
                return Ok((0, Progress::Yield));
            }
            self.grid = pipeline.grid_period();
            self.history = Some(pipeline.quiet_source_frames()?);
        }
        if self.pending_dsp {
            if let Some(window) = &mut self.window {
                let frames =
                    ((self.scratch.len() - self.head) / self.source_format.1 as usize) as u64;
                let skip = window.skip.min(frames);
                self.head += skip as usize * self.source_format.1 as usize;
                window.skip -= skip;
            }
            let step = self.pipeline.as_mut().unwrap().step(
                &self.scratch[self.head..],
                false,
                &mut self.output,
                cancel,
            )?;
            self.head += step.consumed;
            if step.progress == bandlimited::Progress::NeedInput {
                if self.head != self.scratch.len() {
                    return Err(invalid("MP3 SRC left an unconsumed frame"));
                }
                self.scratch.clear();
                self.head = 0;
                self.pending_dsp = false;
            }
            return Ok((0, Progress::Yield));
        }
        if let Some(progress) = self.skip_id3()? {
            return Ok((0, progress));
        }
        if matches!(
            self.flush,
            Flush::Allocate(_) | Flush::Begin(_) | Flush::Decode(_)
        ) {
            self.flush_step()?;
            return Ok((0, Progress::Yield));
        }
        if self.eof {
            if matches!(self.flush, Flush::Done) && !self.validated {
                if let Some(presentation) = &self.presentation {
                    presentation.finish()?;
                }
                self.validated = true;
                return Ok((0, Progress::Yield));
            }
            if matches!(self.flush, Flush::Done)
                && self.window.as_ref().is_some_and(Window::repeats)
            {
                let window = self.window.as_mut().unwrap();
                if window.local != window.frames {
                    return Err(invalid("MP3 loop ended before its presentation"));
                }
                window.completed = window
                    .completed
                    .checked_add(1)
                    .ok_or_else(|| invalid("MP3 loop clock overflow"))?;
                window.local = 0;
                window.skip = 0;
                let cached = self.encoded_origin == 0;
                self.restart_codec(cached);
                self.source_pending = !cached;
                return Ok((0, Progress::Yield));
            }
            if matches!(self.flush, Flush::Done) {
                self.dsp_finished = if let Some(pipeline) = &mut self.pipeline {
                    pipeline.step(&[], true, &mut self.output, cancel)?.progress
                        == bandlimited::Progress::Finished
                } else {
                    true
                };
            } else {
                self.flush_step()?;
            }
            return Ok((0, Progress::Yield));
        }
        let consumed = if self.want_input || self.input_len() <= STREAM_LOOKAHEAD_BYTES {
            if self.keep_encoded
                && !input.is_empty()
                && self.buffer.len() == RECEIVE_BYTES
                && self.encoded_head != 0
            {
                self.buffer.drain(..self.encoded_head);
                self.encoded_origin += self.encoded_head as u64;
                self.encoded_head = 0;
            }
            let count = input
                .len()
                .min(INPUT_BYTES)
                .min(RECEIVE_BYTES - self.buffer.len());
            self.buffer.extend_from_slice(&input[..count]);
            count
        } else {
            0
        };
        if let Some(progress) = self.skip_id3()? {
            return Ok((consumed, progress));
        }
        if !self.started {
            // The existing native probe needs at most this fixed lookahead,
            // including three maximum free-format frames. Never wait for EOF.
            if self.input_len() < NATIVE_BYTES {
                self.want_input = true;
                return Ok((consumed, Progress::NeedInput));
            }
            self.want_input = false;
            self.flush_step()?;
            if matches!(self.flush, Flush::Done) {
                self.flush = Flush::Search;
            }
            if self.input_len() < NATIVE_BYTES {
                self.want_input = true;
            }
            return Ok((consumed, Progress::Yield));
        }
        if self.input_len() <= STREAM_LOOKAHEAD_BYTES {
            return Ok((consumed, Progress::NeedInput));
        }
        let size = self.input_len().min(NATIVE_BYTES);
        if self.decode_frame(size, None)? {
            self.want_input = self.input_len() <= STREAM_LOOKAHEAD_BYTES;
            Ok((consumed, Progress::Yield))
        } else {
            if self.input_len() == RECEIVE_BYTES {
                return Err(invalid("MP3 retained input is full without progress"));
            }
            self.want_input = true;
            Ok((consumed, Progress::NeedInput))
        }
    }
    fn input_len(&self) -> usize {
        self.buffer.len() - self.encoded_head
    }
    fn consume_encoded(&mut self, count: usize) {
        if self.keep_encoded {
            self.encoded_head += count;
        } else {
            self.buffer.drain(..count);
        }
    }
    fn skip_id3(&mut self) -> EngineResult<Option<Progress>> {
        if self.started || !matches!(self.flush, Flush::Search) {
            return Ok(None);
        }
        if self.tag_remaining == 0 {
            self.tag_remaining = id3v2_tag_size(&self.buffer[self.encoded_head..]).unwrap_or(0);
        }
        if self.tag_remaining == 0 {
            return Ok(None);
        }
        let count = self.tag_remaining.min(self.input_len()).min(SCAN_BYTES);
        self.consume_encoded(count);
        self.tag_remaining -= count;
        if self.eof && self.tag_remaining != 0 && self.input_len() == 0 {
            return Err(invalid("MP3 ended inside ID3 metadata"));
        }
        if count == 0 {
            return Ok(None);
        }
        self.want_input = self.input_len() == 0;
        Ok(Some(
            if self.tag_remaining != 0 && self.input_len() == 0 && !self.eof {
                Progress::NeedInput
            } else {
                Progress::Yield
            },
        ))
    }
    fn publish(&mut self, pcm: &mut Vec<f32>) -> EngineResult<()> {
        if self.output.len() < PCM_BATCH_SAMPLES && !self.dsp_finished {
            return Ok(());
        }
        let count = self.output.len().min(STEP_SAMPLES);
        if pcm.capacity().saturating_sub(pcm.len()) < count {
            return Err(invalid("MP3 PCM has insufficient reserved capacity"));
        }
        pcm.extend_from_slice(&self.output[..count]);
        self.output.drain(..count);
        Ok(())
    }
    fn decode_frame(&mut self, size: usize, isolated: Option<usize>) -> EngineResult<bool> {
        let decoded = if let Some(presentation) = &mut self.presentation {
            self.frames.decode_presented(
                &self.buffer[self.encoded_head..self.encoded_head + size],
                presentation,
            )?
        } else {
            return Err(invalid("MP3 presentation was not initialized"));
        };
        let (consumed, format) = match decoded {
            Mp3Step::NeedMoreData => (isolated.unwrap_or(0), None),
            Mp3Step::Skipped(count) => (isolated.unwrap_or(count), None),
            Mp3Step::Frame {
                pcm,
                sample_rate,
                channels,
                consumed,
            } => {
                self.scratch.clear();
                append_as_f32(pcm, &mut self.scratch);
                (consumed, Some((sample_rate, channels)))
            }
        };
        if consumed > self.input_len() {
            return Err(invalid("MP3 native consumed beyond its input"));
        }
        if let Some(format) = format {
            if self.source_format.0 == 0 {
                self.pipeline = Some(Stream::new(format.0, self.target, format.1, None)?);
                self.source_format = format;
            } else if self.source_format != format {
                return Err(invalid("MP3 source format changed"));
            }
            if let Some(window) = &mut self.window {
                window.local = window
                    .local
                    .checked_add((self.scratch.len() / format.1 as usize) as u64)
                    .filter(|n| *n <= window.frames)
                    .ok_or_else(|| invalid("MP3 loop exceeds its presentation"))?;
            }
            self.head = 0;
            self.pending_dsp = true;
        }
        if consumed > 0 {
            self.previous = (consumed <= FRAME_BYTES).then_some(consumed);
            self.consume_encoded(consumed);
        }
        Ok(consumed > 0)
    }
    fn flush_step(&mut self) -> EngineResult<()> {
        let phase = std::mem::replace(&mut self.flush, Flush::Search);
        match phase {
            Flush::Search => {
                if let Some(length) = id3v2_tag_length(&self.buffer[self.encoded_head..]) {
                    self.consume_encoded(length);
                    return Ok(());
                }
                let count = self.input_len().min(SCAN_BYTES);
                if let Some(offset) = next_sync_candidate(
                    &self.buffer[self.encoded_head..self.encoded_head + count],
                    0,
                ) {
                    self.consume_encoded(offset);
                    self.flush = if self.previous.is_some() {
                        Flush::Hint
                    } else {
                        Flush::Full
                    };
                } else if count == self.input_len() {
                    self.consume_encoded(self.input_len());
                    self.flush = Flush::Done;
                } else {
                    self.consume_encoded(count - 1);
                }
            }
            Flush::Allocate(frame) => {
                self.presentation = Some(Presentation::allocate(&frame)?);
                self.flush = Flush::Begin(frame);
            }
            Flush::Begin(frame) => {
                let presentation = self.presentation.as_mut().unwrap();
                let skip_tag = presentation.begin(
                    &self.buffer[self.encoded_head..self.encoded_head + frame.length],
                    &frame,
                    true,
                )?;
                let format = presentation.format();
                let frames = presentation
                    .expected
                    .map(|n| (n / format.1 as usize) as u64);
                if self.source_format.0 != 0
                    && (self.source_format != format || self.media_frames != frames)
                {
                    return Err(invalid("MP3 replay changed its source presentation"));
                }
                self.source_format = format;
                self.media_frames = frames;
                if self.pipeline.is_none() {
                    self.pipeline = Some(Stream::new(format.0, self.target, format.1, frames)?);
                }
                self.started = true;
                if skip_tag {
                    self.consume_encoded(frame.length);
                    self.previous = Some(frame.length);
                } else {
                    self.flush = Flush::Decode(frame);
                }
            }
            Flush::Decode(frame) => {
                let size = if frame.free_format && !self.frames.has_free_format_state() {
                    frame.view
                } else {
                    frame.length
                };
                self.decode_frame(size, Some(frame.length))?;
                self.previous = Some(frame.length);
                self.candidates = 0;
            }
            Flush::Done => self.flush = Flush::Done,
            probe => {
                let size = match probe {
                    Flush::Hint => self.previous.unwrap().min(self.input_len()),
                    Flush::Full => self.input_len().min(NATIVE_BYTES),
                    Flush::Prefix(length) => length,
                    _ => unreachable!(),
                };
                if self.probe.has_free_format_state() && !self.frames.has_free_format_state() {
                    self.probe.invalidate_header();
                }
                let measured = self
                    .probe
                    .measured_frame(&self.buffer[self.encoded_head..self.encoded_head + size]);
                if let Some(mut frame) = measured
                    && (matches!(probe, Flush::Full) || frame.offset + frame.length == size)
                {
                    self.consume_encoded(frame.offset);
                    frame.offset = 0;
                    self.flush = if self.started {
                        Flush::Decode(frame)
                    } else {
                        Flush::Allocate(frame)
                    };
                } else {
                    self.flush = match probe {
                        Flush::Hint => Flush::Full,
                        Flush::Full if self.input_len() >= 4 => Flush::Prefix(4),
                        Flush::Prefix(length) if length < self.input_len().min(FRAME_BYTES) => {
                            Flush::Prefix(length + 1)
                        }
                        _ => {
                            self.candidates += 1;
                            if self.candidates > 32 {
                                self.consume_encoded(self.input_len());
                                Flush::Done
                            } else {
                                self.consume_encoded(1);
                                Flush::Search
                            }
                        }
                    };
                }
            }
        }
        Ok(())
    }
}
fn invalid(detail: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, detail)
}

#[cfg(test)]
#[path = "streaming_mp3_tests.rs"]
mod tests;
