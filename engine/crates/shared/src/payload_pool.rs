//! Fixed-capacity ownership pools for message payloads.
//!
//! Two shapes, and the difference is what a returned slot keeps.
//!
//! [`PayloadPool`] pools the *slot* and drops the value in it. That is right for
//! a payload that owns nothing beyond itself — a touch batch is a fixed array —
//! and its slots are heap-allocated once on the cold construction path.
//!
//! [`RecyclePool`] pools the value *and the buffers it owns*, because a payload
//! whose fields are a `String` or a `Vec` would otherwise free them on return and
//! allocate them again on the next event. That is the whole cost this pool exists
//! to remove, so its slots are never dropped and never re-created; they are reset
//! in place.
//!
//! Acquiring and returning a slot is a bounded, non-blocking channel operation in
//! both. Neither ever falls back to allocating a replacement slot when every slot
//! is in flight: exhaustion refuses, and the caller reports the drop.

use std::{fmt, mem::MaybeUninit, ops::Deref, ops::DerefMut, sync::Arc};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};

struct Slot<T> {
    value: MaybeUninit<T>,
}

struct PoolInner<T> {
    free_tx: Sender<Box<Slot<T>>>,
    free_rx: Receiver<Box<Slot<T>>>,
}

/// Cloneable handle to a fixed number of reusable payload allocations.
pub struct PayloadPool<T> {
    inner: Arc<PoolInner<T>>,
}

impl<T> Clone for PayloadPool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> PayloadPool<T> {
    /// Allocate every slot up front.
    ///
    /// # Panics
    /// Panics when `capacity` is zero; a zero-capacity ownership pool cannot
    /// ever make progress and is therefore a construction error.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "payload pool capacity must be non-zero");
        let (free_tx, free_rx) = crossbeam_channel::bounded(capacity);
        for _ in 0..capacity {
            free_tx
                .try_send(Box::new(Slot {
                    value: MaybeUninit::uninit(),
                }))
                .expect("a freshly created pool has exactly capacity slots");
        }
        Self {
            inner: Arc::new(PoolInner { free_tx, free_rx }),
        }
    }

    /// Move `value` into a free slot without blocking.
    ///
    /// Exhaustion returns the unchanged value to the caller. It never grows
    /// the pool and never allocates a replacement slot.
    #[inline]
    pub fn try_insert(&self, value: T) -> Result<Pooled<T>, T> {
        let mut slot = match self.inner.free_rx.try_recv() {
            Ok(slot) => slot,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Err(value),
        };
        slot.value.write(value);
        Ok(Pooled {
            slot: Some(slot),
            pool: Arc::clone(&self.inner),
        })
    }
}

/// One initialized payload slot.
///
/// Dropping it destroys the current value and returns the allocation to its
/// originating pool, including when a containing channel command is rejected
/// or discarded during shutdown.
pub struct Pooled<T> {
    slot: Option<Box<Slot<T>>>,
    pool: Arc<PoolInner<T>>,
}

impl<T> Deref for Pooled<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // SAFETY: `try_insert` initializes the slot before constructing this
        // value, and only Drop takes it back out.
        unsafe {
            self.slot
                .as_ref()
                .expect("a live pooled payload owns its slot")
                .value
                .assume_init_ref()
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Pooled<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(formatter)
    }
}

impl<T> Drop for Pooled<T> {
    fn drop(&mut self) {
        let mut slot = self
            .slot
            .take()
            .expect("a pooled payload returns its slot exactly once");
        // SAFETY: the slot is initialized for the entire Pooled lifetime and
        // becomes uninitialized again before it is returned to the free list.
        unsafe { slot.value.assume_init_drop() };
        match self.pool.free_tx.try_send(slot) {
            Ok(()) => {}
            // Both cases indicate an invariant failure or final teardown. The
            // value has already been dropped, so releasing the empty allocation
            // is safe and preferable to panicking in Drop.
            Err(TrySendError::Full(slot) | TrySendError::Disconnected(slot)) => drop(slot),
        }
    }
}

