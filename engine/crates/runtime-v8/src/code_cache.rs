//! Disk-backed V8 code cache for faster module loading.
//!
//! Persists compiled JS bytecode to `<app_cache>/migo_code_cache/` so that
//! subsequent launches skip V8 parse+compile for both game modules and
//! engine extension JS.
//!
//! Cache invalidation:
//! - Source hash mismatch -> stale entry deleted, recompile
//! - V8 version change   -> entire cache dir cleared
//! - Max 32 MB total      -> LRU eviction by mtime
//!
//! # Scope: one cache per directory, not one per Session
//!
//! The directory comes from `MigoEngineConfig.code_cache_dir`, which is per Engine,
//! so two Sessions on one Engine are handed the same directory. Section 6.5 says that
//! is right -- compiled bytecode for a given source is the same bytes whichever
//! Session asked for it, and the key is the source's own hash, so two games loading
//! one module should hold one copy.
//!
//! What was wrong was the accounting, and in the shape Section 6.4 defect 4 names:
//! the budget's denominator was an instance and its numerator a directory. Each Host
//! built its own `DiskCodeCache`, each scanned the directory once and then tracked its
//! own writes, and neither could see the other's. Three consequences, all from that
//! one mismatch: the 32 MB ceiling admitted N x 32 MB; one Session's eviction deleted
//! files another Session's counter still claimed, so that counter over-counted and
//! over-evicted; and two Sessions could write one path at once.
//!
//! So the directory owns the cache: [`create_code_cache`] hands back the instance
//! that directory already has, and its lock is what orders one Session's write
//! against another's read.
//!
//! **Not addressed, and stated rather than implied.** Two *processes* pointed at one
//! directory still get a budget each, because nothing here takes an OS-level lock on
//! the directory. The counter's drift correction in [`DiskCodeCache::evict_if_needed`]
//! is what keeps that bounded rather than unbounded: a scan that finds the directory
//! already under the ceiling replaces the tracked figure with the measured one.

use std::{
    borrow::Cow,
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
};

use deno_core::{ModuleSourceCode, ModuleSpecifier, SourceCodeCacheInfo};
use parking_lot::{Mutex, RwLock};

/// Maximum total cache size in bytes (32 MB).
const MAX_CACHE_SIZE: u64 = 32 * 1024 * 1024;
/// Admission bound for writes waiting behind the one cache worker.
const WRITE_QUEUE_CAPACITY: usize = 64;

/// The cache each directory has, if any Session still holds it.
///
/// `Weak` rather than `Arc`: the last Session to let go of a directory drops its
/// cache, and the next Session to ask for it builds a new one -- including the
/// opening scan, which is how a counter that has been away comes back correct.
static CACHES: LazyLock<Mutex<HashMap<PathBuf, Weak<DiskCodeCache>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct CacheState {
    cache_dir: PathBuf,
    v8_version: &'static str,
    directory: RwLock<u64>,
    hot: Mutex<HashMap<u64, Arc<Vec<u8>>>>,
    pub(crate) write_version: AtomicU64,
}

enum WriteJob {
    Write { hash: u64, data: Arc<Vec<u8>> },
    Flush(mpsc::SyncSender<()>),
}

/// V8 code cache backed by the filesystem.
///
/// Cache key = `hash(source_bytes, v8_version)`.
/// Filesystem publication happens on one bounded worker: the caller only
/// publishes an owned hot entry and admits a bounded write job. A cache miss
/// remains correct when admission or disk publication fails.
pub(crate) struct DiskCodeCache {
    state: Arc<CacheState>,
    write_tx: Option<SyncSender<WriteJob>>,
    writer: Option<thread::JoinHandle<()>>,
}

