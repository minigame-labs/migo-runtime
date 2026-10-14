//! Mount-aware code source abstraction for `/code` path resolution.
//!
//! The [`MountTable`] replaces the previous model where `/code` was a bare
//! directory path. It introduces:
//!
//! - **Base mount**: the primary code package directory.
//! - **Overlay mounts**: subpackage directories that shadow subtrees of the base.
//! - **Generation counter**: monotonically increasing on every structural change,
//!   used as part of cache keys to prevent stale hits after hot-update / subpackage
//!   install.
//! - **[`StagingArea`]**: RAII helper for atomic subpackage installation
//!   (extract to staging dir, validate, rename into place, mount).
//!
//! # Extension points
//!
//! [`MountBackend`] is a trait.  The only implementation today is [`DirSource`]
//! (directory-backed), but the trait is designed for future backends:
//!
//! - Seekable pack files (single-file archive with index)
//! - Zstd-seekable compressed archives
//! - In-memory test fixtures
//!
//! Callers that need a real filesystem path use [`MountBackend::real_path`],
//! which returns `None` for non-filesystem backends — forcing those callers
//! to migrate to [`MountBackend::read`] when pack support lands.

use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lru::LruCache;
use parking_lot::{Mutex, RwLock};

// ---------------------------------------------------------------------------
// MountBackend trait
// ---------------------------------------------------------------------------

/// A backend that serves files for a mounted code source.
///
/// All paths passed to methods are **relative** (no leading `/`), e.g.
/// `"lib/utils.js"` or `"subpackages/stage1/game.js"`.
pub trait MountBackend: Send + Sync + fmt::Debug {
    /// Read entire file contents.
    fn read(&self, relative_path: &str) -> io::Result<Vec<u8>>;

    /// Check whether a file exists.
    fn exists(&self, relative_path: &str) -> bool;

    /// Map a relative path to a real filesystem path.
    ///
    /// Returns `None` for non-filesystem backends (future pack files).
    /// Callers that receive `None` must fall back to [`read`](MountBackend::read).
    fn real_path(&self, relative_path: &str) -> Option<PathBuf>;

    /// Root directory of this source, if directory-backed.
    fn root_dir(&self) -> Option<&Path>;

    /// An identity for this source's contents that means the same thing in every
    /// Session, for backends whose files have no real path of their own.
    ///
    /// Directory-backed sources return `None` and want nothing here: a caller can
    /// key on the resolved real path, which is already globally meaningful. A
    /// package has no such path, so without this a cache key can only fall back on
    /// the mount table position -- which is per-Session and identical across
    /// Sessions, and so collides. Returning `None` is always safe: callers must
    /// substitute [`ResolvedCode::source_identity`]'s per-mount fallback rather
    /// than share.
    fn source_identity(&self) -> Option<u64> {
        None
    }

    /// Compute file size + digest for a file entry.
    fn get_file_info(&self, _relative_path: &str, _algorithm: &str) -> io::Result<(u64, String)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "get_file_info not supported by this backend",
        ))
    }

    fn copy_to_path(&self, _relative_path: &str, _dest_path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "copy_to_path not supported by this backend",
        ))
    }

    fn copy_to_writer(&self, _relative_path: &str, _writer: &mut dyn io::Write) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "copy_to_writer not supported by this backend",
        ))
    }

    /// Check if a path is a regular file (not a directory).
    ///
    /// For directory backends this checks the filesystem.
    /// For pack backends this checks the index for an exact entry match.
    fn is_file(&self, relative_path: &str) -> bool {
        let _ = relative_path;
        false
    }

    /// Check if a path represents a directory (has children).
    ///
    /// For directory backends this checks the filesystem.
    /// For pack backends this checks if any entry has this prefix.
    fn is_dir(&self, relative_path: &str) -> bool {
        let _ = relative_path;
        false
    }

    /// List immediate children of a directory prefix.
    ///
    /// `dir_prefix` is `""` for root, `"sub/dir"` for a subdirectory.
    /// Returns entry names (not full paths), e.g. `["main.js", "lib"]`.
    /// Directories are inferred from path components.
    ///
    /// Default implementation returns empty (override for pack backends).
    fn list_dir(&self, dir_prefix: &str) -> Vec<String> {
        let _ = dir_prefix;
        Vec::new()
    }

    /// Get the uncompressed size of an entry, if available without reading.
    ///
    /// Default returns `None` (caller must read + measure).
    fn entry_size(&self, relative_path: &str) -> Option<u64> {
        let _ = relative_path;
        None
    }

    /// Read a byte range with an inflate-size limit.
    ///
    /// For compressed pack entries, if the uncompressed size exceeds
    /// `max_inflate`, the read is rejected instead of inflating.
    /// Default implementation ignores the limit (filesystem backends
    /// don't inflate).
    fn read_range_limited(
        &self,
        relative_path: &str,
        position: u64,
        length: Option<u64>,
        _max_inflate: u64,
    ) -> io::Result<Vec<u8>> {
        self.read_range(relative_path, position, length)
    }

    /// Read a byte range from a file.
    ///
    /// For stored pack entries, this avoids full decompression.
    /// Default implementation reads the full file and slices.
    fn read_range(
        &self,
        relative_path: &str,
        position: u64,
        length: Option<u64>,
    ) -> io::Result<Vec<u8>> {
        let mut data = self.read(relative_path)?;
        let pos = position as usize;
        if pos >= data.len() {
            return Ok(Vec::new());
        }
        data = data[pos..].to_vec();
        if let Some(len) = length {
            data.truncate(len as usize);
        }
        Ok(data)
    }
}

// ---------------------------------------------------------------------------
// DirSource — directory-backed mount backend
// ---------------------------------------------------------------------------

/// A [`MountBackend`] backed by a plain directory on the local filesystem.
#[derive(Debug, Clone)]
pub struct DirSource {
    root: PathBuf,
}

