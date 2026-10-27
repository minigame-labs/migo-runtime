//! Actual pinned package -> shared source/CPU/codec/DSP/player contracts.
use super::*;
use migo_io::task::{PoolKind, PriorityClass};
use shared::vfs::{MountTable, PackSource, PackageWriter, mount::PinnedPackRead};
use std::sync::{Condvar, Mutex, OnceLock};

fn mounts() -> Arc<MountTable> {
    static MOUNTS: OnceLock<Arc<MountTable>> = OnceLock::new();
    MOUNTS
        .get_or_init(|| {
            let dir = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap());
            let path = dir.parent().unwrap().join("sandbox/cache/assets.mpkg");
            let mut writer =
                PackageWriter::new(std::io::BufWriter::new(File::create(&path).unwrap())).unwrap();
            for name in [
                "0-front.m4a",
                "1-tail.m4a",
                "2-reorder.m4a",
                "2-reverse.m4a",
                "2-tail-multi.m4a",
                "1-metadata.m4a",
                "2-buffered-miss.m4a",
                "2-coalesced-rewind.m4a",
                "long-front.m4a",
            ] {
                writer.add_entry(name, &fixture(name)).unwrap();
            }
            writer.finish("game", "1").unwrap();
            let mt = Arc::new(MountTable::new(dir));
            mt.swap_base(Arc::new(PackSource::open(&path, "game", "1").unwrap()));
            mt
        })
        .clone()
}
fn pin(name: &str) -> PinnedPackRead {
    let mt = mounts();
    mt.pin_pack_read(name, mt.generation()).unwrap()
}
async fn produce<const B: bool>(
    pin: PinnedPackRead,
    target: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    steps: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
) {
    let owner = IoScheduler::new(0);
    let io_thread = std::thread::current().id();
    let prepared =
        transport::while_active(&tx, &state, FileReader::open_package(pin, &owner)).await;
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
async fn receive<const B: bool>(pin: PinnedPackRead, target: u32) -> (Received, usize) {
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let reads = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(produce::<B>(
        pin,
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
async fn package_layouts_and_resampling_equal_whole_pcm() {
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
        let rate = candidate::decode(&data, 16_777_216).unwrap().sample_rate;
        for target in [rate, if rate == 48000 { 44100 } else { 48000 }] {
            let (expected, _) = reference(&data, target);
            for cached in [false, true] {
                let (got, _) = if cached {
                    receive::<true>(pin(name), target).await
                } else {
                    receive::<false>(pin(name), target).await
                };
                assert_eq!(
                    (got.ready, got.done, got.errors.len()),
                    (1, 1, 0),
                    "{name}/{target}/{cached}: {:?}",
                    got.errors
                );
                assert_eq!(got.pcm, expected);
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 32);
    println!("W09_M4A_PACKAGE_PCM {comparisons} exact comparisons");
}
#[tokio::test]
async fn cached_package_assets_reuse_the_shared_encoded_window() {
    for name in ["1-tail.m4a", "2-tail-multi.m4a"] {
        let (a, legacy) = receive::<false>(pin(name), 44100).await;
        let (b, cached) = receive::<true>(pin(name), 44100).await;
        assert!(a.errors.is_empty() && b.errors.is_empty());
        assert_eq!(a.pcm, b.pcm);
        assert_eq!((a.done, b.done), (1, 1));
        assert_eq!(cached, 2);
        assert!(legacy > cached);
        println!("W09_M4A_PACKAGE_JOBS {name} {legacy}->{cached}");
    }
}
#[tokio::test]
async fn package_player_preserves_pause_pcm_and_exact_ended() {
    for (name, cached) in [("2-reverse.m4a", false), ("2-tail-multi.m4a", true)] {
        for target in [48000, 44100] {
            let (expected, channels) = reference(&fixture(name), target);
            let frames = expected.len() / channels as usize;
            let state = StreamingState::new();
            let (tx, rx) = stream_channel();
            let steps = Arc::new(AtomicUsize::new(0));
            let task = if cached {
                tokio::spawn(produce::<true>(
                    pin(name),
                    target,
                    tx,
                    state.clone(),
                    steps.clone(),
                    Arc::new(AtomicUsize::new(0)),
                ))
            } else {
                tokio::spawn(produce::<false>(
                    pin(name),
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
                    format!("package://{name}"),
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
                    format!("package://{name}"),
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
async fn package_long_player_exceeds_cache_quota_without_accumulating_pcm() {
    let state = StreamingState::new();
    let (tx, rx) = stream_channel();
    let steps = Arc::new(AtomicUsize::new(0));
    let target = 16000;
    let task = tokio::spawn(produce::<true>(
        pin("long-front.m4a"),
        target,
        tx,
        state.clone(),
        steps.clone(),
        Arc::new(AtomicUsize::new(0)),
    ));
    consume_player::<true>(
        "long-front.m4a",
        "package://long-front.m4a".into(),
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
async fn package_admission_keeps_the_selected_mount_through_real_decode() {
    let dir = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap())
        .parent()
        .unwrap()
        .join("sandbox/cache");
    let mt = MountTable::new(dir.clone());
    for (version, name) in [("old", "0-front.m4a"), ("new", "1-tail.m4a")] {
        let path = dir.join(format!("{version}.mpkg"));
        let mut w = PackageWriter::new(File::create(path).unwrap()).unwrap();
        w.add_entry("clip.m4a", &fixture(name)).unwrap();
        w.finish("game", version).unwrap();
    }
    mt.swap_base(Arc::new(
        PackSource::open(&dir.join("old.mpkg"), "game", "old").unwrap(),
    ));
    let generation = mt.generation();
    let admitted = mt.pin_pack_read("clip.m4a", generation).unwrap();
    mt.swap_base(Arc::new(
        PackSource::open(&dir.join("new.mpkg"), "game", "new").unwrap(),
    ));
    assert!(mt.pin_pack_read("clip.m4a", generation).is_err());
    for (p, name) in [
        (admitted, "0-front.m4a"),
        (
            mt.pin_pack_read("clip.m4a", mt.generation()).unwrap(),
            "1-tail.m4a",
        ),
    ] {
        let (got, _) = receive::<false>(p, 44100).await;
        assert_eq!((got.ready, got.done, got.errors.len()), (1, 1, 0));
        assert_eq!(got.pcm, reference(&fixture(name), 44100).0);
    }
}

pub(super) struct Release(Arc<(Mutex<bool>, Condvar)>);
impl Drop for Release {
    fn drop(&mut self) {
        let (lock, cv) = &*self.0;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
}
pub(super) async fn block_pack(owner: &IoScheduler) -> Release {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Release(gate.clone());
    for _ in 0..2 {
        let gate = gate.clone();
        let started = started.clone();
        drop(
            owner
                .pools()
                .submit_async(PoolKind::Pack, PriorityClass::ForegroundAsync, move || {
                    started.notify_one();
                    let (lock, cv) = &*gate;
                    let mut release = lock.lock().unwrap();
                    while !*release {
                        release = cv.wait(release).unwrap();
                    }
                })
                .unwrap(),
        );
    }
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    release
}
#[tokio::test]
async fn package_setup_and_media_obey_the_actual_pack_class_cap() {
    let selected = pin("1-tail.m4a");
    let owner = IoScheduler::new(0);
    let release = block_pack(&owner).await;
    let mut opening = Box::pin(FileReader::open_package(selected, &owner));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut opening)
            .await
            .is_err(),
        "package setup bypassed Pack cap"
    );
    drop(release);
    let mut reader = opening.await.unwrap();
    let release = block_pack(&owner).await;
    let count = reader.io_reads.clone();
    let mut chunk = Box::pin(reader.chunk());
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut chunk)
            .await
            .is_err(),
        "package media bypassed Pack cap"
    );
    assert_eq!(count.load(Ordering::Acquire), 0);
    drop(release);
    assert!(chunk.await.unwrap().is_some());
}
#[tokio::test]
async fn package_media_uses_reserved_credit_but_controls_need_transient_credit() {
    let owner = IoScheduler::new(0);
    let mut reader = FileReader::open_package(pin("1-tail.m4a"), &owner)
        .await
        .unwrap();
    let available = match owner.pools().reserve_bytes(u64::MAX) {
        Err(migo_io::pools::ByteAdmitError::BytesExhausted { available, .. }) => available,
        _ => panic!("expected byte budget probe"),
    };
    let _hold = owner.pools().reserve_bytes(available).unwrap();
    drop(reader.chunk().await.expect("media was charged twice"));
    assert!(
        reader
            .read_exact_at(0, 8, Instant::now() + Duration::from_secs(1))
            .await
            .is_err(),
        "control bypassed transient byte admission"
    );
}
#[tokio::test]
async fn package_cancel_and_domain_close_discard_queued_and_late_results() {
    let owner = IoScheduler::new(0);
    let mut reader = FileReader::open_package(pin("1-tail.m4a"), &owner)
        .await
        .unwrap();
    reader.delay = Duration::from_millis(200);
    let reads = reader.io_reads.clone();
    let start = Instant::now();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), reader.chunk())
            .await
            .is_err()
    );
    assert!(start.elapsed() < Duration::from_millis(150));
    assert!(reader.chunk().await.is_err());
    tokio::time::sleep(Duration::from_millis(230)).await;
    assert_eq!(reads.load(Ordering::Acquire), 0);
    let mut reader = FileReader::open_package(pin("1-tail.m4a"), &owner)
        .await
        .unwrap();
    let domain = owner.domain();
    reader.after_io = Some(Arc::new(move || domain.close_all()));
    assert!(reader.chunk().await.is_err());
    assert_eq!(reader.io_reads.load(Ordering::Acquire), 1);
    assert!(reader.chunk().await.is_err());
}
