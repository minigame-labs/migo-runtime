//! Key-value storage backed by an embedded SQLite database.
//!
//! This replaces the old `file-per-key` layout. Why SQLite:
//!
//! * **Batched writes** are one transaction instead of N rename+fsync
//!   pairs — measured >10× on the `setStorageBatch` fast path.
//! * **Quota** is O(1): the running charged-footprint total is cached in
//!   `_meta` under key `total_bytes` and updated inside every write
//!   transaction, so `set` / `set_batch` / `info` never scan the `kv` table.
//! * **Crash-safety** comes from WAL + `synchronous=NORMAL`, which
//!   is the combination SQLite itself recommends for KV workloads.
//!   We never lose a committed write on power loss; at worst the
//!   last un-checkpointed WAL frame is replayed on startup.
//! * **Schema migrations** are a pragma bump, not a home-grown
//!   header format.
//!
//! # Threading
//!
//! `KvStore` is cheaply `Clone` (it wraps an `Arc<Mutex<...>>`) and
//! safe to share across threads. Internally every operation takes a
//! short mutex around a single `rusqlite::Connection`. SQLite's own
//! WAL allows concurrent readers with one writer, but using a single
//! Rust `Connection` is simpler, avoids pool bookkeeping, and matches
//! the expected traffic (sub-ms transactions, hundreds of ops/s at
//! peak).  The mutex is released around any blocking fsync via
//! `synchronous=NORMAL`, which only fsyncs at checkpoint time.
//!
//! # Schema
//!
//! ```sql
//! CREATE TABLE kv (
//!     k          TEXT PRIMARY KEY,
//!     v          TEXT NOT NULL,
//!     size       INTEGER NOT NULL,   -- byte length of v, cached
//!     updated_at INTEGER NOT NULL    -- millis since epoch
//! ) WITHOUT ROWID;
//!
//! CREATE INDEX kv_updated ON kv(updated_at);
//!
//! CREATE TABLE _meta (
//!     k TEXT PRIMARY KEY,
//!     v TEXT NOT NULL
//! ) WITHOUT ROWID;
//! ```
//!
//! `WITHOUT ROWID` cuts the per-row overhead by one btree and is the
//! idiomatic shape for a string-keyed KV table.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use shared::error::{EngineError, ErrorCode};

/// Current schema version. Bump on incompatible migrations; the
/// constructor runs `migrate_to_current` and refuses to open a DB
/// newer than `SCHEMA_VERSION`.
const SCHEMA_VERSION: i64 = 2;
/// Maximum UTF-8 bytes in one key. Keys participate in the WITHOUT ROWID
/// primary b-tree and the `kv_updated` index, so the value quota alone cannot
/// bound their storage cost.
const MAX_KEY_BYTES: usize = 16 * 1024;

/// Explicit per-entry reservation for SQLite record and B-tree structure.
///
/// This is a fixed, deterministic charge for the cell pointer, record-header
/// varints, and the stored `size`/`updated_at` fields; it is deliberately a
/// reservation rather than an `encoded * multiplier` estimate. The
/// `kv_updated` index's duplicate key bytes are explicitly excluded because
/// index layout is an SQLite/configuration detail; the key-length guard still
/// bounds that uncharged index cost. WAL frames and SQLite's page cache are
/// also excluded: WAL is transient and bounded by `wal_autocheckpoint`, while
/// page-cache pages are memory-only and bounded by SQLite's configured cache.
const PER_ENTRY_OVERHEAD_BYTES: u64 = 64;

#[inline]
fn charged_bytes(key: &str, value_bytes: u64) -> u64 {
    (key.len() as u64)
        .saturating_add(value_bytes)
        .saturating_add(PER_ENTRY_OVERHEAD_BYTES)
}

/// Maximum number of rows in one store, independent of value bytes.
const MAX_ENTRY_COUNT: u64 = 10_000;

/// Maximum rows returned by one unpaged `info` call. `info_page` can be used
/// by internal callers to consume a larger key set in bounded slices.
const MAX_INFO_KEYS: usize = 1_024;

/// Summary returned by [`KvStore::info`].
///
/// Mirrors the JS-visible `getStorageInfo` response exactly so the
/// upper layer can serialise without another round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvInfo {
    pub keys: Vec<String>,
    pub current_bytes: u64,
    pub limit_bytes: u64,
}

/// Open handle to the KV database.
#[derive(Clone)]
pub struct KvStore {
    inner: Arc<Mutex<Inner>>,
}

// Custom Debug: `rusqlite::Connection` is not Debug.  We only expose
// fields that are cheap to format and safe to log.
impl std::fmt::Debug for KvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.inner.lock();
        f.debug_struct("KvStore")
            .field("path", &g.path)
            .field("quota_bytes", &g.quota_bytes)
            .finish()
    }
}

