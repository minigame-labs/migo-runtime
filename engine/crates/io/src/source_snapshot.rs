//! Bounded snapshots of runtime-managed mutable regular files.
//!
//! The caller supplies the already-open source and a host-private spool root.
//! Every possible source writer must participate in FileMutations, or the host
//! must hand over a quiescent source. Metadata/read-only mappings/link counts
//! alone never establish that obligation. No pathname is reopened for payload.
//! Successful output owns independent anonymous bytes; subsequent source writes
//! cannot change it. Setup and bounded copy steps use the existing Fs executor.
use crate::{
    pools::{ByteTicket, IoPools, PoolError},
    read_at_file::{FILE_CHUNK_BYTES, control_limit},
    scheduler::IoScheduler,
    spool::{SealedSpool, SpoolBudget, SpoolWriter},
    task::{BackendKind, IoRequest, PriorityClass, RequestKind},
};
use shared::vfs::{
    file_io::{FileStamp, read_exact_at},
    file_mutation::{FileMutations, ReadEpoch},
};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

// One scratch block becomes the first sealed-reader slot. Credit includes the
// possible second slot and stays in ChunkPool until its final owner drops.
const MEMORY_BYTES: u64 = 2 * FILE_CHUNK_BYTES as u64;
const STEP_CHUNKS: usize = 16;

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
fn check(scheduler: &IoScheduler, cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "snapshot cancelled",
        ));
    }
    scheduler.ensure_open().map_err(pool_error)
}
fn pool_error(error: PoolError) -> io::Error {
    io::Error::other(error.to_string())
}
fn request() -> IoRequest {
    IoRequest::Copy {
        backend: BackendKind::Filesystem,
        request: RequestKind::Async,
        priority: PriorityClass::ForegroundAsync,
        // Persistent credit is owned by SnapshotCopy/ChunkPool, not each job.
        scratch_bytes: 0,
    }
}

struct SnapshotCopy {
    source: File,
    epoch: ReadEpoch,
    stamp: FileStamp,
    writer: SpoolWriter,
    scratch: Vec<u8>,
    lease: ByteTicket,
    copied: u64,
    max_control_bytes: usize,
}
enum Progress {
    More(SnapshotCopy),
    Done(SealedSpool),
}
impl SnapshotCopy {
    fn new(
        source: File,
        dir: &Path,
        budget: &SpoolBudget,
        max_bytes: u64,
        max_control_bytes: usize,
        pools: &IoPools,
    ) -> io::Result<Self> {
        Self::new_with(
            source,
            dir,
            budget,
            max_bytes,
            max_control_bytes,
            pools,
            || {},
        )
    }
    fn new_with(
        source: File,
        dir: &Path,
        budget: &SpoolBudget,
        max_bytes: u64,
        max_control_bytes: usize,
        pools: &IoPools,
        after_epoch: impl FnOnce(),
    ) -> io::Result<Self> {
        control_limit(max_control_bytes)?;
        if max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid snapshot limit",
            ));
        }
        let mutations = FileMutations::for_file(&source)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot source is not regular",
            )
        })?;
        let epoch = mutations.read_epoch()?;
        after_epoch();
        let stamp = FileStamp::of(source.metadata()?)?;
        epoch.verify()?;
        if stamp.len > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot exceeds limit",
            ));
        }
        let lease = pools
            .reserve_bytes(MEMORY_BYTES)
            .map_err(io::Error::other)?;
        let writer = SpoolWriter::create(dir, budget, max_bytes, Some(stamp.len))?;
        Ok(Self {
            source,
            epoch,
            stamp,
            writer,
            scratch: vec![0; FILE_CHUNK_BYTES],
            lease,
            copied: 0,
            max_control_bytes,
        })
    }
    fn verify(&self) -> io::Result<()> {
        self.epoch.verify()?;
        self.stamp.verify(&self.source)
    }
    #[cfg(test)]
    fn step(self, scheduler: &IoScheduler, cancelled: &AtomicBool) -> io::Result<Progress> {
        self.step_with(scheduler, cancelled, |_| {})
    }
    #[cfg(test)]
    fn step_with(
        self,
        scheduler: &IoScheduler,
        cancelled: &AtomicBool,
        after_read: impl FnMut(u64),
    ) -> io::Result<Progress> {
        self.step_until(scheduler, cancelled, None, after_read)
    }
    fn step_until(
        mut self,
        scheduler: &IoScheduler,
        cancelled: &AtomicBool,
        deadline: Option<tokio::time::Instant>,
        mut after_read: impl FnMut(u64),
    ) -> io::Result<Progress> {
        for _ in 0..STEP_CHUNKS {
            check_deadline(deadline)?;
            check(scheduler, cancelled)?;
            self.verify()?;
            if self.copied == self.stamp.len {
                break;
            }
            let len = (self.stamp.len - self.copied).min(FILE_CHUNK_BYTES as u64) as usize;
            read_exact_at(&self.source, &mut self.scratch[..len], self.copied)?;
            self.writer.append(&self.scratch[..len])?;
            self.copied += len as u64;
            after_read(self.copied);
            self.verify()?;
        }
        check_deadline(deadline)?;
        check(scheduler, cancelled)?;
        if self.copied == self.stamp.len {
            let output =
                self.writer
                    .seal_with_buffer(self.max_control_bytes, self.scratch, self.lease)?;
            Ok(Progress::Done(output))
        } else {
            Ok(Progress::More(self))
        }
    }
}

