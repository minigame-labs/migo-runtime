//! AAC-LC jobs reuse the verified packet/source port and the existing PCM/SRC jobs.
//! Symphonia owns ADTS/MP4 parsing, codec state and presentation trimming.
pub(crate) mod adts;
pub(crate) mod incremental;
pub(crate) mod m4a;
mod m4a_source;

use super::{DecodedAudio, pcm::PcmCollector};
use crate::resample_job::ResampleJob;
use incremental::{Decoder, Progress, SAMPLES_PER_STEP};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};
use symphonia_core::formats::Track;

const INPUT_BYTES: usize = 8192;
const COPY_SAMPLES: usize = 8192;
const MAX_STEPS: usize = 1_048_576;

enum Codec {
    Adts(Box<Decoder<adts::Adts>>),
    M4a(Box<m4a_source::File>),
}
impl Codec {
    fn track(&self) -> Option<&Track> {
        match self {
            Self::Adts(decoder) => decoder.presentation_track(),
            Self::M4a(file) => file.decoder.presentation_track(),
        }
    }
    fn format(&self) -> (u32, u32) {
        match self {
            Self::Adts(d) => (d.rate, d.channels),
            Self::M4a(f) => (f.decoder.rate, f.decoder.channels),
        }
    }
    fn push(&mut self, data: &[u8], eof: bool) -> EngineResult<usize> {
        fn push<P: incremental::Packetizer>(
            decoder: &mut Decoder<P>,
            data: &[u8],
            eof: bool,
        ) -> EngineResult<usize> {
            let n = if data.is_empty() {
                0
            } else {
                decoder.push(data).map_err(codec_error)?
            };
            if eof {
                decoder.finish_input();
            }
            Ok(n)
        }
        match self {
            Self::Adts(d) => push(d.as_mut(), data, eof),
            Self::M4a(f) => push(&mut f.decoder, data, eof),
        }
    }
    fn drain(&mut self, pcm: &mut Vec<f32>, cancel: &AtomicBool) -> EngineResult<Progress> {
        match self {
            Self::Adts(d) => d.drain(pcm, cancel).map_err(codec_error),
            Self::M4a(f) => f.drain(pcm, cancel),
        }
    }
    fn source_step(&mut self, data: &[u8], offset: &mut usize) -> EngineResult<bool> {
        match self {
            Self::Adts(_) => Ok(false),
            Self::M4a(f) => f.source_step(data, offset),
        }
    }
}

