use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

use parking_lot::{Condvar as ParkingCondvar, Mutex};
use shared::error::{EngineError, ErrorCode};

use crate::task::{PoolKind, PriorityClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct HostToken(u64);

struct HostQueue<T> {
    // Each item carries its declared byte count so the accounting in
    // QueueState can move bytes from queued to active at dispatch time.
    jobs: VecDeque<(T, u64)>,
    in_rotation: bool,
}

impl<T> Default for HostQueue<T> {
    fn default() -> Self {
        Self {
            jobs: VecDeque::new(),
            in_rotation: false,
        }
    }
}

struct FairLane<T> {
    hosts: HashMap<HostToken, HostQueue<T>>,
    rotation: VecDeque<HostToken>,
}

impl<T> Default for FairLane<T> {
    fn default() -> Self {
        Self {
            hosts: HashMap::new(),
            rotation: VecDeque::new(),
        }
    }
}

impl<T> FairLane<T> {
    fn push(&mut self, host: HostToken, value: T) {
        self.push_with_bytes(host, value, 0);
    }

    fn push_with_bytes(&mut self, host: HostToken, value: T, byte_count: u64) {
        let queue = self.hosts.entry(host).or_default();
        let was_empty = queue.jobs.is_empty();
        queue.jobs.push_back((value, byte_count));
        if was_empty {
            debug_assert!(!queue.in_rotation);
            queue.in_rotation = true;
            self.rotation.push_back(host);
        }
    }

    fn pop_where(&mut self, eligible: impl FnMut(HostToken) -> bool) -> Option<(HostToken, T)> {
        self.pop_where_with_bytes(eligible)
            .map(|(host, value, _)| (host, value))
    }

    fn pop_where_with_bytes(
        &mut self,
        mut eligible: impl FnMut(HostToken) -> bool,
    ) -> Option<(HostToken, T, u64)> {
        let attempts = self.rotation.len();
        for _ in 0..attempts {
            let host = self
                .rotation
                .pop_front()
                .expect("rotation length changed while popping fair lane");
            if !eligible(host) {
                self.rotation.push_back(host);
                continue;
            }

            let queue = self
                .hosts
                .get_mut(&host)
                .expect("rotation token must have a host queue");
            let (value, byte_count) = queue
                .jobs
                .pop_front()
                .expect("rotation token must have queued work");
            if queue.jobs.is_empty() {
                queue.in_rotation = false;
            } else {
                self.rotation.push_back(host);
            }
            return Some((host, value, byte_count));
        }
        None
    }

    fn has_work(&self) -> bool {
        !self.rotation.is_empty()
    }

    #[cfg(test)]
    fn contains_host(&self, host: HostToken) -> bool {
        self.hosts.contains_key(&host)
    }

    fn remove_host_if_empty(&mut self, host: HostToken) {
        let removable = self
            .hosts
            .get(&host)
            .is_some_and(|queue| queue.jobs.is_empty() && !queue.in_rotation);
        if removable {
            self.hosts.remove(&host);
        }
    }
}

const POOL_COUNT: usize = 5;
const PRIORITY_COUNT: usize = 3;
const LANE_COUNT: usize = POOL_COUNT * PRIORITY_COUNT;

const fn pool_index(pool: PoolKind) -> usize {
    match pool {
        PoolKind::Fs => 0,
        PoolKind::Pack => 1,
        PoolKind::Image => 2,
        PoolKind::Archive => 3,
        PoolKind::Ingest => 4,
    }
}

const fn pool_from_index(index: usize) -> PoolKind {
    match index {
        0 => PoolKind::Fs,
        1 => PoolKind::Pack,
        2 => PoolKind::Image,
        3 => PoolKind::Archive,
        4 => PoolKind::Ingest,
        _ => panic!("invalid pool index"),
    }
}

const fn priority_index(priority: PriorityClass) -> usize {
    match priority {
        PriorityClass::Background => 0,
        PriorityClass::ForegroundAsync => 1,
        PriorityClass::ForegroundBlocking => 2,
    }
}

const fn lane_index(priority: PriorityClass, pool: PoolKind) -> usize {
    priority_index(priority) * POOL_COUNT + pool_index(pool)
}

#[derive(Debug, Clone)]
struct ExecutorConfig {
    worker_count: usize,
    class_caps: [usize; POOL_COUNT],
    host_cap_when_contended: usize,
    aging_interval: u64,
    process_byte_limit: u64,
}

impl ExecutorConfig {
    fn for_workers(worker_count: usize) -> Self {
        let worker_count = worker_count.max(1);
        let cpu_heavy_cap = worker_count.saturating_sub(1).clamp(1, 2);
        Self {
            worker_count,
            // Fs, Pack, Image, Archive, Ingest.
            //
            // Archive gets the CPU-heavy cap rather than 1: extraction is
            // inflate-bound with a working set of tens of kilobytes, and
            // measures 3.5-3.8x on four threads (`bench_parallel_extraction_speedup`,
            // both a 78:1-compressible and an incompressible payload). It used
            // to be pinned at 1 because it shared a class with ingest, whose
            // per-job memory runs to tens or hundreds of MB; that constraint
            // now lives on `Ingest`, where it belongs.
            class_caps: [worker_count, cpu_heavy_cap, cpu_heavy_cap, cpu_heavy_cap, 1],
            host_cap_when_contended: worker_count.div_ceil(2),
            aging_interval: 16,
            process_byte_limit: 256 * 1024 * 1024,
        }
    }

    #[cfg(test)]
    fn with_byte_limit(mut self, process_byte_limit: u64) -> Self {
        self.process_byte_limit = process_byte_limit;
        self
    }

    #[cfg(test)]
    fn class_cap(&self, pool: PoolKind) -> usize {
        self.class_caps[pool_index(pool)]
    }
}

struct Dispatched<T> {
    host: HostToken,
    pool: PoolKind,
    value: T,
    byte_count: u64,
}

/// Whether the per-host dispatch cap applies to a given dispatch pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostFairness {
    /// A host already at its contended cap yields the worker to its peers.
    Enforced,
    /// Taken only after `Enforced` found nothing: no peer could use the free
    /// worker, so the cap has no one left to protect.
    Relaxed,
}

struct QueueState<T> {
    lanes: [FairLane<T>; LANE_COUNT],
    class_cursor: [usize; PRIORITY_COUNT],
    active_by_class: [usize; POOL_COUNT],
    active_by_host: HashMap<HostToken, usize>,
    pending_by_host: HashMap<HostToken, usize>,
    retired_hosts: HashSet<HostToken>,
    active_total: usize,
    pending_total: usize,
    queued_bytes: u64,
    active_bytes: u64,
    reserved_bytes: u64,
    refusals: u64,
    oldest_enqueue: Option<Instant>,
    dispatch_count: u64,
    closed: bool,
}

impl<T> Default for QueueState<T> {
    fn default() -> Self {
        Self {
            lanes: std::array::from_fn(|_| FairLane::default()),
            class_cursor: [0; PRIORITY_COUNT],
            active_by_class: [0; POOL_COUNT],
            active_by_host: HashMap::new(),
            pending_by_host: HashMap::new(),
            retired_hosts: HashSet::new(),
            active_total: 0,
            pending_total: 0,
            queued_bytes: 0,
            active_bytes: 0,
            reserved_bytes: 0,
            refusals: 0,
            oldest_enqueue: None,
            dispatch_count: 0,
            closed: false,
        }
    }
}
impl<T> QueueState<T> {
    fn push(&mut self, host: HostToken, pool: PoolKind, priority: PriorityClass, value: T) {
        self.push_with_bytes(host, pool, priority, value, 0);
    }

