//! Sequential packet delivery; IO owns external index/range acquisition.
use super::{validate_file_type, AtomKind, AtomSpan, AudioIndex, IndexOptions, IndexedPacket};
use symphonia_core::{
    errors::{decode_error, unsupported_error, Error, Result},
    formats::{SeekedTo, Track},
    units::Timestamp,
    packet::PacketRef,
};

mod buffered;
pub use buffered::BufferedStep;
use buffered::Probe;

const DISCARD_STEP: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub struct PacketizerOptions {
    pub index: IndexOptions,
    pub max_skipped_metadata_bytes: u64,
}
impl Default for PacketizerOptions {
    fn default() -> Self {
        Self {
            index: IndexOptions::default(),
            max_skipped_metadata_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexRequest {
    pub media: AtomSpan,
    pub next_atom_offset: Option<u64>,
}

pub enum PacketStep<'a> {
    NeedMore,
    Discard(usize),
    Packet {
        consumed: usize,
        packet: PacketRef<'a>,
    },
    NeedIndex(IndexRequest),
    ReadAt(IndexedPacket),
    End,
}

#[derive(Clone, Copy)]
enum Stage {
    Header,
    Collect { kind: AtomKind, total: usize },
    Skip { end: u64 },
    Media { span: AtomSpan },
    AwaitIndex(IndexRequest),
    AwaitSeek { media: Option<AtomSpan> },
    Finished,
}

pub struct Packetizer {
    options: PacketizerOptions,
    stage: Stage,
    position: u64,
    index: Option<AudioIndex>,
    pending: Option<IndexedPacket>,
    audio_done: bool,
    file_type_seen: bool,
    movie_seen: bool,
    external_index: bool,
    scratch: Vec<u8>,
    skipped_metadata: u64,
    failed: bool,
    buffered_probe: Option<Probe>,
    movie_span: Option<(u64, u64)>,
    seek_traversal: bool,
}

impl Packetizer {
    pub fn new(options: PacketizerOptions) -> Self {
        Self {
            options,
            stage: Stage::Header,
            position: 0,
            index: None,
            pending: None,
            audio_done: false,
            file_type_seen: false,
            movie_seen: false,
            external_index: false,
            scratch: Vec::new(),
            skipped_metadata: 0,
            failed: false,
            buffered_probe: None,
            movie_span: None,
            seek_traversal: false,
        }
    }

    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn track(&self) -> Option<&Track> {
        self.index.as_ref().map(AudioIndex::track)
    }

    /// Seek through the retained index. Packet bytes remain unavailable until
    /// the existing ReadAt request is resolved with a verified mdat/window.
    /// Codec reset and discarding preroll before required_ts belong to callers.
    /// Recoverable target errors leave the current request and input position.
    pub fn seek(&mut self, ts: Timestamp) -> Result<SeekedTo> {
        if self.failed {
            return decode_error("isomp4: streaming packetizer previously failed");
        }
        let result = self.seek_inner(ts);
        self.failed = result.as_ref().is_err_and(|error| !matches!(error, Error::SeekError(_)));
        result
    }

    fn seek_inner(&mut self, ts: Timestamp) -> Result<SeekedTo> {
        let index = self.index.as_mut().ok_or(Error::SeekError(
            symphonia_core::errors::SeekErrorKind::Unseekable,
        ))?;
        let seeked = index.seek(ts)?;
        let packet = index.next_packet()?.ok_or(Error::DecodeError(
            "isomp4: seek index returned no preroll packet",
        ))?;
        let media = match self.stage {
            Stage::Media { span } => Some(span),
            Stage::AwaitSeek { media } => media,
            _ => None,
        };
        self.pending = Some(packet);
        self.audio_done = false;
        self.buffered_probe = None;
        self.skipped_metadata = 0;
        self.seek_traversal = true;
        self.stage = Stage::AwaitSeek { media };
        Ok(seeked)
    }

    /// IO supplies an index obtained for the very same pinned representation.
    /// No encoded input was consumed by NeedIndex; retry its original prefix.
    pub fn install_index(&mut self, index: AudioIndex) -> Result<()> {
        let valid = !self.failed
            && matches!(self.stage, Stage::AwaitIndex(_))
            && index.options.max_index_bytes <= self.options.index.max_index_bytes
            && index.options.max_packet_bytes <= self.options.index.max_packet_bytes
            && index.options.object_len == self.options.index.object_len;
        if !valid {
            self.failed = true;
            return decode_error("isomp4: invalid external streaming index");
        }
        self.buffered_probe = None;
        self.index = Some(index);
        self.external_index = true;
        self.stage = Stage::Header;
        Ok(())
    }

    /// Bind a length learned from the same pinned representation. Only an
    /// unresolved tail-index request may learn a previously unknown length;
    /// this does not consume input, install an index or resume packet delivery.
    pub fn bind_object_len(&mut self, len: u64) -> Result<()> {
        let valid = !self.failed
            && self
                .options
                .index
                .object_len
                .is_none_or(|known| known == len)
            && self.position <= len
            && match self.stage {
                Stage::AwaitIndex(request) => {
                    request.media.body_offset <= len
                        && request.media.end.is_some_and(|end| end <= len)
                }
                _ => false,
            };
        if !valid {
            self.failed = true;
            return decode_error("isomp4: invalid pinned object length");
        }
        self.options.index.object_len = Some(len);
        Ok(())
    }

    /// Resume at ReadAt.offset only after IO verifies the actual response
    /// position, mdat extent and representation identity. Never resets AAC.
    pub fn resume_at(&mut self, media: AtomSpan, offset: u64) -> Result<()> {
        let valid = !self.failed
            && matches!(self.stage, Stage::Media { .. } | Stage::AwaitSeek { .. })
            && media.kind == AtomKind::MediaData
            && matches!(media.header_len, 8 | 16)
            && media.offset.checked_add(media.header_len as u64) == Some(media.body_offset)
            && media.body_offset <= offset
            && media.end.is_none_or(|end| end >= media.body_offset)
            && self
                .options
                .index
                .object_len
                .is_none_or(|len| media.end.is_some_and(|end| end <= len))
            && self.pending.is_some_and(|packet| {
                packet.offset == offset
                    && offset
                        .checked_add(u64::from(packet.size))
                        .is_some_and(|end| {
                            media.end.is_none_or(|limit| end <= limit)
                                && self
                                    .options
                                    .index
                                    .object_len
                                    .is_none_or(|limit| end <= limit)
                        })
            });
        if !valid {
            self.failed = true;
            return decode_error("isomp4: invalid streaming range resume");
        }
        self.buffered_probe = None;
        self.position = offset;
        self.stage = Stage::Media { span: media };
        Ok(())
    }

    pub fn next<'a>(&mut self, input: &'a [u8], eof: bool) -> Result<PacketStep<'a>> {
        if self.failed {
            return decode_error("isomp4: streaming packetizer previously failed");
        }
        let result = self.next_inner(input, eof);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn advance(&mut self, bytes: usize) -> Result<()> {
        self.position = self
            .position
            .checked_add(bytes as u64)
            .ok_or(Error::DecodeError(
                "isomp4: streaming byte position overflow",
            ))?;
        Ok(())
    }

    fn need_more<'a>(eof: bool) -> Result<PacketStep<'a>> {
        if eof {
            decode_error("isomp4: truncated streaming atom or sample")
        } else {
            Ok(PacketStep::NeedMore)
        }
    }

    fn pending(&mut self) -> Result<Option<IndexedPacket>> {
        if self.pending.is_none() && !self.audio_done {
            self.pending = self
                .index
                .as_mut()
                .ok_or(Error::DecodeError("isomp4: streaming index missing"))?
                .next_packet()?;
            self.audio_done = self.pending.is_none();
        }
        Ok(self.pending)
    }

    fn skip<'a>(&mut self, remaining: u64, input: &[u8], eof: bool) -> Result<PacketStep<'a>> {
        let n = remaining.min(input.len() as u64).min(DISCARD_STEP as u64) as usize;
        if n == 0 {
            return Self::need_more(eof);
        }
        self.advance(n)?;
        Ok(PacketStep::Discard(n))
    }

    fn collect(&mut self, span: AtomSpan, input: &[u8]) -> Result<PacketStep<'static>> {
        let cap = if span.kind == AtomKind::FileType {
            64 * 1024
        } else {
            self.options.index.max_index_bytes
        };
        let total = span
            .end
            .and_then(|end| end.checked_sub(self.position))
            .filter(|n| *n <= cap as u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(Error::DecodeError(
                "isomp4: streaming index exceeds configured byte budget",
            ))?;
        if span.kind == AtomKind::Movie {
            self.movie_span = Some((span.offset, span.end.expect("bounded movie atom")));
        }
        self.scratch.clear();
        self.scratch
            .try_reserve_exact(total)
            .map_err(|_| Error::DecodeError("isomp4: streaming index allocation failed"))?;
        self.scratch.extend_from_slice(&input[..span.header_len]);
        self.stage = Stage::Collect {
            kind: span.kind,
            total,
        };
        self.advance(span.header_len)?;
        Ok(PacketStep::Discard(span.header_len))
    }

    fn next_inner<'a>(&mut self, input: &'a [u8], eof: bool) -> Result<PacketStep<'a>> {
        loop {
            match self.stage {
                Stage::Finished => return Ok(PacketStep::End),
                Stage::AwaitIndex(request) => return Ok(PacketStep::NeedIndex(request)),
                Stage::AwaitSeek { .. } => {
                    return self.pending.ok_or(Error::DecodeError(
                        "isomp4: seek lost pending packet",
                    )).map(PacketStep::ReadAt);
                }
                Stage::Collect { kind, total } => {
                    let remaining = total - self.scratch.len();
                    if remaining == 0 {
                        match kind {
                            AtomKind::FileType => {
                                validate_file_type(&self.scratch)?;
                                self.file_type_seen = true;
                            }
                            AtomKind::Movie => {
                                self.index =
                                    Some(AudioIndex::from_moov(&self.scratch, self.options.index)?);
                                self.movie_seen = true;
                            }
                            _ => unreachable!(),
                        }
                        // The parsed index owns only tables/configuration, not
                        // the encoded atom; release this cold buffer at startup.
                        self.scratch = Vec::new();
                        self.stage = Stage::Header;
                        continue;
                    }
                    let n = remaining.min(input.len()).min(DISCARD_STEP);
                    if n == 0 {
                        return Self::need_more(eof);
                    }
                    self.scratch.extend_from_slice(&input[..n]);
                    self.advance(n)?;
                    return Ok(PacketStep::Discard(n));
                }
                Stage::Skip { end } => {
                    if self.position == end {
                        self.stage = Stage::Header;
                        continue;
                    }
                    return self.skip(end - self.position, input, eof);
                }
                Stage::Header => {
                    if input.is_empty() && eof {
                        if !self.file_type_seen
                            || !self.movie_seen
                            || self.index.is_none()
                            || self.pending()?.is_some()
                            || self
                                .options
                                .index
                                .object_len
                                .is_some_and(|len| len != self.position)
                        {
                            return decode_error("isomp4: incomplete streaming presentation");
                        }
                        self.stage = Stage::Finished;
                        return Ok(PacketStep::End);
                    }
                    let Some(span) =
                        AtomSpan::read(input, self.position, self.options.index.object_len)?
                    else {
                        return Self::need_more(eof);
                    };
                    match span.kind {
                        AtomKind::FileType => {
                            if self.file_type_seen {
                                return decode_error("isomp4: duplicate streaming file type");
                            }
                            return self.collect(span, input);
                        }
                        AtomKind::Movie if self.index.is_none() => {
                            return self.collect(span, input);
                        }
                        AtomKind::Movie => {
                            let replay = self.seek_traversal && span.end.is_some_and(|end| {
                                self.movie_span == Some((span.offset, end))
                            });
                            if (!self.external_index || self.movie_seen) && !replay {
                                return decode_error("isomp4: duplicate streaming movie");
                            }
                            let end = span
                                .end
                                .filter(|end| {
                                    end - self.position <= self.options.index.max_index_bytes as u64
                                })
                                .ok_or(Error::DecodeError(
                                    "isomp4: streaming movie exceeds byte budget",
                                ))?;
                            self.movie_span = Some((span.offset, end));
                            self.movie_seen = true;
                            self.stage = Stage::Skip { end };
                        }
                        AtomKind::Fragmented => {
                            return unsupported_error(
                                "isomp4: fragmented streaming is unsupported",
                            );
                        }
                        AtomKind::MediaData => {
                            if !self.file_type_seen {
                                return decode_error("isomp4: streaming media before file type");
                            }
                            if self.index.is_none() {
                                let request = IndexRequest {
                                    media: span,
                                    next_atom_offset: span.end,
                                };
                                self.stage = Stage::AwaitIndex(request);
                                return Ok(PacketStep::NeedIndex(request));
                            }
                            self.stage = Stage::Media { span };
                        }
                        _ => {
                            let end = span.end.ok_or(Error::DecodeError(
                                "isomp4: unsized optional streaming atom",
                            ))?;
                            self.skipped_metadata = self
                                .skipped_metadata
                                .checked_add(end - self.position)
                                .filter(|n| *n <= self.options.max_skipped_metadata_bytes)
                                .ok_or(Error::DecodeError(
                                    "isomp4: skipped streaming metadata exceeds budget",
                                ))?;
                            self.stage = Stage::Skip { end };
                        }
                    }
                    self.advance(span.header_len)?;
                    return Ok(PacketStep::Discard(span.header_len));
                }
                Stage::Media { span } => {
                    let pending = self.pending()?;
                    if let Some(packet) = pending {
                        if packet.offset < self.position {
                            return Ok(PacketStep::ReadAt(packet));
                        }
                    }
                    if span.end == Some(self.position) {
                        self.stage = Stage::Header;
                        continue;
                    }
                    if let Some(packet) = pending {
                        if span.end.is_some_and(|end| packet.offset >= end) {
                            return self.skip(span.end.unwrap() - self.position, input, eof);
                        }
                        let end = packet
                            .offset
                            .checked_add(u64::from(packet.size))
                            .ok_or(Error::DecodeError("isomp4: streaming sample end overflow"))?;
                        if span.end.is_some_and(|limit| end > limit) {
                            return decode_error("isomp4: streaming sample crosses media extent");
                        }
                        if packet.offset > self.position {
                            return self.skip(packet.offset - self.position, input, eof);
                        }
                        if input.len() < packet.size as usize {
                            return Self::need_more(eof);
                        }
                        let borrowed = packet.packet(&input[..packet.size as usize])?;
                        self.advance(packet.size as usize)?;
                        self.pending = None;
                        return Ok(PacketStep::Packet {
                            consumed: packet.size as usize,
                            packet: borrowed,
                        });
                    }
                    if let Some(end) = span.end {
                        return self.skip(end - self.position, input, eof);
                    }
                    if input.is_empty() && eof {
                        self.stage = Stage::Finished;
                        return Ok(PacketStep::End);
                    }
                    return self.skip(u64::MAX, input, eof);
                }
            }
        }
    }
}
