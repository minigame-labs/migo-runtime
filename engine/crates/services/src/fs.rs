//! The game's sandboxed file system: what `migo.getFileSystemManager()` reads
//! and writes.
//!
//! Paths are virtual: `/code` is the game's package, read-only and resolved
//! through the mount table (its files may be on disk or inside a pack);
//! `/user`, `/cache` and `/tmp` are the game's own directories, resolved
//! through the [`VirtualFS`] that pins them. A relative path is relative to
//! `/code`, as on the platforms the API comes from. Open files live in the IO
//! scheduler's domain, so a descriptor is only meaningful to the session whose
//! scheduler opened it.
//!
//! Every blocking step runs through the [`IoScheduler`] -- never a raw
//! `spawn_blocking` -- so a file operation gets the domain-close check, its
//! priority class, byte backpressure and the shared IO metrics. The synchronous
//! calls classify as inline where the scheduler says they are cheap, because
//! their caller is blocked anyway and a worker hop is pure latency.
//!
//! Moved here from the embedded runtime's ops, whose adapters now convert
//! arguments and call these, so the external session's service dispatcher
//! applies the same rules: the same resolution, the same limits, the same
//! messages. Every failure is an `IOError`, as it always was.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use migo_io::domain::DomainError;
use migo_io::fs_ops;
use migo_io::pools::PoolError;
use migo_io::scheduler::IoScheduler;
use migo_io::task::{BackendKind, IoRequest, PriorityClass, RequestKind};
use shared::codec;
use shared::error::{EngineError, ErrorCode};
use shared::protocol::io_cmd::{
    FileId, FileStat, OpenFlag, SavedFileInfo, StatResult, WriteDurability, WriteMode,
    ZipEntryResult,
};
use shared::services::FileService;
use shared::vfs::{FileOp, MountTable, VfsError, VirtualFS};

use crate::error::ServiceError;

/// The class every file-system failure is thrown as.
pub const CLASS_IO_ERROR: &str = "IOError";

/// The largest `/code` entry materialized to a temporary file, for the calls
/// that need a real path (`open`, zip reads, `unzip`).
const MAX_MATERIALIZE_LENGTH: u64 = 512 * 1024 * 1024;

/// A file-system call's result.
pub type FsResult<T> = Result<T, ServiceError>;

/// What a file-system call reads: the session's scheduler, and the game's
/// sandbox once content is mounted. Before that, `vfs` and `mount_table` are
/// `None` and every path fails to resolve, with the message it always had.
#[derive(Clone)]
pub struct FsEnv {
    pub scheduler: Arc<IoScheduler>,
    pub vfs: Option<Arc<VirtualFS>>,
    pub mount_table: Option<Arc<MountTable>>,
}

#[inline]
fn ioerr(msg: impl Into<String>) -> ServiceError {
    ServiceError::classed(CLASS_IO_ERROR, msg)
}

/// An engine error in the text the ops have always thrown: code, summary,
/// detail.
#[inline]
pub fn engine_err(e: EngineError) -> ServiceError {
    ioerr(match &e.detail {
        Some(d) => format!("[{:?}] {} ({})", e.code, e.msg, d),
        None => format!("[{:?}] {}", e.code, e.msg),
    })
}

#[inline]
fn domain_err(err: DomainError) -> ServiceError {
    match err {
        DomainError::Closed => ioerr("IO domain closed"),
        DomainError::Io(err) => engine_err(err),
    }
}

#[inline]
fn pool_err(err: PoolError) -> ServiceError {
    match err {
        PoolError::Closed => ioerr("IO worker pool closed"),
        PoolError::ByteLimitExceeded {
            requested,
            available,
        } => ioerr(format!(
            "IO pending-byte budget exhausted: requested {requested} bytes, {available} available"
        )),
    }
}

#[inline]
fn trace_file_edge(op: &str, target: &str, started_at: Instant, detail: &str) {
    tracing::debug!(
        "[IOTrace] {} {}us target={} {}",
        op,
        started_at.elapsed().as_micros(),
        target,
        detail
    );
}

// ---------------------------------------------------------------------------
// Scheduling
// ---------------------------------------------------------------------------

/// Build the scheduler descriptor for a read.
///
/// `length` is what the caller asked for; `size_hint` is what the backend
/// already knows the read will actually produce. Both bound the result, so
/// the estimate is whichever is smaller.
///
/// Passing a `size_hint` is what lets a small whole-file read reach the
/// scheduler's inline path. `readFile(path)` leaves `length` at `None`, and
/// an unhinted `None` has to assume `MAX_READ_LENGTH` — so without a hint
/// every whole-file read, a 200-byte JSON included, estimates at 100 MiB,
/// classifies as expensive, and pays a worker round-trip that costs more
/// than the read itself.
#[inline]
pub fn read_request(
    backend: BackendKind,
    request: RequestKind,
    length: Option<u64>,
    size_hint: Option<u64>,
) -> IoRequest {
    let estimated_bytes = match (length, size_hint) {
        (Some(length), Some(hint)) => length.min(hint),
        (Some(length), None) => length,
        (None, Some(hint)) => hint,
        (None, None) => shared::protocol::io_cmd::MAX_READ_LENGTH,
    } as usize;

    IoRequest::ReadFile {
        backend,
        request,
        priority: PriorityClass::from(request),
        estimated_bytes,
    }
}

/// Size of `path` for read classification, or `None` when it can't be had.
///
/// Only called on the sync path. There the caller's thread is blocked for the
/// whole operation, so a worker hop is pure added latency and one `stat` buys
/// the chance to skip it — and against the read that follows either way, the
/// syscall is a rounding error. On the async path the caller is not blocked,
/// the hop costs throughput rather than latency, and this `stat` would land
/// on the caller's thread for no gain.
#[inline]
fn fs_read_size_hint(path: &str) -> Option<u64> {
    std::fs::metadata(path).ok().map(|meta| meta.len())
}

#[inline]
pub fn archive_read_request() -> IoRequest {
    read_request(BackendKind::Archive, RequestKind::Async, None, None)
}

#[inline]
fn copy_request(backend: BackendKind, request: RequestKind) -> IoRequest {
    IoRequest::ReadFile {
        backend,
        request,
        priority: PriorityClass::from(request),
        estimated_bytes: shared::protocol::io_cmd::MAX_READ_LENGTH as usize,
    }
}

/// A whole pack entry read or digested on the Pack lane.
#[inline]
fn pack_whole_read_request() -> IoRequest {
    IoRequest::ReadFile {
        backend: BackendKind::Pack,
        request: RequestKind::Async,
        priority: PriorityClass::ForegroundAsync,
        estimated_bytes: shared::protocol::io_cmd::MAX_READ_LENGTH as usize,
    }
}

/// Request descriptor for a generic blocking fs op (write/copy/mkdir/
/// stat/...). Routing these through the scheduler (instead of a raw
/// `tokio::spawn_blocking`) gives them domain-close checks, priority,
/// backpressure and the shared IO metrics.
#[inline]
fn fs_op_request(request: RequestKind) -> IoRequest {
    IoRequest::FsOp {
        request,
        priority: PriorityClass::from(request),
    }
}

