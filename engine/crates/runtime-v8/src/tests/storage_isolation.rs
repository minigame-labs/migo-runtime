//! Key-value storage belongs to one game, not to the host app.
//!
//! This product is sold into game centres: one host app, a catalogue of
//! third-party titles, one after another in the same process. Storage was
//! anchored to the host app's files directory while code, cache and user data
//! were already per game, so every title in a catalogue shared one SQLite file.
//! Three consequences, all reachable through ordinary storage APIs:
//!
//! - one game could read another's saves by guessing keys;
//! - `migo.clearStorage()` -- a normal call for a game resetting itself --
//!   wiped the whole catalogue;
//! - the 10 MB quota was a shared pool a single game could exhaust.
//!
//! It also disagreed with the mini-game platforms this engine is compatible
//! with, where each game has its own 10 MB.
//!
//! These tests pin the isolation at the path level, where the bug was, rather
//! than by writing through the ops: the ops route through the IO scheduler and
//! a real SQLite file, which tests the scheduler and SQLite rather than the
//! question at hand.

#[cfg(test)]
mod storage_isolation_tests {
    use std::path::PathBuf;

    use shared::vfs::game_paths::GamePaths;

    fn paths_for(game_id: &str) -> GamePaths {
        GamePaths::new(
            PathBuf::from("/tmp/host-app/files"),
            PathBuf::from("/tmp/host-app/cache"),
            game_id,
            1,
        )
        .expect("game paths")
    }

    /// The property the fix exists for: two games in one host app never share a
    /// storage root.
    #[test]
    fn two_games_in_one_host_app_get_separate_storage_roots() {
        let a = paths_for("game-a");
        let b = paths_for("game-b");

        assert_ne!(
            a.user_data_dir(),
            b.user_data_dir(),
            "two games share a user-data directory, so they would share storage"
        );
        assert_ne!(
            a.cache_dir(),
            b.cache_dir(),
            "two games share a cache directory, so they would share buffer URLs"
        );
    }

    /// Neither root may be an ancestor of the other: nesting would let one
    /// game's recursive cleanup take the other's data with it.
    #[test]
    fn neither_game_root_contains_the_other() {
        let a = paths_for("game-a");
        let b = paths_for("game-b");

        assert!(
            !a.user_data_dir().starts_with(b.user_data_dir()),
            "game-a's storage sits inside game-b's"
        );
        assert!(
            !b.user_data_dir().starts_with(a.user_data_dir()),
            "game-b's storage sits inside game-a's"
        );
    }

    /// Anchored to the game, not to the host app.
    ///
    /// This is the assertion that fails if `storage_dir` ever goes back to
    /// `app_files_dir`: the host app's files directory is shared, so a storage
    /// root that is *only* one directory below it belongs to every game at once.
    #[test]
    fn the_storage_root_is_below_the_game_directory_not_the_app_directory() {
        let app_files = PathBuf::from("/tmp/host-app/files");
        let paths = paths_for("game-a");

        let user_data = paths.storage().directory();
        assert!(
            user_data.starts_with(&app_files),
            "private storage must live under the host app's persistent files directory"
        );

        // The game id has to appear on the path between the two, which is what
        // makes it per game rather than per app.
        let relative = user_data
            .strip_prefix(&app_files)
            .expect("user data under app files");
        assert!(
            relative.components().any(|c| c.as_os_str() == "game-a"),
            "the game id is absent from {relative:?}; the root is not per game"
        );
    }

    // -----------------------------------------------------------------
    // The resolver itself. The tests above pin what `GamePaths` means;
    // these pin that storage actually asks it, which is where the bug was.
    // -----------------------------------------------------------------

    use std::sync::{Arc, atomic::AtomicBool};

    use deno_core::OpState;
    use shared::{
        channel::ThreadWakeup,
        device::gpu_caps::GpuCaps,
        network_policy::NetworkPolicy,
        op_state::{AudioSender, HostOpState},
        render_command_sender::CommandSender,
    };

    /// An op state carrying the host app's directories, with or without a
    /// loaded game.
    fn op_state_with(game: Option<GamePaths>) -> OpState {
        let mut state = OpState::new(None);
        state.put(host_state_with(game));
        state
    }

