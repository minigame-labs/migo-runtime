//! Game paths generation and validation.
//!
//! This module provides cross-platform game path management.
//! All path generation logic is centralized here to ensure consistency
//! across different platforms (Android, iOS, Desktop).

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[cfg(test)]
mod storage_tests;

pub const KV_STORAGE_SUBDIR: &str = "kv_storage";

/// Host-private persistent storage, shared by clones of one game's paths.
/// Preparation runs on IO before content publication. Failed attempts remain
/// retryable; a successful preparation is a single atomic read on hot paths.
#[derive(Debug)]
pub struct StoragePaths {
    directory: PathBuf,
    legacy: PathBuf,
    prepared: AtomicBool,
    prepare_lock: parking_lot::Mutex<()>,
}
impl StoragePaths {
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn legacy(&self) -> &Path {
        &self.legacy
    }
    pub fn is_prepared(&self) -> bool {
        self.prepared.load(Ordering::Acquire)
    }
    pub fn prepare_with<E>(&self, prepare: impl FnOnce() -> Result<(), E>) -> Result<(), E> {
        if self.is_prepared() {
            return Ok(());
        }
        let _guard = self.prepare_lock.lock();
        if !self.is_prepared() {
            prepare()?;
            self.prepared.store(true, Ordering::Release);
        }
        Ok(())
    }
}

/// Error types for game path operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GamePathError {
    /// Game ID is invalid (wrong format or length)
    InvalidGameId(String),
    /// Game ID names a device rather than a file on one of the supported
    /// platforms, so it cannot become a directory there
    ReservedGameId(String),
    /// Base directory is invalid
    InvalidBaseDir,
}

impl std::fmt::Display for GamePathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GamePathError::InvalidGameId(id) => {
                write!(
                    f,
                    "Invalid game ID '{}': must be 1-64 lower-case alphanumeric, underscore or hyphen",
                    id
                )
            }
            GamePathError::ReservedGameId(id) => {
                write!(
                    f,
                    "Reserved game ID '{}': names a platform device, not a directory",
                    id
                )
            }
            GamePathError::InvalidBaseDir => write!(f, "Invalid base directory"),
        }
    }
}

impl std::error::Error for GamePathError {}

/// Validate a game ID.
///
/// # Rules
/// - Length: 1-64 characters
/// - Allowed characters: a-z, 0-9, _, -
/// - Not a reserved device name (`con`, `nul`, `com1`, ...)
///
/// # Why lower case only
///
/// The id becomes one path component per game, and that component is the
/// storage isolation boundary: separate saves, separate `migo.clearStorage()`,
/// separate quota, separate `/code`. NTFS and the default APFS volume compare
/// path components case-insensitively, so `PuzzleQuest` and `puzzlequest` are
/// two games on Linux and Android and one directory on Windows. Restricting the
/// id space to its own canonical spelling makes the pair unrepresentable
/// instead of resolving it per platform: folding here would leave `game_id()`
/// and the directory name two spellings of one identity, and every other
/// producer of that path (the Java SDK's own `GamePaths`, `GamePathStrings`
/// across the JNI boundary) would have to repeat the fold or disagree.
///
/// A host whose own catalogue uses mixed case owns the mapping, which it
/// already does: id uniqueness is the host's documented obligation.
///
/// # Examples
/// ```
/// use shared::vfs::game_paths::validate_game_id;
///
/// assert!(validate_game_id("my-game").is_ok());
/// assert!(validate_game_id("game_123").is_ok());
/// assert!(validate_game_id("").is_err());
/// assert!(validate_game_id("../hack").is_err());
/// assert!(validate_game_id("MyGame").is_err());
/// assert!(validate_game_id("nul").is_err());
/// ```
pub fn validate_game_id(game_id: &str) -> Result<(), GamePathError> {
    // Hand-rolled validation replaces regex crate dependency for this single use.
    // Allowed: a-z, 0-9, underscore, hyphen. Length: 1-64.
    if game_id.is_empty() || game_id.len() > 64 {
        return Err(GamePathError::InvalidGameId(game_id.to_string()));
    }
    for b in game_id.bytes() {
        match b {
            b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => {}
            _ => return Err(GamePathError::InvalidGameId(game_id.to_string())),
        }
    }
    if is_reserved_device_name(game_id) {
        return Err(GamePathError::ReservedGameId(game_id.to_string()));
    }
    Ok(())
}

