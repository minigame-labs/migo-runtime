//! Synchronous file-system operations.
//!
//! Each function uses blocking `std::fs` calls. Called directly on the V8
//! thread for sync ops or through the process-wide bounded `IoScheduler` for
//! async ops.

use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Instant,
};

#[cfg(feature = "zip-extract")]
use shared::protocol::io_cmd::{ZipEntryData, ZipEntryResult};
use shared::{
    error::{EngineError, ErrorCode},
    protocol::io_cmd::{
        FileId, FileStat, MAX_READ_LENGTH, OpenFlag, SavedFileInfo, StatEntry, StatResult,
        WriteDurability, WriteMode,
    },
    vfs::MountTable,
};

/// Initialized owned storage and its filled prefix. Keeping these separate
/// avoids a fallible shrink at EOF and lets V8 account for all retained storage.
#[derive(Debug)]
pub struct OwnedFileRead {
    storage: Box<[u8]>,
    bytes_read: usize,
}

impl OwnedFileRead {
    pub fn as_slice(&self) -> &[u8] {
        &self.storage[..self.bytes_read]
    }

    pub fn storage_len(&self) -> usize {
        self.storage.len()
    }

    pub fn into_parts(self) -> (Box<[u8]>, usize) {
        (self.storage, self.bytes_read)
    }

    fn into_vec(self) -> Vec<u8> {
        let mut bytes = self.storage.into_vec();
        bytes.truncate(self.bytes_read);
        bytes
    }
}