    fn host_state_with(game: Option<GamePaths>) -> HostOpState {
        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: std::sync::Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id: 1,
            app_cache_dir: PathBuf::from("/tmp/host-app/cache"),
            app_files_dir: PathBuf::from("/tmp/host-app/files"),
            code_dir: None,
            game_paths: game.map(Arc::new),
            vfs: None,
            mount_table: None,
            render_tx,
            text_measurer: None,
            audio_tx: AudioSender::new(shared::audio_channel::disconnected(), ThreadWakeup::new()),
            host_tx,
            device_services: None,
            raf_rx: None,
            raf_demand: Arc::new(shared::raf_signal::RafDemand::new()),
            request_vsync: None,
            sub_packages: Vec::new(),
            workers_path: None,
            network_policy: NetworkPolicy::default(),
            backgrounded: Arc::new(AtomicBool::new(false)),
            timer_backgrounded: Arc::new(AtomicBool::new(false)),
            webgl_context_created: Arc::new(AtomicBool::new(false)),
            context_lost: Arc::new(shared::op_state::ContextLostState::default()),
            code_signing_enabled: false,
            gpu_caps: GpuCaps::new(),
        }
    }

    #[test]
    fn actual_buffer_url_ops_use_the_loaded_vfs_and_return_virtual_paths() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = executor.enter();
        let root = std::env::temp_dir().join(format!("migo-v8-buffer-url-{}", std::process::id()));
        let paths = GamePaths::new(root.join("files"), root.join("cache"), "game", 1).unwrap();
        paths.ensure_directories().unwrap();
        std::fs::create_dir_all(paths.code_dir()).unwrap();
        let mut host = host_state_with(Some(paths.clone()));
        host.vfs = Some(Arc::new(
            shared::vfs::VirtualFS::try_from_game_paths(&paths).unwrap(),
        ));
        deno_core::extension!(
            buffer_url_actual_ops,
            ops = [
                crate::storage::op_create_buffer_url,
                crate::storage::op_revoke_buffer_url
            ]
        );
        let mut runtime = deno_core::JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![buffer_url_actual_ops::init()],
            ..Default::default()
        });
        runtime.op_state().borrow_mut().put(host);
        runtime.execute_script("create-buffer-url", r#"
            globalThis.issued = Deno.core.ops.op_create_buffer_url(new Uint8Array([2,4,8]));
            if (!issued.startsWith('/cache/buffer_urls/buffer-')) throw new Error('host path escaped');
        "#).unwrap();
        let directory = paths.sandbox_cache_dir().join("buffer_urls");
        let files: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(std::fs::read(&files[0]).unwrap(), [2, 4, 8]);
        runtime
            .execute_script(
                "revoke-buffer-url",
                "Deno.core.ops.op_revoke_buffer_url(issued);",
            )
            .unwrap();
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        drop(runtime);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The regression itself: two loaded games must resolve to different files.
    #[test]
    fn the_resolver_gives_two_games_different_storage_files() {
        let a = crate::storage::storage_dir(&op_state_with(Some(paths_for("game-a"))))
            .expect("game-a storage");
        let b = crate::storage::storage_dir(&op_state_with(Some(paths_for("game-b"))))
            .expect("game-b storage");
        assert_ne!(a, b, "both games resolved to {a:?}");
    }

    /// And neither is the host app's shared directory, which is what it used to
    /// be. Pinned by name so a reintroduction reads as what it is.
    #[test]
    fn the_resolver_never_returns_the_host_app_directory() {
        let app_shared = PathBuf::from("/tmp/host-app/files").join("kv_storage");
        let resolved = crate::storage::storage_dir(&op_state_with(Some(paths_for("game-a"))))
            .expect("game-a storage");
        assert_ne!(
            resolved, app_shared,
            "storage resolved to the host app's shared directory"
        );

        let cache_shared = PathBuf::from("/tmp/host-app/cache").join("buffer_urls");
        let buffers = crate::storage::buffer_url_dir(&op_state_with(Some(paths_for("game-a"))))
            .expect("game-a buffer urls");
        assert_ne!(
            buffers, cache_shared,
            "buffer URLs resolved to the host app's shared directory"
        );
    }

    /// With no game loaded there is no correct answer, and the old fallback --
    /// the host app's directory -- is the shared file this exists to prevent.
    /// Failing is the only safe result.
    #[test]
    fn the_resolver_fails_when_no_game_is_loaded() {
        assert!(
            crate::storage::storage_dir(&op_state_with(None)).is_err(),
            "storage resolved with no game loaded; it fell back to a shared location"
        );
        assert!(
            crate::storage::buffer_url_dir(&op_state_with(None)).is_err(),
            "buffer URLs resolved with no game loaded"
        );
    }

    /// A game id that could climb out of its directory would defeat the split
    /// regardless of where the root is anchored.
    #[test]
    fn game_ids_that_could_escape_are_rejected() {
        for hostile in ["..", "../other", "a/../../b", "/absolute", "a/b"] {
            assert!(
                GamePaths::new(
                    PathBuf::from("/tmp/host-app/files"),
                    PathBuf::from("/tmp/host-app/cache"),
                    hostile,
                    1,
                )
                .is_err(),
                "game id {hostile:?} was accepted; it can reach another game's storage"
            );
        }
    }
}
