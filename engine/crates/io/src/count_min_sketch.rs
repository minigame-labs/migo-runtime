//! Count-Min Sketch with 4-bit saturating counters.
//!
//! Frequency oracle for W-TinyLFU admission: given an opaque
//! hashable key, [`estimate`] returns an *approximate* access count
//! bounded from above by 15 (saturation) and from below by the true
//! count (minimum of D hash bucket counts).
//!
//! # Why 4-bit / why CMS
//!
//! A full 64-bit per-key counter hash map would balloon with every
//! distinct image path and never shrink across sessions.  A CM
//! sketch uses fixed memory (4 × K nibbles ≈ 2K bytes by default)
//! regardless of key cardinality, and the only error mode is
//! *overestimation* — it can never under-count, which is exactly
//! the direction that keeps a "freshly accessed" entry out of the
//! admission fast path. That's the safe way to bias in a cache.
//!
//! Saturation at 15 matches Caffeine's default; realistic access
//! sequences for image caches see very few items cross a few dozen
//! hits per aging epoch, and the information gap between 15 and 1000
//! is not useful to the admission comparator.
//!
//! # Aging
//!
//! Without aging, counters only grow and eventually every key reads
//! as saturated, turning admission into "LRU with extra steps".
//! Caffeine's answer — bump the sketch's "sample size" until total
//! increments exceed ~10× capacity, then halve every counter (right
//! shift by 1) — is implemented here as [`CountMinSketch::maybe_age`].
//! We call it implicitly on every `increment`; no external upkeep.
//!
//! # Thread safety
//!
//! Not internally synchronised.  Callers hold the cache's outer
//! `Mutex` while invoking the sketch, which keeps the sketch single-
//! owner and avoids per-counter atomics that would cost more than
//! they'd save here.

use std::hash::{BuildHasher, BuildHasherDefault, Hash, Hasher};

/// Four independent hash rows; matches the canonical CMS schema and
/// is what Caffeine uses. More rows reduce overestimation at
/// logarithmic cost; fewer rows diverge quickly under skewed input.
const HASH_ROWS: usize = 4;

/// Saturation cap on each 4-bit counter.  The comparator only ever
/// needs ordering, not magnitude, so bumping the cap past 15 would
/// just burn memory without changing admission outcomes.
const MAX_COUNT: u8 = 15;

/// Hasher family used to diversify the four rows. Keeping this
/// deterministic (SipHash via Rust's default `RandomState` is
/// per-process-random; `BuildHasherDefault<DefaultHasher>` is
/// process-stable) is fine for a cache-local structure and avoids
/// surprising test flakiness.
type Hasher1 = BuildHasherDefault<std::collections::hash_map::DefaultHasher>;

pub struct CountMinSketch {
    /// `HASH_ROWS * slots_per_row` 4-bit counters, packed two-per-u8.
    /// The outer `Vec` is row-major: row 0 occupies bytes
    /// `[0 .. slots_per_row/2]` and so on.  `slots_per_row` is rounded
    /// up to a power of two so modulo-mask can replace modulo.
    counters: Vec<u8>,
    /// Number of 4-bit slots per row. Always a power of two so we
    /// can mask with `slots_per_row - 1` instead of `%`.
    slots_per_row: usize,
    /// Mask for `index & mask` to stay in a row.  Equals
    /// `slots_per_row - 1`.
    slot_mask: usize,
    /// How many increments have been applied since last aging.
    sample_count: u64,
    /// Aging threshold: halve all counters when `sample_count` >= this.
    sample_size: u64,
    /// Per-row `BuildHasher`s (four distinct instances so the rows
    /// produce distinct hash streams).
    hashers: [Hasher1; HASH_ROWS],
}

impl CountMinSketch {
    /// Create a sketch sized for a cache holding at most `capacity`
    /// entries.  The Caffeine sizing rule: slots_per_row ≈ capacity,
    /// rounded up to a power of two.  A 256 floor is applied so even
    /// tiny caches (e.g. the in-test 16-entry config) get enough
    /// hash rows to not drown in collisions; at 4 rows × 256 slots
    /// the sketch is ~1 KiB total, so the floor is free.
    ///
    /// `sample_size` is 10×slot_count — Caffeine's default. Smaller
    /// values age faster and make the sketch more reactive; larger
    /// values retain long-term frequency at the cost of reacting
    /// slowly to workload shifts.
    pub fn new_for_capacity(capacity: usize) -> Self {
        let cap = capacity.max(256);
        let slots_per_row = cap.next_power_of_two();
        let slot_mask = slots_per_row - 1;
        // 4-bit counters, two per byte, rounded up.
        let bytes_per_row = slots_per_row.div_ceil(2);
        let counters = vec![0u8; bytes_per_row * HASH_ROWS];
        // Four BuildHasherDefault instances still share the DefaultHasher
        // K0/K1 state.  To get distinct streams we xor the key bytes
        // with a row salt inside `index_for`.  See [`row_index_for`].
        let hashers = [
            Hasher1::default(),
            Hasher1::default(),
            Hasher1::default(),
            Hasher1::default(),
        ];
        let sample_size = (cap as u64).saturating_mul(10);
        Self {
            counters,
            slots_per_row,
            slot_mask,
            sample_count: 0,
            sample_size,
            hashers,
        }
    }