fn read_owned_buffer(reader: &mut impl Read, limit: usize) -> Result<OwnedFileRead, EngineError> {
    let mut storage = Vec::new();
    let mut filled = 0;
    let initial = limit.min(64 * 1024);
    storage
        .try_reserve_exact(initial)
        .map_err(|err| io_err(std::io::Error::new(std::io::ErrorKind::OutOfMemory, err)))?;
    storage.resize(initial, 0);
    while filled < limit {
        if filled == storage.len() {
            // Probe before growing, so an exact-fit EOF does not double storage.
            // Reserve explicitly before appending: Read::read_to_end's probe
            // appends infallibly in the pinned stdlib.
            let mut probe = [0; 32];
            let probe_len = probe.len().min(limit - filled);
            let count = match reader.read(&mut probe[..probe_len]) {
                Ok(0) => break,
                Ok(count) => count,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(io_err(err)),
            };
            let next = limit.min(storage.len().saturating_mul(2));
            storage
                .try_reserve_exact(next - storage.len())
                .map_err(|err| io_err(std::io::Error::new(std::io::ErrorKind::OutOfMemory, err)))?;
            storage.resize(next, 0);
            storage[filled..filled + count].copy_from_slice(&probe[..count]);
            filled += count;
        } else {
            match reader.read(&mut storage[filled..]) {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(io_err(err)),
            }
        }
    }
    if filled == 0 {
        // Drop any unused initial allocation rather than retaining it until GC.
        return Ok(OwnedFileRead {
            storage: Box::default(),
            bytes_read: 0,
        });
    }
    // All capacity transferred to V8 must be initialized. resize within capacity
    // cannot allocate; equal length/capacity makes into_boxed_slice a pure move.
    storage.resize(storage.capacity(), 0);
    Ok(OwnedFileRead {
        storage: storage.into_boxed_slice(),
        bytes_read: filled,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[inline]
pub(crate) fn io_err(e: std::io::Error) -> EngineError {
    // Delegates to `impl From<io::Error>` so errno capture stays
    // consistent everywhere the engine converts a syscall failure
    // to an `EngineError` -- don't fork the message/errno logic
    // between here and the generic `?` path.
    EngineError::from(e)
}

/// Context-rich `io::Error` → `EngineError` converter for the fs
/// surface. Attaches the operation name (`"read_file"`, `"stat"`,
/// ...) and the target path so the JS layer can surface them as
/// `err.syscall` / `err.path`, matching Node.js' error shape.
///
/// Use this over [`io_err`] wherever the caller has the path in
/// hand; it costs nothing extra on the happy path and makes error
/// diagnosis noticeably better when games hit a permission or
/// not-found failure.
#[inline]
pub(crate) fn io_err_ctx(e: std::io::Error, op: &'static str, path: &str) -> EngineError {
    EngineError::from(e).with_op(op).with_path(path.to_string())
}

#[inline]
fn code_err(code: ErrorCode) -> EngineError {
    EngineError::new(code)
}

/// `fsync` the directory containing `path` so a newly created file's
/// *name* (the directory entry) survives a crash / power loss.
///
/// Returns the underlying error so callers with a strict durability
/// contract (default-`Durable` `appendFile` creating a new file) can
/// **propagate** it — matching [`crate::atomic_write::atomic_write`],
/// which also fails if the parent-dir fsync fails. Callers with a mere
/// durability *hint* (the `'as'` open flag) intentionally ignore it.
///
/// A no-op returning `Ok(())` on Windows (directory handles don't accept
/// `FlushFileBuffers`).
/// Open a file for appending, reporting whether *this* call created it.
///
/// Race-free against a concurrent create/delete storm: `create_new` is the
/// only *creating* open. The existing-file fallback opens **without**
/// `create`, so a file deleted between the `create_new` (AlreadyExists) and
/// the fallback can't be silently re-created with `created = false` (the
/// TOCTOU that would make a Durable append skip its parent-dir fsync).
/// Instead the fallback sees `NotFound` and we retry `create_new`.
///
/// Bounded to a handful of iterations so a pathological external process
/// alternately creating and deleting the path can't spin forever.
fn open_append_created_aware(path: &str, read: bool) -> Result<(std::fs::File, bool), EngineError> {
    for _ in 0..16 {
        let mut create_opts = std::fs::OpenOptions::new();
        if read {
            create_opts.read(true);
        }
        create_opts.append(true).create_new(true);
        match create_opts.open(path) {
            Ok(f) => return Ok((f, true)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Fallback WITHOUT create: if the file vanished meanwhile we
                // get NotFound and loop back to create_new rather than
                // re-creating it with the wrong `created` flag.
                let mut open_opts = std::fs::OpenOptions::new();
                if read {
                    open_opts.read(true);
                }
                open_opts.append(true);
                match open_opts.open(path) {
                    Ok(f) => return Ok((f, false)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(io_err(e)),
                }
            }
            Err(e) => return Err(io_err(e)),
        }
    }
    Err(EngineError::new(ErrorCode::IoError)
        .with_detail("append open lost a create/delete race repeatedly"))
}

#[inline]
fn fsync_parent_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                return std::fs::File::open(parent)?.sync_all();
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[inline]
fn trace_fs_edge(op: &str, target: &str, started_at: Instant, detail: &str) {
    tracing::debug!(
        "[IOTrace] {} {}us target={} {}",
        op,
        started_at.elapsed().as_micros(),
        target,
        detail
    );
}

#[inline]
fn build_stat(meta: std::fs::Metadata) -> FileStat {
    let mode = get_mode(&meta);
    let size = meta.len();

    let atime = meta
        .accessed()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    FileStat {
        mode,
        size,
        atime,
        mtime,
        is_file: meta.is_file(),
        is_directory: meta.is_dir(),
    }
}

#[inline]
fn get_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    }
    #[cfg(not(unix))]
    {
        if meta.permissions().readonly() {
            0o444
        } else {
            0o666
        }
    }
}

/// Read from a `std::io::Read` with an upper bound on total bytes.
#[cfg(feature = "compress-brotli")]
fn read_to_end_limited<R: Read>(
    reader: &mut R,
    max_len: u64,
    context: &str,
) -> Result<Vec<u8>, EngineError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).map_err(io_err)?;
        if n == 0 {
            break;
        }
        let next_len = out.len().saturating_add(n);
        if next_len as u64 > max_len {
            return Err(EngineError::new(ErrorCode::InvalidArgument)
                .with_detail(format!("{context} exceeds limit {}", max_len)));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

/// Read from a zip entry with position/length support.
fn read_zip_entry_limited<R: Read>(
    reader: &mut R,
    total_size: u64,
    position: Option<u64>,
    length: Option<u64>,
) -> Result<Vec<u8>, EngineError> {
    let start = position.unwrap_or(0).min(total_size);
    if let Some(len) = length {
        if len > MAX_READ_LENGTH {
            return Err(
                EngineError::new(ErrorCode::InvalidArgument).with_detail(format!(
                    "read length {} exceeds limit {}",
                    len, MAX_READ_LENGTH
                )),
            );
        }
    }
    let effective = length.unwrap_or_else(|| total_size.saturating_sub(start));
    if effective > MAX_READ_LENGTH {
        return Err(
            EngineError::new(ErrorCode::InvalidArgument).with_detail(format!(
                "zip entry size {} exceeds limit {}",
                effective, MAX_READ_LENGTH
            )),
        );
    }

    let mut skipped = 0u64;
    let mut scratch = [0u8; 8192];
    while skipped < start {
        let want = (start - skipped).min(scratch.len() as u64) as usize;
        let n = reader.read(&mut scratch[..want]).map_err(io_err)?;
        if n == 0 {
            break;
        }
        skipped += n as u64;
    }

    let mut out = Vec::new();
    while (out.len() as u64) < effective {
        let want = (effective - out.len() as u64).min(scratch.len() as u64) as usize;
        let n = reader.read(&mut scratch[..want]).map_err(io_err)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&scratch[..n]);
    }
    Ok(out)
}

/// Compute a digest hex string from in-memory bytes.
fn compute_digest(data: &[u8], algorithm: &str) -> Result<String, EngineError> {
    use digest::Digest;
    match algorithm {
        "md5" => Ok(hex::encode(md5::Md5::digest(data))),
        "sha1" => Ok(hex::encode(sha1::Sha1::digest(data))),
        "sha256" => Ok(hex::encode(sha2::Sha256::digest(data))),
        _ => Err(EngineError::new(ErrorCode::InvalidArgument)
            .with_detail(format!("unsupported digestAlgorithm: {algorithm}"))),
    }
}

// ---------------------------------------------------------------------------
// FileTable: manages open file descriptors
// ---------------------------------------------------------------------------

/// Manages open file descriptors with ID allocation, temporary file cleanup,
/// and optional synthetic stat data.
pub struct FileTable {
    next_id: FileId,
    free_ids: Vec<FileId>,
    files: HashMap<FileId, std::fs::File>,
    temp_files: HashMap<FileId, PathBuf>,
    synthetic_stats: HashMap<FileId, FileStat>,
    /// FDs opened with a synchronous-write flag (`'as'` / `'as+'`).
    /// Each `write` to these must `fsync` before returning so the
    /// durability the flag name promises actually holds — matching
    /// Node-compatible synchronous append semantics. Kept as a set (not a `File` field)
    /// so the common non-sync fd pays nothing.
    sync_on_write: std::collections::HashSet<FileId>,
}

impl FileTable {
    /// Initial capacity for the file handle map.
    const INITIAL_FILE_CAPACITY: usize = 8;

    pub fn new() -> Self {
        Self {
            next_id: 3, // 0,1,2 reserved for stdio
            free_ids: Vec::new(),
            files: HashMap::with_capacity(Self::INITIAL_FILE_CAPACITY),
            temp_files: HashMap::new(),
            synthetic_stats: HashMap::new(),
            sync_on_write: std::collections::HashSet::new(),
        }
    }

    fn alloc_id(&mut self) -> Result<FileId, EngineError> {
        if let Some(id) = self.free_ids.pop() {
            return Ok(id);
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| EngineError::new(ErrorCode::ExceedMaxConcurrentFdLimit))?;
        Ok(id)
    }

    /// Open a file and return a file descriptor ID.
    pub fn open(
        &mut self,
        path: &str,
        flag: OpenFlag,
        cleanup_path: Option<PathBuf>,
        synthetic_stat: Option<FileStat>,
    ) -> Result<FileId, EngineError> {
        let mut opts = std::fs::OpenOptions::new();

        match flag {
            OpenFlag::Read => {
                opts.read(true);
            }
            OpenFlag::ReadWrite => {
                opts.read(true).write(true);
            }
            OpenFlag::WriteTruncateCreate => {
                opts.write(true).create(true).truncate(true);
            }
            OpenFlag::ReadWriteTruncateCreate => {
                opts.read(true).write(true).create(true).truncate(true);
            }
            OpenFlag::AppendCreate => {
                opts.append(true).create(true);
            }
            OpenFlag::ReadAppendCreate => {
                opts.read(true).append(true).create(true);
            }
            OpenFlag::AppendExclusive => {
                opts.append(true).create_new(true);
            }
            OpenFlag::ReadAppendExclusive => {
                opts.read(true).append(true).create_new(true);
            }
            OpenFlag::AppendSyncCreate => {
                // 'as' - sync hint; treated as append+create
                opts.append(true).create(true);
            }
            OpenFlag::ReadAppendSyncCreate => {
                // 'as+' - sync hint; treated as read+append+create
                opts.read(true).append(true).create(true);
            }
            OpenFlag::WriteExclusive => {
                opts.write(true).create_new(true);
            }
            OpenFlag::ReadWriteExclusive => {
                opts.read(true).write(true).create_new(true);
            }
        }

        let is_sync_append = matches!(
            flag,
            OpenFlag::AppendSyncCreate | OpenFlag::ReadAppendSyncCreate
        );

        // For the sync-append flags, open in a created-aware, race-free way
        // (see `open_append_created_aware`) so we fsync the parent dir *only*
        // when we actually created the file, instead of on every `'as'` open.
        // Other flags use the `opts` built above.
        let (file, created) = if is_sync_append {
            open_append_created_aware(path, matches!(flag, OpenFlag::ReadAppendSyncCreate))?
        } else {
            (opts.open(path).map_err(io_err)?, false)
        };

        let id = self.alloc_id()?;
        // `'as'` / `'as+'` request synchronous appends: remember the fd so
        // every `write` fsyncs before returning (see `write`).
        if is_sync_append {
            self.sync_on_write.insert(id);
            if created {
                // Only a freshly-created file needs its directory entry
                // (the name) made durable. Best-effort here: `'as'` is a
                // durability *hint*, not the strict Durable `appendFile`
                // contract, so a parent-dir fsync failure doesn't fail the
                // open. Per-write `sync_data` (see `write`) keeps contents
                // durable regardless.
                let _ = fsync_parent_dir(Path::new(path));
            }
        }
        self.files.insert(id, file);
        if let Some(path) = cleanup_path {
            self.temp_files.insert(id, path);
        }
        if let Some(stat) = synthetic_stat {
            self.synthetic_stats.insert(id, stat);
        }
        Ok(id)
    }

    fn close_with_cleanup_inner(&mut self, id: FileId) -> Result<Option<PathBuf>, EngineError> {
        self.files
            .remove(&id)
            .map(|file| {
                drop(file);
                let cleanup_path = self.temp_files.remove(&id);
                if let Some(path) = cleanup_path.as_ref() {
                    let _ = std::fs::remove_file(path);
                }
                self.synthetic_stats.remove(&id);
                // Clear the sync flag so a later fd that reuses this id
                // (via `free_ids`) doesn't inherit a stale sync intent.
                self.sync_on_write.remove(&id);
                self.free_ids.push(id);
                cleanup_path
            })
            .ok_or_else(|| code_err(ErrorCode::BadFileDescriptor))
    }

    /// Close a file descriptor. Removes temp files and returns the ID to the pool.
    pub fn close(&mut self, id: FileId) -> Result<(), EngineError> {
        self.close_with_cleanup_inner(id).map(|_| ())
    }

    pub fn close_with_cleanup(&mut self, id: FileId) -> Result<Option<PathBuf>, EngineError> {
        self.close_with_cleanup_inner(id)
    }

    /// Read up to `len` bytes from a file descriptor, optionally seeking first.
    pub fn read(
        &mut self,
        id: FileId,
        len: u64,
        position: Option<u64>,
    ) -> Result<Vec<u8>, EngineError> {
        if len > MAX_READ_LENGTH {
            return Err(
                EngineError::new(ErrorCode::InvalidArgument).with_detail(format!(
                    "read length {} exceeds limit {}",
                    len, MAX_READ_LENGTH
                )),
            );
        }

        self.read_for_buffer(id, len as usize, position)
            .map(OwnedFileRead::into_vec)
    }

    /// Owned staging for BYOB. The caller validates `len` against its actual
    /// destination view; the ordinary read API retains its separate size cap.
    pub fn read_for_buffer(
        &mut self,
        id: FileId,
        len: usize,
        position: Option<u64>,
    ) -> Result<OwnedFileRead, EngineError> {
        let file = self
            .files
            .get_mut(&id)
            .ok_or_else(|| code_err(ErrorCode::BadFileDescriptor))?;

        if let Some(pos) = position {
            file.seek(SeekFrom::Start(pos)).map_err(io_err)?;
        }

        read_owned_buffer(file, len)
    }

    /// Read into a caller-provided buffer, optionally seeking first.
    ///
    /// Fills `buf` from the file and returns the number of bytes read
    /// (`< buf.len()` at EOF). Unlike [`read`](Self::read) this performs
    /// **no allocation**. The caller must provide exclusive access to the
    /// destination for the entire call. This allows a single kernel copy
    /// (no intermediate `Vec` + no V8 `ToJsBuffer` copy + no JS-side
    /// `dst.set`). The length is implicitly bounded by `buf.len()`, which is
    /// itself bounded by the JS-allocated buffer, so no `MAX_READ_LENGTH`
    /// check is needed here.
    pub fn read_into(
        &mut self,
        id: FileId,
        buf: &mut [u8],
        position: Option<u64>,
    ) -> Result<usize, EngineError> {
        let file = self
            .files
            .get_mut(&id)
            .ok_or_else(|| code_err(ErrorCode::BadFileDescriptor))?;

        if let Some(pos) = position {
            file.seek(SeekFrom::Start(pos)).map_err(io_err)?;
        }

        let mut total = 0;
        while total < buf.len() {
            match file.read(&mut buf[total..]) {
                Ok(0) => break, // EOF
                Ok(n) => total += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_err(e)),
            }
        }
        Ok(total)
    }

    /// Write data to a file descriptor, optionally seeking first.
    ///
    /// For fds opened with a synchronous flag (`'as'` / `'as+'`) the
    /// written bytes are flushed to disk (`sync_data`) before returning,
    /// so the durability those flags advertise actually holds.
    pub fn write(
        &mut self,
        id: FileId,
        data: &[u8],
        position: Option<u64>,
    ) -> Result<usize, EngineError> {
        let sync = self.sync_on_write.contains(&id);
        let file = self
            .files
            .get_mut(&id)
            .ok_or_else(|| code_err(ErrorCode::BadFileDescriptor))?;

        if let Some(pos) = position {
            file.seek(SeekFrom::Start(pos)).map_err(io_err)?;
        }

        file.write_all(data).map_err(io_err)?;
        if sync {
            // `sync_data` (fdatasync) rather than `sync_all`: we only need
            // the data + size durable, not the atime/mtime metadata, which
            // is the cheaper guarantee callers of `'as'` actually want.
            file.sync_data().map_err(io_err)?;
        }
        Ok(data.len())
    }

    /// Get file stat for a file descriptor (synthetic stat if available).
    pub fn fstat(&self, id: FileId) -> Result<FileStat, EngineError> {
        match self.synthetic_stats.get(&id) {
            Some(stat) => Ok(stat.clone()),
            None => match self.files.get(&id) {
                Some(file) => {
                    let meta = file.metadata().map_err(io_err)?;
                    Ok(build_stat(meta))
                }
                None => Err(code_err(ErrorCode::BadFileDescriptor)),
            },
        }
    }

    /// Truncate (or extend) a file descriptor to the given length.
    pub fn ftruncate(&mut self, id: FileId, len: u64) -> Result<(), EngineError> {
        let file = self
            .files
            .get_mut(&id)
            .ok_or_else(|| code_err(ErrorCode::BadFileDescriptor))?;

        file.set_len(len).map_err(io_err)?;

        // Best-effort move cursor to end.
        let _ = file.seek(SeekFrom::End(0));
        Ok(())
    }

    /// Close all open file descriptors and clean up temp files.
    pub fn close_all(&mut self) {
        self.files.clear();
        for (_, path) in self.temp_files.drain() {
            let _ = std::fs::remove_file(path);
        }
        self.synthetic_stats.clear();
        self.sync_on_write.clear();
        self.free_ids.clear();
    }
}

// ---------------------------------------------------------------------------
// Free functions: file stat / meta (9)
// ---------------------------------------------------------------------------

/// Check whether a path is accessible and return basic metadata.
/// Returns `(is_file, is_dir, size)`.
pub fn access(path: &str) -> Result<(bool, bool, u64), EngineError> {
    let m = std::fs::metadata(path).map_err(io_err)?;
    Ok((m.is_file(), m.is_dir(), m.len()))
}

/// Get file/directory stat. If `recursive` is true and the path is a
/// directory, returns stats for all files beneath it.
pub fn stat(path: &str, recursive: bool) -> Result<StatResult, EngineError> {
    if !recursive {
        let meta = std::fs::metadata(path).map_err(io_err)?;
        return Ok(StatResult::Single(build_stat(meta)));
    }
    stat_dir_recursive(PathBuf::from(path))
}

