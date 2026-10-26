//! Audio CPU work shares the process executor without retaining its scheduler.
//!
//! Dropping a queued future releases its captured decoder/input/PCM immediately.
//! Its bounded queue placeholder is drained by the executor; running native work
//! cannot be preempted and must obey the codec's per-step budget/cancel flag.
use crate::{
    domain::IoDomain,
    pools::{IoPools, PoolError},
};
use parking_lot::Mutex;
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

#[derive(Clone)]
pub struct AudioCpu {
    pools: IoPools,
    domain: Arc<IoDomain>,
}

// Execution and publication are separate so admission is returned before any
// waiter can observe or wake for the completed result.
pub(crate) trait CpuJob: Send + Sync {
    fn execute(&self);
    fn publish(&self);
}

type Reply<T> = std::thread::Result<Result<T, PoolError>>;
enum Phase<T, F> {
    Queued(F),
    Running,
    Computed(Reply<T>),
    Ready(Reply<T>),
    Cancelled,
}
struct State<T, F> {
    phase: Phase<T, F>,
    waker: Option<Waker>,
}
struct Task<T, F> {
    state: Mutex<State<T, F>>,
    domain: Arc<IoDomain>,
}
impl<T, F> Task<T, F> {
    fn cancel(&self) {
        let retired = {
            let mut state = self.state.lock();
            let phase = std::mem::replace(&mut state.phase, Phase::Cancelled);
            (phase, state.waker.take())
        };
        // Captures, output and custom waker destructors may reenter IO.
        drop(retired);
    }
    fn poll(&self, cx: &mut Context<'_>) -> Poll<Reply<T>> {
        // Clone/drop custom wakers outside the state lock as well.
        let new_waker = cx.waker().clone();
        let mut state = self.state.lock();
        if matches!(state.phase, Phase::Ready(_)) {
            let Phase::Ready(result) = std::mem::replace(&mut state.phase, Phase::Cancelled) else {
                unreachable!()
            };
            drop(state);
            drop(new_waker);
            return Poll::Ready(result);
        }
        let previous = state.waker.replace(new_waker);
        drop(state);
        drop(previous);
        Poll::Pending
    }
}
impl<T: Send + 'static, F: FnOnce() -> T + Send + 'static> CpuJob for Task<T, F> {
    fn execute(&self) {
        let job = {
            let mut state = self.state.lock();
            if !matches!(state.phase, Phase::Queued(_)) {
                return;
            }
            let Phase::Queued(job) = std::mem::replace(&mut state.phase, Phase::Running) else {
                unreachable!()
            };
            job
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if self.domain.is_closed() {
                return Err(PoolError::Closed);
            }
            Ok(job())
        }));
        let mut state = self.state.lock();
        if matches!(state.phase, Phase::Cancelled) {
            drop(state);
            drop(result);
        } else {
            state.phase = Phase::Computed(result);
        }
    }
    fn publish(&self) {
        let wake = {
            let mut state = self.state.lock();
            let phase = std::mem::replace(&mut state.phase, Phase::Cancelled);
            state.phase = match phase {
                Phase::Computed(result) => Phase::Ready(result),
                other => other,
            };
            state.waker.take()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }
}

struct Pending<T, F>(Arc<Task<T, F>>);
impl<T, F> Drop for Pending<T, F> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl AudioCpu {
    pub(crate) fn new(pools: IoPools, domain: Arc<IoDomain>) -> Self {
        Self { pools, domain }
    }

