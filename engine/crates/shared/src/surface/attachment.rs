use std::{
    error::Error,
    fmt,
    num::NonZeroU64,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;

use super::SurfaceRef;

const LIVE_BIT: u64 = 1;
const MAX_GENERATION: u64 = (u64::MAX - LIVE_BIT) >> 1;

/// A non-zero, monotonically increasing Surface attachment generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceGeneration(NonZeroU64);

impl SurfaceGeneration {
    /// Returns the numeric generation value.
    #[inline]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    #[inline]
    fn from_live_word(word: u64) -> Self {
        let generation = word >> 1;
        Self(NonZeroU64::new(generation).expect("a live Surface word has a non-zero generation"))
    }

    #[inline]
    pub(super) fn from_non_zero_value(value: u64) -> Self {
        Self(NonZeroU64::new(value).expect("Surface generations are always non-zero"))
    }
}

/// A non-zero Surface generation supplied by an embedding host.
///
/// This is intentionally distinct from [`SurfaceGeneration`], which is the
/// engine's private presentation epoch.  Keeping the types separate prevents
/// callbacks and release observers from accidentally exposing an internal
/// epoch in place of the host's correlation token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicSurfaceGeneration(NonZeroU64);

impl PublicSurfaceGeneration {
    /// Constructs a public generation, rejecting the reserved zero value.
    #[inline]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the host-supplied numeric value.
    #[inline]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    #[inline]
    const fn from_internal(generation: SurfaceGeneration) -> Self {
        Self(generation.0)
    }
}

/// Failure to issue a fresh Surface generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceGenerationError {
    /// All representable generations have been consumed.
    Exhausted,
}

impl fmt::Display for SurfaceGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted => formatter.write_str("Surface generation space exhausted"),
        }
    }
}

impl Error for SurfaceGenerationError {}

/// Issues and retires Surface attachment generations without a frame-path lock.
///
/// The packed state is `generation << 1 | live_bit`. Generation zero is the
/// never-attached state and is never issued to callers.
#[derive(Debug)]
pub struct SurfaceGenerationGate {
    state: AtomicU64,
}

impl SurfaceGenerationGate {
    /// Creates a gate in the never-attached state.
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    /// Returns a token for the current live generation, issuing the next
    /// generation when the previous one is retired.
    pub fn attach_or_update(
        self: &Arc<Self>,
    ) -> Result<SurfaceLivenessToken, SurfaceGenerationError> {
        let mut current = self.state.load(Ordering::Acquire);

        loop {
            if current & LIVE_BIT != 0 {
                return Ok(SurfaceLivenessToken::new(Arc::clone(self), current));
            }

            let previous_generation = current >> 1;
            if previous_generation >= MAX_GENERATION {
                return Err(SurfaceGenerationError::Exhausted);
            }

            let next_live_word = ((previous_generation + 1) << 1) | LIVE_BIT;
            match self.state.compare_exchange_weak(
                current,
                next_live_word,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(SurfaceLivenessToken::new(Arc::clone(self), next_live_word));
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// Retires the current live generation.
    ///
    /// Returns `None` when the gate is already detached or has never attached.
    pub fn retire_current(&self) -> Option<SurfaceGeneration> {
        let mut current = self.state.load(Ordering::Acquire);

        loop {
            if current & LIVE_BIT == 0 {
                return None;
            }

            let generation = SurfaceGeneration::from_live_word(current);
            let retired_word = current & !LIVE_BIT;
            match self.state.compare_exchange_weak(
                current,
                retired_word,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(generation),
                Err(observed) => current = observed,
            }
        }
    }

    /// Retire only `expected`, leaving a concurrently attached newer
    /// generation untouched.
    pub(crate) fn retire_if_current(&self, expected: SurfaceGeneration) -> bool {
        let live_word = (expected.get() << 1) | LIVE_BIT;
        self.state
            .compare_exchange(
                live_word,
                live_word & !LIVE_BIT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[cfg(test)]
    fn from_max_retired_for_test() -> Self {
        Self {
            state: AtomicU64::new(MAX_GENERATION << 1),
        }
    }
}

impl Default for SurfaceGenerationGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Authoritative, level-triggered native-resource release state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SurfaceReleasePhase {
    /// At least one engine lease may still reach the native resource.
    Pending = 0,
    /// Every resource lease has gone and the platform resource was released.
    Released = 1,
}

/// One-shot wakeup posted after the release level has become observable.
///
/// The callback is deliberately an edge only.  It may be dropped or panic;
/// [`SurfaceReleaseObserver::phase`] remains the source of truth.
pub type SurfaceReleaseNotification = Box<dyn FnOnce(PublicSurfaceGeneration) + Send + 'static>;

/// Failure to prepare a transactional Surface release registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceReleaseRegistrationError {
    /// This attachment generation already has a prepared or committed release.
    AlreadyRegistered,
    /// The Session's pending-release counter cannot be incremented safely.
    PendingCountExhausted,
}

impl fmt::Display for SurfaceReleaseRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRegistered => formatter.write_str("Surface release is already registered"),
            Self::PendingCountExhausted => {
                formatter.write_str("Surface pending-release count exhausted")
            }
        }
    }
}

impl Error for SurfaceReleaseRegistrationError {}

struct ReleaseRegistration {
    pending_count: Arc<AtomicUsize>,
    notification: Option<SurfaceReleaseNotification>,
}

struct SurfaceReleaseShared {
    public_generation: PublicSurfaceGeneration,
    phase: AtomicU8,
    registration: Mutex<Option<ReleaseRegistration>>,
}

impl SurfaceReleaseShared {
    fn new(public_generation: PublicSurfaceGeneration) -> Self {
        Self {
            public_generation,
            phase: AtomicU8::new(SurfaceReleasePhase::Pending as u8),
            registration: Mutex::new(None),
        }
    }

