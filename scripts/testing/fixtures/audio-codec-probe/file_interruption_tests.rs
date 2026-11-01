//! Recover a dropped wait without waiting for or reactivating its FS job.
use super::*;
use std::future::Future;
use std::sync::{Condvar, Mutex};
use tokio::sync::Notify;

struct Gate {
    reached: Notify,
    open: Mutex<bool>,
    changed: Condvar,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            reached: Notify::new(),
            open: Mutex::new(false),
            changed: Condvar::new(),
        })
    }
    fn hook(self: &Arc<Self>) -> Arc<dyn Fn() + Send + Sync> {
        let gate = self.clone();
        Arc::new(move || {
            gate.reached.notify_one();
            let lock = gate.open.lock().unwrap();
            drop(gate.changed.wait_while(lock, |open| !*open).unwrap());
        })
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
fn bytes() -> Vec<u8> {
    let path =
        std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join("1-tail.m4a");
    std::fs::read(path).unwrap()
}
async fn reader(owner: &migo_io::scheduler::IoScheduler) -> FileReader {
    let path =
        std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join("1-tail.m4a");
    let mut reader = FileReader::open(File::open(path).unwrap(), owner)
        .await
        .unwrap();
    reader.chunk_bytes = 16;
    reader
}

#[tokio::test]
async fn interrupted_jobs_keep_their_retired_epoch_before_and_after_disk_io() {
    for after_disk in [false, true] {
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let old = Gate::new();
        let fresh = Gate::new();
        let _release_old = Release(old.clone());
        let _release_fresh = Release(fresh.clone());
        let mut reader = reader(&owner).await;
        let backing = Arc::as_ptr(&reader.file);
        let done = Arc::new(Notify::new());
        let old_succeeded = Arc::new(std::sync::atomic::AtomicBool::new(false));
        reader.after_media_job = Some({
            let done = done.clone();
            let success = old_succeeded.clone();
            Arc::new(move |ok| {
                success.store(ok, Ordering::Release);
                done.notify_one();
            })
        });
        if after_disk {
            reader.after_io = Some(old.hook());
        } else {
            reader.before_media_io = Some(old.hook());
        }
        {
            let wait = reader.chunk();
            tokio::pin!(wait);
            tokio::select! { r = &mut wait => panic!("blocked old IO completed: {}", r.is_ok()), _ = old.reached.notified() => {} }
        }
        let retired = reader.last_read_epoch;
        assert_eq!(reader.read_active.load(Ordering::Acquire), 0);
        assert!(reader.chunk().await.is_err());
        reader.resume_after_interruption().unwrap();
        assert_eq!(reader.position, 0);
        assert_eq!(Arc::as_ptr(&reader.file), backing);
        reader.before_media_io = None;
        reader.after_io = Some(fresh.hook());
        reader.after_media_job = None;
        let active = reader.read_active.clone();
        let reads = reader.io_reads.clone();
        let chunk = {
            let next = reader.chunk();
            tokio::pin!(next);
            // Poll admission before releasing the old job. The new key must
            // remain active until the old job's completion has been observed.
            std::future::poll_fn(|cx| match next.as_mut().poll(cx) {
                std::task::Poll::Pending => std::task::Poll::Ready(()),
                std::task::Poll::Ready(_) => panic!("new IO was not held"),
            })
            .await;
            assert!(active.load(Ordering::Acquire) > retired);
            old.release();
            tokio::time::timeout(std::time::Duration::from_secs(2), done.notified())
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), fresh.reached.notified())
                .await
                .unwrap();
            let old_ok = old_succeeded.load(Ordering::Acquire);
            let count = reads.load(Ordering::Acquire);
            // Always release both workers before asserting, including mutants.
            fresh.release();
            let chunk = next.await.unwrap().unwrap();
            assert!(!old_ok, "retired IO became a successful completion");
            assert_eq!(
                count,
                if after_disk { 2 } else { 1 },
                "retired pre-IO job borrowed the new epoch"
            );
            chunk
        };
        assert_eq!(chunk.as_ref(), &bytes()[..16]);
        assert_eq!(reader.position, 16);
        assert_eq!(reader.media_requests(), 1);
        assert_eq!(reader.media_bytes(), 16);
        assert_eq!(reader.read_active.load(Ordering::Acquire), 0);
        assert_eq!(Arc::as_ptr(&reader.file), backing);
    }
}

