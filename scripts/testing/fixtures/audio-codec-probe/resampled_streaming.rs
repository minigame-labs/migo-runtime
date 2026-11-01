//! Candidate codec -> bounded Rubato steps -> production PCM publication.
use crate::{
    bandlimited, push_decoder,
    streaming_transport::{Decoder, Progress, Step},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};

pub fn core(input: u32, output: u32, channels: u32) -> EngineResult<rubato::Async<f32>> {
    bandlimited::validate_format(input, output, channels).map_err(error)?;
    rubato::Async::new_sinc(
        output as f64 / input as f64,
        1.0,
        &rubato::SincInterpolationParameters::default(),
        128,
        channels as usize,
        rubato::FixedAsync::Input,
    )
    .map_err(|cause| error(cause.to_string()))
}

fn error(message: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, message.into())
}

// Selected from the measured 1/4/8/16 quantum candidates. Retain the explicit
// const-generic arms for comparisons; all share the same input/output bounds.
pub type Resampled = Batched<16>;
#[cfg(feature = "wave-streaming-probe")]
pub type WaveResampled = Batched<16, push_decoder::Wave>;
#[cfg(feature = "adts-streaming-probe")]
pub type AdtsResampled = Batched<16, push_decoder::Adts>;
#[cfg(feature = "vorbis-streaming-probe")]
pub type VorbisResampled = Batched<16, push_decoder::Vorbis>;
#[cfg(feature = "m4a-streaming-probe")]
pub type M4aResampled = Batched<16, push_decoder::M4a<true>>;

#[cfg(feature = "m4a-streaming-probe")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Repetitions {
    Finite(u64),
    Continuous,
}

#[cfg(feature = "m4a-streaming-probe")]
struct LoopState {
    repetitions: Repetitions,
    expected: Option<u64>,
    frames: u64,
    completed: u64,
    sealed: bool,
}

#[cfg(feature = "m4a-streaming-probe")]
impl LoopState {
    fn observe(&mut self, frames: u64) -> EngineResult<()> {
        if self.sealed {
            return Err(error("PCM after loop boundary"));
        }
        let next = self
            .frames
            .checked_add(frames)
            .ok_or_else(|| error("loop frame overflow"))?;
        if self.expected.is_some_and(|n| next > n) {
            return Err(error("loop PCM exceeds its source clock"));
        }
        self.frames = next;
        Ok(())
    }
    fn boundary(&mut self) -> EngineResult<bool> {
        if !self.sealed {
            if self.expected != Some(self.frames) {
                return Err(error("incomplete loop presentation"));
            }
            self.completed = self
                .completed
                .checked_add(1)
                .ok_or_else(|| error("loop count overflow"))?;
            self.sealed = true;
        }
        Ok(match self.repetitions {
            Repetitions::Finite(n) => self.completed < n,
            Repetitions::Continuous => true,
        })
    }
    fn restart(&mut self) {
        self.frames = 0;
        self.sealed = false;
    }
}

pub struct Batched<
    const QUANTA: usize,
    P: push_decoder::incremental::Packetizer = push_decoder::Mp3,
> {
    decoder: push_decoder::incremental::Decoder<P>,
    resampler: Option<bandlimited::Stream<rubato::Async<f32>>>,
    decoded: Vec<f32>,
    position: usize,
    target_rate: u32,
    output_rebase: Option<(u64, u64)>,
    input_ended: bool,
    #[cfg(feature = "m4a-streaming-probe")]
    source_waiting: bool,
    #[cfg(feature = "m4a-streaming-probe")]
    looping: Option<LoopState>,
    decoder_done: bool,
    stopped: bool,
    failed: bool,
}

impl<const QUANTA: usize, P: push_decoder::incremental::Packetizer> Batched<QUANTA, P> {
    pub(crate) fn presentation_track(&self) -> Option<&symphonia::core::formats::Track> {
        self.decoder.presentation_track()
    }
    pub fn new(target_rate: u32) -> Self {
        Self::with_decoder(
            target_rate,
            push_decoder::incremental::Decoder::<P>::new(None),
        )
    }

