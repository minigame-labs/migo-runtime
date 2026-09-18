//! Key-value storage and buffer URLs.
//!
//! Backed by an embedded WAL-mode SQLite database opened lazily per game at
//! `{game user_data_dir}/kv_storage/storage.db` -- per game, not per host app;
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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use migo_io::scheduler::IoScheduler;
use migo_io::storage_ops::{self, StorageInfo};
use migo_io::task::{IoRequest, PriorityClass, RequestKind};
use shared::error::{EngineError, ErrorCode};
use shared::vfs::GamePaths;

use crate::error::{ServiceError, detail_or_message, message_and_detail};

/// Storage directory name under the game's user-data directory.
///
/// The SQLite file itself lives at `{STORAGE_DIR}/storage.db`; the directory
/// layer is kept so per-game cleanup tools that `rm -rf` the folder keep
/// working.
pub const STORAGE_DIR: &str = "kv_storage";

/// Buffer URL directory name under the game's sandbox cache directory.
pub const BUFFER_URL_DIR: &str = "buffer_urls";

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
/// Anchored to the game's own user-data directory, not to the host app's files
/// directory. A host that runs several games would otherwise give all of them
/// one SQLite file: any game could read another's saves by guessing keys, a
/// single `migo.clearStorage()` would wipe the whole catalogue, and the 10 MB
/// quota would be a shared pool one game could exhaust for the rest.
///
/// Fails when no game is loaded rather than falling back to a shared location:
/// a fallback would silently reintroduce the shared file it exists to prevent.
pub fn storage_dir(game_paths: Option<&GamePaths>) -> Result<PathBuf, EngineError> {
    match game_paths {
        Some(paths) => Ok(paths.user_data_dir().join(STORAGE_DIR)),
        None => Err(EngineError::from_detail(
            ErrorCode::InvalidOperation,
            "storage is unavailable before a game is loaded".to_string(),
        )),
    }
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
    let dir = storage_dir(game_paths).map_err(sync_error)?;
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
    let dir = storage_dir(game_paths).map_err(sync_error)?;
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
    let dir = storage_dir(game_paths).map_err(sync_error)?;
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
    let dir = storage_dir(game_paths).map_err(sync_error)?;
    storage_ops::storage_clear_sync_with_scheduler(scheduler, dir, MAX_TOTAL_BYTES)
        .map_err(sync_error)
}

/// `getStorageInfoSync`, as the JSON the JS wrapper parses.
pub fn info_sync(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> Result<String, ServiceError> {
    let dir = storage_dir(game_paths).map_err(sync_error)?;
    let info = storage_ops::storage_info_sync_with_scheduler(scheduler, dir, MAX_TOTAL_BYTES)
        .map_err(sync_error)?;
    Ok(info_to_json(&info))
}

// ---- Awaited: `StorageError` with summary and detail ----

/// Route a blocking KvStore call through the IoScheduler as an async task. The
/// four mutate-style calls share this shape.
async fn run_mutate<F>(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    f: F,
) -> Result<(), ServiceError>
where
    F: FnOnce(&Path) -> Result<(), EngineError> + Send + 'static,
{
    let dir = storage_dir(game_paths).map_err(async_error)?;
    scheduler
        .run_async(
            IoRequest::StorageMutate {
                request: RequestKind::Async,
                priority: PriorityClass::from(RequestKind::Async),
            },
            move || f(&dir).map_err(async_error),
        )
        .await
        .map_err(|error| storage_error(error.to_string()))?
}

pub async fn get(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: String,
) -> Result<String, ServiceError> {
    let dir = storage_dir(game_paths).map_err(async_error)?;
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

pub async fn set(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: String,
    value: String,
) -> Result<(), ServiceError> {
    check_value_and_key(&key, &value).map_err(storage_error)?;
    run_mutate(scheduler, game_paths, move |dir| {
        storage_ops::storage_set(dir, &key, &value, MAX_TOTAL_BYTES)
    })
    .await
}

pub async fn remove(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
    key: String,
) -> Result<(), ServiceError> {
    run_mutate(scheduler, game_paths, move |dir| {
        storage_ops::storage_remove(dir, &key, MAX_TOTAL_BYTES)
    })
    .await
}

pub async fn clear(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> Result<(), ServiceError> {
    run_mutate(scheduler, game_paths, move |dir| {
        storage_ops::storage_clear(dir, MAX_TOTAL_BYTES)
    })
    .await
}

pub async fn info(
    scheduler: Arc<IoScheduler>,
    game_paths: Option<&GamePaths>,
) -> Result<String, ServiceError> {
    let dir = storage_dir(game_paths).map_err(async_error)?;
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

/// `URL.createObjectURL(buffer)`: write the bytes to the game's scratch space
/// and answer with the path.
pub fn create_buffer_url(
    game_paths: Option<&GamePaths>,
    buffer: &[u8],
) -> Result<String, ServiceError> {
    let dir = buffer_url_dir(game_paths).map_err(sync_error)?;
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .map_err(|e| ServiceError::generic(format!("storage: mkdir fail {e}")))?;
    }
    // Unique file name: nanosecond timestamp in hex.
    let id = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{nanos:x}")
    };
    let path = dir.join(&id);
    fs::write(&path, buffer)
        .map_err(|e| ServiceError::generic(format!("createBufferURL:fail {e}")))?;
    Ok(path.to_string_lossy().into_owned())
}

/// `URL.revokeObjectURL(url)`. Deletes only inside the game's buffer URL
/// directory; anything else is not this game's to delete, and is ignored.
pub fn revoke_buffer_url(game_paths: Option<&GamePaths>, url: &str) -> Result<(), ServiceError> {
    let dir = buffer_url_dir(game_paths).map_err(sync_error)?;
    let path = Path::new(url);
    if path.starts_with(&dir) && path.is_file() {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn an_oversized_value_is_refused_with_the_message_games_match_on() {
        let scheduler = Arc::new(IoScheduler::new(1));
        let error = set_sync(scheduler, None, "k", &"x".repeat(MAX_VALUE_SIZE + 1))
            .expect_err("over the value limit");
        assert_eq!(error.message, "setStorage:fail data exceeds max size");
    }
}