impl DirSource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl MountBackend for DirSource {
    fn read(&self, relative_path: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.root.join(relative_path))
    }

    fn exists(&self, relative_path: &str) -> bool {
        self.root.join(relative_path).exists()
    }

    fn real_path(&self, relative_path: &str) -> Option<PathBuf> {
        let p = self.root.join(relative_path);
        // Only return Some when the target actually exists on disk.
        // Returning Some for non-existent paths would prevent overlay parent
        // directory synthesis from running in resolve().
        if p.exists() { Some(p) } else { None }
    }

    fn root_dir(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn is_file(&self, relative_path: &str) -> bool {
        if relative_path.is_empty() {
            return false; // root is a directory
        }
        self.root.join(relative_path).is_file()
    }

    fn is_dir(&self, relative_path: &str) -> bool {
        let path = if relative_path.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative_path)
        };
        path.is_dir()
    }

    fn list_dir(&self, dir_prefix: &str) -> Vec<String> {
        let dir = if dir_prefix.is_empty() {
            self.root.clone()
        } else {
            self.root.join(dir_prefix)
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn entry_size(&self, relative_path: &str) -> Option<u64> {
        std::fs::metadata(self.root.join(relative_path))
            .ok()
            .map(|m| m.len())
    }

    fn copy_to_writer(&self, relative_path: &str, writer: &mut dyn io::Write) -> io::Result<()> {
        let mut file = std::fs::File::open(self.root.join(relative_path))?;
        std::io::copy(&mut file, writer)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ResolvedCode — result of mount resolution
// ---------------------------------------------------------------------------

/// The result of resolving a path through the [`MountTable`].
#[derive(Debug, Clone)]
pub struct ResolvedCode {
    /// Real filesystem path (present for directory-backed mounts).
    /// `None` for pack-backed mounts — use [`MountTable::read`] instead.
    pub real_path: Option<PathBuf>,
    /// Name of the mount that served this file (e.g. `"base"`, `"subpackage:stage1"`).
    pub mount_name: String,
    /// Mount table generation at resolution time.
    pub mount_generation: u64,
    /// Generation when the specific source (overlay or base) was mounted.
    /// Changes only when THIS source is replaced, not when other sources change.
    pub source_mounted_at: u64,
    /// Identity of the mount that served this path, meaningful across Sessions.
    ///
    /// [`source_mounted_at`](Self::source_mounted_at) is **not** usable for that:
    /// it counts mounts within one `MountTable`, every Session owns its own, and a
    /// base mount is `1` in all of them -- so two Sessions resolving the same
    /// virtual path out of different packages produce the same value. Any cache
    /// shared between Sessions must key on this instead.
    ///
    /// It is the backend's own [`MountBackend::source_identity`] when it has one,
    /// so two Sessions mounting byte-identical packages agree and can share the
    /// bytes; otherwise it is an id unique to this mount within the process, so
    /// they cannot be confused for one another. Correct by construction either
    /// way, rather than contingent on every backend implementing the accessor.
    pub source_identity: u64,
}

// ---------------------------------------------------------------------------
// MountEntry (internal)
// ---------------------------------------------------------------------------

/// Hands out an id unique to each mount for the life of the process.
///
/// The per-table `mounted_at` generation cannot do this job: it starts at the same
/// value in every `MountTable`, so two Sessions' base mounts share it.
static NEXT_MOUNT_UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_mount_uid() -> u64 {
    NEXT_MOUNT_UID.fetch_add(1, Ordering::Relaxed)
}

struct MountEntry {
    /// Human-readable name (e.g. `"base"`, `"subpackage:stage1"`).
    name: String,
    /// Path prefix relative to `/code` (no leading slash).
    /// Empty string = base mount (covers everything).
    prefix: String,
    backend: Arc<dyn MountBackend>,
    /// Generation counter value when this entry was mounted.
    /// Used as part of the replace-sensitive identity token.
    mounted_at: u64,
    /// Process-unique id for this mount, used as the cross-Session identity
    /// fallback when the backend offers none of its own.
    uid: u64,
}

impl MountEntry {
    /// What a cross-Session cache may key on. See
    /// [`ResolvedCode::source_identity`].
    fn source_identity(&self) -> u64 {
        self.backend.source_identity().unwrap_or(self.uid)
    }
}

impl fmt::Debug for MountEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MountEntry")
            .field("name", &self.name)
            .field("prefix", &self.prefix)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// MountTable
// ---------------------------------------------------------------------------

struct MountTableInner {
    base: MountEntry,
    overlays: Vec<MountEntry>,
}

/// Mount table for resolving `/code` paths through a layered overlay system.
///
/// Resolution order: overlays (last added = highest priority) then base.
///
/// **Thread safety**: reads (resolve/exists) take a reader lock; writes
/// (mount/unmount/swap) take a writer lock and bump the generation counter.
pub struct MountTable {
    inner: RwLock<MountTableInner>,
    /// Monotonic generation — incremented on every structural change.
    generation: AtomicU64,
    /// The logical `/code` directory path.  Always set regardless of whether
    /// the base mount is directory-backed or pack-backed.  Used to reverse-map
    /// `file://` URLs to `/code`-relative paths in the module loader.
    code_dir: PathBuf,
    /// Memoised results of the expensive `canonicalize` + symlink-chain
    /// verification for filesystem-backed `/code` paths, keyed by
    /// `(generation, real_path)`.
    ///
    /// `/code` is a **read-only** mount (the JS/VFS layer rejects writes to
    /// it) whose on-disk tree is immutable within a generation — every
    /// `mount_overlay` / `unmount_overlay` / `swap_base` bumps `generation`.
    /// So a real path that once passed the full symlink-escape verification
    /// stays valid until the next structural change. Including `generation`
    /// in the key means a mount mutation transparently invalidates every
    /// stale entry (they can never be read back). On a hit we skip ~10–15
    /// `stat`/`lstat`/`realpath` syscalls per read — the dominant cost of
    /// resolving a hot game asset — while `real_path()` still does its single
    /// existence `stat`, so a file deleted within a generation is still
    /// surfaced as "not found".
    verify_cache: Mutex<LruCache<(u64, PathBuf), ()>>,
}

impl fmt::Debug for MountTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner.read();
        f.debug_struct("MountTable")
            .field("generation", &self.generation.load(Ordering::Relaxed))
            .field("base", &inner.base)
            .field("overlays", &inner.overlays)
            .finish()
    }
}

impl MountTable {
    /// Capacity of the per-generation verified-path cache. Sized to hold a
    /// large game's working set of `/code` assets; LRU-evicted beyond that.
    const VERIFY_CACHE_CAPACITY: usize = 2048;

    /// Create a mount table with `base_dir` as the base code directory.
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            code_dir: base_dir.clone(),
            inner: RwLock::new(MountTableInner {
                base: MountEntry {
                    name: "base".to_string(),
                    prefix: String::new(),
                    backend: Arc::new(DirSource::new(base_dir)),
                    mounted_at: 1,
                    uid: next_mount_uid(),
                },
                overlays: Vec::new(),
            }),
            generation: AtomicU64::new(1),
            verify_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(Self::VERIFY_CACHE_CAPACITY).unwrap(),
            )),
        }
    }

    /// `verify_path_containment` for a filesystem-backed `/code` path,
    /// memoised per `(generation, real_path)`. See the `verify_cache` field
    /// doc for why this is sound for the read-only `/code` mount.
    fn verify_contained_cached(&self, real: &Path, root: &Path, generation: u64) -> bool {
        let key = (generation, real.to_path_buf());
        if self.verify_cache.lock().get(&key).is_some() {
            return true;
        }
        // Miss: run the full (expensive) verification. A benign race where
        // two threads verify the same path concurrently just does the work
        // twice and inserts the same idempotent entry — no correctness issue.
        if super::verify_path_containment(real, root, true).is_ok() {
            self.verify_cache.lock().put(key, ());
            true
        } else {
            false
        }
    }

    /// Drop all cached verifications. Called on structural mutations; keying
    /// by generation already makes stale entries unreachable, but clearing
    /// bounds memory so old generations don't linger until LRU eviction.
    fn invalidate_verify_cache(&self) {
        self.verify_cache.lock().clear();
    }

    /// Current generation counter.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    // -------------------------------------------------------------------
    // Resolution
    // -------------------------------------------------------------------

    /// Resolve a *relative* path (no `/code/` prefix) to a real filesystem
    /// path, checking overlays first (most recently added wins), then base.
    ///
    /// The relative path is normalized to prevent `..` escape.  The resolved
    /// real path is then **verified** via `canonicalize` + symlink check,
    /// matching the security guarantees of [`super::VirtualFS::resolve`].
    /// Returns `None` if the path is invalid, escapes the mount root, or
    /// fails symlink verification.
    pub fn resolve(&self, relative_path: &str) -> Option<ResolvedCode> {
        let normalized = normalize_relative_path(relative_path)?;
        let inner = self.inner.read();
        let current_gen = self.generation.load(Ordering::Acquire);

        // Overlays: last-added = highest priority, and matching overlays shadow
        // the base subtree even when the specific path is missing in the overlay.
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            match overlay.backend.real_path(sub) {
                Some(real) => {
                    if let Some(root) = overlay.backend.root_dir() {
                        if !self.verify_contained_cached(&real, root, current_gen) {
                            return None;
                        }
                    }
                    return Some(ResolvedCode {
                        real_path: Some(real),
                        mount_name: overlay.name.clone(),
                        mount_generation: current_gen,
                        source_mounted_at: overlay.mounted_at,
                        source_identity: overlay.source_identity(),
                    });
                }
                None => {
                    if overlay.backend.exists(sub) || overlay.backend.is_dir(sub) {
                        return Some(ResolvedCode {
                            real_path: None,
                            mount_name: overlay.name.clone(),
                            mount_generation: current_gen,
                            source_mounted_at: overlay.mounted_at,
                            source_identity: overlay.source_identity(),
                        });
                    }
                    if !normalized.is_empty() {
                        for candidate in &inner.overlays {
                            if candidate.prefix.starts_with(&normalized)
                                && candidate.prefix.as_bytes().get(normalized.len()) == Some(&b'/')
                            {
                                return Some(ResolvedCode {
                                    real_path: None,
                                    mount_name: "virtual-dir".to_string(),
                                    mount_generation: current_gen,
                                    source_mounted_at: candidate.mounted_at,
                                    source_identity: candidate.source_identity(),
                                });
                            }
                        }
                    }
                    return None;
                }
            }
        }

        // Base mount.
        let base_mounted_at = inner.base.mounted_at;
        let base_identity = inner.base.source_identity();
        match inner.base.backend.real_path(&normalized) {
            Some(real) => {
                if let Some(root) = inner.base.backend.root_dir() {
                    if !self.verify_contained_cached(&real, root, current_gen) {
                        return None;
                    }
                }
                return Some(ResolvedCode {
                    real_path: Some(real),
                    mount_name: inner.base.name.clone(),
                    mount_generation: current_gen,
                    source_mounted_at: base_mounted_at,
                    source_identity: base_identity,
                });
            }
            None => {
                if inner.base.backend.exists(&normalized) || inner.base.backend.is_dir(&normalized)
                {
                    return Some(ResolvedCode {
                        real_path: None,
                        mount_name: inner.base.name.clone(),
                        mount_generation: current_gen,
                        source_mounted_at: base_mounted_at,
                        source_identity: base_identity,
                    });
                }
            }
        }

        // Last resort: synthesized virtual directory from overlay parent prefix.
        if !normalized.is_empty() {
            for overlay in &inner.overlays {
                if overlay.prefix.starts_with(&normalized)
                    && overlay.prefix.as_bytes().get(normalized.len()) == Some(&b'/')
                {
                    return Some(ResolvedCode {
                        real_path: None,
                        mount_name: "virtual-dir".to_string(),
                        mount_generation: current_gen,
                        source_mounted_at: overlay.mounted_at,
                        source_identity: overlay.source_identity(),
                    });
                }
            }
        }

        None
    }

    /// Convenience: resolve a full virtual path like `/code/lib/utils.js`.
    ///
    /// Returns `None` if the path does not start with `/code`.
    pub fn resolve_code_path(&self, virtual_path: &str) -> Option<ResolvedCode> {
        let relative = if virtual_path == "/code" {
            ""
        } else {
            virtual_path.strip_prefix("/code/")?
        };
        self.resolve(relative)
    }

    /// Check whether a file exists via the mount resolution chain.
    pub fn exists(&self, relative_path: &str) -> bool {
        let Some(normalized) = normalize_relative_path(relative_path) else {
            return false;
        };
        let inner = self.inner.read();
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.exists(sub);
        }
        inner.base.backend.exists(&normalized)
    }

    /// Get the identity string for the overlay that covers this path.
    /// Returns the overlay's name (e.g. "subpackage:stage1") if an overlay
    /// matches, or empty string if the path is served by the base mount.
    /// Used as a per-subpackage stable token for loaded-state tracking.
    /// Get a replace-sensitive identity token for the source covering a path.
    ///
    /// For overlay-backed paths: returns `"{overlay_name}@{mounted_generation}"`.
    /// This changes whenever the specific overlay is re-mounted (replaced).
    /// For base-backed paths: returns `"base@{current_generation}"`.
    /// This changes whenever any mount structure change occurs (base swap, etc.).
    /// Returns empty string if mount table is not initialized or path is invalid.
    pub fn overlay_identity_for(&self, relative_path: &str) -> String {
        let Some(normalized) = normalize_relative_path(relative_path) else {
            return String::new();
        };
        let inner = self.inner.read();
        match highest_matching_overlay(&inner.overlays, &normalized) {
            Some((overlay, _)) => {
                format!("{}@{}", overlay.name, overlay.mounted_at)
            }
            // Base: use base's mounted_at, NOT global generation.
            // This only changes when swap_base is called, not when overlays change.
            None => format!("base@{}", inner.base.mounted_at),
        }
    }

    /// Whether the base package itself has this path, as a file or a directory,
    /// whatever overlay is mounted over it: what shipped with the game.
    pub fn in_base(&self, relative_path: &str) -> bool {
        let Some(normalized) = normalize_relative_path(relative_path) else {
            return false;
        };
        let inner = self.inner.read();
        inner.base.backend.exists(&normalized) || inner.base.backend.is_dir(&normalized)
    }

    pub fn has_overlay_for(&self, relative_path: &str) -> bool {
        let Some(normalized) = normalize_relative_path(relative_path) else {
            return false;
        };
        let inner = self.inner.read();
        highest_matching_overlay(&inner.overlays, &normalized).is_some()
    }

    /// Check whether a path exists as a file, directory, or synthesized
    /// overlay parent.  Used by `access()` to give a complete view.
    pub fn exists_or_is_dir(&self, relative_path: &str) -> bool {
        if relative_path.is_empty() {
            return true; // root always exists
        }
        let Some(normalized) = normalize_relative_path(relative_path) else {
            return false;
        };
        let inner = self.inner.read();

        // Check overlay parent prefix synthesis.
        for overlay in &inner.overlays {
            if overlay.prefix.starts_with(&normalized)
                && overlay.prefix.as_bytes().get(normalized.len()) == Some(&b'/')
            {
                return true;
            }
        }
        // Check overlays for files and virtual dirs.
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.exists(sub) || overlay.backend.is_dir(sub);
        }
        // Check base.
        inner.base.backend.exists(&normalized) || inner.base.backend.is_dir(&normalized)
    }

    /// Check whether a path is a regular file (not directory) in the mount chain.
    pub fn is_file(&self, relative_path: &str) -> bool {
        let Some(normalized) = normalize_relative_path(relative_path) else {
            return false;
        };
        let inner = self.inner.read();
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.is_file(sub);
        }
        inner.base.backend.is_file(&normalized)
    }

    pub fn get_file_info(&self, relative_path: &str, algorithm: &str) -> io::Result<(u64, String)> {
        let normalized = normalize_relative_path(relative_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path escapes mount root")
        })?;
        let inner = self.inner.read();
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.get_file_info(sub, algorithm);
        }
        inner.base.backend.get_file_info(&normalized, algorithm)
    }

    pub fn copy_to_path(&self, relative_path: &str, dest_path: &Path) -> io::Result<()> {
        let normalized = normalize_relative_path(relative_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path escapes mount root")
        })?;
        let inner = self.inner.read();
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.copy_to_path(sub, dest_path);
        }
        inner.base.backend.copy_to_path(&normalized, dest_path)
    }

    pub fn copy_to_writer(
        &self,
        relative_path: &str,
        writer: &mut dyn io::Write,
    ) -> io::Result<()> {
        let normalized = normalize_relative_path(relative_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path escapes mount root")
        })?;
        let inner = self.inner.read();
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.copy_to_writer(sub, writer);
        }
        inner.base.backend.copy_to_writer(&normalized, writer)
    }

    /// Read a file through the overlay chain.
    pub fn read(&self, relative_path: &str) -> io::Result<Vec<u8>> {
        let normalized = normalize_relative_path(relative_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path escapes mount root")
        })?;
        let inner = self.inner.read();
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.read(sub);
        }
        inner.base.backend.read(&normalized)
    }

    // -------------------------------------------------------------------
    // Sandbox validation
    // -------------------------------------------------------------------

    /// Check whether a *real* filesystem path falls within any mount root,
    /// **including** canonicalize + symlink verification.
    ///
    /// Used by the module loader to enforce the `/code` sandbox: after
    /// `FsModuleLoader` resolves a specifier to a `file://` URL, this method
    /// verifies the resulting path is inside a known mount directory and
    /// doesn't escape via symlinks.
    pub fn is_allowed_path(&self, path: &Path) -> bool {
        let normalized = super::normalize_path(path);
        let current_gen = self.generation.load(Ordering::Acquire);
        let inner = self.inner.read();

        // Check filesystem-backed mounts (with canonicalize + symlink
        // verification, memoised per generation like `resolve`).
        if let Some(root) = inner.base.backend.root_dir() {
            let norm_root = super::normalize_path(root);
            if normalized.starts_with(&norm_root) {
                return self.verify_contained_cached(path, root, current_gen);
            }
        }
        for overlay in &inner.overlays {
            if let Some(root) = overlay.backend.root_dir() {
                let norm_root = super::normalize_path(root);
                if normalized.starts_with(&norm_root) {
                    return self.verify_contained_cached(path, root, current_gen);
                }
            }
        }

        // For pack-backed mounts: the path won't match any root_dir() since
        // pack backends return None.  Instead, check if the path falls under
        // code_dir and resolves to an entry that exists in the mount table.
        // This is safe because pack entries were validated at ingest/open time.
        let norm_code_dir = super::normalize_path(&self.code_dir);
        if normalized.starts_with(&norm_code_dir) {
            if let Ok(relative) = normalized.strip_prefix(&norm_code_dir) {
                if let Some(relative_str) = relative.to_str() {
                    // Drop the inner read lock before calling resolve (which
                    // also takes a read lock).
                    drop(inner);
                    if let Some(resolved) = self.resolve(relative_str) {
                        // Pack-backed entries are allowed (no fs path to verify).
                        // Dir-backed entries were already checked above and
                        // didn't match, so this path is only reachable for
                        // pack-backed mounts.
                        return resolved.real_path.is_none();
                    }
                }
            }
        }

        false
    }

    /// Read a byte range with inflate-size protection.
    ///
    /// For compressed pack entries, rejects reads where the full inflate
    /// would exceed `max_inflate` bytes.  Stored entries and directory-backed
    /// entries are unaffected.
    pub fn read_range_limited(
        &self,
        relative_path: &str,
        position: u64,
        length: Option<u64>,
        max_inflate: u64,
    ) -> io::Result<Vec<u8>> {
        let normalized = normalize_relative_path(relative_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path escapes mount root")
        })?;
        let inner = self.inner.read();
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay
                .backend
                .read_range_limited(sub, position, length, max_inflate);
        }
        inner
            .base
            .backend
            .read_range_limited(&normalized, position, length, max_inflate)
    }

    /// Read a byte range from a `/code` entry.
    /// For stored pack entries, this seeks directly without full decompression.
    pub fn read_range(
        &self,
        relative_path: &str,
        position: u64,
        length: Option<u64>,
    ) -> io::Result<Vec<u8>> {
        let normalized = normalize_relative_path(relative_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path escapes mount root")
        })?;
        let inner = self.inner.read();
        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.read_range(sub, position, length);
        }
        inner.base.backend.read_range(&normalized, position, length)
    }

    /// Get the uncompressed size of a `/code` entry.
    pub fn entry_size(&self, relative_path: &str) -> Option<u64> {
        let normalized = normalize_relative_path(relative_path)?;
        let inner = self.inner.read();

        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            return overlay.backend.entry_size(sub);
        }
        inner.base.backend.entry_size(&normalized)
    }

    /// List immediate children under a directory prefix.
    ///
    /// Merges entries from overlays and base. Also synthesizes overlay mount
    /// points that are children of the queried directory.
    pub fn list_dir(&self, relative_dir: &str) -> Vec<String> {
        let normalized = normalize_relative_path(relative_dir).unwrap_or_default();
        let dir_prefix = if normalized.is_empty() {
            String::new()
        } else {
            format!("{}/", normalized)
        };

        let inner = self.inner.read();
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();

        // Synthesize overlay mount points visible as children.
        // e.g. listing "" with overlay prefix "subpackages/stage1" → "subpackages"
        // e.g. listing "subpackages" with overlay prefix "subpackages/stage1" → "stage1"
        for overlay in &inner.overlays {
            let tail = if dir_prefix.is_empty() {
                overlay.prefix.as_str()
            } else if let Some(t) = overlay.prefix.strip_prefix(&dir_prefix) {
                t
            } else {
                continue;
            };
            if let Some(first_component) = tail.split('/').next() {
                if !first_component.is_empty() && seen.insert(first_component.to_string()) {
                    result.push(first_component.to_string());
                }
            }
        }

        if let Some((overlay, sub)) = highest_matching_overlay(&inner.overlays, &normalized) {
            for name in overlay.backend.list_dir(sub) {
                if seen.insert(name.clone()) {
                    result.push(name);
                }
            }
            return result;
        }

        // Merge from base backend.
        for name in inner.base.backend.list_dir(&normalized) {
            if seen.insert(name.clone()) {
                result.push(name);
            }
        }
        result.sort();
        result
    }

    // -------------------------------------------------------------------
    // Mutation (mount / unmount / swap)
    // -------------------------------------------------------------------

    /// Add (or replace) a subpackage overlay mount.
    ///
    /// `prefix` is the subpackage root relative to `/code`
    /// (e.g. `"subpackages/stage1"`).
    pub fn mount_overlay(
        &self,
        name: String,
        prefix: String,
        backend: Arc<dyn MountBackend>,
    ) -> bool {
        let Some(normalized_prefix) = normalize_relative_path(&prefix) else {
            tracing::warn!("mount_overlay rejected invalid prefix: {}", prefix);
            return false;
        };

        let mut ancestor = normalized_prefix.as_str();
        while let Some((parent, _)) = ancestor.rsplit_once('/') {
            if self.is_file(parent) {
                tracing::warn!(
                    "mount_overlay rejected due to file/directory prefix conflict: {} blocks {}",
                    parent,
                    normalized_prefix,
                );
                return false;
            }
            ancestor = parent;
        }
        if self.is_file(&normalized_prefix) {
            tracing::warn!(
                "mount_overlay rejected because a file already exists at overlay prefix {}",
                normalized_prefix,
            );
            return false;
        }

        let new_gen = self.generation.fetch_add(1, Ordering::Release) + 1;
        self.invalidate_verify_cache();
        let mut inner = self.inner.write();
        inner.overlays.retain(|e| e.prefix != normalized_prefix);
        inner.overlays.push(MountEntry {
            name,
            prefix: normalized_prefix,
            backend,
            mounted_at: new_gen,
            uid: next_mount_uid(),
        });
        true
    }

    /// Remove a subpackage overlay by prefix.
    ///
    /// The prefix goes through the same normalisation `mount_overlay` applies
    /// before storing it. Comparing the caller's spelling against the stored one
    /// meant `"sub/stage1/"` or `"sub//stage1"` matched nothing and reported
    /// `false` while the overlay stayed mounted, holding its package open and
    /// leaving the verify cache valid -- one rule with two implementations, and
    /// the unmount side had the weaker one.
    pub fn unmount_overlay(&self, prefix: &str) -> bool {
        let Some(normalized_prefix) = normalize_relative_path(prefix) else {
            tracing::warn!("unmount_overlay rejected invalid prefix: {}", prefix);
            return false;
        };
        let mut inner = self.inner.write();
        let before = inner.overlays.len();
        inner.overlays.retain(|e| e.prefix != normalized_prefix);
        let removed = inner.overlays.len() < before;
        if removed {
            self.generation.fetch_add(1, Ordering::Release);
            self.invalidate_verify_cache();
        }
        removed
    }

    /// Atomically swap the base mount (e.g. hot-update of the main package).
    pub fn swap_base(&self, new_backend: Arc<dyn MountBackend>) {
        let new_gen = self.generation.fetch_add(1, Ordering::Release) + 1;
        self.invalidate_verify_cache();
        let mut inner = self.inner.write();
        inner.base = MountEntry {
            name: "base".to_string(),
            prefix: String::new(),
            backend: new_backend,
            mounted_at: new_gen,
            uid: next_mount_uid(),
        };
    }

    /// Get the base code directory path (if directory-backed).
    pub fn base_dir(&self) -> Option<PathBuf> {
        let inner = self.inner.read();
        inner.base.backend.root_dir().map(Path::to_path_buf)
    }

    /// Get the logical `/code` directory path.  Always available regardless
    /// of whether the base mount is directory-backed or pack-backed.
    pub fn code_dir(&self) -> PathBuf {
        let inner = self.inner.read();
        inner
            .base
            .backend
            .root_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.code_dir.clone())
    }
}

