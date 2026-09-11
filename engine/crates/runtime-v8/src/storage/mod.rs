//! Key-value storage ops and buffer URL management.
//!
//! Backed by an embedded WAL-mode SQLite database opened lazily per
//! session at `{game user_data_dir}/kv_storage/storage.db` -- per game, not
//! per host app; see [`storage_dir`].  The on-disk
//! layout (a single `storage.db`) is a full replacement of the
//! previous `file-per-key` hex-named layout — see
//! [`migo_io::storage_ops`] and [`migo_io::kv_store`] for the rationale and
//! schema.
//!
//! ## Limits
//!
//! - Single value: 1 MB
//! - Total charged storage: 10 MB (value bytes, key bytes, and the fixed
//!   per-entry SQLite record/B-tree reservation)
//! - Key bytes: 16 KiB
//! - Entries: 10,000
//!
//! The charged-storage quota deliberately excludes transient WAL frames and
//! SQLite page-cache pages, which are bounded by SQLite configuration rather
//! than committed row content. The secondary index's duplicate key bytes are
//! likewise explicitly excluded; the key-size and entry-count guards bound
//! that configuration-dependent cost without inventing a multiplier.
//!
//! Charged-storage quota and entry count are enforced inside each SQLite
//! transaction using cached running totals; enumeration is paged and the
//! serialized response is capped before it reaches JS.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{Extension, JsBuffer, OpState, op2};
use deno_error::JsErrorBox;
use migo_io::storage_ops::{self, StorageInfo};
use migo_io::task::{IoRequest, PriorityClass, RequestKind};
use shared::error::{EngineError, ErrorCode};
use shared::op_state::HostOpState;

use crate::io_state::IoSchedulerState;

/// Storage directory name under the game's user-data directory.
///
/// The SQLite file itself lives at `{STORAGE_DIR}/storage.db`; the
/// directory layer is kept so per-game cleanup tools that `rm -rf`
/// the folder keep working.
const STORAGE_DIR: &str = "kv_storage";

/// Buffer URL directory name under the game's cache directory.
const BUFFER_URL_DIR: &str = "buffer_urls";

/// Maximum size of a single stored value (1 MB).
const MAX_VALUE_SIZE: usize = 1024 * 1024;

/// Keep oversized keys off the scheduler queue as well as out of SQLite.
const MAX_KEY_SIZE: usize = 16 * 1024;

/// Maximum total storage size in KB (10 MB = 10240 KB).
const LIMIT_SIZE_KB: u32 = 10240;

/// Maximum total storage size in bytes.
///
/// Visible to the crate so the two-Session quota test spends the *shipped* limit
/// rather than one of its own: a fixture with a small quota of its own would prove
/// per-instance accounting without proving that this is the number each game gets.
pub(crate) const MAX_TOTAL_BYTES: u64 = LIMIT_SIZE_KB as u64 * 1024;

/// Cap the wire string built for `getStorageInfo`; the JS wrapper parses this
/// whole string before exposing it, so truncating only after formatting would
/// leave the peak allocation unchanged.
const MAX_INFO_JSON_BYTES: usize = 512 * 1024;

// ==================== Path Helpers ====================

/// Per-game storage root.
///
/// Anchored to the game's own user-data directory, not to the host app's files
/// directory. A host that runs several games -- a game centre with a catalogue
/// of third-party titles is the case this product is sold into -- would
/// otherwise give all of them one SQLite file: any game could read another's
/// saves by guessing keys, a single `migo.clearStorage()` would wipe the whole
/// catalogue, and the 10 MB quota would be a shared pool one game could
/// exhaust for the rest. Code, cache and user-data directories were already
/// per-game; this was the one that was not.
///
/// It also restores the common mini-game platform's own semantics, where each mini-game has its own 10 MB.
///
/// Fails when no game is loaded rather than falling back to a shared location:
/// `game_paths` is populated when a module is evaluated, so content cannot be
/// running without it, and a fallback would silently reintroduce the shared
/// file it exists to prevent.
#[inline]
pub(crate) fn storage_dir(state: &OpState) -> Result<PathBuf, EngineError> {
    let host = state.borrow::<HostOpState>();
    match host.game_paths.as_ref() {
        Some(paths) => Ok(paths.user_data_dir().join(STORAGE_DIR)),
        None => Err(EngineError::from_detail(
            ErrorCode::InvalidOperation,
            "storage is unavailable before a game is loaded".to_string(),
        )),
    }
}

/// Per-game scratch space for `URL.createObjectURL` payloads.
///
/// Same isolation as [`storage_dir`], for the same reason: these are files
/// written on behalf of one game, and the host app's cache directory is shared
/// by every game it runs.
///
/// Inside the sandbox subtree rather than the cache root: the payload is the
/// game's own bytes handed back to it, so the path this returns has to stay one
/// the game can read, and the root is reserved for runtime state.
#[inline]
pub(crate) fn buffer_url_dir(state: &OpState) -> Result<PathBuf, EngineError> {
    let host = state.borrow::<HostOpState>();
    match host.game_paths.as_ref() {
        Some(paths) => Ok(paths.sandbox_cache_dir().join(BUFFER_URL_DIR)),
        None => Err(EngineError::from_detail(
            ErrorCode::InvalidOperation,
            "buffer URLs are unavailable before a game is loaded".to_string(),
        )),
    }
}