    fn push_with_bytes(
        &mut self,
        host: HostToken,
        pool: PoolKind,
        priority: PriorityClass,
        value: T,
        byte_count: u64,
    ) {
        debug_assert!(!self.retired_hosts.contains(&host));
        if self.pending_total == 0 {
            self.oldest_enqueue = Some(Instant::now());
        }
        self.lanes[lane_index(priority, pool)].push_with_bytes(host, value, byte_count);
        self.pending_total += 1;
        self.queued_bytes += byte_count;
        *self.pending_by_host.entry(host).or_default() += 1;
    }

    fn push_reserved(
        &mut self,
        host: HostToken,
        pool: PoolKind,
        priority: PriorityClass,
        value: T,
        byte_count: u64,
    ) {
        self.reserved_bytes = self
            .reserved_bytes
            .checked_sub(byte_count)
            .expect("committed reservation must be reserved");
        self.push_with_bytes(host, pool, priority, value, byte_count);
    }

    fn pop_next(&mut self, config: &ExecutorConfig) -> Option<Dispatched<T>> {
        if self.active_total >= config.worker_count {
            return None;
        }

        if let Some(job) = self.pop_with_fairness(config, HostFairness::Enforced) {
            return Some(job);
        }
        self.pop_with_fairness(config, HostFairness::Relaxed)
    }

    fn pop_with_fairness(
        &mut self,
        config: &ExecutorConfig,
        fairness: HostFairness,
    ) -> Option<Dispatched<T>> {
        let aging_due = self.dispatch_count != 0
            && self.dispatch_count.is_multiple_of(config.aging_interval)
            && self.has_priority_work(PriorityClass::Background)
            && (self.has_priority_work(PriorityClass::ForegroundBlocking)
                || self.has_priority_work(PriorityClass::ForegroundAsync));
        if aging_due {
            if let Some(job) = self.pop_priority(PriorityClass::Background, config, fairness) {
                return Some(job);
            }
        }

        for priority in [
            PriorityClass::ForegroundBlocking,
            PriorityClass::ForegroundAsync,
            PriorityClass::Background,
        ] {
            if let Some(job) = self.pop_priority(priority, config, fairness) {
                return Some(job);
            }
        }
        None
    }

    fn has_priority_work(&self, priority: PriorityClass) -> bool {
        let base = priority_index(priority) * POOL_COUNT;
        self.lanes[base..base + POOL_COUNT]
            .iter()
            .any(FairLane::has_work)
    }

    fn pop_priority(
        &mut self,
        priority: PriorityClass,
        config: &ExecutorConfig,
        fairness: HostFairness,
    ) -> Option<Dispatched<T>> {
        let priority_idx = priority_index(priority);
        let cursor = self.class_cursor[priority_idx];
        let host_contended = fairness == HostFairness::Enforced && self.pending_by_host.len() > 1;
        let active_by_host = &self.active_by_host;
        let host_cap = config.host_cap_when_contended;

        for offset in 0..POOL_COUNT {
            let class_idx = (cursor + offset) % POOL_COUNT;
            if self.active_by_class[class_idx] >= config.class_caps[class_idx] {
                continue;
            }
            let pool = pool_from_index(class_idx);
            let popped = self.lanes[lane_index(priority, pool)].pop_where_with_bytes(|host| {
                !host_contended || active_by_host.get(&host).copied().unwrap_or(0) < host_cap
            });
            let Some((host, value, byte_count)) = popped else {
                continue;
            };

            self.class_cursor[priority_idx] = (class_idx + 1) % POOL_COUNT;
            self.pending_total -= 1;
            self.queued_bytes = self
                .queued_bytes
                .checked_sub(byte_count)
                .expect("dispatched job bytes must be queued");
            let pending = self
                .pending_by_host
                .get_mut(&host)
                .expect("dispatched host must have pending count");
            *pending -= 1;
            if *pending == 0 {
                self.pending_by_host.remove(&host);
            }
            if self.pending_total == 0 {
                self.oldest_enqueue = None;
            }
            self.active_total += 1;
            self.active_bytes += byte_count;
            self.active_by_class[class_idx] += 1;
            *self.active_by_host.entry(host).or_default() += 1;
            self.dispatch_count = self.dispatch_count.wrapping_add(1);
            return Some(Dispatched {
                host,
                pool,
                value,
                byte_count,
            });
        }
        None
    }

    fn complete(&mut self, host: HostToken, pool: PoolKind) {
        self.complete_with_bytes(host, pool, 0);
    }

    fn complete_with_bytes(&mut self, host: HostToken, pool: PoolKind, byte_count: u64) {
        let class_idx = pool_index(pool);
        self.active_total = self
            .active_total
            .checked_sub(1)
            .expect("completed job must be active");
        self.active_bytes = self
            .active_bytes
            .checked_sub(byte_count)
            .expect("completed job bytes must be active");
        self.active_by_class[class_idx] = self.active_by_class[class_idx]
            .checked_sub(1)
            .expect("completed class must be active");
        let active = self
            .active_by_host
            .get_mut(&host)
            .expect("completed host must be active");
        *active -= 1;
        if *active == 0 {
            self.active_by_host.remove(&host);
        }
        self.cleanup_retired_host(host);
    }

    fn retire_host(&mut self, host: HostToken) {
        self.retired_hosts.insert(host);
        self.cleanup_retired_host(host);
    }

    fn cleanup_retired_host(&mut self, host: HostToken) {
        if !self.retired_hosts.contains(&host)
            || self.pending_by_host.contains_key(&host)
            || self.active_by_host.contains_key(&host)
        {
            return;
        }
        for lane in &mut self.lanes {
            lane.remove_host_if_empty(host);
        }
        self.retired_hosts.remove(&host);
    }

    #[cfg(test)]
    fn contains_host(&self, host: HostToken) -> bool {
        self.lanes.iter().any(|lane| lane.contains_host(host))
    }

    #[cfg(test)]
    fn is_retired(&self, host: HostToken) -> bool {
        self.retired_hosts.contains(&host)
    }
}

type Job = Box<dyn FnOnce() + Send + 'static>;

struct ExecutorShared {
    state: Mutex<QueueState<Job>>,
    condvar: ParkingCondvar,
    config: ExecutorConfig,
}