    #[inline]
    fn phase(&self) -> SurfaceReleasePhase {
        match self.phase.load(Ordering::Acquire) {
            value if value == SurfaceReleasePhase::Pending as u8 => SurfaceReleasePhase::Pending,
            value if value == SurfaceReleasePhase::Released as u8 => SurfaceReleasePhase::Released,
            _ => unreachable!("Surface release phase is written only by this module"),
        }
    }

    /// Publish completion only after the native anchor has been destroyed.
    fn complete(&self) {
        let notification = {
            let mut registration = self.registration.lock();
            let notification = registration.take().and_then(|mut registration| {
                let previous = registration.pending_count.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "pending-release count underflow");
                registration.notification.take()
            });

            // Publish RELEASED only after the Session guard has been
            // decremented. An Acquire query that sees RELEASED may therefore
            // immediately destroy the Session without a transient false
            // INVALID_STATE result. Holding the registration mutex through the
            // store also closes a second-registration race at final drop.
            self.phase
                .store(SurfaceReleasePhase::Released as u8, Ordering::Release);
            notification
        };

        if let Some(notification) = notification {
            // A notification implementation may bridge into foreign host code.
            // Panicking from Arc's final-drop path could abort during another
            // unwind, while the level state is already authoritative.  Contain
            // it exactly as the public FFI boundary contains host callbacks.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                notification(self.public_generation);
            }));
        }
    }
}

/// A read-only release observer that does not retain the native resource.
#[derive(Clone)]
pub struct SurfaceReleaseObserver {
    shared: Arc<SurfaceReleaseShared>,
}

impl SurfaceReleaseObserver {
    /// Returns the public generation this release describes.
    #[inline]
    pub fn public_generation(&self) -> PublicSurfaceGeneration {
        self.shared.public_generation
    }

    /// Loads the authoritative level state with Acquire ordering.
    #[inline]
    pub fn phase(&self) -> SurfaceReleasePhase {
        self.shared.phase()
    }
}

impl fmt::Debug for SurfaceReleaseObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceReleaseObserver")
            .field("public_generation", &self.public_generation())
            .field("phase", &self.phase())
            .finish()
    }
}

/// Shared ownership of one attachment generation's native resource anchor.
///
/// `native_anchor` is dropped explicitly before RELEASED is published.  Every
/// object capable of platform/GPU use must carry a [`SurfaceResourceLease`], so
/// the final Arc drop is the native-lifetime completion boundary.
struct SurfaceResource {
    native_anchor: Option<SurfaceRef>,
    public_generation: PublicSurfaceGeneration,
    internal_generation: SurfaceGeneration,
    release: Arc<SurfaceReleaseShared>,
}

