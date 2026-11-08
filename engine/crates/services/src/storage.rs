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

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Weak};

use futures::FutureExt;
use migo_io::scheduler::IoScheduler;
use migo_io::storage_ops::{self, StorageInfo};
use migo_io::task::{IoRequest, PriorityClass, RequestKind};
use parking_lot::Mutex;
use shared::error::{EngineError, ErrorCode};
use shared::vfs::{
    GamePaths, VirtualFS, game_paths::StoragePaths, write_directory::WriteDirectory,
};

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

async fn prepare_async_dir(
    scheduler: &IoScheduler,
    game_paths: Option<&GamePaths>,
) -> Result<PathBuf, EngineError> {
    let paths = storage_paths(game_paths)?;
    if !paths.is_prepared() {
        let owned = paths.clone();
        let request = IoRequest::StorageMutate {
            request: RequestKind::Async,
            priority: PriorityClass::from(RequestKind::Async),
        };
        scheduler
            .run_async(request, move || migo_io::storage_migration::prepare(&owned))
            .await
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
    let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
    storage_ops::storage_get_sync_with_scheduler(scheduler, dir, key.to_string(), MAX_TOTAL_BYTES)
        .map(Option::unwrap_or_default)
        .map_err(sync_error)
}

pub fn set_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: &str,
    value: &str,
) -> Result<(), ServiceError> {
    check_value_and_key(key, value).map_err(ServiceError::generic)?;
    let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
    storage_ops::storage_set_sync_with_scheduler(
        scheduler,
        dir,
        key.to_string(),
        value.to_string(),
        MAX_TOTAL_BYTES,
    )
    .map_err(sync_error)
}

pub fn remove_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: &str,
) -> Result<(), ServiceError> {
    let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
    storage_ops::storage_remove_sync_with_scheduler(
        scheduler,
        dir,
        key.to_string(),
        MAX_TOTAL_BYTES,
    )
    .map_err(sync_error)
}

pub fn clear_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> Result<(), ServiceError> {
    let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
    storage_ops::storage_clear_sync_with_scheduler(scheduler, dir, MAX_TOTAL_BYTES)
        .map_err(sync_error)
}

/// `getStorageInfoSync`, as the JSON the JS wrapper parses.
pub fn info_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> Result<String, ServiceError> {
    let dir = prepare_sync_dir(&scheduler, game_paths).map_err(sync_error)?;
    let info = storage_ops::storage_info_sync_with_scheduler(scheduler, dir, MAX_TOTAL_BYTES)
        .map_err(sync_error)?;
    Ok(info_to_json(&info))
}

// ---- Awaited: `StorageError` with summary and detail ----

/// The mutations of one store, in the order they were asked for.
///
/// A mutation is a blocking SQLite write routed to the scheduler's file-system
/// pool, which has several workers, so two writes to the same key can run at
/// once and finish in either order: a burst of `setStorage` calls -- an autosave
/// -- left an earlier value on disk. Which write is last is a property of the
/// calls, not of which worker was quicker, so each mutation waits for the one
/// asked for before it.
///
/// Tokio's FIFO mutex keeps later mutations behind the active one even when a
/// middle waiter is dropped. Waiting uses no filesystem worker. Once dispatched,
/// the blocking closure owns the guard until it finishes, including when its
/// caller stops awaiting the result.
type MutationChain = tokio::sync::Mutex<()>;

/// One chain per storage directory, kept only while something is in line.
static MUTATION_CHAINS: LazyLock<Mutex<HashMap<PathBuf, Weak<MutationChain>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn mutation_chain(dir: &Path) -> Arc<MutationChain> {
    let mut chains = MUTATION_CHAINS.lock();
    if let Some(chain) = chains.get(dir).and_then(Weak::upgrade) {
        return chain;
    }
    // Nothing in flight for a directory means nothing to wait for: its entry is
    // dead weight, and so are the ones for games that are no longer running.
    chains.retain(|_, chain| chain.strong_count() > 0);
    let chain = Arc::new(MutationChain::default());
    chains.insert(dir.to_path_buf(), Arc::downgrade(&chain));
    chain
}

