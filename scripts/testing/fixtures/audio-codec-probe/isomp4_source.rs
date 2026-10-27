//! Resolve library indexes and media repositions through one pinned source.
//! Only bounded 8/16-byte atom framing runs inline. Full moov parsing and
//! decoder resume use the existing CPU lane; no waiting CPU worker.
use crate::{
    off_worker::OffWorker,
    push_decoder::{M4aDecoder, M4aSourceDecoder, M4aSourceRequest},
    streaming::{StreamMsg, StreamingState},
    streaming_transport::while_active,
};
use bytes::Bytes;
use migo_services::network::http::range;
use std::sync::Arc;
use symphonia_format_isomp4::streaming::{
    AtomKind, AtomSpan, AudioIndex, IndexOptions, IndexRequest, IndexedPacket,
};
use tokio::{sync::mpsc, time::Instant};

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_atoms: u32,
    pub max_skipped_bytes: u64,
    pub index: IndexOptions,
    pub max_resumes: u32,
    pub media: range::StreamLimits,
    #[cfg(test)]
    pub parse_delay: std::time::Duration,
    #[cfg(test)]
    pub resume_delay: std::time::Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_atoms: 32,
            max_skipped_bytes: 4 * 1024 * 1024,
            index: IndexOptions::default(),
            max_resumes: 64,
            media: range::StreamLimits::default(),
            #[cfg(test)]
            parse_delay: std::time::Duration::ZERO,
            #[cfg(test)]
            resume_delay: std::time::Duration::ZERO,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Range(range::Error),
    Invalid(&'static str),
    UnknownLength,
    Format(String),
    Cpu(shared::error::EngineError),
    Io(shared::error::EngineError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "M4A index source: {self:?}")
    }
}
impl std::error::Error for Error {}

pub struct Resolved<D: M4aSourceDecoder = M4aDecoder> {
    pub decoder: OffWorker<D>,
    #[cfg(test)]
    pub parse_thread: std::thread::ThreadId,
}
struct Parsed {
    index: AudioIndex,
    #[cfg(test)]
    thread: std::thread::ThreadId,
}
fn parse_movie(bytes: &[u8], options: IndexOptions) -> Result<Parsed, Error> {
    let index = AudioIndex::from_moov(bytes, options).map_err(|e| Error::Format(e.to_string()))?;
    Ok(Parsed {
        index,
        #[cfg(test)]
        thread: std::thread::current().id(),
    })
}

