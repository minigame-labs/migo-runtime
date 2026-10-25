//! Drive a Send future one poll at a time on the existing fair executor.
//! The waiting task owns the result slot, so cancelling an individual wait
//! cannot discard a poll's result. Dropping the owner cancels queued work;
//! already-running native calls retain their credit until that poll returns.

use parking_lot::Mutex;
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};
use tokio::sync::{Notify, oneshot};

use crate::{
    pools::{ByteAdmitError, ByteTicket, IoPools},
    task::{PoolKind, PriorityClass},
};

struct Signal(Notify);
impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_one();
    }
}

struct State<F> {
    future: Option<Pin<Box<F>>>,
    _retained: ByteTicket,
}

type Flight<T> = oneshot::Receiver<std::thread::Result<Poll<T>>>;

/// A future polled off the caller's thread under its captured Tokio handle.
/// It must be Send: local tasks and engine-affine handles cannot enter here.
pub struct Offloaded<F: Future> {
    state: Arc<Mutex<State<F>>>,
    pools: IoPools,
    class: PoolKind,
    runtime: Option<tokio::runtime::Handle>,
    signal: Arc<Signal>,
    cancelled: Arc<AtomicBool>,
    flight: Option<Flight<F::Output>>,
    waiting: bool,
    finished: bool,
}

impl<F: Future + Send + 'static> Offloaded<F>
where
    F::Output: Send + 'static,
{
    /// Credit covers the retained operation, not an allocator-enforced cap.
    /// Constructing this object needs no running Tokio runtime or new threads.
    pub fn new(
        future: F,
        pools: IoPools,
        class: PoolKind,
        retained_bytes: u64,
    ) -> Result<Self, ByteAdmitError> {
        let retained = pools.reserve_bytes(retained_bytes)?;
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                future: Some(Box::pin(future)),
                _retained: retained,
            })),
            pools,
            class,
            runtime: tokio::runtime::Handle::try_current().ok(),
            signal: Arc::new(Signal(Notify::new())),
            cancelled: Arc::new(AtomicBool::new(false)),
            flight: None,
            waiting: false,
            finished: false,
        })
    }

    pub async fn wait(&mut self) -> io::Result<F::Output> {
        if self.finished {
            return Err(io::Error::other("offloaded future already completed"));
        }
        if self.runtime.is_none() {
            self.runtime = tokio::runtime::Handle::try_current().ok();
        }
        loop {
            if self.waiting {
                self.signal.0.notified().await;
                self.waiting = false;
            }
            if self.flight.is_none() {
                let state = self.state.clone();
                let cancelled = self.cancelled.clone();
                let waker = Waker::from(self.signal.clone());
                let runtime = self.runtime.clone();
                self.flight = Some(
                    self.pools
                        .submit_async_bytes(
                            self.class,
                            PriorityClass::ForegroundAsync,
                            256,
                            move || {
                                // A queued operation abandoned before it starts never polls.
                                if cancelled.load(Ordering::Acquire) {
                                    return Poll::Pending;
                                }
                                let _entered = runtime.as_ref().map(tokio::runtime::Handle::enter);
                                let mut state = state.lock();
                                let result = state
                                    .future
                                    .as_mut()
                                    .expect("one live offloaded future")
                                    .as_mut()
                                    .poll(&mut Context::from_waker(&waker));
                                if result.is_ready() {
                                    state.future = None;
                                }
                                result
                            },
                        )
                        .map_err(io::Error::other)?,
                );
            }
            let result = self.flight.as_mut().unwrap().await;
            self.flight = None;
            match result {
                Ok(Ok(Poll::Ready(value))) => {
                    self.finished = true;
                    return Ok(value);
                }
                Ok(Ok(Poll::Pending)) => self.waiting = true,
                Ok(Err(_)) => {
                    self.finished = true;
                    return Err(io::Error::other("offloaded worker panicked"));
                }
                Err(_) => {
                    self.finished = true;
                    return Err(io::Error::other("offloaded executor closed"));
                }
            }
        }
    }
}

