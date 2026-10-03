//! Unified damage descriptor for mixed Canvas2D + WebGL frames.
//!
//! Every rendering operation (Canvas2D batch, GL draw, GL clear, etc.)
//! produces a `DamageEffect`.  The render thread feeds these into a
//! `FrameDamageAccumulator` which unions rects and tracks FullSurface
//! escalation.  At swap time, the accumulator resolves to `ResolvedDamage`.

use crate::dirty_region::damage_tracker::{DamageTracker, MAX_DAMAGE_RECTS, ResolvedDamage};
use smallvec::SmallVec;

/// Up to [`MAX_DAMAGE_RECTS`] discrete damage rects, `(x, y, width, height)`,
/// inline. Named because the shape appears in a return type, in the callers
/// that pass it to the partial-update driver call, and in the tests that assert
/// coverage over it.
pub(crate) type DamageRects = SmallVec<[(i32, i32, i32, i32); MAX_DAMAGE_RECTS]>;

/// The damage produced by a single rendering operation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DamageEffect {
    /// No visible output (state-only command, offscreen write, etc.).
    NoDamage,
    /// Bounded onscreen write with a known pixel rect.
    OnscreenRect {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    /// Unbounded or un-trackable onscreen write - forces full surface redraw.
    FullSurface,
}

/// Whether the content's frame has drawn into the screen, carried across the
/// packets of the frame: a barrier -- sent mid-frame so that a synchronous call
/// sees what was recorded before it -- runs its batches, and what they drew is
/// presented with the rest of the frame when it ends (`FrameOp::Present`).
/// Never on its own: that would put half a frame on the screen whenever content
/// asked a question mid-frame, and, for a WebGL drawing buffer that is not
/// preserved, clear the half drawn when the rest arrived.
#[derive(Debug, Default)]
pub(crate) struct FrameDrawing(bool);

impl FrameDrawing {
    /// A batch of the frame ran, and drew into the screen or did not.
    #[inline]
    pub(crate) fn batch(&mut self, drew_onscreen: bool) {
        self.0 |= drew_onscreen;
    }

    /// The frame ended: whether it drew into the screen, and the next begins
    /// having drawn nothing.
    #[inline]
    pub(crate) fn frame_ended(&mut self) -> bool {
        std::mem::take(&mut self.0)
    }
}

/// Accumulates `DamageEffect`s across an entire frame, then resolves to
/// `ResolvedDamage` at swap time.
///
/// Rules:
/// - `NoDamage` is ignored.
/// - `OnscreenRect` rects are kept discrete up to
///   [`MAX_DAMAGE_RECTS`] (currently 4).  Past that the accumulator
///   collapses them to a single AABB so the per-frame cost stays
///   bounded.  Multi-rect resolution is exposed to callers that can
///   benefit from it via [`Self::resolve_rects`]; legacy callers
///   using [`Self::resolve`] still see a single AABB.
/// - Any `FullSurface` poisons the frame - resolution always
///   returns `FullSurface`.
/// - Once poisoned, subsequent rects are still accepted (no-op) to
///   avoid short-circuiting callers, but they don't affect the
///   outcome.
pub(crate) struct FrameDamageAccumulator {
    rects: SmallVec<[[i32; 4]; MAX_DAMAGE_RECTS]>,
    force_full: bool,
}