/// Run a blocking fs job through the scheduler on the async path,
/// flattening the `PoolError` and the inner `EngineError`.
async fn run_fs_async<T, F>(scheduler: Arc<IoScheduler>, job: F) -> FsResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, EngineError> + Send + 'static,
{
    scheduler
        .run_async(fs_op_request(RequestKind::Async), job)
        .await
        .map_err(pool_err)?
        .map_err(engine_err)
}

/// Run complete Pack reads/digests on the Pack lane rather than the generic
/// filesystem lane. Mount resolution is performed by the moved closure.
pub async fn run_pack_async<T, F>(
    scheduler: Arc<IoScheduler>,
    request: IoRequest,
    job: F,
) -> FsResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, EngineError> + Send + 'static,
{
    scheduler
        .run_async(request, job)
        .await
        .map_err(pool_err)?
        .map_err(engine_err)
}

/// Run a blocking file-table operation through the bounded filesystem class.
/// Keep `DomainError` distinct until the final flattening so closed-domain and
/// filesystem errors retain their existing messages.
pub async fn run_domain_async<T, F>(scheduler: Arc<IoScheduler>, job: F) -> FsResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DomainError> + Send + 'static,
{
    scheduler
        .run_async(fs_op_request(RequestKind::Async), job)
        .await
        .map_err(pool_err)?
        .map_err(domain_err)
}