/// Recursive directory stat (sync version).
fn stat_dir_recursive(root: PathBuf) -> Result<StatResult, EngineError> {
    let root_meta = std::fs::metadata(&root).map_err(io_err)?;
    if root_meta.is_file() {
        return Ok(StatResult::Single(build_stat(root_meta)));
    }

    // BTreeMap for automatic sorting by key.
    let mut out: BTreeMap<String, FileStat> = BTreeMap::new();
    let mut stack: Vec<PathBuf> = vec![root.clone()];

    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(io_err)?;

        for entry_result in rd {
            let entry = entry_result.map_err(io_err)?;
            let path = entry.path();
            let ft = entry.file_type().map_err(io_err)?;

            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                let meta = entry.metadata().map_err(io_err)?;
                let stat = build_stat(meta);
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, stat);
            }
        }
    }

    // BTreeMap iteration is already sorted by key.
    Ok(StatResult::Recursive(
        out.into_iter()
            .map(|(path, stat)| StatEntry { path, stat })
            .collect(),
    ))
}

/// Create a directory (optionally recursive).
pub fn mkdir(dir_path: &str, recursive: bool) -> Result<(), EngineError> {
    if recursive {
        std::fs::create_dir_all(dir_path).map_err(io_err)
    } else {
        std::fs::create_dir(dir_path).map_err(io_err)
    }
}

/// List direct children of a directory, sorted.
///
/// Entries whose filenames are not valid UTF-8 are rejected with a
/// hard error rather than silently dropped. This matches Node's
/// behaviour: a directory with a ghost file is surfaced to the script
/// so it can either migrate the name or explicitly skip it. Silent
/// drops were a consistent source of "the file is there but my game
/// doesn't see it" bug reports.
pub fn readdir(dir_path: &str) -> Result<Vec<String>, EngineError> {
    let mut entries = Vec::new();
    let rd = std::fs::read_dir(dir_path).map_err(io_err)?;
    for entry_result in rd {
        let entry = entry_result.map_err(io_err)?;
        let os_name = entry.file_name();
        match os_name.to_str() {
            Some(name) => entries.push(name.to_string()),
            None => {
                return Err(EngineError::new(ErrorCode::InvalidArgument)
                    .with_msg("readdir:fail non-UTF-8 filename")
                    .with_detail(format!("dir={} name={:?}", dir_path, os_name)));
            }
        }
    }
    entries.sort_unstable();
    Ok(entries)
}

/// Rename (move) a file or directory.
///
/// Falls back to copy + fsync + rename + unlink when `rename(2)`
/// returns `EXDEV` (source and destination on different mount points,
/// which on Android is the common case when `/tmp` lives on the app
/// cache volume and `/user` lives on scoped storage). The fallback
/// preserves atomicity *at the destination* by writing into a
/// `<new_path>.partN` temp file and renaming it into place.
pub fn rename(old_path: &str, new_path: &str) -> Result<(), EngineError> {
    // `old_path` is the "subject" — Node's convention puts that in
    // `err.path` on rename failures (new path appears in message
    // detail). Matches the JS layer's expectations for `err.path`.
    match std::fs::rename(old_path, new_path) {
        Ok(()) => Ok(()),
        Err(e) if is_exdev(&e) => rename_cross_fs(old_path, new_path).map_err(|fallback_err| {
            io_err_ctx(fallback_err, "rename:exdev", old_path)
                .with_detail(format!("cross-fs rename {old_path} -> {new_path}"))
        }),
        Err(e) => Err(io_err_ctx(e, "rename", old_path)
            .with_detail(format!("rename {old_path} -> {new_path}"))),
    }
}

/// `true` if the error is a cross-mount error (`EXDEV`).
///
/// Hard-coded to 18 because Linux / Android / macOS / iOS all use that
/// value; pulling in a `libc` dependency just to import `libc::EXDEV`
/// would add a whole crate to the dep graph for one numeric literal.
#[cfg(unix)]
fn is_exdev(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(18)
}

#[cfg(not(unix))]
fn is_exdev(_e: &std::io::Error) -> bool {
    false
}

/// Copy-then-delete fallback for [`rename`] when source and
/// destination are on different filesystems. Order of operations is
/// chosen so an interrupted run leaves either the old file intact or
/// a correctly-fsynced new file, never a half-copied intermediate:
///
/// 1. Write to `<new_path>.partN`.
/// 2. `fsync` the tmp file.
/// 3. `rename(tmp, new_path)` — atomic at the destination FS.
/// 4. `fsync` the destination parent directory.
/// 5. Unlink the source.
/// 6. `fsync` the source parent (best-effort; already safe if crashed).
fn rename_cross_fs(old_path: &str, new_path: &str) -> std::io::Result<()> {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read, Write};

    let src_meta = std::fs::symlink_metadata(old_path)?;
    if src_meta.file_type().is_symlink() {
        // Refuse to traverse symlinks in cross-FS move: the runtime's
        // VFS rejects symlink entries elsewhere, and a symlink would
        // otherwise be silently materialised as its target.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rename across filesystems refuses to follow symlinks",
        ));
    }
    if src_meta.file_type().is_dir() {
        // Directory cross-FS rename would require a recursive copy;
        // keep it an explicit failure until a caller actually needs it.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rename across filesystems does not support directories",
        ));
    }

    let new_path_buf = std::path::PathBuf::from(new_path);
    let parent = new_path_buf
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    std::fs::create_dir_all(parent)?;

    // Unique suffix: pid+nanoseconds is collision-resistant enough for
    // the tmp file, and short enough to keep the path under any FS
    // filename limit even for long destination names.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp_name = format!(
        ".{}.{}.{}.part",
        new_path_buf
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("rename"),
        std::process::id(),
        nanos
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut src = File::open(old_path)?;
        let mut dst = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
        }
        dst.flush()?;
        dst.sync_all()?;
    }

    if let Err(e) = std::fs::rename(&tmp_path, new_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Parent directory fsync is a no-op on some filesystems (e.g. tmpfs)
    // and unavailable on Windows; we try and swallow errors because the
    // `rename` above already committed the name, so the tmp file cannot
    // resurface as a live entry.
    #[cfg(unix)]
    {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }

    // Now the destination is durable — unlink the source.
    if let Err(e) = std::fs::remove_file(old_path) {
        // Source unlink failure is non-fatal: destination is valid, so
        // `rename` has observably succeeded. Best-effort log only.
        tracing::warn!(
            "rename_cross_fs: destination committed but source unlink failed: {} ({})",
            old_path,
            e
        );
    }

    #[cfg(unix)]
    {
        if let Some(src_parent) = std::path::Path::new(old_path).parent() {
            if let Ok(dir) = File::open(src_parent) {
                let _ = dir.sync_all();
            }
        }
    }

    Ok(())
}

/// Remove a directory (optionally recursive).
pub fn rmdir(dir_path: &str, recursive: bool) -> Result<(), EngineError> {
    let op: &'static str = if recursive {
        "rmdir:recursive"
    } else {
        "rmdir"
    };
    if recursive {
        std::fs::remove_dir_all(dir_path).map_err(|e| io_err_ctx(e, op, dir_path))
    } else {
        std::fs::remove_dir(dir_path).map_err(|e| io_err_ctx(e, op, dir_path))
    }
}

/// Copy a file.
pub fn copy(src_path: &str, dest_path: &str) -> Result<(), EngineError> {
    let started_at = Instant::now();
    let result = std::fs::copy(src_path, dest_path).map(|_| ()).map_err(|e| {
        io_err_ctx(e, "copy", src_path).with_detail(format!("copy {src_path} -> {dest_path}"))
    });
    match &result {
        Ok(()) => trace_fs_edge("copy", dest_path, started_at, &format!("src={src_path}")),
        Err(err) => trace_fs_edge(
            "copy",
            dest_path,
            started_at,
            &format!("src={src_path} err={err}"),
        ),
    }
    result
}

pub fn copy_mount_entry_to_path(
    mount_table: &MountTable,
    relative_path: &str,
    dest_path: &Path,
) -> Result<(), EngineError> {
    let started_at = Instant::now();
    let result = match mount_table.copy_to_path(relative_path, dest_path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::Unsupported => {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(dest_path)
                .map_err(io_err)?;
            let mut writer = std::io::BufWriter::new(file);
            mount_table
                .copy_to_writer(relative_path, &mut writer)
                .map_err(io_err)?;
            writer.flush().map_err(io_err)
        }
        Err(err) => Err(io_err(err)),
    };
    let target = dest_path.to_string_lossy();
    match &result {
        Ok(()) => trace_fs_edge(
            "copy_mount_entry",
            &target,
            started_at,
            &format!("entry={relative_path}"),
        ),
        Err(err) => trace_fs_edge(
            "copy_mount_entry",
            &target,
            started_at,
            &format!("entry={relative_path} err={err}"),
        ),
    }
    result
}

pub fn materialize_mount_entry_to_temp(
    mount_table: &MountTable,
    relative_path: &str,
    suffix: &str,
) -> Result<PathBuf, EngineError> {
    let started_at = Instant::now();
    let mut dir = mount_table.code_dir();
    let parent = dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    dir = parent.join(".migo-pack-materialized");
    std::fs::create_dir_all(&dir).map_err(io_err)?;

    for _ in 0..32 {
        let mut path = dir.clone();
        path.push(format!(
            "pack_{}_{}{}",
            std::process::id(),
            next_temp_id(),
            suffix
        ));
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(io_err(err)),
        };
        drop(file);

        match copy_mount_entry_to_path(mount_table, relative_path, &path) {
            Ok(()) => {
                trace_fs_edge(
                    "materialize_mount_entry",
                    &path.to_string_lossy(),
                    started_at,
                    &format!("entry={relative_path} suffix={suffix}"),
                );
                return Ok(path);
            }
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                trace_fs_edge(
                    "materialize_mount_entry",
                    &path.to_string_lossy(),
                    started_at,
                    &format!("entry={relative_path} suffix={suffix} err={err}"),
                );
                return Err(err);
            }
        }
    }

    Err(EngineError::new(ErrorCode::IoError)
        .with_detail("failed to allocate unique temp file for pack materialization"))
}

fn next_temp_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TMP_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// File reads (5)
// ---------------------------------------------------------------------------