    fn with_decoder(target_rate: u32, decoder: push_decoder::incremental::Decoder<P>) -> Self {
        assert!((1..=32).contains(&QUANTA));
        Self {
            decoder,
            resampler: None,
            decoded: Vec::with_capacity(P::OUTPUT_SAMPLES_PER_STEP),
            position: 0,
            target_rate,
            output_rebase: None,
            input_ended: false,
            #[cfg(feature = "m4a-streaming-probe")]
            source_waiting: false,
            #[cfg(feature = "m4a-streaming-probe")]
            looping: None,
            decoder_done: false,
            stopped: false,
            failed: false,
        }
    }

    fn outcome(&self, consumed: usize, progress: Progress) -> Step {
        Step {
            consumed,
            progress,
            sample_rate: if self.decoder.rate == 0 {
                0
            } else {
                self.target_rate
            },
            channels: self.decoder.channels,
        }
    }

    fn step_inner(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if self.stopped || cancel.load(Ordering::Acquire) {
            self.stopped = true;
            self.decoded.clear();
            return Ok(self.outcome(0, Progress::Cancelled));
        }
        if !pcm.is_empty() {
            return Err(error("PCM destination must be empty"));
        }
        if self.position < self.decoded.len() {
            let sampler = self.resampler.as_mut().expect("pending PCM has resampler");
            for _ in 0..QUANTA {
                if cancel.load(Ordering::Acquire) {
                    self.stopped = true;
                    pcm.clear();
                    self.decoded.clear();
                    return Ok(self.outcome(0, Progress::Cancelled));
                }
                if pcm.capacity() - pcm.len() < sampler.output_capacity() {
                    break;
                }
                let step = sampler
                    .append_step(&self.decoded[self.position..], false, pcm)
                    .map_err(error)?;
                self.position += step.consumed;
                if self.position == self.decoded.len() {
                    break;
                }
            }
            // Drain before accepting another byte, including when the last
            // resampling quantum filled no PCM because of the startup delay.
            return Ok(self.outcome(0, Progress::Yield));
        }
        self.decoded.clear();
        self.position = 0;
        if self.decoder_done {
            #[cfg(feature = "m4a-streaming-probe")]
            if self
                .looping
                .as_mut()
                .map(LoopState::boundary)
                .transpose()?
                .unwrap_or(false)
            {
                // Codec seeks retain the stream's partial input, filter
                // history, startup-delay compensation and rational counters.
                // Only the final cycle may pad/flush the sinc tail.
                self.decoder
                    .seek(symphonia::core::units::Timestamp::ZERO)
                    .map_err(|cause| error(cause.to_string()))?;
                self.looping.as_mut().expect("loop boundary").restart();
                self.source_waiting = true;
                self.decoder_done = false;
                return Ok(self.outcome(0, Progress::Yield));
            }
            if self.resampler.is_none() {
                return Ok(self.outcome(0, Progress::Finished));
            }
            let sampler = self
                .resampler
                .as_mut()
                .expect("bypass finishes with the decoder");
            let mut progress = Progress::Yield;
            for _ in 0..QUANTA {
                if cancel.load(Ordering::Acquire) {
                    self.stopped = true;
                    pcm.clear();
                    return Ok(self.outcome(0, Progress::Cancelled));
                }
                if pcm.capacity() - pcm.len() < sampler.output_capacity() {
                    break;
                }
                let step = sampler.append_step(&[], true, pcm).map_err(error)?;
                if step.progress == bandlimited::Progress::Finished {
                    progress = Progress::Finished;
                    break;
                }
            }
            return Ok(self.outcome(0, progress));
        }
        #[cfg(feature = "m4a-streaming-probe")]
        let waiting = self.source_waiting;
        #[cfg(not(feature = "m4a-streaming-probe"))]
        let waiting = false;
        let consumed = if waiting {
            // Explicit seek may resolve from retained bytes inside drain.
            // Never push old-position input while the common decoder waits.
            0
        } else if eof {
            if !self.input_ended {
                self.decoder.finish_input();
                self.input_ended = true;
            }
            0
        } else {
            self.decoder.push(input).map_err(error)?
        };
        let progress = self
            .decoder
            .drain(&mut self.decoded, cancel)
            .map_err(error)?;
        #[cfg(feature = "m4a-streaming-probe")]
        if let Some(state) = &mut self.looping {
            state.observe(
                self.decoded
                    .len()
                    .checked_div(self.decoder.channels as usize)
                    .unwrap_or(0) as u64,
            )?;
        }
        if self.decoder.rate != 0
            && self.target_rate != self.decoder.rate
            && self.resampler.is_none()
        {
            self.resampler = Some(
                bandlimited::Stream::new(
                    core(self.decoder.rate, self.target_rate, self.decoder.channels)?,
                    self.decoder.rate,
                    self.target_rate,
                    self.decoder.channels,
                )
                .map_err(error)?,
            );
            if let Some((origin, start)) = self.output_rebase.take() {
                self.resampler
                    .as_mut()
                    .expect("created resampler")
                    .reset_from(origin, start)
                    .map_err(error)?;
            }
        }
        if progress == push_decoder::Progress::Cancelled {
            self.stopped = true;
            return Ok(self.outcome(consumed, Progress::Cancelled));
        }
        #[cfg(feature = "m4a-streaming-probe")]
        {
            self.source_waiting = progress == push_decoder::Progress::NeedSource;
        }
        if self.target_rate == self.decoder.rate {
            // Same-rate playback goes directly to the publication buffer; no
            // filter, PCM copy, resampler construction or extra drain cycle.
            std::mem::swap(pcm, &mut self.decoded);
            #[cfg(feature = "m4a-streaming-probe")]
            if progress == push_decoder::Progress::Finished && self.looping.is_some() {
                self.decoder_done = true;
                return Ok(self.outcome(consumed, Progress::Yield));
            }
            return Ok(self.outcome(
                consumed,
                match progress {
                    push_decoder::Progress::Finished => Progress::Finished,
                    push_decoder::Progress::NeedInput => Progress::NeedInput,
                    #[cfg(feature = "m4a-streaming-probe")]
                    push_decoder::Progress::NeedSource => Progress::NeedSource,
                    push_decoder::Progress::Yield => Progress::Yield,
                    push_decoder::Progress::Cancelled => unreachable!(),
                },
            ));
        }
        self.decoder_done = progress == push_decoder::Progress::Finished;
        #[cfg(feature = "m4a-streaming-probe")]
        if self.source_waiting && self.decoded.is_empty() {
            return Ok(self.outcome(consumed, Progress::NeedSource));
        }
        let progress = if self.decoder_done
            || !self.decoded.is_empty()
            || progress == push_decoder::Progress::Yield
        {
            Progress::Yield
        } else {
            Progress::NeedInput
        };
        Ok(self.outcome(consumed, progress))
    }
}

