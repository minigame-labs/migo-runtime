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
//! releases its captured value without running it.

use migo_io::audio_cpu::AudioCpu;
use shared::error::EngineResult;

/// A value whose work is CPU-bound, reachable only from a blocking thread.
pub(crate) struct OffWorker<T> {
    value: T,
    cpu: AudioCpu,
}

impl<T: Send + 'static> OffWorker<T> {
    pub(crate) fn new(value: T, cpu: AudioCpu) -> Self {
        Self { value, cpu }
    }

    /// Run `step` against the value on a thread the async executor is not waiting
    /// for, and hand back the value along with what the step produced.
    ///
    /// Uses the shared, bounded executor and the owning host's close signal.
    pub(crate) async fn with<R: Send + 'static>(
        self,
        step: impl FnOnce(&mut T) -> R + Send + 'static,
    ) -> EngineResult<(Self, R)> {
        let Self { mut value, cpu } = self;
        let (value, outcome) = cpu
            .run(move || {
                let outcome = step(&mut value);
                (value, outcome)
            })
            .await?;
        Ok((Self { value, cpu }, outcome))
    }
}

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