/// Threshold above which whole-file reads are served via mmap.
///
/// The mmap path is not zero-copy — it maps, then `to_vec`s — so it is not
/// saving a copy. What it trades is one mapping plus a page fault per page
/// against a presized `read`. That trade only pays once the file is large
/// enough for the fault stream to beat the read, and it is a loss below that.
///
/// Measured by `bench_mmap_vs_presized_read` on ext4 with a warm page cache,
/// as a ratio of `read` time to `mmap` time (above 1.0 means mmap wins):
///
/// ```text
///   256 KiB  0.45      2 MiB  0.71       8 MiB  1.20
///   512 KiB  0.61      4 MiB  0.68      16 MiB  1.48
///     1 MiB  0.69
/// ```
///
/// The crossover sits between 4 and 8 MiB, so that is where the threshold
/// goes. It used to be 256 KiB, on the theory that a single mapping issues one
/// readahead where `read` issues many; the measurement says the opposite at
/// that size, and everything from 256 KiB to 4 MiB — which is most of a game's
/// atlases and JSON bundles — was paying 1.4–2.2x for the branch.
///
/// Re-run the bench before moving this. Absolute numbers are host-specific;
/// the shape (per-page fault overhead dominating until the file is large) is
/// not, which is why the old value was wrong on any filesystem.
const MMAP_READ_THRESHOLD: u64 = 8 * 1024 * 1024;

/// Read a file (or a range within it). Enforces MAX_READ_LENGTH.
///
/// Opens the file **once** and uses a single `fstat` for the size /
/// limit checks — the previous implementation did a redundant
/// path-based `std::fs::metadata` before opening on every whole-file
/// read.
///
/// Whole-file reads (`position == None && length == None`) of at least
/// [`MMAP_READ_THRESHOLD`] bytes are served via `mmap` + a single
/// `to_vec` copy **only when `allow_mmap` is set**. mmap is gated to
/// read-only backends (`/code`) because mapping a *writable* file is
/// unsound: if a concurrent writer truncates it while we copy pages
/// out, touching a page past the new EOF raises `SIGBUS` and crashes
/// the process. `/code` is read-only and immutable within a mount
/// generation, so it is safe there; `/user` `/cache` `/tmp` are not and
/// fall back to a presized `read`. Smaller whole-file reads and all
/// range reads use `read` regardless.
pub fn read_file(
    path: &str,
    position: Option<u64>,
    length: Option<u64>,
    allow_mmap: bool,
) -> Result<Vec<u8>, EngineError> {
    let started_at = Instant::now();
    if let Some(len) = length {
        if len > MAX_READ_LENGTH {
            return Err(
                EngineError::new(ErrorCode::InvalidArgument).with_detail(format!(
                    "read length {} exceeds limit {}",
                    len, MAX_READ_LENGTH
                )),
            );
        }
    }

    // Open once; every subsequent size check uses this handle's `fstat`.
    let mut file = std::fs::File::open(path).map_err(|e| io_err_ctx(e, "read_file", path))?;

    // Whole-file read: one fstat serves the limit check, the mmap
    // decision, and the presized allocation.
    if position.is_none() && length.is_none() {
        let file_len = file
            .metadata()
            .map_err(|e| io_err_ctx(e, "read_file:metadata", path))?
            .len();
        if file_len > MAX_READ_LENGTH {
            return Err(
                EngineError::new(ErrorCode::InvalidArgument).with_detail(format!(
                    "remaining file size {} exceeds limit {}",
                    file_len, MAX_READ_LENGTH
                )),
            );
        }

        if allow_mmap && file_len >= MMAP_READ_THRESHOLD {
            // Map the already-open handle (no second `open`). NOT
            // zero-copy: `.to_vec()` still owns a `Vec<u8>` of the full
            // length. Only reached for read-only backends (see fn doc)
            // so the truncation→SIGBUS window doesn't apply.
            match crate::mmap_reader::mmap_bytes_from_file(&file) {
                Ok(mapped) => {
                    let data = mapped.as_slice().to_vec();
                    trace_fs_edge(
                        "read_file",
                        path,
                        started_at,
                        &format!("size={}B mmap=1", data.len()),
                    );
                    return Ok(data);
                }
                Err(e) => {
                    // Exotic FS / kernel restriction: fall through to the
                    // read path using the same open handle.
                    tracing::debug!("mmap read failed for {path}, falling back: {e}");
                }
            }
        }

        // Non-mmap whole-file read: presize to the exact length (bounded
        // by MAX_READ_LENGTH, checked above) so we skip the `Vec` realloc
        // growth loop.
        let mut buf = Vec::with_capacity(file_len as usize);
        (&mut file)
            .take(file_len)
            .read_to_end(&mut buf)
            .map_err(|e| io_err_ctx(e, "read_file", path))?;
        trace_fs_edge(
            "read_file",
            path,
            started_at,
            &format!("size={}B mmap=0", buf.len()),
        );
        return Ok(buf);
    }

    // Range read. When length is unspecified (position-only) verify the
    // remaining bytes from position to EOF are within the limit.
    if length.is_none() {
        let file_len = file
            .metadata()
            .map_err(|e| io_err_ctx(e, "read_file:metadata", path))?
            .len();
        let remaining = file_len.saturating_sub(position.unwrap_or(0));
        if remaining > MAX_READ_LENGTH {
            return Err(
                EngineError::new(ErrorCode::InvalidArgument).with_detail(format!(
                    "remaining file size {} exceeds limit {}",
                    remaining, MAX_READ_LENGTH
                )),
            );
        }
    }

    // Seek to position if specified.
    if let Some(pos) = position {
        file.seek(SeekFrom::Start(pos)).map_err(io_err)?;
    }

    // Read specified length or rest of file.
    let data = if let Some(len) = length {
        // Grow the buffer as bytes actually arrive instead of reserving
        // the full `len` up front: a small file read with a large `len`
        // (e.g. readFile(path, {length: 100 MiB}) on a 1 KiB file) no
        // longer allocates the whole cap and then truncates. Mirrors the
        // fd-based `FileTable::read` fix.
        let mut buf = Vec::with_capacity((len as usize).min(64 * 1024));
        (&mut file)
            .take(len)
            .read_to_end(&mut buf)
            .map_err(|e| io_err_ctx(e, "read_file", path))?;
        buf
    } else {
        read_file_to_end_limited(&mut file, MAX_READ_LENGTH)
            .map_err(|e| e.with_op("read_file").with_path(path.to_string()))?
    };

    trace_fs_edge(
        "read_file",
        path,
        started_at,
        &format!(
            "size={}B position={:?} length={:?} mmap=0",
            data.len(),
            position,
            length
        ),
    );
    Ok(data)
}

/// Read the rest of an open file up to `max_len` bytes.
fn read_file_to_end_limited(
    file: &mut std::fs::File,
    max_len: u64,
) -> Result<Vec<u8>, EngineError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).map_err(io_err)?;
        if n == 0 {
            break;
        }
        let next_len = out.len().saturating_add(n);
        if next_len as u64 > max_len {
            return Err(EngineError::new(ErrorCode::InvalidArgument)
                .with_detail(format!("remaining file size exceeds limit {}", max_len)));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

/// Read and decompress a Brotli-compressed file.
///
/// If `pack_data` is `Some`, decompresses from the provided in-memory bytes
/// instead of reading from `path`.
#[cfg(feature = "compress-brotli")]
pub fn read_compressed_file(
    path: &str,
    pack_data: Option<Vec<u8>>,
) -> Result<Vec<u8>, EngineError> {
    let source: Box<dyn Read> = match pack_data {
        Some(data) => Box::new(std::io::Cursor::new(data)),
        None => Box::new(std::io::BufReader::new(
            std::fs::File::open(path).map_err(io_err)?,
        )),
    };
    let mut reader = brotli::Decompressor::new(source, 4096);
    read_to_end_limited(&mut reader, MAX_READ_LENGTH, "brotli output size")
}

/// Stub when Brotli feature is disabled.
#[cfg(not(feature = "compress-brotli"))]
pub fn read_compressed_file(
    _path: &str,
    _pack_data: Option<Vec<u8>>,
) -> Result<Vec<u8>, EngineError> {
    Err(EngineError::new(ErrorCode::IoError)
        .with_detail("brotli decompression not available (compress-brotli feature disabled)"))
}

/// Read entries from a zip archive, returning per-entry results.
///
/// If `pack_data` is `Some`, reads from the provided in-memory bytes
/// instead of the file at `zip_path`.
#[cfg(feature = "zip-extract")]
pub fn read_zip_entry(
    zip_path: &str,
    entries_json: &str,
    pack_data: Option<Vec<u8>>,
) -> Result<Vec<ZipEntryResult>, EngineError> {
    match pack_data {
        Some(data) => read_zip_entries_from_reader(std::io::Cursor::new(data), entries_json),
        None => {
            let file = std::fs::File::open(zip_path).map_err(io_err)?;
            read_zip_entries_from_reader(std::io::BufReader::new(file), entries_json)
        }
    }
}

/// Stub when zip-extract feature is disabled.
#[cfg(not(feature = "zip-extract"))]
pub fn read_zip_entry(
    _zip_path: &str,
    _entries_json: &str,
    _pack_data: Option<Vec<u8>>,
) -> Result<Vec<shared::protocol::io_cmd::ZipEntryResult>, EngineError> {
    Err(EngineError::new(ErrorCode::IoError)
        .with_detail("readZipEntry not available (zip feature disabled)"))
}

#[cfg(feature = "zip-extract")]
fn read_zip_entries_from_reader<R: Read + std::io::Seek>(
    reader: R,
    entries_json: &str,
) -> Result<Vec<ZipEntryResult>, EngineError> {
    use serde_json;

    let mut archive = zip::ZipArchive::new(reader).map_err(|e| {
        EngineError::new(ErrorCode::IoError).with_detail(format!("invalid zip: {}", e))
    })?;

    let req: serde_json::Value = serde_json::from_str(entries_json).map_err(|e| {
        EngineError::new(ErrorCode::InvalidArgument)
            .with_detail(format!("invalid entries_json: {}", e))
    })?;

    let global_encoding = req
        .get("encoding")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let entries_val = req.get("entries");

    let read_all = entries_val
        .and_then(|v| v.as_str())
        .map(|s| s == "all")
        .unwrap_or(false);

    let mut results = Vec::new();

    if read_all {
        for i in 0..archive.len() {
            let mut entry = match archive.by_index(i) {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            let entry_size = entry.size();
            match read_zip_entry_limited(&mut entry, entry_size, None, None) {
                Ok(buf) => {
                    let data = encode_zip_data(buf, global_encoding.as_deref());
                    results.push(ZipEntryResult {
                        path: name,
                        data: Some(data),
                        err_msg: String::new(),
                    });
                }
                Err(e) => {
                    results.push(ZipEntryResult {
                        path: name,
                        data: None,
                        err_msg: e.to_string(),
                    });
                }
            }
        }
    } else if let Some(arr) = entries_val.and_then(|v| v.as_array()) {
        for item in arr {
            let path = match item.get("path").and_then(|v| v.as_str()) {
                Some(p) => p.to_string(),
                None => continue,
            };
            let encoding = item
                .get("encoding")
                .and_then(|v: &serde_json::Value| v.as_str())
                .or(global_encoding.as_deref());
            let position = item
                .get("position")
                .and_then(|v: &serde_json::Value| v.as_u64());
            let length = item
                .get("length")
                .and_then(|v: &serde_json::Value| v.as_u64());

            match archive.by_name(&path) {
                Ok(mut entry) => {
                    let entry_size = entry.size();
                    match read_zip_entry_limited(&mut entry, entry_size, position, length) {
                        Ok(buf) => {
                            let data = encode_zip_data(buf, encoding);
                            results.push(ZipEntryResult {
                                path,
                                data: Some(data),
                                err_msg: String::new(),
                            });
                        }
                        Err(e) => {
                            results.push(ZipEntryResult {
                                path,
                                data: None,
                                err_msg: e.to_string(),
                            });
                        }
                    }
                }
                Err(e) => {
                    results.push(ZipEntryResult {
                        path,
                        data: None,
                        err_msg: format!("entry not found: {}", e),
                    });
                }
            }
        }
    }

    Ok(results)
}

#[cfg(feature = "zip-extract")]
fn encode_zip_data(data: Vec<u8>, encoding: Option<&str>) -> ZipEntryData {
    use base64::Engine;
    match encoding {
        // No encoding -> the caller wants bytes, and bytes are what the op
        // hands V8. Nothing encodes or copies them on the way.
        None => ZipEntryData::Binary(data),
        Some(enc) => {
            // Delegate to codec for full encoding coverage (utf8, utf16le, ucs2, etc.)
            match shared::codec::decode_bytes(&data, enc) {
                Ok(s) => ZipEntryData::Text(s),
                // Codec doesn't know this encoding. Kept as base64 *text*
                // rather than handed back as bytes: a caller that named an
                // encoding is expecting a string, and switching it to an
                // ArrayBuffer would change a behaviour no test pins.
                Err(_) => {
                    ZipEntryData::Text(base64::engine::general_purpose::STANDARD.encode(&data))
                }
            }
        }
    }
}

/// List saved files (prefix-filtered readdir + stat).
pub fn list_saved_files(
    dir: &str,
    prefix: &str,
    virtual_dir: &str,
) -> Result<Vec<SavedFileInfo>, EngineError> {
    let mut file_list = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(file_list);
        }
        Err(e) => return Err(io_err(e)),
    };
    for entry_result in rd {
        let entry = entry_result.map_err(io_err)?;
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.starts_with(prefix) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if !meta.is_file() {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            file_list.push(SavedFileInfo {
                file_path: format!("{}/{}", virtual_dir, name),
                size: meta.len(),
                create_time: mtime,
            });
        }
    }
    Ok(file_list)
}

