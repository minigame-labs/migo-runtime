//! The generation a runtime belongs to, and the only authority that advances it.
//!
//! A restart replaces the JavaScript isolate. Work the retired isolate started
//! can still complete afterwards, and delivering it into the replacement is the
//! defect this exists to make impossible: every runtime-owned callback carries
//! the generation that created it, and the Host compares before invoking
//! JavaScript.
//!
//! There is one writer and many readers, and the split is in the types rather
//! than in a convention. [`RestartBoundary`] can advance the generation;
//! [`RuntimeGenerationReader`] is what everything else holds and it has no
//! mutation API at all — not a private one, none. A reader that could store
//! would be a second authority, and two authorities disagree.
//!
//! Advancing is `commit(retired, candidate)` with `candidate == retired + 1`,
//! compare-exchanged against the live value. That refuses a stale committer
//! outright instead of letting the last writer win, which is what makes an
//! abandoned candidate harmless.

use std::sync::{
    Arc,
    atomic::{
        AtomicI64,
        Ordering::{AcqRel, Acquire},
    },
};

use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::protocol::host_cmd::HostCommand;

/// Whether `cmd` was produced for a runtime that is no longer the current one.
///
/// The only implementation of the rule, called by both executions before a
/// command reaches content; a free function so it can be driven without a live
/// session.
///
/// A command carrying no generation is never retired — see
/// [`HostCommand::callback_generation`] for the two reasons it may carry none,
/// neither of which means "stale".
pub(crate) fn is_retired_callback(cmd: &HostCommand, current_generation: i64) -> bool {
    matches!(cmd.callback_generation(), Some(produced_for) if produced_for.get() != current_generation)
}

/// The writer. One per Host, constructed before any sender is registered.
pub(crate) struct RestartBoundary {
    current: Arc<AtomicI64>,
}

/// A read-only view of the same generation. Cloneable, and deliberately inert.
#[derive(Clone)]
pub(crate) struct RuntimeGenerationReader {
    current: Arc<AtomicI64>,
}

impl RestartBoundary {
    /// Generations start at one, so zero remains available to mean "no
    /// generation" at boundaries that must express absence.
    pub(crate) fn new() -> Self {
        Self {
            current: Arc::new(AtomicI64::new(1)),
        }
    }

    pub(crate) fn current(&self) -> i64 {
        self.current.load(Acquire)
    }

    pub(crate) fn reader(&self) -> RuntimeGenerationReader {
        RuntimeGenerationReader {
            current: Arc::clone(&self.current),
        }
    }

    /// The generation a candidate runtime would take, without taking it.
    ///
    /// Nothing is mutated here: a candidate that fails to initialise must leave
    /// the live generation exactly as it was, so reserving the number would be
    /// the wrong shape.
    ///
    /// Unused in production until the restart path builds an unpublished
    /// candidate. It exists now, with `commit`, because the alternative is a
    /// later task inventing its own way to advance the generation, and two ways
    /// to advance it is two authorities. Its behaviour is covered by this
    /// module's tests today.
    #[allow(dead_code)]
    pub(crate) fn candidate_generation(&self) -> EngineResult<i64> {
        self.current().checked_add(1).ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidOperation).with_msg("runtime generation exhausted")
        })
    }

    /// Publish `candidate`, but only from exactly the generation it succeeds.
    ///
    /// Unused in production for the same reason as `candidate_generation`, and
    /// paired with it deliberately: reserving a number and publishing it are one
    /// decision, and splitting them across tasks is how the second authority
    /// gets written.
    #[allow(dead_code)]
    pub(crate) fn commit(&self, retired: i64, candidate: i64) -> EngineResult<()> {
        if candidate != retired + 1 {
            return Err(EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("runtime generation commit is not the successor of the retired one")
                .with_detail(format!("retired={retired} candidate={candidate}")));
        }
        self.current
            .compare_exchange(retired, candidate, AcqRel, Acquire)
            .map(|_| ())
            .map_err(|observed| {
                EngineError::new(ErrorCode::InvalidOperation)
                    .with_msg("runtime generation commit lost its race")
                    .with_detail(format!("expected={retired} observed={observed}"))
            })
    }

    /// A boundary whose next candidate would overflow.
    ///
    /// Test-only and private for the same reason the callback-id allocator's is:
    /// a caller able to choose the current generation is a caller able to move
    /// it backwards, and a generation that can go backwards can be reused.
    #[cfg(test)]
    fn nearly_exhausted() -> Self {
        Self {
            current: Arc::new(AtomicI64::new(i64::MAX)),
        }
    }
}