impl Drop for SurfaceResource {
    fn drop(&mut self) {
        // Rust normally runs a type's Drop implementation before its fields.
        // Taking the anchor makes the required platform-release-before-level-
        // publication ordering explicit rather than relying on field order.
        //
        // The ordering above is only worth anything if this drop is the LAST
        // reference to the anchor. The struct comment states that as the rule --
        // "every object capable of platform/GPU use must carry a
        // `SurfaceResourceLease`, so the final Arc drop is the native-lifetime
        // completion boundary" -- and until now nothing observed it.
        //
        // It is observed rather than asserted, deliberately. `complete()` below
        // already explains why this path does not panic: it can run from an Arc's
        // final drop, where a panic could abort during another unwind. A
        // `debug_assert` here would be exactly that panic.
        //
        // WHY IT IS WORTH OBSERVING. On Apple this rule is measurably not holding:
        // a renderer test that reads the layer's reference count at the moment
        // RELEASED becomes visible finds the host's `CAMetalLayer` still alive,
        // and it goes about 30 ms later on another thread. A consumer that frees
        // its layer on RELEASED -- which the phase's documentation entitles it to
        // do -- would be freeing a layer the renderer can still reach. A count
        // greater than one here names that owner's existence at the moment it
        // matters, in every platform's logs, instead of leaving it to a flaky
        // assertion in one platform's test suite.
        if let Some(anchor) = self.native_anchor.as_ref() {
            let outstanding = Arc::strong_count(anchor);
            if outstanding != 1 {
                tracing::warn!(
                    outstanding,
                    generation = self.public_generation.get(),
                    "surface anchor is not the last reference at release; RELEASED is about to \
                     be published while another owner can still reach the native resource"
                );
            }
        }
        drop(self.native_anchor.take());
        self.release.complete();
    }
}

/// A cloneable proof that a consumer may still reach a native Surface.
#[derive(Clone)]
pub struct SurfaceResourceLease {
    resource: Arc<SurfaceResource>,
}

impl SurfaceResourceLease {
    fn new(
        native_anchor: SurfaceRef,
        public_generation: PublicSurfaceGeneration,
        internal_generation: SurfaceGeneration,
    ) -> Self {
        let release = Arc::new(SurfaceReleaseShared::new(public_generation));
        Self {
            resource: Arc::new(SurfaceResource {
                native_anchor: Some(native_anchor),
                public_generation,
                internal_generation,
                release,
            }),
        }
    }

    /// Returns the host-supplied generation for callbacks and diagnostics.
    #[inline]
    pub fn public_generation(&self) -> PublicSurfaceGeneration {
        self.resource.public_generation
    }

    /// Returns the private engine generation this resource belongs to.
    #[inline]
    pub fn internal_generation(&self) -> SurfaceGeneration {
        self.resource.internal_generation
    }

    /// Prepares release bookkeeping before the irreversible generation
    /// retirement boundary. Dropping the transaction rolls the registration
    /// and pending count back; committing it cannot fail.
    pub fn prepare_release(
        &self,
        pending_count: Arc<AtomicUsize>,
        notification: Option<SurfaceReleaseNotification>,
    ) -> Result<PreparedSurfaceRelease, SurfaceReleaseRegistrationError> {
        let mut registration = self.resource.release.registration.lock();
        if registration.is_some() {
            return Err(SurfaceReleaseRegistrationError::AlreadyRegistered);
        }

        pending_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| SurfaceReleaseRegistrationError::PendingCountExhausted)?;

        *registration = Some(ReleaseRegistration {
            pending_count,
            notification,
        });
        drop(registration);

        Ok(PreparedSurfaceRelease {
            release: Arc::clone(&self.resource.release),
            resource_pin: Some(self.clone()),
            committed: false,
        })
    }
}

impl fmt::Debug for SurfaceResourceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceResourceLease")
            .field("public_generation", &self.public_generation())
            .field("internal_generation", &self.internal_generation())
            .finish_non_exhaustive()
    }
}

/// A preallocated release transaction that can still roll back before the
/// generation retirement CAS.
#[must_use = "commit after retiring the generation, or drop to roll back"]
pub struct PreparedSurfaceRelease {
    release: Arc<SurfaceReleaseShared>,
    resource_pin: Option<SurfaceResourceLease>,
    committed: bool,
}

impl PreparedSurfaceRelease {
    /// Irreversibly commits the registration and returns its non-retaining
    /// observer.  All allocation and counter overflow checks happened in
    /// [`SurfaceResourceLease::prepare_release`].
    pub fn commit(mut self) -> SurfaceReleaseObserver {
        self.committed = true;
        let observer = SurfaceReleaseObserver {
            shared: Arc::clone(&self.release),
        };
        // The caller's attachment lease still pins the resource.  Releasing
        // this transactional pin here lets actual host/render ownership decide
        // when the level becomes RELEASED.
        drop(self.resource_pin.take());
        observer
    }
}

impl Drop for PreparedSurfaceRelease {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        let registration = self.release.registration.lock().take();
        if let Some(registration) = registration {
            let previous = registration.pending_count.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0, "pending-release rollback underflow");
            // Dropping, rather than invoking, the notification is essential:
            // retirement never happened and the attachment remains live.
            drop(registration);
        }
    }
}

/// A cloneable, read-only proof tied to one issued Surface generation.
#[derive(Clone, Debug)]
pub struct SurfaceLivenessToken {
    gate: Arc<SurfaceGenerationGate>,
    generation: SurfaceGeneration,
    expected_live_word: u64,
}

