//! Deciding which parts of a `DrawImageBatch` one `SkCanvas::drawAtlas` can take.
//!
//! `drawAtlas` submits many sprites from *one* image in a single call, which is
//! the shape a 2D game's sprite batch already has. What it cannot do is change
//! image mid-call, or place a sprite with a transform that is not a uniform
//! scale plus a translation -- its per-sprite transform is an `RSXform`, which
//! carries one scale and one rotation, not independent x and y scales.
//!
//! So the batch is cut into runs rather than reordered. Reordering is the
//! tempting version and the wrong one: these draws are alpha-blended in the
//! order the content issued them, and moving one past another changes the
//! picture. Only *consecutive* entries sharing an image are merged, which
//! preserves the order exactly.
//!
//! This module is pure -- no Skia types, no GPU -- so the partitioning rules are
//! decidable by host tests, and the renderer is left holding only the Skia call.

use shared::protocol::render_cmd::DrawImageEntry;

/// One stretch of a batch, and how it should be drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchRun {
    /// `[start, end)` share one image and are all uniform-scaled: one
    /// `drawAtlas` call.
    Atlas { start: usize, end: usize },
    /// `[start, end)` must go one `drawImageRect` at a time.
    Individual { start: usize, end: usize },
}

impl BatchRun {
    pub fn range(self) -> (usize, usize) {
        match self {
            Self::Atlas { start, end } | Self::Individual { start, end } => (start, end),
        }
    }

