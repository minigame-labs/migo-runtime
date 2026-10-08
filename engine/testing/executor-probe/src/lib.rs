//! Shared-executor occupancy gate.
//!
//! The design names "the single worker serving all audio streaming" as one of the
//! shared budgets that let one game degrade another, and the gate requires the
//! structural performance properties to be "enforced by tests, not by inspection".
//! Neither of the two gates that already exist can see this one: a CPU-bound step
//! running on an executor other sessions share is not a lock, so
//! `contention-probe` observes nothing, and it is not an allocation, so
//! `alloc-probe` observes nothing either.
//!
//! **What makes this hard to observe, and the shape that answers it.** The claim is
//! about *which executor a call runs on*, and the obvious observation — time the
//! co-tenant — is worthless here, because how long a decode occupies a worker
//! depends on the data it was handed. Worse, the defect's signature is a
//! **deadlock** rather than a wrong value: on a single-worker runtime an inline CPU
//! step means a task spawned afterwards is never polled at all, so a naive test
//! hangs the suite instead of reporting.
//!
//! So the bound lives *inside* the step. The step announces that it has entered,
//! then blocks its own thread until a co-tenant task — spawned onto the same
//! executor only after that announcement — sends it a release. Occupancy is
//! manufactured rather than waited for, so a step that finishes quickly is not
//! thereby excused: it must actually be somewhere the executor can proceed without
//! it. Four details are load-bearing:
//!
//! * **The step reports, not the flag reader.** Under the defect the co-tenant runs
//!   the instant the step stops occupying the worker, so a flag read after the
//!   future completes is a coin flip. The release the step *received* is the
//!   evidence, recorded before the step returns.
//! * **The bound is the step's own.** When no release arrives the step gives up, the
//!   future completes, and the failure is reported and attributed. Nothing hangs.
//! * **Every other worker is occupied first.** A gate that assumed one worker would
//!   pass an inline step on a two-worker runtime, which is the same defect with more
//!   room. The fillers hold every worker but one, so what the gate observes is
//!   whether the step under test frees the last one.
//! * **A body that panics is re-raised as its own failure**, never reported as
//!   occupancy, because fidelity assertions belong in the body.
//!
//! The waiting bound cannot produce a false pass: shortening it can only make a
//! correct step look occupying, which fails closed. Lengthening it lets nothing
//! through, because the co-tenant is spawned before the wait begins and needs only
//! one poll.

use std::{
    future::Future,
    panic,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    time::Duration,
};

use parking_lot::Mutex;
use tokio::runtime::Runtime;

/// How long to wait before concluding that a co-tenant task cannot run.
///
/// Six orders of magnitude above what an unoccupied executor needs, because the
/// cost of being wrong in the other direction is a flaky gate.
pub const PATIENCE: Duration = Duration::from_secs(2);

/// Only one gate runs at a time in a test binary.
///
/// Two gates sharing one executor would each starve the other's co-tenant and each
/// blame *its own* step. Serialising inside the mechanism rather than at the call
/// sites is what makes every failure attributable without a caller having to
/// remember.
static ONE_GATE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// One CPU-bound step, and the executor other sessions share with it.
pub struct SharedExecutor<'a> {
    /// The step under test, quoted verbatim in the failure.
    pub step: &'a str,
    /// The executor it must not occupy, quoted verbatim in the failure so a report
    /// says which shared thing one session's work was sitting on.
    pub executor: &'a str,
    /// How long to wait before calling the executor occupied. [`PATIENCE`] unless a
    /// test is deliberately provoking the failure and does not want to wait for it.
    pub patience: Duration,
}

/// The occupancy handed to the body of the step under test.
///
/// Standing in for CPU-bound work, it holds the thread it runs on until a co-tenant
/// task releases it — which can only happen if that thread was not one the executor
/// needed.
pub struct CpuStep {
    entered: Sender<()>,
    release: Receiver<()>,
    co_tenant_ran: Arc<AtomicBool>,
    patience: Duration,
}

impl CpuStep {
    /// Hold this thread the way the real CPU-bound work would.
    ///
    /// Call it from inside the step under test, wherever the real work would be.
    pub fn occupy(self) {
        // Recorded here, by the step itself, because after this returns the executor
        // is free and the co-tenant runs whether the gate passes or fails.
        if hold_this_thread(&self.entered, &self.release, self.patience) {
            self.co_tenant_ran.store(true, Ordering::Release);
        }
    }
}

/// Announce arrival, then block this thread until released or out of patience.
fn hold_this_thread(entered: &Sender<()>, release: &Receiver<()>, patience: Duration) -> bool {
    let _ = entered.send(());
    release.recv_timeout(patience).is_ok()
}