impl SurfaceLivenessToken {
    fn new(gate: Arc<SurfaceGenerationGate>, expected_live_word: u64) -> Self {
        debug_assert_eq!(expected_live_word & LIVE_BIT, LIVE_BIT);
        Self {
            generation: SurfaceGeneration::from_live_word(expected_live_word),
            gate,
            expected_live_word,
        }
    }

    /// Returns the generation represented by this token.
    #[inline]
    pub const fn generation(&self) -> SurfaceGeneration {
        self.generation
    }

    /// Returns whether this exact generation is still the gate's live one.
    #[inline]
    pub fn is_live(&self) -> bool {
        self.gate.state.load(Ordering::Acquire) == self.expected_live_word
    }
}

/// A read-only Surface reference paired with its generation liveness token.
///
/// Clones keep the platform resource alive but cannot retire or reactivate the
/// generation. Ownership transitions remain centralized in the generation
/// gate and the Host attachment slot.
#[derive(Clone)]
pub struct SurfaceLease {
    // Keep the per-update Surface payload ahead of the resource lease so its
    // platform reference drops first when this is the final consumer.
    surface: SurfaceRef,
    liveness: SurfaceLivenessToken,
    resource: SurfaceResourceLease,
}

impl SurfaceLease {
    /// Pairs a platform Surface with a private generation for legacy/internal
    /// hosts that do not expose a separate public generation.
    pub fn new(surface: SurfaceRef, liveness: SurfaceLivenessToken) -> Self {
        let public_generation = PublicSurfaceGeneration::from_internal(liveness.generation());
        Self::new_tracked(surface, liveness, public_generation)
    }

    /// Starts a tracked native-resource lifetime for a new attachment.
    pub fn new_tracked(
        surface: SurfaceRef,
        liveness: SurfaceLivenessToken,
        public_generation: PublicSurfaceGeneration,
    ) -> Self {
        let resource = SurfaceResourceLease::new(
            Arc::clone(&surface),
            public_generation,
            liveness.generation(),
        );
        Self {
            surface,
            liveness,
            resource,
        }
    }

    /// Creates an updated per-metrics Surface payload for the same attachment
    /// resource and internal generation.
    pub fn with_resource(
        surface: SurfaceRef,
        liveness: SurfaceLivenessToken,
        resource: SurfaceResourceLease,
    ) -> Result<Self, SurfaceGenerationMismatch> {
        if liveness.generation() != resource.internal_generation() {
            return Err(SurfaceGenerationMismatch {
                token: liveness.generation(),
                resource: resource.internal_generation(),
            });
        }
        Ok(Self {
            surface,
            liveness,
            resource,
        })
    }

    /// Returns the generation associated with this Surface handoff.
    #[inline]
    pub const fn generation(&self) -> SurfaceGeneration {
        self.liveness.generation()
    }

    /// Returns the host-supplied generation represented by this resource.
    #[inline]
    pub fn public_generation(&self) -> PublicSurfaceGeneration {
        self.resource.public_generation()
    }

    /// Returns whether this lease's exact generation is still live.
    #[inline]
    pub fn is_live(&self) -> bool {
        self.liveness.is_live()
    }

    /// Returns the physical Surface size in pixels.
    #[inline]
    pub fn size(&self) -> (u32, u32) {
        self.surface.size()
    }

    /// Borrows the underlying platform Surface reference.
    #[inline]
    pub fn surface(&self) -> &SurfaceRef {
        &self.surface
    }

    /// Clones the native-resource lifetime proof for a platform object that
    /// may outlive this particular Surface payload.
    #[inline]
    pub fn resource_lease(&self) -> SurfaceResourceLease {
        self.resource.clone()
    }

    /// Prepares release bookkeeping before the irreversible generation
    /// retirement boundary.  Dropping the returned transaction rolls all
    /// bookkeeping back; committing it cannot fail.
    pub fn prepare_release(
        &self,
        pending_count: Arc<AtomicUsize>,
        notification: Option<SurfaceReleaseNotification>,
    ) -> Result<PreparedSurfaceRelease, SurfaceReleaseRegistrationError> {
        self.resource.prepare_release(pending_count, notification)
    }
}

impl fmt::Debug for SurfaceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SurfaceLease")
            .field("generation", &self.generation())
            .field("public_generation", &self.public_generation())
            .field("live", &self.is_live())
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}

/// A candidate liveness token cannot be paired with another attachment's
/// resource even if the native pointer happens to compare equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceGenerationMismatch {
    token: SurfaceGeneration,
    resource: SurfaceGeneration,
}