impl<const QUANTA: usize, P: push_decoder::incremental::Packetizer> Decoder for Batched<QUANTA, P> {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        if self.failed {
            return Err(error("composed decoder previously failed"));
        }
        let result = self.step_inner(input, eof, pcm, cancel);
        if result.is_err() {
            self.failed = true;
            self.decoded.clear();
            pcm.clear();
        }
        result
    }
}

#[cfg(feature = "m4a-streaming-probe")]
impl<const Q: usize, const BUFFERED: bool> Batched<Q, push_decoder::M4a<BUFFERED>> {
    pub(crate) fn with_repetitions(
        target_rate: u32,
        object_len: Option<u64>,
        repetitions: Repetitions,
    ) -> EngineResult<Self> {
        if matches!(repetitions, Repetitions::Finite(0)) {
            return Err(error("invalid initial loop configuration"));
        }
        let mut value = Self::with_object_len(target_rate, object_len);
        value.looping = Some(LoopState {
            repetitions,
            expected: None,
            frames: 0,
            completed: 0,
            sealed: false,
        });
        Ok(value)
    }
    pub(crate) fn repetitions(&self) -> Option<Repetitions> {
        self.looping.as_ref().map(|s| s.repetitions)
    }
    pub(crate) fn cutover_from(
        &mut self,
        plan: crate::streaming::StreamCutover,
    ) -> symphonia::core::errors::Result<symphonia::core::formats::SeekedTo> {
        use symphonia::core::{
            errors::{Error, SeekErrorKind},
            units::Timestamp,
        };
        let (local, completed) = plan.source_origin();
        let local_ts =
            i64::try_from(local).map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?;
        // The codec validates its retained index before any pending PCM, loop
        // counters or DSP are replaced. Only accepted seeks change traversal.
        let seeked = self.seek_from(
            Timestamp::new(local_ts),
            Some((plan.input_origin(), plan.output_start())),
        )?;
        self.looping = Some(LoopState {
            repetitions: plan
                .cycles()
                .map_or(Repetitions::Continuous, Repetitions::Finite),
            expected: Some(plan.input_frames()),
            frames: local,
            completed,
            sealed: false,
        });
        Ok(seeked)
    }
    pub(crate) fn bind_loop_frames(&mut self, frames: u64) -> EngineResult<()> {
        let state = self
            .looping
            .as_mut()
            .ok_or_else(|| error("missing loop state"))?;
        if frames == 0 || state.expected.is_some_and(|old| old != frames) || state.frames > frames {
            return Err(error("changed loop presentation clock"));
        }
        state.expected = Some(frames);
        Ok(())
    }
    pub(crate) fn retire_loop_error(&mut self) {
        self.failed = true;
        self.decoded.clear();
    }
    #[cfg(test)]
    pub(crate) fn decoded_cycles(&self) -> u64 {
        self.looping.as_ref().map_or(0, |s| s.completed)
    }

