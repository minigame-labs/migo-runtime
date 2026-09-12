//! Reusable canvas IDs without holding a TLS borrow across host callbacks.
//!
//! `push_if_absent` is the only insertion API: it guarantees uniqueness while
//! keeping the cost proportional to C (the number of distinct canvases) rather
//! than C² (what a plain `Vec::contains` check before each push would cost).
//! Below `CONTAINS_THRESHOLD` the check is a short linear scan with no
//! allocation; at or above it a per-frame `HashSet` serves O(1) lookups on
//! average.  The `HashSet` backing allocation is retained in the TLS pool so
//! a steady-state scene that always exceeds the threshold pays zero heap
//! events per frame after warm-up.

use std::cell::RefCell;
use std::collections::HashSet;

/// Below this many pending canvases, a linear Vec scan costs less than the
/// hash overhead.  The shop-open scene typically stays below 4; the threshold
/// is set at 8 so the fast path absorbs any small UI-heavy variation without
/// needing to keep a parallel set.
pub(crate) const CONTAINS_THRESHOLD: usize = 8;

/// Comparison counter for tests that prove the membership curve leaves the
/// quadratic shape.  Only compiled into test artefacts; production builds pay
/// no overhead.
#[cfg(test)]
thread_local! {
    static COMPARE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_compare_count() {
    COMPARE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn get_compare_count() -> usize {
    COMPARE_COUNT.with(std::cell::Cell::get)
}

/// Linear scan of the small Vec, counting comparisons in test builds.
///
/// Used by the small-path branch of `push_if_absent`.  In production builds
/// this compiles to a straight `contains` with no additional branches.
fn contains_counting(ids: &[u32], id: u32) -> bool {
    ids.iter().any(|&x| {
        #[cfg(test)]
        COMPARE_COUNT.with(|count| count.set(count.get() + 1));
        x == id
    })
}

struct TlsScratch {
    ids: Vec<u32>,
    /// Absent when all scenes so far have stayed below `CONTAINS_THRESHOLD`.
    /// Created on first threshold crossing and returned to the TLS pool on
    /// drop so subsequent frames of the same size allocate nothing.
    set: Option<HashSet<u32>>,
}

thread_local! {
    static SCRATCH: RefCell<TlsScratch> = const {
        RefCell::new(TlsScratch { ids: Vec::new(), set: None })
    };
}

pub(crate) struct MaterializeScratch {
    pub(crate) ids: Vec<u32>,
    /// Populated when `ids.len()` first reaches `CONTAINS_THRESHOLD`.
    /// Contains the same elements as `ids`; kept in lock-step so
    /// `push_if_absent` can rely on it without a rebuild each call.
    pub(crate) set: Option<HashSet<u32>>,
}

impl MaterializeScratch {
    pub(crate) fn take(capacity_limit: usize) -> Self {
        let (mut ids, set) = SCRATCH.with(|slot| {
            let mut cached = slot.borrow_mut();
            (
                std::mem::take(&mut cached.ids),
                std::mem::take(&mut cached.set),
            )
        });
        if ids.capacity() > capacity_limit {
            // A previous large scene must not defeat this frame's admission.
            ids = Vec::new();
        }
        ids.reserve_exact(capacity_limit);
        // The set is cleared on each take so it never carries stale IDs from
        // a prior frame into this one.
        let mut set = set;
        if let Some(s) = &mut set {
            s.clear();
        }
        Self { ids, set }
    }

    /// Insert `id` if it is not already pending materialisation.
    ///
    /// Below `CONTAINS_THRESHOLD` a linear Vec scan is used.  At or above the
    /// threshold a `HashSet` is built from the existing Vec contents (once per
    /// large frame) so all subsequent lookups are O(1) average.
    ///
    /// The TLS pool caches the `HashSet` allocation across frames: a
    /// steady-state scene that always exceeds the threshold pays zero heap
    /// events per frame after the first one.
    pub(crate) fn push_if_absent(&mut self, id: u32) {
        if let Some(set) = &mut self.set {
            // Large-path: O(1) average via the parallel HashSet.
            if set.insert(id) {
                self.ids.push(id);
            }
            return;
        }

        // Small-path: linear scan (comparison counted in test builds).
        if contains_counting(&self.ids, id) {
            return;
        }
        self.ids.push(id);

        // Transition to the large path when the threshold is crossed.
        // The one-time build copies at most CONTAINS_THRESHOLD elements.
        if self.ids.len() == CONTAINS_THRESHOLD {
            let set = self.set.get_or_insert_with(HashSet::new);
            set.extend(self.ids.iter().copied());
        }
    }

    /// Drain all pending IDs into `f`, then reset membership state so the
    /// scratch can collect a fresh set of IDs in the same decode pass.
    ///
    /// Must be used instead of calling `drain` directly: a plain `Vec::drain`
    /// empties `ids` but leaves the `set` with the old entries, causing
    /// `push_if_absent` to silently ignore any canvas that re-appears after
    /// the barrier.
    pub(crate) fn drain_into(&mut self, mut f: impl FnMut(u32)) {
        for id in self.ids.drain(..) {
            f(id);
        }
        // Reset the set so the next accumulation phase starts clean.
        if let Some(s) = &mut self.set {
            s.clear();
        }
    }
}

impl Drop for MaterializeScratch {
    fn drop(&mut self) {
        self.ids.clear();
        if let Some(s) = &mut self.set {
            s.clear();
        }
        // Retain at most 32 KiB per decoding thread.  Bigger unadmitted callers
        // can still decode, but cannot permanently enlarge this scratch cache.
        if self.ids.capacity() <= 8192 {
            let _ = SCRATCH.try_with(|slot| {
                let mut cached = slot.borrow_mut();
                if self.ids.capacity() > cached.ids.capacity() {
                    cached.ids = std::mem::take(&mut self.ids);
                }
                // Return the HashSet allocation to the pool so it is reused by
                // the next large frame, zeroing per-frame heap pressure.
                if cached.set.is_none() && self.set.is_some() {
                    cached.set = std::mem::take(&mut self.set);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The comparison count for the small-Vec path must be exactly C*(C-1)/2
    /// below the threshold: every insert scans all predecessors once.
    /// This pins the fast-path semantics independently of the large-path.
    #[test]
    fn small_vec_path_is_used_and_counted_below_threshold() {
        let below = CONTAINS_THRESHOLD - 1; // 7 distinct canvases
        reset_compare_count();
        let mut scratch = MaterializeScratch::take(below + 1);
        for id in 0u32..(below as u32) {
            scratch.push_if_absent(id);
        }
        let count = get_compare_count();
        let expected = below * (below - 1) / 2;
        assert_eq!(
            count, expected,
            "below threshold the Vec scan must make exactly {expected} comparisons \
             (got {count}); the small-path is the contract for the fast-path allocation test"
        );
        assert!(
            scratch.set.is_none(),
            "HashSet must not be built while under the threshold ({below} < {})",
            CONTAINS_THRESHOLD
        );
    }

    /// The total comparison count for C distinct canvases must stay at or
    /// below the small-path ceiling regardless of how large C grows.
    ///
    /// Before the HashSet path existed this test was RED: for C = 16 / 64 / 256
    /// the Vec scan produced C*(C-1)/2 comparisons, far above the bound.
    #[test]
    fn membership_count_leaves_quadratic_shape() {
        // Above-threshold ceiling: the small path runs at most
        // THRESHOLD*(THRESHOLD-1)/2 comparisons before the HashSet takes
        // over, so the total is bounded by that constant regardless of C.
        let max_comparisons = CONTAINS_THRESHOLD * (CONTAINS_THRESHOLD - 1) / 2;

        for &c in &[1u32, 4, 16, 64, 256] {
            reset_compare_count();
            let mut scratch = MaterializeScratch::take(c as usize + 1);
            for id in 0..c {
                scratch.push_if_absent(id);
            }
            let count = get_compare_count();
            if c < CONTAINS_THRESHOLD as u32 {
                let expected = (c as usize) * (c as usize - 1) / 2;
                assert_eq!(
                    count, expected,
                    "c={c}: small Vec path comparison count changed"
                );
            } else {
                assert!(
                    count <= max_comparisons,
                    "c={c}: comparison count {count} exceeds the linear bound \
                     {max_comparisons}; the O(C²) Vec-scan path is still active \
                     for large C"
                );
            }
        }
    }

    /// After a `drain_into` the scratch must accept re-insertions of previously
    /// drained IDs, because a canvas can appear on both sides of a GL barrier
    /// within one frame.
    #[test]
    fn drain_into_resets_membership_so_drained_ids_can_reappear() {
        let mut scratch = MaterializeScratch::take(32);
        // First accumulation: canvases 0..10 (exceeds threshold).
        for id in 0u32..10 {
            scratch.push_if_absent(id);
        }
        assert_eq!(scratch.ids.len(), 10);

        // Drain (simulates a GL-barrier materialize sweep).
        let mut materialized: Vec<u32> = Vec::new();
        scratch.drain_into(|id| materialized.push(id));
        assert_eq!(materialized.len(), 10);
        assert!(scratch.ids.is_empty());

        // Second accumulation: canvas 5 appeared again (same frame, new batch).
        scratch.push_if_absent(5);
        assert_eq!(
            scratch.ids,
            vec![5],
            "canvas 5 must be re-accepted after a drain; otherwise its second batch \
             is never materialized and the renderer sees stale pixels"
        );
    }

    /// All IDs that were pushed are seen by drain_into, even across the
    /// small→large transition.
    #[test]
    fn drain_into_yields_all_pushed_ids_once() {
        let mut scratch = MaterializeScratch::take(32);
        // Push more than threshold, including duplicates.
        for id in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 3, 7] {
            scratch.push_if_absent(id);
        }
        let mut seen: Vec<u32> = Vec::new();
        scratch.drain_into(|id| seen.push(id));
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }
}