    /// Active stream refill is foreground async work. One task allocation owns
    /// the closure, cancellation, reply and waker; workers never wait for IO.
    pub async fn run<T, F>(&self, job: F) -> EngineResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if self.domain.is_closed() {
            return Err(PoolError::Closed.into());
        }
        let pending = Pending(Arc::new(Task {
            state: Mutex::new(State {
                phase: Phase::Queued(job),
                waker: None,
            }),
            domain: Arc::clone(&self.domain),
        }));
        self.pools
            .submit_audio(pending.0.clone())
            .map_err(EngineError::from)?;
        let mut closed = pin!(self.domain.closed());
        let result = poll_fn(|cx| {
            if closed.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(PoolError::Closed));
            }
            pending.0.poll(cx).map(Ok)
        })
        .await;
        drop(pending);
        if self.domain.is_closed() {
            return Err(PoolError::Closed.into());
        }
        match result {
            Ok(Ok(result)) => result.map_err(EngineError::from),
            Ok(Err(_panic)) => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "an audio CPU step did not finish: worker panicked",
            )),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        scheduler::IoScheduler,
        task::{PoolKind, PriorityClass},
    };
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    const WAIT: Duration = Duration::from_secs(2);
    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.take();
        }
    }
    fn blocked_worker(owner: &IoScheduler) -> Release {
        let (release, wait) = mpsc::channel();
        let (started, observed) = mpsc::channel();
        owner
            .pools()
            .submit_async(PoolKind::Fs, PriorityClass::ForegroundAsync, move || {
                let _ = started.send(());
                let _ = wait.recv();
            })
            .unwrap();
        let guard = Release(Some(release));
        observed.recv_timeout(WAIT).expect("worker did not start");
        guard
    }
    async fn queue<F: Future>(future: std::pin::Pin<&mut F>) {
        let mut future = future;
        poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    struct Capture(Arc<AtomicBool>);
    impl Drop for Capture {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn computed_result_is_not_observable_until_publication() {
        let task = Task {
            state: Mutex::new(State {
                phase: Phase::Queued(|| 17),
                waker: None,
            }),
            domain: Arc::new(IoDomain::new()),
        };
        let mut cx = Context::from_waker(Waker::noop());
        task.execute();
        assert!(
            task.poll(&mut cx).is_pending(),
            "computed result escaped before admission credit returned"
        );
        task.publish();
        assert!(matches!(task.poll(&mut cx), Poll::Ready(Ok(Ok(17)))));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn warmed_audio_handoff_uses_one_caller_allocation_per_step() {
        let owner = IoScheduler::local_for_test(509, 1);
        let cpu = owner.audio_cpu();
        for _ in 0..64 {
            assert_eq!(cpu.run(|| 17).await.unwrap(), 17);
        }
        let before = migo_alloc_probe::thread_counts().allocation_events();
        for _ in 0..128 {
            assert_eq!(cpu.run(|| 17).await.unwrap(), 17);
        }
        let events = migo_alloc_probe::thread_counts().allocation_events() - before;
        assert_eq!(
            events, 128,
            "each warmed handoff must have exactly one task allocation on the caller; CPU/native allocators are outside this counter"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_cancellation_releases_capture_without_executing() {
        let owner = IoScheduler::local_for_test(501, 1);
        let _release = blocked_worker(&owner);
        let cpu = owner.audio_cpu();
        let dropped = Arc::new(AtomicBool::new(false));
        let capture = Capture(dropped.clone());
        let ran = Arc::new(AtomicBool::new(false));
        let job_ran = ran.clone();
        let mut work = Box::pin(cpu.run(move || {
            job_ran.store(true, Ordering::Release);
            drop(capture);
        }));
        queue(work.as_mut()).await;
        drop(work);
        assert!(
            dropped.load(Ordering::Acquire),
            "queued decoder retained after cancellation"
        );
        assert!(!ran.load(Ordering::Acquire));
        assert_eq!(
            owner.pending_work_for_test(),
            1,
            "bounded placeholder remains until dispatch"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_placeholders_still_enforce_host_admission() {
        let owner = IoScheduler::local_for_test(502, 1);
        let _release = blocked_worker(&owner);
        let cpu = owner.audio_cpu();
        for _ in 0..2 {
            let mut work = Box::pin(cpu.run(|| ()));
            queue(work.as_mut()).await;
            drop(work);
        }
        let error = cpu
            .run(|| ())
            .await
            .expect_err("cancellation must not permit an unbounded empty queue");
        assert_eq!(error.code, ErrorCode::InputSaturated);
        assert_eq!(owner.pending_work_for_test(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_wakes_all_queued_audio_and_releases_captures() {
        let owner = IoScheduler::local_for_test(503, 1);
        let _release = blocked_worker(&owner);
        let cpu = owner.audio_cpu();
        let dropped_a = Arc::new(AtomicBool::new(false));
        let dropped_b = Arc::new(AtomicBool::new(false));
        let a = Capture(dropped_a.clone());
        let b = Capture(dropped_b.clone());
        let mut first = Box::pin(cpu.run(move || drop(a)));
        let mut second = Box::pin(cpu.run(move || drop(b)));
        queue(first.as_mut()).await;
        queue(second.as_mut()).await;
        owner.close();
        assert!(
            tokio::time::timeout(WAIT, first)
                .await
                .expect("close must wake first queued job")
                .is_err()
        );
        assert!(
            tokio::time::timeout(WAIT, second)
                .await
                .expect("close must wake second queued job")
                .is_err()
        );
        assert!(dropped_a.load(Ordering::Acquire) && dropped_b.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_close_returns_without_join_and_discards_result() {
        let owner = IoScheduler::local_for_test(504, 1);
        let cpu = owner.audio_cpu();
        let (release, wait) = mpsc::channel();
        let _release = Release(Some(release));
        let (started, observed) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let job_finished = finished.clone();
        let mut work = Box::pin(cpu.run(move || {
            let _ = started.send(());
            let _ = wait.recv();
            job_finished.store(true, Ordering::Release);
            42
        }));
        queue(work.as_mut()).await;
        observed
            .recv_timeout(WAIT)
            .expect("audio worker did not start");
        owner.close();
        assert!(
            tokio::time::timeout(WAIT, work)
                .await
                .expect("close cannot wait for native work")
                .is_err()
        );
        assert!(
            !finished.load(Ordering::Acquire),
            "running native work is not preempted"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_cancellation_drops_the_late_owned_result() {
        let owner = IoScheduler::local_for_test(510, 1);
        let cpu = owner.audio_cpu();
        let (release, wait) = mpsc::channel();
        let release = Release(Some(release));
        let (started, observed) = mpsc::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let capture = Capture(dropped.clone());
        let mut work = Box::pin(cpu.run(move || {
            let _ = started.send(());
            let _ = wait.recv();
            capture
        }));
        queue(work.as_mut()).await;
        observed
            .recv_timeout(WAIT)
            .expect("audio work did not start");
        drop(work);
        assert!(
            !dropped.load(Ordering::Acquire),
            "native work remains active"
        );
        drop(release);
        tokio::time::timeout(WAIT, async {
            while !dropped.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled work retained the late owned result");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn audio_handle_does_not_keep_scheduler_scope_open() {
        let owner = IoScheduler::local_for_test(505, 1);
        let cpu = owner.audio_cpu();
        drop(owner);
        assert!(cpu.run(|| 1).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn panic_is_reported_and_worker_can_run_the_next_step() {
        let owner = IoScheduler::local_for_test(506, 1);
        let cpu = owner.audio_cpu();
        assert_eq!(
            cpu.run(|| panic!("decoder panic")).await.unwrap_err().code,
            ErrorCode::Internal
        );
        assert_eq!(cpu.run(|| 42).await.unwrap(), 42);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_host_close_does_not_reject_another_hosts_audio() {
        let (first, second) = IoScheduler::shared_executor_pair_for_test(507, 508, 1);
        first.close();
        assert!(first.audio_cpu().run(|| 1).await.is_err());
        assert_eq!(second.audio_cpu().run(|| 2).await.unwrap(), 2);
    }
}
