//! Per-render-thread diagnostic accumulation.
//!
//! Hot draw/cache helpers update native thread-local ordinary integers. The
//! render thread publishes those deltas to its session `DebugStats` once at the
//! end of a physical frame. Calls on threads without an installed sink are
//! cheap no-ops and cannot contaminate another host's metrics.

use std::cell::{RefCell, UnsafeCell};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use shared::stats::DebugStats;

use crate::render_frame_state::{FrameDiagnosticAccumulator, FrameDiagnosticBatch};

struct HotDiagnostics {
    active: bool,
    accumulator: FrameDiagnosticAccumulator,
}

thread_local! {
    // No destructor and a const initializer keep the draw-path TLS access on
    // the platform's native fast TLS path. The private helper below is the only
    // way to obtain a mutable reference.
    static HOT: UnsafeCell<HotDiagnostics> = const {
        UnsafeCell::new(HotDiagnostics {
            active: false,
            accumulator: FrameDiagnosticAccumulator::new(),
        })
    };

    // The Arc is cold: touched only on install, frame flush, and test teardown.
    static SINK: RefCell<Option<Arc<DebugStats>>> = const { RefCell::new(None) };
}

/// Bind diagnostic publication to the current render thread and session.
/// Reinstalling replaces the sink and discards unpublished deltas.
pub fn install(stats: Arc<DebugStats>) {
    SINK.with(|sink| {
        *sink.borrow_mut() = Some(stats);
    });
    HOT.with(|hot| {
        // SAFETY: `HOT` is thread-local. No reference leaves this closure, and
        // every mutation entry point in this module is synchronous and
        // non-reentrant.
        let hot = unsafe { &mut *hot.get() };
        hot.accumulator = FrameDiagnosticAccumulator::new();
        hot.active = true;
    });
}

/// Test-only teardown for the current thread's sink.
#[cfg(test)]
pub fn uninstall_for_tests() {
    HOT.with(|hot| {
        // SAFETY: same thread-local, non-escaping invariant as `install`.
        let hot = unsafe { &mut *hot.get() };
        hot.active = false;
        hot.accumulator = FrameDiagnosticAccumulator::new();
    });
    SINK.with(|sink| {
        *sink.borrow_mut() = None;
    });
}

#[inline(always)]
fn with_accumulator(f: impl FnOnce(&mut FrameDiagnosticAccumulator)) {
    HOT.with(|hot| {
        // SAFETY: `HOT` has one instance per thread, no mutable reference
        // escapes, and `f` is always an internal non-reentrant field update.
        let hot = unsafe { &mut *hot.get() };
        if hot.active {
            f(&mut hot.accumulator);
        }
    });
}

#[inline]
fn add_if_nonzero(dst: &std::sync::atomic::AtomicU32, delta: u32) {
    if delta != 0 {
        dst.fetch_add(delta, Ordering::Relaxed);
    }
}