/// Announce arrival, then block this thread until the gate lets go.
///
/// Unbounded on purpose, and the bound it does *not* take is load-bearing. A filler
/// that gave up on the same deadline as the step under test would free a worker at
/// the very moment the step was still waiting, and the co-tenant would run — so an
/// occupying step on a multi-worker executor would pass. The gate releases these by
/// dropping their senders, which happens on the way out whether it passed or failed.
fn hold_until_released(entered: &Sender<()>, release: &Receiver<()>) {
    let _ = entered.send(());
    let _ = release.recv();
}

/// Run `under_test` on `runtime` with every other worker occupied, and fail unless a
/// co-tenant task spawned afterwards runs while the step is still in flight.
///
/// Returns the future's value, so the caller can assert that the work it asked for
/// actually happened — this helper cannot know what the step was supposed to
/// produce, and a body that does nothing but occupy would otherwise pass.
#[track_caller]
pub fn assert_leaves_the_executor_free<T, Fut>(
    what: SharedExecutor<'_>,
    runtime: &Runtime,
    under_test: impl FnOnce(CpuStep) -> Fut,
) -> T
where
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let _sole_gate = ONE_GATE_AT_A_TIME.lock();
    // Held to the end of this function, including an unwind: dropping the senders is
    // what lets the fillers go.
    let _filler_releases = occupy_every_worker_but_one(&what, runtime);

    let co_tenant_ran = Arc::new(AtomicBool::new(false));
    let (entered, arrival) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let task = runtime.spawn(under_test(CpuStep {
        entered,
        release: released,
        co_tenant_ran: Arc::clone(&co_tenant_ran),
        patience: what.patience,
    }));

    match arrival.recv_timeout(what.patience) {
        Ok(()) => {}
        // The body dropped the step or panicked before reaching it. Either way the
        // gate observed nothing, which must not read as a pass.
        Err(RecvTimeoutError::Disconnected) => match runtime.block_on(task) {
            Err(join) if join.is_panic() => panic::resume_unwind(join.into_panic()),
            _ => panic!(
                "{step}: the body finished without occupying anything, so this gate \
                 observed nothing. Call `CpuStep::occupy` where the CPU-bound work is.",
                step = what.step,
            ),
        },
        Err(RecvTimeoutError::Timeout) => panic!(
            "{step}: did not reach its CPU-bound work within {patience:?}, so this gate \
             observed nothing. Something else is holding {executor}.",
            step = what.step,
            patience = what.patience,
            executor = what.executor,
        ),
    }

    // Spawned only now, so it is queued against a step that is already in flight.
    runtime.spawn(async move {
        // Yield first: a download task has to be *resumed* rather than merely
        // started, so require the executor to poll this twice.
        tokio::task::yield_now().await;
        let _ = release.send(());
    });

    let outcome = match runtime.block_on(task) {
        Ok(outcome) => outcome,
        // The body's own assertion, not occupancy. Misattributing it would hide it.
        Err(join) if join.is_panic() => panic::resume_unwind(join.into_panic()),
        Err(join) => panic!("{step}: {join}", step = what.step),
    };

    assert!(
        co_tenant_ran.load(Ordering::Acquire),
        "{step}: a co-tenant task spawned onto {executor} did not run within \
         {patience:?} while the step was in flight, so the step occupies {executor} \
         and one session's work stalls every other session's. Section 6.4 defect 4 \
         requires the work to leave the shared executor.",
        step = what.step,
        executor = what.executor,
        patience = what.patience,
    );

    outcome
}

/// Fill every worker but one, so the gate observes the last one rather than
/// assuming there was only ever one.
#[track_caller]
fn occupy_every_worker_but_one(what: &SharedExecutor<'_>, runtime: &Runtime) -> Vec<Sender<()>> {
    let others = runtime.metrics().num_workers() - 1;
    let mut releases = Vec::with_capacity(others);
    let mut arrivals = Vec::with_capacity(others);

    for _ in 0..others {
        let (entered, arrival) = mpsc::channel();
        let (release, released) = mpsc::channel();
        // Inline on purpose: a filler is meant to hold the worker that polls it.
        runtime.spawn(async move {
            hold_until_released(&entered, &released);
        });
        releases.push(release);
        arrivals.push(arrival);
    }

    for arrival in &arrivals {
        assert!(
            arrival.recv_timeout(what.patience).is_ok(),
            "{step}: could not occupy all {workers} workers of {executor} within \
             {patience:?}, so the last free one is not this gate's to observe.",
            step = what.step,
            workers = others + 1,
            executor = what.executor,
            patience = what.patience,
        );
    }

    releases
}