/// Own the exact reader while its long-media response is active. Neither
/// identity nor counters are copied when a new source request interrupts it.
enum ReaderOwner {
    Control(range::Reader),
    Media(range::OwnedStream),
    Failed,
}
pub struct HttpReader {
    owner: ReaderOwner,
    media_limits: range::StreamLimits,
    completed_media_requests: u64,
    completed_media_bytes: u64,
}
impl HttpReader {
    fn new(reader: range::Reader, media_limits: range::StreamLimits) -> Self {
        Self {
            owner: ReaderOwner::Control(reader),
            media_limits,
            completed_media_requests: 0,
            completed_media_bytes: 0,
        }
    }
    fn reader(&self) -> Option<&range::Reader> {
        match &self.owner {
            ReaderOwner::Control(reader) => Some(reader),
            ReaderOwner::Media(stream) => Some(stream.reader()),
            ReaderOwner::Failed => None,
        }
    }
    fn length(&self) -> Option<u64> {
        self.reader().and_then(range::Reader::length)
    }
    fn requests(&self) -> u32 {
        self.reader().map_or(0, range::Reader::requests)
    }
    fn bytes_received(&self) -> u64 {
        self.reader().map_or(0, range::Reader::bytes_received)
    }
    fn suspend(&mut self) {
        self.owner = match std::mem::replace(&mut self.owner, ReaderOwner::Failed) {
            ReaderOwner::Media(stream) => {
                self.completed_media_requests = self
                    .completed_media_requests
                    .saturating_add(u64::from(stream.requests()));
                self.completed_media_bytes = self
                    .completed_media_bytes
                    .saturating_add(stream.bytes_received());
                ReaderOwner::Control(stream.into_reader())
            }
            owner => owner,
        };
    }
    async fn read_exact_at(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, range::Error> {
        self.suspend();
        match &mut self.owner {
            ReaderOwner::Control(reader) => reader.read_exact_at(offset, len, deadline).await,
            _ => Err(range::Error::PreviouslyFailed),
        }
    }
    fn start(
        &mut self,
        offset: u64,
        len: u64,
        limits: range::StreamLimits,
    ) -> Result<(), range::Error> {
        self.suspend();
        let ReaderOwner::Control(reader) = std::mem::replace(&mut self.owner, ReaderOwner::Failed)
        else {
            return Err(range::Error::PreviouslyFailed);
        };
        self.owner = ReaderOwner::Media(reader.into_stream_at(offset, len, limits)?);
        Ok(())
    }
    async fn chunk(&mut self) -> Result<Option<Bytes>, range::Error> {
        match &mut self.owner {
            ReaderOwner::Media(stream) => stream.chunk().await,
            _ => Err(range::Error::PreviouslyFailed),
        }
    }
    fn media_requests(&self) -> u64 {
        self.completed_media_requests
            .saturating_add(match &self.owner {
                ReaderOwner::Media(s) => u64::from(s.requests()),
                _ => 0,
            })
    }
    fn media_bytes(&self) -> u64 {
        self.completed_media_bytes
            .saturating_add(match &self.owner {
                ReaderOwner::Media(s) => s.bytes_received(),
                _ => 0,
            })
    }
}

/// Static source boundary shared by HTTP and local positional readers.
pub trait Reader: Send {
    type Chunk: AsRef<[u8]> + Send + 'static;
    fn length(&self) -> Option<u64>;
    fn requests(&self) -> u32;
    fn bytes_received(&self) -> u64;
    fn media_requests(&self) -> u64;
    fn media_bytes(&self) -> u64;
    fn suspend(&mut self);
    fn read_exact_at(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, Error>> + Send;
    fn start(&mut self, offset: u64, len: u64) -> Result<(), Error>;
    fn chunk(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<Self::Chunk>, Error>> + Send;
}
impl Reader for HttpReader {
    type Chunk = Bytes;
    fn length(&self) -> Option<u64> {
        HttpReader::length(self)
    }
    fn requests(&self) -> u32 {
        HttpReader::requests(self)
    }
    fn bytes_received(&self) -> u64 {
        HttpReader::bytes_received(self)
    }
    fn media_requests(&self) -> u64 {
        HttpReader::media_requests(self)
    }
    fn media_bytes(&self) -> u64 {
        HttpReader::media_bytes(self)
    }
    fn suspend(&mut self) {
        HttpReader::suspend(self)
    }
    async fn read_exact_at(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, Error> {
        HttpReader::read_exact_at(self, offset, len, deadline)
            .await
            .map_err(Error::Range)
    }
    fn start(&mut self, offset: u64, len: u64) -> Result<(), Error> {
        HttpReader::start(self, offset, len, self.media_limits).map_err(Error::Range)
    }
    async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        HttpReader::chunk(self).await.map_err(Error::Range)
    }
}

pub struct Repositioned<D: M4aSourceDecoder = M4aDecoder, C = Bytes> {
    pub decoder: OffWorker<D>,
    pub first: C,
    #[cfg(test)]
    pub resume_thread: std::thread::ThreadId,
}
pub enum MediaRead<C = Bytes> {
    Data(C),
    End,
    Cancelled,
}

pub struct IndexSource<R: Reader = HttpReader> {
    reader: R,
    limits: Limits,
    media_spans: Vec<AtomSpan>,
    scan_position: u64,
    discovery_atoms: u32,
    resumes: u32,
}
impl IndexSource<HttpReader> {
    pub fn new(reader: range::Reader, limits: Limits) -> Self {
        Self::with_reader(HttpReader::new(reader, limits.media), limits)
    }
}
impl<R: Reader> IndexSource<R> {
    pub fn with_reader(reader: R, limits: Limits) -> Self {
        Self {
            reader,
            limits,
            media_spans: Vec::new(),
            scan_position: 0,
            discovery_atoms: 0,
            resumes: 0,
        }
    }
    pub fn length(&self) -> Option<u64> {
        self.reader.length()
    }
    pub fn requests(&self) -> u32 {
        self.reader.requests()
    }
    pub fn bytes_received(&self) -> u64 {
        self.reader.bytes_received()
    }

    async fn read(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> Result<Option<Vec<u8>>, Error> {
        let result =
            while_active(tx, state, self.reader.read_exact_at(offset, len, deadline)).await;
        result.transpose()
    }

    /// None means cancellation or receiver exit; the decoder is then dropped.
    /// RangeUnsupported/NoStrongValidator remain typed for the future bounded
    /// fallback owner, rather than being converted into successful stream EOF.
    pub async fn resolve<D: M4aSourceDecoder>(
        &mut self,
        request: IndexRequest,
        mut decoder: OffWorker<D>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> Result<Option<Resolved<D>>, Error> {
        if request.media.kind != AtomKind::MediaData
            || request.next_atom_offset != request.media.end
        {
            return Err(Error::Invalid("invalid library index request"));
        }
        let mut position = request.next_atom_offset.ok_or(Error::Invalid(
            "unsized media has no following index extent",
        ))?;
        let mut atoms = 0_u32;
        let mut skipped = 0_u64;
        loop {
            atoms = atoms
                .checked_add(1)
                .filter(|n| *n <= self.limits.max_atoms)
                .ok_or(Error::Invalid("index atom budget exceeded"))?;
            let Some(mut header) = self.read(position, 8, deadline, tx, state).await? else {
                return Ok(None);
            };
            let mut span = AtomSpan::read(&header, position, self.reader.length())
                .map_err(|e| Error::Format(e.to_string()))?;
            if span.is_none() {
                let Some(extended) = self.read(position, 16, deadline, tx, state).await? else {
                    return Ok(None);
                };
                header = extended;
                span = AtomSpan::read(&header, position, self.reader.length())
                    .map_err(|e| Error::Format(e.to_string()))?;
            }
            let span = span.ok_or(Error::Invalid("incomplete atom header"))?;
            let end = span
                .end
                .filter(|end| *end > position)
                .ok_or(Error::Invalid("unbounded tail atom"))?;
            if span.kind == AtomKind::Fragmented {
                return Err(Error::Invalid(
                    "fragmented MP4 is outside the selected profile",
                ));
            }
            if span.kind == AtomKind::Movie {
                let len = end
                    .checked_sub(position)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= self.limits.index.max_index_bytes)
                    .ok_or(Error::Invalid(
                        "index byte budget exceeded before body read",
                    ))?;
                let Some(bytes) = self.read(position, len, deadline, tx, state).await? else {
                    return Ok(None);
                };
                let length = self.reader.length();
                let options = IndexOptions {
                    object_len: length.or(self.limits.index.object_len),
                    ..self.limits.index
                };
                let active = state.clone();
                #[cfg(test)]
                let parse_delay = self.limits.parse_delay;
                let operation = decoder.with(move |value| {
                    if active.is_cancelled() {
                        return Err(Error::Invalid("cancelled before index parse"));
                    }
                    if value.source_request() != Some(M4aSourceRequest::Index(request)) {
                        return Err(Error::Invalid("pending index request changed"));
                    }
                    #[cfg(test)]
                    std::thread::sleep(parse_delay);
                    if Instant::now() >= deadline {
                        return Err(Error::Range(range::Error::Timeout));
                    }
                    let parsed = parse_movie(&bytes, options)?;
                    if active.is_cancelled() {
                        return Err(Error::Invalid("cancelled before index install"));
                    }
                    value
                        .install_index_with_length(parsed.index, length)
                        .map_err(Error::Format)?;
                    #[cfg(test)]
                    {
                        Ok(parsed.thread)
                    }
                    #[cfg(not(test))]
                    {
                        Ok(())
                    }
                });
                let operation = tokio::time::timeout_at(deadline, operation);
                let Some(result) = while_active(tx, state, operation).await else {
                    return Ok(None);
                };
                let result = result.map_err(|_| Error::Range(range::Error::Timeout))?;
                let (returned, parsed) = result.map_err(Error::Cpu)?;
                #[cfg(test)]
                let parse_thread = parsed?;
                #[cfg(not(test))]
                parsed?;
                decoder = returned;
                self.remember_media(request.media)?;
                return Ok(Some(Resolved {
                    decoder,
                    #[cfg(test)]
                    parse_thread,
                }));
            }
            if span.kind == AtomKind::MediaData {
                self.remember_media(span)?;
            }
            if span.kind != AtomKind::MediaData {
                skipped = skipped
                    .checked_add(end - position)
                    .filter(|n| *n <= self.limits.max_skipped_bytes)
                    .ok_or(Error::Invalid("skipped tail metadata budget exceeded"))?;
            }
            position = end;
        }
    }
    pub fn media_requests(&self) -> u64 {
        self.reader.media_requests()
    }
    pub fn media_bytes_received(&self) -> u64 {
        self.reader.media_bytes()
    }
    pub fn resumes(&self) -> u32 {
        self.resumes
    }

    fn remember_media(&mut self, span: AtomSpan) -> Result<(), Error> {
        if !self.media_spans.contains(&span) {
            if self.media_spans.len() >= self.limits.max_atoms as usize {
                return Err(Error::Invalid("media extent cache budget exceeded"));
            }
            self.media_spans.push(span);
        }
        Ok(())
    }

    async fn find_media(
        &mut self,
        packet: IndexedPacket,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> Result<Option<AtomSpan>, Error> {
        let end = packet
            .offset
            .checked_add(u64::from(packet.size))
            .filter(|end| *end > packet.offset)
            .ok_or(Error::Invalid("invalid requested packet extent"))?;
        if let Some(span) = self.media_spans.iter().find(|span| {
            span.body_offset <= packet.offset && span.end.is_some_and(|limit| end <= limit)
        }) {
            return Ok(Some(*span));
        }
        loop {
            self.discovery_atoms = self
                .discovery_atoms
                .checked_add(1)
                .filter(|count| *count <= self.limits.max_atoms)
                .ok_or(Error::Invalid("media discovery atom budget exceeded"))?;
            let position = self.scan_position;
            if self
                .reader
                .length()
                .is_some_and(|len| len.saturating_sub(position) < 8)
            {
                return Err(Error::Invalid("packet is not in a media atom"));
            }
            let Some(header) = self.read(position, 8, deadline, tx, state).await? else {
                return Ok(None);
            };
            let span = match AtomSpan::read(&header, position, self.reader.length())
                .map_err(|e| Error::Format(e.to_string()))?
            {
                Some(span) => span,
                None => {
                    let Some(header) = self.read(position, 16, deadline, tx, state).await? else {
                        return Ok(None);
                    };
                    AtomSpan::read(&header, position, self.reader.length())
                        .map_err(|e| Error::Format(e.to_string()))?
                        .ok_or(Error::Invalid("incomplete media atom header"))?
                }
            };
            if span.kind == AtomKind::Fragmented {
                return Err(Error::Invalid(
                    "fragmented MP4 is outside the selected profile",
                ));
            }
            let limit = span.end.ok_or(Error::UnknownLength)?;
            self.scan_position = limit;
            if span.kind == AtomKind::MediaData {
                self.remember_media(span)?;
            }
            if span.offset <= packet.offset && packet.offset < limit {
                if span.kind != AtomKind::MediaData
                    || packet.offset < span.body_offset
                    || end > limit
                {
                    return Err(Error::Invalid("packet does not fit actual media extent"));
                }
                return Ok(Some(span));
            }
        }
    }

    /// Validate the new body before clearing prefetch or resuming the decoder.
    /// The first chunk and CPU queue wait share the discovery's absolute deadline;
    /// subsequent media uses independent header/idle limits and no duration cap.
    pub async fn read_at<D: M4aSourceDecoder>(
        &mut self,
        packet: IndexedPacket,
        mut decoder: OffWorker<D>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> Result<Option<Repositioned<D, R::Chunk>>, Error> {
        self.resumes = self
            .resumes
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_resumes)
            .ok_or(Error::Invalid("source reposition budget exceeded"))?;
        self.reader.suspend();
        let Some(media) = self.find_media(packet, tx, state, deadline).await? else {
            return Ok(None);
        };
        // Completing a finite mdat is not proof of a complete container. Resume
        // through the object's actual end, including a physical tail moov.
        let length = self.reader.length().ok_or(Error::UnknownLength)?;
        let len = length
            .checked_sub(packet.offset)
            .filter(|n| *n > 0)
            .ok_or(Error::Invalid("packet outside pinned object"))?;
        self.reader.start(packet.offset, len)?;
        let first_read = tokio::time::timeout_at(deadline, self.reader.chunk());
        let Some(first) = while_active(tx, state, first_read).await else {
            return Ok(None);
        };
        let first = first
            .map_err(|_| Error::Range(range::Error::Timeout))??
            .ok_or(Error::Invalid("empty reposition response"))?;
        let active = state.clone();
        #[cfg(test)]
        let resume_delay = self.limits.resume_delay;
        let resume = decoder.with(move |value| {
            #[cfg(test)]
            std::thread::sleep(resume_delay);
            if active.is_cancelled() {
                return Err(Error::Invalid("cancelled before source resume"));
            }
            if Instant::now() >= deadline {
                return Err(Error::Range(range::Error::Timeout));
            }
            if value.source_request() != Some(M4aSourceRequest::ReadAt(packet)) {
                return Err(Error::Invalid("pending packet request changed"));
            }
            value
                .resume_at(media, packet.offset)
                .map_err(Error::Format)?;
            #[cfg(test)]
            {
                Ok(std::thread::current().id())
            }
            #[cfg(not(test))]
            {
                Ok(())
            }
        });
        let resume_wait = tokio::time::timeout_at(deadline, resume);
        let Some(result) = while_active(tx, state, resume_wait).await else {
            return Ok(None);
        };
        let (returned, resumed) = result
            .map_err(|_| Error::Range(range::Error::Timeout))?
            .map_err(Error::Cpu)?;
        #[cfg(test)]
        let resume_thread = resumed?;
        #[cfg(not(test))]
        resumed?;
        decoder = returned;
        Ok(Some(Repositioned {
            decoder,
            first,
            #[cfg(test)]
            resume_thread,
        }))
    }

    pub async fn media_chunk(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> Result<MediaRead<R::Chunk>, Error> {
        match while_active(tx, state, self.reader.chunk()).await {
            None => Ok(MediaRead::Cancelled),
            Some(Err(error)) => Err(error),
            Some(Ok(Some(bytes))) => Ok(MediaRead::Data(bytes)),
            Some(Ok(None)) => Ok(MediaRead::End),
        }
    }
}
