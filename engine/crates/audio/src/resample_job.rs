//! Owned whole-file SRC. One mature kernel operation per step; exact duration
//! belongs to the original source clock even through multiple filter stages.
use crate::decoder::DecodedAudio;
#[cfg(test)]
use crate::streaming_sinc::kernel;
use crate::streaming_sinc::{STEP_SAMPLES, Stream};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "bandlimited.rs"]
pub(crate) mod bandlimited;

const MAX_STEPS: usize = 262_144;

enum Phase {
    Running {
        input: DecodedAudio,
        output: DecodedAudio,
        pipeline: Stream,
        position: usize,
        steps: usize,
    },
    Complete(DecodedAudio),
    Cancelled,
    Failed(ErrorCode),
}
pub(crate) struct ResampleJob {
    phase: Phase,
}

impl ResampleJob {
    pub(crate) fn new(input: DecodedAudio, target_rate: u32) -> EngineResult<Self> {
        if input.sample_rate == 0 || target_rate == 0 {
            return Err(invalid("resampling requires nonzero sample rates"));
        }
        if !crate::limits::pcm_samples_within_budget(input.samples.len()) {
            return Err(invalid("resampler input exceeds the PCM budget"));
        }
        let channels = input.channels as usize;
        if (channels == 0 && !input.samples.is_empty())
            || channels > crate::limits::MAX_AUDIO_CHANNELS as usize
            || (channels != 0 && !input.samples.len().is_multiple_of(channels))
        {
            return Err(invalid(
                "resampler input must contain supported complete sample frames",
            ));
        }
        if input.sample_rate == target_rate || input.samples.is_empty() {
            return Ok(Self {
                phase: Phase::Complete(input),
            });
        }
        let output_frames =
            (input.frame_count() as u128 * target_rate as u128).div_ceil(input.sample_rate as u128);
        let samples = output_frames * channels as u128;
        if samples > crate::limits::MAX_AUDIO_PCM_SAMPLES as u128 {
            return Err(invalid("resampled output exceeds the PCM budget"));
        }
        let pipeline = Stream::new(
            input.sample_rate,
            target_rate,
            input.channels,
            Some(input.frame_count() as u64),
        )?;
        let output = DecodedAudio {
            samples: Vec::with_capacity(samples as usize),
            sample_rate: target_rate,
            channels: input.channels,
        };
        Ok(Self {
            phase: Phase::Running {
                input,
                output,
                pipeline,
                position: 0,
                steps: 0,
            },
        })
    }
    pub(crate) fn step(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        if cancel.load(Ordering::Acquire) {
            self.phase = Phase::Cancelled;
        }
        let result = self.advance(cancel);
        if let Err(error) = &result {
            self.phase = if error.code == ErrorCode::Cancelled {
                Phase::Cancelled
            } else {
                Phase::Failed(error.code)
            };
        }
        result
    }
    fn advance(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        let Phase::Running {
            input,
            output,
            pipeline,
            position,
            steps,
        } = &mut self.phase
        else {
            return match &self.phase {
                Phase::Complete(_) => Ok(true),
                Phase::Cancelled => Err(EngineError::from_detail(
                    ErrorCode::Cancelled,
                    "audio resampling cancelled",
                )),
                Phase::Failed(code) => {
                    Err(EngineError::from_detail(*code, "audio resampling retired"))
                }
                _ => unreachable!(),
            };
        };
        *steps += 1;
        if *steps > MAX_STEPS {
            return Err(invalid("SRC step budget exceeded"));
        }
        let end = (*position + (STEP_SAMPLES / input.channels as usize) * input.channels as usize)
            .min(input.samples.len());
        let step = pipeline.step(
            &input.samples[*position..end],
            *position == input.samples.len(),
            &mut output.samples,
            cancel,
        )?;
        *position += step.consumed;
        if matches!(step.progress, bandlimited::Progress::Finished) {
            let expected = (input.frame_count() as u128 * output.sample_rate as u128)
                .div_ceil(input.sample_rate as u128)
                * input.channels as u128;
            if output.samples.len() as u128 != expected || *position != input.samples.len() {
                return Err(invalid("SRC did not finish the complete source duration"));
            }
            let complete = DecodedAudio {
                samples: std::mem::take(&mut output.samples),
                sample_rate: output.sample_rate,
                channels: output.channels,
            };
            self.phase = Phase::Complete(complete);
            return Ok(true);
        }
        Ok(false)
    }
    pub(crate) fn finish(self) -> EngineResult<DecodedAudio> {
        match self.phase {
            Phase::Complete(audio) => Ok(audio),
            Phase::Cancelled => Err(EngineError::from_detail(
                ErrorCode::Cancelled,
                "audio resampling cancelled",
            )),
            Phase::Failed(code) => Err(EngineError::from_detail(code, "audio resampling retired")),
            _ => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "audio resampling has not finished",
            )),
        }
    }
}
fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, detail.into())
}
#[cfg(test)]
#[path = "resample_job_tests.rs"]
pub(crate) mod tests;
