//! Process-wide, poll-scoped deadline watchdog.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use deno_core::v8;
use shared::error::{EngineError, EngineResult, ErrorCode};

// ---------------------------------------------------------------------------
// Atomic per-isolate deadline state machine
// ---------------------------------------------------------------------------

/// `execution_state`: no V8 execution is active.
const DISARMED: u64 = 0;
/// `execution_state`: a timeout won and termination is sticky for this isolate.
const TERMINATED: u64 = 1;
// Any value `>= 2` encodes an absolute monotonic deadline (ms since the
// scheduler epoch). `InitOptions` clamps the public timeout to 5-120s, so a
// real deadline is always far above the two sentinels; the scheduler also
// floors encoded deadlines at 2 as defence in depth.

/// Outcome of a monitor reconciliation at an accounted wake deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reconcile {
    /// No execution is active for this target; the monitor should drop its
    /// wake responsibility.
    Inactive,
    /// A later live deadline was observed; the monitor should keep this target
    /// scheduled and wake at the carried deadline.
    Reschedule(u64),
    /// This call won the expiry CAS (deadline -> terminal). Its caller — and
    /// only its caller — must run [`Target::fire_once`].
    Fired,
    /// Already terminal (a previous reconcile fired). Nothing to do.
    Terminal,
}

/// Lock-free deadline state for a single isolate. Normal arm/disarm are plain
/// atomics; only the unscheduled->scheduled transition needs the scheduler's
/// control mutex + condvar (handled by [`Scheduler`]).
struct Target {
    /// `DISARMED` / `TERMINATED` sentinel or an absolute encoded deadline.
    execution_state: AtomicU64,
    /// The monitor currently holds a wake responsibility for this target.
    /// Set true by the owner on the arm slow path, cleared false by the monitor
    /// once it observes no live deadline. Kept set across normal disarm/re-arm
    /// cycles so the hot path never locks or notifies.
    scheduled: AtomicBool,
    /// The (possibly older) deadline the monitor has already accounted for. With
    /// a fixed timeout and monotonic clock a later arm is never earlier than
    /// this, so a re-arm while `scheduled` can safely skip the notify.
    wake_deadline: AtomicU64,
    /// Thread-safe termination action (V8 `terminate_execution`).
    terminate: Arc<dyn Fn() + Send + Sync>,
    /// Once-only timeout observer, invoked with the configured timeout.
    observer: Arc<dyn Fn(Duration) + Send + Sync>,
    /// Guards `terminate` + `observer` so they run exactly once.
    observer_fired: AtomicBool,
}

impl Target {
    fn new(
        terminate: Arc<dyn Fn() + Send + Sync>,
        observer: Arc<dyn Fn(Duration) + Send + Sync>,
    ) -> Self {
        Self {
            execution_state: AtomicU64::new(DISARMED),
            scheduled: AtomicBool::new(false),
            wake_deadline: AtomicU64::new(0),
            terminate,
            observer,
            observer_fired: AtomicBool::new(false),
        }
    }

