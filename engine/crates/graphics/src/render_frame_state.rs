use std::cell::Cell;
use std::time::Duration;

pub(crate) const DEFERRED_CLEANUP_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const DEFERRED_CLEANUP_UNUSED_AGE: Duration = Duration::from_millis(200);

/// Coalesces deferred Skia cleanup onto frames the renderer was already asked
/// to process. It never schedules a timer and advances directly to `now`, so a
/// stalled or paused renderer does not run catch-up sweeps.
pub(crate) struct DeferredCleanupCadence {
    last_run: Cell<Duration>,
}

impl DeferredCleanupCadence {
    pub(crate) fn new(now: Duration) -> Self {
        Self {
            last_run: Cell::new(now),
        }
    }

    #[inline]
    pub(crate) fn should_run(&self, now: Duration) -> bool {
        if now.saturating_sub(self.last_run.get()) < DEFERRED_CLEANUP_INTERVAL {
            return false;
        }
        self.last_run.set(now);
        true
    }

    #[inline]
    pub(crate) fn mark_forced(&self, now: Duration) {
        self.last_run.set(now);
    }
}

/// One render thread's diagnostic deltas and latest gauge samples. All fields
/// are plain integers so draw/cache hot paths do not touch shared atomics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameDiagnosticBatch {
    pub(crate) draw_calls: u32,
    pub(crate) state_changes: u32,
    /// Draws issued with **zero driver-visible state changes since the previous
    /// draw of the same frame**.
    ///
    /// The upper bound on what a draw-call batching pass could merge, and the
    /// number nobody had. `state_changes / draw_calls` says how much state churn
    /// a frame has, but not whether any of it sits *between* draws: a frame
    /// could change state heavily during setup and then issue fifty untouched
    /// draws, or alternate one change per draw, and the ratio reads the same.
    ///
    /// Counted after dedup, which is the point. A state command the shadow
    /// swallowed never reached the driver, so two draws separated only by
    /// deduped commands are adjacent as far as the GPU is concerned — and it is
    /// the GPU's view that decides whether a merge is possible.
    ///
    /// **An upper bound, not an opportunity** — see [`Self::mergeable_draws`]
    /// for the narrower count. Adjacency says nothing reached the driver in
    /// between; it does not say the two draws *could* become one.
    ///
    /// The first draw of a frame is never adjacent: there is no previous draw to
    /// merge it with.
    pub(crate) adjacent_draws: u32,
    /// Draws that are adjacent **and could actually be merged with the previous
    /// one**: same primitive mode, and a vertex or index range that continues
    /// exactly where the previous draw's ended.
    ///
    /// The number [`Self::adjacent_draws`] cannot give. A frame can be 98%
    /// adjacent and 0% mergeable — drawing the same six vertices sixty-four
    /// times has nothing between the draws, but folding them into one draw of
    /// 384 vertices would paint different pixels. Only a contiguous range makes
    /// the merge an identity.
    ///
    /// So this is the figure that can *open* the batching question, where
    /// adjacency alone can only close it. Still an upper bound in one respect,
    /// stated rather than implied: it does not check that the two draws read the
    /// same buffers, because a bound buffer change is a driver state change and
    /// would have broken adjacency first.
    pub(crate) mergeable_draws: u32,
    pub(crate) texture_upload_bytes: u32,
    pub(crate) measure_text_hits: u32,
    pub(crate) measure_text_misses: u32,
    pub(crate) shape_cache_hits: u32,
    pub(crate) shape_cache_misses: u32,
    pub(crate) sk_image_wrapper_hits: u32,
    pub(crate) sk_image_wrapper_misses: u32,
    pub(crate) skia_context_resets: u32,
    pub(crate) canvas2d_snapshots_taken: u32,
    pub(crate) canvas2d_snapshot_fallbacks: u32,
    pub(crate) canvas2d_snapshot_uploads: u32,
    pub(crate) canvas2d_snapshot_forced_readbacks: u32,
    pub(crate) shadow_filter_hits: u32,
    pub(crate) shadow_filter_misses: u32,
    pub(crate) dash_effect_hits: u32,
    pub(crate) dash_effect_misses: u32,
    pub(crate) gradient_hits: u32,
    pub(crate) gradient_misses: u32,
    pub(crate) text_cache_hits: u32,
    pub(crate) text_cache_misses: u32,
    pub(crate) text_cache_bytes: Option<u32>,
    pub(crate) text_cache_entries: Option<u32>,
    pub(crate) render_queue_len: Option<u32>,
    pub(crate) sk_image_wrappers: Option<u32>,
    pub(crate) deferred_uploads: Option<u32>,
}

