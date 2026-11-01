//! Product Vorbis uses the same mature packetizer/decoder as the W09 bounded port.
//! The library owns page validation, packet assembly and presentation trimming.
use super::DecodedAudio;
use shared::error::{EngineError, EngineResult, ErrorCode};
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_format_ogg::streaming::{PacketStep, PacketizerOptions, VorbisPacketizer};

const INPUT_BYTES: usize = 8192;
const MAX_WINDOW: usize = 65536;
const MAX_STEPS: usize = 131072;

pub fn decode(data: &[u8]) -> EngineResult<DecodedAudio> {
    decode_with_limit(data, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize)
}
fn decode_with_limit(data: &[u8], limit: usize) -> EngineResult<DecodedAudio> {
    let limit = limit.min(crate::limits::MAX_AUDIO_PCM_SAMPLES as usize);
    if data.len() > 16 * 1024 * 1024 {
        return Err(invalid("OGG encoded input exceeds 16MiB"));
    }
    let mut packetizer = VorbisPacketizer::new(PacketizerOptions::default());
    let mut decoder = None;
    let mut offset = 0;
    let mut window = INPUT_BYTES;
    let mut output = DecodedAudio {
        samples: Vec::new(),
        sample_rate: 0,
        channels: 0,
    };
    for _ in 0..MAX_STEPS {
        let end = (offset + window).min(data.len());
        let input = &data[offset..end];
        let eof = end == data.len();
        let step = packetizer.next(input, eof).map_err(codec_error)?;
        let consumed = match step {
            PacketStep::NeedMore => {
                if eof || window == MAX_WINDOW {
                    return Err(invalid("OGG framing exceeded its input window"));
                }
                window = (window + INPUT_BYTES).min(MAX_WINDOW);
                continue;
            }
            PacketStep::Discard(consumed) => consumed,
            PacketStep::Packet { consumed } => {
                if decoder.is_none() {
                    let params = packetizer
                        .track()
                        .and_then(|t| t.codec_params.as_ref())
                        .and_then(|p| p.audio())
                        .ok_or_else(|| invalid("OGG has no audio parameters"))?;
                    let rate = params
                        .sample_rate
                        .ok_or_else(|| invalid("OGG has no source rate"))?;
                    let channels = params
                        .channels
                        .as_ref()
                        .ok_or_else(|| invalid("OGG has no channels"))?
                        .count() as u32;
                    if !(crate::limits::MIN_SAMPLE_RATE..=crate::limits::MAX_SAMPLE_RATE)
                        .contains(&rate)
                        || !(1..=32).contains(&channels)
                    {
                        return Err(invalid("OGG audio format outside supported limits"));
                    }
                    output.sample_rate = rate;
                    output.channels = channels;
                    decoder = Some(
                        symphonia_codec_vorbis::VorbisDecoder::try_new(
                            params,
                            &AudioDecoderOptions::default().gapless(true),
                        )
                        .map_err(codec_error)?,
                    );
                }
                let packet = packetizer
                    .packet(input)
                    .ok_or_else(|| invalid("OGG packet view missing"))?;
                let block = decoder
                    .as_mut()
                    .unwrap()
                    .decode_ref(&packet)
                    .map_err(codec_error)?;
                if block.spec().rate() != output.sample_rate
                    || block.spec().channels().count() != output.channels as usize
                {
                    return Err(invalid("OGG audio format changed"));
                }
                let samples = output
                    .samples
                    .len()
                    .checked_add(block.samples_interleaved())
                    .filter(|n| *n <= limit)
                    .ok_or_else(|| invalid("OGG decode exceeds the PCM budget"))?;
                if samples > output.samples.capacity() {
                    let capacity = output
                        .samples
                        .capacity()
                        .saturating_mul(2)
                        .max(samples)
                        .min(limit);
                    output
                        .samples
                        .try_reserve_exact(capacity - output.samples.len())
                        .map_err(|_| {
                            EngineError::from_detail(
                                ErrorCode::OutOfMemory,
                                "OGG PCM reservation refused",
                            )
                        })?;
                }
                let start = output.samples.len();
                output.samples.resize(samples, 0.0);
                block.copy_to_slice_interleaved(&mut output.samples[start..]);
                consumed
            }
            PacketStep::End => {
                if offset != data.len() || output.samples.is_empty() {
                    return Err(invalid("OGG has no complete playable audio"));
                }
                return Ok(output);
            }
        };
        if consumed > input.len() {
            return Err(invalid("OGG consumed beyond its input"));
        }
        offset += consumed;
        if consumed != 0 {
            window = INPUT_BYTES;
        }
    }
    Err(invalid("OGG decode exceeded its work budget"))
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

