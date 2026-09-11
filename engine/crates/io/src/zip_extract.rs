//! Native zip extraction using the `zip` crate.
//!
//! This provides high-performance, cross-platform zip extraction with:
//! - Path traversal protection
//! - Progress callbacks
//! - Streaming extraction (low memory usage)
//! - Resource budget (entry count, total uncompressed bytes, per-entry size,
//!   compression ratio) to defend against zip bombs

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(not(unix))]
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{debug, error, trace};
// Only the `#[cfg(unix)]` block that replays a zip entry's unix mode logs at
// warn level; a zip entry's mode has no counterpart on other targets, so the
// import has to carry the same gate or it is unused there.
#[cfg(unix)]
use tracing::warn;
use zip::ZipArchive;

use crate::{
    pools::PoolError,
    scheduler::IoScheduler,
    task::{BackendKind, IoRequest, PriorityClass},
};

/// Resource limits applied during zip extraction / package ingest.
///
/// Path safety alone is not enough: an attacker can still craft a small
/// archive that unpacks into gigabytes of data (zip bomb), creates
/// hundreds of thousands of tiny entries (inode bomb), or repeatedly
/// inflates the same chunk (high-ratio bomb). Every extraction path
/// must check every axis **twice**: once cheaply against advertised
/// header sizes, and once against the actually-written bytes while
/// streaming.
#[derive(Debug, Clone, Copy)]
pub struct ExtractBudget {
    /// Maximum number of entries (files + directories).
    pub max_entries: usize,
    /// Maximum sum of uncompressed bytes across all entries.
    pub max_total_uncompressed: u64,
    /// Maximum uncompressed bytes for a single entry.
    pub max_entry_uncompressed: u64,
    /// Maximum `uncompressed / compressed` ratio per entry. A value
    /// of `0` disables the check (useful for `Stored` entries).
    pub max_compression_ratio: u64,
}

impl ExtractBudget {
    /// Default budget used by the engine:
    ///
    /// - 20 000 entries: covers realistic subpackages, rejects inode bombs.
    /// - 256 MiB total: covers realistic game assets without letting one
    ///   archive swallow user data.
    /// - 100 MiB per entry: matches `MAX_READ_LENGTH` elsewhere in the
    ///   engine so no single entry can exceed what the rest of the IO
    ///   stack is willing to read anyway.
    /// - 200× compression ratio: deflate on realistic game assets is
    ///   typically 3–10×; 200× is an order-of-magnitude safety net that
    ///   still rejects adversarial inflate bombs.
    pub const DEFAULT: Self = Self {
        max_entries: 20_000,
        max_total_uncompressed: 256 * 1024 * 1024,
        max_entry_uncompressed: 100 * 1024 * 1024,
        max_compression_ratio: 200,
    };
}

impl Default for ExtractBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn unzip_request_for(zip_path: &Path) -> IoRequest {
    let compressed_bytes = std::fs::metadata(zip_path)
        .map(|meta| meta.len() as usize)
        .unwrap_or(0);
    IoRequest::Unzip {
        backend: BackendKind::Archive,
        priority: PriorityClass::Background,
        compressed_bytes,
    }
}

/// Error type for zip operations
#[derive(Debug)]
pub enum ZipError {
    /// The zip file was not found
    NotFound(String),
    /// IO error during extraction
    Io(io::Error),
    /// Invalid zip archive
    InvalidArchive(String),
    /// Path traversal attempt detected (security)
    PathTraversal(String),
    /// Failed to create directory
    CreateDirFailed(String),
    /// Extraction exceeded the configured budget (zip bomb defense)
    BudgetExceeded(String),
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZipError::NotFound(path) => write!(f, "Zip file not found: {}", path),
            ZipError::Io(e) => write!(f, "IO error: {}", e),
            ZipError::InvalidArchive(msg) => write!(f, "Invalid archive: {}", msg),
            ZipError::PathTraversal(path) => write!(f, "Path traversal detected: {}", path),
            ZipError::CreateDirFailed(path) => write!(f, "Failed to create directory: {}", path),
            ZipError::BudgetExceeded(msg) => write!(f, "Extraction budget exceeded: {}", msg),
        }
    }
}

impl std::error::Error for ZipError {}

impl From<io::Error> for ZipError {
    fn from(e: io::Error) -> Self {
        ZipError::Io(e)
    }
}

impl From<zip::result::ZipError> for ZipError {
    fn from(e: zip::result::ZipError) -> Self {
        ZipError::InvalidArchive(e.to_string())
    }
}

impl From<PoolError> for ZipError {
    fn from(err: PoolError) -> Self {
        match err {
            PoolError::Closed => ZipError::Io(io::Error::other("IO worker pool closed")),
            PoolError::ByteLimitExceeded {
                requested,
                available,
            } => ZipError::Io(io::Error::other(format!(
                "IO pending-byte budget exhausted: requested {requested} bytes, {available} available"
            ))),
        }
    }
}

