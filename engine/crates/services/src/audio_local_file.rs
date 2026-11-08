//! Sandbox files are captured once on the session's existing Fs executor.
//! A sealed source, its byte credit and its disk credit outlive every running
//! read, including a dropped source-generation waiter.
use super::*;
use migo_io::read_at_file::FILE_CHUNK_BYTES;
use migo_io::task::{PoolKind, PriorityClass};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const CONTROL_BYTES: usize = 2 * 1024 * 1024;

#[cfg(test)]
#[derive(Clone, Default)]
pub(super) struct Hooks {
    pub after_seal: Option<Arc<dyn Fn() + Send + Sync>>,
    pub before_read: Option<Arc<dyn Fn() + Send + Sync>>,
    pub after_read: Option<Arc<dyn Fn() + Send + Sync>>,
}
struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
fn check(
    domain: &migo_io::domain::IoDomain,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> std::io::Result<()> {
    if cancelled.load(Ordering::Acquire) || domain.is_closed() || Instant::now() >= deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "local audio source retired",
        ));
    }
    Ok(())
}
fn capture_error(error: std::io::Error) -> ServiceError {
    if error
        .get_ref()
        .is_some_and(|cause| cause.is::<migo_io::source_snapshot::SnapshotTooLarge>())
    {
        input_exceeds_limit()
    } else {
        // Neither host paths nor platform-specific errors cross to content.
        file_error("Failed to open local audio file")
    }
}
pub(super) async fn read(
    vfs: Arc<VirtualFS>,
    path: String,
    context: LocalAudioFile,
) -> Result<Vec<u8>, ServiceError> {
    let deadline = Instant::now() + TIMEOUT;
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = Cancel(cancelled.clone());
    let domain = context.scheduler.domain();
    tokio::select! {
        biased;
        _ = domain.closed() => Err(file_error("Failed to read local audio file")),
        result = tokio::time::timeout_at(deadline, collect(vfs, path, context, cancelled, deadline)) =>
            result.map_err(|_| file_error("Failed to read local audio file"))?,
    }
}
async fn collect(
    vfs: Arc<VirtualFS>,
    path: String,
    context: LocalAudioFile,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Vec<u8>, ServiceError> {
    let sealed = capture(
        vfs,
        path,
        &context,
        MAX_ENCODED_AUDIO_BYTES as u64,
        deadline,
    )
    .await?;
    collect_sealed(sealed, context, cancelled, deadline).await
}

pub(super) async fn capture(
    vfs: Arc<VirtualFS>,
    path: String,
    context: &LocalAudioFile,
    maximum: u64,
    deadline: Instant,
) -> Result<Arc<migo_io::spool::SealedSpool>, ServiceError> {
    use shared::vfs::mount::PinnedAssetRead;
    let pin = path
        .strip_prefix("/code/")
        .and_then(|relative| {
            context
                .mounts
                .as_ref()
                .map(|mounts| mounts.pin_asset_read(relative, mounts.generation()))
        })
        .transpose()
        .map_err(|_| file_error("Failed to open local audio file"))?;
    let result = match pin {
        Some(PinnedAssetRead::Package(pin)) => {
            migo_io::source_snapshot::snapshot_package_until(
                context.scheduler.clone(),
                pin,
                context.spool_dir.clone(),
                migo_io::spool::SpoolBudget::process(),
                maximum,
                CONTROL_BYTES,
                deadline,
            )
            .await
        }
        Some(PinnedAssetRead::Directory(pin)) => {
            migo_io::source_snapshot::snapshot_open_until(
                context.scheduler.clone(),
                move || pin.open(),
                context.spool_dir.clone(),
                migo_io::spool::SpoolBudget::process(),
                maximum,
                CONTROL_BYTES,
                deadline,
            )
            .await
        }
        None => {
            migo_io::source_snapshot::snapshot_open_until(
                context.scheduler.clone(),
                move || {
                    vfs.open_regular_for_read(&path)
                        .map_err(std::io::Error::other)
                },
                context.spool_dir.clone(),
                migo_io::spool::SpoolBudget::process(),
                maximum,
                CONTROL_BYTES,
                deadline,
            )
            .await
        }
    };
    let sealed = result.map_err(|error| {
        if maximum != MAX_ENCODED_AUDIO_BYTES as u64
            && error
                .get_ref()
                .is_some_and(|e| e.is::<migo_io::source_snapshot::SnapshotTooLarge>())
        {
            engine_error(EngineError::from_detail(
                ErrorCode::InputSaturated,
                "local audio file exceeds the 64MiB source limit",
            ))
        } else {
            capture_error(error)
        }
    })?;
    Ok(Arc::new(sealed))
}

pub(super) async fn collect_sealed(
    sealed: Arc<migo_io::spool::SealedSpool>,
    context: LocalAudioFile,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Vec<u8>, ServiceError> {
    #[cfg(test)]
    if let Some(hook) = &context.hooks.after_seal {
        hook();
    }
    // Length was captured on Fs under the read epoch, before sealing. There is
    // no second stat, source pathname reopen or metadata-derived permission.
    let length = usize::try_from(sealed.reader().len()).map_err(|_| input_exceeds_limit())?;
    if length > MAX_ENCODED_AUDIO_BYTES {
        return Err(input_exceeds_limit());
    }
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .map_err(|_| audio_error("Failed to allocate local audio buffer"))?;
    if data.capacity() > MAX_ENCODED_AUDIO_BYTES {
        return Err(input_exceeds_limit());
    }
    let domain = context.scheduler.domain();
    while data.len() < length {
        let offset = data.len();
        let count = (length - offset).min(FILE_CHUNK_BYTES);
        let source = sealed.clone();
        let check_domain = domain.clone();
        #[cfg(test)]
        let hooks = context.hooks.clone();
        let flag = cancelled.clone();
        // SnapshotCopy already reserved both 64KiB slots. The task borrows
        // that exact pool and retains its owner until the real Fs job returns.
        let read = context
            .scheduler
            .pools()
            .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, move || {
                #[cfg(test)]
                if let Some(hook) = &hooks.before_read {
                    hook();
                }
                check(&check_domain, &flag, deadline)?;
                let chunk = source.reader().chunk_at(offset as u64, count)?;
                #[cfg(test)]
                if let Some(hook) = &hooks.after_read {
                    hook();
                }
                check(&check_domain, &flag, deadline)?;
                Ok::<_, std::io::Error>(chunk)
            })
            .map_err(|_| file_error("Failed to read local audio file"))?;
        let chunk = read
            .await
            .map_err(|_| file_error("Failed to read local audio file"))?
            .map_err(|_| file_error("Failed to read local audio file"))?
            .map_err(|_| file_error("Failed to read local audio file"))?;
        check(&domain, &cancelled, deadline)
            .map_err(|_| file_error("Failed to read local audio file"))?;
        data.extend_from_slice(chunk.as_ref());
    }
    Ok(data)
}