impl FrameDiagnosticBatch {
    const EMPTY: Self = Self {
        draw_calls: 0,
        state_changes: 0,
        adjacent_draws: 0,
        mergeable_draws: 0,
        texture_upload_bytes: 0,
        measure_text_hits: 0,
        measure_text_misses: 0,
        shape_cache_hits: 0,
        shape_cache_misses: 0,
        sk_image_wrapper_hits: 0,
        sk_image_wrapper_misses: 0,
        skia_context_resets: 0,
        canvas2d_snapshots_taken: 0,
        canvas2d_snapshot_fallbacks: 0,
        canvas2d_snapshot_uploads: 0,
        canvas2d_snapshot_forced_readbacks: 0,
        shadow_filter_hits: 0,
        shadow_filter_misses: 0,
        dash_effect_hits: 0,
        dash_effect_misses: 0,
        gradient_hits: 0,
        gradient_misses: 0,
        text_cache_hits: 0,
        text_cache_misses: 0,
        text_cache_bytes: None,
        text_cache_entries: None,
        render_queue_len: None,
        sk_image_wrappers: None,
        deferred_uploads: None,
    };
}

/// What a draw covered, for the contiguity test behind
/// [`FrameDiagnosticBatch::mergeable_draws`].
///
/// `start` and `len` are in whichever unit the call uses — vertices for
/// `glDrawArrays`, indices for `glDrawElements` — and `elements` keeps the two
/// from being compared with each other. A `drawArrays` ending at vertex 6 and a
/// `drawElements` starting at index 6 are not contiguous with one another; they
/// are not even the same kind of thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DrawShape {
    /// GL primitive mode (`GL_TRIANGLES`, …). Two draws of different modes
    /// cannot merge whatever their ranges.
    pub(crate) mode: u32,
    /// Whether the range counts indices (`glDrawElements`) or vertices
    /// (`glDrawArrays`).
    pub(crate) elements: bool,
    pub(crate) start: i32,
    pub(crate) len: i32,
}

impl DrawShape {
    /// Does `next` begin exactly where `self` ended, in the same mode and the
    /// same unit?
    ///
    /// The strict version on purpose. An overlapping or gapped range could
    /// still be merged by a clever pass, but only by changing what is painted,
    /// and this counter is meant to bound *identity-preserving* merges — the
    /// only ones a renderer may perform without the game's consent.
    #[inline]
    pub(crate) fn continues_into(self, next: Self) -> bool {
        self.mode == next.mode
            && self.elements == next.elements
            && self.len > 0
            && next.len > 0
            && self.start.checked_add(self.len) == Some(next.start)
    }
}

