//! Anchored /code files use the same codec/index/DSP/player as HTTP.
use super::*;
use crate::isomp4_file::{FileReader, IndexedFile};
use crate::isomp4_source::Reader;
use migo_io::scheduler::IoScheduler;
use shared::vfs::VirtualFS;
use std::fs::File;

#[path = "isomp4_directory_contracts.rs"]
mod directory;
#[path = "isomp4_package_contracts.rs"]
mod package;

fn vfs() -> Arc<VirtualFS> {
    let code = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap());
    let base = code.parent().unwrap().join("sandbox");
    Arc::new(VirtualFS::new(
        code,
        base.join("user"),
        base.join("cache"),
        base.join("tmp"),
    ))
}
async fn produce_file<const B: bool>(
    name: String,
    target: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    steps: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
) {
    let owner = IoScheduler::new(0);
    let io_thread = std::thread::current().id();
    let prepared = transport::while_active(
        &tx,
        &state,
        FileReader::open_code(vfs(), format!("/code/{name}"), &owner),
    )
    .await;
    let reader = match prepared {
        None => return,
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            transport::send(&tx, &state, StreamMsg::Error(e.to_string())).await;
            return;
        }
    };
    let trace = reader.io_reads.clone();
    let length = reader.len();
    let result = transport::run_source(
        owner.audio_cpu(),
        IndexedFile::from_reader(reader),
        Some(length),
        &tx,
        &state,
        move |length| Observed::<B> {
            decoder: Batched::with_object_len(target, length),
            steps,
            io_thread,
        },
    )
    .await;
    transport::publish_result(&tx, &state, result).await;
    reads.store(trace.load(Ordering::Acquire), Ordering::Release);
}
async fn receive_file<const B: bool>(name: &str, target: u32) -> (Received, usize) {
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let reads = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(produce_file::<B>(
        name.to_owned(),
        target,
        tx,
        state.clone(),
        Arc::new(AtomicUsize::new(0)),
        reads.clone(),
    ));
    let mut got = Received {
        pcm: Vec::new(),
        ready: 0,
        done: 0,
        errors: Vec::new(),
    };
    while let Some(message) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        match message {
            StreamMsg::Ready { sample_rate, .. } => {
                assert_eq!(sample_rate, target);
                got.ready += 1;
            }
            StreamMsg::Samples(pcm) => {
                assert_eq!((got.ready, got.done, got.errors.len()), (1, 0, 0));
                got.pcm.extend_from_slice(&pcm);
            }
            StreamMsg::Done => got.done += 1,
            StreamMsg::Error(e) => got.errors.push(e),
        }
    }
    task.await.unwrap();
    assert_eq!(
        state.download_complete.load(Ordering::Acquire),
        got.done == 1
    );
    (got, reads.load(Ordering::Acquire))
}
#[tokio::test]
async fn anchored_file_layouts_and_resampling_equal_whole_pcm() {
    let mut comparisons = 0;
    for name in [
        "0-front.m4a",
        "1-tail.m4a",
        "2-reorder.m4a",
        "2-reverse.m4a",
        "2-tail-multi.m4a",
        "1-metadata.m4a",
        "2-buffered-miss.m4a",
        "2-coalesced-rewind.m4a",
    ] {
        let data = fixture(name);
        let audio = candidate::decode(&data, 16_777_216).unwrap();
        for target in [
            audio.sample_rate,
            if audio.sample_rate == 48000 {
                44100
            } else {
                48000
            },
        ] {
            let (expected, _) = reference(&data, target);
            for cached in [false, true] {
                let (got, _) = if cached {
                    receive_file::<true>(name, target).await
                } else {
                    receive_file::<false>(name, target).await
                };
                assert_eq!(
                    (got.ready, got.done, got.errors.len()),
                    (1, 1, 0),
                    "{name}/{target}/{cached}: {:?}",
                    got.errors
                );
                assert_eq!(got.pcm, expected, "{name}/{target}/{cached}");
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 32);
    println!("W09_M4A_LOCAL_PCM {comparisons} exact comparisons");
}
#[tokio::test]
async fn cached_local_assets_avoid_extra_file_index_and_rewind_operations() {
    for name in ["1-tail.m4a", "2-tail-multi.m4a"] {
        let (a, legacy) = receive_file::<false>(name, 44100).await;
        let (b, cached) = receive_file::<true>(name, 44100).await;
        assert!(a.errors.is_empty() && b.errors.is_empty());
        assert_eq!(a.pcm, b.pcm);
        assert_eq!((a.done, b.done), (1, 1));
        assert_eq!(cached, 2, "one encoded read plus verified EOF");
        assert!(legacy > cached);
        println!("W09_M4A_LOCAL_READS {name} {legacy}->{cached}");
    }
}
#[tokio::test]
async fn cancelled_file_wait_leaves_io_worker_free_and_reader_failed() {
    let owner = IoScheduler::new(0);
    let mut reader = FileReader::open_code(vfs(), "/code/1-tail.m4a".into(), &owner)
        .await
        .unwrap();
    reader.delay = Duration::from_millis(250);
    let count = reader.io_reads.clone();
    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_millis(20), reader.chunk()).await;
    assert!(result.is_err());
    assert!(
        started.elapsed() < Duration::from_millis(150),
        "file IO ran inline"
    );
    assert!(reader.chunk().await.is_err());
    tokio::time::sleep(Duration::from_millis(280)).await;
    assert_eq!(
        count.load(Ordering::Acquire),
        0,
        "cancelled queued read still performed IO"
    );
}
#[tokio::test]
async fn one_absolute_file_deadline_and_closed_domain_reject_new_and_queued_io() {
    let owner = IoScheduler::new(0);
    let mut reader = FileReader::open_code(vfs(), "/code/1-tail.m4a".into(), &owner)
        .await
        .unwrap();
    let count = reader.io_reads.clone();
    assert!(
        reader
            .read_exact_at(0, 8, Instant::now() - Duration::from_millis(1))
            .await
            .is_err()
    );
    assert_eq!(count.load(Ordering::Acquire), 0);
    assert_eq!(
        reader.submits, 0,
        "expired request still entered the FS queue"
    );
    assert!(reader.chunk().await.is_err());
    let mut reader = FileReader::open_code(vfs(), "/code/1-tail.m4a".into(), &owner)
        .await
        .unwrap();
    reader.delay = Duration::from_millis(200);
    let count = reader.io_reads.clone();
    let operation = reader.chunk();
    tokio::pin!(operation);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut operation)
            .await
            .is_err()
    );
    owner.close();
    assert!(operation.await.is_err());
    assert_eq!(count.load(Ordering::Acquire), 0);
    assert!(
        FileReader::open_code(vfs(), "/code/1-tail.m4a".into(), &owner)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn file_controls_reject_caps_overflow_and_mutation_instead_of_eof() {
    let owner = IoScheduler::new(0);
    for (offset, len) in [(0, 2 * 1024 * 1024 + 1), (u64::MAX, 16)] {
        let mut reader = FileReader::open_code(vfs(), "/code/1-tail.m4a".into(), &owner)
            .await
            .unwrap();
        assert!(
            reader
                .read_exact_at(offset, len, Instant::now() + Duration::from_secs(1))
                .await
                .is_err()
        );
        assert!(reader.chunk().await.is_err());
        assert_eq!(reader.io_reads.load(Ordering::Acquire), 0);
    }
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap())
        .parent()
        .unwrap()
        .join("sandbox/tmp/changed.m4a");
    std::fs::write(&path, fixture("1-tail.m4a")).unwrap();
    let mut reader = FileReader::open(File::open(&path).unwrap(), &owner)
        .await
        .unwrap();
    let chunk = reader.chunk().await.unwrap().unwrap();
    assert!(!chunk.as_ref().is_empty());
    drop(chunk);
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(2)
        .unwrap();
    assert!(
        reader.chunk().await.is_err(),
        "file change became successful EOF"
    );
    assert!(reader.chunk().await.is_err());
}
#[tokio::test]
async fn local_source_admission_uses_vfs_anchored_readonly_code_handles() {
    let owner = IoScheduler::new(0);
    for path in [
        "/user/clip.m4a",
        "/code/../user/clip.m4a",
        "/code/missing.m4a",
    ] {
        assert!(
            IndexedFile::open_code(vfs(), path.into(), &owner)
                .await
                .is_err()
        );
    }
    #[cfg(unix)]
    {
        let root = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap());
        let link = root.join("unsafe-link.m4a");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(root.join("1-tail.m4a"), &link).unwrap();
        assert!(
            IndexedFile::open_code(vfs(), "/code/unsafe-link.m4a".into(), &owner)
                .await
                .is_err()
        );
        std::fs::remove_file(link).unwrap();
    }
}
#[tokio::test]
async fn local_player_keeps_pause_backpressure_pcm_and_exact_ended() {
    for (name, cached) in [("2-reverse.m4a", false), ("2-tail-multi.m4a", true)] {
        for target in [48000, 44100] {
            let (expected, channels) = reference(&fixture(name), target);
            let frames = expected.len() / channels as usize;
            let state = StreamingState::new();
            let (tx, rx) = stream_channel();
            let steps = Arc::new(AtomicUsize::new(0));
            let task = if cached {
                tokio::spawn(produce_file::<true>(
                    name.into(),
                    target,
                    tx,
                    state.clone(),
                    steps.clone(),
                    Arc::new(AtomicUsize::new(0)),
                ))
            } else {
                tokio::spawn(produce_file::<false>(
                    name.into(),
                    target,
                    tx,
                    state.clone(),
                    steps.clone(),
                    Arc::new(AtomicUsize::new(0)),
                ))
            };
            if cached {
                consume_player::<true>(
                    name,
                    format!("/code/{name}"),
                    target,
                    true,
                    false,
                    expected,
                    channels,
                    frames,
                    state,
                    steps,
                    task,
                    rx,
                )
                .await;
            } else {
                consume_player::<false>(
                    name,
                    format!("/code/{name}"),
                    target,
                    true,
                    false,
                    expected,
                    channels,
                    frames,
                    state,
                    steps,
                    task,
                    rx,
                )
                .await;
            }
        }
    }
}
#[tokio::test]
async fn local_long_player_exceeds_cache_quota_without_accumulating_pcm() {
    let target = 16000;
    let state = StreamingState::new();
    let (tx, rx) = stream_channel();
    let steps = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(produce_file::<true>(
        "long-front.m4a".into(),
        target,
        tx,
        state.clone(),
        steps.clone(),
        Arc::new(AtomicUsize::new(0)),
    ));
    consume_player::<true>(
        "long-front.m4a",
        "/code/long-front.m4a".into(),
        target,
        true,
        true,
        Vec::new(),
        1,
        35_225_600,
        state,
        steps,
        task,
        rx,
    )
    .await;
}

