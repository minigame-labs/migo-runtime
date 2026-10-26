//! Typed MP3 binding over the common bounded streaming decoder.
use symphonia::core::{
    codecs::audio::{AudioCodecParameters, AudioDecoderOptions, well_known::CODEC_ID_MP3},
    errors::Result as FormatResult,
    formats::Track,
};
use symphonia_bundle_mp3::streaming::{MpaPacketizer, PacketStep as MpaStep, PacketizerOptions};

#[path = "incremental_decoder.rs"]
pub(crate) mod incremental;
pub use incremental::{ENCODED_CAPACITY, Progress, SAMPLES_PER_STEP};
pub type Decoder = incremental::Decoder<Mp3>;

pub struct Mp3(MpaPacketizer);
impl incremental::Packetizer for Mp3 {
    type Codec = symphonia_bundle_mp3::MpaDecoder;
    fn new() -> Self {
        Self(MpaPacketizer::new(PacketizerOptions::default()))
    }
    fn track(&self) -> Option<&Track> {
        self.0.track()
    }
    fn next<'a>(
        &mut self,
        input: &'a [u8],
        eof: bool,
    ) -> FormatResult<incremental::PacketStep<'a>> {
        use incremental::PacketStep;
        self.0.next(input, eof).map(|step| match step {
            MpaStep::NeedMore => PacketStep::NeedMore,
            MpaStep::Discard(n) => PacketStep::Discard(n),
            MpaStep::Packet { consumed, packet } => PacketStep::Packet { consumed, packet },
            MpaStep::End => PacketStep::End,
        })
    }
    fn validate_codec(params: &AudioCodecParameters) -> Result<(), String> {
        if params.codec == CODEC_ID_MP3 {
            Ok(())
        } else {
            Err("streaming only supports MP3".into())
        }
    }
    fn create_decoder(params: &AudioCodecParameters) -> Result<Self::Codec, String> {
        Self::Codec::try_new(params, &AudioDecoderOptions::default().gapless(true))
            .map_err(|e| format!("decoder: {e}"))
    }
}

#[cfg(feature = "wave-streaming-probe")]
#[path = "wave_streaming.rs"]
mod wave;
#[cfg(feature = "wave-streaming-probe")]
pub type WavDecoder = incremental::Decoder<wave::Wave>;
#[cfg(feature = "wave-streaming-probe")]
pub use wave::Wave;

#[cfg(feature = "adts-streaming-probe")]
#[path = "adts_streaming.rs"]
mod adts;
#[cfg(feature = "adts-streaming-probe")]
pub type AdtsDecoder = incremental::Decoder<adts::Adts>;
#[cfg(feature = "adts-streaming-probe")]
pub use adts::Adts;

#[cfg(feature = "vorbis-streaming-probe")]
#[path = "vorbis_streaming.rs"]
mod vorbis;
#[cfg(feature = "vorbis-streaming-probe")]
pub type VorbisDecoder = incremental::Decoder<vorbis::Vorbis>;
#[cfg(feature = "vorbis-streaming-probe")]
pub use vorbis::Vorbis;
