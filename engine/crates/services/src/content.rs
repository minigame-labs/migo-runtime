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

/// How a session treats the signature on the content it mounts: the choice
/// `InitOptions` makes, resolved once when the session starts.
#[cfg(feature = "code-signing")]
#[derive(Clone, Debug)]
pub enum ContentSigning {
    /// Content is not verified.
    Disabled,
    /// Every package is verified against this key before anything is served.
    Verify(shared::vfs::integrity::IntegrityVerifier),
    /// Verification was asked for and cannot be done -- no key, or a key that
    /// does not parse. Fail closed: every load is refused with this error, as
    /// the embedded execution refuses every module.
    Misconfigured(EngineError),
}

#[cfg(feature = "code-signing")]
impl ContentSigning {
    /// The same decision, and the same errors, as the embedded execution's
    /// `HostJsRuntime` makes from the same options.
    pub fn from_options(enabled: bool, public_key_hex: Option<&str>) -> Self {
        if !enabled {
            return Self::Disabled;
        }
        match public_key_hex {
            Some(key) if !key.is_empty() => {
                match shared::vfs::integrity::IntegrityVerifier::from_hex_pubkey(key) {
                    Ok(verifier) => Self::Verify(verifier),
                    Err(error) => Self::Misconfigured(error),
                }
            }
            _ => Self::Misconfigured(
                EngineError::new(ErrorCode::CodeSignatureInvalid)
                    .with_msg("code signing enabled but public key is missing")
                    .with_detail("set InitOptions.code_signing_pubkey (hex Ed25519 public key)"),
            ),
        }
    }
}

#[cfg(feature = "code-signing")]
impl MountedContent {
    /// Verify, then mount -- the embedded execution's launch sequence, for an
    /// execution whose module loader is in another process.
    ///
    /// Verification happens before anything is mounted, so no byte of an
    /// unverified package can be served. A sealed launch receipt answers a
    /// relaunch without re-hashing; a miss verifies every file and seals the
    /// tree. Subpackages are not restored under signing: downloaded packages
    /// carry no signature.
    pub fn mount_signed(
        files_dir: &Path,
        cache_dir: &Path,
        game_id: &str,
        entry: &str,
        session_id: i32,
        scheduler: &IoScheduler,
        signing: &ContentSigning,
    ) -> EngineResult<Self> {
        let verifier = match signing {
            ContentSigning::Disabled => {
                return Self::mount(files_dir, cache_dir, game_id, session_id, scheduler);
            }
            ContentSigning::Misconfigured(error) => return Err(error.clone()),
            ContentSigning::Verify(verifier) => verifier,
        };
        let PreparedContent { game_paths, vfs } =
            prepare_content(files_dir, cache_dir, game_id, session_id)?;
        let code_dir = game_paths.code_dir().to_path_buf();
        let receipt = game_paths.integrity_receipt_path();
        if verifier
            .verify_launch_receipt(&code_dir, &receipt, entry)?
            .is_none()
        {
            verifier.verify_and_promote_for_launch(&code_dir, &receipt, entry)?;
        }
        let mount_table = mount_code(&game_paths, true, scheduler);
        Ok(Self {
            game_paths: Arc::new(game_paths),
            vfs: Arc::new(vfs),
            mount_table,
        })
    }
}

/// Why a content module could not be served.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleError {
    /// Nothing is there.
    NotFound(String),
    /// Something is there that content may not load as a module: a path that
    /// leaves the package, or one an overlay shadows.
    Refused(String),
    /// It is there and could not be read as module text.
    Unreadable(String),
}

impl std::fmt::Display for ModuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(why) | Self::Refused(why) | Self::Unreadable(why) => f.write_str(why),
        }
    }
}

