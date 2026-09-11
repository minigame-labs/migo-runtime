use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc, time::Instant};

use deno_core::{JsBuffer, OpState, ToJsBuffer, op2};
use migo_io::fs_ops;
use shared::{
    codec,
    error::{EngineError, ErrorCode},
    op_state::HostOpState,
    protocol::io_cmd::{
        FileId, FileStat, OpenFlag, SavedFileInfo, StatResult, WriteDurability, WriteMode,
    },
    vfs::{FileOp, VfsError, VirtualFS},
};

use crate::io_state::IoSchedulerState;
use migo_io::{
    domain::DomainError,
    pools::PoolError,
    scheduler::IoScheduler,
    task::{BackendKind, IoRequest, PriorityClass, RequestKind},
};
use shared::protocol::io_cmd::ZipEntryData;

const MAX_MATERIALIZE_LENGTH: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum IOError {
    #[class("IOError")]
    #[error("{0}")]
    Message(String),
}

impl From<&str> for IOError {
    #[inline]
    fn from(value: &str) -> Self {
        IOError::Message(value.to_string())
    }
}

impl From<String> for IOError {
    #[inline]
    fn from(value: String) -> Self {
        IOError::Message(value)
    }
}

impl From<ErrorCode> for IOError {
    #[inline]
    fn from(e: ErrorCode) -> Self {
        IOError::Message(e.default_message().to_string())
    }
}

impl From<EngineError> for IOError {
    #[inline]
    fn from(e: EngineError) -> Self {
        match &e.detail {
            Some(d) => IOError::Message(format!("[{:?}] {} ({})", e.code, e.msg, d)),
            None => IOError::Message(format!("[{:?}] {}", e.code, e.msg)),
        }
    }
}

#[inline]
fn ioerr(msg: impl Into<String>) -> IOError {
    IOError::Message(msg.into())
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

#[inline]
fn get_scheduler(state: &OpState) -> Arc<IoScheduler> {
    state.borrow::<IoSchedulerState>().0.clone()
}

#[inline]
fn domain_err(err: DomainError) -> IOError {
    match err {
        DomainError::Closed => ioerr("IO domain closed"),
        DomainError::Io(err) => IOError::from(err),
    }
}

#[inline]
fn pool_err(err: PoolError) -> IOError {
    match err {
        PoolError::Closed => ioerr("IO worker pool closed"),
    }
}

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
fn read_request(
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
/// on the V8 thread for no gain.
#[inline]
fn fs_read_size_hint(path: &str) -> Option<u64> {
    std::fs::metadata(path).ok().map(|meta| meta.len())
}

#[inline]
fn archive_read_request() -> IoRequest {
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
async fn run_fs_async<T, F>(scheduler: Arc<IoScheduler>, job: F) -> Result<T, IOError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, EngineError> + Send + 'static,
{
    scheduler
        .run_async(fs_op_request(RequestKind::Async), job)
        .await
        .map_err(pool_err)?
        .map_err(IOError::from)
}

/// Run a blocking file-table operation through the bounded filesystem class.
/// Keep `DomainError` distinct until the final flattening so closed-domain and
/// filesystem errors retain their existing JS-visible messages.
async fn run_domain_async<T, F>(scheduler: Arc<IoScheduler>, job: F) -> Result<T, IOError>
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
/// calling (V8) thread exactly as before — but now behind the scheduler's
/// domain-close guard and metrics.
fn run_fs_sync<T, F>(scheduler: &IoScheduler, job: F) -> Result<T, IOError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, EngineError> + Send + 'static,
{
    scheduler
        .run_sync(&fs_op_request(RequestKind::Sync), job)
        .map_err(pool_err)?
        .map_err(IOError::from)
}

async fn copy_pack_file_async(
    scheduler: Arc<IoScheduler>,
    mount_table: Arc<shared::vfs::MountTable>,
    relative_path: String,
    dest_path: PathBuf,
) -> Result<(), IOError> {
    let request = copy_request(BackendKind::Pack, RequestKind::Async);
    scheduler
        .run_async(request, move || {
            fs_ops::copy_mount_entry_to_path(&mount_table, &relative_path, &dest_path)
        })
        .await
        .map_err(pool_err)?
        .map_err(IOError::from)
}

/// Get VFS + MountTable from async op state.
#[inline]
fn get_vfs_async(
    state: &Rc<RefCell<OpState>>,
) -> (Option<Arc<VirtualFS>>, Option<Arc<shared::vfs::MountTable>>) {
    let st = state.borrow();
    let host = st.borrow::<HostOpState>();
    (host.vfs.clone(), host.mount_table.clone())
}

/// Get VFS + MountTable from sync op state.
#[inline]
fn get_vfs_sync(state: &OpState) -> (Option<Arc<VirtualFS>>, Option<Arc<shared::vfs::MountTable>>) {
    let host = state.borrow::<HostOpState>();
    (host.vfs.clone(), host.mount_table.clone())
}

/// Result of path resolution: either a real filesystem path for IO-thread
/// operations, or a virtual path backed by a package (data read inline).
enum ResolvedPath {
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
fn resolve_path_vfs(
    vfs: Option<&VirtualFS>,
    mount_table: Option<&shared::vfs::MountTable>,
    path: &str,
    op: FileOp,
) -> Result<ResolvedPath, IOError> {
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

/// Extract a filesystem path from a resolved path, returning an error for
/// pack-backed paths.  Used by ops that haven't been adapted for pack reads.
#[inline]
fn require_fs_path(resolved: ResolvedPath) -> Result<String, IOError> {
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
fn code_relative(virtual_path: &str) -> &str {
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
fn read_pack_bytes(
    mount_table: Option<&shared::vfs::MountTable>,
    virtual_path: &str,
) -> Result<Vec<u8>, IOError> {
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
    mount_table: Option<&shared::vfs::MountTable>,
    virtual_path: &str,
    suffix: &str,
) -> Result<String, IOError> {
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
        .map_err(IOError::from)
}

fn materialize_pack_to_temp_checked(
    scheduler: &IoScheduler,
    mount_table: Option<&shared::vfs::MountTable>,
    virtual_path: &str,
    suffix: &str,
) -> Result<String, IOError> {
    scheduler.ensure_open().map_err(pool_err)?;
    materialize_pack_to_temp(mount_table, virtual_path, suffix)
}

async fn materialize_pack_to_temp_async(
    scheduler: Arc<IoScheduler>,
    mount_table: Arc<shared::vfs::MountTable>,
    virtual_path: String,
    suffix: &'static str,
) -> Result<String, IOError> {
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
        .map_err(IOError::from)
}

/// Construct a StatResult for a pack-backed /code path.
/// Returns IOError for not-found, matching filesystem stat behavior.
fn pack_stat(
    mount_table: Option<&shared::vfs::MountTable>,
    virtual_path: &str,
    recursive: bool,
) -> Result<StatResult, IOError> {
    use shared::protocol::io_cmd::{FileStat, StatEntry, StatResult};
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

// pack_get_file_info removed — pack getFileInfo now routes through IO thread
// with pack_data to compute real digests (md5/sha1/sha256).

#[inline]
fn parse_open_flag(flag: &str) -> Result<OpenFlag, IOError> {
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

/// Map the JS `durable` flag to a durability level. Defaults to `Durable`
/// (crash-safe) — callers must explicitly opt into `Fast`.
#[inline]
fn durability_from(durable: bool) -> WriteDurability {
    if durable {
        WriteDurability::Durable
    } else {
        WriteDurability::Fast
    }
}

/// Validate backing-store metadata before creating any Rust byte slice.
/// `#[buffer]` uses deno_core's direct converter, which bypasses serde_v8's
/// shared/resizable rejection. Even JsBuffer::len() dereferences the bytes.
fn validate_file_buffer(buf: JsBuffer) -> Result<JsBuffer, IOError> {
    let slice = buf.into_parts();
    let (store, _) = slice.clone().into_parts();
    if store.is_shared() || store.is_resizable_by_user_javascript() {
        return Err(ioerr("file IO requires a fixed, nonshared buffer"));
    }
    Ok(JsBuffer::from_parts(slice))
}

/// Sync calls may borrow validated JS bytes while V8 is blocked. Async
/// calls must turn this payload into owned bytes before returning a future.
enum WritePayload {
    /// Validated V8 bytes, borrowed only while the isolate is blocked.
    Js(JsBuffer),
    /// Bytes produced by string encoding, which allocates either way.
    Encoded(Vec<u8>),
}

impl std::ops::Deref for WritePayload {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            WritePayload::Js(buf) => buf,
            WritePayload::Encoded(bytes) => bytes,
        }
    }
}

impl WritePayload {
    fn into_owned(self) -> Result<Vec<u8>, IOError> {
        match self {
            Self::Encoded(bytes) => Ok(bytes),
            Self::Js(buf) => {
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

/// Resolve the write payload from the buffer/string pair the op received.
fn prepare_data(
    data_buf: Option<JsBuffer>,
    data_str: Option<String>,
    encoding: Option<String>,
) -> Result<WritePayload, IOError> {
    if let Some(js_buf) = data_buf {
        validate_file_buffer(js_buf).map(WritePayload::Js)
    } else if let Some(s) = data_str {
        let enc = encoding.as_deref().unwrap_or("utf8");
        codec::encode_string(&s, enc)
            .map(WritePayload::Encoded)
            .map_err(|e| ioerr(e.to_string()))
    } else {
        Err(ioerr("No data provided"))
    }
}

//
// Access - check if path exists (uses VFS)
//
#[op2(async(lazy), fast)]
pub async fn op_access(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
) -> Result<bool, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            // Pack-backed: check existence via mount table.
            Ok(mt
                .as_deref()
                .map(|m| {
                    let rel = code_relative(&virtual_path);
                    m.exists_or_is_dir(rel)
                })
                .unwrap_or(false))
        }
        ResolvedPath::Filesystem(full_path) => {
            let vpath = path.clone();
            let result = run_fs_async(scheduler, move || {
                let t0 = std::time::Instant::now();
                let r = fs_ops::access(&full_path);
                let disk_ms = t0.elapsed().as_millis() as u64;
                if disk_ms >= 30 {
                    tracing::warn!("[IOTrace] access slow {}ms path={}", disk_ms, vpath);
                }
                r
            })
            .await?;
            Ok(result.0 || result.1)
        }
    }
}

#[op2(fast)]
pub fn op_access_sync(state: &mut OpState, #[string] path: String) -> Result<bool, IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => Ok(mt
            .as_deref()
            .map(|m| {
                let rel = code_relative(&virtual_path);
                m.exists_or_is_dir(rel)
            })
            .unwrap_or(false)),
        ResolvedPath::Filesystem(full_path) => {
            let scheduler = get_scheduler(state);
            run_fs_sync(&scheduler, move || fs_ops::access(&full_path))
                .map(|(is_file, is_dir, _size)| is_file || is_dir)
        }
    }
}

//
// Write / append (path) - uses VFS with Write/Create permission
//
#[op2(async(lazy))]
pub fn op_write_or_append_file(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[buffer] data_buf: Option<JsBuffer>,
    #[string] data_str: Option<String>,
    #[string] encoding: Option<String>,
    append: bool,
    durable: bool,
) -> Result<impl std::future::Future<Output = Result<bool, IOError>>, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let full_path = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &path,
        FileOp::Write,
    )?)?;
    let mode = mode_from_append(append);
    let durability = durability_from(durable);
    let payload = prepare_data(data_buf, data_str, encoding)?.into_owned()?;

    Ok(async move {
        run_fs_async(scheduler, move || {
            fs_ops::write_file(&full_path, &payload, mode, durability)
        })
        .await
    })
}

#[op2]
pub fn op_write_or_append_file_sync(
    state: &mut OpState,
    #[string] path: String,
    #[buffer] data_buf: Option<JsBuffer>,
    #[string] data_str: Option<String>,
    #[string] encoding: Option<String>,
    append: bool,
    durable: bool,
) -> Result<bool, IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let full_path = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &path,
        FileOp::Write,
    )?)?;
    let mode = mode_from_append(append);
    let durability = durability_from(durable);
    let payload = prepare_data(data_buf, data_str, encoding)?;

    run_fs_sync(&scheduler, move || {
        fs_ops::write_file(&full_path, &payload, mode, durability)
    })
}

