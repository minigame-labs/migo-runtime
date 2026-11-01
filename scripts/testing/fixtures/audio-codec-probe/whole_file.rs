//! Candidate whole-file ownership over the existing bounded codec/sinc ports.
//! Only a completed, continuous buffer leaves this owner. No parser lives here.
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::decoder::DecodedAudio;
use crate::{
    decoder::pcm::PcmCollector,
    resampled_streaming::{Resampled, VorbisResampled, WaveResampled},
    streaming_transport::{Decoder, Progress, Step},
};
use shared::error::{EngineError, EngineResult, ErrorCode};

const INPUT_BYTES: usize = 8192;
const COPY_SAMPLES: usize = 8192;
const SCRATCH_SAMPLES: usize = crate::bandlimited::MAX_SCRATCH_SAMPLES;
const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_STEPS: usize = 131_072;
#[path = "whole_file_duration.rs"]
mod duration;

#[cfg(feature = "m4a-streaming-probe")]
#[path = "whole_file_m4a.rs"]
mod m4a;

#[derive(Clone, Copy)]
pub enum Format {
    Mp3,
    Wave,
    Vorbis,
    #[cfg(feature = "adts-streaming-probe")]
    Adts,
    #[cfg(feature = "m4a-streaming-probe")]
    M4a,
}

enum Codec {
    Mp3(Box<Resampled>),
    Wave(Box<WaveResampled>),
    Vorbis(Box<VorbisResampled>),
    #[cfg(feature = "adts-streaming-probe")]
    Adts(Box<crate::resampled_streaming::AdtsResampled>),
    #[cfg(feature = "m4a-streaming-probe")]
    M4a(Box<m4a::File>),
}
impl Codec {
    fn expected_samples(&self, target_rate: u32) -> EngineResult<Option<usize>> {
        let track = match self {
            Self::Mp3(decoder) => decoder.presentation_track(),
            Self::Wave(decoder) => decoder.presentation_track(),
            Self::Vorbis(decoder) => decoder.presentation_track(),
            #[cfg(feature = "adts-streaming-probe")]
            Self::Adts(decoder) => decoder.presentation_track(),
            #[cfg(feature = "m4a-streaming-probe")]
            Self::M4a(decoder) => decoder.decoder.presentation_track(),
        };
        let Some(track) = track else {
            return Ok(None);
        };
        let Some(frames) = track.num_frames else {
            return Ok(None);
        };
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or_else(|| invalid("whole-file track has no audio parameters"))?;
        let rate = params
            .sample_rate
            .ok_or_else(|| invalid("whole-file track has no sample rate"))?;
        let channels = params
            .channels
            .as_ref()
            .ok_or_else(|| invalid("whole-file track has no channels"))?
            .count() as u32;
        let timeline = crate::streaming::StreamTimeline::new(frames, rate, target_rate, channels)?;
        timeline
            .output_frames()
            .checked_mul(u64::from(channels))
            .and_then(|samples| usize::try_from(samples).ok())
            .map(Some)
            .ok_or_else(|| invalid("whole-file declared output overflows"))
    }
    fn step(
        &mut self,
        data: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        match self {
            Self::Mp3(decoder) => decoder.step(data, eof, pcm, cancel),
            Self::Wave(decoder) => decoder.step(data, eof, pcm, cancel),
            Self::Vorbis(decoder) => decoder.step(data, eof, pcm, cancel),
            #[cfg(feature = "adts-streaming-probe")]
            Self::Adts(decoder) => decoder.step(data, eof, pcm, cancel),
            #[cfg(feature = "m4a-streaming-probe")]
            Self::M4a(decoder) => decoder.step(data, eof, pcm, cancel),
        }
    }

    fn source_step(&mut self, _data: &[u8], _offset: &mut usize) -> EngineResult<bool> {
        #[cfg(feature = "m4a-streaming-probe")]
        if let Self::M4a(decoder) = self {
            return decoder.source_step(_data, _offset);
        }
        Ok(false)
    }

    fn source_waiting(&self) -> bool {
        #[cfg(feature = "m4a-streaming-probe")]
        if let Self::M4a(decoder) = self {
            return decoder.source_waiting();
        }
        false
    }
}

struct Decode<B> {
    scan: Option<duration::Scan>,
    input: B,
    codec: Codec,
    offset: usize,
    pcm: Vec<f32>,
    head: usize,
    collector: PcmCollector,
    samples: usize,
    metadata: Option<(u32, u32)>,
    expected_samples: Option<usize>,
    ended: bool,
}