/// Run a blocking fs job through the scheduler on the sync path. Sync
/// (ForegroundBlocking) ops classify as Inline, so the job runs on the
/// calling thread -- behind the scheduler's domain-close guard and metrics.
fn run_fs_sync<T, F>(scheduler: &IoScheduler, job: F) -> FsResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, EngineError> + Send + 'static,
{
    scheduler
        .run_sync(&fs_op_request(RequestKind::Sync), job)
        .map_err(pool_err)?
        .map_err(engine_err)
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Result of path resolution: either a real filesystem path for IO-thread
/// operations, or a virtual path backed by a package (data read inline).
pub enum ResolvedPath {
    /// Real filesystem path — send to IO thread.
    Filesystem(String),
    /// Pack-backed — data must be read via MountTable::read(), not IO thread.
    Pack { virtual_path: String },
}

/// Resolve a path using VFS + MountTable.
///
/// Virtual paths must start with /user, /cache, /code, or /tmp.
/// Relative paths (not starting with '/') are treated as relative to the
/// game code directory (/code/), matching the target platform's readFile
/// semantics where relative paths resolve against the game package root.
///
/// `/code` paths are resolved through the mount table when available,
/// falling back to VFS if not.  Other virtual paths go through VFS.
#[inline]
pub fn resolve_path_vfs(
    vfs: Option<&VirtualFS>,
    mount_table: Option<&MountTable>,
    path: &str,
    op: FileOp,
) -> FsResult<ResolvedPath> {
    // Platform compat: relative paths map to /code/ (game package directory)
    let resolved;
    let virtual_path = if !path.starts_with('/') {
        resolved = format!("/code/{}", path);
        &resolved
    } else {
        path
    };

    // /code paths: prefer mount table.
    if virtual_path == "/code" || virtual_path.starts_with("/code/") {
        // /code is read-only — reject writes early.
        if matches!(op, FileOp::Write | FileOp::Create | FileOp::Delete) {
            return Err(ioerr(format!("Permission denied: {}", path)));
        }
        if let Some(mt) = mount_table {
            let res = mt
                .resolve_code_path(virtual_path)
                .ok_or_else(|| ioerr(format!("Path resolution failed: {}", path)))?;
            return match res.real_path {
                Some(real) => Ok(ResolvedPath::Filesystem(
                    real.to_string_lossy().into_owned(),
                )),
                None => Ok(ResolvedPath::Pack {
                    virtual_path: virtual_path.to_string(),
                }),
            };
        }
        // Fallback to VFS for /code if no mount table.
    }

    // /user, /cache, /tmp (and /code fallback) — go through VFS.
    let vfs = vfs.ok_or_else(|| ioerr("File system not initialized"))?;
    vfs.resolve(virtual_path, op)
        .map(|p| ResolvedPath::Filesystem(p.to_string_lossy().into_owned()))
        .map_err(|e| match e {
            VfsError::PathNotAllowed => ioerr(format!(
                "Path not allowed: {}. Use /user, /cache, /code, or /tmp",
                path
            )),
            VfsError::PermissionDenied => ioerr(format!("Permission denied: {}", path)),
            VfsError::PathTraversal => ioerr(format!("Path traversal detected: {}", path)),
            VfsError::SymlinkEscape => ioerr(format!("Symlink resolves outside sandbox: {}", path)),
            VfsError::SymlinkNotAllowed => {
                ioerr(format!("Symlinks not allowed in this directory: {}", path))
            }
            VfsError::InvalidPath => ioerr(format!("Invalid path: {}", path)),
        })
}

impl FsEnv {
    #[inline]
    fn resolve(&self, path: &str, op: FileOp) -> FsResult<ResolvedPath> {
        resolve_path_vfs(self.vfs.as_deref(), self.mount_table.as_deref(), path, op)
    }

    /// A path that must be a real file: writes, and the calls that have no
    /// pack-backed form.
    #[inline]
    fn resolve_fs(&self, path: &str, op: FileOp) -> FsResult<String> {
        require_fs_path(self.resolve(path, op)?)
    }

    #[inline]
    fn mounts(&self) -> FsResult<&Arc<MountTable>> {
        self.mount_table
            .as_ref()
            .ok_or_else(|| ioerr("mount table not initialized"))
    }
}

/// Extract a filesystem path from a resolved path, returning an error for
/// pack-backed paths.  Used by ops that haven't been adapted for pack reads.
#[inline]
fn require_fs_path(resolved: ResolvedPath) -> FsResult<String> {
    match resolved {
        ResolvedPath::Filesystem(p) => Ok(p),
        ResolvedPath::Pack { virtual_path } => Err(ioerr(format!(
            "Operation not supported on pack-backed path: {}",
            virtual_path,
        ))),
    }
}

/// Strip the `/code/` prefix to get a mount-table relative path.
#[inline]
pub fn code_relative(virtual_path: &str) -> &str {
    virtual_path.strip_prefix("/code/").unwrap_or("")
}

/// Whether a virtual path refers to the read-only `/code` mount.
///
/// mmap-based whole-file reads are only safe on read-only, immutable
/// backends: `/code` is read-only and immutable within a mount generation,
/// so mapping its files can't hit the truncation→SIGBUS window. `/user`
/// `/cache` `/tmp` are writable and must not be mmap'd. Mirrors
/// `resolve_path_vfs`'s mapping of relative paths onto `/code`.
#[inline]
fn is_read_only_code_path(path: &str) -> bool {
    !path.starts_with('/') || path == "/code" || path.starts_with("/code/")
}

/// Read bytes for a /code path via MountTable.  Used by read-oriented ops
/// when the path resolves to a pack backend.
#[inline]
fn read_pack_bytes(mount_table: Option<&MountTable>, virtual_path: &str) -> FsResult<Vec<u8>> {
    let mt = mount_table.ok_or_else(|| ioerr("mount table not initialized"))?;
    let relative = code_relative(virtual_path);
    let max_len = shared::protocol::io_cmd::MAX_READ_LENGTH;
    if let Some(size) = mt.entry_size(relative) {
        if size > max_len {
            return Err(ioerr(format!(
                "file size {} exceeds limit {}",
                size, max_len
            )));
        }
    }
    mt.read_range_limited(relative, 0, None, max_len)
        .map_err(|e| ioerr(format!("pack read failed: {e}")))
}

fn materialize_pack_to_temp(
    mount_table: Option<&MountTable>,
    virtual_path: &str,
    suffix: &str,
) -> FsResult<String> {
    let mt = mount_table.ok_or_else(|| ioerr("mount table not initialized"))?;
    let relative = code_relative(virtual_path);
    if let Some(size) = mt.entry_size(relative) {
        if size > MAX_MATERIALIZE_LENGTH {
            return Err(ioerr(format!(
                "file size {} exceeds materialize limit {}",
                size, MAX_MATERIALIZE_LENGTH,
            )));
        }
    }
    fs_ops::materialize_mount_entry_to_temp(mt, relative, suffix)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(engine_err)
}

pub fn materialize_pack_to_temp_checked(
    scheduler: &IoScheduler,
    mount_table: Option<&MountTable>,
    virtual_path: &str,
    suffix: &str,
) -> FsResult<String> {
    scheduler.ensure_open().map_err(pool_err)?;
    materialize_pack_to_temp(mount_table, virtual_path, suffix)
}

pub async fn materialize_pack_to_temp_async(
    scheduler: Arc<IoScheduler>,
    mount_table: Arc<MountTable>,
    virtual_path: String,
    suffix: &'static str,
) -> FsResult<String> {
    let relative = code_relative(&virtual_path).to_string();
    if let Some(size) = mount_table.entry_size(&relative) {
        if size > MAX_MATERIALIZE_LENGTH {
            return Err(ioerr(format!(
                "file size {} exceeds materialize limit {}",
                size, MAX_MATERIALIZE_LENGTH,
            )));
        }
    }

    let request = copy_request(BackendKind::Pack, RequestKind::Async);
    scheduler
        .run_async(request, move || {
            fs_ops::materialize_mount_entry_to_temp(&mount_table, &relative, suffix)
        })
        .await
        .map_err(pool_err)?
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(engine_err)
}

pub async fn copy_pack_file_async(
    scheduler: Arc<IoScheduler>,
    mount_table: Arc<MountTable>,
    relative_path: String,
    dest_path: PathBuf,
) -> FsResult<()> {
    let request = copy_request(BackendKind::Pack, RequestKind::Async);
    scheduler
        .run_async(request, move || {
            fs_ops::copy_mount_entry_to_path(&mount_table, &relative_path, &dest_path)
        })
        .await
        .map_err(pool_err)?
        .map_err(engine_err)
}

/// Construct a StatResult for a pack-backed /code path.
/// Returns IOError for not-found, matching filesystem stat behavior.
fn pack_stat(
    mount_table: Option<&MountTable>,
    virtual_path: &str,
    recursive: bool,
) -> FsResult<StatResult> {
    use shared::protocol::io_cmd::StatEntry;
    let rel = code_relative(virtual_path);
    let mt = mount_table.ok_or_else(|| ioerr("mount table not initialized"))?;

    // Check if it's a file entry (not a directory).
    if mt.is_file(rel) {
        let size = mt.entry_size(rel).unwrap_or(0);
        return Ok(StatResult::Single(FileStat {
            mode: 0o444,
            size,
            atime: 0,
            mtime: 0,
            is_file: true,
            is_directory: false,
        }));
    }

    // Check if it's a directory prefix (has children).
    let children = mt.list_dir(rel);
    if !children.is_empty() || rel.is_empty() {
        if recursive {
            // Match filesystem stat_dir_recursive semantics:
            // - Only files, no directory entries
            // - Paths relative to queried dir
            // - Sorted by path (BTreeMap)
            use std::collections::BTreeMap;
            let mut out: BTreeMap<String, FileStat> = BTreeMap::new();
            let mut stack: Vec<(String, String)> = children
                .iter()
                .map(|name| {
                    let full = if rel.is_empty() {
                        name.clone()
                    } else {
                        format!("{rel}/{name}")
                    };
                    (name.clone(), full)
                })
                .collect();
            while let Some((rel_name, full_path)) = stack.pop() {
                if mt.is_file(&full_path) {
                    // File entry — add to output.
                    let size = mt.entry_size(&full_path).unwrap_or(0);
                    out.insert(
                        rel_name,
                        FileStat {
                            mode: 0o444,
                            size,
                            atime: 0,
                            mtime: 0,
                            is_file: true,
                            is_directory: false,
                        },
                    );
                } else {
                    // Directory — recurse but don't add to output.
                    for child in mt.list_dir(&full_path) {
                        let child_rel = format!("{rel_name}/{child}");
                        let child_full = format!("{full_path}/{child}");
                        stack.push((child_rel, child_full));
                    }
                }
            }
            return Ok(StatResult::Recursive(
                out.into_iter()
                    .map(|(path, stat)| StatEntry { path, stat })
                    .collect(),
            ));
        }
        return Ok(StatResult::Single(FileStat {
            mode: 0o555,
            size: 0,
            atime: 0,
            mtime: 0,
            is_file: false,
            is_directory: true,
        }));
    }

    // Not found — return error matching filesystem stat behavior.
    Err(ioerr(format!(
        "No such file or directory: {}",
        virtual_path
    )))
}

/// The stat a pack entry opened for reading reports.
fn pack_file_stat(mount_table: Option<&MountTable>, virtual_path: &str) -> FsResult<FileStat> {
    match pack_stat(mount_table, virtual_path, false)? {
        StatResult::Single(stat) => Ok(stat),
        StatResult::Recursive(_) => unreachable!("a non-recursive stat is single"),
    }
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// Parse a Node-style open flag.
#[inline]
pub fn parse_open_flag(flag: &str) -> FsResult<OpenFlag> {
    const WRITE_EXCLUSIVE: &str = concat!("w", "x");
    const READ_WRITE_EXCLUSIVE: &str = concat!("w", "x", "+");
    match flag {
        "r" => Ok(OpenFlag::Read),
        "r+" => Ok(OpenFlag::ReadWrite),
        "w" => Ok(OpenFlag::WriteTruncateCreate),
        "w+" => Ok(OpenFlag::ReadWriteTruncateCreate),
        "a" => Ok(OpenFlag::AppendCreate),
        "a+" => Ok(OpenFlag::ReadAppendCreate),
        "ax" => Ok(OpenFlag::AppendExclusive),
        "ax+" => Ok(OpenFlag::ReadAppendExclusive),
        "as" => Ok(OpenFlag::AppendSyncCreate),
        "as+" => Ok(OpenFlag::ReadAppendSyncCreate),
        WRITE_EXCLUSIVE => Ok(OpenFlag::WriteExclusive),
        READ_WRITE_EXCLUSIVE => Ok(OpenFlag::ReadWriteExclusive),
        _ => Err(ioerr(format!("Invalid open flag: {flag}"))),
    }
}

/// Convert OpenFlag to VFS FileOp for permission checking.
#[inline]
fn open_flag_to_vfs_op(flag: &OpenFlag) -> FileOp {
    match flag {
        OpenFlag::Read => FileOp::Read,
        OpenFlag::ReadWrite
        | OpenFlag::ReadWriteTruncateCreate
        | OpenFlag::ReadAppendCreate
        | OpenFlag::ReadAppendExclusive
        | OpenFlag::ReadAppendSyncCreate
        | OpenFlag::ReadWriteExclusive => FileOp::Write,
        OpenFlag::WriteTruncateCreate
        | OpenFlag::AppendCreate
        | OpenFlag::AppendExclusive
        | OpenFlag::AppendSyncCreate
        | OpenFlag::WriteExclusive => FileOp::Create,
    }
}

#[inline]
fn mode_from_append(append: bool) -> WriteMode {
    if append {
        WriteMode::Append
    } else {
        WriteMode::Overwrite
    }
}

/// Map the `durable` flag to a durability level. The JS layer defaults it to
/// `Durable` (crash-safe); callers must explicitly opt into `Fast`.
#[inline]
fn durability_from(durable: bool) -> WriteDurability {
    if durable {
        WriteDurability::Durable
    } else {
        WriteDurability::Fast
    }
}

/// What a write carries, before it is resolved to bytes: the caller's own
/// bytes, which a synchronous call may borrow while its caller is blocked, or
/// bytes produced by encoding a string, which allocates either way.
pub enum Payload<B> {
    Bytes(B),
    Encoded(Vec<u8>),
}

impl<B: Deref<Target = [u8]>> Deref for Payload<B> {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            Payload::Bytes(bytes) => bytes,
            Payload::Encoded(bytes) => bytes,
        }
    }
}