impl ExecutorShared {
    fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    fn enqueue(
        &self,
        host: HostToken,
        pool: PoolKind,
        priority: PriorityClass,
        job: Job,
    ) -> Result<(), PoolError> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(PoolError::Closed);
        }
        state.push(host, pool, priority, job);
        self.condvar.notify_one();
        Ok(())
    }

    fn enqueue_with_bytes(
        &self,
        host: HostToken,
        pool: PoolKind,
        priority: PriorityClass,
        byte_count: u64,
        job: Job,
    ) -> Result<(), ByteAdmitError> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(ByteAdmitError::Closed);
        }
        let used = state
            .queued_bytes
            .saturating_add(state.active_bytes)
            .saturating_add(state.reserved_bytes);
        let available = self.config.process_byte_limit.saturating_sub(used);
        if byte_count > available {
            state.refusals = state.refusals.saturating_add(1);
            return Err(ByteAdmitError::BytesExhausted {
                requested: byte_count,
                available,
            });
        }
        state.push_with_bytes(host, pool, priority, job, byte_count);
        self.condvar.notify_one();
        Ok(())
    }
    fn reserve_bytes(self: &Arc<Self>, byte_count: u64) -> Result<ByteTicket, ByteAdmitError> {
        let mut state = self.state.lock();
        let used = state
            .queued_bytes
            .saturating_add(state.active_bytes)
            .saturating_add(state.reserved_bytes);
        let available = self.config.process_byte_limit.saturating_sub(used);
        if byte_count > available {
            state.refusals = state.refusals.saturating_add(1);
            return Err(ByteAdmitError::BytesExhausted {
                requested: byte_count,
                available,
            });
        }
        state.reserved_bytes = state.reserved_bytes.saturating_add(byte_count);
        Ok(ByteTicket {
            shared: Arc::clone(self),
            byte_count,
            committed: false,
        })
    }

    fn release_reserved(&self, byte_count: u64) {
        let mut state = self.state.lock();
        state.reserved_bytes = state
            .reserved_bytes
            .checked_sub(byte_count)
            .expect("released reservation must be reserved");
    }

    fn enqueue_reserved(
        &self,
        host: HostToken,
        pool: PoolKind,
        priority: PriorityClass,
        byte_count: u64,
        job: Job,
    ) -> Result<(), ByteAdmitError> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(ByteAdmitError::Closed);
        }
        state.push_reserved(host, pool, priority, job, byte_count);
        self.condvar.notify_one();
        Ok(())
    }

    fn next_job(&self, completed: Option<(HostToken, PoolKind, u64)>) -> Option<Dispatched<Job>> {
        let mut state = self.state.lock();
        if let Some((host, pool, byte_count)) = completed {
            state.complete_with_bytes(host, pool, byte_count);
            if state.pending_total != 0 {
                self.condvar.notify_one();
            }
        }
        loop {
            if let Some(job) = state.pop_next(&self.config) {
                return Some(job);
            }
            if state.closed && state.pending_total == 0 {
                return None;
            }
            self.condvar.wait(&mut state);
        }
    }

    fn retire_host(&self, host: HostToken) {
        self.state.lock().retire_host(host);
    }

    fn close(&self) {
        self.state.lock().closed = true;
        self.condvar.notify_all();
    }
}

struct HostRegistration {
    token: HostToken,
    host_id: i32,
    executor: Arc<ProcessIoExecutor>,
}

impl Drop for HostRegistration {
    fn drop(&mut self) {
        self.executor.shared.retire_host(self.token);
    }
}

struct ProcessIoExecutor {
    shared: Arc<ExecutorShared>,
    workers: OnceLock<Mutex<Vec<thread::JoinHandle<()>>>>,
    next_host_token: AtomicU64,
}

impl ProcessIoExecutor {
    fn new(config: ExecutorConfig) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(ExecutorShared {
                state: Mutex::new(QueueState::default()),
                condvar: ParkingCondvar::new(),
                config,
            }),
            workers: OnceLock::new(),
            next_host_token: AtomicU64::new(1),
        })
    }

    fn register_host(self: &Arc<Self>, host_id: i32) -> Arc<HostRegistration> {
        let token = HostToken(
            self.next_host_token
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(1)
                })
                .unwrap_or_else(|_| panic!("process IO host token space exhausted")),
        );
        Arc::new(HostRegistration {
            token,
            host_id,
            executor: Arc::clone(self),
        })
    }

    fn ensure_started(&self) {
        self.workers.get_or_init(|| {
            let mut workers = Vec::with_capacity(self.shared.config.worker_count);
            for worker_index in 0..self.shared.config.worker_count {
                let shared = Arc::clone(&self.shared);
                workers.push(
                    thread::Builder::new()
                        .name(format!("Migo-IO-{worker_index}"))
                        .spawn(move || worker_main(shared))
                        .expect("failed to spawn process IO executor thread"),
                );
            }
            Mutex::new(workers)
        });
    }

    fn submit<T, F>(
        &self,
        registration: &HostRegistration,
        pool: PoolKind,
        priority: PriorityClass,
        job: F,
    ) -> Result<JobHandle<T>, PoolError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        debug_assert!(std::ptr::eq(self, Arc::as_ptr(&registration.executor)));
        if self.shared.is_closed() {
            return Err(PoolError::Closed);
        }
        self.ensure_started();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        self.shared.enqueue(
            registration.token,
            pool,
            priority,
            Box::new(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let _ = result_tx.send(result);
            }),
        )?;
        Ok(JobHandle { rx: result_rx })
    }

    fn submit_bytes<T, F>(
        &self,
        registration: &HostRegistration,
        pool: PoolKind,
        priority: PriorityClass,
        byte_count: u64,
        job: F,
    ) -> Result<JobHandle<T>, ByteAdmitError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        debug_assert!(std::ptr::eq(self, Arc::as_ptr(&registration.executor)));
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        self.shared.enqueue_with_bytes(
            registration.token,
            pool,
            priority,
            byte_count,
            Box::new(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let _ = result_tx.send(result);
            }),
        )?;
        self.ensure_started();
        Ok(JobHandle { rx: result_rx })
    }

    fn submit_async_bytes<T, F>(
        &self,
        registration: &HostRegistration,
        pool: PoolKind,
        priority: PriorityClass,
        byte_count: u64,
        job: F,
    ) -> Result<tokio::sync::oneshot::Receiver<std::thread::Result<T>>, ByteAdmitError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        debug_assert!(std::ptr::eq(self, Arc::as_ptr(&registration.executor)));
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.shared.enqueue_with_bytes(
            registration.token,
            pool,
            priority,
            byte_count,
            Box::new(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let _ = result_tx.send(result);
            }),
        )?;
        self.ensure_started();
        Ok(result_rx)
    }

    fn submit_async<T, F>(
        &self,
        registration: &HostRegistration,
        pool: PoolKind,
        priority: PriorityClass,
        job: F,
    ) -> Result<tokio::sync::oneshot::Receiver<std::thread::Result<T>>, PoolError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        debug_assert!(std::ptr::eq(self, Arc::as_ptr(&registration.executor)));
        if self.shared.is_closed() {
            return Err(PoolError::Closed);
        }
        self.ensure_started();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.shared.enqueue(
            registration.token,
            pool,
            priority,
            Box::new(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let _ = result_tx.send(result);
            }),
        )?;
        Ok(result_rx)
    }

    #[cfg(test)]
    fn started_thread_count(&self) -> usize {
        self.workers
            .get()
            .map(|workers| workers.lock().len())
            .unwrap_or(0)
    }
}

fn worker_main(shared: Arc<ExecutorShared>) {
    let mut completed = None;
    while let Some(job) = shared.next_job(completed.take()) {
        let host = job.host;
        let pool = job.pool;
        let byte_count = job.byte_count;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.value));
        if result.is_err() {
            tracing::error!("process IO executor job wrapper panicked");
        }
        completed = Some((host, pool, byte_count));
    }
}
impl Drop for ProcessIoExecutor {
    fn drop(&mut self) {
        self.shared.close();
        let Some(workers) = self.workers.get() else {
            return;
        };
        let handles = std::mem::take(&mut *workers.lock());
        let current = thread::current().id();
        if handles.iter().any(|handle| handle.thread().id() == current) {
            // Joining any peer from inside this executor can deadlock when
            // that peer is waiting for the current job to release a class
            // slot. Dropping JoinHandles detaches them; `close` above wakes
            // the peers and they drain all already-accepted work before exit.
            return;
        }
        for handle in handles {
            let _ = handle.join();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteAdmitError {
    Closed,
    BytesExhausted { requested: u64, available: u64 },
}

impl fmt::Display for ByteAdmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("IO worker pool closed"),
            Self::BytesExhausted {
                requested,
                available,
            } => write!(
                f,
                "IO pending-byte budget exhausted: requested {requested} bytes, {available} available"
            ),
        }
    }
}
pub struct ByteTicket {
    shared: Arc<ExecutorShared>,
    byte_count: u64,
    committed: bool,
}