//
// Open / close - uses VFS with appropriate permission based on open flag
//
#[op2(async(lazy), fast)]
pub async fn op_open_file(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[string] flag: String,
) -> Result<u32, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let open_flag = parse_open_flag(&flag)?;
    let vfs_op = open_flag_to_vfs_op(&open_flag);
    let (full_path, cleanup_path, synthetic_stat) =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, vfs_op)? {
            ResolvedPath::Filesystem(p) => (p, None, None),
            ResolvedPath::Pack { virtual_path } => {
                if open_flag != OpenFlag::Read {
                    return Err(ioerr(format!(
                        "pack-backed open only supports read mode: {}",
                        virtual_path
                    )));
                }
                let mount_table = mt
                    .clone()
                    .ok_or_else(|| ioerr("mount table not initialized"))?;
                let temp_path = materialize_pack_to_temp_async(
                    Arc::clone(&scheduler),
                    mount_table,
                    virtual_path.clone(),
                    ".open",
                )
                .await?;
                let stat = match pack_stat(mt.as_deref(), &virtual_path, false)? {
                    StatResult::Single(stat) => stat,
                    StatResult::Recursive(_) => unreachable!(),
                };
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
            scheduler
                .domain()
                .remove_temp_file(std::path::Path::new(&path));
        }
    }
    result
}

#[op2(fast)]
pub fn op_open_file_sync(
    state: &mut OpState,
    #[string] path: String,
    #[string] flag: String,
) -> Result<u32, IOError> {
    let started_at = Instant::now();
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let open_flag = parse_open_flag(&flag)?;
    let vfs_op = open_flag_to_vfs_op(&open_flag);
    let (full_path, cleanup_path, synthetic_stat) =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, vfs_op)? {
            ResolvedPath::Filesystem(p) => (p, None, None),
            ResolvedPath::Pack { virtual_path } => {
                if open_flag != OpenFlag::Read {
                    return Err(ioerr(format!(
                        "pack-backed open only supports read mode: {}",
                        virtual_path
                    )));
                }
                let temp_path = materialize_pack_to_temp_checked(
                    &scheduler,
                    mt.as_deref(),
                    &virtual_path,
                    ".open",
                )?;
                let stat = match pack_stat(mt.as_deref(), &virtual_path, false)? {
                    StatResult::Single(stat) => stat,
                    StatResult::Recursive(_) => unreachable!(),
                };
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
                domain.remove_temp_file(std::path::Path::new(&path));
            }
            domain_err(e)
        });

    match &result {
        Ok(rid) => trace_file_edge("open_sync", &path, started_at, &format!("rid={rid}")),
        Err(err) => trace_file_edge("open_sync", &path, started_at, &format!("err={err}")),
    }

    result
}