struct Inner {
    conn: Connection,
    path: PathBuf,
    quota_bytes: u64,
    /// Running charged footprint across all rows in `kv` (value bytes, key
    /// bytes, and the fixed per-entry reservation). Loaded once at open time
    /// from `_meta.total_bytes` (or reconciled on first use if missing /
    /// corrupt) and updated inside every write transaction. This is the only
    /// source-of-truth for quota checks; no read path does `SUM`.
    total_bytes: u64,
    /// Running row count, maintained in the same transaction as
    /// `total_bytes` so entry admission remains O(1).
    total_entries: u64,
}
const META_TOTAL_BYTES: &str = "total_bytes";
const META_TOTAL_ENTRIES: &str = "total_entries";

impl KvStore {
    /// Number of strong handles, including the cache's own handle.
    ///
    /// This is crate-visible so the storage cache can distinguish an
    /// idle entry from one still held by an operation/session without
    /// exposing `Arc` internals in the public API.
    pub(crate) fn strong_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    /// Open (creating if missing) the KV DB at `path`.
    ///
    /// * `quota_bytes` — charged storage cap in bytes (values, keys, and the
    ///   fixed per-entry reservation).
    ///
    /// Initialisation is idempotent; multiple processes on the same
    /// file is **not** supported (SQLite would permit it with
    /// locking but we don't guarantee correctness of the cached
    /// totals across processes).
    pub fn open(path: impl AsRef<Path>, quota_bytes: u64) -> Result<Self, EngineError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                EngineError::new(ErrorCode::IoError)
                    .with_msg("kv: mkdir parent failed")
                    .with_detail(format!("{}: {}", parent.display(), e))
            })?;
        }

        let mut conn = Connection::open(&path).map_err(sql_err("kv: open"))?;

        // WAL + NORMAL sync is SQLite's recommended KV config:
        //   - WAL gives concurrent reads during writes and is faster
        //     than rollback journal for small transactions.
        //   - synchronous=NORMAL fsyncs at checkpoint boundaries only;
        //     committed writes survive app crashes, and only a power
        //     failure between commit and checkpoint can lose (at most)
        //     the last un-checkpointed frames. Acceptable for KV.
        //   - temp_store=MEMORY keeps transient B-tree pages in RAM.
        //   - wal_autocheckpoint=1000 (pages) is the default; keep it
        //     explicit so future sqlite upgrades can't silently change.
        // Wait rather than fail instantly when another connection holds a lock.
        // SQLite's default is a zero timeout, so any contention -- a checkpoint,
        // or a second process opening the same file -- surfaces as an immediate
        // `database is locked` rather than a short wait. Set before the first
        // pragma, since `journal_mode` is itself a lock-taking statement.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql_err("kv: busy_timeout"))?;

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(sql_err("kv: pragma journal_mode"))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(sql_err("kv: pragma synchronous"))?;
        conn.pragma_update(None, "temp_store", "MEMORY")
            .map_err(sql_err("kv: pragma temp_store"))?;
        conn.pragma_update(None, "wal_autocheckpoint", 1000)
            .map_err(sql_err("kv: pragma wal_autocheckpoint"))?;
        // `foreign_keys` is off by default anyway, but make it
        // explicit so a future ALTER doesn't silently enable it.
        conn.pragma_update(None, "foreign_keys", "OFF")
            .map_err(sql_err("kv: pragma foreign_keys"))?;

        migrate_to_current(&mut conn)?;

        // Load (or reconcile) the cached total so every write/read path after
        // this is O(1). If `_meta.total_bytes` is missing, we do a single
        // O(N) charged-footprint reconciliation and persist the result; this
        // is the only place in the KV store that scans `kv`.
        let total_bytes = load_or_reconcile_total(&conn)?;
        let total_entries = load_or_reconcile_entries(&conn)?;

        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                conn,
                path,
                quota_bytes,
                total_bytes,
                total_entries,
            })),
        })
    }

    /// Read the value for `key`, or `Ok(None)` if absent.
    pub fn get(&self, key: &str) -> Result<Option<String>, EngineError> {
        let started = std::time::Instant::now();
        let g = self.inner.lock();
        let out = g
            .conn
            .query_row("SELECT v FROM kv WHERE k = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(sql_err("kv: get"));
        shared::stats::io_metrics_global()
            .record_op(shared::stats::OpClass::StorageGet, started.elapsed());
        out
    }

    /// Write `value` under `key`, honouring the configured quota.
    ///
    /// Quota accounting counts **the new total** after replacement
    /// (old key's size is deducted), so repeatedly overwriting a
    /// single key never inflates the total.  If the write would
    /// exceed the quota the call returns `ResourceExhausted` and
    /// the DB is untouched.
    pub fn set(&self, key: &str, value: &str) -> Result<(), EngineError> {
        let started = std::time::Instant::now();
        let result = self.set_inner(key, value);
        shared::stats::io_metrics_global()
            .record_op(shared::stats::OpClass::StorageSet, started.elapsed());
        result
    }

    fn set_inner(&self, key: &str, value: &str) -> Result<(), EngineError> {
        if key.len() > MAX_KEY_BYTES {
            return Err(EngineError::new(ErrorCode::InvalidArgument)
                .with_msg("setStorage:fail key exceeds max size")
                .with_detail(format!(
                    "key length {} bytes > limit {} bytes",
                    key.len(),
                    MAX_KEY_BYTES
                )));
        }
        let mut g = self.inner.lock();
        let quota = g.quota_bytes;
        let current_total = g.total_bytes;
        let current_entries = g.total_entries;
        let tx = g
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_err("kv: begin"))?;

        let old_size: Option<i64> = tx
            .query_row("SELECT size FROM kv WHERE k = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(sql_err("kv: set: read old"))?;
        let new_size = value.len() as i64;
        let projected_entries = current_entries.saturating_add(u64::from(old_size.is_none()));
        if projected_entries > MAX_ENTRY_COUNT {
            return Err(EngineError::new(ErrorCode::OutOfMemory)
                .with_msg("setStorage:fail storage entry limit exceeded")
                .with_detail(format!(
                    "projected {} entries > limit {}",
                    projected_entries, MAX_ENTRY_COUNT
                )));
        }
        // `total_bytes` is always non-negative; saturating_sub avoids
        // panic on a hypothetical corrupt row with negative size. The key
        // and fixed per-entry reservation are part of each row's charge.
        let old_charged = old_size
            .map(|size| charged_bytes(key, size.max(0) as u64))
            .unwrap_or(0);
        let projected = current_total
            .saturating_sub(old_charged)
            .saturating_add(charged_bytes(key, new_size as u64));
        if projected > quota {
            return Err(EngineError::new(ErrorCode::OutOfMemory)
                .with_msg("setStorage:fail storage limit exceeded")
                .with_detail(format!(
                    "projected {} bytes > quota {} bytes",
                    projected, quota
                )));
        }

        tx.execute(
            "INSERT INTO kv(k, v, size, updated_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(k) DO UPDATE SET v = excluded.v, size = excluded.size, \
             updated_at = excluded.updated_at",
            params![key, value, new_size, now_ms()],
        )
        .map_err(sql_err("kv: set: upsert"))?;
        persist_total_in_tx(&tx, projected)?;
        persist_entries_in_tx(&tx, projected_entries)?;
        tx.commit().map_err(sql_err("kv: set: commit"))?;
        // Update caches only after the commit succeeded; otherwise a
        // failed commit would leave cached totals ahead of the DB.
        g.total_bytes = projected;
        g.total_entries = projected_entries;
        Ok(())
    }

    /// Atomic batch set. All inputs land or none do — including the
    /// quota check, which is evaluated against the *final* projected
    /// total so a batch can overwrite existing keys without a
    /// transient overflow.
    pub fn set_batch(&self, items: &[(&str, &str)]) -> Result<(), EngineError> {
        if items.is_empty() {
            return Ok(());
        }
        for (key, _) in items {
            if key.len() > MAX_KEY_BYTES {
                return Err(EngineError::new(ErrorCode::InvalidArgument)
                    .with_msg("setStorageBatch:fail key exceeds max size")
                    .with_detail(format!(
                        "key length {} bytes > limit {} bytes",
                        key.len(),
                        MAX_KEY_BYTES
                    )));
            }
        }

        let mut g = self.inner.lock();
        let quota = g.quota_bytes;
        let current_total = g.total_bytes;
        let current_entries = g.total_entries;
        let tx = g
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_err("kv: batch: begin"))?;

        // Project the new total against the cached value. We must
        // look up each incoming key's old size to handle overwrites
        // correctly, but we never rescan the table for a running total.
        //
        // A key repeated inside one batch is projected against what the
        // previous item in this batch will leave behind.
        let mut projected: i128 = current_total as i128;
        let mut projected_entries: i128 = current_entries as i128;
        {
            let mut q = tx
                .prepare_cached("SELECT size FROM kv WHERE k = ?1")
                .map_err(sql_err("kv: batch: prep size"))?;
            let mut pending: HashMap<&str, u64> = HashMap::with_capacity(items.len());
            for (k, v) in items {
                let old = match pending.get(k) {
                    Some(&staged) => staged,
                    None => {
                        let stored: Option<i64> = q
                            .query_row([k], |r| r.get(0))
                            .optional()
                            .map_err(sql_err("kv: batch: size"))?;
                        if stored.is_none() {
                            projected_entries += 1;
                        }
                        stored
                            .map(|size| charged_bytes(k, size.max(0) as u64))
                            .unwrap_or(0)
                    }
                };
                let new = charged_bytes(k, v.len() as u64);
                projected = projected - old as i128 + new as i128;
                pending.insert(k, new);
            }
        }

        if projected_entries < 0
            || projected_entries as u128 > MAX_ENTRY_COUNT as u128
            || projected < 0
            || projected as u128 > quota as u128
        {
            let detail = if projected_entries as u128 > MAX_ENTRY_COUNT as u128 {
                format!(
                    "projected {} entries > limit {}",
                    projected_entries, MAX_ENTRY_COUNT
                )
            } else {
                format!("projected {} bytes > quota {} bytes", projected, quota)
            };
            return Err(EngineError::new(ErrorCode::OutOfMemory)
                .with_msg(if projected_entries as u128 > MAX_ENTRY_COUNT as u128 {
                    "setStorageBatch:fail storage entry limit exceeded"
                } else {
                    "setStorageBatch:fail storage limit exceeded"
                })
                .with_detail(detail));
        }
        let projected = projected as u64;
        let projected_entries = projected_entries as u64;

        {
            let mut up = tx
                .prepare_cached(
                    "INSERT INTO kv(k, v, size, updated_at) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v, size = excluded.size, \
                 updated_at = excluded.updated_at",
                )
                .map_err(sql_err("kv: batch: prep upsert"))?;
            let ts = now_ms();
            for (k, v) in items {
                up.execute(params![k, v, v.len() as i64, ts])
                    .map_err(sql_err("kv: batch: upsert"))?;
            }
        }
        persist_total_in_tx(&tx, projected)?;
        persist_entries_in_tx(&tx, projected_entries)?;
        tx.commit().map_err(sql_err("kv: batch: commit"))?;
        g.total_bytes = projected;
        g.total_entries = projected_entries;
        Ok(())
    }

    /// Remove `key` if present. No error on missing key.
    pub fn remove(&self, key: &str) -> Result<(), EngineError> {
        let mut g = self.inner.lock();
        let current_total = g.total_bytes;
        let current_entries = g.total_entries;
        let tx = g
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_err("kv: remove: begin"))?;
        let old_size: Option<i64> = tx
            .query_row("SELECT size FROM kv WHERE k = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(sql_err("kv: remove: read old"))?;
        tx.execute("DELETE FROM kv WHERE k = ?1", [key])
            .map_err(sql_err("kv: remove"))?;
        let old_charged = old_size
            .map(|size| charged_bytes(key, size.max(0) as u64))
            .unwrap_or(0);
        let new_total = current_total.saturating_sub(old_charged);
        let new_entries = current_entries.saturating_sub(u64::from(old_size.is_some()));
        persist_total_in_tx(&tx, new_total)?;
        persist_entries_in_tx(&tx, new_entries)?;
        tx.commit().map_err(sql_err("kv: remove: commit"))?;
        g.total_bytes = new_total;
        g.total_entries = new_entries;
        Ok(())
    }

    /// Remove every row. Much faster than N `DELETE` statements
    /// because SQLite takes the truncate-optimisation path when a
    /// WHERE clause is absent.
    pub fn clear(&self) -> Result<(), EngineError> {
        let mut g = self.inner.lock();
        let tx = g
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_err("kv: clear: begin"))?;
        tx.execute("DELETE FROM kv", [])
            .map_err(sql_err("kv: clear"))?;
        persist_total_in_tx(&tx, 0)?;
        persist_entries_in_tx(&tx, 0)?;
        tx.commit().map_err(sql_err("kv: clear: commit"))?;
        g.total_bytes = 0;
        g.total_entries = 0;
        Ok(())
    }

    /// List a bounded first page of keys and the summary totals.
    ///
    /// Ordering is deterministic (`updated_at DESC, k ASC`) so callers that
    /// diff consecutive snapshots see stable output. Use `info_page` to
    /// consume additional pages without materializing the complete list.
    pub fn info(&self) -> Result<KvInfo, EngineError> {
        let started = std::time::Instant::now();
        let result = self.info_page(0, MAX_INFO_KEYS);
        shared::stats::io_metrics_global()
            .record_op(shared::stats::OpClass::StorageInfo, started.elapsed());
        result
    }

    pub fn info_page(&self, offset: usize, limit: usize) -> Result<KvInfo, EngineError> {
        let g = self.inner.lock();
        let limit = limit.min(MAX_INFO_KEYS);
        let mut stmt = g
            .conn
            .prepare(
                "SELECT k FROM kv ORDER BY updated_at DESC, k ASC \
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(sql_err("kv: info: prepare"))?;
        let rows = stmt
            .query_map(
                params![limit as i64, offset.min(i64::MAX as usize) as i64],
                |r| r.get::<_, String>(0),
            )
            .map_err(sql_err("kv: info: query"))?;
        let mut keys = Vec::with_capacity(limit);
        for row in rows {
            keys.push(row.map_err(sql_err("kv: info: row"))?);
        }
        Ok(KvInfo {
            keys,
            current_bytes: g.total_bytes,
            limit_bytes: g.quota_bytes,
        })
    }

    /// Test-only: force a WAL checkpoint. Production never needs
    /// this; SQLite's autocheckpoint handles it. `wal_checkpoint`
    /// returns a 3-integer row (busy, log_pages, ckpt_pages) so we
    /// must use a `query_row`, not `execute`.
    #[cfg(test)]
    fn checkpoint(&self) -> Result<(), EngineError> {
        let g = self.inner.lock();
        g.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .map_err(sql_err("kv: checkpoint"))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Schema migration
// ---------------------------------------------------------------------------

fn migrate_to_current(conn: &mut Connection) -> Result<(), EngineError> {
    let mut current: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_err("kv: read user_version"))?;
    if current > SCHEMA_VERSION {
        return Err(EngineError::new(ErrorCode::Unsupported)
            .with_msg("kv: db schema is newer than this binary")
            .with_detail(format!("db={}, supported={}", current, SCHEMA_VERSION)));
    }

    // v0 -> v1: initial schema.
    if current < 1 {
        let tx = conn.transaction().map_err(sql_err("kv: migrate begin"))?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS kv (
                k          TEXT PRIMARY KEY,
                v          TEXT NOT NULL,
                size       INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            ) WITHOUT ROWID;

            CREATE INDEX IF NOT EXISTS kv_updated ON kv(updated_at);

            CREATE TABLE IF NOT EXISTS _meta (
                k TEXT PRIMARY KEY,
                v TEXT NOT NULL
            ) WITHOUT ROWID;

            PRAGMA user_version = 1;
            "#,
        )
        .map_err(sql_err("kv: migrate v1"))?;
        tx.commit().map_err(sql_err("kv: migrate commit"))?;
        current = 1;
    }

    // v1 -> v2: `total_bytes` used to contain value bytes only. Delete it so
    // the first open after this migration reconciles key bytes and the fixed
    // per-entry reservation instead of trusting a stale value-only total.
    if current < 2 {
        let tx = conn
            .transaction()
            .map_err(sql_err("kv: migrate v2 begin"))?;
        tx.execute("DELETE FROM _meta WHERE k = ?1", [META_TOTAL_BYTES])
            .map_err(sql_err("kv: migrate v2 total"))?;
        tx.execute_batch("PRAGMA user_version = 2;")
            .map_err(sql_err("kv: migrate v2"))?;
        tx.commit().map_err(sql_err("kv: migrate v2 commit"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[inline]
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn sql_err(ctx: &'static str) -> impl Fn(rusqlite::Error) -> EngineError {
    move |e| {
        EngineError::new(ErrorCode::IoError)
            .with_msg(ctx)
            .with_detail(e.to_string())
    }
}

/// Persist `total_bytes` into the `_meta` table as part of the caller's
/// write transaction, so commit atomicity covers both the row change
/// and the cached total.
fn persist_total_in_tx(
    tx: &rusqlite::Transaction<'_>,
    total_bytes: u64,
) -> Result<(), EngineError> {
    tx.execute(
        "INSERT INTO _meta(k, v) VALUES (?1, ?2) \
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![META_TOTAL_BYTES, total_bytes.to_string()],
    )
    .map_err(sql_err("kv: meta: set total"))?;
    Ok(())
}

fn persist_entries_in_tx(
    tx: &rusqlite::Transaction<'_>,
    total_entries: u64,
) -> Result<(), EngineError> {
    tx.execute(
        "INSERT INTO _meta(k, v) VALUES (?1, ?2) \
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![META_TOTAL_ENTRIES, total_entries.to_string()],
    )
    .map_err(sql_err("kv: meta: set entries"))?;
    Ok(())
}

/// On open, read the cached running total from `_meta.total_bytes`. If
/// it's missing or corrupt (e.g. the DB was written by an older binary
/// that never maintained the cache), fall back to a single charged-footprint
/// reconciliation and persist the result so subsequent opens are O(1).
fn load_or_reconcile_total(conn: &Connection) -> Result<u64, EngineError> {
    let cached: Option<String> = conn
        .query_row(
            "SELECT v FROM _meta WHERE k = ?1",
            [META_TOTAL_BYTES],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err("kv: meta: get total"))?;

    if let Some(s) = cached {
        if let Ok(n) = s.parse::<u64>() {
            return Ok(n);
        }
    }

    // Missing or corrupt — reconcile once. The key length is measured as
    // UTF-8 bytes (`CAST(... AS BLOB)`), matching Rust's `str::len`.
    let reconciled: i64 = conn
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(size + length(CAST(k AS BLOB)) + {}), 0) FROM kv",
                PER_ENTRY_OVERHEAD_BYTES
            ),
            [],
            |r| r.get(0),
        )
        .map_err(sql_err("kv: meta: reconcile charged total"))?;
    let reconciled = reconciled.max(0) as u64;
    conn.execute(
        "INSERT INTO _meta(k, v) VALUES (?1, ?2) \
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![META_TOTAL_BYTES, reconciled.to_string()],
    )
    .map_err(sql_err("kv: meta: persist reconciled total"))?;
    Ok(reconciled)
}

/// On open, read the cached row count. Older databases have no metadata row,
/// so reconcile once and persist it; writes thereafter update it transactionally.
fn load_or_reconcile_entries(conn: &Connection) -> Result<u64, EngineError> {
    let cached: Option<String> = conn
        .query_row(
            "SELECT v FROM _meta WHERE k = ?1",
            [META_TOTAL_ENTRIES],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err("kv: meta: get entries"))?;

    if let Some(s) = cached {
        if let Ok(n) = s.parse::<u64>() {
            return Ok(n);
        }
    }

    let reconciled: i64 = conn
        .query_row("SELECT COUNT(*) FROM kv", [], |r| r.get(0))
        .map_err(sql_err("kv: meta: reconcile entries"))?;
    let reconciled = reconciled.max(0) as u64;
    conn.execute(
        "INSERT INTO _meta(k, v) VALUES (?1, ?2) \
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![META_TOTAL_ENTRIES, reconciled.to_string()],
    )
    .map_err(sql_err("kv: meta: persist reconciled entries"))?;
    Ok(reconciled)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const QUOTA: u64 = 1024; // 1 KiB quota for exhaustion tests

    fn open(dir: &Path) -> KvStore {
        KvStore::open(dir.join("storage.db"), QUOTA).expect("open")
    }

    #[test]
    fn set_and_get_roundtrips() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("alpha", "one").unwrap();
        kv.set("beta", "two").unwrap();
        assert_eq!(kv.get("alpha").unwrap().as_deref(), Some("one"));
        assert_eq!(kv.get("beta").unwrap().as_deref(), Some("two"));
    }

    #[test]
    fn missing_key_returns_none() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        assert_eq!(kv.get("nope").unwrap(), None);
    }

    #[test]
    fn overwrite_same_key_keeps_total_accurate() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("k", "aaaa").unwrap();
        kv.set("k", "bbbbbbbb").unwrap(); // 8 bytes
        let info = kv.info().unwrap();
        assert_eq!(info.keys, vec!["k".to_string()]);
        assert_eq!(info.current_bytes, 73);
    }

    #[test]
    fn remove_is_idempotent() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("x", "1").unwrap();
        kv.remove("x").unwrap();
        kv.remove("x").unwrap(); // second call is a no-op
        assert_eq!(kv.get("x").unwrap(), None);
    }

    #[test]
    fn clear_removes_everything() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("a", "x").unwrap();
        kv.set("b", "y").unwrap();
        kv.clear().unwrap();
        let info = kv.info().unwrap();
        assert!(info.keys.is_empty());
        assert_eq!(info.current_bytes, 0);
    }

    #[test]
    fn info_reports_all_keys_and_total_bytes() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("k1", "aa").unwrap();
        kv.set("k2", "bbb").unwrap();
        let info = kv.info().unwrap();
        assert_eq!(info.current_bytes, 137);
        assert_eq!(info.limit_bytes, QUOTA);
        let mut sorted = info.keys.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["k1".to_string(), "k2".to_string()]);
    }

    #[test]
    fn quota_exceeded_returns_error_and_does_not_write() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        // Fill to the brim.
        let big = "x".repeat(QUOTA as usize - 4 - PER_ENTRY_OVERHEAD_BYTES as usize - 10);
        kv.set("fill", &big).unwrap();
        // One more byte should push us over.
        let err = kv.set("overflow", &"y".repeat(100)).unwrap_err();
        assert_eq!(err.code, ErrorCode::OutOfMemory);
        assert_eq!(
            kv.get("overflow").unwrap(),
            None,
            "aborted write must not leak"
        );
    }

    #[test]
    fn overwrite_does_not_trigger_transient_quota_overflow() {
        // If naive accounting added the new size *before* subtracting
        // the old, an at-capacity key rewrite would fail.  Verify the
        // replace-aware math.
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        let big = "x".repeat(QUOTA as usize - 1 - PER_ENTRY_OVERHEAD_BYTES as usize);
        kv.set("k", &big).unwrap();
        let big2 = "y".repeat(big.len());
        kv.set("k", &big2).unwrap(); // must succeed
        assert_eq!(kv.get("k").unwrap().unwrap().len(), big.len());
    }

    #[test]
    fn batch_set_is_atomic_on_quota_overflow() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        // batch sums to QUOTA+1 -> must reject entirely.
        let a = "a".repeat(500);
        let b = "b".repeat(525);
        let items = vec![("a", a.as_str()), ("b", b.as_str())];
        let err = kv.set_batch(&items).unwrap_err();
        assert_eq!(err.code, ErrorCode::OutOfMemory);
        assert_eq!(kv.get("a").unwrap(), None);
        assert_eq!(kv.get("b").unwrap(), None);
    }

    #[test]
    fn batch_set_applies_all_on_success() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        let items = vec![("a", "1"), ("b", "22"), ("c", "333")];
        kv.set_batch(&items).unwrap();
        assert_eq!(kv.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(kv.get("b").unwrap().as_deref(), Some("22"));
        assert_eq!(kv.get("c").unwrap().as_deref(), Some("333"));
        let info = kv.info().unwrap();
        assert_eq!(info.current_bytes, 201);
    }

    #[test]
    fn batch_repeating_a_key_keeps_the_total_exact() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("k", "0123456789").unwrap();
        assert_eq!(kv.info().unwrap().current_bytes, 75);

        // Last write wins, so the row ends at 5 bytes. Projecting each
        // occurrence against the row still on disk would deduct the 10-byte
        // original twice and credit both new values, leaving the cached total
        // 3 bytes short of the truth.
        kv.set_batch(&[("k", "abc"), ("k", "de456")]).unwrap();

        assert_eq!(kv.get("k").unwrap().as_deref(), Some("de456"));
        assert_eq!(kv.info().unwrap().current_bytes, 70);
    }

    #[test]
    fn repeated_key_total_survives_reopen() {
        // The drift is persisted to `_meta.total_bytes`, which is only ever
        // reconciled against the charged-footprint formula when it is
        // missing -- so a wrong value here would outlive the process rather
        // than self-heal.
        let dir = tempdir().unwrap();
        {
            let kv = open(dir.path());
            kv.set_batch(&[("dup", "aaaa"), ("dup", "b"), ("other", "cc")])
                .unwrap();
            kv.checkpoint().unwrap();
        }

        let kv = open(dir.path());
        assert_eq!(kv.get("dup").unwrap().as_deref(), Some("b"));
        assert_eq!(kv.info().unwrap().current_bytes, 139);
    }

    #[test]
    fn empty_batch_is_noop() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set_batch(&[]).unwrap();
        assert!(kv.info().unwrap().keys.is_empty());
    }

    #[test]
    fn reopen_preserves_data() {
        let dir = tempdir().unwrap();
        {
            let kv = open(dir.path());
            kv.set("persist", "forever").unwrap();
            kv.checkpoint().unwrap();
        }
        let kv = open(dir.path());
        assert_eq!(kv.get("persist").unwrap().as_deref(), Some("forever"));
    }

    #[test]
    fn refuses_newer_schema_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("storage.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 999).unwrap();
        }
        let err = KvStore::open(&path, QUOTA).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
    }

    #[test]
    fn handles_unicode_keys_and_values() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("日本語", "値").unwrap();
        kv.set("emoji🔑", "🎯").unwrap();
        assert_eq!(kv.get("日本語").unwrap().as_deref(), Some("値"));
        assert_eq!(kv.get("emoji🔑").unwrap().as_deref(), Some("🎯"));
    }

    #[test]
    fn keys_with_sql_metacharacters_are_literal() {
        // If we ever accidentally swapped bound params for string
        // interpolation, a key like "'; DROP TABLE kv; --" would
        // destroy the DB.  Verify parameterisation.
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        let evil = "'; DROP TABLE kv; --";
        kv.set(evil, "still here").unwrap();
        assert_eq!(kv.get(evil).unwrap().as_deref(), Some("still here"));
        // The kv table must still exist.
        assert!(kv.info().is_ok());
    }

    #[test]
    fn total_bytes_stays_in_sync_across_operations() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("a", "aa").unwrap();
        kv.set("b", "bbb").unwrap();
        assert_eq!(kv.info().unwrap().current_bytes, 135);
        kv.set("a", "aaaaa").unwrap();
        assert_eq!(kv.info().unwrap().current_bytes, 138);
        kv.remove("b").unwrap();
        assert_eq!(kv.info().unwrap().current_bytes, 70);
        kv.set_batch(&[("c", "cc"), ("d", "d")]).unwrap();
        assert_eq!(kv.info().unwrap().current_bytes, 203);
        kv.clear().unwrap();
        assert_eq!(kv.info().unwrap().current_bytes, 0);
    }

    #[test]
    fn reopen_reuses_cached_total_without_rescanning() {
        let dir = tempdir().unwrap();
        {
            let kv = open(dir.path());
            kv.set("k", "hello").unwrap();
        }
        // Reopen: the cached total must survive the process boundary.
        let kv = open(dir.path());
        assert_eq!(kv.info().unwrap().current_bytes, 70);
    }

    #[test]
    fn missing_meta_total_reconciles_on_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("storage.db");
        {
            let kv = KvStore::open(&path, QUOTA).unwrap();
            kv.set("a", "12345").unwrap();
            kv.checkpoint().unwrap();
        }
        // Simulate an older binary that never wrote `_meta.total_bytes`.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("DELETE FROM _meta WHERE k = 'total_bytes'", [])
                .unwrap();
        }
        let kv = KvStore::open(&path, QUOTA).unwrap();
        assert_eq!(kv.info().unwrap().current_bytes, 70);
    }

    #[test]
    fn parallel_clones_share_underlying_db() {
        use std::sync::Barrier;
        use std::thread;

        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        let b = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();
        for i in 0..4u32 {
            let kv = kv.clone();
            let b = b.clone();
            handles.push(thread::spawn(move || {
                b.wait();
                kv.set(&format!("k{}", i), &i.to_string()).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let info = kv.info().unwrap();
        assert_eq!(info.keys.len(), 4);
    }
    #[test]
    fn large_key_is_rejected_even_with_tiny_value() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        let key = "k".repeat(16 * 1024 + 1);
        let err = kv.set(&key, "x").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert_eq!(kv.info().unwrap().current_bytes, 0);
    }

    #[test]
    fn entry_count_is_bounded_independently_of_value_quota() {
        let dir = tempdir().unwrap();
        let kv = KvStore::open(dir.path().join("storage.db"), u64::MAX).unwrap();
        let items: Vec<(String, String)> = (0..=10_000)
            .map(|i| (format!("key-{i}"), "v".to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = items
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let err = kv.set_batch(&refs).unwrap_err();
        assert_eq!(err.code, ErrorCode::OutOfMemory);
        assert!(kv.info().unwrap().keys.is_empty());
    }

    #[test]
    fn info_page_returns_disjoint_bounded_slices() {
        let dir = tempdir().unwrap();
        let kv = open(dir.path());
        kv.set("a", "1").unwrap();
        kv.set("b", "2").unwrap();
        kv.set("c", "3").unwrap();

        let first = kv.info_page(0, 2).unwrap();
        let second = kv.info_page(2, 2).unwrap();
        assert_eq!(first.keys.len(), 2);
        assert_eq!(second.keys.len(), 1);
        assert!(first.keys.iter().all(|key| !second.keys.contains(key)));
    }

    /// FIO-04 regression: quota must refuse a write when key bytes push the
    /// true footprint over the limit, even when value bytes are identical.
    ///
    /// Store (Q=100): a 1-byte key + 30-byte value charges 95 bytes and fits;
    /// a 40-byte key with the same 30-byte value charges 134 bytes and fails.
    /// Old code (value bytes only) admits both because each value is 30 bytes.
    #[test]
    fn quota_refuses_when_key_bytes_cross_limit() {
        let dir = tempdir().unwrap();
        let kv = KvStore::open(dir.path().join("storage.db"), 100).unwrap();
        let value = "v".repeat(30);
        kv.set("k", &value).unwrap();

        let large_key = "K".repeat(40);
        let err = kv.set(&large_key, &value).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::OutOfMemory,
            "quota must refuse when key bytes contribute to the true footprint"
        );
        assert_eq!(
            kv.get(&large_key).unwrap(),
            None,
            "rejected write must not have leaked into the store"
        );
    }

    /// FIO-04 regression: per-entry overhead must prevent bypassing the quota
    /// with a high entry count of tiny values.
    ///
    /// Store (Q=500): batch of 8 entries each with a 2-byte key and 1-byte value →
    /// charged 8×(2+1+64)=536 > 500. Must be refused.
    /// Old code (value bytes only): 8×1=8 ≤ 500 → admits → this test is RED.
    #[test]
    fn per_entry_overhead_limits_tiny_value_entries() {
        let dir = tempdir().unwrap();
        let kv = KvStore::open(dir.path().join("storage.db"), 500).unwrap();
        let items: Vec<(&str, &str)> = vec![
            ("k0", "v"),
            ("k1", "v"),
            ("k2", "v"),
            ("k3", "v"),
            ("k4", "v"),
            ("k5", "v"),
            ("k6", "v"),
            ("k7", "v"),
        ];
        let err = kv.set_batch(&items).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::OutOfMemory,
            "per-entry overhead must count toward the quota even when values are tiny"
        );
        // Batch is atomic — nothing should have landed.
        assert!(kv.info().unwrap().keys.is_empty());
    }
}