    pub fn seek(
        &mut self,
        ts: symphonia::core::units::Timestamp,
    ) -> symphonia::core::errors::Result<symphonia::core::formats::SeekedTo> {
        self.seek_from(ts, None)
    }

    pub(crate) fn seek_from(
        &mut self,
        ts: symphonia::core::units::Timestamp,
        clock: Option<(u64, u64)>,
    ) -> symphonia::core::errors::Result<symphonia::core::formats::SeekedTo> {
        use symphonia::core::errors::Error;
        if self.failed || self.stopped {
            return Err(Error::DecodeError("composed decoder is retired"));
        }
        let seeked = match self.decoder.seek(ts) {
            Ok(seeked) => seeked,
            Err(error) => {
                if !matches!(error, Error::SeekError(_)) {
                    self.failed = true;
                }
                return Err(error);
            }
        };
        self.decoded.clear();
        self.position = 0;
        if let Some(resampler) = &mut self.resampler {
            if let Some((origin, start)) = clock {
                if resampler.reset_from(origin, start).is_err() {
                    self.failed = true;
                    return Err(Error::DecodeError("invalid resampler seek clock"));
                }
            } else {
                resampler.reset();
            }
            self.output_rebase = None;
        } else {
            self.output_rebase = clock;
        }
        self.decoder_done = false;
        self.source_waiting = true;
        Ok(seeked)
    }

    pub(crate) fn seek_single_from(
        &mut self,
        ts: symphonia::core::units::Timestamp,
        clock: Option<(u64, u64)>,
    ) -> symphonia::core::errors::Result<symphonia::core::formats::SeekedTo> {
        let seeked = self.seek_from(ts, clock)?;
        self.looping = None;
        Ok(seeked)
    }