#[tokio::test]
async fn interrupted_controls_and_closed_domains_cannot_be_recovered() {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let mut control = reader(&owner).await;
    control.delay = std::time::Duration::from_millis(80);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(5),
            control.read_exact_at(0, 8, Instant::now() + std::time::Duration::from_secs(1))
        )
        .await
        .is_err()
    );
    assert!(control.resume_after_interruption().is_err());
    assert!(control.chunk().await.is_err());
    let mut closed = reader(&owner).await;
    closed.delay = std::time::Duration::from_millis(80);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(5), closed.chunk())
            .await
            .is_err()
    );
    owner.close();
    assert!(closed.resume_after_interruption().is_err());
    assert!(closed.chunk().await.is_err());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(closed.io_reads.load(Ordering::Acquire), 0);
    assert_eq!(control.io_reads.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn interruption_after_a_delivered_prefix_preserves_cursor_and_accounting() {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let mut reader = reader(&owner).await;
    let backing = Arc::as_ptr(&reader.file);
    assert_eq!(
        reader.chunk().await.unwrap().unwrap().as_ref(),
        &bytes()[..16]
    );
    let gate = Gate::new();
    let _release = Release(gate.clone());
    let completed = Arc::new(Notify::new());
    reader.before_media_io = Some(gate.hook());
    reader.after_media_job = Some({
        let completed = completed.clone();
        Arc::new(move |ok| {
            assert!(!ok, "retired prefix read completed successfully");
            completed.notify_one();
        })
    });
    {
        let next = reader.chunk();
        tokio::pin!(next);
        tokio::select! { result = &mut next => panic!("held IO completed: {}", result.is_ok()), _ = gate.reached.notified() => {} }
    }
    reader.resume_after_interruption().unwrap();
    assert_eq!(reader.position, 16);
    assert_eq!(reader.media_requests(), 1);
    assert_eq!(reader.media_bytes(), 16);
    assert_eq!(Arc::as_ptr(&reader.file), backing);
    gate.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed.notified())
        .await
        .unwrap();
    reader.before_media_io = None;
    reader.after_media_job = None;
    assert_eq!(
        reader.chunk().await.unwrap().unwrap().as_ref(),
        &bytes()[16..32]
    );
    assert_eq!(reader.position, 32);
    assert_eq!(reader.media_requests(), 2);
    assert_eq!(reader.media_bytes(), 32);
}

#[tokio::test]
async fn failed_or_exhausted_file_operations_stay_terminal_without_submitting_io() {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    for case in 0..5 {
        let mut reader = reader(&owner).await;
        match case {
            0 => reader.last_read_epoch = u64::MAX,
            1 => reader.requests = u32::MAX,
            2 => reader.control_bytes = u64::MAX,
            3 => reader.media_reads = u64::MAX,
            _ => reader.media_bytes = u64::MAX,
        }
        let error = if case == 1 || case == 2 {
            reader
                .read_exact_at(0, 8, Instant::now() + std::time::Duration::from_secs(1))
                .await
                .err()
                .unwrap()
        } else {
            reader.chunk().await.err().unwrap()
        };
        if case == 0 {
            assert_eq!(error.code, ErrorCode::InputSaturated);
        }
        assert!(reader.resume_after_interruption().is_err(), "case {case}");
        assert!(reader.chunk().await.is_err());
        assert_eq!(reader.submits, 0);
        assert_eq!(reader.io_reads.load(Ordering::Acquire), 0);
    }
    let mut invalid = reader(&owner).await;
    assert!(invalid.start(u64::MAX, 2).is_err());
    assert!(invalid.resume_after_interruption().is_err());
}

