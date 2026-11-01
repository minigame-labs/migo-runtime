//! Incremental raw MP3 frames and persistent sinc, one native/DSP operation per
//! CPU turn. Fixed PCM batching keeps four transport slots useful after SRC.
use crate::decoder::mp3::{
    FrameBoundary, MAX_SAMPLES_PER_FRAME, Mp3FrameDecoder, Mp3Step, STREAM_LOOKAHEAD_BYTES,
    append_as_f32, id3v2_tag_length, next_sync_candidate,
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
    Decode(FrameBoundary),
    Done,
}
struct Owner {
    frames: Mp3FrameDecoder,
    probe: Mp3FrameDecoder,
    buffer: Vec<u8>,
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
}

pub(crate) struct Mp3StreamDecoder {
    owner: Option<Owner>,
    error: Option<ErrorCode>,
    format: (u32, u32),
}
impl Mp3StreamDecoder {
    pub(crate) fn new(target: u32) -> Self {
        Self {
            owner: Some(Owner {
                frames: Mp3FrameDecoder::new(),
                probe: Mp3FrameDecoder::new(),
                buffer: Vec::with_capacity(RECEIVE_BYTES),
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
            }),
            error: None,
            format: (0, 0),
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
        let (consumed, progress) = owner.advance(input, eof, cancel)?;
        if owner.source_format.0 != 0 {
            self.format = (owner.target, owner.source_format.1);
        }
        owner.publish(pcm)?;
        let progress = if owner.dsp_finished && owner.output.is_empty() {
            Progress::Finished
        } else {
            progress
        };
        if progress == Progress::Finished {
            self.owner = None;
        }
        Ok(Step {
            consumed,
            progress,
            sample_rate: self.format.0,
            channels: self.format.1,
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
impl Decoder for Mp3StreamDecoder {
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
    ) -> EngineResult<(usize, Progress)> {
        if self.target == 0 || ((eof || self.eof) && !input.is_empty()) {
            return Err(invalid("MP3 requires a valid target and separate EOF"));
        }
        self.eof |= eof;
        if self.dsp_finished {
            return Ok((0, Progress::Yield));
        }
        if self.pending_dsp {
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
        if self.eof {
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
        let consumed = if self.want_input || self.buffer.len() <= STREAM_LOOKAHEAD_BYTES {
            let count = input
                .len()
                .min(INPUT_BYTES)
                .min(RECEIVE_BYTES - self.buffer.len());
            self.buffer.extend_from_slice(&input[..count]);
            count
        } else {
            0
        };
        if self.buffer.len() <= STREAM_LOOKAHEAD_BYTES {
            return Ok((consumed, Progress::NeedInput));
        }
        let size = self.buffer.len().min(NATIVE_BYTES);
        if self.decode_frame(size, None)? {
            self.want_input = self.buffer.len() <= STREAM_LOOKAHEAD_BYTES;
            Ok((consumed, Progress::Yield))
        } else {
            if self.buffer.len() == RECEIVE_BYTES {
                return Err(invalid("MP3 retained input is full without progress"));
            }
            self.want_input = true;
            Ok((consumed, Progress::NeedInput))
        }
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
        let (consumed, format) = match self.frames.decode(&self.buffer[..size]) {
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
        if consumed > self.buffer.len() {
            return Err(invalid("MP3 native consumed beyond its input"));
        }
        if let Some(format) = format {
            if self.source_format.0 == 0 {
                self.pipeline = Some(Stream::new(format.0, self.target, format.1, None)?);
                self.source_format = format;
            } else if self.source_format != format {
                return Err(invalid("MP3 source format changed"));
            }
            self.head = 0;
            self.pending_dsp = true;
        }
        if consumed > 0 {
            self.previous = (consumed <= FRAME_BYTES).then_some(consumed);
            self.buffer.drain(..consumed);
        }
        Ok(consumed > 0)
    }
    fn flush_step(&mut self) -> EngineResult<()> {
        let phase = std::mem::replace(&mut self.flush, Flush::Search);
        match phase {
            Flush::Search => {
                if let Some(length) = id3v2_tag_length(&self.buffer) {
                    self.buffer.drain(..length);
                    return Ok(());
                }
                let count = self.buffer.len().min(SCAN_BYTES);
                if let Some(offset) = next_sync_candidate(&self.buffer[..count], 0) {
                    self.buffer.drain(..offset);
                    self.flush = if self.previous.is_some() {
                        Flush::Hint
                    } else {
                        Flush::Full
                    };
                } else if count == self.buffer.len() {
                    self.buffer.clear();
                    self.flush = Flush::Done;
                } else {
                    self.buffer.drain(..count - 1);
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
                    Flush::Hint => self.previous.unwrap().min(self.buffer.len()),
                    Flush::Full => self.buffer.len().min(NATIVE_BYTES),
                    Flush::Prefix(length) => length,
                    _ => unreachable!(),
                };
                if self.probe.has_free_format_state() && !self.frames.has_free_format_state() {
                    self.probe.invalidate_header();
                }
                let measured = self.probe.measured_frame(&self.buffer[..size]);
                if let Some(mut frame) = measured
                    && (matches!(probe, Flush::Full) || frame.offset + frame.length == size)
                {
                    self.buffer.drain(..frame.offset);
                    frame.offset = 0;
                    self.flush = Flush::Decode(frame);
                } else {
                    self.flush = match probe {
                        Flush::Hint => Flush::Full,
                        Flush::Full if self.buffer.len() >= 4 => Flush::Prefix(4),
                        Flush::Prefix(length) if length < self.buffer.len().min(FRAME_BYTES) => {
                            Flush::Prefix(length + 1)
                        }
                        _ => {
                            self.candidates += 1;
                            if self.candidates > 32 {
                                self.buffer.clear();
                                Flush::Done
                            } else {
                                self.buffer.drain(..1);
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
