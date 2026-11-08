//! Whole-file decode: the streaming readers (`PcmSource`) over bytes in
//! memory, collected in bounded steps, then converted to the target rate. One
//! decode path serves streams, `decodeAudioData` and loaded clips alike.
use super::{DecodedAudio, pcm::PcmCollector};
use crate::pcm_source::{PcmSource, SNIFF_BYTES};
use crate::resample_job::ResampleJob;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use symphonia_core::io::MediaSource;

/// Encoded input a whole-file decode accepts.
const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// PCM handed to the collector at once (its per-append bound).
const APPEND_SAMPLES: usize = 8192;

/// The bytes as a seekable source for the readers.
struct Bytes<B>(Cursor<B>);
impl<B: AsRef<[u8]>> Read for Bytes<B> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
impl<B: AsRef<[u8]>> Seek for Bytes<B> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(to)
    }
}
impl<B: AsRef<[u8]> + Send + Sync> MediaSource for Bytes<B> {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.0.get_ref().as_ref().len() as u64)
    }
}

struct Decode {
    source: Box<dyn PcmSource>,
    collector: PcmCollector,
    pcm: Vec<f32>,
    any: bool,
}

enum Phase {
    Decode(Box<Decode>),
    Flatten(PcmCollector, u32, u32),
    Resample(ResampleJob),
    Complete(DecodedAudio),
    Failed(ErrorCode),
}

pub(crate) struct WholeDecodeJob {
    phase: Phase,
    target: Option<u32>,
}

impl WholeDecodeJob {
    /// Recognise and open the input (one step's work: readers parse their
    /// headers here).
    pub(crate) fn new<B>(input: B, target: Option<u32>) -> EngineResult<Self>
    where
        B: AsRef<[u8]> + Send + Sync + 'static,
    {
        let data = input.as_ref();
        if target == Some(0) || data.len() > MAX_INPUT_BYTES {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "audio decode input or target outside limits",
            ));
        }
        if data.len() < 4 {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "Audio data too short to detect format",
            ));
        }
        let prefix = data[..data.len().min(SNIFF_BYTES)].to_vec();
        let source = crate::pcm_source::open(Box::new(Bytes(Cursor::new(input))), &prefix)?;
        // A declared length is reserved once (in its own step) instead of
        // growing pages.
        let expected = source
            .frames()
            .and_then(|frames| frames.checked_mul(u64::from(source.channels())))
            .map(|samples| usize::try_from(samples).unwrap_or(usize::MAX));
        Ok(Self {
            phase: Phase::Decode(Box::new(Decode {
                source,
                collector: PcmCollector::with_expected_samples(expected),
                pcm: Vec::new(),
                any: false,
            })),
            target,
        })
    }

    /// One bounded unit of work; `true` once the PCM is complete.
    pub(crate) fn step(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        if cancel.load(Ordering::Acquire) {
            self.phase = Phase::Failed(ErrorCode::Cancelled);
        }
        let phase = std::mem::replace(&mut self.phase, Phase::Failed(ErrorCode::Internal));
        match self.advance(phase, cancel) {
            Ok((phase, done)) => {
                self.phase = phase;
                Ok(done)
            }
            Err(error) => {
                self.phase = Phase::Failed(error.code);
                Err(error)
            }
        }
    }

    fn advance(&self, phase: Phase, cancel: &AtomicBool) -> EngineResult<(Phase, bool)> {
        Ok(match phase {
            Phase::Decode(mut owner) => {
                if owner.collector.needs_reservation() {
                    owner.collector.step()?;
                    return Ok((Phase::Decode(owner), false));
                }
                let Decode {
                    source,
                    collector,
                    pcm,
                    any,
                } = &mut *owner;
                pcm.clear();
                if source.read(pcm)? {
                    *any |= !pcm.is_empty();
                    for chunk in pcm.chunks(APPEND_SAMPLES) {
                        collector.append_with(chunk, |sample| sample)?;
                    }
                    return Ok((Phase::Decode(owner), false));
                }
                if !*any {
                    return Err(EngineError::from_detail(
                        ErrorCode::InvalidArgument,
                        "audio decode produced no samples",
                    ));
                }
                let (rate, channels) = (source.rate(), source.channels());
                let Decode { mut collector, .. } = *owner;
                collector.seal()?;
                (Phase::Flatten(collector, rate, channels), false)
            }
            Phase::Flatten(mut collector, rate, channels) => {
                if !collector.step()? {
                    return Ok((Phase::Flatten(collector, rate, channels), false));
                }
                let audio = DecodedAudio {
                    samples: collector.finish()?,
                    sample_rate: rate,
                    channels,
                };
                match self.target {
                    Some(target) => (Phase::Resample(ResampleJob::new(audio, target)?), false),
                    None => (Phase::Complete(audio), true),
                }
            }
            Phase::Resample(mut resampler) => {
                if resampler.step(cancel)? {
                    (Phase::Complete(resampler.finish()?), true)
                } else {
                    (Phase::Resample(resampler), false)
                }
            }
            Phase::Complete(audio) => (Phase::Complete(audio), true),
            Phase::Failed(code) => {
                return Err(EngineError::from_detail(code, "audio decode job retired"));
            }
        })
    }

    pub(crate) fn finish(self) -> EngineResult<DecodedAudio> {
        match self.phase {
            Phase::Complete(audio) => Ok(audio),
            Phase::Failed(code) => Err(EngineError::from_detail(code, "audio decode job retired")),
            _ => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "audio decode job has not finished",
            )),
        }
    }
}

#[cfg(test)]
#[path = "whole_tests.rs"]
mod tests;