impl<B: Deref<Target = [u8]>> Payload<B> {
    /// Owned bytes, for a call that returns before the write happens.
    pub fn into_owned(self) -> FsResult<Vec<u8>> {
        match self {
            Self::Encoded(bytes) => Ok(bytes),
            Self::Bytes(buf) => {
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(buf.len())
                    .map_err(|err| ioerr(format!("write buffer allocation failed: {err}")))?;
                bytes.extend_from_slice(&buf);
                Ok(bytes)
            }
        }
    }
}

/// Resolve a write's data from the bytes-or-string pair the call received:
/// bytes when there are any, otherwise the string in `encoding` (UTF-8 when
/// none is named).
pub fn payload<B>(
    bytes: Option<B>,
    text: Option<String>,
    encoding: Option<String>,
) -> FsResult<Payload<B>> {
    if let Some(bytes) = bytes {
        Ok(Payload::Bytes(bytes))
    } else if let Some(s) = text {
        let enc = encoding.as_deref().unwrap_or("utf8");
        codec::encode_string(&s, enc)
            .map(Payload::Encoded)
            .map_err(|e| ioerr(e.to_string()))
    } else {
        Err(ioerr("No data provided"))
    }
}

/// A write's data as owned bytes, for a caller that already owns them: its
/// bytes are moved, not copied.
pub fn owned_payload(
    bytes: Option<Vec<u8>>,
    text: Option<String>,
    encoding: Option<String>,
) -> FsResult<Vec<u8>> {
    Ok(match payload(bytes, text, encoding)? {
        Payload::Bytes(bytes) | Payload::Encoded(bytes) => bytes,
    })
}

// ---------------------------------------------------------------------------
// The calls
// ---------------------------------------------------------------------------
//
// Each call has the shape of the op it serves. An `async fn` resolves its path
// when first polled; a `fn` returning `FsResult<impl Future>` does its checks
// and prepares its data now, which is where the op's eager errors come from.

/// Whether `path` names a file or a directory.
pub async fn access(env: FsEnv, path: String) -> FsResult<bool> {
    match env.resolve(&path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            // Pack-backed: check existence via mount table.
            Ok(env
                .mount_table
                .as_deref()
                .map(|m| m.exists_or_is_dir(code_relative(&virtual_path)))
                .unwrap_or(false))
        }
        ResolvedPath::Filesystem(full_path) => {
            let result = run_fs_async(env.scheduler, move || {
                let t0 = Instant::now();
                let r = fs_ops::access(&full_path);
                let disk_ms = t0.elapsed().as_millis() as u64;
                if disk_ms >= 30 {
                    tracing::warn!("[IOTrace] access slow {}ms path={}", disk_ms, path);
                }
                r
            })
            .await?;
            Ok(result.0 || result.1)
        }
    }
}

pub fn access_sync(env: &FsEnv, path: &str) -> FsResult<bool> {
    match env.resolve(path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => Ok(env
            .mount_table
            .as_deref()
            .map(|m| m.exists_or_is_dir(code_relative(&virtual_path)))
            .unwrap_or(false)),
        ResolvedPath::Filesystem(full_path) => {
            run_fs_sync(&env.scheduler, move || fs_ops::access(&full_path))
                .map(|(is_file, is_dir, _size)| is_file || is_dir)
        }
    }
}

/// Write or append a whole file. The path is resolved and the data prepared
/// now; the write happens when the returned future runs.
pub fn write_or_append<F>(
    env: &FsEnv,
    path: &str,
    prepare: F,
    append: bool,
    durable: bool,
) -> FsResult<impl Future<Output = FsResult<bool>> + Send + use<F>>
where
    F: FnOnce() -> FsResult<Vec<u8>>,
{
    let full_path = env.resolve_fs(path, FileOp::Write)?;
    let mode = mode_from_append(append);
    let durability = durability_from(durable);
    let payload = prepare()?;
    let scheduler = Arc::clone(&env.scheduler);
    Ok(async move {
        run_fs_async(scheduler, move || {
            fs_ops::write_file(&full_path, &payload, mode, durability)
        })
        .await
    })
}

pub fn write_or_append_sync<P>(
    env: &FsEnv,
    path: &str,
    prepare: impl FnOnce() -> FsResult<P>,
    append: bool,
    durable: bool,
) -> FsResult<bool>
where
    P: Deref<Target = [u8]> + Send + 'static,
{
    let full_path = env.resolve_fs(path, FileOp::Write)?;
    let mode = mode_from_append(append);
    let durability = durability_from(durable);
    let payload = prepare()?;
    run_fs_sync(&env.scheduler, move || {
        fs_ops::write_file(&full_path, &payload, mode, durability)
    })
}

