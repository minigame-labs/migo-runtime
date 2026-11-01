//! Opt-in presentation metadata for the pinned candidate codec/DSP path.
//! The library owns trims and timebases; this adapter never parses MP4 boxes.
use crate::{
    push_decoder::{M4a, M4aSeekDecoder, M4aSourceDecoder, M4aSourceRequest},
    resampled_streaming::{Batched, Repetitions},
    streaming::StreamTimeline,
    streaming_transport::{Decoder, Step},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{num::NonZeroU32, sync::atomic::AtomicBool};
use symphonia::core::{
    formats::{SeekedTo, Track},
    units::{TimeBase, Timestamp},
};
use symphonia_format_isomp4::streaming::{AtomSpan, AudioIndex};

fn invalid(message: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, message)
}
pub(crate) fn from_track(track: &Track, output_rate: u32) -> EngineResult<StreamTimeline> {
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| invalid("missing presentation audio parameters"))?;
    let rate = params
        .sample_rate
        .and_then(NonZeroU32::new)
        .ok_or_else(|| invalid("missing presentation sample rate"))?;
    let frames = track
        .num_frames
        .ok_or_else(|| invalid("missing playable presentation duration"))?;
    if track.time_base != Some(TimeBase::from_recip(rate))
        || track.duration.map(|d| d.get()) != Some(frames)
    {
        return Err(invalid(
            "presentation duration is not in playable sample frames",
        ));
    }
    let channels = params
        .channels
        .as_ref()
        .ok_or_else(|| invalid("missing presentation channels"))?
        .count() as u32;
    StreamTimeline::new(frames, rate.get(), output_rate, channels)
}
pub(crate) struct Timed<const B: bool> {
    inner: Batched<1, M4a<B>>,
    output_rate: u32,
    terminal_seek: bool,
    terminal_remaining: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct SeekPlan {
    input_origin: u64,
    output_start: u64,
}
fn seek_plan(timeline: StreamTimeline, target: u64) -> EngineResult<SeekPlan> {
    let position = timeline.position(target)?;
    let input = u64::from(position.divisor);
    let output = u64::from(timeline.format().0);
    let (mut a, mut b) = (input, output);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let period = input / a;
    // A complete filter of real history removes the reset boundary. Aligning
    // to input/gcd(input,output) makes the library's unchanged interpolation
    // grid coincide with the track grid, with at most one second extra input.
    let history = rubato::SincInterpolationParameters::default()
        .sinc_len
        .next_multiple_of(8) as u64
        + 2;
    let input_origin = if input == output {
        target
    } else {
        target.saturating_sub(history) / period * period
    };
    Ok(SeekPlan {
        input_origin,
        output_start: position.frame + u64::from(position.phase != 0),
    })
}
impl<const B: bool> Timed<B> {
    fn seek_single(&mut self, target: Timestamp) -> symphonia::core::errors::Result<SeekedTo> {
        use symphonia::core::errors::{Error, SeekErrorKind};
        let track = self
            .inner
            .presentation_track()
            .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
        let frame =
            u64::try_from(target.get()).map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?;
        if track.num_frames.is_none_or(|frames| frame > frames) {
            return Err(Error::SeekError(SeekErrorKind::OutOfRange));
        }
        let timeline = from_track(track, self.output_rate)
            .map_err(|_| Error::DecodeError("invalid presentation seek clock"))?;
        let terminal_seek = track.num_frames == Some(frame);
        let mut plan =
            seek_plan(timeline, frame).map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?;
        let bypass_terminal = terminal_seek
            && timeline
                .position(0)
                .map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?
                .divisor
                == self.output_rate;
        if bypass_terminal {
            // The codec API positions on a real playable frame. Drain its last
            // frame for a same-rate endpoint; the wrapper publishes no PCM.
            plan.input_origin = frame - 1;
        }
        let mut seeked = self.inner.seek_single_from(
            Timestamp::new(plan.input_origin as i64),
            Some((plan.input_origin, plan.output_start)),
        )?;
        self.terminal_seek = bypass_terminal;
        self.terminal_remaining = if bypass_terminal {
            timeline.format().1 as usize
        } else {
            0
        };
        seeked.required_ts = target;
        Ok(seeked)
    }
    /// CPU-owned output-position cutover. Source recovery uses the existing
    /// indexed transaction; publishing/accepting the new revision is separate.
    pub(crate) fn cutover(
        &mut self,
        output_start: u64,
        enabled: bool,
    ) -> symphonia::core::errors::Result<SeekedTo> {
        use symphonia::core::errors::{Error, SeekErrorKind};
        let track = self
            .inner
            .presentation_track()
            .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
        let clock = from_track(track, self.output_rate)
            .map_err(|_| Error::DecodeError("invalid cutover source clock"))?;
        let history = rubato::SincInterpolationParameters::default()
            .sinc_len
            .next_multiple_of(8) as u64
            + 2;
        let plan = crate::streaming::StreamCutover::new(clock, output_start, enabled, history)
            .map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?;
        let seeked = self.inner.cutover_from(plan)?;
        self.terminal_seek = false;
        self.terminal_remaining = 0;
        Ok(seeked)
    }
    pub(crate) fn looping(
        output_rate: u32,
        len: Option<u64>,
        repetitions: Repetitions,
    ) -> EngineResult<Self> {
        Ok(Self {
            inner: Batched::with_repetitions(output_rate, len, repetitions)?,
            output_rate,
            terminal_seek: false,
            terminal_remaining: 0,
        })
    }
    pub(crate) fn new(output_rate: u32, len: Option<u64>) -> Self {
        Self {
            inner: Batched::with_object_len(output_rate, len),
            output_rate,
            terminal_seek: false,
            terminal_remaining: 0,
        }
    }
}
impl<const B: bool> Decoder for Timed<B> {
    fn presentation(&self) -> EngineResult<Option<crate::streaming::StreamPresentation>> {
        self.inner
            .presentation_track()
            .map(|track| {
                let timeline = from_track(track, self.output_rate)?;
                use crate::streaming::StreamPresentation;
                match self.inner.repetitions() {
                    None => Ok(StreamPresentation::single(timeline)),
                    Some(Repetitions::Finite(cycles)) => {
                        StreamPresentation::repeated(timeline, Some(cycles))
                    }
                    Some(Repetitions::Continuous) => StreamPresentation::repeated(timeline, None),
                }
            })
            .transpose()
    }
    fn timeline(&self) -> EngineResult<Option<StreamTimeline>> {
        self.inner
            .presentation_track()
            .map(|t| {
                let timeline = from_track(t, self.output_rate)?;
                match self.inner.repetitions() {
                    Some(Repetitions::Continuous) => Ok(None),
                    Some(Repetitions::Finite(cycles)) => {
                        let frames = t
                            .num_frames
                            .expect("validated clock")
                            .checked_mul(cycles)
                            .ok_or_else(|| invalid("repeated presentation duration overflow"))?;
                        let (output, channels) = timeline.format();
                        StreamTimeline::new(frames, timeline.position(0)?.divisor, output, channels)
                            .map(Some)
                    }
                    None => Ok(Some(timeline)),
                }
            })
            .transpose()
            .map(Option::flatten)
    }
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        let result = (|| {
            let step = self.inner.step(input, eof, pcm, cancel)?;
            if self.terminal_seek {
                // Still drain the real source to EOF. A terminal presentation
                // has no output, including the unfiltered same-rate bypass.
                if pcm.len() > self.terminal_remaining
                    || (step.progress == crate::streaming_transport::Progress::Finished
                        && pcm.len() != self.terminal_remaining)
                {
                    return Err(invalid(
                        "terminal bypass does not match the source endpoint",
                    ));
                }
                self.terminal_remaining -= pcm.len();
                pcm.clear();
            }
            if self.inner.repetitions().is_some()
                && let Some(track) = self.inner.presentation_track()
            {
                from_track(track, self.output_rate)?;
                let frames = track.num_frames.expect("validated clock");
                self.inner.bind_loop_frames(frames)?;
                // Validate total count before this step reaches publication.
                self.timeline()?;
            }
            Ok(step)
        })();
        if result.is_err() {
            self.inner.retire_loop_error();
            pcm.clear();
        }
        result
    }
}
impl<const B: bool> M4aSourceDecoder for Timed<B> {
    fn source_request(&self) -> Option<M4aSourceRequest> {
        self.inner.source_request()
    }
    fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        len: Option<u64>,
    ) -> Result<(), String> {
        self.inner.install_index_with_length(index, len)
    }
    fn resume_at(&mut self, span: AtomSpan, offset: u64) -> Result<(), String> {
        self.inner.resume_at(span, offset)
    }
}
impl<const B: bool> M4aSeekDecoder for Timed<B> {
    fn cutover(
        &mut self,
        output_start: u64,
        enabled: bool,
    ) -> symphonia::core::errors::Result<SeekedTo> {
        Timed::cutover(self, output_start, enabled)
    }
    fn seek_loop(
        &mut self,
        target: Timestamp,
        enabled: bool,
    ) -> symphonia::core::errors::Result<SeekedTo> {
        use symphonia::core::errors::{Error, SeekErrorKind};
        let frame =
            u64::try_from(target.get()).map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?;
        let track = self
            .inner
            .presentation_track()
            .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
        let clock = from_track(track, self.output_rate)
            .map_err(|_| Error::DecodeError("invalid cyclic seek clock"))?;
        let traversal = crate::streaming::StreamTraversal::seek(clock, frame, enabled)
            .map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?;
        if !enabled {
            return self.seek_single(target);
        }
        let mut seeked = self.cutover(traversal.output_start(), true)?;
        seeked.required_ts = target;
        Ok(seeked)
    }
    fn seek(&mut self, target: Timestamp) -> symphonia::core::errors::Result<SeekedTo> {
        use symphonia::core::errors::{Error, SeekErrorKind};
        if self.inner.repetitions().is_some() {
            return Err(Error::SeekError(SeekErrorKind::Unseekable));
        }
        self.seek_single(target)
    }
}
#[cfg(test)]
#[path = "loop_contracts.rs"]
mod loop_tests;

#[cfg(test)]
#[path = "loop_cutover_contracts.rs"]
mod cutover_tests;

#[cfg(test)]
#[path = "live_loop_contracts.rs"]
mod live_loop_tests;
#[cfg(test)]
#[path = "phase_contracts.rs"]
mod phase_tests;
#[cfg(test)]
#[path = "timeline_contracts.rs"]
mod tests;
