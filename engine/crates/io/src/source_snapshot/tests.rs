use super::*;
use std::{
    fs::{self, FileTimes},
    future::Future,
    io::{Seek, SeekFrom, Write},
    sync::{Barrier, atomic::AtomicUsize},
    task::{Context, Poll, Waker},
    time::Duration,
};

struct Fixture {
    root: tempfile::TempDir,
    path: PathBuf,
    dir: PathBuf,
    data: Vec<u8>,
    scheduler: Arc<IoScheduler>,
    budget: SpoolBudget,
}
impl Fixture {
    fn new(len: usize) -> Self {
        Self::with_memory(len, 128 * 1024)
    }
    fn with_memory(len: usize, memory: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let dir = root.path().join("private");
        let data = (0..len).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        fs::write(&path, &data).unwrap();
        Self {
            root,
            path,
            dir,
            data,
            scheduler: Arc::new(IoScheduler::local_with_byte_limit_for_test(927, 2, memory)),
            budget: SpoolBudget::new((len as u64).max(1), 1).unwrap(),
        }
    }
    fn begin(&self) -> io::Result<SnapshotCopy> {
        SnapshotCopy::new(
            File::open(&self.path)?,
            &self.dir,
            &self.budget,
            (self.data.len() as u64).max(1),
            4096,
            self.scheduler.pools(),
        )
    }
    fn future(&self) -> impl Future<Output = io::Result<SealedSpool>> + Send + 'static {
        snapshot_file(
            self.scheduler.clone(),
            File::open(&self.path).unwrap(),
            self.dir.clone(),
            self.budget.clone(),
            (self.data.len() as u64).max(1),
            4096,
        )
    }
    fn released(&self) {
        assert!(
            self.scheduler.pools().reserve_bytes(128 * 1024).is_ok(),
            "memory credit leaked"
        );
        let probe = SpoolWriter::create(&self.dir, &self.budget, 1, Some(0));
        assert!(probe.is_ok(), "disk/file credit leaked");
        drop(probe);
        assert_eq!(fs::read_dir(&self.dir).unwrap().count(), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.scheduler.close();
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn finish(mut copy: SnapshotCopy, scheduler: &IoScheduler) -> SealedSpool {
    loop {
        match copy.step(scheduler, &AtomicBool::new(false)).unwrap() {
            Progress::More(next) => copy = next,
            Progress::Done(output) => return output,
        }
    }
}
fn exact(output: &SealedSpool, expected: &[u8]) {
    assert_eq!(output.reader().len(), expected.len() as u64);
    for (i, chunk) in expected.chunks(FILE_CHUNK_BYTES).enumerate() {
        assert_eq!(
            output
                .reader()
                .chunk_at((i * FILE_CHUNK_BYTES) as u64, chunk.len())
                .unwrap()
                .as_ref(),
            chunk
        );
    }
}
fn rewrite_and_restore(file: &mut File, byte: u8) {
    let modified = file.metadata().unwrap().modified().unwrap();
    let mutations = FileMutations::for_file(file).unwrap().unwrap();
    let _write = mutations.into_write().unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&[byte]).unwrap();
    file.set_times(FileTimes::new().set_modified(modified))
        .unwrap();
}

#[test]
fn public_snapshot_pins_the_source_yields_between_jobs_and_survives_later_writes() {
    let f = Fixture::new(2 * 1024 * 1024 + 17);
    let pending = f.future();
    let old = f.root.path().join("renamed");
    fs::rename(&f.path, &old).unwrap();
    fs::write(&f.path, b"replacement pathname").unwrap();
    let output = runtime().block_on(pending).unwrap();
    assert_eq!(
        f.scheduler.metrics().delegated_runs,
        3,
        "copy did not yield every 1MiB"
    );
    assert_eq!(f.scheduler.metrics().inline_runs, 0);
    let mut writer = File::options().write(true).open(old).unwrap();
    rewrite_and_restore(&mut writer, 255);
    exact(&output, &f.data);
    drop(output);
    f.released();
}

#[test]
fn bounded_steps_reuse_scratch_without_steady_allocations_and_transfer_it_to_the_reader() {
    let f = Fixture::new(8 * 1024 * 1024 + 31);
    let before_setup = migo_alloc_probe::thread_counts();
    let copy = f.begin().unwrap();
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before_setup.bytes_allocated
            < 128 * 1024,
        "snapshot setup allocated entry-sized Rust storage"
    );
    let pointer = copy.scratch.as_ptr();
    let mut state = Some(copy);
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "mutable source bounded snapshot step",
            warmup: 1,
            measured: 4,
        },
        |_| {
            let before = state.as_ref().unwrap().copied;
            let Progress::More(next) = state
                .take()
                .unwrap()
                .step(&f.scheduler, &AtomicBool::new(false))
                .unwrap()
            else {
                panic!("unbounded copy step")
            };
            assert_eq!(next.copied - before, 1024 * 1024);
            assert_eq!(next.scratch.as_ptr(), pointer);
            state = Some(next);
        },
    );
    let before_seal = migo_alloc_probe::thread_counts();
    let output = finish(state.take().unwrap(), &f.scheduler);
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before_seal.bytes_allocated < 4096,
        "seal allocated another media buffer instead of transferring scratch"
    );
    let chunk = output.reader().chunk_at(0, 128).unwrap();
    assert_eq!(
        chunk.as_ref().as_ptr(),
        pointer,
        "scratch was discarded at seal"
    );
    assert_eq!(chunk.as_ref(), &f.data[..128]);
    drop(chunk);
    exact(&output, &f.data);
}

