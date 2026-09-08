//! Bounded recycler for command vectors crossing the host/render thread boundary.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_channel::{Receiver, Sender, bounded};

use crate::protocol::render_cmd::{Canvas2DCmd, GLCmd};

pub const GL_COMMAND_VEC_INITIAL_CAPACITY: usize = 16;
pub const CANVAS_COMMAND_VEC_INITIAL_CAPACITY: usize = 8;
pub const COMMAND_VEC_POOL_SLOTS: usize = 16;
pub const FRAME_OP_VEC_INITIAL_CAPACITY: usize = 8;
pub const FRAME_OP_VEC_POOL_BUDGET_OPS_PER_SLOT: usize = 128;

/// Commands per slot the pool's memory budget is sized for.
///
/// **This is a budget, not a per-vector ceiling, and the difference is the whole
/// point.** It was a ceiling: a vector that outgrew it was dropped, so a frame
/// one command past it started the next frame from the minimum capacity and
/// regrew — six reallocations and about 175 KiB of copying, every frame, on the
/// thread running the game, for one command's difference. A cliff, not a
/// gradient.
///
/// What the ceiling was protecting is memory: one pathological frame must not
/// leave a huge allocation parked in the pool forever. A per-vector element
/// count does not express that. It bounds the wrong quantity (one vector, not
/// the pool), in the wrong unit (elements, so the same constant means different
/// amounts for `GLCmd` and `Canvas2DCmd`), and it already permitted
/// `slots * this * size_of::<T>()` bytes to sit in a full pool anyway.
///
/// So the pool now bounds exactly that quantity — its own retained bytes — and
/// spends the allowance on whatever shape the workload has: one large vector or
/// sixteen small ones. **The permitted worst case is unchanged by construction**,
/// because the budget is derived from the same arithmetic the old rule already
/// allowed, and no single frame size is special any more.
pub const COMMAND_VEC_POOL_BUDGET_COMMANDS_PER_SLOT: usize = 512;

/// Element types the recycler holds, each naming the one pool its vectors
/// belong to.
///
/// The pool is chosen by the element type rather than carried inside every
/// vector, so a [`PooledVec`] occupies exactly what a `Vec` occupies. That
/// matters here: these vectors live inside `FrameOp`, which is itself held by
/// the vector this trait also governs, so a per-vector back-pointer would be
/// paid once per command batch and once per frame packet.
pub trait Pooled: Sized + 'static {
    fn pool() -> &'static CommandVecPool<Self>;
}

/// A command vector on loan from its pool, which returns itself when dropped.
///
/// **The return is drop glue, not an obligation, and that is the whole point.**
/// The previous shape handed out a bare `Vec` and asked every consumer to call
/// `recycle_*` when it was finished. Forgetting that call is invisible: every
/// caller still gets a vector, just a freshly allocated one, so a pool that has
/// silently stopped retaining anything looks exactly like a pool that is
/// working. It is invisible to the allocation gates too — a leaked loan is a
/// *de*allocation, which a burst that counts allocations cannot see by
/// construction. Mutation testing confirmed it: deleting `append_gl_batch`'s
/// recycle call failed no test in the binary.
///
/// So the call is gone rather than guarded. There is no `recycle` to forget,
/// and a consumer that simply lets the vector fall out of scope does the right
/// thing.
///
/// `Deref`/`DerefMut` reach the underlying `Vec` so call sites read as they did
/// before. That does leave `mem::take` able to steal the allocation, which is
/// deliberate: the failure this type exists to remove is *forgetting* to return
/// a loan, not deciding to keep one.
pub struct PooledVec<T: Pooled> {
    inner: Vec<T>,
}

impl<T: Pooled> PooledVec<T> {
    /// Takes a vector from `T`'s pool, reusing a retained allocation when one
    /// is available.
    #[inline]
    pub fn take() -> Self {
        Self {
            inner: T::pool().take(),
        }
    }

    /// Loan a vector reserved for at most this many elements. Oversized
    /// retained allocations remain reusable when there is room for both; they
    /// cannot make a small admitted batch exceed its memory estimate or starve
    /// the new scene of reusable storage.
    pub fn take_with_capacity_limit(max_capacity: usize) -> Self {
        Self {
            inner: T::pool().take_with_capacity_limit(max_capacity),
        }
    }
}

impl<T: Pooled> Default for PooledVec<T> {
    #[inline]
    fn default() -> Self {
        Self::take()
    }
}