impl ByteTicket {
    fn into_parts(mut self) -> (Arc<ExecutorShared>, u64) {
        self.committed = true;
        (Arc::clone(&self.shared), self.byte_count)
    }
}

impl Drop for ByteTicket {
    fn drop(&mut self) {
        if !self.committed {
            self.shared.release_reserved(self.byte_count);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolError {
    Closed,
    ByteLimitExceeded { requested: u64, available: u64 },
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoolError::Closed => f.write_str("IO worker pool closed"),
            PoolError::ByteLimitExceeded {
                requested,
                available,
            } => write!(
                f,
                "IO pending-byte budget exhausted: requested {requested} bytes, {available} available"
            ),
        }
    }
}

impl From<ByteAdmitError> for PoolError {
    fn from(err: ByteAdmitError) -> Self {
        match err {
            ByteAdmitError::Closed => Self::Closed,
            ByteAdmitError::BytesExhausted {
                requested,
                available,
            } => Self::ByteLimitExceeded {
                requested,
                available,
            },
        }
    }
}

impl From<PoolError> for EngineError {
    fn from(err: PoolError) -> Self {
        match err {
            PoolError::Closed => {
                EngineError::new(ErrorCode::IoError).with_detail("IO worker pool closed")
            }
            PoolError::ByteLimitExceeded {
                requested,
                available,
            } => EngineError::new(ErrorCode::IoError).with_detail(format!(
                "IO pending-byte budget exhausted: requested {requested} bytes, {available} available"
            )),
        }
    }
}

pub struct JobHandle<T> {
    rx: mpsc::Receiver<std::thread::Result<T>>,
}

impl<T> JobHandle<T> {
    pub fn join(self) -> Result<T, PoolError> {
        match self.rx.recv().map_err(|_| PoolError::Closed)? {
            Ok(value) => Ok(value),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
}

static PROCESS_IO_EXECUTOR: OnceLock<Arc<ProcessIoExecutor>> = OnceLock::new();

fn default_executor_config() -> ExecutorConfig {
    let worker_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2)
        .clamp(2, 6);
    ExecutorConfig::for_workers(worker_count)
}

fn process_io_executor() -> Arc<ProcessIoExecutor> {
    Arc::clone(
        PROCESS_IO_EXECUTOR.get_or_init(|| ProcessIoExecutor::new(default_executor_config())),
    )
}

#[derive(Clone)]
pub struct IoPools {
    registration: Arc<HostRegistration>,
}

impl IoPools {
    pub fn new(host_id: i32) -> Self {
        Self::with_executor(host_id, process_io_executor())
    }

    pub fn run<T, F>(&self, pool: PoolKind, priority: PriorityClass, job: F) -> Result<T, PoolError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        self.registration
            .executor
            .submit(&self.registration, pool, priority, job)?
            .join()
    }

    pub fn submit_async<T, F>(
        &self,
        pool: PoolKind,
        priority: PriorityClass,
        job: F,
    ) -> Result<tokio::sync::oneshot::Receiver<std::thread::Result<T>>, PoolError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        self.registration
            .executor
            .submit_async(&self.registration, pool, priority, job)
    }
    pub fn run_bytes<T, F>(
        &self,
        pool: PoolKind,
        priority: PriorityClass,
        byte_count: u64,
        job: F,
    ) -> Result<T, ByteAdmitError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        self.registration
            .executor
            .submit_bytes(&self.registration, pool, priority, byte_count, job)?
            .join()
            .map_err(|_| ByteAdmitError::Closed)
    }

    pub fn submit_async_bytes<T, F>(
        &self,
        pool: PoolKind,
        priority: PriorityClass,
        byte_count: u64,
        job: F,
    ) -> Result<tokio::sync::oneshot::Receiver<std::thread::Result<T>>, ByteAdmitError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        self.registration.executor.submit_async_bytes(
            &self.registration,
            pool,
            priority,
            byte_count,
            job,
        )
    }
    pub fn reserve_bytes(&self, byte_count: u64) -> Result<ByteTicket, ByteAdmitError> {
        self.registration.executor.shared.reserve_bytes(byte_count)
    }

    pub fn submit_reserved<T, F>(
        &self,
        ticket: ByteTicket,
        pool: PoolKind,
        priority: PriorityClass,
        job: F,
    ) -> Result<JobHandle<T>, ByteAdmitError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if !Arc::ptr_eq(&ticket.shared, &self.registration.executor.shared) {
            return Err(ByteAdmitError::Closed);
        }
        let (shared, byte_count) = ticket.into_parts();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let result = shared.enqueue_reserved(
            self.registration.token,
            pool,
            priority,
            byte_count,
            Box::new(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let _ = result_tx.send(result);
            }),
        );
        if let Err(error) = result {
            shared.release_reserved(byte_count);
            return Err(error);
        }
        self.registration.executor.ensure_started();
        Ok(JobHandle { rx: result_rx })
    }

    fn with_executor(host_id: i32, executor: Arc<ProcessIoExecutor>) -> Self {
        Self {
            registration: executor.register_host(host_id),
        }
    }

    #[cfg(test)]

    pub(crate) fn shared_pair_for_test(
        first_host_id: i32,
        second_host_id: i32,
        worker_count: usize,
    ) -> (Self, Self) {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(worker_count));
        (
            Self::with_executor(first_host_id, Arc::clone(&executor)),
            Self::with_executor(second_host_id, executor),
        )
    }

    #[cfg(test)]
    pub(crate) fn local_for_test(host_id: i32, worker_count: usize) -> Self {
        Self::with_executor(
            host_id,
            ProcessIoExecutor::new(ExecutorConfig::for_workers(worker_count)),
        )
    }
    #[cfg(test)]
    pub(crate) fn local_with_byte_limit_for_test(
        host_id: i32,
        worker_count: usize,
        byte_limit: u64,
    ) -> Self {
        Self::with_executor(
            host_id,
            ProcessIoExecutor::new(
                ExecutorConfig::for_workers(worker_count).with_byte_limit(byte_limit),
            ),
        )
    }

    #[cfg(test)]
    pub(crate) fn pending_work_for_test(&self) -> usize {
        self.registration.executor.shared.state.lock().pending_total
    }

    #[cfg(test)]
    pub(crate) fn started_thread_count_for_test(&self) -> usize {
        self.registration.executor.started_thread_count()
    }
    #[cfg(test)]
    pub(crate) fn byte_metrics_for_test(&self) -> (u64, u64, Option<std::time::Duration>, u64) {
        let state = self.registration.executor.shared.state.lock();
        (
            state.queued_bytes,
            state.active_bytes,
            state.oldest_enqueue.map(|instant| instant.elapsed()),
            state.refusals,
        )
    }
}

impl fmt::Debug for IoPools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IoPools")
            .field("host_id", &self.registration.host_id)
            .field("host_token", &self.registration.token)
            .finish()
    }
}

#[cfg(test)]
mod r5_queue_tests {
    use super::{ExecutorConfig, FairLane, HostToken, QueueState, pool_index};
    use crate::task::{PoolKind, PriorityClass};

    #[test]
    fn lane_is_fifo_within_one_host() {
        let mut lane = FairLane::default();
        lane.push(HostToken(1), 1_u32);
        lane.push(HostToken(1), 2_u32);

        assert_eq!(lane.pop_where(|_| true), Some((HostToken(1), 1)));
        assert_eq!(lane.pop_where(|_| true), Some((HostToken(1), 2)));
    }