    /// Arm (or re-arm) to `deadline`, refusing to overwrite terminal state.
    /// Returns `true` if the deadline is now live, `false` if already terminal.
    fn arm_at(&self, deadline: u64) -> bool {
        let mut cur = self.execution_state.load(Ordering::Acquire);
        loop {
            if cur == TERMINATED {
                return false;
            }
            match self.execution_state.compare_exchange_weak(
                cur,
                deadline,
                Ordering::SeqCst,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Clear a live deadline to `DISARMED`. Never overwrites terminal state.
    /// Returns `true` iff a live deadline was cleared.
    fn disarm(&self) -> bool {
        let mut cur = self.execution_state.load(Ordering::Acquire);
        loop {
            if cur == TERMINATED || cur == DISARMED {
                return false;
            }
            match self.execution_state.compare_exchange_weak(
                cur,
                DISARMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Reconcile at an accounted wake. `now` is an explicit encoded monotonic
    /// tick so this is fully deterministic and sleep-free in tests.
    fn reconcile(&self, now: u64) -> Reconcile {
        loop {
            let cur = self.execution_state.load(Ordering::Acquire);
            match cur {
                DISARMED => return Reconcile::Inactive,
                TERMINATED => return Reconcile::Terminal,
                deadline => {
                    if deadline > now {
                        return Reconcile::Reschedule(deadline);
                    }
                    // Expired. CAS the exact observed deadline to terminal; this
                    // CAS is the timeout/disarm linearization point. A failure
                    // means a concurrent disarm or re-arm changed the state, so
                    // re-evaluate rather than firing on a stale observation.
                    match self.execution_state.compare_exchange(
                        deadline,
                        TERMINATED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return Reconcile::Fired,
                        Err(_) => continue,
                    }
                }
            }
        }
    }

    /// Run the termination action and timeout observer exactly once, even if
    /// called again after the state is already terminal.
    fn fire_once(&self, elapsed: Duration) {
        if self
            .observer_fired
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let _ = catch_unwind(AssertUnwindSafe(|| (self.terminate)()));
            let _ = catch_unwind(AssertUnwindSafe(|| (self.observer)(elapsed)));
        }
    }
}

// ---------------------------------------------------------------------------
// Process scheduler
// ---------------------------------------------------------------------------

/// A registered target plus the configured budget passed to its observer when
/// it fires.
struct Registration {
    target: Weak<Target>,
    budget: Duration,
}

struct SchedulerInner {
    targets: Vec<Registration>,
    /// Only set for test schedulers so their monitor thread can be stopped.
    #[cfg_attr(not(test), allow(dead_code))]
    shutdown: bool,
}

/// The one process-wide monitor. Its single `Migo-Watchdog` OS thread parks on
/// a condvar whenever no registered target needs a deadline check, so a truly
/// idle process has zero steady-state watchdog wakeups.
pub(crate) struct Scheduler {
    epoch: Instant,
    inner: Mutex<SchedulerInner>,
    cv: Condvar,
    // Test-observable counters (cheap relaxed atomics).
    thread_starts: AtomicUsize,
    slow_path_arms: AtomicUsize,
    notifications: AtomicUsize,
    reconciliations: AtomicUsize,
}

static GLOBAL_SCHEDULER: OnceLock<Option<&'static Scheduler>> = OnceLock::new();

impl Scheduler {
    fn new_parked() -> Self {
        Self {
            epoch: Instant::now(),
            inner: Mutex::new(SchedulerInner {
                targets: Vec::new(),
                shutdown: false,
            }),
            cv: Condvar::new(),
            thread_starts: AtomicUsize::new(0),
            slow_path_arms: AtomicUsize::new(0),
            notifications: AtomicUsize::new(0),
            reconciliations: AtomicUsize::new(0),
        }
    }

    /// Leak one scheduler and start its monitor thread. Leaking is intentional:
    /// the process-wide monitor must outlive every isolate for the whole
    /// process, and the thread parks (no cost) when nothing is scheduled.
    fn spawn_leaked() -> EngineResult<&'static Scheduler> {
        let scheduler: &'static Scheduler = Box::leak(Box::new(Self::new_parked()));
        scheduler.thread_starts.fetch_add(1, Ordering::AcqRel);
        std::thread::Builder::new()
            .name("Migo-Watchdog".into())
            .spawn(move || scheduler.run())
            .map_err(|e| {
                EngineError::new(ErrorCode::Internal)
                    .with_msg("failed to spawn Migo-Watchdog thread")
                    .with_detail(e.to_string())
            })?;
        Ok(scheduler)
    }

    /// The process-wide scheduler, created lazily. A failed thread spawn is
    /// cached as `None`, matching the "log and continue without a watchdog"
    /// policy for restart/Worker creation while still surfacing an error to the
    /// caller.
    fn global() -> EngineResult<&'static Scheduler> {
        (*GLOBAL_SCHEDULER.get_or_init(|| Self::spawn_leaked().ok())).ok_or_else(|| {
            EngineError::new(ErrorCode::Internal).with_msg("process watchdog scheduler unavailable")
        })
    }

    #[cfg(test)]
    pub(crate) fn new_test() -> &'static Scheduler {
        Self::spawn_leaked().expect("spawn test watchdog thread")
    }

    #[inline]
    fn now_encoded(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Encode `now + timeout` as an absolute deadline, floored above the two
    /// sentinels (`DISARMED` / `TERMINATED`).
    #[inline]
    fn encode_deadline(&self, timeout: Duration) -> u64 {
        self.now_encoded()
            .saturating_add(timeout.as_millis() as u64)
            .max(2)
    }

    fn lock(&self) -> MutexGuard<'_, SchedulerInner> {
        // Poison recovery: a panic in one observer/test must not disable
        // protection for every other isolate.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn register(&self, target: &Arc<Target>, budget: Duration) {
        let mut inner = self.lock();
        inner.targets.push(Registration {
            target: Arc::downgrade(target),
            budget,
        });
    }

    /// Remove a target's registration under the control mutex. Called from
    /// `DeadlineWatchdog::drop` after the target has been disarmed.
    fn unregister(&self, target: &Arc<Target>) {
        let mut inner = self.lock();
        inner
            .targets
            .retain(|r| r.target.upgrade().is_some_and(|t| !Arc::ptr_eq(&t, target)));
    }

    /// Owner-thread arm slow path. The caller has already applied
    /// `arm_at(deadline)` to `target`.
    fn on_armed(&self, target: &Target, deadline: u64) {
        // Fast path: normal poll-to-poll re-arms use a fixed timeout plus a
        // monotonic clock, so their deadline is not earlier than the accounted
        // wake. Extension removal is the exception: it can shorten the deadline
        // and must wake a monitor sleeping on the old, longer value.
        if target.scheduled.load(Ordering::SeqCst) {
            let accounted = target.wake_deadline.load(Ordering::SeqCst);
            if deadline >= accounted {
                return;
            }

            // The earlier-deadline path is cold. Serialize with the monitor's
            // scan-to-wait interval, re-check ownership, then either update its
            // wake responsibility or re-take an ownership it just released.
            let _guard = self.lock();
            if target.scheduled.load(Ordering::SeqCst) {
                if deadline < target.wake_deadline.load(Ordering::SeqCst) {
                    target.wake_deadline.store(deadline, Ordering::SeqCst);
                    self.notifications.fetch_add(1, Ordering::AcqRel);
                    self.cv.notify_one();
                }
                return;
            }

            target.wake_deadline.store(deadline, Ordering::SeqCst);
            if target
                .scheduled
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                self.slow_path_arms.fetch_add(1, Ordering::AcqRel);
                self.notifications.fetch_add(1, Ordering::AcqRel);
                self.cv.notify_one();
            }
            return;
        }
        // Slow path: publish the accounted deadline, then win the
        // unscheduled -> scheduled transition and notify under the control
        // mutex. The monitor holds the same mutex from scan through condvar
        // entry, so this notify cannot be lost.
        target.wake_deadline.store(deadline, Ordering::SeqCst);
        if target
            .scheduled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.slow_path_arms.fetch_add(1, Ordering::AcqRel);
            let _guard = self.lock();
            self.notifications.fetch_add(1, Ordering::AcqRel);
            self.cv.notify_one();
        }
    }

