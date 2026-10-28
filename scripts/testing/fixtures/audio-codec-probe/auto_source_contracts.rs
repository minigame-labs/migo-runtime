use crate::{
    local_source::{DirectoryOwnership, FileReader, SnapshotOptions},
    local_source_contracts::{self as local, Format},
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
        "auto-source-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn options(dir: PathBuf, bytes: u64) -> SnapshotOptions {
    SnapshotOptions {
        dir,
        budget: SpoolBudget::new(bytes.max(1), 1).unwrap(),
        max_bytes: bytes.max(1),
        deadline: Instant::now() + Duration::from_secs(10),
    }
}
#[derive(Clone, Copy)]
enum Route {
    Snapshot,
    Immutable,
    Package,
}
async fn open(name: &str, route: Route, owner: Arc<IoScheduler>, fragment: usize) -> FileReader {
    let root = root();
    let input = root.join("input");
    std::fs::create_dir_all(&input).unwrap();
    let data = std::fs::read(local::assets().join(name)).unwrap();
    std::fs::write(input.join("clip"), &data).unwrap();
    let mounts = MountTable::new(input.clone());
    if matches!(route, Route::Package) {
        let path = root.join("asset.mpkg");
        let mut w = PackageWriter::new(File::create(&path).unwrap()).unwrap();
        w.add_entry("clip", &data).unwrap();
        w.finish("game", "1").unwrap();
        mounts.swap_base(Arc::new(PackSource::open(&path, "game", "1").unwrap()));
    }
    let pin = mounts.pin_asset_read("clip", mounts.generation()).unwrap();
    let snapshot_dir = root.join("private");
    let (ownership, max) = match route {
        Route::Snapshot => (DirectoryOwnership::Snapshot, data.len() as u64),
        _ => (DirectoryOwnership::Immutable, 1),
    };
    let mut reader =
        FileReader::open_asset(pin, ownership, owner, options(snapshot_dir.clone(), max))
            .await
            .unwrap();
    if matches!(route, Route::Snapshot) {
        assert!(snapshot_dir.is_dir());
        migo_io::fs_ops::write_file(
            input.join("clip").to_str().unwrap(),
            b"source changed after seal",
            shared::protocol::io_cmd::WriteMode::Overwrite,
            shared::protocol::io_cmd::WriteDurability::Fast,
        )
        .unwrap();
    } else {
        assert!(
            !snapshot_dir.exists(),
            "direct immutable/package source was copied"
        );
    }
    reader.chunk_bytes = fragment;
    reader
}

#[tokio::test]
async fn automatic_backends_and_snapshot_ownership_preserve_all_sequential_pcm() {
    let mut count = 0;
    for (format, ext) in [
        (Format::Mp3, "mp3"),
        (Format::Wave, "wav"),
        (Format::Adts, "aac"),
        (Format::Vorbis, "ogg"),
    ] {
        for i in 0..4 {
            let name = format!("{i}.{ext}");
            let data = std::fs::read(local::assets().join(&name)).unwrap();
            let native = crate::candidate::decode(&data, 16_777_216)
                .unwrap()
                .sample_rate;
            for target in [native, if native == 48000 { 44100 } else { 48000 }] {
                let (expected, _) = local::reference(&data, target);
                for route in [Route::Snapshot, Route::Immutable, Route::Package] {
                    for fragment in [137, 65536] {
                        let owner = Arc::new(IoScheduler::new(0));
                        let reader = open(&name, route, owner.clone(), fragment).await;
                        let got = local::receive(
                            format,
                            Arc::try_unwrap(owner).ok().unwrap(),
                            reader,
                            target,
                        )
                        .await;
                        assert_eq!(
                            (got.ready, got.done, got.errors.len()),
                            (1, 1, 0),
                            "{:?}",
                            got.errors
                        );
                        assert_eq!(got.pcm, expected, "{name}/{target}/{fragment}");
                        count += 1;
                    }
                }
            }
        }
    }
    assert_eq!(count, 192);
    println!("W09_AUTO_SOURCE_PCM {count} exact comparisons");
}

#[tokio::test]
async fn failed_snapshot_admission_never_falls_back_to_direct_read() {
    let root = root();
    let path = root.join("input");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("clip"), vec![0; 100]).unwrap();
    let mounts = MountTable::new(path);
    let owner = Arc::new(IoScheduler::new(0));
    let private = root.join("private");
    let pin = mounts.pin_asset_read("clip", mounts.generation()).unwrap();
    assert!(
        FileReader::open_asset(
            pin,
            DirectoryOwnership::Snapshot,
            owner,
            options(private.clone(), 99)
        )
        .await
        .is_err()
    );
    assert!(!private.exists());
}

#[tokio::test]
async fn closed_and_expired_setup_do_not_create_private_state() {
    let root = root();
    let input = root.join("input");
    std::fs::create_dir_all(&input).unwrap();
    std::fs::write(input.join("clip"), b"data").unwrap();
    let mounts = MountTable::new(input);
    for closed in [false, true] {
        let owner = Arc::new(IoScheduler::new(0));
        let mut limit = options(root.join("private"), 100);
        if closed {
            owner.close();
        } else {
            limit.deadline = Instant::now();
        }
        assert!(
            FileReader::open_asset(
                mounts.pin_asset_read("clip", mounts.generation()).unwrap(),
                DirectoryOwnership::Snapshot,
                owner,
                limit
            )
            .await
            .is_err()
        );
        assert!(!root.join("private").exists());
    }
}

