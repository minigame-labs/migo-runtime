use super::*;
use crate::decoder::aac::incremental::{Decoder as Packets, Progress as PacketProgress};
use crate::resample_job::bandlimited;
use crate::streaming_sinc::{STEP_SAMPLES, Stream};

const INPUT_BYTES: usize = 8192;
const PCM_BATCH_SAMPLES: usize = 4096;
struct Window {
    frames: u64,
    local: u64,
    completed: u64,
    cycles: Option<u64>,
    presentation: crate::streaming::StreamPresentation,
}
impl Window {
    fn repeats(&self) -> bool {
        self.cycles
            .is_none_or(|end| self.completed.checked_add(1).is_some_and(|next| next < end))
    }
}
pub(super) struct Owner<P: Packetizer> {
    pub(super) decoder: Packets<P>,
    native: Vec<f32>,
    head: usize,
    pipeline: Option<Stream>,
    output: Vec<f32>,
    output_head: usize,
    pub(super) input_ended: bool,
    codec_done: bool,
    source_done: bool,
    filtered: bool,
    done: bool,
    target: u32,
    window: Option<Window>,
    history: Option<u64>,
    grid: u64,
}
pub(crate) struct PacketStream<P: Packetizer> {
    pub(super) owner: Option<Owner<P>>,
    format: (u32, u32),
    clock: Option<crate::streaming::StreamTimeline>,
    error: Option<ErrorCode>,
    retained: bool,
}
impl<P: Packetizer> PacketStream<P> {
    #[cfg(test)]
    pub(super) fn watches(&self) -> Vec<(migo_alloc_probe::DeallocationWatch, usize)> {
        let o = self.owner.as_ref().unwrap();
        let (input, bytes) = o.decoder.input_allocation();
        [
            (input, bytes),
            (o.native.as_ptr().cast(), o.native.capacity() * 4),
            (o.output.as_ptr().cast(), o.output.capacity() * 4),
        ]
        .into_iter()
        .map(|(p, n)| (migo_alloc_probe::DeallocationWatch::new(p).unwrap(), n))
        .collect()
    }
    pub(super) fn new(target: u32) -> Self {
        Self::with_decoder(target, Packets::new(None))
    }
    pub(crate) fn retain(&mut self) {
        self.retained = true;
        self.owner.as_mut().unwrap().decoder.retain_history();
    }
    pub(super) fn with_decoder(target: u32, decoder: Packets<P>) -> Self {
        Self {
            owner: Some(Owner {
                decoder,
                native: Vec::with_capacity(P::OUTPUT_SAMPLES_PER_STEP),
                head: 0,
                pipeline: None,
                output: Vec::with_capacity(P::OUTPUT_SAMPLES_PER_STEP.max(2 * STEP_SAMPLES)),
                output_head: 0,
                input_ended: false,
                codec_done: false,
                source_done: false,
                filtered: false,
                done: false,
                target,
                window: None,
                history: None,
                grid: 1,
            }),
            format: (0, 0),
            clock: None,
            error: None,
            retained: false,
        }
    }
    fn advance(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if let Some(code) = self.error {
            return Err(EngineError::from_detail(code, "packet stream retired"));
        }
        if cancel.load(Ordering::Acquire) {
            self.owner = None;
            self.error = Some(ErrorCode::Cancelled);
            return Ok(self.outcome(0, Progress::Cancelled));
        }
        let Some(owner) = self.owner.as_mut() else {
            if !input.is_empty() {
                return Err(invalid("packet stream input after EOF"));
            }
            return Ok(self.outcome(0, Progress::Finished));
        };
        let ready = owner.output.len() - owner.output_head;
        if ready >= PCM_BATCH_SAMPLES || (owner.codec_done && ready != 0) || owner.done {
            let channels = self.format.1 as usize;
            let n = ready.min(STEP_SAMPLES) / channels * channels;
            if pcm.capacity() - pcm.len() < n {
                return Err(invalid("packet stream PCM loan capacity is insufficient"));
            }
            pcm.extend_from_slice(&owner.output[owner.output_head..owner.output_head + n]);
            owner.output_head += n;
            let done = owner.done && owner.output_head == owner.output.len();
            if owner.output_head == owner.output.len() {
                owner.output.clear();
                owner.output_head = 0;
            }
            if done && !self.retained {
                self.owner = None;
            }
            return Ok(self.outcome(
                0,
                if done {
                    Progress::Finished
                } else {
                    Progress::Yield
                },
            ));
        }
        if owner.output_head != 0 {
            owner.output.copy_within(owner.output_head.., 0);
            owner.output.truncate(ready);
            owner.output_head = 0;
        }
        if owner.history.is_none() {
            if let Some(pipeline) = &mut owner.pipeline {
                if self.retained && pipeline.is_preparing() {
                    // Prepare one existing SRC layer per bounded CPU step,
                    // even when the original HTTP body has not supplied PCM.
                    pipeline.step(&[], false, &mut owner.output, cancel)?;
                    return Ok(self.outcome(0, Progress::Yield));
                }
                if !pipeline.is_preparing() {
                    owner.history = Some(pipeline.quiet_source_frames()?);
                }
            } else if owner.decoder.rate != 0 && owner.decoder.rate == owner.target {
                owner.history = Some(0);
            }
        }
        if owner.head < owner.native.len() {
            if let Some(pipeline) = &mut owner.pipeline {
                let step = pipeline.step(
                    &owner.native[owner.head..],
                    false,
                    &mut owner.output,
                    cancel,
                )?;
                owner.head += step.consumed;
            } else {
                let channels = owner.decoder.channels as usize;
                let n = (owner.native.len() - owner.head)
                    .min(STEP_SAMPLES)
                    .min(owner.output.capacity() - owner.output.len())
                    / channels
                    * channels;
                owner
                    .output
                    .extend_from_slice(&owner.native[owner.head..owner.head + n]);
                owner.head += n;
            }
            return Ok(self.outcome(0, Progress::Yield));
        }
        owner.native.clear();
        owner.head = 0;
        let repeats = owner.window.as_ref().is_some_and(Window::repeats);
        if owner.source_done && repeats {
            let window = owner.window.as_mut().unwrap();
            if window.local != window.frames {
                return Err(invalid(
                    "loop source ended before its complete presentation",
                ));
            }
            owner
                .decoder
                .seek(symphonia_core::units::Timestamp::ZERO)
                .map_err(|e| invalid(e.to_string()))?;
            window.completed = window
                .completed
                .checked_add(1)
                .ok_or_else(|| invalid("loop source clock overflow"))?;
            window.local = 0;
            owner.input_ended = false;
            owner.codec_done = false;
            owner.source_done = false;
            return Ok(self.outcome(0, Progress::Yield));
        }
        if owner.codec_done && !owner.filtered && !repeats {
            owner.filtered = if let Some(pipeline) = &mut owner.pipeline {
                pipeline
                    .step(&[], true, &mut owner.output, cancel)?
                    .progress
                    == bandlimited::Progress::Finished
            } else {
                true
            };
            owner.done = owner.filtered && owner.source_done;
            return Ok(self.outcome(0, Progress::Yield));
        }
        let consumed = if owner.decoder.awaits_source() {
            0
        } else if eof {
            if !input.is_empty() {
                return Err(invalid("packet stream EOF must follow the accepted suffix"));
            }
            owner.decoder.finish_input();
            owner.input_ended = true;
            0
        } else {
            owner
                .decoder
                .push(&input[..input.len().min(INPUT_BYTES)])
                .map_err(invalid)?
        };
        let progress = owner
            .decoder
            .drain(&mut owner.native, cancel)
            .map_err(invalid)?;
        if let Some(window) = &mut owner.window
            && owner.decoder.channels != 0
        {
            window.local = window
                .local
                .checked_add((owner.native.len() / owner.decoder.channels as usize) as u64)
                .filter(|frames| *frames <= window.frames)
                .ok_or_else(|| invalid("loop source exceeds its presentation"))?;
        }
        if owner.decoder.rate != 0 {
            self.format = (owner.target, owner.decoder.channels);
            if let Some(frames) = owner
                .decoder
                .presentation_track()
                .and_then(|t| t.num_frames)
            {
                let clock = crate::streaming::StreamTimeline::new(
                    frames,
                    owner.decoder.rate,
                    owner.target,
                    owner.decoder.channels,
                )?;
                if self.clock.is_some_and(|old| old != clock) {
                    return Err(invalid("packet stream presentation changed"));
                }
                self.clock = Some(clock);
            }
            if owner.target != owner.decoder.rate && owner.pipeline.is_none() {
                let pipeline = Stream::new(
                    owner.decoder.rate,
                    owner.target,
                    owner.decoder.channels,
                    owner
                        .decoder
                        .presentation_track()
                        .and_then(|t| t.num_frames),
                )?;
                owner.grid = pipeline.grid_period();
                owner.pipeline = Some(pipeline);
            }
        }
        owner.source_done = progress == PacketProgress::Finished;
        owner.codec_done = owner.source_done || owner.decoder.awaits_source_eof();
        owner.done = owner.source_done && owner.filtered;
        let progress = match progress {
            PacketProgress::Cancelled => Progress::Cancelled,
            PacketProgress::NeedSource => Progress::NeedSource,
            PacketProgress::NeedInput if owner.input_ended => {
                return Err(invalid("packet stream requested input after EOF"));
            }
            PacketProgress::NeedInput if owner.native.is_empty() => {
                if self.retained && owner.decoder.rate != 0 && owner.history.is_none() {
                    Progress::Yield
                } else {
                    Progress::NeedInput
                }
            }
            PacketProgress::NeedInput | PacketProgress::Yield | PacketProgress::Finished => {
                Progress::Yield
            }
        };
        Ok(self.outcome(consumed, progress))
    }
    fn outcome(&self, consumed: usize, progress: Progress) -> Step {
        // A retained source's first Ready promises that seek is usable now.
        let preparing = self.retained && self.owner.as_ref().is_some_and(|o| o.history.is_none());
        Step {
            consumed,
            progress,
            sample_rate: if preparing { 0 } else { self.format.0 },
            channels: if preparing { 0 } else { self.format.1 },
        }
    }
}
impl<P: Packetizer> Decoder for PacketStream<P> {
    fn timeline(&self) -> EngineResult<Option<crate::streaming::StreamTimeline>> {
        Ok(self.clock)
    }
    fn presentation(&self) -> EngineResult<Option<crate::streaming::StreamPresentation>> {
        Ok(self
            .owner
            .as_ref()
            .and_then(|o| o.window.as_ref())
            .map(|w| w.presentation)
            .or_else(|| self.clock.map(crate::streaming::StreamPresentation::single)))
    }
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if !pcm.is_empty() {
            return Err(invalid("packet stream destination must be empty"));
        }
        let result = self.advance(input, eof, pcm, cancel);
        if let Err(error) = &result {
            self.owner = None;
            self.error = Some(error.code);
            pcm.clear();
        }
        result
    }
}