    #[test]
    fn lane_round_robins_hosts_with_unequal_backlog() {
        let mut lane = FairLane::default();
        for id in 1..=4 {
            lane.push(HostToken(1), id);
        }
        lane.push(HostToken(2), 20);

        assert_eq!(lane.pop_where(|_| true), Some((HostToken(1), 1)));
        assert_eq!(lane.pop_where(|_| true), Some((HostToken(2), 20)));
        assert_eq!(lane.pop_where(|_| true), Some((HostToken(1), 2)));
    }

    #[test]
    fn blocking_precedes_async_and_background() {
        let config = ExecutorConfig::for_workers(6);
        let mut state = QueueState::default();
        state.push(HostToken(1), PoolKind::Fs, PriorityClass::Background, 1_u32);
        state.push(
            HostToken(1),
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            2,
        );
        state.push(
            HostToken(1),
            PoolKind::Fs,
            PriorityClass::ForegroundBlocking,
            3,
        );

        assert_eq!(state.pop_next(&config).unwrap().value, 3);
        state.complete(HostToken(1), PoolKind::Fs);
        assert_eq!(state.pop_next(&config).unwrap().value, 2);
        state.complete(HostToken(1), PoolKind::Fs);
        assert_eq!(state.pop_next(&config).unwrap().value, 1);
    }

    #[test]
    fn background_gets_the_sixteenth_aging_opportunity() {
        let config = ExecutorConfig::for_workers(6);
        let mut state = QueueState::default();
        state.push(
            HostToken(1),
            PoolKind::Fs,
            PriorityClass::Background,
            999_u32,
        );
        for id in 0..33 {
            state.push(
                HostToken(1),
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                id,
            );
        }

        let mut background_at = None;
        for dispatch_index in 0..34 {
            let dispatched = state.pop_next(&config).unwrap();
            state.complete(dispatched.host, dispatched.pool);
            if dispatched.value == 999 {
                background_at = Some(dispatch_index);
                break;
            }
        }

        assert_eq!(background_at, Some(16));
    }

    #[test]
    fn class_cursor_prevents_one_class_from_owning_a_priority() {
        let config = ExecutorConfig::for_workers(6);
        let mut state = QueueState::default();
        for id in [1_u32, 2] {
            state.push(
                HostToken(1),
                PoolKind::Fs,
                PriorityClass::ForegroundBlocking,
                id,
            );
        }
        for id in [20_u32, 21] {
            state.push(
                HostToken(1),
                PoolKind::Image,
                PriorityClass::ForegroundBlocking,
                id,
            );
        }

        let mut order = Vec::new();
        for _ in 0..4 {
            let dispatched = state.pop_next(&config).unwrap();
            order.push(dispatched.value);
            state.complete(dispatched.host, dispatched.pool);
        }
        assert_eq!(order, vec![1, 20, 2, 21]);
    }

    #[test]
    fn blocked_class_is_skipped_without_dropping_its_job() {
        let config = ExecutorConfig::for_workers(6);
        let mut state = QueueState::default();
        state.active_by_class[pool_index(PoolKind::Image)] = config.class_cap(PoolKind::Image);
        state.push(
            HostToken(1),
            PoolKind::Image,
            PriorityClass::ForegroundBlocking,
            20_u32,
        );
        state.push(
            HostToken(1),
            PoolKind::Fs,
            PriorityClass::ForegroundBlocking,
            1,
        );

        let fs = state.pop_next(&config).unwrap();
        assert_eq!(fs.value, 1);
        state.complete(fs.host, fs.pool);
        assert_eq!(state.pending_total, 1, "blocked image job remains queued");
    }

    #[test]
    fn single_host_can_fill_all_fs_slots() {
        let config = ExecutorConfig::for_workers(6);
        let mut state = QueueState::default();
        for id in 0_u32..7 {
            state.push(
                HostToken(1),
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                id,
            );
        }

        for expected in 0_u32..6 {
            assert_eq!(state.pop_next(&config).unwrap().value, expected);
        }
        assert!(state.pop_next(&config).is_none(), "all workers are active");
        assert_eq!(state.active_total, 6);
        assert_eq!(state.pending_total, 1);
    }

    #[test]
    fn contending_host_caps_new_dispatches_at_half_workers() {
        let config = ExecutorConfig::for_workers(6);
        let mut state = QueueState::default();
        for id in 0_u32..3 {
            state.push(
                HostToken(1),
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                id,
            );
            assert_eq!(state.pop_next(&config).unwrap().host, HostToken(1));
        }
        state.push(
            HostToken(1),
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            10,
        );
        state.push(
            HostToken(2),
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            20,
        );

        let next = state.pop_next(&config).unwrap();
        assert_eq!(next.host, HostToken(2));
        assert_eq!(next.value, 20);
    }

    /// The per-host cap is a sharing rule, so it may only cost a worker when
    /// there is someone to share with. Here the peer's only queued job sits in
    /// a class that is already full, so honouring the cap would park a thread
    /// and stall the one host that can actually use it -- fairness paid for
    /// with throughput that nobody collects.
    #[test]
    fn capped_host_still_gets_a_worker_no_peer_can_use() {
        // Four workers, so the contended cap is two and the host holding three
        // is unambiguously over it.
        let config = ExecutorConfig::for_workers(4);
        let mut state = QueueState::default();
        for id in 0_u32..3 {
            state.push(
                HostToken(1),
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                id,
            );
            assert_eq!(state.pop_next(&config).unwrap().host, HostToken(1));
        }

        // Host 1 still has work. Host 2's only work is Archive, whose class cap
        // is spent, so nothing host 2 owns can be dispatched.
        state.push(
            HostToken(1),
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            10,
        );
        state.push(
            HostToken(2),
            PoolKind::Archive,
            PriorityClass::ForegroundAsync,
            20,
        );
        state.active_by_class[pool_index(PoolKind::Archive)] = config.class_cap(PoolKind::Archive);

        let next = state
            .pop_next(&config)
            .expect("a free worker must not park while dispatchable work is queued");
        assert_eq!(next.host, HostToken(1));
        assert_eq!(next.value, 10);
    }

    /// The relaxed pass must not become a way around the class caps: those
    /// bound how much of a resource is in flight, not how it is shared.
    #[test]
    fn relaxed_pass_still_honours_class_caps() {
        let config = ExecutorConfig::for_workers(4);
        let mut state = QueueState::default();
        state.active_by_class[pool_index(PoolKind::Archive)] = config.class_cap(PoolKind::Archive);
        state.push(
            HostToken(1),
            PoolKind::Archive,
            PriorityClass::ForegroundBlocking,
            1_u32,
        );

        assert!(
            state.pop_next(&config).is_none(),
            "a full class must stay full even when no other host is waiting"
        );
        assert_eq!(state.pending_total, 1);
    }

    #[test]
    fn image_pack_and_archive_respect_class_caps() {
        let config = ExecutorConfig::for_workers(6);
        for (pool, cap) in [
            (PoolKind::Image, 2_usize),
            (PoolKind::Pack, 2),
            (PoolKind::Archive, 2),
            // The one class that must stay serial: a single ingest can hold
            // an image's RGBA, its ETC2 blocks and its KTX2 container at once.
            (PoolKind::Ingest, 1),
        ] {
            let mut state = QueueState::default();
            for id in 0_u32..4 {
                state.push(HostToken(1), pool, PriorityClass::ForegroundAsync, id);
            }
            for _ in 0..cap {
                assert!(state.pop_next(&config).is_some());
            }
            assert!(
                state.pop_next(&config).is_none(),
                "{pool:?} must stop dispatching at its class cap"
            );
            assert_eq!(state.active_by_class[pool_index(pool)], cap);
        }
    }