impl<T: Pooled> Drop for PooledVec<T> {
    #[inline]
    fn drop(&mut self) {
        // Emptied here rather than refused for being non-empty: a partially
        // consumed loan still owns an allocation worth keeping, and dropping
        // the remaining elements is what dropping the vector would do anyway.
        T::pool().reclaim(std::mem::take(&mut self.inner));
    }
}

impl<T: Pooled> std::ops::Deref for PooledVec<T> {
    type Target = Vec<T>;

    #[inline]
    fn deref(&self) -> &Vec<T> {
        &self.inner
    }
}

impl<T: Pooled> std::ops::DerefMut for PooledVec<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.inner
    }
}

/// Adopts an existing vector into the pool's population.
///
/// Mostly a test convenience, and sound in production too: the pool's retention
/// budget bounds what it keeps regardless of where a vector came from.
impl<T: Pooled> From<Vec<T>> for PooledVec<T> {
    #[inline]
    fn from(inner: Vec<T>) -> Self {
        Self { inner }
    }
}

impl<T: Pooled + std::fmt::Debug> std::fmt::Debug for PooledVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl<T: Pooled + PartialEq> PartialEq for PooledVec<T> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

/// Consuming iteration that still returns the allocation.
///
/// `std::vec::IntoIter` owns the buffer and frees it, which would defeat the
/// pool on every `for op in packet.into_ops()`. Reversing once and popping from
/// the back yields the same order in O(n) with no unsafe and no second
/// allocation, and leaves the emptied vector for [`PooledVec`]'s own `Drop` —
/// including when the loop breaks early.
pub struct PooledIntoIter<T: Pooled> {
    vec: PooledVec<T>,
}

impl<T: Pooled> Iterator for PooledIntoIter<T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<T> {
        self.vec.inner.pop()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.vec.inner.len();
        (remaining, Some(remaining))
    }
}

impl<T: Pooled> ExactSizeIterator for PooledIntoIter<T> {}

impl<T: Pooled> IntoIterator for PooledVec<T> {
    type Item = T;
    type IntoIter = PooledIntoIter<T>;

    #[inline]
    fn into_iter(mut self) -> PooledIntoIter<T> {
        self.inner.reverse();
        PooledIntoIter { vec: self }
    }
}

impl<'a, T: Pooled> IntoIterator for &'a PooledVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    #[inline]
    fn into_iter(self) -> std::slice::Iter<'a, T> {
        self.inner.iter()
    }
}

impl<T: Pooled> Extend<T> for PooledVec<T> {
    #[inline]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.inner.extend(iter);
    }
}

/// A bounded recycler for one element type's command vectors.
///
/// Public only because [`Pooled`] names it; it cannot be constructed from
/// outside this crate.
pub struct CommandVecPool<T> {
    sender: Sender<Vec<T>>,
    receiver: Receiver<Vec<T>>,
    minimum_capacity: usize,
    retained_byte_budget: usize,
    retained_bytes: AtomicUsize,
}

impl<T> CommandVecPool<T> {
    /// `budget_commands_per_slot` stays in *commands*, deliberately: the pool
    /// converts to bytes itself, so no call site can pass one unit where the
    /// other is meant. A `usize` budget in bytes next to a `usize` count of
    /// commands is a mistake the compiler cannot catch, and the first draft of
    /// this change made it.
    pub(crate) fn new(
        slots: usize,
        minimum_capacity: usize,
        budget_commands_per_slot: usize,
    ) -> Self {
        assert!(slots > 0, "command vector pool must have at least one slot");
        assert!(
            minimum_capacity <= budget_commands_per_slot,
            "a vector at the minimum capacity must fit the pool's budget"
        );
        let (sender, receiver) = bounded(slots);
        Self {
            sender,
            receiver,
            minimum_capacity,
            retained_byte_budget: slots
                .saturating_mul(budget_commands_per_slot)
                .saturating_mul(size_of::<T>()),
            retained_bytes: AtomicUsize::new(0),
        }
    }

    #[inline]
    /// Takes the capacity rather than the vector: a recycled vector is empty, so
    /// what it occupies is the allocation the next frame will reuse, never its
    /// length.
    fn bytes_of(capacity: usize) -> usize {
        capacity.saturating_mul(size_of::<T>())
    }