    pub fn len(self) -> usize {
        let (start, end) = self.range();
        end - start
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// Whether one entry's destination is its source under a uniform scale.
///
/// Compared by cross-multiplication rather than by dividing: `dw / sw` and
/// `dh / sh` can round to the same `f32` while describing different rectangles,
/// and the division also has to special-case a zero source. The products are
/// computed in `f64` so that two `f32` multiplications cannot round into
/// agreement either.
pub fn is_uniformly_scaled(entry: &DrawImageEntry) -> bool {
    if !(entry.sx.is_finite()
        && entry.sy.is_finite()
        && entry.sw.is_finite()
        && entry.sh.is_finite()
        && entry.dx.is_finite()
        && entry.dy.is_finite()
        && entry.dw.is_finite()
        && entry.dh.is_finite())
    {
        return false;
    }
    // A zero or negative extent is not a sprite; `drawImageRect` defines those
    // (as nothing, or as a flip) and `RSXform` does not.
    if entry.sw <= 0.0 || entry.sh <= 0.0 || entry.dw <= 0.0 || entry.dh <= 0.0 {
        return false;
    }
    f64::from(entry.dw) * f64::from(entry.sh) == f64::from(entry.dh) * f64::from(entry.sw)
}

/// The uniform scale factor for an entry that [`is_uniformly_scaled`].
pub fn uniform_scale(entry: &DrawImageEntry) -> f32 {
    entry.dw / entry.sw
}

/// Cut a batch into the longest possible runs.
///
/// A run becomes an atlas draw only when it is at least two entries long: one
/// sprite through `drawAtlas` is the same GPU work as one `drawImageRect` plus
/// the cost of building the arrays, so the merge has to actually merge
/// something to be worth doing.
///
/// **Test helper only.** Production calls [`partition_into`] directly, passing
/// the scratch `Vec` owned by the canvas context so the run table is not
/// allocated on the render thread every frame. This wrapper exists for the
/// tests that assert output structure and do not need that optimisation.
pub fn partition(entries: &[DrawImageEntry], min_run: usize) -> Vec<BatchRun> {
    let mut runs = Vec::new();
    partition_into(entries, min_run, &mut runs);
    runs
}

/// Fill caller-owned scratch with the longest consecutive sprite runs.
///
/// The renderer invokes this once per batch. Keeping the output allocation
/// with its render owner avoids returning a temporary run table to the
/// allocator every frame, including the overflow at 17 sprites.
pub fn partition_into(entries: &[DrawImageEntry], min_run: usize, runs: &mut Vec<BatchRun>) {
    runs.clear();
    let mut index = 0usize;

    while index < entries.len() {
        if !is_uniformly_scaled(&entries[index]) {
            let start = index;
            while index < entries.len() && !is_uniformly_scaled(&entries[index]) {
                index += 1;
            }
            push_individual(runs, start, index);
            continue;
        }

        let start = index;
        let image = entries[index].image_id;
        while index < entries.len()
            && entries[index].image_id == image
            && is_uniformly_scaled(&entries[index])
        {
            index += 1;
        }
        if index - start >= min_run.max(2) {
            runs.push(BatchRun::Atlas { start, end: index });
        } else {
            push_individual(runs, start, index);
        }
    }
}

/// Append an individual run, merging with a preceding one so the caller never
/// sees two adjacent individual runs it would have to join itself.
fn push_individual(runs: &mut Vec<BatchRun>, start: usize, end: usize) {
    if let Some(BatchRun::Individual {
        start: prev_start,
        end: prev_end,
    }) = runs.last().copied()
        && prev_end == start
    {
        *runs.last_mut().expect("just matched") = BatchRun::Individual {
            start: prev_start,
            end,
        };
        return;
    }
    runs.push(BatchRun::Individual { start, end });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(image: u32, dw: f32, dh: f32) -> DrawImageEntry {
        DrawImageEntry {
            image_id: image,
            sx: 0.0,
            sy: 0.0,
            sw: 16.0,
            sh: 16.0,
            dx: 0.0,
            dy: 0.0,
            dw,
            dh,
        }
    }

    fn uniform(image: u32) -> DrawImageEntry {
        entry(image, 16.0, 16.0)
    }

    #[test]
    fn a_run_of_one_image_becomes_one_atlas_draw() {
        let batch = [uniform(1), uniform(1), uniform(1)];
        assert_eq!(
            partition(&batch, 2),
            vec![BatchRun::Atlas { start: 0, end: 3 }]
        );
    }

    #[test]
    fn an_image_change_cuts_the_run() {
        let batch = [uniform(1), uniform(1), uniform(2), uniform(2)];
        assert_eq!(
            partition(&batch, 2),
            vec![
                BatchRun::Atlas { start: 0, end: 2 },
                BatchRun::Atlas { start: 2, end: 4 },
            ]
        );
    }

    #[test]
    fn interleaved_images_never_merge_across_each_other() {
        // The tempting optimisation is to gather all of image 1 and then all of
        // image 2. These are alpha-blended in issue order, so that would change
        // the picture -- the partition must stay a partition of consecutive
        // ranges.
        let batch = [uniform(1), uniform(2), uniform(1), uniform(2)];
        let runs = partition(&batch, 2);
        assert_eq!(runs, vec![BatchRun::Individual { start: 0, end: 4 }]);
    }

    #[test]
    fn a_lone_sprite_is_not_worth_an_atlas_call() {
        let batch = [uniform(1), uniform(2), uniform(2)];
        assert_eq!(
            partition(&batch, 2),
            vec![
                BatchRun::Individual { start: 0, end: 1 },
                BatchRun::Atlas { start: 1, end: 3 },
            ]
        );
    }

    #[test]
    fn a_non_uniform_scale_drops_out_of_the_atlas() {
        // 32x16 from a 16x16 source is two different scales; `RSXform` has one.
        let batch = [uniform(1), entry(1, 32.0, 16.0), uniform(1), uniform(1)];
        assert_eq!(
            partition(&batch, 2),
            vec![
                BatchRun::Individual { start: 0, end: 2 },
                BatchRun::Atlas { start: 2, end: 4 },
            ]
        );
    }

    #[test]
    fn uniform_scaling_up_and_down_both_qualify() {
        assert!(is_uniformly_scaled(&entry(1, 32.0, 32.0)));
        assert!(is_uniformly_scaled(&entry(1, 8.0, 8.0)));
        assert!(!is_uniformly_scaled(&entry(1, 32.0, 33.0)));
    }

    #[test]
    fn degenerate_and_non_finite_rectangles_are_refused() {
        assert!(!is_uniformly_scaled(&entry(1, 0.0, 0.0)));
        assert!(!is_uniformly_scaled(&entry(1, -16.0, -16.0)));
        assert!(!is_uniformly_scaled(&entry(1, f32::NAN, 16.0)));
        assert!(!is_uniformly_scaled(&entry(
            1,
            f32::INFINITY,
            f32::INFINITY
        )));

        let mut zero_source = uniform(1);
        zero_source.sw = 0.0;
        assert!(!is_uniformly_scaled(&zero_source));
    }

    #[test]
    fn adjacent_individual_runs_are_reported_as_one() {
        // A non-uniform entry followed by a lone sprite must not produce two
        // individual runs the caller would have to notice were adjacent.
        let batch = [entry(1, 32.0, 16.0), uniform(2)];
        assert_eq!(
            partition(&batch, 2),
            vec![BatchRun::Individual { start: 0, end: 2 }]
        );
    }

    #[test]
    fn the_atlas_transform_lands_the_sprite_exactly_where_draw_image_rect_would() {
        // The claim the atlas path rests on: for a uniformly scaled entry, the
        // `RSXform` Skia is handed maps the source rect onto the same rectangle
        // `drawImageRect` would have drawn into. Checked against Skia's own
        // `RSXform` arithmetic rather than re-derived here, because re-deriving
        // it would only prove this file agrees with itself.
        for (sw, sh, dx, dy, dw, dh) in [
            (16.0f32, 16.0f32, 0.0f32, 0.0f32, 16.0f32, 16.0f32),
            (16.0, 16.0, 12.5, -7.25, 32.0, 32.0),
            (64.0, 32.0, 100.0, 200.0, 32.0, 16.0),
            (7.0, 3.0, 1.5, 2.5, 21.0, 9.0),
        ] {
            let e = DrawImageEntry {
                image_id: 1,
                sx: 4.0,
                sy: 8.0,
                sw,
                sh,
                dx,
                dy,
                dw,
                dh,
            };
            assert!(is_uniformly_scaled(&e), "fixture must be eligible");

            let scale = uniform_scale(&e);
            let xform = skia_safe::RSXform::new(scale, 0.0, (e.dx, e.dy));
            let quad = xform.to_quad((e.sw, e.sh));

            let expected = [
                (e.dx, e.dy),
                (e.dx + e.dw, e.dy),
                (e.dx + e.dw, e.dy + e.dh),
                (e.dx, e.dy + e.dh),
            ];
            for (corner, (want_x, want_y)) in quad.iter().zip(expected) {
                assert!(
                    (corner.x - want_x).abs() <= 1e-3 && (corner.y - want_y).abs() <= 1e-3,
                    "corner {corner:?} should be ({want_x}, {want_y}) for {e:?}"
                );
            }
        }
    }

    #[test]
    fn an_empty_batch_has_no_runs() {
        assert!(partition(&[], 2).is_empty());
    }

    #[test]
    fn every_entry_appears_exactly_once_and_in_order() {
        let batch = [
            uniform(1),
            uniform(1),
            entry(1, 32.0, 16.0),
            uniform(3),
            uniform(3),
            uniform(3),
            uniform(4),
        ];
        let mut covered = Vec::new();
        for run in partition(&batch, 2) {
            let (start, end) = run.range();
            assert!(start < end, "an empty run is never useful");
            covered.extend(start..end);
        }
        assert_eq!(covered, (0..batch.len()).collect::<Vec<_>>());
    }
    #[test]
    fn partition_into_warm_runs_have_no_steady_state_allocations() {
        use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};

        for count in [1usize, 16, 17, 1024] {
            let entries: Vec<_> = (0..count).map(|_| uniform(1)).collect();
            let mut scratch = Vec::new();
            partition_into(&entries, 2, &mut scratch);
            assert_no_steady_state_allocation(
                Burst {
                    path: "draw_atlas::partition_into",
                    warmup: 4,
                    measured: 32,
                },
                |_| {
                    partition_into(&entries, 2, &mut scratch);
                },
            );
            assert!(!scratch.is_empty());
        }
    }

    /// This is the closest host-test seam to the production fast path:
    /// `Canvas2DContext::try_fast_path_draw_image` owns these three vectors,
    /// clears them for each batch/run, and fills the geometry vectors before
    /// submitting `drawAtlas`. A full context cannot be built here without
    /// EGL, Ganesh, and an `ImageStore`, so this keeps the production scratch
    /// sequence explicit while leaving GPU submission untested.
    #[test]
    fn production_sprite_scratch_has_no_warm_allocations() {
        use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};

        fn frame(
            entries: &[DrawImageEntry],
            use_atlas: bool,
            runs: &mut Vec<BatchRun>,
            xforms: &mut Vec<skia_safe::RSXform>,
            tex: &mut Vec<skia_safe::Rect>,
        ) {
            runs.clear();
            if use_atlas {
                partition_into(entries, 2, runs);
            } else {
                runs.push(BatchRun::Individual {
                    start: 0,
                    end: entries.len(),
                });
            }

            for run in runs.iter().copied() {
                if let BatchRun::Atlas { start, end } = run {
                    xforms.clear();
                    tex.clear();
                    for entry in &entries[start..end] {
                        xforms.push(skia_safe::RSXform::new(
                            uniform_scale(entry),
                            0.0,
                            (entry.dx, entry.dy),
                        ));
                        tex.push(skia_safe::Rect::from_xywh(
                            entry.sx, entry.sy, entry.sw, entry.sh,
                        ));
                    }
                }
            }
        }

        for use_atlas in [true, false] {
            for count in [1usize, 16, 17, 1024] {
                let entries: Vec<_> = (0..count).map(|_| uniform(1)).collect();
                let mut runs = Vec::new();
                let mut xforms = Vec::new();
                let mut tex = Vec::new();
                frame(&entries, use_atlas, &mut runs, &mut xforms, &mut tex);

                assert_no_steady_state_allocation(
                    Burst {
                        path: if use_atlas {
                            "draw_atlas::production_sprite_scratch::atlas"
                        } else {
                            "draw_atlas::production_sprite_scratch::individual"
                        },
                        warmup: 4,
                        measured: 32,
                    },
                    |_| frame(&entries, use_atlas, &mut runs, &mut xforms, &mut tex),
                );
                assert!(!runs.is_empty());
                if use_atlas && count >= 2 {
                    assert_eq!(xforms.len(), count);
                    assert_eq!(tex.len(), count);
                }
            }
        }
    }
}
