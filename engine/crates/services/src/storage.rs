//! Key-value storage and buffer URLs.
//!
//! Backed by an embedded WAL-mode SQLite database opened lazily per game at
//! `{game files}/runtime_state/kv_storage/storage.db`, outside the VFS roots;
//! see [`storage_dir`]. The schema and the quota accounting are
//! [`migo_io::storage_ops`] and [`migo_io::kv_store`]; this module is the rules
//! an op applies on top of them and the errors content sees, moved here from
//! the embedded runtime's ops so both executions apply the same ones.
//!
//! ## Limits
//!
//! - Single value: 1 MB
//! - Total charged storage: 10 MB (value bytes, key bytes, and the fixed
//!   per-entry SQLite record/B-tree reservation)
//! - Key bytes: 16 KiB
//! - Entries: 10,000
//!
//! ## Two error shapes, kept
//!
//! The synchronous calls throw a plain `Error` whose message is the engine
//! error's detail; the awaited ones throw `StorageError` with summary and
//! detail. That difference predates this module and content can observe it, so
//! it is preserved rather than tidied.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Weak};

use migo_io::scheduler::IoScheduler;
use migo_io::storage_ops::{self, StorageInfo};
use migo_io::task::{IoRequest, PoolKind, PriorityClass, RequestKind};
use parking_lot::{Condvar, Mutex};
use shared::error::{EngineError, ErrorCode};
use shared::vfs::{
    GamePaths, VirtualFS, game_paths::StoragePaths, write_directory::WriteDirectory,
};
use tokio::sync::oneshot;

use crate::error::{ServiceError, detail_or_message, message_and_detail};

/// Reserved leaf name, shared by legacy and private storage namespaces.
pub const STORAGE_DIR: &str = shared::vfs::game_paths::KV_STORAGE_SUBDIR;

/// Buffer URL directory name under the game's sandbox cache directory.
pub const BUFFER_URL_DIR: &str = "buffer_urls";
const BUFFER_URL_ROOT: &str = "/cache/buffer_urls";

static NEXT_BUFFER_URL: AtomicU64 = AtomicU64::new(0);

/// Maximum size of a single stored value (1 MB).
pub const MAX_VALUE_SIZE: usize = 1024 * 1024;

/// Keep oversized keys off the scheduler queue as well as out of SQLite.
pub const MAX_KEY_SIZE: usize = 16 * 1024;

/// Maximum total storage size in KB (10 MB = 10240 KB).
pub const LIMIT_SIZE_KB: u32 = 10240;

/// Maximum total storage size in bytes: what each game gets.
pub const MAX_TOTAL_BYTES: u64 = LIMIT_SIZE_KB as u64 * 1024;

/// Cap the wire string built for `getStorageInfo`; the JS wrapper parses this
/// whole string before exposing it, so truncating only after formatting would
/// leave the peak allocation unchanged.
const MAX_INFO_JSON_BYTES: usize = 512 * 1024;

/// The class the awaited storage calls throw.
pub const CLASS_STORAGE_ERROR: &str = "StorageError";

/// Per-game storage root.
///
/// Host-private and persistent per game. A host that runs several games must not give them
/// one SQLite file: any game could read another's saves by guessing keys, a
/// single `migo.clearStorage()` would wipe the whole catalogue, and the 10 MB
/// quota would be a shared pool one game could exhaust for the rest.
///
/// Fails when no game is loaded rather than falling back to a shared location:
/// a fallback would silently reintroduce the shared file it exists to prevent.
pub fn storage_dir(game_paths: Option<&GamePaths>) -> Result<PathBuf, EngineError> {
    storage_paths(game_paths).map(|paths| paths.directory().to_path_buf())
}

fn storage_paths(game_paths: Option<&GamePaths>) -> Result<&Arc<StoragePaths>, EngineError> {
    match game_paths {
        Some(paths) => Ok(paths.storage()),
        None => Err(EngineError::from_detail(
            ErrorCode::InvalidOperation,
            "storage is unavailable before a game is loaded".to_string(),
        )),
    }
}

fn prepare_sync_dir(
    scheduler: &IoScheduler,
    game_paths: Option<&GamePaths>,
) -> Result<PathBuf, EngineError> {
    let paths = storage_paths(game_paths)?;
    if !paths.is_prepared() {
        let owned = paths.clone();
        let request = IoRequest::StorageMutate {
            request: RequestKind::Sync,
            priority: PriorityClass::from(RequestKind::Sync),
        };
        scheduler
            .run_sync(&request, move || {
                migo_io::storage_migration::prepare(&owned)
            })
            .map_err(EngineError::from)??;
    }
    Ok(paths.directory().to_path_buf())
}

/// Per-game scratch space for `URL.createObjectURL` payloads.
///
/// Inside the sandbox subtree rather than the cache root: the payload is the
/// game's own bytes handed back to it, so the path has to stay one the game can
/// read, and the root is reserved for runtime state.
pub fn buffer_url_dir(game_paths: Option<&GamePaths>) -> Result<PathBuf, EngineError> {
    match game_paths {
        Some(paths) => Ok(paths.sandbox_cache_dir().join(BUFFER_URL_DIR)),
        None => Err(EngineError::from_detail(
            ErrorCode::InvalidOperation,
            "buffer URLs are unavailable before a game is loaded".to_string(),
        )),
    }
}

