//! Typed Vorbis binding; Ogg page/packet/timestamp/assembly stay in the library.
use crate::decoder::aac::incremental::{PacketStep, Packetizer};
use shared::error::EngineResult;
use symphonia_core::{
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
    // One metadata/page candidate per original CPU step, including tail CRC.
    const DISCARDS_PER_STEP: usize = 1;
    const SEEK_FROM_START: bool = true;
    fn new() -> Self {
        Self(VorbisPacketizer::new(PacketizerOptions::default()))
    }
    fn track(&self) -> Option<&Track> {
        self.0.track()
    }
    fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> FormatResult<PacketStep<'a>> {
        let step = self.0.next(input, eof)?;
        if self.0.source_request().is_some() {
            return Ok(PacketStep::NeedSource);
        }
        Ok(match step {
            OggStep::NeedMore => PacketStep::NeedMore,
            OggStep::Discard(n) => PacketStep::Discard(n),
            OggStep::Packet { consumed } => PacketStep::BufferedPacket { consumed },
            OggStep::End => PacketStep::End,
        })
    }
    fn seek(
        &mut self,
        target: symphonia_core::units::Timestamp,
    ) -> FormatResult<symphonia_core::formats::SeekedTo> {
        self.0.seek(target)
    }
    fn resume_buffered(&mut self, input: &[u8], head: usize) -> FormatResult<Option<usize>> {
        let Some(offset) = self.0.source_request() else {
            return Ok(None);
        };
        // A completed small source still occupies the original input Vec.
        // Larger sources reopen the same sealed File through the common driver.
        let Some(at) = self
            .0
            .position()
            .checked_sub(head as u64)
            .and_then(|base| offset.checked_sub(base))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < input.len())
        else {
            return Ok(None);
        };
        self.0.resume_at(offset)?;
        Ok(Some(at))
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

impl super::packet::PacketStream<Vorbis> {
    pub(crate) fn vorbis(target: u32, length: u64) -> Self {
        let packets = Vorbis(VorbisPacketizer::with_object_len(
            PacketizerOptions::default(),
            length,
        ));
        let mut decoder = Self::with_decoder(
            target,
            crate::decoder::aac::incremental::Decoder::with_packetizer(packets, None),
        );
        decoder.retain();
        decoder
    }
    pub(crate) fn vorbis_request(&self) -> Option<u64> {
        self.owner
            .as_ref()?
            .decoder
            .source_packetizer()?
            .0
            .source_request()
    }
    pub(crate) fn resume_vorbis(&mut self, offset: u64) -> EngineResult<()> {
        let owner = self
            .owner
            .as_mut()
            .ok_or_else(|| super::invalid("Vorbis decoder retired"))?;
        owner
            .decoder
            .resume_source(true, |p| p.0.resume_at(offset).map_err(|e| e.to_string()))
            .map_err(super::invalid)?;
        owner.input_ended = false;
        Ok(())
    }
    pub(crate) fn reposition_vorbis(
        &mut self,
        target: i64,
        cutover: Option<crate::stream_control::LoopRequest>,
    ) -> FormatResult<(Option<u64>, u64, bool)> {
        let (start, terminal) = self.reposition_window(target, cutover)?;
        Ok((
            if terminal {
                None
            } else {
                self.vorbis_request()
            },
            start,
            terminal,
        ))
    }
}

#[cfg(test)]
#[path = "../streaming_vorbis_control_tests.rs"]
pub(crate) mod tests;
