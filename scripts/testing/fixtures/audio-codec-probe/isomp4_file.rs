//! Anchored immutable-file/package source. IO uses existing Fs/Pack classes;
//! the same IndexSource owns MP4 discovery, CPU parsing and decoder resumes.
use crate::{
    isomp4_source::{self, IndexSource, Limits, MediaRead, Reader},
    off_worker::OffWorker,
    push_decoder::{M4aSourceDecoder, M4aSourceRequest},
    streaming::{RESPONSE_HEADER_TIMEOUT, StreamMsg, StreamingState},
    streaming_transport::{Decoder, Source, SourceRead, SourceResume},
};
use migo_io::{
    pools::IoPools,
    read_at_file::{FILE_CHUNK_BYTES, FileChunk, ReadAtFile},
    read_at_package::ReadAtPackage,
    task::{PoolKind, PriorityClass},
};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{
    fs::File,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{sync::mpsc, time::Instant};
fn error(cause: impl std::fmt::Display) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, cause.to_string())
}
fn check_deadline(deadline: Instant) -> std::io::Result<()> {
    if Instant::now() >= deadline {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "file deadline exceeded",
        ))
    } else {
        Ok(())
    }
}

// A cancelled operation leaves `failed` set, so this flag is never reused
// while its previous job can still run. One allocation per reader, none per IO.
struct ReadActive(Arc<AtomicBool>);
impl Drop for ReadActive {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

enum Backing {
    File(ReadAtFile),
    Package(ReadAtPackage),
}
impl Backing {
    fn len(&self) -> u64 {
        match self {
            Self::File(v) => v.len(),
            Self::Package(v) => v.len(),
        }
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn kind(&self) -> PoolKind {
        match self {
            Self::File(_) => PoolKind::Fs,
            Self::Package(_) => PoolKind::Pack,
        }
    }
    fn media_bytes(&self) -> u64 {
        match self {
            Self::File(_) => FILE_CHUNK_BYTES as u64,
            Self::Package(_) => 0,
        }
    }
    fn read_exact_at(&self, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        match self {
            Self::File(v) => v.read_exact_at(offset, len),
            Self::Package(v) => v.read_exact_at(offset, len),
        }
    }
    fn chunk_at(&self, offset: u64, len: usize) -> std::io::Result<FileChunk> {
        match self {
            Self::File(v) => v.chunk_at(offset, len),
            Self::Package(v) => v.chunk_at(offset, len),
        }
    }
}

pub struct FileReader {
    file: Arc<Backing>,
    pools: IoPools,
    domain: Arc<migo_io::domain::IoDomain>,
    position: u64,
    end: u64,
    max_control_bytes: usize,
    failed: bool,
    read_active: Arc<AtomicBool>,
    #[cfg(test)]
    pub io_reads: Arc<std::sync::atomic::AtomicUsize>,
    requests: u32,
    control_bytes: u64,
    media_reads: u64,
    media_bytes: u64,
    #[cfg(test)]
    pub delay: std::time::Duration,
    #[cfg(test)]
    pub submits: u32,
    #[cfg(test)]
    pub after_io: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl FileReader {
    /// `file` comes from VFS::open_regular_for_read or a private spool owner.
    /// It must remain immutable; metadata checks are not a cryptographic snapshot.
    pub async fn open(
        file: File,
        scheduler: &migo_io::scheduler::IoScheduler,
    ) -> EngineResult<Self> {
        Self::build(
            scheduler,
            Instant::now() + RESPONSE_HEADER_TIMEOUT,
            move || Ok(file),
        )
        .await
    }
    pub async fn open_code(
        vfs: Arc<shared::vfs::VirtualFS>,
        path: String,
        scheduler: &migo_io::scheduler::IoScheduler,
    ) -> EngineResult<Self> {
        if !path.starts_with("/code/") {
            return Err(error("streaming file must be an immutable /code asset"));
        }
        Self::build(
            scheduler,
            Instant::now() + RESPONSE_HEADER_TIMEOUT,
            move || {
                vfs.open_regular_for_read(&path)
                    .map_err(std::io::Error::other)
            },
        )
        .await
    }
    pub async fn open_package(
        pin: shared::vfs::mount::PinnedPackRead,
        scheduler: &migo_io::scheduler::IoScheduler,
    ) -> EngineResult<Self> {
        let pools = scheduler.pools().clone();
        Self::build_source(
            scheduler,
            PoolKind::Pack,
            0,
            Instant::now() + RESPONSE_HEADER_TIMEOUT,
            move || {
                ReadAtPackage::new(pin, &pools, Limits::default().index.max_index_bytes)
                    .map(Backing::Package)
            },
        )
        .await
    }
    async fn build(
        scheduler: &migo_io::scheduler::IoScheduler,
        deadline: Instant,
        open: impl FnOnce() -> std::io::Result<File> + Send + 'static,
    ) -> EngineResult<Self> {
        Self::build_source(
            scheduler,
            PoolKind::Fs,
            FILE_CHUNK_BYTES as u64,
            deadline,
            move || {
                ReadAtFile::new(open()?, Limits::default().index.max_index_bytes).map(Backing::File)
            },
        )
        .await
    }
    async fn build_source(
        scheduler: &migo_io::scheduler::IoScheduler,
        kind: PoolKind,
        setup_bytes: u64,
        deadline: Instant,
        open: impl FnOnce() -> std::io::Result<Backing> + Send + 'static,
    ) -> EngineResult<Self> {
        scheduler.ensure_open().map_err(error)?;
        check_deadline(deadline).map_err(error)?;
        let pools = scheduler.pools().clone();
        let domain = scheduler.domain();
        let check = domain.clone();
        let read_active = Arc::new(AtomicBool::new(true));
        let _operation = ReadActive(read_active.clone());
        let active = read_active.clone();
        let max_control_bytes = Limits::default().index.max_index_bytes;
        let task = pools
            .submit_async_bytes(
                kind,
                PriorityClass::ForegroundAsync,
                setup_bytes,
                move || {
                    if check.is_closed() || !active.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    check_deadline(deadline)?;
                    let file = open()?;
                    if check.is_closed() || !active.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    check_deadline(deadline)?;
                    Ok(file)
                },
            )
            .map_err(error)?;
        let file = tokio::time::timeout_at(deadline, task)
            .await
            .map_err(|_| error("file setup deadline exceeded"))?
            .map_err(error)?
            .map_err(|_| error("file setup worker panicked"))?
            .map_err(error)?;
        if domain.is_closed() {
            return Err(error("file domain closed"));
        }
        check_deadline(deadline).map_err(error)?;
        let end = file.len();
        Ok(Self {
            file: Arc::new(file),
            pools,
            domain,
            position: 0,
            end,
            max_control_bytes,
            failed: false,
            read_active,
            #[cfg(test)]
            io_reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            requests: 0,
            control_bytes: 0,
            media_reads: 0,
            media_bytes: 0,
            #[cfg(test)]
            delay: std::time::Duration::ZERO,
            #[cfg(test)]
            submits: 0,
            #[cfg(test)]
            after_io: None,
        })
    }
    pub fn len(&self) -> u64 {
        self.file.len()
    }
    pub fn is_empty(&self) -> bool {
        self.file.is_empty()
    }
    fn active(&self) -> Result<(), isomp4_source::Error> {
        if self.failed || self.domain.is_closed() {
            Err(isomp4_source::Error::Invalid(
                "file reader previously failed",
            ))
        } else {
            Ok(())
        }
    }
}
impl Reader for FileReader {
    type Chunk = FileChunk;
    fn length(&self) -> Option<u64> {
        Some(self.file.len())
    }
    fn requests(&self) -> u32 {
        self.requests
    }
    fn bytes_received(&self) -> u64 {
        self.control_bytes
    }
    fn media_requests(&self) -> u64 {
        self.media_reads
    }
    fn media_bytes(&self) -> u64 {
        self.media_bytes
    }
    fn suspend(&mut self) {}
    async fn read_exact_at(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, isomp4_source::Error> {
        self.active()?;
        self.failed = true;
        if Instant::now() >= deadline {
            return Err(isomp4_source::Error::Invalid(
                "file control deadline exceeded",
            ));
        }
        if len > self.max_control_bytes
            || offset
                .checked_add(len as u64)
                .is_none_or(|end| end > self.file.len())
        {
            return Err(isomp4_source::Error::Invalid(
                "local control extent exceeds bound",
            ));
        }
        #[cfg(test)]
        {
            self.submits += 1;
        }
        self.read_active.store(true, Ordering::Release);
        let _operation = ReadActive(self.read_active.clone());
        let read_active = self.read_active.clone();
        #[cfg(test)]
        let io_reads = self.io_reads.clone();
        #[cfg(test)]
        let after_io = self.after_io.clone();
        let file = self.file.clone();
        let domain = self.domain.clone();
        #[cfg(test)]
        let delay = self.delay;
        let task = self
            .pools
            .submit_async_bytes(
                self.file.kind(),
                PriorityClass::ForegroundAsync,
                len as u64,
                move || {
                    #[cfg(test)]
                    std::thread::sleep(delay);
                    if domain.is_closed() || !read_active.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    // Do not rely on the receiver polling its timeout promptly.
                    check_deadline(deadline)?;
                    #[cfg(test)]
                    io_reads.fetch_add(1, Ordering::Relaxed);
                    let result = file.read_exact_at(offset, len);
                    #[cfg(test)]
                    if let Some(hook) = after_io {
                        hook();
                    }
                    if domain.is_closed() || !read_active.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    // Disk work cannot be interrupted; reject late completions.
                    check_deadline(deadline)?;
                    result
                },
            )
            .map_err(|e| isomp4_source::Error::Io(error(e)))?;
        let data = tokio::time::timeout_at(deadline, task)
            .await
            .map_err(|_| isomp4_source::Error::Invalid("file control deadline exceeded"))?
            .map_err(|e| isomp4_source::Error::Io(error(e)))?
            .map_err(|_| isomp4_source::Error::Invalid("file control worker panicked"))?
            .map_err(|e| isomp4_source::Error::Io(error(e)))?;
        check_deadline(deadline).map_err(|e| isomp4_source::Error::Io(error(e)))?;
        self.requests += 1;
        self.control_bytes += data.len() as u64;
        self.failed = false;
        Ok(data)
    }
    fn start(&mut self, offset: u64, len: u64) -> Result<(), isomp4_source::Error> {
        self.active()?;
        self.failed = true;
        let end = offset
            .checked_add(len)
            .filter(|end| len > 0 && *end <= self.file.len())
            .ok_or(isomp4_source::Error::Invalid("invalid local media extent"))?;
        self.position = offset;
        self.end = end;
        self.failed = false;
        Ok(())
    }
    async fn chunk(&mut self) -> Result<Option<FileChunk>, isomp4_source::Error> {
        self.active()?;
        self.failed = true;
        let len = (self.end - self.position).min(FILE_CHUNK_BYTES as u64) as usize;
        #[cfg(test)]
        {
            self.submits += 1;
        }
        self.read_active.store(true, Ordering::Release);
        let _operation = ReadActive(self.read_active.clone());
        let read_active = self.read_active.clone();
        #[cfg(test)]
        let io_reads = self.io_reads.clone();
        #[cfg(test)]
        let after_io = self.after_io.clone();
        let file = self.file.clone();
        let domain = self.domain.clone();
        let position = self.position;
        #[cfg(test)]
        let delay = self.delay;
        let task = self
            .pools
            .submit_async_bytes(
                self.file.kind(),
                PriorityClass::ForegroundAsync,
                self.file.media_bytes(),
                move || {
                    #[cfg(test)]
                    std::thread::sleep(delay);
                    if domain.is_closed() || !read_active.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    #[cfg(test)]
                    io_reads.fetch_add(1, Ordering::Relaxed);
                    let result = file.chunk_at(position, len);
                    #[cfg(test)]
                    if let Some(hook) = after_io {
                        hook();
                    }
                    if domain.is_closed() || !read_active.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    result
                },
            )
            .map_err(|e| isomp4_source::Error::Io(error(e)))?;
        let chunk = task
            .await
            .map_err(|e| isomp4_source::Error::Io(error(e)))?
            .map_err(|_| isomp4_source::Error::Invalid("file media worker panicked"))?
            .map_err(|e| isomp4_source::Error::Io(error(e)))?;
        self.position += len as u64;
        self.media_reads += 1;
        self.media_bytes += len as u64;
        self.failed = false;
        Ok((len != 0).then_some(chunk))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn file_setup_timeout_does_not_block_async_worker() {
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap())
            .join("1-tail.m4a");
        let started = Instant::now();
        let result = FileReader::build(&owner, started + Duration::from_millis(20), move || {
            std::thread::sleep(Duration::from_millis(200));
            File::open(path)
        })
        .await;
        assert!(result.is_err());
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "file setup waited for disk"
        );
        tokio::time::sleep(Duration::from_millis(230)).await;
    }
}
