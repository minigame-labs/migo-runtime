//! Resolve the library's pending tail index through the shared pinned source.
//! Only bounded 8/16-byte atom framing runs inline. Full moov parsing and
//! installation run in the decoder's existing CPU lane; no waiting CPU worker.
use crate::{
    off_worker::OffWorker,
    push_decoder::{M4aDecoder, M4aSourceRequest},
    streaming::{StreamMsg, StreamingState},
    streaming_transport::while_active,
};
use migo_services::network::http::range;
use std::sync::Arc;
use symphonia_format_isomp4::streaming::{
    AtomKind, AtomSpan, AudioIndex, IndexOptions, IndexRequest,
};
use tokio::{sync::mpsc, time::Instant};

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_atoms: u32,
    pub max_skipped_bytes: u64,
    pub index: IndexOptions,
    #[cfg(test)]
    pub parse_delay: std::time::Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_atoms: 32,
            max_skipped_bytes: 4 * 1024 * 1024,
            index: IndexOptions::default(),
            #[cfg(test)]
            parse_delay: std::time::Duration::ZERO,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Range(range::Error),
    Invalid(&'static str),
    Format(String),
    Cpu(shared::error::EngineError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "M4A index source: {self:?}")
    }
}
impl std::error::Error for Error {}

pub struct Resolved {
    pub decoder: OffWorker<M4aDecoder>,
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

pub struct IndexSource {
    reader: range::Reader,
    limits: Limits,
}
impl IndexSource {
    pub fn new(reader: range::Reader, limits: Limits) -> Self {
        Self { reader, limits }
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
        result.transpose().map_err(Error::Range)
    }

    /// None means cancellation or receiver exit; the decoder is then dropped.
    /// RangeUnsupported/NoStrongValidator remain typed for the future bounded
    /// fallback owner, rather than being converted into successful stream EOF.
    pub async fn resolve(
        &mut self,
        request: IndexRequest,
        mut decoder: OffWorker<M4aDecoder>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
        deadline: Instant,
    ) -> Result<Option<Resolved>, Error> {
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
                return Ok(Some(Resolved {
                    decoder,
                    #[cfg(test)]
                    parse_thread,
                }));
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
}