// ---------------------------------------------------------------------------
// StagingArea — atomic subpackage installation
// ---------------------------------------------------------------------------

/// What a completed package install produced.
///
/// The digest is here rather than derived later because the install already held
/// the whole package in memory; recovering it afterwards would mean reading the
/// file again.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub identity: super::package::PackageIdentity,
    pub digest: super::package::PackageDigest,
}

/// RAII helper for atomic subpackage installation.
///
/// # Flow
///
/// 1. `StagingArea::create(staging_root, name)` — creates a temp directory.
/// 2. Extract / download into [`dir()`](StagingArea::dir).
/// 3. [`validate()`](StagingArea::validate) — basic sanity check.
/// 4. [`install()`](StagingArea::install) — atomic rename + mount.
///
/// On drop, any un-installed staging directory is cleaned up.
pub struct StagingArea {
    staging_dir: PathBuf,
    name: String,
    /// Set to true after a successful rename so Drop doesn't double-clean.
    consumed: bool,
}

impl StagingArea {
    /// Create a new staging area under `staging_root`.
    ///
    /// The actual staging directory is
    /// `<staging_root>/.staging_<name>_<millis>`.
    pub fn create(staging_root: &Path, name: &str) -> io::Result<Self> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let staging_dir = staging_root.join(format!(".staging_{name}_{ts}"));
        std::fs::create_dir_all(&staging_dir)?;
        Ok(Self {
            staging_dir,
            name: name.to_string(),
            consumed: false,
        })
    }

    /// Path to the staging directory (target for extraction).
    pub fn dir(&self) -> &Path {
        &self.staging_dir
    }

    /// Basic validation: directory exists and is non-empty.
    pub fn validate(&self) -> io::Result<bool> {
        if !self.staging_dir.exists() {
            return Ok(false);
        }
        let mut entries = std::fs::read_dir(&self.staging_dir)?;
        Ok(entries.next().is_some())
    }

    /// Atomically install the staged content.
    ///
    /// 1. If `final_dir` exists, rename it to a trash path.
    /// 2. Rename staging → `final_dir` (atomic on same filesystem).
    /// 3. Mount the new directory in the mount table.
    /// 4. Clean up the trash.
    ///
    /// On failure the old directory is restored and the staging dir is
    /// cleaned up by [`Drop`].
    pub fn install(
        mut self,
        mount_table: &MountTable,
        final_dir: &Path,
        mount_prefix: &str,
    ) -> io::Result<()> {
        // Step 1: move old out of the way.
        let trash_dir = if final_dir.exists() {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let trash = final_dir.with_extension(format!("trash_{ts}"));
            std::fs::rename(final_dir, &trash)?;
            Some(trash)
        } else {
            if let Some(parent) = final_dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            None
        };

        // Step 2: atomic rename staging → final.
        match std::fs::rename(&self.staging_dir, final_dir) {
            Ok(()) => {
                self.consumed = true; // prevent Drop cleanup

                // Step 3: mount overlay.
                if !mount_table.mount_overlay(
                    format!("subpackage:{}", self.name),
                    mount_prefix.to_string(),
                    Arc::new(DirSource::new(final_dir.to_path_buf())),
                ) {
                    if let Some(ref trash) = trash_dir {
                        let _ = std::fs::remove_dir_all(final_dir);
                        let _ = std::fs::rename(trash, final_dir);
                    } else {
                        let _ = std::fs::remove_dir_all(final_dir);
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "mount overlay rejected due to prefix conflict",
                    ));
                }

                // Step 4: clean up trash (best-effort).
                if let Some(trash) = trash_dir {
                    let _ = std::fs::remove_dir_all(trash);
                }

                Ok(())
            }
            Err(rename_err) => {
                // Restore old directory.
                if let Some(ref trash) = trash_dir {
                    if let Err(restore_err) = std::fs::rename(trash, final_dir) {
                        tracing::error!(
                            "subpackage staging: rename staging->final failed: {}, \
                             restore trash->final also failed: {}. \
                             Old data at {:?}",
                            rename_err,
                            restore_err,
                            trash,
                        );
                    }
                }
                // staging_dir still exists → Drop will clean it up.
                Err(rename_err)
            }
        }
    }

    /// Install a `.mpkg` package file from the staging directory.
    ///
    /// 1. Validates the package in the staging dir.
    /// 2. Atomically renames the `.mpkg` file to `final_path`.
    /// 3. Opens the package and mounts it as a [`super::package::PackSource`].
    ///
    /// This is the pack-native install path — the runtime reads entries
    /// directly from the package file, never unpacking to a directory.
    pub fn install_package(
        self,
        mount_table: &MountTable,
        pkg_filename: &str,
        final_path: &Path,
        mount_prefix: &str,
        package_name: &str,
        package_version: &str,
    ) -> Result<InstalledPackage, io::Error> {
        self.install_package_signed(
            mount_table,
            pkg_filename,
            final_path,
            mount_prefix,
            package_name,
            package_version,
            None,
            None,
        )
    }

    /// Install a staged package, optionally verifying a host-supplied
    /// manifest + signature pair against the runtime's registered
    /// [`super::package::SignatureVerifier`]. Signatures are the trust
    /// root for subpackage installs: without a verifier registered or
    /// with a verifier present but validation failing, the package
    /// never replaces a live mount.
    #[allow(clippy::too_many_arguments)]
    pub fn install_package_signed(
        mut self,
        mount_table: &MountTable,
        pkg_filename: &str,
        final_path: &Path,
        mount_prefix: &str,
        package_name: &str,
        package_version: &str,
        manifest: Option<&[u8]>,
        signature: Option<&[u8]>,
    ) -> Result<InstalledPackage, io::Error> {
        let staged_pkg = self.staging_dir.join(pkg_filename);
        if !staged_pkg.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("staged package not found: {}", staged_pkg.display()),
            ));
        }

        // Full validation including payload checksums BEFORE moving.
        // This prevents a corrupted .mpkg from replacing a working version.
        super::package::validate_package(&staged_pkg, true)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        // Signature verification. We read the full package bytes
        // (bounded by the ingest-time ExtractBudget that originally
        // produced the .mpkg, so this is safe) and hand them to the
        // host-registered verifier. The runtime refuses to proceed
        // if verification fails; when no verifier has been registered
        // at all, `verify_package_signature` logs a one-shot warning
        // and accepts — that matches the pre-trust-chain behaviour so
        // rollout can be gradual.
        let pkg_bytes = std::fs::read(&staged_pkg)?;
        super::package::verify_package_signature(&pkg_bytes, manifest, signature)
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

        // Taken from the bytes the read above already produced, so the identity a
        // shared cache keys this package's entries on costs no second pass. It
        // describes the bytes that are about to be renamed into place, which the
        // game cannot substitute: the staging directory is outside every VFS
        // mapping. See `GamePaths::sandbox_cache_dir`.
        let digest = super::package::PackageDigest::of_bytes(&pkg_bytes);

        // Ensure parent directory of final_path exists.
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent)?;
            // Before the replacement, not after a successful one, and refused
            // rather than warned about: the window this closes is a process kill
            // between the rename and the caller's record write, and if the record
            // cannot be made to stop describing the outgoing file then the file
            // must not be replaced.
            PackageManifest::forget_digest_for(parent, final_path)?;
        }

        // Move old file out of the way if it exists.
        let trash_path = if final_path.exists() {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let trash = final_path.with_extension(format!("trash_{ts}"));
            std::fs::rename(final_path, &trash)?;
            Some(trash)
        } else {
            None
        };

        // Atomic rename staged package → final path.
        match std::fs::rename(&staged_pkg, final_path) {
            Ok(()) => {
                self.consumed = true;

                // Open and mount.  If open fails after rename succeeded,
                // restore the old package from trash to avoid leaving the
                // system in a half-failed state.
                let source = match super::package::PackSource::open_with_recorded_digest(
                    final_path,
                    package_name,
                    package_version,
                    &digest,
                ) {
                    Ok(s) => s,
                    Err(open_err) => {
                        tracing::error!(
                            "install_package: PackSource::open failed after rename: {}",
                            open_err,
                        );
                        // Restore: move new (broken) out, restore old from trash.
                        let broken = final_path.with_extension("broken");
                        let _ = std::fs::rename(final_path, &broken);
                        if let Some(ref trash) = trash_path {
                            let _ = std::fs::rename(trash, final_path);
                        } else {
                            let _ = std::fs::remove_file(&broken);
                            self.cleanup();
                        }
                        let _ = std::fs::remove_file(&broken);
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("PackSource::open failed: {open_err}"),
                        ));
                    }
                };

                let identity = source.identity().clone();

                if !mount_table.mount_overlay(
                    format!("subpackage:{}", self.name),
                    mount_prefix.to_string(),
                    Arc::new(source),
                ) {
                    if let Some(ref trash) = trash_path {
                        let _ = std::fs::remove_file(final_path);
                        let _ = std::fs::rename(trash, final_path);
                    } else {
                        let _ = std::fs::remove_file(final_path);
                        self.cleanup();
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "mount overlay rejected due to prefix conflict",
                    ));
                }

                // Clean up trash + staging remnants.
                if let Some(trash) = trash_path {
                    let _ = std::fs::remove_file(trash);
                }
                self.cleanup();

                Ok(InstalledPackage { identity, digest })
            }
            Err(rename_err) => {
                // Restore old file.
                if let Some(ref trash) = trash_path {
                    let _ = std::fs::rename(trash, final_path);
                }
                Err(rename_err)
            }
        }
    }

    /// Explicitly abort staging and clean up.
    pub fn abort(mut self) {
        self.cleanup();
        self.consumed = true;
    }

    fn cleanup(&self) {
        if self.staging_dir.exists() {
            let _ = std::fs::remove_dir_all(&self.staging_dir);
        }
    }
}