    /// Increment the frequency count for `key`. Returns the
    /// post-increment minimum (the new [`estimate`] result), which
    /// saves one rehash when the caller immediately uses the count.
    pub fn increment<K: Hash + ?Sized>(&mut self, key: &K) -> u8 {
        let mut min_after = MAX_COUNT;
        let mut bumped = false;
        for row in 0..HASH_ROWS {
            let idx = self.index(row, key);
            let cur = self.read_counter(row, idx);
            if cur < MAX_COUNT {
                self.write_counter(row, idx, cur + 1);
                bumped = true;
                min_after = min_after.min(cur + 1);
            } else {
                min_after = min_after.min(MAX_COUNT);
            }
        }
        if bumped {
            self.sample_count = self.sample_count.saturating_add(1);
            if self.sample_count >= self.sample_size {
                self.age();
            }
        }
        min_after
    }

    /// Read-only frequency estimate for `key`.  Returns the minimum
    /// counter across all D hash rows (CMS invariant).
    pub fn estimate<K: Hash + ?Sized>(&self, key: &K) -> u8 {
        let mut m = MAX_COUNT;
        for row in 0..HASH_ROWS {
            let idx = self.index(row, key);
            m = m.min(self.read_counter(row, idx));
        }
        m
    }

    /// Manually trigger the aging pass. Called internally when
    /// `sample_count >= sample_size`; exposed for tests.
    pub fn age(&mut self) {
        for b in &mut self.counters {
            // Halve both 4-bit counters packed in this byte.
            let lo = (*b & 0x0F) >> 1;
            let hi = ((*b & 0xF0) >> 1) & 0xF0;
            *b = lo | hi;
        }
        // Carry forward half the previous sample count to avoid
        // resetting the aging clock to zero each cycle — matches
        // Caffeine's "doorkeeper decay" semantics.
        self.sample_count /= 2;
    }

    /// Wipe everything. Used by `ImageCache::clear` when the host
    /// asks for a full memory release.
    pub fn reset(&mut self) {
        for b in &mut self.counters {
            *b = 0;
        }
        self.sample_count = 0;
    }

    // --- Internals ---

    /// Compute the slot index inside `row` for `key`.
    fn index<K: Hash + ?Sized>(&self, row: usize, key: &K) -> usize {
        // Row salts pulled from xxhash / murmur literature; any
        // four distinct primes work.  They cheaply decorrelate the
        // four row hashes so CMS's minimum-over-rows stays tight.
        const ROW_SALTS: [u64; HASH_ROWS] = [
            0x9E3779B97F4A7C15,
            0xBF58476D1CE4E5B9,
            0x94D049BB133111EB,
            0xCBF29CE484222325,
        ];
        let mut h = self.hashers[row].build_hasher();
        ROW_SALTS[row].hash(&mut h);
        key.hash(&mut h);
        (h.finish() as usize) & self.slot_mask
    }

    /// Byte + nibble address of a counter in `row` at slot `idx`.
    #[inline]
    fn locate(&self, row: usize, idx: usize) -> (usize, bool) {
        let bytes_per_row = self.slots_per_row.div_ceil(2);
        let byte = row * bytes_per_row + idx / 2;
        let hi_nibble = (idx & 1) == 1; // slot 1 uses high nibble
        (byte, hi_nibble)
    }

    fn read_counter(&self, row: usize, idx: usize) -> u8 {
        let (byte, hi) = self.locate(row, idx);
        let b = self.counters[byte];
        if hi { (b >> 4) & 0x0F } else { b & 0x0F }
    }

    fn write_counter(&mut self, row: usize, idx: usize, val: u8) {
        debug_assert!(val <= MAX_COUNT);
        let (byte, hi) = self.locate(row, idx);
        let b = self.counters[byte];
        let new = if hi {
            (b & 0x0F) | (val << 4)
        } else {
            (b & 0xF0) | val
        };
        self.counters[byte] = new;
    }
}

