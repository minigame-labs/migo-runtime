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
//! The value moves in and out rather than being borrowed, so nothing is queued and
//! nothing is buffered: the caller's backpressure is exactly what it was, one step at
//! a time in the order the chunks arrived. A step that panics takes the value with
//! it, which is why [`OffWorker::with`] hands `Self` back only on success — a
//! poisoned wrapper is not a state a caller can reach.

use shared::error::{EngineError, EngineResult, ErrorCode};

/// A value whose work is CPU-bound, reachable only from a blocking thread.
pub(crate) struct OffWorker<T> {
    value: T,
}

impl<T: Send + 'static> OffWorker<T> {
    pub(crate) fn new(value: T) -> Self {
        Self { value }
    }

    /// Run `step` against the value on a thread the async executor is not waiting
    /// for, and hand back the value along with what the step produced.
    ///
    /// Requires a tokio runtime context, which is where CPU-bound work would
    /// otherwise be a problem.
    pub(crate) async fn with<R: Send + 'static>(
        self,
        step: impl FnOnce(&mut T) -> R + Send + 'static,
    ) -> EngineResult<(Self, R)> {
        let mut value = self.value;
        match tokio::task::spawn_blocking(move || {
            let outcome = step(&mut value);
            (value, outcome)
        })
        .await
        {
            Ok((value, outcome)) => Ok((Self { value }, outcome)),
            // The step panicked, or the runtime is shutting down. Either way the
            // value is gone, so the caller gets an error instead of a wrapper it
            // could ask again.
            Err(joined) => Err(EngineError::from_detail(
                ErrorCode::Internal,
                format!("a step off the worker did not finish: {joined}"),
            )),
        }
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
        let counted = executor().block_on(async {
            let (value, first) = OffWorker::new(Vec::new())
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
        let outcome = executor().block_on(async {
            OffWorker::new(())
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