/// Progress callback type
pub type ProgressCallback = Box<dyn Fn(f32, usize, usize) + Send>;

/// Resolving an entry's path exactly once, in the kernel.
///
/// The path-based checks elsewhere in this file answer "does this name point
/// inside `dest`?" — but they answer it about a *name*, and then a separate
/// syscall resolves that name again. Between the two, any component can become
/// a symlink. No amount of re-checking closes that; the resolution has to
/// happen once.
///
/// So every component below `dest` is reached with `openat` from a held
/// directory descriptor, under `O_NOFOLLOW`. A component that is a symlink
/// fails with `ELOOP` at the moment of use rather than passing a check and
/// being swapped afterwards. `dest` itself is opened once, up front.
#[cfg(unix)]
mod dirfd {
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};

    /// Directories are opened read-only; we only ever use them as a resolution
    /// base for `openat`/`mkdirat`.
    const DIR_FLAGS: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    fn component_cstring(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "zip entry path component contains a NUL byte",
            )
        })
    }

    /// Open `path` as the resolution base. Unlike every call below this one
    /// takes a full path, because it is the trusted root the caller already
    /// canonicalised.
    pub(super) fn open_root(path: &Path) -> io::Result<OwnedFd> {
        let c_path = component_cstring(path.as_os_str())?;
        // SAFETY: `c_path` is NUL-terminated and outlives the call; the flags
        // are valid for a directory open.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor, checked non-negative, owned by us.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Create `name` under `parent` if absent, then open it. An existing
    /// *symlink* named `name` makes the open fail rather than be followed.
    fn open_or_create_child_dir(parent: &OwnedFd, name: &CString) -> io::Result<OwnedFd> {
        // SAFETY: `name` is NUL-terminated and outlives the call; `parent` is
        // a live directory descriptor.
        let made = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
        if made < 0 {
            let err = io::Error::last_os_error();
            // Already there is the common case — every entry after the first
            // in a given directory. Anything else is real.
            if err.kind() != io::ErrorKind::AlreadyExists {
                return Err(err);
            }
        }

        // SAFETY: same contract as above; `DIR_FLAGS` carries `O_NOFOLLOW`, so
        // a symlink here is refused instead of traversed.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), DIR_FLAGS) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fresh descriptor, checked non-negative, owned by us.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Reject anything that is not a plain name. `.`, `..`, a root, or a
    /// prefix reaching this far would mean the caller's normalisation let
    /// something through, and resolving it here would undo the containment the
    /// descriptor walk exists to provide.
    fn plain_components(relative: &Path) -> io::Result<Vec<CString>> {
        relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => component_cstring(name),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "zip entry path component is not a plain name",
                )),
            })
            .collect()
    }

    /// Walk `relative` from `root`, creating each directory along the way, and
    /// return a descriptor for the last one.
    pub(super) fn create_dir_chain(root: &OwnedFd, relative: &Path) -> io::Result<OwnedFd> {
        let mut current = root.try_clone()?;
        for name in plain_components(relative)? {
            current = open_or_create_child_dir(&current, &name)?;
        }
        Ok(current)
    }

    /// Create (or truncate) `name` directly under `dir`.
    ///
    /// `O_NOFOLLOW` without `O_EXCL`: overwriting a plain file left by an
    /// earlier extraction is expected, following a symlink is not.
    pub(super) fn create_file(dir: &OwnedFd, name: &OsStr) -> io::Result<File> {
        let c_name = component_cstring(name)?;
        // SAFETY: `c_name` is NUL-terminated and outlives the call; `dir` is a
        // live directory descriptor; `O_CREAT` is paired with a mode argument.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fresh descriptor, checked non-negative, owned by us.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

/// The destination tree an extraction writes into.
///
/// One abstraction with two implementations so the extraction loop does not
/// carry platform branches. On Unix it holds descriptors and resolves every
/// component through `openat`; elsewhere it falls back to path operations,
/// which is what the whole file used to do.
struct OutputTree {
    #[cfg(unix)]
    root: std::os::fd::OwnedFd,
    #[cfg(not(unix))]
    root: PathBuf,
    /// Directories this extraction has already made, keyed by their path
    /// relative to the root. Archives are many files under few directories, so
    /// without this every entry re-walks (and on Unix re-opens) its ancestry.
    #[cfg(unix)]
    dirs: HashMap<PathBuf, std::os::fd::OwnedFd>,
    #[cfg(not(unix))]
    dirs: HashSet<PathBuf>,
}

impl OutputTree {
    #[cfg(unix)]
    fn new(dest_canonical: &Path) -> io::Result<Self> {
        Ok(Self {
            root: dirfd::open_root(dest_canonical)?,
            dirs: HashMap::new(),
        })
    }

    #[cfg(not(unix))]
    fn new(dest_canonical: &Path) -> io::Result<Self> {
        Ok(Self {
            root: dest_canonical.to_path_buf(),
            dirs: HashSet::new(),
        })
    }

    /// Materialise `relative` as a directory, creating every level.
    #[cfg(unix)]
    fn ensure_dir(&mut self, relative: &Path) -> io::Result<()> {
        if !self.dirs.contains_key(relative) {
            let fd = dirfd::create_dir_chain(&self.root, relative)?;
            self.dirs.insert(relative.to_path_buf(), fd);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn ensure_dir(&mut self, relative: &Path) -> io::Result<()> {
        if !self.dirs.contains(relative) {
            fs::create_dir_all(self.root.join(relative))?;
            self.dirs.insert(relative.to_path_buf());
        }
        Ok(())
    }

    /// Create (or truncate) the file at `relative`, making its parents first.
    #[cfg(unix)]
    fn create_file(&mut self, relative: &Path) -> io::Result<File> {
        let name = relative.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "zip entry has no file name")
        })?;
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        self.ensure_dir(parent)?;
        dirfd::create_file(&self.dirs[parent], name)
    }

    #[cfg(not(unix))]
    fn create_file(&mut self, relative: &Path) -> io::Result<File> {
        if relative.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zip entry has no file name",
            ));
        }
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        self.ensure_dir(parent)?;
        // No `O_NOFOLLOW` counterpart here, so this platform keeps the weaker
        // guarantee the path-based checks above provide.
        File::create(self.root.join(relative))
    }
}

