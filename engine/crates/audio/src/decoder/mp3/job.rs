//! One owner for MP3 codec, paged PCM, finalization and optional device/context DSP.
use std::sync::atomic::{AtomicBool, Ordering};

use shared::error::{EngineError, EngineResult, ErrorCode};

use super::{FileStep, Mp3FileDecoder};
use crate::{
    decoder::{DecodedAudio, pcm::PcmCollector},
    resample_job::ResampleJob,
};

#[cfg(test)]
thread_local! {
    static TRACE: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

struct Decode<B> {
    decoder: Mp3FileDecoder<B>,
    collector: PcmCollector,
    metadata: Option<(u32, u32)>,
    hint_checked: bool,
}

enum Phase<B> {
    Decode(Box<Decode<B>>),
    Flatten(PcmCollector, u32, u32),
    Decoded(DecodedAudio),
    Resample(ResampleJob),
    Complete(DecodedAudio),
    Failed(ErrorCode),
}

pub(crate) struct Mp3DecodeJob<B> {
    phase: Phase<B>,
    target_rate: Option<u32>,
}

impl<B: AsRef<[u8]>> Mp3DecodeJob<B> {
    pub(crate) fn new(input: B, target_rate: Option<u32>) -> Self {
        Self {
            phase: Phase::Decode(Box::new(Decode {
                decoder: Mp3FileDecoder::new(input),
                collector: PcmCollector::new(),
                metadata: None,
                hint_checked: false,
            })),
            target_rate,
        }
    }

    pub(crate) fn step(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        if cancel.load(Ordering::Acquire) {
            self.phase = Phase::Failed(ErrorCode::Cancelled);
        }
        match self.advance(cancel) {
            Ok(done) => Ok(done),
            Err(error) => {
                self.phase = Phase::Failed(error.code);
                Err(error)
            }
        }
    }

    fn advance(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        let phase = std::mem::replace(&mut self.phase, Phase::Failed(ErrorCode::Internal));
        match phase {
            Phase::Decode(mut owner) => {
                if !owner.hint_checked
                    && let Some(expected) = owner.decoder.presentation_samples()
                {
                    owner.hint_checked = true;
                    if expected.is_some() {
                        owner.collector = PcmCollector::with_expected_samples(expected);
                    }
                }
                if owner.collector.needs_reservation() {
                    owner.collector.step()?;
                    self.phase = Phase::Decode(owner);
                    return Ok(false);
                }
                match owner.decoder.step(cancel)? {
                    FileStep::Frame {
                        pcm,
                        sample_rate,
                        channels,
                    } => {
                        owner.collector.append_i16(pcm)?;
                        owner.metadata.get_or_insert((sample_rate, channels));
                    }
                    FileStep::Yield => {}
                    FileStep::Finished => {
                        let Some((rate, channels)) = owner.metadata else {
                            return Err(EngineError::from_detail(
                                ErrorCode::InvalidArgument,
                                "MP3 decode produced no samples",
                            ));
                        };
                        owner.collector.seal()?;
                        self.phase = Phase::Flatten(owner.collector, rate, channels);
                        return Ok(false);
                    }
                }
                self.phase = Phase::Decode(owner);
                Ok(false)
            }
            Phase::Flatten(mut collector, sample_rate, channels) => {
                #[cfg(test)]
                TRACE.with(|trace| {
                    let (flat, dsp) = trace.get();
                    trace.set((flat + 1, dsp));
                });
                self.phase = if collector.step()? {
                    Phase::Decoded(DecodedAudio {
                        samples: collector.finish()?,
                        sample_rate,
                        channels,
                    })
                } else {
                    Phase::Flatten(collector, sample_rate, channels)
                };
                Ok(false)
            }
            Phase::Decoded(audio) => {
                if let Some(rate) = self.target_rate {
                    self.phase = Phase::Resample(ResampleJob::new(audio, rate)?);
                    Ok(false)
                } else {
                    self.phase = Phase::Complete(audio);
                    Ok(true)
                }
            }
            Phase::Resample(mut resampler) => {
                #[cfg(test)]
                TRACE.with(|trace| {
                    let (flat, dsp) = trace.get();
                    trace.set((flat, dsp + 1));
                });
                if resampler.step(cancel)? {
                    self.phase = Phase::Complete(resampler.finish()?);
                    Ok(true)
                } else {
                    self.phase = Phase::Resample(resampler);
                    Ok(false)
                }
            }
            Phase::Complete(audio) => {
                self.phase = Phase::Complete(audio);
                Ok(true)
            }
            Phase::Failed(code) => {
                self.phase = Phase::Failed(code);
                Err(EngineError::from_detail(code, "MP3 decode job retired"))
            }
        }
    }

    pub(crate) fn finish(self) -> EngineResult<DecodedAudio> {
        match self.phase {
            Phase::Complete(audio) => Ok(audio),
            Phase::Failed(code) => Err(EngineError::from_detail(code, "MP3 decode job retired")),
            _ => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "MP3 decode job has not finished",
            )),
        }
    }
}

#[cfg(test)]
#[path = "job_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "product_tests.rs"]
mod product_tests;