/// Whether a game ID names a Windows device rather than a file.
///
/// Rejected on every platform, not only Windows. These names pass the character
/// rule, and on Windows every path containing one resolves to a device: the
/// per-game directory cannot be created, so the session fails at
/// `ensure_directories`. A platform-conditional rejection would make one id
/// valid on three platforms and invalid on the fourth, which is the divergence
/// the rest of this rule exists to avoid.
///
/// The caller has already applied the character rule, so `game_id` is lower
/// case here and the comparison needs no folding.
fn is_reserved_device_name(game_id: &str) -> bool {
    if matches!(game_id, "con" | "prn" | "aux" | "nul") {
        return true;
    }
    // COM0-COM9 and LPT0-LPT9. Windows reserves the whole numbered range, and a
    // digit suffix is the only thing distinguishing them from ordinary ids.
    let suffix = game_id
        .strip_prefix("com")
        .or_else(|| game_id.strip_prefix("lpt"));
    matches!(suffix, Some(digit) if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit())
}

/// Paths for a specific game instance.
///
/// All paths are absolute and isolated per game.
/// This struct is created by the runtime and can be serialized
/// back to the platform layer for cleanup operations.
#[derive(Debug, Clone)]
pub struct GamePaths {
    /// Game identifier
    game_id: String,
    /// Game code directory (read-only for the game)
    code_dir: PathBuf,
    /// User data directory (persistent, survives app updates)
    user_data_dir: PathBuf,
    /// SQLite and its journals are not game VFS files.
    storage: Arc<StoragePaths>,
    /// Cache directory (may be cleared by system)
    cache_dir: PathBuf,
    /// Game-writable subtree of the cache directory (mapped to `/cache`)
    sandbox_cache_dir: PathBuf,
    /// This session's temporary directory (cleared when the session ends)
    temp_dir: PathBuf,
    /// This session's game recording: runtime state, never mapped, gone with
    /// the session like `temp_dir`.
    recording_dir: PathBuf,
    /// Host-only session spool directory, outside every game VFS mapping.
    spool_dir: PathBuf,
}

impl GamePaths {
    /// Base subdirectory for all games.
    const GAMES_SUBDIR: &'static str = "migo/games";
    /// Parent of the per-session temporary directories.
    const TEMP_SUBDIR: &'static str = "tmp";
    /// Parent of the per-session recording directories.
    const RECORDING_SUBDIR: &'static str = "recordings";
    const SPOOL_SUBDIR: &'static str = "audio-spool";