/// Reader wrapper that caps the number of bytes that can be read from
/// the underlying stream. Used for streaming decompression so a single
/// entry cannot exceed `max_entry_uncompressed` even if its zip header
/// lied about its size.
struct LimitedEntryReader<'a, R: Read> {
    inner: &'a mut R,
    remaining: u64,
}

impl<'a, R: Read> LimitedEntryReader<'a, R> {
    fn new(inner: &'a mut R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<'a, R: Read> Read for LimitedEntryReader<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "entry size exceeds per-entry budget",
            ));
        }
        let max = std::cmp::min(buf.len() as u64, self.remaining) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Extract a zip file to the destination directory with the default budget.
///
/// See [`extract_zip_with_budget`] for the full-featured version.
pub fn extract_zip(
    zip_path: &Path,
    dest_dir: &Path,
    progress: Option<ProgressCallback>,
) -> Result<(), ZipError> {
    extract_zip_with_budget(zip_path, dest_dir, progress, ExtractBudget::default())
}

/// Extract a zip file to the destination directory, enforcing a resource
/// budget against zip-bomb-style DoS inputs.
///
/// # Security
/// - Path traversal (`..`, absolute paths, symlink entries) is rejected.
/// - Ancestor directory symlinks are rejected (TOCTOU hardening).
/// - Advertised sizes are budget-checked **before** inflate; the
///   streaming decompressor is also bounded so a lying header cannot
///   bypass the per-entry cap.
/// - A running total of actually-written bytes enforces
///   `max_total_uncompressed`.
pub fn extract_zip_with_budget(
    zip_path: &Path,
    dest_dir: &Path,
    progress: Option<ProgressCallback>,
    budget: ExtractBudget,
) -> Result<(), ZipError> {
    debug!(
        "extract_zip: zip={} dest={} budget={:?}",
        zip_path.display(),
        dest_dir.display(),
        budget
    );

    if !zip_path.exists() {
        return Err(ZipError::NotFound(zip_path.display().to_string()));
    }

    let file = File::open(zip_path)?;
    let reader = BufReader::with_capacity(64 * 1024, file);
    let mut archive = ZipArchive::new(reader)?;

    let total_files = archive.len();
    debug!("extract_zip: {} files in archive", total_files);

    if total_files > budget.max_entries {
        return Err(ZipError::BudgetExceeded(format!(
            "entry count {} exceeds limit {}",
            total_files, budget.max_entries
        )));
    }

    // Cheap header-time pre-scan. We pay a single metadata pass so we
    // can reject obviously bomb-shaped archives without touching inflate.
    let mut advertised_total: u64 = 0;
    for i in 0..total_files {
        let entry = archive.by_index_raw(i)?;
        let advertised = entry.size();
        if advertised > budget.max_entry_uncompressed {
            return Err(ZipError::BudgetExceeded(format!(
                "entry '{}' advertises {} bytes, exceeds per-entry limit {}",
                entry.name(),
                advertised,
                budget.max_entry_uncompressed
            )));
        }
        advertised_total = advertised_total.saturating_add(advertised);
        if advertised_total > budget.max_total_uncompressed {
            return Err(ZipError::BudgetExceeded(format!(
                "advertised total {} bytes exceeds limit {}",
                advertised_total, budget.max_total_uncompressed
            )));
        }
    }

    fs::create_dir_all(dest_dir)?;
    let dest_canonical = dest_dir.canonicalize()?;

    let mut written_total: u64 = 0;
    let mut tree = OutputTree::new(&dest_canonical)?;

    for i in 0..total_files {
        let mut file = archive.by_index(i)?;
        let file_name = file.name().to_string();

        trace!("extract_zip: processing [{}] {}", i, file_name);

        // Base the entry path on the *canonical* destination, not the raw
        // `dest_dir`. On Windows `canonicalize()` returns a verbatim path
        // (`\\?\C:\...`), and the containment check below compares against
        // `dest_canonical`; joining onto the non-verbatim `dest_dir` produced a
        // path that never shared that prefix, so `starts_with(&dest_canonical)`
        // was false for *every* entry and rejected even a plain "a.txt" as a
        // traversal. Joining onto the canonical base makes both sides carry the
        // same prefix (verbatim on Windows, none on Unix), so the comparison is
        // correct on both. A `..`/absolute entry still normalizes outside the
        // base and is still rejected.
        let outpath = dest_canonical.join(&file_name);

        if file.is_symlink() {
            error!("extract_zip: symlink entry rejected: {}", file_name);
            return Err(ZipError::PathTraversal(format!(
                "symlink entry not allowed: {}",
                file_name
            )));
        }

        let outpath_normalized = normalize_path(&outpath);
        if !outpath_normalized.starts_with(&dest_canonical) {
            error!(
                "extract_zip: path traversal detected: {} -> {}",
                file_name,
                outpath_normalized.display()
            );
            return Err(ZipError::PathTraversal(file_name));
        }

        // Verify the deepest ancestor that already exists, not just the
        // immediate parent.
        //
        // Checking `parent` alone and skipping when it does not exist left a
        // hole: an attacker who pre-planted a symlink further up gets it
        // followed by the `create_dir_all` below, and nothing ever looked at
        // it. Walking up to the first existing component means the check
        // always runs against something real.
        if let Some(parent) = outpath_normalized.parent() {
            let mut existing = parent;
            while !existing.exists() {
                match existing.parent() {
                    // `dest_canonical` itself exists, so this terminates.
                    Some(up) => existing = up,
                    None => break,
                }
            }
            match std::fs::canonicalize(existing) {
                Ok(canonical_ancestor) => {
                    if !canonical_ancestor.starts_with(&dest_canonical) {
                        error!(
                            "extract_zip: ancestor symlink escape detected: {} -> {}",
                            existing.display(),
                            canonical_ancestor.display()
                        );
                        return Err(ZipError::PathTraversal(format!(
                            "ancestor directory escapes target via symlink: {}",
                            file_name
                        )));
                    }
                }
                Err(e) => {
                    error!(
                        "extract_zip: cannot canonicalize ancestor {}: {} — rejecting entry {}",
                        existing.display(),
                        e,
                        file_name
                    );
                    return Err(ZipError::PathTraversal(format!(
                        "cannot verify ancestor directory: {}",
                        file_name
                    )));
                }
            }
        }

        // Everything below is addressed relative to the root the tree holds,
        // so a component can no longer be re-resolved between check and use.
        let relative = outpath_normalized
            .strip_prefix(&dest_canonical)
            .map_err(|_| ZipError::PathTraversal(file_name.clone()))?;

        if file.is_dir() {
            trace!("extract_zip: creating directory {}", outpath.display());
            tree.ensure_dir(relative)?;
        } else {
            // Per-entry ratio check: uncompressed / compressed.
            let compressed = file.compressed_size();
            let uncompressed_hdr = file.size();
            if budget.max_compression_ratio > 0 && compressed > 0 {
                let ratio = uncompressed_hdr / compressed;
                if ratio > budget.max_compression_ratio {
                    return Err(ZipError::BudgetExceeded(format!(
                        "entry '{}' compression ratio {} exceeds limit {}",
                        file_name, ratio, budget.max_compression_ratio
                    )));
                }
            }

            // Streaming copy bounded by the minimum of the per-entry
            // cap and the remaining total budget. If either is hit we
            // reject the archive instead of silently truncating.
            let remaining_total = budget.max_total_uncompressed.saturating_sub(written_total);
            let cap = std::cmp::min(budget.max_entry_uncompressed, remaining_total);
            let mut limited = LimitedEntryReader::new(&mut file, cap + 1);

            let mut outfile = tree.create_file(relative)?;
            let actually_written = match io::copy(&mut limited, &mut outfile) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    let _ = fs::remove_file(&outpath);
                    return Err(ZipError::BudgetExceeded(format!(
                        "entry '{}' exceeded per-entry or total budget",
                        file_name
                    )));
                }
                Err(e) => return Err(ZipError::Io(e)),
            };

            if actually_written > budget.max_entry_uncompressed {
                let _ = fs::remove_file(&outpath);
                return Err(ZipError::BudgetExceeded(format!(
                    "entry '{}' wrote {} bytes, exceeds per-entry limit {}",
                    file_name, actually_written, budget.max_entry_uncompressed
                )));
            }

            written_total = written_total.saturating_add(actually_written);
            if written_total > budget.max_total_uncompressed {
                let _ = fs::remove_file(&outpath);
                return Err(ZipError::BudgetExceeded(format!(
                    "archive wrote {} bytes, exceeds total limit {}",
                    written_total, budget.max_total_uncompressed
                )));
            }

            trace!(
                "extract_zip: extracted {} ({} bytes, total {})",
                outpath.display(),
                actually_written,
                written_total
            );

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    if let Err(e) = fs::set_permissions(&outpath, fs::Permissions::from_mode(mode))
                    {
                        warn!("extract_zip: failed to set permissions: {}", e);
                    }
                }
            }
        }

        if let Some(ref callback) = progress {
            let prog = (i + 1) as f32 / total_files as f32;
            callback(prog, i + 1, total_files);
        }
    }

    debug!(
        "extract_zip: completed, {} files extracted, {} bytes total",
        total_files, written_total
    );
    Ok(())
}

