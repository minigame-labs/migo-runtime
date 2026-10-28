//! Real executor credit, actual package scratch, and the four service entrypoints.
use super::*;
use migo_io::pools::{ByteAdmitError, IoPools};
use migo_io::task::PoolKind;
use shared::vfs::{
    MountBackend,
    package::{PackSource, PackageReadLimits, PackageWriter},
};
use std::{
    fmt,
    fs::File,
    future::Future,
    io::{self, Read},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    task::{Context, Poll, Waker},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn bound() -> u64 {
    let limits = PackageReadLimits::default();
    (limits.max_encoded_chunk + limits.max_decoded_chunk) as u64
}
fn available(pools: &IoPools) -> u64 {
    match pools.reserve_bytes(u64::MAX) {
        Err(ByteAdmitError::BytesExhausted { available, .. }) => available,
        _ => panic!("expected finite isolated quota"),
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
struct ObservedPack {
    source: Arc<PackSource>,
    pools: IoPools,
    calls: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    origin: std::thread::ThreadId,
    limit: u64,
}
impl fmt::Debug for ObservedPack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ObservedPack")
    }
}
impl MountBackend for ObservedPack {
    fn read(&self, _: &str) -> io::Result<Vec<u8>> {
        panic!("copy collected whole package")
    }
    fn exists(&self, path: &str) -> bool {
        path == "asset"
    }
    fn real_path(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn root_dir(&self) -> Option<&Path> {
        None
    }
    fn is_file(&self, path: &str) -> bool {
        path == "asset"
    }
    fn entry_size(&self, path: &str) -> Option<u64> {
        self.source.reader().entry_raw_size(path)
    }
    fn open_copy_source(&self, path: &str) -> io::Result<shared::vfs::mount::CopySource> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_ne!(
            std::thread::current().id(),
            self.origin,
            "copy writes ran inline"
        );
        assert_eq!(
            available(&self.pools),
            0,
            "scratch was not reserved exactly once before source opening"
        );
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected source open failure"));
        }
        let source = self.source.open_copy_source(path)?;
        let shared::vfs::mount::CopySource::Package(reader) = &source else {
            panic!("not actual package reader")
        };
        assert!(
            reader.scratch_bytes() as u64 <= self.limit,
            "real reader exceeds admitted credit"
        );
        assert!(
            reader.scratch_bytes() > PackageReadLimits::default().max_decoded_chunk,
            "fixture must require encoded and decoded buffers"
        );
        Ok(source)
    }
}
struct Fixture {
    root: PathBuf,
    scheduler: Arc<IoScheduler>,
    mounts: Arc<MountTable>,
    calls: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    bytes: Vec<u8>,
    limit: u64,
}
impl Fixture {
    fn new(limit: u64) -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-copy-budget-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for name in ["code", "user", "cache", "tmp"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let chunk = PackageReadLimits::default().max_decoded_chunk;
        let bytes = (0..chunk * 3 + 73)
            .map(|n| (n % 251) as u8)
            .collect::<Vec<_>>();
        let path = root.join("source.mpkg");
        let mut writer =
            PackageWriter::with_chunk_size(File::create(&path).unwrap(), chunk as u32).unwrap();
        writer.add_entry("asset", &bytes).unwrap();
        writer.finish("game", "1").unwrap();
        let source = Arc::new(PackSource::open(&path, "game", "1").unwrap());
        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(701, 2, limit));
        let calls = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(false));
        let mounts = Arc::new(MountTable::new(root.join("code")));
        assert!(mounts.mount_overlay(
            "pack".into(),
            String::new(),
            Arc::new(ObservedPack {
                source,
                pools: scheduler.pools().clone(),
                calls: calls.clone(),
                fail: fail.clone(),
                origin: std::thread::current().id(),
                limit
            })
        ));
        Self {
            root,
            scheduler,
            mounts,
            calls,
            fail,
            bytes,
            limit,
        }
    }
    fn target(&self) -> PathBuf {
        self.root.join("user/output")
    }
    fn env(&self) -> FsEnv {
        FsEnv {
            scheduler: self.scheduler.clone(),
            mount_table: Some(self.mounts.clone()),
            vfs: Some(Arc::new(VirtualFS::new(
                self.root.join("code"),
                self.root.join("user"),
                self.root.join("cache"),
                self.root.join("tmp"),
            ))),
        }
    }
    fn credit_returned(&self) {
        assert_eq!(
            available(self.scheduler.pools()),
            self.limit,
            "copy credit leaked after completion"
        );
    }
    fn bytes_equal(&self, path: impl AsRef<Path>) {
        assert_eq!(std::fs::read(path).unwrap(), self.bytes);
        self.credit_returned();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.scheduler.close();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn async_copy_fits_only_bounded_scratch_and_returns_its_credit() {
    let f = Fixture::new(bound());
    assert!(f.bytes.len() as u64 > f.limit);
    runtime()
        .block_on(copy_pack_file_async(
            f.scheduler.clone(),
            f.mounts.clone(),
            "asset".into(),
            f.target(),
        ))
        .unwrap();
    f.bytes_equal(f.target());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn async_materialization_uses_the_same_bounded_credit() {
    let f = Fixture::new(bound());
    let path = runtime()
        .block_on(materialize_pack_to_temp_async(
            f.scheduler.clone(),
            f.mounts.clone(),
            "/code/asset".into(),
            ".bin",
        ))
        .unwrap();
    f.bytes_equal(path);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn sync_copy_is_admitted_on_pack_before_source_open() {
    let f = Fixture::new(bound());
    copy_sync(&f.env(), "/code/asset", "/user/output").unwrap();
    f.bytes_equal(f.target());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn sync_materialization_is_admitted_on_pack_before_source_open() {
    let f = Fixture::new(bound());
    let path =
        materialize_pack_to_temp_checked(&f.scheduler, Some(&f.mounts), "/code/asset", ".bin")
            .unwrap();
    f.bytes_equal(path);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn exhausted_credit_rejects_all_four_paths_before_source_or_destination_work() {
    let f = Fixture::new(bound() - 1);
    std::fs::write(f.target(), b"keep").unwrap();
    let rt = runtime();
    let results = [
        rt.block_on(copy_pack_file_async(
            f.scheduler.clone(),
            f.mounts.clone(),
            "asset".into(),
            f.target(),
        )),
        rt.block_on(materialize_pack_to_temp_async(
            f.scheduler.clone(),
            f.mounts.clone(),
            "/code/asset".into(),
            ".bin",
        ))
        .map(|_| ()),
        copy_sync(&f.env(), "/code/asset", "/user/output"),
        materialize_pack_to_temp_checked(&f.scheduler, Some(&f.mounts), "/code/asset", ".bin")
            .map(|_| ()),
    ];
    for result in results {
        assert!(
            result
                .unwrap_err()
                .message
                .contains("IO pending-byte budget exhausted")
        );
    }
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(f.target()).unwrap(), b"keep");
    assert!(!f.root.join(".migo-pack-materialized").exists());
    f.credit_returned();
}
#[test]
fn source_failure_returns_credit_and_later_copy_can_succeed() {
    let f = Fixture::new(bound());
    f.fail.store(true, Ordering::SeqCst);
    let rt = runtime();
    let results = [
        rt.block_on(copy_pack_file_async(
            f.scheduler.clone(),
            f.mounts.clone(),
            "asset".into(),
            f.target(),
        )),
        rt.block_on(materialize_pack_to_temp_async(
            f.scheduler.clone(),
            f.mounts.clone(),
            "/code/asset".into(),
            ".bin",
        ))
        .map(|_| ()),
        copy_sync(&f.env(), "/code/asset", "/user/output"),
        materialize_pack_to_temp_checked(&f.scheduler, Some(&f.mounts), "/code/asset", ".bin")
            .map(|_| ()),
    ];
    for result in results {
        assert!(
            result
                .unwrap_err()
                .message
                .contains("injected source open failure")
        );
        f.credit_returned();
    }
    assert!(!f.target().exists());
    f.fail.store(false, Ordering::SeqCst);
    rt.block_on(copy_pack_file_async(
        f.scheduler.clone(),
        f.mounts.clone(),
        "asset".into(),
        f.target(),
    ))
    .unwrap();
    f.bytes_equal(f.target());
}
#[test]
fn copy_never_uses_the_cached_read_inline_route_even_with_zero_scratch() {
    for request in [RequestKind::Sync, RequestKind::Async] {
        let f = Fixture::new(bound());
        let priority = PriorityClass::from(request);
        let req = IoRequest::Copy {
            backend: BackendKind::Pack,
            request,
            priority,
            scratch_bytes: 0,
        };
        assert_eq!(
            f.scheduler.classify(&req),
            migo_io::scheduler::RouteDecision::Delegated(PoolKind::Pack)
        );
        assert_eq!(req.priority(), priority);
    }
}
struct BlockingBackend {
    pools: IoPools,
    channels: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}
impl fmt::Debug for BlockingBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BlockingBackend")
    }
}
struct BlockingReader {
    pools: IoPools,
    channels: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}
impl Read for BlockingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some((ready, release)) = self.channels.take() {
            assert_eq!(available(&self.pools), 0, "active copy has no credit");
            ready.send(()).unwrap();
            release
                .recv_timeout(Duration::from_secs(5))
                .map_err(io::Error::other)?;
            assert_eq!(
                available(&self.pools),
                0,
                "receiver cancellation released active scratch early"
            );
            buffer[0] = b'x';
            Ok(1)
        } else {
            Ok(0)
        }
    }
}
impl MountBackend for BlockingBackend {
    fn read(&self, _: &str) -> io::Result<Vec<u8>> {
        panic!("whole stream fallback")
    }
    fn exists(&self, _: &str) -> bool {
        true
    }
    fn real_path(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn root_dir(&self) -> Option<&Path> {
        None
    }
    fn open_copy_source(&self, _: &str) -> io::Result<shared::vfs::mount::CopySource> {
        Ok(shared::vfs::mount::CopySource::Stream(Box::new(
            BlockingReader {
                pools: self.pools.clone(),
                channels: self.channels.lock().unwrap().take(),
            },
        )))
    }
}
#[test]
fn dropping_receiver_and_closing_domain_keep_credit_until_active_copy_finishes() {
    let f = Fixture::new(bound());
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    assert!(f.mounts.mount_overlay(
        "blocked".into(),
        String::new(),
        Arc::new(BlockingBackend {
            pools: f.scheduler.pools().clone(),
            channels: Mutex::new(Some((ready_tx, release_rx)))
        })
    ));
    let mut future = Box::pin(copy_pack_file_async(
        f.scheduler.clone(),
        f.mounts.clone(),
        "asset".into(),
        f.target(),
    ));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(future);
    f.scheduler.close();
    assert_eq!(available(f.scheduler.pools()), 0);
    release_tx.send(()).unwrap();
    // The same Pack lane has one slot, so this barrier starts after the active
    // copy has released its credit, not just after its reader sent a signal.
    let barrier = f
        .scheduler
        .pools()
        .submit_async(PoolKind::Pack, PriorityClass::ForegroundAsync, || ())
        .unwrap();
    runtime().block_on(barrier).unwrap().unwrap();
    f.credit_returned();
    assert_eq!(std::fs::read(f.target()).unwrap(), b"x");
}

#[test]
fn scratch_and_media_pool_totals_reject_overflow_without_allocating() {
    let overflowing = PackageReadLimits {
        max_encoded_chunk: usize::MAX,
        max_decoded_chunk: 1,
    };
    assert!(overflowing.max_scratch_bytes().is_err());
    assert!(migo_io::read_at_package::ReadAtPackage::reservation_bytes(overflowing).is_err());
    let near = PackageReadLimits {
        max_encoded_chunk: usize::MAX - 1024,
        max_decoded_chunk: 1,
    };
    assert!(near.max_scratch_bytes().is_ok());
    assert!(migo_io::read_at_package::ReadAtPackage::reservation_bytes(near).is_err());
}