    /// Create game paths from base directories, game ID and session ID.
    ///
    /// # Arguments
    /// * `files_dir` - Platform's persistent files directory
    ///   - Android: `Context.getFilesDir()`
    ///   - iOS: `NSDocumentDirectory`
    ///   - Desktop: `~/.local/share/app/` or `%APPDATA%/app/`
    /// * `cache_dir` - Platform's cache directory
    ///   - Android: `Context.getCacheDir()`
    ///   - iOS: `NSCachesDirectory`
    ///   - Desktop: `~/.cache/app/` or `%TEMP%/app/`
    /// * `game_id` - Unique game identifier
    /// * `session_id` - The session these paths belong to, which is what keeps
    ///   `/tmp` private to it. Two concurrently live sessions of the *same* game
    ///   deliberately share `/user` and `/cache` — that is one game's save data —
    ///   but `/tmp` is documented as lasting for a session, and sharing it made
    ///   the first session to close delete the other's live files mid-write.
    ///
    /// # Returns
    /// `GamePaths` on success, `GamePathError` if game_id is invalid.
    pub fn new(
        files_dir: impl AsRef<Path>,
        cache_dir: impl AsRef<Path>,
        game_id: &str,
        session_id: i32,
    ) -> Result<Self, GamePathError> {
        validate_game_id(game_id)?;

        let files_base = files_dir.as_ref().join(Self::GAMES_SUBDIR).join(game_id);
        let cache_base = cache_dir.as_ref().join(Self::GAMES_SUBDIR).join(game_id);

        Ok(Self {
            game_id: game_id.to_string(),
            code_dir: files_base.join("code"),
            user_data_dir: files_base.join("user_data"),
            storage: Arc::new(StoragePaths {
                directory: files_base.join("runtime_state").join(KV_STORAGE_SUBDIR),
                legacy: files_base.join("user_data").join(KV_STORAGE_SUBDIR),
                prepared: AtomicBool::new(false),
                prepare_lock: parking_lot::Mutex::new(()),
            }),
            cache_dir: cache_base.clone(),
            sandbox_cache_dir: cache_base.join("sandbox"),
            // A component rather than a suffix, so everything a sweep may remove
            // is confined to one subtree and no cleanup can reach the install
            // store or the staging directories that sit beside it.
            temp_dir: cache_base
                .join(Self::TEMP_SUBDIR)
                .join(session_id.to_string()),
            recording_dir: cache_base
                .join(Self::RECORDING_SUBDIR)
                .join(session_id.to_string()),
            spool_dir: cache_base
                .join(Self::SPOOL_SUBDIR)
                .join(session_id.to_string()),
        })
    }

    /// Get the game ID.
    pub fn game_id(&self) -> &str {
        &self.game_id
    }

    /// Get the code directory path (read-only for game).
    pub fn code_dir(&self) -> &Path {
        &self.code_dir
    }

    /// Persistent receipt for the sealed code tree. It is deliberately a
    /// sibling of `/code`: game VFS access cannot reach it and Android cache
    /// eviction does not discard the verification result.
    pub fn integrity_receipt_path(&self) -> PathBuf {
        self.code_dir
            .with_file_name("code.integrity-receipt-v1.json")
    }

    /// Advisory process lock serializing receipt promotion for this game.
    pub fn integrity_lock_path(&self) -> PathBuf {
        self.integrity_receipt_path().with_extension("lock")
    }

    /// Get the user data directory path (persistent).
    pub fn user_data_dir(&self) -> &Path {
        &self.user_data_dir
    }

    pub fn storage(&self) -> &Arc<StoragePaths> {
        &self.storage
    }

    /// Get the cache directory path.
    ///
    /// This is the per-game cache **root**, and it is runtime state rather than
    /// game data: the package install store, install staging directories and the
    /// derived-asset cache all live directly under it. It must never be the
    /// target of a VFS mapping — the game-writable cache is
    /// [`sandbox_cache_dir`](Self::sandbox_cache_dir), a child of it.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// The subtree of the cache root the game may write, mapped to `/cache`.
    ///
    /// Separate from [`cache_dir`](Self::cache_dir) because the root also holds
    /// the runtime's own records. A game that could write those would decide
    /// which package bytes a later session mounts as `/code`, what identity a
    /// shared cache keys them under, and could swap a staged package between the
    /// moment an install validates it and the moment it is renamed into place.
    pub fn sandbox_cache_dir(&self) -> &Path {
        &self.sandbox_cache_dir
    }

    /// Get the temporary directory path.
    ///
    /// Private to one session, not shared by every session of the game: `/tmp`
    /// lasts for a session, and the previous shape let one session's teardown
    /// delete another live session's files.
    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    /// Host-private disk spools. Created lazily on the Fs class, never mapped
    /// to `/tmp` or `/cache`, and cleaned with this session's temporary state.
    pub fn spool_dir(&self) -> &Path {
        &self.spool_dir
    }

