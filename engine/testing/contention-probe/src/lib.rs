//! Cross-session lock gate.
//!
//! The gate requires, for every per-event path, "a contention regression test
//! that fails when a per-event operation acquires a lock shared beyond its own
//! session". Reading the code and observing that it resolves its handles at
//! bring-up is not that test — these are "enforced by tests, not by
//! inspection", and the first attempt at one was withdrawn because it took the shared
//! lock inside the very helper it called, so it passed with and without the property.
//!
//! **How this observes an acquisition rather than reasoning about one.** Hold the
//! shared lock in *write* mode, then require the per-event operation to complete
//! anyway. A path that resolved its handles at bring-up never touches the lock and
//! finishes in microseconds. A path that looks its session up per event blocks until
//! the guard is released, which it is not. Contention is manufactured rather than
//! waited for, so an *uncontended* acquisition — the thing a load test would miss —
//! is caught too.
//!
//! Three details are load-bearing:
//!
//! * **A write guard, not a read guard.** An `RwLock` admits concurrent readers, so a
//!   held read guard would let a per-event `read()` straight through.
//! * **The operation runs on another thread.** On the guard holder's own thread a
//!   `parking_lot` re-entrant acquisition deadlocks rather than fails, which would
//!   hang the suite instead of reporting the defect.
//! * **A body that panics is reported as its own failure**, never as a block.
//!   Fidelity assertions belong in the body, and misattributing them to contention
//!   would hide them.
//!
//! The waiting bound cannot produce a false pass. Shortening it can only make a
//! correct path look blocked, which fails closed; nothing about lengthening it lets a
//! blocking path through, because the guard is held for the whole wait.

use std::{
    panic,
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, ThreadId},
    time::Duration,
};

use parking_lot::{Mutex, RwLock};

/// How long to wait before concluding a per-event operation blocked.
///
/// Six orders of magnitude above what an unblocked path needs, because the cost of
/// being wrong in the other direction is a flaky gate.
pub const PATIENCE: Duration = Duration::from_secs(2);

/// Only one gate runs at a time in a test binary.
///
/// Two gates each holding a different process-wide lock will each blame *their* lock
/// for the other's guard: the operation really did block, on something the report does
/// not name. Serialising inside the mechanism rather than at the call sites is what
/// makes every failure attributable without a caller having to remember.
static ONE_GATE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// One per-event path, and the cross-session lock it must not need.
pub struct PerEventPath<'a> {
    /// The operation under test, quoted verbatim in the failure.
    pub path: &'a str,
    /// The lock held while it runs, quoted verbatim in the failure so a report says
    /// which shared state the path reached for.
    pub shared_lock: &'a str,
    /// How long to wait before calling it blocked. [`PATIENCE`] unless a test is
    /// deliberately provoking the failure and does not want to wait for it.
    pub patience: Duration,
}

