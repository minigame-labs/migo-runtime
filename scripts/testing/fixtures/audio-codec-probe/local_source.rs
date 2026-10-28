//! Format-independent anchored file/directory/package source.
//! Fs/Pack IO and one shared CPU/PCM publication loop serve every codec.
use crate::{
    off_worker::OffWorker,
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
/// Per-control allocation bound; sequential codecs do not issue control reads.
pub const MAX_CONTROL_BYTES: usize = 2 * 1024 * 1024;

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
    Spool(migo_io::spool::SealedSpool),
}
impl Backing {
    fn len(&self) -> u64 {
        match self {
            Self::File(v) => v.len(),
            Self::Package(v) => v.len(),
            Self::Spool(v) => v.reader().len(),
        }
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn kind(&self) -> PoolKind {
        match self {
            Self::File(_) => PoolKind::Fs,
            Self::Spool(_) => PoolKind::Fs,
            Self::Package(_) => PoolKind::Pack,
        }
    }
    fn media_bytes(&self) -> u64 {
        match self {
            Self::File(_) => FILE_CHUNK_BYTES as u64,
            Self::Spool(_) => FILE_CHUNK_BYTES as u64,
            Self::Package(_) => 0,
        }
    }
    fn read_exact_at(&self, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        match self {
            Self::File(v) => v.read_exact_at(offset, len),
            Self::Package(v) => v.read_exact_at(offset, len),
            Self::Spool(v) => v.reader().read_exact_at(offset, len),
        }
    }
    fn chunk_at(&self, offset: u64, len: usize) -> std::io::Result<FileChunk> {
        match self {
            Self::File(v) => v.chunk_at(offset, len),
            Self::Package(v) => v.chunk_at(offset, len),
            Self::Spool(v) => v.reader().chunk_at(offset, len),
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
    #[cfg(test)]
    pub chunk_bytes: usize,
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
    /// Sealing already captured metadata on Fs. Admission only moves the owner;
    /// no second disk operation, worker hop or transient media credit is needed.
    pub fn open_spool(
        spool: migo_io::spool::SealedSpool,
        scheduler: &migo_io::scheduler::IoScheduler,
        deadline: Instant,
    ) -> EngineResult<Self> {
        scheduler.ensure_open().map_err(error)?;
        check_deadline(deadline).map_err(error)?;
        Ok(Self::from_backing(
            Backing::Spool(spool),
            scheduler.pools().clone(),
            scheduler.domain(),
            Arc::new(AtomicBool::new(false)),
        ))
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
    pub async fn open_directory(
        pin: shared::vfs::mount::PinnedDirectoryRead,
        scheduler: &migo_io::scheduler::IoScheduler,
    ) -> EngineResult<Self> {
        Self::build(
            scheduler,
            Instant::now() + RESPONSE_HEADER_TIMEOUT,
            move || pin.open(),
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
            move || ReadAtPackage::new(pin, &pools, MAX_CONTROL_BYTES).map(Backing::Package),
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
            move || ReadAtFile::new(open()?, MAX_CONTROL_BYTES).map(Backing::File),
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
        Ok(Self::from_backing(file, pools, domain, read_active))
    }
    fn from_backing(
        file: Backing,
        pools: IoPools,
        domain: Arc<migo_io::domain::IoDomain>,
        read_active: Arc<AtomicBool>,
    ) -> Self {
        let end = file.len();
        Self {
            file: Arc::new(file),
            pools,
            domain,
            position: 0,
            end,
            max_control_bytes: MAX_CONTROL_BYTES,
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
            #[cfg(test)]
            chunk_bytes: FILE_CHUNK_BYTES,
        }
    }
    pub fn len(&self) -> u64 {
        self.file.len()
    }
    pub fn is_empty(&self) -> bool {
        self.file.is_empty()
    }
    fn active(&self) -> Result<(), EngineError> {
        if self.failed || self.domain.is_closed() {
            Err(error("file reader previously failed"))
        } else {
            Ok(())
        }
    }
    pub fn length(&self) -> Option<u64> {
        Some(self.file.len())
    }
    pub fn requests(&self) -> u32 {
        self.requests
    }
    pub fn bytes_received(&self) -> u64 {
        self.control_bytes
    }
    pub fn media_requests(&self) -> u64 {
        self.media_reads
    }
    pub fn media_bytes(&self) -> u64 {
        self.media_bytes
    }
    pub fn suspend(&mut self) {}
    pub async fn read_exact_at(
        &mut self,
        offset: u64,
        len: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, EngineError> {
        self.active()?;
        self.failed = true;
        if Instant::now() >= deadline {
            return Err(error("file control deadline exceeded"));
        }
        if len > self.max_control_bytes
            || offset
                .checked_add(len as u64)
                .is_none_or(|end| end > self.file.len())
        {
            return Err(error("local control extent exceeds bound"));
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
            .map_err(error)?;
        let data = tokio::time::timeout_at(deadline, task)
            .await
            .map_err(|_| error("file control deadline exceeded"))?
            .map_err(error)?
            .map_err(|_| error("file control worker panicked"))?
            .map_err(error)?;
        check_deadline(deadline).map_err(error)?;
        self.requests += 1;
        self.control_bytes += data.len() as u64;
        self.failed = false;
        Ok(data)
    }
    pub fn start(&mut self, offset: u64, len: u64) -> Result<(), EngineError> {
        self.active()?;
        self.failed = true;
        let end = offset
            .checked_add(len)
            .filter(|end| len > 0 && *end <= self.file.len())
            .ok_or(error("invalid local media extent"))?;
        self.position = offset;
        self.end = end;
        self.failed = false;
        Ok(())
    }
    pub async fn chunk(&mut self) -> Result<Option<FileChunk>, EngineError> {
        self.active()?;
        self.failed = true;
        #[cfg(not(test))]
        let chunk_bytes = FILE_CHUNK_BYTES;
        #[cfg(test)]
        let chunk_bytes = self.chunk_bytes;
        assert!((1..=FILE_CHUNK_BYTES).contains(&chunk_bytes));
        let len = (self.end - self.position).min(chunk_bytes as u64) as usize;
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
            .map_err(error)?;
        let chunk = task
            .await
            .map_err(error)?
            .map_err(|_| error("file media worker panicked"))?
            .map_err(error)?;
        self.position += len as u64;
        self.media_reads += 1;
        self.media_bytes += len as u64;
        self.failed = false;
        Ok((len != 0).then_some(chunk))
    }
}

impl<D: Decoder> Source<D> for FileReader {
    type Request = ();
    fn pending(_decoder: &D) -> EngineResult<()> {
        Err(error(
            "sequential local decoder requested source reposition",
        ))
    }
    async fn read(
        &mut self,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        self.chunk().await.map(|chunk| match chunk {
            Some(chunk) => SourceRead::Data(chunk),
            None => SourceRead::End,
        })
    }
    async fn resume(
        &mut self,
        _request: (),
        _decoder: OffWorker<Option<D>>,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<D>>> {
        Err(error("sequential local source cannot reposition"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn file_setup_timeout_does_not_block_async_worker() {
        let owner = migo_io::scheduler::IoScheduler::new(0);

        let started = Instant::now();
        let result = FileReader::build(&owner, started + Duration::from_millis(20), move || {
            std::thread::sleep(Duration::from_millis(200));
            Err(std::io::Error::other("deliberate slow setup failure"))
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