    #[cfg(test)]
    pub(crate) fn registered_len(&self) -> usize {
        self.lock().targets.len()
    }

    /// Monitor loop. Holds the control mutex continuously from each scan through
    /// condvar entry, so an owner's first-arm notify can never be lost. Never
    /// invokes V8 or the observer while holding the mutex.
    fn run(&self) {
        let mut guard = self.lock();
        loop {
            if guard.shutdown {
                return;
            }
            let now = self.now_encoded();
            let mut earliest: Option<u64> = None;
            let mut fired: Vec<(Arc<Target>, Duration)> = Vec::new();

            guard.targets.retain(|reg| {
                let Some(target) = reg.target.upgrade() else {
                    return false; // prune dropped targets
                };
                if !target.scheduled.load(Ordering::SeqCst) {
                    return true;
                }
                // Only reconcile at (or past) the accounted wake deadline. On an
                // earlier wake — a notify from another target's first arm, or a
                // spurious condvar wake — a target whose wake has not arrived is
                // merely re-included in `earliest`. This is what lets rapid
                // arm/disarm cycles coalesce: `scheduled`/`wake_deadline` stay
                // set until the committed wake, so the hot path never re-locks.
                let wake = target.wake_deadline.load(Ordering::SeqCst);
                if now < wake {
                    earliest = Some(earliest.map_or(wake, |e| e.min(wake)));
                    return true;
                }
                self.reconciliations.fetch_add(1, Ordering::AcqRel);
                match target.reconcile(now) {
                    Reconcile::Inactive => {
                        // Drop our wake responsibility, then double-check for an
                        // arm that raced the clear. With SeqCst ordering, if the
                        // owner's arm read `scheduled == true` it must be ordered
                        // before this clear, hence its execution_state store is
                        // visible here and we re-take; otherwise the owner reads
                        // `false` and its own CAS+notify wins.
                        target.scheduled.store(false, Ordering::SeqCst);
                        let st = target.execution_state.load(Ordering::SeqCst);
                        if st != DISARMED && st != TERMINATED {
                            target.wake_deadline.store(st, Ordering::SeqCst);
                            target.scheduled.store(true, Ordering::SeqCst);
                            earliest = Some(earliest.map_or(st, |e| e.min(st)));
                        }
                    }
                    Reconcile::Reschedule(d) => {
                        target.wake_deadline.store(d, Ordering::SeqCst);
                        earliest = Some(earliest.map_or(d, |e| e.min(d)));
                    }
                    Reconcile::Fired => {
                        target.scheduled.store(false, Ordering::SeqCst);
                        fired.push((target, reg.budget));
                    }
                    Reconcile::Terminal => {
                        target.scheduled.store(false, Ordering::SeqCst);
                    }
                }
                true
            });

            if !fired.is_empty() {
                drop(guard);
                for (target, budget) in fired {
                    // Contain observer/termination panics so one bad callback
                    // cannot take down protection for every other isolate.
                    let _ = catch_unwind(AssertUnwindSafe(|| target.fire_once(budget)));
                }
                guard = self.lock();
                continue;
            }

            match earliest {
                None => {
                    guard = self.cv.wait(guard).unwrap_or_else(|e| e.into_inner());
                }
                Some(deadline) => {
                    let now2 = self.now_encoded();
                    if deadline <= now2 {
                        continue; // already due; rescan immediately
                    }
                    let dur = Duration::from_millis(deadline - now2);
                    guard = self
                        .cv
                        .wait_timeout(guard, dur)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Owner-thread RAII controller and public API
// ---------------------------------------------------------------------------

/// Once-only timeout observer, invoked with the configured timeout budget.
pub type TimeoutObserver = Arc<dyn Fn(Duration) + Send + Sync>;

/// Configuration for a per-isolate deadline watchdog.
#[derive(Clone)]
pub struct DeadlineWatchdogConfig {
    timeout: Duration,
    label: Arc<str>,
    observer: TimeoutObserver,
}

impl DeadlineWatchdogConfig {
    pub fn new(timeout: Duration, label: impl Into<Arc<str>>) -> Self {
        Self {
            timeout,
            label: label.into(),
            observer: Arc::new(|_| {}),
        }
    }

    pub fn with_observer(mut self, observer: TimeoutObserver) -> Self {
        self.observer = observer;
        self
    }
}

/// Isolate-owner-thread controller. Intentionally `!Send`/`!Sync`
/// (`PhantomData<Rc<()>>`): nesting/pause/extension counters use `Cell`/
/// `RefCell`, so only the owner thread may arm/disarm and the hot path needs no
/// per-isolate mutex. Only the scheduler target is `Send + Sync`.
pub struct DeadlineWatchdog {
    scheduler: &'static Scheduler,
    target: Arc<Target>,
    timeout: Duration,
    execution_depth: Cell<u32>,
    pause_depth: Cell<u32>,
    extensions: RefCell<Vec<(u64, Duration)>>,
    next_extension_id: Cell<u64>,
    _owner_thread: PhantomData<Rc<()>>,
}

impl DeadlineWatchdog {
    /// Register a V8 isolate with the process scheduler. Fails only if the one
    /// process monitor thread cannot be created.
    pub fn register_isolate(
        isolate: v8::IsolateHandle,
        config: DeadlineWatchdogConfig,
    ) -> EngineResult<Self> {
        let scheduler = Scheduler::global()?;
        Ok(Self::register_isolate_on(scheduler, isolate, config))
    }

    /// Register `isolate` with a specific scheduler. Crate-internal so tests can
    /// use an isolated scheduler for deterministic counters; production uses the
    /// process-global scheduler via [`Self::register_isolate`].
    pub(crate) fn register_isolate_on(
        scheduler: &'static Scheduler,
        isolate: v8::IsolateHandle,
        config: DeadlineWatchdogConfig,
    ) -> Self {
        tracing::debug!(
            label = %config.label,
            timeout_ms = config.timeout.as_millis() as u64,
            "installing process deadline watchdog"
        );
        let terminate: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            isolate.terminate_execution();
        });
        Self::register_on(scheduler, config.timeout, terminate, config.observer)
    }

    fn register_on(
        scheduler: &'static Scheduler,
        timeout: Duration,
        terminate: Arc<dyn Fn() + Send + Sync>,
        observer: TimeoutObserver,
    ) -> Self {
        let target = Arc::new(Target::new(terminate, observer));
        scheduler.register(&target, timeout);
        Self {
            scheduler,
            target,
            timeout,
            execution_depth: Cell::new(0),
            pause_depth: Cell::new(0),
            extensions: RefCell::new(Vec::new()),
            next_extension_id: Cell::new(0),
            _owner_thread: PhantomData,
        }
    }

    #[cfg(test)]
    fn new_test(
        scheduler: &'static Scheduler,
        timeout: Duration,
        terminate: Arc<dyn Fn() + Send + Sync>,
        observer: TimeoutObserver,
    ) -> Self {
        Self::register_on(scheduler, timeout, terminate, observer)
    }

    /// The effective budget: the configured timeout, widened by the largest
    /// live extension.
    fn effective_timeout(&self) -> Duration {
        let mut t = self.timeout;
        for (_, ext) in self.extensions.borrow().iter() {
            if *ext > t {
                t = *ext;
            }
        }
        t
    }

    #[inline]
    fn should_be_armed(&self) -> bool {
        self.execution_depth.get() > 0 && self.pause_depth.get() == 0
    }

    /// (Re)establish the live deadline from the current monotonic time plus the
    /// effective budget.
    fn arm_now(&self) {
        let deadline = self.scheduler.encode_deadline(self.effective_timeout());
        if self.target.arm_at(deadline) {
            self.scheduler.on_armed(&self.target, deadline);
        }
    }

    /// Begin a guarded V8 execution section. Nesting only bumps the owner-thread
    /// depth; the outermost enter arms.
    pub fn enter(&self) -> ExecutionScope<'_> {
        let prev = self.execution_depth.get();
        self.execution_depth.set(prev + 1);
        if prev == 0 && self.pause_depth.get() == 0 {
            self.arm_now();
        }
        ExecutionScope { watchdog: self }
    }

    /// Suspend the deadline for an explicitly trusted synchronous section.
    pub fn pause(&self) -> PauseScope<'_> {
        let prev = self.pause_depth.get();
        self.pause_depth.set(prev + 1);
        if prev == 0 && self.execution_depth.get() > 0 {
            self.target.disarm();
        }
        PauseScope { watchdog: self }
    }

    /// Widen the deadline for a known long-running trusted task.
    pub fn extend(&self, timeout_from_now: Duration) -> TimeoutExtension<'_> {
        let id = self.next_extension_id.get();
        self.next_extension_id.set(id + 1);
        self.extensions.borrow_mut().push((id, timeout_from_now));
        if self.should_be_armed() {
            self.arm_now();
        }
        TimeoutExtension { watchdog: self, id }
    }