fn publish(stats: &DebugStats, batch: FrameDiagnosticBatch) {
    add_if_nonzero(&stats.draw_calls, batch.draw_calls);
    add_if_nonzero(&stats.state_changes, batch.state_changes);
    add_if_nonzero(&stats.adjacent_draws, batch.adjacent_draws);
    add_if_nonzero(&stats.mergeable_draws, batch.mergeable_draws);
    // This is a per-frame gauge, not a cumulative byte counter.
    stats
        .texture_upload_bytes
        .store(batch.texture_upload_bytes, Ordering::Relaxed);
    add_if_nonzero(&stats.measure_text_hits, batch.measure_text_hits);
    add_if_nonzero(&stats.measure_text_misses, batch.measure_text_misses);
    add_if_nonzero(&stats.shape_cache_hits, batch.shape_cache_hits);
    add_if_nonzero(&stats.shape_cache_misses, batch.shape_cache_misses);
    add_if_nonzero(&stats.sk_image_wrapper_hits, batch.sk_image_wrapper_hits);
    add_if_nonzero(
        &stats.sk_image_wrapper_misses,
        batch.sk_image_wrapper_misses,
    );
    add_if_nonzero(&stats.skia_context_resets, batch.skia_context_resets);
    add_if_nonzero(
        &stats.canvas2d_snapshots_taken,
        batch.canvas2d_snapshots_taken,
    );
    add_if_nonzero(
        &stats.canvas2d_snapshot_fallbacks,
        batch.canvas2d_snapshot_fallbacks,
    );
    add_if_nonzero(
        &stats.canvas2d_snapshot_uploads,
        batch.canvas2d_snapshot_uploads,
    );
    add_if_nonzero(
        &stats.canvas2d_snapshot_forced_readbacks,
        batch.canvas2d_snapshot_forced_readbacks,
    );
    add_if_nonzero(&stats.shadow_filter_hits, batch.shadow_filter_hits);
    add_if_nonzero(&stats.shadow_filter_misses, batch.shadow_filter_misses);
    add_if_nonzero(&stats.dash_effect_hits, batch.dash_effect_hits);
    add_if_nonzero(&stats.dash_effect_misses, batch.dash_effect_misses);
    add_if_nonzero(&stats.gradient_hits, batch.gradient_hits);
    add_if_nonzero(&stats.gradient_misses, batch.gradient_misses);
    add_if_nonzero(&stats.text_cache_hits, batch.text_cache_hits);
    add_if_nonzero(&stats.text_cache_misses, batch.text_cache_misses);

    if let Some(value) = batch.text_cache_bytes {
        stats.text_cache_bytes.store(value, Ordering::Relaxed);
    }
    if let Some(value) = batch.text_cache_entries {
        stats.text_cache_entries.store(value, Ordering::Relaxed);
    }
    if let Some(value) = batch.render_queue_len {
        stats.render_queue_len.store(value, Ordering::Relaxed);
    }
    if let Some(value) = batch.sk_image_wrappers {
        stats.sk_image_wrappers.store(value, Ordering::Relaxed);
    }
    if let Some(value) = batch.deferred_uploads {
        stats.deferred_uploads.store(value, Ordering::Relaxed);
    }
}

/// Publish and reset the current thread's pending diagnostic frame.
pub fn flush_frame() {
    let batch = HOT.with(|hot| {
        // SAFETY: same thread-local, non-escaping invariant as the hot helper.
        let hot = unsafe { &mut *hot.get() };
        hot.active.then(|| hot.accumulator.drain())
    });
    let Some(batch) = batch else {
        return;
    };
    SINK.with(|sink| {
        if let Some(stats) = sink.borrow().as_deref() {
            publish(stats, batch);
        }
    });
}

macro_rules! forward_counter {
    ($function:ident, $method:ident) => {
        #[inline(always)]
        pub fn $function() {
            with_accumulator(|diagnostics| diagnostics.$method());
        }
    };
}

forward_counter!(bump_draw_call, bump_draw_call);

/// [`bump_draw_call`] for a draw whose vertex or index range is known, so the
/// contiguity behind `mergeable_draws` can be tested. See
/// [`crate::render_frame_state::DrawShape`].
#[inline(always)]
pub(crate) fn bump_draw_call_shaped(shape: crate::render_frame_state::DrawShape) {
    with_accumulator(|diagnostics| diagnostics.bump_draw_call_shaped(shape));
}
forward_counter!(bump_state_change, bump_state_change);
forward_counter!(hit_measure_cache, hit_measure_cache);
forward_counter!(miss_measure_cache, miss_measure_cache);
forward_counter!(hit_shape_cache, hit_shape_cache);
forward_counter!(miss_shape_cache, miss_shape_cache);
forward_counter!(hit_sk_image_wrapper, hit_sk_image_wrapper);
forward_counter!(miss_sk_image_wrapper, miss_sk_image_wrapper);
forward_counter!(bump_skia_context_reset, bump_skia_context_reset);
forward_counter!(bump_canvas2d_snapshot_taken, bump_canvas2d_snapshot_taken);
forward_counter!(
    bump_canvas2d_snapshot_fallback,
    bump_canvas2d_snapshot_fallback
);
forward_counter!(bump_canvas2d_snapshot_upload, bump_canvas2d_snapshot_upload);
forward_counter!(
    bump_canvas2d_snapshot_forced_readback,
    bump_canvas2d_snapshot_forced_readback
);
forward_counter!(hit_shadow_filter_cache, hit_shadow_filter_cache);
forward_counter!(miss_shadow_filter_cache, miss_shadow_filter_cache);
forward_counter!(hit_dash_effect_cache, hit_dash_effect_cache);
forward_counter!(miss_dash_effect_cache, miss_dash_effect_cache);
forward_counter!(hit_gradient_cache, hit_gradient_cache);
forward_counter!(miss_gradient_cache, miss_gradient_cache);
forward_counter!(hit_text_cache, hit_text_cache);
forward_counter!(miss_text_cache, miss_text_cache);

