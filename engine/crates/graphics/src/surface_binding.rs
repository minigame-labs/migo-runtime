use std::fmt;

use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    surface::{
        PublicSurfaceGeneration, SurfaceGeneration, SurfaceLease, SurfaceReleaseDisposition,
        SurfaceReleaseTransactionError, release_retired_resource,
    },
};

/// EGL recreation policy derived before native-window extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecreateKind {
    Initial,
    SameGeneration,
    NewGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceBindingError {
    StaleGeneration,
    ConflictingLiveGeneration,
    RecreateInProgress,
}

impl fmt::Display for SurfaceBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration => formatter.write_str("stale Surface generation"),
            Self::ConflictingLiveGeneration => {
                formatter.write_str("conflicting live Surface generation")
            }
            Self::RecreateInProgress => {
                formatter.write_str("Surface recreate transaction already in progress")
            }
        }
    }
}

impl std::error::Error for SurfaceBindingError {}

/// The render thread's single retained native Surface resource binding.
///
/// A retired lease remains here until EGL replacement or teardown so the
/// underlying ANativeWindow cannot be released while EGL may still reference
/// it. Liveness is independent and remains one Acquire load on present checks.
pub(crate) struct RenderSurfaceBinding {
    current: Option<SurfaceLease>,
    pending: Option<SurfaceLease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallPhase {
    BeforePreviousInvalidation,
    PreviousInvalidated,
    CandidateReferenced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateCleanup {
    NotRequired,
    Released,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentationDisposition {
    PreviousUsable,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateLeaseDisposition {
    Unbound,
    Retained,
}

#[derive(Debug, Clone)]
pub(crate) struct SurfaceInstallFailure {
    pub(crate) error: shared::error::EngineError,
    pub(crate) presentation: PresentationDisposition,
    pub(crate) candidate: CandidateLeaseDisposition,
}

impl SurfaceInstallFailure {
    pub(crate) const fn new(
        error: shared::error::EngineError,
        presentation: PresentationDisposition,
        candidate: CandidateLeaseDisposition,
    ) -> Self {
        Self {
            error,
            presentation,
            candidate,
        }
    }

    pub(crate) fn from_phase(
        error: shared::error::EngineError,
        had_usable_previous: bool,
        phase: InstallPhase,
        cleanup: CandidateCleanup,
    ) -> Self {
        let (presentation, candidate) =
            install_failure_disposition(had_usable_previous, phase, cleanup);
        Self::new(error, presentation, candidate)
    }
}

#[derive(Debug)]
pub(crate) enum SurfaceRecreateError {
    Binding(SurfaceBindingError),
    Install(SurfaceInstallFailure),
}

/// Execute the one ownership transaction shared by startup and later Surface
/// updates. The candidate is retained before `install` can touch EGL, then
/// settled only from CanvasManager's explicit ownership disposition.
pub(crate) fn run_surface_recreate<T>(
    binding: &mut RenderSurfaceBinding,
    candidate: SurfaceLease,
    install: impl FnOnce(RecreateKind, &SurfaceLease) -> Result<T, SurfaceInstallFailure>,
) -> Result<(RecreateKind, T), SurfaceRecreateError> {
    let transaction = binding
        .begin_recreate(candidate)
        .map_err(SurfaceRecreateError::Binding)?;
    let kind = transaction.kind();
    // Drained before `commit` drops the previous generation's lease, for the
    // reason `release_retired_resource` gives: whatever the native install and
    // the previous surface's teardown autorelease must not outlive that
    // generation's RELEASED.
    let installed = {
        let _install_pool = shared::objc_autorelease::autorelease_scope();
        install(kind, transaction.candidate())
    };
    match installed {
        Ok(value) => {
            transaction.commit();
            Ok((kind, value))
        }
        Err(failure) => {
            match failure.candidate {
                CandidateLeaseDisposition::Unbound => transaction.abort_unbound(),
                CandidateLeaseDisposition::Retained => transaction.commit(),
            }
            Err(SurfaceRecreateError::Install(failure))
        }
    }
}

/// Apply a direct render-control release without ever dropping the installed
/// native resource before EGL teardown succeeds.
pub(crate) fn release_surface_binding(
    binding: &mut RenderSurfaceBinding,
    generation: SurfaceGeneration,
    teardown: impl FnOnce() -> EngineResult<()>,
) -> EngineResult<SurfaceReleaseDisposition> {
    if binding.pending.is_some() {
        return Err(EngineError::new(ErrorCode::InvalidOperation)
            .with_msg("cannot release Surface during recreate transaction"));
    }

    release_retired_resource(
        &mut binding.current,
        generation,
        SurfaceLease::generation,
        SurfaceLease::is_live,
        teardown,
    )
    .map_err(|error| match error {
        SurfaceReleaseTransactionError::GenerationStillLive => {
            EngineError::new(ErrorCode::InvalidOperation)
                .with_msg("render release requested for a live Surface generation")
                .with_detail(format!("generation={}", generation.get()))
        }
        SurfaceReleaseTransactionError::Teardown(error) => error,
    })
}

/// Classify a failed onscreen install without inferring native ownership from
/// an error code. A referenced candidate remains retained unless cleanup
/// explicitly proved that its EGL objects were released.
pub(crate) fn install_failure_disposition(
    had_usable_previous: bool,
    phase: InstallPhase,
    cleanup: CandidateCleanup,
) -> (PresentationDisposition, CandidateLeaseDisposition) {
    let presentation = if had_usable_previous && phase == InstallPhase::BeforePreviousInvalidation {
        PresentationDisposition::PreviousUsable
    } else {
        PresentationDisposition::Unavailable
    };
    let candidate =
        if phase == InstallPhase::CandidateReferenced && cleanup != CandidateCleanup::Released {
            CandidateLeaseDisposition::Retained
        } else {
            CandidateLeaseDisposition::Unbound
        };
    (presentation, candidate)
}

/// A staged Surface recreate. Dropping this value deliberately leaves the
/// candidate in `RenderSurfaceBinding::pending`: panic unwind must retain the
/// native resource until CanvasManager has torn EGL down.
pub(crate) struct SurfaceRecreateGuard<'a> {
    binding: &'a mut RenderSurfaceBinding,
    kind: RecreateKind,
}

impl SurfaceRecreateGuard<'_> {
    pub(crate) const fn kind(&self) -> RecreateKind {
        self.kind
    }

    pub(crate) fn candidate(&self) -> &SurfaceLease {
        self.binding
            .pending
            .as_ref()
            .expect("staged Surface candidate must exist")
    }

    /// Commit resource ownership after EGL has installed or retained the
    /// candidate. The previous lease is released by this assignment.
    pub(crate) fn commit(self) {
        let candidate = self
            .binding
            .pending
            .take()
            .expect("staged Surface candidate must exist");
        self.binding.current = Some(candidate);
    }

    /// Release a candidate only after CanvasManager proves no EGL object can
    /// reference it.
    pub(crate) fn abort_unbound(self) {
        self.binding.pending = None;
    }
}

impl RenderSurfaceBinding {
    pub(crate) const fn new() -> Self {
        Self {
            current: None,
            pending: None,
        }
    }

    #[inline]
    pub(crate) fn generation(&self) -> Option<SurfaceGeneration> {
        self.current.as_ref().map(SurfaceLease::generation)
    }

    #[inline]
    pub(crate) fn public_generation(&self) -> Option<PublicSurfaceGeneration> {
        self.current.as_ref().map(SurfaceLease::public_generation)
    }

    pub(crate) fn pending_generation(&self) -> Option<SurfaceGeneration> {
        self.pending.as_ref().map(SurfaceLease::generation)
    }

    /// Validate and classify a recreate before raw-handle or EGL access.
    pub(crate) fn preflight(
        &self,
        candidate: &SurfaceLease,
    ) -> Result<RecreateKind, SurfaceBindingError> {
        if self.pending.is_some() {
            return Err(SurfaceBindingError::RecreateInProgress);
        }
        if !candidate.is_live() {
            return Err(SurfaceBindingError::StaleGeneration);
        }

        let Some(current) = self.current.as_ref() else {
            return Ok(RecreateKind::Initial);
        };

        let current_generation = current.generation();
        let candidate_generation = candidate.generation();
        if candidate_generation < current_generation {
            return Err(SurfaceBindingError::StaleGeneration);
        }
        if candidate_generation == current_generation {
            return Ok(RecreateKind::SameGeneration);
        }
        if current.is_live() {
            return Err(SurfaceBindingError::ConflictingLiveGeneration);
        }

        Ok(RecreateKind::NewGeneration)
    }

    /// Validate and retain a candidate before the first EGL/native operation.
    pub(crate) fn begin_recreate(
        &mut self,
        candidate: SurfaceLease,
    ) -> Result<SurfaceRecreateGuard<'_>, SurfaceBindingError> {
        let kind = self.preflight(&candidate)?;
        debug_assert!(self.pending.is_none());
        self.pending = Some(candidate);
        Ok(SurfaceRecreateGuard {
            binding: self,
            kind,
        })
    }

    /// Install a lease directly, skipping staging. **Tests only.**
    ///
    /// Production installs a candidate exclusively through
    /// `SurfaceRecreateGuard::commit`, which exists only once `preflight` has
    /// rejected a dead, older or conflicting generation. This assigns `current`
    /// unconditionally, so reaching it from production would drop the previous
    /// lease while EGL may still reference its native resource — a
    /// use-after-free in the driver, which is the failure the staging
    /// discipline exists to prevent.
    ///
    /// `#[cfg(test)]` rather than a comment asking callers not to use it: the
    /// method is absent from every shipped build, so the bypass cannot acquire
    /// a production caller later. A seam that exists only so a test can skip a
    /// guard has no business in the guarded type's public surface.
    #[cfg(test)]
    pub(crate) fn commit(&mut self, lease: SurfaceLease) {
        debug_assert!(self.pending.is_none());
        self.current = Some(lease);
    }

    /// Acknowledge only a matching generation that is already retired.
    pub(crate) fn on_surface_destroyed(&self, generation: SurfaceGeneration) -> bool {
        self.current
            .as_ref()
            .is_some_and(|lease| lease.generation() == generation && !lease.is_live())
    }

    /// One lock-free liveness read used at the existing present boundaries.
    #[inline]
    pub(crate) fn is_live(&self) -> bool {
        self.current.as_ref().is_some_and(SurfaceLease::is_live)
    }

    /// Release the retained native resource only after EGL teardown.
    pub(crate) fn clear_after_egl_teardown(&mut self) {
        self.pending = None;
        self.current = None;
    }

    /// Permanently retain native ownership when EGL teardown produced no proof
    /// that the driver released its references.
    ///
    /// This is a fail-safe terminal path: leaking one retired native target is
    /// preferable to returning it to a host while an EGL implementation may
    /// still dereference it. No allocation is used here, so the path remains
    /// safe during OOM/panic cleanup as well.
    pub(crate) fn quarantine_after_failed_egl_teardown(&mut self) {
        if let Some(pending) = self.pending.take() {
            std::mem::forget(pending);
        }
        if let Some(current) = self.current.take() {
            std::mem::forget(current);
        }
    }
}

impl Drop for RenderSurfaceBinding {
    fn drop(&mut self) {
        // Every proven-success path clears explicitly. Reaching Drop with a
        // lease means unwind or an unhandled terminal path interrupted EGL
        // teardown, so fail closed instead of guessing that native ownership
        // is safe to return.
        self.quarantine_after_failed_egl_teardown();
    }
}

impl Default for RenderSurfaceBinding {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use shared::surface::{
        Surface, SurfaceGenerationGate, SurfaceLease, SurfaceLivenessToken, SurfaceRef,
        SurfaceReleasePhase,
    };

