//! Storage (KV) operations — SQLite-backed.
//!
//! This file is the Rust-side facade for the `getStorage / setStorage /
//! removeStorage / clearStorage / getStorageInfo` small-game APIs.
//! The underlying backend is a single WAL-mode SQLite database per
//! game storage directory, created lazily at `<storage_dir>/storage.db` on first
//! access.  See [`crate::kv_store`] for the schema and concurrency
//! model. Async work uses the existing `IoScheduler`; synchronous entry points
//! run inline and can wait for database locks, commit sync and checkpoints.
//!
//! ## Migration from the previous file-per-key layout
//!
//! The prior implementation stored each key as a hex-named file
//! under `<storage_dir>/`. That layout is gone; on first open the
//! old `.dat` files (if any) are left on disk untouched — the host
//! app can `rm -rf` them after confirming its users migrated, or we
//! can wire an automated import later (not done here because the
//! product decision is "no history compat").

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use shared::error::EngineError;

use crate::{
    kv_store::{KvInfo, KvStore},
    pools::PoolError,
    scheduler::IoScheduler,
    task::{IoRequest, PriorityClass, RequestKind},
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Summary returned by [`storage_info`]. Wire format of the JS
/// `getStorageInfoSync` is built in the runtime-v8 layer; this type
/// is the structured equivalent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageInfo {
    pub keys: Vec<String>,
    pub current_bytes: u64,
    pub limit_bytes: u64,
}