/// A payload whose slot is reset in place rather than dropped.
///
/// `recycle` runs on the consumer's thread when the loan falls out of scope. It
/// decides what the slot keeps: the contents are gone either way, so what it is
/// really choosing is which allocations survive to serve the next event.
pub trait Recyclable: Default + Send + 'static {
    /// Empty the payload while keeping the allocations worth keeping.
    ///
    /// A buffer this leaves in place is one the next event does not have to
    /// allocate; a buffer it releases is one the pool will not park. Types with
    /// an unbounded field should release anything past the size their protocol
    /// actually permits, so one malformed event cannot leave a large allocation
    /// resident for the life of the process.
    fn recycle(&mut self);
}

struct RecycleInner<T> {
    free_tx: Sender<Box<T>>,
    free_rx: Receiver<Box<T>>,
    /// Slots created so far. Never decreases: a slot is reset, never released.
    population: std::sync::atomic::AtomicUsize,
    capacity: usize,
}

/// Cloneable handle to a bounded population of reusable, buffer-owning payloads.
///
/// **The population is grown on demand and never shrinks**, which is where this
/// differs from [`PayloadPool`] and the difference is deliberate rather than
/// incidental. `PayloadPool` allocates every slot up front because touch input
/// starts on the first frame of every Session, so the cost is paid by a Session
/// that is certainly going to use it. This pool backs a capability most content
/// never touches at all, and its slots own buffers, so preallocating the same
/// number of them would charge every Session for a peripheral it does not have.
///
/// Growing on demand keeps both properties that matter: an unused pool costs one
/// empty channel, and a pool in steady state allocates nothing, because after the
/// first burst the high-water mark is already resident. Growth is bounded by
/// `capacity`, so the pool can hold no more than the queue it feeds.
pub struct RecyclePool<T: Recyclable> {
    inner: Arc<RecycleInner<T>>,
}

impl<T: Recyclable> Clone for RecyclePool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: Recyclable> RecyclePool<T> {
    /// # Panics
    /// Panics when `capacity` is zero; a pool that can never hand out a slot
    /// cannot make progress and is therefore a construction error.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "recycle pool capacity must be non-zero");
        // Bounded at exactly `capacity`, which is what makes a return infallible:
        // the population can never exceed the channel's room for it.
        let (free_tx, free_rx) = crossbeam_channel::bounded(capacity);
        Self {
            inner: Arc::new(RecycleInner {
                free_tx,
                free_rx,
                population: std::sync::atomic::AtomicUsize::new(0),
                capacity,
            }),
        }
    }

    /// Take a reset slot, or `None` when every slot is in flight.
    ///
    /// Never blocks and never exceeds the configured capacity.
    #[inline]
    pub fn try_acquire(&self) -> Option<Recycled<T>> {
        let slot = match self.inner.free_rx.try_recv() {
            Ok(slot) => slot,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => self.grow()?,
        };
        Some(Recycled {
            slot: Some(slot),
            pool: Arc::clone(&self.inner),
        })
    }

    /// The one path that allocates, taken at most `capacity` times per pool.
    ///
    /// Claiming the population slot before allocating is what bounds it: a
    /// concurrent grower that loses the exchange retries against the new value
    /// and stops at the cap rather than racing past it.
    #[cold]
    fn grow(&self) -> Option<Box<T>> {
        use std::sync::atomic::Ordering;

        let mut population = self.inner.population.load(Ordering::Relaxed);
        loop {
            if population == self.inner.capacity {
                return None;
            }
            match self.inner.population.compare_exchange_weak(
                population,
                population + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Box::default()),
                Err(observed) => population = observed,
            }
        }
    }

    /// Slots created so far, for tests that need to see growth stop.
    #[must_use]
    pub fn population(&self) -> usize {
        self.inner
            .population
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// One payload on loan from its [`RecyclePool`], returned by drop glue.
///
/// The return is drop glue rather than an obligation for the same reason the
/// command vector pool made that choice: a forgotten `release` is invisible,
/// because every caller still gets a payload and a pool that has silently
/// stopped recycling looks exactly like one that is working.
pub struct Recycled<T: Recyclable> {
    slot: Option<Box<T>>,
    pool: Arc<RecycleInner<T>>,
}

impl<T: Recyclable> Deref for Recycled<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.slot
            .as_ref()
            .expect("a live recycled payload owns its slot")
    }
}