/// A sync call's error: a plain `Error` with the engine error's detail.
fn sync_error(error: EngineError) -> ServiceError {
    ServiceError::generic(detail_or_message(&error))
}

/// An awaited call's error: `StorageError` with summary and detail.
fn async_error(error: EngineError) -> ServiceError {
    ServiceError::classed(CLASS_STORAGE_ERROR, message_and_detail(&error))
}

fn storage_error(message: impl Into<String>) -> ServiceError {
    ServiceError::classed(CLASS_STORAGE_ERROR, message)
}

/// Serialize [`StorageInfo`] into the JSON shape expected by the JS wrapper.
/// Sizes are reported in KiB (ceil), mirroring the legacy format.
pub fn info_to_json(info: &StorageInfo) -> String {
    // Ceil to KiB so a 1-byte value reports currentSize=1.
    let current_kib = info.current_bytes.div_ceil(1024);
    let limit_kib = info.limit_bytes.div_ceil(1024);
    let suffix = format!(r#"],"currentSize":{current_kib},"limitSize":{limit_kib}}}"#);
    let mut out =
        String::with_capacity(MAX_INFO_JSON_BYTES.min(info.keys.len().saturating_mul(16)));
    out.push_str(r#"{"keys":["#);
    let mut first = true;
    for k in &info.keys {
        // Escape one key at a time so an oversized key list never creates a
        // second full-size staging string.
        let mut escaped = String::with_capacity(k.len());
        for c in k.chars() {
            match c {
                '"' => escaped.push_str("\\\""),
                '\\' => escaped.push_str("\\\\"),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    escaped.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => escaped.push(c),
            }
        }
        let separator_len = usize::from(!first);
        if out
            .len()
            .saturating_add(separator_len)
            .saturating_add(escaped.len())
            .saturating_add(2)
            .saturating_add(suffix.len())
            > MAX_INFO_JSON_BYTES
        {
            break;
        }
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        out.push_str(&escaped);
        out.push('"');
    }
    out.push_str(&suffix);
    debug_assert!(out.len() <= MAX_INFO_JSON_BYTES);
    out
}

fn check_value_and_key(key: &str, value: &str) -> Result<(), &'static str> {
    if value.len() > MAX_VALUE_SIZE {
        return Err("setStorage:fail data exceeds max size");
    }
    if key.len() > MAX_KEY_SIZE {
        return Err("setStorage:fail key exceeds max size");
    }
    Ok(())
}

// ---- Synchronous: a plain `Error` with the detail ----

/// `getStorageSync`. A missing key is `""`, which the JS wrapper's
/// `deserialize("")` contract turns into an empty string.
pub fn get_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: &str,
) -> Result<String, ServiceError> {
    sync_in_line(&scheduler, game_paths, || {
        let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
        storage_ops::storage_get_sync_with_scheduler(
            Arc::clone(&scheduler),
            dir,
            key.to_string(),
            MAX_TOTAL_BYTES,
        )
        .map(Option::unwrap_or_default)
        .map_err(sync_error)
    })
}

pub fn set_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: &str,
    value: &str,
) -> Result<(), ServiceError> {
    check_value_and_key(key, value).map_err(ServiceError::generic)?;
    sync_in_line(&scheduler, game_paths, || {
        let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
        storage_ops::storage_set_sync_with_scheduler(
            Arc::clone(&scheduler),
            dir,
            key.to_string(),
            value.to_string(),
            MAX_TOTAL_BYTES,
        )
        .map_err(sync_error)
    })
}

pub fn remove_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: &str,
) -> Result<(), ServiceError> {
    sync_in_line(&scheduler, game_paths, || {
        let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
        storage_ops::storage_remove_sync_with_scheduler(
            Arc::clone(&scheduler),
            dir,
            key.to_string(),
            MAX_TOTAL_BYTES,
        )
        .map_err(sync_error)
    })
}

pub fn clear_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> Result<(), ServiceError> {
    sync_in_line(&scheduler, game_paths, || {
        let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
        storage_ops::storage_clear_sync_with_scheduler(Arc::clone(&scheduler), dir, MAX_TOTAL_BYTES)
            .map_err(sync_error)
    })
}

/// `getStorageInfoSync`, as the JSON the JS wrapper parses.
pub fn info_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> Result<String, ServiceError> {
    sync_in_line(&scheduler, game_paths, || {
        let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
        let info = storage_ops::storage_info_sync_with_scheduler(
            Arc::clone(&scheduler),
            dir,
            MAX_TOTAL_BYTES,
        )
        .map_err(sync_error)?;
        Ok(info_to_json(&info))
    })
}

// ---- Awaited: `StorageError` with summary and detail ----

/// The operations of one store, in the order they were asked for.
///
/// Every operation on a store -- write, read or info, synchronous or awaited --
/// takes its place here when it is called and runs after everything asked for
/// before it. A burst of `setStorage` calls used to leave an earlier value on disk
/// when the file-system pool's workers finished them out of order; and since only
/// the awaited writes were in line, a `setStorageSync` could be overwritten by an
/// earlier `setStorage` still waiting, and a `getStorage` or `getStorageSync` after
/// a `setStorage` could read the value from before it.
///
/// Queued work is dispatched to the pool by whoever finishes the item before it,
/// never by polling on the JS thread, so a synchronous call can wait for the work
/// ahead of it without waiting on the thread it is blocking. With nothing in line,
/// a synchronous call runs at once on the calling thread, as before.
struct Line {
    state: Mutex<LineState>,
    idle: Condvar,
}

