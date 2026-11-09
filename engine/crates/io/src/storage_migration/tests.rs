use super::*;
use crate::kv_store::KvStore;
use shared::vfs::{GamePaths, VirtualFS};
use std::{
    process::Command,
    sync::{Arc, Barrier},
};

fn paths(root: &Path, session: i32) -> GamePaths {
    let paths = GamePaths::new(root.join("files"), root.join("cache"), "game", session).unwrap();
    paths.ensure_directories().unwrap();
    fs::create_dir_all(paths.code_dir()).unwrap();
    paths
}
fn seed(dir: &Path, value: &str) {
    let store = KvStore::open(dir.join("storage.db"), 1024 * 1024).unwrap();
    store.set("saved", value).unwrap();
}
fn value(dir: &Path) -> String {
    KvStore::open(dir.join("storage.db"), 1024 * 1024)
        .unwrap()
        .get("saved")
        .unwrap()
        .unwrap()
}

#[test]
#[ignore = "subprocess fixture: exits without SQLite destructors"]
fn wal_fixture_child() {
    let directory = std::env::var_os("MIGO_STORAGE_WAL_FIXTURE").expect("fixture directory");
    let store = KvStore::open(Path::new(&directory).join("storage.db"), 1024 * 1024).unwrap();
    store.set("saved", "committed-only-in-wal").unwrap();
    std::process::exit(0);
}

#[test]
fn migration_preserves_committed_wal_and_keeps_sqlite_outside_the_game_vfs() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    assert!(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage_migration::tests::wal_fixture_child",
                "--ignored"
            ])
            .env("MIGO_STORAGE_WAL_FIXTURE", paths.storage().legacy())
            .status()
            .unwrap()
            .success()
    );
    let before = fs::read(paths.storage().legacy().join("storage.db")).unwrap();
    assert!(
        !before
            .windows(b"committed-only-in-wal".len())
            .any(|s| s == b"committed-only-in-wal")
    );
    assert!(
        fs::metadata(paths.storage().legacy().join("storage.db-wal"))
            .unwrap()
            .len()
            > 0
    );
    prepare(paths.storage()).unwrap();
    assert!(!paths.storage().legacy().exists());
    assert_eq!(value(paths.storage().directory()), "committed-only-in-wal");
    let vfs = VirtualFS::try_from_game_paths(&paths).unwrap();
    assert!(
        vfs.open_regular_for_read(
            paths
                .storage()
                .directory()
                .join("storage.db")
                .to_str()
                .unwrap()
        )
        .is_err()
    );
    assert!(
        vfs.open_regular_for_read("/user/../runtime_state/kv_storage/storage.db")
            .is_err()
    );
    assert!(
        vfs.open_regular_for_read("/user/kv_storage/storage.db")
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn an_unsafe_legacy_entry_does_not_move_or_publish_and_can_be_retried() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    let legacy = paths.storage().legacy();
    fs::create_dir_all(legacy).unwrap();
    fs::write(root.path().join("outside"), b"untouched").unwrap();
    std::os::unix::fs::symlink(root.path().join("outside"), legacy.join("storage.db")).unwrap();
    assert!(prepare(paths.storage()).is_err());
    assert!(!paths.storage().is_prepared());
    assert!(!paths.storage().directory().exists());
    assert!(
        fs::symlink_metadata(legacy.join("storage.db"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        !paths
            .storage()
            .directory()
            .parent()
            .unwrap()
            .join(READY)
            .exists()
    );
    fs::remove_file(legacy.join("storage.db")).unwrap();
    seed(legacy, "retry preserved");
    prepare(paths.storage()).unwrap();
    assert_eq!(value(paths.storage().directory()), "retry preserved");
    assert_eq!(fs::read(root.path().join("outside")).unwrap(), b"untouched");
}

#[test]
fn a_moved_directory_without_a_marker_resumes_without_reinitializing_data() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    seed(paths.storage().legacy(), "before interruption");
    fs::create_dir_all(paths.storage().directory().parent().unwrap()).unwrap();
    fs::rename(paths.storage().legacy(), paths.storage().directory()).unwrap();
    prepare(paths.storage()).unwrap();
    assert_eq!(value(paths.storage().directory()), "before interruption");
    assert_eq!(
        fs::read(paths.storage().directory().parent().unwrap().join(READY)).unwrap(),
        MAGIC
    );
}

#[test]
fn hardlinked_legacy_files_cannot_expose_private_writes_through_a_game_alias() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    seed(paths.storage().legacy(), "single owner");
    let alias = paths.user_data_dir().join("alias.db");
    fs::hard_link(paths.storage().legacy().join("storage.db"), &alias).unwrap();
    assert!(prepare(paths.storage()).is_err());
    assert!(!paths.storage().directory().exists());
    assert!(paths.storage().legacy().join("storage.db").exists());
    fs::remove_file(alias).unwrap();
    prepare(paths.storage()).unwrap();
    assert_eq!(value(paths.storage().directory()), "single owner");
}