struct Decode<B> {
    input: B,
    codec: Codec,
    offset: usize,
    pcm: Vec<f32>,
    head: usize,
    collector: PcmCollector,
    expected: Option<usize>,
    samples: usize,
    ended: bool,
}
enum Phase<B> {
    Decode(Box<Decode<B>>),
    Flatten(PcmCollector, u32, u32),
    Decoded(DecodedAudio),
    Resample(ResampleJob),
    Complete(DecodedAudio),
    Failed(ErrorCode),
}
pub(crate) struct AacDecodeJob<B> {
    phase: Phase<B>,
    target: Option<u32>,
    limit: usize,
    steps: usize,
}
impl<B: AsRef<[u8]>> AacDecodeJob<B> {
    pub(crate) fn new(input: B, target: Option<u32>, m4a: bool) -> EngineResult<Self> {
        Self::with_limit(
            input,
            target,
            m4a,
            crate::limits::MAX_AUDIO_PCM_SAMPLES as usize,
        )
    }
    fn with_limit(input: B, target: Option<u32>, m4a: bool, limit: usize) -> EngineResult<Self> {
        if target == Some(0) || input.as_ref().len() > 16 * 1024 * 1024 {
            return Err(invalid("AAC input or target outside limits"));
        }
        let limit = limit.min(crate::limits::MAX_AUDIO_PCM_SAMPLES as usize);
        let codec = if m4a {
            Codec::M4a(Box::new(m4a_source::File::new(limit, input.as_ref().len())))
        } else {
            Codec::Adts(Box::new(Decoder::new(Some(limit as u64))))
        };
        Ok(Self {
            phase: Phase::Decode(Box::new(Decode {
                input,
                codec,
                offset: 0,
                pcm: Vec::with_capacity(SAMPLES_PER_STEP),
                head: 0,
                collector: PcmCollector::new(),
                expected: None,
                samples: 0,
                ended: false,
            })),
            target,
            limit,
            steps: 0,
        })
    }
    pub(crate) fn step(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        if cancel.load(Ordering::Acquire) {
            self.phase = Phase::Failed(ErrorCode::Cancelled);
        }
        let result = if matches!(self.phase, Phase::Complete(_) | Phase::Failed(_)) {
            self.advance(cancel)
        } else {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                Err(invalid("AAC decode work budget exceeded"))
            } else {
                self.advance(cancel)
            }
        };
        if let Err(error) = &result {
            self.phase = Phase::Failed(error.code);
        }
        result
    }
    fn advance(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        let phase = std::mem::replace(&mut self.phase, Phase::Failed(ErrorCode::Internal));
        match phase {
            Phase::Decode(mut owner) => {
                if owner.collector.needs_reservation() {
                    owner.collector.step()?;
                } else if owner.head < owner.pcm.len() {
                    let end = (owner.head + COPY_SAMPLES).min(owner.pcm.len());
                    owner
                        .collector
                        .append_with(&owner.pcm[owner.head..end], std::convert::identity)?;
                    owner.samples += end - owner.head;
                    owner.head = end;
                } else if owner.ended {
                    let (rate, channels) = owner.codec.format();
                    if rate == 0
                        || owner.samples == 0
                        || owner.expected.is_some_and(|n| n != owner.samples)
                    {
                        return Err(invalid("AAC output disagrees with its presentation"));
                    }
                    let Decode {
                        input,
                        codec,
                        pcm,
                        mut collector,
                        ..
                    } = *owner;
                    // The original shared CPU retires every encoded/codec owner before
                    // unknown-length final reservation, SRC and result publication.
                    drop((input, codec, pcm));
                    collector.seal()?;
                    self.phase = Phase::Flatten(collector, rate, channels);
                    return Ok(false);
                } else {
                    owner.pcm.clear();
                    owner.head = 0;
                    let data = owner.input.as_ref();
                    if owner.codec.source_step(data, &mut owner.offset)? {
                        self.phase = Phase::Decode(owner);
                        return Ok(false);
                    }
                    let end = (owner.offset + INPUT_BYTES).min(data.len());
                    let eof = owner.offset == data.len();
                    owner.offset += owner.codec.push(&data[owner.offset..end], eof)?;
                    let progress = owner.codec.drain(&mut owner.pcm, cancel)?;
                    if owner.pcm.len() > SAMPLES_PER_STEP
                        || owner
                            .samples
                            .checked_add(owner.pcm.len())
                            .is_none_or(|n| n > self.limit)
                    {
                        return Err(invalid("AAC decoded PCM exceeds its budget"));
                    }
                    if let Some(track) = owner.codec.track()
                        && let Some((frames, channels)) = track.num_frames.zip(
                            track
                                .codec_params
                                .as_ref()
                                .and_then(|p| p.audio())
                                .and_then(|p| p.channels.as_ref())
                                .map(|c| c.count()),
                        )
                    {
                        let expected = frames
                            .checked_mul(channels as u64)
                            .and_then(|n| usize::try_from(n).ok())
                            .filter(|n| *n <= self.limit)
                            .ok_or_else(|| invalid("AAC declared PCM exceeds its budget"))?;
                        if owner.expected.is_some_and(|n| n != expected) {
                            return Err(invalid("AAC presentation changed"));
                        }
                        if owner.expected.is_none() && owner.samples == 0 {
                            owner.collector = PcmCollector::with_expected_samples(Some(expected));
                        }
                        owner.expected = Some(expected);
                    }
                    match progress {
                        Progress::Finished => {
                            if owner.offset != data.len() {
                                return Err(invalid("AAC ended before input was consumed"));
                            }
                            owner.ended = true;
                        }
                        Progress::NeedInput if eof => {
                            return Err(invalid("AAC requested input after EOF"));
                        }
                        Progress::Cancelled => {
                            return Err(EngineError::from_detail(
                                ErrorCode::Cancelled,
                                "AAC codec cancelled",
                            ));
                        }
                        Progress::NeedInput | Progress::NeedSource | Progress::Yield => {}
                    }
                }
                self.phase = Phase::Decode(owner);
                Ok(false)
            }
            Phase::Flatten(mut collector, rate, channels) => {
                self.phase = if collector.step()? {
                    Phase::Decoded(DecodedAudio {
                        samples: collector.finish()?,
                        sample_rate: rate,
                        channels,
                    })
                } else {
                    Phase::Flatten(collector, rate, channels)
                };
                Ok(false)
            }
            Phase::Decoded(audio) => {
                if let Some(rate) = self.target {
                    let output = (audio.frame_count() as u128 * rate as u128)
                        .div_ceil(audio.sample_rate as u128)
                        * audio.channels as u128;
                    if output > self.limit as u128 {
                        return Err(invalid("AAC converted PCM exceeds its budget"));
                    }
                    self.phase = Phase::Resample(ResampleJob::new(audio, rate)?);
                    Ok(false)
                } else {
                    self.phase = Phase::Complete(audio);
                    Ok(true)
                }
            }
            Phase::Resample(mut job) => {
                if job.step(cancel)? {
                    self.phase = Phase::Complete(job.finish()?);
                    Ok(true)
                } else {
                    self.phase = Phase::Resample(job);
                    Ok(false)
                }
            }
            Phase::Complete(audio) => {
                self.phase = Phase::Complete(audio);
                Ok(true)
            }
            Phase::Failed(code) => {
                self.phase = Phase::Failed(code);
                Err(EngineError::from_detail(code, "AAC decode job retired"))
            }
        }
    }
    pub(crate) fn finish(self) -> EngineResult<DecodedAudio> {
        match self.phase {
            Phase::Complete(audio) => Ok(audio),
            Phase::Failed(code) => Err(EngineError::from_detail(code, "AAC decode job retired")),
            _ => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "AAC decode job incomplete",
            )),
        }
    }
}
fn invalid(detail: &'static str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, detail)
}
fn codec_error(detail: String) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, detail)
}

#[cfg(test)]
mod tests;