#[derive(Default)]
struct LineState {
    queue: VecDeque<Job>,
    /// An item is queued for or running on a worker, or a synchronous call holds
    /// the store. Nothing else starts while it is set.
    busy: bool,
}

/// A queued item. It runs the store operation and returns the delivery of its
/// result, which happens after the line has moved on.
type Job = Box<dyn FnOnce() -> Delivery + Send>;
type Delivery = Box<dyn FnOnce() + Send>;

/// One line per storage directory, kept only while something holds it.
static LINES: LazyLock<Mutex<HashMap<PathBuf, Weak<Line>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn line(dir: &Path) -> Arc<Line> {
    let mut lines = LINES.lock();
    if let Some(line) = lines.get(dir).and_then(Weak::upgrade) {
        return line;
    }
    // Nothing holding a directory's line means nothing in it: its entry is dead
    // weight, and so are the ones for games that are no longer running.
    lines.retain(|_, line| line.strong_count() > 0);
    let line = Arc::new(Line {
        state: Mutex::new(LineState::default()),
        idle: Condvar::new(),
    });
    lines.insert(dir.to_path_buf(), Arc::downgrade(&line));
    line
}

impl Line {
    /// Queue `job` behind everything asked for before it; start the line if idle.
    fn push(self: &Arc<Self>, scheduler: &Arc<IoScheduler>, job: Job) {
        let mut state = self.state.lock();
        state.queue.push_back(job);
        if !state.busy {
            state.busy = true;
            drop(state);
            Runner::dispatch(Arc::clone(self), Arc::clone(scheduler));
        }
    }

    /// After an item or a synchronous call: start the next queued item, or go idle.
    fn advance(self: Arc<Self>, scheduler: &Arc<IoScheduler>) {
        let mut state = self.state.lock();
        if state.queue.is_empty() {
            state.busy = false;
            drop(state);
            self.idle.notify_all();
        } else {
            drop(state);
            Runner::dispatch(self, Arc::clone(scheduler));
        }
    }

    /// Run `call` in its place: once everything asked for before it is done.
    fn run_sync<T>(self: Arc<Self>, scheduler: &Arc<IoScheduler>, call: impl FnOnce() -> T) -> T {
        let mut state = self.state.lock();
        while state.busy {
            self.idle.wait(&mut state);
        }
        state.busy = true;
        drop(state);
        // Released even if `call` unwinds, so the line is never left held.
        struct Release(Option<Arc<Line>>, Arc<IoScheduler>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(line) = self.0.take() {
                    line.advance(&self.1);
                }
            }
        }
        let _release = Release(Some(self), Arc::clone(scheduler));
        call()
    }
}

/// Runs the next item of a line on a file-system worker.
///
/// Dropped without running -- the executor shut down with it queued -- it still
/// moves the line: the items left fail on the closed domain without touching the
/// store, so nothing waits on them forever.
struct Runner {
    line: Option<Arc<Line>>,
    scheduler: Arc<IoScheduler>,
}

impl Runner {
    fn dispatch(line: Arc<Line>, scheduler: Arc<IoScheduler>) {
        let runner = Runner {
            line: Some(line),
            scheduler: Arc::clone(&scheduler),
        };
        // A refused submission drops the runner, which drains the line here.
        let _ = scheduler.pools().submit_async(
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            move || runner.run(),
        );
    }

