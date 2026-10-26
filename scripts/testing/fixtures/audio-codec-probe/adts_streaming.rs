//! AAC-LC binding. ADTS parsing, timestamping and codec remain in Symphonia.
use super::incremental::{self, PacketStep, Packetizer};
use symphonia::core::{
    codecs::audio::{AudioCodecParameters, AudioDecoderOptions, well_known::CODEC_ID_AAC},
    errors::Result as FormatResult,
    formats::Track,
};
use symphonia_codec_aac::streaming::{AdtsPacketizer, PacketStep as AdtsStep, PacketizerOptions};

pub struct Adts(AdtsPacketizer);
impl Packetizer for Adts {
    type Codec = symphonia_codec_aac::AacDecoder;
    fn new() -> Self {
        Self(AdtsPacketizer::new(PacketizerOptions::default()))
    }
    fn track(&self) -> Option<&Track> {
        self.0.track()
    }
    fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> FormatResult<PacketStep<'a>> {
        self.0.next(input, eof).map(|step| match step {
            AdtsStep::NeedMore => PacketStep::NeedMore,
            AdtsStep::Discard(n) => PacketStep::Discard(n),
            AdtsStep::Packet { consumed, packet } => PacketStep::Packet { consumed, packet },
            AdtsStep::End => PacketStep::End,
        })
    }
    fn validate_codec(params: &AudioCodecParameters) -> Result<(), String> {
        if params.codec == CODEC_ID_AAC {
            Ok(())
        } else {
            Err("ADTS stream requires AAC-LC".into())
        }
    }
    fn create_decoder(params: &AudioCodecParameters) -> Result<Self::Codec, String> {
        Self::Codec::try_new(params, &AudioDecoderOptions::default().gapless(true))
            .map_err(|e| format!("decoder: {e}"))
    }
}
