//! Owned frame buffers, and the credit a frame holds until the renderer is done.
//!
//! # Why the bytes have to be copied
//!
//! A packet arrives as a borrowed slice: Swift hands the C ABI a pointer into a
//! `Data` it owns for the duration of one call. The renderer is on another
//! thread and finishes later, so nothing derived from that pointer may outlive
//! the call. There is exactly one honest answer -- copy once, into a buffer this
//! side owns -- and the copy is the reason the pool exists: doing it with a
//! fresh `Vec` every frame would allocate and free a packet-sized buffer sixty
//! to a hundred and twenty times a second, on the render path, in a lane whose
//! whole justification is memory.
//!
//! # Why the pool grows rather than pre-allocating
//!
//! The obvious pool reserves `max_credits + 1` buffers of the maximum packet
//! size up front. At the 4 MiB ceiling that is twelve megabytes held for the
//! life of every session, almost all of it never touched: a real frame is tens
//! of kilobytes. Peak memory is the thing this lane is measured on, so buffers
//! grow to the sizes actually seen and are then reused -- allocation happens
//! during warm-up and not afterwards, which is the property that matters on the
//! render path, and the steady-state footprint is the content's, not the cap's.
//!
//! # Why the credit is an RAII token
//!
//! A credit has to come back on every path: the renderer finished, the renderer
//! rejected the frame after accepting it, the context was lost, the generation
//! went away, the session shut down. Five paths, and a counter decremented by
//! hand at each of them is five places to forget -- and forgetting stalls the
//! producer permanently, which presents as a hang rather than as an error.
//! Holding the credit in a value that returns it when dropped makes the five
//! paths one path.

use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicU32, AtomicUsize, Ordering},
};

/// The credit window, shared between the ingress and every frame in flight.
///
/// Separate from [`crate::FrameIngress`] because a completion token has to
/// return its credit from wherever the renderer finished, which is not where
/// the ingress lives and not necessarily the same thread.
#[derive(Debug)]
pub struct CreditWindow {
    max: u32,
    in_flight: AtomicU32,
}

impl CreditWindow {
    pub(crate) fn new(max: u32) -> Self {
        Self {
            max,
            in_flight: AtomicU32::new(0),
        }
    }

    #[inline]
    pub fn max(&self) -> u32 {
        self.max
    }