    #[inline]
    fn take(&self) -> Vec<T> {
        let mut commands = match self.receiver.try_recv() {
            Ok(recycled) => {
                // Released before the capacity below can change it, so what is
                // subtracted is exactly what `recycle` added.
                self.retained_bytes
                    .fetch_sub(Self::bytes_of(recycled.capacity()), Ordering::Relaxed);
                recycled
            }
            Err(_) => Vec::with_capacity(self.minimum_capacity),
        };
        debug_assert!(commands.is_empty());
        if commands.capacity() < self.minimum_capacity {
            commands.reserve_exact(self.minimum_capacity);
        }
        commands
    }

    fn take_with_capacity_limit(&self, max_capacity: usize) -> Vec<T> {
        if max_capacity == 0 {
            return Vec::new();
        }
        for _ in 0..self.receiver.len() {
            let Ok(mut commands) = self.receiver.try_recv() else {
                break;
            };
            self.retained_bytes
                .fetch_sub(Self::bytes_of(commands.capacity()), Ordering::Relaxed);
            if commands.capacity() <= max_capacity {
                commands.reserve_exact(max_capacity);
                return commands;
            }
            let needed =
                Self::bytes_of(commands.capacity()).saturating_add(Self::bytes_of(max_capacity));
            let leaves_bytes = self
                .retained_bytes
                .load(Ordering::Relaxed)
                .saturating_add(needed)
                <= self.retained_byte_budget;
            let leaves_slot = self.receiver.len() + 1 < self.receiver.capacity().unwrap_or(0);
            if leaves_bytes && leaves_slot {
                let _ = self.try_recycle(commands);
            }
        }
        Vec::with_capacity(max_capacity)
    }

    #[inline]
    fn recycle(&self, commands: Vec<T>) -> bool {
        if !commands.is_empty() || commands.capacity() == 0 {
            return false;
        }
        let bytes = Self::bytes_of(commands.capacity());
        if bytes > self.retained_byte_budget {
            return false;
        }
        let mut commands = match self.try_recycle(commands) {
            Ok(()) => return true,
            Err(commands) => commands,
        };
        // Cold overflow: a tiny vector returned by a flush must not prevent
        // the large batch from returning forever. Prefer the larger reusable
        // allocation, without increasing either slots or retained bytes.
        // A fixed number of attempts also bounds work under concurrent returns.
        for _ in 0..self.receiver.len() {
            let Ok(victim) = self.receiver.try_recv() else {
                break;
            };
            let victim_bytes = Self::bytes_of(victim.capacity());
            self.retained_bytes
                .fetch_sub(victim_bytes, Ordering::Relaxed);
            if victim_bytes >= bytes {
                let _ = self.try_recycle(victim);
                continue;
            }
            drop(victim);
            commands = match self.try_recycle(commands) {
                Ok(()) => return true,
                Err(commands) => commands,
            };
        }
        false
    }

    fn try_recycle(&self, commands: Vec<T>) -> Result<(), Vec<T>> {
        let bytes = Self::bytes_of(commands.capacity());
        if self
            .retained_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |retained| {
                retained
                    .checked_add(bytes)
                    .filter(|total| *total <= self.retained_byte_budget)
            })
            .is_err()
        {
            return Err(commands);
        }
        self.sender.try_send(commands).map_err(|error| {
            self.retained_bytes.fetch_sub(bytes, Ordering::Relaxed);
            error.into_inner()
        })
    }

    /// Empties a returned loan and offers it back. Separate from [`Self::recycle`]
    /// so that method keeps refusing a non-empty vector — the refusal is what
    /// stops a caller from parking live commands in the pool, and a loan being
    /// dropped is the one case where clearing is the caller's intent anyway.
    #[inline]
    fn reclaim(&self, mut commands: Vec<T>) {
        commands.clear();
        let _ = self.recycle(commands);
    }

    #[cfg(test)]
    fn retained_bytes(&self) -> usize {
        self.retained_bytes.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn retained_byte_budget(&self) -> usize {
        self.retained_byte_budget
    }
}

impl Pooled for GLCmd {
    fn pool() -> &'static CommandVecPool<GLCmd> {
        static POOL: OnceLock<CommandVecPool<GLCmd>> = OnceLock::new();
        POOL.get_or_init(|| {
            CommandVecPool::new(
                COMMAND_VEC_POOL_SLOTS,
                GL_COMMAND_VEC_INITIAL_CAPACITY,
                COMMAND_VEC_POOL_BUDGET_COMMANDS_PER_SLOT,
            )
        })
    }
}

