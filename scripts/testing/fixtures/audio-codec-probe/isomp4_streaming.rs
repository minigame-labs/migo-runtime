//! Typed AAC binding. MP4 parsing and source positions belong to the library;
//! the common decoder retains its codec while IO resolves explicit requests.
use super::incremental::{BufferedSource, Decoder, ENCODED_CAPACITY, PacketStep, Packetizer};
use symphonia::core::{
    codecs::audio::{
        AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_AAC,
    },
    errors::Result as FormatResult,
    formats::Track,
};
pub use symphonia_format_isomp4::streaming::AtomSpan;
use symphonia_format_isomp4::streaming::{
    AudioIndex, BufferedStep, IndexOptions, IndexRequest, IndexedPacket, PacketStep as Mp4Step,
    Packetizer as Mp4Packetizer, PacketizerOptions,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceRequest {
    Index(IndexRequest),
    ReadAt(IndexedPacket),
}

pub struct M4a<const BUFFERED: bool = false> {
    packets: Mp4Packetizer,
    request: Option<SourceRequest>,
}

impl<const BUFFERED: bool> M4a<BUFFERED> {
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

impl<const BUFFERED: bool> Packetizer for M4a<BUFFERED> {
    const RETAIN_ENCODED_HISTORY: bool = BUFFERED;
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
    fn resume_buffered(
        &mut self,
        input: &[u8],
        head: usize,
        eof: bool,
    ) -> FormatResult<BufferedSource> {
        if !BUFFERED {
            return Ok(BufferedSource::Unavailable);
        }
        let base = self.packets.position().checked_sub(head as u64).ok_or(
            symphonia::core::errors::Error::DecodeError("invalid buffered source base"),
        )?;
        let resumed = self.packets.try_buffered(base, input, ENCODED_CAPACITY)?;
        match resumed {
            BufferedStep::Unavailable => Ok(BufferedSource::Unavailable),
            BufferedStep::NeedMore if eof => Err(symphonia::core::errors::Error::DecodeError(
                "incomplete buffered source at EOF",
            )),
            BufferedStep::NeedMore if input.len() == ENCODED_CAPACITY && head == 0 => {
                Ok(BufferedSource::Unavailable)
            }
            BufferedStep::NeedMore => Ok(BufferedSource::NeedInput),
            BufferedStep::Yield => Ok(BufferedSource::Yield),
            BufferedStep::Index => {
                self.request = None;
                Ok(BufferedSource::Resume(head))
            }
            BufferedStep::ReadAt { offset } => {
                let head = usize::try_from(offset.checked_sub(base).ok_or(
                    symphonia::core::errors::Error::DecodeError(
                        "buffered source rewind before input",
                    ),
                )?)
                .map_err(|_| {
                    symphonia::core::errors::Error::DecodeError("buffered cursor not addressable")
                })?;
                self.request = None;
                Ok(BufferedSource::Resume(head))
            }
        }
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
}

impl<const BUFFERED: bool> Decoder<M4a<BUFFERED>> {
    pub fn with_object_len(cap: Option<u64>, object_len: Option<u64>) -> Self {
        Self::with_packetizer(M4a::configured(object_len), cap)
    }

    pub fn source_request(&self) -> Option<SourceRequest> {
        self.source_packetizer().and_then(|p| p.request)
    }

    /// Supply an index parsed off-worker from the same pinned representation.
    /// The original unconsumed mdat prefix remains in the common input buffer.
    pub fn install_index(&mut self, index: AudioIndex) -> Result<(), String> {
        self.install_index_with_length(index, None)
    }

    pub fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        length: Option<u64>,
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

/// Source operations are also implementable by a DSP wrapper without moving
/// parsing, codec state or IO into that wrapper.
pub trait M4aSourceDecoder: Send + 'static {
    fn source_request(&self) -> Option<SourceRequest>;
    fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        length: Option<u64>,
    ) -> Result<(), String>;
    fn resume_at(&mut self, media: AtomSpan, offset: u64) -> Result<(), String>;
}
impl<const BUFFERED: bool> M4aSourceDecoder for Decoder<M4a<BUFFERED>> {
    fn source_request(&self) -> Option<SourceRequest> {
        Decoder::<M4a<BUFFERED>>::source_request(self)
    }
    fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        length: Option<u64>,
    ) -> Result<(), String> {
        Decoder::<M4a<BUFFERED>>::install_index_with_length(self, index, length)
    }
    fn resume_at(&mut self, media: AtomSpan, offset: u64) -> Result<(), String> {
        Decoder::<M4a<BUFFERED>>::resume_at(self, media, offset)
    }
}

// The transport creates the decoder in its first CPU step. Resolving a source
// request keeps that same optional owner, without a second initialization job.
impl<D: M4aSourceDecoder> M4aSourceDecoder for Option<D> {
    fn source_request(&self) -> Option<SourceRequest> {
        self.as_ref().and_then(M4aSourceDecoder::source_request)
    }
    fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        length: Option<u64>,
    ) -> Result<(), String> {
        self.as_mut()
            .ok_or_else(|| "source request before decoder initialization".to_owned())?
            .install_index_with_length(index, length)
    }
    fn resume_at(&mut self, media: AtomSpan, offset: u64) -> Result<(), String> {
        self.as_mut()
            .ok_or_else(|| "source request before decoder initialization".to_owned())?
            .resume_at(media, offset)
    }
}