/// Open a file and answer its descriptor. A `/code` entry inside a pack is
/// materialized to a temporary file first, removed when the descriptor is
/// closed.
pub async fn open(env: FsEnv, path: String, flag: String) -> FsResult<u32> {
    let open_flag = parse_open_flag(&flag)?;
    let vfs_op = open_flag_to_vfs_op(&open_flag);
    let scheduler = env.scheduler;
    let (full_path, cleanup_path, synthetic_stat) = match resolve_path_vfs(
        env.vfs.as_deref(),
        env.mount_table.as_deref(),
        &path,
        vfs_op,
    )? {
        ResolvedPath::Filesystem(p) => (p, None, None),
        ResolvedPath::Pack { virtual_path } => {
            if open_flag != OpenFlag::Read {
                return Err(ioerr(format!(
                    "pack-backed open only supports read mode: {}",
                    virtual_path
                )));
            }
            let mount_table = env
                .mount_table
                .clone()
                .ok_or_else(|| ioerr("mount table not initialized"))?;
            let temp_path = materialize_pack_to_temp_async(
                Arc::clone(&scheduler),
                mount_table,
                virtual_path.clone(),
                ".open",
            )
            .await?;
            let stat = pack_file_stat(env.mount_table.as_deref(), &virtual_path)?;
            (temp_path.clone(), Some(temp_path), Some(stat))
        }
    };
    let domain = scheduler.domain();
    let cleanup_on_error = cleanup_path.clone();
    let cleanup_path_for_open = cleanup_path.map(PathBuf::from);

    let result = run_domain_async(Arc::clone(&scheduler), move || {
        domain.open_file(
            PathBuf::from(full_path).as_path(),
            open_flag,
            cleanup_path_for_open,
            synthetic_stat,
        )
    })
    .await;

    if result.is_err() {
        if let Some(path) = cleanup_on_error {
            scheduler.domain().remove_temp_file(Path::new(&path));
        }
    }
    result
}

pub fn open_sync(env: &FsEnv, path: &str, flag: &str) -> FsResult<u32> {
    let started_at = Instant::now();
    let scheduler = &env.scheduler;
    let open_flag = parse_open_flag(flag)?;
    let vfs_op = open_flag_to_vfs_op(&open_flag);
    let (full_path, cleanup_path, synthetic_stat) = match env.resolve(path, vfs_op)? {
        ResolvedPath::Filesystem(p) => (p, None, None),
        ResolvedPath::Pack { virtual_path } => {
            if open_flag != OpenFlag::Read {
                return Err(ioerr(format!(
                    "pack-backed open only supports read mode: {}",
                    virtual_path
                )));
            }
            let temp_path = materialize_pack_to_temp_checked(
                scheduler,
                env.mount_table.as_deref(),
                &virtual_path,
                ".open",
            )?;
            let stat = pack_file_stat(env.mount_table.as_deref(), &virtual_path)?;
            (temp_path.clone(), Some(temp_path), Some(stat))
        }
    };
    let domain = scheduler.domain();
    let cleanup_on_error = cleanup_path.clone();
    let cleanup_path_for_open = cleanup_path.map(PathBuf::from);

    let result = domain
        .open_file(
            PathBuf::from(full_path).as_path(),
            open_flag,
            cleanup_path_for_open,
            synthetic_stat,
        )
        .map_err(|e| {
            if let Some(path) = cleanup_on_error {
                domain.remove_temp_file(Path::new(&path));
            }
            domain_err(e)
        });

    match &result {
        Ok(rid) => trace_file_edge("open_sync", path, started_at, &format!("rid={rid}")),
        Err(err) => trace_file_edge(
            "open_sync",
            path,
            started_at,
            &format!("err={}", err.message),
        ),
    }

    result
}

/// Close a descriptor. The same work whether awaited or not: the file table
/// is in memory, and closing a materialized pack entry removes its file.
pub fn close(env: &FsEnv, rid: FileId) -> FsResult<()> {
    env.scheduler.domain().close_file(rid).map_err(domain_err)
}

pub async fn copy(env: FsEnv, src_path: String, dest_path: String) -> FsResult<()> {
    let src_resolved = env.resolve(&src_path, FileOp::Read)?;
    let dest_full = env.resolve_fs(&dest_path, FileOp::Create)?;
    match src_resolved {
        ResolvedPath::Filesystem(src_full) => {
            run_fs_async(env.scheduler, move || fs_ops::copy(&src_full, &dest_full)).await
        }
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = Arc::clone(env.mounts()?);
            let rel = code_relative(&virtual_path).to_string();
            copy_pack_file_async(env.scheduler, mount_table, rel, PathBuf::from(dest_full)).await
        }
    }
}

pub fn copy_sync(env: &FsEnv, src_path: &str, dest_path: &str) -> FsResult<()> {
    let src_resolved = env.resolve(src_path, FileOp::Read)?;
    let dest_full = env.resolve_fs(dest_path, FileOp::Create)?;
    match src_resolved {
        ResolvedPath::Filesystem(src_full) => {
            run_fs_sync(&env.scheduler, move || fs_ops::copy(&src_full, &dest_full))
        }
        ResolvedPath::Pack { virtual_path } => {
            let rel = code_relative(&virtual_path);
            fs_ops::copy_mount_entry_to_path(env.mounts()?, rel, Path::new(&dest_full))
                .map_err(engine_err)
        }
    }
}

/// A descriptor's stat. In-memory, like [`close`].
pub fn fstat(env: &FsEnv, rid: FileId) -> FsResult<FileStat> {
    env.scheduler.domain().fstat(rid).map_err(domain_err)
}

pub fn ftruncate(env: &FsEnv, rid: FileId, len: u64) -> FsResult<()> {
    env.scheduler
        .domain()
        .ftruncate(rid, len)
        .map_err(domain_err)
}

pub async fn mkdir(env: FsEnv, dir_path: String, recursive: bool) -> FsResult<()> {
    let full_path = env.resolve_fs(&dir_path, FileOp::Create)?;
    run_fs_async(env.scheduler, move || fs_ops::mkdir(&full_path, recursive)).await
}

pub fn mkdir_sync(env: &FsEnv, dir_path: &str, recursive: bool) -> FsResult<()> {
    let full_path = env.resolve_fs(dir_path, FileOp::Create)?;
    run_fs_sync(&env.scheduler, move || fs_ops::mkdir(&full_path, recursive))
}

/// The names in a `/code` directory that lives in a pack.
fn pack_readdir(env: &FsEnv, virtual_path: &str) -> FsResult<Vec<String>> {
    let m = env.mounts()?;
    let rel = code_relative(virtual_path);
    if !m.exists_or_is_dir(rel) {
        return Err(ioerr(format!(
            "No such file or directory: {}",
            virtual_path
        )));
    }
    if m.is_file(rel) {
        return Err(ioerr(format!("Not a directory: {}", virtual_path)));
    }
    Ok(m.list_dir(rel))
}

pub async fn readdir(env: FsEnv, dir_path: String) -> FsResult<Vec<String>> {
    match env.resolve(&dir_path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => pack_readdir(&env, &virtual_path),
        ResolvedPath::Filesystem(full_path) => {
            run_fs_async(env.scheduler, move || fs_ops::readdir(&full_path)).await
        }
    }
}