enum Phase<B> {
    Decode(Box<Decode<B>>),
    Flatten(PcmCollector, u32, u32),
    Complete(DecodedAudio),
    Failed(ErrorCode),
}

pub struct WholeFileJob<B> {
    phase: Phase<B>,
    target_rate: u32,
    limit: usize,
    remaining_steps: usize,
}

impl<B: AsRef<[u8]>> WholeFileJob<B> {
    /// The optional caller limit can tighten, but never raise, the 64MiB cap.
    pub fn new(input: B, format: Format, target_rate: u32, limit: usize) -> EngineResult<Self> {
        if input.as_ref().len() > MAX_INPUT_BYTES {
            return Err(invalid("whole-file encoded input exceeds 16MiB"));
        }
        if !(crate::limits::MIN_SAMPLE_RATE..=crate::limits::MAX_SAMPLE_RATE).contains(&target_rate)
        {
            return Err(invalid("whole-file target rate outside supported limits"));
        }
        let codec = match format {
            Format::Mp3 => Codec::Mp3(Box::new(Resampled::new(target_rate))),
            Format::Wave => Codec::Wave(Box::new(WaveResampled::new(target_rate))),
            Format::Vorbis => Codec::Vorbis(Box::new(VorbisResampled::new(target_rate))),
            #[cfg(feature = "adts-streaming-probe")]
            Format::Adts => Codec::Adts(Box::new(crate::resampled_streaming::AdtsResampled::new(
                target_rate,
            ))),
            #[cfg(feature = "m4a-streaming-probe")]
            Format::M4a => Codec::M4a(Box::new(m4a::File::new(target_rate, input.as_ref().len()))),
        };
        Ok(Self {
            phase: Phase::Decode(Box::new(Decode {
                scan: duration::Scan::new(format),
                input,
                codec,
                offset: 0,
                pcm: Vec::with_capacity(SCRATCH_SAMPLES),
                head: 0,
                collector: PcmCollector::new(),
                samples: 0,
                metadata: None,
                expected_samples: None,
                ended: false,
            })),
            target_rate,
            limit: limit.min(crate::limits::MAX_AUDIO_PCM_SAMPLES as usize),
            remaining_steps: MAX_STEPS,
        })
    }