impl FrameDamageAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            rects: SmallVec::new(),
            force_full: false,
        }
    }

    /// Feed a damage effect from any rendering source.
    pub(crate) fn add(&mut self, effect: DamageEffect) {
        match effect {
            DamageEffect::NoDamage => {}
            DamageEffect::OnscreenRect {
                x,
                y,
                width,
                height,
            } => {
                if width <= 0 || height <= 0 {
                    return;
                }
                if self.rects.len() < MAX_DAMAGE_RECTS {
                    self.rects.push([x, y, width, height]);
                    return;
                }
                // Past the cap: union everything held *and the incoming rect*
                // into slot 0, so the list is one rect again.
                //
                // **The overflow is not sticky, and that is the better
                // behaviour rather than an oversight** — the comment this
                // replaces talked itself into "sticky" mid-sentence and then
                // described code that was never written. Because the collapse
                // leaves `rects.len() == 1`, the next few adds take the branch
                // above and append discretely again, so a frame with many
                // scattered updates keeps reporting up to `MAX_DAMAGE_RECTS`
                // useful regions with slot 0 as a growing AABB of everything
                // already folded in. Unioning into slot 0 forever instead would
                // degrade to one near-full-surface rect after the fifth update
                // and stay there for the rest of the frame.
                //
                // What the cycle cannot do is under-report, and that is the
                // property worth pinning: slot 0 always covers every rect
                // folded into it, and the discrete slots cover themselves.
                // `coverage_holds_for_every_rect_count_around_the_cap` asserts
                // it for every count either side of the cap, because an
                // under-reported region is what leaves a stale pixel on screen.
                let mut left = x;
                let mut top = y;
                let mut right = x + width;
                let mut bottom = y + height;
                for r in &self.rects {
                    left = left.min(r[0]);
                    top = top.min(r[1]);
                    right = right.max(r[0] + r[2]);
                    bottom = bottom.max(r[1] + r[3]);
                }
                self.rects.clear();
                self.rects.push([left, top, right - left, bottom - top]);
            }
            DamageEffect::FullSurface => {
                self.force_full = true;
            }
        }
    }

    /// Resolve accumulated damage for the given surface dimensions
    /// as a single bounding-box [`ResolvedDamage`].  Used by the
    /// swap path / stats; callers that want per-rect granularity
    /// (partial-update driver call) should use
    /// [`Self::resolve_rects`].
    ///
    /// Unused by production code today -- the swap path already
    /// switched to [`Self::resolve_rects`] for partial-update
    /// support, and no stats consumer has been wired to the
    /// bounding-box form yet. Kept, not deleted: it is exhaustively
    /// unit-tested and a legitimate, correct alternate view of the
    /// same accumulator.
    #[allow(dead_code)]
    pub(crate) fn resolve(&self, surface_size: (i32, i32)) -> ResolvedDamage {
        let mut tracker = DamageTracker::new(surface_size);
        for r in &self.rects {
            tracker.mark_rect((r[0], r[1], r[2], r[3]));
        }
        if self.force_full {
            tracker.mark_requires_full_redraw();
        }
        tracker.resolve()
    }

    /// Resolve accumulated damage to up to [`MAX_DAMAGE_RECTS`]
    /// discrete rects (clipped to `surface_size`).  Returns `None`
    /// when the frame requires a full redraw; otherwise a
    /// non-empty list.
    pub(crate) fn resolve_rects(&self, surface_size: (i32, i32)) -> Option<DamageRects> {
        let mut tracker = DamageTracker::new(surface_size);
        for r in &self.rects {
            tracker.mark_rect((r[0], r[1], r[2], r[3]));
        }
        if self.force_full {
            tracker.mark_requires_full_redraw();
        }
        tracker.resolve_rects()
    }

    #[allow(dead_code)]
    /// Whether any damage has been recorded (rect or full).
    pub(crate) fn has_damage(&self) -> bool {
        !self.rects.is_empty() || self.force_full
    }

    /// Read the accumulated bounding rect (for staging into
    /// `CanvasManager::pending_damage_rect` during the transition
    /// period).  Returns `None` when no rects are recorded.
    #[allow(dead_code)]
    pub(crate) fn accumulated_rect(&self) -> Option<[i32; 4]> {
        if self.rects.is_empty() {
            return None;
        }
        let mut left = self.rects[0][0];
        let mut top = self.rects[0][1];
        let mut right = left + self.rects[0][2];
        let mut bottom = top + self.rects[0][3];
        for r in self.rects.iter().skip(1) {
            left = left.min(r[0]);
            top = top.min(r[1]);
            right = right.max(r[0] + r[2]);
            bottom = bottom.max(r[1] + r[3]);
        }
        Some([left, top, right - left, bottom - top])
    }

    #[allow(dead_code)]
    /// Whether FullSurface has been triggered.
    pub(crate) fn is_full_surface(&self) -> bool {
        self.force_full
    }

    /// Reset for next frame.
    pub(crate) fn reset(&mut self) {
        self.rects.clear();
        self.force_full = false;
    }
}