pub fn readdir_sync(env: &FsEnv, dir_path: &str) -> FsResult<Vec<String>> {
    match env.resolve(dir_path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => pack_readdir(env, &virtual_path),
        ResolvedPath::Filesystem(full_path) => {
            run_fs_sync(&env.scheduler, move || fs_ops::readdir(&full_path))
        }
    }
}

pub async fn unlink(env: FsEnv, file_path: String) -> FsResult<()> {
    let full_path = env.resolve_fs(&file_path, FileOp::Delete)?;
    run_fs_async(env.scheduler, move || fs_ops::unlink(&full_path)).await
}

pub fn unlink_sync(env: &FsEnv, file_path: &str) -> FsResult<()> {
    let full_path = env.resolve_fs(file_path, FileOp::Delete)?;
    run_fs_sync(&env.scheduler, move || fs_ops::unlink(&full_path))
}

/// Rename: delete on the source, create on the destination.
pub async fn rename(env: FsEnv, old_path: String, new_path: String) -> FsResult<()> {
    let old_full = env.resolve_fs(&old_path, FileOp::Delete)?;
    let new_full = env.resolve_fs(&new_path, FileOp::Create)?;
    run_fs_async(env.scheduler, move || fs_ops::rename(&old_full, &new_full)).await
}

pub fn rename_sync(env: &FsEnv, old_path: &str, new_path: &str) -> FsResult<()> {
    let old_full = env.resolve_fs(old_path, FileOp::Delete)?;
    let new_full = env.resolve_fs(new_path, FileOp::Create)?;
    run_fs_sync(&env.scheduler, move || fs_ops::rename(&old_full, &new_full))
}

pub async fn rmdir(env: FsEnv, dir_path: String, recursive: bool) -> FsResult<()> {
    let full_path = env.resolve_fs(&dir_path, FileOp::Delete)?;
    run_fs_async(env.scheduler, move || fs_ops::rmdir(&full_path, recursive)).await
}

pub fn rmdir_sync(env: &FsEnv, dir_path: &str, recursive: bool) -> FsResult<()> {
    let full_path = env.resolve_fs(dir_path, FileOp::Delete)?;
    run_fs_sync(&env.scheduler, move || fs_ops::rmdir(&full_path, recursive))
}

pub async fn stat(env: FsEnv, path: String, recursive: bool) -> FsResult<StatResult> {
    match env.resolve(&path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            pack_stat(env.mount_table.as_deref(), &virtual_path, recursive)
        }
        ResolvedPath::Filesystem(full_path) => {
            run_fs_async(env.scheduler, move || fs_ops::stat(&full_path, recursive)).await
        }
    }
}

pub fn stat_sync(env: &FsEnv, path: &str, recursive: bool) -> FsResult<StatResult> {
    match env.resolve(path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            pack_stat(env.mount_table.as_deref(), &virtual_path, recursive)
        }
        ResolvedPath::Filesystem(full_path) => {
            run_fs_sync(&env.scheduler, move || fs_ops::stat(&full_path, recursive))
        }
    }
}

/// Write to a descriptor at `position`, or at its cursor. The data is prepared
/// now; the write happens when the returned future runs.
pub fn write_fd<F>(
    env: &FsEnv,
    rid: FileId,
    prepare: F,
    position: Option<u64>,
) -> FsResult<impl Future<Output = FsResult<usize>> + Send + use<F>>
where
    F: FnOnce() -> FsResult<Vec<u8>>,
{
    let scheduler = Arc::clone(&env.scheduler);
    let domain = scheduler.domain();
    let payload = prepare()?;
    Ok(async move {
        run_domain_async(scheduler, move || {
            domain.write_file(rid, &payload, position)
        })
        .await
    })
}

pub fn write_fd_sync<P>(
    env: &FsEnv,
    rid: FileId,
    prepare: impl FnOnce() -> FsResult<P>,
    position: Option<u64>,
) -> FsResult<usize>
where
    P: Deref<Target = [u8]>,
{
    let domain = env.scheduler.domain();
    let payload = prepare()?;
    domain
        .write_file(rid, &payload, position)
        .map_err(domain_err)
}

/// Check a pack read against the read limit, and the scheduler's size hint for
/// it. Taken once: the limit check and the hint both want the entry size, and
/// a pack entry size is a hashmap lookup.
fn pack_read_plan(
    mount_table: &MountTable,
    rel: &str,
    position: Option<u64>,
    length: Option<u64>,
) -> FsResult<Option<u64>> {
    let max_len = shared::protocol::io_cmd::MAX_READ_LENGTH;
    // Reject if explicit length > MAX_READ_LENGTH.
    if let Some(len) = length {
        if len > max_len {
            return Err(ioerr(format!(
                "read length {} exceeds limit {}",
                len, max_len,
            )));
        }
    }
    let entry_size = mount_table.entry_size(rel);
    // When length is not specified, check the effective read size
    // (entry_size - position) against the limit.  This prevents
    // unbounded reads from oversized pack entries regardless of
    // whether position is specified.
    if length.is_none() {
        if let Some(entry_sz) = entry_size {
            let effective = entry_sz.saturating_sub(position.unwrap_or(0));
            if effective > max_len {
                return Err(ioerr(format!(
                    "file size {} exceeds limit {}",
                    effective, max_len,
                )));
            }
        }
    }
    Ok(entry_size.map(|sz| sz.saturating_sub(position.unwrap_or(0))))
}

/// Read a file, whole or a range of it.
pub async fn read_file(
    env: FsEnv,
    path: String,
    position: Option<u64>,
    length: Option<u64>,
) -> FsResult<Vec<u8>> {
    let request_kind = RequestKind::Async;
    match env.resolve(&path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = Arc::clone(env.mounts()?);
            let rel = code_relative(&virtual_path).to_string();
            let size_hint = pack_read_plan(&mount_table, &rel, position, length)?;
            let request = read_request(BackendKind::Pack, request_kind, length, size_hint);
            let max_len = shared::protocol::io_cmd::MAX_READ_LENGTH;
            env.scheduler
                .run_async(request, move || {
                    mount_table.read_range_limited(&rel, position.unwrap_or(0), length, max_len)
                })
                .await
                .map_err(pool_err)?
                .map_err(|e| ioerr(format!("pack read failed: {e}")))
        }
        ResolvedPath::Filesystem(full_path) => {
            let allow_mmap = is_read_only_code_path(&path);
            // No size hint on the async path: see `fs_read_size_hint`.
            let request = read_request(BackendKind::Filesystem, request_kind, length, None);
            env.scheduler
                .run_async(request, move || {
                    let t0 = Instant::now();
                    let r = fs_ops::read_file(&full_path, position, length, allow_mmap);
                    let disk_ms = t0.elapsed().as_millis() as u64;
                    if let Ok(ref d) = r {
                        if disk_ms >= 30 {
                            tracing::warn!(
                                "[IOTrace] read slow {}ms size={}B path={}",
                                disk_ms,
                                d.len(),
                                path
                            );
                        }
                    }
                    r
                })
                .await
                .map_err(pool_err)?
                .map_err(engine_err)
        }
    }
}