    pub fn with_object_len(target_rate: u32, object_len: Option<u64>) -> Self {
        Self::with_decoder(
            target_rate,
            push_decoder::M4aDecoderMode::<BUFFERED>::with_object_len(None, object_len),
        )
    }

    fn source_resume(
        &mut self,
        reposition: bool,
        operation: impl FnOnce(&mut push_decoder::M4aDecoderMode<BUFFERED>) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.failed || self.stopped || !self.source_waiting || self.position < self.decoded.len()
        {
            self.failed = true;
            return Err("composed decoder has no drained source request".into());
        }
        if let Err(error) = operation(&mut self.decoder) {
            self.failed = true;
            return Err(error);
        }
        self.source_waiting = false;
        if reposition {
            self.input_ended = false;
        }
        Ok(())
    }
}

#[cfg(feature = "m4a-streaming-probe")]
impl<const Q: usize, const BUFFERED: bool> push_decoder::M4aSeekDecoder
    for Batched<Q, push_decoder::M4a<BUFFERED>>
{
    fn seek(
        &mut self,
        ts: symphonia::core::units::Timestamp,
    ) -> symphonia::core::errors::Result<symphonia::core::formats::SeekedTo> {
        Batched::<Q, push_decoder::M4a<BUFFERED>>::seek(self, ts)
    }
}

#[cfg(feature = "m4a-streaming-probe")]
impl<const Q: usize, const BUFFERED: bool> push_decoder::M4aSourceDecoder
    for Batched<Q, push_decoder::M4a<BUFFERED>>
{
    fn source_request(&self) -> Option<push_decoder::M4aSourceRequest> {
        if self.failed || self.stopped || !self.source_waiting || self.position < self.decoded.len()
        {
            return None;
        }
        self.decoder.source_request()
    }
    fn install_index_with_length(
        &mut self,
        index: symphonia_format_isomp4::streaming::AudioIndex,
        length: Option<u64>,
    ) -> Result<(), String> {
        self.source_resume(false, |decoder| {
            decoder.install_index_with_length(index, length)
        })
    }
    fn resume_at(
        &mut self,
        media: symphonia_format_isomp4::streaming::AtomSpan,
        offset: u64,
    ) -> Result<(), String> {
        self.source_resume(true, |decoder| decoder.resume_at(media, offset))
    }
}

#[cfg(all(test, feature = "m4a-streaming-probe"))]
mod loop_clock_tests {
    use super::*;
    fn clock() -> LoopState {
        LoopState {
            repetitions: Repetitions::Finite(2),
            expected: Some(3),
            frames: 0,
            completed: 0,
            sealed: false,
        }
    }
    #[test]
    fn only_exact_cycles_advance_and_a_final_boundary_is_idempotent() {
        let mut s = clock();
        assert!(s.boundary().is_err());
        s.observe(2).unwrap();
        assert!(s.boundary().is_err());
        assert!(s.observe(2).is_err());
        assert_eq!(s.frames, 2);
        let mut s = clock();
        s.observe(3).unwrap();
        assert!(s.boundary().unwrap());
        assert_eq!(s.completed, 1);
        assert!(s.observe(0).is_err());
        s.restart();
        assert_eq!(s.frames, 0);
        assert!(!s.sealed);
        s.observe(3).unwrap();
        assert!(!s.boundary().unwrap());
        assert!(!s.boundary().unwrap());
        assert_eq!(s.completed, 2);
    }
    #[test]
    fn loop_counters_reject_overflow_without_wrapping() {
        let mut s = LoopState {
            repetitions: Repetitions::Continuous,
            expected: Some(u64::MAX),
            frames: u64::MAX,
            completed: 0,
            sealed: false,
        };
        assert!(s.observe(1).is_err());
        assert_eq!(s.frames, u64::MAX);
        s.completed = u64::MAX;
        assert!(s.boundary().is_err());
        assert_eq!(s.completed, u64::MAX);
        assert!(!s.sealed);
    }
}