    /// Whether this isolate's deadline fired (sticky).
    pub fn timed_out(&self) -> bool {
        self.target.execution_state.load(Ordering::Acquire) == TERMINATED
    }
}

impl Drop for DeadlineWatchdog {
    fn drop(&mut self) {
        // Disarm first so a monitor reconcile in flight observes a
        // non-fireable target, then remove the registration under the control
        // mutex (ownership order: unregister the watchdog before the runtime is
        // dropped by the owner).
        self.target.disarm();
        self.scheduler.unregister(&self.target);
    }
}

/// RAII guard for an armed execution section. Disarms on the outermost drop.
pub struct ExecutionScope<'a> {
    watchdog: &'a DeadlineWatchdog,
}

impl Drop for ExecutionScope<'_> {
    fn drop(&mut self) {
        let prev = self.watchdog.execution_depth.get();
        self.watchdog.execution_depth.set(prev - 1);
        if prev == 1 && self.watchdog.pause_depth.get() == 0 {
            self.watchdog.target.disarm();
        }
    }
}

/// RAII guard suspending the deadline. Re-arms on the final drop if execution
/// is still active.
pub struct PauseScope<'a> {
    watchdog: &'a DeadlineWatchdog,
}

impl Drop for PauseScope<'_> {
    fn drop(&mut self) {
        let prev = self.watchdog.pause_depth.get();
        self.watchdog.pause_depth.set(prev - 1);
        if prev == 1 && self.watchdog.execution_depth.get() > 0 {
            self.watchdog.arm_now();
        }
    }
}

