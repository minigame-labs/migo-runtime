//! Typed WAVE/PCM binding; RIFF/fmt/fact/data parsing stays in Symphonia.
use super::incremental::{self, PacketStep, Packetizer};
use symphonia::core::{
    codecs::audio::{AudioCodecParameters, AudioDecoderOptions, well_known::*},
    errors::Result as FormatResult,
    formats::Track,
};
use symphonia_format_riff::streaming::{PacketStep as WavStep, PacketizerOptions, WavPacketizer};

pub struct Wave(WavPacketizer);
impl Packetizer for Wave {
    type Codec = symphonia_codec_pcm::PcmDecoder;
    // PCM conversion can consume a complete bounded input buffer. Optional
    // metadata still obeys the shared 4096-byte scanning budget.
    const ENCODED_BYTES_PER_STEP: usize = incremental::ENCODED_CAPACITY;
    fn new() -> Self {
        Self(WavPacketizer::new(PacketizerOptions {
            max_format_bytes: incremental::ENCODED_CAPACITY,
            max_packet_bytes: incremental::SAMPLES_PER_STEP,
        }))
    }
    fn track(&self) -> Option<&Track> {
        self.0.track()
    }
    fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> FormatResult<PacketStep<'a>> {
        self.0.next(input, eof).map(|step| match step {
            WavStep::NeedMore => PacketStep::NeedMore,
            WavStep::Discard(n) => PacketStep::Discard(n),
            WavStep::Packet { consumed, packet } => PacketStep::Packet { consumed, packet },
            WavStep::End => PacketStep::End,
        })
    }
    fn validate_codec(params: &AudioCodecParameters) -> Result<(), String> {
        if [
            CODEC_ID_PCM_U8,
            CODEC_ID_PCM_S16LE,
            CODEC_ID_PCM_S24LE,
            CODEC_ID_PCM_S32LE,
            CODEC_ID_PCM_F32LE,
        ]
        .contains(&params.codec)
        {
            Ok(())
        } else {
            Err("streaming WAVE profile supports integer PCM and float32".into())
        }
    }
    fn create_decoder(params: &AudioCodecParameters) -> Result<Self::Codec, String> {
        Self::Codec::try_new(params, &AudioDecoderOptions::default())
            .map_err(|e| format!("decoder: {e}"))
    }
}