/// Intersect two axis-aligned rects `(x, y, width, height)`.
/// Returns `None` if the intersection is empty.
pub(crate) fn intersect_rects(
    a: (i32, i32, i32, i32),
    b: (i32, i32, i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let left = a.0.max(b.0);
    let top = a.1.max(b.1);
    let right = (a.0 + a.2).min(b.0 + b.2);
    let bottom = (a.1 + a.3).min(b.1 + b.3);
    if left < right && top < bottom {
        Some((left, top, right - left, bottom - top))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_damage_produces_full_surface_fallback() {
        let acc = FrameDamageAccumulator::new();
        // No damage at all → DamageTracker returns FullSurface (no dirty rect).
        assert_eq!(acc.resolve((1080, 1920)), ResolvedDamage::FullSurface);
    }

    #[test]
    fn single_onscreen_rect_produces_partial() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 300,
            height: 200,
        });
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 10,
                y: 20,
                width: 300,
                height: 200,
            }
        );
    }

    #[test]
    fn multiple_rects_union_to_bounding_box() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::OnscreenRect {
            x: 200,
            y: 300,
            width: 150,
            height: 100,
        });
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 10,
                y: 20,
                width: 340,
                height: 380,
            }
        );
    }

    #[test]
    fn full_surface_overrides_any_rects() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::FullSurface);
        acc.add(DamageEffect::OnscreenRect {
            x: 200,
            y: 300,
            width: 50,
            height: 50,
        });
        assert_eq!(acc.resolve((1080, 1920)), ResolvedDamage::FullSurface);
    }

    #[test]
    fn no_damage_is_ignored() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::NoDamage);
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::NoDamage);
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 10,
                y: 20,
                width: 100,
                height: 50,
            }
        );
    }

    /// Simulates: Canvas2D rect + viewport-bounded GL draw → union partial.
    #[test]
    fn canvas2d_rect_plus_gl_viewport_unions_to_partial() {
        let mut acc = FrameDamageAccumulator::new();
        // Canvas2D batch damage
        acc.add(DamageEffect::OnscreenRect {
            x: 0,
            y: 0,
            width: 200,
            height: 100,
        });
        // GL draw call viewport
        acc.add(DamageEffect::OnscreenRect {
            x: 300,
            y: 400,
            width: 200,
            height: 200,
        });
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 0,
                y: 0,
                width: 500,
                height: 600,
            }
        );
    }

    /// Simulates: scissor-bounded clear as OnscreenRect → partial damage.
    #[test]
    fn scissor_bounded_clear_produces_partial_damage() {
        let mut acc = FrameDamageAccumulator::new();
        // Clear bounded by scissor rect [50, 50, 200, 200].
        acc.add(DamageEffect::OnscreenRect {
            x: 50,
            y: 50,
            width: 200,
            height: 200,
        });
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 50,
                y: 50,
                width: 200,
                height: 200,
            }
        );
    }

    /// Clear without scissor → FullSurface.
    #[test]
    fn unbounded_clear_forces_full_surface() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::FullSurface);
        assert_eq!(acc.resolve((1080, 1920)), ResolvedDamage::FullSurface);
    }

    /// User FBO draw produces NoDamage → does not affect onscreen.
    #[test]
    fn user_fbo_draw_does_not_affect_onscreen_accumulator() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 10,
            width: 100,
            height: 100,
        });
        // User FBO draw → NoDamage (filtered before reaching accumulator).
        acc.add(DamageEffect::NoDamage);
        assert_eq!(
            acc.resolve((1080, 1920)),
            ResolvedDamage::Partial {
                x: 10,
                y: 10,
                width: 100,
                height: 100,
            }
        );
    }

    /// Any FullSurface after partial rects poisons the accumulator.
    #[test]
    fn full_surface_after_partial_rects_poisons_accumulator() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::OnscreenRect {
            x: 200,
            y: 300,
            width: 150,
            height: 100,
        });
        acc.add(DamageEffect::FullSurface);
        assert_eq!(acc.resolve((1080, 1920)), ResolvedDamage::FullSurface);
    }

    #[test]
    fn reset_clears_accumulated_state() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        acc.add(DamageEffect::FullSurface);
        assert!(acc.has_damage());
        assert!(acc.is_full_surface());

        acc.reset();
        assert!(!acc.has_damage());
        assert!(!acc.is_full_surface());
        // After reset, resolves to FullSurface (no dirty rect = fallback).
        assert_eq!(acc.resolve((1080, 1920)), ResolvedDamage::FullSurface);
    }

    #[test]
    fn zero_size_rect_is_ignored() {
        let mut acc = FrameDamageAccumulator::new();
        acc.add(DamageEffect::OnscreenRect {
            x: 10,
            y: 20,
            width: 0,
            height: 50,
        });
        assert!(!acc.has_damage());
    }

    // ---- Overflow past MAX_DAMAGE_RECTS ------------------------------
    //
    // The collapse branch had no test. It is the one place the accumulator
    // discards information, so it is the one place it could under-report — and
    // an under-reported damage region means the compositor is told a changed
    // pixel did not change, which leaves the old one on screen.
    //
    // Every test below states the invariant the same way: whatever
    // `resolve_rects` returns must *cover* every rect that was added. That is
    // the accumulator's whole contract, and it holds regardless of how the
    // collapse chooses to group things.

    /// Does the resolved damage cover `rect` entirely?
    fn covered(resolved: &Option<DamageRects>, rect: (i32, i32, i32, i32)) -> bool {
        match resolved {
            // FullSurface covers everything by definition.
            None => true,
            Some(rects) => rects.iter().any(|r| {
                r.0 <= rect.0
                    && r.1 <= rect.1
                    && r.0 + r.2 >= rect.0 + rect.2
                    && r.1 + r.3 >= rect.1 + rect.3
            }),
        }
    }

    /// `n` rects marching diagonally, each 10x10, 100 apart — so no two touch
    /// and a lost rect cannot be hidden by an overlapping neighbour.
    fn diagonal_rects(n: usize) -> Vec<(i32, i32, i32, i32)> {
        (0..n)
            .map(|i| (i as i32 * 100, i as i32 * 100, 10, 10))
            .collect()
    }

    #[test]
    fn the_rect_that_triggers_the_collapse_is_not_lost() {
        let inputs = diagonal_rects(MAX_DAMAGE_RECTS + 1);
        let mut acc = FrameDamageAccumulator::new();
        for &(x, y, width, height) in &inputs {
            acc.add(DamageEffect::OnscreenRect {
                x,
                y,
                width,
                height,
            });
        }
        let resolved = acc.resolve_rects((4096, 4096));
        for rect in inputs {
            assert!(
                covered(&resolved, rect),
                "rect {rect:?} was dropped by the collapse; resolved = {resolved:?}"
            );
        }
    }

    /// Two collapses, so the second one has to union a slot that is *already*
    /// a collapsed AABB rather than a plain rect.
    #[test]
    fn rects_survive_more_than_one_collapse() {
        let inputs = diagonal_rects(MAX_DAMAGE_RECTS * 2 + 1);
        let mut acc = FrameDamageAccumulator::new();
        for &(x, y, width, height) in &inputs {
            acc.add(DamageEffect::OnscreenRect {
                x,
                y,
                width,
                height,
            });
        }
        let resolved = acc.resolve_rects((4096, 4096));
        for rect in inputs {
            assert!(
                covered(&resolved, rect),
                "rect {rect:?} was dropped across two collapses; resolved = {resolved:?}"
            );
        }
    }

    /// Held for every count either side of the cap, so an off-by-one in the
    /// `<` comparison shows up rather than hiding between two tested sizes.
    #[test]
    fn coverage_holds_for_every_rect_count_around_the_cap() {
        for n in 1..=(MAX_DAMAGE_RECTS * 3) {
            let inputs = diagonal_rects(n);
            let mut acc = FrameDamageAccumulator::new();
            for &(x, y, width, height) in &inputs {
                acc.add(DamageEffect::OnscreenRect {
                    x,
                    y,
                    width,
                    height,
                });
            }
            let resolved = acc.resolve_rects((4096, 4096));
            for rect in inputs {
                assert!(
                    covered(&resolved, rect),
                    "{n} rects: {rect:?} not covered; resolved = {resolved:?}"
                );
            }
        }
    }

    /// The collapse must stay bounded — that is the reason it exists. Past the
    /// cap the accumulator must never hold more than `MAX_DAMAGE_RECTS`.
    #[test]
    fn the_accumulator_never_holds_more_than_the_cap() {
        let mut acc = FrameDamageAccumulator::new();
        for i in 0..100 {
            acc.add(DamageEffect::OnscreenRect {
                x: i * 10,
                y: i * 10,
                width: 5,
                height: 5,
            });
            assert!(
                acc.rects.len() <= MAX_DAMAGE_RECTS,
                "after {} adds the accumulator held {} rects",
                i + 1,
                acc.rects.len()
            );
        }
    }

    /// The collapse must not spill to the heap: `rects` is a `SmallVec` sized
    /// to the cap precisely so a per-command path never allocates.
    #[test]
    fn a_steady_state_damage_accumulation_never_reaches_the_heap() {
        let mut acc = FrameDamageAccumulator::new();

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "damage_effect: per-command accumulation past the rect cap",
                warmup: 4,
                measured: 64,
            },
            |iteration| {
                // Well past the cap, so every iteration exercises the collapse.
                for i in 0..32i32 {
                    acc.add(DamageEffect::OnscreenRect {
                        x: i * 10 + iteration as i32,
                        y: i * 10,
                        width: 5,
                        height: 5,
                    });
                }
                let held = acc.rects.len();
                acc.reset();
                held
            },
        );
    }

    // ---- intersect_rects tests ----

    #[test]
    fn intersect_overlapping_rects() {
        assert_eq!(
            intersect_rects((0, 0, 100, 100), (50, 50, 100, 100)),
            Some((50, 50, 50, 50))
        );
    }

    #[test]
    fn intersect_non_overlapping_rects() {
        assert_eq!(intersect_rects((0, 0, 50, 50), (100, 100, 50, 50)), None);
    }

    #[test]
    fn intersect_contained_rect() {
        assert_eq!(
            intersect_rects((0, 0, 1000, 1000), (100, 200, 50, 60)),
            Some((100, 200, 50, 60))
        );
    }

    #[test]
    fn intersect_touching_edge_is_empty() {
        assert_eq!(intersect_rects((0, 0, 100, 100), (100, 0, 100, 100)), None);
    }
}