#[test]
fn memory_and_disk_admission_precede_temporary_files_and_payload() {
    let f = Fixture::with_memory(1024, 128 * 1024 - 1);
    assert!(f.begin().is_err());
    assert!(!f.dir.exists(), "disk work preceded memory admission");
    let f = Fixture::new(1024);
    let held =
        SpoolWriter::create(&f.root.path().join("held"), &f.budget, 1024, Some(1024)).unwrap();
    assert!(f.begin().is_err());
    assert!(!f.dir.exists(), "disk work preceded disk admission");
    drop(held);
    f.released();
}

#[test]
fn invalid_limits_and_oversized_sources_fail_before_temporary_files() {
    let f = Fixture::new(100);
    for (limit, control) in [(0, 1), (99, 1), (100, 0), (100, usize::MAX)] {
        assert!(
            SnapshotCopy::new(
                File::open(&f.path).unwrap(),
                &f.dir,
                &f.budget,
                limit,
                control,
                f.scheduler.pools()
            )
            .is_err()
        );
        assert!(!f.dir.exists());
    }
}

#[cfg(unix)]
#[test]
fn nonregular_sources_never_create_a_spool() {
    let f = Fixture::new(3);
    assert!(
        SnapshotCopy::new(
            File::open(f.root.path()).unwrap(),
            &f.dir,
            &f.budget,
            100,
            1,
            f.scheduler.pools()
        )
        .is_err()
    );
    assert!(!f.dir.exists());
}

#[test]
fn an_active_writer_is_rejected_before_reading_length_or_creating_a_spool() {
    let f = Fixture::new(32);
    let file = File::open(&f.path).unwrap();
    let mutations = FileMutations::for_file(&file).unwrap().unwrap();
    let _writer = mutations.begin_write().unwrap();
    assert_eq!(f.begin().err().unwrap().kind(), io::ErrorKind::WouldBlock);
    assert!(!f.dir.exists());
}

#[test]
fn epoch_is_captured_before_metadata_and_verified_before_disk_admission() {
    let f = Fixture::new(32);
    let mut writer = File::options().write(true).open(&f.path).unwrap();
    assert!(
        SnapshotCopy::new_with(
            File::open(&f.path).unwrap(),
            &f.dir,
            &f.budget,
            100,
            1,
            f.scheduler.pools(),
            || rewrite_and_restore(&mut writer, 255)
        )
        .is_err()
    );
    assert!(
        !f.dir.exists(),
        "metadata race was admitted before epoch verification"
    );
}