/// Convert a `glDrawElements` byte offset into a number of indices.
///
/// `glDrawElements` takes its offset in bytes but its count in indices, so a
/// contiguity test has to bring them into one unit or it compares a byte
/// position against an index position and finds contiguity where there is none.
///
/// An unrecognised `index_type` yields `-1`, a position no draw can continue
/// into or from, so an unknown type reads as "not mergeable" rather than
/// silently colliding with index 0.
#[inline]
pub(crate) fn indices_from_byte_offset(offset: i32, index_type: u32) -> i32 {
    let stride = match index_type {
        glow::UNSIGNED_BYTE => 1,
        glow::UNSIGNED_SHORT => 2,
        glow::UNSIGNED_INT => 4,
        _ => return -1,
    };
    offset / stride
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameDiagnosticAccumulator {
    pending: FrameDiagnosticBatch,
    /// `pending.state_changes` as of the previous draw in this frame, so
    /// [`Self::bump_draw_call`] can tell whether anything reached the driver in
    /// between. Reset by [`Self::drain`] along with the batch.
    state_changes_at_last_draw: u32,
    /// The previous draw's shape, for the contiguity test behind
    /// [`FrameDiagnosticBatch::mergeable_draws`].
    last_draw: Option<DrawShape>,
}

macro_rules! counter_method {
    ($name:ident, $field:ident) => {
        #[inline]
        pub(crate) fn $name(&mut self) {
            self.pending.$field = self.pending.$field.saturating_add(1);
        }
    };
}

impl FrameDiagnosticAccumulator {
    pub(crate) const fn new() -> Self {
        Self {
            pending: FrameDiagnosticBatch::EMPTY,
            state_changes_at_last_draw: 0,
            last_draw: None,
        }
    }

    /// Count a draw, and count it as *adjacent* when nothing reached the
    /// driver since the previous draw. See
    /// [`FrameDiagnosticBatch::adjacent_draws`].
    ///
    /// Hand-written rather than `counter_method!` because it reads a second
    /// field: the adjacency has to be decided at the draw, since by drain time
    /// the interleaving is gone.
    #[inline]
    pub(crate) fn bump_draw_call(&mut self) {
        self.record_draw(None);
    }

    /// [`Self::bump_draw_call`] for a draw whose shape is known, so the
    /// contiguity behind [`FrameDiagnosticBatch::mergeable_draws`] can be
    /// tested.
    ///
    /// Draws that cannot describe a shape — Canvas2D paints, instanced draws —
    /// go through `bump_draw_call` and count toward `draw_calls` and adjacency
    /// but never toward `mergeable_draws`. That is the honest answer for them
    /// rather than a default: a Canvas2D paint has no vertex range to be
    /// contiguous with, and merging instanced draws is a different question.
    #[inline]
    pub(crate) fn bump_draw_call_shaped(&mut self, shape: DrawShape) {
        self.record_draw(Some(shape));
    }

    #[inline]
    fn record_draw(&mut self, shape: Option<DrawShape>) {
        let adjacent = self.pending.draw_calls > 0
            && self.pending.state_changes == self.state_changes_at_last_draw;
        if adjacent {
            self.pending.adjacent_draws = self.pending.adjacent_draws.saturating_add(1);

            // Mergeable only if adjacent *and* this draw continues the previous
            // range. Both shapes must be known: an unshaped draw in between
            // breaks the chain, which is correct — whatever it painted sits
            // between them.
            if let (Some(prev), Some(next)) = (self.last_draw, shape)
                && prev.continues_into(next)
            {
                self.pending.mergeable_draws = self.pending.mergeable_draws.saturating_add(1);
            }
        }
        self.state_changes_at_last_draw = self.pending.state_changes;
        self.last_draw = shape;
        self.pending.draw_calls = self.pending.draw_calls.saturating_add(1);
    }
    counter_method!(bump_state_change, state_changes);
    counter_method!(hit_measure_cache, measure_text_hits);
    counter_method!(miss_measure_cache, measure_text_misses);
    counter_method!(hit_shape_cache, shape_cache_hits);
    counter_method!(miss_shape_cache, shape_cache_misses);
    counter_method!(hit_sk_image_wrapper, sk_image_wrapper_hits);
    counter_method!(miss_sk_image_wrapper, sk_image_wrapper_misses);
    counter_method!(bump_skia_context_reset, skia_context_resets);
    counter_method!(bump_canvas2d_snapshot_taken, canvas2d_snapshots_taken);
    counter_method!(bump_canvas2d_snapshot_fallback, canvas2d_snapshot_fallbacks);
    counter_method!(bump_canvas2d_snapshot_upload, canvas2d_snapshot_uploads);
    counter_method!(
        bump_canvas2d_snapshot_forced_readback,
        canvas2d_snapshot_forced_readbacks
    );
    counter_method!(hit_shadow_filter_cache, shadow_filter_hits);
    counter_method!(miss_shadow_filter_cache, shadow_filter_misses);
    counter_method!(hit_dash_effect_cache, dash_effect_hits);
    counter_method!(miss_dash_effect_cache, dash_effect_misses);
    counter_method!(hit_gradient_cache, gradient_hits);
    counter_method!(miss_gradient_cache, gradient_misses);
    counter_method!(hit_text_cache, text_cache_hits);
    counter_method!(miss_text_cache, text_cache_misses);

    #[inline]
    pub(crate) fn add_upload_bytes(&mut self, bytes: u32) {
        self.pending.texture_upload_bytes = self.pending.texture_upload_bytes.saturating_add(bytes);
    }

    #[inline]
    pub(crate) fn set_text_cache_gauges(&mut self, bytes: u32, entries: u32) {
        self.pending.text_cache_bytes = Some(bytes);
        self.pending.text_cache_entries = Some(entries);
    }

    #[inline]
    pub(crate) fn set_render_queue_len(&mut self, len: u32) {
        self.pending.render_queue_len = Some(len);
    }

    #[inline]
    pub(crate) fn set_sk_image_wrapper_count(&mut self, count: u32) {
        self.pending.sk_image_wrappers = Some(count);
    }

    #[inline]
    pub(crate) fn set_deferred_uploads(&mut self, count: u32) {
        self.pending.deferred_uploads = Some(count);
    }

    #[inline]
    pub(crate) fn drain(&mut self) -> FrameDiagnosticBatch {
        // `state_changes_at_last_draw` deliberately *not* reset here. It looks
        // like it should be, and a first version did — but the value is
        // unreadable while stale: the adjacency check runs only when
        // `pending.draw_calls > 0`, which means this frame already had a draw
        // and that draw already overwrote the reference. Removing the reset
        // failed no test, which is how it was found to be dead. The `draw_calls`
        // guard in `bump_draw_call` is the mechanism, and
        // `the_first_draw_of_a_frame_is_never_adjacent` covers it.
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod adjacency {
    use super::*;

    /// Replay a frame described as a string: `d` is a draw, `s` is a driver
    /// state change. Returns `(draw_calls, adjacent_draws)`.
    ///
    /// A string because the property under test is about *interleaving*, and a
    /// string shows the interleaving in the test rather than burying it in a
    /// loop.
    fn replay(frame: &str) -> (u32, u32) {
        let mut acc = FrameDiagnosticAccumulator::new();
        for c in frame.chars() {
            match c {
                'd' => acc.bump_draw_call(),
                's' => acc.bump_state_change(),
                other => panic!("frame script has {other:?}; use 'd' or 's'"),
            }
        }
        let batch = acc.drain();
        (batch.draw_calls, batch.adjacent_draws)
    }

    const TRIANGLES: u32 = glow::TRIANGLES;
    const LINES: u32 = glow::LINES;

    fn arrays(start: i32, len: i32) -> DrawShape {
        DrawShape {
            mode: TRIANGLES,
            elements: false,
            start,
            len,
        }
    }

    /// Replay a frame of shaped draws. `None` marks a state change.
    fn replay_shaped(events: &[Option<DrawShape>]) -> (u32, u32, u32) {
        let mut acc = FrameDiagnosticAccumulator::new();
        for e in events {
            match e {
                Some(shape) => acc.bump_draw_call_shaped(*shape),
                None => acc.bump_state_change(),
            }
        }
        let b = acc.drain();
        (b.draw_calls, b.adjacent_draws, b.mergeable_draws)
    }

    /// **The case that justifies separating the two counters.**
    ///
    /// Sixty-four draws of the same six vertices, nothing between them: every
    /// draw after the first is adjacent, and *none* is mergeable. Folding them
    /// into one draw of 384 vertices would paint different pixels, so a batching
    /// pass has nothing to do here however adjacent the draws look.
    ///
    /// This is exactly what `draw-batching-sprite` measures on a real driver —
    /// 98.4% adjacent — and why that figure alone could not have opened the
    /// batching question.
    #[test]
    fn redrawing_one_range_is_all_adjacent_and_none_mergeable() {
        let events: Vec<Option<DrawShape>> = std::iter::once(None)
            .chain((0..64).map(|_| Some(arrays(0, 6))))
            .collect();
        assert_eq!(replay_shaped(&events), (64, 63, 0));
    }

    /// A sprite batch that walks a shared buffer: each draw continues the last,
    /// so every adjacent draw is genuinely mergeable.
    #[test]
    fn a_walk_through_one_buffer_is_fully_mergeable() {
        let events: Vec<Option<DrawShape>> = std::iter::once(None)
            .chain((0..64).map(|i| Some(arrays(i * 6, 6))))
            .collect();
        assert_eq!(replay_shaped(&events), (64, 63, 63));
    }

    /// What blocks a merge, one reason at a time.
    #[test]
    fn a_merge_needs_mode_unit_and_contiguity_all_three() {
        // Contiguous, same mode: mergeable.
        assert_eq!(
            replay_shaped(&[Some(arrays(0, 6)), Some(arrays(6, 6))]).2,
            1
        );

        // A gap: the second draw skips vertices 6..12.
        assert_eq!(
            replay_shaped(&[Some(arrays(0, 6)), Some(arrays(12, 6))]).2,
            0
        );

        // Overlap: contiguity is strict, because a merge must preserve what is
        // painted and an overlapping merge would not.
        assert_eq!(
            replay_shaped(&[Some(arrays(0, 6)), Some(arrays(3, 6))]).2,
            0
        );

        // Different primitive mode, perfectly contiguous.
        let lines = DrawShape {
            mode: LINES,
            elements: false,
            start: 6,
            len: 6,
        };
        assert_eq!(replay_shaped(&[Some(arrays(0, 6)), Some(lines)]).2, 0);

        // Same numbers, but one counts vertices and the other indices. A
        // `drawArrays` ending at vertex 6 is not continued by a `drawElements`
        // starting at index 6 — they are not the same kind of position.
        let elements = DrawShape {
            mode: TRIANGLES,
            elements: true,
            start: 6,
            len: 6,
        };
        assert_eq!(replay_shaped(&[Some(arrays(0, 6)), Some(elements)]).2, 0);

        // A state change between two contiguous draws breaks adjacency, and
        // mergeability with it.
        assert_eq!(
            replay_shaped(&[Some(arrays(0, 6)), None, Some(arrays(6, 6))]),
            (2, 0, 0)
        );

        // An empty draw cannot anchor a merge in either direction.
        assert_eq!(
            replay_shaped(&[Some(arrays(0, 0)), Some(arrays(0, 6))]).2,
            0
        );
        assert_eq!(
            replay_shaped(&[Some(arrays(0, 6)), Some(arrays(6, 0))]).2,
            0
        );
    }

    /// A draw with no shape — a Canvas2D paint, an instanced draw — counts and
    /// breaks the chain, rather than being assumed contiguous with its
    /// neighbours.
    #[test]
    fn an_unshaped_draw_breaks_the_mergeable_chain() {
        let mut acc = FrameDiagnosticAccumulator::new();
        acc.bump_draw_call_shaped(arrays(0, 6));
        acc.bump_draw_call(); // unshaped: a Canvas2D paint between two GL draws
        acc.bump_draw_call_shaped(arrays(6, 6));
        let b = acc.drain();
        assert_eq!(
            (b.draw_calls, b.adjacent_draws, b.mergeable_draws),
            (3, 2, 0),
            "an unshaped draw was treated as though it painted nothing between \
             the two GL draws"
        );
    }

    /// The index-unit conversion, since a byte offset compared against an index
    /// count is the easiest way to invent contiguity that is not there.
    #[test]
    fn byte_offsets_convert_to_index_positions() {
        assert_eq!(indices_from_byte_offset(0, glow::UNSIGNED_SHORT), 0);
        assert_eq!(indices_from_byte_offset(12, glow::UNSIGNED_SHORT), 6);
        assert_eq!(indices_from_byte_offset(12, glow::UNSIGNED_BYTE), 12);
        assert_eq!(indices_from_byte_offset(12, glow::UNSIGNED_INT), 3);
        assert_eq!(
            indices_from_byte_offset(12, glow::FLOAT),
            -1,
            "an unrecognised index type must not land on a real index position"
        );

        // End to end: 6 shorts from byte 0 is continued by 6 shorts from byte 12.
        let first = DrawShape {
            mode: TRIANGLES,
            elements: true,
            start: indices_from_byte_offset(0, glow::UNSIGNED_SHORT),
            len: 6,
        };
        let second = DrawShape {
            mode: TRIANGLES,
            elements: true,
            start: indices_from_byte_offset(12, glow::UNSIGNED_SHORT),
            len: 6,
        };
        assert!(first.continues_into(second));
    }

    /// The two bounding cases, which are the whole reason the counter exists.
    #[test]
    fn a_sprite_batch_is_all_adjacent_and_a_material_switch_is_none() {
        // Set state once, then draw ten times: nine merge candidates.
        assert_eq!(replay("sssdddddddddd"), (10, 9));

        // One state change per draw — a material switch between every sprite.
        // Nothing is adjacent, so batching has nothing to work with.
        assert_eq!(replay("sdsdsdsdsdsdsdsdsdsd"), (10, 0));
    }

    /// The first draw of a frame has no predecessor, so it is never adjacent —
    /// otherwise every frame would report one merge that cannot exist.
    #[test]
    fn the_first_draw_of_a_frame_is_never_adjacent() {
        assert_eq!(replay("d"), (1, 0));
        assert_eq!(replay("sd"), (1, 0));
        assert_eq!(replay("dd"), (2, 1));
    }

    /// Adjacency is per frame, and **the `draw_calls > 0` guard is what makes it
    /// so** — not any resetting at drain time.
    ///
    /// This test's first version claimed the opposite. It drained a frame and
    /// asserted that the next frame's opening draw was not scored against the
    /// previous one, on the theory that a reset in `drain` prevented it. Removing
    /// that reset left the test green, because the stale reference is unreadable:
    /// the check runs only when a draw has already happened *this* frame, and
    /// that draw overwrote the reference. Deleting the guard instead turns this
    /// red, and `the_first_draw_of_a_frame_is_never_adjacent` with it.
    ///
    /// Kept, rewritten, because the property is worth pinning even though the
    /// mechanism turned out to be somewhere else: a multi-frame sequence must
    /// score each frame on its own interleaving.
    #[test]
    fn each_frame_is_scored_on_its_own_interleaving() {
        let mut acc = FrameDiagnosticAccumulator::new();
        let mut frames = Vec::new();

        // Frame 1: setup then a sprite run. Frame 2: opens with a draw, then a
        // material switch per draw. Frame 3: a single draw.
        for script in ["ssddd", "dsdsd", "d"] {
            for c in script.chars() {
                match c {
                    'd' => acc.bump_draw_call(),
                    's' => acc.bump_state_change(),
                    _ => unreachable!(),
                }
            }
            let b = acc.drain();
            frames.push((b.draw_calls, b.adjacent_draws));
        }

        assert_eq!(
            frames,
            vec![(3, 2), (3, 0), (1, 0)],
            "frames were not scored independently; got {frames:?}"
        );
    }

    /// Mixed shapes, spelled out, because this is the arithmetic every reported
    /// number depends on.
    #[test]
    fn mixed_frames_score_the_way_the_interleaving_reads() {
        // Two runs of three draws, separated by a state change: 2 + 2 adjacent.
        assert_eq!(replay("dddsddd"), (6, 4));
        // A run, then alternating: 4 adjacent in the run, 0 after.
        assert_eq!(replay("dddddsdsdsd"), (8, 4));
        // State churn with no draws at all scores nothing.
        assert_eq!(replay("ssssss"), (0, 0));
        // Draws with churn before them all, none between: still all adjacent.
        assert_eq!(replay("ssssddd"), (3, 2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cleanup_is_not_due_before_250_ms() {
        let cadence = DeferredCleanupCadence::new(Duration::from_millis(10));

        assert!(!cadence.should_run(Duration::from_millis(259)));
    }

    #[test]
    fn cleanup_is_due_at_250_ms() {
        let cadence = DeferredCleanupCadence::new(Duration::from_millis(10));

        assert!(cadence.should_run(Duration::from_millis(260)));
        assert!(!cadence.should_run(Duration::from_millis(260)));
    }

    #[test]
    fn cleanup_coalesces_missed_intervals_without_catch_up() {
        let cadence = DeferredCleanupCadence::new(Duration::ZERO);

        assert!(cadence.should_run(Duration::from_secs(5)));
        assert!(!cadence.should_run(Duration::from_secs(5)));
        assert!(!cadence.should_run(Duration::from_millis(5_249)));
        assert!(cadence.should_run(Duration::from_millis(5_250)));
    }

    #[test]
    fn forced_cleanup_restarts_the_deadline() {
        let cadence = DeferredCleanupCadence::new(Duration::ZERO);

        cadence.mark_forced(Duration::from_millis(240));
        assert!(!cadence.should_run(Duration::from_millis(489)));
        assert!(cadence.should_run(Duration::from_millis(490)));
    }

    #[test]
    fn regressed_timestamp_does_not_trigger_cleanup() {
        let cadence = DeferredCleanupCadence::new(Duration::from_secs(2));

        assert!(!cadence.should_run(Duration::from_secs(1)));
        assert!(!cadence.should_run(Duration::from_millis(2_249)));
        assert!(cadence.should_run(Duration::from_millis(2_250)));
    }

    #[test]
    fn diagnostic_drain_returns_deltas_and_resets_them() {
        let mut diagnostics = FrameDiagnosticAccumulator::default();
        diagnostics.bump_draw_call();
        diagnostics.bump_draw_call();
        diagnostics.hit_shadow_filter_cache();
        diagnostics.add_upload_bytes(42);

        let first = diagnostics.drain();
        assert_eq!(first.draw_calls, 2);
        assert_eq!(first.shadow_filter_hits, 1);
        assert_eq!(first.texture_upload_bytes, 42);

        let second = diagnostics.drain();
        assert_eq!(second.draw_calls, 0);
        assert_eq!(second.shadow_filter_hits, 0);
        assert_eq!(second.texture_upload_bytes, 0);
    }

    #[test]
    fn diagnostic_gauges_publish_latest_sample_only() {
        let mut diagnostics = FrameDiagnosticAccumulator::default();
        diagnostics.set_render_queue_len(7);
        diagnostics.set_render_queue_len(3);
        diagnostics.set_text_cache_gauges(8_192, 12);
        diagnostics.set_text_cache_gauges(4_096, 5);

        let batch = diagnostics.drain();
        assert_eq!(batch.render_queue_len, Some(3));
        assert_eq!(batch.text_cache_bytes, Some(4_096));
        assert_eq!(batch.text_cache_entries, Some(5));

        let empty = diagnostics.drain();
        assert_eq!(empty.render_queue_len, None);
        assert_eq!(empty.text_cache_bytes, None);
        assert_eq!(empty.text_cache_entries, None);
    }

    #[test]
    fn diagnostic_local_arithmetic_saturates() {
        let mut diagnostics = FrameDiagnosticAccumulator::default();
        diagnostics.add_upload_bytes(u32::MAX - 1);
        diagnostics.add_upload_bytes(10);
        for _ in 0..3 {
            diagnostics.bump_canvas2d_snapshot_taken();
        }

        let batch = diagnostics.drain();
        assert_eq!(batch.texture_upload_bytes, u32::MAX);
        assert_eq!(batch.canvas2d_snapshots_taken, 3);
    }

    #[test]
    fn render_thread_installs_and_flushes_session_diagnostics() {
        let source = include_str!("render_thread.rs");
        assert_eq!(
            source
                .matches("render_diagnostics::install(debug_stats.clone())")
                .count(),
            1,
            "one render thread must bind exactly one session-local sink"
        );
        assert_eq!(
            source.matches("render_diagnostics::flush_frame()").count(),
            1,
            "physical-frame tail must publish diagnostics exactly once"
        );
    }

    /// Presentation is coalesced by the physical-frame loop, so executing a
    /// packet's ops must never sweep every live context — that sweep is what
    /// made multiple packets in one display frame cost multiple all-context
    /// walks.
    ///
    /// **Scoped to the whole op executor rather than to the `Present` arm.** The
    /// earlier version split the source on `"FrameOp::Present => {"` and
    /// inspected the text up to the next arm. That pinned one arm out of five,
    /// so a sweep introduced in `Materialize` or `GlBatch` passed it; and it was
    /// coupled to the arm's brace style closely enough that turning the arm into
    /// an expression broke the extraction rather than the property. Both are the
    /// same mistake — a guard covering only the face it was written for — so the
    /// assertion now covers every arm and does not depend on how any of them is
    /// written.
    #[test]
    fn executing_a_frame_op_never_sweeps_every_context() {
        let source = include_str!("render_thread.rs");
        let executor = source
            .split_once("fn execute_frame_op(")
            .and_then(|(_, tail)| tail.split_once("\nfn "))
            .map(|(body, _)| body)
            .expect("execute_frame_op is where a packet's ops are executed");

        assert!(
            !executor.contains("perform_deferred_cleanup"),
            "executing one op performs the deferred-cleanup sweep, which belongs \
             to the physical-frame tail"
        );
        assert!(
            !executor.contains("contexts_2d_iter_mut"),
            "executing one op walks every live 2D context"
        );
    }

    /// A host that posts a frame callback per request delivers two vsyncs with one
    /// frame time when two requests precede the display's next frame; each ran a
    /// frame, and content got two animation frames with one timestamp (a 0 ms step,
    /// 4 runs in 6 on a Mate 30). The vsync branch drops a frame time it has handled
    /// before it can signal RAF, and only then records the new one.
    ///
    /// The repeat still answered what was asked, and can be the only answer: the
    /// frame the render thread asked for, when a host answers two requests back to
    /// back and the second's frame time converts no later than the first's. Dropped
    /// whole -- no arm -- it left the request noted as asked forever and nothing asked
    /// the host again, so the loop stopped with content awaiting a frame (the macOS
    /// headless host on CI: two frames requested, two answered, sixty seconds of
    /// nothing). So the branch clears the request before the check, and a repeat that
    /// runs no frame arms again for demand that remains.
    #[test]
    fn a_vsync_for_a_frame_already_handled_runs_no_frame_and_still_lets_the_next_be_asked() {
        let source = include_str!("render_thread.rs");
        let branch = source
            .split_once("Wake::Vsync => {")
            .map(|(_, tail)| tail)
            .expect("the render loop has a vsync branch");
        let received = branch
            .find("vsync.try_recv()")
            .expect("the branch takes the frame time");
        let cleared = branch
            .find("forget_frame_request();")
            .expect("the vsync clears what was asked");
        let dropped = branch
            .find("if frame_time_ms <= last_vsync_ms {")
            .expect("a frame time already handled is dropped");
        let recorded = branch
            .find("last_vsync_ms = frame_time_ms;")
            .expect("the handled frame time is recorded");
        let signalled = branch.find("signal_raf(").expect("the branch signals RAF");
        assert!(
            received < cleared && cleared < dropped && dropped < recorded && recorded < signalled,
            "the vsync clears the request, and a repeated frame time is dropped before anything of the frame runs"
        );
        let skip = &branch[dropped..recorded];
        assert!(
            skip.contains("arm_if_needed(")
                && skip.contains("continue;")
                && !skip.contains("signal_raf("),
            "a repeat runs no frame, and arms again for demand that remains"
        );
    }

    #[test]
    fn periodic_and_pressure_cleanup_are_wired_without_a_timer() {
        let render = include_str!("render_thread.rs");
        let manager = include_str!("canvas/manager/mod.rs");

        assert!(render.contains("cleanup_cadence.should_run(start_time.elapsed())"));
        assert!(render.matches("cleanup_cadence.mark_forced").count() >= 2);
        assert!(render.contains("cm.perform_deferred_cleanup_all("));
        assert!(manager.contains("fn perform_deferred_cleanup_all("));
        assert!(!render.contains("tick(DEFERRED_CLEANUP_INTERVAL)"));
        assert!(!render.contains("sleep(DEFERRED_CLEANUP_INTERVAL)"));
    }
}
