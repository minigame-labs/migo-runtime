use super::*;
use crate::vfs::VirtualFS;
use std::sync::atomic::AtomicU64;
static NEXT: AtomicU64 = AtomicU64::new(0);

#[test]
fn kv_storage_is_persistent_per_game_and_outside_every_game_mapping() {
    let root = std::env::temp_dir().join(format!(
        "migo-private-kv-paths-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let game = GamePaths::new(root.join("files"), root.join("cache"), "a", 1).unwrap();
    let session = GamePaths::new(root.join("files"), root.join("cache"), "a", 2).unwrap();
    let other = GamePaths::new(root.join("files"), root.join("cache"), "b", 3).unwrap();
    assert_eq!(game.storage().directory(), session.storage().directory());
    assert_ne!(game.storage().directory(), other.storage().directory());
    assert!(Arc::ptr_eq(game.storage(), game.clone().storage()));
    game.ensure_directories().unwrap();
    std::fs::create_dir_all(game.code_dir()).unwrap();
    std::fs::create_dir_all(game.storage().directory()).unwrap();
    let file = game.storage().directory().join("storage.db");
    std::fs::write(&file, b"persistent").unwrap();
    let vfs = VirtualFS::try_from_game_paths(&game).unwrap();
    assert!(vfs.open_regular_for_read(file.to_str().unwrap()).is_err());
    assert!(
        vfs.open_regular_for_read("/user/../runtime_state/kv_storage/storage.db")
            .is_err()
    );
    for mapping in [
        game.user_data_dir(),
        game.code_dir(),
        game.sandbox_cache_dir(),
        game.temp_dir(),
    ] {
        assert!(!file.starts_with(mapping));
    }
    drop(vfs);
    game.clean_cache().unwrap();
    game.remove_temp().unwrap();
    assert_eq!(std::fs::read(file).unwrap(), b"persistent");
    let _ = std::fs::remove_dir_all(root);
}