#[tokio::test]
async fn domain_close_after_disk_read_discards_late_data() {
    let owner = IoScheduler::new(0);
    let mut reader = FileReader::open_code(vfs(), "/code/1-tail.m4a".into(), &owner)
        .await
        .unwrap();
    let domain = owner.domain();
    reader.after_io = Some(Arc::new(move || domain.close_all()));
    assert!(
        reader.chunk().await.is_err(),
        "late file data escaped domain close"
    );
    assert_eq!(reader.io_reads.load(Ordering::Acquire), 1);
    assert!(reader.chunk().await.is_err());
}

#[tokio::test]
async fn expired_queued_control_skips_disk_even_without_polling_timeout() {
    let owner = IoScheduler::new(0);
    let mut reader = FileReader::open_code(vfs(), "/code/1-tail.m4a".into(), &owner)
        .await
        .unwrap();
    reader.delay = Duration::from_millis(100);
    let reads = reader.io_reads.clone();
    let mut operation =
        Box::pin(reader.read_exact_at(0, 8, Instant::now() + Duration::from_millis(20)));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(std::future::Future::poll(operation.as_mut(), &mut cx).is_pending());
    // Deliberately stop this executor from polling its timer. FS owns the deadline too.
    std::thread::sleep(Duration::from_millis(180));
    assert!(operation.await.is_err());
    assert_eq!(reads.load(Ordering::Acquire), 0, "expired job touched disk");
    assert!(reader.chunk().await.is_err());
}

#[tokio::test]
async fn late_control_completion_is_rejected_without_polling_timeout() {
    let owner = IoScheduler::new(0);
    let mut reader = FileReader::open_code(vfs(), "/code/1-tail.m4a".into(), &owner)
        .await
        .unwrap();
    reader.after_io = Some(Arc::new(|| std::thread::sleep(Duration::from_millis(100))));
    let reads = reader.io_reads.clone();
    let mut operation =
        Box::pin(reader.read_exact_at(0, 8, Instant::now() + Duration::from_millis(20)));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(std::future::Future::poll(operation.as_mut(), &mut cx).is_pending());
    std::thread::sleep(Duration::from_millis(180));
    assert!(
        operation.await.is_err(),
        "late file completion became successful"
    );
    assert_eq!(reads.load(Ordering::Acquire), 1);
    assert!(reader.chunk().await.is_err());
}
