//! A cold device burst must wait before filling ordinary Audio admission, and
//! one executor's preparation bound must not serialize unrelated executors.
use super::*;
use crate::scheduler::IoScheduler;

pub(super) fn contracts() {
    for (workers, capacity) in [(1, 1), (2, 1), (6, 2)] {
        let (first, second) = IoScheduler::shared_executor_pair_for_test(331, 332, workers);
        let a = first.background_audio_cpu();
        let b = second.background_audio_cpu();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut permits = Vec::new();
        for _ in 0..capacity {
            permits.push(runtime.block_on(a.preparation_permit()).unwrap());
        }
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut waiting = Box::pin(b.preparation_permit());
        assert!(
            waiting.as_mut().poll(&mut cx).is_pending(),
            "preparation admission exceeded existing CPU parallelism"
        );

        let independent = IoScheduler::local_for_test(333, workers);
        let independent_cpu = independent.background_audio_cpu();
        let mut other = Box::pin(independent_cpu.preparation_permit());
        assert!(
            matches!(other.as_mut().poll(&mut cx), Poll::Ready(Ok(_))),
            "preparation on one executor blocked another executor"
        );

        second.close();
        assert!(
            matches!(waiting.as_mut().poll(&mut cx), Poll::Ready(Err(_))),
            "preparation wait ignored host close"
        );
        drop(waiting);
        let mut cancelled = Box::pin(a.preparation_permit());
        assert!(cancelled.as_mut().poll(&mut cx).is_pending());
        drop(cancelled);
        permits.pop();
        let mut reusable = Box::pin(a.preparation_permit());
        assert!(
            matches!(reusable.as_mut().poll(&mut cx), Poll::Ready(Ok(_))),
            "cancelled preparation waiter leaked admission"
        );
        let mut closed = Box::pin(b.preparation_permit());
        assert!(
            matches!(closed.as_mut().poll(&mut cx), Poll::Ready(Err(_))),
            "closed host acquired preparation admission"
        );
    }
}
