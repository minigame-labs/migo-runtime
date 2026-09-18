//! Where a game's content is, and the sandbox it is read through.
//!
//! One set of rules for both executions. The embedded runtime mounts content
//! when it evaluates a game's entry module; the external session mounts it when
//! the host names the content (`migo_session_load_content`), because there the
//! entry module is evaluated by WebKit in another process and what this process
//! owns is everything that module will ask for: `/code`, `/user`, `/cache` and
//! `/tmp`. The paths, the pinned roots and the mount table are built by the
//! functions below in both cases, so a file one execution can read is a file the
//! other can.

use std::path::Path;
use std::sync::Arc;

use migo_io::scheduler::IoScheduler;
use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::vfs::{GamePaths, MountTable, VirtualFS};

/// A game's paths and its sandbox, before the mount table exists.
///
/// The two halves are separate steps because the embedded runtime verifies the
/// package's signature between them: the directories have to exist to be
/// verified, and nothing may be mounted until they are.
pub struct PreparedContent {
    pub game_paths: GamePaths,
    pub vfs: VirtualFS,
}

/// Build a game's paths and sandbox, creating its directories.
///
/// `session_id` keeps `/tmp` private to one session; see [`GamePaths::new`].
pub fn prepare_content(
    files_dir: &Path,
    cache_dir: &Path,
    game_id: &str,
    session_id: i32,
) -> EngineResult<PreparedContent> {
    let game_paths = GamePaths::new(files_dir, cache_dir, game_id, session_id).map_err(|e| {
        EngineError::new(ErrorCode::InvalidArgument)
            .with_msg("create game paths")
            .with_detail(e.to_string())
    })?;
    game_paths.ensure_directories().map_err(|e| {
        EngineError::new(ErrorCode::IoError)
            .with_msg("create game directories")
            .with_detail(e.to_string())
    })?;
    // Pin every sandbox root before exposing the VFS to untrusted code. A
    // path-only VFS would allow the mapping root itself to be replaced between
    // construction and a later read.
    let vfs = VirtualFS::try_from_game_paths(&game_paths).map_err(|_| {
        EngineError::new(ErrorCode::IoError)
            .with_msg("initialize sandbox filesystem")
            .with_detail("failed to pin sandbox roots")
    })?;
    Ok(PreparedContent { game_paths, vfs })
}

/// Mount a game's code: the base package, and the subpackages a previous
/// session installed.
///
/// `code_signing_enabled` skips restoring downloaded subpackages, which carry no
/// signature. Also schedules the derived-texture cache prune: it gains a sidecar
/// on every decode miss and nothing else caps the directory, so its budget only
/// holds if someone prunes at session start. Fire-and-forget on the background
/// lane -- a launch does not wait for a directory scan.
pub fn mount_code(
    game_paths: &GamePaths,
    code_signing_enabled: bool,
    scheduler: &IoScheduler,
) -> Arc<MountTable> {
    let mount_table = Arc::new(MountTable::new(game_paths.code_dir().to_path_buf()));
    shared::vfs::mount::restore_installed_packages(
        &mount_table,
        game_paths.cache_dir(),
        code_signing_enabled,
    );
    migo_io::schedule_derived_cache_prune(scheduler, game_paths.cache_dir());
    mount_table
}

/// A game's content, mounted: what every service that touches a file reads
/// through.
#[derive(Clone)]
pub struct MountedContent {
    pub game_paths: Arc<GamePaths>,
    pub vfs: Arc<VirtualFS>,
    pub mount_table: Arc<MountTable>,
}

impl MountedContent {
    /// Prepare and mount in one step, for an execution that verifies no
    /// signature in between.
    pub fn mount(
        files_dir: &Path,
        cache_dir: &Path,
        game_id: &str,
        session_id: i32,
        scheduler: &IoScheduler,
    ) -> EngineResult<Self> {
        let PreparedContent { game_paths, vfs } =
            prepare_content(files_dir, cache_dir, game_id, session_id)?;
        let mount_table = mount_code(&game_paths, false, scheduler);
        Ok(Self {
            game_paths: Arc::new(game_paths),
            vfs: Arc::new(vfs),
            mount_table,
        })
    }
}

impl std::fmt::Debug for MountedContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MountedContent")
            .field("game_paths", &self.game_paths)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounting_creates_the_game_directories_and_resolves_code_under_the_install_root() {
        let root =
            std::env::temp_dir().join(format!("migo-services-content-{}", std::process::id()));
        let files = root.join("files");
        let cache = root.join("cache");
        // Installed first, as a host does: the code directory is the package's,
        // created by deployment, and mounting content that was never installed
        // is refused rather than mounted empty.
        let installed = GamePaths::new(&files, &cache, "my-game", 1).unwrap();
        std::fs::create_dir_all(installed.code_dir()).unwrap();
        let scheduler = IoScheduler::new(1);
        let mounted =
            MountedContent::mount(&files, &cache, "my-game", 1, &scheduler).expect("a valid id");
        // The layout the platform SDKs install into and the C header documents.
        assert!(mounted.game_paths.code_dir().starts_with(&files));
        assert!(mounted.game_paths.code_dir().ends_with("code"));
        assert!(mounted.game_paths.user_data_dir().is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn content_that_was_never_installed_is_refused() {
        let root =
            std::env::temp_dir().join(format!("migo-services-absent-{}", std::process::id()));
        let scheduler = IoScheduler::new(1);
        let error =
            MountedContent::mount(&root.join("f"), &root.join("c"), "absent", 1, &scheduler)
                .expect_err("no code directory");
        assert_eq!(error.code, ErrorCode::IoError);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_invalid_game_id_is_refused_before_anything_is_created() {
        let root = std::env::temp_dir().join(format!("migo-services-badid-{}", std::process::id()));
        let scheduler = IoScheduler::new(1);
        let error =
            MountedContent::mount(&root.join("f"), &root.join("c"), "../escape", 1, &scheduler)
                .expect_err("a traversal is not a game id");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(
            !root.join("f").exists(),
            "nothing was created for a refused id"
        );
    }
}