#[tokio::test]
async fn idle_unpolled_and_eof_recovery_preserve_the_same_file_and_counters() {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let mut reader = reader(&owner).await;
    let backing = Arc::as_ptr(&reader.file);
    drop(reader.chunk());
    reader.resume_after_interruption().unwrap();
    assert_eq!(reader.submits, 0);
    assert_eq!(reader.last_read_epoch, 1);
    reader.start(8, 16).unwrap();
    assert_eq!(
        reader.chunk().await.unwrap().unwrap().as_ref(),
        &bytes()[8..24]
    );
    assert!(reader.chunk().await.unwrap().is_none());
    let requests = reader.media_requests();
    for _ in 0..3 {
        reader.resume_after_interruption().unwrap();
    }
    assert_eq!(reader.media_requests(), requests);
    assert_eq!(reader.position, 24);
    assert_eq!(Arc::as_ptr(&reader.file), backing);
    reader.start(0, 16).unwrap();
    assert_eq!(
        reader.chunk().await.unwrap().unwrap().as_ref(),
        &bytes()[..16]
    );
}

#[tokio::test]
async fn real_interruption_recovery_has_no_caller_heap_activity() {
    use migo_alloc_probe::thread_counts;
    let before = thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(
        thread_counts().allocation_events() > before.allocation_events(),
        "counting allocator missing"
    );
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let gate = Gate::new();
    let _release = Release(gate.clone());
    let mut reader = reader(&owner).await;
    let completed = Arc::new(Notify::new());
    let old_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));
    reader.before_media_io = Some(gate.hook());
    reader.after_media_job = Some({
        let done = completed.clone();
        let ok = old_ok.clone();
        Arc::new(move |result| {
            ok.store(result, Ordering::Release);
            done.notify_one();
        })
    });
    for _ in 0..16 {
        // Reset only the test gate between independent real cancelled IO jobs.
        *gate.open.lock().unwrap() = false;
        {
            let wait = reader.chunk();
            tokio::pin!(wait);
            tokio::select! { r=&mut wait=>panic!("held IO completed: {}",r.is_ok()),_ = gate.reached.notified()=>{} }
        }
        let before = thread_counts();
        reader.resume_after_interruption().unwrap();
        let after = thread_counts();
        gate.release();
        tokio::time::timeout(std::time::Duration::from_secs(2), completed.notified())
            .await
            .unwrap();
        assert_eq!(after.allocation_events() - before.allocation_events(), 0);
        assert_eq!(after.deallocations - before.deallocations, 0);
        assert!(!old_ok.load(Ordering::Acquire));
    }
    assert_eq!(reader.io_reads.load(Ordering::Acquire), 0);
    assert_eq!(reader.position, 0);
    println!("W09_FILE_READ_RECOVERY 16 real interruptions;0 caller heap events;0 disk reads");
}

#[tokio::test]
async fn indexed_source_uses_the_same_recovered_cursor_and_keeps_hard_cancel_terminal() {
    use crate::isomp4_file::IndexedFile;
    use crate::resampled_streaming::M4aResampled;
    use crate::streaming::{StreamingState, stream_channel};
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let mut reader = reader(&owner).await;
    reader.delay = std::time::Duration::from_millis(80);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(5), reader.chunk())
            .await
            .is_err()
    );
    reader.delay = std::time::Duration::ZERO;
    let mut source = IndexedFile::from_reader(reader);
    source.resume_after_read_interruption().unwrap();
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let SourceRead::Data(chunk) = Source::<M4aResampled>::read(&mut source, &tx, &state)
        .await
        .unwrap()
    else {
        panic!("missing recovered data")
    };
    assert_eq!(chunk.as_ref(), &bytes()[..16]);
    state.cancel();
    assert!(matches!(
        Source::<M4aResampled>::read(&mut source, &tx, &state)
            .await
            .unwrap(),
        SourceRead::Cancelled
    ));
}