#[op2(async(lazy), fast)]
pub async fn op_close_file(state: Rc<RefCell<OpState>>, #[smi] rid: FileId) -> Result<(), IOError> {
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let domain = scheduler.domain();

    domain.close_file(rid).map_err(domain_err)
}

#[op2(fast)]
pub fn op_close_file_sync(state: &mut OpState, #[smi] rid: FileId) -> Result<(), IOError> {
    get_scheduler(state)
        .domain()
        .close_file(rid)
        .map_err(domain_err)
}

//
// Copy - uses VFS for both source (Read) and destination (Create)
//
#[op2(async(lazy), fast)]
pub async fn op_copy_file(
    state: Rc<RefCell<OpState>>,
    #[string] src_path: String,
    #[string] dest_path: String,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let src_resolved = resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &src_path, FileOp::Read)?;
    let dest_full = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &dest_path,
        FileOp::Create,
    )?)?;

    match src_resolved {
        ResolvedPath::Filesystem(src_full) => {
            run_fs_async(scheduler, move || fs_ops::copy(&src_full, &dest_full)).await
        }
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = mt
                .clone()
                .ok_or_else(|| ioerr("mount table not initialized"))?;
            let rel = code_relative(&virtual_path).to_string();
            copy_pack_file_async(scheduler, mount_table, rel, PathBuf::from(dest_full)).await
        }
    }
}

#[op2(fast)]
pub fn op_copy_file_sync(
    state: &mut OpState,
    #[string] src_path: String,
    #[string] dest_path: String,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let src_resolved = resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &src_path, FileOp::Read)?;
    let dest_full = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &dest_path,
        FileOp::Create,
    )?)?;

    match src_resolved {
        ResolvedPath::Filesystem(src_full) => {
            run_fs_sync(&scheduler, move || fs_ops::copy(&src_full, &dest_full))
        }
        ResolvedPath::Pack { virtual_path } => {
            let m = mt
                .as_deref()
                .ok_or_else(|| ioerr("mount table not initialized"))?;
            let rel = code_relative(&virtual_path);
            fs_ops::copy_mount_entry_to_path(m, rel, std::path::Path::new(&dest_full))
                .map_err(IOError::from)
        }
    }
}

//
// fstat / ftruncate
//
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_fstat(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
) -> Result<FileStat, IOError> {
    let domain = {
        let st = state.borrow();
        get_scheduler(&st).domain()
    };

    domain.fstat(rid).map_err(domain_err)
}

#[op2]
#[serde]
pub fn op_fstat_sync(state: &mut OpState, #[smi] rid: FileId) -> Result<FileStat, IOError> {
    get_scheduler(state).domain().fstat(rid).map_err(domain_err)
}

#[op2(async(lazy), fast)]
pub async fn op_ftruncate(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
    #[smi] len: u64,
) -> Result<(), IOError> {
    let domain = {
        let st = state.borrow();
        get_scheduler(&st).domain()
    };

    domain.ftruncate(rid, len).map_err(domain_err)
}

#[op2(fast)]
pub fn op_ftruncate_sync(
    state: &mut OpState,
    #[smi] rid: FileId,
    #[smi] len: u64,
) -> Result<(), IOError> {
    get_scheduler(state)
        .domain()
        .ftruncate(rid, len)
        .map_err(domain_err)
}

//
// mkdir / readdir - uses VFS with Create/Read permissions
//
#[op2(async(lazy), fast)]
pub async fn op_mkdir(
    state: Rc<RefCell<OpState>>,
    #[string] dir_path: String,
    recursive: bool,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let full_path = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &dir_path,
        FileOp::Create,
    )?)?;

    run_fs_async(scheduler, move || fs_ops::mkdir(&full_path, recursive)).await
}

#[op2(fast)]
pub fn op_mkdir_sync(
    state: &mut OpState,
    #[string] dir_path: String,
    recursive: bool,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let full_path = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &dir_path,
        FileOp::Create,
    )?)?;

    run_fs_sync(&scheduler, move || fs_ops::mkdir(&full_path, recursive))
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_readdir(
    state: Rc<RefCell<OpState>>,
    #[string] dir_path: String,
) -> Result<Vec<String>, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &dir_path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            let m = mt
                .as_deref()
                .ok_or_else(|| ioerr("mount table not initialized"))?;
            let rel = code_relative(&virtual_path);
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
        ResolvedPath::Filesystem(full_path) => {
            run_fs_async(scheduler, move || fs_ops::readdir(&full_path)).await
        }
    }
}

#[op2]
#[serde]
pub fn op_readdir_sync(
    state: &mut OpState,
    #[string] dir_path: String,
) -> Result<Vec<String>, IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &dir_path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            let m = mt
                .as_deref()
                .ok_or_else(|| ioerr("mount table not initialized"))?;
            let rel = code_relative(&virtual_path);
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
        ResolvedPath::Filesystem(full_path) => {
            run_fs_sync(&scheduler, move || fs_ops::readdir(&full_path))
        }
    }
}

//
// unlink / rename / rmdir - uses VFS with Delete/Write permissions
//
#[op2(async(lazy), fast)]
pub async fn op_unlink(
    state: Rc<RefCell<OpState>>,
    #[string] file_path: String,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let full_path = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &file_path,
        FileOp::Delete,
    )?)?;

    run_fs_async(scheduler, move || fs_ops::unlink(&full_path)).await
}

#[op2(fast)]
pub fn op_unlink_sync(state: &mut OpState, #[string] file_path: String) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let full_path = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &file_path,
        FileOp::Delete,
    )?)?;

    run_fs_sync(&scheduler, move || fs_ops::unlink(&full_path))
}

#[op2(async(lazy), fast)]
pub async fn op_rename(
    state: Rc<RefCell<OpState>>,
    #[string] old_path: String,
    #[string] new_path: String,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    // Rename needs delete on source and create on destination
    let old_full = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &old_path,
        FileOp::Delete,
    )?)?;
    let new_full = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &new_path,
        FileOp::Create,
    )?)?;

    run_fs_async(scheduler, move || fs_ops::rename(&old_full, &new_full)).await
}

#[op2(fast)]
pub fn op_rename_sync(
    state: &mut OpState,
    #[string] old_path: String,
    #[string] new_path: String,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let old_full = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &old_path,
        FileOp::Delete,
    )?)?;
    let new_full = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &new_path,
        FileOp::Create,
    )?)?;

    run_fs_sync(&scheduler, move || fs_ops::rename(&old_full, &new_full))
}

#[op2(async(lazy), fast)]
pub async fn op_rmdir(
    state: Rc<RefCell<OpState>>,
    #[string] dir_path: String,
    recursive: bool,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let full_path = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &dir_path,
        FileOp::Delete,
    )?)?;

    run_fs_async(scheduler, move || fs_ops::rmdir(&full_path, recursive)).await
}