impl<F: Future> Drop for Offloaded<F> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Run an owned future; dropping this wait abandons the operation.
pub async fn run<F: Future + Send + 'static>(
    future: F,
    pools: IoPools,
    class: PoolKind,
    retained_bytes: u64,
) -> io::Result<F::Output>
where
    F::Output: Send + 'static,
{
    Offloaded::new(future, pools, class, retained_bytes)
        .map_err(io::Error::other)?
        .wait()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::atomic::AtomicUsize, time::Duration};

    #[tokio::test]
    async fn future_polls_off_thread_with_its_tokio_handle_and_self_wakes() {
        let caller = std::thread::current().id();
        let pools = IoPools::local_for_test(7092, 2);
        let value = run(
            async move {
                assert_ne!(std::thread::current().id(), caller);
                tokio::time::sleep(Duration::from_millis(1)).await;
                17
            },
            pools,
            PoolKind::Tls,
            1024,
        )
        .await
        .unwrap();
        assert_eq!(value, 17);
    }

    #[tokio::test]
    async fn cancelled_wait_preserves_the_once_polled_result() {
        let pools = IoPools::local_for_test(7093, 2);
        let (release, gate) = std::sync::mpsc::channel();
        let mut task = Offloaded::new(
            async move {
                gate.recv().unwrap();
                42
            },
            pools,
            PoolKind::Tls,
            1024,
        )
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), task.wait())
                .await
                .is_err()
        );
        release.send(()).unwrap();
        assert_eq!(task.wait().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn abandoned_blocking_tls_does_not_stall_fs_and_returns_credit_when_it_finishes() {
        let pools = IoPools::local_with_byte_limit_for_test(7094, 2, 4096);
        let (release, gate) = std::sync::mpsc::channel();
        let mut task = Offloaded::new(
            async move {
                gate.recv().unwrap();
                42
            },
            pools.clone(),
            PoolKind::Tls,
            1024,
        )
        .unwrap();
        let state = Arc::downgrade(&task.state);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), task.wait())
                .await
                .is_err()
        );
        drop(task);
        assert!(
            pools.reserve_bytes(4096).is_err(),
            "running poll must retain its memory credit"
        );
        let fs = pools
            .submit_async_bytes(PoolKind::Fs, PriorityClass::ForegroundAsync, 0, || 23)
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), fs)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            23
        );
        release.send(()).unwrap();
        let fence = pools
            .submit_async_bytes(PoolKind::Tls, PriorityClass::ForegroundAsync, 0, || ())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), fence)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(state.upgrade().is_none());
        assert!(
            pools.reserve_bytes(4096).is_ok(),
            "completed abandoned poll leaked memory credit"
        );
    }

    #[tokio::test]
    async fn a_pending_future_does_not_busy_poll_and_can_resume_after_wait_cancel() {
        let polls = Arc::new(AtomicUsize::new(0));
        let count = polls.clone();
        let ready = Arc::new(AtomicBool::new(false));
        let seen_ready = ready.clone();
        let saved = Arc::new(Mutex::new(None::<Waker>));
        let waker = saved.clone();
        let future = std::future::poll_fn(move |cx| {
            count.fetch_add(1, Ordering::Relaxed);
            *waker.lock() = Some(cx.waker().clone());
            if seen_ready.load(Ordering::Acquire) {
                Poll::Ready(5)
            } else {
                Poll::Pending
            }
        });
        let mut task = Offloaded::new(
            future,
            IoPools::local_for_test(7095, 2),
            PoolKind::Tls,
            1024,
        )
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(25), task.wait())
                .await
                .is_err()
        );
        assert_eq!(polls.load(Ordering::Relaxed), 1);
        ready.store(true, Ordering::Release);
        saved.lock().take().unwrap().wake();
        assert_eq!(task.wait().await.unwrap(), 5);
    }
}