    /// One existing codec/DSP operation, one bounded PCM copy, or one final copy.
    pub fn step(&mut self, cancel: &AtomicBool) -> EngineResult<bool> {
        if cancel.load(Ordering::Acquire) {
            self.phase = Phase::Failed(ErrorCode::Cancelled);
        }
        let result = match self.phase {
            Phase::Failed(code) => Err(EngineError::from_detail(code, "whole-file job retired")),
            Phase::Complete(_) => Ok(true),
            _ => self
                .remaining_steps
                .checked_sub(1)
                .ok_or_else(|| invalid("whole-file work budget exceeded"))
                .and_then(|remaining| {
                    self.remaining_steps = remaining;
                    self.advance(cancel)
                }),
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
                if let Some(scan) = owner.scan.as_mut() {
                    match scan.step(owner.input.as_ref(), self.target_rate, self.limit)? {
                        duration::Outcome::Yield => {}
                        duration::Outcome::Fallback => {
                            owner.scan = None;
                        }
                        duration::Outcome::Complete { samples, channels } => {
                            owner.expected_samples = Some(samples);
                            owner.metadata = Some((self.target_rate, channels));
                            owner.collector = PcmCollector::with_expected_samples(Some(samples));
                            owner.scan = None;
                        }
                    }
                } else if owner.collector.needs_reservation() {
                    owner.collector.step()?;
                } else if owner.head < owner.pcm.len() {
                    let end = (owner.head + COPY_SAMPLES).min(owner.pcm.len());
                    let count = end - owner.head;
                    let total = owner
                        .samples
                        .checked_add(count)
                        .filter(|samples| *samples <= self.limit)
                        .ok_or_else(|| invalid("whole-file PCM exceeds its output budget"))?;
                    owner
                        .collector
                        .append_with(&owner.pcm[owner.head..end], std::convert::identity)?;
                    owner.head = end;
                    owner.samples = total;
                } else if owner.ended {
                    if owner
                        .expected_samples
                        .is_some_and(|expected| expected != owner.samples)
                    {
                        return Err(invalid(
                            "whole-file output disagrees with the source timeline",
                        ));
                    }
                    let Decode {
                        input,
                        codec,
                        pcm,
                        mut collector,
                        metadata,
                        ..
                    } = *owner;
                    // Retire input, codec/DSP and scratch before publication.
                    // Unknown lengths reserve their final buffer only afterwards;
                    // a declared result already occupies its single allocation.
                    drop((input, codec, pcm));
                    let (rate, channels) =
                        metadata.ok_or_else(|| invalid("whole-file stream has no audio format"))?;
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
                    let offered = end - owner.offset;
                    let eof = owner.offset == data.len();
                    let step =
                        owner
                            .codec
                            .step(&data[owner.offset..end], eof, &mut owner.pcm, cancel)?;
                    if step.consumed > offered || owner.pcm.len() > SCRATCH_SAMPLES {
                        return Err(invalid("whole-file codec exceeded its step contract"));
                    }
                    owner.offset += step.consumed;
                    if let Some(expected) = owner.codec.expected_samples(self.target_rate)? {
                        if expected > self.limit
                            || owner.expected_samples.is_some_and(|old| old != expected)
                        {
                            return Err(invalid(
                                "whole-file declared output exceeds or changes its budget",
                            ));
                        }
                        if owner.expected_samples.is_none() && owner.samples == 0 {
                            owner.collector = PcmCollector::with_expected_samples(Some(expected));
                        }
                        owner.expected_samples = Some(expected);
                    }
                    if step.sample_rate != 0 {
                        let format = (step.sample_rate, step.channels);
                        if step.sample_rate != self.target_rate
                            || !(1..=crate::limits::MAX_AUDIO_CHANNELS).contains(&step.channels)
                            || owner.metadata.is_some_and(|previous| previous != format)
                        {
                            return Err(invalid("whole-file codec changed its output format"));
                        }
                        owner.metadata = Some(format);
                    }
                    if !owner.pcm.is_empty() && owner.metadata.is_none() {
                        return Err(invalid("whole-file PCM has no output format"));
                    }
                    match step.progress {
                        Progress::Cancelled => {
                            return Err(EngineError::from_detail(
                                ErrorCode::Cancelled,
                                "whole-file codec cancelled",
                            ));
                        }
                        Progress::NeedSource => {
                            if !owner.codec.source_waiting() {
                                return Err(invalid(
                                    "whole-file codec has no pending source request",
                                ));
                            }
                        }
                        Progress::Finished => {
                            if owner.offset != data.len() {
                                return Err(invalid(
                                    "whole-file codec ended before input was consumed",
                                ));
                            }
                            owner.ended = true;
                        }
                        Progress::NeedInput if eof => {
                            return Err(invalid("whole-file codec requested input after EOF"));
                        }
                        Progress::NeedInput | Progress::Yield => {}
                    }
                }
                self.phase = Phase::Decode(owner);
                Ok(false)
            }
            Phase::Flatten(mut collector, sample_rate, channels) => {
                if collector.step()? {
                    self.phase = Phase::Complete(DecodedAudio {
                        samples: collector.finish()?,
                        sample_rate,
                        channels,
                    });
                    Ok(true)
                } else {
                    self.phase = Phase::Flatten(collector, sample_rate, channels);
                    Ok(false)
                }
            }
            Phase::Complete(audio) => {
                self.phase = Phase::Complete(audio);
                Ok(true)
            }
            Phase::Failed(code) => Err(EngineError::from_detail(code, "whole-file job retired")),
        }
    }

    pub fn finish(self) -> EngineResult<DecodedAudio> {
        match self.phase {
            Phase::Complete(audio) => Ok(audio),
            Phase::Failed(code) => Err(EngineError::from_detail(code, "whole-file job retired")),
            _ => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "whole-file job has not finished",
            )),
        }
    }
}

fn invalid(detail: &'static str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, detail)
}

#[cfg(test)]
#[path = "whole_file_contracts.rs"]
mod tests;

#[cfg(all(
    test,
    feature = "m4a-streaming-probe",
    feature = "adts-streaming-probe"
))]
#[path = "whole_file_codecs_contracts.rs"]
mod codecs_tests;

#[cfg(all(
    test,
    feature = "m4a-streaming-probe",
    feature = "adts-streaming-probe"
))]
#[path = "whole_file_known_contracts.rs"]
mod known_tests;
