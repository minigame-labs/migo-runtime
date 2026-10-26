//! Typed Vorbis binding; Ogg page/packet/timestamp/assembly stay in the library.
use super::incremental::{self, PacketStep, Packetizer};
use symphonia::core::{
    codecs::audio::{AudioCodecParameters, AudioDecoderOptions, well_known::CODEC_ID_VORBIS},
    errors::Result as FormatResult,
    formats::Track,
    packet::PacketRef,
};
use symphonia_format_ogg::streaming::{PacketStep as OggStep, PacketizerOptions, VorbisPacketizer};
pub struct Vorbis(VorbisPacketizer);
impl Packetizer for Vorbis {
    type Codec = symphonia_codec_vorbis::VorbisDecoder;
    // One legal long block can produce 4096 frames in each of eight channels.
    // This fits the existing PCM publication buffer; other formats keep their
    // old smaller decode scratch and packet counts.
    const OUTPUT_SAMPLES_PER_STEP: usize = 32 * 1024;
    fn new() -> Self {
        Self(VorbisPacketizer::new(PacketizerOptions::default()))
    }
    fn track(&self) -> Option<&Track> {
        self.0.track()
    }
    fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> FormatResult<PacketStep<'a>> {
        self.0.next(input, eof).map(|step| match step {
            OggStep::NeedMore => PacketStep::NeedMore,
            OggStep::Discard(n) => PacketStep::Discard(n),
            OggStep::Packet { consumed } => PacketStep::BufferedPacket { consumed },
            OggStep::End => PacketStep::End,
        })
    }
    fn buffered_packet<'a>(&'a self, input: &'a [u8]) -> Option<PacketRef<'a>> {
        self.0.packet(input)
    }
    fn validate_codec(params: &AudioCodecParameters) -> Result<(), String> {
        if params.codec == CODEC_ID_VORBIS {
            Ok(())
        } else {
            Err("streaming Ogg profile requires Vorbis".into())
        }
    }
    fn create_decoder(params: &AudioCodecParameters) -> Result<Self::Codec, String> {
        Self::Codec::try_new(params, &AudioDecoderOptions::default().gapless(true))
            .map_err(|e| format!("decoder: {e}"))
    }
}
