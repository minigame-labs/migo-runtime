use super::*;
use shared::vfs::{MountBackend, MountTable, PackSource, PackageWriter};
use std::{
    sync::{Mutex, atomic::AtomicUsize, mpsc},
    time::Duration,
};
const WAIT: Duration = Duration::from_secs(10);
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn memory() -> u64 {
    PackageReadLimits::default().max_scratch_bytes().unwrap() as u64 + MEMORY_BYTES
}
struct Fixture {
    root: tempfile::TempDir,
    data: Vec<u8>,
    path: PathBuf,
    scheduler: Arc<IoScheduler>,
    budget: SpoolBudget,
}
impl Fixture {
    fn new(length: usize, memory: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("audio.mpkg");
        let data = (0..length).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        let mut writer =
            PackageWriter::with_chunk_size(File::create(&path).unwrap(), 65536).unwrap();
        writer.add_entry("clip", &data).unwrap();
        writer.finish("game", "1").unwrap();
        Self {
            root,
            data,
            path,
            scheduler: Arc::new(IoScheduler::local_with_byte_limit_for_test(1041, 1, memory)),
            budget: SpoolBudget::new(8 * 1024 * 1024, 1).unwrap(),
        }
    }
    fn pin(&self) -> PinnedPackRead {
        let mounts = MountTable::new(self.root.path().to_path_buf());
        mounts.swap_base(Arc::new(PackSource::open(&self.path, "game", "1").unwrap()));
        mounts.pin_pack_read("clip", mounts.generation()).unwrap()
    }
    fn capture(
        &self,
        pin: PinnedPackRead,
        max: u64,
        until: tokio::time::Instant,
    ) -> impl Future<Output = io::Result<SealedSpool>> + Send + 'static {
        snapshot_package_until(
            self.scheduler.clone(),
            pin,
            self.root.path().join("private"),
            self.budget.clone(),
            max,
            4096,
            until,
        )
    }
    fn released(&self) {
        assert!(
            self.scheduler.pools().reserve_bytes(memory()).is_ok(),
            "package capture retained scratch credit"
        );
        assert!(
            SpoolWriter::create(&self.root.path().join("private"), &self.budget, 1, Some(0))
                .is_ok(),
            "package capture retained disk credit"
        );
    }
}
#[test]
fn complete_package_bytes_are_crc_checked_sealed_and_keep_the_original_backing() {
    for len in [0, 1, 65536, 65537, 2_097_169] {
        let f = Fixture::new(len, memory());
        let pin = f.pin();
        std::fs::rename(&f.path, f.root.path().join("old.mpkg")).unwrap();
        std::fs::write(&f.path, b"replacement").unwrap();
        let sealed = runtime()
            .block_on(f.capture(pin, 4 * 1024 * 1024, tokio::time::Instant::now() + WAIT))
            .expect("package snapshot refused pinned bytes");
        assert_eq!(sealed.reader().len(), len as u64);
        for (i, bytes) in f.data.chunks(FILE_CHUNK_BYTES).enumerate() {
            assert_eq!(
                sealed
                    .reader()
                    .chunk_at((i * FILE_CHUNK_BYTES) as u64, bytes.len())
                    .unwrap()
                    .as_ref(),
                bytes,
                "package snapshot changed complete bytes"
            );
        }
        assert!(
            f.scheduler
                .pools()
                .reserve_bytes(memory() - MEMORY_BYTES)
                .is_ok(),
            "package reader scratch survived the Pack job"
        );
        assert!(
            f.scheduler
                .pools()
                .reserve_bytes(memory() - MEMORY_BYTES + 1)
                .is_err(),
            "sealed reader returned original media credit early"
        );
        assert_eq!(
            f.scheduler.metrics().delegated_runs,
            1,
            "package copy bypassed original Pack scheduling"
        );
        drop(sealed);
        f.released();
    }
}
#[test]
fn bad_whole_entry_crc_never_publishes_a_sealed_source() {
    let f = Fixture::new(131089, memory());
    let mut bytes = std::fs::read(&f.path).unwrap();
    let index = u64::from_le_bytes(bytes[24..32].try_into().unwrap()) as usize;
    bytes[index + 2 + 4 + 8] ^= 1; // Single writer entry "clip": path length/path/raw size precede CRC.
    std::fs::write(&f.path, bytes).unwrap();
    let error = runtime()
        .block_on(f.capture(f.pin(), 4 * 1024 * 1024, tokio::time::Instant::now() + WAIT))
        .err()
        .expect("bad package CRC published a sealed source");
    assert!(
        matches!(
            error
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<shared::vfs::package::PackageError>()),
            Some(shared::vfs::package::PackageError::ChecksumMismatch { .. })
        ),
        "wrong package CRC failure: {error}"
    );
    f.released();
}

struct Observed {
    source: PackSource,
    opens: Arc<AtomicUsize>,
    pools: IoPools,
    gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}