#[test]
fn a_rollback_journal_moves_with_its_database_for_sqlite_recovery() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    seed(paths.storage().legacy(), "committed");
    let journal = paths.storage().legacy().join("storage.db-journal");
    fs::write(&journal, b"not a hot journal").unwrap();
    prepare(paths.storage()).unwrap();
    let target = paths.storage().directory();
    assert_eq!(
        fs::read(target.join("storage.db-journal")).unwrap(),
        b"not a hot journal"
    );
    assert!(!paths.storage().legacy().exists());
    assert_eq!(value(target), "committed");
}

#[test]
fn game_entries_beside_the_legacy_database_stay_game_visible() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    let legacy = paths.storage().legacy();
    seed(legacy, "migrated");
    fs::create_dir(legacy.join("levels")).unwrap();
    fs::write(legacy.join("levels").join("1.json"), b"{}").unwrap();
    fs::write(legacy.join("notes.txt"), b"game file").unwrap();
    prepare(paths.storage()).unwrap();
    let target = paths.storage().directory();
    assert_eq!(value(target), "migrated");
    assert_eq!(fs::read(legacy.join("notes.txt")).unwrap(), b"game file");
    assert_eq!(
        fs::read(legacy.join("levels").join("1.json")).unwrap(),
        b"{}"
    );
    assert!(!legacy.join("storage.db").exists());
    let mut private: Vec<_> = fs::read_dir(target)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    private.sort();
    assert!(private.iter().all(|name| is_family(name)), "{private:?}");
}

#[test]
fn a_legacy_directory_without_a_database_is_left_to_the_game() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    let legacy = paths.storage().legacy();
    fs::create_dir_all(legacy.join("assets")).unwrap();
    fs::write(legacy.join("assets").join("a.bin"), b"game").unwrap();
    prepare(paths.storage()).unwrap();
    assert_eq!(
        fs::read(legacy.join("assets").join("a.bin")).unwrap(),
        b"game"
    );
    assert_eq!(
        fs::read_dir(paths.storage().directory()).unwrap().count(),
        0
    );
    seed(paths.storage().directory(), "fresh");
    assert_eq!(value(paths.storage().directory()), "fresh");
}

#[test]
fn an_interrupted_return_of_game_entries_resumes_before_readiness() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    let legacy = paths.storage().legacy();
    seed(legacy, "kept");
    fs::write(legacy.join("first"), b"1").unwrap();
    fs::write(legacy.join("second"), b"2").unwrap();
    let target = paths.storage().directory();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    // Crash shape: the store moved and one game entry was already handed back.
    fs::rename(legacy, target).unwrap();
    fs::create_dir(legacy).unwrap();
    fs::rename(target.join("first"), legacy.join("first")).unwrap();
    prepare(paths.storage()).unwrap();
    assert_eq!(fs::read(legacy.join("first")).unwrap(), b"1");
    assert_eq!(fs::read(legacy.join("second")).unwrap(), b"2");
    assert!(!target.join("second").exists());
    assert_eq!(value(target), "kept");
}