#[cfg(test)]
mod tests {
    use super::{PATIENCE, SharedExecutor, assert_leaves_the_executor_free};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };
    use tokio::runtime::{Builder, Runtime};

    // Short, because these tests provoke the failure on purpose and the wait is the
    // whole cost of it.
    const IMPATIENT: Duration = Duration::from_millis(50);

    fn probe(patience: Duration) -> SharedExecutor<'static> {
        SharedExecutor {
            step: "the step under test",
            executor: "the shared executor",
            patience,
        }
    }

    fn executor(workers: usize) -> Runtime {
        Builder::new_multi_thread()
            .worker_threads(workers)
            .build()
            .expect("a test executor")
    }

    #[test]
    fn a_step_that_leaves_the_worker_returns_its_value() {
        let doubled =
            assert_leaves_the_executor_free(probe(PATIENCE), &executor(1), |step| async {
                tokio::task::spawn_blocking(move || {
                    step.occupy();
                    21 * 2
                })
                .await
                .expect("the blocking step")
            });

        assert_eq!(doubled, 42);
    }

    #[test]
    #[should_panic(expected = "occupies the shared executor")]
    fn a_step_that_runs_inline_on_the_worker_is_reported() {
        // The defect's exact shape, and the reason the step reports rather than a
        // flag read afterwards: this co-tenant *does* run, moments after the step
        // gives up and frees the worker.
        assert_leaves_the_executor_free(probe(IMPATIENT), &executor(1), |step| async {
            step.occupy();
        });
    }

    #[test]
    #[should_panic(expected = "occupies the shared executor")]
    fn a_step_that_runs_inline_on_a_multi_worker_executor_is_reported_too() {
        // The case a gate built for one worker cannot see: the same inline step with
        // more room. Nothing here would fail if the fillers were dropped.
        assert_leaves_the_executor_free(probe(IMPATIENT), &executor(4), |step| async {
            step.occupy();
        });
    }

    #[test]
    fn a_step_that_leaves_a_multi_worker_executor_free_still_passes() {
        // The other side of the fillers: they must not be mistaken for the defect.
        let value = assert_leaves_the_executor_free(probe(PATIENCE), &executor(4), |step| async {
            tokio::task::spawn_blocking(move || {
                step.occupy();
                7
            })
            .await
            .expect("the blocking step")
        });

        assert_eq!(value, 7);
    }

    #[test]
    #[should_panic(expected = "finished without occupying anything")]
    fn a_body_that_never_occupies_cannot_pass() {
        // Without this refusal, deleting the `occupy` call from a gate would turn it
        // into a permanent silent pass.
        assert_leaves_the_executor_free(probe(IMPATIENT), &executor(1), |step| async move {
            drop(step);
        });
    }

    #[test]
    #[should_panic(expected = "the step's own assertion")]
    fn a_panicking_body_reports_its_own_failure_rather_than_occupancy() {
        assert_leaves_the_executor_free(probe(PATIENCE), &executor(1), |_step| async {
            panic!("the step's own assertion")
        });
    }

    #[test]
    fn the_failure_names_the_step_and_the_executor_it_sat_on() {
        let runtime = executor(1);
        let payload = std::panic::catch_unwind(|| {
            assert_leaves_the_executor_free(probe(IMPATIENT), &runtime, |step| async {
                step.occupy();
            });
        })
        .expect_err("an occupying step must fail");
        let message = payload
            .downcast_ref::<String>()
            .expect("assertion messages are formatted");

        assert!(
            message.contains("the step under test"),
            "the step is missing from: {message}"
        );
        assert!(
            message.contains("the shared executor"),
            "the executor is missing from: {message}"
        );
    }

    #[test]
    fn two_gates_never_share_an_executor_and_so_cannot_blame_each_other() {
        // Run in turn, each gate finds a worker for its own co-tenant and both pass.
        // Run at once, one gate's filler and step occupy the only worker the other
        // gate's co-tenant could have used, and that gate reports a defect that is
        // not its own.
        let shared = Arc::new(executor(1));
        let passes = Arc::new(AtomicUsize::new(0));

        let gates: Vec<_> = (0..2)
            .map(|_| {
                let runtime = Arc::clone(&shared);
                let passes = Arc::clone(&passes);
                thread::spawn(move || {
                    assert_leaves_the_executor_free(probe(IMPATIENT), &runtime, |step| async {
                        tokio::task::spawn_blocking(move || step.occupy())
                            .await
                            .expect("the blocking step")
                    });
                    passes.fetch_add(1, Ordering::Relaxed);
                })
            })
            .collect();

        for gate in gates {
            gate.join().expect("both gates must pass");
        }
        assert_eq!(passes.load(Ordering::Relaxed), 2);
    }
}