#[inline]
fn get_scheduler(state: &OpState) -> Arc<migo_io::scheduler::IoScheduler> {
    state.borrow::<IoSchedulerState>().0.clone()
}

#[inline]
fn pool_err(err: migo_io::pools::PoolError) -> StorageError {
    StorageError::Message(err.to_string())
}

fn ensure_dir(dir: &std::path::Path) -> Result<(), JsErrorBox> {
    if !dir.exists() {
        fs::create_dir_all(dir)
            .map_err(|e| JsErrorBox::generic(format!("storage: mkdir fail {e}")))?;
    }
    Ok(())
}

/// Convert an engine-layer error to the JS-visible message. Keeps
/// the user-facing string equivalent to the old ops so existing
/// error-match code in games keeps working.
fn js_err(e: EngineError) -> JsErrorBox {
    match &e.detail {
        Some(d) => JsErrorBox::generic(d.clone()),
        None => JsErrorBox::generic(e.msg.to_string()),
    }
}
/// Serialize [`StorageInfo`] into the JSON shape expected by the JS wrapper.
/// Sizes are reported in KiB (ceil), mirroring the legacy format.
fn info_to_json(info: &StorageInfo) -> String {
    // Ceil to KiB so a 1-byte value reports currentSize=1.
    let current_kib = (info.current_bytes + 1023) / 1024;
    let limit_kib = (info.limit_bytes + 1023) / 1024;
    let suffix = format!(r#"],"currentSize":{current_kib},"limitSize":{limit_kib}}}"#);
    let mut out =
        String::with_capacity(MAX_INFO_JSON_BYTES.min(info.keys.len().saturating_mul(16)));
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

// ==================== Sync Storage Ops ====================

#[op2]
#[string]
pub fn op_storage_get(state: &mut OpState, #[string] key: &str) -> Result<String, JsErrorBox> {
    let scheduler = get_scheduler(state);
    let dir = storage_dir(state).map_err(js_err)?;
    // Missing key maps to "" so the JS-side `deserialize("")` contract
    // (return empty string) keeps working without a wire-format change.
    storage_ops::storage_get_sync_with_scheduler(scheduler, dir, key.to_string(), MAX_TOTAL_BYTES)
        .map(|opt| opt.unwrap_or_default())
        .map_err(js_err)
}

#[op2(fast)]
pub fn op_storage_set(
    state: &mut OpState,
    #[string] key: &str,
    #[string] value: &str,
) -> Result<(), JsErrorBox> {
    if value.len() > MAX_VALUE_SIZE {
        return Err(JsErrorBox::generic("setStorage:fail data exceeds max size"));
    }
    if key.len() > MAX_KEY_SIZE {
        return Err(JsErrorBox::generic("setStorage:fail key exceeds max size"));
    }
    let scheduler = get_scheduler(state);
    let dir = storage_dir(state).map_err(js_err)?;
    storage_ops::storage_set_sync_with_scheduler(
        scheduler,
        dir,
        key.to_string(),
        value.to_string(),
        MAX_TOTAL_BYTES,
    )
    .map_err(js_err)
}

#[op2(fast)]
pub fn op_storage_remove(state: &mut OpState, #[string] key: &str) -> Result<(), JsErrorBox> {
    let scheduler = get_scheduler(state);
    let dir = storage_dir(state).map_err(js_err)?;
    storage_ops::storage_remove_sync_with_scheduler(
        scheduler,
        dir,
        key.to_string(),
        MAX_TOTAL_BYTES,
    )
    .map_err(js_err)
}

#[op2(fast)]
pub fn op_storage_clear(state: &mut OpState) -> Result<(), JsErrorBox> {
    let scheduler = get_scheduler(state);
    let dir = storage_dir(state).map_err(js_err)?;
    storage_ops::storage_clear_sync_with_scheduler(scheduler, dir, MAX_TOTAL_BYTES).map_err(js_err)
}

#[op2]
#[string]
pub fn op_storage_info(state: &mut OpState) -> Result<String, JsErrorBox> {
    let scheduler = get_scheduler(state);
    let dir = storage_dir(state).map_err(js_err)?;
    let info = storage_ops::storage_info_sync_with_scheduler(scheduler, dir, MAX_TOTAL_BYTES)
        .map_err(js_err)?;
    Ok(info_to_json(&info))
}

// ==================== Async Storage Ops ====================

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum StorageError {
    #[class("StorageError")]
    #[error("{0}")]
    Message(String),
}

impl From<EngineError> for StorageError {
    #[inline]
    fn from(e: EngineError) -> Self {
        match &e.detail {
            Some(d) => StorageError::Message(format!("{} ({})", e.msg, d)),
            None => StorageError::Message(e.msg.to_string()),
        }
    }
}

/// Route a blocking KvStore call through the IoScheduler as an async
/// task. All four mutate-style async ops share this shape, so folding
/// the boilerplate into one helper keeps the call sites obvious.
async fn run_mutate_async<F>(state: Rc<RefCell<OpState>>, f: F) -> Result<(), StorageError>
where
    F: FnOnce(&std::path::Path) -> Result<(), EngineError> + Send + 'static,
{
    let (scheduler, dir) = {
        let st = state.borrow();
        (get_scheduler(&st), storage_dir(&st)?)
    };
    scheduler
        .run_async(
            IoRequest::StorageMutate {
                request: RequestKind::Async,
                priority: PriorityClass::from(RequestKind::Async),
            },
            move || f(&dir).map_err(StorageError::from),
        )
        .await
        .map_err(pool_err)?
}

#[op2(async(lazy), fast)]
#[string]
pub async fn op_storage_get_async(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
) -> Result<String, StorageError> {
    let (scheduler, dir) = {
        let st = state.borrow();
        (get_scheduler(&st), storage_dir(&st)?)
    };
    storage_ops::storage_get_with_scheduler(
        scheduler,
        dir,
        key,
        MAX_TOTAL_BYTES,
        RequestKind::Async,
    )
    .await
    .map(|opt| opt.unwrap_or_default())
    .map_err(StorageError::from)
}

#[op2(async(lazy), fast)]
pub async fn op_storage_set_async(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
    #[string] value: String,
) -> Result<(), StorageError> {
    if value.len() > MAX_VALUE_SIZE {
        return Err(StorageError::Message(
            "setStorage:fail data exceeds max size".into(),
        ));
    }
    if key.len() > MAX_KEY_SIZE {
        return Err(StorageError::Message(
            "setStorage:fail key exceeds max size".into(),
        ));
    }
    run_mutate_async(state, move |dir| {
        storage_ops::storage_set(dir, &key, &value, MAX_TOTAL_BYTES)
    })
    .await
}

#[op2(async(lazy), fast)]
pub async fn op_storage_remove_async(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
) -> Result<(), StorageError> {
    run_mutate_async(state, move |dir| {
        storage_ops::storage_remove(dir, &key, MAX_TOTAL_BYTES)
    })
    .await
}

#[op2(async(lazy), fast)]
pub async fn op_storage_clear_async(state: Rc<RefCell<OpState>>) -> Result<(), StorageError> {
    run_mutate_async(state, move |dir| {
        storage_ops::storage_clear(dir, MAX_TOTAL_BYTES)
    })
    .await
}

#[op2(async(lazy), fast)]
#[string]
pub async fn op_storage_info_async(state: Rc<RefCell<OpState>>) -> Result<String, StorageError> {
    let (scheduler, dir) = {
        let st = state.borrow();
        (get_scheduler(&st), storage_dir(&st)?)
    };
    let info = scheduler
        .run_async(
            IoRequest::StorageInfo {
                request: RequestKind::Async,
                priority: PriorityClass::from(RequestKind::Async),
            },
            move || storage_ops::storage_info(&dir, MAX_TOTAL_BYTES).map_err(StorageError::from),
        )
        .await
        .map_err(pool_err)??;
    Ok(info_to_json(&info))
}

// ==================== Buffer URL Ops ====================

#[op2]
#[string]
pub fn op_create_buffer_url(
    state: &mut OpState,
    #[buffer] buffer: JsBuffer,
) -> Result<String, JsErrorBox> {
    let dir = buffer_url_dir(state).map_err(js_err)?;
    ensure_dir(&dir)?;

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
    fs::write(&path, &*buffer)
        .map_err(|e| JsErrorBox::generic(format!("createBufferURL:fail {e}")))?;

    Ok(path.to_string_lossy().into_owned())
}

#[op2(fast)]
pub fn op_revoke_buffer_url(state: &mut OpState, #[string] url: &str) -> Result<(), JsErrorBox> {
    let dir = buffer_url_dir(state).map_err(js_err)?;
    let path = std::path::Path::new(url);
    // Only allow deleting files within the buffer URL directory.
    if path.starts_with(&dir) && path.is_file() {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

// ==================== Extension ====================

deno_core::extension!(
    host_v8_storage,
    deps = [host_v8_base],
    ops = [
        op_storage_get,
        op_storage_set,
        op_storage_remove,
        op_storage_clear,
        op_storage_info,
        op_storage_get_async,
        op_storage_set_async,
        op_storage_remove_async,
        op_storage_clear_async,
        op_storage_info_async,
        op_create_buffer_url,
        op_revoke_buffer_url,
    ],
    esm = [
        dir "src/storage",
        "01_storage.js",
    ],
);

pub fn storage_extensions() -> Vec<Extension> {
    vec![host_v8_storage::init()]
}

pub fn storage_lazy_extensions() -> Vec<Extension> {
    vec![host_v8_storage::lazy_init()]
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
}
