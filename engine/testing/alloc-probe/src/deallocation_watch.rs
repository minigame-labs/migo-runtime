//! Fixed-allocation dealloc identity across migrating executor workers.
use std::sync::atomic::{AtomicUsize, Ordering};

// A watch never allocates or calls user code inside GlobalAlloc. Slots are
// reserved before handing off a live fixed allocation, then released by RAII.
const SLOTS: usize = 32;
struct Slot {
    state: AtomicUsize, // low 3 bits: 0 free, 1 reserved, 2 watching, 3 freeing, 4 freed; high bits: generation
    pointer: AtomicUsize,
    bytes: AtomicUsize,
}
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static WATCHES: [Slot; SLOTS] = [const {
    Slot {
        state: AtomicUsize::new(0),
        pointer: AtomicUsize::new(0),
        bytes: AtomicUsize::new(0),
    }
}; SLOTS];

/// Watches one fixed allocation's actual `CountingAllocator::dealloc` return.
///
/// Register before another thread can free the allocation. No memory is read
/// through the pointer. Reallocations are outside this fixed-allocation API;
/// an unused watch can be cancelled by dropping it. Multiple watches of the
/// same address are supported. This utility remains a dev-dependency only.
pub struct DeallocationWatch {
    index: usize,
}
impl DeallocationWatch {
    /// Reserve a non-allocating observation slot for a live allocation address.
    /// Returns None for null or when all observation slots are occupied.
    pub fn new(pointer: *const u8) -> Option<Self> {
        if pointer.is_null() {
            return None;
        }
        for (index, slot) in WATCHES.iter().enumerate() {
            let state = slot.state.load(Ordering::Acquire);
            if state & 7 == 0
                && slot
                    .state
                    .compare_exchange(
                        state,
                        state.wrapping_add(1),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            {
                slot.pointer.store(pointer as usize, Ordering::Relaxed);
                slot.bytes.store(0, Ordering::Relaxed);
                ACTIVE.fetch_add(1, Ordering::Release);
                slot.state.store(state.wrapping_add(2), Ordering::Release);
                return Some(Self { index });
            }
        }
        None
    }
    /// The exact allocation Layout size after the wrapped allocator freed it.
    /// None means no matching dealloc has completed during this watch.
    pub fn freed_bytes(&self) -> Option<usize> {
        let slot = &WATCHES[self.index];
        (slot.state.load(Ordering::Acquire) & 7 == 4).then(|| slot.bytes.load(Ordering::Relaxed))
    }
}
impl Drop for DeallocationWatch {
    fn drop(&mut self) {
        let slot = &WATCHES[self.index];
        loop {
            let state = slot.state.load(Ordering::Acquire);
            if state & 7 == 3 {
                std::hint::spin_loop();
                continue;
            }
            if slot
                .state
                .compare_exchange(
                    state,
                    (state & !7).wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                break;
            }
        }
        slot.pointer.store(0, Ordering::Relaxed);
        slot.bytes.store(0, Ordering::Relaxed);
        ACTIVE.fetch_sub(1, Ordering::Release);
        let generation = slot.state.load(Ordering::Relaxed) & !7;
        slot.state
            .store(generation.wrapping_add(8), Ordering::Release);
    }
}

pub(super) fn claim(pointer: *mut u8) -> u32 {
    if ACTIVE.load(Ordering::Acquire) == 0 {
        return 0;
    }
    let mut matches = 0u32;
    for (index, slot) in WATCHES.iter().enumerate() {
        let state = slot.state.load(Ordering::Acquire);
        if state & 7 == 2
            && slot.pointer.load(Ordering::Relaxed) == pointer as usize
            && slot
                .state
                .compare_exchange(
                    state,
                    state.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            matches |= 1u32 << index;
        }
    }
    matches
}
pub(super) fn complete(mut matches: u32, bytes: usize) {
    while matches != 0 {
        let index = matches.trailing_zeros() as usize;
        matches &= matches - 1;
        let slot = &WATCHES[index];
        slot.bytes.store(bytes, Ordering::Relaxed);
        slot.state.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CountingAllocator;
    use std::alloc::{GlobalAlloc, Layout};
    fn allocate(layout: Layout) -> *mut u8 {
        // SAFETY: this nonzero valid Layout is released once by the test.
        let p = unsafe { CountingAllocator::system().alloc(layout) };
        assert!(!p.is_null());
        p
    }
    #[test]
    fn actual_dealloc_is_visible_after_return_and_an_unrelated_free_is_not() {
        let layout = Layout::from_size_align(257, 8).unwrap();
        let p = allocate(layout);
        let watch = DeallocationWatch::new(p).unwrap();
        let other = allocate(layout);
        // SAFETY: other is a distinct live allocation of this exact Layout.
        unsafe { CountingAllocator::system().dealloc(other, layout) };
        assert_eq!(watch.freed_bytes(), None);
        // SAFETY: p is live and is freed exactly once.
        unsafe { CountingAllocator::system().dealloc(p, layout) };
        assert_eq!(watch.freed_bytes(), Some(257));
    }
    #[test]
    fn identity_follows_a_foreign_worker_without_comparing_thread_local_totals() {
        let layout = Layout::from_size_align(1024, 16).unwrap();
        let p = allocate(layout);
        let watch = DeallocationWatch::new(p).unwrap();
        let pointer = p as usize;
        std::thread::spawn(move || {
            // SAFETY: ownership of this live allocation moves to this worker.
            unsafe { CountingAllocator::system().dealloc(pointer as *mut u8, layout) };
        })
        .join()
        .unwrap();
        assert_eq!(watch.freed_bytes(), Some(1024));
    }
    #[test]
    fn multiple_watchers_and_cancelled_slots_remain_independent() {
        let layout = Layout::from_size_align(33, 8).unwrap();
        let p = allocate(layout);
        let a = DeallocationWatch::new(p).unwrap();
        let b = DeallocationWatch::new(p).unwrap();
        drop(a);
        let c = DeallocationWatch::new(p).unwrap();
        // SAFETY: exactly one deallocation for this live allocation.
        unsafe { CountingAllocator::system().dealloc(p, layout) };
        assert_eq!(b.freed_bytes(), Some(33));
        assert_eq!(c.freed_bytes(), Some(33));
    }
    #[test]
    fn observation_stays_pending_until_the_wrapped_deallocator_returns() {
        struct BeforeReturn<'a>(&'a DeallocationWatch);
        // SAFETY: this test allocator forwards the same valid layout and pointer
        // to System exactly once; observations never read the freed allocation.
        unsafe impl GlobalAlloc for BeforeReturn<'_> {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                unsafe { std::alloc::System.alloc(layout) }
            }
            unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
                assert_eq!(self.0.freed_bytes(), None);
                unsafe { std::alloc::System.dealloc(pointer, layout) };
                assert_eq!(self.0.freed_bytes(), None);
            }
        }
        let layout = Layout::from_size_align(17, 8).unwrap();
        let p = allocate(layout);
        let watch = DeallocationWatch::new(p).unwrap();
        let allocator = CountingAllocator {
            inner: BeforeReturn(&watch),
        };
        // SAFETY: p is still live and this is its only deallocation.
        unsafe { allocator.dealloc(p, layout) };
        assert_eq!(watch.freed_bytes(), Some(17));
        assert!(DeallocationWatch::new(std::ptr::null()).is_none());
    }
}