    fn run(mut self) {
        let Some(line) = self.line.take() else {
            return;
        };
        let job = line.state.lock().queue.pop_front();
        // Moves the line on even if the item unwinds.
        struct Advance(Option<Arc<Line>>, Arc<IoScheduler>);
        impl Drop for Advance {
            fn drop(&mut self) {
                if let Some(line) = self.0.take() {
                    line.advance(&self.1);
                }
            }
        }
        let advance = Advance(Some(line), Arc::clone(&self.scheduler));
        let delivery = job.map(|job| job());
        // The line has moved on, and this worker no longer holds it, before the
        // caller hears the result.
        drop(advance);
        if let Some(delivery) = delivery {
            delivery();
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let Some(line) = self.line.take() else {
            return;
        };
        // Every item sees the closed domain and only delivers its error.
        loop {
            let job = {
                let mut state = line.state.lock();
                match state.queue.pop_front() {
                    Some(job) => job,
                    None => {
                        state.busy = false;
                        break;
                    }
                }
            };
            job()();
        }
        line.idle.notify_all();
    }
}

/// Take a place in this store's line now, and run `f` in it on a file-system
/// worker.
///
/// The place is taken when this is called, not when the future is first polled:
/// the caller runs in the order the calls arrived, and a runtime that spawns the
/// futures may poll them in any order. The inputs are charged to the process byte
/// budget before the place is taken, and a refusal drops `f` here, unpolled. A
/// response dropped before its item starts withdraws it and returns its bytes; one
/// dropped after keeps them until the item finishes.
fn run_in_line<T, F>(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    retained_bytes: u64,
    f: F,
) -> impl Future<Output = Result<T, ServiceError>> + Send + use<T, F>
where
    T: Send + 'static,
    F: FnOnce(&Path) -> Result<T, EngineError> + Send + 'static,
{
    let placed = storage_paths(game_paths)
        .map_err(async_error)
        .and_then(|paths| {
            scheduler
                .ensure_open()
                .map_err(|error| storage_error(error.to_string()))?;
            let bytes = scheduler
                .pools()
                .reserve_bytes(retained_bytes)
                .map_err(|error| storage_error(error.to_string()))?;
            // Holds the bytes while the item waits. Whoever takes them first wins:
            // the item, when it starts, or the response, when it is dropped.
            let claim = Arc::new(Mutex::new(Some(bytes)));
            let (tx, rx) = oneshot::channel();
            let dir = paths.clone();
            let item_claim = Arc::clone(&claim);
            let domain = scheduler.domain();
            line(dir.directory()).push(
                &scheduler,
                Box::new(move || -> Delivery {
                    let Some(bytes) = item_claim.lock().take() else {
                        return Box::new(|| {});
                    };
                    let result = if domain.is_closed() {
                        Err(storage_error("IO worker pool closed"))
                    } else {
                        migo_io::storage_migration::prepare(&dir)
                            .and_then(|()| f(dir.directory()))
                            .map_err(async_error)
                    };
                    drop(bytes);
                    Box::new(move || {
                        let _ = tx.send(result);
                    })
                }),
            );
            Ok((rx, Withdraw(claim)))
        });
    async move {
        let (rx, withdraw) = placed?;
        let domain = scheduler.domain();
        let result = tokio::select! {
            biased;
            _ = domain.closed() => Err(storage_error("IO worker pool closed")),
            result = rx => result.unwrap_or_else(|_| Err(storage_error("IO worker pool closed"))),
        };
        drop(withdraw);
        result
    }
}

/// Withdraws a waiting item when its response is dropped, returning its bytes.
struct Withdraw(Arc<Mutex<Option<migo_io::pools::ByteTicket>>>);

impl Drop for Withdraw {
    fn drop(&mut self) {
        self.0.lock().take();
    }
}

/// The mutate-style awaited calls.
fn run_mutate<F>(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    retained_bytes: u64,
    f: F,
) -> impl Future<Output = Result<(), ServiceError>> + Send + use<F>
where
    F: FnOnce(&Path) -> Result<(), EngineError> + Send + 'static,
{
    run_in_line(scheduler, game_paths, retained_bytes, f)
}

/// Run a synchronous call in this store's line. Without a game there is no store,
/// and `call` reports that itself.
fn sync_in_line<T>(
    scheduler: &Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    call: impl FnOnce() -> T,
) -> T {
    match storage_paths(game_paths) {
        Ok(paths) => line(paths.directory()).run_sync(scheduler, call),
        Err(_) => call(),
    }
}

/// `getStorage`: read in its place, after every write asked for before it.
pub fn get(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: String,
) -> impl Future<Output = Result<String, ServiceError>> + Send + use<> {
    let retained_bytes = key.capacity() as u64;
    run_in_line(scheduler, game_paths, retained_bytes, move |dir| {
        storage_ops::storage_get(dir, &key, MAX_TOTAL_BYTES).map(Option::unwrap_or_default)
    })
}

pub fn set(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: String,
    value: String,
) -> impl Future<Output = Result<(), ServiceError>> + Send + use<> {
    // Validated, and the place in line taken, when this is called.
    let pending = check_value_and_key(&key, &value)
        .map_err(storage_error)
        .map(|()| {
            let retained_bytes = key.capacity() as u64 + value.capacity() as u64;
            run_mutate(scheduler, game_paths, retained_bytes, move |dir| {
                storage_ops::storage_set(dir, &key, &value, MAX_TOTAL_BYTES)
            })
        });
    async move { pending?.await }
}

pub fn remove(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: String,
) -> impl Future<Output = Result<(), ServiceError>> + Send + use<> {
    let retained_bytes = key.capacity() as u64;
    run_mutate(scheduler, game_paths, retained_bytes, move |dir| {
        storage_ops::storage_remove(dir, &key, MAX_TOTAL_BYTES)
    })
}

pub fn clear(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> impl Future<Output = Result<(), ServiceError>> + Send + use<> {
    run_mutate(scheduler, game_paths, 0, move |dir| {
        storage_ops::storage_clear(dir, MAX_TOTAL_BYTES)
    })
}

/// `getStorageInfo`, in its place in the store's line.
pub fn info(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> impl Future<Output = Result<String, ServiceError>> + Send + use<> {
    let info = run_in_line(scheduler, game_paths, 0, move |dir| {
        storage_ops::storage_info(dir, MAX_TOTAL_BYTES)
    });
    async move { info.await.map(|info| info_to_json(&info)) }
}

// ---- Buffer URLs ----

/// `URL.createObjectURL(buffer)`: write beneath the captured VFS cache root and
/// return an opaque virtual path, without exposing/reopening a host pathname.
pub fn create_buffer_url(vfs: Option<&VirtualFS>, buffer: &[u8]) -> Result<String, ServiceError> {
    let vfs = vfs.ok_or_else(|| {
        ServiceError::generic("buffer URLs are unavailable before a game is loaded")
    })?;
    let dir = vfs
        .write_directory(BUFFER_URL_ROOT, true)
        .map_err(|e| ServiceError::generic(format!("createBufferURL:fail {e}")))?;
    let name = create_buffer_url_in(
        &dir,
        || {
            let id = NEXT_BUFFER_URL
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| io::Error::other("buffer URL identifier exhausted"))?;
            Ok(format!("buffer-{}-{id:x}", std::process::id()))
        },
        |file| file.write_all(buffer),
    )
    .map_err(|e| ServiceError::generic(format!("createBufferURL:fail {e}")))?;
    Ok(format!("{BUFFER_URL_ROOT}/{name}"))
}

fn create_buffer_url_in(
    dir: &WriteDirectory,
    mut next_name: impl FnMut() -> io::Result<String>,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<String> {
    let (mut file, name) = loop {
        let name = next_name()?;
        match dir.create_new(&name) {
            Ok(file) => break (file, name),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        let mutations = shared::vfs::file_mutation::FileMutations::for_file(&file)?
            .ok_or_else(|| io::Error::other("buffer URL is not a regular file"))?;
        let _write = mutations.into_write()?;
        write(&mut file)
    })();
    if let Err(error) = result {
        drop(file);
        let _ = dir.remove_file(&name);
        return Err(error);
    }
    Ok(name)
}

/// `URL.revokeObjectURL(url)`. Deletes only inside the game's buffer URL
/// directory; anything else is not this game's to delete, and is ignored.
pub fn revoke_buffer_url(vfs: Option<&VirtualFS>, url: &str) -> Result<(), ServiceError> {
    let vfs = vfs.ok_or_else(|| {
        ServiceError::generic("buffer URLs are unavailable before a game is loaded")
    })?;
    let Some(name) = url
        .strip_prefix(BUFFER_URL_ROOT)
        .and_then(|tail| tail.strip_prefix('/'))
    else {
        return Ok(());
    };
    if let Ok(dir) = vfs.write_directory(BUFFER_URL_ROOT, false) {
        // The capability accepts one portable child name, so traversal, nested
        // paths and Windows aliases fail before native unlink. Missing or
        // replaced directories are a harmless best-effort revocation failure.
        let _ = dir.remove_file(name);
    }
    Ok(())
}

#[cfg(test)]
mod buffer_url_tests;

#[cfg(test)]
mod buffer_url_namespace_tests;

#[cfg(test)]
mod private_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    #[test]
    fn info_json_is_bounded_for_large_key_lists() {
        let info = StorageInfo {
            keys: (0..100)
                .map(|i| format!("key-{i}-{}", "x".repeat(8 * 1024)))
                .collect(),
            current_bytes: 1,
            limit_bytes: 10 * 1024 * 1024,
        };
        let json = info_to_json(&info);
        assert!(
            json.len() <= 512 * 1024,
            "getStorageInfo JSON must stay below the cap, got {} bytes",
            json.len()
        );
    }

    /// The JS wrapper `JSON.parse`s this and hands the object to content, so
    /// it has to be a whole JSON object -- the check #225 lacked when it dropped
    /// the opening `{"keys":[` and `getStorageInfo` threw on every platform.
    #[test]
    fn info_json_is_the_object_the_wrapper_parses() {
        let info = StorageInfo {
            keys: vec!["a".into(), "quote\"d".into(), "line\nbreak".into()],
            current_bytes: 1,
            limit_bytes: 10 * 1024 * 1024,
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&info_to_json(&info)).expect("getStorageInfo must be JSON");
        assert_eq!(
            parsed,
            serde_json::json!({
                "keys": ["a", "quote\"d", "line\nbreak"],
                "currentSize": 1,
                "limitSize": 10240,
            })
        );
        let truncated = StorageInfo {
            keys: (0..100)
                .map(|i| format!("key-{i}-{}", "x".repeat(8 * 1024)))
                .collect(),
            current_bytes: 1,
            limit_bytes: 1,
        };
        let parsed: serde_json::Value = serde_json::from_str(&info_to_json(&truncated))
            .expect("a truncated key list is still JSON");
        assert!(
            parsed["keys"]
                .as_array()
                .is_some_and(|keys| !keys.is_empty())
        );
    }

    #[test]
    fn storage_before_a_game_is_loaded_is_refused_in_each_shape() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let sync = get_sync(scheduler.clone(), None, "k").expect_err("no game");
        assert_eq!(sync.class, "Error");
        assert_eq!(
            sync.message,
            "storage is unavailable before a game is loaded"
        );
        let awaited = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(get(scheduler, None, "k".into()))
            .expect_err("no game");
        assert_eq!(awaited.class, CLASS_STORAGE_ERROR);
    }

    struct TestPaths {
        root: PathBuf,
        paths: GamePaths,
    }

    impl std::ops::Deref for TestPaths {
        type Target = GamePaths;

        fn deref(&self) -> &GamePaths {
            &self.paths
        }
    }

    impl Drop for TestPaths {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn paths(game: &str) -> TestPaths {
        static NEXT_PATH: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "migo-storage-order-tests-{}-{}",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ));
        let paths = GamePaths::new(root.join("files"), root.join("cache"), game, 1).expect("paths");
        paths.ensure_directories().expect("test directories");
        TestPaths { root, paths }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    use std::sync::mpsc;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(10);

    /// Queue an item that holds the store's line until `release` is sent. Items
    /// are dispatched when they are queued, so a test that needs others to wait
    /// holds the line with this first.
    fn hold(
        scheduler: &Arc<IoScheduler>,
        paths: &GamePaths,
        bytes: u64,
    ) -> (
        impl Future<Output = Result<(), ServiceError>> + use<>,
        mpsc::Sender<()>,
    ) {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let held = run_mutate(Arc::clone(scheduler), Some(paths), bytes, move |_| {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(WAIT);
            Ok(())
        });
        started_rx
            .recv_timeout(WAIT)
            .expect("the holding item started");
        (held, release_tx)
    }

    /// A burst of `setStorage` calls left an earlier value on disk: the writes ran on
    /// several workers and finished in whichever order. The last write for a key has
    /// to be the last one asked for, whatever the workers do and whatever order the
    /// futures are polled in.
    #[test]
    fn mutations_of_one_store_apply_in_the_order_they_were_asked_for() {
        const N: usize = 24;
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-order");
        let applied = Arc::new(Mutex::new(Vec::new()));
        // Asked for in order; each later one is quicker, so with nothing in line the
        // last would finish first.
        let writes: Vec<_> = (0..N)
            .map(|i| {
                let applied = Arc::clone(&applied);
                run_mutate(Arc::clone(&scheduler), Some(&paths), 0, move |_| {
                    std::thread::sleep(Duration::from_millis(((N - i) * 2) as u64));
                    applied.lock().push(i);
                    Ok(())
                })
            })
            .collect();
        // Polled in the opposite order: the place in line is taken when the future is
        // made, not when it is first polled.
        runtime().block_on(async move {
            let tasks: Vec<_> = writes.into_iter().rev().map(tokio::spawn).collect();
            for task in tasks {
                task.await.expect("joined").expect("applied");
            }
        });
        assert_eq!(*applied.lock(), (0..N).collect::<Vec<_>>());
    }

    /// A write whose response is dropped before it starts is withdrawn, and the ones
    /// behind it still run.
    #[test]
    fn a_dropped_write_is_withdrawn_and_does_not_stop_the_line() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-dropped");
        let applied = Arc::new(Mutex::new(Vec::new()));
        let write = |i: usize| {
            let applied = Arc::clone(&applied);
            run_mutate(Arc::clone(&scheduler), Some(&paths), 0, move |_| {
                applied.lock().push(i);
                Ok(())
            })
        };
        let (held, release) = hold(&scheduler, &paths, 0);
        let (first, second, third) = (write(0), write(1), write(2));
        drop(second);
        release.send(()).unwrap();
        runtime().block_on(async move {
            let all = async { held.await.and(first.await).and(third.await) };
            tokio::time::timeout(WAIT, all)
                .await
                .expect("the write behind a dropped one was not left waiting")
                .expect("applied");
        });
        assert_eq!(*applied.lock(), vec![0, 2]);
    }