impl From<KvInfo> for StorageInfo {
    fn from(k: KvInfo) -> Self {
        Self {
            keys: k.keys,
            current_bytes: k.current_bytes,
            limit_bytes: k.limit_bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-session KvStore handle cache
// ---------------------------------------------------------------------------
//
// Opening SQLite involves filesystem IO, WAL setup and potentially a busy
// wait; we keep a bounded LRU of handles per storage directory. The
// cache is keyed by absolute path so different `HostOpState` instances
// (e.g. separate app ids in a test harness) get separate DBs.
//
// A directory's slot serializes first opens without holding the cache mutex.
// Different directories can proceed while SQLite waits on one database.
// If all slots are opening or in use, reject the new directory: an uncached
// connection could create a second owner of the same cached quota totals.
const MAX_CACHED_STORES: usize = 64;

struct StoreEntry {
    store: Arc<Mutex<Option<KvStore>>>,
    last_used: u64,
}

static STORES: LazyLock<Mutex<HashMap<PathBuf, StoreEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static STORE_CLOCK: AtomicU64 = AtomicU64::new(0);

#[inline]
fn next_store_tick() -> u64 {
    STORE_CLOCK.fetch_add(1, Ordering::Relaxed)
}

fn store_for(dir: &Path, quota_bytes: u64) -> Result<KvStore, EngineError> {
    store_for_in(&STORES, dir, quota_bytes)
}

fn store_for_in(
    stores: &Mutex<HashMap<PathBuf, StoreEntry>>,
    dir: &Path,
    quota_bytes: u64,
) -> Result<KvStore, EngineError> {
    let key = dir.to_path_buf();
    let (slot, evicted) = {
        let mut map = stores.lock();
        if let Some(entry) = map.get_mut(&key) {
            entry.last_used = next_store_tick();
            (entry.store.clone(), None)
        } else {
            let evicted = if map.len() >= MAX_CACHED_STORES {
                let victim = map
                    .iter()
                    .filter(|(_, entry)| {
                        Arc::strong_count(&entry.store) == 1
                            && entry.store.try_lock().is_some_and(|slot| {
                                slot.as_ref().is_none_or(|store| store.strong_count() == 1)
                            })
                    })
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(path, _)| path.clone());
                let path = victim.ok_or_else(|| {
                    EngineError::new(shared::error::ErrorCode::ExceedMaxConcurrentFdLimit)
                        .with_msg("storage handle limit reached; retry after an active operation")
                })?;
                map.remove(&path)
            } else {
                None
            };
            let slot = Arc::new(Mutex::new(None));
            map.insert(
                key,
                StoreEntry {
                    store: slot.clone(),
                    last_used: next_store_tick(),
                },
            );
            (slot, evicted)
        }
    };
    // SQLite close/checkpoint and cold open must both run outside the cache
    // mutex. Arc pins keep opening/waiting slots out of the eviction set.
    drop(evicted);
    let mut ready = slot.lock();
    if let Some(store) = ready.as_ref() {
        return Ok(store.clone());
    }
    let store = KvStore::open(dir.join("storage.db"), quota_bytes)?;
    // Keep a failed open empty so a waiting/later caller can retry.
    *ready = Some(store.clone());
    Ok(store)
}

// No test-only cache reset exists on purpose. Every test keys off its own
// `tempdir()`, which is a fresh unique absolute path, so a stale entry for
// that key is impossible and there is nothing to reset. A global
// `STORES.lock().clear()` used to run before each test and was actively
// harmful: cargo runs tests in parallel threads against this one process-wide
// map, so one test's reset would wipe the entry another test had just
// inserted, out from under it.

// ---------------------------------------------------------------------------
// Scheduler plumbing
// ---------------------------------------------------------------------------

#[inline]
fn pool_err(err: PoolError) -> EngineError {
    EngineError::from(err)
}

#[inline]
fn storage_get_request(request: RequestKind, estimated_bytes: usize) -> IoRequest {
    IoRequest::StorageGet {
        request,
        priority: PriorityClass::from(request),
        estimated_bytes,
    }
}

#[inline]
fn storage_mutate_request(request: RequestKind) -> IoRequest {
    IoRequest::StorageMutate {
        request,
        priority: PriorityClass::from(request),
    }
}

#[inline]
fn storage_info_request(request: RequestKind) -> IoRequest {
    IoRequest::StorageInfo {
        request,
        priority: PriorityClass::from(request),
    }
}

// ---------------------------------------------------------------------------
// Direct (unscheduled) operations — used by tests and by the async
// variants below; callers on the host thread should prefer the
// `*_sync_with_scheduler` wrappers so budget accounting is correct.
// ---------------------------------------------------------------------------

/// Read a stored value.  Returns `Ok(None)` for a missing key.
///
/// The old file-based implementation returned an empty string on
/// missing key (to match the JS `''` sentinel); we now return
/// `Option` so the JS layer can distinguish "empty string value" from
/// "no such key".
pub fn storage_get(dir: &Path, key: &str, quota_bytes: u64) -> Result<Option<String>, EngineError> {
    store_for(dir, quota_bytes)?.get(key)
}

/// Write `value` under `key` subject to the `quota_bytes` cap.
pub fn storage_set(
    dir: &Path,
    key: &str,
    value: &str,
    quota_bytes: u64,
) -> Result<(), EngineError> {
    store_for(dir, quota_bytes)?.set(key, value)
}

/// Batch write. Atomic: either every item lands or none does.
pub fn storage_set_batch(
    dir: &Path,
    items: &[(&str, &str)],
    quota_bytes: u64,
) -> Result<(), EngineError> {
    store_for(dir, quota_bytes)?.set_batch(items)
}

pub fn storage_remove(dir: &Path, key: &str, quota_bytes: u64) -> Result<(), EngineError> {
    store_for(dir, quota_bytes)?.remove(key)
}

pub fn storage_clear(dir: &Path, quota_bytes: u64) -> Result<(), EngineError> {
    store_for(dir, quota_bytes)?.clear()
}

pub fn storage_info(dir: &Path, quota_bytes: u64) -> Result<StorageInfo, EngineError> {
    store_for(dir, quota_bytes)?.info().map(Into::into)
}

// ---------------------------------------------------------------------------
// Scheduler-routed wrappers (host-thread entry points)
// ---------------------------------------------------------------------------
//
// These match the shape of the pre-existing runtime-v8 integration
// — the JS op calls `*_sync_with_scheduler` which dispatches through
// the IoScheduler so budget/priority accounting keeps working.
// Internally each closure calls the direct helpers above; the thin
// wrapping lets us preserve the scheduler contract without scattering
// `IoScheduler` references through every KvStore call site.

pub fn storage_get_sync_with_scheduler(
    scheduler: Arc<IoScheduler>,
    dir: PathBuf,
    key: String,
    quota_bytes: u64,
) -> Result<Option<String>, EngineError> {
    // Zero, deliberately: it makes the read classify `Inline` and run on the
    // calling thread.
    //
    // A sync caller is blocked for the whole operation either way, so handing
    // the query to a worker only adds the round-trip -- ~26us against a KV
    // lookup that costs a few. And the size that would justify delegating is
    // the *value's*, which nobody knows until the row is read; the only way to
    // find out is a second query as expensive as the first.
    //
    // Do not "fix" this to a nominal upper bound. Anything above
    // `CheapPolicy::small_copy_bytes` flips every `getStorageSync` onto a
    // worker and pays that round-trip for nothing.
    let request = storage_get_request(RequestKind::Sync, 0);
    scheduler
        .run_sync(&request, move || storage_get(&dir, &key, quota_bytes))
        .map_err(pool_err)?
}

pub async fn storage_get_with_scheduler(
    scheduler: Arc<IoScheduler>,
    dir: PathBuf,
    key: String,
    quota_bytes: u64,
    request: RequestKind,
) -> Result<Option<String>, EngineError> {
    // Zero for the same reason as the sync entry point above. On the async
    // path it changes nothing -- `classify_storage_get` delegates anything
    // that is not `Sync` + `ForegroundBlocking` regardless.
    let req = storage_get_request(request, 0);
    match request {
        RequestKind::Sync => scheduler
            .run_sync(&req, move || storage_get(&dir, &key, quota_bytes))
            .map_err(pool_err)?,
        RequestKind::Async => scheduler
            .run_async(req, move || storage_get(&dir, &key, quota_bytes))
            .await
            .map_err(pool_err)?,
    }
}

pub fn storage_set_sync_with_scheduler(
    scheduler: Arc<IoScheduler>,
    dir: PathBuf,
    key: String,
    value: String,
    quota_bytes: u64,
) -> Result<(), EngineError> {
    let request = storage_mutate_request(RequestKind::Sync);
    scheduler
        .run_sync(&request, move || {
            storage_set(&dir, &key, &value, quota_bytes)
        })
        .map_err(pool_err)?
}

pub fn storage_remove_sync_with_scheduler(
    scheduler: Arc<IoScheduler>,
    dir: PathBuf,
    key: String,
    quota_bytes: u64,
) -> Result<(), EngineError> {
    let request = storage_mutate_request(RequestKind::Sync);
    scheduler
        .run_sync(&request, move || storage_remove(&dir, &key, quota_bytes))
        .map_err(pool_err)?
}

pub fn storage_clear_sync_with_scheduler(
    scheduler: Arc<IoScheduler>,
    dir: PathBuf,
    quota_bytes: u64,
) -> Result<(), EngineError> {
    let request = storage_mutate_request(RequestKind::Sync);
    scheduler
        .run_sync(&request, move || storage_clear(&dir, quota_bytes))
        .map_err(pool_err)?
}

pub fn storage_info_sync_with_scheduler(
    scheduler: Arc<IoScheduler>,
    dir: PathBuf,
    quota_bytes: u64,
) -> Result<StorageInfo, EngineError> {
    let request = storage_info_request(RequestKind::Sync);
    scheduler
        .run_sync(&request, move || storage_info(&dir, quota_bytes))
        .map_err(pool_err)?
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const QUOTA: u64 = 1024 * 1024; // 1 MiB in tests

    fn fresh_dir() -> tempfile::TempDir {
        tempdir().unwrap()
    }

    /// A sync `getStorage` must run on the calling thread.
    ///
    /// The estimate the wrapper passes is what decides this, and it is a bare
    /// `0` whose reason lives only in a comment. This pins the outcome, so
    /// raising that estimate to something that "looks more honest" fails here
    /// instead of quietly adding a worker round-trip to every synchronous
    /// storage read.
    #[test]
    fn sync_storage_get_runs_inline_not_on_a_worker() {
        // A private executor, not the process-global one: the thread-count
        // assertion below is only evidence if no other test could have started
        // those threads.
        let scheduler = Arc::new(IoScheduler::local_for_test(431, 2));
        let dir = fresh_dir();
        storage_set(dir.path(), "k", "v", QUOTA).unwrap();

        let value = storage_get_sync_with_scheduler(
            Arc::clone(&scheduler),
            dir.path().to_path_buf(),
            "k".to_string(),
            QUOTA,
        )
        .unwrap();
        assert_eq!(value.as_deref(), Some("v"));

        let metrics = scheduler.metrics();
        assert_eq!(metrics.inline_runs, 1, "the read did not run inline");
        assert_eq!(metrics.delegated_runs, 0);
        // The strongest evidence available: a delegated run would have had to
        // start the pool to execute anywhere.
        assert_eq!(
            scheduler.pools().started_thread_count_for_test(),
            0,
            "a synchronous storage read started IO worker threads"
        );
    }

    /// Concurrent first writes to a brand-new store must all land.
    ///
    /// Ten `setStorage` calls issued together is ordinary content behaviour, and
    /// they arrive on separate scheduler threads against a directory where no
    /// `storage.db` exists yet -- so every one of them misses the handle cache
    /// and races to open and initialise the same file. Sequential writes never
    /// showed a problem; this is the shape that did.
    #[test]
    fn concurrent_first_writes_to_a_new_store_all_land() {
        let dir = fresh_dir();
        let path = dir.path().to_path_buf();
        let mut handles = Vec::new();
        for i in 0..10 {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                let key = format!("k{i}");
                storage_set(&p, &key, &format!("v{i}"), QUOTA).map_err(|e| format!("{key}: {e:?}"))
            }));
        }
        let mut failures = Vec::new();
        for h in handles {
            if let Err(e) = h.join().expect("writer thread") {
                failures.push(e);
            }
        }
        assert!(failures.is_empty(), "writes failed: {failures:?}");
        for i in 0..10 {
            assert_eq!(
                storage_get(&path, &format!("k{i}"), QUOTA)
                    .unwrap()
                    .as_deref(),
                Some(format!("v{i}").as_str()),
                "k{i} did not survive the concurrent open"
            );
        }
    }

    #[test]
    fn a_blocked_cold_open_does_not_hold_up_other_directories() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        // A private cache exercises the production function without clearing
        // the process-wide cache used by parallel tests.
        let stores = Mutex::new(HashMap::new());
        let blocked_dir = fresh_dir();
        let warm_dir = fresh_dir();
        let new_dir = fresh_dir();
        let warm = store_for_in(&stores, warm_dir.path(), QUOTA).unwrap();
        warm.set("seed", "stable").unwrap();
        let blocker = rusqlite::Connection::open(blocked_dir.path().join("storage.db")).unwrap();
        blocker
            .execute_batch("CREATE TABLE blocker(v); BEGIN EXCLUSIVE")
            .unwrap();

        std::thread::scope(|scope| {
            let cold = scope.spawn(|| store_for_in(&stores, blocked_dir.path(), QUOTA));
            let deadline = Instant::now() + Duration::from_secs(2);
            let opening = loop {
                if let Some(map) = stores.try_lock() {
                    if let Some(entry) = map.get(blocked_dir.path()) {
                        if entry.store.try_lock().is_none() {
                            break true;
                        }
                    }
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            let (send, receive) = mpsc::channel();
            let stores = &stores;
            let warm_path = warm_dir.path();
            let new_path = new_dir.path();
            let other = scope.spawn(move || {
                let started = Instant::now();
                let value = store_for_in(stores, warm_path, QUOTA)
                    .unwrap()
                    .get("seed")
                    .unwrap();
                store_for_in(stores, new_path, QUOTA)
                    .unwrap()
                    .set("new", "value")
                    .unwrap();
                send.send((started.elapsed(), value)).unwrap();
            });
            // A lock dependency cannot be told from a slow runner by a wall-clock
            // budget (4 ms locally, >2 s on a hosted Windows image). It can be
            // told by order: the unrelated work must finish while the cold open
            // is still waiting on SQLite, which only the ROLLBACK below ends.
            let progress = receive.recv_timeout(Duration::from_secs(60));
            let cold_still_waiting = !cold.is_finished();
            blocker.execute_batch("ROLLBACK").unwrap();
            let cold_result = cold.join().unwrap();
            other.join().unwrap();
            assert!(opening, "cold open must not hold the cache mutex");
            let (elapsed, value) = progress.expect("other directories must make progress");
            assert!(
                cold_still_waiting,
                "other directories finished only after the blocked cold open ended ({elapsed:?})"
            );
            assert_eq!(value.as_deref(), Some("stable"));
            cold_result.unwrap().set("after", "unlock").unwrap();
            eprintln!("warm read + unrelated cold write before SQLite unlock: {elapsed:?}");
        });
    }

    #[test]
    fn saturated_cache_does_not_create_unshared_quota_owners() {
        let stores = Mutex::new(HashMap::new());
        let dirs: Vec<_> = (0..MAX_CACHED_STORES).map(|_| fresh_dir()).collect();
        let mut active: Vec<_> = dirs
            .iter()
            .map(|dir| store_for_in(&stores, dir.path(), QUOTA).unwrap())
            .collect();
        let overflow = fresh_dir();
        let value = "x".repeat(50);
        match store_for_in(&stores, overflow.path(), 150) {
            Err(error) => {
                assert_eq!(
                    error.code,
                    shared::error::ErrorCode::ExceedMaxConcurrentFdLimit
                );
                assert!(!overflow.path().join("storage.db").exists());
            }
            Ok(first) => {
                // A second uncached connection would also load a zero total,
                // allowing two 115-byte charges into a 150-byte store.
                let second = store_for_in(&stores, overflow.path(), 150).unwrap();
                first.set("a", &value).unwrap();
                assert!(
                    second.set("b", &value).is_err(),
                    "an overflow open must not bypass the store quota"
                );
            }
        }
        drop(active.pop());
        let first = store_for_in(&stores, overflow.path(), 150).unwrap();
        let second = store_for_in(&stores, overflow.path(), 150).unwrap();
        first.set("a", &value).unwrap();
        assert!(second.set("b", &value).is_err());
        assert_eq!(second.info().unwrap().current_bytes, 115);
        assert_eq!(second.info().unwrap().keys, vec!["a"]);
        assert_eq!(stores.lock().len(), MAX_CACHED_STORES);
    }

    #[test]
    fn failed_cold_open_can_be_retried_in_the_same_slot() {
        let stores = Mutex::new(HashMap::new());
        let dir = fresh_dir();
        let db = dir.path().join("storage.db");
        std::fs::create_dir(&db).unwrap();
        assert!(store_for_in(&stores, dir.path(), QUOTA).is_err());
        std::fs::remove_dir(&db).unwrap();
        store_for_in(&stores, dir.path(), QUOTA)
            .unwrap()
            .set("retry", "works")
            .unwrap();
        assert_eq!(
            store_for_in(&stores, dir.path(), QUOTA)
                .unwrap()
                .get("retry")
                .unwrap()
                .as_deref(),
            Some("works")
        );
        assert_eq!(stores.lock().len(), 1);
    }

    #[test]
    fn set_get_remove_roundtrip() {
        let dir = fresh_dir();
        storage_set(dir.path(), "k", "v", QUOTA).unwrap();
        assert_eq!(
            storage_get(dir.path(), "k", QUOTA).unwrap().as_deref(),
            Some("v")
        );
        storage_remove(dir.path(), "k", QUOTA).unwrap();
        assert_eq!(storage_get(dir.path(), "k", QUOTA).unwrap(), None);
    }

    #[test]
    fn clear_empties_store() {
        let dir = fresh_dir();
        storage_set(dir.path(), "a", "1", QUOTA).unwrap();
        storage_set(dir.path(), "b", "2", QUOTA).unwrap();
        storage_clear(dir.path(), QUOTA).unwrap();
        let info = storage_info(dir.path(), QUOTA).unwrap();
        assert!(info.keys.is_empty());
        assert_eq!(info.current_bytes, 0);
    }

    #[test]
    fn info_reports_keys_and_total_bytes() {
        let dir = fresh_dir();
        storage_set(dir.path(), "k1", "ab", QUOTA).unwrap();
        storage_set(dir.path(), "k2", "xyz", QUOTA).unwrap();
        let info = storage_info(dir.path(), QUOTA).unwrap();
        assert_eq!(info.current_bytes, 137);
        assert_eq!(info.limit_bytes, QUOTA);
        let mut sorted = info.keys.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["k1".to_string(), "k2".to_string()]);
    }

    #[test]
    fn batch_set_is_atomic() {
        let dir = fresh_dir();
        let items = vec![("a", "1"), ("b", "2"), ("c", "3")];
        storage_set_batch(dir.path(), &items, QUOTA).unwrap();
        assert_eq!(
            storage_get(dir.path(), "b", QUOTA).unwrap().as_deref(),
            Some("2")
        );
        // Overflow batch — nothing should land.
        let big = "x".repeat(QUOTA as usize);
        let overflow = vec![("a", big.as_str()), ("z", big.as_str())];
        let err = storage_set_batch(dir.path(), &overflow, QUOTA).unwrap_err();
        assert_eq!(err.code, shared::error::ErrorCode::OutOfMemory);
        // Earlier keys untouched.
        assert_eq!(
            storage_get(dir.path(), "a", QUOTA).unwrap().as_deref(),
            Some("1")
        );
    }

    #[test]
    fn second_open_of_same_dir_reuses_handle() {
        let dir = fresh_dir();
        storage_set(dir.path(), "once", "v", QUOTA).unwrap();
        // Re-access: should hit the cache and still see the data.
        let val = storage_get(dir.path(), "once", QUOTA).unwrap();
        assert_eq!(val.as_deref(), Some("v"));
        // Exactly one entry in the global store cache for this path.
        let n = STORES
            .lock()
            .keys()
            .filter(|p| p.as_path() == dir.path())
            .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn store_cache_has_a_hard_limit_and_never_evicts_an_active_store() {
        let active_dir = fresh_dir();
        let active = store_for(active_dir.path(), QUOTA).unwrap();
        let mut dirs = Vec::with_capacity(MAX_CACHED_STORES + 1);
        for index in 0..=MAX_CACHED_STORES {
            let dir = fresh_dir();
            storage_set(dir.path(), "k", &index.to_string(), QUOTA).unwrap();
            dirs.push(dir);
        }

        let map = STORES.lock();
        assert!(
            map.len() <= MAX_CACHED_STORES,
            "store cache exceeded hard limit: {}",
            map.len()
        );
        let cached_test_dirs = dirs
            .iter()
            .filter(|dir| map.contains_key(dir.path()))
            .count();
        assert!(
            cached_test_dirs <= MAX_CACHED_STORES,
            "store cache exceeded hard limit: {cached_test_dirs}"
        );
        assert!(
            map.contains_key(active_dir.path()),
            "an active store was evicted"
        );
        drop(map);
        drop(active);
    }

    #[test]
    fn scheduler_wrapper_round_trip() {
        let dir = fresh_dir();
        let scheduler = Arc::new(IoScheduler::new(42));
        storage_set_sync_with_scheduler(
            scheduler.clone(),
            dir.path().to_path_buf(),
            "k".into(),
            "v".into(),
            QUOTA,
        )
        .unwrap();
        let got =
            storage_get_sync_with_scheduler(scheduler, dir.path().to_path_buf(), "k".into(), QUOTA)
                .unwrap();
        assert_eq!(got.as_deref(), Some("v"));
    }

    // --- Lightweight benchmarks (run with `cargo test -- --ignored`) ---
    //
    // Both paths use the same durable commit policy. Report host observations
    // without a fixed timing/ratio assertion: filesystem sync latency varies.

    const BENCH_QUOTA: u64 = 32 * 1024 * 1024;

    #[test]
    #[ignore]
    fn bench_sequential_set_1000x200() {
        let dir = fresh_dir();
        let start = std::time::Instant::now();
        for i in 0..1000 {
            let k = format!("key_{i:04}");
            storage_set(dir.path(), &k, &"x".repeat(200), BENCH_QUOTA).unwrap();
        }
        let elapsed = start.elapsed();
        eprintln!("sequential 1000×200B set: {:.3?}", elapsed);
    }

    #[test]
    #[ignore]
    fn bench_batch_set_1000x200() {
        let dir = fresh_dir();
        let kvs: Vec<(String, String)> = (0..1000)
            .map(|i| (format!("key_{i:04}"), "x".repeat(200)))
            .collect();
        let items: Vec<(&str, &str)> = kvs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        let start = std::time::Instant::now();
        storage_set_batch(dir.path(), &items, BENCH_QUOTA).unwrap();
        let elapsed = start.elapsed();
        eprintln!("batch 1000×200B set: {:.3?}", elapsed);
    }
}