/// RAII guard for a temporary deadline extension. Re-arms from the current time
/// with the reduced budget on drop.
pub struct TimeoutExtension<'a> {
    watchdog: &'a DeadlineWatchdog,
    id: u64,
}

impl Drop for TimeoutExtension<'_> {
    fn drop(&mut self) {
        self.watchdog
            .extensions
            .borrow_mut()
            .retain(|(id, _)| *id != self.id);
        if self.watchdog.should_be_armed() {
            self.watchdog.arm_now();
        }
    }
}

/// Guarded-poll helper: arm immediately before `Future::poll` and disarm
/// immediately after it returns `Pending` or `Ready`. Never holds an
/// [`ExecutionScope`] across `.await`.
pub fn poll_guarded<F: Future>(
    watchdog: Option<&DeadlineWatchdog>,
    future: Pin<&mut F>,
    cx: &mut Context<'_>,
) -> Poll<F::Output> {
    let _scope = watchdog.map(DeadlineWatchdog::enter);
    future.poll(cx)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn test_target(fired: Arc<AtomicUsize>) -> Target {
        Target::new(
            Arc::new(move || {
                fired.fetch_add(1, Ordering::AcqRel);
            }),
            Arc::new(|_| {}),
        )
    }

    #[test]
    fn disarm_wins_before_expiry_cas() {
        let fired = Arc::new(AtomicUsize::new(0));
        let target = test_target(Arc::clone(&fired));
        assert!(target.arm_at(100));
        assert!(target.disarm());
        assert_eq!(target.reconcile(100), Reconcile::Inactive);
        assert_eq!(fired.load(Ordering::Acquire), 0);
    }

    #[test]
    fn expiry_cas_is_sticky_and_fires_once() {
        let fired = Arc::new(AtomicUsize::new(0));
        let target = test_target(Arc::clone(&fired));
        assert!(target.arm_at(100));
        assert_eq!(target.reconcile(100), Reconcile::Fired);
        target.fire_once(Duration::from_millis(100));
        assert_eq!(target.reconcile(101), Reconcile::Terminal);
        target.fire_once(Duration::from_millis(101));
        assert_eq!(fired.load(Ordering::Acquire), 1);
    }

    #[test]
    fn stale_accounted_deadline_observes_newer_rearm() {
        let target = test_target(Arc::new(AtomicUsize::new(0)));
        assert!(target.arm_at(100));
        assert!(target.disarm());
        assert!(target.arm_at(200));
        assert_eq!(target.reconcile(100), Reconcile::Reschedule(200));
    }

    #[test]
    fn terminal_state_cannot_be_disarmed_or_rearmed() {
        let target = test_target(Arc::new(AtomicUsize::new(0)));
        assert!(target.arm_at(100));
        assert_eq!(target.reconcile(100), Reconcile::Fired);
        assert!(!target.disarm());
        assert!(!target.arm_at(200));
        assert_eq!(target.execution_state.load(Ordering::Acquire), TERMINATED);
    }

    #[test]
    fn inactive_reconcile_and_concurrent_rearm_keep_one_owner() {
        let target = test_target(Arc::new(AtomicUsize::new(0)));
        assert!(target.arm_at(100));
        assert!(target.disarm());
        assert_eq!(target.reconcile(100), Reconcile::Inactive);
        assert!(target.arm_at(200));
        assert_eq!(target.reconcile(100), Reconcile::Reschedule(200));
    }

    #[test]
    fn inactive_handoff_uses_one_sequentially_consistent_order() {
        let source = include_str!("watchdog.rs");
        let arm_at = source
            .split("fn arm_at(&self, deadline: u64) -> bool")
            .nth(1)
            .and_then(|rest| rest.split("/// Clear a live deadline").next())
            .expect("arm_at implementation must precede disarm");

        assert!(
            arm_at.contains("Ordering::SeqCst,\n                Ordering::Acquire,"),
            "successful deadline publication must share the SeqCst order used by \
             the scheduled handoff; failure loads need only Acquire"
        );
    }

    #[test]
    fn termination_panic_does_not_skip_timeout_observer() {
        let observed = Arc::new(AtomicUsize::new(0));
        let observed_for_callback = Arc::clone(&observed);
        let target = Target::new(
            Arc::new(|| panic!("injected termination panic")),
            Arc::new(move |_| {
                observed_for_callback.fetch_add(1, Ordering::AcqRel);
            }),
        );

        let result = catch_unwind(AssertUnwindSafe(|| {
            target.fire_once(Duration::from_millis(100));
        }));

        assert!(result.is_ok(), "fire_once must contain callback panics");
        assert_eq!(
            observed.load(Ordering::Acquire),
            1,
            "the timeout observer must still run after termination panics"
        );
    }

    // R4 Task 8: the disarm-vs-expiry linearization must hold under real thread
    // contention, not just sequentially.
    #[test]
    fn concurrent_disarm_and_reconcile_resolve_at_most_once() {
        use std::sync::Barrier;
        for _ in 0..300 {
            let fired = Arc::new(AtomicUsize::new(0));
            let target = Arc::new(test_target(Arc::clone(&fired)));
            assert!(target.arm_at(100));

            let barrier = Arc::new(Barrier::new(2));
            let disarmer = {
                let t = Arc::clone(&target);
                let b = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    b.wait();
                    t.disarm();
                })
            };

            barrier.wait();
            let outcome = target.reconcile(100); // now == deadline -> expired
            if outcome == Reconcile::Fired {
                target.fire_once(Duration::from_millis(100));
            }
            disarmer.join().unwrap();

            // Never a torn or still-live deadline after the race.
            let state = target.execution_state.load(Ordering::Acquire);
            assert!(
                state == DISARMED || state == TERMINATED,
                "state must be a sentinel, got {state}"
            );
            // The action fired iff (and exactly once when) reconcile won the CAS.
            let f = fired.load(Ordering::Acquire);
            assert!(f <= 1, "fire_once must be idempotent");
            assert_eq!(
                f == 1,
                outcome == Reconcile::Fired && state == TERMINATED,
                "fires exactly when reconcile won the expiry CAS"
            );
        }
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Poll `f` until it returns true or `timeout` elapses.
    fn wait_until<F: Fn() -> bool>(f: F, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            if f() {
                return true;
            }
            if start.elapsed() >= timeout {
                return f();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn counting_wd(
        scheduler: &'static Scheduler,
        timeout: Duration,
    ) -> (DeadlineWatchdog, Arc<AtomicUsize>) {
        let fired = Arc::new(AtomicUsize::new(0));
        let f = Arc::clone(&fired);
        let wd = DeadlineWatchdog::new_test(
            scheduler,
            timeout,
            Arc::new(move || {
                f.fetch_add(1, Ordering::AcqRel);
            }),
            Arc::new(|_| {}),
        );
        (wd, fired)
    }

    #[test]
    fn first_arm_wakes_an_indefinitely_parked_scheduler() {
        let sched = Scheduler::new_test();
        let (wd, fired) = counting_wd(sched, Duration::from_millis(40));
        let _scope = wd.enter();
        assert!(
            wait_until(
                || fired.load(Ordering::Acquire) == 1,
                Duration::from_secs(3)
            ),
            "a first arm must wake the parked monitor and fire"
        );
    }

    #[test]
    fn normal_rearm_while_scheduled_takes_no_control_lock_or_notify() {
        let sched = Scheduler::new_test();
        // Long timeout so the monitor parks far in the future during the loop.
        let (wd, _fired) = counting_wd(sched, Duration::from_secs(3600));
        for _ in 0..100_000 {
            let _scope = wd.enter();
        }
        assert_eq!(
            sched.slow_path_arms.load(Ordering::Acquire),
            1,
            "only the first arm should take the control-lock slow path"
        );
        assert_eq!(
            sched.notifications.load(Ordering::Acquire),
            1,
            "only the first arm should notify the monitor"
        );
    }

    #[test]
    fn earlier_rearm_updates_accounted_wake_and_notifies() {
        let sched = Scheduler::new_parked();
        let target = Target::new(Arc::new(|| {}), Arc::new(|_| {}));
        target.scheduled.store(true, Ordering::SeqCst);
        target.wake_deadline.store(1_000, Ordering::SeqCst);

        assert!(target.arm_at(100));
        sched.on_armed(&target, 100);

        assert_eq!(
            target.wake_deadline.load(Ordering::SeqCst),
            100,
            "dropping an extension must move the accounted wake earlier"
        );
        assert_eq!(
            sched.notifications.load(Ordering::Acquire),
            1,
            "an earlier accounted wake must notify a monitor sleeping on the old deadline"
        );
        assert_eq!(
            sched.slow_path_arms.load(Ordering::Acquire),
            0,
            "an already-scheduled target must not be counted as a first-arm slow path"
        );
    }

    #[test]
    fn last_disarm_causes_at_most_one_tail_wake_then_indefinite_park() {
        let sched = Scheduler::new_test();
        let (wd, fired) = counting_wd(sched, Duration::from_millis(40));
        {
            let _scope = wd.enter();
        } // outer drop -> disarm; scheduled stays true until the tail wake
        // After the tail wake the monitor clears scheduling and parks forever.
        assert!(
            wait_until(
                || !wd.target.scheduled.load(Ordering::Acquire),
                Duration::from_secs(3)
            ),
            "the tail wake must clear scheduling"
        );
        let r1 = sched.reconciliations.load(Ordering::Acquire);
        std::thread::sleep(Duration::from_millis(200));
        let r2 = sched.reconciliations.load(Ordering::Acquire);
        assert_eq!(r1, r2, "an idle target must not cause periodic wakeups");
        assert_eq!(
            fired.load(Ordering::Acquire),
            0,
            "a disarmed target must never fire"
        );
    }

    #[test]
    fn nested_execution_scope_disarms_only_on_outer_drop() {
        let sched = Scheduler::new_test();
        let (wd, _fired) = counting_wd(sched, Duration::from_secs(3600));
        let outer = wd.enter();
        assert!(wd.target.execution_state.load(Ordering::Acquire) >= 2);
        {
            let _inner = wd.enter();
            assert!(wd.target.execution_state.load(Ordering::Acquire) >= 2);
        }
        assert!(
            wd.target.execution_state.load(Ordering::Acquire) >= 2,
            "still armed after inner scope drop"
        );
        drop(outer);
        assert_eq!(
            wd.target.execution_state.load(Ordering::Acquire),
            DISARMED,
            "disarmed only when the outer scope drops"
        );
    }

    #[test]
    fn nested_pause_rearms_only_on_final_resume() {
        let sched = Scheduler::new_test();
        let (wd, _fired) = counting_wd(sched, Duration::from_secs(3600));
        let _exec = wd.enter();
        assert!(wd.target.execution_state.load(Ordering::Acquire) >= 2);
        let outer_pause = wd.pause();
        assert_eq!(
            wd.target.execution_state.load(Ordering::Acquire),
            DISARMED,
            "first pause disarms"
        );
        {
            let _inner_pause = wd.pause();
            assert_eq!(wd.target.execution_state.load(Ordering::Acquire), DISARMED);
        }
        assert_eq!(
            wd.target.execution_state.load(Ordering::Acquire),
            DISARMED,
            "still paused after inner resume"
        );
        drop(outer_pause);
        assert!(
            wd.target.execution_state.load(Ordering::Acquire) >= 2,
            "re-armed only on the final resume, while execution is active"
        );
    }

    #[test]
    fn multiple_extensions_use_maximum_live_budget() {
        let sched = Scheduler::new_test();
        let (wd, _fired) = counting_wd(sched, Duration::from_millis(100));
        let _exec = wd.enter();
        let base = wd.target.execution_state.load(Ordering::Acquire);
        let ext_a = wd.extend(Duration::from_secs(10));
        let after_a = wd.target.execution_state.load(Ordering::Acquire);
        assert!(
            after_a > base + 5_000,
            "a large extension moves the deadline out"
        );
        let ext_b = wd.extend(Duration::from_secs(5));
        let after_b = wd.target.execution_state.load(Ordering::Acquire);
        assert!(
            after_b >= after_a,
            "a smaller extension keeps the maximum live budget"
        );
        drop(ext_a);
        let after_drop_a = wd.target.execution_state.load(Ordering::Acquire);
        assert!(
            after_drop_a < after_b,
            "dropping the largest extension shrinks the budget to the next max"
        );
        drop(ext_b);
        let after_drop_b = wd.target.execution_state.load(Ordering::Acquire);
        assert!(
            after_drop_b < after_drop_a,
            "dropping all extensions returns to the configured budget"
        );
    }

    #[test]
    fn drop_unregisters_before_target_action() {
        let sched = Scheduler::new_test();
        let (wd, fired) = counting_wd(sched, Duration::from_millis(40));
        let scope = wd.enter();
        assert_eq!(sched.registered_len(), 1);
        drop(scope);
        drop(wd);
        assert_eq!(
            sched.registered_len(),
            0,
            "drop must unregister the target under the control mutex"
        );
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            fired.load(Ordering::Acquire),
            0,
            "an unregistered + disarmed target must not fire"
        );
    }

    #[test]
    fn observer_panic_does_not_kill_scheduler() {
        let sched = Scheduler::new_test();
        // Target A: its observer panics when it fires.
        let wd_panic = DeadlineWatchdog::new_test(
            sched,
            Duration::from_millis(30),
            Arc::new(|| {}),
            Arc::new(|_| panic!("observer boom")),
        );
        let _panic_scope = wd_panic.enter();
        // Give the panicking target time to fire and unwind inside the monitor.
        assert!(wait_until(|| wd_panic.timed_out(), Duration::from_secs(3)));
        // Target B registered afterwards must still be protected.
        let (wd_ok, fired_ok) = counting_wd(sched, Duration::from_millis(30));
        let _ok_scope = wd_ok.enter();
        assert!(
            wait_until(
                || fired_ok.load(Ordering::Acquire) == 1,
                Duration::from_secs(3)
            ),
            "the monitor must survive an observer panic and keep protecting other isolates"
        );
        assert_eq!(
            sched.thread_starts.load(Ordering::Acquire),
            1,
            "no monitor thread respawn"
        );
    }

    #[test]
    fn many_targets_share_one_scheduler_thread() {
        let sched = Scheduler::new_test();
        let mut wds = Vec::new();
        let mut fireds = Vec::new();
        for _ in 0..8 {
            let (wd, f) = counting_wd(sched, Duration::from_millis(40));
            wds.push(wd);
            fireds.push(f);
        }
        let _scopes: Vec<_> = wds.iter().map(|w| w.enter()).collect();
        for f in &fireds {
            assert!(
                wait_until(|| f.load(Ordering::Acquire) == 1, Duration::from_secs(3)),
                "every target must fire"
            );
        }
        assert_eq!(
            sched.thread_starts.load(Ordering::Acquire),
            1,
            "N targets must share exactly one monitor thread"
        );
    }

    // R4 Task 8: many owner threads each register + arm their own (!Send)
    // watchdog on the shared monitor; every one must fire exactly once with no
    // lost wakeup, no double-fire, and still a single monitor thread.
    #[test]
    fn concurrent_targets_each_fire_exactly_once() {
        let sched = Scheduler::new_test();
        let fireds: Vec<Arc<AtomicUsize>> =
            (0..16).map(|_| Arc::new(AtomicUsize::new(0))).collect();
        let handles: Vec<_> = fireds
            .iter()
            .map(|f| {
                let f = Arc::clone(f);
                std::thread::spawn(move || {
                    // The watchdog is !Send: build it on its owning thread.
                    let ff = Arc::clone(&f);
                    let wd = DeadlineWatchdog::new_test(
                        sched,
                        Duration::from_millis(40),
                        Arc::new(move || {
                            ff.fetch_add(1, Ordering::AcqRel);
                        }),
                        Arc::new(|_| {}),
                    );
                    let _scope = wd.enter();
                    // Hold the scope past the deadline so it fires, then drop
                    // (disarm + unregister) — racing the monitor's reconcile.
                    std::thread::sleep(Duration::from_millis(300));
                })
            })
            .collect();

        for f in &fireds {
            assert!(
                wait_until(|| f.load(Ordering::Acquire) >= 1, Duration::from_secs(5)),
                "every concurrent target must fire (no lost wakeup)"
            );
        }
        for h in handles {
            h.join().unwrap();
        }
        for f in &fireds {
            assert_eq!(
                f.load(Ordering::Acquire),
                1,
                "each target fires exactly once (no double-fire)"
            );
        }
        assert_eq!(
            sched.thread_starts.load(Ordering::Acquire),
            1,
            "one monitor thread total"
        );
    }

    // R4 Task 8: a fired (terminal) target must never be cleared by a pause,
    // extension, or re-enter — nor re-fire.
    #[test]
    fn terminal_state_is_not_cleared_by_pause_extension_or_reenter() {
        let sched = Scheduler::new_test();
        let (wd, fired) = counting_wd(sched, Duration::from_millis(30));
        {
            let _scope = wd.enter();
            assert!(
                wait_until(|| wd.timed_out(), Duration::from_secs(3)),
                "target must fire"
            );
        }
        assert!(wd.timed_out());

        {
            let _pause = wd.pause();
            assert!(wd.timed_out(), "pause must not clear terminal");
        }
        assert!(
            wd.timed_out(),
            "pause drop must not re-arm a terminal target"
        );

        {
            let _ext = wd.extend(Duration::from_secs(10));
            assert!(wd.timed_out(), "extension must not clear terminal");
        }
        assert!(
            wd.timed_out(),
            "extension drop must not re-arm a terminal target"
        );

        {
            let _scope = wd.enter();
            assert!(wd.timed_out(), "re-enter must not clear terminal");
        }
        assert!(wd.timed_out());

        // Give any (incorrect) re-arm time to fire before asserting the count.
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            fired.load(Ordering::Acquire),
            1,
            "a terminal target fires exactly once and stays terminal"
        );
    }
}