#[op2(fast)]
pub fn op_rmdir_sync(
    state: &mut OpState,
    #[string] dir_path: String,
    recursive: bool,
) -> Result<(), IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let full_path = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &dir_path,
        FileOp::Delete,
    )?)?;

    run_fs_sync(&scheduler, move || fs_ops::rmdir(&full_path, recursive))
}

//
// stat - uses VFS with Read permission
//
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_stat(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    recursive: bool,
) -> Result<StatResult, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => pack_stat(mt.as_deref(), &virtual_path, recursive),
        ResolvedPath::Filesystem(full_path) => {
            run_fs_async(scheduler, move || fs_ops::stat(&full_path, recursive)).await
        }
    }
}

#[op2]
#[serde]
pub fn op_stat_sync(
    state: &mut OpState,
    #[string] path: String,
    recursive: bool,
) -> Result<StatResult, IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => pack_stat(mt.as_deref(), &virtual_path, recursive),
        ResolvedPath::Filesystem(full_path) => {
            run_fs_sync(&scheduler, move || fs_ops::stat(&full_path, recursive))
        }
    }
}

//
// write(fd)
//
#[op2(async(lazy))]
#[bigint]
pub fn op_write_file(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
    #[buffer] data_buf: Option<JsBuffer>,
    #[string] data_str: Option<String>,
    #[string] encoding: Option<String>,
    #[bigint] position: Option<u64>,
) -> Result<impl std::future::Future<Output = Result<usize, IOError>>, IOError> {
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let domain = scheduler.domain();
    let payload = prepare_data(data_buf, data_str, encoding)?.into_owned()?;

    Ok(async move {
        run_domain_async(scheduler, move || {
            domain.write_file(rid, &payload, position)
        })
        .await
    })
}

#[op2]
#[bigint]
pub fn op_write_file_sync(
    state: &mut OpState,
    #[smi] rid: FileId,
    #[buffer] data_buf: Option<JsBuffer>,
    #[string] data_str: Option<String>,
    #[string] encoding: Option<String>,
    #[bigint] position: Option<u64>,
) -> Result<usize, IOError> {
    let domain = get_scheduler(state).domain();
    let payload = prepare_data(data_buf, data_str, encoding)?;

    domain
        .write_file(rid, &payload, position)
        .map_err(domain_err)
}

//
// readFile (path) - uses VFS for path resolution
//
#[op2(async(lazy))]
#[serde]
pub async fn op_read_file(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[bigint] position: Option<u64>,
    #[bigint] length: Option<u64>,
) -> Result<ToJsBuffer, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let request_kind = RequestKind::Async;
    match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
        ResolvedPath::Pack { virtual_path } => {
            let mount_table = mt
                .clone()
                .ok_or_else(|| ioerr("mount table not initialized"))?;
            let rel = code_relative(&virtual_path).to_string();
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
            // Taken once: the limit check below and the scheduler's size hint
            // both want it, and a pack entry size is a hashmap lookup.
            let entry_size = mount_table.entry_size(&rel);
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
            let size_hint = entry_size.map(|sz| sz.saturating_sub(position.unwrap_or(0)));
            let request = read_request(BackendKind::Pack, request_kind, length, size_hint);
            let data = scheduler
                .run_async(request, move || {
                    mount_table.read_range_limited(&rel, position.unwrap_or(0), length, max_len)
                })
                .await
                .map_err(pool_err)?
                .map_err(|e| ioerr(format!("pack read failed: {e}")))?;
            Ok(data.into())
        }
        ResolvedPath::Filesystem(full_path) => {
            let vpath = path.clone();
            let allow_mmap = is_read_only_code_path(&path);
            // No size hint on the async path: see `fs_read_size_hint`.
            let request = read_request(BackendKind::Filesystem, request_kind, length, None);
            let data = scheduler
                .run_async(request, move || {
                    let t0 = std::time::Instant::now();
                    let r = fs_ops::read_file(&full_path, position, length, allow_mmap);
                    let disk_ms = t0.elapsed().as_millis() as u64;
                    if let Ok(ref d) = r {
                        if disk_ms >= 30 {
                            tracing::warn!(
                                "[IOTrace] read slow {}ms size={}B path={}",
                                disk_ms,
                                d.len(),
                                vpath
                            );
                        }
                    }
                    r
                })
                .await
                .map_err(pool_err)?
                .map_err(IOError::from)?;
            Ok(data.into())
        }
    }
}

#[op2]
#[serde]
pub fn op_read_file_sync(
    state: &mut OpState,
    #[string] path: String,
    #[bigint] position: Option<u64>,
    #[bigint] length: Option<u64>,
) -> Result<ToJsBuffer, IOError> {
    let started_at = Instant::now();
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let request_kind = RequestKind::Sync;
    let result: Result<ToJsBuffer, IOError> =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
            ResolvedPath::Pack { virtual_path } => {
                let mount_table = mt
                    .clone()
                    .ok_or_else(|| ioerr("mount table not initialized"))?;
                let rel = code_relative(&virtual_path).to_string();
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
                // Taken once: the limit check below and the scheduler's size
                // hint both want it, and a pack entry size is a hashmap lookup.
                let entry_size = mount_table.entry_size(&rel);
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
                let size_hint = entry_size.map(|sz| sz.saturating_sub(position.unwrap_or(0)));
                let request = read_request(BackendKind::Pack, request_kind, length, size_hint);
                let data = scheduler
                    .run_sync(&request, move || {
                        mount_table.read_range_limited(&rel, position.unwrap_or(0), length, max_len)
                    })
                    .map_err(pool_err)?
                    .map_err(|e| ioerr(format!("pack read failed: {e}")))?;
                Ok(data.into())
            }
            ResolvedPath::Filesystem(full_path) => {
                let allow_mmap = is_read_only_code_path(&path);
                // Skip the stat when `length` alone already classifies the
                // read as cheap — it could only confirm what we know.
                let size_hint = match length {
                    Some(len) if len <= scheduler.policy().small_read_bytes as u64 => None,
                    _ => fs_read_size_hint(&full_path),
                };
                let request =
                    read_request(BackendKind::Filesystem, request_kind, length, size_hint);
                let data = scheduler
                    .run_sync(&request, move || {
                        fs_ops::read_file(&full_path, position, length, allow_mmap)
                    })
                    .map_err(pool_err)?
                    .map_err(IOError::from)?;
                Ok(data.into())
            }
        };

    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => {
            trace_file_edge("read_file_sync", &path, started_at, "ok");
            if elapsed_ms >= 30 {
                // Sync reads block V8; surface the path so callers can
                // see which game-side readFileSync is freezing the
                // event loop.
                tracing::warn!("[IOTrace] readFileSync slow {}ms path={}", elapsed_ms, path);
            }
        }
        Err(err) => trace_file_edge("read_file_sync", &path, started_at, &format!("err={err}")),
    }

    result
}