impl Pooled for Canvas2DCmd {
    fn pool() -> &'static CommandVecPool<Canvas2DCmd> {
        static POOL: OnceLock<CommandVecPool<Canvas2DCmd>> = OnceLock::new();
        POOL.get_or_init(|| {
            CommandVecPool::new(
                COMMAND_VEC_POOL_SLOTS,
                CANVAS_COMMAND_VEC_INITIAL_CAPACITY,
                COMMAND_VEC_POOL_BUDGET_COMMANDS_PER_SLOT,
            )
        })
    }
}

impl Pooled for crate::protocol::FrameOp {
    /// The frame packet's own op vector, which crosses the same thread boundary
    /// its contents do — built on the thread running the game, consumed on the
    /// render thread — and so needs the same cross-thread recycler rather than a
    /// thread-local scratch buffer.
    ///
    /// Its own dimensions, because an op is not a command. A packet holds one
    /// op per segment plus a `BeginFrame`, the `Materialize` ops at each
    /// Canvas2D→WebGL boundary and a `Present`: single digits for a typical
    /// frame, and the worst case measured on the shop scene was about
    /// sixty-five. So the minimum capacity is small, and the per-slot budget is
    /// a quarter of the command pools' — a `FrameOp` is several times the width
    /// of a `GLCmd`, so matching their command count would reserve far more
    /// memory for a vector that holds far fewer elements.
    fn pool() -> &'static CommandVecPool<crate::protocol::FrameOp> {
        static POOL: OnceLock<CommandVecPool<crate::protocol::FrameOp>> = OnceLock::new();
        POOL.get_or_init(|| {
            CommandVecPool::new(
                COMMAND_VEC_POOL_SLOTS,
                FRAME_OP_VEC_INITIAL_CAPACITY,
                FRAME_OP_VEC_POOL_BUDGET_OPS_PER_SLOT,
            )
        })
    }
}

#[inline]
pub fn take_gl_command_vec() -> PooledVec<GLCmd> {
    PooledVec::take()
}

#[inline]
pub fn take_canvas_command_vec() -> PooledVec<Canvas2DCmd> {
    PooledVec::take()
}

#[cfg(test)]
mod tests {
    use super::{CommandVecPool, Pooled, PooledVec};
    use std::sync::OnceLock;