/// Route a blocking KvStore call through the IoScheduler as an async task. The
/// mutate-style calls share this shape.
///
/// Mutations of one directory apply in the order this was called. The place in
/// line is taken here, when the future is made, and not when it is first polled:
/// the caller that builds the future runs in the order the calls arrived, and a
/// runtime that spawns them may poll them in any order.
fn run_mutate<F>(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    retained_bytes: u64,
    f: F,
) -> impl Future<Output = Result<(), ServiceError>> + Send + use<F>
where
    F: FnOnce(&Path) -> Result<(), EngineError> + Send + 'static,
{
    let place = storage_paths(game_paths)
        .map_err(async_error)
        .and_then(|paths| {
            scheduler
                .ensure_open()
                .map_err(|error| storage_error(error.to_string()))?;
            // Waiting for this directory's turn retains the inputs too. Use
            // the existing process budget before taking a place in line, and
            // keep its ticket through the accepted blocking job. Rejection
            // drops `f` here, even if the returned future is never polled.
            let bytes = scheduler
                .pools()
                .reserve_bytes(retained_bytes)
                .map_err(|error| storage_error(error.to_string()))?;
            let dir = paths.clone();
            let mut wait = Box::pin(mutation_chain(dir.directory()).lock_owned());
            // Register in call order before returning the future. Bypass the
            // cooperative budget for this one lock poll so an exhausted caller
            // cannot defer registration until after a later mutation. The
            // pinned waiter is retained when pending; normal awaits remain
            // cooperative and can remove cancelled waiters from Tokio's queue.
            let ready = tokio::task::unconstrained(wait.as_mut()).now_or_never();
            Ok((dir, wait, ready, bytes, f))
        });
    async move {
        let (dir, wait, ready, bytes, f) = place?;
        let turn = match ready {
            Some(turn) => turn,
            None => {
                let domain = scheduler.domain();
                tokio::select! {
                    biased;
                    _ = domain.closed() => {
                        return Err(storage_error("IO worker pool closed"));
                    }
                    turn = wait => turn,
                }
            }
        };
        scheduler
            .run_async(
                IoRequest::StorageMutate {
                    request: RequestKind::Async,
                    priority: PriorityClass::from(RequestKind::Async),
                },
                move || {
                    // This owned guard also keeps the weak registry entry alive
                    // while an accepted job is queued or running.
                    let _turn = turn;
                    let _bytes = bytes;
                    migo_io::storage_migration::prepare(&dir)
                        .and_then(|()| f(dir.directory()))
                        .map_err(async_error)
                },
            )
            .await
            .map_err(|error| storage_error(error.to_string()))?
    }
}

