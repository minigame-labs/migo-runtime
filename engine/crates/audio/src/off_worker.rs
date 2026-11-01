//! Keeps CPU-bound work off an async worker that sessions share.
//!
//! A shared async worker can let one game's streaming work occupy the only
//! worker thread and delay another game's I/O. The way it was spent was not a
//! lock or an allocation but occupancy: a CPU-bound step running inline in an
//! async task holds the worker thread that polls it, and a worker holding one task's
//! CPU work cannot poll anyone else's I/O.
//!
//! This type is the answer, and the shape is the point. It owns the value whose work
//! is CPU-bound, its field is private, and it has no accessor — so an async task
//! holding one cannot reach the value except inside [`OffWorker::with`], whose step
//! runs on a blocking thread. Running the work on the worker anyway is a compile
//! error rather than a convention someone has to remember, which is the same
//! instrument `LiveContextCount` uses for the Skia budget.
//!
//! One caller submits one bounded step at a time. Process/host admission and
//! scheduling belong to the shared audio CPU lane; dropping a queued operation
//! transfers its value, captures and unclaimed result to CPU-only disposal.

use migo_io::audio_cpu::AudioCpu;
use shared::error::EngineResult;
use tokio::sync::mpsc;

/// A value whose work is CPU-bound, reachable only from a blocking thread.
pub(crate) struct OffWorker<T> {
    // Same exclusive allocation across every CPU turn, including waiting custody.
    value: Option<Box<T>>,
    cpu: AudioCpu,
    retired: mpsc::Sender<Retired<T>>,
}
// Destroy state before a captured input/output that may own its final credit.
struct Retired<T> {
    _value: Box<T>,
    _adjunct: Option<Box<dyn Send>>,
}
impl<T> Drop for OffWorker<T> {
    fn drop(&mut self) {
        let Some(value) = self.value.take() else {
            return;
        };
        if self
            .retired
            .try_send(Retired {
                _value: value,
                _adjunct: None,
            })
            .is_err()
        {
            unreachable!("exclusive OffWorker retirement receiver disappeared");
        }
    }
}
// Protect both a not-yet-executed step and a computed result until extraction.
// The owner is linear: only one such message can ever retire this allocation.
struct Handoff<T, P: Send + 'static> {
    value: Option<(OffWorker<T>, P)>,
}
impl<T, P: Send + 'static> Handoff<T, P> {
    fn take(mut self) -> (OffWorker<T>, P) {
        self.value.take().unwrap()
    }
}
impl<T, P: Send + 'static> Drop for Handoff<T, P> {
    fn drop(&mut self) {
        if let Some((mut owner, adjunct)) = self.value.take() {
            let value = owner.value.take().unwrap();
            if owner
                .retired
                .try_send(Retired {
                    _value: value,
                    _adjunct: Some(Box::new(adjunct)),
                })
                .is_err()
            {
                unreachable!("exclusive OffWorker handoff receiver disappeared");
            }
        }
    }
}
async fn retire<T: Send + 'static>(mut incoming: mpsc::Receiver<Retired<T>>, cpu: AudioCpu) {
    // Each unique owner owns its original resource credits until this drop.
    // The process-lifetime receiver is never tied to the caller's abort handle.
    if let Some(value) = incoming.recv().await {
        cpu.retirement().retire(value).await;
    }
}
impl<T: Send + 'static> OffWorker<T> {
    pub(crate) fn new(value: T, cpu: AudioCpu) -> Self {
        Self::from_box(Box::new(value), cpu)
    }
    /// Transfer already boxed waiting custody without reallocating its state.
    pub(crate) fn from_box(value: Box<T>, cpu: AudioCpu) -> Self {
        let (retired, incoming) = mpsc::channel(1);
        crate::streaming::get_stream_runtime().spawn(retire(incoming, cpu.clone()));
        Self {
            value: Some(value),
            cpu,
            retired,
        }
    }
    /// Custody starts before first poll. Cancellation never runs a queued step;
    /// captures and unclaimed results retire on the original bounded CPU lane.
    pub(crate) fn with<R: Send + 'static>(
        self,
        step: impl FnOnce(&mut T) -> R + Send + 'static,
    ) -> impl std::future::Future<Output = EngineResult<(Self, R)>> + Send + 'static {
        let call = Handoff {
            value: Some((self, step)),
        };
        async move {
            let cpu = call.value.as_ref().unwrap().0.cpu.clone();
            let result = cpu
                .run(move || {
                    let (mut owner, step) = call.take();
                    let outcome = step(owner.value.as_mut().unwrap().as_mut());
                    Handoff {
                        value: Some((owner, outcome)),
                    }
                })
                .await?;
            Ok(result.take())
        }
    }
}