//
// read(fd) - fd-based read into buffer
//
#[op2(async(lazy))]
#[serde]
pub async fn op_read_fd(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
    #[bigint] length: u64,
    #[bigint] position: Option<u64>,
) -> Result<ToJsBuffer, IOError> {
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let domain = scheduler.domain();
    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Async,
        Some(length),
        None,
    );

    scheduler
        .run_async(request, move || domain.read_file(rid, length, position))
        .await
        .map_err(pool_err)?
        .map(|data| data.into())
        .map_err(domain_err)
}

#[op2]
#[serde]
pub fn op_read_fd_sync(
    state: &mut OpState,
    #[smi] rid: FileId,
    #[bigint] length: u64,
    #[bigint] position: Option<u64>,
) -> Result<ToJsBuffer, IOError> {
    let started_at = Instant::now();
    let scheduler = get_scheduler(state);
    let domain = scheduler.domain();
    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Sync,
        Some(length),
        None,
    );

    let target = format!("rid={rid}");
    let result: Result<ToJsBuffer, IOError> = scheduler
        .run_sync(&request, move || domain.read_file(rid, length, position))
        .map_err(pool_err)?
        .map(|data| data.into())
        .map_err(domain_err);

    match &result {
        Ok(_) => trace_file_edge("read_fd_sync", &target, started_at, "ok"),
        Err(err) => trace_file_edge("read_fd_sync", &target, started_at, &format!("err={err}")),
    }

    result
}

/// Expose the filled prefix as a Uint8Array while its backing ArrayBuffer
/// represents all initialized storage retained until V8 releases the result.
pub struct FileReadBuffer(fs_ops::OwnedFileRead);

impl<'a> deno_core::ToV8<'a> for FileReadBuffer {
    type Error = IOError;

    fn to_v8<'i>(
        self,
        scope: &mut deno_core::v8::PinScope<'a, 'i>,
    ) -> Result<deno_core::v8::Local<'a, deno_core::v8::Value>, Self::Error> {
        use deno_core::v8;
        let (storage, filled) = self.0.into_parts();
        let buffer = if storage.is_empty() {
            v8::ArrayBuffer::new(scope, 0)
        } else {
            let backing =
                v8::ArrayBuffer::new_backing_store_from_boxed_slice(storage).make_shared();
            v8::ArrayBuffer::with_backing_store(scope, &backing)
        };
        v8::Uint8Array::new(scope, buffer, 0, filled)
            .map(|view| view.into())
            .ok_or_else(|| ioerr("failed to create file read view"))
    }
}

// Async BYOB reads stage owned bytes on the worker. Only the JS completion
// writes the original view; retaining a backing store never grants exclusivity.
#[op2(async(lazy))]
pub fn op_read_fd_into(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: FileId,
    #[buffer] buf: JsBuffer,
    #[bigint] position: Option<u64>,
) -> Result<impl std::future::Future<Output = Result<FileReadBuffer, IOError>>, IOError> {
    let buf = validate_file_buffer(buf)?;
    let len = buf.len();
    drop(buf);
    let scheduler = get_scheduler(&state.borrow());
    let domain = scheduler.domain();
    let request = read_request(
        BackendKind::Filesystem,
        RequestKind::Async,
        Some(len as u64),
        None,
    );
    Ok(async move {
        scheduler
            .run_async(request, move || {
                // Storage is already boxed on the worker without a shrink allocation.
                domain
                    .read_file_for_buffer(rid, len, position)
                    .map(FileReadBuffer)
            })
            .await
            .map_err(pool_err)?
            .map_err(domain_err)
    })
}

// The sync path blocks V8 until the worker joins, so validated fixed/nonshared
// backing can be written directly. Number returns preserve counts above i32::MAX.

#[op2]
#[number]
pub fn op_read_fd_into_sync(
    state: &mut OpState,
    #[smi] rid: FileId,
    #[buffer] buf: JsBuffer,
    #[bigint] position: Option<u64>,
) -> Result<usize, IOError> {
    let mut buf = validate_file_buffer(buf)?;
    let scheduler = get_scheduler(state);
    let domain = scheduler.domain();
    let len = buf.len() as u64;
    let request = read_request(BackendKind::Filesystem, RequestKind::Sync, Some(len), None);
    scheduler
        .run_sync(&request, move || {
            domain.read_file_into(rid, buf.as_mut(), position)
        })
        .map_err(pool_err)?
        .map_err(domain_err)
}

//
// readCompressedFile (path) - read brotli-compressed file
//
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_read_compressed_file(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
) -> Result<ToJsBuffer, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let (full_path, pack_data) =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
            ResolvedPath::Filesystem(p) => (p, None),
            ResolvedPath::Pack { virtual_path } => {
                let data = read_pack_bytes(mt.as_deref(), &virtual_path)?;
                (virtual_path, Some(data))
            }
        };

    run_fs_async(scheduler, move || {
        fs_ops::read_compressed_file(&full_path, pack_data)
    })
    .await
    .map(|data| data.into())
}

#[op2]
#[serde]
pub fn op_read_compressed_file_sync(
    state: &mut OpState,
    #[string] path: String,
) -> Result<ToJsBuffer, IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let (full_path, pack_data) =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
            ResolvedPath::Filesystem(p) => (p, None),
            ResolvedPath::Pack { virtual_path } => {
                let data = read_pack_bytes(mt.as_deref(), &virtual_path)?;
                (virtual_path, Some(data))
            }
        };

    run_fs_sync(&scheduler, move || {
        fs_ops::read_compressed_file(&full_path, pack_data)
    })
    .map(|data| data.into())
}

// ============================ ReadZipEntry ============================

/// One entry's result on its way to JS.
///
/// `text` and `bytes` are mutually exclusive, expressed as two optional
/// fields rather than an enum on purpose: `serde(untagged)` routes through
/// serde's content-buffering serializer, which loses the marker serde_v8 uses
/// to turn a [`ToJsBuffer`] into a `Uint8Array` — the bytes would arrive in JS
/// as a plain array of numbers. Two fields keep the buffer on serde_v8's fast
/// path, and the JS shim picks whichever is present.
#[derive(serde::Serialize)]
struct ZipEntryPayload {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<ToJsBuffer>,
    #[serde(rename = "errMsg")]
    err_msg: String,
}

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_read_zip_entry(
    state: Rc<RefCell<OpState>>,
    #[string] zip_path: String,
    #[string] entries_json: String,
) -> Result<Vec<ZipEntryPayload>, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let (full_path, pack_data, cleanup_path) =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &zip_path, FileOp::Read)? {
            ResolvedPath::Filesystem(p) => (p, None, None),
            ResolvedPath::Pack { virtual_path } => {
                let mount_table = mt
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
                (temp_path.clone(), None, Some(temp_path))
            }
        };

    let results = scheduler
        .run_async(archive_read_request(), move || {
            fs_ops::read_zip_entry(&full_path, &entries_json, pack_data)
        })
        .await;

    if let Some(path) = &cleanup_path {
        scheduler
            .domain()
            .remove_temp_file(std::path::Path::new(path));
    }

    let results = results.map_err(pool_err)?.map_err(IOError::from)?;

    // A list, not a map: the shim builds the keyed object, and a list keeps the
    // archive's own entry order instead of `serde_json::Map`'s sorted one.
    // Binary payloads ride out as `ToJsBuffer`, which serde_v8 adopts as a
    // `Uint8Array` backing store rather than copying.
    Ok(results
        .into_iter()
        .map(|entry| {
            let (text, bytes) = match entry.data {
                Some(ZipEntryData::Text(s)) => (Some(s), None),
                Some(ZipEntryData::Binary(b)) => (None, Some(b.into())),
                None => (None, None),
            };
            ZipEntryPayload {
                path: entry.path,
                text,
                bytes,
                err_msg: entry.err_msg,
            }
        })
        .collect())
}