#[inline(always)]
pub fn add_upload_bytes(bytes: u32) {
    with_accumulator(|diagnostics| diagnostics.add_upload_bytes(bytes));
}

#[inline(always)]
pub fn set_text_cache_gauges(bytes: u32, entries: u32) {
    with_accumulator(|diagnostics| diagnostics.set_text_cache_gauges(bytes, entries));
}

#[inline(always)]
pub fn set_render_queue_len(len: u32) {
    with_accumulator(|diagnostics| diagnostics.set_render_queue_len(len));
}

#[inline(always)]
pub fn set_sk_image_wrapper_count(count: u32) {
    with_accumulator(|diagnostics| diagnostics.set_sk_image_wrapper_count(count));
}

#[inline(always)]
pub fn set_deferred_uploads(count: u32) {
    with_accumulator(|diagnostics| diagnostics.set_deferred_uploads(count));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn bumps_are_noop_without_sink() {
        uninstall_for_tests();
        bump_draw_call();
        add_upload_bytes(1024);
        hit_measure_cache();
        flush_frame();
    }

    #[test]
    fn bumps_publish_only_at_frame_flush() {
        uninstall_for_tests();
        let stats = Arc::new(DebugStats::default());
        install(stats.clone());
        bump_draw_call();
        bump_draw_call();
        add_upload_bytes(42);
        hit_measure_cache();
        miss_measure_cache();
        assert_eq!(stats.draw_calls.load(Ordering::Relaxed), 0);
        assert_eq!(stats.texture_upload_bytes.load(Ordering::Relaxed), 0);

        flush_frame();
        assert_eq!(stats.draw_calls.load(Ordering::Relaxed), 2);
        assert_eq!(stats.texture_upload_bytes.load(Ordering::Relaxed), 42);
        assert_eq!(stats.measure_text_hits.load(Ordering::Relaxed), 1);
        assert_eq!(stats.measure_text_misses.load(Ordering::Relaxed), 1);

        flush_frame();
        assert_eq!(stats.draw_calls.load(Ordering::Relaxed), 2);
        assert_eq!(stats.measure_text_hits.load(Ordering::Relaxed), 1);
        uninstall_for_tests();
    }

    #[test]
    fn gauges_keep_latest_sample_until_flush() {
        uninstall_for_tests();
        let stats = Arc::new(DebugStats::default());
        install(stats.clone());
        set_render_queue_len(9);
        set_render_queue_len(4);
        set_text_cache_gauges(8_192, 12);
        set_text_cache_gauges(4_096, 5);

        flush_frame();
        assert_eq!(stats.render_queue_len.load(Ordering::Relaxed), 4);
        assert_eq!(stats.text_cache_bytes.load(Ordering::Relaxed), 4_096);
        assert_eq!(stats.text_cache_entries.load(Ordering::Relaxed), 5);
        uninstall_for_tests();
    }

    #[test]
    fn texture_upload_bytes_are_per_frame_not_cumulative() {
        uninstall_for_tests();
        let stats = Arc::new(DebugStats::default());
        install(stats.clone());
        add_upload_bytes(42);
        flush_frame();
        assert_eq!(stats.texture_upload_bytes.load(Ordering::Relaxed), 42);

        flush_frame();
        assert_eq!(stats.texture_upload_bytes.load(Ordering::Relaxed), 0);
        add_upload_bytes(7);
        flush_frame();
        assert_eq!(stats.texture_upload_bytes.load(Ordering::Relaxed), 7);
        uninstall_for_tests();
    }

    #[test]
    fn reinstall_discards_old_pending_deltas() {
        uninstall_for_tests();
        let old = Arc::new(DebugStats::default());
        install(old.clone());
        bump_draw_call();

        let replacement = Arc::new(DebugStats::default());
        install(replacement.clone());
        flush_frame();
        assert_eq!(old.draw_calls.load(Ordering::Relaxed), 0);
        assert_eq!(replacement.draw_calls.load(Ordering::Relaxed), 0);
        uninstall_for_tests();
    }

    #[test]
    fn sinks_are_isolated_by_thread() {
        uninstall_for_tests();
        let a = Arc::new(DebugStats::default());
        let b = Arc::new(DebugStats::default());
        let a_thread = a.clone();
        let b_thread = b.clone();

        let first = std::thread::spawn(move || {
            install(a_thread);
            bump_draw_call();
            bump_draw_call();
            flush_frame();
            uninstall_for_tests();
        });
        let second = std::thread::spawn(move || {
            install(b_thread);
            bump_draw_call();
            flush_frame();
            uninstall_for_tests();
        });
        first.join().unwrap();
        second.join().unwrap();

        assert_eq!(a.draw_calls.load(Ordering::Relaxed), 2);
        assert_eq!(b.draw_calls.load(Ordering::Relaxed), 1);
        uninstall_for_tests();
    }

    /// Per-session accounting test, for the gauges specifically.
    ///
    /// `sinks_are_isolated_by_thread` above cannot stand in for this. Counters
    /// publish with `fetch_add`, so a defect that merged two sessions' *gauges* while
    /// leaving the counters alone would leave it green: gauges publish with `store`,
    /// where merging means the session that flushed last silently speaks for both.
    ///
    /// The four phases are ordered rather than raced. Two threads setting gauges at
    /// once would catch a merged gauge only when the interleaving happened to cross,
    /// which is a coin flip; forcing *set A, set B, flush A, flush B* makes a merge
    /// certain to show up, because A's flush would then publish B's value.
    #[test]
    fn a_gauge_set_before_another_session_sets_its_own_still_reports_its_own() {
        uninstall_for_tests();
        let a = Arc::new(DebugStats::default());
        let b = Arc::new(DebugStats::default());
        let a_thread = a.clone();
        let b_thread = b.clone();
        // Four phases, one thread acting in each.
        let phase = Arc::new(std::sync::Barrier::new(2));
        let a_phase = Arc::clone(&phase);
        let b_phase = Arc::clone(&phase);

        // Deliberately different values: equal ones would pass under a merge.
        let first = std::thread::spawn(move || {
            install(a_thread);
            set_text_cache_gauges(8_192, 12);
            set_render_queue_len(3);
            a_phase.wait(); // 1: A has set
            a_phase.wait(); // 2: B has set
            flush_frame();
            a_phase.wait(); // 3: A has flushed
            a_phase.wait(); // 4: B has flushed
            uninstall_for_tests();
        });
        let second = std::thread::spawn(move || {
            install(b_thread);
            b_phase.wait(); // 1
            set_text_cache_gauges(4_096, 5);
            set_render_queue_len(9);
            b_phase.wait(); // 2
            b_phase.wait(); // 3
            flush_frame();
            b_phase.wait(); // 4
            uninstall_for_tests();
        });
        first.join().unwrap();
        second.join().unwrap();

        assert_eq!(a.text_cache_bytes.load(Ordering::Relaxed), 8_192);
        assert_eq!(a.text_cache_entries.load(Ordering::Relaxed), 12);
        assert_eq!(a.render_queue_len.load(Ordering::Relaxed), 3);
        assert_eq!(b.text_cache_bytes.load(Ordering::Relaxed), 4_096);
        assert_eq!(b.text_cache_entries.load(Ordering::Relaxed), 5);
        assert_eq!(b.render_queue_len.load(Ordering::Relaxed), 9);
        uninstall_for_tests();
    }
}