/// Run `operation` on another thread while `shared` is write-locked, and fail if it
/// does not finish.
///
/// Returns the operation's value, so the caller can assert that the burst it asked
/// for actually happened — this helper cannot know what the path was supposed to do,
/// and an operation that does nothing would pass.
#[track_caller]
pub fn assert_completes_while_locked<L, T>(
    path: PerEventPath<'_>,
    shared: &RwLock<L>,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T
where
    T: Send + 'static,
{
    complete_while_held(path, || shared.write(), operation)
}

/// [`assert_completes_while_locked`] for a shared state held behind a plain `Mutex`.
///
/// A `Mutex` has one side, so the read/write care the `RwLock` form documents does not
/// arise: holding it is exclusive by construction and any acquisition on the path
/// blocks. Everything else — the other thread, the re-raised panic, the one gate at a
/// time — is the same mechanism, which is why they share a body.
#[track_caller]
pub fn assert_completes_while_mutex_locked<L, T>(
    path: PerEventPath<'_>,
    shared: &Mutex<L>,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T
where
    T: Send + 'static,
{
    complete_while_held(path, || shared.lock(), operation)
}

/// The mechanism both public forms share.
///
/// `hold` is a closure rather than an already-taken guard, and that is not stylistic:
/// an argument would be evaluated *before* the call, taking the caller's lock ahead of
/// `ONE_GATE_AT_A_TIME` and inverting the order against a concurrent gate. The
/// mechanism's own `two_gates_never_overlap_and_so_cannot_blame_each_other_s_lock`
/// caught exactly that when this body was first factored out. `G` is unbounded because
/// all this needs of the guard is to hold it and drop it at the three points that
/// matter.
#[track_caller]
fn complete_while_held<G, T>(
    path: PerEventPath<'_>,
    hold: impl FnOnce() -> G,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T
where
    T: Send + 'static,
{
    let _sole_gate = ONE_GATE_AT_A_TIME.lock();
    let guard = hold();

    let (finished, wait) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("contention-probe".to_owned())
        .spawn(move || {
            let outcome = operation();
            // Reporting before the thread winds down keeps a body that blocks on the
            // way out from reading as a completion.
            let _ = finished.send(outcome);
        })
        .expect("a probe thread");

    match wait.recv_timeout(path.patience) {
        Ok(outcome) => {
            drop(guard);
            worker.join().expect("a completed operation cannot panic");
            outcome
        }
        // The body panicked before reporting. Release the lock so nothing is stuck,
        // then re-raise its panic: it is the caller's assertion, not a block.
        Err(RecvTimeoutError::Disconnected) => {
            drop(guard);
            match worker.join() {
                Ok(()) => unreachable!("a worker that reported nothing did not complete"),
                Err(payload) => panic::resume_unwind(payload),
            }
        }
        Err(RecvTimeoutError::Timeout) => {
            // Dropping the guard here lets the blocked worker finish and exit rather
            // than outliving the test still parked on the lock.
            drop(guard);
            panic!(
                "{path}: did not complete in {patience:?} while {lock} was held, \
                 so it acquires a lock shared beyond its own session. Section 7.3 requires \
                 per-event paths to resolve their handles once, at bring-up.",
                path = path.path,
                patience = path.patience,
                lock = path.shared_lock,
            )
        }
    }
}

/// The thread a probe body ran on, for the mechanism's own tests.
#[must_use]
pub fn current_thread() -> ThreadId {
    thread::current().id()
}

#[cfg(test)]
mod tests {
    use super::{PATIENCE, PerEventPath, assert_completes_while_locked, current_thread};
    use parking_lot::RwLock;
    use std::{thread, time::Duration};

    // Short, because these tests provoke the failure on purpose and the wait is the
    // whole cost of it.
    const IMPATIENT: Duration = Duration::from_millis(50);

    fn probe(patience: Duration) -> PerEventPath<'static> {
        PerEventPath {
            path: "the operation under test",
            shared_lock: "the shared registry",
            patience,
        }
    }

    #[test]
    fn an_operation_that_leaves_the_lock_alone_completes_and_returns_its_value() {
        static SHARED: RwLock<u32> = RwLock::new(7);
        let doubled = assert_completes_while_locked(probe(PATIENCE), &SHARED, || 21 * 2);
        assert_eq!(doubled, 42);
    }

    #[test]
    #[should_panic(expected = "did not complete in")]
    fn an_operation_that_takes_a_read_guard_is_reported() {
        // The case a held *read* guard would let through, since an `RwLock` admits
        // concurrent readers.
        static SHARED: RwLock<u32> = RwLock::new(0);
        assert_completes_while_locked(probe(IMPATIENT), &SHARED, || *SHARED.read());
    }

    #[test]
    #[should_panic(expected = "acquires a lock shared beyond its own session")]
    fn an_operation_that_takes_a_write_guard_is_reported() {
        static SHARED: RwLock<u32> = RwLock::new(0);
        assert_completes_while_locked(probe(IMPATIENT), &SHARED, || *SHARED.write() += 1);
    }

    #[test]
    fn the_operation_runs_on_a_thread_of_its_own() {
        // Not cosmetic: on the guard holder's thread a re-entrant acquisition parks
        // forever, so the defect would hang the suite instead of failing it.
        static SHARED: RwLock<u32> = RwLock::new(0);
        let here = current_thread();
        let there = assert_completes_while_locked(probe(PATIENCE), &SHARED, current_thread);
        assert_ne!(there, here);
    }

    #[test]
    #[should_panic(expected = "the operation's own assertion")]
    fn a_panicking_operation_reports_its_own_failure_rather_than_a_block() {
        static SHARED: RwLock<u32> = RwLock::new(0);
        assert_completes_while_locked(probe(PATIENCE), &SHARED, || {
            panic!("the operation's own assertion")
        });
    }

    #[test]
    fn the_failure_names_the_path_and_the_lock_it_reached_for() {
        static SHARED: RwLock<u32> = RwLock::new(0);
        let payload = std::panic::catch_unwind(|| {
            assert_completes_while_locked(probe(IMPATIENT), &SHARED, || *SHARED.read());
        })
        .expect_err("a blocking operation must fail");
        let message = payload
            .downcast_ref::<String>()
            .expect("assertion messages are formatted");

        assert!(
            message.contains("the operation under test"),
            "the path is missing from: {message}"
        );
        assert!(
            message.contains("the shared registry"),
            "the lock is missing from: {message}"
        );
    }

    #[test]
    fn two_gates_never_overlap_and_so_cannot_blame_each_other_s_lock() {
        static FIRST: RwLock<u32> = RwLock::new(0);
        static SECOND: RwLock<u32> = RwLock::new(0);

        // Each operation reads the *other* gate's lock, and sleeps first so that both
        // guards would certainly be held at once if the gates ran together. Run in
        // turn, each operation finds the other lock free and both gates pass. Run at
        // once, each operation parks on the other's guard and both report a block that
        // is not theirs. The sleep is how the overlap is made certain rather than
        // likely; a barrier cannot do it, because serialised gates would never release
        // it.
        fn cross_read(other: &'static RwLock<u32>) -> u32 {
            thread::sleep(Duration::from_millis(20));
            *other.read()
        }

        let first = thread::spawn(|| {
            assert_completes_while_locked(probe(IMPATIENT), &FIRST, || cross_read(&SECOND))
        });
        let second = thread::spawn(|| {
            assert_completes_while_locked(probe(IMPATIENT), &SECOND, || cross_read(&FIRST))
        });

        first.join().expect("the first gate must pass");
        second.join().expect("the second gate must pass");
    }
}
