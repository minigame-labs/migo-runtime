//! Format-independent anchored file/directory/package source.
//! Fs/Pack IO and one shared CPU/PCM publication loop serve every codec.
use crate::{
    off_worker::OffWorker,
    streaming::{RESPONSE_HEADER_TIMEOUT, StreamMsg, StreamingState},
    streaming_transport::{Decoder, InterruptibleSource, Source, SourceRead, SourceResume},
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
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{sync::mpsc, time::Instant};
/// Per-control allocation bound; sequential codecs do not issue control reads.
pub const MAX_CONTROL_BYTES: usize = 2 * 1024 * 1024;

/// Host-supplied byte ownership, never inferred from a path, stat or link count.
pub enum DirectoryOwnership {
    /// All writers cooperate with runtime mutation epochs, or the host hands
    /// over a quiescent object. Always create independent private bytes.
    Snapshot,
    /// The host/publisher guarantees the opened generation remains immutable
    /// for the reader's lifetime, including aliases outside the game mapping.
    Immutable,
}

pub struct SnapshotOptions {
    pub dir: std::path::PathBuf,
    pub budget: migo_io::spool::SpoolBudget,
    pub max_bytes: u64,
    pub deadline: Instant,
}
impl SnapshotOptions {
    pub fn session(paths: &shared::vfs::GamePaths) -> Self {
        Self {
            dir: paths.spool_dir().to_path_buf(),
            budget: migo_io::spool::SpoolBudget::process(),
            max_bytes: migo_io::spool::DEFAULT_SPOOL_FILE_BYTES,
            deadline: Instant::now() + RESPONSE_HEADER_TIMEOUT,
        }
    }
}

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

// One reusable reader allocation, with a distinct key for every IO operation.
// Re-enabling a boolean could let an old queued job borrow a later read's
// activity. A cancelled job must remain cancelled even after recovery.
struct ReadActive {
    active: Arc<AtomicU64>,
    epoch: u64,
}
impl Drop for ReadActive {
    fn drop(&mut self) {
        let _ = self
            .active
            .compare_exchange(self.epoch, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

#[cfg(test)]
struct MediaJobEnd {
    hook: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    succeeded: bool,
}
#[cfg(test)]
impl Drop for MediaJobEnd {
    fn drop(&mut self) {
        if let Some(hook) = &self.hook {
            hook(self.succeeded);
        }
    }
}

pub(crate) enum Backing {
    File(ReadAtFile),
    Package(ReadAtPackage),
    Spool(migo_io::spool::SealedSpool),
}
impl Backing {
    pub(crate) fn len(&self) -> u64 {
        match self {
            Self::File(v) => v.len(),
            Self::Package(v) => v.len(),
            Self::Spool(v) => v.reader().len(),
        }
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(crate) fn kind(&self) -> PoolKind {
        match self {
            Self::File(_) => PoolKind::Fs,
            Self::Spool(_) => PoolKind::Fs,
            Self::Package(_) => PoolKind::Pack,
        }
    }
    fn media_bytes(&self) -> u64 {
        match self {
            Self::File(v) => v.transient_media_bytes(),
            Self::Spool(v) => v.reader().transient_media_bytes(),
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
    pub(crate) fn read_into_at(&self, offset: u64, output: &mut [u8]) -> std::io::Result<()> {
        match self {
            Self::File(v) => v.read_into_at(offset, output),
            Self::Package(v) => v.read_into_at(offset, output),
            Self::Spool(v) => v.reader().read_into_at(offset, output),
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
    pub(crate) file: Arc<Backing>,
    pub(crate) pools: IoPools,
    pub(crate) domain: Arc<migo_io::domain::IoDomain>,
    position: u64,
    end: u64,
    max_control_bytes: usize,
    pub(crate) failed: bool,
    read_active: Arc<AtomicU64>,
    last_read_epoch: u64,
    media_interrupted: bool,
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
    pub before_media_io: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pub after_media_job: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    #[cfg(test)]
    pub chunk_bytes: usize,
}
impl FileReader {
    /// Automatic backend selection uses a single admitted capability. Package
    /// bytes stay on Pack; directory bytes follow the explicit host contract.
    /// A missing/failed backend never retries another mount or whole-file read.
    pub async fn open_asset(
        source: shared::vfs::mount::PinnedAssetRead,
        ownership: DirectoryOwnership,
        scheduler: Arc<migo_io::scheduler::IoScheduler>,
        snapshot: SnapshotOptions,
    ) -> EngineResult<Self> {
        scheduler.ensure_open().map_err(error)?;
        check_deadline(snapshot.deadline).map_err(error)?;
        match source {
            shared::vfs::mount::PinnedAssetRead::Package(pin) => {
                Self::open_package_at(pin, &scheduler, snapshot.deadline).await
            }
            shared::vfs::mount::PinnedAssetRead::Directory(pin) => match ownership {
                DirectoryOwnership::Immutable => {
                    Self::build(&scheduler, snapshot.deadline, move || pin.open()).await
                }
                DirectoryOwnership::Snapshot => {
                    let deadline = snapshot.deadline;
                    let operation = migo_io::source_snapshot::snapshot_open_until(
                        scheduler.clone(),
                        move || pin.open(),
                        snapshot.dir,
                        snapshot.budget,
                        snapshot.max_bytes,
                        MAX_CONTROL_BYTES,
                        deadline,
                    );
                    let domain = scheduler.domain();
                    let sealed = tokio::select! {
                        biased;
                        _ = domain.closed() => return Err(error("asset domain closed")),
                        result = tokio::time::timeout_at(deadline, operation) => result
                            .map_err(|_| error("asset snapshot deadline exceeded"))?
                            .map_err(error)?,
                    };
                    Self::open_spool(sealed, &scheduler, deadline)
                }
            },
        }
    }
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
            Arc::new(AtomicU64::new(0)),
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
        Self::open_package_at(pin, scheduler, Instant::now() + RESPONSE_HEADER_TIMEOUT).await
    }
    async fn open_package_at(
        pin: shared::vfs::mount::PinnedPackRead,
        scheduler: &migo_io::scheduler::IoScheduler,
        deadline: Instant,
    ) -> EngineResult<Self> {
        let pools = scheduler.pools().clone();
        Self::build_source(scheduler, PoolKind::Pack, 0, deadline, move || {
            ReadAtPackage::new(pin, &pools, MAX_CONTROL_BYTES).map(Backing::Package)
        })
        .await
    }
    async fn build(
        scheduler: &migo_io::scheduler::IoScheduler,
        deadline: Instant,
        open: impl FnOnce() -> std::io::Result<File> + Send + 'static,
    ) -> EngineResult<Self> {
        let pools = scheduler.pools().clone();
        Self::build_source(scheduler, PoolKind::Fs, 0, deadline, move || {
            ReadAtFile::with_budget(open()?, MAX_CONTROL_BYTES, &pools).map(Backing::File)
        })
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
        let read_active = Arc::new(AtomicU64::new(1));
        let _operation = ReadActive {
            active: read_active.clone(),
            epoch: 1,
        };
        let active = read_active.clone();
        let task = pools
            .submit_async_bytes(
                kind,
                PriorityClass::ForegroundAsync,
                setup_bytes,
                move || {
                    if check.is_closed() || active.load(Ordering::Acquire) != 1 {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    check_deadline(deadline)?;
                    let file = open()?;
                    if check.is_closed() || active.load(Ordering::Acquire) != 1 {
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
        read_active: Arc<AtomicU64>,
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
            last_read_epoch: read_active.load(Ordering::Acquire),
            read_active,
            media_interrupted: false,
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
            before_media_io: None,
            #[cfg(test)]
            after_media_job: None,
            #[cfg(test)]
            chunk_bytes: FILE_CHUNK_BYTES,
        }
    }
    fn begin_read(&mut self) -> EngineResult<ReadActive> {
        let epoch = self.last_read_epoch.checked_add(1).ok_or_else(|| {
            EngineError::from_detail(ErrorCode::InputSaturated, "file read epoch exhausted")
        })?;
        self.last_read_epoch = epoch;
        self.read_active.store(epoch, Ordering::Release);
        Ok(ReadActive {
            active: self.read_active.clone(),
            epoch,
        })
    }

    /// Only a dropped pending media wait can be resumed. Returned errors,
    /// control cancellations and domain closure remain fail-closed. The old job
    /// keeps its retired epoch; recovery never waits for its disk work to exit.
    pub fn resume_after_interruption(&mut self) -> EngineResult<()> {
        if self.domain.is_closed() {
            return Err(error("file domain closed"));
        }
        if self.media_interrupted && self.read_active.load(Ordering::Acquire) == 0 {
            self.media_interrupted = false;
            self.failed = false;
        }
        self.active()
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
        let requests = self
            .requests
            .checked_add(1)
            .ok_or_else(|| error("file request counter overflow"))?;
        let control_bytes = self
            .control_bytes
            .checked_add(len as u64)
            .ok_or_else(|| error("file control byte counter overflow"))?;
        let _operation = self.begin_read()?;
        let epoch = _operation.epoch;
        #[cfg(test)]
        {
            self.submits += 1;
        }
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
                    if domain.is_closed() || read_active.load(Ordering::Acquire) != epoch {
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
                    if domain.is_closed() || read_active.load(Ordering::Acquire) != epoch {
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
        self.requests = requests;
        self.control_bytes = control_bytes;
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
        self.media_interrupted = true;
        let result = self.next_chunk().await;
        self.media_interrupted = false;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }
    async fn next_chunk(&mut self) -> Result<Option<FileChunk>, EngineError> {
        #[cfg(not(test))]
        let chunk_bytes = FILE_CHUNK_BYTES;
        #[cfg(test)]
        let chunk_bytes = self.chunk_bytes;
        assert!((1..=FILE_CHUNK_BYTES).contains(&chunk_bytes));
        let len = (self.end - self.position).min(chunk_bytes as u64) as usize;
        let media_reads = self
            .media_reads
            .checked_add(1)
            .ok_or_else(|| error("file media request counter overflow"))?;
        let media_bytes = self
            .media_bytes
            .checked_add(len as u64)
            .ok_or_else(|| error("file media byte counter overflow"))?;
        let _operation = self.begin_read()?;
        let epoch = _operation.epoch;
        #[cfg(test)]
        {
            self.submits += 1;
        }
        let read_active = self.read_active.clone();
        #[cfg(test)]
        let io_reads = self.io_reads.clone();
        #[cfg(test)]
        let after_io = self.after_io.clone();
        #[cfg(test)]
        let before_io = self.before_media_io.clone();
        #[cfg(test)]
        let after_job = self.after_media_job.clone();
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
                    let mut completion = MediaJobEnd {
                        hook: after_job,
                        succeeded: false,
                    };
                    #[cfg(test)]
                    if let Some(hook) = before_io {
                        hook();
                    }

                    #[cfg(test)]
                    std::thread::sleep(delay);
                    if domain.is_closed() || read_active.load(Ordering::Acquire) != epoch {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    #[cfg(test)]
                    io_reads.fetch_add(1, Ordering::Relaxed);
                    let result = file.chunk_at(position, len);
                    #[cfg(test)]
                    if let Some(hook) = after_io {
                        hook();
                    }
                    if domain.is_closed() || read_active.load(Ordering::Acquire) != epoch {
                        return Err(std::io::Error::other("file domain closed"));
                    }
                    #[cfg(test)]
                    {
                        completion.succeeded = result.is_ok();
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
        self.media_reads = media_reads;
        self.media_bytes = media_bytes;
        Ok((len != 0).then_some(chunk))
    }
}

impl InterruptibleSource for FileReader {
    fn resume_after_read_interruption(&mut self) -> EngineResult<()> {
        self.resume_after_interruption()
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

#[cfg(test)]
#[path = "file_interruption_tests.rs"]
mod interruption_tests;