impl fmt::Display for SurfaceGenerationMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Surface token generation {} does not match resource generation {}",
            self.token.get(),
            self.resource.get()
        )
    }
}

impl Error for SurfaceGenerationMismatch {}

/// Result of applying a generation-tagged teardown request to an installed
/// render binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceReleaseDisposition {
    /// The matching (or older retired) binding was torn down and dropped.
    Released,
    /// No render binding remained installed.
    AlreadyAbsent,
    /// A newer binding is installed and was deliberately left untouched.
    Superseded,
}

/// Failure of the teardown-before-drop ownership transaction.
#[derive(Debug, PartialEq, Eq)]
pub enum SurfaceReleaseTransactionError<E> {
    /// Control attempted to release a generation that is still presentation
    /// live, indicating an ordering/programming error before retirement.
    GenerationStillLive,
    /// Platform/EGL teardown failed; the installed ownership is retained.
    Teardown(E),
}

impl<E: fmt::Display> fmt::Display for SurfaceReleaseTransactionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenerationStillLive => {
                formatter.write_str("cannot release a live Surface generation")
            }
            Self::Teardown(error) => write!(formatter, "Surface teardown failed: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for SurfaceReleaseTransactionError<E> {}

/// Runs a platform teardown before dropping a generation-tagged binding.
///
/// This pure ownership transaction is shared by the render implementation and
/// host-runnable tests.  On any error `current` is bit-for-bit untouched; only
/// successful teardown may release its final [`SurfaceResourceLease`].
pub fn release_retired_resource<T, E>(
    current: &mut Option<T>,
    requested_generation: SurfaceGeneration,
    generation: impl FnOnce(&T) -> SurfaceGeneration,
    is_live: impl FnOnce(&T) -> bool,
    teardown: impl FnOnce() -> Result<(), E>,
) -> Result<SurfaceReleaseDisposition, SurfaceReleaseTransactionError<E>> {
    let Some(installed) = current.as_ref() else {
        return Ok(SurfaceReleaseDisposition::AlreadyAbsent);
    };
    let installed_generation = generation(installed);
    if installed_generation > requested_generation {
        return Ok(SurfaceReleaseDisposition::Superseded);
    }
    if is_live(installed) {
        return Err(SurfaceReleaseTransactionError::GenerationStillLive);
    }

    teardown().map_err(SurfaceReleaseTransactionError::Teardown)?;
    drop(current.take());
    Ok(SurfaceReleaseDisposition::Released)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use super::{
        PublicSurfaceGeneration, SurfaceGenerationError, SurfaceGenerationGate, SurfaceLease,
        SurfaceReleaseDisposition, SurfaceReleasePhase, SurfaceReleaseTransactionError,
        release_retired_resource,
    };
    use crate::surface::{Surface, SurfaceRef};

    #[derive(Debug)]
    struct TestSurface {
        size: (u32, u32),
        drops: Arc<AtomicUsize>,
    }

    impl TestSurface {
        fn new(size: (u32, u32), drops: Arc<AtomicUsize>) -> Self {
            Self { size, drops }
        }
    }

    impl Surface for TestSurface {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn size(&self) -> (u32, u32) {
            self.size
        }
    }

    impl Drop for TestSurface {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn lease(
        token: super::SurfaceLivenessToken,
        marker: u32,
        drops: &Arc<AtomicUsize>,
    ) -> SurfaceLease {
        let surface: SurfaceRef = Arc::new(TestSurface::new((marker, marker), Arc::clone(drops)));
        SurfaceLease::new(surface, token)
    }

    /// The completion boundary is only a boundary when this drop is the last
    /// reference, and this checks the observation that says so.
    ///
    /// Two `SurfaceResource`s are built over the same anchor: one where the anchor
    /// is uniquely owned, and one where a clone is deliberately held. Only the
    /// second is a violation, and the difference between them is the whole content
    /// of the check -- a check that could not tell them apart would be reporting
    /// its own presence.
    ///
    /// It reads the count the same way the drop does rather than capturing log
    /// output: the point is that the condition discriminates, and a tracing
    /// subscriber in a unit test would be testing the subscriber.
    #[test]
    fn the_completion_boundary_notices_an_anchor_it_does_not_uniquely_own() {
        let drops = Arc::new(AtomicUsize::new(0));
        let anchor: SurfaceRef = Arc::new(TestSurface::new((1, 1), Arc::clone(&drops)));

        assert_eq!(
            Arc::strong_count(&anchor),
            1,
            "a freshly built anchor is uniquely owned, which is the case the check must NOT report"
        );

        let onlooker = Arc::clone(&anchor);
        assert_eq!(
            Arc::strong_count(&anchor),
            2,
            "an owner outside the attachment graph is exactly what the check reports, and it is \
             what the Apple renderer measured: the host CAMetalLayer still reachable at the moment \
             RELEASED became visible"
        );

        drop(onlooker);
        assert_eq!(Arc::strong_count(&anchor), 1);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "dropping a clone must not destroy the surface; only the last reference does"
        );
        drop(anchor);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "the final drop is the native-lifetime completion boundary the struct documents"
        );
    }

    #[test]
    fn initial_attach_issues_first_live_generation() {
        let gate = Arc::new(SurfaceGenerationGate::new());

        let token = gate.attach_or_update().unwrap();

        assert_eq!(token.generation().get(), 1);
        assert!(token.is_live());
    }

    #[test]
    fn repeated_update_reuses_the_live_generation() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = gate.attach_or_update().unwrap();

        let second = gate.attach_or_update().unwrap();

        assert_eq!(second.generation(), first.generation());
        assert!(first.is_live());
        assert!(second.is_live());
    }

    #[test]
    fn retirement_invalidates_all_tokens_for_the_current_generation() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = gate.attach_or_update().unwrap();
        let retry = gate.attach_or_update().unwrap();

        assert_eq!(gate.retire_current(), Some(first.generation()));

        assert!(!first.is_live());
        assert!(!retry.is_live());
    }

    #[test]
    fn duplicate_retirement_is_idempotent() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = gate.attach_or_update().unwrap();

        assert_eq!(gate.retire_current(), Some(first.generation()));
        assert_eq!(gate.retire_current(), None);
    }

    #[test]
    fn retired_generation_never_becomes_live_again() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = gate.attach_or_update().unwrap();
        assert_eq!(first.generation().get(), 1);
        assert!(first.is_live());

        assert_eq!(gate.retire_current(), Some(first.generation()));
        assert!(!first.is_live());

        let second = gate.attach_or_update().unwrap();
        assert_eq!(second.generation().get(), 2);
        assert!(second.is_live());
        assert!(!first.is_live());
    }

    #[test]
    fn exhausted_generation_fails_closed_without_changing_state() {
        let gate = Arc::new(SurfaceGenerationGate::from_max_retired_for_test());

        assert!(matches!(
            gate.attach_or_update(),
            Err(SurfaceGenerationError::Exhausted)
        ));
        assert_eq!(gate.retire_current(), None);
        assert!(matches!(
            gate.attach_or_update(),
            Err(SurfaceGenerationError::Exhausted)
        ));
    }

    #[test]
    fn lease_clones_share_generation_size_and_liveness() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let token = gate.attach_or_update().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let surface: SurfaceRef = Arc::new(TestSurface::new((1920, 1080), drops.clone()));
        let lease = SurfaceLease::new(surface, token);

        let retry = lease.clone();

        assert_eq!(lease.generation(), retry.generation());
        assert_eq!(lease.size(), (1920, 1080));
        assert_eq!(retry.size(), (1920, 1080));
        assert!(lease.is_live());
        assert!(retry.is_live());
    }

    #[test]
    fn retired_lease_cannot_be_revived_by_a_new_generation() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let token = gate.attach_or_update().unwrap();
        let surface: SurfaceRef = Arc::new(TestSurface::new((1, 1), Arc::new(AtomicUsize::new(0))));
        let lease = SurfaceLease::new(surface, token);

        assert_eq!(gate.retire_current(), Some(lease.generation()));
        assert!(!lease.is_live());

        let replacement = gate.attach_or_update().unwrap();
        assert!(replacement.is_live());
        assert_ne!(replacement.generation(), lease.generation());
        assert!(!lease.is_live());
    }

    #[test]
    fn lease_clones_hold_the_surface_until_the_final_drop() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let token = gate.attach_or_update().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let surface: SurfaceRef = Arc::new(TestSurface::new((1, 1), drops.clone()));
        let lease = SurfaceLease::new(surface, token);
        let retry = lease.clone();

        drop(lease);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(retry);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn stale_token_never_revives_during_concurrent_generation_churn() {
        const READER_COUNT: usize = 4;
        const GENERATION_COUNT: u64 = 10_000;

        let gate = Arc::new(SurfaceGenerationGate::new());
        let stale = gate.attach_or_update().unwrap();
        assert_eq!(gate.retire_current(), Some(stale.generation()));
        assert!(!stale.is_live());

        let running = Arc::new(AtomicBool::new(true));
        let readers: Vec<_> = (0..READER_COUNT)
            .map(|_| {
                let stale = stale.clone();
                let running = running.clone();
                std::thread::spawn(move || {
                    while running.load(Ordering::Acquire) {
                        assert!(!stale.is_live());
                        std::hint::spin_loop();
                    }
                    assert!(!stale.is_live());
                })
            })
            .collect();

        for expected in 2..=GENERATION_COUNT {
            let current = gate.attach_or_update().unwrap();
            assert_eq!(current.generation().get(), expected);
            assert!(current.is_live());
            assert_eq!(gate.retire_current(), Some(current.generation()));
            assert!(!current.is_live());
        }

        running.store(false, Ordering::Release);
        for reader in readers {
            reader.join().unwrap();
        }
        assert!(!stale.is_live());
    }

    #[test]
    fn public_generation_is_non_zero() {
        assert!(PublicSurfaceGeneration::new(0).is_none());
        assert_eq!(PublicSurfaceGeneration::new(41).unwrap().get(), 41);
    }

    #[test]
    fn release_observer_does_not_keep_the_resource_alive() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let token = gate.attach_or_update().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let surface: SurfaceRef = Arc::new(TestSurface::new((640, 480), drops.clone()));
        let lease =
            SurfaceLease::new_tracked(surface, token, PublicSurfaceGeneration::new(7).unwrap());
        let pending = Arc::new(AtomicUsize::new(0));

        let release = lease
            .prepare_release(Arc::clone(&pending), None)
            .unwrap()
            .commit();
        assert_eq!(release.public_generation().get(), 7);
        assert_eq!(release.phase(), SurfaceReleasePhase::Pending);
        assert_eq!(pending.load(Ordering::Acquire), 1);

        drop(lease);

        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(release.phase(), SurfaceReleasePhase::Released);
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn every_resource_lease_must_drop_before_release() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let token = gate.attach_or_update().unwrap();
        let surface: SurfaceRef =
            Arc::new(TestSurface::new((640, 480), Arc::new(AtomicUsize::new(0))));
        let first =
            SurfaceLease::new_tracked(surface, token, PublicSurfaceGeneration::new(9).unwrap());
        let render = first.clone();
        let pending = Arc::new(AtomicUsize::new(0));
        let release = first
            .prepare_release(Arc::clone(&pending), None)
            .unwrap()
            .commit();

        drop(first);
        assert_eq!(release.phase(), SurfaceReleasePhase::Pending);
        assert_eq!(pending.load(Ordering::Acquire), 1);

        drop(render);
        assert_eq!(release.phase(), SurfaceReleasePhase::Released);
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn release_notification_observes_published_level_state_and_fires_once() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let token = gate.attach_or_update().unwrap();
        let surface: SurfaceRef =
            Arc::new(TestSurface::new((640, 480), Arc::new(AtomicUsize::new(0))));
        let lease =
            SurfaceLease::new_tracked(surface, token, PublicSurfaceGeneration::new(11).unwrap());
        let second = lease.clone();
        let pending = Arc::new(AtomicUsize::new(0));
        let notifications = Arc::new(AtomicUsize::new(0));
        let notification_count = Arc::clone(&notifications);
        let observed_generation = Arc::new(AtomicUsize::new(0));
        let callback_generation = Arc::clone(&observed_generation);
        let release = lease
            .prepare_release(
                Arc::clone(&pending),
                Some(Box::new(move |generation| {
                    callback_generation.store(generation.get() as usize, Ordering::Release);
                    notification_count.fetch_add(1, Ordering::AcqRel);
                })),
            )
            .unwrap()
            .commit();

        drop(lease);
        assert_eq!(notifications.load(Ordering::Acquire), 0);
        drop(second);

        assert_eq!(release.phase(), SurfaceReleasePhase::Released);
        assert_eq!(notifications.load(Ordering::Acquire), 1);
        assert_eq!(observed_generation.load(Ordering::Acquire), 11);
        drop(release);
        assert_eq!(notifications.load(Ordering::Acquire), 1);
    }

    #[test]
    fn uncommitted_release_registration_rolls_back_without_notification() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let token = gate.attach_or_update().unwrap();
        let surface: SurfaceRef =
            Arc::new(TestSurface::new((640, 480), Arc::new(AtomicUsize::new(0))));
        let lease =
            SurfaceLease::new_tracked(surface, token, PublicSurfaceGeneration::new(13).unwrap());
        let pending = Arc::new(AtomicUsize::new(0));
        let notifications = Arc::new(AtomicUsize::new(0));
        let notification_count = Arc::clone(&notifications);

        let prepared = lease
            .prepare_release(
                Arc::clone(&pending),
                Some(Box::new(move |_| {
                    notification_count.fetch_add(1, Ordering::AcqRel);
                })),
            )
            .unwrap();
        assert_eq!(pending.load(Ordering::Acquire), 1);
        drop(prepared);

        assert_eq!(pending.load(Ordering::Acquire), 0);
        drop(lease);
        assert_eq!(notifications.load(Ordering::Acquire), 0);
    }

    #[test]
    fn concurrent_final_drops_complete_release_once() {
        const CLONES: usize = 16;

        let gate = Arc::new(SurfaceGenerationGate::new());
        let token = gate.attach_or_update().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let surface: SurfaceRef = Arc::new(TestSurface::new((1, 1), Arc::clone(&drops)));
        let lease =
            SurfaceLease::new_tracked(surface, token, PublicSurfaceGeneration::new(17).unwrap());
        let pending = Arc::new(AtomicUsize::new(0));
        let notifications = Arc::new(AtomicUsize::new(0));
        let notification_count = Arc::clone(&notifications);
        let release = lease
            .prepare_release(
                Arc::clone(&pending),
                Some(Box::new(move |_| {
                    notification_count.fetch_add(1, Ordering::AcqRel);
                })),
            )
            .unwrap()
            .commit();
        let mut threads = Vec::with_capacity(CLONES);
        for clone in (0..CLONES).map(|_| lease.clone()) {
            threads.push(std::thread::spawn(move || drop(clone)));
        }
        drop(lease);
        for thread in threads {
            thread.join().unwrap();
        }

        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(pending.load(Ordering::Acquire), 0);
        assert_eq!(notifications.load(Ordering::Acquire), 1);
        assert_eq!(release.phase(), SurfaceReleasePhase::Released);
    }

    #[test]
    fn release_transaction_drops_binding_only_after_teardown_success() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let lease = lease(
            gate.attach_or_update().unwrap(),
            1,
            &Arc::new(AtomicUsize::new(0)),
        );
        let generation = lease.generation();
        let pending = Arc::new(AtomicUsize::new(0));
        let release = lease
            .prepare_release(Arc::clone(&pending), None)
            .unwrap()
            .commit();
        gate.retire_current().unwrap();
        let mut current = Some(lease);
        let teardown_called = AtomicBool::new(false);

        let disposition = release_retired_resource(
            &mut current,
            generation,
            |lease| lease.generation(),
            |lease| lease.is_live(),
            || {
                assert_eq!(release.phase(), SurfaceReleasePhase::Pending);
                teardown_called.store(true, Ordering::Release);
                Ok::<_, ()>(())
            },
        )
        .unwrap();

        assert_eq!(disposition, SurfaceReleaseDisposition::Released);
        assert!(teardown_called.load(Ordering::Acquire));
        assert!(current.is_none());
        assert_eq!(release.phase(), SurfaceReleasePhase::Released);
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn release_transaction_retains_binding_when_teardown_fails() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let lease = lease(
            gate.attach_or_update().unwrap(),
            1,
            &Arc::new(AtomicUsize::new(0)),
        );
        let generation = lease.generation();
        gate.retire_current().unwrap();
        let mut current = Some(lease);

        let result = release_retired_resource(
            &mut current,
            generation,
            |lease| lease.generation(),
            |lease| lease.is_live(),
            || Err("eglDestroySurface failed"),
        );

        assert_eq!(
            result,
            Err(SurfaceReleaseTransactionError::Teardown(
                "eglDestroySurface failed"
            ))
        );
        assert!(current.is_some());
    }

    #[test]
    fn release_transaction_ignores_older_request_for_newer_binding() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let first = gate.attach_or_update().unwrap().generation();
        gate.retire_current().unwrap();
        let current = lease(
            gate.attach_or_update().unwrap(),
            2,
            &Arc::new(AtomicUsize::new(0)),
        );
        let mut current = Some(current);

        let disposition = release_retired_resource(
            &mut current,
            first,
            |lease| lease.generation(),
            |lease| lease.is_live(),
            || -> Result<(), ()> { panic!("an older request cannot tear down the newer binding") },
        )
        .unwrap();

        assert_eq!(disposition, SurfaceReleaseDisposition::Superseded);
        assert!(current.as_ref().unwrap().is_live());
    }

    #[test]
    fn release_transaction_rejects_matching_live_generation() {
        let gate = Arc::new(SurfaceGenerationGate::new());
        let lease = lease(
            gate.attach_or_update().unwrap(),
            1,
            &Arc::new(AtomicUsize::new(0)),
        );
        let generation = lease.generation();
        let mut current = Some(lease);

        let result = release_retired_resource(
            &mut current,
            generation,
            |lease| lease.generation(),
            |lease| lease.is_live(),
            || -> Result<(), ()> {
                panic!("a live generation cannot be torn down by release control")
            },
        );

        assert_eq!(
            result,
            Err(SurfaceReleaseTransactionError::GenerationStillLive)
        );
        assert!(current.unwrap().is_live());
    }
}