#[test]
fn restored_metadata_cannot_hide_mutation_between_copy_steps() {
    let f = Fixture::new(1024 * 1024 + 9);
    let Progress::More(copy) = f
        .begin()
        .unwrap()
        .step(&f.scheduler, &AtomicBool::new(false))
        .unwrap()
    else {
        panic!("step was not bounded")
    };
    let mut writer = File::options().write(true).open(&f.path).unwrap();
    rewrite_and_restore(&mut writer, 255);
    assert!(
        copy.stamp.verify(&copy.source).is_ok(),
        "fixture did not restore observed metadata"
    );
    let mut reads = 0;
    assert!(
        copy.step_with(&f.scheduler, &AtomicBool::new(false), |_| reads += 1)
            .is_err()
    );
    assert_eq!(reads, 0, "changed epoch reached another payload read");
    f.released();
}

#[test]
fn mutation_after_the_final_read_never_publishes_a_sealed_snapshot() {
    let f = Fixture::new(1024 * 1024);
    let mut writer = File::options().write(true).open(&f.path).unwrap();
    let copy = f.begin().unwrap();
    let result = copy.step_with(&f.scheduler, &AtomicBool::new(false), |offset| {
        if offset == f.data.len() as u64 {
            rewrite_and_restore(&mut writer, 255);
        }
    });
    assert!(result.is_err());
    f.released();
}

#[test]
fn observed_untracked_metadata_changes_are_also_rejected() {
    let f = Fixture::new(27);
    let writer = File::options().write(true).open(&f.path).unwrap();
    let modified = writer.metadata().unwrap().modified().unwrap() + Duration::from_secs(10);
    let copy = f.begin().unwrap();
    assert!(
        copy.step_with(&f.scheduler, &AtomicBool::new(false), |_| writer
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap())
            .is_err()
    );
    f.released();
}

#[test]
fn cancellation_and_domain_close_interrupt_an_active_step_at_a_chunk_boundary() {
    for close in [false, true] {
        let f = Fixture::new(1024 * 1024 + 7);
        let flag = AtomicBool::new(false);
        let mut reads = 0;
        let result = f.begin().unwrap().step_with(&f.scheduler, &flag, |_| {
            reads += 1;
            if close {
                f.scheduler.close();
            } else {
                flag.store(true, Ordering::Relaxed);
            }
        });
        assert!(result.is_err());
        assert_eq!(reads, 1, "cancelled step continued reading payload");
        // Domain close forbids a new byte reservation; a separate test checks
        // credit after cancellation while the domain is still live.
        if !close {
            f.released();
        } else {
            assert_eq!(fs::read_dir(&f.dir).unwrap().count(), 0);
        }
    }
}

#[test]
fn cancellation_after_the_final_read_does_not_publish_output() {
    let f = Fixture::new(1024 * 1024);
    let flag = AtomicBool::new(false);
    assert!(
        f.begin()
            .unwrap()
            .step_with(&f.scheduler, &flag, |offset| {
                if offset == f.data.len() as u64 {
                    flag.store(true, Ordering::Relaxed);
                }
            })
            .is_err()
    );
    f.released();
}

#[test]
fn byte_credit_follows_the_last_chunk_after_the_reader_is_dropped() {
    let f = Fixture::new(100);
    let output = finish(f.begin().unwrap(), &f.scheduler);
    let chunk = output.reader().chunk_at(0, 100).unwrap();
    assert!(f.scheduler.pools().reserve_bytes(1).is_err());
    drop(output);
    assert!(
        f.scheduler.pools().reserve_bytes(1).is_err(),
        "chunk lost its persistent memory credit"
    );
    assert_eq!(chunk.as_ref(), f.data);
    drop(chunk);
    f.released();
}