// ============================ Unzip ============================

/// Unzip operation with platform service dispatch.
///
/// If the platform provides a `FileService` (e.g. Android's JNI `java.util.zip`),
/// uses that. Otherwise falls back to Rust `zip` crate via the archive scheduler pool.
#[op2(async(lazy), fast)]
pub async fn op_unzip(
    state: Rc<RefCell<OpState>>,
    #[string] zip_file_path: String,
    #[string] target_path: String,
) -> Result<(), IOError> {
    let (vfs, mt, file_svc) = {
        let st = state.borrow();
        let host = st.borrow::<HostOpState>();
        let file_svc = host.device_services.as_ref().and_then(|s| s.file());
        (host.vfs.clone(), host.mount_table.clone(), file_svc)
    };
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };

    let (full_zip_path, cleanup_path) =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &zip_file_path, FileOp::Read)? {
            ResolvedPath::Filesystem(p) => (p, None),
            ResolvedPath::Pack { virtual_path } => {
                let mount_table = mt
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
        vfs.as_deref(),
        mt.as_deref(),
        &target_path,
        FileOp::Write,
    )?)?;

    // Platform-specific unzip (e.g. Android JNI)
    if let Some(svc) = file_svc {
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
            .map_err(|e| IOError::Message(e.to_string()));
        if let Some(path) = cleanup_path {
            scheduler
                .domain()
                .remove_temp_file(std::path::Path::new(&path));
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
    .map_err(|e| IOError::from(EngineError::new(ErrorCode::IoError).with_detail(e.to_string())));

    if let Some(path) = cleanup_path {
        scheduler
            .domain()
            .remove_temp_file(std::path::Path::new(&path));
    }
    result
}

// ============================ GetFileInfo ============================

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_get_file_info(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[string] algorithm: String,
) -> Result<(u64, String), IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let (full_path, pack_data) =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
            ResolvedPath::Pack { virtual_path } => {
                let m = mt
                    .as_deref()
                    .ok_or_else(|| ioerr("mount table not initialized"))?;
                let rel = code_relative(&virtual_path);
                return m
                    .get_file_info(rel, &algorithm)
                    .map_err(|e| ioerr(format!("pack getFileInfo failed: {e}")));
            }
            ResolvedPath::Filesystem(fp) => (fp, None),
        };

    run_fs_async(scheduler, move || {
        fs_ops::get_file_info(&full_path, &algorithm, pack_data)
    })
    .await
}

#[op2]
#[serde]
pub fn op_get_file_info_sync(
    state: &mut OpState,
    #[string] path: String,
    #[string] algorithm: String,
) -> Result<(u64, String), IOError> {
    let (vfs, mt) = get_vfs_sync(state);
    let scheduler = get_scheduler(state);
    let (full_path, pack_data) =
        match resolve_path_vfs(vfs.as_deref(), mt.as_deref(), &path, FileOp::Read)? {
            ResolvedPath::Pack { virtual_path } => {
                let m = mt
                    .as_deref()
                    .ok_or_else(|| ioerr("mount table not initialized"))?;
                let rel = code_relative(&virtual_path);
                return m
                    .get_file_info(rel, &algorithm)
                    .map_err(|e| ioerr(format!("pack getFileInfo failed: {e}")));
            }
            ResolvedPath::Filesystem(fp) => (fp, None),
        };

    run_fs_sync(&scheduler, move || {
        fs_ops::get_file_info(&full_path, &algorithm, pack_data)
    })
}

// ============================ ListSavedFiles ============================