    #[inline]
    pub fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::Acquire)
    }

    #[inline]
    pub fn remaining(&self) -> u32 {
        self.max.saturating_sub(self.in_flight())
    }

    /// Take one credit, or report that there is none.
    ///
    /// Compare-and-swap rather than fetch-add-then-check: the latter can exceed
    /// the limit between the two operations, and the whole point of the window
    /// is that it cannot be exceeded.
    pub(crate) fn try_acquire(&self) -> bool {
        let mut current = self.in_flight.load(Ordering::Acquire);
        loop {
            if current >= self.max {
                return false;
            }
            match self.in_flight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    /// Give one back. Saturating: a double return is a bug in the renderer, and
    /// the useful failure is a stalled producer someone investigates rather than
    /// a counter that wraps and turns backpressure off.
    fn release(&self) {
        let mut current = self.in_flight.load(Ordering::Acquire);
        loop {
            if current == 0 {
                return;
            }
            match self.in_flight.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }
}

/// A bounded set of reusable frame buffers.
#[derive(Debug)]
pub struct FramePool {
    idle: Mutex<Vec<Vec<u8>>>,
    /// How many buffers may be kept for reuse. One more than the credit window,
    /// so a frame can be copied in while the window's worth are still out.
    max_idle: usize,
    /// The largest packet this pool will hold, which is the session's ceiling.
    max_bytes: usize,
    /// Bytes currently retained across all idle buffers, for the memory ledger.
    idle_bytes: AtomicUsize,
    /// Buffers allocated since construction. Steady state is zero growth; a
    /// number that keeps rising means the pool is being defeated somewhere.
    allocations: AtomicUsize,
}

impl FramePool {
    pub(crate) fn new(max_idle: usize, max_bytes: usize) -> Self {
        Self {
            idle: Mutex::new(Vec::with_capacity(max_idle)),
            max_idle,
            max_bytes,
            idle_bytes: AtomicUsize::new(0),
            allocations: AtomicUsize::new(0),
        }
    }

    /// Buffers allocated since construction, for the allocation gate and for
    /// telemetry that wants to see warm-up end.
    pub fn allocations(&self) -> usize {
        self.allocations.load(Ordering::Relaxed)
    }

    /// Bytes retained in idle buffers.
    pub fn idle_bytes(&self) -> usize {
        self.idle_bytes.load(Ordering::Relaxed)
    }

    /// The lock, with poisoning recovered rather than propagated.
    ///
    /// `std` rather than a dependency: this crate parses bytes produced by
    /// content JavaScript in another process, and every dependency it takes is
    /// another thing inside that trust boundary. A poisoned lock here means a
    /// thread panicked while holding a list of byte buffers; the list is still
    /// a list of byte buffers, and unwrapping would turn someone else's panic
    /// into an abort on the render path.
    fn idle(&self) -> MutexGuard<'_, Vec<Vec<u8>>> {
        self.idle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn acquire(&self, wanted: usize) -> Option<Vec<u8>> {
        if wanted > self.max_bytes {
            return None;
        }
        let mut idle = self.idle();
        // The largest idle buffer that fits, so a big frame does not take a
        // small buffer and grow it while a big one sits unused.
        let best = idle
            .iter()
            .enumerate()
            .filter(|(_, buffer)| buffer.capacity() >= wanted)
            .min_by_key(|(_, buffer)| buffer.capacity())
            .map(|(index, _)| index);
        match best {
            Some(index) => {
                let mut buffer = idle.swap_remove(index);
                self.idle_bytes
                    .fetch_sub(buffer.capacity(), Ordering::Relaxed);
                buffer.clear();
                Some(buffer)
            }
            None => {
                drop(idle);
                self.allocations.fetch_add(1, Ordering::Relaxed);
                Some(Vec::with_capacity(wanted))
            }
        }
    }

    fn release(&self, buffer: Vec<u8>) {
        // A buffer that grew past the ceiling is dropped rather than kept: the
        // ceiling can be lowered while frames are in flight, and retaining an
        // over-sized buffer would make the lowered ceiling a suggestion.
        if buffer.capacity() > self.max_bytes {
            return;
        }
        let mut idle = self.idle();
        if idle.len() >= self.max_idle {
            return;
        }
        self.idle_bytes
            .fetch_add(buffer.capacity(), Ordering::Relaxed);
        idle.push(buffer);
    }
}

/// One consumer's place in the bounded frame window. Moving this guard does
/// not allocate. It returns its credit on consumption, cancellation or unwind.
#[derive(Debug)]
pub struct FrameCredit {
    window: Arc<CreditWindow>,
}

impl Drop for FrameCredit {
    fn drop(&mut self) {
        self.window.release();
    }
}

/// One accepted frame: the bytes, owned, and the credit they hold.
///
/// Dropping this returns the buffer to the pool and the credit to the window.
/// After decoding, `into_credit` returns the bytes to their pool while moving
/// the credit into the owned render operations that still await consumption.
#[derive(Debug)]
pub struct PooledFrame {
    bytes: Vec<u8>,
    pool: Arc<FramePool>,
    credit: Option<FrameCredit>,
    /// The sequence this frame was accepted as, for correlating a completion
    /// with the packet that caused it.
    sequence: u64,
}

impl PooledFrame {
    pub(crate) fn new(
        source: &[u8],
        pool: &Arc<FramePool>,
        credits: &Arc<CreditWindow>,
        sequence: u64,
    ) -> Option<Self> {
        let mut bytes = pool.acquire(source.len())?;
        bytes.extend_from_slice(source);
        Some(Self {
            bytes,
            pool: Arc::clone(pool),
            credit: Some(FrameCredit {
                window: Arc::clone(credits),
            }),
            sequence,
        })
    }

    /// The packet, owned by this process.
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[inline]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Return wire storage now, keeping the consumer's credit outstanding.
    pub fn into_credit(mut self) -> FrameCredit {
        self.credit.take().expect("a frame owns exactly one credit")
    }

    /// Re-validate the owned copy.
    ///
    /// The borrowed slice was validated before the copy; this exists so a
    /// consumer that only ever sees the owned form does not have to take the
    /// earlier validation on trust across a thread boundary. It re-reads the
    /// same bytes, so it cannot disagree unless the copy did.
    pub fn frame(&self) -> Result<crate::WireFrame<'_>, crate::WireError> {
        crate::validate(&self.bytes)
    }
}

impl Drop for PooledFrame {
    fn drop(&mut self) {
        let buffer = std::mem::take(&mut self.bytes);
        self.pool.release(buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pool's two bounds are not reachable through `FrameIngress` today: it
    /// sizes `max_idle` to one more than the credit window, and the ceiling
    /// cannot be lowered mid-session because the builder takes `self` by value.
    /// They are still enforced, and they are tested here rather than left as
    /// branches nothing exercises -- an unreachable guard is one a later change
    /// makes reachable with nobody watching, and both of these bound memory.
    fn frame(pool: &Arc<FramePool>, credits: &Arc<CreditWindow>, len: usize) -> PooledFrame {
        assert!(credits.try_acquire(), "the test window has a credit");
        PooledFrame::new(&vec![7u8; len], pool, credits, 1).expect("within the ceiling")
    }

    #[test]
    fn the_pool_retains_at_most_its_bound() {
        let pool = Arc::new(FramePool::new(2, 4096));
        let credits = Arc::new(CreditWindow::new(8));
        let frames: Vec<PooledFrame> = (0..5).map(|_| frame(&pool, &credits, 512)).collect();
        assert_eq!(
            pool.allocations(),
            5,
            "nothing to reuse while all five are out"
        );
        drop(frames);
        assert!(
            pool.idle_bytes() <= 2 * 512,
            "the pool kept {} bytes with a bound of two buffers",
            pool.idle_bytes()
        );
    }

    #[test]
    fn a_buffer_larger_than_the_ceiling_is_not_retained() {
        let pool = Arc::new(FramePool::new(4, 1024));
        let credits = Arc::new(CreditWindow::new(8));
        drop(frame(&pool, &credits, 512));
        let retained = pool.idle_bytes();
        assert!(retained >= 512, "a buffer within the ceiling comes back");

        // A buffer whose capacity exceeds the ceiling cannot be produced
        // through `PooledFrame::new` -- `acquire` refuses the length -- so the
        // release path is exercised directly, which is the only way this branch
        // is reachable at all.
        pool.release(Vec::with_capacity(4096));
        assert_eq!(
            pool.idle_bytes(),
            retained,
            "an over-sized buffer must not be retained: the ceiling can be lowered, and a \
             pool that kept one would make the lower ceiling a suggestion"
        );
    }

    #[test]
    fn a_packet_above_the_ceiling_gets_no_buffer() {
        let pool = Arc::new(FramePool::new(2, 128));
        let credits = Arc::new(CreditWindow::new(2));
        assert!(credits.try_acquire());
        assert!(
            PooledFrame::new(&[0u8; 129], &pool, &credits, 1).is_none(),
            "the pool refuses a packet above its ceiling rather than growing to it"
        );
    }

    #[test]
    fn the_credit_window_cannot_be_exceeded_or_driven_below_zero() {
        let window = CreditWindow::new(2);
        assert!(window.try_acquire());
        assert!(window.try_acquire());
        assert!(!window.try_acquire(), "the window is exactly two deep");
        assert_eq!(window.remaining(), 0);

        window.release();
        window.release();
        window.release();
        window.release();
        assert_eq!(
            window.in_flight(),
            0,
            "release saturates rather than wrapping"
        );
        assert_eq!(window.remaining(), 2);
    }

    /// The largest buffer that fits, not the first: a small frame taking the
    /// big buffer would leave the big frame to allocate a second one, and the
    /// pool would hold two where one was needed.
    #[test]
    fn acquire_takes_the_smallest_buffer_that_fits() {
        let pool = Arc::new(FramePool::new(4, 8192));
        let credits = Arc::new(CreditWindow::new(8));
        // Held together, then released together. Dropping the first before
        // taking the second would let the second reuse it, and the pool would
        // end up holding one buffer where this test needs two.
        let big = frame(&pool, &credits, 4096);
        let small_seed = frame(&pool, &credits, 64);
        drop((big, small_seed));
        let allocations = pool.allocations();
        assert_eq!(allocations, 2, "two live frames need two buffers");

        let small = frame(&pool, &credits, 32);
        assert_eq!(
            pool.allocations(),
            allocations,
            "the 64-byte buffer was reused"
        );
        let large = frame(&pool, &credits, 4000);
        assert_eq!(
            pool.allocations(),
            allocations,
            "the 4096-byte buffer was still there for the large frame"
        );
        drop((small, large));
    }
}