    /// Where this session's game recording is made (`getGameRecorder`): under
    /// the cache root, which no VFS mapping reaches -- content never sees the
    /// file, as on the reference; the host is handed it to share.
    pub fn recording_path(&self) -> PathBuf {
        self.recording_dir.join("replay.mp4")
    }

    /// Ensure all directories exist.
    ///
    /// Creates the directories if they don't exist.
    /// Note: `code_dir` is created by the game deployment process,
    /// not by this method.
    pub fn ensure_directories(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.user_data_dir)?;
        std::fs::create_dir_all(&self.sandbox_cache_dir)?;
        std::fs::create_dir_all(&self.temp_dir)?;
        Ok(())
    }

    /// Clean the temporary directory.
    ///
    /// Removes all files in the temp directory but keeps the directory itself.
    pub fn clean_temp(&self) -> std::io::Result<()> {
        if self.temp_dir.exists() {
            std::fs::remove_dir_all(&self.temp_dir)?;
        }
        // A dead session's recording under this id goes with its temp files.
        if self.recording_dir.exists() {
            std::fs::remove_dir_all(&self.recording_dir)?;
        }
        if self.spool_dir.exists() {
            std::fs::remove_dir_all(&self.spool_dir)?;
        }
        std::fs::create_dir_all(&self.temp_dir)?;
        Ok(())
    }

    /// Remove this session's temporary directory entirely, at session end.
    ///
    /// The sibling of [`clean_temp`](Self::clean_temp): that one runs at the
    /// start and leaves an empty directory, this one runs at teardown and
    /// leaves nothing. `temp_dir` is `tmp/{session_id}`, and a session id is
    /// unique within its process, so no live session shares this path — the
    /// removal is confined to a subtree this session alone owns and never
    /// touches `tmp/{other_id}`.
    ///
    /// Absent is success: teardown runs on every exit path, including a panic
    /// before the first module eval created the directory.
    pub fn remove_temp(&self) -> std::io::Result<()> {
        for dir in [&self.temp_dir, &self.recording_dir, &self.spool_dir] {
            match std::fs::remove_dir_all(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Clean the cache directory (including temp).
    ///
    /// Removes all cached files. User data is preserved.
    pub fn clean_cache(&self) -> std::io::Result<()> {
        if self.cache_dir.exists() {
            std::fs::remove_dir_all(&self.cache_dir)?;
        }
        std::fs::create_dir_all(&self.sandbox_cache_dir)?;
        std::fs::create_dir_all(&self.temp_dir)?;
        Ok(())
    }

    /// Delete all game data (uninstall).
    ///
    /// Removes both persistent and cache data.
    /// Call this when the user uninstalls a game.
    pub fn delete_all(&self) -> std::io::Result<()> {
        // Delete files dir (user_data + code)
        let files_game_dir = self.user_data_dir.parent().unwrap_or(&self.user_data_dir);
        if files_game_dir.exists() {
            prepare_code_tree_for_removal(&self.code_dir)?;
            std::fs::remove_dir_all(files_game_dir)?;
        }

        // Delete cache dir
        if self.cache_dir.exists() {
            std::fs::remove_dir_all(&self.cache_dir)?;
        }

        Ok(())
    }

    /// Get the total size of user data in bytes.
    pub fn user_data_size(&self) -> u64 {
        dir_size(&self.user_data_dir)
    }

    /// Get the total size of cache in bytes.
    pub fn cache_size(&self) -> u64 {
        dir_size(&self.cache_dir)
    }

    /// Serialize paths to a format suitable for FFI/JNI.
    ///
    /// Returns paths as strings for cross-language communication.
    pub fn to_strings(&self) -> GamePathStrings {
        GamePathStrings {
            game_id: self.game_id.clone(),
            code_dir: self.code_dir.to_string_lossy().into_owned(),
            user_data_dir: self.user_data_dir.to_string_lossy().into_owned(),
            cache_dir: self.cache_dir.to_string_lossy().into_owned(),
            temp_dir: self.temp_dir.to_string_lossy().into_owned(),
        }
    }
}

/// String representation of game paths for FFI.
#[derive(Debug, Clone)]
pub struct GamePathStrings {
    pub game_id: String,
    pub code_dir: String,
    pub user_data_dir: String,
    pub cache_dir: String,
    pub temp_dir: String,
}

#[cfg(unix)]
fn prepare_code_tree_for_removal(code_dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = match std::fs::symlink_metadata(code_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }

    let mut directories = vec![code_dir.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let metadata = std::fs::symlink_metadata(&directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        std::fs::set_permissions(
            &directory,
            std::fs::Permissions::from_mode(metadata.mode() | 0o300),
        )?;
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                directories.push(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_code_tree_for_removal(_code_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Calculate directory size recursively.
fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }

    let mut size = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                size += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if path.is_dir() {
                size += dir_size(&path);
            }
        }
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-language rule table. Embedded rather than read at run time so
    /// that moving or deleting it fails the build instead of quietly leaving
    /// this suite with nothing to check.
    const VECTORS: &str = include_str!("game-id-vectors.txt");

    fn vectors() -> Vec<(bool, &'static str)> {
        let mut parsed = Vec::new();
        for line in VECTORS.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let verdict = fields.next().expect("non-empty line has a verdict");
            let id = fields.next().unwrap_or_else(|| panic!("no id in {line:?}"));
            assert!(fields.next().is_none(), "more than two fields in {line:?}");
            let expected = match verdict {
                "ok" => true,
                "err" => false,
                other => panic!("unknown verdict {other:?} in {line:?}"),
            };
            parsed.push((expected, id));
        }
        parsed
    }

    #[test]
    fn shared_vectors_agree_with_the_rule() {
        let vectors = vectors();
        // An emptied or all-one-verdict table would satisfy the loop below
        // without checking anything.
        assert!(
            vectors.len() >= 25,
            "vector table shrank: {}",
            vectors.len()
        );
        assert!(vectors.iter().any(|(expected, _)| *expected));
        assert!(vectors.iter().any(|(expected, _)| !*expected));

        for (expected, id) in vectors {
            assert_eq!(
                validate_game_id(id).is_ok(),
                expected,
                "vector {id:?} expected ok={expected}"
            );
        }
    }

    #[test]
    fn accepted_ids_are_their_own_lower_case_spelling() {
        // The property the case rule exists for, stated over arbitrary input
        // rather than over the vectors: on a case-insensitive filesystem, two
        // accepted ids that fold together are one directory. They cannot fold
        // together if every accepted id is already folded.
        for id in vectors()
            .into_iter()
            .filter(|(expected, _)| *expected)
            .map(|(_, id)| id)
            .chain(["game", "a-b_c9", "0000"])
        {
            assert_eq!(
                id,
                id.to_ascii_lowercase(),
                "accepted id {id:?} is not case-canonical"
            );
        }
        for id in ["Game", "gamE", "GAME", "My-Game", &("a".repeat(63) + "A")] {
            assert!(
                validate_game_id(id).is_err(),
                "{id:?} folds onto an accepted id and must be rejected"
            );
        }
    }

    #[test]
    fn the_accepted_character_set_is_exactly_lower_case_alphanumeric_underscore_and_hyphen() {
        // Exhaustive over single-byte ids, so no widening of the allowlist can
        // hide in a character the vectors happen not to name.
        for byte in 0u8..=255 {
            let id = char::from(byte).to_string();
            let expected = matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-');
            assert_eq!(
                validate_game_id(&id).is_ok(),
                expected,
                "byte {byte:#04x} ({:?})",
                char::from(byte)
            );
        }
    }

    #[test]
    fn reserved_device_names_are_rejected_on_every_platform() {
        // These pass the character rule. On Windows the per-game directory
        // cannot be created at all, so accepting them here would make one id
        // work on three platforms and fail on the fourth.
        let mut reserved: Vec<String> = vec![
            "con".to_string(),
            "prn".to_string(),
            "aux".to_string(),
            "nul".to_string(),
        ];
        for digit in '0'..='9' {
            reserved.push(format!("com{digit}"));
            reserved.push(format!("lpt{digit}"));
        }
        for id in &reserved {
            assert_eq!(
                validate_game_id(id),
                Err(GamePathError::ReservedGameId(id.clone())),
                "{id:?} names a device"
            );
        }
        // The boundary: an ordinary id that merely starts with a device name.
        for id in ["null", "conx", "com", "lpt", "com10", "con-1", "auxiliary"] {
            assert!(validate_game_id(id).is_ok(), "{id:?} is an ordinary id");
        }
    }

    #[test]
    fn the_two_refusals_name_the_rule_they_broke() {
        // A reserved id satisfies the character rule, so reporting it with the
        // character rule's message would send a host looking for a character
        // that is not there. That distinguishable diagnostic is the whole reason
        // the two refusals are separate variants.
        let bad_character = GamePathError::InvalidGameId("Game".to_string()).to_string();
        let reserved = GamePathError::ReservedGameId("nul".to_string()).to_string();

        assert!(bad_character.contains("lower-case"), "{bad_character}");
        assert!(bad_character.contains("Game"), "{bad_character}");
        assert!(reserved.contains("device"), "{reserved}");
        assert!(reserved.contains("nul"), "{reserved}");
        assert_ne!(bad_character, reserved);
    }

    #[test]
    fn test_valid_game_ids() {
        assert!(validate_game_id("my-game").is_ok());
        assert!(validate_game_id("game_123").is_ok());
        assert!(validate_game_id("game").is_ok());
        assert!(validate_game_id("a").is_ok());
        assert!(validate_game_id("a".repeat(64).as_str()).is_ok());
    }

    #[test]
    fn test_invalid_game_ids() {
        assert!(validate_game_id("").is_err());
        assert!(validate_game_id("a".repeat(65).as_str()).is_err());
        assert!(validate_game_id("../hack").is_err());
        assert!(validate_game_id("game/sub").is_err());
        assert!(validate_game_id("game.name").is_err());
        assert!(validate_game_id("game name").is_err());
    }

    #[test]
    fn test_game_paths_creation() {
        let paths = GamePaths::new("/data/files", "/data/cache", "test-game", 7).unwrap();

        assert_eq!(paths.game_id(), "test-game");
        assert!(paths.code_dir().ends_with("migo/games/test-game/code"));
        assert!(
            paths
                .user_data_dir()
                .ends_with("migo/games/test-game/user_data")
        );
        assert!(paths.cache_dir().ends_with("migo/games/test-game"));
        assert!(paths.temp_dir().ends_with("migo/games/test-game/tmp/7"));
    }

    /// The defect this shape exists for: two live sessions of the *same* game had
    /// one temp directory, so the first to close deleted the other's live files.
    #[test]
    fn two_sessions_of_one_game_get_separate_temp_directories() {
        let first = GamePaths::new("/data/files", "/data/cache", "test-game", 1).unwrap();
        let second = GamePaths::new("/data/files", "/data/cache", "test-game", 2).unwrap();

        assert_ne!(first.temp_dir(), second.temp_dir());
        // Neither contains the other, so removing one recursively cannot reach the
        // other -- distinct paths alone would still allow `tmp/1` and `tmp/1/2`.
        assert!(!first.temp_dir().starts_with(second.temp_dir()));
        assert!(!second.temp_dir().starts_with(first.temp_dir()));

        // Everything else is per *game* on purpose: two sessions of one game are
        // one save file, and splitting these would give the same game two.
        assert_eq!(first.user_data_dir(), second.user_data_dir());
        assert_eq!(first.sandbox_cache_dir(), second.sandbox_cache_dir());
        assert_eq!(first.code_dir(), second.code_dir());
    }

    /// A sweep of abandoned temp directories walks the shared parent, so that
    /// parent must not be the cache root: the install store and the staging
    /// directories live there, and a game deciding which package bytes a later
    /// session mounts is the reason `/cache` is a subdirectory in the first place.
    #[test]
    fn temp_directories_share_a_parent_below_the_cache_root() {
        let paths = GamePaths::new("/data/files", "/data/cache", "test-game", 3).unwrap();
        let temp_root = paths.temp_dir().parent().expect("temp dir has a parent");

        assert_eq!(
            temp_root,
            PathBuf::from("/data/cache/migo/games/test-game/tmp")
        );
        assert_ne!(temp_root, paths.cache_dir());
        assert!(temp_root.starts_with(paths.cache_dir()));
        assert!(!paths.sandbox_cache_dir().starts_with(temp_root));
    }

    #[test]
    fn spools_are_unmapped_session_owned_and_cleaned_with_temp_state() {
        let root = scratch_root("private_spool");
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "test-game", 7).unwrap();
        let sibling =
            GamePaths::new(root.join("files"), root.join("cache"), "test-game", 8).unwrap();
        assert!(paths.spool_dir().starts_with(paths.cache_dir()));
        for mapped in [
            paths.code_dir(),
            paths.user_data_dir(),
            paths.sandbox_cache_dir(),
            paths.temp_dir(),
        ] {
            assert!(!paths.spool_dir().starts_with(mapped));
        }
        assert_ne!(paths.spool_dir(), sibling.spool_dir());
        std::fs::create_dir_all(sibling.spool_dir()).unwrap();
        std::fs::write(sibling.spool_dir().join("keep"), b"x").unwrap();
        for cleanup in [GamePaths::clean_temp, GamePaths::remove_temp] {
            std::fs::create_dir_all(paths.spool_dir()).unwrap();
            std::fs::write(paths.spool_dir().join("stale"), b"x").unwrap();
            cleanup(&paths).unwrap();
            assert!(!paths.spool_dir().exists());
            assert!(sibling.spool_dir().join("keep").exists());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// A session's recording is runtime state: under the cache root, outside
    /// everything content is mapped to, and gone with the session.
    #[test]
    fn a_recording_is_the_session_s_and_never_content_s() {
        let root = std::env::temp_dir().join(format!("migo-recording-{}", std::process::id()));
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "test-game", 7).unwrap();
        let recording = paths.recording_path();
        assert!(recording.starts_with(paths.cache_dir()));
        assert!(!recording.starts_with(paths.sandbox_cache_dir()));
        assert!(!recording.starts_with(paths.temp_dir()));
        let other = GamePaths::new(root.join("files"), root.join("cache"), "test-game", 8).unwrap();
        assert_ne!(recording, other.recording_path(), "one per session");

        std::fs::create_dir_all(recording.parent().unwrap()).unwrap();
        std::fs::write(&recording, b"mp4").unwrap();
        paths.remove_temp().unwrap();
        assert!(!recording.exists(), "gone with the session");
        std::fs::create_dir_all(recording.parent().unwrap()).unwrap();
        std::fs::write(&recording, b"mp4").unwrap();
        paths.clean_temp().unwrap();
        assert!(
            !recording.exists(),
            "a dead session's, under this id, is gone at start"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn integrity_receipt_is_persistent_sibling_of_code() {
        let paths = GamePaths::new("/data/files", "/data/cache", "test-game", 1).unwrap();

        assert_eq!(
            paths.integrity_receipt_path(),
            PathBuf::from("/data/files/migo/games/test-game/code.integrity-receipt-v1.json")
        );
        assert_eq!(
            paths.integrity_lock_path(),
            PathBuf::from("/data/files/migo/games/test-game/code.integrity-receipt-v1.lock")
        );
        assert!(
            !paths
                .integrity_receipt_path()
                .starts_with(paths.cache_dir())
        );
        assert!(!paths.integrity_receipt_path().starts_with(paths.code_dir()));
    }

    /// Unique filesystem root for one test, cleaned up by the caller.
    fn scratch_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after the epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "migo_game_paths_{tag}_{}_{nanos}",
            std::process::id()
        ))
    }

    /// A session ending must take its own `/tmp` with it, and touch no other
    /// session's. The desktop platforms have no equivalent of the Android
    /// sweep, so without this call every session leaves its temp subtree behind
    /// under the cache root for the life of the install.
    #[test]
    fn remove_temp_deletes_this_sessions_directory_and_leaves_a_sibling() {
        let root = scratch_root("remove_temp");
        let ending = GamePaths::new(root.join("files"), root.join("cache"), "game", 4).unwrap();
        let live = GamePaths::new(root.join("files"), root.join("cache"), "game", 5).unwrap();
        ending.ensure_directories().unwrap();
        live.ensure_directories().unwrap();
        std::fs::write(ending.temp_dir().join("scratch.bin"), b"x").unwrap();
        std::fs::write(live.temp_dir().join("scratch.bin"), b"y").unwrap();

        ending.remove_temp().unwrap();

        assert!(
            !ending.temp_dir().exists(),
            "the ending session's temp dir survived"
        );
        assert!(
            live.temp_dir().join("scratch.bin").exists(),
            "a concurrent session's temp files were removed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A crash leaves a temp subtree behind, and session ids are a per-process
    /// counter from 1, so the next run's first session inherits the same
    /// numbered directory. `/tmp` is documented to start empty for a session,
    /// so the stale contents must be gone before the game can read them.
    #[test]
    fn clean_temp_empties_a_directory_left_by_a_previous_session_with_the_same_id() {
        let root = scratch_root("clean_temp");
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "game", 1).unwrap();
        paths.ensure_directories().unwrap();
        std::fs::write(paths.temp_dir().join("stale.bin"), b"from a dead session").unwrap();

        paths.clean_temp().unwrap();

        assert!(
            paths.temp_dir().is_dir(),
            "clean_temp must leave the dir in place"
        );
        assert_eq!(
            std::fs::read_dir(paths.temp_dir()).unwrap().count(),
            0,
            "stale files from the previous session survived"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Teardown runs on every exit path including a panic before the first
    /// module eval, so the directory may never have been created.
    #[test]
    fn remove_temp_is_ok_when_the_directory_was_never_created() {
        let root = scratch_root("remove_temp_absent");
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "game", 9).unwrap();

        paths
            .remove_temp()
            .expect("removing an absent temp dir is not an error");

        assert!(!paths.temp_dir().exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn delete_all_removes_a_sealed_code_tree_and_receipt() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // Unique per invocation. This test clears write bits, so a run that dies
        // between sealing and `delete_all` leaves a tree `remove_dir_all` cannot
        // traverse; a fixed path then fails every later run of the whole suite,
        // which is how this was found -- one mutation run poisoned it and the
        // failure read as a regression in the change under test.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after the epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "migo_game_paths_sealed_delete_{}_{nanos}",
            std::process::id()
        ));
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "sealed", 1).unwrap();
        paths.ensure_directories().unwrap();
        std::fs::create_dir_all(paths.code_dir().join("nested")).unwrap();
        std::fs::write(paths.code_dir().join("nested/game.js"), "code").unwrap();
        std::fs::write(paths.integrity_receipt_path(), "receipt").unwrap();
        std::fs::write(paths.integrity_lock_path(), "lock").unwrap();
        for directory in [
            paths.code_dir().join("nested"),
            paths.code_dir().to_path_buf(),
        ] {
            let mode = std::fs::metadata(&directory).unwrap().mode();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(mode & !0o222))
                .unwrap();
        }

        paths.delete_all().unwrap();

        assert!(!root.join("files/migo/games/sealed").exists());
        assert!(!root.join("cache/migo/games/sealed").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
