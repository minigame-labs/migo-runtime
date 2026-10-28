use crate::{
    format_source::{self, Format, Limits},
    local_source::{DirectoryOwnership, FileReader, SnapshotOptions},
    local_source_contracts as local,
    streaming::{StreamMsg, StreamingState, stream_channel},
    streaming_transport,
};
use migo_io::{scheduler::IoScheduler, spool::SpoolBudget};
use shared::vfs::{MountTable, PackSource, PackageWriter};
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn root() -> PathBuf {
    let p = local::assets().parent().unwrap().join(format!(
        "format-source-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}
async fn open(data: &[u8], route: usize, owner: Arc<IoScheduler>) -> FileReader {
    let root = root();
    let dir = root.join("code");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("wrong.mp3"), data).unwrap();
    let mt = MountTable::new(dir);
    if route == 2 {
        let path = root.join("clip.mpkg");
        let mut w = PackageWriter::new(File::create(&path).unwrap()).unwrap();
        w.add_entry("wrong.mp3", data).unwrap();
        w.finish("game", "1").unwrap();
        mt.swap_base(Arc::new(PackSource::open(&path, "game", "1").unwrap()));
    }
    FileReader::open_asset(
        mt.pin_asset_read("wrong.mp3", mt.generation()).unwrap(),
        if route == 1 {
            DirectoryOwnership::Snapshot
        } else {
            DirectoryOwnership::Immutable
        },
        owner,
        SnapshotOptions {
            dir: root.join("private"),
            budget: SpoolBudget::new((data.len() as u64).max(1), 1).unwrap(),
            max_bytes: (data.len() as u64).max(1),
            deadline: Instant::now() + Duration::from_secs(10),
        },
    )
    .await
    .unwrap()
}
async fn receive(data: &[u8], route: usize, target: u32, fragment: usize) -> local::Received {
    let owner = Arc::new(IoScheduler::new(0));
    let mut reader = open(data, route, owner.clone()).await;
    reader.chunk_bytes = fragment;
    let owner = Arc::try_unwrap(owner).ok().unwrap();
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    let s = state.clone();
    let task = tokio::spawn(async move {
        let result = format_source::run(
            &owner,
            reader,
            Instant::now() + Duration::from_secs(10),
            target,
            &tx,
            &s,
        )
        .await;
        streaming_transport::publish_result(&tx, &s, result).await;
    });
    let mut got = local::Received {
        pcm: Vec::new(),
        ready: 0,
        done: 0,
        errors: Vec::new(),
    };
    while let Some(msg) = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
    {
        match msg {
            StreamMsg::Ready { sample_rate, .. } => {
                assert_eq!(sample_rate, target);
                got.ready += 1
            }
            StreamMsg::Samples(p) => {
                assert_eq!((got.ready, got.done, got.errors.len()), (1, 0, 0));
                got.pcm.extend_from_slice(&p)
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
    got
}
fn m4a(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name)).unwrap()
}
#[tokio::test]
async fn library_detection_selects_all_five_codecs_and_preserves_exact_pcm() {
    let mut assets = Vec::new();
    for ext in ["mp3", "wav", "aac", "ogg"] {
        for i in [0, 3] {
            assets.push(std::fs::read(local::assets().join(format!("{i}.{ext}"))).unwrap());
        }
    }
    for name in [
        "0-front.m4a",
        "1-tail.m4a",
        "2-reverse.m4a",
        "1-metadata.m4a",
    ] {
        assets.push(m4a(name));
    }
    let mut count = 0;
    for data in assets {
        let rate = crate::candidate::decode(&data, 16_777_216)
            .unwrap()
            .sample_rate;
        for target in [rate, if rate == 48000 { 44100 } else { 48000 }] {
            let (expected, _) = local::reference(&data, target);
            for route in 0..3 {
                for fragment in [137, 65536] {
                    let got = receive(&data, route, target, fragment).await;
                    assert_eq!(
                        (got.ready, got.done, got.errors.len()),
                        (1, 1, 0),
                        "{:?}",
                        got.errors
                    );
                    assert_eq!(got.pcm, expected);
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 144);
    println!("W09_FORMAT_AUTO_PCM {count} exact comparisons");
}
#[tokio::test]
async fn identification_does_not_construct_a_tail_index_or_consume_playback() {
    let owner = Arc::new(IoScheduler::new(0));
    // A complete ftyp with no movie can be identified, but cannot open as a
    // complete MP4 demuxer. This makes eager reader construction observable.
    let data = b"\0\0\0\x10ftypM4A \0\0\0\0";
    let mut reader = open(data, 0, owner.clone()).await;
    let found = format_source::probe(
        &mut reader,
        Instant::now() + Duration::from_secs(2),
        Limits {
            max_seek_bytes: 0,
            ..Limits::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(found.format, Format::M4a);
    assert_eq!(found.seek_bytes, 0);
    assert_eq!(reader.chunk().await.unwrap().unwrap().as_ref(), data);
    let data = m4a("1-tail.m4a");
    let mut reader = open(&data, 0, owner.clone()).await;
    let found = format_source::probe(
        &mut reader,
        Instant::now() + Duration::from_secs(2),
        Limits {
            max_seek_bytes: 0,
            ..Limits::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(found.format, Format::M4a);
    assert!(found.bytes <= 1024);
}
fn tagged(data: &[u8], size: usize) -> Vec<u8> {
    let mut v = b"ID3\x04\0\0".to_vec();
    v.extend_from_slice(&[
        ((size >> 21) & 127) as u8,
        ((size >> 14) & 127) as u8,
        ((size >> 7) & 127) as u8,
        (size & 127) as u8,
    ]);
    v.resize(10 + size, 0);
    v.extend_from_slice(data);
    v
}
#[tokio::test]
async fn metadata_skipping_reuses_library_seek_and_never_guesses_id3_means_mp3() {
    let mp3 = std::fs::read(local::assets().join("0.mp3")).unwrap();
    let data = tagged(&mp3, 6 * 1024 * 1024);
    let owner = Arc::new(IoScheduler::new(0));
    let mut reader = open(&data, 0, owner.clone()).await;
    let found = format_source::probe(
        &mut reader,
        Instant::now() + Duration::from_secs(2),
        Limits::default(),
    )
    .await
    .unwrap();
    assert_eq!(found.format, Format::Mp3);
    assert!(found.seek_bytes > 5 * 1024 * 1024);
    assert!(found.bytes < 128 * 1024);
    assert!(found.reads < 32);
    let aac = std::fs::read(local::assets().join("0.aac")).unwrap();
    let data = tagged(&aac, 37);
    let mut reader = open(&data, 0, owner.clone()).await;
    assert_eq!(
        format_source::probe(
            &mut reader,
            Instant::now() + Duration::from_secs(2),
            Limits::default()
        )
        .await
        .unwrap()
        .format,
        Format::Adts
    );
    let (expected, _) = local::reference(&data, 48000);
    let got = receive(&data, 0, 48000, 137).await;
    assert_eq!((got.ready, got.done, got.errors.len()), (1, 1, 0));
    assert_eq!(got.pcm, expected);
}
#[tokio::test]
async fn read_seek_and_deadline_limits_fail_sticky_without_codec_output() {
    let data = tagged(
        &std::fs::read(local::assets().join("0.mp3")).unwrap(),
        256 * 1024,
    );
    for limits in [
        Limits {
            max_read_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_reads: 1,
            ..Limits::default()
        },
        Limits {
            max_seek_bytes: 1,
            ..Limits::default()
        },
    ] {
        let owner = Arc::new(IoScheduler::new(0));
        let mut reader = open(&data, 0, owner.clone()).await;
        assert!(
            format_source::probe(&mut reader, Instant::now() + Duration::from_secs(2), limits)
                .await
                .is_err()
        );
        assert!(reader.chunk().await.is_err());
        assert!(
            format_source::probe(
                &mut reader,
                Instant::now() + Duration::from_secs(2),
                Limits::default()
            )
            .await
            .is_err()
        );
    }
    let owner = Arc::new(IoScheduler::new(0));
    let mut reader = open(&data, 0, owner.clone()).await;
    assert!(
        format_source::probe(&mut reader, Instant::now(), Limits::default())
            .await
            .is_err()
    );
    assert!(reader.chunk().await.is_err());
}
#[tokio::test]
async fn probe_memory_credit_is_reserved_before_work_and_returned_after_it() {
    let data = std::fs::read(local::assets().join("0.mp3")).unwrap();
    for bytes in [128 * 1024 - 1, 128 * 1024] {
        let owner = Arc::new(IoScheduler::new(0));
        let mut reader = open(&data, 0, owner.clone()).await;
        let probe_owner = IoScheduler::isolated_for_admission_probe(910, 2, bytes);
        reader.pools = probe_owner.pools().clone();
        let found = format_source::probe(
            &mut reader,
            Instant::now() + Duration::from_secs(2),
            Limits::default(),
        )
        .await;
        assert_eq!(found.is_ok(), bytes == 128 * 1024);
        assert!(
            reader.pools.reserve_bytes(bytes).is_ok(),
            "probe credit leaked"
        );
        if found.is_err() {
            assert!(reader.chunk().await.is_err());
        }
    }
}
#[tokio::test]
async fn queued_probe_obeys_pack_cancellation_and_deadline_without_repolling() {
    use migo_io::task::{PoolKind, PriorityClass};
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let data = std::fs::read(local::assets().join("0.mp3")).unwrap();
    for cancel in [false, true] {
        let owner = Arc::new(IoScheduler::isolated_for_admission_probe(
            911,
            2,
            2 * 1024 * 1024,
        ));
        let mut reader = open(&data, 2, owner.clone()).await;
        let (started, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let blocker = owner
            .pools()
            .submit_async(PoolKind::Pack, PriorityClass::ForegroundAsync, move || {
                started.send(()).unwrap();
                wait.recv().unwrap();
            })
            .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let pools = reader.pools.clone();
        let lease = pools.reserve_bytes(1).unwrap();
        drop(lease);
        let deadline = Instant::now() + Duration::from_millis(30);
        let mut pending = Box::pin(format_source::probe(
            &mut reader,
            deadline,
            Limits::default(),
        ));
        assert!(matches!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        // A real Fs completion makes a wrong class observable, without polling
        // the probe future to drive its timeout or teardown to close its domain.
        owner
            .pools()
            .run(PoolKind::Fs, PriorityClass::Background, || ())
            .unwrap();
        let package =
            migo_io::read_at_package::ReadAtPackage::reservation_bytes(Default::default()).unwrap();
        let spare = 2 * 1024 * 1024 - package - 128 * 1024;
        assert!(
            pools.reserve_bytes(spare + 1).is_err(),
            "probe ran outside blocked Pack"
        );
        if cancel {
            drop(pending);
        } else {
            std::thread::sleep(Duration::from_millis(60));
            release.send(()).unwrap();
            blocker.await.unwrap().unwrap();
            owner
                .pools()
                .run(PoolKind::Pack, PriorityClass::Background, || ())
                .unwrap();
            assert!(pending.await.is_err());
            assert!(reader.chunk().await.is_err());
            continue;
        }
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        owner
            .pools()
            .run(PoolKind::Pack, PriorityClass::Background, || ())
            .unwrap();
        assert!(reader.chunk().await.is_err());
    }
}
#[tokio::test]
async fn unknown_or_truncated_headers_publish_one_error_and_no_done() {
    for data in [
        &b"not an audio container"[..],
        &b"ID3\x04\0\0\x7f\x7f\x7f\x7f"[..],
    ] {
        let got = receive(data, 0, 48000, 137).await;
        assert_eq!((got.ready, got.done, got.errors.len()), (0, 0, 1));
        assert!(got.pcm.is_empty());
    }
}
