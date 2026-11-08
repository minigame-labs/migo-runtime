use super::*;
use migo_io::task::{PoolKind, PriorityClass};
use shared::audio_file::AudioFile;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
const WAIT: Duration = Duration::from_secs(5);
fn source(
    length: usize,
) -> (
    std::path::PathBuf,
    Vec<u8>,
    LocalAudioFile,
    Arc<dyn AudioFile>,
) {
    let (base, _, vfs) = sandbox("migo_file_loan");
    let data = (0..length)
        .map(|n| ((n * 41 + 23) % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(base.join("user/clip"), &data).unwrap();
    let context = file_context(&base);
    let file = runtime()
        .block_on(context.clone().source(vfs, "/user/clip".into()))
        .unwrap();
    (base, data, context, file)
}
fn read(
    file: &Arc<dyn AudioFile>,
    offset: u64,
    length: usize,
) -> shared::audio_file::AudioFileChunk {
    runtime()
        .block_on(
            file.clone()
                .read_at(
                    offset,
                    length,
                    Arc::new(AtomicBool::new(false)),
                    Instant::now() + WAIT,
                )
                .unwrap(),
        )
        .unwrap()
        .unwrap()
        .unwrap()
}
#[test]
fn file_capability_keeps_complete_bytes_and_the_captured_prefix_after_source_changes() {
    let (base, data, _, file) = source(131089);
    assert_eq!(file.prefix(), &data[..8]);
    assert_eq!(file.length(), data.len() as u64);
    fs::write(base.join("user/clip"), b"another object").unwrap();
    let mut offset = 0;
    while offset < data.len() {
        let n = (data.len() - offset).min(65536);
        let loan = read(&file, offset as u64, n);
        assert_eq!(
            loan.as_ref(),
            &data[offset..offset + n],
            "sealed file loan changed complete bytes"
        );
        offset += n;
    }
    let _ = fs::remove_dir_all(base);
}
#[test]
fn queued_file_read_keeps_admission_after_its_receiver_and_caller_drop() {
    let (base, _, vfs) = sandbox("migo_file_queued");
    fs::write(base.join("user/clip"), vec![7; 65537]).unwrap();
    let mut context = file_context(&base);
    context.scheduler =
        Arc::new(migo_io::scheduler::IoScheduler::isolated_for_admission_probe(998, 1, 128 * 1024));
    let file = runtime()
        .block_on(context.clone().source(vfs, "/user/clip".into()))
        .unwrap();
    let (started, waiting) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let block = context
        .scheduler
        .pools()
        .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, move || {
            started.send(()).unwrap();
            held.recv_timeout(WAIT).unwrap();
        })
        .unwrap();
    waiting.recv_timeout(WAIT).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let request = file
        .clone()
        .read_at(0, 65536, cancel.clone(), Instant::now() + WAIT)
        .unwrap();
    cancel.store(true, Ordering::Release);
    drop(request);
    drop(file);
    assert!(
        context.scheduler.pools().reserve_bytes(128 * 1024).is_err(),
        "queued file source returned physical credit before original Fs completion"
    );
    release.send(()).unwrap();
    let rt = runtime();
    rt.block_on(block).unwrap().unwrap();
    let barrier = context
        .scheduler
        .pools()
        .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, || ())
        .unwrap();
    rt.block_on(barrier).unwrap().unwrap();
    assert!(
        context.scheduler.pools().reserve_bytes(128 * 1024).is_ok(),
        "cancelled file source did not retire its original credit"
    );
    let _ = fs::remove_dir_all(base);
}
#[test]
fn returned_file_loan_keeps_original_source_credit_until_the_last_loan_drops() {
    let (base, _, vfs) = sandbox("migo_file_retained");
    fs::write(base.join("user/clip"), vec![9; 65537]).unwrap();
    let mut context = file_context(&base);
    context.scheduler =
        Arc::new(migo_io::scheduler::IoScheduler::isolated_for_admission_probe(999, 2, 128 * 1024));
    let file = runtime()
        .block_on(context.clone().source(vfs, "/user/clip".into()))
        .unwrap();
    let loan = read(&file, 0, 65536);
    let owner = Arc::downgrade(&file);
    drop(file);
    assert!(
        owner.upgrade().is_some(),
        "file loan released the sealed object before consumption"
    );
    assert!(
        context.scheduler.pools().reserve_bytes(128 * 1024).is_err(),
        "file loan lost the actual source owner"
    );
    assert!(loan.as_ref().iter().all(|b| *b == 9));
    drop(loan);
    assert!(
        context.scheduler.pools().reserve_bytes(128 * 1024).is_ok(),
        "last file loan retained source credit"
    );
    let _ = fs::remove_dir_all(base);
}
#[test]
fn invalid_cancelled_expired_and_closed_file_requests_cannot_become_successful_bytes() {
    let (base, _, context, file) = source(131089);
    let rt = runtime();
    for (offset, len) in [(0, 0), (0, 65537), (u64::MAX, 2), (131089, 1)] {
        assert!(
            file.clone()
                .read_at(
                    offset,
                    len,
                    Arc::new(AtomicBool::new(false)),
                    Instant::now() + WAIT
                )
                .is_err(),
            "file read ignored bounds before enqueue"
        );
    }
    assert!(
        file.clone()
            .read_at(0, 1, Arc::new(AtomicBool::new(true)), Instant::now() + WAIT)
            .is_err(),
        "cancelled file read was enqueued"
    );
    let expired = file
        .clone()
        .read_at(
            0,
            1,
            Arc::new(AtomicBool::new(false)),
            Instant::now() - Duration::from_millis(1),
        )
        .unwrap();
    assert!(
        rt.block_on(expired).unwrap().unwrap().is_err(),
        "expired queued file work returned successful bytes"
    );
    let notified = file.closed_signal().notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    context.scheduler.close();
    rt.block_on(notified);
    assert!(
        file.clone()
            .read_at(
                0,
                1,
                Arc::new(AtomicBool::new(false)),
                Instant::now() + WAIT
            )
            .is_err(),
        "closed file domain still accepted a read"
    );
    let _ = fs::remove_dir_all(base);
}
#[test]
fn actual_large_file_command_holds_a_sealed_capability_and_waits_for_native_readiness() {
    let (base, _, vfs) = sandbox("migo_file_large_command");
    let path = base.join("user/clip");
    let native = fs::File::create(&path).unwrap();
    native
        .set_len((MAX_ENCODED_AUDIO_BYTES + 1) as u64)
        .unwrap();
    drop(native);
    let context = file_context(&base);
    let (tx, rx) = hosted();
    let work = started_load(
        tx,
        InnerAudioSources {
            code_dir: None,
            vfs: Some(vfs),
            local_file: Some(context),
            admit_remote: |_: &Url| Ok(()),
        },
        1,
        "/user/clip".into(),
        &rx,
    );
    let rt = runtime();
    let task = rt.spawn(work);
    let message = rt.block_on(async {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(value) = rx.try_recv() {
                    break value;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    });
    let AudioCmd::InnerAudioLoadFile {
        source, load, resp, ..
    } = message
    else {
        panic!("large local source was materialized instead of handed to the native file stream")
    };
    assert_eq!(source.length(), (MAX_ENCODED_AUDIO_BYTES + 1) as u64);
    assert_eq!(source.queued_bytes(), 128 * 1024);
    assert_eq!(load.source_id(), 101);
    assert!(
        !task.is_finished(),
        "local source finished before native readiness"
    );
    resp.send(Err(EngineError::from_detail(
        ErrorCode::InvalidArgument,
        "bad format",
    )))
    .unwrap();
    assert_eq!(
        rt.block_on(task).unwrap().unwrap_err().class,
        CLASS_ENCODING_ERROR,
        "local file codec failure lost its original class"
    );
    drop(source);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn index_control_checks_bounds_and_admits_exact_storage_before_enqueuing() {
    let (base, _, vfs) = sandbox("migo_file_index_limits");
    let size = 2 * 1024 * 1024;
    fs::write(base.join("user/clip"), vec![13; size + 1]).unwrap();
    let mut context = file_context(&base);
    context.scheduler = Arc::new(
        migo_io::scheduler::IoScheduler::isolated_for_admission_probe(
            1012,
            1,
            (size + 128 * 1024) as u64,
        ),
    );
    let file = runtime()
        .block_on(context.clone().source(vfs, "/user/clip".into()))
        .unwrap();
    for (offset, length) in [(0, 0), (0, size + 1), (u64::MAX, 1), (size as u64, 2)] {
        assert!(
            file.clone()
                .read_control_at(
                    offset,
                    length,
                    Arc::new(AtomicBool::new(false)),
                    Instant::now() + WAIT
                )
                .is_err(),
            "index control ignored its bounds before enqueue"
        );
    }
    assert!(
        file.clone()
            .read_control_at(0, 1, Arc::new(AtomicBool::new(true)), Instant::now() + WAIT)
            .is_err(),
        "cancelled index was enqueued"
    );
    let expired = file
        .clone()
        .read_control_at(
            0,
            1,
            Arc::new(AtomicBool::new(false)),
            Instant::now() - Duration::from_millis(1),
        )
        .unwrap();
    assert!(
        runtime().block_on(expired).unwrap().unwrap().is_err(),
        "expired index work returned successful bytes"
    );
    let fill = context
        .scheduler
        .pools()
        .reserve_bytes(size as u64)
        .unwrap();
    let error = file
        .clone()
        .read_control_at(
            0,
            size,
            Arc::new(AtomicBool::new(false)),
            Instant::now() + WAIT,
        )
        .err()
        .expect("index allocation bypassed existing IO byte admission");
    assert_eq!(
        error.code,
        ErrorCode::InputSaturated,
        "index allocation bypassed existing IO byte admission"
    );
    drop(fill);
    let loan = runtime()
        .block_on(
            file.clone()
                .read_control_at(
                    0,
                    size,
                    Arc::new(AtomicBool::new(false)),
                    Instant::now() + WAIT,
                )
                .unwrap(),
        )
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(loan.as_ref().len(), size);
    assert!(loan.as_ref().iter().all(|v| *v == 13));
    assert!(
        context
            .scheduler
            .pools()
            .reserve_bytes(size as u64)
            .is_err(),
        "returned index loan released its storage credit"
    );
    drop(loan);
    assert!(context.scheduler.pools().reserve_bytes(size as u64).is_ok());
    let _ = fs::remove_dir_all(base);
}
#[test]
fn cancelled_queued_index_read_keeps_source_and_reserved_credit_until_real_fs_return() {
    let (base, _, vfs) = sandbox("migo_file_index_cancel");
    let size = 2 * 1024 * 1024;
    fs::write(base.join("user/clip"), vec![17; size + 1]).unwrap();
    let mut context = file_context(&base);
    context.scheduler = Arc::new(
        migo_io::scheduler::IoScheduler::isolated_for_admission_probe(
            1013,
            1,
            (size + 128 * 1024) as u64,
        ),
    );
    let file = runtime()
        .block_on(context.clone().source(vfs, "/user/clip".into()))
        .unwrap();
    let (started, waiting) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let block = context
        .scheduler
        .pools()
        .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, move || {
            started.send(()).unwrap();
            held.recv_timeout(WAIT).unwrap();
        })
        .unwrap();
    waiting.recv_timeout(WAIT).unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let read = file
        .clone()
        .read_control_at(0, size, flag.clone(), Instant::now() + WAIT)
        .unwrap();
    flag.store(true, Ordering::Release);
    drop((file, read));
    let charged = context
        .scheduler
        .pools()
        .reserve_bytes(size as u64)
        .is_err();
    release.send(()).unwrap();
    let rt = runtime();
    rt.block_on(block).unwrap().unwrap();
    let barrier = context
        .scheduler
        .pools()
        .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, || ())
        .unwrap();
    rt.block_on(barrier).unwrap().unwrap();
    assert!(
        charged,
        "queued index returned reserved storage before real Fs completion"
    );
    assert!(
        context
            .scheduler
            .pools()
            .reserve_bytes((size + 128 * 1024) as u64)
            .is_ok(),
        "cancelled index retained source or storage credit"
    );
    let _ = fs::remove_dir_all(base);
}