    use super::{
        CandidateCleanup, CandidateLeaseDisposition, InstallPhase, PresentationDisposition,
        RecreateKind, RenderSurfaceBinding, SurfaceBindingError, SurfaceInstallFailure,
        SurfaceRecreateError, install_failure_disposition, release_surface_binding,
        run_surface_recreate,
    };

    const RENDER_THREAD: &str = include_str!("render_thread.rs");
    const CANVAS_MANAGER: &str = include_str!("canvas/manager/mod.rs");

    fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .expect("function signature must exist");
        let source = &source[start..];
        let open = source.find('{').expect("function body must open");
        let mut depth = 0usize;
        for (offset, ch) in source[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &source[open + 1..open + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("function body must close");
    }

    /// Whether `haystack` contains `needle`, ignoring whitespace entirely.
    ///
    /// These assertions are about structure, not layout. Spelling one as a
    /// single-line literal makes rustfmt an author of the contract: reflowing
    /// `a.b(c)` across two lines then reads as the call having been removed.
    /// That is not hypothetical -- it is how this check started failing.
    fn contains_ignoring_whitespace(haystack: &str, needle: &str) -> bool {
        let strip =
            |text: &str| -> String { text.chars().filter(|ch| !ch.is_whitespace()).collect() };
        strip(haystack).contains(&strip(needle))
    }

    #[derive(Debug)]
    struct TestSurface {
        marker: u32,
        drops: Arc<AtomicUsize>,
    }

    impl Surface for TestSurface {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn size(&self) -> (u32, u32) {
            (self.marker, self.marker)
        }
    }

    impl Drop for TestSurface {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn lease(token: SurfaceLivenessToken, marker: u32, drops: &Arc<AtomicUsize>) -> SurfaceLease {
        let surface: SurfaceRef = Arc::new(TestSurface {
            marker,
            drops: Arc::clone(drops),
        });
        SurfaceLease::new(surface, token)
    }

    #[test]
    fn preflight_classifies_initial_same_and_new_generations() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let same = lease(gate.attach_or_update().unwrap(), 2, &drops);
        let mut binding = RenderSurfaceBinding::new();

        assert_eq!(binding.preflight(&first), Ok(RecreateKind::Initial));
        binding.commit(first);
        assert_eq!(binding.preflight(&same), Ok(RecreateKind::SameGeneration));

        gate.retire_current().unwrap();
        let next = lease(gate.attach_or_update().unwrap(), 3, &drops);
        assert_eq!(binding.preflight(&next), Ok(RecreateKind::NewGeneration));
    }

    #[test]
    fn stale_and_conflicting_live_candidates_fail_closed() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let stale = first.clone();
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(first);

        gate.retire_current().unwrap();
        let next = lease(gate.attach_or_update().unwrap(), 2, &drops);
        binding.commit(next);
        assert_eq!(
            binding.preflight(&stale),
            Err(SurfaceBindingError::StaleGeneration)
        );

        let foreign = Arc::new(SurfaceGenerationGate::new());
        foreign.attach_or_update().unwrap();
        foreign.retire_current().unwrap();
        foreign.attach_or_update().unwrap();
        foreign.retire_current().unwrap();
        let foreign_generation_three = lease(foreign.attach_or_update().unwrap(), 3, &drops);
        assert_eq!(
            binding.preflight(&foreign_generation_three),
            Err(SurfaceBindingError::ConflictingLiveGeneration)
        );
    }

