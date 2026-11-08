//! Sealed local sources cross the audio protocol as immutable IO capabilities.
use super::*;
use migo_io::read_at_file::{FILE_CHUNK_BYTES, FileChunk};
use migo_io::task::{PoolKind, PriorityClass};
use shared::audio_file::{AudioFile, AudioFileCancel, AudioFileChunk, AudioFileRead};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant;
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct File {
    sealed: Arc<migo_io::spool::SealedSpool>,
    scheduler: Arc<migo_io::scheduler::IoScheduler>,
    domain: Arc<migo_io::domain::IoDomain>,
    prefix: [u8; 8],
    prefix_len: usize,
}
struct Loan {
    chunk: FileChunk,
    _file: Arc<File>,
}
impl AsRef<[u8]> for Loan {
    fn as_ref(&self) -> &[u8] {
        self.chunk.as_ref()
    }
}
// Field order frees the exact boxed allocation before returning its credit.
struct Control {
    data: Box<[u8]>,
    _lease: migo_io::pools::ByteTicket,
    _source: Arc<File>,
}
impl AsRef<[u8]> for Control {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
fn io_error() -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, "local audio source unavailable")
}
impl AudioFile for File {
    fn length(&self) -> u64 {
        self.sealed.reader().len()
    }
    fn prefix(&self) -> &[u8] {
        &self.prefix[..self.prefix_len]
    }
    fn queued_bytes(&self) -> usize {
        2 * FILE_CHUNK_BYTES
    }
    fn is_closed(&self) -> bool {
        self.domain.is_closed()
    }
    fn read_blocking(&self, offset: u64, output: &mut [u8]) -> Result<(), EngineError> {
        if self.is_closed() {
            return Err(io_error());
        }
        self.sealed
            .reader()
            .read_into_at(offset, output)
            .map_err(|_| io_error())
    }
    fn closed_signal(&self) -> &tokio::sync::Notify {
        self.domain.closed_signal()
    }
    fn read_at(
        self: Arc<Self>,
        offset: u64,
        length: usize,
        cancelled: Arc<dyn AudioFileCancel>,
        deadline: std::time::Instant,
    ) -> Result<AudioFileRead, EngineError> {
        if self.is_closed()
            || cancelled.is_cancelled()
            || length == 0
            || length > FILE_CHUNK_BYTES
            || offset
                .checked_add(length as u64)
                .is_none_or(|end| end > self.length())
        {
            return Err(io_error());
        }
        let pools = self.scheduler.pools().clone();
        pools
            .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, move || {
                let check = || {
                    if cancelled.is_cancelled() || self.is_closed() {
                        return Err(io_error());
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(EngineError::from_detail(
                            ErrorCode::Timeout,
                            "local audio read deadline exceeded",
                        ));
                    }
                    Ok(())
                };
                check()?;
                let chunk = self
                    .sealed
                    .reader()
                    .chunk_at(offset, length)
                    .map_err(|_| io_error())?;
                check()?;
                Ok(AudioFileChunk::new(Loan { chunk, _file: self }))
            })
            .map_err(|_| io_error())
    }
    fn read_control_at(
        self: Arc<Self>,
        offset: u64,
        bytes: usize,
        abort: Arc<dyn AudioFileCancel>,
        until: std::time::Instant,
    ) -> Result<AudioFileRead, EngineError> {
        if self.is_closed()
            || abort.is_cancelled()
            || bytes == 0
            || bytes > 2 * 1024 * 1024
            || offset
                .checked_add(bytes as u64)
                .is_none_or(|end| end > self.length())
        {
            return Err(io_error());
        }
        let lease = self
            .scheduler
            .pools()
            .reserve_bytes(bytes as u64)
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    "local audio index memory exhausted",
                )
            })?;
        let pools = self.scheduler.pools().clone();
        pools
            .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, move || {
                let check = || {
                    if abort.is_cancelled() || self.is_closed() {
                        return Err(io_error());
                    }
                    if std::time::Instant::now() >= until {
                        return Err(EngineError::from_detail(
                            ErrorCode::Timeout,
                            "local audio index deadline exceeded",
                        ));
                    }
                    Ok(())
                };
                check()?;
                let mut data = vec![0; bytes].into_boxed_slice();
                self.sealed
                    .reader()
                    .read_into_at(offset, &mut data)
                    .map_err(|_| io_error())?;
                check()?;
                Ok(AudioFileChunk::new(Control {
                    data,
                    _lease: lease,
                    _source: self,
                }))
            })
            .map_err(|_| io_error())
    }
}
async fn captured(
    vfs: Arc<VirtualFS>,
    path: String,
    context: &LocalAudioFile,
    deadline: Instant,
) -> Result<Arc<migo_io::spool::SealedSpool>, ServiceError> {
    local_file::capture(
        vfs,
        path,
        context,
        migo_io::spool::DEFAULT_SPOOL_FILE_BYTES,
        deadline,
    )
    .await
}
pub(super) async fn wrap(
    sealed: Arc<migo_io::spool::SealedSpool>,
    context: LocalAudioFile,
    deadline: Instant,
    cancelled: Arc<dyn AudioFileCancel>,
) -> Result<Arc<dyn AudioFile>, ServiceError> {
    let owner = sealed.clone();
    let domain = context.scheduler.domain();
    let check_domain = domain.clone();
    let prefix_len = sealed.reader().len().min(8) as usize;
    let read = context
        .scheduler
        .pools()
        .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, move || {
            if cancelled.is_cancelled() || check_domain.is_closed() || Instant::now() >= deadline {
                return Err(io_error());
            }
            let mut prefix = [0; 8];
            owner
                .reader()
                .read_into_at(0, &mut prefix[..prefix_len])
                .map_err(|_| io_error())?;
            if cancelled.is_cancelled() || check_domain.is_closed() || Instant::now() >= deadline {
                return Err(io_error());
            }
            Ok(prefix)
        })
        .map_err(|_| file_error("Failed to read local audio file"))?;
    let prefix = read
        .await
        .map_err(|_| file_error("Failed to read local audio file"))?
        .map_err(|_| file_error("Failed to read local audio file"))?
        .map_err(|_| file_error("Failed to read local audio file"))?;
    Ok(Arc::new(File {
        sealed,
        scheduler: context.scheduler,
        domain,
        prefix,
        prefix_len,
    }))
}
pub(super) async fn source(
    vfs: Arc<VirtualFS>,
    path: String,
    context: LocalAudioFile,
) -> Result<Arc<dyn AudioFile>, ServiceError> {
    let deadline = Instant::now() + TIMEOUT;
    let domain = context.scheduler.domain();
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = Cancel(cancelled.clone());
    tokio::select! { biased;
        _=domain.closed()=>Err(file_error("Failed to read local audio file")),
        result=tokio::time::timeout_at(deadline,async {
            let sealed=captured(vfs,path,&context,deadline).await?;
            wrap(sealed,context,deadline,cancelled).await
        })=>result.map_err(|_|file_error("Failed to read local audio file"))?,
    }
}
pub(super) async fn prepare(
    vfs: Arc<VirtualFS>,
    path: String,
    context: LocalAudioFile,
) -> Result<LocalAudioPrepared, ServiceError> {
    let deadline = Instant::now() + TIMEOUT;
    let domain = context.scheduler.domain();
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = Cancel(cancelled.clone());
    tokio::select! { biased;
        _=domain.closed()=>Err(file_error("Failed to read local audio file")),
        result=tokio::time::timeout_at(deadline,async {
            let sealed=captured(vfs,path,&context,deadline).await?;
            if sealed.reader().len()<=MAX_ENCODED_AUDIO_BYTES as u64 {
                local_file::collect_sealed(sealed,context,cancelled,deadline).await.map(LocalAudioPrepared::Data)
            } else { wrap(sealed,context,deadline,cancelled).await.map(LocalAudioPrepared::Stream) }
        })=>result.map_err(|_|file_error("Failed to read local audio file"))?,
    }
}