impl<T: Recyclable> DerefMut for Recycled<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        self.slot
            .as_mut()
            .expect("a live recycled payload owns its slot")
    }
}

impl<T: Recyclable + fmt::Debug> fmt::Debug for Recycled<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(formatter)
    }
}

impl<T: Recyclable> Drop for Recycled<T> {
    fn drop(&mut self) {
        let mut slot = self
            .slot
            .take()
            .expect("a recycled payload returns its slot exactly once");
        slot.recycle();
        match self.pool.free_tx.try_send(slot) {
            Ok(()) => {}
            // Unreachable while the invariant holds — the channel has room for
            // the whole population, and holding a loan keeps both endpoints
            // alive — so releasing the allocation is preferable to panicking in
            // Drop. The population counter deliberately keeps counting it: a
            // pool that lost a slot must not grow past its cap to replace it.
            Err(TrySendError::Full(slot) | TrySendError::Disconnected(slot)) => drop(slot),
        }
    }
}

#[cfg(test)]
mod recycle_pool_tests {
    use super::{Recyclable, RecyclePool, Recycled};
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};

    /// A payload that owns a buffer, which is the only reason this pool exists.
    ///
    /// Its `recycle` keeps the allocation and drops the contents, and it counts
    /// its own resets so a test can tell "the slot came back" from "a fresh slot
    /// was made" without reading the pool's internals.
    #[derive(Debug, Default)]
    struct Payload {
        bytes: Vec<u8>,
        recycled: u32,
    }

    impl Recyclable for Payload {
        fn recycle(&mut self) {
            self.bytes.clear();
            self.recycled += 1;
        }
    }

    #[test]
    fn a_returned_slot_is_the_next_acquisition() {
        let pool: RecyclePool<Payload> = RecyclePool::new(4);

        let mut first = pool.try_acquire().expect("an empty pool grows on demand");
        first.bytes.extend_from_slice(b"hello");
        let address = first.bytes.as_ptr();
        drop(first);

        let second = pool.try_acquire().expect("the returned slot is available");
        assert_eq!(
            second.recycled, 1,
            "the slot was recycled rather than rebuilt"
        );
        assert!(second.bytes.is_empty(), "a reused slot carries no contents");
        // Identity rather than capacity: the same address is proof the buffer
        // was kept, while a capacity is only proof of `Vec`'s growth strategy --
        // asserting the exact number here failed against a buffer that had been
        // kept perfectly well, because five bytes reserve eight.
        assert_eq!(
            second.bytes.as_ptr(),
            address,
            "the buffer is kept, which is the whole purpose of this pool"
        );
        assert!(second.bytes.capacity() >= 5);
    }

    #[test]
    fn the_population_grows_only_to_the_high_water_mark() {
        let pool: RecyclePool<Payload> = RecyclePool::new(8);
        assert_eq!(pool.population(), 0, "an unused pool costs nothing");

        let held: Vec<Recycled<Payload>> = (0..3)
            .map(|_| pool.try_acquire().expect("under capacity"))
            .collect();
        assert_eq!(pool.population(), 3);
        drop(held);

        for _ in 0..16 {
            let slot = pool.try_acquire().expect("a returned slot is reusable");
            drop(slot);
        }
        assert_eq!(
            pool.population(),
            3,
            "traffic at a depth already reached must not make another slot"
        );
    }

    #[test]
    fn an_exhausted_pool_refuses_rather_than_exceeding_its_capacity() {
        const CAPACITY: usize = 3;
        let pool: RecyclePool<Payload> = RecyclePool::new(CAPACITY);

        let held: Vec<Recycled<Payload>> = (0..CAPACITY)
            .map(|_| pool.try_acquire().expect("within capacity"))
            .collect();

        assert!(pool.try_acquire().is_none(), "the cap is a cap");
        assert_eq!(pool.population(), CAPACITY);

        drop(held);
        assert!(
            pool.try_acquire().is_some(),
            "refusing while full must not poison the pool"
        );
    }

    #[test]
    #[should_panic(expected = "capacity must be non-zero")]
    fn a_pool_that_can_never_hand_out_a_slot_is_a_construction_error() {
        let _: RecyclePool<Payload> = RecyclePool::new(0);
    }

    /// Steady-state allocation gate, for the mechanism rather than for one of its consumers.
    ///
    /// The warm-up covers both one-time costs: the population growing to the
    /// depth the burst uses, and each slot's buffer reaching the size it carries.
    #[test]
    fn steady_state_recycling_never_reaches_the_heap() {
        const IN_FLIGHT: usize = 4;
        let pool: RecyclePool<Payload> = RecyclePool::new(IN_FLIGHT);
        // Reserved outside the burst: growth here would be attributed to the pool.
        let mut held: Vec<Recycled<Payload>> = Vec::with_capacity(IN_FLIGHT);

        assert_no_steady_state_allocation(
            Burst {
                path: "recycle_pool: acquire, fill, and return at full occupancy",
                warmup: 4,
                measured: 64,
            },
            |_| {
                for _ in 0..IN_FLIGHT {
                    let mut slot = pool.try_acquire().expect("a returned slot is reusable");
                    slot.bytes.extend_from_slice(&[7_u8; 24]);
                    held.push(slot);
                }
                assert!(pool.try_acquire().is_none(), "an exhausted pool refuses");
                let carried: usize = held.iter().map(|slot| slot.bytes.len()).sum();
                assert_eq!(carried, IN_FLIGHT * 24);
                held.clear();
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{PayloadPool, Pooled};
    use crate::protocol::host_cmd::{TouchData, TouchPoint, TouchType};
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};

    fn touch_sample() -> TouchData {
        TouchData {
            touch_type: TouchType::Move,
            count: 1,
            points: [TouchPoint::default(); 10],
            timestamp_ms: 0,
        }
    }

    /// Steady-state allocation gate, and the claim `TouchData`'s own documentation makes: a single
    /// memcpy into a preallocated slot keeps steady-state input allocation-free.
    ///
    /// The burst runs at full occupancy and then asks for one slot too many, because
    /// exhaustion is where a pool is most tempted to allocate a replacement.
    #[test]
    fn steady_state_payload_traffic_never_reaches_the_heap() {
        const IN_FLIGHT: usize = 4;
        let pool: PayloadPool<TouchData> = PayloadPool::new(IN_FLIGHT);
        // Reserved outside the burst: this is the harness's own bookkeeping, and a
        // growth here would be attributed to the pool.
        let mut in_flight: Vec<Pooled<TouchData>> = Vec::with_capacity(IN_FLIGHT);

        assert_no_steady_state_allocation(
            Burst {
                path: "payload_pool: acquire, read and return at full occupancy",
                warmup: 2,
                measured: 64,
            },
            |_| {
                for _ in 0..IN_FLIGHT {
                    in_flight.push(
                        pool.try_insert(touch_sample())
                            .expect("a returned slot is reusable"),
                    );
                }
                assert!(
                    pool.try_insert(touch_sample()).is_err(),
                    "an exhausted pool must refuse, not grow"
                );

                let points: usize = in_flight.iter().map(|held| usize::from(held.count)).sum();
                assert_eq!(points, IN_FLIGHT);
                in_flight.clear();
                points
            },
        );
    }
}
