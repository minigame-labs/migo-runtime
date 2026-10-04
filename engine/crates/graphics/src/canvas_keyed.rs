//! A per-canvas table for state the render thread reaches on every GL command.
//!
//! **A hash map was the wrong shape for this and the per-command path paid for
//! it twice**, once in `renderergl::handler`'s dedup shadow and once in
//! `webgl_gpu_budget`'s binding ledger. Both are keyed by canvas, both hold one
//! entry for a single-canvas game, and both were `HashMap<CanvasId, _>` built
//! for a handful of entries — so every lookup was a SipHash-1-3 of a `u32` to
//! find the only candidate. Measured in this crate at the shipped
//! `opt-level = "z"`: 19 ns per lookup, and a frame of 300 sprite batches runs
//! thousands of them.
//!
//! Two properties of the workload make a keyed scan strictly better:
//!
//! - The entry count is tiny and bounded by how many canvases a page creates.
//! - Commands arrive in runs that share a canvas.
//!
//! So the hot path is a single `u32` compare against the entry the previous
//! command resolved to (1.0 ns/lookup, 19x), and the fallback is a linear scan
//! that still beat the hash map at four canvases switching on every single
//! command (3.1 vs 18.8 ns).
//!
//! The memo is an *index*, never a pointer, and it is re-checked against the
//! key on every use — a stale memo is a miss, never a wrong canvas. An earlier
//! revision promoted the hot entry to the front of the vector instead; that
//! moved the whole value (a `CanvasGLState` is eleven hash tables, about half a
//! kilobyte) on every switch and measured 3x *slower* than the hash map it
//! replaced on alternating canvases. Nothing here moves an existing entry.
//!
//! [`CanvasKeyed::entry`] mirrors `HashMap`'s shape on purpose, so adopting
//! this stays a container swap rather than a rewrite of the call sites.

use shared::protocol::render_cmd::CanvasId;

/// Reserved up front so the canvas counts that occur never move a value.
///
/// This is load-bearing for `CanvasManager::begin_canvas2d_gl_scope_for`, which
/// takes a `*mut CanvasGLState` out of one of these tables and holds it across
/// a Skia scope. That is sound only while the backing store does not move —
/// the same obligation the `HashMap` this replaced carried, since a `HashMap`
/// likewise invalidates references when it grows, but now stated and testable.
pub(crate) const CANVAS_KEYED_RESERVE: usize = 4;

#[derive(Debug)]
pub(crate) struct CanvasKeyed<V> {
    entries: Vec<(CanvasId, V)>,
    /// Index the previous lookup resolved to. Always re-checked against the
    /// key, so correctness never depends on it being current.
    hot: usize,
}

impl<V> Default for CanvasKeyed<V> {
    fn default() -> Self {
        Self {
            entries: Vec::with_capacity(CANVAS_KEYED_RESERVE),
            hot: 0,
        }
    }
}

/// Pending `entry(id)` lookup, resolved by [`CanvasKeyedEntry::or_default`].
pub(crate) struct CanvasKeyedEntry<'a, V> {
    table: &'a mut CanvasKeyed<V>,
    id: CanvasId,
}

impl<'a, V: Default> CanvasKeyedEntry<'a, V> {
    /// Resolve to this canvas's value, creating a default one if the canvas has
    /// not been seen.
    #[inline]
    pub(crate) fn or_default(self) -> &'a mut V {
        self.table.value_mut(self.id)
    }
}