    #[test]
    fn completing_a_job_releases_class_and_host_slots() {
        let config = ExecutorConfig::for_workers(2);
        let mut state = QueueState::default();
        state.push(
            HostToken(7),
            PoolKind::Image,
            PriorityClass::ForegroundAsync,
            1_u32,
        );
        state.push(
            HostToken(7),
            PoolKind::Image,
            PriorityClass::ForegroundAsync,
            2,
        );

        let first = state.pop_next(&config).unwrap();
        assert!(state.pop_next(&config).is_none());
        state.complete(first.host, first.pool);
        assert_eq!(state.active_total, 0);
        assert!(!state.active_by_host.contains_key(&HostToken(7)));
        assert_eq!(state.pop_next(&config).unwrap().value, 2);
    }

    #[test]
    fn live_host_keeps_empty_lane_for_capacity_reuse() {
        let config = ExecutorConfig::for_workers(2);
        let mut state = QueueState::default();
        state.push(
            HostToken(3),
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            1_u32,
        );
        let job = state.pop_next(&config).unwrap();
        state.complete(job.host, job.pool);

        assert!(state.contains_host(HostToken(3)));
    }

    #[test]
    fn retiring_empty_host_removes_lane_state_immediately() {
        let config = ExecutorConfig::for_workers(2);
        let mut state = QueueState::default();
        state.push(
            HostToken(4),
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            1_u32,
        );
        let job = state.pop_next(&config).unwrap();
        state.complete(job.host, job.pool);

        state.retire_host(HostToken(4));

        assert!(!state.contains_host(HostToken(4)));
        assert!(!state.is_retired(HostToken(4)));
    }

    #[test]
    fn retiring_queued_host_drains_then_removes_accepted_state() {
        let config = ExecutorConfig::for_workers(2);
        let mut state = QueueState::default();
        state.push(
            HostToken(5),
            PoolKind::Pack,
            PriorityClass::ForegroundAsync,
            7_u32,
        );
        state.retire_host(HostToken(5));
        assert!(state.contains_host(HostToken(5)));
        assert!(state.is_retired(HostToken(5)));

        let job = state.pop_next(&config).unwrap();
        assert_eq!(job.value, 7);
        state.complete(job.host, job.pool);

        assert!(!state.contains_host(HostToken(5)));
        assert!(!state.is_retired(HostToken(5)));
    }
}

