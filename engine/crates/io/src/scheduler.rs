use crate::{
    cost::CheapPolicy,
    domain::IoDomain,
    pools::{IoPools, PoolError},
    task::{BackendKind, IoRequest, PoolKind, PriorityClass, RequestKind},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex as StdMutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

type PackageVerificationGate = tokio::sync::Mutex<()>;

// Coalesce same-package launch misses before they consume a bounded FS worker.
// Weak values avoid retaining the gates themselves; dead path entries are
// pruned whenever a new gate is created. Cross-process exclusion remains the
// integrity layer's responsibility via its on-disk promotion lock.
static PACKAGE_VERIFICATION_GATES: LazyLock<
    StdMutex<HashMap<PathBuf, Weak<PackageVerificationGate>>>,
> = LazyLock::new(|| StdMutex::new(HashMap::new()));

fn package_verification_gate(receipt_path: &Path) -> Arc<PackageVerificationGate> {
    let mut gates = PACKAGE_VERIFICATION_GATES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(gate) = gates.get(receipt_path).and_then(Weak::upgrade) {
        return gate;
    }

    gates.retain(|_, gate| gate.strong_count() != 0);
    let gate = Arc::new(PackageVerificationGate::new(()));
    gates.insert(receipt_path.to_path_buf(), Arc::downgrade(&gate));
    gate
}

#[cfg(test)]
type WorkerStartHook = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

#[cfg(test)]
fn run_worker_start_test_hook(slot: &std::sync::Mutex<Option<WorkerStartHook>>) {
    let hook = slot.lock().unwrap().clone();
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    Inline,
    Delegated(PoolKind),
}

/// Deliberately **not** `Clone`. `Drop` closes the shared `Arc<IoDomain>`, so
/// a second owner dropping first would shut down IO for every other holder —
/// a struct-level clone and an `Arc::clone` would read identically at the call
/// site and mean opposite things. Share one scheduler through `Arc`, which is
/// what every caller already does.
pub struct IoScheduler {
    host_id: i32,
    pools: IoPools,
    domain: Arc<IoDomain>,
    policy: CheapPolicy,
    metrics: Arc<SchedulerMetrics>,
    #[cfg(test)]
    worker_start_hook: Arc<std::sync::Mutex<Option<WorkerStartHook>>>,
    #[cfg(test)]
    image_job_runs: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulerMetricsSnapshot {
    pub inline_runs: u64,
    pub delegated_runs: u64,
    pub rejected_runs: u64,
    pub sync_wait_micros: u64,
}

#[derive(Default)]
struct SchedulerMetrics {
    inline_runs: AtomicU64,
    delegated_runs: AtomicU64,
    rejected_runs: AtomicU64,
    sync_wait_micros: AtomicU64,
}

impl IoScheduler {
    pub fn new(host_id: i32) -> Self {
        Self::with_pools(host_id, IoPools::new(host_id))
    }

    fn with_pools(host_id: i32, pools: IoPools) -> Self {
        Self {
            host_id,
            pools,
            domain: Arc::new(IoDomain::new()),
            policy: CheapPolicy::default(),
            metrics: Arc::new(SchedulerMetrics::default()),
            #[cfg(test)]
            worker_start_hook: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            image_job_runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub fn host_id(&self) -> i32 {
        self.host_id
    }

    pub fn policy(&self) -> CheapPolicy {
        self.policy
    }

    pub fn pools(&self) -> &IoPools {
        &self.pools
    }

    pub fn domain(&self) -> Arc<IoDomain> {
        Arc::clone(&self.domain)
    }

    pub fn metrics(&self) -> SchedulerMetricsSnapshot {
        SchedulerMetricsSnapshot {
            inline_runs: self.metrics.inline_runs.load(Ordering::Relaxed),
            delegated_runs: self.metrics.delegated_runs.load(Ordering::Relaxed),
            rejected_runs: self.metrics.rejected_runs.load(Ordering::Relaxed),
            sync_wait_micros: self.metrics.sync_wait_micros.load(Ordering::Relaxed),
        }
    }

    pub fn close(&self) {
        self.domain.close_all();
    }

    pub fn ensure_open(&self) -> Result<(), PoolError> {
        if self.domain.is_closed() {
            Err(PoolError::Closed)
        } else {
            Ok(())
        }
    }

    pub fn classify(&self, req: &IoRequest) -> RouteDecision {
        classify_request(req, &self.policy)
    }

    pub fn run_sync<T, F>(&self, req: &IoRequest, job: F) -> Result<T, PoolError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if self.ensure_open().is_err() {
            self.metrics.rejected_runs.fetch_add(1, Ordering::Relaxed);
            return Err(PoolError::Closed);
        }

        match self.classify(req) {
            RouteDecision::Inline => {
                self.metrics.inline_runs.fetch_add(1, Ordering::Relaxed);
                Ok(job())
            }
            RouteDecision::Delegated(pool) => {
                self.metrics.delegated_runs.fetch_add(1, Ordering::Relaxed);
                let started_at = Instant::now();
                let priority = req.priority();
                let domain = Arc::clone(&self.domain);
                #[cfg(test)]
                let worker_start_hook = Arc::clone(&self.worker_start_hook);
                let result = self.pools.run(pool, priority, move || {
                    #[cfg(test)]
                    run_worker_start_test_hook(&worker_start_hook);

                    if domain.is_closed() {
                        return Err(PoolError::Closed);
                    }
                    Ok(job())
                });
                let wait_micros = started_at.elapsed().as_micros() as u64;
                self.metrics
                    .sync_wait_micros
                    .fetch_add(wait_micros, Ordering::Relaxed);
                tracing::debug!(
                    "[IOScheduler {}] sync wait pool={:?} elapsed_us={}",
                    self.host_id,
                    pool,
                    wait_micros
                );
                // Observability: bump the process-global slow-IO
                // counter when wall-clock crosses 100 ms. Uses the
                // IO scheduler's measurement because it includes
                // both pool queue wait + the job itself, which is
                // the latency the JS caller actually sees.
                shared::stats::io_metrics_global().record_if_slow(wait_micros / 1000);
                match result {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(err)) => {
                        self.metrics.rejected_runs.fetch_add(1, Ordering::Relaxed);
                        Err(err)
                    }
                    Err(err) => Err(err),
                }
            }
        }
    }

    pub async fn run_async<T, F>(&self, req: IoRequest, job: F) -> Result<T, PoolError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if self.ensure_open().is_err() {
            self.metrics.rejected_runs.fetch_add(1, Ordering::Relaxed);
            return Err(PoolError::Closed);
        }

        match self.classify(&req) {
            RouteDecision::Inline => {
                self.metrics.inline_runs.fetch_add(1, Ordering::Relaxed);
                Ok(job())
            }
            RouteDecision::Delegated(pool) => {
                self.metrics.delegated_runs.fetch_add(1, Ordering::Relaxed);
                let priority = req.priority();
                let domain = Arc::clone(&self.domain);
                #[cfg(test)]
                let worker_start_hook = Arc::clone(&self.worker_start_hook);
                // Inline requests intentionally bypass this admission branch:
                // they never enter a FairLane, so they cannot accumulate the
                // queued backing-store bytes this budget protects. Only the
                // delegated path needs a pending-byte credit.
                // Zero means the request has no trustworthy size hint; it
                // remains admitted and is accounted as an operation without
                // claiming a byte budget that the caller did not provide.
                let byte_count = pending_bytes_hint(&req);
                let rx = self
                    .pools
                    .submit_async_bytes(pool, priority, byte_count, move || {
                        #[cfg(test)]
                        run_worker_start_test_hook(&worker_start_hook);

                        if domain.is_closed() {
                            return Err(PoolError::Closed);
                        }
                        Ok(job())
                    })?;
                match rx.await {
                    Ok(Ok(Ok(value))) => Ok(value),
                    Ok(Ok(Err(err))) => {
                        self.metrics.rejected_runs.fetch_add(1, Ordering::Relaxed);
                        Err(err)
                    }
                    Ok(Err(payload)) => std::panic::resume_unwind(payload),
                    Err(_) => Err(PoolError::Closed),
                }
            }
        }
    }

    /// Run full install verification without allowing concurrent misses for
    /// the same receipt to occupy multiple bounded filesystem workers.
    pub async fn run_package_verification<T, F>(
        &self,
        receipt_path: PathBuf,
        priority: PriorityClass,
        job: F,
    ) -> Result<T, PoolError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if self.ensure_open().is_err() {
            self.metrics.rejected_runs.fetch_add(1, Ordering::Relaxed);
            return Err(PoolError::Closed);
        }

        // This is an async wait, so another launch for the same package does
        // not consume a worker while the first verifier hashes/seals it.
        let gate = package_verification_gate(&receipt_path);
        let guard = gate.lock_owned().await;

        // run_async rechecks the domain after the wait and again when the
        // delegated closure starts, preserving close semantics. Move the
        // guard into that closure so cancelling the receiver cannot release
        // the package gate while its blocking worker is still running.
        self.run_async(IoRequest::VerifyPackage { priority }, move || {
            let _guard = guard;
            job()
        })
        .await
    }
}