    /// The bug the line exists for, across the two API shapes: a `setStorageSync`
    /// made after a `setStorage` that is still waiting used to run at once, and the
    /// earlier write then overwrote it.
    #[test]
    fn a_synchronous_write_waits_for_the_awaited_writes_asked_for_before_it() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = Arc::new(paths("storage-sync-after-async"));
        let (held, release) = hold(&scheduler, &paths, 0);
        let earlier = set(
            Arc::clone(&scheduler),
            Some(&paths),
            "k".into(),
            "awaited".into(),
        );
        let (done_tx, done_rx) = mpsc::channel();
        let sync_writer = {
            let (scheduler, paths) = (Arc::clone(&scheduler), Arc::clone(&paths));
            std::thread::spawn(move || {
                set_sync(scheduler, Some(&paths), "k", "synchronous").unwrap();
                done_tx.send(()).unwrap();
            })
        };
        let waited = done_rx.recv_timeout(Duration::from_millis(200)).is_err();
        release.send(()).unwrap();
        runtime()
            .block_on(async { held.await.and(earlier.await) })
            .unwrap();
        sync_writer.join().unwrap();
        assert!(
            waited,
            "the synchronous write ran ahead of the one asked for first"
        );
        assert_eq!(
            get_sync(Arc::clone(&scheduler), Some(&paths), "k").unwrap(),
            "synchronous"
        );
    }

    /// Reads are in line too: a read made after a write sees it, whichever shape
    /// either call has.
    #[test]
    fn reads_see_every_write_asked_for_before_them() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = Arc::new(paths("storage-read-after-write"));
        let (held, release) = hold(&scheduler, &paths, 0);
        let write = set(
            Arc::clone(&scheduler),
            Some(&paths),
            "k".into(),
            "written".into(),
        );
        let awaited_read = get(Arc::clone(&scheduler), Some(&paths), "k".into());
        let awaited_info = info(Arc::clone(&scheduler), Some(&paths));
        let sync_reader = {
            let (scheduler, paths) = (Arc::clone(&scheduler), Arc::clone(&paths));
            std::thread::spawn(move || get_sync(scheduler, Some(&paths), "k").unwrap())
        };
        let (read, info) = runtime().block_on(async {
            // Polled while the line is held, so a read that is not in line would
            // run now, ahead of the write.
            let (read, info) = (tokio::spawn(awaited_read), tokio::spawn(awaited_info));
            tokio::time::sleep(Duration::from_millis(50)).await;
            release.send(()).unwrap();
            held.await.unwrap();
            write.await.unwrap();
            (read.await.unwrap().unwrap(), info.await.unwrap().unwrap())
        });
        assert_eq!(read, "written");
        assert_eq!(sync_reader.join().unwrap(), "written");
        assert!(
            info.contains("\"k\""),
            "info missed the write before it: {info}"
        );
    }

    /// With nothing in line a synchronous call runs at once on its own thread, and
    /// leaves the line idle for the next.
    #[test]
    fn an_idle_line_lets_synchronous_calls_through_and_stays_idle() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-idle-sync");
        set_sync(Arc::clone(&scheduler), Some(&paths), "k", "v").unwrap();
        assert_eq!(
            get_sync(Arc::clone(&scheduler), Some(&paths), "k").unwrap(),
            "v"
        );
        let dir = storage_dir(Some(&paths)).unwrap();
        assert!(
            LINES.lock().get(&dir).and_then(Weak::upgrade).is_none(),
            "a synchronous call left its line held"
        );
    }

    #[test]
    fn dropping_a_middle_waiter_does_not_let_a_later_write_run_first() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-cancelled-middle");
        let (held, release) = hold(&scheduler, &paths, 0);
        let first = set(
            Arc::clone(&scheduler),
            Some(&paths),
            "k".into(),
            "first".into(),
        );
        let middle = set(
            Arc::clone(&scheduler),
            Some(&paths),
            "k".into(),
            "middle".into(),
        );
        let last = set(
            Arc::clone(&scheduler),
            Some(&paths),
            "k".into(),
            "last".into(),
        );
        drop(middle);
        release.send(()).unwrap();
        runtime().block_on(async {
            held.await.expect("held");
            let (a, b) = tokio::join!(first, last);
            a.expect("first write");
            b.expect("last write");
        });
        assert_eq!(get_sync(scheduler, Some(&paths), "k").unwrap(), "last");
    }

    #[test]
    fn cancelling_a_running_write_keeps_its_place_until_the_worker_finishes() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-cancelled-running");
        let dir = storage_dir(Some(&paths)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let first = run_mutate(Arc::clone(&scheduler), Some(&paths), 0, move |dir| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            let result = storage_ops::storage_set(dir, "k", "first", MAX_TOTAL_BYTES);
            let _ = done_tx.send(());
            result
        });
        started_rx.recv_timeout(WAIT).unwrap();
        drop(first);
        let still_held = LINES
            .lock()
            .get(&dir)
            .is_some_and(|line| line.strong_count() > 0);
        let ran_last = Arc::new(Mutex::new(false));
        let mut last = Box::pin({
            let ran_last = Arc::clone(&ran_last);
            run_mutate(Arc::clone(&scheduler), Some(&paths), 0, move |dir| {
                *ran_last.lock() = true;
                storage_ops::storage_set(dir, "k", "last", MAX_TOTAL_BYTES)
            })
        });
        let waiting = last.as_mut().now_or_never().is_none() && !*ran_last.lock();
        // Always release the worker before asserting the cancellation behavior.
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(WAIT).unwrap();
        runtime().block_on(last).expect("last write");
        assert!(still_held, "a running write must keep its line");
        assert!(waiting, "a dropped response let the next write start early");
        assert_eq!(get_sync(scheduler, Some(&paths), "k").unwrap(), "last");
        assert!(LINES.lock().get(&dir).and_then(Weak::upgrade).is_none());
    }

    #[test]
    fn waiting_mutations_charge_string_capacity_when_they_are_asked_for() {
        let mut key = String::with_capacity(64);
        key.push('k');
        let mut value = String::with_capacity(128);
        value.push('v');
        let retained = (key.capacity() + value.capacity()) as u64;
        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(1, 1, retained));
        let paths = paths("storage-pending-bytes");
        let (held, release) = hold(&scheduler, &paths, 0);
        let first = set(Arc::clone(&scheduler), Some(&paths), key, value);
        assert!(
            scheduler.pools().reserve_bytes(1).is_err(),
            "a mutation must charge retained capacity when it is asked for"
        );
        let refused = set(
            Arc::clone(&scheduler),
            Some(&paths),
            "another".into(),
            "value".into(),
        )
        .now_or_never()
        .expect("an over-budget mutation must not join the line")
        .expect_err("budget exhausted");
        assert_eq!(refused.class, CLASS_STORAGE_ERROR);
        assert!(refused.message.contains("pending-byte budget exhausted"));
        drop(first);
        drop(
            scheduler
                .pools()
                .reserve_bytes(retained)
                .expect("a withdrawn mutation returns its bytes"),
        );
        release.send(()).unwrap();
        runtime().block_on(held).expect("held");
        runtime()
            .block_on(set(
                Arc::clone(&scheduler),
                Some(&paths),
                "k".into(),
                "retry".into(),
            ))
            .expect("retry after released reservation");
        assert_eq!(get_sync(scheduler, Some(&paths), "k").unwrap(), "retry");
    }

    #[test]
    fn rejected_mutations_drop_their_capture_without_polling() {
        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(1, 1, 1));
        let paths = paths("storage-rejected-capture");
        let payload = Arc::new(vec![0_u8; 2]);
        let weak = Arc::downgrade(&payload);
        let rejected = run_mutate(scheduler, Some(&paths), 2, move |_| {
            drop(payload);
            Ok(())
        });
        assert!(
            weak.upgrade().is_none(),
            "a rejected mutation must drop its payload before the first poll"
        );
        assert!(rejected.now_or_never().unwrap().is_err());
    }

    #[test]
    fn cancelled_responses_keep_running_mutation_bytes_until_completion() {
        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(1, 1, 16));
        let paths = paths("storage-running-bytes");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = run_mutate(Arc::clone(&scheduler), Some(&paths), 8, move |dir| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT).unwrap();
            storage_ops::storage_set(dir, "k", "first", MAX_TOTAL_BYTES)
        });
        started_rx.recv_timeout(WAIT).unwrap();
        let waiting = run_mutate(Arc::clone(&scheduler), Some(&paths), 8, |_| Ok(()));
        let full = scheduler.pools().reserve_bytes(1).is_err();
        drop(waiting);
        let waiting_returned = scheduler.pools().reserve_bytes(8).is_ok();
        drop(first);
        let running_still_charged = scheduler.pools().reserve_bytes(9).is_err();
        // Release the worker before asserting so a failing test cannot strand it.
        release_tx.send(()).unwrap();
        runtime()
            .block_on(run_mutate(Arc::clone(&scheduler), Some(&paths), 0, |_| {
                Ok(())
            }))
            .expect("wait for the accepted write to finish");
        assert!(full, "waiting mutation inputs must share the byte budget");
        assert!(waiting_returned, "a dropped waiter must return its bytes");
        assert!(
            running_still_charged,
            "a cancelled response must keep accepted mutation bytes charged"
        );
        drop(scheduler.pools().reserve_bytes(16).unwrap());
        assert_eq!(get_sync(scheduler, Some(&paths), "k").unwrap(), "first");
    }

    #[test]
    fn host_close_wakes_items_waiting_in_line_and_they_do_not_run() {
        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(1, 1, 16));
        let paths = paths("storage-waiting-close");
        let (held, release) = hold(&scheduler, &paths, 8);
        let last = run_mutate(Arc::clone(&scheduler), Some(&paths), 8, |_| {
            panic!("an item queued before a host close must not run")
        });
        scheduler.close();
        let error = runtime()
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), last).await })
            .expect("host close must wake a storage waiter")
            .expect_err("closed host");
        assert_eq!(error.class, CLASS_STORAGE_ERROR);
        assert_eq!(error.message, "IO worker pool closed");
        drop(scheduler.pools().reserve_bytes(8).unwrap());
        // The held item was already running: its response returns on the close,
        // and its bytes when the worker finishes it.
        release.send(()).unwrap();
        let _ = runtime().block_on(held);
        let deadline = std::time::Instant::now() + WAIT;
        while scheduler.pools().reserve_bytes(16).is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "a finished item kept its bytes"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn creation_order_is_reserved_even_with_an_exhausted_cooperative_budget() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-cooperative-order");
        runtime().block_on(async move {
            tokio::spawn(async move {
                let permits = tokio::sync::Semaphore::new(1);
                for _ in 0..1024 {
                    if !tokio::task::coop::has_budget_remaining() {
                        break;
                    }
                    drop(permits.acquire().await.unwrap());
                }
                assert!(!tokio::task::coop::has_budget_remaining());
                let first = set(
                    Arc::clone(&scheduler),
                    Some(&paths),
                    "k".into(),
                    "first".into(),
                );
                let last = set(
                    Arc::clone(&scheduler),
                    Some(&paths),
                    "k".into(),
                    "last".into(),
                );
                tokio::task::yield_now().await;
                let (a, b) = tokio::join!(last, first);
                a.expect("last write");
                b.expect("first write");
                assert_eq!(get_sync(scheduler, Some(&paths), "k").unwrap(), "last");
            })
            .await
            .expect("joined ordering test");
        });
    }

    /// The line is per store: another game's writes do not wait behind this one's.
    #[test]
    fn two_stores_do_not_wait_for_each_other() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let (one, two) = (paths("storage-one"), paths("storage-two"));
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let blocked = run_mutate(Arc::clone(&scheduler), Some(&one), 0, move |_| {
            // Holds the first store's line until the second store's write has run.
            done_rx.recv_timeout(WAIT).map_err(|_| {
                EngineError::from_detail(ErrorCode::Internal, "the other store waited".to_string())
            })
        });
        let free = run_mutate(scheduler, Some(&two), 0, move |_| {
            let _ = done_tx.send(());
            Ok(())
        });
        runtime().block_on(async move {
            let (a, b) = tokio::join!(tokio::spawn(blocked), tokio::spawn(free));
            a.expect("joined").expect("the first store's write");
            b.expect("joined").expect("the second store's write");
        });
    }

    /// Nothing in line means no entry kept: a launcher that runs many games does not
    /// collect a line per game.
    #[test]
    fn a_finished_line_leaves_nothing_behind() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-pruned");
        runtime()
            .block_on(run_mutate(scheduler, Some(&paths), 0, |_| Ok(())))
            .expect("applied");
        let dir = storage_dir(Some(&paths)).expect("dir");
        assert!(
            LINES.lock().get(&dir).and_then(Weak::upgrade).is_none(),
            "a line with nothing in it is not kept alive"
        );
    }

    #[test]
    fn an_oversized_value_is_refused_with_the_message_games_match_on() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let error = set_sync(scheduler, None, "k", &"x".repeat(MAX_VALUE_SIZE + 1))
            .expect_err("over the value limit");
        assert_eq!(error.message, "setStorage:fail data exceeds max size");
    }
}