#[cfg(test)]
mod r5_executor_tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Duration;

    use super::{
        ExecutorConfig, IoPools, PoolError, ProcessIoExecutor, default_executor_config, pool_index,
    };
    use crate::task::{PoolKind, PriorityClass};

    #[test]
    fn executor_starts_no_threads_before_first_submit() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(3));
        let _host = executor.register_host(10);

        assert_eq!(executor.started_thread_count(), 0);
    }

    #[test]
    fn first_submit_starts_exactly_configured_threads_once() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(3));
        let host = executor.register_host(11);

        let first = executor
            .submit(&host, PoolKind::Fs, PriorityClass::ForegroundAsync, || {
                1_u32
            })
            .unwrap();
        assert_eq!(first.join().unwrap(), 1);
        assert_eq!(executor.started_thread_count(), 3);

        let second = executor
            .submit(&host, PoolKind::Fs, PriorityClass::ForegroundAsync, || {
                2_u32
            })
            .unwrap();
        assert_eq!(second.join().unwrap(), 2);
        assert_eq!(executor.started_thread_count(), 3);
    }

    #[test]
    fn worker_names_use_migo_io_prefix() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        let host = executor.register_host(12);
        let name = executor
            .submit(&host, PoolKind::Fs, PriorityClass::ForegroundAsync, || {
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string()
            })
            .unwrap()
            .join()
            .unwrap();

        assert!(
            name.starts_with("Migo-IO-"),
            "unexpected worker name: {name}"
        );
    }

    #[test]
    fn same_numeric_host_id_gets_distinct_registration_tokens() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        let first = executor.register_host(13);
        let second = executor.register_host(13);

        assert_ne!(first.token, second.token);
        assert_eq!(first.host_id, second.host_id);
    }

    #[test]
    fn host_registration_token_exhaustion_never_wraps() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        executor
            .next_host_token
            .store(u64::MAX - 1, Ordering::Relaxed);

        let last = executor.register_host(13);
        assert_eq!(last.token, super::HostToken(u64::MAX - 1));
        let exhausted = catch_unwind(AssertUnwindSafe(|| executor.register_host(13)));
        assert!(exhausted.is_err());
    }

    #[test]
    fn submit_async_resolves_on_process_worker() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        let host = executor.register_host(14);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let name = runtime.block_on(async {
            executor
                .submit_async(
                    &host,
                    PoolKind::Image,
                    PriorityClass::ForegroundAsync,
                    || {
                        std::thread::current()
                            .name()
                            .unwrap_or("unnamed")
                            .to_string()
                    },
                )
                .unwrap()
                .await
                .unwrap()
                .unwrap()
        });

        assert!(name.starts_with("Migo-IO-"));
    }

    #[test]
    fn user_panic_payload_is_preserved_and_capacity_survives() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        let host = executor.register_host(15);
        let panicking = executor
            .submit(
                &host,
                PoolKind::Pack,
                PriorityClass::ForegroundAsync,
                || -> () { panic!("r5-panic-payload") },
            )
            .unwrap();

        let payload = catch_unwind(AssertUnwindSafe(|| panicking.join()))
            .expect_err("join must resume the user panic");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"r5-panic-payload"));

        let next = executor
            .submit(
                &host,
                PoolKind::Pack,
                PriorityClass::ForegroundAsync,
                || 42_u32,
            )
            .unwrap();
        assert_eq!(next.join().unwrap(), 42);
    }

    #[test]
    fn concurrent_submitters_start_one_worker_set_and_run_once() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(4));
        let host = executor.register_host(16);
        let runs = Arc::new(AtomicUsize::new(0));
        let submitters: Vec<_> = (0..32)
            .map(|_| {
                let executor = Arc::clone(&executor);
                let host = Arc::clone(&host);
                let runs = Arc::clone(&runs);
                std::thread::spawn(move || {
                    executor
                        .submit(
                            &host,
                            PoolKind::Fs,
                            PriorityClass::ForegroundAsync,
                            move || {
                                runs.fetch_add(1, Ordering::SeqCst);
                            },
                        )
                        .unwrap()
                        .join()
                        .unwrap();
                })
            })
            .collect();

        for submitter in submitters {
            submitter.join().unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 32);
        assert_eq!(executor.started_thread_count(), 4);
    }

    fn assert_runtime_class_cap(pool: PoolKind, expected_cap: usize) {
        let config = ExecutorConfig::for_workers(4);
        let executor = ProcessIoExecutor::new(config.clone());
        let host = executor.register_host(17);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let current = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let mut handles = Vec::new();

        for _ in 0..8 {
            let gate = Arc::clone(&gate);
            let current = Arc::clone(&current);
            let maximum = Arc::clone(&maximum);
            let started_tx = started_tx.clone();
            handles.push(
                executor
                    .submit(&host, pool, PriorityClass::ForegroundAsync, move || {
                        let active = current.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(active, Ordering::SeqCst);
                        started_tx.send(()).unwrap();

                        let (lock, condvar) = &*gate;
                        let mut released = lock.lock().unwrap();
                        while !*released {
                            released = condvar.wait(released).unwrap();
                        }
                        current.fetch_sub(1, Ordering::SeqCst);
                    })
                    .unwrap(),
            );
        }
        drop(started_tx);

        let all_expected_started =
            (0..expected_cap).all(|_| started_rx.recv_timeout(Duration::from_secs(2)).is_ok());
        let (active, pending) = {
            let state = executor.shared.state.lock();
            (state.active_by_class[pool_index(pool)], state.pending_total)
        };

        let (lock, condvar) = &*gate;
        *lock.lock().unwrap() = true;
        condvar.notify_all();
        for handle in handles {
            handle.join().unwrap();
        }

        assert!(
            all_expected_started,
            "{pool:?} did not reach its configured cap"
        );
        assert_eq!(active, expected_cap, "{pool:?} exceeded its class cap");
        assert_eq!(pending, 8 - expected_cap);
        assert_eq!(maximum.load(Ordering::SeqCst), expected_cap);
    }

    #[test]
    fn runtime_class_caps_bound_concurrent_work() {
        for (pool, expected_cap) in [
            (PoolKind::Fs, 4),
            (PoolKind::Pack, 2),
            (PoolKind::Image, 2),
            (PoolKind::Archive, 2),
            (PoolKind::Ingest, 1),
        ] {
            assert_runtime_class_cap(pool, expected_cap);
        }
    }

    #[test]
    fn close_before_first_submit_rejects_without_starting_threads() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        let host = executor.register_host(18);
        executor.shared.close();

        let result = executor.submit(&host, PoolKind::Fs, PriorityClass::ForegroundAsync, || {
            1_u32
        });

        assert!(matches!(result, Err(PoolError::Closed)));
        assert_eq!(executor.started_thread_count(), 0);
    }

    #[test]
    fn close_drains_jobs_accepted_before_close() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(1));
        let host = executor.register_host(19);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();

        let first_gate = Arc::clone(&gate);
        let first = executor
            .submit(
                &host,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                move || {
                    started_tx.send(()).unwrap();
                    let (lock, condvar) = &*first_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = condvar.wait(released).unwrap();
                    }
                    1_u32
                },
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = executor
            .submit(&host, PoolKind::Fs, PriorityClass::ForegroundAsync, || {
                2_u32
            })
            .unwrap();

        executor.shared.close();
        let (lock, condvar) = &*gate;
        *lock.lock().unwrap() = true;
        condvar.notify_all();

        assert_eq!(first.join().unwrap(), 1);
        assert_eq!(second.join().unwrap(), 2);
    }

    #[test]
    fn final_executor_owner_can_drop_inside_its_worker() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        let host = executor.register_host(20);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let executor_in_job = Arc::clone(&executor);
        let job_gate = Arc::clone(&gate);

        let handle = executor
            .submit(
                &host,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                move || {
                    started_tx.send(()).unwrap();
                    let (lock, condvar) = &*job_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = condvar.wait(released).unwrap();
                    }
                    drop(executor_in_job);
                    21_u32
                },
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        drop(host);
        drop(executor);
        let (lock, condvar) = &*gate;
        *lock.lock().unwrap() = true;
        condvar.notify_all();

        let result = handle
            .rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker self-drop must not deadlock")
            .expect("worker result must not be disconnected");
        assert_eq!(result, 21);
    }

    #[test]
    fn worker_side_final_drop_does_not_join_workers_blocked_by_its_class_slot() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        let host = executor.register_host(23);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let executor_in_job = Arc::clone(&executor);
        let first_gate = Arc::clone(&gate);
        let first = executor
            .submit(
                &host,
                PoolKind::Archive,
                PriorityClass::ForegroundAsync,
                move || {
                    started_tx.send(()).unwrap();
                    let (lock, condvar) = &*first_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = condvar.wait(released).unwrap();
                    }
                    drop(executor_in_job);
                    1_u32
                },
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = executor
            .submit(
                &host,
                PoolKind::Archive,
                PriorityClass::ForegroundAsync,
                || 2_u32,
            )
            .unwrap();

        drop(host);
        drop(executor);
        let (lock, condvar) = &*gate;
        *lock.lock().unwrap() = true;
        condvar.notify_all();

        let first_result = first
            .rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker-side executor drop must not wait on a class-blocked worker")
            .unwrap();
        let second_result = second
            .rx
            .recv_timeout(Duration::from_secs(2))
            .expect("accepted archive work must drain after close")
            .unwrap();
        assert_eq!((first_result, second_result), (1, 2));
    }

    #[test]
    fn completion_wakes_a_peer_when_class_and_host_caps_unblock_two_jobs() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(2));
        let host_a = executor.register_host(24);
        let host_b = executor.register_host(25);
        let archive_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let fs_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (archive_a_started_tx, archive_a_started_rx) = mpsc::channel();
        let (archive_b_started_tx, archive_b_started_rx) = mpsc::channel();
        let (fs_started_tx, fs_started_rx) = mpsc::channel();

        let first_gate = Arc::clone(&archive_gate);
        let archive_a = executor
            .submit(
                &host_a,
                PoolKind::Archive,
                PriorityClass::ForegroundAsync,
                move || {
                    archive_a_started_tx.send(()).unwrap();
                    let (lock, condvar) = &*first_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = condvar.wait(released).unwrap();
                    }
                },
            )
            .unwrap();
        archive_a_started_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();

        let archive_b = executor
            .submit(
                &host_b,
                PoolKind::Archive,
                PriorityClass::ForegroundAsync,
                move || archive_b_started_tx.send(()).unwrap(),
            )
            .unwrap();
        let second_gate = Arc::clone(&fs_gate);
        let fs_a = executor
            .submit(
                &host_a,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                move || {
                    fs_started_tx.send(()).unwrap();
                    let (lock, condvar) = &*second_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = condvar.wait(released).unwrap();
                    }
                },
            )
            .unwrap();

        let (lock, condvar) = &*archive_gate;
        *lock.lock().unwrap() = true;
        condvar.notify_all();
        fs_started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let archive_b_started = archive_b_started_rx
            .recv_timeout(Duration::from_millis(250))
            .is_ok();

        let (lock, condvar) = &*fs_gate;
        *lock.lock().unwrap() = true;
        condvar.notify_all();
        archive_a.join().unwrap();
        fs_a.join().unwrap();
        archive_b.join().unwrap();

        assert!(
            archive_b_started,
            "a peer worker stayed parked after completion unblocked archive capacity"
        );
    }

    /// Section 6.4 lists per-host fairness on the shared IO executor among the
    /// properties that are "already enforced", and Section 7.3 records it as the
    /// one of those with no gate named against it. `QueueState`'s own tests drive
    /// the policy directly; this drives it the way a Session does -- through
    /// `submit`, real workers, and the dispatch that follows a completion.
    ///
    /// The property: while two hosts both have work queued, a worker that frees
    /// goes to the host that is not already over its contended cap. Without it,
    /// one game's queue depth decides when another game's IO runs.
    #[test]
    fn a_worker_freed_under_contention_goes_to_the_host_that_is_not_hogging_it() {
        // Four workers, so the contended cap is two: the flooding host holding
        // three when one frees is unambiguously over it.
        const WORKERS: usize = 4;
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(WORKERS));
        let flooder = executor.register_host(51);
        let neighbour = executor.register_host(52);

        // Exit permits rather than a broadcast gate, because the test has to free
        // *exactly one* worker. Releasing them all would let the neighbour run on
        // whichever worker happened to be idle, which is the question rather than
        // the answer. The permits are the test's alone and share no deadline with
        // the neighbour's wait below: a timeout that freed a worker would hand an
        // unfair executor the very thing the neighbour was waiting for.
        let permits = Arc::new((Mutex::new(0_usize), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();

        let mut flood = Vec::new();
        for _ in 0..WORKERS + 2 {
            let permits = Arc::clone(&permits);
            let started_tx = started_tx.clone();
            flood.push(
                executor
                    .submit(
                        &flooder,
                        PoolKind::Fs,
                        PriorityClass::ForegroundAsync,
                        move || {
                            started_tx.send(()).unwrap();
                            let (lock, condvar) = &*permits;
                            let mut left = lock.lock().unwrap();
                            while *left == 0 {
                                left = condvar.wait(left).unwrap();
                            }
                            *left -= 1;
                        },
                    )
                    .unwrap(),
            );
        }

        // The neighbour must arrive at a *full* executor. Handed an idle worker it
        // would run whatever the policy said, and the gate would pass having
        // observed nothing -- so saturation is asserted, not assumed.
        for _ in 0..WORKERS {
            started_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the flooding host must occupy every worker before the neighbour submits");
        }
        assert!(
            started_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "more than {WORKERS} flooding jobs are running, so this executor is not              saturated the way the rest of this test assumes"
        );

        let (ran_tx, ran_rx) = mpsc::channel();
        let neighbour_job = executor
            .submit(
                &neighbour,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                move || ran_tx.send(()).unwrap(),
            )
            .unwrap();

        // Free exactly one worker. The flooding host still holds three of the four
        // and still has two jobs queued, so the freed one is the neighbour's.
        {
            let (lock, condvar) = &*permits;
            *lock.lock().unwrap() += 1;
            condvar.notify_one();
        }
        let neighbour_ran = ran_rx.recv_timeout(Duration::from_secs(5)).is_ok();

        // Drain before asserting: on failure the flooding jobs are still parked,
        // and a bare assertion here would leave the suite deadlocked on the joins
        // instead of reporting the starvation.
        {
            let (lock, condvar) = &*permits;
            *lock.lock().unwrap() += flood.len();
            condvar.notify_all();
        }
        for job in flood {
            job.join().unwrap();
        }
        neighbour_job.join().unwrap();

        assert!(
            neighbour_ran,
            "the worker freed under contention went to the flooding host's backlog,              so one game's queue depth decides when another game's IO runs"
        );
    }

    #[test]
    fn io_pools_share_one_process_executor_but_not_registration_tokens() {
        let first = IoPools::new(30);
        let second = IoPools::new(30);

        assert!(Arc::ptr_eq(
            &first.registration.executor,
            &second.registration.executor,
        ));
        assert_ne!(first.registration.token, second.registration.token);
    }

    #[test]
    fn default_process_worker_count_is_mobile_bounded() {
        let worker_count = default_executor_config().worker_count;
        assert!((2..=6).contains(&worker_count));
    }
}