/// Copy at most `max_bytes` into a sealed private object, without entry-sized
/// RAM or a new worker pool. Each Fs job copies at most 1MiB, checking close,
/// cancellation and the same read epoch per 64KiB block. Overlap fails instead
/// of retrying indefinitely or publishing a partial/mixed object. The consumed
/// File remains live through final verification, preventing native ID reuse.
pub async fn snapshot_file(
    scheduler: Arc<IoScheduler>,
    source: File,
    spool_dir: PathBuf,
    budget: SpoolBudget,
    max_bytes: u64,
    max_control_bytes: usize,
) -> io::Result<SealedSpool> {
    snapshot_open(
        scheduler,
        move || Ok(source),
        spool_dir,
        budget,
        max_bytes,
        max_control_bytes,
    )
    .await
}

/// The same snapshot protocol with source opening deferred to the first Fs
/// job. Pass an already-captured directory/VFS capability, never resolve a
/// later mount generation in this opener. Dropped/unpolled/closed operations
/// cannot acquire new source authority. The opener has the same cooperative
/// writer or quiescent-host obligation as [`snapshot_file`].
pub async fn snapshot_open(
    scheduler: Arc<IoScheduler>,
    open: impl FnOnce() -> io::Result<File> + Send + 'static,
    spool_dir: PathBuf,
    budget: SpoolBudget,
    max_bytes: u64,
    max_control_bytes: usize,
) -> io::Result<SealedSpool> {
    snapshot_open_inner(
        scheduler,
        open,
        spool_dir,
        budget,
        max_bytes,
        max_control_bytes,
        None,
    )
    .await
}

/// Apply one absolute deadline to admission, queued opening, every copy step
/// and final sealing. Expired work is rejected even when its async waiter has
/// not polled the timeout again.
pub async fn snapshot_open_until(
    scheduler: Arc<IoScheduler>,
    open: impl FnOnce() -> io::Result<File> + Send + 'static,
    spool_dir: PathBuf,
    budget: SpoolBudget,
    max_bytes: u64,
    max_control_bytes: usize,
    deadline: tokio::time::Instant,
) -> io::Result<SealedSpool> {
    snapshot_open_inner(
        scheduler,
        open,
        spool_dir,
        budget,
        max_bytes,
        max_control_bytes,
        Some(deadline),
    )
    .await
}

fn check_deadline(deadline: Option<tokio::time::Instant>) -> io::Result<()> {
    if deadline.is_some_and(|end| tokio::time::Instant::now() >= end) {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "snapshot deadline exceeded",
        ))
    } else {
        Ok(())
    }
}

async fn snapshot_open_inner(
    scheduler: Arc<IoScheduler>,
    open: impl FnOnce() -> io::Result<File> + Send + 'static,
    spool_dir: PathBuf,
    budget: SpoolBudget,
    max_bytes: u64,
    max_control_bytes: usize,
    deadline: Option<tokio::time::Instant>,
) -> io::Result<SealedSpool> {
    control_limit(max_control_bytes)?;
    if max_bytes == 0 || !spool_dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid snapshot limits or root",
        ));
    }
    check_deadline(deadline)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(cancelled.clone());
    let worker = scheduler.clone();
    let flag = cancelled.clone();
    let mut progress = scheduler
        .run_async(request(), move || {
            check_deadline(deadline)?;
            check(&worker, &flag)?;
            let source = open()?;
            SnapshotCopy::new(
                source,
                &spool_dir,
                &budget,
                max_bytes,
                max_control_bytes,
                worker.pools(),
            )?
            .step_until(&worker, &flag, deadline, |_| {})
        })
        .await
        .map_err(pool_error)??;
    loop {
        match progress {
            Progress::Done(output) => return Ok(output),
            Progress::More(copy) => {
                let worker = scheduler.clone();
                let flag = cancelled.clone();
                progress = scheduler
                    .run_async(request(), move || {
                        copy.step_until(&worker, &flag, deadline, |_| {})
                    })
                    .await
                    .map_err(pool_error)??;
            }
        }
    }
}

#[cfg(test)]
mod tests;
