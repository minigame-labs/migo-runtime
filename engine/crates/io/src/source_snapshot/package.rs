//! Package capture reuses the validated, pinned reader and its whole-entry CRC.
//! Only archive work runs on Pack; the sealed result uses the normal Fs reader.
use super::*;
use shared::vfs::{mount::PinnedPackRead, package::PackageReadLimits};
use std::io::Write;

struct Output<'a> {
    writer: &'a mut SpoolWriter,
    scheduler: &'a IoScheduler,
    cancelled: &'a AtomicBool,
    deadline: tokio::time::Instant,
}
impl Write for Output<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        for bytes in input.chunks(FILE_CHUNK_BYTES) {
            check_deadline(Some(self.deadline))?;
            // Write::write_all retries Interrupted. Cancellation/close is
            // terminal here; preserve the cause without asking it to retry.
            check(self.scheduler, self.cancelled).map_err(io::Error::other)?;
            self.writer.append(bytes)?;
        }
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Capture one already-selected package entry into the existing private spool.
/// The original package backing identity, validated chunk extents and CRC stay
/// authoritative. There is no host-path or whole-entry Vec fallback. The single
/// Pack job is capped by `max_bytes`; cancellation/deadline are checked before
/// opening, per 64KiB write and after the final CRC, before publishing the seal.
/// Rust reader scratch is admitted before its allocation and freed on Pack
/// before credit returns. The native zstd context/shared index are not included
/// in that payload charge, as for the existing ReadAtPackage contract.
pub async fn snapshot_package_until(
    scheduler: Arc<IoScheduler>,
    source: PinnedPackRead,
    spool_dir: PathBuf,
    budget: SpoolBudget,
    max_bytes: u64,
    max_control_bytes: usize,
    deadline: tokio::time::Instant,
) -> io::Result<SealedSpool> {
    control_limit(max_control_bytes)?;
    if max_bytes == 0 || !spool_dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid package snapshot limits",
        ));
    }
    check_deadline(Some(deadline))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(cancelled.clone());
    let worker = scheduler.clone();
    scheduler
        .run_async(
            IoRequest::Copy {
                backend: BackendKind::Pack,
                request: RequestKind::Async,
                priority: PriorityClass::ForegroundAsync,
                scratch_bytes: 0,
            },
            move || {
                check_deadline(Some(deadline))?;
                check(&worker, &cancelled)?;
                let length = source.entry_size().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "package entry unavailable")
                })?;
                if length > max_bytes {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        SnapshotTooLarge,
                    ));
                }
                let limits = PackageReadLimits::default();
                // Reverse local drop order: reader before its scratch ticket.
                let _scratch_lease = worker
                    .pools()
                    .reserve_bytes(limits.max_scratch_bytes()? as u64)
                    .map_err(io::Error::other)?;
                let media_lease = worker
                    .pools()
                    .reserve_bytes(MEMORY_BYTES)
                    .map_err(io::Error::other)?;
                let mut writer = SpoolWriter::create(&spool_dir, &budget, max_bytes, Some(length))?;
                let mut reader = source.open_reader(limits)?;
                if reader.len() != length {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "package entry length changed",
                    ));
                }
                reader
                    .copy_to(&mut Output {
                        writer: &mut writer,
                        scheduler: &worker,
                        cancelled: &cancelled,
                        deadline,
                    })
                    .map_err(io::Error::other)?;
                check_deadline(Some(deadline))?;
                check(&worker, &cancelled)?;
                // Destroy both real reusable reader buffers on this Pack worker.
                drop(reader);
                writer.seal_with_buffer(max_control_bytes, vec![0; FILE_CHUNK_BYTES], media_lease)
            },
        )
        .await
        .map_err(pool_error)?
}

#[cfg(test)]
mod tests;