#[test]
fn empty_sources_still_hold_one_disk_slot_until_the_reader_drops() {
    let f = Fixture::new(0);
    let output = runtime().block_on(f.future()).unwrap();
    assert!(output.reader().is_empty());
    assert!(SpoolWriter::create(&f.dir, &f.budget, 1, Some(0)).is_err());
    drop(output);
    f.released();
}

#[test]
fn dropping_an_unpolled_future_does_no_native_work() {
    let f = Fixture::new(100);
    drop(f.future());
    assert!(!f.dir.exists());
    assert_eq!(f.scheduler.metrics().delegated_runs, 0);
}

#[test]
fn dropping_the_public_future_cancels_a_dispatched_job_before_setup() {
    let mut f = Fixture::new(1024);
    // A single Fs worker plus a completed fence observes the cancelled job's
    // actual completion. Queue length alone excludes an already-running job.
    f.scheduler = Arc::new(IoScheduler::local_with_byte_limit_for_test(
        928,
        1,
        128 * 1024,
    ));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let calls = Arc::new(AtomicUsize::new(0));
    let (e, r, c) = (entered.clone(), release.clone(), calls.clone());
    f.scheduler
        .install_worker_start_test_hook(Arc::new(move || {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                e.wait();
                r.wait();
            }
        }));
    let mut future = Box::pin(f.future());
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    entered.wait();
    drop(future);
    release.wait();
    f.scheduler
        .pools()
        .run(crate::task::PoolKind::Fs, PriorityClass::Background, || ())
        .unwrap();
    assert!(!f.dir.exists(), "cancelled future still created a spool");
}

#[test]
fn dropping_an_unfinished_copy_returns_both_ledgers_without_publishing() {
    let f = Fixture::new(1024 * 1024 + 1);
    let Progress::More(copy) = f
        .begin()
        .unwrap()
        .step(&f.scheduler, &AtomicBool::new(false))
        .unwrap()
    else {
        panic!("expected unfinished copy")
    };
    assert!(f.scheduler.pools().reserve_bytes(1).is_err());
    assert!(SpoolWriter::create(&f.dir, &f.budget, 1, Some(0)).is_err());
    drop(copy);
    f.released();
}

#[test]
fn an_active_cancelled_job_retains_credit_until_its_native_step_finishes() {
    let f = Fixture::new(1024 * 1024 + 1);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let flag = Arc::new(AtomicBool::new(false));
    let guard = CancelOnDrop(flag.clone());
    let copy = f.begin().unwrap();
    let worker = f.scheduler.clone();
    let (e, r) = (entered.clone(), release.clone());
    let pending = f
        .scheduler
        .pools()
        .submit_async(
            crate::task::PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            move || {
                copy.step_with(&worker, &flag, |_| {
                    e.wait();
                    r.wait();
                })
            },
        )
        .unwrap();
    entered.wait();
    drop(guard);
    let memory_held = f.scheduler.pools().reserve_bytes(1).is_err();
    let disk_held = SpoolWriter::create(&f.dir, &f.budget, 1, Some(0)).is_err();
    release.wait();
    let result = runtime().block_on(pending).unwrap().unwrap();
    assert!(result.is_err());
    assert!(
        memory_held && disk_held,
        "active native work lost its credit"
    );
    f.released();
}

#[test]
fn invalid_reused_buffers_cannot_escape_the_memory_credit_bound() {
    let f = Fixture::new(1);
    for oversize_capacity in [false, true] {
        let mut buffer = Vec::with_capacity(if oversize_capacity {
            2 * FILE_CHUNK_BYTES
        } else {
            FILE_CHUNK_BYTES
        });
        buffer.resize(
            if oversize_capacity {
                FILE_CHUNK_BYTES
            } else {
                7
            },
            0,
        );
        let lease = f.scheduler.pools().reserve_bytes(128 * 1024).unwrap();
        let writer = SpoolWriter::create(&f.dir, &f.budget, 1, Some(0)).unwrap();
        assert!(writer.seal_with_buffer(1, buffer, lease).is_err());
        f.released();
    }
}
