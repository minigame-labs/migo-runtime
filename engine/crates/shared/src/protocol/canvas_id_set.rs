//! A small set of canvas ids, gathered without touching the heap.

use smallvec::SmallVec;

use crate::protocol::render_cmd::CanvasId;

/// How many distinct canvases fit before the set spills.
///
/// Sized for the heaviest scene profiled — the shop-open frame that motivated
/// the render thread's phase reorder, at about thirty offscreen Canvas2D
/// labels. Nothing bounds a game above that, so the capacity is *not* what
/// keeps a busier scene off the heap; a set that outlives the frame filling it
/// is. What the capacity decides is whether such a scene reaches the heap at
/// all, once.
pub const CANVAS_ID_SET_INLINE_CAPACITY: usize = 32;

/// A set of canvas ids, gathered without touching the heap, for a caller that
/// needs the ids themselves.
///
/// The render path had three independently written `HashSet`s of canvas ids,
/// each built and thrown away every frame or every batch to answer a question
/// about a handful of small integers. Two of the three need the ids themselves —
/// which canvases still hold unmaterialised 2D work, and which ones a WebGL batch
/// touched — and are what this type serves. The third only needed to know whether
/// two sets intersect, which is a boolean, and answers it by scanning the frame's
/// ops directly with no container at all.
///
/// At these sizes a linear scan over an inline array beats hashing on every
/// count that matters — no allocation, no hashing, and the whole set inside a
/// couple of cache lines. One type rather than two call sites' worth of
/// `SmallVec` handling, so the deduplication a set implies is written once and
/// gated once; an inline `SmallVec` that a caller forgets to check before
/// pushing is a set only by intention.
///
/// **Above the inline capacity it spills to the heap, and where that costs
/// anything is decided by the caller, not here.** A set constructed per frame
/// and dropped pays that allocation on every frame of the scene, for as long as
/// the scene is on screen — which is what the steady-state zero-allocation requirement forbids, and what
/// "correct but slower on a scene nobody has produced yet" understated. A set
/// that outlives the frame pays it once and then never, whatever the scene
/// does. So a per-frame caller acquires it through [`CanvasIdSet::begin`] from a
/// field of the object that spans frames, and the only caller that keeps a local
/// is the one whose count is the number of distinct live WebGL canvases in a
/// single batch rather than the number of Canvas2D labels in a scene.
#[derive(Debug, Default, Clone)]
pub struct CanvasIdSet {
    ids: SmallVec<[CanvasId; CANVAS_ID_SET_INLINE_CAPACITY]>,
}

impl CanvasIdSet {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Empties the set and hands it out for one frame's use.
    ///
    /// The reuse is of the allocation, never of the contents: a set held across
    /// frames that was not emptied would report a previous frame's canvases as
    /// this frame's. Acquiring it through this method is what keeps that from
    /// being a rule a caller has to remember.
    #[inline]
    pub fn begin(&mut self) -> &mut Self {
        self.clear();
        self
    }

    /// Adds `id`, reporting whether it was not already present.
    #[inline]
    pub fn insert(&mut self, id: CanvasId) -> bool {
        if self.ids.contains(&id) {
            return false;
        }
        self.ids.push(id);
        true
    }

    #[inline]
    pub fn contains(&self, id: CanvasId) -> bool {
        self.ids.contains(&id)
    }

    /// Empties the set while keeping whatever capacity it reached, so a set
    /// reused across frames stops allocating after the first spill.
    #[inline]
    pub fn clear(&mut self) {
        self.ids.clear();
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the set has reached the heap. Public because "this set outlives
    /// the frame so its spill is paid once" is a property of a *caller*, and a
    /// caller in another crate has to be able to assert it.
    #[inline]
    pub fn spilled(&self) -> bool {
        self.ids.spilled()
    }

    /// How many ids the set can hold before it allocates again. Same reason as
    /// [`CanvasIdSet::spilled`].
    #[inline]
    pub fn capacity(&self) -> usize {
        self.ids.capacity()
    }

    /// Iterates in insertion order.
    ///
    /// Deliberately not a `HashSet`'s arbitrary order: the callers feed these
    /// ids straight into emitted render ops, and an order that varies run to
    /// run would make a packet's contents unreproducible for no benefit.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = CanvasId> + '_ {
        self.ids.iter().copied()
    }
}

impl<'a> IntoIterator for &'a CanvasIdSet {
    type Item = CanvasId;
    type IntoIter = std::iter::Copied<std::slice::Iter<'a, CanvasId>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.ids.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};

    const WARMUP: usize = 4;
    const MEASURED: usize = 64;