// ---------------------------------------------------------------------------
// File writes (5)
// ---------------------------------------------------------------------------

/// Write data to a file (overwrite or append) at the requested durability.
///
/// With [`WriteDurability::Durable`] (the default) overwrite uses
/// [`crate::atomic_write::atomic_write`] (`temp -> fsync -> rename -> dir
/// fsync`) so a crash or power loss never leaves the target truncated —
/// readers observe either the old or the new bytes — and append `fsync`s
/// after writing so an `appendFile` immediately followed by power loss
/// can't lose the just-appended bytes.
///
/// With [`WriteDurability::Fast`] overwrite does a plain truncating
/// `std::fs::write` and append skips the `fsync`: higher throughput, but a
/// crash can leave a torn (overwrite) or lost (append) write. Only for
/// scratch/cache data the caller can afford to lose.
pub fn write_file(
    path: &str,
    data: &[u8],
    mode: WriteMode,
    durability: WriteDurability,
) -> Result<bool, EngineError> {
    match mode {
        WriteMode::Overwrite => match durability {
            WriteDurability::Durable => crate::atomic_write::atomic_write(path, data)
                .map(|_| true)
                .map_err(io_err),
            WriteDurability::Fast => std::fs::write(path, data)
                .map(|_| true)
                .map_err(|e| io_err_ctx(e, "write_file", path)),
        },
        WriteMode::Append => {
            // Distinguish a freshly-created file from an append to an
            // existing one, race-free (see `open_append_created_aware`).
            // Only a newly-created file needs a parent-dir fsync (its *name*
            // must be durable); an existing file's directory entry already
            // survived a prior fsync.
            let (mut file, created) = open_append_created_aware(path, false)?;
            file.write_all(data).map_err(io_err)?;
            if durability == WriteDurability::Durable {
                // Ensure the appended region is on disk before we return
                // success to JS; without this an `appendFile` immediately
                // followed by a power loss can lose the just-appended bytes.
                file.sync_all().map_err(io_err)?;
                if created {
                    // Data + size are durable, but for a file we just
                    // created the directory entry (the name itself) also
                    // needs an fsync, else a crash right after create+append
                    // can lose the whole file. Propagate the error (like
                    // atomic_write) so a Durable append can't silently
                    // claim crash-safety it didn't achieve.
                    fsync_parent_dir(Path::new(path)).map_err(io_err)?;
                }
            }
            Ok(true)
        }
    }
}

/// Write from a byte slice (placeholder for V8 SharedRef path).
///
/// In the current IO-channel model, `WriteShared` copies bytes out of V8
/// BackingStore before crossing the thread boundary.  For direct-call mode
/// the caller will have already performed the copy, so this function just
/// delegates to `write_file`.
pub fn write_shared(
    path: &str,
    data: &[u8],
    mode: WriteMode,
    durability: WriteDurability,
) -> Result<bool, EngineError> {
    write_file(path, data, mode, durability)
}

/// Delete a file.
pub fn unlink(file_path: &str) -> Result<(), EngineError> {
    std::fs::remove_file(file_path).map_err(|e| io_err_ctx(e, "unlink", file_path))
}

// ---------------------------------------------------------------------------
// File hash (1)
// ---------------------------------------------------------------------------