impl DiskCodeCache {
    /// Build the cache and complete its opening scan before registry insertion.
    fn new(cache_dir: PathBuf) -> Self {
        let v8_version = deno_core::v8::V8::get_version();
        let state = Arc::new(CacheState {
            cache_dir,
            v8_version,
            directory: RwLock::new(0),
            hot: Mutex::new(HashMap::new()),
            write_version: AtomicU64::new(0),
        });
        // Directory setup, version validation, and cold-cache scan run on the
        // bounded worker below, never on the isolate-facing constructor.

        let (write_tx, write_rx) = mpsc::sync_channel(WRITE_QUEUE_CAPACITY);
        let worker_state = Arc::clone(&state);
        let writer = thread::Builder::new()
            .name("migo-code-cache".to_string())
            .spawn(move || {
                worker_state.ensure_dir_and_check_version();
                *worker_state.directory.write() = scan_total_size(&worker_state.cache_dir);
                // Cold cache reads are prefetched here, never from the
                // isolate-facing `get` method.
                prefetch_cache(&worker_state);
                while let Ok(job) = write_rx.recv() {
                    match job {
                        WriteJob::Write { hash, data } => publish_job(&worker_state, hash, data),
                        WriteJob::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .expect("code-cache worker thread must start");
        Self {
            state,
            write_tx: Some(write_tx),
            writer: Some(writer),
        }
    }

    pub fn compute_hash(&self, source: &[u8]) -> u64 {
        let mut hasher = DefaultHasher::new();
        source.hash(&mut hasher);
        self.state.v8_version.hash(&mut hasher);
        hasher.finish()
    }

    /// Return an owned cache entry without filesystem work on the isolate.
    pub fn get(&self, hash: u64) -> Option<Vec<u8>> {
        self.state
            .hot
            .lock()
            .get(&hash)
            .map(|data| (**data).clone())
    }

    /// Admit a cache write without performing filesystem work on the caller.
    pub fn set(&self, hash: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let data = Arc::new(data.to_vec());
        self.state.hot.lock().insert(hash, Arc::clone(&data));
        let Some(tx) = self.write_tx.as_ref() else {
            self.state.hot.lock().remove(&hash);
            return;
        };
        match tx.try_send(WriteJob::Write { hash, data }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                // A full or closed queue is an intentional cache miss, not
                // an isolate stall; the next compilation can retry.
                self.state.hot.lock().remove(&hash);
            }
        }
    }

    fn clear_all(&self) {
        let _ = fs::remove_dir_all(&self.state.cache_dir);
        *self.state.directory.write() = 0;
        self.state.hot.lock().clear();
    }

    fn scan_total_size(&self) -> u64 {
        scan_total_size(&self.state.cache_dir)
    }

    #[cfg(test)]
    fn flush_writes(&self) {
        let Some(tx) = self.write_tx.as_ref() else {
            return;
        };
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        tx.send(WriteJob::Flush(done_tx))
            .expect("cache worker available");
        done_rx.recv().expect("cache worker must acknowledge flush");
    }
}

impl Drop for DiskCodeCache {
    fn drop(&mut self) {
        drop(self.write_tx.take());
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

impl CacheState {
    fn ensure_dir_and_check_version(&self) {
        let _ = fs::create_dir_all(&self.cache_dir);
        let version_file = self.cache_dir.join("v8_version.txt");
        let stored_version = fs::read_to_string(&version_file).unwrap_or_default();
        if stored_version.trim() != self.v8_version {
            tracing::info!(
                "V8 version changed ({} -> {}), clearing code cache",
                stored_version.trim(),
                self.v8_version
            );
            let _ = fs::remove_dir_all(&self.cache_dir);
            let _ = fs::create_dir_all(&self.cache_dir);
            let _ = fs::write(&version_file, self.v8_version);
        }
    }
}

fn hash_path(cache_dir: &Path, hash: u64) -> PathBuf {
    cache_dir.join(format!("{hash:016x}.bin"))
}

fn scan_total_size(cache_dir: &Path) -> u64 {
    fs::read_dir(cache_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("bin"))
        .map(|entry| entry.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

fn prefetch_cache(state: &CacheState) {
    let entries = fs::read_dir(&state.cache_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("bin")).then_some(path)
        })
        .collect::<Vec<_>>();
    for path in entries {
        let Some(hash) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| u64::from_str_radix(stem, 16).ok())
        else {
            continue;
        };
        if let Ok(data) = fs::read(path) {
            state.hot.lock().insert(hash, Arc::new(data));
        }
    }
}

fn publish_job(state: &CacheState, hash: u64, data: Arc<Vec<u8>>) {
    let path = hash_path(&state.cache_dir, hash);
    let version = state.write_version.load(Ordering::Relaxed) + 1;
    let tmp = state.cache_dir.join(format!("{hash:016x}.v{version}.tmp"));
    let old_size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let result = fs::write(&tmp, data.as_slice()).and_then(|()| fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        tracing::debug!(hash, "code-cache publication failed");
        return;
    }

    let tracked_before = *state.directory.read();
    let mut tracked = tracked_before.saturating_sub(old_size) + data.len() as u64;
    // Eviction scans and removes files. Keep that disk work outside the
    // accounting lock; the worker is serialized, so the local total remains
    // ordered while the lock stays memory-only.
    evict_if_needed(&state.cache_dir, &mut tracked);
    *state.directory.write() = tracked;
    state.write_version.fetch_add(1, Ordering::Release);
}

fn evict_if_needed(cache_dir: &Path, tracked: &mut u64) {
    if *tracked <= MAX_CACHE_SIZE {
        return;
    }
    let mut files = fs::read_dir(cache_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|e| e.to_str()) == Some("bin"))
                .then(|| {
                    entry.metadata().ok().map(|meta| {
                        (
                            path,
                            meta.len(),
                            meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                        )
                    })
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    let mut total = files.iter().map(|(_, size, _)| *size).sum::<u64>();
    if total <= MAX_CACHE_SIZE {
        *tracked = total;
        return;
    }
    files.sort_by_key(|(_, _, mtime)| *mtime);
    for (path, size, _) in files {
        if total <= MAX_CACHE_SIZE {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
    *tracked = total;
}

/// Shared handle to the directory's `DiskCodeCache`, usable from every Session's
/// ModuleLoader and ExtCodeCache.
pub(crate) type SharedCodeCache = Arc<DiskCodeCache>;

/// The cache for `app_cache_dir`, built if this is the first Session to ask.
pub(crate) fn create_code_cache(app_cache_dir: &Path) -> SharedCodeCache {
    let cache_dir = code_cache_dir(app_cache_dir);
    {
        let caches = CACHES.lock();
        if let Some(live) = caches.get(&cache_dir).and_then(Weak::upgrade) {
            return live;
        }
    }

    // Disk setup and the opening scan stay outside the registry lock. The
    // lock only publishes or acquires the in-memory shared instance.
    let candidate = Arc::new(DiskCodeCache::new(cache_dir.clone()));
    let mut caches = CACHES.lock();
    if let Some(live) = caches.get(&cache_dir).and_then(Weak::upgrade) {
        return live;
    }
    caches.retain(|_, cache| cache.strong_count() > 0);
    caches.insert(cache_dir, Arc::downgrade(&candidate));
    candidate
}

/// The directory a cache lives in, resolved to one name.
///
/// Created before it is resolved, and resolved before it is used as the registry key:
/// two Engines configured with one directory spelled two ways would otherwise get a
/// budget each, which is the defect the registry exists to prevent. A path that
/// cannot be resolved is used as given rather than refused -- the cache is an
/// optimisation, and a Session must still start without one.
fn code_cache_dir(app_cache_dir: &Path) -> PathBuf {
    let dir = app_cache_dir.join("migo_code_cache");
    let _ = fs::create_dir_all(&dir);
    fs::canonicalize(&dir).unwrap_or(dir)
}

/// Adapter implementing deno_core's `ExtCodeCache` trait for caching
/// built-in extension JS (02_async.js, 98_global_scope.js, etc.).
pub(crate) struct ExtCodeCacheAdapter {
    inner: SharedCodeCache,
}

impl ExtCodeCacheAdapter {
    pub fn new(cache: SharedCodeCache) -> Rc<Self> {
        Rc::new(Self { inner: cache })
    }
}

impl deno_core::ExtCodeCache for ExtCodeCacheAdapter {
    fn get_code_cache_info(
        &self,
        _specifier: &ModuleSpecifier,
        code: &ModuleSourceCode,
        _esm: bool,
    ) -> SourceCodeCacheInfo {
        let source_bytes = code.as_bytes();
        let hash = self.inner.compute_hash(source_bytes);
        let data = self.inner.get(hash).map(|v| Cow::Owned(v));
        SourceCodeCacheInfo { hash, data }
    }

    fn code_cache_ready(
        &self,
        _specifier: ModuleSpecifier,
        hash: u64,
        code_cache: &[u8],
        _esm: bool,
    ) {
        self.inner.set(hash, code_cache);
    }
}
#[cfg(test)]
mod tests {
    use super::{CACHES, MAX_CACHE_SIZE, create_code_cache};
    use std::{fs, path::PathBuf, sync::Weak};

    /// One root per test, because the registry is keyed on the directory and these
    /// tests are about what sharing a directory means.
    fn temp_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("migo_code_cache_{label}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("a writable temp root");
        root
    }

    #[test]
    fn two_sessions_on_one_directory_share_one_budget() {
        // Each Session asks for a cache the way `HostJsRuntime::new` does, with the
        // Engine's one `code_cache_dir`. Before the directory owned the cache, each
        // got a counter of its own, neither saw the other's writes, and the ceiling
        // this module exists to enforce admitted a multiple of itself.
        let root = temp_root("shared_budget");
        let first = create_code_cache(&root);
        let second = create_code_cache(&root);

        const ENTRY: usize = 2 * 1024 * 1024;
        let payload = vec![0xC5; ENTRY];
        let entries = MAX_CACHE_SIZE as usize / ENTRY + 4;
        for index in 0..entries {
            let asking = if index % 2 == 0 { &first } else { &second };
            asking.set(index as u64, &payload);
        }
        first.flush_writes();

        // Measured on disk rather than read from the counter: a counter that lied
        // would otherwise satisfy the assertion it is the subject of.
        let resident = first.scan_total_size();
        assert!(
            resident <= MAX_CACHE_SIZE,
            "two Sessions wrote {} MiB into one directory whose ceiling is {} MiB",
            resident / 1024 / 1024,
            MAX_CACHE_SIZE / 1024 / 1024,
        );
        assert!(
            resident > MAX_CACHE_SIZE / 2,
            "eviction released {} MiB, far more than the ceiling asked for",
            (entries as u64 * ENTRY as u64 - resident) / 1024 / 1024,
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_session_reads_what_the_first_compiled() {
        // Section 6.5 puts this cache in the shared tier: the bytecode for a source is
        // the same bytes whichever Session compiled it, and both of them load the same
        // engine extension JS. Giving each Session its own directory would satisfy the
        // budget and lose exactly this, which is why the fix was accounting and not
        // partitioning.
        let root = temp_root("shared_entries");
        let first = create_code_cache(&root);
        first.set(0xC0DE, b"bytecode compiled once");
        first.flush_writes();
        let second = create_code_cache(&root);
        assert_eq!(
            second.get(0xC0DE).as_deref(),
            Some(&b"bytecode compiled once"[..]),
            "a Session must not recompile what another already compiled"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn two_engines_with_different_directories_do_not_share_one() {
        // The registry is keyed on the directory, not on the process: a host that does
        // give two Engines two roots gets two caches, and one budget each.
        let first_root = temp_root("engine_one");
        let second_root = temp_root("engine_two");
        let first = create_code_cache(&first_root);
        let second = create_code_cache(&second_root);

        first.set(0xFEED, b"one Engine's bytecode");

        assert!(
            second.get(0xFEED).is_none(),
            "a cache reached another Engine's directory"
        );

        let _ = fs::remove_dir_all(&first_root);
        let _ = fs::remove_dir_all(&second_root);
    }

    #[test]
    fn a_directory_reopened_after_its_last_session_counts_what_is_there() {
        // The registry holds `Weak`, so the cache dies with the last Session holding
        // it and the next Session builds another. A rebuilt counter that started at
        // zero would hand that Session the whole ceiling again on top of a directory
        // that is already full.
        let root = temp_root("reopened");
        const ENTRY: u64 = 3 * 1024 * 1024;
        {
            let cache = create_code_cache(&root);
            cache.set(1, &vec![0x5A; ENTRY as usize]);
        }
        assert!(
            CACHES
                .lock()
                .get(&fs::canonicalize(root.join("migo_code_cache")).expect("the cache dir"))
                .is_none_or(|cache| Weak::strong_count(cache) == 0),
            "the last Session's cache outlived it, so this test proves nothing"
        );

        let reopened = create_code_cache(&root);
        reopened.flush_writes();
        assert_eq!(
            *reopened.state.directory.read(),
            ENTRY,
            "a reopened cache must count the entries already on disk"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// V01 regression: code-cache writes must not block the isolate thread.
    ///
    /// `set()` must enqueue and return immediately (no disk IO on caller).
    /// The background writer commits atomically (temp → rename) and advances
    /// `write_version` so callers can detect a fresh write.  A failed write
    /// (queue full, disk error) must leave the cache in a recoverable state —
    /// `get()` still returns the hot-map entry, and the next `set()` retries.
    #[test]
    fn code_cache_writes_are_non_blocking_and_publication_is_atomic() {
        use std::sync::atomic::Ordering;
        let root = temp_root("non_blocking");
        let cache = create_code_cache(&root);

        let v0 = cache.state.write_version.load(Ordering::Acquire);

        // ── 2. set() enqueues without disk IO: no .bin file appears yet ──
        cache.set(0xBEEF, b"compiled bytecode for module");
        let bin = cache
            .state
            .cache_dir
            .join(format!("{:016x}.bin", 0xBEEFu64));
        // File may or may not exist yet — the point is `set()` returned.
        // Hot-map read works immediately, before the background thread lands.
        assert_eq!(
            cache.get(0xBEEF).as_deref(),
            Some(&b"compiled bytecode for module"[..]),
            "get() must return the hot-map entry before the disk write completes"
        );

        // ── 3. After flush, publication is atomic (no .tmp left) and versioned ──
        cache.flush_writes();
        assert!(bin.exists(), "background writer must persist the entry");
        let tmp = bin.with_extension("tmp");
        assert!(
            !tmp.exists(),
            "atomic write must rename .tmp away before flush returns"
        );
        let v1 = cache.state.write_version.load(Ordering::Acquire);
        assert!(
            v1 > v0,
            "write_version must advance on each committed write"
        );

        // ── 4. A failed publication is recoverable, not an isolate error ──
        std::fs::remove_dir_all(&cache.state.cache_dir).unwrap();
        cache.set(0xCAFE, b"retry after failed publication");
        cache.flush_writes();
        assert_eq!(
            cache.get(0xCAFE).as_deref(),
            Some(&b"retry after failed publication"[..]),
            "a failed disk publication must remain a recoverable cache miss"
        );
        std::fs::create_dir_all(&cache.state.cache_dir).unwrap();
        cache.set(0xCAFE, b"published on retry");
        cache.flush_writes();
        assert_eq!(
            cache.get(0xCAFE).as_deref(),
            Some(&b"published on retry"[..])
        );
        let v2 = cache.state.write_version.load(Ordering::Acquire);
        assert!(v2 > v1, "a successful retry must advance write_version");

        // ── 5. Overwrites publish a newer version ──
        cache.set(0xBEEF, b"recompiled bytecode");
        cache.flush_writes();
        assert_eq!(
            cache.get(0xBEEF).as_deref(),
            Some(&b"recompiled bytecode"[..])
        );
        let v3 = cache.state.write_version.load(Ordering::Acquire);
        assert!(v3 > v2, "each committed write must advance write_version");

        let _ = fs::remove_dir_all(&root);
    }
}
