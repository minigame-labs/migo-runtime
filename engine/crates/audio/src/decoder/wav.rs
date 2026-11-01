//! Whole-file WAVE ownership over the mature bounded RIFF/PCM port.
//! Parsing and sample conversion stay in Symphonia; scheduling stays external.
use super::{DecodedAudio, pcm::PcmCollector};
use crate::resample_job::ResampleJob;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions, well_known::*};
use symphonia_format_riff::streaming::{PacketStep, PacketizerOptions, WavPacketizer};

const INPUT_BYTES: usize = 8192;
const FORMAT_BYTES: usize = 65536;
const COPY_SAMPLES: usize = 8192;
const MAX_STEPS: usize = 131_072;

#[cfg(test)]
#[derive(Default)]
struct Trace {
    parser_calls: usize,
    input_view: usize,
    consumed: usize,
    decoded_samples: usize,
    reservations: usize,
}
#[cfg(test)]
thread_local! { static TRACE: std::cell::RefCell<Trace> = const { std::cell::RefCell::new(Trace { parser_calls:0,input_view:0,consumed:0,decoded_samples:0,reservations:0 }) }; }

struct Decode<B> {
    input: B,
    parser: WavPacketizer,
    codec: Option<symphonia_codec_pcm::PcmDecoder>,
    scratch: Option<Box<[f32; COPY_SAMPLES]>>,
    collector: PcmCollector,
    offset: usize,
    window: usize,
    source: Option<(u32, u32)>,
    playable: bool,
    steps: usize,
    samples: usize,
}
enum Phase<B> {
    Decode(Box<Decode<B>>),
    Flatten(PcmCollector, u32, u32),
    Decoded(DecodedAudio),
    Resample(ResampleJob),
    Complete(DecodedAudio),
    Failed(ErrorCode),
}
pub(crate) struct WavDecodeJob<B> {
    phase: Phase<B>,
    target: Option<u32>,
    limit: usize,
}