impl PacketStream<crate::decoder::aac::m4a::M4a> {
    pub(crate) fn file(target: u32, length: u64) -> Self {
        Self::with_decoder(target, Packets::with_object_len(None, Some(length)))
    }
    pub(crate) fn source_request(&self) -> Option<crate::decoder::aac::m4a::SourceRequest> {
        self.owner.as_ref()?.decoder.source_request()
    }
    pub(crate) fn install_index(
        &mut self,
        index: symphonia_format_isomp4::streaming::AudioIndex,
        length: u64,
    ) -> EngineResult<()> {
        self.owner
            .as_mut()
            .ok_or_else(|| invalid("local M4A decoder retired"))?
            .decoder
            .install_index_with_length(index, Some(length))
            .map_err(invalid)
    }
    pub(crate) fn resume_at(
        &mut self,
        span: symphonia_format_isomp4::streaming::AtomSpan,
        offset: u64,
    ) -> EngineResult<()> {
        let owner = self
            .owner
            .as_mut()
            .ok_or_else(|| invalid("local M4A decoder retired"))?;
        owner.decoder.resume_at(span, offset).map_err(invalid)?;
        owner.input_ended = false;
        Ok(())
    }
    pub(crate) fn install_http_index(
        &mut self,
        index: symphonia_format_isomp4::streaming::AudioIndex,
        length: u64,
    ) -> EngineResult<()> {
        self.owner
            .as_mut()
            .ok_or_else(|| invalid("HTTP M4A decoder retired"))?
            .decoder
            .install_index_for_range(index, length)
            .map_err(invalid)
    }
}

#[path = "packet_control.rs"]
mod control;