#[cfg(test)]
impl IoScheduler {
    // Per-instance, NOT a process-global slot — see IoDomain's hook for the
    // same deadlock rationale: parallel tests each own their own scheduler, so
    // another test's worker starting no longer trips this test's barrier hook.
    pub(crate) fn install_worker_start_test_hook(&self, hook: WorkerStartHook) {
        *self.worker_start_hook.lock().unwrap() = Some(hook);
    }

    // Per-instance counter of image jobs routed through this scheduler. A
    // process-global counter made the image tests' reset→assert(==1) windows
    // race under parallel execution; each test owns its own scheduler.
    pub(crate) fn note_image_job_run(&self) {
        self.image_job_runs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn image_job_run_count(&self) -> usize {
        self.image_job_runs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn shared_executor_pair_for_test(
        first_host_id: i32,
        second_host_id: i32,
        worker_count: usize,
    ) -> (Self, Self) {
        let (first_pools, second_pools) =
            IoPools::shared_pair_for_test(first_host_id, second_host_id, worker_count);
        (
            Self::with_pools(first_host_id, first_pools),
            Self::with_pools(second_host_id, second_pools),
        )
    }

    pub(crate) fn local_for_test(host_id: i32, worker_count: usize) -> Self {
        Self::with_pools(host_id, IoPools::local_for_test(host_id, worker_count))
    }

    pub(crate) fn local_with_byte_limit_for_test(
        host_id: i32,
        worker_count: usize,
        byte_limit: u64,
    ) -> Self {
        Self::with_pools(
            host_id,
            IoPools::local_with_byte_limit_for_test(host_id, worker_count, byte_limit),
        )
    }

    pub(crate) fn pending_work_for_test(&self) -> usize {
        self.pools.pending_work_for_test()
    }
}

impl Default for IoScheduler {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Drop for IoScheduler {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn classify_request(req: &IoRequest, policy: &CheapPolicy) -> RouteDecision {
    match req {
        IoRequest::ReadFile {
            backend,
            request: _,
            priority,
            estimated_bytes,
        } => classify_read(*backend, *priority, *estimated_bytes, policy),
        IoRequest::GetFileInfo {
            backend, priority, ..
        } => classify_metadata(*backend, *priority),
        IoRequest::DecodeImage { cache_hit, .. } => {
            if *cache_hit {
                RouteDecision::Inline
            } else {
                RouteDecision::Delegated(PoolKind::Image)
            }
        }
        IoRequest::Unzip { .. } => RouteDecision::Delegated(PoolKind::Archive),
        // Not `Archive`: ingest holds an image's encoded bytes, its decoded
        // RGBA, its ETC2 blocks and its KTX2 container simultaneously, so it
        // stays serial while extraction runs several at a time.
        IoRequest::PackageIngest { .. } => RouteDecision::Delegated(PoolKind::Ingest),
        IoRequest::VerifyPackage { .. } => RouteDecision::Delegated(PoolKind::Fs),
        IoRequest::StorageGet {
            request,
            priority,
            estimated_bytes,
        } => classify_storage_get(*request, *priority, *estimated_bytes, policy),
        IoRequest::StorageMutate { .. } | IoRequest::StorageInfo { .. } => {
            RouteDecision::Delegated(PoolKind::Fs)
        }
        // Generic fs ops (write/copy/mkdir/stat/...): sync/foreground-blocking
        // runs inline on the caller (V8) thread — matching the pre-scheduler
        // behaviour where sync fs ops ran directly on the V8 thread; async
        // work fans out to the fs pool, replacing the raw `spawn_blocking`
        // so it becomes bounded, prioritized and domain-close-aware.
        IoRequest::FsOp { priority, .. } => {
            if *priority == PriorityClass::ForegroundBlocking {
                RouteDecision::Inline
            } else {
                RouteDecision::Delegated(PoolKind::Fs)
            }
        }
    }
}

/// Return a producer-supplied upper bound for bytes retained by a delegated
/// request. Requests without a trustworthy bound use zero: they still use the
/// bounded worker/operation lanes, but do not pretend an unknown payload fits
/// a byte budget.
fn pending_bytes_hint(req: &IoRequest) -> u64 {
    match req {
        IoRequest::ReadFile {
            estimated_bytes, ..
        }
        | IoRequest::DecodeImage {
            encoded_bytes: estimated_bytes,
            ..
        }
        | IoRequest::Unzip {
            compressed_bytes: estimated_bytes,
            ..
        }
        | IoRequest::PackageIngest {
            compressed_bytes: estimated_bytes,
            ..
        }
        | IoRequest::StorageGet {
            estimated_bytes, ..
        } => *estimated_bytes as u64,
        IoRequest::GetFileInfo { .. }
        | IoRequest::VerifyPackage { .. }
        | IoRequest::StorageMutate { .. }
        | IoRequest::StorageInfo { .. }
        | IoRequest::FsOp { .. } => 0,
    }
}

fn classify_read(
    backend: BackendKind,
    priority: PriorityClass,
    estimated_bytes: usize,
    policy: &CheapPolicy,
) -> RouteDecision {
    match backend {
        // Small foreground pack reads (icons, JSON, atlas descriptors)
        // run inline: the bytes are usually already in the OS page
        // cache and a hot inflate cache hit returns instantly, so a
        // pool hop just adds a thread handoff to a sub-millisecond
        // operation. Everything else still fans out to the pack pool.
        BackendKind::Pack => {
            if is_foreground(priority) && estimated_bytes <= policy.small_read_bytes {
                RouteDecision::Inline
            } else {
                RouteDecision::Delegated(PoolKind::Pack)
            }
        }
        BackendKind::Archive => RouteDecision::Delegated(PoolKind::Archive),
        BackendKind::Filesystem => {
            if is_foreground(priority) && estimated_bytes <= policy.small_read_bytes {
                RouteDecision::Inline
            } else {
                RouteDecision::Delegated(PoolKind::Fs)
            }
        }
    }
}

fn classify_metadata(backend: BackendKind, priority: PriorityClass) -> RouteDecision {
    match backend {
        // Pack metadata is an in-memory hashmap lookup; a pool hop is
        // pure overhead. Inline foreground requests.
        BackendKind::Pack if is_foreground(priority) => RouteDecision::Inline,
        BackendKind::Pack => RouteDecision::Delegated(PoolKind::Pack),
        BackendKind::Archive => RouteDecision::Delegated(PoolKind::Archive),
        BackendKind::Filesystem if is_foreground(priority) => RouteDecision::Inline,
        BackendKind::Filesystem => RouteDecision::Delegated(PoolKind::Fs),
    }
}

fn classify_storage_get(
    request: RequestKind,
    priority: PriorityClass,
    estimated_bytes: usize,
    policy: &CheapPolicy,
) -> RouteDecision {
    if request == RequestKind::Sync
        && priority == PriorityClass::ForegroundBlocking
        && estimated_bytes <= policy.small_copy_bytes
    {
        RouteDecision::Inline
    } else {
        RouteDecision::Delegated(PoolKind::Fs)
    }
}

fn is_foreground(priority: PriorityClass) -> bool {
    matches!(
        priority,
        PriorityClass::ForegroundBlocking | PriorityClass::ForegroundAsync
    )
}

#[cfg(test)]
mod tests {
    use parking_lot::{Condvar, Mutex};
    use std::{
        future::Future,
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::{Context, Poll, Waker},
    };

    use crate::{
        cost::CheapPolicy,
        fs_ops::read_file,
        pools::PoolError,
        scheduler::{IoScheduler, RouteDecision, classify_request},
        task::{BackendKind, IoRequest, PoolKind, PriorityClass, RequestKind},
    };
    fn delegated_sync_pack_reads_use_pack_pool() {
        let scheduler = IoScheduler::new(7);
        // Foreground reads under the inline threshold short-circuit, so
        // size the request comfortably above it to verify delegation.
        let req = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 256 * 1024,
        };

        let thread_name = scheduler
            .run_sync(&req, || {
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string()
            })
            .unwrap();

        assert!(thread_name.starts_with("Migo-IO-"));
    }

    #[test]
    fn scheduler_does_not_spawn_pools_until_delegated_work_runs() {
        let scheduler = IoScheduler::local_for_test(11, 3);

        assert_eq!(scheduler.pools().started_thread_count_for_test(), 0);

        let req = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 256 * 1024,
        };

        let _ = scheduler.run_sync(&req, || 1usize).unwrap();

        assert_eq!(scheduler.pools().started_thread_count_for_test(), 3);
    }

    #[test]
    fn scheduler_records_inline_and_delegated_counts() {
        let scheduler = IoScheduler::new(17);
        let inline_req = IoRequest::ReadFile {
            backend: BackendKind::Filesystem,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 32,
        };
        let delegated_req = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 256 * 1024,
        };

        scheduler.run_sync(&inline_req, || 1usize).unwrap();
        scheduler.run_sync(&delegated_req, || 2usize).unwrap();

        let metrics = scheduler.metrics();
        assert_eq!(metrics.inline_runs, 1);
        assert_eq!(metrics.delegated_runs, 1);
    }

    #[test]
    fn queued_delegated_work_rechecks_domain_closure_before_running() {
        // Archive pool is intentionally single-threaded so the queueing
        // semantics this test exercises (second job blocked behind the
        // first, then sees Closed when the scheduler is shut down) are
        // observable without timing-dependent assertions.
        let scheduler = Arc::new(IoScheduler::local_for_test(19, 2));
        let req = IoRequest::Unzip {
            backend: BackendKind::Filesystem,
            priority: PriorityClass::ForegroundAsync,
            compressed_bytes: 64 * 1024,
        };

        let hook_calls = Arc::new(AtomicUsize::new(0));
        let hook_entered = Arc::new(Barrier::new(2));
        let hook_release = Arc::new(Barrier::new(2));
        let hook_calls_clone = Arc::clone(&hook_calls);
        let hook_entered_clone = Arc::clone(&hook_entered);
        let hook_release_clone = Arc::clone(&hook_release);
        scheduler.install_worker_start_test_hook(Arc::new(move || {
            let call = hook_calls_clone.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 2 {
                hook_entered_clone.wait();
                hook_release_clone.wait();
            }
        }));

        let first_scheduler = Arc::clone(&scheduler);
        let first_req = req.clone();
        let first_started = Arc::new(Barrier::new(2));
        let first_release = Arc::new(Barrier::new(2));
        let first_started_thread = Arc::clone(&first_started);
        let first_release_thread = Arc::clone(&first_release);
        let first = std::thread::spawn(move || {
            first_scheduler
                .run_sync(&first_req, move || {
                    first_started_thread.wait();
                    first_release_thread.wait();
                    1usize
                })
                .unwrap()
        });

        first_started.wait();

        let second_scheduler = Arc::clone(&scheduler);
        let second_req = req.clone();
        let second_ran = Arc::new(AtomicBool::new(false));
        let second_ran_thread = Arc::clone(&second_ran);
        let second = std::thread::spawn(move || {
            second_scheduler.run_sync(&second_req, move || {
                second_ran_thread.store(true, Ordering::SeqCst);
                2usize
            })
        });

        first_release.wait();
        hook_entered.wait();
        scheduler.close();
        hook_release.wait();

        assert_eq!(first.join().unwrap(), 1);
        let second_result = second.join().unwrap();

        assert!(matches!(
            second_result,
            Err(crate::pools::PoolError::Closed)
        ));
        assert!(!second_ran.load(Ordering::SeqCst));
    }

    #[test]
    fn closing_one_domain_does_not_reject_another_host_on_shared_executor() {
        let (scheduler_a, scheduler_b) = IoScheduler::shared_executor_pair_for_test(21, 22, 2);
        let scheduler_a = Arc::new(scheduler_a);
        let archive_req = IoRequest::Unzip {
            backend: BackendKind::Filesystem,
            priority: PriorityClass::ForegroundAsync,
            compressed_bytes: 64 * 1024,
        };

        let first_started = Arc::new(Barrier::new(2));
        let first_release = Arc::new(Barrier::new(2));
        let first_scheduler = Arc::clone(&scheduler_a);
        let first_req = archive_req.clone();
        let first_started_job = Arc::clone(&first_started);
        let first_release_job = Arc::clone(&first_release);
        let first = std::thread::spawn(move || {
            first_scheduler.run_sync(&first_req, move || {
                first_started_job.wait();
                first_release_job.wait();
                1_u32
            })
        });
        first_started.wait();

        let queued_user_ran = Arc::new(AtomicBool::new(false));
        let queued_user_ran_job = Arc::clone(&queued_user_ran);
        let queued_scheduler = Arc::clone(&scheduler_a);
        let queued_req = archive_req.clone();
        let queued = std::thread::spawn(move || {
            queued_scheduler.run_sync(&queued_req, move || {
                queued_user_ran_job.store(true, Ordering::SeqCst);
                2_u32
            })
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while scheduler_a.pending_work_for_test() == 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(scheduler_a.pending_work_for_test(), 1);
        scheduler_a.close();

        let pack_req = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 256 * 1024,
        };
        assert_eq!(scheduler_b.run_sync(&pack_req, || 3_u32).unwrap(), 3);

        first_release.wait();
        assert_eq!(first.join().unwrap().unwrap(), 1);
        assert!(matches!(
            queued.join().unwrap(),
            Err(crate::pools::PoolError::Closed)
        ));
        assert!(!queued_user_ran.load(Ordering::SeqCst));
    }

    #[test]
    fn scheduler_routes_pack_backed_reads_to_pack_pool() {
        let req = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
            estimated_bytes: 128 * 1024,
        };

        assert_eq!(
            classify_request(&req, &CheapPolicy::default()),
            RouteDecision::Delegated(PoolKind::Pack)
        );
    }

    #[test]
    fn scheduler_keeps_small_fs_sync_reads_inline() {
        let req = IoRequest::ReadFile {
            backend: BackendKind::Filesystem,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 1024,
        };

        let policy = CheapPolicy {
            small_read_bytes: 4 * 1024,
            small_copy_bytes: 512,
        };

        assert_eq!(classify_request(&req, &policy), RouteDecision::Inline);
    }

    #[test]
    fn scheduler_keeps_small_fs_async_reads_inline() {
        let req = IoRequest::ReadFile {
            backend: BackendKind::Filesystem,
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
            estimated_bytes: 1024,
        };

        let policy = CheapPolicy {
            small_read_bytes: 4 * 1024,
            small_copy_bytes: 512,
        };

        assert_eq!(classify_request(&req, &policy), RouteDecision::Inline);
    }

    #[test]
    fn scheduler_keeps_fs_async_metadata_inline() {
        let req = IoRequest::GetFileInfo {
            backend: BackendKind::Filesystem,
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
        };

        assert_eq!(
            classify_request(&req, &CheapPolicy::default()),
            RouteDecision::Inline
        );
    }

    #[test]
    fn scheduler_keeps_small_sync_storage_get_inline() {
        let req = IoRequest::StorageGet {
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 128,
        };

        let policy = CheapPolicy {
            small_read_bytes: 4 * 1024,
            small_copy_bytes: 512,
        };

        assert_eq!(classify_request(&req, &policy), RouteDecision::Inline);
    }

    #[test]
    fn scheduler_keeps_sync_fs_ops_inline() {
        // Sync (ForegroundBlocking) generic fs ops run inline on the caller
        // (V8) thread, matching pre-scheduler behaviour.
        let req = IoRequest::FsOp {
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
        };
        assert_eq!(
            classify_request(&req, &CheapPolicy::default()),
            RouteDecision::Inline
        );
    }

    #[test]
    fn scheduler_routes_async_fs_ops_to_fs_pool() {
        // Async generic fs ops (writes/mutations/metadata) fan out to the fs
        // pool instead of tokio's unbounded blocking pool.
        let req = IoRequest::FsOp {
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
        };
        assert_eq!(
            classify_request(&req, &CheapPolicy::default()),
            RouteDecision::Delegated(PoolKind::Fs)
        );
    }

    #[test]
    fn package_verification_routes_to_fs_pool_at_startup_priority() {
        let req = IoRequest::VerifyPackage {
            priority: PriorityClass::ForegroundBlocking,
        };

        assert_eq!(req.priority(), PriorityClass::ForegroundBlocking);
        assert_eq!(
            classify_request(&req, &CheapPolicy::default()),
            RouteDecision::Delegated(PoolKind::Fs)
        );
    }

    #[test]
    fn package_verification_runs_on_bounded_fs_worker() {
        let scheduler = IoScheduler::new(203);
        let req = IoRequest::VerifyPackage {
            priority: PriorityClass::ForegroundBlocking,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let thread_name = runtime
            .block_on(scheduler.run_async(req, || {
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string()
            }))
            .unwrap();

        assert!(thread_name.starts_with("Migo-IO-"));
    }

    #[test]
    fn package_verification_is_rejected_after_domain_close() {
        let scheduler = IoScheduler::new(205);
        scheduler.close();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_job = Arc::clone(&ran);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = runtime.block_on(scheduler.run_package_verification(
            std::env::temp_dir().join("migo-closed-package-verification"),
            PriorityClass::ForegroundBlocking,
            move || ran_in_job.store(true, Ordering::SeqCst),
        ));

        assert!(matches!(result, Err(crate::pools::PoolError::Closed)));
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn same_package_verifications_wait_before_worker_dispatch() {
        let scheduler = Arc::new(IoScheduler::local_for_test(207, 4));
        let receipt_key = std::env::temp_dir().join(format!(
            "migo-package-verification-singleflight-{}",
            std::process::id()
        ));
        let (first_started_tx, first_started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let first_scheduler = Arc::clone(&scheduler);
        let first_key = receipt_key.clone();
        let first = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(first_scheduler.run_package_verification(
                first_key,
                PriorityClass::ForegroundBlocking,
                move || {
                    first_started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            ))
        });
        first_started_rx.recv().unwrap();

        let second_started = Arc::new(AtomicBool::new(false));
        let second_started_in_job = Arc::clone(&second_started);
        let (second_submitted_tx, second_submitted_rx) = std::sync::mpsc::channel();
        let second_scheduler = Arc::clone(&scheduler);
        let second = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            second_submitted_tx.send(()).unwrap();
            runtime.block_on(second_scheduler.run_package_verification(
                receipt_key,
                PriorityClass::ForegroundBlocking,
                move || second_started_in_job.store(true, Ordering::SeqCst),
            ))
        });
        second_submitted_rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !second_started.load(Ordering::SeqCst),
            "same-package waiter occupied another FS worker instead of awaiting the keyed gate"
        );
        assert_eq!(
            scheduler.metrics().delegated_runs,
            1,
            "same-package waiter was dispatched before the first verifier released the gate"
        );

        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert!(second_started.load(Ordering::SeqCst));
    }