#[test]
fn ambiguous_histories_are_reported_without_overwriting_either_store() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    seed(paths.storage().legacy(), "legacy");
    seed(paths.storage().directory(), "private");
    assert!(prepare(paths.storage()).is_err());
    assert!(!paths.storage().is_prepared());
    assert_eq!(value(paths.storage().legacy()), "legacy");
    assert_eq!(value(paths.storage().directory()), "private");
    assert!(
        !paths
            .storage()
            .directory()
            .parent()
            .unwrap()
            .join(READY)
            .exists()
    );
}

#[test]
fn a_ready_private_store_ignores_a_recreated_game_visible_legacy_directory() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    prepare(paths.storage()).unwrap();
    seed(paths.storage().directory(), "private authoritative");
    seed(paths.storage().legacy(), "game forged");
    let other = super::tests::paths(root.path(), 2);
    prepare(other.storage()).unwrap();
    assert_eq!(value(other.storage().directory()), "private authoritative");
    assert_eq!(value(other.storage().legacy()), "game forged");
}

#[test]
fn initialization_holds_a_kernel_lock_before_inspecting_the_namespace() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path(), 1);
    prepare_inner_with(paths.storage(), || {
        let probe = File::open(paths.storage().directory().parent().unwrap().join(LOCK)).unwrap();
        assert!(matches!(
            probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
    })
    .unwrap();
}

#[test]
fn a_corrupt_readiness_record_never_selects_or_overwrites_a_store() {
    let root = tempfile::tempdir().unwrap();
    let first = paths(root.path(), 1);
    prepare(first.storage()).unwrap();
    seed(first.storage().directory(), "private");
    fs::write(
        first.storage().directory().parent().unwrap().join(READY),
        vec![b'x'; MAGIC.len()],
    )
    .unwrap();
    let retry = paths(root.path(), 2);
    assert!(prepare(retry.storage()).is_err());
    assert!(!retry.storage().is_prepared());
    assert_eq!(value(retry.storage().directory()), "private");
}

#[cfg(unix)]
#[test]
fn sqlite_open_refuses_a_database_symlink_before_any_schema_write() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside");
    seed(&outside, "outside");
    let link = root.path().join("alias.db");
    std::os::unix::fs::symlink(outside.join("storage.db"), &link).unwrap();
    assert!(KvStore::open(&link, 1024 * 1024).is_err());
    assert_eq!(value(&outside), "outside");
}

/// A link above the store is the host's, not a planted one: an Android app's own data
/// path starts with /data/user/0, a link in the app's mount namespace, and NOFOLLOW
/// refused every open beneath it. The link at the database is still refused.
#[cfg(unix)]
#[test]
fn sqlite_open_accepts_a_link_above_the_store_and_still_refuses_one_at_the_database() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("data/data/app");
    fs::create_dir_all(&real).unwrap();
    let alias = root.path().join("user0");
    std::os::unix::fs::symlink(root.path().join("data/data"), &alias).unwrap();
    let store = KvStore::open(alias.join("app/kv/storage.db"), 1024 * 1024)
        .expect("a link in the host's own path refused the store");
    drop(store);
    assert!(real.join("kv/storage.db").is_file());

    let outside = root.path().join("outside");
    seed(&outside, "outside");
    let planted = alias.join("app/kv/planted.db");
    std::os::unix::fs::symlink(outside.join("storage.db"), &planted).unwrap();
    assert!(KvStore::open(&planted, 1024 * 1024).is_err());
    assert_eq!(value(&outside), "outside");
}

#[test]
fn independent_sessions_serialize_initialization_and_share_the_migrated_store() {
    let root = tempfile::tempdir().unwrap();
    let original = paths(root.path(), 1);
    seed(original.storage().legacy(), "one migration");
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|id| {
            let paths = paths(root.path(), id + 10);
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                prepare(paths.storage()).unwrap();
                paths
            })
        })
        .collect();
    for thread in threads {
        let paths = thread.join().unwrap();
        assert!(paths.storage().is_prepared());
        assert_eq!(paths.storage().directory(), original.storage().directory());
    }
    assert_eq!(value(original.storage().directory()), "one migration");
}