    #[test]
    fn a_repeated_id_is_held_once_and_reported_as_already_present() {
        let mut set = CanvasIdSet::new();

        assert!(set.insert(7));
        assert!(
            !set.insert(7),
            "a second insert must report the id was present"
        );
        assert_eq!(set.len(), 1);
        assert!(set.contains(7));
    }

    #[test]
    fn iteration_follows_insertion_order() {
        let mut set = CanvasIdSet::new();
        for id in [9, 3, 9, 12, 3] {
            set.insert(id);
        }

        assert_eq!(set.iter().collect::<Vec<_>>(), vec![9, 3, 12]);
    }

    #[test]
    fn clearing_empties_the_set_without_giving_up_its_capacity() {
        let mut set = CanvasIdSet::new();
        for id in 0..(CANVAS_ID_SET_INLINE_CAPACITY as CanvasId + 40) {
            set.insert(id);
        }
        let spilled = set.ids.capacity();
        assert!(
            set.ids.spilled(),
            "fixture must reach the heap to have capacity worth keeping"
        );

        set.clear();

        assert!(set.is_empty());
        assert_eq!(
            set.ids.capacity(),
            spilled,
            "a set reused across frames gave up its allocation and has to grow again"
        );
    }

    /// Past the inline capacity the set must still answer correctly. A spill
    /// that lost entries would tell the render thread a canvas is untouched
    /// when it is, and the Canvas2D context that needed invalidating would keep
    /// drawing against stale Skia state.
    #[test]
    fn a_spilled_set_still_holds_every_id() {
        let count = CANVAS_ID_SET_INLINE_CAPACITY as CanvasId + 40;
        let mut set = CanvasIdSet::new();
        for id in 0..count {
            set.insert(id);
        }

        assert_eq!(set.len(), count as usize);
        for id in 0..count {
            assert!(set.contains(id), "the spill lost canvas {id}");
        }
        assert!(!set.contains(count));
    }

    /// Steady-state allocation gate: these sets are built on per-frame and per-batch paths, so
    /// whatever they cost, the engine pays once a frame for as long as it runs.
    /// The `HashSet`s this type replaced each cost an allocation and a free
    /// there.
    #[test]
    fn gathering_a_frames_canvases_never_reaches_the_heap() {
        // The shape the render path actually produces: a scene's worth of
        // distinct canvases, each addressed more than once.
        assert_no_steady_state_allocation(
            Burst {
                path: "canvas id set: gather one frame's canvases",
                warmup: WARMUP,
                measured: MEASURED,
            },
            |_| {
                let mut set = CanvasIdSet::new();
                for id in 0..24 {
                    set.insert(id);
                    set.insert(id);
                }
                std::hint::black_box(set.contains(23))
            },
        );
    }

    /// The property that makes a set above the inline capacity affordable at
    /// all: refilling one that outlives the frame reaches the heap on the first
    /// scene and never again, however far above the capacity the scene sits.
    ///
    /// The gate above cannot state this — it constructs its set inside the burst
    /// and so is only zero for a scene that *fits*. Both are needed: one says
    /// the ordinary scene never allocates, this one says the extraordinary scene
    /// does not allocate *repeatedly*, and the second is the one the steady-state requirement is
    /// actually about.
    #[test]
    fn refilling_a_reused_set_far_above_the_inline_capacity_never_reaches_the_heap() {
        const DISTINCT: CanvasId = CANVAS_ID_SET_INLINE_CAPACITY as CanvasId * 3;
        let mut set = CanvasIdSet::new();

        assert_no_steady_state_allocation(
            Burst {
                path: "canvas id set: refill a reused set above its inline capacity",
                warmup: WARMUP,
                measured: MEASURED,
            },
            |_| {
                let ids = set.begin();
                for id in 0..DISTINCT {
                    ids.insert(id);
                }
                std::hint::black_box(ids.len())
            },
        );

        assert!(
            set.spilled(),
            "the fixture stayed inline, so it never measured a spill being paid once"
        );
        assert_eq!(set.len(), DISTINCT as usize);
    }

    /// `begin` is the whole reuse contract: the allocation carries over, the
    /// contents do not. A set handed out still holding the previous frame's ids
    /// would report canvases as having work this frame that they were given last
    /// frame.
    #[test]
    fn begin_hands_out_an_empty_set_that_kept_its_capacity() {
        let mut set = CanvasIdSet::new();
        for id in 0..(CANVAS_ID_SET_INLINE_CAPACITY as CanvasId + 40) {
            set.insert(id);
        }
        assert!(
            set.spilled(),
            "fixture must reach the heap to have capacity worth keeping"
        );
        let reached = set.capacity();

        let reused = set.begin();

        assert!(reused.is_empty(), "a previous frame's ids survived `begin`");
        assert!(!reused.contains(0));
        assert_eq!(reused.capacity(), reached);
    }
}