pub fn read_file_sync(
    env: &FsEnv,
    path: &str,
    position: Option<u64>,
    length: Option<u64>,
) -> FsResult<Vec<u8>> {
    let started_at = Instant::now();
    let scheduler = &env.scheduler;
    let request_kind = RequestKind::Sync;
    let result: FsResult<Vec<u8>> = match env.resolve(path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = Arc::clone(env.mounts()?);
            let rel = code_relative(&virtual_path).to_string();
            let size_hint = pack_read_plan(&mount_table, &rel, position, length)?;
            let request = read_request(BackendKind::Pack, request_kind, length, size_hint);
            let max_len = shared::protocol::io_cmd::MAX_READ_LENGTH;
            scheduler
                .run_sync(&request, move || {
                    mount_table.read_range_limited(&rel, position.unwrap_or(0), length, max_len)
                })
                .map_err(pool_err)?
                .map_err(|e| ioerr(format!("pack read failed: {e}")))
        }
        ResolvedPath::Filesystem(full_path) => {
            let allow_mmap = is_read_only_code_path(path);
            // Skip the stat when `length` alone already classifies the
            // read as cheap — it could only confirm what we know.
            let size_hint = match length {
                Some(len) if len <= scheduler.policy().small_read_bytes as u64 => None,
                _ => fs_read_size_hint(&full_path),
            };
            let request = read_request(BackendKind::Filesystem, request_kind, length, size_hint);
            scheduler
                .run_sync(&request, move || {
                    fs_ops::read_file(&full_path, position, length, allow_mmap)
                })
                .map_err(pool_err)?
                .map_err(engine_err)
        }
    };

    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => {
            trace_file_edge("read_file_sync", path, started_at, "ok");
            if elapsed_ms >= 30 {
                // Sync reads block the caller; surface the path so callers can
                // see which game-side readFileSync is freezing the event loop.
                tracing::warn!("[IOTrace] readFileSync slow {}ms path={}", elapsed_ms, path);
            }
        }
        Err(err) => trace_file_edge(
            "read_file_sync",
            path,
            started_at,
            &format!("err={}", err.message),
        ),
    }

    result
}

/// Read up to `length` bytes from a descriptor.
pub async fn read_fd(
    env: FsEnv,
    rid: FileId,
    length: u64,
    position: Option<u64>,
) -> FsResult<Vec<u8>> {
    let domain = env.scheduler.domain();
    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Async,
        Some(length),
        None,
    );
    env.scheduler
        .run_async(request, move || domain.read_file(rid, length, position))
        .await
        .map_err(pool_err)?
        .map_err(domain_err)
}

pub fn read_fd_sync(
    env: &FsEnv,
    rid: FileId,
    length: u64,
    position: Option<u64>,
) -> FsResult<Vec<u8>> {
    let started_at = Instant::now();
    let domain = env.scheduler.domain();
    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Sync,
        Some(length),
        None,
    );

    let result = env
        .scheduler
        .run_sync(&request, move || domain.read_file(rid, length, position))
        .map_err(pool_err)?
        .map_err(domain_err);

    let target = format!("rid={rid}");
    match &result {
        Ok(_) => trace_file_edge("read_fd_sync", &target, started_at, "ok"),
        Err(err) => trace_file_edge(
            "read_fd_sync",
            &target,
            started_at,
            &format!("err={}", err.message),
        ),
    }

    result
}

/// Read up to `len` bytes from a descriptor into storage staged on the worker:
/// the caller copies the filled prefix into its own buffer when it completes,
/// so no caller memory is held while the read is in flight.
pub async fn read_fd_for_buffer(
    env: FsEnv,
    rid: FileId,
    len: usize,
    position: Option<u64>,
) -> FsResult<fs_ops::OwnedFileRead> {
    let domain = env.scheduler.domain();
    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Async,
        Some(len as u64),
        None,
    );
    env.scheduler
        .run_async(request, move || {
            // Storage is already boxed on the worker without a shrink allocation.
            domain.read_file_for_buffer(rid, len, position)
        })
        .await
        .map_err(pool_err)?
        .map_err(domain_err)
}

/// [`read_fd_for_buffer`] for a caller that is blocked until it returns and
/// whose buffer is elsewhere -- in another process -- so the filled bytes go
/// back to it instead of into it.
pub fn read_fd_for_buffer_sync(
    env: &FsEnv,
    rid: FileId,
    len: usize,
    position: Option<u64>,
) -> FsResult<fs_ops::OwnedFileRead> {
    let domain = env.scheduler.domain();
    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Sync,
        Some(len as u64),
        None,
    );
    env.scheduler
        .run_sync(&request, move || {
            domain.read_file_for_buffer(rid, len, position)
        })
        .map_err(pool_err)?
        .map_err(domain_err)
}

/// Read from a descriptor directly into `buf`. For a caller that is blocked
/// until this returns, so the buffer is not touched by anyone else meanwhile.
pub fn read_fd_into_sync<B>(
    env: &FsEnv,
    rid: FileId,
    mut buf: B,
    position: Option<u64>,
) -> FsResult<usize>
where
    B: AsMut<[u8]> + Send + 'static,
{
    let domain = env.scheduler.domain();
    let len = buf.as_mut().len() as u64;
    let request = read_request(BackendKind::Filesystem, RequestKind::Sync, Some(len), None);
    env.scheduler
        .run_sync(&request, move || {
            domain.read_file_into(rid, buf.as_mut(), position)
        })
        .map_err(pool_err)?
        .map_err(domain_err)
}

/// Read and Brotli-decompress a file.
pub async fn read_compressed(env: FsEnv, path: String) -> FsResult<Vec<u8>> {
    match env.resolve(&path, FileOp::Read)? {
        ResolvedPath::Filesystem(full_path) => {
            run_fs_async(env.scheduler, move || {
                fs_ops::read_compressed_file(&full_path, None)
            })
            .await
        }
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = Arc::clone(env.mounts()?);
            run_pack_async(env.scheduler, pack_whole_read_request(), move || {
                // Keep the complete pack read/decompress chain on the Pack
                // worker. Re-resolve after queueing so remounts are honored.
                let virtual_path = reresolve_pack(&mount_table, &virtual_path)?;
                let data = read_pack_bytes(Some(mount_table.as_ref()), &virtual_path)
                    .map_err(|e| EngineError::new(ErrorCode::IoError).with_detail(e.message))?;
                fs_ops::read_compressed_file(&virtual_path, Some(data))
            })
            .await
        }
    }
}

pub fn read_compressed_sync(env: &FsEnv, path: &str) -> FsResult<Vec<u8>> {
    let (full_path, pack_data) = match env.resolve(path, FileOp::Read)? {
        ResolvedPath::Filesystem(p) => (p, None),
        ResolvedPath::Pack { virtual_path } => {
            let data = read_pack_bytes(env.mount_table.as_deref(), &virtual_path)?;
            (virtual_path, Some(data))
        }
    };
    run_fs_sync(&env.scheduler, move || {
        fs_ops::read_compressed_file(&full_path, pack_data)
    })
}

