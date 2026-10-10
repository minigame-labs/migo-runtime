//! Per-session lifecycle of the sandbox `/tmp` directory.
//!
//! `/tmp` is documented to start empty for a session and not to outlive it.
//! Android's Java SDK enforces that with `GamePaths.sweepAbandonedTemp`, run
//! once per session before the engine starts. Every other host — Linux,
//! Windows, OpenHarmony, and any bare C embedder — reaches the engine through
//! this crate and had no equivalent: `GamePaths::clean_temp` had no caller, so
//! a session's `tmp/{id}` subtree was created at module eval and never removed.
//!
//! [`SessionTemp`] is that missing lifecycle, as an RAII guard the host holds
//! for exactly one session. It is not the abandoned-directory sweep — that
//! needs a live-session predicate this layer does not have — so a host that
//! crashes still leaves `tmp/{id}` behind for an id other than the next
//! session's. Same-id reuse is covered, because session ids are a per-process
//! counter and a fresh process starts again from 1.

use std::path::Path;

use shared::vfs::game_paths::GamePaths;

use super::HostId;

/// Owns one session's temporary directory for the length of that session.
///
/// It is also where files a host hands over in a result land
/// (`shared::services::host_files`), so the directory is registered there for
/// the same span.
pub(crate) struct SessionTemp {
    paths: GamePaths,
    session_id: HostId,
}

impl SessionTemp {
    /// Guarantee `/tmp` starts empty for this session and take responsibility
    /// for removing it at teardown.
    ///
    /// Returns `None` when `game_id` cannot form a path; the session is about
    /// to fail module evaluation for the same reason, so there is nothing to
    /// own. The clean is best effort: module evaluation's own
    /// `ensure_directories` recreates the directory regardless, and a session
    /// that cannot clear a stale `/tmp` is not one to abort over it.
    pub(crate) fn prepare(
        files_dir: &Path,
        cache_dir: &Path,
        game_id: &str,
        session_id: HostId,
    ) -> Option<Self> {
        let paths = GamePaths::new(files_dir, cache_dir, game_id, session_id).ok()?;
        let _ = paths.clean_temp();
        shared::services::host_files::register_session_temp(
            session_id,
            paths.temp_dir().to_path_buf(),
        );
        Some(Self { paths, session_id })
    }
}

impl Drop for SessionTemp {
    fn drop(&mut self) {
        shared::services::host_files::unregister_session(self.session_id);
        let _ = self.paths.remove_temp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_root(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after the epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "migo_session_temp_{tag}_{}_{nanos}",
            std::process::id()
        ))
    }

    /// A crash leaves `tmp/{id}` behind, and the next run's first session gets
    /// the same numbered directory. The stale contents must be gone before the
    /// game can read `/tmp`.
    #[test]
    fn prepare_empties_tmp_left_by_a_previous_session_with_the_same_id() {
        let root = scratch_root("prepare");
        let files = root.join("files");
        let cache = root.join("cache");
        let stale = GamePaths::new(&files, &cache, "game", 1).unwrap();
        stale.ensure_directories().unwrap();
        std::fs::write(stale.temp_dir().join("stale.bin"), b"dead session").unwrap();

        let guard = SessionTemp::prepare(&files, &cache, "game", 1).expect("valid game id");

        assert_eq!(
            std::fs::read_dir(stale.temp_dir()).unwrap().count(),
            0,
            "prepare did not clear the reused temp directory"
        );
        drop(guard);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The session ending takes its own `/tmp` with it.
    #[test]
    fn dropping_the_guard_removes_this_sessions_temp_subtree() {
        let root = scratch_root("drop");
        let files = root.join("files");
        let cache = root.join("cache");
        let paths = GamePaths::new(&files, &cache, "game", 7).unwrap();

        let guard = SessionTemp::prepare(&files, &cache, "game", 7).expect("valid game id");
        paths.ensure_directories().unwrap();
        std::fs::write(paths.temp_dir().join("scratch.bin"), b"in use").unwrap();
        assert!(paths.temp_dir().exists());

        drop(guard);

        assert!(
            !paths.temp_dir().exists(),
            "the session's temp subtree survived teardown"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A second live session's temp directory is a sibling, so teardown of one
    /// must not reach it.
    #[test]
    fn teardown_leaves_a_concurrent_sessions_temp_directory_alone() {
        let root = scratch_root("sibling");
        let files = root.join("files");
        let cache = root.join("cache");
        let other = GamePaths::new(&files, &cache, "game", 2).unwrap();
        other.ensure_directories().unwrap();
        std::fs::write(other.temp_dir().join("live.bin"), b"still running").unwrap();

        let guard = SessionTemp::prepare(&files, &cache, "game", 1).expect("valid game id");
        drop(guard);

        assert!(
            other.temp_dir().join("live.bin").exists(),
            "a concurrent session's temp files were removed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepare_returns_none_for_a_game_id_that_cannot_form_a_path() {
        let root = scratch_root("badid");
        assert!(
            SessionTemp::prepare(&root.join("files"), &root.join("cache"), "../hack", 1).is_none()
        );
    }
}