#[op2(async(lazy), fast)]
#[serde]
pub async fn op_list_saved_files(
    state: Rc<RefCell<OpState>>,
    #[string] dir: String,
    #[string] prefix: String,
) -> Result<Vec<SavedFileInfo>, IOError> {
    let (vfs, mt) = get_vfs_async(&state);
    let scheduler = {
        let st = state.borrow();
        get_scheduler(&st)
    };
    let full_dir = require_fs_path(resolve_path_vfs(
        vfs.as_deref(),
        mt.as_deref(),
        &dir,
        FileOp::Read,
    )?)?;
    let virtual_dir = dir;

    run_fs_async(scheduler, move || {
        fs_ops::list_saved_files(&full_dir, &prefix, &virtual_dir)
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        io,
        panic::{AssertUnwindSafe, catch_unwind},
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::{Context, Poll, Waker},
        time::Duration,
    };

    use ::migo_io::{
        domain::DomainError,
        scheduler::{IoScheduler, RouteDecision},
        task::PoolKind,
    };
    use shared::{
        protocol::io_cmd::OpenFlag,
        vfs::{MountBackend, MountTable},
    };

    use super::{
        IOError, archive_read_request, copy_pack_file_async, materialize_pack_to_temp_async,
        materialize_pack_to_temp_checked, read_request, run_domain_async,
    };
    use ::migo_io::task::{BackendKind, RequestKind};

    /// A whole-file read carries no `length`, so the estimate rests entirely on
    /// the size hint. Without one the request has to assume `MAX_READ_LENGTH`
    /// and every `readFile` of a small config or atlas descriptor pays a worker
    /// round-trip that costs more than reading the bytes.
    #[test]
    fn small_whole_file_reads_stay_inline_when_the_size_is_known() {
        let scheduler = IoScheduler::new(1220);

        let hinted = read_request(
            BackendKind::Pack,
            RequestKind::Sync,
            None,
            Some(200), // a 200-byte JSON, the shape this path exists for
        );
        assert_eq!(scheduler.classify(&hinted), RouteDecision::Inline);

        let unhinted = read_request(BackendKind::Pack, RequestKind::Sync, None, None);
        assert_eq!(
            scheduler.classify(&unhinted),
            RouteDecision::Delegated(PoolKind::Pack),
            "an unknown size must stay conservative and delegate"
        );
    }

    /// The hint narrows the estimate; it must never widen it past what the
    /// caller asked for, or a large file would drag a small ranged read out of
    /// the inline path.
    #[test]
    fn size_hint_never_raises_the_estimate_above_the_requested_length() {
        let scheduler = IoScheduler::new(1221);

        let small_read_of_big_file = read_request(
            BackendKind::Filesystem,
            RequestKind::Sync,
            Some(512),
            Some(64 * 1024 * 1024),
        );
        assert_eq!(
            scheduler.classify(&small_read_of_big_file),
            RouteDecision::Inline
        );
    }

    /// A hint smaller than the requested length is the truth about how many
    /// bytes will come back, so it decides the classification.
    #[test]
    fn size_hint_lowers_the_estimate_below_an_oversized_request() {
        let scheduler = IoScheduler::new(1222);

        // `readFileSync(path, {length: 8MB})` against a 100-byte file.
        let request = read_request(
            BackendKind::Filesystem,
            RequestKind::Sync,
            Some(8 * 1024 * 1024),
            Some(100),
        );
        assert_eq!(scheduler.classify(&request), RouteDecision::Inline);
    }

    /// Large reads must keep going to a worker whatever the hint says, or a
    /// multi-megabyte `readFileSync` would run a full decode-length copy on the
    /// V8 thread outside the pool's accounting.
    #[test]
    fn large_whole_file_reads_still_delegate() {
        let scheduler = IoScheduler::new(1223);

        let request = read_request(
            BackendKind::Filesystem,
            RequestKind::Sync,
            None,
            Some(8 * 1024 * 1024),
        );
        assert_eq!(
            scheduler.classify(&request),
            RouteDecision::Delegated(PoolKind::Fs)
        );
    }

    /// The one thing about the zip-entry payload that cannot be reasoned out
    /// from the type: what serde_v8 actually hands JS for a `ToJsBuffer` that
    /// sits inside a struct inside a `Vec`.
    ///
    /// If it degrades to a plain array of numbers, the whole point of moving
    /// off base64 is lost and the JS shim would silently produce garbage. This
    /// runs the real serializer through a real isolate and asks JS.
    #[test]
    fn zip_entry_bytes_reach_js_as_a_uint8array() {
        use deno_core::{FastString, JsRuntime, RuntimeOptions, op2};

        #[op2]
        #[serde]
        fn op_zip_payload_fixture() -> Vec<super::ZipEntryPayload> {
            vec![
                super::ZipEntryPayload {
                    path: "img/bg.png".to_string(),
                    text: None,
                    bytes: Some(vec![0x00, 0xFF, 0x80, 0x01].into()),
                    err_msg: String::new(),
                },
                super::ZipEntryPayload {
                    path: "cfg.json".to_string(),
                    text: Some("{\"a\":1}".to_string()),
                    bytes: None,
                    err_msg: String::new(),
                },
                super::ZipEntryPayload {
                    path: "gone.txt".to_string(),
                    text: None,
                    bytes: None,
                    err_msg: "entry not found".to_string(),
                },
            ]
        }

        deno_core::extension!(zip_payload_fixture, ops = [op_zip_payload_fixture]);

        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions: vec![zip_payload_fixture::init()],
            ..Default::default()
        });

        // Assertions as `throw`s: `execute_script` surfaces them as `Err`.
        let script = r#"
            const r = Deno.core.ops.op_zip_payload_fixture();
            if (r.length !== 3) throw new Error("length " + r.length);

            const bin = r[0];
            if (bin.path !== "img/bg.png") throw new Error("path " + bin.path);
            if (!(bin.bytes instanceof Uint8Array)) {
                throw new Error("bytes is " + Object.prototype.toString.call(bin.bytes)
                    + " -- serde_v8 did not produce a typed array");
            }
            if (bin.bytes.length !== 4) throw new Error("byte length " + bin.bytes.length);
            if (bin.bytes[0] !== 0 || bin.bytes[1] !== 255 || bin.bytes[2] !== 128 || bin.bytes[3] !== 1) {
                throw new Error("bytes " + Array.from(bin.bytes).join(","));
            }
            // The shim hands `.buffer` to callers, so the view must cover it exactly.
            if (bin.bytes.byteOffset !== 0 || bin.bytes.byteLength !== bin.bytes.buffer.byteLength) {
                throw new Error("view is not exact: offset=" + bin.bytes.byteOffset
                    + " len=" + bin.bytes.byteLength + " buf=" + bin.bytes.buffer.byteLength);
            }
            if (bin.text !== undefined) throw new Error("text should be absent");

            const txt = r[1];
            if (txt.text !== '{"a":1}') throw new Error("text " + txt.text);
            if (txt.bytes !== undefined) throw new Error("bytes should be absent");

            const err = r[2];
            if (err.errMsg !== "entry not found") throw new Error("errMsg " + err.errMsg);
            if (err.text !== undefined || err.bytes !== undefined) {
                throw new Error("a failed entry must carry no payload");
            }
        "#;

        rt.execute_script("<test:zip-payload>", FastString::from_static(script))
            .expect("zip entry payload shape");
    }

    /// The other half of the zip-entry path: the shim that turns the op's list
    /// into the `{entries: {...}}` object callers actually see. Driven through
    /// the real ESM export in a live runtime, so it is the shipped function
    /// under test rather than a copy of it.
    #[test]
    fn zip_entries_shim_maps_bytes_text_and_failures() {
        use deno_core::{FastString, JsRuntime, RuntimeOptions};

        deno_core::extension!(
            zip_shim_bridge,
            deps = [host_v8_file],
            esm_entry_point = "ext:zip_shim_bridge/bridge.js",
            esm = ["ext:zip_shim_bridge/bridge.js" = {
                source = r#"
                    import { zipEntriesFromOpResult } from "ext:host_v8_file/02_file_manager.js";
                    globalThis.__zipEntriesFromOpResult = zipEntriesFromOpResult;
                "#
            },],
        );

        let mut extensions = crate::main_extensions(zip_test_host_state());
        extensions.push(zip_shim_bridge::init());
        let mut rt = JsRuntime::new(RuntimeOptions {
            extensions,
            ..Default::default()
        });

        let script = r#"
            const shape = globalThis.__zipEntriesFromOpResult([
                { path: "img/bg.png", bytes: new Uint8Array([0, 255, 128, 1]), errMsg: "" },
                { path: "cfg.json",   text: '{"a":1}',                          errMsg: "" },
                { path: "gone.txt",                                             errMsg: "entry not found" },
            ]);

            const bin = shape.entries["img/bg.png"];
            if (!(bin.data instanceof ArrayBuffer)) {
                throw new Error("binary data is " + Object.prototype.toString.call(bin.data));
            }
            const view = new Uint8Array(bin.data);
            if (view.length !== 4 || view[1] !== 255 || view[2] !== 128) {
                throw new Error("bytes " + Array.from(view).join(","));
            }
            if (bin.errMsg !== "") throw new Error("errMsg " + bin.errMsg);

            const txt = shape.entries["cfg.json"];
            if (txt.data !== '{"a":1}') throw new Error("text " + txt.data);

            const gone = shape.entries["gone.txt"];
            if (gone.data !== null) throw new Error("a failed entry must carry null data");
            if (gone.errMsg !== "entry not found") throw new Error("errMsg " + gone.errMsg);

            // Archive order, not sorted order -- the op returns a list for this reason.
            const keys = Object.keys(shape.entries).join(",");
            if (keys !== "img/bg.png,cfg.json,gone.txt") throw new Error("order " + keys);
        "#;

        rt.execute_script("<test:zip-shim>", FastString::from_static(script))
            .expect("zip entries shim");
    }

    pub(super) fn zip_test_host_state() -> shared::op_state::HostOpState {
        use shared::channel::ThreadWakeup;
        use shared::device::gpu_caps::GpuCaps;
        use shared::op_state::{AudioSender, HostOpState, NetworkPolicy};
        use shared::render_command_sender::CommandSender;
        use std::sync::atomic::AtomicBool;

        let (render_tx, _render_rx) = CommandSender::new();
        let (host_tx, _critical_host_tx, _host_rx) = shared::host_channel::channel(1);

        HostOpState {
            callback_ids: Arc::new(shared::callback_id::CallbackIdAllocator::default()),
            runtime_generation: 1,
            id: 1,
            app_cache_dir: PathBuf::from("/tmp/cache"),
            app_files_dir: PathBuf::from("/tmp/files"),
            code_dir: None,
            game_paths: None,
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
    fn q12_production_file_ops_do_not_escape_to_tokio_blocking_pool() {
        let source = include_str!("fs.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("fs production source");

        assert!(
            !production.contains("tokio::task::spawn_blocking"),
            "production file ops still bypass the bounded R5 executor"
        );
    }

    #[test]
    fn q12_async_domain_jobs_run_on_r5_fs_worker() {
        let scheduler = Arc::new(IoScheduler::new(1210));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let thread_name = runtime
            .block_on(run_domain_async(scheduler, || {
                Ok::<_, DomainError>(
                    std::thread::current()
                        .name()
                        .unwrap_or("unnamed")
                        .to_string(),
                )
            }))
            .unwrap();

        assert!(thread_name.starts_with("Migo-IO-"));
    }

    #[test]
    fn q12_domain_adapter_preserves_open_and_positioned_write_semantics() {
        let dir = temp_dir("q12_domain_file_semantics");
        let path = dir.join("file.bin");
        std::fs::write(&path, b"abc").unwrap();
        let scheduler = Arc::new(IoScheduler::new(1215));
        let domain = scheduler.domain();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let domain_for_open = Arc::clone(&domain);
        let path_for_open = path.clone();
        let rid = runtime
            .block_on(run_domain_async(Arc::clone(&scheduler), move || {
                domain_for_open.open_file(&path_for_open, OpenFlag::ReadWrite, None, None)
            }))
            .unwrap();
        let domain_for_write = Arc::clone(&domain);
        let written = runtime
            .block_on(run_domain_async(Arc::clone(&scheduler), move || {
                domain_for_write.write_file(rid, b"Z", Some(1))
            }))
            .unwrap();

        assert_eq!(written, 1);
        domain.close_file(rid).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"aZc");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn q12_zip_entry_reads_use_archive_class() {
        let scheduler = IoScheduler::new(1211);

        assert_eq!(
            scheduler.classify(&archive_read_request()),
            RouteDecision::Delegated(PoolKind::Archive)
        );
    }

    #[test]
    fn q12_closed_scheduler_rejects_domain_job_before_it_runs() {
        let scheduler = Arc::new(IoScheduler::new(1212));
        scheduler.close();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_job = Arc::clone(&ran);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = runtime.block_on(run_domain_async(scheduler, move || {
            ran_in_job.store(true, Ordering::SeqCst);
            Ok::<_, DomainError>(())
        }));

        assert!(matches!(
            result,
            Err(IOError::Message(ref message)) if message == "IO worker pool closed"
        ));
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn q12_cancelling_waiter_does_not_abort_in_flight_domain_job() {
        let scheduler = Arc::new(IoScheduler::new(1213));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let mut future = Box::pin(run_domain_async(scheduler, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            finished_tx.send(()).unwrap();
            Ok::<_, DomainError>(())
        }));
        let mut context = Context::from_waker(Waker::noop());

        assert!(matches!(
            Future::poll(future.as_mut(), &mut context),
            Poll::Pending
        ));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(future);
        release_tx.send(()).unwrap();

        finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn q12_domain_adapter_preserves_worker_panic_payload() {
        let scheduler = Arc::new(IoScheduler::new(1214));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _: Result<(), IOError> = runtime.block_on(run_domain_async(scheduler, || {
                panic!("q12-domain-worker-panic")
            }));
        }));
        let payload = result.expect_err("worker panic must propagate to the host boundary");
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str));

        assert_eq!(message, Some("q12-domain-worker-panic"));
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("migo_js_file_{label}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[derive(Debug)]
    struct TrackingBackend {
        data: Vec<u8>,
        calls: Arc<AtomicUsize>,
    }

    impl MountBackend for TrackingBackend {
        fn read(&self, _relative_path: &str) -> io::Result<Vec<u8>> {
            Ok(self.data.clone())
        }

        fn exists(&self, _relative_path: &str) -> bool {
            true
        }

        fn real_path(&self, _relative_path: &str) -> Option<PathBuf> {
            None
        }

        fn root_dir(&self) -> Option<&Path> {
            None
        }

        fn copy_to_writer(
            &self,
            _relative_path: &str,
            writer: &mut dyn io::Write,
        ) -> io::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            writer.write_all(&self.data)
        }

        fn is_file(&self, relative_path: &str) -> bool {
            relative_path == "copy.txt"
        }
    }

    #[test]
    fn async_pack_copy_uses_scheduler_worker_path() {
        let scheduler = Arc::new(IoScheduler::new(19));
        let dir = temp_dir("pack_copy_async");
        let base = dir.join("base");
        let dest = dir.join("dest.txt");
        std::fs::create_dir_all(&base).unwrap();

        let mount_table = Arc::new(MountTable::new(base));
        let calls = Arc::new(AtomicUsize::new(0));
        assert!(mount_table.mount_overlay(
            "overlay".to_string(),
            String::new(),
            Arc::new(TrackingBackend {
                data: b"pack-copy".to_vec(),
                calls: Arc::clone(&calls),
            }),
        ));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(copy_pack_file_async(
                Arc::clone(&scheduler),
                Arc::clone(&mount_table),
                "copy.txt".to_string(),
                dest.clone(),
            ))
            .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"pack-copy");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn async_pack_materialization_uses_scheduler_worker_path() {
        let scheduler = Arc::new(IoScheduler::new(23));
        let dir = temp_dir("pack_materialize_async");
        let base = dir.join("base");
        std::fs::create_dir_all(&base).unwrap();

        let mount_table = Arc::new(MountTable::new(base));
        let calls = Arc::new(AtomicUsize::new(0));
        assert!(mount_table.mount_overlay(
            "overlay".to_string(),
            String::new(),
            Arc::new(TrackingBackend {
                data: b"pack-materialized".to_vec(),
                calls: Arc::clone(&calls),
            }),
        ));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let materialized = runtime
            .block_on(materialize_pack_to_temp_async(
                Arc::clone(&scheduler),
                Arc::clone(&mount_table),
                "/code/copy.txt".to_string(),
                ".materialized",
            ))
            .unwrap();

        assert_eq!(std::fs::read(&materialized).unwrap(), b"pack-materialized");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_file(&materialized);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_pack_materialization_skips_copy_when_scheduler_is_closed() {
        let scheduler = IoScheduler::new(29);
        scheduler.close();

        let dir = temp_dir("pack_materialize_sync_closed");
        let base = dir.join("base");
        std::fs::create_dir_all(&base).unwrap();

        let mount_table = MountTable::new(base);
        let calls = Arc::new(AtomicUsize::new(0));
        assert!(mount_table.mount_overlay(
            "overlay".to_string(),
            String::new(),
            Arc::new(TrackingBackend {
                data: b"pack-materialized".to_vec(),
                calls: Arc::clone(&calls),
            }),
        ));

        let result = materialize_pack_to_temp_checked(
            &scheduler,
            Some(&mount_table),
            "/code/copy.txt",
            ".materialized",
        );

        assert!(
            matches!(result, Err(super::IOError::Message(msg)) if msg == "IO worker pool closed")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
#[path = "fs_byob_tests.rs"]
mod byob_tests;