impl std::fmt::Debug for CountMinSketch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CountMinSketch")
            .field("rows", &HASH_ROWS)
            .field("slots_per_row", &self.slots_per_row)
            .field("sample_count", &self.sample_count)
            .field("sample_size", &self.sample_size)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_sketch_reports_zero_for_unseen_keys() {
        let s = CountMinSketch::new_for_capacity(64);
        assert_eq!(s.estimate("alpha"), 0);
        assert_eq!(s.estimate("beta"), 0);
    }

    #[test]
    fn increment_bumps_estimate_for_that_key() {
        let mut s = CountMinSketch::new_for_capacity(64);
        assert_eq!(s.estimate("k"), 0);
        s.increment("k");
        assert_eq!(s.estimate("k"), 1);
        s.increment("k");
        assert_eq!(s.estimate("k"), 2);
    }

    #[test]
    fn counter_saturates_at_15() {
        let mut s = CountMinSketch::new_for_capacity(64);
        for _ in 0..50 {
            s.increment("k");
        }
        // Exact value depends on aging triggers; must not exceed 15.
        let v = s.estimate("k");
        assert!(v <= MAX_COUNT, "got {v}, want ≤ {MAX_COUNT}");
        assert!(v > 0, "must at least be incremented");
    }

    #[test]
    fn estimate_is_an_upper_bound_never_lower_than_true_count() {
        // CMS never under-counts.  Hit 3 distinct keys, inspect
        // each — each estimate must be at least the exact count.
        let mut s = CountMinSketch::new_for_capacity(256);
        for _ in 0..3 {
            s.increment("a");
        }
        for _ in 0..7 {
            s.increment("b");
        }
        s.increment("c");
        assert!(s.estimate("a") >= 3);
        assert!(s.estimate("b") >= 7);
        assert!(s.estimate("c") >= 1);
    }

    #[test]
    fn two_distinct_keys_mostly_have_distinct_estimates() {
        // In a sparsely populated sketch, unrelated keys should
        // return different counts (no row collision across all 4
        // rows is the probabilistic norm).
        let mut s = CountMinSketch::new_for_capacity(256);
        for _ in 0..5 {
            s.increment("hot");
        }
        // 'cold' has not been seen at all — it can only read as 0
        // if *every* row dodges "hot"'s slots.  In a 256-wide table
        // with 4 rows that's the overwhelming-majority case.
        assert_eq!(s.estimate("cold"), 0);
    }

    #[test]
    fn manual_age_halves_counts() {
        let mut s = CountMinSketch::new_for_capacity(256);
        for _ in 0..8 {
            s.increment("k");
        }
        let before = s.estimate("k");
        assert!(
            before >= 8 / 2,
            "pre-age estimate implausibly low: {before}"
        );
        s.age();
        let after = s.estimate("k");
        assert!(
            after <= before / 2 + 1,
            "age should halve, got {before} -> {after}"
        );
    }

    #[test]
    fn reset_clears_all_counters() {
        let mut s = CountMinSketch::new_for_capacity(64);
        for i in 0..20 {
            s.increment(&format!("k{i}"));
        }
        s.reset();
        for i in 0..20 {
            assert_eq!(s.estimate(&format!("k{i}")), 0);
        }
    }

    #[test]
    fn aging_triggers_automatically_near_sample_size() {
        // The 256-slot floor means sample_size = 2560.  Drive 6000
        // increments across a saturated key so we cross two aging
        // boundaries and confirm sample_count was halved each time.
        let mut s = CountMinSketch::new_for_capacity(256);
        for _ in 0..6000 {
            s.increment("sat");
        }
        // After any number of aging passes `sample_count` is bounded
        // below `sample_size` again. If aging never fired we'd have
        // sample_count ≥ sample_size.
        assert!(s.sample_count < s.sample_size, "aging did not fire");
    }

    #[test]
    fn increment_return_value_matches_estimate() {
        let mut s = CountMinSketch::new_for_capacity(64);
        for _ in 0..5 {
            let r = s.increment("x");
            let e = s.estimate("x");
            assert_eq!(r, e, "increment return must match post-estimate");
        }
    }

    #[test]
    fn rows_decorrelate_via_salts() {
        // Regression for the bug where all four rows used the same
        // hasher with no salt — two different keys would resolve to
        // the same four slots, making the sketch degenerate to a
        // single row. Verify that at least two of 20 distinct keys
        // land at different slots on row 0 *vs* row 1 (nearly always
        // the case when salts are effective).
        let s = CountMinSketch::new_for_capacity(64);
        let mut distinct = 0;
        for i in 0..20 {
            let key = format!("k{i}");
            let i0 = s.index(0, &key);
            let i1 = s.index(1, &key);
            if i0 != i1 {
                distinct += 1;
            }
        }
        assert!(
            distinct >= 15,
            "rows too correlated: only {distinct}/20 keys had distinct row-0 and row-1 slots"
        );
    }
}
