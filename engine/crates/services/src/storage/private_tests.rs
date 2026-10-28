use super::*;
use migo_io::{kv_store::KvStore, task::PoolKind};
use std::{
    sync::{atomic::AtomicU64, mpsc},
    thread,
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    paths: GamePaths,
    scheduler: Arc<IoScheduler>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "migo-private-storage-service-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "game", 1).unwrap();
        paths.ensure_directories().unwrap();
        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(
            729,
            2,
            1024 * 1024,
        ));
        Self {
            root,
            paths,
            scheduler,
        }
    }
    fn seed(&self, value: &str) {
        KvStore::open(
            self.paths.storage().legacy().join("storage.db"),
            MAX_TOTAL_BYTES,
        )
        .unwrap()
        .set("saved", value)
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.scheduler.close();
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn synchronous_storage_migrates_existing_data_and_never_reuses_a_game_forged_legacy_store() {
    let f = Fixture::new();
    f.seed("legacy saved");
    assert_eq!(
        get_sync(f.scheduler.clone(), Some(&f.paths), "saved").unwrap(),
        "legacy saved"
    );
    assert!(f.paths.storage().is_prepared());
    assert!(!f.paths.storage().legacy().exists());
    f.seed("game forged");
    set_sync(
        f.scheduler.clone(),
        Some(&f.paths),
        "saved",
        "private updated",
    )
    .unwrap();
    assert_eq!(
        get_sync(f.scheduler.clone(), Some(&f.paths), "saved").unwrap(),
        "private updated"
    );
    assert_eq!(
        KvStore::open(
            f.paths.storage().legacy().join("storage.db"),
            MAX_TOTAL_BYTES
        )
        .unwrap()
        .get("saved")
        .unwrap()
        .unwrap(),
        "game forged"
    );
    clear_sync(f.scheduler.clone(), Some(&f.paths)).unwrap();
    assert_eq!(
        get_sync(f.scheduler.clone(), Some(&f.paths), "saved").unwrap(),
        ""
    );
}

#[test]
fn asynchronous_mutation_order_survives_first_private_preparation() {
    let f = Fixture::new();
    f.seed("legacy");
    let first = set(
        f.scheduler.clone(),
        Some(&f.paths),
        "saved".into(),
        "first".into(),
    );
    let second = set(
        f.scheduler.clone(),
        Some(&f.paths),
        "saved".into(),
        "second".into(),
    );
    runtime().block_on(async {
        let (two, one) = tokio::join!(second, first);
        one.unwrap();
        two.unwrap();
        assert_eq!(
            get(f.scheduler.clone(), Some(&f.paths), "saved".into())
                .await
                .unwrap(),
            "second"
        );
        assert!(
            info(f.scheduler.clone(), Some(&f.paths))
                .await
                .unwrap()
                .contains("saved")
        );
    });
    assert!(!f.paths.storage().legacy().exists());
}

#[test]
fn content_mount_prepares_storage_before_publishing_the_game_filesystem() {
    let f = Fixture::new();
    f.seed("pre-content");
    fs::create_dir_all(f.paths.code_dir()).unwrap();
    let mounted =
        crate::content::prepare_content(&f.root.join("files"), &f.root.join("cache"), "game", 2)
            .unwrap();
    assert!(mounted.game_paths.storage().is_prepared());
    assert!(!mounted.game_paths.storage().legacy().exists());
    assert!(
        mounted
            .vfs
            .open_regular_for_read("/user/kv_storage/storage.db")
            .is_err()
    );
    assert_eq!(
        get_sync(f.scheduler.clone(), Some(&mounted.game_paths), "saved").unwrap(),
        "pre-content"
    );
}

#[test]
fn prepared_synchronous_reads_do_not_wait_for_another_fs_worker() {
    let f = Fixture::new();
    set_sync(f.scheduler.clone(), Some(&f.paths), "saved", "warm").unwrap();
    let mut releases = Vec::new();
    let mut jobs = Vec::new();
    for _ in 0..2 {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        jobs.push(
            f.scheduler
                .pools()
                .submit_async(PoolKind::Fs, PriorityClass::ForegroundAsync, move || {
                    started_tx.send(()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(10));
                })
                .unwrap(),
        );
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        releases.push(release_tx);
    }
    let scheduler = f.scheduler.clone();
    let paths = f.paths.clone();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let result = get_sync(scheduler, Some(&paths), "saved");
        let _ = tx.send(result);
    });
    let early = rx.recv_timeout(Duration::from_millis(500));
    for release in releases {
        let _ = release.send(());
    }
    reader.join().unwrap();
    runtime().block_on(async {
        for job in jobs {
            job.await.unwrap().unwrap();
        }
    });
    assert_eq!(
        early.expect("warm read submitted another Fs job").unwrap(),
        "warm"
    );
}