/// Normalize a path without requiring it to exist.
/// This handles .. and . components manually.
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            c => {
                result.push(c);
            }
        }
    }

    result
}

/// Extract zip file asynchronously (runs in a blocking thread pool).
///
/// Uses the default `ExtractBudget`. For a custom budget see
/// [`extract_zip_with_scheduler_and_budget`].
pub async fn extract_zip_with_scheduler(
    scheduler: Arc<IoScheduler>,
    zip_path: PathBuf,
    dest_dir: PathBuf,
    progress_tx: Option<tokio::sync::mpsc::Sender<(f32, usize, usize)>>,
) -> Result<(), ZipError> {
    extract_zip_with_scheduler_and_budget(
        scheduler,
        zip_path,
        dest_dir,
        progress_tx,
        ExtractBudget::default(),
    )
    .await
}

pub async fn extract_zip_with_scheduler_and_budget(
    scheduler: Arc<IoScheduler>,
    zip_path: PathBuf,
    dest_dir: PathBuf,
    progress_tx: Option<tokio::sync::mpsc::Sender<(f32, usize, usize)>>,
    budget: ExtractBudget,
) -> Result<(), ZipError> {
    let request = unzip_request_for(&zip_path);

    scheduler
        .run_async(request, move || {
            let progress: Option<ProgressCallback> = progress_tx.map(|tx| {
                Box::new(move |prog: f32, current: usize, total: usize| {
                    let _ = tx.blocking_send((prog, current, total));
                }) as ProgressCallback
            });

            extract_zip_with_budget(&zip_path, &dest_dir, progress, budget)
        })
        .await
        .map_err(ZipError::from)?
}