impl<B: AsRef<[u8]>> WavDecodeJob<B> {
    pub(crate) fn new(input: B, target: Option<u32>) -> EngineResult<Self> {
        Self::with_limit(input, target, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize)
    }
    fn with_limit(input: B, target: Option<u32>, limit: usize) -> EngineResult<Self> {
        if input.as_ref().len() > 16 * 1024 * 1024 || target == Some(0) {
            return Err(invalid("WAVE input or target rate exceeds its policy"));
        }
        Ok(Self {
            phase: Phase::Decode(Box::new(Decode {
                input,
                parser: WavPacketizer::new(PacketizerOptions {
                    max_format_bytes: FORMAT_BYTES,
                    max_packet_bytes: INPUT_BYTES,
                }),
                codec: None,
                scratch: None,
                collector: PcmCollector::new(),
                offset: 0,
                window: INPUT_BYTES,
                source: None,
                playable: false,
                steps: 0,
                samples: 0,
            })),
            target,
            limit: limit.min(crate::limits::MAX_AUDIO_PCM_SAMPLES as usize),
        })
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
                owner.steps += 1;
                if owner.steps > MAX_STEPS {
                    return Err(invalid("WAVE step budget exceeded"));
                }
                if owner.collector.needs_reservation() {
                    #[cfg(test)]
                    TRACE.with(|t| t.borrow_mut().reservations += 1);
                    owner.collector.step()?;
                    self.phase = Phase::Decode(owner);
                    return Ok(false);
                }
                if owner.codec.is_none()
                    && let Some(track) = owner.parser.track()
                {
                    let params = track
                        .codec_params
                        .as_ref()
                        .and_then(|p| p.audio())
                        .ok_or_else(|| invalid("WAVE has no PCM parameters"))?;
                    if ![
                        CODEC_ID_PCM_U8,
                        CODEC_ID_PCM_S16LE,
                        CODEC_ID_PCM_S24LE,
                        CODEC_ID_PCM_S32LE,
                        CODEC_ID_PCM_F32LE,
                    ]
                    .contains(&params.codec)
                    {
                        return Err(invalid("WAVE codec outside the PCM profile"));
                    }
                    let rate = params
                        .sample_rate
                        .filter(|rate| *rate != 0)
                        .ok_or_else(|| invalid("WAVE has no source rate"))?;
                    let channels = params
                        .channels
                        .as_ref()
                        .map(|c| c.count() as u32)
                        .filter(|n| (1..=crate::limits::MAX_AUDIO_CHANNELS).contains(n))
                        .ok_or_else(|| invalid("WAVE channel count outside the profile"))?;
                    let expected = track
                        .num_frames
                        .filter(|frames| *frames != 0)
                        .map(|frames| {
                            frames
                                .checked_mul(u64::from(channels))
                                .and_then(|n| usize::try_from(n).ok())
                                .filter(|n| *n <= self.limit)
                                .ok_or_else(|| invalid("WAVE declared PCM exceeds its budget"))
                        })
                        .transpose()?;
                    owner.collector = PcmCollector::with_expected_samples(expected);
                    owner.codec = Some(
                        symphonia_codec_pcm::PcmDecoder::try_new(
                            params,
                            &AudioDecoderOptions::default(),
                        )
                        .map_err(codec_error)?,
                    );
                    owner.scratch = Some(Box::new([0.; COPY_SAMPLES]));
                    owner.source = Some((rate, channels));
                    self.phase = Phase::Decode(owner);
                    return Ok(false);
                }
                let data = owner.input.as_ref();
                let end = (owner.offset + owner.window).min(data.len());
                let input = &data[owner.offset..end];
                let eof = end == data.len();
                #[cfg(test)]
                TRACE.with(|t| {
                    let mut t = t.borrow_mut();
                    t.parser_calls += 1;
                    t.input_view = input.len();
                });
                let consumed = match owner.parser.next(input, eof).map_err(codec_error)? {
                    PacketStep::NeedMore => {
                        if eof || owner.window == FORMAT_BYTES {
                            return Err(invalid("WAVE format exceeds its input window"));
                        }
                        owner.window = (owner.window + INPUT_BYTES).min(FORMAT_BYTES);
                        self.phase = Phase::Decode(owner);
                        return Ok(false);
                    }
                    PacketStep::Discard(n) => n,
                    PacketStep::Packet { consumed, packet } => {
                        let block = owner
                            .codec
                            .as_mut()
                            .ok_or_else(|| invalid("WAVE packet precedes PCM configuration"))?
                            .decode_ref(&packet)
                            .map_err(codec_error)?;
                        let (rate, channels) = owner.source.unwrap();
                        if block.spec().rate() != rate
                            || block.spec().channels().count() != channels as usize
                        {
                            return Err(invalid("WAVE source format changed"));
                        }
                        let n = block.samples_interleaved();
                        #[cfg(test)]
                        TRACE.with(|t| t.borrow_mut().decoded_samples = n);
                        if n > COPY_SAMPLES {
                            return Err(invalid("WAVE PCM packet exceeds its copy budget"));
                        }
                        let total = owner
                            .samples
                            .checked_add(n)
                            .ok_or_else(|| invalid("WAVE PCM length overflow"))?;
                        if total > self.limit {
                            return Err(invalid("WAVE PCM exceeds its output budget"));
                        }
                        let scratch = owner.scratch.as_mut().unwrap();
                        block.copy_to_slice_interleaved(&mut scratch[..n]);
                        owner.collector.append_with(&scratch[..n], |v| v)?;
                        owner.samples = total;
                        owner.playable |= n != 0;
                        consumed
                    }
                    PacketStep::End => {
                        if !owner.playable {
                            return Err(invalid("WAVE has no complete playable audio"));
                        }
                        let (rate, channels) = owner
                            .source
                            .ok_or_else(|| invalid("WAVE missing source format"))?;
                        owner.collector.seal()?;
                        // Retire encoded/codec/scratch before the final collection or DSP phase.
                        self.phase = Phase::Flatten(owner.collector, rate, channels);
                        return Ok(false);
                    }
                };
                if consumed > input.len() {
                    return Err(invalid("WAVE consumed beyond its input"));
                }
                #[cfg(test)]
                TRACE.with(|t| t.borrow_mut().consumed = consumed);
                owner.offset += consumed;
                if consumed != 0 {
                    owner.window = INPUT_BYTES;
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
                Err(EngineError::from_detail(code, "WAVE decode job retired"))
            }
        }
    }
    pub(crate) fn finish(self) -> EngineResult<DecodedAudio> {
        match self.phase {
            Phase::Complete(audio) => Ok(audio),
            Phase::Failed(code) => Err(EngineError::from_detail(code, "WAVE decode job retired")),
            _ => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "WAVE decode job has not completed",
            )),
        }
    }
}
#[cfg(test)]
pub fn decode(data: &[u8]) -> EngineResult<DecodedAudio> {
    drain(WavDecodeJob::new(data, None)?)
}
#[cfg(test)]
fn drain<B: AsRef<[u8]>>(mut job: WavDecodeJob<B>) -> EngineResult<DecodedAudio> {
    let cancel = AtomicBool::new(false);
    while !job.step(&cancel)? {}
    job.finish()
}
fn invalid(message: &'static str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, message)
}
fn codec_error(error: symphonia_core::errors::Error) -> EngineError {
    EngineError::from_detail(
        ErrorCode::InvalidArgument,
        format!("WAVE decode error: {error}"),
    )
}

#[cfg(test)]
#[path = "wav_tests.rs"]
mod tests;
