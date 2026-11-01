//! Owned Vorbis steps; packet assembly, gapless timing and conversion stay in Symphonia.
use super::super::{DecodedAudio, pcm::PcmCollector};
use crate::resample_job::ResampleJob;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::atomic::{AtomicBool, Ordering};
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_format_ogg::streaming::{PacketStep, PacketizerOptions, VorbisPacketizer};

#[path = "ogg_duration.rs"]
mod duration;

const INPUT_BYTES: usize = 8192;
const MAX_WINDOW: usize = 65536;
const COPY_SAMPLES: usize = 8192;
const MAX_PARSER_CALLS: usize = 131072;
// Separate parser, codec and copies retain the former parser-call allowance.
// SRC has its own smaller bound; this also caps all ownership transitions.
const MAX_STEPS: usize = 1_048_576;

#[cfg(test)]
#[derive(Default)]
struct Trace {
    parser_calls: usize,
    codec_calls: usize,
    input_view: usize,
    copied_samples: usize,
}
#[cfg(test)]
thread_local! { static TRACE: std::cell::RefCell<Trace> = const { std::cell::RefCell::new(Trace { parser_calls:0,codec_calls:0,input_view:0,copied_samples:0 }) }; }

enum Action {
    Parse,
    Packet { consumed: usize, view: usize },
    Copy { frame: usize },
}
struct Decode<B> {
    scan: Option<duration::Scan>,
    input: B,
    parser: VorbisPacketizer,
    codec: Option<symphonia_codec_vorbis::VorbisDecoder>,
    scratch: Option<Box<[f32; COPY_SAMPLES]>>,
    collector: PcmCollector,
    offset: usize,
    window: usize,
    calls: usize,
    samples: usize,
    source: Option<(u32, u32)>,
    action: Action,
}
enum Phase<B> {
    Decode(Box<Decode<B>>),
    Flatten(PcmCollector, u32, u32),
    Decoded(DecodedAudio),
    Resample(ResampleJob),
    Complete(DecodedAudio),
    Failed(ErrorCode),
}
pub(crate) struct OggDecodeJob<B> {
    phase: Phase<B>,
    target: Option<u32>,
    limit: usize,
    steps: usize,
}
impl<B: AsRef<[u8]>> OggDecodeJob<B> {
    pub(crate) fn new(input: B, target: Option<u32>) -> EngineResult<Self> {
        Self::with_limit(input, target, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize)
    }
    pub(super) fn with_limit(input: B, target: Option<u32>, limit: usize) -> EngineResult<Self> {
        if input.as_ref().len() > 16 * 1024 * 1024 {
            return Err(invalid("OGG encoded input exceeds 16MiB"));
        }
        if target == Some(0) {
            return Err(invalid("OGG target rate must be nonzero"));
        }
        Ok(Self {
            phase: Phase::Decode(Box::new(Decode {
                input,
                scan: Some(duration::Scan::new()),
                parser: VorbisPacketizer::new(PacketizerOptions::default()),
                codec: None,
                scratch: None,
                collector: PcmCollector::new(),
                offset: 0,
                window: INPUT_BYTES,
                calls: 0,
                samples: 0,
                source: None,
                action: Action::Parse,
            })),
            target,
            limit: limit.min(crate::limits::MAX_AUDIO_PCM_SAMPLES as usize),
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
                Err(invalid("OGG decode exceeded its work budget"))
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
                if let Some(scan) = owner.scan.as_mut() {
                    match scan.step(owner.input.as_ref(), self.limit)? {
                        duration::Outcome::Yield => {}
                        duration::Outcome::Fallback => owner.scan = None,
                        duration::Outcome::Complete(samples) => {
                            owner.scan = None;
                            owner.collector = PcmCollector::with_expected_samples(Some(samples));
                        }
                    }
                    self.phase = Phase::Decode(owner);
                    return Ok(false);
                }
                if owner.collector.needs_reservation() {
                    owner.collector.step()?;
                    self.phase = Phase::Decode(owner);
                    return Ok(false);
                }
                match owner.action {
                    Action::Parse => {
                        owner.calls += 1;
                        if owner.calls > MAX_PARSER_CALLS {
                            return Err(invalid("OGG framing exceeded its work budget"));
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
                        match owner.parser.next(input, eof).map_err(codec_error)? {
                            PacketStep::NeedMore => {
                                if eof || owner.window == MAX_WINDOW {
                                    return Err(invalid("OGG framing exceeded its input window"));
                                }
                                owner.window = (owner.window + INPUT_BYTES).min(MAX_WINDOW);
                            }
                            PacketStep::Discard(n) => {
                                if n > input.len() {
                                    return Err(invalid("OGG consumed beyond its input"));
                                }
                                owner.offset += n;
                                if n != 0 {
                                    owner.window = INPUT_BYTES;
                                }
                            }
                            PacketStep::Packet { consumed } => {
                                if consumed > input.len() {
                                    return Err(invalid("OGG consumed beyond its input"));
                                }
                                owner.action = Action::Packet {
                                    consumed,
                                    view: input.len(),
                                };
                            }
                            PacketStep::End => {
                                if owner.offset != data.len() || owner.samples == 0 {
                                    return Err(invalid("OGG has no complete playable audio"));
                                }
                                let (rate, channels) = owner
                                    .source
                                    .ok_or_else(|| invalid("OGG has no audio parameters"))?;
                                let Decode {
                                    input,
                                    parser,
                                    codec,
                                    scratch,
                                    mut collector,
                                    ..
                                } = *owner;
                                // Unknown-length final reservation follows source retirement.
                                drop((input, parser, codec, scratch));
                                collector.seal()?;
                                self.phase = Phase::Flatten(collector, rate, channels);
                                return Ok(false);
                            }
                        }
                    }
                    Action::Packet { consumed, view } => {
                        if let Some(codec) = owner.codec.as_mut() {
                            let input = &owner.input.as_ref()[owner.offset..owner.offset + view];
                            let packet = owner
                                .parser
                                .packet(input)
                                .ok_or_else(|| invalid("OGG packet view missing"))?;
                            #[cfg(test)]
                            TRACE.with(|t| t.borrow_mut().codec_calls += 1);
                            let block = codec.decode_ref(&packet).map_err(codec_error)?;
                            let (rate, channels) = owner.source.unwrap();
                            if block.spec().rate() != rate
                                || block.spec().channels().count() != channels as usize
                            {
                                return Err(invalid("OGG audio format changed"));
                            }
                            owner.samples = owner
                                .samples
                                .checked_add(block.samples_interleaved())
                                .filter(|n| *n <= self.limit)
                                .ok_or_else(|| invalid("OGG decode exceeds the PCM budget"))?;
                            owner.offset += consumed;
                            if consumed != 0 {
                                owner.window = INPUT_BYTES;
                            }
                            owner.action = Action::Copy { frame: 0 };
                        } else {
                            let params = owner
                                .parser
                                .track()
                                .and_then(|t| t.codec_params.as_ref())
                                .and_then(|p| p.audio())
                                .ok_or_else(|| invalid("OGG has no audio parameters"))?;
                            let (rate, channels) = format(params)?;
                            owner.codec = Some(
                                symphonia_codec_vorbis::VorbisDecoder::try_new(
                                    params,
                                    &AudioDecoderOptions::default().gapless(true),
                                )
                                .map_err(codec_error)?,
                            );
                            owner.scratch = Some(Box::new([0.; COPY_SAMPLES]));
                            owner.source = Some((rate, channels));
                        }
                    }
                    Action::Copy { mut frame } => {
                        let block = owner.codec.as_ref().unwrap().last_decoded();
                        let done = copy_block(
                            block,
                            &mut frame,
                            &mut owner.scratch.as_mut().unwrap()[..],
                            &mut owner.collector,
                        )?;
                        owner.action = if done {
                            Action::Parse
                        } else {
                            Action::Copy { frame }
                        };
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
                Err(EngineError::from_detail(code, "OGG decode job retired"))
            }
        }
    }
    pub(crate) fn finish(self) -> EngineResult<DecodedAudio> {
        match self.phase {
            Phase::Complete(audio) => Ok(audio),
            Phase::Failed(code) => Err(EngineError::from_detail(code, "OGG decode job retired")),
            _ => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "OGG decode job has not completed",
            )),
        }
    }
}
// Both the product and the nonzero long-block regression use this cursor.
// Filtering/conversion remain in the mature library; only publication is split.
fn copy_block(
    block: symphonia_core::audio::GenericAudioBufferRef<'_>,
    frame: &mut usize,
    scratch: &mut [f32],
    collector: &mut PcmCollector,
) -> EngineResult<bool> {
    let channels = block.spec().channels().count();
    let end = (*frame + COPY_SAMPLES / channels).min(block.frames());
    let n = (end - *frame) * channels;
    if n != 0 {
        let view = block.slice(*frame..end);
        if !collector.append_direct(n, |out| view.copy_to_slice_interleaved(out))? {
            view.copy_to_slice_interleaved(&mut scratch[..n]);
            collector.append_with(&scratch[..n], |v| v)?;
        }
        #[cfg(test)]
        TRACE.with(|t| t.borrow_mut().copied_samples = n);
    }
    *frame = end;
    Ok(end == block.frames())
}
fn format(
    params: &symphonia_core::codecs::audio::AudioCodecParameters,
) -> EngineResult<(u32, u32)> {
    let rate = params
        .sample_rate
        .ok_or_else(|| invalid("OGG has no source rate"))?;
    let channels = params
        .channels
        .as_ref()
        .ok_or_else(|| invalid("OGG has no channels"))?
        .count() as u32;
    if !(crate::limits::MIN_SAMPLE_RATE..=crate::limits::MAX_SAMPLE_RATE).contains(&rate)
        || !(1..=32).contains(&channels)
    {
        return Err(invalid("OGG audio format outside supported limits"));
    }
    Ok((rate, channels))
}
fn invalid(message: &'static str) -> EngineError {
    EngineError::from_detail(ErrorCode::InvalidArgument, message)
}
fn codec_error(error: symphonia_core::errors::Error) -> EngineError {
    EngineError::from_detail(
        ErrorCode::InvalidArgument,
        format!("OGG decode error: {error}"),
    )
}

#[cfg(test)]
#[path = "ogg_job_tests.rs"]
mod tests;