impl<V> CanvasKeyed<V> {
    #[inline]
    pub(crate) fn entry(&mut self, id: CanvasId) -> CanvasKeyedEntry<'_, V> {
        CanvasKeyedEntry { table: self, id }
    }

    #[inline]
    pub(crate) fn get(&self, id: &CanvasId) -> Option<&V> {
        // Reads the memo but cannot refresh it — `&self`. Still worth checking:
        // the caller that just resolved this canvas through a `&mut` path is
        // the common one.
        if let Some((key, value)) = self.entries.get(self.hot)
            && key == id
        {
            return Some(value);
        }
        self.entries
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, value)| value)
    }

    /// Like [`Self::get`] but refreshes the memo, and mutable.
    ///
    /// Distinct from [`Self::value_mut`] because this must not create: callers
    /// use the `None` to mean "this canvas has no 2D context", which is a real
    /// answer rather than a reason to make one.
    #[inline]
    pub(crate) fn get_mut(&mut self, id: &CanvasId) -> Option<&mut V> {
        if let Some((key, _)) = self.entries.get(self.hot)
            && key == id
        {
            return Some(&mut self.entries[self.hot].1);
        }
        let pos = self.entries.iter().position(|(key, _)| key == id)?;
        self.hot = pos;
        Some(&mut self.entries[pos].1)
    }

    #[inline]
    pub(crate) fn contains_key(&self, id: &CanvasId) -> bool {
        self.entries.iter().any(|(key, _)| key == id)
    }

    /// Replace this canvas's value wholesale, as context recreation does.
    #[inline]
    pub(crate) fn insert(&mut self, id: CanvasId, value: V) -> Option<V> {
        match self.entries.iter_mut().find(|(key, _)| *key == id) {
            Some(slot) => Some(std::mem::replace(&mut slot.1, value)),
            None => {
                self.entries.push((id, value));
                None
            }
        }
    }

    #[inline]
    pub(crate) fn remove(&mut self, id: &CanvasId) -> Option<V> {
        let pos = self.entries.iter().position(|(key, _)| key == id)?;
        let removed = self.entries.swap_remove(pos).1;
        // `swap_remove` moved the last entry into `pos`, so the memo now names
        // a different canvas. Correctness does not rest on this reset —
        // `value_mut` re-checks the key, which is what makes a stale memo a
        // miss rather than a wrong answer — so this is only a hint that saves
        // the next lookup one wasted compare.
        self.hot = 0;
        Some(removed)
    }

    /// Every canvas's value. Used by the sweeps that must run for state GL
    /// shares across the whole EGL share group — texture and program deletion.
    #[inline]
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries.iter_mut().map(|(_, value)| value)
    }

    /// Every canvas that has a value.
    #[inline]
    pub(crate) fn keys(&self) -> impl Iterator<Item = &CanvasId> {
        self.entries.iter().map(|(id, _)| id)
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.hot = 0;
    }

    /// Take every entry, leaving the table empty.
    ///
    /// For teardown, which drops each canvas's value under its own GL context
    /// and so needs to own them one at a time. The allocation is given back
    /// with them — this is not a per-frame path.
    #[inline]
    pub(crate) fn drain(&mut self) -> impl Iterator<Item = (CanvasId, V)> + '_ {
        self.hot = 0;
        self.entries.drain(..)
    }

    /// Reserve room for `additional` more canvases, reporting failure instead
    /// of aborting.
    ///
    /// Mirrors `HashMap::try_reserve`, which
    /// [`crate::webgl_gpu_budget::WebGlGpuBudget::create_texture`] uses to turn
    /// an allocation failure into `GpuAllocationError::OutOfMemory`. A budget
    /// ledger that aborts the process on a failed reserve would defeat the
    /// point of having a budget.
    #[inline]
    pub(crate) fn try_reserve(
        &mut self,
        additional: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        self.entries.try_reserve(additional)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

impl<V: Default> CanvasKeyed<V> {
    #[inline]
    pub(crate) fn value_mut(&mut self, id: CanvasId) -> &mut V {
        // The run of commands this one belongs to almost always resolved here
        // already.
        if let Some((key, _)) = self.entries.get(self.hot)
            && *key == id
        {
            return &mut self.entries[self.hot].1;
        }
        self.resolve(id)
    }

    /// Out of line so the memo hit stays a compare and a return.
    #[inline(never)]
    fn resolve(&mut self, id: CanvasId) -> &mut V {
        match self.entries.iter().position(|(key, _)| *key == id) {
            Some(pos) => self.hot = pos,
            None => {
                self.entries.push((id, V::default()));
                self.hot = self.entries.len() - 1;
            }
        }
        &mut self.entries[self.hot].1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload whose value is derivable from its canvas id, so a lookup that
    /// returns the wrong canvas is visible rather than merely suspicious.
    #[derive(Debug, Default, PartialEq, Eq)]
    struct Marked(u32);

    fn mark(table: &mut CanvasKeyed<Marked>, id: CanvasId) {
        table.entry(id).or_default().0 = id * 1000 + 7;
    }
    fn read(table: &CanvasKeyed<Marked>, id: CanvasId) -> Option<u32> {
        table.get(&id).map(|m| m.0)
    }

    #[test]
    fn a_repeated_lookup_of_the_same_canvas_resolves_to_one_value() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        t.entry(1).or_default().0 = 42;
        assert_eq!(t.entry(1).or_default().0, 42);
        assert_eq!(t.len(), 1, "the second lookup created a second entry");
    }

    #[test]
    fn distinct_canvases_get_distinct_values() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        for id in [1u32, 2, 7, 99] {
            mark(&mut t, id);
        }
        assert_eq!(t.len(), 4);
        for id in [1u32, 2, 7, 99] {
            assert_eq!(read(&t, id), Some(id * 1000 + 7), "canvas {id}");
        }
    }

    /// The one way an index memo can be wrong: `remove` uses `swap_remove`, so
    /// the entry that lands in the removed slot is a *different* canvas. A memo
    /// trusted without re-checking its key hands that canvas's value to whoever
    /// asks next — which would let one canvas's dedup decisions be made against
    /// another canvas's driver state, or one canvas's textures be charged to
    /// another's GPU budget.
    #[test]
    fn a_memo_left_over_from_a_removed_canvas_never_resolves_to_another_canvas() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        for id in [1u32, 2, 3, 4] {
            mark(&mut t, id);
        }
        // Make canvas 2 the memoised entry, then remove it. `swap_remove` moves
        // canvas 4 into slot 1, which the stale memo still points at.
        assert_eq!(t.entry(2).or_default().0, 2007);
        assert!(t.remove(&2).is_some());

        assert!(read(&t, 2).is_none(), "the removed canvas is still present");
        for id in [1u32, 3, 4] {
            assert_eq!(
                t.entry(id).or_default().0,
                id * 1000 + 7,
                "canvas {id} resolved to another canvas's value after a removal"
            );
        }
        assert_eq!(t.len(), 3);
    }

    /// Removing the *last* entry: the memo can point one past the end, which
    /// must read as a miss and not panic.
    #[test]
    fn a_memo_pointing_past_the_end_is_a_miss_not_a_panic() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        mark(&mut t, 1);
        mark(&mut t, 2);
        assert_eq!(t.entry(2).or_default().0, 2007);
        assert!(t.remove(&2).is_some());

        assert_eq!(t.entry(1).or_default().0, 1007);
        // And the removed id is re-creatable, fresh.
        assert_eq!(t.entry(2).or_default().0, 0);
    }

    #[test]
    fn insert_replaces_the_value_and_hands_back_the_old_one() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        mark(&mut t, 5);
        assert_eq!(
            t.insert(5, Marked(1)),
            Some(Marked(5007)),
            "the previous value was dropped instead of returned"
        );
        assert_eq!(read(&t, 5), Some(1));
        assert_eq!(t.len(), 1, "insert over an existing canvas duplicated it");
    }

    #[test]
    fn insert_of_an_unseen_canvas_adds_it_and_reports_no_previous() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        assert!(t.insert(3, Marked(0)).is_none());
        assert_eq!(t.len(), 1);
    }

    /// The share-group sweeps (program and texture deletion) depend on this
    /// reaching every canvas — a sweep that misses one leaves a shadow claiming
    /// a deleted object is still bound.
    #[test]
    fn values_mut_reaches_every_canvas() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        for id in [1u32, 4, 8] {
            mark(&mut t, id);
        }
        let mut seen = 0;
        for value in t.values_mut() {
            value.0 = 0;
            seen += 1;
        }
        assert_eq!(seen, 3);
        for id in [1u32, 4, 8] {
            assert_eq!(
                read(&t, id),
                Some(0),
                "canvas {id} was skipped by the sweep"
            );
        }
    }

    #[test]
    fn clear_drops_every_canvas_and_the_memo_with_them() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        mark(&mut t, 1);
        mark(&mut t, 2);
        t.clear();
        assert_eq!(t.len(), 0);
        // A lookup after `clear` must build a fresh value, not resurrect one.
        assert_eq!(t.entry(1).or_default().0, 0);
    }

    /// `get` has to find a canvas that is not the memoised one — both users ask
    /// about canvases other than the one the last command touched.
    #[test]
    fn get_finds_a_canvas_that_is_not_the_memoised_one() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        mark(&mut t, 1);
        mark(&mut t, 2);
        // The memo now points at canvas 2.
        assert_eq!(read(&t, 1), Some(1007));
        assert_eq!(read(&t, 2), Some(2007));
    }

    /// `get_mut` must not create. Callers read its `None` as "this canvas has
    /// no 2D context", which is a real answer — a version that made one would
    /// hand back an unusable context and lose the distinction.
    #[test]
    fn get_mut_reports_an_absent_canvas_rather_than_creating_one() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        mark(&mut t, 1);
        assert!(t.get_mut(&9).is_none());
        assert_eq!(t.len(), 1, "get_mut created an entry for an absent canvas");
    }

    /// `get_mut` refreshes the memo, so the same canvas asked twice in a row —
    /// which is what a Canvas2D command does, once to classify damage and once
    /// to run — resolves through it the second time.
    #[test]
    fn get_mut_finds_a_canvas_that_is_not_the_memoised_one() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        mark(&mut t, 1);
        mark(&mut t, 2);
        // The memo points at canvas 2.
        t.get_mut(&1).expect("canvas 1 exists").0 = 77;
        assert_eq!(read(&t, 1), Some(77));
        assert_eq!(read(&t, 2), Some(2007), "canvas 2 was written instead");
        // And again, now through the memo.
        t.get_mut(&1).expect("canvas 1 exists").0 = 78;
        assert_eq!(read(&t, 1), Some(78));
        assert_eq!(read(&t, 2), Some(2007));
    }

    #[test]
    fn contains_key_answers_for_present_and_absent_canvases() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        mark(&mut t, 4);
        assert!(t.contains_key(&4));
        assert!(!t.contains_key(&5));
        assert!(t.remove(&4).is_some());
        assert!(!t.contains_key(&4));
    }

    /// `drain` hands every entry over and leaves the table empty — teardown
    /// drops each canvas's value under its own GL context, so it needs to own
    /// them one at a time.
    #[test]
    fn drain_yields_every_entry_and_empties_the_table() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        for id in [1u32, 6, 11] {
            mark(&mut t, id);
        }
        let mut drained: Vec<(CanvasId, u32)> = t.drain().map(|(id, m)| (id, m.0)).collect();
        drained.sort_unstable();
        assert_eq!(drained, vec![(1, 1007), (6, 6007), (11, 11007)]);
        assert_eq!(t.len(), 0);
        // And the memo went with them: a lookup after the drain must miss.
        assert!(t.get_mut(&1).is_none());
    }

    /// See [`CANVAS_KEYED_RESERVE`]: a pointer taken out of this table and held
    /// across a Skia scope dangles if adding a canvas reallocates.
    #[test]
    fn the_reserved_capacity_covers_the_documented_canvas_count_without_moving() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        let first = t.entry(1).or_default() as *mut Marked;
        for id in 2..=CANVAS_KEYED_RESERVE as u32 {
            t.entry(id).or_default();
        }
        assert_eq!(
            t.entry(1).or_default() as *mut Marked,
            first,
            "adding canvases up to the reserve moved an existing value, so a \
             pointer held across a Skia scope would dangle"
        );
    }

    /// The GPU budget turns a failed reserve into `OutOfMemory` rather than
    /// letting the process abort, so the fallible path has to exist and work.
    #[test]
    fn try_reserve_grows_the_table_without_disturbing_it() {
        let mut t: CanvasKeyed<Marked> = CanvasKeyed::default();
        mark(&mut t, 1);
        t.try_reserve(64).expect("a 64-canvas reserve fits");
        assert_eq!(read(&t, 1), Some(1007), "try_reserve disturbed an entry");
        mark(&mut t, 2);
        assert_eq!(read(&t, 2), Some(2007));
    }
}