impl Drop for StagingArea {
    fn drop(&mut self) {
        if !self.consumed {
            self.cleanup();
        }
    }
}

// ---------------------------------------------------------------------------
// PackageManifest — per-game record of installed packages
// ---------------------------------------------------------------------------

/// Manifest entry for one installed package.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManifestEntry {
    /// Mount prefix (e.g. `"subpackages/stage1"`).
    pub prefix: String,
    /// Package version string.
    pub version: String,
    /// Hex [`super::package::PackageDigest`] of the installed package file.
    ///
    /// A cache identity, not a trust anchor: it lets a session start key
    /// pack-backed entries on the package's bytes without reading them again.
    /// Optional because a record written before the field existed has none, and
    /// because it is always recomputable from the file it describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
}

/// Per-game manifest mapping package name → install metadata.
///
/// Stored as `manifest.json` in the per-game package store directory.
/// Read on session start to restore overlay mounts.  Written on every
/// successful `install_package`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PackageManifest {
    pub packages: std::collections::HashMap<String, ManifestEntry>,
}

impl PackageManifest {
    /// Read manifest from `{pkg_store_dir}/manifest.json`.
    /// Returns default (empty) if file doesn't exist.
    pub fn load(pkg_store_dir: &Path) -> Self {
        let path = pkg_store_dir.join("manifest.json");
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Write manifest to `{pkg_store_dir}/manifest.json`.
    pub fn save(&self, pkg_store_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(pkg_store_dir)?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        std::fs::write(pkg_store_dir.join("manifest.json"), json)
    }

    /// Record a newly installed package.
    pub fn record(
        &mut self,
        name: String,
        prefix: String,
        version: String,
        digest: &super::package::PackageDigest,
    ) {
        self.packages.insert(
            name,
            ManifestEntry {
                prefix,
                version,
                content_digest: Some(digest.to_hex()),
            },
        );
    }

    /// Stop any record from describing the bytes at `pkg_path` before they are
    /// replaced.
    ///
    /// A digest describes a file, so it has to be dropped before that file
    /// changes. The intermediate state — a record with no digest — is always
    /// safe, because a restore digests the package it finds. The state this
    /// avoids is not: a replacement whose own record never lands (the manifest
    /// write is only a warning for `loadSubpackage`) would leave the previous
    /// digest naming the new bytes, and a Session still holding the previous
    /// package would then share a decoded entry with them.
    fn forget_digest_for(store_dir: &Path, pkg_path: &Path) -> io::Result<()> {
        let mut manifest = Self::load(store_dir);
        let mut changed = false;
        for (name, entry) in manifest.packages.iter_mut() {
            if entry.content_digest.is_some() && store_dir.join(format!("{name}.mpkg")) == pkg_path
            {
                entry.content_digest = None;
                changed = true;
            }
        }
        if changed {
            manifest.save(store_dir)
        } else {
            Ok(())
        }
    }
}

/// Per-game package store directory path.
///
/// `{game_cache_dir}/packages/`
/// where `game_cache_dir` = `{platform_cache_dir}/migo/games/{game_id}`
pub fn package_store_dir(game_cache_dir: &Path) -> PathBuf {
    game_cache_dir.join("packages")
}

/// Restore installed packages from the per-game manifest.
///
/// Called during session startup (`evaluate_module`) after creating the
/// MountTable.  Reads `manifest.json`, opens each `.mpkg`, and mounts
/// as overlay.  Silently skips packages that no longer exist on disk.
///
/// Each mount is keyed on the digest the install recorded, so a session start
/// costs the package index and nothing more. A record that predates the field is
/// digested here and the result written back, paying once per installed package
/// instead of once per session.
///
/// When `code_signing_enabled` is true, skips restoration entirely —
/// downloaded subpackages lack Ed25519 signatures and must not be loaded.
pub fn restore_installed_packages(
    mount_table: &MountTable,
    game_cache_dir: &Path,
    code_signing_enabled: bool,
) {
    if code_signing_enabled {
        tracing::info!("code signing enabled: skipping subpackage restore from package store");
        return;
    }
    let store = package_store_dir(game_cache_dir);
    let mut manifest = PackageManifest::load(&store);
    let mut digested_here: Vec<(String, super::package::PackageDigest)> = Vec::new();

    for (name, entry) in &manifest.packages {
        let pkg_path = store.join(format!("{name}.mpkg"));
        if !pkg_path.exists() {
            tracing::warn!(
                "manifest references missing package: {name} at {}",
                pkg_path.display()
            );
            continue;
        }
        let digest = match entry
            .content_digest
            .as_deref()
            .and_then(super::package::PackageDigest::parse_hex)
        {
            Some(recorded) => recorded,
            None => match super::package::PackageDigest::of_file(&pkg_path) {
                Ok(computed) => {
                    digested_here.push((name.clone(), computed));
                    computed
                }
                Err(e) => {
                    tracing::warn!("failed to digest package '{name}': {e}");
                    continue;
                }
            },
        };
        match super::package::PackSource::open_with_recorded_digest(
            &pkg_path,
            name,
            &entry.version,
            &digest,
        ) {
            Ok(source) => {
                if mount_table.mount_overlay(
                    format!("subpackage:{name}"),
                    entry.prefix.clone(),
                    Arc::new(source),
                ) {
                    tracing::info!("restored package '{name}' at prefix '{}'", entry.prefix);
                } else {
                    tracing::warn!(
                        "failed to mount restored package '{name}' at '{}'",
                        entry.prefix
                    );
                }
            }
            Err(e) => {
                tracing::warn!("failed to open package '{name}': {e}");
            }
        }
    }

    if !digested_here.is_empty() {
        for (name, digest) in digested_here {
            if let Some(entry) = manifest.packages.get_mut(&name) {
                entry.content_digest = Some(digest.to_hex());
            }
        }
        if let Err(e) = manifest.save(&store) {
            tracing::warn!("failed to persist package digests (recomputed next session): {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Normalize a relative path, resolving `.` and `..` components textually.
///
/// Returns `None` if the path attempts to escape above the root (`..` with
/// nothing to pop) or contains dangerous characters.
fn normalize_relative_path(path: &str) -> Option<String> {
    // Reject dangerous characters.
    for b in path.bytes() {
        if b < 0x20 || b == b'\\' {
            return None;
        }
    }

    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                if parts.pop().is_none() {
                    return None; // escape above root
                }
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// Given a normalized relative path and an overlay prefix, check if the
/// path falls under the overlay and return the sub-path.
///
/// Example: path=`"subpackages/stage1/game.js"`, prefix=`"subpackages/stage1"`
/// → returns `Some("game.js")`.
fn strip_overlay_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(path);
    }
    if path == prefix {
        return Some("");
    }
    path.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
}

fn highest_matching_overlay<'a>(
    overlays: &'a [MountEntry],
    normalized: &'a str,
) -> Option<(&'a MountEntry, &'a str)> {
    let mut best: Option<(&'a MountEntry, &'a str)> = None;
    let mut best_len = 0usize;
    for overlay in overlays.iter().rev() {
        if let Some(sub) = strip_overlay_prefix(normalized, &overlay.prefix) {
            let len = overlay.prefix.len();
            if best.is_none() || len > best_len {
                best = Some((overlay, sub));
                best_len = len;
            }
        }
    }
    best
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::package;
    use std::fs;

    // -----------------------------------------------------------------------
    // normalize_relative_path
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_simple() {
        assert_eq!(normalize_relative_path("a/b/c"), Some("a/b/c".into()));
    }

    #[test]
    fn normalize_dot_current() {
        assert_eq!(normalize_relative_path("a/./b"), Some("a/b".into()));
    }

    #[test]
    fn normalize_dot_dot_valid() {
        assert_eq!(normalize_relative_path("a/b/../c"), Some("a/c".into()));
    }

    #[test]
    fn normalize_dot_dot_escape() {
        assert_eq!(normalize_relative_path("a/../../c"), None);
        assert_eq!(normalize_relative_path("../x"), None);
    }

    #[test]
    fn normalize_empty() {
        assert_eq!(normalize_relative_path(""), Some(String::new()));
    }

    #[test]
    fn normalize_reject_backslash() {
        assert_eq!(normalize_relative_path("a\\b"), None);
    }

    #[test]
    fn normalize_reject_control() {
        assert_eq!(normalize_relative_path("a\x00b"), None);
        assert_eq!(normalize_relative_path("a\nb"), None);
    }

    // -----------------------------------------------------------------------
    // strip_overlay_prefix
    // -----------------------------------------------------------------------

    #[test]
    fn strip_overlay_match() {
        assert_eq!(
            strip_overlay_prefix("sub/stage1/game.js", "sub/stage1"),
            Some("game.js")
        );
    }

    #[test]
    fn strip_overlay_exact() {
        assert_eq!(strip_overlay_prefix("sub/stage1", "sub/stage1"), Some(""));
    }

    #[test]
    fn strip_overlay_no_match() {
        assert_eq!(strip_overlay_prefix("other/file.js", "sub/stage1"), None);
    }

    #[test]
    fn strip_overlay_empty_prefix() {
        assert_eq!(strip_overlay_prefix("any/path.js", ""), Some("any/path.js"));
    }

    // -----------------------------------------------------------------------
    // DirSource
    // -----------------------------------------------------------------------

    #[test]
    fn dir_source_real_path() {
        // Use a real temp dir with actual files so real_path() finds them.
        let dir = make_test_dir("dir_src_rp");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/utils.js"), "//").unwrap();
        let src = DirSource::new(dir.clone());
        assert_eq!(
            src.real_path("lib/utils.js"),
            Some(dir.join("lib/utils.js"))
        );
        // Non-existent file returns None.
        assert_eq!(src.real_path("nonexistent.js"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // MountTable — basic resolution
    // -----------------------------------------------------------------------

    fn make_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("migo_mount_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mount_resolve_base() {
        let base = make_test_dir("resolve_base");
        fs::write(base.join("main.js"), "// main").unwrap();

        let mt = MountTable::new(base.clone());
        let res = mt.resolve("main.js").unwrap();
        assert_eq!(res.real_path, Some(base.join("main.js")));
        assert_eq!(res.mount_name, "base");
        assert_eq!(res.mount_generation, 1);

        let _ = fs::remove_dir_all(&base);
    }

    /// What shipped with the game is the base package's, whatever is mounted
    /// over it; a path only an overlay has is not.
    #[test]
    fn in_base_is_the_base_package_under_any_overlay() {
        let base = make_test_dir("in_base");
        let overlay_dir = make_test_dir("in_base_pkg");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join("sub/a.js"), "// shipped").unwrap();
        fs::write(overlay_dir.join("a.js"), "// mounted").unwrap();

        let mt = MountTable::new(base.clone());
        mt.mount_overlay(
            "subpackage:sub".into(),
            "sub".into(),
            Arc::new(DirSource::new(overlay_dir.clone())),
        );
        mt.mount_overlay(
            "subpackage:late".into(),
            "late".into(),
            Arc::new(DirSource::new(overlay_dir.clone())),
        );

        assert!(mt.in_base("sub"));
        assert!(mt.in_base("sub/a.js"));
        assert!(mt.exists("late/a.js"));
        assert!(!mt.in_base("late"));
        assert!(!mt.in_base("../escape"));

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&overlay_dir);
    }

    #[test]
    fn mount_resolve_overlay_shadows_base() {
        let base = make_test_dir("overlay_shadow");
        let overlay_dir = make_test_dir("overlay_shadow_pkg");

        let sub_dir = base.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(sub_dir.join("a.js"), "// old").unwrap();

        fs::write(overlay_dir.join("a.js"), "// new").unwrap();

        let mt = MountTable::new(base.clone());
        mt.mount_overlay(
            "subpackage:sub".into(),
            "sub".into(),
            Arc::new(DirSource::new(overlay_dir.clone())),
        );

        let res = mt.resolve("sub/a.js").unwrap();
        assert_eq!(res.real_path, Some(overlay_dir.join("a.js")));
        assert_eq!(res.mount_name, "subpackage:sub");
        assert_eq!(res.mount_generation, 2); // gen bumped by mount_overlay

        // Non-overlaid real file in base still goes to base.
        fs::write(base.join("main.js"), "// base").unwrap();
        let res2 = mt.resolve("main.js").unwrap();
        assert_eq!(res2.real_path, Some(base.join("main.js")));

        // Non-existent file resolves to None.
        assert!(mt.resolve("nonexistent.js").is_none());

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&overlay_dir);
    }

    #[test]
    fn mount_generation_increments() {
        let base = make_test_dir("gen_inc");
        let mt = MountTable::new(base.clone());
        assert_eq!(mt.generation(), 1);

        mt.mount_overlay(
            "a".into(),
            "a".into(),
            Arc::new(DirSource::new(base.join("a"))),
        );
        assert_eq!(mt.generation(), 2);

        mt.unmount_overlay("a");
        assert_eq!(mt.generation(), 3);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn mount_is_allowed_path() {
        let base = make_test_dir("allowed");
        let overlay = make_test_dir("allowed_overlay");

        let mt = MountTable::new(base.clone());
        mt.mount_overlay(
            "sub".into(),
            "sub".into(),
            Arc::new(DirSource::new(overlay.clone())),
        );

        assert!(mt.is_allowed_path(&base.join("main.js")));
        assert!(mt.is_allowed_path(&overlay.join("game.js")));
        assert!(!mt.is_allowed_path(Path::new("/etc/passwd")));
        assert!(!mt.is_allowed_path(Path::new("/data/other/file")));

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&overlay);
    }

    #[test]
    fn mount_resolve_code_path() {
        let base = make_test_dir("code_path");
        // Create actual file so DirSource::real_path() finds it.
        fs::write(base.join("main.js"), "//").unwrap();
        let mt = MountTable::new(base.clone());

        let res = mt.resolve_code_path("/code/main.js").unwrap();
        assert_eq!(res.real_path, Some(base.join("main.js")));

        // Non-existent file in code dir should not resolve.
        assert!(mt.resolve_code_path("/code/nonexistent.js").is_none());
        assert!(mt.resolve_code_path("/user/data.json").is_none());
        assert!(mt.resolve_code_path("no_slash").is_none());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn mount_resolve_dotdot_escape_blocked() {
        let base = make_test_dir("dotdot");
        let mt = MountTable::new(base.clone());

        assert!(mt.resolve("../../etc/passwd").is_none());
        assert!(mt.resolve("a/../../b").is_none());

        let _ = fs::remove_dir_all(&base);
    }

    // -----------------------------------------------------------------------
    // StagingArea — atomic install
    // -----------------------------------------------------------------------

    #[test]
    fn staging_install_fresh() {
        let root = make_test_dir("staging_fresh");
        let final_dir = root.join("pkg");

        let staging = StagingArea::create(&root, "test").unwrap();
        fs::write(staging.dir().join("game.js"), "// game").unwrap();
        assert!(staging.validate().unwrap());

        let mt = MountTable::new(root.join("code"));
        staging.install(&mt, &final_dir, "pkg").unwrap();

        assert!(final_dir.join("game.js").exists());
        assert_eq!(mt.generation(), 2); // mount_overlay bumped it

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn staging_install_replaces_old() {
        let root = make_test_dir("staging_replace");
        let final_dir = root.join("pkg");

        // Old version.
        fs::create_dir_all(&final_dir).unwrap();
        fs::write(final_dir.join("old.js"), "// old").unwrap();

        // Stage new version.
        let staging = StagingArea::create(&root, "test").unwrap();
        fs::write(staging.dir().join("new.js"), "// new").unwrap();

        let mt = MountTable::new(root.join("code"));
        staging.install(&mt, &final_dir, "pkg").unwrap();

        assert!(final_dir.join("new.js").exists());
        assert!(!final_dir.join("old.js").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn staging_abort_cleans_up() {
        let root = make_test_dir("staging_abort");
        let staging = StagingArea::create(&root, "test").unwrap();
        let dir = staging.dir().to_path_buf();
        fs::write(dir.join("temp.js"), "tmp").unwrap();
        assert!(dir.exists());

        staging.abort();
        assert!(!dir.exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn staging_drop_cleans_up() {
        let root = make_test_dir("staging_drop");
        let dir;
        {
            let staging = StagingArea::create(&root, "test").unwrap();
            dir = staging.dir().to_path_buf();
            fs::write(dir.join("temp.js"), "tmp").unwrap();
            // staging dropped here without install/abort
        }
        assert!(!dir.exists());

        let _ = fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // Module sandbox simulation (is_allowed_path)
    // -----------------------------------------------------------------------

    #[test]
    fn sandbox_import_within_code_allowed() {
        let base = make_test_dir("sandbox_ok");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join("main.js"), "//").unwrap();
        fs::write(base.join("sub/a.js"), "//").unwrap();

        let mt = MountTable::new(base.clone());

        // /code/main.js import ./sub/a → resolves to base/sub/a.js
        assert!(mt.is_allowed_path(&base.join("sub/a.js")));
        // /code/main.js itself
        assert!(mt.is_allowed_path(&base.join("main.js")));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sandbox_import_outside_blocked() {
        let base = make_test_dir("sandbox_outside");
        let mt = MountTable::new(base.clone());

        // import ../outside → resolves to parent dir
        let escaped = base.parent().unwrap().join("outside.js");
        assert!(!mt.is_allowed_path(&escaped));

        // absolute host path
        assert!(!mt.is_allowed_path(Path::new("/etc/passwd")));
        assert!(!mt.is_allowed_path(Path::new("/data/other/app/secrets")));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sandbox_import_normalized_escape_blocked() {
        let base = make_test_dir("sandbox_norm_escape");
        let mt = MountTable::new(base.clone());

        // Crafted path that after normalization escapes
        let crafted = base.join("a/../../etc/passwd");
        assert!(!mt.is_allowed_path(&crafted));

        let _ = fs::remove_dir_all(&base);
    }

    // -----------------------------------------------------------------------
    // Cache identity verification
    // -----------------------------------------------------------------------

    #[test]
    fn cache_identity_changes_on_mount_update() {
        let base = make_test_dir("cache_id");
        let overlay_v1 = make_test_dir("cache_id_v1");
        let overlay_v2 = make_test_dir("cache_id_v2");

        fs::write(overlay_v1.join("img.png"), "v1").unwrap();
        fs::write(overlay_v2.join("img.png"), "v2").unwrap();

        let mt = MountTable::new(base.clone());

        // Load from v1
        mt.mount_overlay(
            "pkg".into(),
            "pkg".into(),
            Arc::new(DirSource::new(overlay_v1.clone())),
        );
        let res1 = mt.resolve("pkg/img.png").unwrap();
        let gen1 = res1.mount_generation;

        // Upgrade to v2
        mt.mount_overlay(
            "pkg".into(),
            "pkg".into(),
            Arc::new(DirSource::new(overlay_v2.clone())),
        );
        let res2 = mt.resolve("pkg/img.png").unwrap();
        let gen2 = res2.mount_generation;

        // Same path, different generation → different cache keys
        assert!(gen2 > gen1, "generation must increase: {} > {}", gen2, gen1);

        // Construct cache keys as the image loader would
        let key1 = format!("{:?}:g{}", res1.real_path, gen1);
        let key2 = format!("{:?}:g{}", res2.real_path, gen2);
        assert_ne!(key1, key2, "cache keys must differ after mount update");

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&overlay_v1);
        let _ = fs::remove_dir_all(&overlay_v2);
    }

    // -----------------------------------------------------------------------
    // Install record — the digest a shared cache keys pack-backed entries on
    // -----------------------------------------------------------------------

    fn write_package(path: &Path, entry: &str, content: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let file = fs::File::create(path).unwrap();
        let mut writer = package::PackageWriter::new(io::BufWriter::new(file)).unwrap();
        writer.add_entry(entry, content).unwrap();
        writer.finish("stage1", "1.0").unwrap();
    }

    /// Install a package built in a staging area, returning the install result.
    fn install_staged_package(
        mount_table: &MountTable,
        cache_root: &Path,
        content: &[u8],
    ) -> InstalledPackage {
        let staging = StagingArea::create(cache_root, "stage1").unwrap();
        write_package(&staging.dir().join("stage1.mpkg"), "main.js", content);
        staging
            .install_package(
                mount_table,
                "stage1.mpkg",
                &package_store_dir(cache_root).join("stage1.mpkg"),
                "subpackages/stage1",
                "stage1",
                "1.0",
            )
            .unwrap()
    }

    fn mounted_identity(mount_table: &MountTable) -> u64 {
        mount_table
            .resolve_code_path("/code/subpackages/stage1/main.js")
            .expect("subpackage entry must resolve")
            .source_identity
    }

    #[test]
    fn install_reports_the_digest_of_the_bytes_it_installed() {
        let root = make_test_dir("install_digest");
        let cache_root = root.join("cache");
        let mt = MountTable::new(root.join("code"));

        let installed = install_staged_package(&mt, &cache_root, b"// stage one");

        let final_path = package_store_dir(&cache_root).join("stage1.mpkg");
        assert_eq!(
            installed.digest,
            package::PackageDigest::of_file(&final_path).unwrap(),
        );
        assert_eq!(mounted_identity(&mt), installed.digest.cache_identity());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn replacing_a_package_drops_the_digest_recorded_for_the_old_one() {
        let root = make_test_dir("replace_invalidates_digest");
        let cache_root = root.join("cache");
        let store = package_store_dir(&cache_root);
        let mt = MountTable::new(root.join("code"));

        let first = install_staged_package(&mt, &cache_root, b"// one");
        let mut manifest = PackageManifest::load(&store);
        manifest.record(
            "stage1".into(),
            "subpackages/stage1".into(),
            "1.0".into(),
            &first.digest,
        );
        manifest.save(&store).unwrap();

        // A replacement whose record never lands: the manifest write is a warning
        // for `loadSubpackage`, so a full disk leaves exactly this state. The old
        // record must not survive it, or the next session mounts these bytes under
        // the previous package's identity and shares a cache entry with whoever
        // still has that package.
        let second = install_staged_package(&mt, &cache_root, b"// two, larger");
        assert_ne!(first.digest, second.digest);

        let next_session = MountTable::new(root.join("code"));
        restore_installed_packages(&next_session, &cache_root, false);

        assert_eq!(
            mounted_identity(&next_session),
            second.digest.cache_identity()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_recorded_digest_is_the_identity_without_re_reading_the_package() {
        let root = make_test_dir("restore_recorded_digest");
        let cache_root = root.join("cache");
        let store = package_store_dir(&cache_root);

        install_staged_package(&MountTable::new(root.join("code")), &cache_root, b"// one");

        // Only the runtime can write this record -- the store is outside every VFS
        // mapping, which `no_vfs_mapping_contains_the_runtime_cache_root` pins. So
        // restore may take the recorded digest as the identity, and that is what
        // keeps a session start free of a pass over every installed package.
        let recorded = package::PackageDigest::of_bytes(b"recorded by the runtime");
        let mut manifest = PackageManifest::load(&store);
        manifest.record(
            "stage1".into(),
            "subpackages/stage1".into(),
            "1.0".into(),
            &recorded,
        );
        manifest.save(&store).unwrap();

        let next_session = MountTable::new(root.join("code"));
        restore_installed_packages(&next_session, &cache_root, false);

        assert_eq!(mounted_identity(&next_session), recorded.cache_identity());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_record_without_a_digest_mounts_the_bytes_own_identity_and_persists_it() {
        let root = make_test_dir("restore_legacy_record");
        let cache_root = root.join("cache");
        let store = package_store_dir(&cache_root);

        install_staged_package(&MountTable::new(root.join("code")), &cache_root, b"// one");

        // The shape a record written before the field existed has.
        fs::write(
            store.join("manifest.json"),
            r#"{"packages":{"stage1":{"prefix":"subpackages/stage1","version":"1.0"}}}"#,
        )
        .unwrap();

        let next_session = MountTable::new(root.join("code"));
        restore_installed_packages(&next_session, &cache_root, false);

        let from_bytes = package::PackageDigest::of_file(&store.join("stage1.mpkg")).unwrap();
        assert_eq!(mounted_identity(&next_session), from_bytes.cache_identity());
        assert_eq!(
            PackageManifest::load(&store).packages["stage1"]
                .content_digest
                .as_deref(),
            Some(from_bytes.to_hex().as_str()),
            "the digest has to be persisted, or every later session pays for it again",
        );

        let _ = fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // Staging failure doesn't pollute
    // -----------------------------------------------------------------------

    #[test]
    fn staging_failure_preserves_old_mount() {
        let root = make_test_dir("staging_fail");
        let code_dir = root.join("code");
        let pkg_dir = code_dir.join("pkg");

        // Set up existing package in base
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("game.js"), "// v1").unwrap();

        let mt = MountTable::new(code_dir.clone());

        // Stage a new version
        let staging = StagingArea::create(&root, "test").unwrap();
        fs::write(staging.dir().join("game.js"), "// v2").unwrap();

        // Abort instead of install — simulates failure
        staging.abort();

        // Old version still readable through mount table
        assert!(mt.exists("pkg/game.js"));
        let data = mt.read("pkg/game.js").unwrap();
        assert_eq!(String::from_utf8_lossy(&data), "// v1");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn staging_not_visible_before_install() {
        let root = make_test_dir("staging_invisible");
        let code_dir = root.join("code");
        fs::create_dir_all(&code_dir).unwrap();

        let mt = MountTable::new(code_dir.clone());

        // Create staging area (NOT under code_dir)
        let staging = StagingArea::create(&root, "newpkg").unwrap();
        fs::write(staging.dir().join("secret.js"), "// secret").unwrap();

        // The staging dir is NOT under code_dir, so mount table can't see it
        assert!(!mt.exists("secret.js"));
        assert!(!mt.is_allowed_path(&staging.dir().join("secret.js")));

        staging.abort();
        let _ = fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // Mount swap_base
    // -----------------------------------------------------------------------

    #[test]
    fn swap_base_changes_resolution() {
        let v1 = make_test_dir("swap_v1");
        let v2 = make_test_dir("swap_v2");

        fs::write(v1.join("main.js"), "v1").unwrap();
        fs::write(v2.join("main.js"), "v2").unwrap();

        let mt = MountTable::new(v1.clone());
        let res1 = mt.resolve("main.js").unwrap();
        assert_eq!(res1.real_path, Some(v1.join("main.js")));

        mt.swap_base(Arc::new(DirSource::new(v2.clone())));
        let res2 = mt.resolve("main.js").unwrap();
        assert_eq!(res2.real_path, Some(v2.join("main.js")));
        assert!(res2.mount_generation > res1.mount_generation);

        let _ = fs::remove_dir_all(&v1);
        let _ = fs::remove_dir_all(&v2);
    }

    // -----------------------------------------------------------------------
    // Verified-path cache (P0 perf): correctness must be unchanged
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_repeat_hits_verify_cache_and_stays_correct() {
        // Second resolve of the same path takes the verify-cache fast path
        // (skips canonicalize + symlink chain walk). Result must be identical.
        let base = make_test_dir("verify_cache_hit");
        fs::create_dir_all(base.join("a/b")).unwrap();
        fs::write(base.join("a/b/c.js"), "//").unwrap();

        let mt = MountTable::new(base.clone());
        let r1 = mt.resolve("a/b/c.js").unwrap();
        let r2 = mt.resolve("a/b/c.js").unwrap();
        assert_eq!(r1.real_path, r2.real_path);
        assert_eq!(r1.mount_generation, r2.mount_generation);
        assert_eq!(r2.real_path, Some(base.join("a/b/c.js")));

        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn verify_cache_never_caches_a_rejected_symlink() {
        // A rejected (escaping) path must never be inserted into the cache,
        // so a repeat resolve stays rejected rather than being served a
        // stale "valid" entry.
        let base = make_test_dir("verify_cache_reject");
        let outside = make_test_dir("verify_cache_outside");
        fs::write(outside.join("secret.txt"), "leaked").unwrap();
        std::os::unix::fs::symlink(&outside, base.join("evil")).unwrap();

        let mt = MountTable::new(base.clone());
        assert!(mt.resolve("evil/secret.txt").is_none());
        assert!(mt.resolve("evil/secret.txt").is_none());

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn swap_base_invalidates_verify_cache() {
        // Priming the cache under generation N must not leak a stale real
        // path after swap_base bumps the generation and clears the cache.
        let v1 = make_test_dir("verify_cache_swap_v1");
        let v2 = make_test_dir("verify_cache_swap_v2");
        fs::write(v1.join("main.js"), "v1").unwrap();
        fs::write(v2.join("main.js"), "v2").unwrap();

        let mt = MountTable::new(v1.clone());
        let r1 = mt.resolve("main.js").unwrap();
        assert_eq!(r1.real_path, Some(v1.join("main.js")));

        mt.swap_base(Arc::new(DirSource::new(v2.clone())));
        let r2 = mt.resolve("main.js").unwrap();
        assert_eq!(r2.real_path, Some(v2.join("main.js")));

        let _ = fs::remove_dir_all(&v1);
        let _ = fs::remove_dir_all(&v2);
    }

    // -----------------------------------------------------------------------
    // Symlink escape via MountTable (issue #1 regression test)
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn mount_resolve_rejects_symlink_escape() {
        let base = make_test_dir("mt_symlink_escape");
        let outside = make_test_dir("mt_symlink_outside");
        fs::write(outside.join("secret.txt"), "leaked").unwrap();

        // Plant a symlink inside the code dir pointing outside.
        std::os::unix::fs::symlink(&outside, base.join("evil")).unwrap();

        let mt = MountTable::new(base.clone());

        // resolve() must reject the symlink.
        assert!(
            mt.resolve("evil/secret.txt").is_none(),
            "symlink escape through resolve() must be blocked"
        );

        // is_allowed_path() must also reject.
        assert!(
            !mt.is_allowed_path(&base.join("evil/secret.txt")),
            "symlink escape through is_allowed_path() must be blocked"
        );

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn mount_resolve_rejects_symlink_within_code() {
        let base = make_test_dir("mt_symlink_internal");
        fs::create_dir_all(base.join("real")).unwrap();
        fs::write(base.join("real/ok.js"), "//").unwrap();

        // Symlink within code dir — policy denies all symlinks in /code.
        std::os::unix::fs::symlink(base.join("real/ok.js"), base.join("link.js")).unwrap();

        let mt = MountTable::new(base.clone());
        assert!(
            mt.resolve("link.js").is_none(),
            "symlink within code dir must be denied (policy: deny_symlinks_in_code_dir)"
        );

        // Direct real file is fine.
        assert!(mt.resolve("real/ok.js").is_some());

        let _ = fs::remove_dir_all(&base);
    }

    // -----------------------------------------------------------------------
    // Overlay priority (last mounted wins)
    // -----------------------------------------------------------------------

    #[test]
    fn overlay_last_wins() {
        let base = make_test_dir("overlay_prio");
        let ov1 = make_test_dir("overlay_prio_1");
        let ov2 = make_test_dir("overlay_prio_2");

        fs::write(ov1.join("x.js"), "ov1").unwrap();
        fs::write(ov2.join("x.js"), "ov2").unwrap();

        let mt = MountTable::new(base.clone());
        mt.mount_overlay(
            "a".into(),
            "pkg".into(),
            Arc::new(DirSource::new(ov1.clone())),
        );
        mt.mount_overlay(
            "b".into(),
            "pkg".into(),
            Arc::new(DirSource::new(ov2.clone())),
        );

        // "b" was mounted last for the same prefix, so it replaces "a"
        let res = mt.resolve("pkg/x.js").unwrap();
        assert_eq!(res.real_path, Some(ov2.join("x.js")));

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&ov1);
        let _ = fs::remove_dir_all(&ov2);
    }

    // -----------------------------------------------------------------------
    // Phase 2 correctness: overlay parent dir vs non-existent base path
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_overlay_parent_not_shadowed_by_nonexistent_base() {
        // Issue 1: DirSource::real_path() always returns Some even if the path
        // doesn't exist. This causes resolve() to return a filesystem path for
        // "subpackages" even though it doesn't exist in base, preventing the
        // overlay parent synthesis from running.
        let base = make_test_dir("resolve_nobase");
        // base is EMPTY — no "subpackages" dir or file exists.

        let overlay_dir = make_test_dir("resolve_nobase_overlay");
        fs::write(overlay_dir.join("game.js"), "// game").unwrap();

        let mt = MountTable::new(base.clone());
        mt.mount_overlay(
            "sub".into(),
            "subpackages/stage1".into(),
            Arc::new(DirSource::new(overlay_dir.clone())),
        );

        // "subpackages" doesn't exist in base, but IS a parent of an
        // overlay prefix. It must resolve as a virtual directory.
        let res = mt.resolve("subpackages").unwrap();
        assert!(
            res.real_path.is_none(),
            "non-existent base path must not shadow overlay parent: got real_path={:?}",
            res.real_path,
        );

        // list_dir must see the overlay mount point.
        let root_listing = mt.list_dir("");
        assert!(
            root_listing.contains(&"subpackages".to_string()),
            "root listing must contain 'subpackages': {:?}",
            root_listing
        );

        let sub_listing = mt.list_dir("subpackages");
        assert!(
            sub_listing.contains(&"stage1".to_string()),
            "subpackages listing must contain 'stage1': {:?}",
            sub_listing
        );

        // exists_or_is_dir must be true for both.
        assert!(mt.exists_or_is_dir("subpackages"));
        assert!(mt.exists_or_is_dir("subpackages/stage1"));

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&overlay_dir);
    }

    #[test]
    fn resolve_real_base_file_beats_overlay_parent() {
        // Issue 1 counterpart: if base ACTUALLY has a file called "subpackages",
        // it must take priority over the overlay parent synthesis.
        let base = make_test_dir("resolve_realbase");
        fs::write(base.join("subpackages"), "i am a real file").unwrap();

        let overlay_dir = make_test_dir("resolve_realbase_overlay");
        fs::write(overlay_dir.join("game.js"), "//").unwrap();

        let mt = MountTable::new(base.clone());
        mt.mount_overlay(
            "sub".into(),
            "subpackages/stage1".into(),
            Arc::new(DirSource::new(overlay_dir.clone())),
        );

        // "subpackages" exists as a real file in base — must get real_path.
        let res = mt.resolve("subpackages").unwrap();
        assert!(
            res.real_path.is_some(),
            "real base file must take priority over overlay parent synthesis",
        );

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&overlay_dir);
    }

    // -----------------------------------------------------------------------
    // Issue 1: mixed dir/pack recursive stat must not treat dirs as files
    // -----------------------------------------------------------------------

    #[test]
    fn mixed_mount_recursive_stat_files_only() {
        // DirSource::entry_size() returns Some for directories (metadata.len()),
        // so code using entry_size().is_some() to mean "is file" is wrong.
        // MountTable must have a reliable is_file check.
        let base = make_test_dir("mixed_stat");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join("top.txt"), "top").unwrap();
        fs::write(base.join("sub/inner.txt"), "inner").unwrap();

        let mt = MountTable::new(base.clone());

        // entry_size on a directory should NOT count as "is file".
        assert!(mt.is_file("top.txt"), "top.txt must be a file");
        assert!(!mt.is_file("sub"), "sub must NOT be a file");
        assert!(mt.is_file("sub/inner.txt"), "sub/inner.txt must be a file");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn overlay_subtree_hides_base_entries() {
        let base = make_test_dir("overlay_shadow_base");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join("sub/old.txt"), "old").unwrap();

        let overlay = make_test_dir("overlay_shadow_overlay");
        fs::write(overlay.join("new.txt"), "new").unwrap();

        let mt = MountTable::new(base.clone());
        mt.mount_overlay(
            "sub".into(),
            "sub".into(),
            Arc::new(DirSource::new(overlay.clone())),
        );

        assert!(
            !mt.exists("sub/old.txt"),
            "overlay subtree must hide base file"
        );
        assert!(
            mt.read("sub/old.txt").is_err(),
            "hidden base file must not be readable"
        );

        let listing = mt.list_dir("sub");
        assert!(listing.contains(&"new.txt".to_string()));
        assert!(!listing.contains(&"old.txt".to_string()));

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&overlay);
    }

    #[test]
    fn more_specific_overlay_beats_broader_overlay() {
        let base = make_test_dir("overlay_specific_base");
        let broad = make_test_dir("overlay_specific_broad");
        let nested = make_test_dir("overlay_specific_nested");
        fs::create_dir_all(broad.join("nested")).unwrap();
        fs::write(broad.join("nested/value.txt"), "broad").unwrap();
        fs::write(nested.join("value.txt"), "nested").unwrap();

        let mt = MountTable::new(base.clone());
        mt.mount_overlay(
            "nested".into(),
            "sub/nested".into(),
            Arc::new(DirSource::new(nested.clone())),
        );
        mt.mount_overlay(
            "broad".into(),
            "sub".into(),
            Arc::new(DirSource::new(broad.clone())),
        );

        let data = mt.read("sub/nested/value.txt").unwrap();
        assert_eq!(data, b"nested");

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&broad);
        let _ = fs::remove_dir_all(&nested);
    }
}