    #[test]
    fn tagged_destroy_only_acknowledges_the_matching_retired_generation() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let old_generation = first.generation();
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(first);

        gate.retire_current().unwrap();
        let next = lease(gate.attach_or_update().unwrap(), 2, &drops);
        let next_generation = next.generation();
        binding.commit(next);

        assert!(!binding.on_surface_destroyed(old_generation));
        assert_eq!(binding.generation(), Some(next_generation));
        assert!(!binding.on_surface_destroyed(next_generation));

        assert_eq!(gate.retire_current(), Some(next_generation));
        assert!(binding.on_surface_destroyed(next_generation));
        assert!(!binding.is_live());
    }

    #[test]
    fn retirement_during_recreate_commits_one_stale_resource_binding() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let candidate = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let mut binding = RenderSurfaceBinding::new();

        assert_eq!(binding.preflight(&candidate), Ok(RecreateKind::Initial));
        gate.retire_current().unwrap();
        binding.commit(candidate);

        assert!(!binding.is_live());
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        binding.clear_after_egl_teardown();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn replacement_and_teardown_keep_exactly_one_bounded_resource() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(first);

        gate.retire_current().unwrap();
        let next = lease(gate.attach_or_update().unwrap(), 2, &drops);
        binding.commit(next);
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        binding.clear_after_egl_teardown();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn dropping_without_egl_release_proof_quarantines_native_ownership() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let lease = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(lease);