#[cfg(test)]
#[path = "off_worker_custody_tests.rs"]
mod custody_tests;

#[cfg(test)]
mod tests {
    use super::OffWorker;
    use tokio::runtime::{Builder, Runtime};

    fn executor() -> Runtime {
        Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .expect("a test executor")
    }

    #[test]
    fn handoff_future_size_is_independent_of_the_owned_payload() {
        let host = migo_io::scheduler::IoScheduler::new(0);
        let small = OffWorker::new((), host.audio_cpu()).with(|_| ());
        let large = OffWorker::new([0_u8; 64 * 1024], host.audio_cpu()).with(|_| ());
        assert_eq!(std::mem::size_of_val(&small), std::mem::size_of_val(&large));
    }

    #[test]
    fn successive_steps_keep_the_same_exclusive_value_address() {
        super::custody_tests::custody();
        let host = migo_io::scheduler::IoScheduler::new(0);
        executor().block_on(async {
            let (owner, first) = OffWorker::new([0_u8; 8192], host.audio_cpu())
                .with(|value| value.as_ptr() as usize)
                .await
                .unwrap();
            let (owner, second) = owner
                .with(|value| {
                    value[0] = 19;
                    value.as_ptr() as usize
                })
                .await
                .unwrap();
            let (_, byte) = owner.with(|value| value[0]).await.unwrap();
            assert_eq!(first, second);
            assert_eq!(byte, 19);
        });
    }

    // The property this type exists for -- that a step leaves the worker polling it
    // free -- is gated once, on the production path, by
    // `streaming::tests::the_decode_step_leaves_the_shared_streaming_worker_free`. A
    // second gate here would only restate it against a runtime nobody ships, and two
    // guards on one case pin it no better than one. What is left here is what that
    // gate cannot see: whether the value survives the round trip.

    #[test]
    fn what_a_step_changed_is_what_the_next_step_sees() {
        // The value crosses two thread boundaries per step. A `with` that handed back
        // anything but the value the step mutated would restart the decoder's buffer
        // on every chunk, which is a wrong answer rather than a slow one.
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let counted = executor().block_on(async {
            let (value, first) = OffWorker::new(Vec::new(), owner.audio_cpu())
                .with(|log: &mut Vec<&str>| {
                    log.push("first");
                    log.len()
                })
                .await
                .expect("the first step");
            let (_value, second) = value
                .with(|log: &mut Vec<&str>| {
                    log.push("second");
                    log.clone()
                })
                .await
                .expect("the second step");
            (first, second)
        });

        assert_eq!(counted, (1, vec!["first", "second"]));
    }

    #[test]
    fn a_step_that_panics_takes_the_value_with_it_and_reports() {
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let outcome = executor().block_on(async {
            OffWorker::new((), owner.audio_cpu())
                .with(|_value| panic!("the step's own failure"))
                .await
                .map(|_| ())
        });

        // Not a panic on the caller's thread, and no wrapper handed back: a track
        // whose decoder died reports the error and stops rather than asking again.
        let error = outcome.expect_err("a panicking step cannot succeed");
        assert!(
            error.to_string().contains("did not finish"),
            "the failure must say the step did not finish: {error}"
        );
    }
}
