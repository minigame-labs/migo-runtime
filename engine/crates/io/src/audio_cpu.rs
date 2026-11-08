//! Audio CPU work shares the process executor without retaining its scheduler.
//!
//! Dropping a queued future releases its captured decoder/input/PCM immediately.
//! Its bounded queue placeholder is drained by the executor; running native work
//! cannot be preempted and must obey the codec's per-step budget/cancel flag.
use crate::{
    domain::IoDomain,
    pools::{IoPools, PoolError},
    task::PriorityClass,
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
    priority: PriorityClass,
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
    pub(crate) fn new(pools: IoPools, domain: Arc<IoDomain>, priority: PriorityClass) -> Self {
        Self {
            pools,
            domain,
            priority,
        }
    }

    /// Demote preparation to the existing background lane of this same host.
    pub fn background(&self) -> Self {
        Self {
            priority: PriorityClass::Background,
            ..self.clone()
        }
    }

    /// Bound device preparations across hosts before they submit CPU steps.
    /// The device owner must retain this permit across running cancellation,
    /// releasing it only after successful preparation or actual retirement.
    pub async fn preparation_permit(&self) -> EngineResult<tokio::sync::OwnedSemaphorePermit> {
        let mut closed = pin!(self.domain.closed());
        let mut admission = pin!(self.pools.audio_preparation_permit());
        let permit = poll_fn(|cx| {
            if closed.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(PoolError::Closed));
            }
            admission.as_mut().poll(cx).map(Ok)
        })
        .await
        .map_err(EngineError::from)?;
        if self.is_closed() {
            return Err(PoolError::Closed.into());
        }
        Ok(permit)
    }

    /// Check the same close level used by CPU admission and completion.
    pub fn is_closed(&self) -> bool {
        self.domain.is_closed()
    }

    /// A typed foreground audio request is waiting to start on this executor.
    /// Background turns use this hint after a bounded step, preserving progress
    /// even under sustained foreground work. It is not an admission reservation.
    pub fn refill_pending(&self) -> bool {
        self.pools.audio_refill_pending()
    }

    /// Activity sequence shared by this executor's foreground audio requests.
    /// Compare for inequality: wrapping is allowed. Cancelled/rejected requests
    /// also express demand; background work leaves the sequence unchanged.
    pub fn refill_generation(&self) -> usize {
        self.pools.audio_refill_generation()
    }

    /// Observe host shutdown while waiting for local decode admission.
    pub async fn closed(&self) {
        self.domain.closed().await;
    }

    /// Run one bounded step at the priority fixed by the owning handle.
    /// Active refill is foreground; whole-file preparation is background.
    /// One task allocation owns the closure, cancellation, reply and waker;
    /// workers never wait for IO.
    pub async fn run<T, F>(&self, job: F) -> EngineResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if self.domain.is_closed() {
            return Err(PoolError::Closed.into());
        }
        let refill = self.pools.track_audio_refill(self.priority);
        let pending = Pending(Arc::new(Task {
            state: Mutex::new(State {
                phase: Phase::Queued(move || {
                    drop(refill);
                    job()
                }),
                waker: None,
            }),
            domain: Arc::clone(&self.domain),
        }));
        self.pools
            .submit_audio(self.priority, pending.0.clone())
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
#[path = "audio_preparation_tests.rs"]
mod preparation_tests;

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

    #[test]
    fn background_demotion_retains_the_same_host_and_close_notification() {
        super::preparation_tests::contracts();
        let host = IoScheduler::new(0);
        let foreground = host.audio_cpu();
        let background = foreground.background();
        assert_eq!(foreground.priority, PriorityClass::ForegroundAsync);
        assert_eq!(background.priority, PriorityClass::Background);
        assert!(Arc::ptr_eq(&foreground.domain, &background.domain));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        host.close();
        runtime.block_on(background.closed());
        assert!(runtime.block_on(background.run(|| 42)).is_err());
    }

    #[test]
    fn completed_refills_leave_a_shared_generation_without_pending_work() {
        let (first, second) = IoScheduler::shared_executor_pair_for_test(921, 922, 2);
        let background = first.background_audio_cpu();
        let foreground = second.audio_cpu();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let initial = background.refill_generation();
        assert_eq!(runtime.block_on(background.run(|| 23)).unwrap(), 23);
        assert_eq!(background.refill_generation(), initial);
        drop(foreground.run(|| 17));
        assert_eq!(
            background.refill_generation(),
            initial,
            "an unpolled request is not demand"
        );
        for count in 1..=2 {
            assert_eq!(runtime.block_on(foreground.run(|| 17)).unwrap(), 17);
            assert!(!background.refill_pending());
            assert_eq!(background.refill_generation(), initial.wrapping_add(count));
            assert_eq!(
                background.refill_generation(),
                foreground.refill_generation()
            );
        }
        second.close();
        assert!(runtime.block_on(foreground.run(|| 17)).is_err());
        assert_eq!(background.refill_generation(), initial.wrapping_add(2));
    }

    #[test]
    fn queued_refill_hint_is_shared_counted_and_cancelled_before_execution() {
        let (first, second) = IoScheduler::shared_executor_pair_for_test(911, 912, 1);
        let release = blocked_worker(&first);
        let background = first.background_audio_cpu();
        let foreground = second.audio_cpu();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut bg = Box::pin(background.run(|| 23));
            queue(bg.as_mut()).await;
            assert!(
                !background.refill_pending(),
                "background work advertised a refill"
            );
            let mut fg1 = Box::pin(foreground.run(|| panic!("cancelled refill ran")));
            let mut fg2 = Box::pin(foreground.run(|| panic!("cancelled refill ran")));
            queue(fg1.as_mut()).await;
            queue(fg2.as_mut()).await;
            assert!(background.refill_pending() && foreground.refill_pending());
            drop(fg1);
            assert!(
                background.refill_pending(),
                "one of two refills is still queued"
            );
            drop(fg2);
            assert!(
                !background.refill_pending(),
                "cancelled placeholder kept the hint"
            );
            assert_eq!(
                background.refill_generation(),
                2,
                "cancellation erased activity"
            );
            drop(release);
            assert_eq!(bg.await.unwrap(), 23);
        });
    }

    #[test]
    fn refill_hint_retires_before_execution_rejection_and_domain_close() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let host = IoScheduler::local_for_test(913, 1);
        let cpu = host.audio_cpu();
        let inside = cpu.clone();
        assert_eq!(
            runtime
                .block_on(cpu.run(move || {
                    assert!(
                        !inside.refill_pending(),
                        "running refill still advertised as queued"
                    );
                    17
                }))
                .unwrap(),
            17
        );
        let release = blocked_worker(&host);
        let background = host.background_audio_cpu();
        runtime.block_on(async {
            let mut bg1 = Box::pin(background.run(|| 1));
            let mut bg2 = Box::pin(background.run(|| 2));
            queue(bg1.as_mut()).await;
            queue(bg2.as_mut()).await;
            assert_eq!(
                cpu.run(|| 3).await.unwrap_err().code,
                ErrorCode::InputSaturated
            );
            assert!(!cpu.refill_pending(), "rejected admission leaked its hint");
            drop(release);
            assert_eq!((bg1.await.unwrap(), bg2.await.unwrap()), (1, 2));
        });
        let release = blocked_worker(&host);
        runtime.block_on(async {
            let mut queued = Box::pin(cpu.run(|| panic!("closed domain refill ran")));
            queue(queued.as_mut()).await;
            assert!(cpu.refill_pending());
            host.close();
            assert!(queued.await.is_err());
            assert!(!cpu.refill_pending(), "closed domain kept its queued hint");
        });
        drop(release);
    }

    pub(super) const WAIT: Duration = Duration::from_secs(2);
    pub(super) struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.take();
        }
    }
    pub(super) fn blocked_worker(owner: &IoScheduler) -> Release {
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
    pub(super) async fn queue<F: Future>(future: std::pin::Pin<&mut F>) {
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
        for background in [false, true] {
            let owner = IoScheduler::local_for_test(509, 1);
            let cpu = if background {
                owner.background_audio_cpu()
            } else {
                owner.audio_cpu()
            };
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_cancellation_releases_capture_without_executing() {
        for background in [false, true] {
            let owner = IoScheduler::local_for_test(501, 1);
            let _release = blocked_worker(&owner);
            let cpu = if background {
                owner.background_audio_cpu()
            } else {
                owner.audio_cpu()
            };
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_placeholders_still_enforce_host_admission() {
        for background in [false, true] {
            let owner = IoScheduler::local_for_test(502, 1);
            let _release = blocked_worker(&owner);
            let cpu = if background {
                owner.background_audio_cpu()
            } else {
                owner.audio_cpu()
            };
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_wakes_all_queued_audio_and_releases_captures() {
        for background in [false, true] {
            let owner = IoScheduler::local_for_test(503, 1);
            let _release = blocked_worker(&owner);
            let cpu = if background {
                owner.background_audio_cpu()
            } else {
                owner.audio_cpu()
            };
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_close_returns_without_join_and_discards_result() {
        for background in [false, true] {
            let owner = IoScheduler::local_for_test(504, 1);
            let cpu = if background {
                owner.background_audio_cpu()
            } else {
                owner.audio_cpu()
            };
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_cancellation_drops_the_late_owned_result() {
        for background in [false, true] {
            let owner = IoScheduler::local_for_test(510, 1);
            let cpu = if background {
                owner.background_audio_cpu()
            } else {
                owner.audio_cpu()
            };
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn audio_handle_does_not_keep_scheduler_scope_open() {
        for background in [false, true] {
            let owner = IoScheduler::local_for_test(505, 1);
            let cpu = if background {
                owner.background_audio_cpu()
            } else {
                owner.audio_cpu()
            };
            drop(owner);
            assert!(cpu.run(|| 1).await.is_err());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn panic_is_reported_and_worker_can_run_the_next_step() {
        for background in [false, true] {
            let owner = IoScheduler::local_for_test(506, 1);
            let cpu = if background {
                owner.background_audio_cpu()
            } else {
                owner.audio_cpu()
            };
            assert_eq!(
                cpu.run(|| panic!("decoder panic")).await.unwrap_err().code,
                ErrorCode::Internal
            );
            assert_eq!(cpu.run(|| 42).await.unwrap(), 42);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_host_close_does_not_reject_another_hosts_audio() {
        let (first, second) = IoScheduler::shared_executor_pair_for_test(507, 508, 1);
        first.close();
        assert!(first.audio_cpu().run(|| 1).await.is_err());
        assert_eq!(second.audio_cpu().run(|| 2).await.unwrap(), 2);
    }
}

#[cfg(test)]
#[path = "audio_cpu_background_tests.rs"]
mod background_tests;