pub async fn extract_zip_async(
    scheduler: Arc<IoScheduler>,
    zip_path: PathBuf,
    dest_dir: PathBuf,
    progress_tx: Option<tokio::sync::mpsc::Sender<(f32, usize, usize)>>,
) -> Result<(), ZipError> {
    extract_zip_with_scheduler(scheduler, zip_path, dest_dir, progress_tx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;

    use crate::scheduler::IoScheduler;

    /// Is the Archive class cap of 1 leaving anything on the table?
    ///
    /// `ExecutorConfig::for_workers` pins that class to a single worker, so
    /// every unzip and package ingest in the process runs one at a time. That
    /// is either correct (extraction is I/O bound, so concurrency buys nothing
    /// and costs memory) or it is throughput being discarded. Which one is a
    /// property of the workload, not of the pool — so this measures the
    /// workload directly, extracting the same archives sequentially and then
    /// one thread each.
    ///
    /// Entries are Deflated, not Stored: a Stored archive makes this a
    /// file-copy benchmark and would answer a different question.
    #[test]
    #[ignore]
    fn bench_parallel_extraction_speedup() {
        const ARCHIVES: usize = 4;
        const ENTRIES_PER_ARCHIVE: usize = 8;
        const ENTRY_BYTES: usize = 512 * 1024;

        let root = std::env::temp_dir().join(format!(
            "migo_zip_parallel_bench_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        // Two payload profiles, because the answer must not depend on which
        // one a game happens to ship: `text` stands for JSON/atlas descriptors
        // that deflate crushes, `binary` for PNG/audio that is already
        // compressed and passes through nearly as-is.
        let profile = std::env::var("MIGO_BENCH_PAYLOAD").unwrap_or_else(|_| "text".to_string());
        let payload: Vec<u8> = match profile.as_str() {
            // splitmix64 finalizer: uniform bytes, so deflate finds nothing and
            // the archive is the same size as its contents. A linear generator
            // will not do here — its high bits change slowly and compress by
            // tens to one, which is the opposite of what this profile is for.
            "binary" => (0..ENTRY_BYTES)
                .map(|i| {
                    let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
                    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                    (z ^ (z >> 31)) as u8
                })
                .collect(),
            _ => (0..ENTRY_BYTES)
                .map(|i| (((i as u64).wrapping_mul(2654435761) >> 16) & 0x3F) as u8)
                .collect(),
        };

        let mut archives = Vec::new();
        for a in 0..ARCHIVES {
            let zip_path = root.join(format!("input_{a}.zip"));
            let file = File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for e in 0..ENTRIES_PER_ARCHIVE {
                zip.start_file(format!("dir/entry_{e}.bin"), options)
                    .unwrap();
                zip.write_all(&payload).unwrap();
            }
            zip.finish().unwrap();
            archives.push(zip_path);
        }

        let uncompressed =
            (ARCHIVES * ENTRIES_PER_ARCHIVE * ENTRY_BYTES) as f64 / (1024.0 * 1024.0);
        let compressed: u64 = archives
            .iter()
            .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .sum();

        let extract_serial = |label: &str| {
            let out_root = root.join(label);
            let started = std::time::Instant::now();
            for (i, zip_path) in archives.iter().enumerate() {
                extract_zip_with_budget(
                    zip_path,
                    &out_root.join(format!("a{i}")),
                    None,
                    ExtractBudget::default(),
                )
                .unwrap();
            }
            let elapsed = started.elapsed();
            let _ = std::fs::remove_dir_all(&out_root);
            elapsed
        };
        let extract_parallel = |label: &str| {
            let out_root = root.join(label);
            let started = std::time::Instant::now();
            std::thread::scope(|scope| {
                for (i, zip_path) in archives.iter().enumerate() {
                    let out = out_root.join(format!("a{i}"));
                    scope.spawn(move || {
                        extract_zip_with_budget(zip_path, &out, None, ExtractBudget::default())
                            .unwrap();
                    });
                }
            });
            let elapsed = started.elapsed();
            let _ = std::fs::remove_dir_all(&out_root);
            elapsed
        };

        // Warm once, then alternate which identical extraction/cleanup scope
        // runs first so filesystem and allocator order do not favor one side.
        extract_serial("warm");
        let mut sequential_samples = Vec::new();
        let mut parallel_samples = Vec::new();
        for round in 0..4 {
            if round % 2 == 0 {
                sequential_samples.push(extract_serial(&format!("seq_{round}")));
                parallel_samples.push(extract_parallel(&format!("par_{round}")));
            } else {
                parallel_samples.push(extract_parallel(&format!("par_{round}")));
                sequential_samples.push(extract_serial(&format!("seq_{round}")));
            }
        }
        let sequential = sequential_samples.iter().sum::<std::time::Duration>() / 4;
        let parallel = parallel_samples.iter().sum::<std::time::Duration>() / 4;

        eprintln!(
            "payload={profile}  {ARCHIVES} archives x {ENTRIES_PER_ARCHIVE} entries x {} KiB = {uncompressed:.0} MiB uncompressed ({:.1} MiB on disk, {:.1}:1)",
            ENTRY_BYTES / 1024,
            compressed as f64 / (1024.0 * 1024.0),
            uncompressed / (compressed as f64 / (1024.0 * 1024.0)).max(f64::MIN_POSITIVE)
        );
        eprintln!(
            "cores available   {}",
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0)
        );
        eprintln!("sequential (same cleanup scope, alternating order)  {sequential:>12?}");
        eprintln!("parallel   ({ARCHIVES} extraction threads, same cleanup scope) {parallel:>12?}");
        eprintln!(
            "speedup {:.2}x  -- Archive cap is worker_count.saturating_sub(1).clamp(1,2)",
            sequential.as_secs_f64() / parallel.as_secs_f64().max(f64::MIN_POSITIVE)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// An entry whose own path was pre-planted as a symlink must not be
    /// followed.
    ///
    /// This is the reachable half of the extraction TOCTOU: the entry name
    /// comes from the archive, so its final component is what an attacker aims
    /// somewhere else. The containment check passes — the path really is
    /// inside `dest` — and only the open refusing to traverse the link stops
    /// the write from landing outside.
    #[cfg(unix)]
    #[test]
    fn entry_whose_path_is_a_symlink_is_refused() {
        let base = std::env::temp_dir().join(format!(
            "migo_zip_symlink_leaf_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dest = base.join("dest");
        let outside = base.join("outside");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let victim = outside.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();

        // The archive writes `loot.txt`; that name already exists inside dest
        // as a link pointing out of it.
        std::os::unix::fs::symlink(&victim, dest.join("loot.txt")).unwrap();

        let zip_path = base.join("evil.zip");
        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("loot.txt", options).unwrap();
        zip.write_all(b"OVERWRITTEN").unwrap();
        zip.finish().unwrap();

        let result = extract_zip_with_budget(&zip_path, &dest, None, ExtractBudget::default());

        assert!(
            result.is_err(),
            "extraction through a symlinked entry path must fail"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"original",
            "the file outside dest was overwritten through the symlink"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A symlinked directory is refused even when it points back *inside*
    /// `dest`.
    ///
    /// This is the case that isolates what the descriptor walk adds. The
    /// path-based checks pass it: canonicalising `dest/assets` yields
    /// `dest/real`, which is contained, so containment has nothing to object
    /// to. Only `openat` with `O_NOFOLLOW` refuses to traverse it.
    ///
    /// Refusing is the point rather than an over-reach: a link the extraction
    /// is willing to walk is a link an attacker can re-aim afterwards, and an
    /// app's own sandbox directory has no legitimate reason to contain one.
    #[cfg(unix)]
    #[test]
    fn symlinked_directory_is_refused_even_when_it_stays_inside_dest() {
        let base = std::env::temp_dir().join(format!(
            "migo_zip_symlink_inside_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dest = base.join("dest");
        std::fs::create_dir_all(dest.join("real")).unwrap();
        std::os::unix::fs::symlink(dest.join("real"), dest.join("assets")).unwrap();

        let zip_path = base.join("input.zip");
        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("assets/x.txt", options).unwrap();
        zip.write_all(b"payload").unwrap();
        zip.finish().unwrap();

        let result = extract_zip_with_budget(&zip_path, &dest, None, ExtractBudget::default());

        // Which defence fired is the assertion, not just that one did. A
        // `PathTraversal` here would mean containment rejected it and this test
        // proves nothing about `openat`; `ELOOP` from the kernel is the
        // descriptor walk refusing to traverse the link.
        match result {
            Err(ZipError::Io(err)) => {
                // `O_DIRECTORY | O_NOFOLLOW` meeting a symlink reports ENOTDIR
                // on Linux ("that is not a directory") and ELOOP on the BSDs
                // ("that is a link"). Both mean the kernel refused to traverse
                // it, which is the property under test; neither is a fallback
                // for the other.
                let code = err.raw_os_error();
                assert!(
                    code == Some(libc::ENOTDIR) || code == Some(libc::ELOOP),
                    "expected the open to be refused as a symlink, got {err:?}"
                );
            }
            other => panic!("expected the descriptor walk to refuse the link, got {other:?}"),
        }
        assert!(
            !dest.join("real").join("x.txt").exists(),
            "the entry was written through the link"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The containment check must not be skipped just because the immediate
    /// parent has not been created yet.
    ///
    /// A symlinked directory further up used to go unexamined: the check only
    /// ran when `parent.exists()`, and for a nested entry it does not until
    /// `create_dir_all` runs — which would have followed the link first.
    #[cfg(unix)]
    #[test]
    fn symlinked_ancestor_is_caught_before_its_child_is_created() {
        let base = std::env::temp_dir().join(format!(
            "migo_zip_symlink_ancestor_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dest = base.join("dest");
        let outside = base.join("outside");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        // `dest/assets` is a link out of the tree. The entry lands two levels
        // below it, so its immediate parent (`dest/assets/img`) does not exist
        // and the old check had nothing to look at.
        std::os::unix::fs::symlink(&outside, dest.join("assets")).unwrap();

        let zip_path = base.join("evil.zip");
        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("assets/img/planted.bin", options).unwrap();
        zip.write_all(b"PLANTED").unwrap();
        zip.finish().unwrap();

        let result = extract_zip_with_budget(&zip_path, &dest, None, ExtractBudget::default());

        assert!(
            result.is_err(),
            "an entry under a symlinked ancestor must be rejected"
        );
        assert!(
            !outside.join("img").exists(),
            "directories were created outside dest through the symlinked ancestor"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_normalize_path() {
        let p = normalize_path(Path::new("/a/b/../c/./d"));
        assert_eq!(p, PathBuf::from("/a/c/d"));

        let p = normalize_path(Path::new("a/b/../../c"));
        assert_eq!(p, PathBuf::from("c"));
    }

    #[test]
    fn test_path_traversal_detection() {
        let dest = Path::new("/tmp/dest");
        let malicious = dest.join("../../../etc/passwd");
        let normalized = normalize_path(&malicious);
        let dest_canonical = dest.to_path_buf();
        assert!(!normalized.starts_with(&dest_canonical));
    }

    #[test]
    fn unzip_requests_use_archive_pool() {
        let dir =
            std::env::temp_dir().join(format!("migo_zip_extract_scheduler_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let zip_path = dir.join("input.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("hello.txt", options).unwrap();
        zip.write_all(b"hello archive").unwrap();
        zip.finish().unwrap();

        let dest_dir = dir.join("out");
        let scheduler = Arc::new(IoScheduler::local_for_test(31, 2));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime
            .block_on(extract_zip_with_scheduler(
                Arc::clone(&scheduler),
                zip_path.clone(),
                dest_dir.clone(),
                None,
            ))
            .unwrap();

        assert_eq!(scheduler.pools().started_thread_count_for_test(), 2);
        assert_eq!(
            std::fs::read(dest_dir.join("hello.txt")).unwrap(),
            b"hello archive"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unzip_requests_default_to_background_priority() {
        let request = unzip_request_for(Path::new("/tmp/archive.zip"));
        match request {
            IoRequest::Unzip { priority, .. } => {
                assert_eq!(priority, PriorityClass::Background);
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn extract_zip_async_uses_provided_scheduler() {
        let dir =
            std::env::temp_dir().join(format!("migo_zip_extract_async_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let zip_path = dir.join("input.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("hello.txt", options).unwrap();
        zip.write_all(b"hello archive async").unwrap();
        zip.finish().unwrap();

        let dest_dir = dir.join("out");
        let scheduler = Arc::new(IoScheduler::new(43));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime
            .block_on(extract_zip_async(
                Arc::clone(&scheduler),
                zip_path.clone(),
                dest_dir.clone(),
                None,
            ))
            .unwrap();

        assert_eq!(
            std::fs::read(dest_dir.join("hello.txt")).unwrap(),
            b"hello archive async"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn budget_rejects_too_many_entries() {
        let dir =
            std::env::temp_dir().join(format!("migo_zip_budget_entries_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let zip_path = dir.join("many.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for i in 0..10 {
            zip.start_file(format!("f_{i}.txt"), options).unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();

        let dest_dir = dir.join("out");
        let budget = ExtractBudget {
            max_entries: 3,
            ..ExtractBudget::DEFAULT
        };
        let res = extract_zip_with_budget(&zip_path, &dest_dir, None, budget);
        assert!(matches!(res, Err(ZipError::BudgetExceeded(_))));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn budget_rejects_single_large_entry() {
        let dir =
            std::env::temp_dir().join(format!("migo_zip_budget_entry_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let zip_path = dir.join("big.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("big.bin", options).unwrap();
        zip.write_all(&vec![0u8; 4096]).unwrap();
        zip.finish().unwrap();

        let dest_dir = dir.join("out");
        let budget = ExtractBudget {
            max_entry_uncompressed: 1024,
            max_total_uncompressed: 1024,
            ..ExtractBudget::DEFAULT
        };
        let res = extract_zip_with_budget(&zip_path, &dest_dir, None, budget);
        assert!(matches!(res, Err(ZipError::BudgetExceeded(_))));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn budget_rejects_total_overflow() {
        let dir =
            std::env::temp_dir().join(format!("migo_zip_budget_total_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let zip_path = dir.join("total.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for i in 0..4 {
            zip.start_file(format!("f_{i}.bin"), options).unwrap();
            zip.write_all(&vec![0u8; 1024]).unwrap();
        }
        zip.finish().unwrap();

        let dest_dir = dir.join("out");
        let budget = ExtractBudget {
            max_entry_uncompressed: 4096,
            max_total_uncompressed: 2048,
            ..ExtractBudget::DEFAULT
        };
        let res = extract_zip_with_budget(&zip_path, &dest_dir, None, budget);
        assert!(matches!(res, Err(ZipError::BudgetExceeded(_))));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn budget_allows_normal_archive() {
        let dir = std::env::temp_dir().join(format!("migo_zip_budget_ok_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let zip_path = dir.join("ok.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("a.txt", options).unwrap();
        zip.write_all(b"hello").unwrap();
        zip.start_file("b.txt", options).unwrap();
        zip.write_all(b"world").unwrap();
        zip.finish().unwrap();

        let dest_dir = dir.join("out");
        extract_zip_with_budget(&zip_path, &dest_dir, None, ExtractBudget::default()).unwrap();
        assert_eq!(std::fs::read(dest_dir.join("a.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dest_dir.join("b.txt")).unwrap(), b"world");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
