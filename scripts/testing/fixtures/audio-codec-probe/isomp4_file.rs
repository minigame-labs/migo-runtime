//! M4A index adapter over the shared format-independent local source.
pub use crate::local_source::FileReader;
use crate::{
    isomp4_source::{self, IndexSource, Limits, MediaRead, Reader},
    off_worker::OffWorker,
    push_decoder::{M4aSourceDecoder, M4aSourceRequest},
    streaming::{RESPONSE_HEADER_TIMEOUT, StreamMsg, StreamingState},
    streaming_transport::{Decoder, Source, SourceRead, SourceResume},
};
use migo_io::read_at_file::FileChunk;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{fs::File, sync::Arc};
use tokio::{sync::mpsc, time::Instant};
fn error(cause: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, cause.to_string())
}
impl Reader for FileReader {
    type Chunk = FileChunk;
    fn length(&self) -> Option<u64> {
        FileReader::length(self)
    }
    fn requests(&self) -> u32 {
        FileReader::requests(self)
    }
    fn bytes_received(&self) -> u64 {
        FileReader::bytes_received(self)
    }
    fn media_requests(&self) -> u64 {
        FileReader::media_requests(self)
    }
    fn media_bytes(&self) -> u64 {
        FileReader::media_bytes(self)
    }
    fn suspend(&mut self) {
        FileReader::suspend(self)
    }
    async fn read_exact_at(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, isomp4_source::Error> {
        FileReader::read_exact_at(self, offset, len, deadline)
            .await
            .map_err(isomp4_source::Error::Io)
    }
    fn start(&mut self, offset: u64, len: u64) -> Result<(), isomp4_source::Error> {
        FileReader::start(self, offset, len).map_err(isomp4_source::Error::Io)
    }
    async fn chunk(&mut self) -> Result<Option<FileChunk>, isomp4_source::Error> {
        FileReader::chunk(self)
            .await
            .map_err(isomp4_source::Error::Io)
    }
}
pub struct IndexedFile {
    source: IndexSource<FileReader>,
    pending: Option<FileChunk>,
}
impl IndexedFile {
    pub async fn open(
        file: File,
        scheduler: &migo_io::scheduler::IoScheduler,
    ) -> EngineResult<Self> {
        let reader = FileReader::open(file, scheduler).await?;
        Ok(Self::from_reader(reader))
    }
    pub async fn open_code(
        vfs: Arc<shared::vfs::VirtualFS>,
        path: String,
        scheduler: &migo_io::scheduler::IoScheduler,
    ) -> EngineResult<Self> {
        Ok(Self::from_reader(
            FileReader::open_code(vfs, path, scheduler).await?,
        ))
    }
    pub fn from_reader(reader: FileReader) -> Self {
        let mut limits = Limits::default();
        limits.index.max_index_bytes = limits
            .index
            .max_index_bytes
            .min(crate::local_source::MAX_CONTROL_BYTES);
        limits.index.object_len = Some(reader.len());
        Self {
            source: IndexSource::with_reader(reader, limits),
            pending: None,
        }
    }
    pub fn length(&self) -> u64 {
        self.source.length().expect("local length is known")
    }
}
impl<D: Decoder + M4aSourceDecoder> Source<D> for IndexedFile {
    type Request = M4aSourceRequest;
    fn pending(decoder: &D) -> EngineResult<Self::Request> {
        decoder
            .source_request()
            .ok_or_else(|| error("local source command missing"))
    }
    async fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        if let Some(chunk) = self.pending.take() {
            return Ok(SourceRead::Data(chunk));
        }
        match self.source.media_chunk(tx, state).await.map_err(error)? {
            MediaRead::Data(chunk) => Ok(SourceRead::Data(chunk)),
            MediaRead::End => Ok(SourceRead::End),
            MediaRead::Cancelled => Ok(SourceRead::Cancelled),
        }
    }
    async fn resume(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<D>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<D>>> {
        let deadline = Instant::now() + RESPONSE_HEADER_TIMEOUT;
        match request {
            M4aSourceRequest::Index(index) => {
                let Some(result) = self
                    .source
                    .resolve(index, decoder, tx, state, deadline)
                    .await
                    .map_err(error)?
                else {
                    return Ok(None);
                };
                Ok(Some(SourceResume {
                    decoder: result.decoder,
                    restart: false,
                }))
            }
            M4aSourceRequest::ReadAt(packet) => {
                let Some(result) = self
                    .source
                    .read_at(packet, decoder, tx, state, deadline)
                    .await
                    .map_err(error)?
                else {
                    return Ok(None);
                };
                self.pending = Some(result.first);
                Ok(Some(SourceResume {
                    decoder: result.decoder,
                    restart: true,
                }))
            }
        }
    }
}