pub async fn get(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: String,
) -> Result<String, ServiceError> {
    let dir = prepare_async_dir(&scheduler, game_paths)
        .await
        .map_err(async_error)?;
    storage_ops::storage_get_with_scheduler(
        scheduler,
        dir,
        key,
        MAX_TOTAL_BYTES,
        RequestKind::Async,
    )
    .await
    .map(Option::unwrap_or_default)
    .map_err(async_error)
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

pub async fn info(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> Result<String, ServiceError> {
    let dir = prepare_async_dir(&scheduler, game_paths)
        .await
        .map_err(async_error)?;
    let info = scheduler
        .run_async(
            IoRequest::StorageInfo {
                request: RequestKind::Async,
                priority: PriorityClass::from(RequestKind::Async),
            },
            move || storage_ops::storage_info(&dir, MAX_TOTAL_BYTES).map_err(async_error),
        )
        .await
        .map_err(|error| storage_error(error.to_string()))??;
    Ok(info_to_json(&info))
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
                    std::thread::sleep(std::time::Duration::from_millis(((N - i) * 2) as u64));
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

    /// A write that is dropped before it runs does not stop the ones behind it.
    #[test]
    fn a_dropped_write_does_not_stop_the_line() {
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
        let (first, second, third) = (write(0), write(1), write(2));
        drop(second);
        runtime().block_on(async move {
            let both = async { first.await.and(third.await) };
            tokio::time::timeout(std::time::Duration::from_secs(10), both)
                .await
                .expect("the write behind a dropped one was not left waiting")
                .expect("applied");
        });
        assert_eq!(*applied.lock(), vec![0, 2]);
    }

    #[test]
    fn dropping_a_middle_waiter_does_not_dispatch_a_later_write_first() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-cancelled-middle");
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
        let mut last = Box::pin(set(
            Arc::clone(&scheduler),
            Some(&paths),
            "k".into(),
            "last".into(),
        ));
        drop(middle);
        assert!(last.as_mut().now_or_never().is_none());
        assert_eq!(
            scheduler.metrics().delegated_runs,
            0,
            "a cancelled middle waiter must not let a later write dispatch first"
        );
        runtime().block_on(async {
            let (a, b) = tokio::join!(first, last);
            a.expect("first write");
            b.expect("last write");
        });
        assert_eq!(get_sync(scheduler, Some(&paths), "k").unwrap(), "last");
    }

    #[test]
    fn cancelling_a_running_write_keeps_its_gate_until_the_worker_finishes() {
        use std::sync::mpsc;
        use std::time::Duration;

        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-cancelled-running");
        let dir = storage_dir(Some(&paths)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let mut first = Box::pin(run_mutate(
            Arc::clone(&scheduler),
            Some(&paths),
            0,
            move |dir| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                let result = storage_ops::storage_set(dir, "k", "first", MAX_TOTAL_BYTES);
                let _ = done_tx.send(());
                result
            },
        ));
        assert!(first.as_mut().now_or_never().is_none());
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        drop(first);
        let still_owned = MUTATION_CHAINS
            .lock()
            .get(&dir)
            .is_some_and(|chain| chain.strong_count() > 0);
        let mut last = Box::pin(set(
            Arc::clone(&scheduler),
            Some(&paths),
            "k".into(),
            "last".into(),
        ));
        let completed = last.as_mut().now_or_never();
        let waiting = completed.is_none();
        let dispatched = scheduler.metrics().delegated_runs;
        // Always release the worker before asserting the cancellation behavior.
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        match completed {
            Some(result) => result.expect("last write"),
            None => runtime().block_on(last).expect("last write"),
        }
        assert!(
            still_owned,
            "a running mutation must keep its directory gate alive"
        );
        assert!(waiting);
        assert_eq!(dispatched, 1, "cancelled response released an active write");
        assert_eq!(get_sync(scheduler, Some(&paths), "k").unwrap(), "last");
        assert!(
            MUTATION_CHAINS
                .lock()
                .get(&dir)
                .and_then(Weak::upgrade)
                .is_none()
        );
    }

    #[test]
    fn waiting_mutations_charge_string_capacity_before_the_first_poll() {
        let mut key = String::with_capacity(64);
        key.push('k');
        let mut value = String::with_capacity(128);
        value.push('v');
        let retained = (key.capacity() + value.capacity()) as u64;
        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(1, 1, retained));
        let paths = paths("storage-pending-bytes");
        let first = set(Arc::clone(&scheduler), Some(&paths), key, value);
        assert!(
            scheduler.pools().reserve_bytes(1).is_err(),
            "a mutation must charge retained capacity before it is polled"
        );
        let refused = set(
            Arc::clone(&scheduler),
            Some(&paths),
            "another".into(),
            "value".into(),
        )
        .now_or_never()
        .expect("over-budget mutation must not join the directory queue")
        .expect_err("budget exhausted");
        assert_eq!(refused.class, CLASS_STORAGE_ERROR);
        assert!(refused.message.contains("pending-byte budget exhausted"));
        assert_eq!(scheduler.metrics().delegated_runs, 0);
        assert!(
            !storage_dir(Some(&paths))
                .unwrap()
                .join("storage.db")
                .exists()
        );
        drop(first);
        drop(scheduler.pools().reserve_bytes(retained).unwrap());
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
        use std::sync::mpsc;
        use std::time::Duration;

        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(1, 1, 16));
        let paths = paths("storage-running-bytes");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut first = Box::pin(run_mutate(
            Arc::clone(&scheduler),
            Some(&paths),
            8,
            move |dir| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                storage_ops::storage_set(dir, "k", "first", MAX_TOTAL_BYTES)
            },
        ));
        assert!(first.as_mut().now_or_never().is_none());
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
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
    fn host_close_wakes_mutations_waiting_for_their_directory() {
        use std::time::Duration;

        let scheduler = Arc::new(IoScheduler::isolated_for_admission_probe(1, 1, 16));
        let paths = paths("storage-waiting-close");
        let first = run_mutate(Arc::clone(&scheduler), Some(&paths), 8, |_| Ok(()));
        let mut last = Box::pin(run_mutate(Arc::clone(&scheduler), Some(&paths), 8, |_| {
            panic!("closed waiter must not run")
        }));
        assert!(last.as_mut().now_or_never().is_none());
        scheduler.close();
        let error = runtime()
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), last).await })
            .expect("host close must wake a storage waiter")
            .expect_err("closed host");
        assert_eq!(error.class, CLASS_STORAGE_ERROR);
        assert_eq!(error.message, "IO worker pool closed");
        assert_eq!(scheduler.metrics().delegated_runs, 0);
        drop(scheduler.pools().reserve_bytes(8).unwrap());
        drop(first);
        drop(scheduler.pools().reserve_bytes(16).unwrap());
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
                let mut last = Box::pin(set(
                    Arc::clone(&scheduler),
                    Some(&paths),
                    "k".into(),
                    "last".into(),
                ));
                tokio::task::yield_now().await;
                assert!(last.as_mut().now_or_never().is_none());
                assert_eq!(
                    scheduler.metrics().delegated_runs,
                    0,
                    "creation order must survive an exhausted cooperative budget"
                );
                let (a, b) = tokio::join!(first, last);
                a.expect("first write");
                b.expect("last write");
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
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let blocked = run_mutate(Arc::clone(&scheduler), Some(&one), 0, move |_| {
            // Holds the first store's line until the second store's write has run.
            done_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| {
                    EngineError::from_detail(
                        ErrorCode::Internal,
                        "the other store waited".to_string(),
                    )
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
    /// collect a chain per game.
    #[test]
    fn a_finished_line_leaves_nothing_behind() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let paths = paths("storage-pruned");
        runtime()
            .block_on(run_mutate(scheduler, Some(&paths), 0, |_| Ok(())))
            .expect("applied");
        let dir = storage_dir(Some(&paths)).expect("dir");
        assert!(
            MUTATION_CHAINS
                .lock()
                .get(&dir)
                .and_then(Weak::upgrade)
                .is_none(),
            "a chain with nothing in flight is not kept alive"
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