impl RuntimeGenerationReader {
    pub(crate) fn current(&self) -> i64 {
        self.current.load(Acquire)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use shared::{callback_id::CallbackIdAllocator, error::ErrorCode};

    use super::RestartBoundary;

    /// Stands in for the three places that hold op-state in Task 2: two runtime
    /// states across a restart and one Worker. What matters is that they hold
    /// clones of one allocator, not that they are `HostOpState`.
    struct SimulatedState {
        callback_ids: Arc<CallbackIdAllocator>,
        runtime_generation: i64,
    }

    #[test]
    fn restart_boundary_starts_at_one_and_its_reader_agrees() {
        let boundary = RestartBoundary::new();
        let reader = boundary.reader();

        assert_eq!(boundary.current(), 1);
        assert_eq!(reader.current(), 1);
    }

    #[test]
    fn restart_boundary_shares_one_callback_id_space_across_states_and_worker() {
        let boundary = RestartBoundary::new();
        let callback_ids = Arc::new(CallbackIdAllocator::default());

        let first = SimulatedState {
            callback_ids: Arc::clone(&callback_ids),
            runtime_generation: boundary.current(),
        };
        let worker = SimulatedState {
            callback_ids: Arc::clone(&first.callback_ids),
            runtime_generation: first.runtime_generation,
        };
        let second = SimulatedState {
            callback_ids: Arc::clone(&callback_ids),
            runtime_generation: boundary.current(),
        };

        assert!(Arc::ptr_eq(&first.callback_ids, &worker.callback_ids));
        assert!(Arc::ptr_eq(&first.callback_ids, &second.callback_ids));
        // Interleaved on purpose: a per-state allocator would restart the
        // sequence and every one of these would be 1.
        assert_eq!(first.callback_ids.allocate(), Ok(1));
        assert_eq!(worker.callback_ids.allocate(), Ok(2));
        assert_eq!(second.callback_ids.allocate(), Ok(3));
    }

    #[test]
    fn a_candidate_generation_does_not_move_the_live_one() {
        let boundary = RestartBoundary::new();
        let reader = boundary.reader();

        assert_eq!(boundary.candidate_generation().unwrap(), 2);
        assert_eq!(boundary.candidate_generation().unwrap(), 2);
        assert_eq!(boundary.current(), 1);
        assert_eq!(reader.current(), 1, "an abandoned candidate is invisible");
    }

    #[test]
    fn an_exact_commit_publishes_to_every_reader() {
        let boundary = RestartBoundary::new();
        let reader = boundary.reader();
        let candidate = boundary.candidate_generation().unwrap();

        boundary.commit(1, candidate).expect("successor commits");

        assert_eq!(boundary.current(), 2);
        assert_eq!(reader.current(), 2);
    }

    #[test]
    fn a_stale_or_skipping_commit_is_refused_and_changes_nothing() {
        let boundary = RestartBoundary::new();
        boundary.commit(1, 2).expect("first restart");

        // Stale: this committer still believes it is retiring generation 1.
        let stale = boundary.commit(1, 2).unwrap_err();
        assert_eq!(stale.code, ErrorCode::InvalidOperation);
        assert!(stale.msg.contains("lost its race"));

        // Skipping: a successor that is not the next one would leave a
        // generation nobody ever ran, so ids stamped with it match nothing.
        let skipping = boundary.commit(2, 4).unwrap_err();
        assert_eq!(skipping.code, ErrorCode::InvalidOperation);
        assert!(skipping.msg.contains("successor"));

        assert_eq!(boundary.current(), 2, "a refused commit must not publish");
    }

    #[test]
    fn generation_exhaustion_fails_without_mutating() {
        let boundary = RestartBoundary::nearly_exhausted();

        let error = boundary.candidate_generation().unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidOperation);
        assert!(error.msg.contains("runtime generation exhausted"));
        assert_eq!(boundary.current(), i64::MAX, "a refusal must not advance");
    }
}