    #[test]
    fn package_verification_cancelled_receiver_keeps_gate_until_worker_finishes() {
        let scheduler = Arc::new(IoScheduler::local_for_test(209, 4));
        let receipt_key = std::env::temp_dir().join(format!(
            "migo-package-verification-cancel-{}",
            std::process::id()
        ));
        let (first_started_tx, first_started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let mut first = Box::pin(scheduler.run_package_verification(
            receipt_key.clone(),
            PriorityClass::ForegroundBlocking,
            move || {
                first_started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            },
        ));

        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            Future::poll(first.as_mut(), &mut context),
            Poll::Pending
        ));
        first_started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();

        // Cancelling the receiver must not release the per-package gate while
        // its already-dispatched blocking closure is still running.
        drop(first);

        let second_started = Arc::new(AtomicBool::new(false));
        let second_started_in_job = Arc::clone(&second_started);
        let second_scheduler = Arc::clone(&scheduler);
        let (second_submitted_tx, second_submitted_rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            second_submitted_tx.send(()).unwrap();
            runtime.block_on(second_scheduler.run_package_verification(
                receipt_key,
                PriorityClass::ForegroundBlocking,
                move || second_started_in_job.store(true, Ordering::SeqCst),
            ))
        });
        second_submitted_rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let started_before_release = second_started.load(Ordering::SeqCst);
        let delegated_before_release = scheduler.metrics().delegated_runs;

        release_tx.send(()).unwrap();
        second.join().unwrap().unwrap();

        assert!(
            !started_before_release,
            "cancelling the first receiver released its gate while the worker was still running"
        );
        assert_eq!(
            delegated_before_release, 1,
            "cancelling the first receiver allowed another same-package worker dispatch"
        );
    }

    #[test]
    fn scheduler_routes_unzip_requests_to_archive_pool() {
        let req = IoRequest::Unzip {
            backend: BackendKind::Filesystem,
            priority: PriorityClass::ForegroundAsync,
            compressed_bytes: 32 * 1024,
        };

        assert_eq!(
            classify_request(&req, &CheapPolicy::default()),
            RouteDecision::Delegated(PoolKind::Archive)
        );
    }

    /// Extraction and ingest must not share a class.
    ///
    /// They ran on one before, which forced a single cap onto two opposite
    /// profiles: ingest holds tens to hundreds of MB per job and has to stay
    /// serial, extraction holds tens of KB and measures 3.5-3.8x on four
    /// threads. One cap could only serve the dangerous one, so extraction was
    /// pinned at 1 for a constraint that was never its own.
    #[test]
    fn extraction_and_ingest_are_scheduled_as_separate_classes() {
        let unzip = IoRequest::Unzip {
            backend: BackendKind::Filesystem,
            priority: PriorityClass::Background,
            compressed_bytes: 4 * 1024 * 1024,
        };
        let ingest = IoRequest::PackageIngest {
            priority: PriorityClass::Background,
            compressed_bytes: 4 * 1024 * 1024,
        };
        let policy = CheapPolicy::default();

        assert_eq!(
            classify_request(&unzip, &policy),
            RouteDecision::Delegated(PoolKind::Archive)
        );
        assert_eq!(
            classify_request(&ingest, &policy),
            RouteDecision::Delegated(PoolKind::Ingest)
        );
    }

    #[test]
    fn scheduler_routes_uncached_image_decode_requests_to_image_pool() {
        let req = IoRequest::DecodeImage {
            backend: BackendKind::Filesystem,
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
            encoded_bytes: 128 * 1024,
            cache_hit: false,
        };

        assert_eq!(
            classify_request(&req, &CheapPolicy::default()),
            RouteDecision::Delegated(PoolKind::Image)
        );
    }

    #[test]
    fn scheduler_keeps_cached_image_decode_requests_inline() {
        let req = IoRequest::DecodeImage {
            backend: BackendKind::Filesystem,
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
            encoded_bytes: 128 * 1024,
            cache_hit: true,
        };

        assert_eq!(
            classify_request(&req, &CheapPolicy::default()),
            RouteDecision::Inline
        );
    }

    #[test]
    fn delegated_async_work_preserves_panic_payload() {
        let scheduler = IoScheduler::new(13);
        let req = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
            estimated_bytes: 256 * 1024,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = catch_unwind(AssertUnwindSafe(|| {
            runtime.block_on(async {
                let _: Result<(), _> = scheduler
                    .run_async(req, || -> () { panic!("pack worker panic") })
                    .await;
            });
        }));

        let payload = result.expect_err("expected delegated panic to propagate");
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic>");
        assert_eq!(message, "pack worker panic");
    }

    /// What the inline fast path is actually worth, in the shape
    /// `readFileSync` hits it.
    ///
    /// Both requests describe the same 200-byte whole-file read. The hinted
    /// one carries the size the backend already knew and classifies `Inline`;
    /// the unhinted one has to assume `MAX_READ_LENGTH`, classifies
    /// `Delegated`, and pays a worker round-trip for a read that would have
    /// finished in the time the handoff takes.
    ///
    /// `#[ignore]` and reported rather than bounded: the absolute numbers are
    /// hardware-dependent. The assertion is only on the direction, which is
    /// the part that must never regress.
    #[test]
    #[ignore]
    fn bench_inline_vs_delegated_dispatch() {
        const ITERATIONS: u32 = 2000;
        let scheduler = IoScheduler::new(401);

        let hinted = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: 200,
        };
        let unhinted = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Sync,
            priority: PriorityClass::ForegroundBlocking,
            estimated_bytes: shared::protocol::io_cmd::MAX_READ_LENGTH as usize,
        };

        assert_eq!(scheduler.classify(&hinted), RouteDecision::Inline);
        assert_eq!(
            scheduler.classify(&unhinted),
            RouteDecision::Delegated(PoolKind::Pack)
        );

        // Dispatch-only: consume a changing input and the returned value so
        // the compiler cannot fold either closure to a constant.
        scheduler
            .run_sync(&unhinted, || std::hint::black_box(0_u8))
            .unwrap();
        let inline = {
            let started = std::time::Instant::now();
            for i in 0..ITERATIONS {
                let input = std::hint::black_box(i as u8);
                let output = scheduler
                    .run_sync(&hinted, move || std::hint::black_box(input.wrapping_add(1)))
                    .unwrap();
                std::hint::black_box(output);
            }
            started.elapsed()
        };
        let delegated = {
            let started = std::time::Instant::now();
            for i in 0..ITERATIONS {
                let input = std::hint::black_box(i as u8);
                let output = scheduler
                    .run_sync(&unhinted, move || {
                        std::hint::black_box(input.wrapping_add(1))
                    })
                    .unwrap();
                std::hint::black_box(output);
            }
            started.elapsed()
        };

        eprintln!(
            "inline    {:>10?} total  {:>10?}/call",
            inline,
            inline / ITERATIONS
        );
        eprintln!(
            "delegated {:>10?} total  {:>10?}/call",
            delegated,
            delegated / ITERATIONS
        );
        eprintln!(
            "worker round-trip costs {:.1}x an inline dispatch",
            delegated.as_secs_f64() / inline.as_secs_f64().max(f64::MIN_POSITIVE)
        );

        // End-to-end small-file section: dispatch timings above are not a
        // filesystem claim. Both routes now execute the same owned 200-byte
        // operation, and the returned bytes are consumed.
        let probe = std::env::temp_dir().join(format!("migo-small-read-{}", std::process::id()));
        std::fs::write(&probe, vec![0xA5u8; 200]).unwrap();
        let probe_path = probe.to_string_lossy().into_owned();
        let real_inline = {
            let started = std::time::Instant::now();
            for i in 0..ITERATIONS {
                let path = probe_path.clone();
                let output = scheduler
                    .run_sync(&hinted, move || {
                        let bytes = read_file(&path, None, None, false).unwrap();
                        std::hint::black_box(bytes[(i as usize) % bytes.len()])
                    })
                    .unwrap();
                std::hint::black_box(output);
            }
            started.elapsed()
        };
        let real_delegated = {
            let started = std::time::Instant::now();
            for i in 0..ITERATIONS {
                let path = probe_path.clone();
                let output = scheduler
                    .run_sync(&unhinted, move || {
                        let bytes = read_file(&path, None, None, false).unwrap();
                        std::hint::black_box(bytes[(i as usize) % bytes.len()])
                    })
                    .unwrap();
                std::hint::black_box(output);
            }
            started.elapsed()
        };
        let _ = std::fs::remove_file(&probe);
        eprintln!(
            "real small-file read inline {:>10?}/call; delegated {:>10?}/call",
            real_inline / ITERATIONS,
            real_delegated / ITERATIONS
        );

        assert!(
            inline < delegated,
            "the inline path is no longer cheaper than a worker round-trip \
             (inline {inline:?}, delegated {delegated:?})"
        );
    }

    #[test]
    fn run_async_completes_without_tokio_blocking_threads() {
        let scheduler = IoScheduler::new(201);
        let req = IoRequest::ReadFile {
            backend: BackendKind::Pack,
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
            estimated_bytes: 256 * 1024,
        };

        // Build a multi-thread runtime with exactly 1 blocking thread.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();

        // Saturate the one blocking thread with a long-running task.
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        runtime.spawn(async move {
            tokio::task::spawn_blocking(move || {
                let _ = release_rx.recv(); // hold the blocking thread forever
            });
        });
        // Let the blocking task start.
        std::thread::sleep(std::time::Duration::from_millis(100));

        // If run_async still uses spawn_blocking, this will time out because
        // the blocking pool is exhausted. With oneshot-based submit_async it
        // completes immediately via the dedicated worker pool thread.
        let result = runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                scheduler.run_async(req, || 42usize),
            )
            .await
        });

        // Release the blocked thread.
        let _ = release_tx.send(());

        assert!(
            result.is_ok(),
            "run_async timed out — still using spawn_blocking?"
        );
        assert_eq!(result.unwrap().unwrap(), 42);
    }
    #[test]
    fn run_async_hinted_request_refuses_when_process_bytes_are_saturated() {
        let scheduler =
            std::sync::Arc::new(IoScheduler::local_with_byte_limit_for_test(202, 1, 10));
        let request = IoRequest::ReadFile {
            backend: BackendKind::Archive,
            request: RequestKind::Async,
            priority: PriorityClass::ForegroundAsync,
            estimated_bytes: 8,
        };
        let gate = std::sync::Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let first_gate = std::sync::Arc::clone(&gate);
        let first_scheduler = std::sync::Arc::clone(&scheduler);
        let first = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(first_scheduler.run_async(request, move || {
                started_tx.send(()).unwrap();
                let (lock, condvar) = &*first_gate;
                let mut released = lock.lock();
                while !*released {
                    condvar.wait(&mut released);
                }
                1_u8
            }))
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let refused = runtime.block_on(scheduler.run_async(
            IoRequest::ReadFile {
                backend: BackendKind::Archive,
                request: RequestKind::Async,
                priority: PriorityClass::ForegroundAsync,
                estimated_bytes: 4,
            },
            || 2_u8,
        ));
        assert!(matches!(
            refused,
            Err(PoolError::ByteLimitExceeded {
                requested: 4,
                available: 2
            })
        ));

        *gate.0.lock() = true;
        gate.1.notify_all();
        assert_eq!(first.join().unwrap().unwrap(), 1);
    }
}