impl std::fmt::Debug for Observed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservedPackage")
    }
}
impl MountBackend for Observed {
    fn has_package_reader(&self) -> bool {
        true
    }
    fn entry_size(&self, p: &str) -> Option<u64> {
        self.source.entry_size(p)
    }
    fn open_package_entry(
        &self,
        p: &str,
        limits: PackageReadLimits,
    ) -> io::Result<shared::vfs::package::PackageEntryReader> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        assert!(
            self.pools.reserve_bytes(1).is_err(),
            "package reader allocated before scratch/media admission"
        );
        if let Some((start, release)) = self.gate.lock().unwrap().take() {
            start.send(()).unwrap();
            release.recv_timeout(WAIT).unwrap();
        }
        self.source.open_package_entry(p, limits)
    }
    fn read(&self, _: &str) -> io::Result<Vec<u8>> {
        panic!("package capture used whole-entry backend read")
    }
    fn exists(&self, _: &str) -> bool {
        panic!("package capture probed exists")
    }
    fn real_path(&self, _: &str) -> Option<PathBuf> {
        panic!("package capture reopened a host pathname")
    }
    fn root_dir(&self) -> Option<&Path> {
        None
    }
}
fn observed(
    f: &Fixture,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
) -> (PinnedPackRead, Arc<AtomicUsize>) {
    let opens = Arc::new(AtomicUsize::new(0));
    let mounts = MountTable::new(f.root.path().to_path_buf());
    mounts.swap_base(Arc::new(Observed {
        source: PackSource::open(&f.path, "game", "1").unwrap(),
        opens: opens.clone(),
        pools: f.scheduler.pools().clone(),
        gate: Mutex::new(gate),
    }));
    let shared::vfs::mount::PinnedAssetRead::Package(pin) =
        mounts.pin_asset_read("clip", mounts.generation()).unwrap()
    else {
        panic!("package pin missing")
    };
    (pin, opens)
}
#[test]
fn length_and_memory_limits_precede_reader_allocation_and_never_fall_back() {
    let f = Fixture::new(131089, memory());
    let (pin, opens) = observed(&f, None);
    assert!(
        runtime()
            .block_on(f.capture(pin, 131088, tokio::time::Instant::now() + WAIT))
            .is_err(),
        "oversized package entry opened"
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        0,
        "package size limit followed reader allocation"
    );
    f.released();
    let held = f.scheduler.pools().reserve_bytes(1).unwrap();
    let (pin, opens) = observed(&f, None);
    assert!(
        runtime()
            .block_on(f.capture(pin, 4 * 1024 * 1024, tokio::time::Instant::now() + WAIT))
            .is_err(),
        "package scratch admission was bypassed"
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        0,
        "unadmitted package reader was allocated"
    );
    drop(held);
    f.released();
}
#[test]
fn dropped_running_capture_retains_actual_pack_job_credit_until_completion() {
    let f = Fixture::new(131089, memory());
    let rt = runtime();
    let (start, started) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let (pin, opens) = observed(&f, Some((start, blocked)));
    let mut capture = Box::pin(f.capture(pin, 4 * 1024 * 1024, tokio::time::Instant::now() + WAIT));
    rt.block_on(std::future::poll_fn(|cx| {
        assert!(capture.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    }));
    started.recv_timeout(WAIT).unwrap();
    drop(capture);
    assert!(
        f.scheduler.pools().reserve_bytes(1).is_err(),
        "cancelled package returned credit before real Pack completion"
    );
    assert!(
        SpoolWriter::create(&f.root.path().join("private"), &f.budget, 1, Some(0)).is_err(),
        "cancelled package returned disk credit early"
    );
    release.send(()).unwrap();
    rt.block_on(
        f.scheduler
            .pools()
            .submit_async_bytes(
                crate::task::PoolKind::Pack,
                PriorityClass::ForegroundAsync,
                0,
                || (),
            )
            .unwrap(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    f.released();
}
#[test]
fn expired_or_closed_capture_never_opens_a_package_reader() {
    let f = Fixture::new(131089, memory());
    let (pin, opens) = observed(&f, None);
    assert!(
        runtime()
            .block_on(f.capture(pin, 4 * 1024 * 1024, tokio::time::Instant::now() - WAIT))
            .is_err(),
        "expired package source opened"
    );
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    f.scheduler.close();
    let (pin, opens) = observed(&f, None);
    assert!(
        runtime()
            .block_on(f.capture(pin, 4 * 1024 * 1024, tokio::time::Instant::now() + WAIT))
            .is_err(),
        "closed package source opened"
    );
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    f.released();
}
#[test]
fn every_package_output_block_checks_cancellation_and_the_same_deadline() {
    let f = Fixture::new(1, memory());
    let mut writer = SpoolWriter::create(
        &f.root.path().join("private"),
        &f.budget,
        131072,
        Some(131072),
    )
    .unwrap();
    let flag = AtomicBool::new(true);
    let mut output = Output {
        writer: &mut writer,
        scheduler: &f.scheduler,
        cancelled: &flag,
        deadline: tokio::time::Instant::now() + WAIT,
    };
    let error = output
        .write_all(&[0; 65536])
        .expect_err("cancelled package wrote a payload block");
    assert_eq!(
        error.kind(),
        io::ErrorKind::Other,
        "cancelled package retried until deadline"
    );
    flag.store(false, Ordering::Relaxed);
    output.deadline = tokio::time::Instant::now() - WAIT;
    assert!(
        output.write_all(&[0; 65536]).is_err(),
        "expired package wrote a payload block"
    );
    writer
        .append(&[17; 65536])
        .expect("rejected package changed partial output");
    writer
        .append(&[17; 65536])
        .expect("rejected package changed partial output");
    drop(writer);
    f.released();
}

#[test]
fn an_empty_package_still_checks_the_original_deadline_after_the_running_open() {
    let f = Fixture::new(0, memory());
    let rt = runtime();
    let (start, started) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let (pin, _) = observed(&f, Some((start, blocked)));
    let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
    let mut task = Box::pin(f.capture(pin, 4 * 1024 * 1024, deadline));
    rt.block_on(std::future::poll_fn(|cx| {
        assert!(task.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    }));
    started.recv_timeout(WAIT).unwrap();
    while tokio::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    release.send(()).unwrap();
    assert!(
        rt.block_on(task).is_err(),
        "expired running package published a sealed source"
    );
    f.released();
}