impl MountedContent {
    /// The text the module at `request_path` -- a path in the package, as the
    /// content origin receives it: `/js/main.js` -- is evaluated as.
    ///
    /// The embedded runtime's module loader's rules, for a loader that is
    /// WebKit's: resolved through the mount table, so a subpackage overlay or a
    /// pack-backed package serves its own files and a path an overlay claims
    /// is not found underneath it; contained, so a symlink out of the package
    /// is refused; UTF-8, as V8 requires; and rewritten by
    /// [`shared::cjs_compat::module_source`], so a CommonJS `game.js` runs as
    /// the same module in both executions.
    pub fn module_source(&self, request_path: &str) -> Result<Vec<u8>, ModuleError> {
        let relative = request_path.strip_prefix('/').unwrap_or(request_path);
        let mounts = &self.mount_table;
        let bytes = match mounts.resolve(relative) {
            // `resolve` verified the real path is inside its mount.
            Some(found) => match found.real_path {
                Some(path) => std::fs::read(&path).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        ModuleError::NotFound(format!("no module at {request_path}"))
                    } else {
                        ModuleError::Unreadable(format!("{request_path}: {error}"))
                    }
                })?,
                None => mounts.read(relative).map_err(|error| {
                    ModuleError::Unreadable(format!(
                        "failed to read module from package: {request_path}: {error}"
                    ))
                })?,
            },
            None if mounts.has_overlay_for(relative) => {
                return Err(ModuleError::Refused(format!(
                    "module import blocked by mounted overlay shadow: {request_path}"
                )));
            }
            None => {
                return Err(ModuleError::NotFound(format!(
                    "no module at {request_path}"
                )));
            }
        };
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            ModuleError::Unreadable(format!(
                "module source is not UTF-8: {request_path}: {error}"
            ))
        })?;
        Ok(match shared::cjs_compat::module_source(text) {
            Some(rewritten) => rewritten.into_bytes(),
            None => bytes,
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

    /// An installed package, signed as a publisher signs one. Answers the key
    /// it verifies against, hex-encoded as `InitOptions` carries it.
    #[cfg(feature = "code-signing")]
    fn install_signed(code: &Path, files: &[(&str, &str)], seed: u8) -> String {
        std::fs::create_dir_all(code).unwrap();
        for (name, text) in files {
            std::fs::write(code.join(name), text).unwrap();
        }
        let key = shared::vfs::integrity::sign_package_fixture(code, "game.js", [seed; 32]);
        key.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Restore owner write on a sealed tree so the test can delete it -- the
    /// trusted uninstall every installer performs.
    #[cfg(all(feature = "code-signing", unix))]
    fn unseal(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            let child = entry.path();
            if child.is_dir() && !child.is_symlink() {
                let _ = std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o755));
                unseal(&child);
            }
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }

    /// The external execution's launch sequence under signing: what verifies is
    /// mounted and sealed; what does not is refused before anything is mounted;
    /// a misconfigured session refuses every load, as the embedded one does.
    #[cfg(all(feature = "code-signing", unix))]
    #[test]
    fn signed_content_is_verified_and_sealed_before_it_is_mounted() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("migo-services-signed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files = root.join("files");
        let cache = root.join("cache");
        let scheduler = IoScheduler::new(1);
        let code_of = |id: &str| {
            GamePaths::new(&files, &cache, id, 1)
                .unwrap()
                .code_dir()
                .to_path_buf()
        };
        let hex = install_signed(&code_of("good"), &[("game.js", "console.log(1)")], 7);
        let signing = ContentSigning::from_options(true, Some(&hex));
        assert!(matches!(signing, ContentSigning::Verify(_)));

        let mounted = MountedContent::mount_signed(
            &files, &cache, "good", "game.js", 1, &scheduler, &signing,
        )
        .expect("a package signed with the session's key mounts");
        let mode = std::fs::metadata(mounted.game_paths.code_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o222, 0, "the verified tree is sealed read-only");
        // A relaunch is answered by the sealed receipt, and mounts again.
        MountedContent::mount_signed(&files, &cache, "good", "game.js", 2, &scheduler, &signing)
            .expect("a sealed package relaunches");

        install_signed(&code_of("tampered"), &[("game.js", "console.log(1)")], 7);
        std::fs::write(code_of("tampered").join("game.js"), "console.log(2)").unwrap();
        let error = MountedContent::mount_signed(
            &files, &cache, "tampered", "game.js", 1, &scheduler, &signing,
        )
        .err()
        .expect("a file changed after signing is refused");
        assert!(error.to_string().contains("hash"), "{error}");

        std::fs::create_dir_all(code_of("unsigned")).unwrap();
        std::fs::write(code_of("unsigned").join("game.js"), "console.log(1)").unwrap();
        assert!(
            MountedContent::mount_signed(
                &files, &cache, "unsigned", "game.js", 1, &scheduler, &signing
            )
            .is_err(),
            "a package with no signature is refused when the session verifies"
        );
        MountedContent::mount_signed(
            &files,
            &cache,
            "unsigned",
            "game.js",
            1,
            &scheduler,
            &ContentSigning::Disabled,
        )
        .expect("and mounts when it does not");

        let missing = ContentSigning::from_options(true, None);
        let error = MountedContent::mount_signed(
            &files, &cache, "good", "game.js", 3, &scheduler, &missing,
        )
        .err()
        .expect("signing on and no key is fail-closed");
        assert!(
            error.to_string().contains("public key is missing"),
            "{error}"
        );

        unseal(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_module_is_served_as_the_loader_evaluates_it() {
        let root =
            std::env::temp_dir().join(format!("migo-services-modules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files = root.join("files");
        let cache = root.join("cache");
        let installed = GamePaths::new(&files, &cache, "m", 1).unwrap();
        let code = installed.code_dir().to_path_buf();
        std::fs::create_dir_all(code.join("js")).unwrap();
        std::fs::write(code.join("game.js"), "require('./js/main');").unwrap();
        std::fs::write(code.join("js/main.mjs"), "export const a = 1;").unwrap();
        std::fs::write(code.join("bad.js"), [0xff, 0xfe]).unwrap();
        let scheduler = IoScheduler::new(1);
        let mounted = MountedContent::mount(&files, &cache, "m", 1, &scheduler).unwrap();

        let game = mounted.module_source("/game.js").unwrap();
        assert_eq!(
            game,
            shared::cjs_compat::wrap_cjs("require('./js/main');").into_bytes(),
            "CommonJS runs wrapped, as in the embedded loader"
        );
        assert_eq!(
            mounted.module_source("/js/main.mjs").unwrap(),
            b"export const a = 1;",
            "an ES module is served as written"
        );
        assert!(matches!(
            mounted.module_source("/absent.js"),
            Err(ModuleError::NotFound(_))
        ));
        assert!(matches!(
            mounted.module_source("/bad.js"),
            Err(ModuleError::Unreadable(_))
        ));
        assert!(
            mounted.module_source("/../files/escape.js").is_err(),
            "a path out of the package resolves to nothing"
        );
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
