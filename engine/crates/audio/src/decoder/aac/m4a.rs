//! Typed AAC binding. MP4 parsing and source positions belong to the library;
//! the common decoder retains its codec while IO resolves explicit requests.
use super::incremental::{Decoder, PacketStep, Packetizer};
use symphonia_core::{
    codecs::audio::{AudioCodecParameters, AudioDecoderOptions, well_known::CODEC_ID_AAC},
    errors::Result as FormatResult,
    formats::Track,
};
use symphonia_format_isomp4::streaming::AtomSpan;
use symphonia_format_isomp4::streaming::{
    AudioIndex, IndexOptions, IndexRequest, IndexedPacket, PacketStep as Mp4Step,
    Packetizer as Mp4Packetizer, PacketizerOptions,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceRequest {
    Index(IndexRequest),
    ReadAt(IndexedPacket),
}

pub struct M4a {
    packets: Mp4Packetizer,
    request: Option<SourceRequest>,
}

impl M4a {
    fn configured(object_len: Option<u64>) -> Self {
        Self {
            packets: Mp4Packetizer::new(PacketizerOptions {
                index: IndexOptions {
                    object_len,
                    ..Default::default()
                },
                ..Default::default()
            }),
            request: None,
        }
    }
}

impl Packetizer for M4a {
    const MAX_TRAILING_BYTES: usize = 16 * 1024 * 1024;
    type Codec = symphonia_codec_aac::AacDecoder;
    fn new() -> Self {
        Self::configured(None)
    }
    fn track(&self) -> Option<&Track> {
        self.packets.track()
    }

    fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> FormatResult<PacketStep<'a>> {
        Ok(match self.packets.next(input, eof)? {
            Mp4Step::NeedMore => PacketStep::NeedMore,
            Mp4Step::Discard(n) => PacketStep::Discard(n),
            Mp4Step::Packet { consumed, packet } => PacketStep::Packet { consumed, packet },
            Mp4Step::NeedIndex(request) => {
                self.request = Some(SourceRequest::Index(request));
                PacketStep::NeedSource
            }
            Mp4Step::ReadAt(packet) => {
                self.request = Some(SourceRequest::ReadAt(packet));
                PacketStep::NeedSource
            }
            Mp4Step::End => PacketStep::End,
        })
    }

    fn validate_codec(params: &AudioCodecParameters) -> Result<(), String> {
        if params.codec != CODEC_ID_AAC {
            return Err("streaming M4A profile requires AAC-LC".into());
        }
        Ok(())
    }
    fn create_decoder(params: &AudioCodecParameters) -> Result<Self::Codec, String> {
        Self::Codec::try_new(params, &AudioDecoderOptions::default().gapless(true))
            .map_err(|e| format!("decoder: {e}"))
    }
    fn seek(
        &mut self,
        target: symphonia_core::units::Timestamp,
    ) -> FormatResult<symphonia_core::formats::SeekedTo> {
        let seeked = self.packets.seek(target)?;
        self.request = match self.packets.next(&[], false)? {
            Mp4Step::ReadAt(packet) => Some(SourceRequest::ReadAt(packet)),
            _ => {
                return Err(symphonia_core::errors::Error::DecodeError(
                    "M4A seek lost source request",
                ));
            }
        };
        Ok(seeked)
    }
    fn resume_buffered(&mut self, input: &[u8], head: usize) -> FormatResult<Option<usize>> {
        use symphonia_core::errors::Error;
        use symphonia_format_isomp4::streaming::BufferedStep;
        if !matches!(self.request, Some(SourceRequest::ReadAt(_))) {
            return Ok(None);
        }
        let base = self
            .packets
            .position()
            .checked_sub(head as u64)
            .ok_or(Error::DecodeError("M4A cached input coordinates changed"))?;
        match self
            .packets
            .try_buffered(base, input, super::incremental::ENCODED_CAPACITY)?
        {
            BufferedStep::ReadAt { offset } => {
                let head = offset
                    .checked_sub(base)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= input.len())
                    .ok_or(Error::DecodeError("M4A cached cursor outside input"))?;
                self.request = None;
                Ok(Some(head))
            }
            BufferedStep::Index => Err(Error::DecodeError(
                "cached seek replaced its retained index",
            )),
            BufferedStep::Unavailable | BufferedStep::NeedMore | BufferedStep::Yield => Ok(None),
        }
    }
}

impl Decoder<M4a> {
    pub fn with_object_len(cap: Option<u64>, object_len: Option<u64>) -> Self {
        Self::with_packetizer(M4a::configured(object_len), cap)
    }

    pub fn source_request(&self) -> Option<SourceRequest> {
        self.source_packetizer().and_then(|p| p.request)
    }

    /// Supply an index parsed off-worker from the same pinned representation.
    /// The original unconsumed mdat prefix remains in the common input buffer.
    pub fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        length: Option<u64>,
    ) -> Result<(), String> {
        self.install_index(index, length, false)
    }

    /// HTTP can close a stalled initial body after the tail index is installed.
    /// Reuse the library's first-packet seek; IO must still validate actual mdat.
    pub fn install_index_for_range(
        &mut self,
        index: AudioIndex,
        length: u64,
    ) -> Result<(), String> {
        self.install_index(index, Some(length), true)
    }

    fn install_index(
        &mut self,
        index: AudioIndex,
        length: Option<u64>,
        first_packet: bool,
    ) -> Result<(), String> {
        self.resume_source(false, |p| {
            if !matches!(p.request, Some(SourceRequest::Index(_))) {
                return Err("M4A decoder is not awaiting an index".into());
            }
            if let Some(length) = length {
                p.packets
                    .bind_object_len(length)
                    .map_err(|e| e.to_string())?;
            }
            p.packets.install_index(index).map_err(|e| e.to_string())?;
            if first_packet {
                p.packets
                    .seek(symphonia_core::units::Timestamp::ZERO)
                    .map_err(|e| e.to_string())?;
            }
            p.request = None;
            Ok(())
        })
    }

    /// IO must verify the actual mdat, response offset and object version.
    pub fn resume_at(&mut self, media: AtomSpan, offset: u64) -> Result<(), String> {
        self.resume_source(true, |p| {
            if !matches!(p.request, Some(SourceRequest::ReadAt(_))) {
                return Err("M4A decoder is not awaiting a source reposition".into());
            }
            p.packets
                .resume_at(media, offset)
                .map_err(|e| e.to_string())?;
            p.request = None;
            Ok(())
        })
    }
}