    /// Gives a test its own element type, and so its own pool.
    ///
    /// `cargo test` runs these concurrently against one process, so two tests
    /// sharing a pool would take each other's vectors and each would be
    /// asserting about the other's allocations. A distinct type per test is the
    /// isolation, and it costs nothing at runtime because the pool is selected
    /// by type.
    macro_rules! private_pool_type {
        ($name:ident, $slots:expr, $minimum:expr, $budget:expr) => {
            #[derive(Debug, PartialEq, Eq)]
            struct $name(u32);

            impl Pooled for $name {
                fn pool() -> &'static CommandVecPool<$name> {
                    static POOL: OnceLock<CommandVecPool<$name>> = OnceLock::new();
                    POOL.get_or_init(|| CommandVecPool::new($slots, $minimum, $budget))
                }
            }
        };
    }

    private_pool_type!(ScopeExit, 1, 4, 512);
    private_pool_type!(PartiallyConsumed, 1, 4, 512);
    private_pool_type!(FullyConsumed, 1, 4, 512);
    private_pool_type!(Adopted, 1, 4, 512);

    /// **The property the previous shape could not have.** A loan was returned
    /// by calling `recycle_*`, and forgetting the call was invisible: every
    /// caller still got a vector, just a freshly allocated one. Nothing observed
    /// the difference — an allocation gate cannot, because a lost loan is a
    /// *de*allocation. Mutation proved it: deleting `append_gl_batch`'s recycle
    /// call failed no test in the binary.
    ///
    /// Here the vector simply goes out of scope, with no return call to write or
    /// to forget, and the allocation comes back.
    #[test]
    fn a_loan_that_falls_out_of_scope_returns_its_allocation() {
        let allocation = {
            let mut commands = PooledVec::<ScopeExit>::take();
            commands.reserve_exact(64);
            commands.push(ScopeExit(1));
            commands.as_ptr()
        };

        let reused = PooledVec::<ScopeExit>::take();
        assert_eq!(
            reused.as_ptr(),
            allocation,
            "a loan that went out of scope did not come back to its pool"
        );
        assert!(reused.is_empty(), "a returned loan must arrive empty");
    }

    /// A loan the consumer stopped reading part-way through still owns an
    /// allocation worth keeping. The pool's own `recycle` refuses a non-empty
    /// vector — that refusal stops a caller parking live commands in the pool —
    /// so the drop path has to empty it rather than hand it over as-is.
    #[test]
    fn a_partially_consumed_loan_still_returns_its_allocation() {
        let allocation = {
            let mut commands = PooledVec::<PartiallyConsumed>::take();
            commands.reserve_exact(64);
            commands.extend([0, 1, 2, 3].map(PartiallyConsumed));
            let ptr = commands.as_ptr();
            for command in commands {
                if command.0 == 1 {
                    break;
                }
            }
            ptr
        };

        assert_eq!(
            PooledVec::<PartiallyConsumed>::take().as_ptr(),
            allocation,
            "a loan abandoned mid-iteration lost its allocation"
        );
    }

    /// Consuming iteration has to yield submission order — the render thread
    /// executes ops in the order it receives them — while still leaving the
    /// buffer for the pool. `std::vec::IntoIter` gets the order right and frees
    /// the buffer, which is why this iterator is not that one.
    #[test]
    fn consuming_iteration_yields_in_order_and_returns_the_allocation() {
        let mut commands = PooledVec::<FullyConsumed>::take();
        commands.reserve_exact(64);
        commands.extend([10, 20, 30].map(FullyConsumed));
        let allocation = commands.as_ptr();

        let seen: Vec<u32> = commands.into_iter().map(|command| command.0).collect();
        assert_eq!(
            seen,
            vec![10, 20, 30],
            "consuming iteration reordered the ops"
        );

        assert_eq!(
            PooledVec::<FullyConsumed>::take().as_ptr(),
            allocation,
            "a fully consumed loan did not return its allocation"
        );
    }

    /// A plain `Vec` adopted into a loan joins the pool's population when it is
    /// dropped. Test fixtures build batches this way, and it is sound in
    /// production too: the retention budget bounds what the pool keeps whatever
    /// the vector's origin.
    #[test]
    fn an_adopted_vector_joins_the_pool() {
        let mut donated = Vec::with_capacity(64);
        donated.push(Adopted(7));
        let allocation = donated.as_ptr();

        drop(PooledVec::from(donated));

        assert_eq!(
            PooledVec::<Adopted>::take().as_ptr(),
            allocation,
            "an adopted vector was dropped instead of being kept"
        );
    }

    /// The pool's worst case in bytes, stated rather than assumed.
    ///
    /// A budget expressed in *commands per slot* means nothing on its own — the
    /// same number reserves wildly different amounts for element types of
    /// different widths, which is why the frame-op pool does not simply inherit
    /// the command pools' figure. This pins what each pool may actually hold so
    /// that changing a constant, or widening one of these enums, has to be a
    /// decision someone takes rather than a number that drifts.
    #[test]
    fn each_pool_states_the_memory_it_may_retain() {
        use crate::protocol::FrameOp;
        use crate::protocol::render_cmd::{Canvas2DCmd, GLCmd};

        let worst_case =
            |per_slot: usize, width: usize| super::COMMAND_VEC_POOL_SLOTS * per_slot * width;

        let gl = worst_case(
            super::COMMAND_VEC_POOL_BUDGET_COMMANDS_PER_SLOT,
            size_of::<GLCmd>(),
        );
        let canvas = worst_case(
            super::COMMAND_VEC_POOL_BUDGET_COMMANDS_PER_SLOT,
            size_of::<Canvas2DCmd>(),
        );
        let frame_ops = worst_case(
            super::FRAME_OP_VEC_POOL_BUDGET_OPS_PER_SLOT,
            size_of::<FrameOp>(),
        );

        // A wrapper that costs nothing is the reason the pool is selected by
        // element type instead of carried in each vector.
        assert_eq!(
            size_of::<super::PooledVec<GLCmd>>(),
            size_of::<Vec<GLCmd>>(),
            "a loan must occupy exactly what the vector it wraps occupies"
        );

        assert!(
            frame_ops < gl / 2,
            "the frame-op pool reserves {frame_ops} bytes against the GL pool's \
             {gl}: an op is several times a command's width, so matching the \
             command budget would reserve far more for far fewer elements"
        );

        // Currently 786 KiB GL, 448 KiB Canvas2D, 112 KiB frame ops — about
        // 1.33 MiB in total, and reached only by a process that really did
        // produce sixteen vectors that wide. The command pools' share of that is
        // inherited: the byte budget was derived to permit exactly what the
        // older per-vector element ceiling already permitted, so this test
        // records that figure rather than proposing a different one.
        //
        // The ceiling below is a ceiling on the ceiling. It is not a target; it
        // is what stops a widened `GLCmd` from quietly multiplying the reserve,
        // since the budget counts elements and the bytes follow the enum. A
        // command is 96 bytes today, and narrowing it — boxing the few variants
        // that carry uploads — would cut the largest share of this directly.
        // That is a measurement someone should take, not a change to make blind,
        // and it is recorded as such rather than done here.
        let total = gl + canvas + frame_ops;
        assert!(
            total <= 2 * 1024 * 1024,
            "the three pools may retain {total} bytes between them ({gl} GL, \
             {canvas} Canvas2D, {frame_ops} frame ops); the recycler is meant to \
             avoid per-frame allocation, not to hold a cache this size"
        );
    }

    #[test]
    fn recycled_vector_reuses_its_allocation() {
        let pool = CommandVecPool::<u32>::new(1, 4, 8);
        let mut commands = pool.take();
        commands.extend_from_slice(&[1, 2, 3, 4]);
        commands.clear();
        let allocation = commands.as_ptr();
        let capacity = commands.capacity();

        assert!(pool.recycle(commands));
        let reused = pool.take();
        assert_eq!(reused.as_ptr(), allocation);
        assert_eq!(reused.capacity(), capacity);
        assert!(reused.is_empty());
    }

    #[test]
    fn a_full_budget_vector_displaces_the_small_flush_replacement() {
        let pool = CommandVecPool::<u32>::new(16, 8, 512);
        let mut commands = pool.take();
        commands.reserve_exact(8192);
        let allocation = commands.as_ptr();
        // A decoder used to take this replacement just before handing over the
        // big batch. It returns before the renderer releases that batch.
        let replacement = pool.take();
        assert!(pool.recycle(replacement));
        assert!(
            pool.recycle(commands),
            "the small replacement stranded the full budget allocation"
        );
        assert!(pool.retained_bytes() <= pool.retained_byte_budget());
        let reused = pool.take();
        assert_eq!(reused.as_ptr(), allocation);
        assert_eq!(reused.capacity(), 8192);
    }

    #[test]
    fn a_bounded_loan_does_not_inherit_a_previous_frames_large_capacity() {
        let pool = CommandVecPool::<u32>::new(4, 4, 512);
        assert!(pool.recycle(Vec::with_capacity(1024)));
        let bounded = pool.take_with_capacity_limit(8);
        assert_eq!(bounded.capacity(), 8);
        assert_eq!(
            pool.take().capacity(),
            1024,
            "large allocation remains reusable"
        );
    }

    #[test]
    fn a_smaller_scene_can_reuse_loans_after_a_full_budget_frame() {
        let pool = CommandVecPool::<u32>::new(16, 8, 512);
        assert!(pool.recycle(Vec::with_capacity(8192)));
        let small = pool.take_with_capacity_limit(8);
        let allocation = small.as_ptr();
        assert!(
            pool.recycle(small),
            "the obsolete large allocation starved small batches"
        );
        let reused = pool.take_with_capacity_limit(8);
        assert_eq!(reused.as_ptr(), allocation);
        assert!(pool.retained_bytes() <= pool.retained_byte_budget());
    }

    #[test]
    fn full_pool_drops_excess_vectors_without_blocking() {
        let pool = CommandVecPool::<u32>::new(1, 4, 8);
        assert!(pool.recycle(Vec::with_capacity(4)));
        assert!(!pool.recycle(Vec::with_capacity(4)));
    }

    /// The pathological frame the budget exists for: a single vector that would
    /// fill the pool by itself never gets in, however many slots are free.
    #[test]
    fn a_vector_larger_than_the_whole_budget_is_not_retained() {
        let pool = CommandVecPool::<u32>::new(1, 4, 8);
        assert!(!pool.recycle(Vec::with_capacity(9)));
        assert_eq!(pool.retained_bytes(), 0, "a refusal reserved budget anyway");
        let fresh = pool.take();
        assert!(fresh.capacity() >= 4);
    }

    /// **The property the per-vector ceiling never actually delivered.** It
    /// bounded one vector while letting every slot hold one, so the pool's real
    /// worst case was `slots ×` the ceiling. The budget bounds the pool, so
    /// retention stops when the bytes run out even though slots remain.
    #[test]
    fn the_pool_bounds_its_own_bytes_not_the_size_of_one_vector() {
        // Four slots, sixteen commands of budget in total.
        let pool = CommandVecPool::<u32>::new(4, 4, 4);
        assert_eq!(pool.retained_byte_budget(), 4 * 4 * size_of::<u32>());

        assert!(
            pool.recycle(Vec::with_capacity(8)),
            "first half of the budget"
        );
        assert!(
            pool.recycle(Vec::with_capacity(8)),
            "second half of the budget"
        );
        assert_eq!(pool.retained_bytes(), pool.retained_byte_budget());

        assert!(
            !pool.recycle(Vec::with_capacity(8)),
            "the pool kept retaining past its budget because two slots were free"
        );
        assert_eq!(
            pool.retained_bytes(),
            pool.retained_byte_budget(),
            "the refused vector left its reservation behind"
        );
    }

    /// A reservation that outlives its vector shrinks the budget for the rest of
    /// the process, and a pool that has quietly stopped retaining anything is
    /// indistinguishable from one that is working — every caller still gets a
    /// vector, just a fresh one every time. So the accounting has to come back to
    /// zero, on the refusal paths as much as on the success path.
    #[test]
    fn retention_accounting_returns_to_zero_however_the_vectors_leave() {
        // One slot, but budget enough for many vectors: the two limits have to be
        // separated or "refused by a full pool" is really "refused by the budget"
        // and the rollback below is never reached. A mutant that dropped that
        // rollback survived the first version of this test for exactly that
        // reason.
        let pool = CommandVecPool::<u32>::new(1, 4, 512);
        let one = 8 * size_of::<u32>();
        assert!(
            one * 2 <= pool.retained_byte_budget(),
            "fixture needs budget to spare so a refusal can come from the slots"
        );

        // Accepted, then reclaimed.
        assert!(pool.recycle(Vec::with_capacity(8)));
        assert_eq!(pool.retained_bytes(), one);
        let taken = pool.take();
        assert_eq!(pool.retained_bytes(), 0);
        drop(taken);

        // Refused by the single occupied slot, with budget still available: the
        // reservation taken to attempt the placement must be given back.
        assert!(pool.recycle(Vec::with_capacity(8)));
        assert!(
            !pool.recycle(Vec::with_capacity(8)),
            "fixture needs the second recycle to be refused by a full pool"
        );
        assert_eq!(
            pool.retained_bytes(),
            one,
            "a vector refused by a full pool kept its reservation, so the budget \
             shrinks every time the pool is full until nothing is ever retained"
        );

        // Refused for being non-empty, before any reservation is taken.
        assert!(!pool.recycle(vec![1]));
        assert_eq!(pool.retained_bytes(), one);

        let _ = pool.take();
        assert_eq!(pool.retained_bytes(), 0);
    }

    /// A frame heavier than the retention rule allows must still keep its
    /// allocation. Under the per-vector element ceiling it did not: crossing the
    /// ceiling by one command dropped the vector, so the next frame regrew it
    /// from the minimum -- six reallocations and 175 KiB of copying per frame, on
    /// the thread running the game, for one command's difference.
    #[test]
    fn a_vector_heavier_than_one_frames_allowance_keeps_its_allocation() {
        // Production shape: the same slot count and per-slot allowance the real
        // pools use, and a vector twice that allowance.
        let pool = CommandVecPool::<u32>::new(16, 4, 512);
        let mut commands = pool.take();
        commands.reserve_exact(1024);
        let allocation = commands.as_ptr();
        let capacity = commands.capacity();

        assert!(
            pool.recycle(commands),
            "a vector the pool has room for was dropped because of its length"
        );
        let reused = pool.take();
        assert_eq!(reused.as_ptr(), allocation);
        assert_eq!(reused.capacity(), capacity);
    }

    #[test]
    fn non_empty_vector_is_rejected_without_panicking() {
        let pool = CommandVecPool::<u32>::new(1, 4, 8);

        assert!(!pool.recycle(vec![1]));
        assert!(pool.receiver.is_empty());
    }
}