/// Compute file size + digest in a single pass (streaming, 8 KB buffer).
///
/// If `pack_data` is `Some`, computes from the provided bytes instead of
/// reading from `path`.
pub fn get_file_info(
    path: &str,
    algorithm: &str,
    pack_data: Option<Vec<u8>>,
) -> Result<(u64, String), EngineError> {
    if let Some(data) = pack_data {
        let digest_hex = compute_digest(&data, algorithm)?;
        return Ok((data.len() as u64, digest_hex));
    }

    use digest::Digest;

    let meta = std::fs::metadata(path).map_err(io_err)?;
    let size = meta.len();

    // For small files, read all at once.
    if size <= 4 * 1024 * 1024 {
        let data = std::fs::read(path).map_err(io_err)?;
        let digest_hex = compute_digest(&data, algorithm)?;
        return Ok((size, digest_hex));
    }

    // For large files, stream to avoid loading everything into memory.
    let mut file = std::io::BufReader::new(std::fs::File::open(path).map_err(io_err)?);
    let mut buf = [0u8; 8192];

    macro_rules! hash_loop {
        ($hasher:expr) => {{
            let mut h = $hasher;
            loop {
                let n = file.read(&mut buf).map_err(io_err)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex::encode(h.finalize())
        }};
    }

    let digest_hex = match algorithm {
        "md5" => hash_loop!(md5::Md5::new()),
        "sha1" => hash_loop!(sha1::Sha1::new()),
        "sha256" => hash_loop!(sha2::Sha256::new()),
        _ => {
            return Err(EngineError::new(ErrorCode::InvalidArgument)
                .with_detail(format!("unsupported digestAlgorithm: {algorithm}")));
        }
    };

    Ok((size, digest_hex))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use shared::vfs::{MountBackend, MountTable};

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("migo_fsops_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Does the mmap branch in `read_file` earn its keep?
    ///
    /// It maps the file and then immediately `to_vec()`s it, so it is not
    /// saving a copy — the comment there says as much. The claim is that one
    /// mapping triggers a single readahead where `read` issues many. This puts
    /// a number on it, because the branch costs an `allow_mmap` flag threaded
    /// through four call sites plus a soundness rule (never map a file a
    /// writer could truncate) and should only exist if it pays.
    ///
    /// Page cache is warmed first, so this measures the warm path both games
    /// and this test actually hit on a second load. A cold-cache comparison
    /// needs `drop_caches` and root.
    #[test]
    #[ignore]
    fn bench_mmap_vs_presized_read() {
        const ITERATIONS: u32 = 50;
        for size_kib in [256usize, 512, 1024, 2048, 4096, 8192, 16384] {
            let dir = tmp_dir(&format!("bench_mmap_{size_kib}"));
            let path = dir.join("payload.bin");
            std::fs::write(&path, vec![0xA5u8; size_kib * 1024]).unwrap();
            let p = path.to_str().unwrap();

            for _ in 0..5 {
                std::hint::black_box(read_file(p, None, None, false).unwrap());
                std::hint::black_box(read_file(p, None, None, true).unwrap());
            }

            let via_read = {
                let started = Instant::now();
                for _ in 0..ITERATIONS {
                    std::hint::black_box(read_file(p, None, None, false).unwrap());
                }
                started.elapsed()
            };
            let via_mmap = {
                let started = Instant::now();
                for _ in 0..ITERATIONS {
                    std::hint::black_box(read_file(p, None, None, true).unwrap());
                }
                started.elapsed()
            };

            eprintln!(
                "{size_kib:>5} KiB   read {:>10?}/call   mmap {:>10?}/call   mmap is {:.2}x read",
                via_read / ITERATIONS,
                via_mmap / ITERATIONS,
                via_read.as_secs_f64() / via_mmap.as_secs_f64().max(f64::MIN_POSITIVE)
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // ---------------------------------------------------------------------
    // readZipEntry
    //
    // These pin the JS-visible contract of a public API that had no coverage:
    // binary entries arrive as bytes, entries with an encoding arrive decoded,
    // and a missing entry reports rather than fails the batch.
    // ---------------------------------------------------------------------

    #[cfg(feature = "zip-extract")]
    fn entry_text(result: &ZipEntryResult) -> Option<&str> {
        match result.data.as_ref()? {
            ZipEntryData::Text(s) => Some(s.as_str()),
            ZipEntryData::Binary(_) => panic!("expected text for {}", result.path),
        }
    }

    #[cfg(feature = "zip-extract")]
    fn entry_bytes(result: &ZipEntryResult) -> Option<&[u8]> {
        match result.data.as_ref()? {
            ZipEntryData::Binary(b) => Some(b.as_slice()),
            ZipEntryData::Text(_) => panic!("expected bytes for {}", result.path),
        }
    }

    #[cfg(feature = "zip-extract")]
    fn write_test_zip(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        use std::io::Write;

        let zip_path = dir.join("entries.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
        zip_path
    }

    #[cfg(feature = "zip-extract")]
    #[test]
    fn zip_entry_without_encoding_stays_raw_bytes() {
        let dir = tmp_dir("zip_entry_binary");
        // Bytes that are not valid UTF-8, so only a binary-safe transport can
        // carry them intact.
        let payload: Vec<u8> = vec![0x00, 0xFF, 0x89, 0x50, 0x4E, 0x47, 0x80, 0x01];
        let zip_path = write_test_zip(&dir, &[("img/bg.png", &payload)]);

        let results = read_zip_entry(
            zip_path.to_str().unwrap(),
            r#"{"entries":[{"path":"img/bg.png"}]}"#,
            None,
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "img/bg.png");
        assert_eq!(results[0].err_msg, "");
        assert_eq!(
            entry_bytes(&results[0]).expect("entry data"),
            payload.as_slice(),
            "binary entries must survive the transport byte-for-byte"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "zip-extract")]
    #[test]
    fn zip_entry_with_utf8_encoding_is_decoded_text() {
        let dir = tmp_dir("zip_entry_text");
        let zip_path = write_test_zip(&dir, &[("cfg.json", b"{\"a\":1}")]);

        let results = read_zip_entry(
            zip_path.to_str().unwrap(),
            r#"{"encoding":"utf8","entries":[{"path":"cfg.json"}]}"#,
            None,
        )
        .unwrap();

        assert_eq!(entry_text(&results[0]), Some("{\"a\":1}"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "zip-extract")]
    #[test]
    fn missing_zip_entry_reports_without_failing_the_batch() {
        let dir = tmp_dir("zip_entry_missing");
        let zip_path = write_test_zip(&dir, &[("present.txt", b"here")]);

        let results = read_zip_entry(
            zip_path.to_str().unwrap(),
            r#"{"encoding":"utf8","entries":[{"path":"absent.txt"},{"path":"present.txt"}]}"#,
            None,
        )
        .unwrap();

        assert_eq!(
            results.len(),
            2,
            "a missing entry must not drop its sibling"
        );
        assert_eq!(results[0].path, "absent.txt");
        assert!(results[0].data.is_none());
        assert!(
            results[0].err_msg.contains("not found"),
            "unexpected error: {}",
            results[0].err_msg
        );
        assert_eq!(entry_text(&results[1]), Some("here"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "zip-extract")]
    #[test]
    fn zip_entries_all_reads_every_file_and_skips_directories() {
        let dir = tmp_dir("zip_entry_all");
        let zip_path = write_test_zip(&dir, &[("a.txt", b"one"), ("sub/b.txt", b"two")]);

        let results = read_zip_entry(
            zip_path.to_str().unwrap(),
            r#"{"encoding":"utf8","entries":"all"}"#,
            None,
        )
        .unwrap();

        let mut seen: Vec<(String, String)> = results
            .iter()
            .map(|r| {
                (
                    r.path.clone(),
                    entry_text(r).unwrap_or_default().to_string(),
                )
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("a.txt".to_string(), "one".to_string()),
                ("sub/b.txt".to_string(), "two".to_string()),
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "zip-extract")]
    #[test]
    fn zip_entry_honours_position_and_length() {
        let dir = tmp_dir("zip_entry_range");
        let zip_path = write_test_zip(&dir, &[("data.txt", b"0123456789")]);

        let results = read_zip_entry(
            zip_path.to_str().unwrap(),
            r#"{"encoding":"utf8","entries":[{"path":"data.txt","position":3,"length":4}]}"#,
            None,
        )
        .unwrap();

        assert_eq!(entry_text(&results[0]), Some("3456"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "zip-extract")]
    #[test]
    fn zip_entry_reads_from_in_memory_pack_data() {
        let dir = tmp_dir("zip_entry_packdata");
        let zip_path = write_test_zip(&dir, &[("in_pack.txt", b"from memory")]);
        let bytes = std::fs::read(&zip_path).unwrap();

        // The pack-backed path parses the archive out of a buffer rather than
        // a file, and must produce the same results.
        let results = read_zip_entry(
            "/nonexistent/ignored.zip",
            r#"{"encoding":"utf8","entries":[{"path":"in_pack.txt"}]}"#,
            Some(bytes),
        )
        .unwrap();

        assert_eq!(entry_text(&results[0]), Some("from memory"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[derive(Debug)]
    struct WriterOnlyBackend {
        data: Vec<u8>,
        writer_calls: Arc<AtomicUsize>,
    }

    impl MountBackend for WriterOnlyBackend {
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
            self.writer_calls.fetch_add(1, Ordering::SeqCst);
            writer.write_all(&self.data)
        }

        fn is_file(&self, relative_path: &str) -> bool {
            relative_path == "overlay.txt"
        }
    }

    #[test]
    fn copy_mount_entry_to_path_falls_back_to_writer_when_path_copy_is_unsupported() {
        let dir = tmp_dir("mount_copy_fallback");
        let base = dir.join("base");
        let dest = dir.join("dest.txt");
        std::fs::create_dir_all(&base).unwrap();

        let mount_table = MountTable::new(base);
        let writer_calls = Arc::new(AtomicUsize::new(0));
        assert!(mount_table.mount_overlay(
            "overlay".to_string(),
            String::new(),
            Arc::new(WriterOnlyBackend {
                data: b"from-writer".to_vec(),
                writer_calls: Arc::clone(&writer_calls),
            }),
        ));

        copy_mount_entry_to_path(&mount_table, "overlay.txt", &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), b"from-writer");
        assert_eq!(writer_calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_table_open_close() {
        let dir = tmp_dir("ft_open_close");
        let path = dir.join("test.txt");
        std::fs::write(&path, b"hello").unwrap();

        let mut ft = FileTable::new();
        let id = ft
            .open(path.to_str().unwrap(), OpenFlag::Read, None, None)
            .unwrap();
        assert!(id >= 3);

        let stat = ft.fstat(id).unwrap();
        assert!(stat.is_file);
        assert_eq!(stat.size, 5);

        ft.close(id).unwrap();
        assert!(ft.close(id).is_err()); // double close

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_table_read_write() {
        let dir = tmp_dir("ft_rw");
        let path = dir.join("rw.txt");

        let mut ft = FileTable::new();
        let wid = ft
            .open(
                path.to_str().unwrap(),
                OpenFlag::WriteTruncateCreate,
                None,
                None,
            )
            .unwrap();
        ft.write(wid, b"abcdef", None).unwrap();
        ft.close(wid).unwrap();

        let rid = ft
            .open(path.to_str().unwrap(), OpenFlag::Read, None, None)
            .unwrap();
        let data = ft.read(rid, 6, None).unwrap();
        assert_eq!(&data, b"abcdef");

        // partial read with position
        let data2 = ft.read(rid, 3, Some(2)).unwrap();
        assert_eq!(&data2, b"cde");

        ft.close(rid).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_table_append_sync_flag_writes_and_syncs() {
        // `'as'` (AppendSyncCreate) must append and durably sync each
        // write. We can't observe fsync directly in a unit test, but we
        // verify the write path (which now calls sync_data) succeeds and
        // the bytes land correctly on repeated appends.
        let dir = tmp_dir("ft_append_sync");
        let path = dir.join("as.log");

        let mut ft = FileTable::new();
        let id = ft
            .open(
                path.to_str().unwrap(),
                OpenFlag::AppendSyncCreate,
                None,
                None,
            )
            .unwrap();
        ft.write(id, b"line1\n", None).unwrap();
        ft.write(id, b"line2\n", None).unwrap();
        ft.close(id).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"line1\nline2\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_table_sync_flag_cleared_on_close_for_reused_id() {
        // The sync-on-write set must not leak across fd reuse: after
        // closing an `'as'` fd, the next fd (which reuses the id from
        // free_ids) must not inherit the sync intent. A non-sync write
        // to the reused id must still behave correctly.
        let dir = tmp_dir("ft_sync_reuse");
        let sync_path = dir.join("sync.log");
        let plain_path = dir.join("plain.txt");

        let mut ft = FileTable::new();
        let sync_id = ft
            .open(
                sync_path.to_str().unwrap(),
                OpenFlag::AppendSyncCreate,
                None,
                None,
            )
            .unwrap();
        ft.close(sync_id).unwrap();

        // Reuses `sync_id` from free_ids.
        let reused_id = ft
            .open(
                plain_path.to_str().unwrap(),
                OpenFlag::WriteTruncateCreate,
                None,
                None,
            )
            .unwrap();
        assert_eq!(reused_id, sync_id, "id should be reused from free_ids");
        assert!(
            !ft.sync_on_write.contains(&reused_id),
            "reused fd must not inherit stale sync intent"
        );
        ft.write(reused_id, b"data", None).unwrap();
        ft.close(reused_id).unwrap();

        assert_eq!(std::fs::read(&plain_path).unwrap(), b"data");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_table_read_into_fills_caller_buffer() {
        let dir = tmp_dir("ft_read_into");
        let path = dir.join("ri.txt");
        std::fs::write(&path, b"abcdef").unwrap();

        let mut ft = FileTable::new();
        let id = ft
            .open(path.to_str().unwrap(), OpenFlag::Read, None, None)
            .unwrap();

        let mut buf = [0u8; 4];
        let n = ft.read_into(id, &mut buf, None).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"abcd");

        // Positional read where the buffer is larger than the remaining
        // bytes returns a short count (EOF), not an error.
        let mut buf2 = [0u8; 10];
        let n2 = ft.read_into(id, &mut buf2, Some(4)).unwrap();
        assert_eq!(n2, 2);
        assert_eq!(&buf2[..2], b"ef");

        // Bad fd is an error.
        assert!(ft.read_into(9999, &mut buf, None).is_err());

        ft.close(id).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_table_ftruncate() {
        let dir = tmp_dir("ft_trunc");
        let path = dir.join("trunc.txt");
        std::fs::write(&path, b"0123456789").unwrap();

        let mut ft = FileTable::new();
        let id = ft
            .open(path.to_str().unwrap(), OpenFlag::ReadWrite, None, None)
            .unwrap();
        ft.ftruncate(id, 5).unwrap();
        let stat = ft.fstat(id).unwrap();
        assert_eq!(stat.size, 5);
        ft.close(id).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_table_temp_cleanup() {
        let dir = tmp_dir("ft_temp");
        let path = dir.join("temp.txt");
        std::fs::write(&path, b"temp").unwrap();
        assert!(path.exists());

        let mut ft = FileTable::new();
        let id = ft
            .open(
                path.to_str().unwrap(),
                OpenFlag::Read,
                Some(path.clone()),
                None,
            )
            .unwrap();
        ft.close(id).unwrap();
        assert!(!path.exists()); // temp file removed on close
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn access_existing_file() {
        let dir = tmp_dir("access_file");
        let path = dir.join("a.txt");
        std::fs::write(&path, b"x").unwrap();

        let (is_file, is_dir, size) = access(path.to_str().unwrap()).unwrap();
        assert!(is_file);
        assert!(!is_dir);
        assert_eq!(size, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn access_missing_file() {
        assert!(access("/nonexistent_path_fsops_test").is_err());
    }

    #[test]
    fn stat_single_file() {
        let dir = tmp_dir("stat_single");
        let path = dir.join("s.txt");
        std::fs::write(&path, b"stat").unwrap();

        let result = stat(path.to_str().unwrap(), false).unwrap();
        match result {
            StatResult::Single(s) => {
                assert!(s.is_file);
                assert_eq!(s.size, 4);
            }
            _ => panic!("expected Single"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stat_recursive() {
        let dir = tmp_dir("stat_rec");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("a.txt"), b"aa").unwrap();
        std::fs::write(sub.join("b.txt"), b"bbb").unwrap();

        let result = stat(dir.to_str().unwrap(), true).unwrap();
        match result {
            StatResult::Recursive(entries) => {
                assert_eq!(entries.len(), 2);
                // sorted: a.txt before sub/b.txt
                assert_eq!(entries[0].path, "a.txt");
                assert!(entries[1].path.contains("b.txt"));
            }
            _ => panic!("expected Recursive"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn readdir_sorted() {
        let dir = tmp_dir("readdir_sorted");
        std::fs::write(dir.join("c.txt"), b"").unwrap();
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        std::fs::write(dir.join("b.txt"), b"").unwrap();

        let entries = readdir(dir.to_str().unwrap()).unwrap();
        assert_eq!(entries, vec!["a.txt", "b.txt", "c.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mkdir_and_rmdir() {
        let dir = tmp_dir("mkdir_rm");
        let nested = dir.join("a/b/c");

        mkdir(nested.to_str().unwrap(), true).unwrap();
        assert!(nested.is_dir());

        rmdir(dir.join("a").to_str().unwrap(), true).unwrap();
        assert!(!dir.join("a").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_file() {
        let dir = tmp_dir("rename_f");
        let src = dir.join("old.txt");
        let dst = dir.join("new.txt");
        std::fs::write(&src, b"data").unwrap();

        rename(src.to_str().unwrap(), dst.to_str().unwrap()).unwrap();
        assert!(!src.exists());
        assert!(dst.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_file() {
        let dir = tmp_dir("copy_f");
        let src = dir.join("src.txt");
        let dst = dir.join("dst.txt");
        std::fs::write(&src, b"copy_me").unwrap();

        copy(src.to_str().unwrap(), dst.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"copy_me");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_basic() {
        let dir = tmp_dir("rf_basic");
        let path = dir.join("r.txt");
        std::fs::write(&path, b"0123456789").unwrap();

        // Full read
        let data = read_file(path.to_str().unwrap(), None, None, true).unwrap();
        assert_eq!(&data, b"0123456789");

        // Partial read with position + length
        let data2 = read_file(path.to_str().unwrap(), Some(3), Some(4), true).unwrap();
        assert_eq!(&data2, b"3456");

        // Read from position to end
        let data3 = read_file(path.to_str().unwrap(), Some(7), None, true).unwrap();
        assert_eq!(&data3, b"789");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deterministic but non-trivially-compressible payload so we
    /// can't accidentally benchmark the CPU cache instead of the IO
    /// path.
    fn big_payload(bytes: usize) -> Vec<u8> {
        (0..bytes)
            .map(|i| (i as u32).wrapping_mul(2654435761) as u8)
            .collect()
    }

    #[test]
    fn read_file_mmap_path_returns_bit_exact_bytes() {
        // Payload well above MMAP_READ_THRESHOLD so the mmap branch
        // fires. We round-trip through both `read_file` (which
        // takes the mmap fast path) and `std::fs::read` (the raw
        // reference) and verify byte-exact equality.
        let dir = tmp_dir("rf_mmap_bits");
        let path = dir.join("big.bin");
        let payload = big_payload((MMAP_READ_THRESHOLD as usize) + 8 * 1024);
        std::fs::write(&path, &payload).unwrap();

        let via_mmap = read_file(path.to_str().unwrap(), None, None, true).unwrap();
        let via_std = std::fs::read(&path).unwrap();
        assert_eq!(via_mmap.len(), payload.len());
        assert_eq!(via_mmap, via_std);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_mmap_threshold_is_effective() {
        // Two files, one below threshold and one above. Both must
        // come back with correct content — the test isn't timing-
        // sensitive, just a regression guard that the size branch
        // doesn't flip behaviour on the boundary.
        let dir = tmp_dir("rf_mmap_thresh");
        let small = dir.join("small.bin");
        let big = dir.join("big.bin");
        let small_data = big_payload((MMAP_READ_THRESHOLD as usize) - 1024);
        let big_data = big_payload((MMAP_READ_THRESHOLD as usize) + 1);
        std::fs::write(&small, &small_data).unwrap();
        std::fs::write(&big, &big_data).unwrap();

        assert_eq!(
            read_file(small.to_str().unwrap(), None, None, true)
                .unwrap()
                .len(),
            small_data.len()
        );
        assert_eq!(
            read_file(big.to_str().unwrap(), None, None, true)
                .unwrap()
                .len(),
            big_data.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_disallow_mmap_still_returns_correct_bytes() {
        // Writable-dir reads pass allow_mmap=false (mmap of a truncatable
        // file risks SIGBUS). A large file must still read correctly via
        // the presized read path.
        let dir = tmp_dir("rf_no_mmap");
        let path = dir.join("big.bin");
        let payload = big_payload((MMAP_READ_THRESHOLD as usize) + 4096);
        std::fs::write(&path, &payload).unwrap();

        let via_read = read_file(path.to_str().unwrap(), None, None, false).unwrap();
        assert_eq!(via_read.len(), payload.len());
        assert_eq!(via_read, payload);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_partial_range_skips_mmap_but_still_correct() {
        // Sub-range reads must never take the mmap shortcut; the
        // mmap branch is only for whole-file reads. Here the file
        // is big enough that the mmap threshold would trigger if
        // the branch were taken incorrectly, and we assert the
        // bytes match exactly. Behaviour regression guard.
        let dir = tmp_dir("rf_partial_mmap");
        let path = dir.join("big.bin");
        let payload = big_payload((MMAP_READ_THRESHOLD as usize) + 1024);
        std::fs::write(&path, &payload).unwrap();

        // Read a 64-byte range from the middle.
        let start = payload.len() as u64 / 2;
        let got = read_file(path.to_str().unwrap(), Some(start), Some(64), true).unwrap();
        assert_eq!(got.len(), 64);
        assert_eq!(
            got.as_slice(),
            &payload[start as usize..start as usize + 64]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn io_err_captures_errno_and_path_for_missing_file() {
        // Regression guard: when a syscall fails, the engine error
        // must carry the POSIX errno (negative, Node convention)
        // plus the subject path so the JS layer can surface
        // `err.errno === -2` / `err.path === '/…'` without
        // string-parsing.
        let bad = "/does/not/exist/migo_errno_probe";
        let err = read_file(bad, None, None, true).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        // errno is the raw OS error, negated (Node convention). Its numeric
        // value is platform-specific (ENOENT=2 on Unix, ERROR_PATH_NOT_FOUND=3
        // on Windows), so assert against ground truth from the same failing
        // syscall rather than a hardcoded Unix value. This still guards the real
        // invariant: the wrapping layer must carry the OS errno through, negated,
        // and not drop or invent it.
        let expected = -(std::fs::File::open(bad)
            .unwrap_err()
            .raw_os_error()
            .unwrap());
        assert_eq!(err.errno, Some(expected), "errno not captured: {err:?}");
        assert!(err.errno.unwrap() < 0, "errno must be negative: {err:?}");
        assert_eq!(err.path.as_deref(), Some(bad));
        assert_eq!(err.op, Some("read_file"));
    }

    #[test]
    fn io_err_unlink_attaches_op_and_path() {
        let bad = "/does/not/exist/migo_unlink_probe";
        let err = unlink(bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        // Platform-specific errno; derive ground truth from std (see
        // io_err_captures_errno_and_path_for_missing_file).
        let expected = -(std::fs::remove_file(bad)
            .unwrap_err()
            .raw_os_error()
            .unwrap());
        assert_eq!(err.errno, Some(expected));
        assert_eq!(err.op, Some("unlink"));
        assert_eq!(err.path.as_deref(), Some(bad));
    }

    #[test]
    fn io_err_rename_attaches_source_path() {
        let src = "/does/not/exist/migo_rename_src";
        let dst = "/tmp/migo_rename_dst";
        let err = rename(src, dst).unwrap_err();
        // Platform-specific errno; derive ground truth from the same failing
        // std::fs::rename rather than hardcoding the Unix ENOENT value.
        let expected = -(std::fs::rename(src, dst)
            .unwrap_err()
            .raw_os_error()
            .unwrap());
        assert_eq!(err.errno, Some(expected));
        assert_eq!(err.op, Some("rename"));
        assert_eq!(err.path.as_deref(), Some(src));
        // detail should still mention the destination so diagnosis is self-contained.
        assert!(
            err.detail
                .as_deref()
                .map(|d| d.contains(dst))
                .unwrap_or(false),
            "detail did not mention dst: {:?}",
            err.detail
        );
    }

    #[test]
    fn read_file_whole_file_over_max_read_length_rejected() {
        // Regression: the mmap fast path must still honour the
        // MAX_READ_LENGTH quota. Construct a synthetic metadata
        // check via a real file whose size exceeds MAX_READ_LENGTH
        // would be too slow; instead we verify the length guard on
        // the explicit-length code path (shared behaviour, same
        // error message).
        //
        // The metadata-sourced size check in the mmap branch uses
        // the exact same MAX_READ_LENGTH constant, so the unit
        // test here is asserting the guard is the same constant
        // we advertise in the public doc comment.
        let dir = tmp_dir("rf_max");
        let path = dir.join("tiny.bin");
        std::fs::write(&path, b"tiny").unwrap();
        let err = read_file(
            path.to_str().unwrap(),
            None,
            Some(MAX_READ_LENGTH + 1),
            true,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_file_large_length_on_small_file_returns_only_file_bytes() {
        // Regression: a big `length` on a tiny file must return just the
        // file's bytes without pre-allocating the full (valid, <=
        // MAX_READ_LENGTH) length. Behaviour guard for the growth-based
        // read path that replaced `vec![0u8; len]`.
        let dir = tmp_dir("rf_big_len_small_file");
        let path = dir.join("tiny.bin");
        std::fs::write(&path, b"0123456789").unwrap();

        // length far larger than the file, but within MAX_READ_LENGTH.
        let data = read_file(path.to_str().unwrap(), None, Some(8 * 1024 * 1024), true).unwrap();
        assert_eq!(&data, b"0123456789");

        // With a position + oversized length: returns from position to EOF.
        let data2 =
            read_file(path.to_str().unwrap(), Some(4), Some(8 * 1024 * 1024), true).unwrap();
        assert_eq!(&data2, b"456789");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owned_byob_large_capacity_on_tiny_file_preserves_the_ordinary_read_limit() {
        let dir = tmp_dir("owned_byob_capacity");
        let path = dir.join("tiny");
        std::fs::write(&path, b"abcdef").unwrap();
        let mut table = FileTable::new();
        let rid = table
            .open(path.to_str().unwrap(), OpenFlag::Read, None, None)
            .unwrap();
        let data = table
            .read_for_buffer(rid, (MAX_READ_LENGTH + 1) as usize, None)
            .unwrap();
        assert_eq!(data.as_slice(), b"abcdef");
        assert!(
            data.storage_len() <= 128 * 1024,
            "tiny read reserved the destination's large size"
        );
        assert!(table.read(rid, MAX_READ_LENGTH + 1, None).is_err());
        table.close(rid).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn owned_byob_zero_read_seeks_and_still_validates_the_descriptor() {
        let dir = tmp_dir("owned_byob_zero_seek");
        let path = dir.join("data");
        std::fs::write(&path, b"abcdef").unwrap();
        let mut table = FileTable::new();
        let rid = table
            .open(path.to_str().unwrap(), OpenFlag::Read, None, None)
            .unwrap();
        assert!(
            table
                .read_for_buffer(rid, 0, Some(3))
                .unwrap()
                .as_slice()
                .is_empty()
        );
        assert_eq!(
            table.read_for_buffer(rid, 2, None).unwrap().as_slice(),
            b"de"
        );
        assert_eq!(
            table.read_for_buffer(rid, 8, None).unwrap().as_slice(),
            b"f"
        );
        assert!(
            table
                .read_for_buffer(rid, 8, None)
                .unwrap()
                .as_slice()
                .is_empty()
        );
        table.close(rid).unwrap();
        assert!(table.read_for_buffer(rid, 0, Some(0)).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn owned_byob_growth_eof_and_non_power_of_two_limits() {
        for (file_len, limit) in [(65536, 200000), (65537, 200000), (200000, 100001)] {
            let source: Vec<u8> = (0..file_len).map(|i| (i % 251) as u8).collect();
            let mut reader = std::io::Cursor::new(&source);
            let result = read_owned_buffer(&mut reader, limit).unwrap();
            let expected = file_len.min(limit);
            assert_eq!(result.as_slice(), &source[..expected]);
            assert_eq!(reader.position(), expected as u64);
            if file_len == 65536 {
                assert_eq!(result.storage_len(), 65536, "exact EOF grew storage");
            }
            let (storage, filled) = result.into_parts();
            assert!(storage[filled..].iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn owned_byob_retries_interrupted_short_reads_and_returns_io_errors() {
        struct Reader {
            cursor: std::io::Cursor<Vec<u8>>,
            interrupt: bool,
            fail: bool,
        }
        impl Read for Reader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.interrupt = !self.interrupt;
                if self.interrupt {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                if self.fail && self.cursor.position() >= 65536 {
                    return Err(std::io::ErrorKind::PermissionDenied.into());
                }
                let count = buf.len().min(7);
                self.cursor.read(&mut buf[..count])
            }
        }
        let mut reader = Reader {
            cursor: std::io::Cursor::new(vec![42; 70000]),
            interrupt: false,
            fail: false,
        };
        assert_eq!(
            read_owned_buffer(&mut reader, 100000).unwrap().as_slice(),
            vec![42; 70000]
        );
        reader.cursor.set_position(0);
        reader.fail = true;
        assert!(read_owned_buffer(&mut reader, 100000).is_err());
    }

    #[test]
    fn write_file_overwrite_and_append() {
        let dir = tmp_dir("wf_modes");
        let path = dir.join("w.txt");

        write_file(
            path.to_str().unwrap(),
            b"hello",
            WriteMode::Overwrite,
            WriteDurability::Durable,
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");

        write_file(
            path.to_str().unwrap(),
            b" world",
            WriteMode::Append,
            WriteDurability::Durable,
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");

        // Fast durability: overwrite still lands correctly (just not fsync'd).
        write_file(
            path.to_str().unwrap(),
            b"new",
            WriteMode::Overwrite,
            WriteDurability::Fast,
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_file_append_creates_new_file_then_appends() {
        // First durable append to a non-existent path exercises the
        // created=true branch (parent-dir fsync); the second hits the
        // existing-file branch. Then a Fast append to a fresh file.
        let dir = tmp_dir("wf_append_new");
        let path = dir.join("newlog.txt");
        assert!(!path.exists());

        write_file(
            path.to_str().unwrap(),
            b"line1\n",
            WriteMode::Append,
            WriteDurability::Durable,
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"line1\n");

        write_file(
            path.to_str().unwrap(),
            b"line2\n",
            WriteMode::Append,
            WriteDurability::Durable,
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"line1\nline2\n");

        let path2 = dir.join("fast.txt");
        write_file(
            path2.to_str().unwrap(),
            b"x",
            WriteMode::Append,
            WriteDurability::Fast,
        )
        .unwrap();
        assert_eq!(std::fs::read(&path2).unwrap(), b"x");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unlink_file() {
        let dir = tmp_dir("unlink_f");
        let path = dir.join("del.txt");
        std::fs::write(&path, b"bye").unwrap();

        unlink(path.to_str().unwrap()).unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_file_info_md5() {
        let dir = tmp_dir("fi_md5");
        let path = dir.join("hash.txt");
        std::fs::write(&path, b"hello").unwrap();

        let (size, digest) = get_file_info(path.to_str().unwrap(), "md5", None).unwrap();
        assert_eq!(size, 5);
        assert_eq!(digest, "5d41402abc4b2a76b9719d911017c592");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_file_info_from_pack_data() {
        let (size, digest) =
            get_file_info("unused_path", "sha256", Some(b"test".to_vec())).unwrap();
        assert_eq!(size, 4);
        assert_eq!(
            digest,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn get_file_info_unsupported_algo() {
        let dir = tmp_dir("fi_unsup");
        let path = dir.join("algo.txt");
        std::fs::write(&path, b"x").unwrap();

        let err = get_file_info(path.to_str().unwrap(), "crc32", None).unwrap_err();
        assert!(err.to_string().contains("unsupported"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_saved_files_filters_prefix() {
        let dir = tmp_dir("lsf");
        std::fs::write(dir.join("game_save1.dat"), b"a").unwrap();
        std::fs::write(dir.join("game_save2.dat"), b"bb").unwrap();
        std::fs::write(dir.join("other.dat"), b"ccc").unwrap();
        std::fs::create_dir(dir.join("game_subdir")).unwrap();

        let files = list_saved_files(dir.to_str().unwrap(), "game_save", "/user").unwrap();
        assert_eq!(files.len(), 2);
        for f in &files {
            assert!(f.file_path.starts_with("/user/game_save"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_saved_files_missing_dir() {
        let files = list_saved_files("/nonexistent_lsf_dir", "", "/virtual").unwrap();
        assert!(files.is_empty());
    }

    #[cfg(feature = "compress-brotli")]
    #[test]
    fn read_to_end_limited_rejects_oversized() {
        let mut reader = std::io::Cursor::new(vec![1u8; 16]);
        let err = read_to_end_limited(&mut reader, 8, "test size").unwrap_err();
        assert!(err.to_string().contains("exceeds limit"));
    }

    #[test]
    fn read_zip_entry_limited_respects_remaining() {
        let mut reader = std::io::Cursor::new(vec![7u8; 32]);
        let err =
            read_zip_entry_limited(&mut reader, MAX_READ_LENGTH + 16, Some(4), None).unwrap_err();
        assert!(err.to_string().contains("exceeds limit"));

        let mut ok_reader = std::io::Cursor::new(vec![9u8; 16]);
        let data = read_zip_entry_limited(&mut ok_reader, 16, Some(8), None).unwrap();
        assert_eq!(data, vec![9u8; 8]);
    }

    #[test]
    fn read_zip_entry_limited_rejects_explicit_oversized_length() {
        let mut reader = std::io::Cursor::new(vec![0u8; 8]);
        let err =
            read_zip_entry_limited(&mut reader, 8, Some(0), Some(MAX_READ_LENGTH + 1)).unwrap_err();
        assert!(err.to_string().contains("read length"));
    }

    #[test]
    fn file_table_close_all() {
        let dir = tmp_dir("ft_close_all");
        let p1 = dir.join("a.txt");
        let p2 = dir.join("b.txt");
        std::fs::write(&p1, b"1").unwrap();
        std::fs::write(&p2, b"2").unwrap();

        let mut ft = FileTable::new();
        let id1 = ft
            .open(p1.to_str().unwrap(), OpenFlag::Read, None, None)
            .unwrap();
        let _id2 = ft
            .open(p2.to_str().unwrap(), OpenFlag::Read, None, None)
            .unwrap();
        ft.close_all();

        // After close_all, nothing should be accessible.
        assert!(ft.fstat(id1).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_table_synthetic_stat() {
        let dir = tmp_dir("ft_synth");
        let path = dir.join("syn.txt");
        std::fs::write(&path, b"x").unwrap();

        let synth = FileStat {
            mode: 0o777,
            size: 999,
            atime: 100,
            mtime: 200,
            is_file: true,
            is_directory: false,
        };

        let mut ft = FileTable::new();
        let id = ft
            .open(
                path.to_str().unwrap(),
                OpenFlag::Read,
                None,
                Some(synth.clone()),
            )
            .unwrap();

        // Should return synthetic stat, not real.
        let s = ft.fstat(id).unwrap();
        assert_eq!(s.size, 999);
        assert_eq!(s.mtime, 200);

        ft.close(id).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
