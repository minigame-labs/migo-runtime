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
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{Extension, JsBuffer, OpState, op2};
use deno_error::JsErrorBox;
use migo_services::storage;
use shared::op_state::HostOpState;

use crate::io_state::IoSchedulerState;

/// The per-game quota, re-exported for the two-Session quota test so it spends
/// the *shipped* limit rather than one of its own.
#[cfg(test)]
pub(crate) const MAX_TOTAL_BYTES: u64 = storage::MAX_TOTAL_BYTES;

#[inline]
fn get_scheduler(state: &OpState) -> Arc<migo_io::scheduler::IoScheduler> {
    state.borrow::<IoSchedulerState>().0.clone()
}

/// The game's paths, if a game is loaded. Cloned out of op state so an awaited
/// call does not hold a borrow across its await.
#[inline]
fn game_paths(state: &OpState) -> Option<Arc<shared::vfs::GamePaths>> {
    state.borrow::<HostOpState>().game_paths.clone()
}

/// The storage root the ops resolve for the game this op state has loaded:
/// the production resolver over this state's `game_paths`, which is what the
/// isolation tests put their questions to.
#[cfg(test)]
pub(crate) fn storage_dir(
    state: &OpState,
) -> Result<std::path::PathBuf, shared::error::EngineError> {
    storage::storage_dir(game_paths(state).as_deref())
}

/// As [`storage_dir`], for buffer URLs.
#[cfg(test)]
pub(crate) fn buffer_url_dir(
    state: &OpState,
) -> Result<std::path::PathBuf, shared::error::EngineError> {
    storage::buffer_url_dir(game_paths(state).as_deref())
}

/// A service error as the class content has always caught.
fn js_err(error: migo_services::ServiceError) -> JsErrorBox {
    JsErrorBox::new(error.class, error.message)
}

/// Registered in JS as a constructor under this exact name (see
/// `01_storage.js`); the awaited ops throw it.
#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum StorageError {
    #[class("StorageError")]
    #[error("{0}")]
    Message(String),
}

fn storage_err(error: migo_services::ServiceError) -> StorageError {
    StorageError::Message(error.message)
}

// The rules and messages are `migo_services::storage`'s; these are adapters,
// so the external session's service dispatcher applies the same ones.

// ==================== Sync Storage Ops ====================

#[op2]
#[string]
pub fn op_storage_get(state: &mut OpState, #[string] key: &str) -> Result<String, JsErrorBox> {
    storage::get_sync(get_scheduler(state), game_paths(state).as_deref(), key).map_err(js_err)
}

#[op2(fast)]
pub fn op_storage_set(
    state: &mut OpState,
    #[string] key: &str,
    #[string] value: &str,
) -> Result<(), JsErrorBox> {
    storage::set_sync(
        get_scheduler(state),
        game_paths(state).as_deref(),
        key,
        value,
    )
    .map_err(js_err)
}

#[op2(fast)]
pub fn op_storage_remove(state: &mut OpState, #[string] key: &str) -> Result<(), JsErrorBox> {
    storage::remove_sync(get_scheduler(state), game_paths(state).as_deref(), key).map_err(js_err)
}

#[op2(fast)]
pub fn op_storage_clear(state: &mut OpState) -> Result<(), JsErrorBox> {
    storage::clear_sync(get_scheduler(state), game_paths(state).as_deref()).map_err(js_err)
}

#[op2]
#[string]
pub fn op_storage_info(state: &mut OpState) -> Result<String, JsErrorBox> {
    storage::info_sync(get_scheduler(state), game_paths(state).as_deref()).map_err(js_err)
}

// ==================== Async Storage Ops ====================

fn scheduler_and_paths(
    state: &Rc<RefCell<OpState>>,
) -> (
    Arc<migo_io::scheduler::IoScheduler>,
    Option<Arc<shared::vfs::GamePaths>>,
) {
    let st = state.borrow();
    (get_scheduler(&st), game_paths(&st))
}

#[op2(async(lazy), fast)]
#[string]
pub async fn op_storage_get_async(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
) -> Result<String, StorageError> {
    let (scheduler, paths) = scheduler_and_paths(&state);
    storage::get(scheduler, paths.as_deref(), key)
        .await
        .map_err(storage_err)
}

#[op2(async(lazy), fast)]
pub async fn op_storage_set_async(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
    #[string] value: String,
) -> Result<(), StorageError> {
    let (scheduler, paths) = scheduler_and_paths(&state);
    storage::set(scheduler, paths.as_deref(), key, value)
        .await
        .map_err(storage_err)
}

#[op2(async(lazy), fast)]
pub async fn op_storage_remove_async(
    state: Rc<RefCell<OpState>>,
    #[string] key: String,
) -> Result<(), StorageError> {
    let (scheduler, paths) = scheduler_and_paths(&state);
    storage::remove(scheduler, paths.as_deref(), key)
        .await
        .map_err(storage_err)
}

#[op2(async(lazy), fast)]
pub async fn op_storage_clear_async(state: Rc<RefCell<OpState>>) -> Result<(), StorageError> {
    let (scheduler, paths) = scheduler_and_paths(&state);
    storage::clear(scheduler, paths.as_deref())
        .await
        .map_err(storage_err)
}

#[op2(async(lazy), fast)]
#[string]
pub async fn op_storage_info_async(state: Rc<RefCell<OpState>>) -> Result<String, StorageError> {
    let (scheduler, paths) = scheduler_and_paths(&state);
    storage::info(scheduler, paths.as_deref())
        .await
        .map_err(storage_err)
}

// ==================== Buffer URL Ops ====================

#[op2]
#[string]
pub fn op_create_buffer_url(
    state: &mut OpState,
    #[buffer] buffer: JsBuffer,
) -> Result<String, JsErrorBox> {
    storage::create_buffer_url(game_paths(state).as_deref(), &buffer).map_err(js_err)
}

#[op2(fast)]
pub fn op_revoke_buffer_url(state: &mut OpState, #[string] url: &str) -> Result<(), JsErrorBox> {
    storage::revoke_buffer_url(game_paths(state).as_deref(), url).map_err(js_err)
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