#[cfg(test)]
mod r5_byte_credit_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use parking_lot::{Condvar, Mutex};

    use super::{ByteAdmitError, ExecutorConfig, IoPools, ProcessIoExecutor};
    use crate::task::{PoolKind, PriorityClass};

    fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, condvar) = &**gate;
        *lock.lock() = true;
        condvar.notify_all();
    }
    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn byte_saturation_refuses_before_item_count() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(1).with_byte_limit(10));
        let host = executor.register_host(60);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let first_gate = Arc::clone(&gate);
        let first = executor
            .submit_bytes(
                &host,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                8,
                move || {
                    started_tx.send(()).unwrap();
                    let (lock, condvar) = &*first_gate;
                    let mut released = lock.lock();
                    while !*released {
                        condvar.wait(&mut released);
                    }
                },
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let dropped = Arc::new(AtomicUsize::new(0));
        let probe = DropProbe(Arc::clone(&dropped));
        let refused = executor.submit_bytes(
            &host,
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            4,
            move || drop(probe),
        );
        assert!(matches!(
            refused,
            Err(ByteAdmitError::BytesExhausted {
                requested: 4,
                available: 2
            })
        ));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(executor.shared.state.lock().active_total, 1);
        assert_eq!(executor.shared.state.lock().pending_total, 0);

        release(&gate);
        first.join().unwrap();
    }

    #[test]
    fn run_bytes_preserves_structured_saturation_at_public_caller() {
        let pools = IoPools::local_with_byte_limit_for_test(63, 1, 4);
        let refused = pools.run_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 8, || ());
        assert!(matches!(
            refused,
            Err(ByteAdmitError::BytesExhausted {
                requested: 8,
                available: 4
            })
        ));
    }

    #[test]
    fn queued_active_age_and_refusal_counters_follow_credit_lifecycle() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(1).with_byte_limit(10));
        let host = executor.register_host(61);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let first_gate = Arc::clone(&gate);
        let first = executor
            .submit_bytes(
                &host,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                7,
                move || {
                    started_tx.send(()).unwrap();
                    let (lock, condvar) = &*first_gate;
                    let mut released = lock.lock();
                    while !*released {
                        condvar.wait(&mut released);
                    }
                },
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = executor
            .submit_bytes(
                &host,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                2,
                || {},
            )
            .unwrap();
        let refused = executor.submit_bytes(
            &host,
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            2,
            || {},
        );
        assert!(matches!(
            refused,
            Err(ByteAdmitError::BytesExhausted {
                requested: 2,
                available: 1
            })
        ));
        {
            let state = executor.shared.state.lock();
            assert_eq!(state.queued_bytes, 2);
            assert_eq!(state.active_bytes, 7);
            assert!(state.oldest_enqueue.is_some());
            assert_eq!(state.refusals, 1);
        }

        release(&gate);
        first.join().unwrap();
        second.join().unwrap();
        let state = executor.shared.state.lock();
        assert_eq!(state.queued_bytes, 0);
        assert_eq!(state.active_bytes, 0);
        assert!(state.oldest_enqueue.is_none());
        assert_eq!(state.refusals, 1);
    }

    #[test]
    fn dropping_uncommitted_ticket_returns_credit_once() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(1).with_byte_limit(8));
        let ticket = executor.shared.reserve_bytes(8).unwrap();
        drop(ticket);
        let ticket = executor.shared.reserve_bytes(8).unwrap();
        drop(ticket);
        let host = executor.register_host(62);
        let completed = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&completed);
        let job = executor
            .submit_bytes(
                &host,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                8,
                move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                },
            )
            .unwrap();
        job.join().unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        let state = executor.shared.state.lock();
        assert_eq!(state.queued_bytes, 0);
        assert_eq!(state.active_bytes, 0);
        assert_eq!(state.reserved_bytes, 0);
    }

    #[test]
    fn reserved_and_direct_admission_share_one_process_bound() {
        let executor = ProcessIoExecutor::new(ExecutorConfig::for_workers(1).with_byte_limit(8));
        let ticket = executor.shared.reserve_bytes(6).unwrap();
        let host = executor.register_host(64);
        let refused = executor.submit_bytes(
            &host,
            PoolKind::Fs,
            PriorityClass::ForegroundAsync,
            4,
            || (),
        );
        assert!(matches!(
            refused,
            Err(ByteAdmitError::BytesExhausted {
                requested: 4,
                available: 2
            })
        ));
        drop(ticket);
        let accepted = executor
            .submit_bytes(
                &host,
                PoolKind::Fs,
                PriorityClass::ForegroundAsync,
                8,
                || (),
            )
            .unwrap();
        accepted.join().unwrap();
    }
}