#[tokio::test]
async fn automatic_package_open_never_falls_through_a_missing_overlay() {
    let root = root();
    let input = root.join("input");
    std::fs::create_dir_all(input.join("sub")).unwrap();
    std::fs::write(input.join("sub/clip"), b"base must not be used").unwrap();
    let path = root.join("missing.mpkg");
    let mut w = PackageWriter::new(File::create(&path).unwrap()).unwrap();
    w.add_entry("other", b"other").unwrap();
    w.finish("game", "1").unwrap();
    let mounts = MountTable::new(input);
    assert!(mounts.mount_overlay(
        "package".into(),
        "sub".into(),
        Arc::new(PackSource::open(&path, "game", "1").unwrap())
    ));
    assert!(
        FileReader::open_asset(
            mounts
                .pin_asset_read("sub/clip", mounts.generation())
                .unwrap(),
            DirectoryOwnership::Immutable,
            Arc::new(IoScheduler::new(0)),
            options(root.join("private"), 100)
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn snapshot_media_uses_persistent_credit_without_charging_it_again() {
    let root = root();
    let input = root.join("input");
    std::fs::create_dir_all(&input).unwrap();
    let data = vec![7; 128];
    std::fs::write(input.join("clip"), &data).unwrap();
    let mounts = MountTable::new(input);
    let owner = Arc::new(IoScheduler::isolated_for_admission_probe(
        315,
        2,
        128 * 1024,
    ));
    let mut reader = FileReader::open_asset(
        mounts.pin_asset_read("clip", mounts.generation()).unwrap(),
        DirectoryOwnership::Snapshot,
        owner.clone(),
        options(root.join("private"), 128),
    )
    .await
    .unwrap();
    assert!(owner.pools().reserve_bytes(1).is_err());
    let chunk = reader.chunk().await.unwrap().unwrap();
    assert_eq!(chunk.as_ref(), data);
    drop(reader);
    assert!(owner.pools().reserve_bytes(1).is_err());
    drop(chunk);
    assert!(owner.pools().reserve_bytes(128 * 1024).is_ok());
}

#[derive(Debug)]
struct ProbePackage(Arc<std::sync::atomic::AtomicBool>);
impl shared::vfs::MountBackend for ProbePackage {
    fn has_package_reader(&self) -> bool {
        true
    }
    fn read(&self, _: &str) -> std::io::Result<Vec<u8>> {
        panic!("whole backend read")
    }
    fn exists(&self, _: &str) -> bool {
        panic!("stat at admission")
    }
    fn real_path(&self, _: &str) -> Option<PathBuf> {
        panic!("path at admission")
    }
    fn root_dir(&self) -> Option<&std::path::Path> {
        None
    }
    fn open_package_entry(
        &self,
        _: &str,
        _: shared::vfs::package::PackageReadLimits,
    ) -> std::io::Result<shared::vfs::package::PackageEntryReader> {
        self.0.store(true, Ordering::SeqCst);
        Err(std::io::Error::other("observed package opening"))
    }
}
async fn queued_probe(expire: bool) {
    use migo_io::task::{PoolKind, PriorityClass};
    use std::{
        future::Future,
        sync::mpsc,
        task::{Context, Poll, Waker},
    };
    let root = root();
    let owner = Arc::new(IoScheduler::isolated_for_admission_probe(
        316,
        2,
        1024 * 1024,
    ));
    let (tx, rx) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let blocker = owner
        .pools()
        .submit_async(PoolKind::Pack, PriorityClass::ForegroundAsync, move || {
            tx.send(()).unwrap();
            wait.recv().unwrap();
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let opened = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mounts = MountTable::new(root.clone());
    mounts.swap_base(Arc::new(ProbePackage(opened.clone())));
    let mut limit = options(root.join("private"), 1024);
    if expire {
        limit.deadline = Instant::now() + Duration::from_millis(30);
    }
    let mut pending = Box::pin(FileReader::open_asset(
        mounts.pin_asset_read("clip", mounts.generation()).unwrap(),
        DirectoryOwnership::Immutable,
        owner.clone(),
        limit,
    ));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(matches!(pending.as_mut().poll(&mut cx), Poll::Pending));
    if expire {
        std::thread::sleep(Duration::from_millis(60));
    }
    owner
        .pools()
        .run(PoolKind::Fs, PriorityClass::Background, || ())
        .unwrap();
    let opened_while_pack_blocked = opened.load(Ordering::SeqCst);
    release.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    owner
        .pools()
        .run(PoolKind::Pack, PriorityClass::Background, || ())
        .unwrap();
    let opened_before_repoll = opened.load(Ordering::SeqCst);
    assert!(pending.await.is_err());
    assert!(!opened_while_pack_blocked, "package setup ran outside Pack");
    assert_eq!(
        opened_before_repoll, !expire,
        "queued source did not retain its deadline"
    );
}
#[tokio::test]
async fn automatic_package_setup_uses_pack_without_native_work_at_admission() {
    queued_probe(false).await;
}
#[tokio::test]
async fn automatic_package_setup_keeps_the_original_deadline_without_repoll() {
    queued_probe(true).await;
}