        drop(binding);

        assert_eq!(
            drops.load(Ordering::Relaxed),
            0,
            "an unproven EGL teardown must never return the native resource"
        );
    }

    #[test]
    fn recreate_transaction_stages_before_commit_and_drops_previous_once() {
        let first_drops = Arc::new(AtomicUsize::new(0));
        let next_drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &first_drops);
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(first);

        gate.retire_current().unwrap();
        let next = lease(gate.attach_or_update().unwrap(), 2, &next_drops);
        let next_generation = next.generation();
        let transaction = binding.begin_recreate(next).unwrap();

        assert_eq!(transaction.kind(), RecreateKind::NewGeneration);
        assert_eq!(first_drops.load(Ordering::Relaxed), 0);
        assert_eq!(next_drops.load(Ordering::Relaxed), 0);
        transaction.commit();

        assert_eq!(binding.generation(), Some(next_generation));
        assert_eq!(first_drops.load(Ordering::Relaxed), 1);
        assert_eq!(next_drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn unbound_abort_releases_only_the_candidate() {
        let first_drops = Arc::new(AtomicUsize::new(0));
        let next_drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &first_drops);
        let first_generation = first.generation();
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(first);

        gate.retire_current().unwrap();
        let next = lease(gate.attach_or_update().unwrap(), 2, &next_drops);
        binding.begin_recreate(next).unwrap().abort_unbound();

        assert_eq!(binding.generation(), Some(first_generation));
        assert_eq!(first_drops.load(Ordering::Relaxed), 0);
        assert_eq!(next_drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn panic_keeps_pending_candidate_until_egl_teardown() {
        let first_drops = Arc::new(AtomicUsize::new(0));
        let next_drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &first_drops);
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(first);

        gate.retire_current().unwrap();
        let next = lease(gate.attach_or_update().unwrap(), 2, &next_drops);
        let next_generation = next.generation();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _transaction = binding.begin_recreate(next).unwrap();
            panic!("simulated EGL panic");
        }));

        assert!(unwind.is_err());
        assert_eq!(binding.pending_generation(), Some(next_generation));
        assert_eq!(first_drops.load(Ordering::Relaxed), 0);
        assert_eq!(next_drops.load(Ordering::Relaxed), 0);

        binding.clear_after_egl_teardown();
        assert_eq!(first_drops.load(Ordering::Relaxed), 1);
        assert_eq!(next_drops.load(Ordering::Relaxed), 1);
    }

    /// **A release arriving while a recreate is staged must be refused, and the
    /// state that makes that reachable is the one the test above establishes.**
    /// `unsettled_transaction_blocks_a_second_recreate` covers the other half —
    /// a second *recreate* — but nothing covered the release path, so
    /// `release_surface_binding`'s first guard had no test.
    ///
    /// It is not an unreachable defensive check. Both entry points are driven
    /// from the render thread and so cannot interleave on their own, but a panic
    /// during install leaves the candidate in `pending` *deliberately*, to keep
    /// the native resource alive until CanvasManager has torn EGL down. In that
    /// quarantined state a render-control release is exactly what can arrive
    /// next, and without the guard it would run `release_retired_resource`
    /// against `current` — tearing EGL down and clearing the slot — while a
    /// staged candidate is still waiting to be committed into it. Two ownership
    /// transactions on one binding, and the thing that loses is the native
    /// window.
    ///
    /// So the assertion is not merely that it errors: it is that the refusal
    /// leaves *both* leases exactly where the quarantine put them, with nothing
    /// dropped, so the eventual `clear_after_egl_teardown` still owns both.
    #[test]
    fn a_release_during_a_quarantined_recreate_is_refused_and_changes_nothing() {
        let first_drops = Arc::new(AtomicUsize::new(0));
        let next_drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &first_drops);
        let first_generation = first.generation();
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(first);

        gate.retire_current().unwrap();
        let next = lease(gate.attach_or_update().unwrap(), 2, &next_drops);
        let next_generation = next.generation();

        // Reach the quarantine the same way production does: panic mid-install.
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _transaction = binding.begin_recreate(next).unwrap();
            panic!("simulated EGL panic");
        }));
        assert!(unwind.is_err());
        assert_eq!(binding.pending_generation(), Some(next_generation));

        // A render-control release for the retired generation now arrives.
        let mut teardown_ran = false;
        let error = release_surface_binding(&mut binding, first_generation, || {
            teardown_ran = true;
            Ok(())
        })
        .expect_err("a release must not interleave with a staged recreate");

        assert_eq!(error.code, shared::error::ErrorCode::InvalidOperation);
        assert!(
            !teardown_ran,
            "EGL teardown ran for a binding whose recreate had not settled"
        );
        // Nothing moved: both leases are still owned by the binding.
        assert_eq!(binding.generation(), Some(first_generation));
        assert_eq!(binding.pending_generation(), Some(next_generation));
        assert_eq!(first_drops.load(Ordering::Relaxed), 0);
        assert_eq!(next_drops.load(Ordering::Relaxed), 0);

        // And the quarantine still resolves normally afterwards, releasing both.
        binding.clear_after_egl_teardown();
        assert_eq!(first_drops.load(Ordering::Relaxed), 1);
        assert_eq!(next_drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unsettled_transaction_blocks_a_second_recreate() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let mut binding = RenderSurfaceBinding::new();

        drop(binding.begin_recreate(first).unwrap());
        let same = lease(gate.attach_or_update().unwrap(), 2, &drops);
        assert!(matches!(
            binding.begin_recreate(same),
            Err(SurfaceBindingError::RecreateInProgress)
        ));

        binding.clear_after_egl_teardown();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn install_failure_before_update_invalidation_preserves_previous_surface() {
        assert_eq!(
            install_failure_disposition(
                true,
                InstallPhase::BeforePreviousInvalidation,
                CandidateCleanup::NotRequired,
            ),
            (
                PresentationDisposition::PreviousUsable,
                CandidateLeaseDisposition::Unbound,
            )
        );
    }

    #[test]
    fn initial_install_failure_never_claims_a_previous_usable_surface() {
        assert_eq!(
            install_failure_disposition(
                false,
                InstallPhase::BeforePreviousInvalidation,
                CandidateCleanup::NotRequired,
            ),
            (
                PresentationDisposition::Unavailable,
                CandidateLeaseDisposition::Unbound,
            )
        );
    }

    #[test]
    fn failure_after_previous_invalidation_is_unavailable_and_unbound() {
        assert_eq!(
            install_failure_disposition(
                true,
                InstallPhase::PreviousInvalidated,
                CandidateCleanup::NotRequired,
            ),
            (
                PresentationDisposition::Unavailable,
                CandidateLeaseDisposition::Unbound,
            )
        );
    }

    #[test]
    fn candidate_cleanup_result_controls_whether_lease_must_be_retained() {
        assert_eq!(
            install_failure_disposition(
                true,
                InstallPhase::CandidateReferenced,
                CandidateCleanup::Released,
            ),
            (
                PresentationDisposition::Unavailable,
                CandidateLeaseDisposition::Unbound,
            )
        );
        assert_eq!(
            install_failure_disposition(
                true,
                InstallPhase::CandidateReferenced,
                CandidateCleanup::Failed,
            ),
            (
                PresentationDisposition::Unavailable,
                CandidateLeaseDisposition::Retained,
            )
        );
    }

    #[test]
    fn shared_recreate_runner_commits_initial_and_update_success() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let initial = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let mut binding = RenderSurfaceBinding::new();

        let (kind, value) = run_surface_recreate(&mut binding, initial, |kind, candidate| {
            assert_eq!(kind, RecreateKind::Initial);
            assert_eq!(candidate.generation().get(), 1);
            Ok(7u32)
        })
        .unwrap();
        assert_eq!((kind, value), (RecreateKind::Initial, 7));

        let same = lease(gate.attach_or_update().unwrap(), 2, &drops);
        let (kind, ()) = run_surface_recreate(&mut binding, same, |kind, candidate| {
            assert_eq!(kind, RecreateKind::SameGeneration);
            assert_eq!(candidate.generation().get(), 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(kind, RecreateKind::SameGeneration);
    }

    #[test]
    fn shared_recreate_runner_settles_failure_from_explicit_disposition() {
        let first_drops = Arc::new(AtomicUsize::new(0));
        let candidate_drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &first_drops);
        let first_generation = first.generation();
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(first);

        gate.retire_current().unwrap();
        let candidate = lease(gate.attach_or_update().unwrap(), 2, &candidate_drops);
        let candidate_generation = candidate.generation();
        let error = run_surface_recreate(&mut binding, candidate, |_kind, _candidate| {
            Err::<(), _>(SurfaceInstallFailure::new(
                shared::error::EngineError::new(shared::error::ErrorCode::RenderBackendError),
                PresentationDisposition::Unavailable,
                CandidateLeaseDisposition::Retained,
            ))
        })
        .unwrap_err();
        assert!(matches!(error, SurfaceRecreateError::Install(_)));
        assert_eq!(binding.generation(), Some(candidate_generation));
        assert_eq!(first_drops.load(Ordering::Relaxed), 1);
        assert_eq!(candidate_drops.load(Ordering::Relaxed), 0);

        gate.retire_current().unwrap();
        let unbound = lease(gate.attach_or_update().unwrap(), 3, &candidate_drops);
        let error = run_surface_recreate(&mut binding, unbound, |_kind, _candidate| {
            Err::<(), _>(SurfaceInstallFailure::new(
                shared::error::EngineError::new(shared::error::ErrorCode::RenderBackendError),
                PresentationDisposition::Unavailable,
                CandidateLeaseDisposition::Unbound,
            ))
        })
        .unwrap_err();
        assert!(matches!(error, SurfaceRecreateError::Install(_)));
        assert_eq!(binding.generation(), Some(candidate_generation));
        assert_ne!(binding.generation(), Some(first_generation));
        assert_eq!(candidate_drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn startup_and_updates_use_the_same_surface_install_transaction() {
        assert_eq!(
            RENDER_THREAD.matches("install_surface_lease(").count(),
            3,
            "one helper definition plus exactly the startup and update call sites are required"
        );
        let helper = function_body(RENDER_THREAD, "fn install_surface_lease");
        assert!(helper.contains("run_surface_recreate("));
        assert!(helper.contains("cm.create_onscreen("));
    }

    #[test]
    fn surface_install_preflights_before_platform_preparation_and_revalidates_when_staged() {
        let helper = function_body(RENDER_THREAD, "fn install_surface_lease");
        let preflight = helper
            .find(".preflight(&lease)")
            .expect("generation preflight must be explicit before platform preparation");
        let prepare = helper
            .find(".prepare_surface_for_lease(")
            .expect("platform preparation must remain on the attach cold path");
        let transaction = helper
            .find("run_surface_recreate(")
            .expect("the common staged ownership transaction must be used");

        assert!(
            preflight < prepare,
            "stale generations must fail before platform conversion"
        );
        assert!(
            prepare < transaction,
            "pure preparation must finish before staging"
        );
        assert!(
            helper[transaction..].contains(".validate_prepared("),
            "the prepared backend identity must be checked again inside the staged transaction"
        );
    }

    #[test]
    fn context_recovery_waits_for_a_live_fully_installed_surface_binding() {
        let loop_source = &RENDER_THREAD[RENDER_THREAD
            .find("// --- Deferred EGL context recovery ---")
            .expect("deferred recovery block must exist")..];
        let recovery = &loop_source[..loop_source
            .find("match wait.next(")
            .expect("recovery gate must precede the render thread's wait point")];

        assert!(recovery.contains("render_binding.is_live()"));
        assert!(recovery.contains("render_binding.pending_generation().is_none()"));
        assert!(recovery.contains("cm.is_surface_recovery_ready()"));
        assert!(
            recovery.find("is_surface_recovery_ready").unwrap()
                < recovery.find("cm.try_recover_context()").unwrap(),
            "recovery readiness must be established before any EGL rebuild"
        );
    }

    #[test]
    fn canvas_manager_drop_delegates_to_idempotent_full_teardown() {
        let teardown = function_body(CANVAS_MANAGER, "pub(crate) fn destroy_all");
        assert!(teardown.contains("if self.teardown_complete"));
        assert!(teardown.contains("drop(self.upload_thread.take())"));
        assert!(teardown.contains("self.egl.shutdown()"));
        assert!(
            teardown.contains("windows_explicitly_released || display_terminated"),
            "native ownership needs an explicit-surface or display-termination proof"
        );
        assert!(teardown.contains("self.native_release_confirmed"));
        assert!(teardown.contains("self.quarantine_prepared_native_targets()"));
        assert!(
            teardown.find("self.egl.shutdown()").unwrap()
                < teardown.find("self.installed_surface = None").unwrap(),
            "the native target must remain retained through final EGL display teardown"
        );

        let drop_body = function_body(CANVAS_MANAGER, "fn drop(&mut self)");
        assert!(drop_body.contains("catch_unwind"));
        assert!(drop_body.contains("self.destroy_all()"));
        assert!(drop_body.contains("!self.native_release_confirmed"));
        assert!(drop_body.contains("self.quarantine_prepared_native_targets()"));
    }

    #[test]
    fn render_owner_releases_native_lease_only_with_teardown_proof() {
        let teardown = function_body(RENDER_THREAD, "fn destroy_render_owner");
        let pool = teardown
            .find("autorelease_scope()")
            .expect("EGL teardown must drain its autoreleased objects before any lease drops");
        let proof = teardown
            .find("cm.destroy_all()")
            .expect("CanvasManager must supply the native-release proof");
        assert!(pool < proof);
        let proof = teardown
            .find("if destroyed")
            .expect("the release must branch on the teardown's proof");
        let release = teardown
            .find("render_binding.clear_after_egl_teardown()")
            .expect("the proven branch must release the render lease");
        let quarantine = teardown
            .find("render_binding.quarantine_after_failed_egl_teardown()")
            .expect("the unproven branch must quarantine the render lease");

        assert!(proof < release && release < quarantine);
    }

    #[test]
    fn onscreen_detach_checks_egl_destroy_before_releasing_native_ownership() {
        let detach = function_body(CANVAS_MANAGER, "fn destroy_onscreen_internal");
        assert!(detach.contains("if let Err(error) = self.egl.destroy_surface"));
        assert!(detach.contains("self.canvases.insert(id, entry)"));
        assert!(detach.contains("eglDestroySurface(onscreen) failed"));
    }

    #[test]
    fn live_egl_fast_paths_keep_the_installed_platform_target() {
        let create = function_body(CANVAS_MANAGER, "pub(crate) fn create_onscreen");
        let skip_start = create
            .find("InstallPolicy::Skip")
            .expect("skip policy must exist");
        let resize_start = create
            .find("InstallPolicy::FastResize")
            .expect("fast-resize policy must exist");
        let full_start = create
            .find("Validate the client API")
            .expect("full recreation must follow the fast paths");
        let skip = &create[skip_start..resize_start];
        let resize = &create[resize_start..full_start];

        assert!(
            !contains_ignoring_whitespace(skip, "self.installed_surface = Some(target)"),
            "Skip must discard the candidate while EGL retains the installed target"
        );
        assert!(
            contains_ignoring_whitespace(resize, "installed.reconfigure_from(target.as_ref())"),
            "FastResize must reconfigure the installed target in place"
        );
        assert!(
            !contains_ignoring_whitespace(resize, "self.installed_surface = Some(target)"),
            "FastResize must not replace a target still referenced by EGL"
        );
    }

    #[test]
    fn matching_retired_binding_releases_only_after_egl_teardown_succeeds() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let lease = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let generation = lease.generation();
        let pending = Arc::new(AtomicUsize::new(0));
        let release = lease
            .prepare_release(Arc::clone(&pending), None)
            .unwrap()
            .commit();
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(lease);
        gate.retire_current().unwrap();

        let disposition = release_surface_binding(&mut binding, generation, || {
            assert_eq!(release.phase(), SurfaceReleasePhase::Pending);
            assert_eq!(drops.load(Ordering::Acquire), 0);
            Ok(())
        })
        .unwrap();

        assert_eq!(
            disposition,
            shared::surface::SurfaceReleaseDisposition::Released
        );
        assert_eq!(binding.generation(), None);
        assert_eq!(release.phase(), SurfaceReleasePhase::Released);
        assert_eq!(pending.load(Ordering::Acquire), 0);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn failed_egl_teardown_retains_binding_and_release_stays_pending() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let lease = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let generation = lease.generation();
        let pending = Arc::new(AtomicUsize::new(0));
        let release = lease
            .prepare_release(Arc::clone(&pending), None)
            .unwrap()
            .commit();
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(lease);
        gate.retire_current().unwrap();

        let error = release_surface_binding(&mut binding, generation, || {
            Err(shared::error::EngineError::new(
                shared::error::ErrorCode::RenderBackendError,
            ))
        })
        .unwrap_err();

        assert_eq!(error.code, shared::error::ErrorCode::RenderBackendError);
        assert_eq!(binding.generation(), Some(generation));
        assert_eq!(release.phase(), SurfaceReleasePhase::Pending);
        assert_eq!(pending.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 0);

        binding.clear_after_egl_teardown();
        assert_eq!(release.phase(), SurfaceReleasePhase::Released);
    }

    #[test]
    fn stale_release_never_tears_down_a_newer_live_binding() {
        let drops = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = lease(gate.attach_or_update().unwrap(), 1, &drops);
        let first_generation = first.generation();
        gate.retire_current().unwrap();
        let second = lease(gate.attach_or_update().unwrap(), 2, &drops);
        let second_generation = second.generation();
        let mut binding = RenderSurfaceBinding::new();
        binding.commit(second);

        let disposition = release_surface_binding(&mut binding, first_generation, || {
            panic!("a stale release must not touch a newer EGLSurface")
        })
        .unwrap();

        assert_eq!(
            disposition,
            shared::surface::SurfaceReleaseDisposition::Superseded
        );
        assert_eq!(binding.generation(), Some(second_generation));
        assert!(binding.is_live());
        drop(first);
    }

    #[test]
    fn release_without_an_installed_binding_is_idempotent() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let generation = gate.attach_or_update().unwrap().generation();
        gate.retire_current().unwrap();
        let mut binding = RenderSurfaceBinding::new();

        let disposition = release_surface_binding(&mut binding, generation, || {
            panic!("no EGL teardown exists when no binding is installed")
        })
        .unwrap();

        assert_eq!(
            disposition,
            shared::surface::SurfaceReleaseDisposition::AlreadyAbsent
        );
    }

    #[test]
    fn preserved_context_and_drawing_buffer_move_together_before_first_make_current() {
        let create = function_body(CANVAS_MANAGER, "pub(crate) fn create_onscreen");
        let take_context = create
            .find("self.preserved_ctx.take()")
            .expect("resume must take the preserved context");
        let take_buffer = create
            .find("self.preserved_drawing_buffer.take()")
            .expect("resume must transfer the paired DrawingBuffer");
        let first_make_current = create
            .find("self.make_current_needed(id)")
            .expect("candidate context must be made current before use");

        assert!(
            take_context < take_buffer && take_buffer < first_make_current,
            "the preserved context and DrawingBuffer must enter candidate ownership together before the first fallible make-current"
        );
        assert!(
            create.contains("drawing_buffer: pending.drawing_buffer"),
            "the staged DrawingBuffer must move with its context into CanvasEntry"
        );

        let cleanup = function_body(CANVAS_MANAGER, "fn cleanup_pending_onscreen");
        assert!(
            cleanup.contains("self.preserved_drawing_buffer = pending.drawing_buffer.take()"),
            "partial cleanup must restore the staged context/DB pair for retry"
        );
    }
}