/// Re-resolve a `/code` path on the worker that is about to read it, so a
/// remount that happened while the job queued is honored.
fn reresolve_pack(mount_table: &MountTable, virtual_path: &str) -> Result<String, EngineError> {
    let resolved = resolve_path_vfs(None, Some(mount_table), virtual_path, FileOp::Read)
        .map_err(|e| EngineError::new(ErrorCode::IoError).with_detail(e.message))?;
    let ResolvedPath::Pack { virtual_path } = resolved else {
        return Err(EngineError::new(ErrorCode::IoError)
            .with_detail("pack path resolved to a filesystem path"));
    };
    Ok(virtual_path)
}

/// Read named entries out of a zip archive, each as text or bytes, in the
/// archive's own order.
pub async fn read_zip_entry(
    env: FsEnv,
    zip_path: String,
    entries_json: String,
) -> FsResult<Vec<ZipEntryResult>> {
    let scheduler = env.scheduler;
    let (full_path, cleanup_path) = match resolve_path_vfs(
        env.vfs.as_deref(),
        env.mount_table.as_deref(),
        &zip_path,
        FileOp::Read,
    )? {
        ResolvedPath::Filesystem(p) => (p, None),
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = env
                .mount_table
                .clone()
                .ok_or_else(|| ioerr("mount table not initialized"))?;
            let temp_path = materialize_pack_to_temp_async(
                Arc::clone(&scheduler),
                mount_table,
                virtual_path,
                ".zip",
            )
            .await?;
            scheduler
                .domain()
                .register_temp_file(PathBuf::from(temp_path.clone()));
            (temp_path.clone(), Some(temp_path))
        }
    };

    let results = scheduler
        .run_async(archive_read_request(), move || {
            fs_ops::read_zip_entry(&full_path, &entries_json, None)
        })
        .await;

    if let Some(path) = &cleanup_path {
        scheduler.domain().remove_temp_file(Path::new(path));
    }

    results.map_err(pool_err)?.map_err(engine_err)
}

/// Extract a zip archive into a directory: through the platform's own unzip
/// when it has one (Android's `java.util.zip`), otherwise the `zip` crate on
/// the archive pool.
pub async fn unzip(
    env: FsEnv,
    zip_file_path: String,
    target_path: String,
    file_service: Option<Arc<dyn FileService>>,
) -> FsResult<()> {
    let scheduler = env.scheduler;
    let (full_zip_path, cleanup_path) = match resolve_path_vfs(
        env.vfs.as_deref(),
        env.mount_table.as_deref(),
        &zip_file_path,
        FileOp::Read,
    )? {
        ResolvedPath::Filesystem(p) => (p, None),
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = env
                .mount_table
                .clone()
                .ok_or_else(|| ioerr("mount table not initialized"))?;
            let temp_path = materialize_pack_to_temp_async(
                Arc::clone(&scheduler),
                mount_table,
                virtual_path,
                ".unzip",
            )
            .await?;
            scheduler
                .domain()
                .register_temp_file(PathBuf::from(temp_path.clone()));
            (temp_path.clone(), Some(temp_path))
        }
    };
    let full_dest_dir = require_fs_path(resolve_path_vfs(
        env.vfs.as_deref(),
        env.mount_table.as_deref(),
        &target_path,
        FileOp::Write,
    )?)?;

    // Platform-specific unzip (e.g. Android JNI)
    if let Some(svc) = file_service {
        let zip = full_zip_path.clone();
        let dest = full_dest_dir.clone();
        let compressed_bytes = std::fs::metadata(&zip)
            .map(|meta| meta.len() as usize)
            .unwrap_or(0);
        let result = scheduler
            .run_async(
                IoRequest::Unzip {
                    backend: BackendKind::Archive,
                    priority: PriorityClass::Background,
                    compressed_bytes,
                },
                move || svc.unzip(&zip, &dest),
            )
            .await
            .map_err(pool_err)?
            .map(|_| ())
            .map_err(|e| ioerr(e.to_string()));
        if let Some(path) = cleanup_path {
            scheduler.domain().remove_temp_file(Path::new(&path));
        }
        return result;
    }

    let result = migo_io::extract_zip_with_scheduler(
        Arc::clone(&scheduler),
        PathBuf::from(&full_zip_path),
        PathBuf::from(&full_dest_dir),
        None,
    )
    .await
    .map(|_| ())
    .map_err(|e| engine_err(EngineError::new(ErrorCode::IoError).with_detail(e.to_string())));

    if let Some(path) = cleanup_path {
        scheduler.domain().remove_temp_file(Path::new(&path));
    }
    result
}

/// A file's size and digest (`md5`, `sha1`, `sha256`).
pub async fn get_file_info(env: FsEnv, path: String, algorithm: String) -> FsResult<(u64, String)> {
    match env.resolve(&path, FileOp::Read)? {
        ResolvedPath::Filesystem(full_path) => {
            run_fs_async(env.scheduler, move || {
                fs_ops::get_file_info(&full_path, &algorithm, None)
            })
            .await
        }
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = Arc::clone(env.mounts()?);
            run_pack_async(env.scheduler, pack_whole_read_request(), move || {
                // Re-resolve the mount on the Pack lane. Digesting an entry
                // opens, decompresses, and hashes every chunk; none of that
                // belongs on the caller's thread.
                let virtual_path = reresolve_pack(&mount_table, &virtual_path)?;
                mount_table
                    .get_file_info(code_relative(&virtual_path), &algorithm)
                    .map_err(|e| {
                        EngineError::new(ErrorCode::IoError)
                            .with_detail(format!("pack getFileInfo failed: {e}"))
                    })
            })
            .await
        }
    }
}

pub fn get_file_info_sync(env: &FsEnv, path: &str, algorithm: &str) -> FsResult<(u64, String)> {
    match env.resolve(path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => env
            .mounts()?
            .get_file_info(code_relative(&virtual_path), algorithm)
            .map_err(|e| ioerr(format!("pack getFileInfo failed: {e}"))),
        ResolvedPath::Filesystem(full_path) => {
            let algorithm = algorithm.to_string();
            run_fs_sync(&env.scheduler, move || {
                fs_ops::get_file_info(&full_path, &algorithm, None)
            })
        }
    }
}

/// The files `saveFile` put under `dir` whose names start with `prefix`.
pub async fn list_saved_files(
    env: FsEnv,
    dir: String,
    prefix: String,
) -> FsResult<Vec<SavedFileInfo>> {
    let full_dir = env.resolve_fs(&dir, FileOp::Read)?;
    run_fs_async(env.scheduler, move || {
        fs_ops::list_saved_files(&full_dir, &prefix, &dir)
    })
    .await
}

#[cfg(test)]
#[path = "fs_tests.rs"]
mod tests;
