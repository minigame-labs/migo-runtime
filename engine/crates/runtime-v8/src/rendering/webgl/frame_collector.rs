//! Unified frame command collector for Canvas2D + WebGL interleaving.
//!
//! Replaces the separate `FrameCommandCollector` + `GlBatchCollector` with a
//! single ordered timeline of segments. At frame end, one `FramePacket` is
//! built with `Materialize` barriers inserted at Canvas2D→GL transitions.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use shared::command_vec_pool::{PooledVec, take_canvas_command_vec, take_gl_command_vec};
use shared::protocol::frame_packet::FrameOp;
use shared::protocol::render_cmd::{
    Canvas2DCmd, CanvasBatchPayload, DirtyRect, GLCmd, GlBatchPayload,
};

// ── Segment types ──────────────────────────────────────────────────

pub(crate) struct Canvas2DSegment {
    pub canvas_id: u32,
    pub commands: PooledVec<Canvas2DCmd>,
    pub dirty_rect: Option<DirtyRect>,
    /// Once true, dirty_rect is permanently None for this segment.
    /// Set when a command with unknowable bounds is encountered (text,
    /// path draw, clip). Prevents the scissor hint from being used.
    dirty_poisoned: bool,
}

impl Canvas2DSegment {
    #[inline]
    fn mark_dirty(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if self.dirty_poisoned {
            return;
        }
        self.dirty_rect = Some(match self.dirty_rect {
            Some(rect) => {
                let nx = rect.x.min(x);
                let ny = rect.y.min(y);
                let nx2 = (rect.x + rect.width).max(x + w);
                let ny2 = (rect.y + rect.height).max(y + h);
                DirtyRect {
                    x: nx,
                    y: ny,
                    width: nx2 - nx,
                    height: ny2 - ny,
                }
            }
            None => DirtyRect {
                x,
                y,
                width: w,
                height: h,
            },
        });
    }

    /// Permanently invalidate dirty_rect for this segment.
    /// Called when a command has unknowable bounds (text, path, clip).
    #[inline]
    fn poison_dirty(&mut self) {
        self.dirty_rect = None;
        self.dirty_poisoned = true;
    }

    /// Classify one command's effect on the segment's scissor hint.
    ///
    /// **Every variant is listed, and the unreachable default poisons.** This
    /// hint becomes a real `glScissor` on the render thread —
    /// `render_thread::apply_scissor`, reached through `prepare_batch_scissor` —
    /// so a command whose damage this under-reports has its pixels clipped away,
    /// with no GL error and nothing in a log.
    ///
    /// `Canvas2DCmd` is `#[non_exhaustive]`, so a match from this crate cannot
    /// be exhaustive and the compiler will not report a new variant here. That
    /// attribute is a declaration that variants *will* be added — which is
    /// exactly why the catch-all previously reading `_ => {}` was the wrong
    /// default. It promised, of every command not yet written, that it damages
    /// nothing.
    ///
    /// So the default is now [`Self::poison_dirty`]: an unclassified command
    /// costs one full-segment repaint and cannot be wrong. The trade is
    /// deliberate and one-directional — correctness comes free, and a new
    /// command that really is boundable has to be added to the arms below to get
    /// its scissor back. Losing a little partial-update efficiency until someone
    /// notices is recoverable; shipping a clipped draw is not.
    ///
    /// The arms are spelled out rather than collapsed into that default because
    /// the classification *is* the documentation: each group below records why a
    /// command cannot widen a rect-shaped bound, and that reasoning is what a
    /// future reader needs in order to place a new variant correctly.
    ///
    /// Note the division of labour with the render thread's second gate.
    /// `canvas2d_dispatcher::state_allows_partial` independently discards the
    /// hint when the *authoritative* 2D state is unsafe (shadow, non-source-over
    /// blend, `globalAlpha != 1`, high miter limit, non-axis-aligned CTM). That
    /// gate covers state, not commands: it re-derives safety from what the state
    /// became, so a command that widens damage without touching one of those
    /// five fields passes it. Both layers are needed, and this one is the only
    /// one that sees commands.
    #[inline]
    fn mark_dirty_for_cmd(&mut self, cmd: &Canvas2DCmd) {
        match cmd {
            // ── Safe rect/image draws (known bounds, no state dependency) ──
            Canvas2DCmd::FillRect { x, y, w, h }
            | Canvas2DCmd::ClearRect { x, y, w, h } => {
                self.mark_dirty(*x, *y, *w, *h);
            }
            Canvas2DCmd::DrawImage { dx, dy, dw, dh, .. } => {
                self.mark_dirty(*dx, *dy, *dw, *dh);
            }
            Canvas2DCmd::DrawImageBatch { draws } => {
                for d in draws {
                    self.mark_dirty(d.dx, d.dy, d.dw, d.dh);
                }
            }

            // ── Draws with unknowable bounds — poison scissor hint ──
            Canvas2DCmd::StrokeRect { .. } // lineWidth expansion unknown JS-side
            | Canvas2DCmd::FillText { .. }
            | Canvas2DCmd::StrokeText { .. }
            | Canvas2DCmd::Fill
            | Canvas2DCmd::FillEvenOdd
            | Canvas2DCmd::Stroke
            | Canvas2DCmd::Clip
            | Canvas2DCmd::ClipEvenOdd => {
                self.poison_dirty();
            }

            // ── State changes that invalidate subsequent draw bounds ──
            // Transform commands: subsequent rect/image coords no longer
            // map 1:1 to screen pixels.
            Canvas2DCmd::SetTransform { .. }
            | Canvas2DCmd::Translate { .. }
            | Canvas2DCmd::Rotate { .. }
            | Canvas2DCmd::Scale { .. }
            | Canvas2DCmd::ResetTransform => {
                self.poison_dirty();
            }
            // Shadow commands: any of these becoming active expands draw bounds.
            Canvas2DCmd::SetShadowBlur { .. }
            | Canvas2DCmd::SetShadowColor { .. }
            | Canvas2DCmd::SetShadowOffsetX { .. }
            | Canvas2DCmd::SetShadowOffsetY { .. } => {
                self.poison_dirty();
            }
            // LineWidth: affects StrokeRect bounds (already poisoned above)
            // but also affects Stroke (path) which is already poisoned.
            // Poison anyway to be safe with any future stroke-related draws.
            Canvas2DCmd::SetLineWidth { .. } => {
                self.poison_dirty();
            }

            // Composite mode. `copy`, `source-in`, `destination-out`, `xor` and
            // friends write — or clear — pixels the source geometry never
            // covers, so a rect bound is meaningless under them.
            //
            // The render thread's `state_allows_partial` rejects any non
            // source-over blend too, and would catch this on its own. Poisoning
            // here as well is deliberate: this arm should say what the command
            // means rather than defer to a gate in another crate that happens to
            // agree today, and a mode change costs at most one segment repaint.
            Canvas2DCmd::SetCompositeOperation { .. } => {
                self.poison_dirty();
            }

            // A resize re-specifies the backing store, which per spec clears the
            // canvas and resets the context. The reallocation makes the surface
            // transparent whatever the scissor says, so the hint cannot clip a
            // draw here — but it can leave the *presented* window showing older
            // content outside a small rect while the canvas behind it went
            // transparent. Full repaint for the segment.
            Canvas2DCmd::ResizeCanvas { .. } => {
                self.poison_dirty();
            }

            // ── No effect on the bounds of a subsequent rect-shaped draw ──
            //
            // Path building: geometry accumulates but paints nothing. What
            // eventually paints it is `Fill` / `Stroke` / `Clip`, all poisoned
            // above, so the path itself never needs a bound.
            Canvas2DCmd::BeginPath
            | Canvas2DCmd::ClosePath
            | Canvas2DCmd::MoveTo { .. }
            | Canvas2DCmd::LineTo { .. }
            | Canvas2DCmd::QuadraticCurveTo { .. }
            | Canvas2DCmd::BezierCurveTo { .. }
            | Canvas2DCmd::Arc { .. }
            | Canvas2DCmd::ArcTo { .. }
            | Canvas2DCmd::Rect { .. }
            | Canvas2DCmd::Ellipse { .. } => {}

            // Paint styles: change the colour of covered pixels, never which
            // pixels are covered.
            Canvas2DCmd::SetFillStyle { .. }
            | Canvas2DCmd::SetStrokeStyle { .. }
            | Canvas2DCmd::SetFillStyleGradient { .. }
            | Canvas2DCmd::SetStrokeStyleGradient { .. }
            | Canvas2DCmd::SetFillStylePattern { .. }
            | Canvas2DCmd::SetStrokeStylePattern { .. }
            | Canvas2DCmd::SetGlobalAlpha { .. } => {}

            // Stroke geometry attributes. These do widen a stroke's bounds, but
            // every command that strokes — `Stroke`, `StrokeRect`, `StrokeText`
            // — is poisoned above, so no rect-shaped bound ever depends on them.
            // (`SetLineWidth` is nonetheless poisoned, one arm up, as a hedge
            // against a future bounded stroke.)
            Canvas2DCmd::SetLineCap { .. }
            | Canvas2DCmd::SetLineJoin { .. }
            | Canvas2DCmd::SetMiterLimit { .. }
            | Canvas2DCmd::SetLineDash { .. }
            | Canvas2DCmd::SetLineDashOffset { .. } => {}

            // Text attributes: only `FillText` / `StrokeText` consume them, and
            // both are poisoned above.
            Canvas2DCmd::SetFont { .. }
            | Canvas2DCmd::SetTextAlign { .. }
            | Canvas2DCmd::SetTextBaseline { .. }
            | Canvas2DCmd::SetTextDirection { .. } => {}

            // `save` / `restore` move the whole state, transform and composite
            // mode included — but the poison is sticky for the segment, so any
            // transform or mode change *inside* it has already poisoned by the
            // time a `restore` could put one back. Nothing left to widen.
            Canvas2DCmd::Save | Canvas2DCmd::Restore => {}

            // Reads, not writes: no pixel changes.
            Canvas2DCmd::MeasureText { .. }
            | Canvas2DCmd::GetImageData { .. }
            | Canvas2DCmd::CaptureSnapshot { .. }
            | Canvas2DCmd::ReadSnapshotPixels { .. } => {}

            // Context creation paints nothing.
            Canvas2DCmd::CreateContext2D => {}

            // Unreachable today — every variant above is named. Required
            // because `Canvas2DCmd` is `#[non_exhaustive]`, and it poisons
            // because that attribute means this arm is where a variant added
            // tomorrow arrives. See the classification note above: an
            // unclassified command must cost a repaint, not a clipped draw.
            _ => {
                self.poison_dirty();
            }
        }
    }
}

pub(crate) struct GlSegment {
    pub commands: PooledVec<GLCmd>,
}

pub(crate) enum FrameSegment {
    Canvas2D(Canvas2DSegment),
    GL(GlSegment),
}

// ── Current-segment tracker ────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum CurrentKind {
    None,
    Canvas2D(u32), // canvas_id
    GL,
}

// ── Collector ──────────────────────────────────────────────────────

pub(crate) struct UnifiedFrameCollector {
    segments: Vec<FrameSegment>,
    current: CurrentKind,
    /// Rough byte count of all commands pushed since the last flush.
    /// Incremented in `push_canvas2d` / `push_gl` and zeroed when
    /// `build_frame_packet_inner` consumes the segments.  See
    /// [`AUTO_FLUSH_SOFT_BUDGET_BYTES`] for the flush policy.
    pending_bytes: usize,
    /// High-water mark across every partial batch in the current logical JS
    /// frame. A sync/auto-flush barrier resets `pending_bytes` but not this
    /// value; only `build_frame_packet` publishes and resets it.
    frame_peak_bytes: usize,
    /// Which canvases hold unmaterialised 2D work at the point the segment scan
    /// has reached. Reset at the start of every packet and at every
    /// Canvas2D→GL boundary.
    ///
    /// **A field rather than a local because a scene decides its size and
    /// nothing caps a scene.** One entry per distinct Canvas2D target in the
    /// run, which for the UI this collector was profiled against is one per
    /// text label; above the set's inline capacity a local would allocate and
    /// free on the thread running the game *every frame*, for as long as the
    /// scene is on screen. Held here, that spill is paid once.
    pending_2d: shared::protocol::CanvasIdSet,
    diagnostics_host_id: Option<i32>,
    diagnostics_stats: Option<Arc<shared::stats::DebugStats>>,
    /// How many times this frame's commands crossed from JavaScript into
    /// native, and what they carried.
    boundary: BoundaryCounts,
}

/// The JavaScript-to-native crossing count, which is the whole of
/// `android-ceiling-review.md`'s G2 and the number a command stream exists to
/// move.
///
/// **Counted rather than inferred.** The alternative is reading it off the shape
/// of the code -- "2D goes through the stream now, so it must be one" -- which
/// is exactly the reasoning that cannot notice a call site that quietly fell
/// back to an op. A frame that encodes three hundred commands and crosses three
/// hundred times looks identical from the outside to one that crosses once.
///
/// It is a window rather than a frame: at sixty frames a second a per-frame line
/// is unreadable and a single frame is a sample, not a summary. The window
/// closes when the rate gate lets a line through.
#[derive(Default)]
struct BoundaryCounts {
    /// Crossings in the frame being built.
    crossings: u32,
    /// Frames, crossings and commands since the last line was emitted.
    window_frames: u32,
    window_crossings: u64,
    window_commands: u64,
    window_worst_frame: u32,
}

/// Soft cap on the approximate byte-size of pending commands in the
/// collector.  When a single-frame batch crosses this threshold, call
/// sites should invoke [`UnifiedFrameCollector::flush_as_barrier`]
/// to hand the accumulated work off to the render thread rather than
/// keeping it stacked in JS-side memory.
///
/// Chromium's `CanvasResourceProvider::auto_flush` drives its backlog
/// cutoff from per-recording pinned-image byte budgets, not command
/// counts: a few MB of `bufferData` pinned by a single call can
/// outweigh thousands of tiny `fillRect`s.  We mirror that model by
/// tracking bytes rather than just segment count.
///
/// 4 MB is a conservative starting point — large enough to cover
/// typical small-game frames (hundreds of draws, kilobytes of
/// uniforms) without waking the render thread mid-frame for every
/// workload, small enough that a runaway `bufferData(4_MB_mesh)` or a
/// gigantic DrawImageBatch can't pin the JS heap unbounded while the
/// render thread is blocked.
pub(crate) const AUTO_FLUSH_SOFT_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// Frames per boundary-counting window.
///
/// Five seconds at sixty frames a second, matched to the rate gate so a window
/// is closed at roughly the moment its line is emitted. They are two clocks and
/// they will drift; what matters is that neither can run away, and that a line
/// always describes a window rather than a single frame.
const BOUNDARY_WINDOW_FRAMES: u32 = 300;

impl UnifiedFrameCollector {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::new_inner(None, None)
    }

    pub(crate) fn with_host_id(host_id: i32) -> Self {
        Self::new_inner(Some(host_id), None)
    }

    #[cfg(test)]
    fn with_diagnostics(stats: Arc<shared::stats::DebugStats>) -> Self {
        Self::new_inner(None, Some(stats))
    }

    fn new_inner(
        diagnostics_host_id: Option<i32>,
        diagnostics_stats: Option<Arc<shared::stats::DebugStats>>,
    ) -> Self {
        Self {
            segments: Vec::new(),
            current: CurrentKind::None,
            pending_bytes: 0,
            frame_peak_bytes: 0,
            pending_2d: shared::protocol::CanvasIdSet::new(),
            diagnostics_host_id,
            diagnostics_stats,
            boundary: BoundaryCounts::default(),
        }
    }

    /// One call from JavaScript reached the collector.
    ///
    /// Called from the two funnels every direct op goes through and once per
    /// submitted command-stream buffer, so a buffer carrying three hundred
    /// records counts one -- which is the distinction the number exists to make.
    #[inline]
    pub(crate) fn record_boundary_crossing(&mut self) {
        self.boundary.crossings = self.boundary.crossings.saturating_add(1);
    }

    /// `(frames, crossings, commands)` accumulated since the window last closed.
    #[cfg(test)]
    pub(crate) fn boundary_window_for_test(&self) -> (u32, u64, u64) {
        (
            self.boundary.window_frames,
            self.boundary.window_crossings,
            self.boundary.window_commands,
        )
    }

    #[inline]
    fn record_pending_peak(&mut self) {
        self.frame_peak_bytes = self.frame_peak_bytes.max(self.pending_bytes);
    }

    /// The open GL segment, opening one if the current segment is not GL.
    ///
    /// **Exists so the invariant has one home.** `current == CurrentKind::GL`
    /// implies the last segment is a `FrameSegment::GL`, and both fields are
    /// private to this type, so a violation could only be a bug in this file.
    /// Three call sites used to establish that and then re-derive it with
    /// `if let Some(FrameSegment::GL(seg)) = self.segments.last_mut()`, whose
    /// `else` was to fall out of the block — silently dropping the command
    /// while `pending_bytes` had already counted it.
    ///
    /// A dropped render command is the worst shape a failure can take here:
    /// no error, no log, a frame that is simply wrong, and — because the
    /// violation would be structural rather than per-command — every push after
    /// it dropped too. `build_frame_packet_inner` already carries a "defence in
    /// depth" reset for the *mirror* of this (`pending_bytes` counted but no
    /// segment landed), so the concern was live; what was missing was making the
    /// cause loud rather than papering over the symptom.
    ///
    /// `expect` follows the same reasoning as
    /// `SurfaceRecreateGuard::candidate` in `migo-graphics`: a structural
    /// invariant that the type alone controls is asserted, not degraded around.
    #[inline]
    fn gl_segment(&mut self) -> &mut GlSegment {
        if self.current != CurrentKind::GL {
            self.segments.push(FrameSegment::GL(GlSegment {
                commands: take_gl_command_vec(),
            }));
            self.current = CurrentKind::GL;
        }
        match self.segments.last_mut() {
            Some(FrameSegment::GL(seg)) => seg,
            _ => unreachable!(
                "CurrentKind::GL requires the last segment to be GL; \
                 the collector's own fields are the only way to break that"
            ),
        }
    }

    /// The open Canvas2D segment for `canvas_id`, opening one if the current
    /// segment is a different canvas or a GL segment. See [`Self::gl_segment`]
    /// for why the invariant is asserted here rather than re-derived.
    #[inline]
    fn canvas2d_segment(&mut self, canvas_id: u32) -> &mut Canvas2DSegment {
        if self.current != CurrentKind::Canvas2D(canvas_id) {
            self.segments.push(FrameSegment::Canvas2D(Canvas2DSegment {
                canvas_id,
                commands: take_canvas_command_vec(),
                dirty_rect: None,
                dirty_poisoned: false,
            }));
            self.current = CurrentKind::Canvas2D(canvas_id);
        }
        match self.segments.last_mut() {
            Some(FrameSegment::Canvas2D(seg)) => seg,
            _ => unreachable!(
                "CurrentKind::Canvas2D requires the last segment to be that \
                 canvas's; the collector's own fields are the only way to \
                 break that"
            ),
        }
    }

    fn publish_frame_peak(&mut self) {
        let peak = self.frame_peak_bytes.min(u32::MAX as usize) as u32;
        self.frame_peak_bytes = 0;

        if self.diagnostics_stats.is_none()
            && let Some(host_id) = self.diagnostics_host_id
        {
            self.diagnostics_stats = shared::stats::get_stats(host_id);
        }
        if let Some(stats) = &self.diagnostics_stats {
            stats.collector_pending_bytes.store(peak, Ordering::Relaxed);
        }
    }

    /// Rough upper bound on the bytes currently retained by pending
    /// commands.  Counted per-push using `std::mem::size_of` plus a
    /// conservative per-variant overhead: we overshoot slightly on
    /// small scalar commands and undercount when a variant owns a
    /// long-lived heap buffer (a future refinement could inspect the
    /// `Vec` capacities), so the value is a *heuristic* for flush
    /// decisions — never load-bearing for correctness.
    #[inline]
    /// `#[allow(dead_code)]`: only exercised by tests today, but
    /// exposed as public-in-crate so the debug overlay / future
    /// profiling hooks can read the live byte budget without
    /// poking `self.pending_bytes` directly.
    #[allow(dead_code)]
    pub(crate) fn approx_pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    /// `true` when the accumulated batch has grown past the soft
    /// budget — call sites that reach a frame boundary should treat
    /// this as a hint to flush a barrier instead of continuing to
    /// accumulate.  Purely advisory: skipping the flush can't break
    /// correctness, only degrade memory / latency.
    #[inline]
    pub(crate) fn should_auto_flush(&self) -> bool {
        self.pending_bytes >= AUTO_FLUSH_SOFT_BUDGET_BYTES
    }

    pub(crate) fn push_canvas2d(&mut self, canvas_id: u32, cmd: Canvas2DCmd) {
        // Walk the full deep size (enum base + heap payload) so the
        // budget reflects reality.  The previous implementation
        // only added `size_of::<Canvas2DCmd>()`, missing text
        // strings, line dash vectors, gradient stops, and
        // DrawImageBatch slot arrays — a single `fillText` with a
        // 1 KB user string reported ~150 B instead of 1.15 KB.
        //
        // Read before the command moves into the segment; counted after, so
        // `build_frame_packet_inner`'s "segments empty ⇒ pending_bytes == 0"
        // still holds at every point a caller could observe.
        let bytes = cmd.approx_deep_size_bytes();

        let seg = self.canvas2d_segment(canvas_id);
        seg.mark_dirty_for_cmd(&cmd);
        // Peephole: a run of adjacent `drawImage` calls is one
        // `DrawImageBatch`.  Content emits one `drawImage` per sprite, so
        // without this the render thread pays a full command dispatch
        // (make-current, split borrow, paint build) per sprite and the
        // `drawAtlas` run batching downstream never sees a run at all.
        // See `fold_draw_image_run` for why adjacency is the whole
        // precondition.
        // Decide on references first. Every Canvas2D command in the engine goes
        // through here, and handing a ~140-byte enum to the fold and taking it
        // back would put that cost on `fillRect` and every state setter too --
        // paid by the many to serve the few.
        let foldable = matches!(cmd, Canvas2DCmd::DrawImage { .. })
            && matches!(
                seg.commands.last(),
                Some(Canvas2DCmd::DrawImage { .. } | Canvas2DCmd::DrawImageBatch { .. })
            );
        if foldable {
            let tail = seg
                .commands
                .last_mut()
                .expect("checked non-empty on the line above");
            let rejected = shared::protocol::render_cmd::fold_draw_image_run(tail, cmd);
            debug_assert!(
                rejected.is_none() || matches!(rejected, Some(Canvas2DCmd::DrawImage { .. })),
                "the fold may only hand back what it was given"
            );
            if rejected.is_none() {
                self.pending_bytes = self.pending_bytes.saturating_add(bytes);
                self.record_pending_peak();
                return;
            }
            // Only reachable when the batch is already at the protocol cap.
            seg.commands
                .push(rejected.expect("checked Some on the line above"));
        } else {
            seg.commands.push(cmd);
        }

        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
        self.record_pending_peak();
    }

    pub(crate) fn push_gl(&mut self, cmd: GLCmd) {
        // Deep size walk - see the Canvas2D counterpart.  This is
        // especially critical for `BufferData(Vec<u8>)` /
        // `TexImage2D(Arc<Vec<u8>>)` / `ShaderSource(String)`, the
        // three GL variants most likely to single-handedly blow
        // the budget.
        let bytes = cmd.approx_deep_size_bytes();

        self.gl_segment().commands.push(cmd);

        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
        self.record_pending_peak();
    }

    /// Fast-path variant of [`Self::push_gl`] for scalar-only
    /// commands (viewport, bind*, uniform scalars, enable/disable,
    /// draw, scissor...).
    ///
    /// Scalar ops are the majority of a WebGL frame by count —
    /// Cocos/three.js emit hundreds per frame — and every byte of
    /// overhead multiplies.  Versus `push_gl` this drops the heavy
    /// per-command work while keeping the memory bound:
    ///
    /// 1. No `approx_deep_size_bytes()` match.  The enum base size
    ///    is a compile-time constant and the heap payload is known
    ///    to be zero, so we add `size_of::<GLCmd>()` directly.
    /// 2. `push_gl_fast` itself never flushes — it only maintains
    ///    `pending_bytes`.  The caller (`queue_gl_fire_and_forget`)
    ///    performs a cheap `should_auto_flush()` field comparison
    ///    right after this push and cuts a barrier when a
    ///    scalar/inline-uniform storm crosses the 4 MiB soft budget.
    ///    Untrusted code that synchronously enqueues tens of
    ///    thousands of these (each ~`size_of::<GLCmd>()`) therefore
    ///    cannot pin unbounded JS-side memory until frame end.
    ///
    /// Callers MUST uphold the precondition that the pushed
    /// variant carries no heap payload.  Passing a
    /// `BufferData { data: Some(_) }` through this path would
    /// under-count the batch and defeat the auto-flush guard.
    #[inline]
    pub(crate) fn push_gl_fast(&mut self, cmd: GLCmd) {
        const BASE_BYTES: usize = std::mem::size_of::<GLCmd>();
        self.gl_segment().commands.push(cmd);
        self.pending_bytes = self.pending_bytes.saturating_add(BASE_BYTES);
        self.record_pending_peak();
    }

    /// Bulk-append a decoded GL command batch from the stream submit path.
    ///
    /// Design §7 contract:
    /// - Empty `commands`: return immediately (no segment, no stats).
    /// - Non-empty + current segment is GL: `Vec::append` into it, which empties
    ///   the input.
    /// - Non-empty + current segment is not GL: move the input vec directly into a new
    ///   GL segment (same as `push_gl` creates a `GlSegment`).
    ///
    /// Two of those three paths finish holding an emptied loan, and returning it
    /// to the pool used to be an explicit call on each — which is exactly the
    /// obligation that turned out to be unguardable: deleting either call failed
    /// no test in the binary, because a lost loan is a deallocation and the
    /// allocation gates cannot see one. The parameter is a
    /// [`PooledVec`], so the return is drop glue on every path, including the
    /// early one, and there is nothing left to forget.
    /// - Update `pending_bytes` ONCE via `saturating_add(approx_bytes)`.
    /// - Update the logical-frame high-water mark exactly once.
    /// - Check the 4 MiB soft budget ONCE; returns `true` when the budget is exceeded
    ///   so the caller can invoke `maybe_auto_flush` (which re-borrows `OpState`).
    pub(crate) fn append_gl_batch(
        &mut self,
        mut commands: PooledVec<GLCmd>,
        approx_bytes: usize,
    ) -> bool {
        if commands.is_empty() {
            // Nothing to do — bail without touching stats. The loan returns
            // itself on the way out.
            return false;
        }

        if self.current == CurrentKind::GL {
            // Extend the current GL segment in place, leaving `commands` empty
            // to return its allocation when it goes out of scope.
            //
            // Not `Self::gl_segment`: that would take a fresh pooled vec on the
            // miss path and then copy into it, where the `else` below moves this
            // one in whole. The invariant is asserted the same way, for the same
            // reason — a batch dropped here is an entire decoded command stream
            // gone, with `approx_bytes` still counted against the budget.
            match self.segments.last_mut() {
                Some(FrameSegment::GL(seg)) => seg.commands.append(&mut commands),
                _ => unreachable!(
                    "CurrentKind::GL requires the last segment to be GL; \
                     the collector's own fields are the only way to break that"
                ),
            }
        } else {
            // Start a new GL segment by moving the vec directly in.
            self.segments.push(FrameSegment::GL(GlSegment { commands }));
            self.current = CurrentKind::GL;
        }

        // Update byte budget exactly once.
        self.pending_bytes = self.pending_bytes.saturating_add(approx_bytes);
        self.record_pending_peak();

        // Report whether auto-flush is needed; caller handles it.
        self.should_auto_flush()
    }

    /// Build a single FramePacket from all accumulated segments.
    /// Inserts `Materialize` ops at each Canvas2D→GL boundary.
    /// When `materialize_trailing` is true, also materializes any remaining
    /// pending 2D canvases at the end (needed for sync barriers).
    /// Resets the collector for the next frame.
    fn build_frame_packet_inner(
        &mut self,
        present: bool,
        materialize_trailing: bool,
    ) -> Option<shared::FramePacket> {
        if self.segments.is_empty() {
            // Defence in depth: if a prior `push_*` incremented
            // `pending_bytes` but a bug prevented the segment
            // from landing in `segments`, the early return here
            // would otherwise leave a stale counter that tricks
            // `should_auto_flush` into firing forever.  Practical
            // reachability is zero (every push creates a segment
            // before incrementing the counter) but the invariant
            // ("segments empty ⇒ pending_bytes == 0") is cheap to
            // enforce and makes the flush protocol easier to
            // audit.
            self.pending_bytes = 0;
            self.current = CurrentKind::None;
            return None;
        }

        self.current = CurrentKind::None;
        self.pending_bytes = 0;

        let mut builder = shared::FramePacketBuilder::new(0, 0.0).push(FrameOp::BeginFrame);

        // Track which canvases have unmaterialized 2D work as we scan.
        // Canvas2D segments add to the set; GL segments consume it.
        // `begin` hands the set over empty and keeps the capacity earlier
        // frames reached, so a scene wider than its inline array allocates on
        // its first frame rather than on all of them.
        let pending_2d = self.pending_2d.begin();

        // `drain` rather than `mem::take`: the segments belong to the packet, the
        // list holding them belongs to the next frame.  Taking hands both away and
        // leaves this vector at zero capacity, so the next frame's first push
        // allocates it again — once per frame, forever, on the thread running the
        // game.  Draining moves the segments out and keeps the allocation.
        for seg in self.segments.drain(..) {
            match seg {
                FrameSegment::Canvas2D(s) => {
                    pending_2d.insert(s.canvas_id);
                    builder = builder.push(FrameOp::CanvasBatch(CanvasBatchPayload {
                        canvas_id: s.canvas_id,
                        commands: s.commands,
                        present,
                        dirty_rect: s.dirty_rect,
                    }));
                }
                FrameSegment::GL(s) => {
                    // Insert Materialize for all canvases with pending 2D work.
                    for cid in pending_2d.iter() {
                        builder = builder.push(FrameOp::Materialize { canvas_id: cid });
                    }
                    pending_2d.clear();
                    builder = builder.push(FrameOp::GlBatch(GlBatchPayload {
                        commands: s.commands,
                    }));
                }
            }
        }

        // Barrier mode: materialize any trailing pending 2D canvases so
        // a subsequent sync readback (readPixels, getImageData) sees results.
        if materialize_trailing && !pending_2d.is_empty() {
            for cid in pending_2d.iter() {
                builder = builder.push(FrameOp::Materialize { canvas_id: cid });
            }
        }

        if present {
            builder = builder.push(FrameOp::Present);
        }

        Some(builder.finish())
    }

    /// Build a single FramePacket from all accumulated segments.
    /// Inserts `Materialize` ops at each Canvas2D→GL boundary.
    /// Resets the collector for the next frame.
    pub(crate) fn build_frame_packet(&mut self, present: bool) -> Option<shared::FramePacket> {
        let packet = self.build_frame_packet_inner(present, false);
        self.publish_frame_peak();
        self.close_boundary_frame(packet.as_ref());
        packet
    }

    /// Fold the finished frame into the window, and report the window when the
    /// rate gate allows.
    ///
    /// A barrier flush mid-frame does not end a frame, so this is called only
    /// from the presenting path: the crossings a barrier already counted stay
    /// counted, which is right -- they crossed.
    fn close_boundary_frame(&mut self, packet: Option<&shared::FramePacket>) {
        let commands: u64 = packet
            .map(|packet| {
                packet
                    .ops()
                    .iter()
                    .map(|op| match op {
                        FrameOp::CanvasBatch(batch) => batch.commands.len() as u64,
                        FrameOp::GlBatch(batch) => batch.commands.len() as u64,
                        _ => 0,
                    })
                    .sum()
            })
            .unwrap_or(0);

        let crossings = std::mem::take(&mut self.boundary.crossings);
        self.boundary.window_frames = self.boundary.window_frames.saturating_add(1);
        self.boundary.window_crossings = self
            .boundary
            .window_crossings
            .saturating_add(u64::from(crossings));
        self.boundary.window_commands = self.boundary.window_commands.saturating_add(commands);
        self.boundary.window_worst_frame = self.boundary.window_worst_frame.max(crossings);

        // The gate is checked before the window is read so a run that never
        // reaches the interval costs one atomic load per frame and nothing else.
        shared::info_rate_limited!(
            std::time::Duration::from_secs(5),
            "[boundary] frames={} crossings/frame={:.2} worst={} commands/frame={:.1} commands/crossing={:.1}",
            self.boundary.window_frames,
            self.boundary.window_crossings as f64 / self.boundary.window_frames.max(1) as f64,
            self.boundary.window_worst_frame,
            self.boundary.window_commands as f64 / self.boundary.window_frames.max(1) as f64,
            self.boundary.window_commands as f64 / self.boundary.window_crossings.max(1) as f64,
        );
        if self.boundary.window_frames >= BOUNDARY_WINDOW_FRAMES {
            self.boundary.window_frames = 0;
            self.boundary.window_crossings = 0;
            self.boundary.window_commands = 0;
            self.boundary.window_worst_frame = 0;
        }
    }

    /// Flush all pending segments as a non-presenting partial FramePacket.
    /// Used before sync operations (getImageData, readPixels, GL queries).
    /// Materializes ALL trailing pending 2D canvases so the sync op sees results.
    pub(crate) fn flush_as_barrier(&mut self) -> Option<shared::FramePacket> {
        self.build_frame_packet_inner(false, true)
    }

    // ── Canvas2D forwarding methods ────────────────────────────────
    // These mirror FrameCommandCollector's API so ops only need a type
    // change in their borrow call. Dedup is intentionally omitted in
    // this first pass; the render thread handles redundant state-sets
    // correctly. Dedup can be re-added as a follow-up optimization.

    #[inline]
    pub(crate) fn push(&mut self, canvas_id: u32, cmd: Canvas2DCmd) {
        self.push_canvas2d(canvas_id, cmd);
    }

    #[inline]
    pub(crate) fn set_fill_color(&mut self, canvas_id: u32, color: shared::protocol::color::Color) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetFillStyle { color });
    }

    #[inline]
    pub(crate) fn set_stroke_color(
        &mut self,
        canvas_id: u32,
        color: shared::protocol::color::Color,
    ) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetStrokeStyle { color });
    }

    #[inline]
    pub(crate) fn set_line_width(&mut self, canvas_id: u32, width: f32) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetLineWidth { width });
    }

    #[inline]
    pub(crate) fn set_global_alpha(&mut self, canvas_id: u32, alpha: f32) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetGlobalAlpha { alpha });
    }

    #[inline]
    pub(crate) fn set_composite_operation(&mut self, canvas_id: u32, op: u8) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetCompositeOperation { op });
    }

    #[inline]
    pub(crate) fn set_line_dash(&mut self, canvas_id: u32, segments: Vec<f32>) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetLineDash { segments });
    }

    #[inline]
    pub(crate) fn set_line_dash_offset(&mut self, canvas_id: u32, offset: f32) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetLineDashOffset { offset });
    }

    #[inline]
    pub(crate) fn set_shadow_blur(&mut self, canvas_id: u32, blur: f32) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetShadowBlur { blur });
    }

    #[inline]
    pub(crate) fn set_shadow_color(
        &mut self,
        canvas_id: u32,
        color: shared::protocol::color::Color,
    ) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetShadowColor { color });
    }

    #[inline]
    pub(crate) fn set_shadow_offset_x(&mut self, canvas_id: u32, offset: f32) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetShadowOffsetX { offset });
    }

    #[inline]
    pub(crate) fn set_shadow_offset_y(&mut self, canvas_id: u32, offset: f32) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetShadowOffsetY { offset });
    }

    #[inline]
    pub(crate) fn set_fill_style_gradient(
        &mut self,
        canvas_id: u32,
        gradient_type: shared::protocol::render_cmd::GradientType,
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
        stops: Vec<shared::protocol::render_cmd::GradientStop>,
    ) {
        self.push_canvas2d(
            canvas_id,
            Canvas2DCmd::SetFillStyleGradient {
                gradient_type,
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
                stops,
            },
        );
    }

    #[inline]
    pub(crate) fn set_stroke_style_gradient(
        &mut self,
        canvas_id: u32,
        gradient_type: shared::protocol::render_cmd::GradientType,
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
        stops: Vec<shared::protocol::render_cmd::GradientStop>,
    ) {
        self.push_canvas2d(
            canvas_id,
            Canvas2DCmd::SetStrokeStyleGradient {
                gradient_type,
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
                stops,
            },
        );
    }

    #[inline]
    pub(crate) fn set_fill_style_pattern(
        &mut self,
        canvas_id: u32,
        image_id: u32,
        repeat_x: bool,
        repeat_y: bool,
    ) {
        self.push_canvas2d(
            canvas_id,
            Canvas2DCmd::SetFillStylePattern {
                image_id,
                repeat_x,
                repeat_y,
            },
        );
    }

    #[inline]
    pub(crate) fn set_stroke_style_pattern(
        &mut self,
        canvas_id: u32,
        image_id: u32,
        repeat_x: bool,
        repeat_y: bool,
    ) {
        self.push_canvas2d(
            canvas_id,
            Canvas2DCmd::SetStrokeStylePattern {
                image_id,
                repeat_x,
                repeat_y,
            },
        );
    }

    #[inline]
    pub(crate) fn set_line_cap(&mut self, canvas_id: u32, cap: u8) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetLineCap { cap });
    }

    #[inline]
    pub(crate) fn set_line_join(&mut self, canvas_id: u32, join: u8) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetLineJoin { join });
    }

    #[inline]
    pub(crate) fn set_miter_limit(&mut self, canvas_id: u32, limit: f32) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetMiterLimit { limit });
    }

    #[inline]
    pub(crate) fn set_text_align(
        &mut self,
        canvas_id: u32,
        align: shared::protocol::render_cmd::TextAlign,
    ) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetTextAlign { align });
    }

    #[inline]
    pub(crate) fn set_text_baseline(
        &mut self,
        canvas_id: u32,
        baseline: shared::protocol::render_cmd::TextBaseline,
    ) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetTextBaseline { baseline });
    }

    #[inline]
    pub(crate) fn set_font(&mut self, canvas_id: u32, font: String) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::SetFont { font });
    }

    /// Reset dedup state (no-op without dedup) and push Restore.
    pub(crate) fn restore(&mut self, canvas_id: u32) {
        self.push_canvas2d(canvas_id, Canvas2DCmd::Restore);
    }

    /// Count GL commands across all GL segments (test-only helper).
    #[cfg(test)]
    pub(crate) fn gl_cmd_count_for_test(&self) -> usize {
        self.segments
            .iter()
            .filter_map(|seg| {
                if let FrameSegment::GL(s) = seg {
                    Some(s.commands.len())
                } else {
                    None
                }
            })
            .sum()
    }

    /// Count GL segments (test-only helper).
    #[cfg(test)]
    pub(crate) fn gl_segment_count_for_test(&self) -> usize {
        self.segments
            .iter()
            .filter(|seg| matches!(seg, FrameSegment::GL(_)))
            .count()
    }
}

/// Flush all pending unified segments to the render thread as a barrier.
/// Call before any sync operation that observes prior drawing results.
///
/// Returns `Ok(())` when there was nothing to flush **or** the barrier was
/// delivered. Returns `Err` only when delivery failed (channel timeout /
/// disconnect).
///
/// Delivery uses [`CommandSender::dispatch`] rather than the legacy
/// `send()`: a `FramePacket` classifies as `Draw`, so `dispatch` applies
/// the bounded-**blocking** policy and the barrier is guaranteed to reach
/// the render thread (or surface an error) instead of being **silently
/// dropped** when the channel is full. This is load-bearing for sync
/// readbacks (`getImageData` / `readPixels`): if the barrier were dropped,
/// the subsequent sync read would observe un-materialized 2D content or a
/// stale GL state. `flush_as_barrier` has already drained the collector, so
/// a silent drop would also *lose* those draw commands outright.
pub(crate) fn flush_unified_barrier(
    state: &mut deno_core::OpState,
) -> Result<(), shared::render_command_sender::SendError> {
    let packet = {
        if let Some(collector) = state.try_borrow_mut::<UnifiedFrameCollector>() {
            collector.flush_as_barrier()
        } else {
            None
        }
    };

    let Some(packet) = packet else {
        return Ok(());
    };

    // Diag: how many GL ops did the caller pile up before the
    // sync barrier?  A spike here correlates with the
    // [MigoPerf][SyncOp] wait time on the render-thread side.
    let gl_ops = packet
        .ops()
        .iter()
        .map(|op| match op {
            shared::protocol::FrameOp::GlBatch(p) => p.commands.len(),
            _ => 0,
        })
        .sum::<usize>();
    if gl_ops >= 64 {
        tracing::warn!("[MigoPerf][SyncFlush] sync barrier flushed {gl_ops} pending GL ops");
    }
    let ctx = state.borrow::<shared::op_state::CanvasOpState>();
    ctx.tx
        .dispatch(shared::protocol::render_cmd::RenderCommand::FramePacket(
            packet,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    fn clear_command() -> GLCmd {
        GLCmd::Clear {
            canvas_id: 1u32.into(),
            bit_field: 0x4000,
        }
    }

    #[test]
    fn pushes_do_not_publish_before_logical_frame_end() {
        let stats = Arc::new(shared::stats::DebugStats::default());
        let mut collector = UnifiedFrameCollector::with_diagnostics(stats.clone());

        collector.append_gl_batch(vec![clear_command()].into(), 4_096);

        assert_eq!(
            stats.collector_pending_bytes.load(Ordering::Relaxed),
            0,
            "per-op collection must not write the shared diagnostic field"
        );
    }

    #[test]
    fn barrier_preserves_the_logical_frame_peak() {
        let stats = Arc::new(shared::stats::DebugStats::default());
        let mut collector = UnifiedFrameCollector::with_diagnostics(stats.clone());
        collector.append_gl_batch(vec![clear_command()].into(), 4_096);

        assert!(collector.flush_as_barrier().is_some());
        assert_eq!(stats.collector_pending_bytes.load(Ordering::Relaxed), 0);

        collector.append_gl_batch(vec![clear_command()].into(), 1_024);
        assert!(collector.build_frame_packet(true).is_some());
        assert_eq!(stats.collector_pending_bytes.load(Ordering::Relaxed), 4_096);
    }

    #[test]
    fn frame_end_publishes_peak_then_resets_for_next_frame() {
        let stats = Arc::new(shared::stats::DebugStats::default());
        let mut collector = UnifiedFrameCollector::with_diagnostics(stats.clone());

        collector.append_gl_batch(vec![clear_command()].into(), 8_192);
        assert!(collector.build_frame_packet(true).is_some());
        assert_eq!(stats.collector_pending_bytes.load(Ordering::Relaxed), 8_192);

        collector.append_gl_batch(vec![clear_command()].into(), 512);
        assert!(collector.build_frame_packet(true).is_some());
        assert_eq!(stats.collector_pending_bytes.load(Ordering::Relaxed), 512);

        assert!(collector.build_frame_packet(true).is_none());
        assert_eq!(stats.collector_pending_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn frame_peak_saturates_at_u32_max() {
        let stats = Arc::new(shared::stats::DebugStats::default());
        let mut collector = UnifiedFrameCollector::with_diagnostics(stats.clone());

        collector.append_gl_batch(vec![clear_command()].into(), usize::MAX);
        assert!(collector.build_frame_packet(true).is_some());
        assert_eq!(
            stats.collector_pending_bytes.load(Ordering::Relaxed),
            u32::MAX
        );
    }

    #[test]
    fn two_collectors_publish_to_distinct_session_stats() {
        let stats_a = Arc::new(shared::stats::DebugStats::default());
        let stats_b = Arc::new(shared::stats::DebugStats::default());
        let mut a = UnifiedFrameCollector::with_diagnostics(stats_a.clone());
        let mut b = UnifiedFrameCollector::with_diagnostics(stats_b.clone());

        a.append_gl_batch(vec![clear_command()].into(), 1_111);
        b.append_gl_batch(vec![clear_command()].into(), 2_222);
        assert!(a.build_frame_packet(true).is_some());
        assert!(b.build_frame_packet(true).is_some());

        assert_eq!(
            stats_a.collector_pending_bytes.load(Ordering::Relaxed),
            1_111
        );
        assert_eq!(
            stats_b.collector_pending_bytes.load(Ordering::Relaxed),
            2_222
        );
    }

    #[test]
    fn host_stats_lookup_retries_after_startup_race() {
        static NEXT_HOST_ID: std::sync::atomic::AtomicI32 =
            std::sync::atomic::AtomicI32::new(-1_000_000);
        let host_id = NEXT_HOST_ID.fetch_sub(1, Ordering::Relaxed);
        shared::stats::unregister_stats(host_id);
        let mut collector = UnifiedFrameCollector::with_host_id(host_id);

        collector.append_gl_batch(vec![clear_command()].into(), 111);
        assert!(collector.build_frame_packet(true).is_some());

        let stats = shared::stats::stats_for(host_id);
        collector.append_gl_batch(vec![clear_command()].into(), 333);
        assert!(collector.build_frame_packet(true).is_some());
        assert_eq!(stats.collector_pending_bytes.load(Ordering::Relaxed), 333);
        shared::stats::unregister_stats(host_id);
    }

    #[test]
    fn production_wiring_has_no_per_op_global_publication() {
        let collector = include_str!("frame_collector.rs");
        let extension = include_str!("mod.rs");
        let old_setter = ["set_collector_", "pending_bytes"].concat();
        let old_static = ["COLLECTOR_", "PENDING_BYTES"].concat();

        assert!(!collector.contains(&old_setter));
        assert!(!collector.contains(&old_static));
        assert!(extension.contains("UnifiedFrameCollector::with_host_id(host_id)"));
    }

    #[test]
    fn empty_collector_returns_none() {
        let mut c = UnifiedFrameCollector::new();
        assert!(c.build_frame_packet(true).is_none());
    }

    #[test]
    fn canvas2d_only_frame_produces_single_canvas_batch() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        assert!(matches!(ops[0], FrameOp::BeginFrame));
        assert!(
            matches!(&ops[1], FrameOp::CanvasBatch(p) if p.canvas_id == 1 && p.commands.len() == 2)
        );
        assert!(matches!(ops.last().unwrap(), FrameOp::Present));
    }

    #[test]
    fn gl_only_frame_produces_single_gl_batch_no_present() {
        let mut c = UnifiedFrameCollector::new();
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });

        let packet = c.build_frame_packet(false).unwrap();
        let ops = packet.ops();
        assert!(matches!(ops[0], FrameOp::BeginFrame));
        assert!(matches!(&ops[1], FrameOp::GlBatch(p) if p.commands.len() == 1));
        assert!(!matches!(ops.last().unwrap(), FrameOp::Present));
    }

    #[test]
    fn canvas2d_then_gl_inserts_materialize_barrier() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        // [BeginFrame, CanvasBatch(1), Materialize(1), GlBatch, Present]
        assert_eq!(ops.len(), 5);
        assert!(matches!(ops[0], FrameOp::BeginFrame));
        assert!(matches!(&ops[1], FrameOp::CanvasBatch(p) if p.canvas_id == 1));
        assert!(matches!(ops[2], FrameOp::Materialize { canvas_id: 1 }));
        assert!(matches!(&ops[3], FrameOp::GlBatch(_)));
        assert!(matches!(ops[4], FrameOp::Present));
    }

    /// The pending-canvas set spans frames so its allocation does; its
    /// *contents* must not. A frame handed a set still holding the previous
    /// frame's canvases would emit a `Materialize` for each of them — an
    /// `eglMakeCurrent` and a Skia flush per stale canvas, on canvases with no
    /// work to flush and possibly none left to flush it to.
    ///
    /// **The first frame must end on Canvas2D work, and that is the whole
    /// fixture.** A GL segment consumes the pending run at the boundary, so a
    /// frame that ends with one leaves the set empty and inherits nothing — this
    /// test's first version ended both frames that way and a set that was never
    /// emptied passed it. What survives a frame boundary is a *trailing* 2D run:
    /// nothing follows it to consume it, and a non-barrier packet does not
    /// materialise it.
    #[test]
    fn a_frames_materialize_ops_name_only_that_frames_canvases() {
        fn materialized(packet: &shared::FramePacket) -> Vec<u32> {
            packet
                .ops()
                .iter()
                .filter_map(|op| match op {
                    FrameOp::Materialize { canvas_id } => Some(*canvas_id),
                    _ => None,
                })
                .collect()
        }

        let mut c = UnifiedFrameCollector::new();

        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_canvas2d(2, Canvas2DCmd::Save);
        let trailing = c.build_frame_packet(true).unwrap();
        assert_eq!(
            materialized(&trailing),
            Vec::<u32>::new(),
            "a trailing 2D run is materialized lazily, so this frame leaves 1 and 2 pending"
        );

        c.push_canvas2d(4, Canvas2DCmd::Save);
        c.push_gl(GLCmd::Clear {
            canvas_id: 99,
            bit_field: 0x4000,
        });
        let next = c.build_frame_packet(true).unwrap();
        assert_eq!(
            materialized(&next),
            vec![4],
            "the previous frame's canvases were materialized again"
        );
    }

    #[test]
    fn gl_then_canvas2d_does_not_insert_materialize() {
        let mut c = UnifiedFrameCollector::new();
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        // [BeginFrame, GlBatch, CanvasBatch(1), Present] — no Materialize
        assert_eq!(ops.len(), 4);
        assert!(matches!(ops[0], FrameOp::BeginFrame));
        assert!(matches!(&ops[1], FrameOp::GlBatch(_)));
        assert!(matches!(&ops[2], FrameOp::CanvasBatch(p) if p.canvas_id == 1));
        assert!(matches!(ops[3], FrameOp::Present));
    }

    #[test]
    fn interleaved_2d_gl_2d_produces_correct_sequence() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 20.0,
                h: 20.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        // [BeginFrame, CanvasBatch(1), Materialize(1), GlBatch, CanvasBatch(1), Present]
        assert_eq!(ops.len(), 6);
        assert!(matches!(&ops[1], FrameOp::CanvasBatch(p) if p.commands.len() == 1));
        assert!(matches!(ops[2], FrameOp::Materialize { canvas_id: 1 }));
        assert!(matches!(&ops[3], FrameOp::GlBatch(p) if p.commands.len() == 1));
        assert!(matches!(&ops[4], FrameOp::CanvasBatch(p) if p.commands.len() == 1));
    }

    #[test]
    fn multiple_canvases_all_get_materialized_before_gl() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_canvas2d(2, Canvas2DCmd::Save);
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        let mat_count = ops
            .iter()
            .filter(|op| matches!(op, FrameOp::Materialize { .. }))
            .count();
        assert_eq!(mat_count, 2);
    }

    #[test]
    fn build_resets_collector_for_next_frame() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Save);
        let _ = c.build_frame_packet(true);
        assert!(c.build_frame_packet(true).is_none());
    }

    #[test]
    fn interleaved_single_command_segments_do_not_reserve_256_slots_each() {
        let mut c = UnifiedFrameCollector::new();
        for _ in 0..50 {
            c.push_canvas2d(1, Canvas2DCmd::Save);
            c.push_gl(GLCmd::Clear {
                canvas_id: 1u32.into(),
                bit_field: 0x4000,
            });
        }

        let reserved_bytes = c
            .segments
            .iter()
            .map(|segment| match segment {
                FrameSegment::Canvas2D(segment) => {
                    segment.commands.capacity() * std::mem::size_of::<Canvas2DCmd>()
                }
                FrameSegment::GL(segment) => {
                    segment.commands.capacity() * std::mem::size_of::<GLCmd>()
                }
            })
            .sum::<usize>();
        let old_256_slot_bytes =
            50 * 256 * (std::mem::size_of::<Canvas2DCmd>() + std::mem::size_of::<GLCmd>());

        // The ceiling is derived from the pool's own rule rather than picked, and
        // it has to be, because the capacities here are not this collector's
        // decision: each segment takes a vector from a process-wide pool whose
        // contents depend on what every other test in this binary recycled. An
        // earlier fixed threshold read as a statement about the collector while
        // actually measuring the pool, and it began failing when the pool started
        // retaining by bytes instead of by length -- the aggregate bound never
        // moved, only the distribution did.
        //
        // What the pool can hand out in total is its byte budget; anything beyond
        // that is freshly allocated at the minimum capacity. That sum is a true
        // upper bound whatever the pool happens to hold.
        use shared::command_vec_pool::{
            CANVAS_COMMAND_VEC_INITIAL_CAPACITY, COMMAND_VEC_POOL_BUDGET_COMMANDS_PER_SLOT,
            COMMAND_VEC_POOL_SLOTS, GL_COMMAND_VEC_INITIAL_CAPACITY,
        };
        let pool_budget = |command_bytes: usize| {
            COMMAND_VEC_POOL_SLOTS * COMMAND_VEC_POOL_BUDGET_COMMANDS_PER_SLOT * command_bytes
        };
        let ceiling = pool_budget(std::mem::size_of::<GLCmd>())
            + pool_budget(std::mem::size_of::<Canvas2DCmd>())
            + 50 * GL_COMMAND_VEC_INITIAL_CAPACITY * std::mem::size_of::<GLCmd>()
            + 50 * CANVAS_COMMAND_VEC_INITIAL_CAPACITY * std::mem::size_of::<Canvas2DCmd>();

        assert!(
            reserved_bytes <= ceiling,
            "100 single-command interleaved segments reserved {reserved_bytes} bytes, \
             above the {ceiling} the pool's budget plus fresh minimums allows; the old \
             256-slot policy would have reserved {old_256_slot_bytes}"
        );
    }

    #[test]
    fn approx_bytes_grows_monotonically_until_flush() {
        let mut c = UnifiedFrameCollector::new();
        assert_eq!(c.approx_pending_bytes(), 0);
        c.push_canvas2d(1, Canvas2DCmd::Save);
        let after_one = c.approx_pending_bytes();
        assert!(after_one > 0, "after one push should be non-zero");
        c.push_gl(GLCmd::Clear {
            canvas_id: 1u32.into(),
            bit_field: 0x4000,
        });
        assert!(c.approx_pending_bytes() > after_one, "second push must add");
        // A barrier flush drains the counter back to zero.
        let _ = c.flush_as_barrier();
        assert_eq!(c.approx_pending_bytes(), 0, "flush must reset bytes");
    }

    // ---- Deep-size budgeting regression (P0-2) ----------------------
    //
    // Old impl added `size_of::<Cmd>()` only, so a single
    // `bufferData(8MB)` pushed ~200 B into pending_bytes and
    // `should_auto_flush()` never tripped.  These tests pin the
    // correct behaviour: payload heap bytes must contribute to the
    // budget within a factor of 2 of the true payload size.

    #[test]
    fn single_large_buffer_data_trips_auto_flush() {
        let mut c = UnifiedFrameCollector::new();
        let data = vec![0u8; 8 * 1024 * 1024]; // 8 MB mesh
        c.push_gl(GLCmd::BufferData {
            canvas_id: 1u32.into(),
            target: 0x8892,
            size: data.len() as i32,
            data: Some(data),
            usage: 0x88E4,
        });
        assert!(
            c.approx_pending_bytes() >= 8 * 1024 * 1024,
            "8MB bufferData under-accounted: {}",
            c.approx_pending_bytes(),
        );
        assert!(
            c.should_auto_flush(),
            "8MB single push must trip the 4MB soft budget"
        );
    }

    #[test]
    fn many_small_shader_sources_sum_near_their_total_payload() {
        // Regression for "count pages, not ops": 1000 shader
        // sources of ~4 KB each — assert the budget reflects the
        // aggregate text bytes, not just the enum wrappers.
        let mut c = UnifiedFrameCollector::new();
        for i in 0..1000u32 {
            let mut source = String::with_capacity(5000);
            source.push_str(&format!("// sh{}\n", i));
            source.push_str(&"x".repeat(4000));
            c.push_gl(GLCmd::ShaderSource {
                shader_id: (i + 1).into(),
                source,
                resp: None,
            });
        }
        let bytes = c.approx_pending_bytes();
        let lower = 1000 * 4000; // at least the text body per source
        assert!(
            bytes >= lower,
            "1000 sources should account for >={lower} bytes; got {bytes}",
        );
        assert!(
            c.should_auto_flush(),
            "{bytes} bytes must trip 4MB auto-flush",
        );
    }

    #[test]
    fn draw_image_batch_capacity_changes_reflect_in_budget() {
        let mut c1 = UnifiedFrameCollector::new();
        c1.push_canvas2d(
            1,
            Canvas2DCmd::DrawImageBatch {
                draws: vec![entry(); 10],
            },
        );
        let small = c1.approx_pending_bytes();

        let mut c2 = UnifiedFrameCollector::new();
        c2.push_canvas2d(
            1,
            Canvas2DCmd::DrawImageBatch {
                draws: vec![entry(); 1000],
            },
        );
        let large = c2.approx_pending_bytes();

        assert!(
            large
                > small + 900 * std::mem::size_of::<shared::protocol::render_cmd::DrawImageEntry>(),
            "1000-entry batch ({}b) should far exceed 10-entry batch ({}b)",
            large,
            small,
        );
    }

    fn entry() -> shared::protocol::render_cmd::DrawImageEntry {
        shared::protocol::render_cmd::DrawImageEntry {
            image_id: 1,
            sx: 0.0,
            sy: 0.0,
            sw: 1.0,
            sh: 1.0,
            dx: 0.0,
            dy: 0.0,
            dw: 1.0,
            dh: 1.0,
        }
    }

    // ── Task 3 RED: append_gl_batch ──────────────────────────────────────────

    #[test]
    fn append_gl_batch_into_empty_collector_creates_one_gl_segment() {
        let mut c = UnifiedFrameCollector::new();
        let cmds = vec![
            GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0x4000,
            },
            GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0x4100,
            },
        ];
        c.append_gl_batch(cmds.into(), 128);

        let packet = c.build_frame_packet(false).unwrap();
        let ops = packet.ops();
        // [BeginFrame, GlBatch(2)]
        assert_eq!(ops.len(), 2);
        assert!(matches!(&ops[1], FrameOp::GlBatch(p) if p.commands.len() == 2));
    }

    #[test]
    fn append_gl_batch_into_existing_gl_segment_merges_and_preserves_order() {
        let mut c = UnifiedFrameCollector::new();
        // Start with one GL command via push_gl
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });

        let cmds = vec![
            GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0x4100,
            },
            GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0x4200,
            },
        ];
        c.append_gl_batch(cmds.into(), 64);

        let packet = c.build_frame_packet(false).unwrap();
        let ops = packet.ops();
        // Must be a single GlBatch with all 3 commands in order
        assert_eq!(ops.len(), 2, "must be single GL segment");
        if let FrameOp::GlBatch(p) = &ops[1] {
            assert_eq!(p.commands.len(), 3);
            // Check order: first 0x4000, then 0x4100, then 0x4200
            assert!(matches!(
                &p.commands[0],
                GLCmd::Clear {
                    bit_field: 0x4000,
                    ..
                }
            ));
            assert!(matches!(
                &p.commands[1],
                GLCmd::Clear {
                    bit_field: 0x4100,
                    ..
                }
            ));
            assert!(matches!(
                &p.commands[2],
                GLCmd::Clear {
                    bit_field: 0x4200,
                    ..
                }
            ));
        } else {
            panic!("expected GlBatch");
        }
    }

    #[test]
    fn append_gl_batch_pending_bytes_increases_once_by_approx_bytes() {
        let mut c = UnifiedFrameCollector::new();
        assert_eq!(c.approx_pending_bytes(), 0);
        c.append_gl_batch(
            vec![GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0,
            }]
            .into(),
            777,
        );
        assert_eq!(c.approx_pending_bytes(), 777);
        // A second batch adds exactly the specified approx_bytes
        c.append_gl_batch(
            vec![GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0,
            }]
            .into(),
            333,
        );
        assert_eq!(c.approx_pending_bytes(), 1110);
    }

    #[test]
    fn append_gl_batch_crossing_soft_budget_makes_should_auto_flush_true() {
        let mut c = UnifiedFrameCollector::new();
        assert!(!c.should_auto_flush());
        // Append with approx_bytes that crosses the 4MiB budget
        c.append_gl_batch(
            vec![GLCmd::Clear {
                canvas_id: 1,
                bit_field: 0,
            }]
            .into(),
            AUTO_FLUSH_SOFT_BUDGET_BYTES + 1,
        );
        assert!(c.should_auto_flush());
    }

    #[test]
    fn append_gl_batch_empty_creates_no_segment_and_does_not_publish_bytes() {
        let mut c = UnifiedFrameCollector::new();
        c.append_gl_batch(Vec::new().into(), 999);
        // No segment created
        assert!(c.build_frame_packet(false).is_none());
        // pending_bytes stays at 0
        assert_eq!(c.approx_pending_bytes(), 0);
    }

    #[test]
    fn empty_flush_does_not_leak_pending_bytes() {
        // Regression: `build_frame_packet_inner` early-returned
        // when segments was empty WITHOUT resetting the bytes
        // counter.  A bug that bumps `pending_bytes` without
        // landing a segment would then make `should_auto_flush`
        // stick on "true" forever.  The unconditional reset on
        // the empty path pins the invariant:
        //   segments.is_empty() ⇒ pending_bytes == 0.
        let mut c = UnifiedFrameCollector::new();
        c.pending_bytes = 7777;
        assert!(c.build_frame_packet(true).is_none());
        assert_eq!(c.approx_pending_bytes(), 0);
        assert!(!c.should_auto_flush());
    }

    #[test]
    fn flush_as_barrier_then_build_frame_packet_keeps_counter_zero() {
        // Successive flushes must repeatedly land at zero.  The
        // old path would occasionally double-reset via two
        // codepaths that both wrote to `pending_bytes`; this
        // test pins the idempotence.
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_gl(GLCmd::Clear {
            canvas_id: 1u32.into(),
            bit_field: 0x4000,
        });
        let _ = c.flush_as_barrier();
        assert_eq!(c.approx_pending_bytes(), 0);

        c.push_canvas2d(1, Canvas2DCmd::Save);
        let _ = c.build_frame_packet(true);
        assert_eq!(c.approx_pending_bytes(), 0);

        assert!(c.build_frame_packet(true).is_none());
        assert_eq!(c.approx_pending_bytes(), 0);
    }

    #[test]
    fn should_auto_flush_trips_only_past_budget() {
        let mut c = UnifiedFrameCollector::new();
        assert!(!c.should_auto_flush());
        // Pushing a handful of small commands must stay below budget.
        for _ in 0..8 {
            c.push_canvas2d(1, Canvas2DCmd::Save);
        }
        assert!(
            !c.should_auto_flush(),
            "small frames must not trip the auto-flush signal"
        );
        // Manually push the counter past the threshold to verify
        // the predicate fires without allocating megabytes in the
        // test.
        c.pending_bytes = AUTO_FLUSH_SOFT_BUDGET_BYTES + 1;
        assert!(c.should_auto_flush());
    }

    #[test]
    fn flush_as_barrier_produces_non_presenting_packet() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });

        let packet = c.flush_as_barrier().unwrap();
        let ops = packet.ops();
        // Should NOT have Present at the end
        assert!(!matches!(ops.last().unwrap(), FrameOp::Present));
        // But should still have Materialize
        assert!(
            ops.iter()
                .any(|op| matches!(op, FrameOp::Materialize { .. }))
        );
    }

    #[test]
    fn second_gl_segment_after_more_2d_gets_new_materialize() {
        let mut c = UnifiedFrameCollector::new();
        // 2D → GL → 2D → GL
        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });
        c.push_canvas2d(1, Canvas2DCmd::Restore);
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        // [BeginFrame, CB(1), Mat(1), GB, CB(1), Mat(1), GB, Present]
        let mat_count = ops
            .iter()
            .filter(|op| matches!(op, FrameOp::Materialize { .. }))
            .count();
        assert_eq!(
            mat_count, 2,
            "each 2D→GL transition needs its own Materialize"
        );
    }

    #[test]
    fn consecutive_canvas2d_same_canvas_stay_in_one_segment() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_canvas2d(1, Canvas2DCmd::Restore);
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        // [BeginFrame, CanvasBatch(1, 3 cmds), Present]
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[1], FrameOp::CanvasBatch(p) if p.commands.len() == 3));
    }

    #[test]
    fn consecutive_gl_ops_stay_in_one_segment() {
        let mut c = UnifiedFrameCollector::new();
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        });
        c.push_gl(GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4100,
        });

        let packet = c.build_frame_packet(false).unwrap();
        let ops = packet.ops();
        // [BeginFrame, GlBatch(2 cmds)]
        assert_eq!(ops.len(), 2);
        assert!(matches!(&ops[1], FrameOp::GlBatch(p) if p.commands.len() == 2));
    }

    // ── Blocker 1: barrier must materialize trailing pending 2D ──

    #[test]
    fn barrier_materializes_trailing_pending_2d_for_sync_readback() {
        // Scenario: only 2D work pending, then sync GL readback (readPixels).
        // The barrier packet must include Materialize so the sync read sees 2D results.
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );

        let packet = c.flush_as_barrier().unwrap();
        let ops = packet.ops();
        // Must have Materialize(1) AFTER the CanvasBatch
        let has_materialize = ops
            .iter()
            .any(|op| matches!(op, FrameOp::Materialize { canvas_id: 1 }));
        assert!(
            has_materialize,
            "barrier must materialize trailing pending 2D canvases; got: {ops:?}"
        );
    }

    #[test]
    fn barrier_materializes_multiple_trailing_2d_canvases() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_canvas2d(2, Canvas2DCmd::Save);

        let packet = c.flush_as_barrier().unwrap();
        let ops = packet.ops();
        let mat_count = ops
            .iter()
            .filter(|op| matches!(op, FrameOp::Materialize { .. }))
            .count();
        assert_eq!(
            mat_count, 2,
            "barrier must materialize all pending 2D canvases"
        );
    }

    // ── Blocker 2: dirty_rect must be tracked per segment ──

    #[test]
    fn canvas2d_segment_tracks_dirty_rect_for_fill_rects() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 10.0,
                y: 20.0,
                w: 100.0,
                h: 50.0,
            },
        );
        c.push_canvas2d(
            1,
            Canvas2DCmd::ClearRect {
                x: 200.0,
                y: 30.0,
                w: 50.0,
                h: 60.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        let dirty = match &ops[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        // Bounding box of (10,20,100,50) union (200,30,50,60) = (10,20,240,70)
        let d = dirty.expect("dirty_rect must be Some for safe draw commands");
        assert!((d.x - 10.0).abs() < 0.001);
        assert!((d.y - 20.0).abs() < 0.001);
        assert!((d.width - 240.0).abs() < 0.001);
        assert!((d.height - 70.0).abs() < 0.001);
    }

    #[test]
    fn canvas2d_segment_dirty_rect_none_for_state_only_commands() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Save);
        c.push_canvas2d(1, Canvas2DCmd::Restore);

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        let dirty = match &ops[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "no draws = no dirty_rect");
    }

    fn sprite(dx: f32) -> Canvas2DCmd {
        Canvas2DCmd::DrawImage {
            image_id: 1,
            sx: 0.0,
            sy: 0.0,
            sw: 16.0,
            sh: 16.0,
            dx,
            dy: 0.0,
            dw: 16.0,
            dh: 16.0,
        }
    }

    /// Content emits one `drawImage` per sprite. Without folding, a
    /// 3-sprite frame costs three full render-thread command dispatches
    /// and the `drawAtlas` batching downstream never sees a run.
    #[test]
    fn a_run_of_draw_images_reaches_the_render_thread_as_one_batch() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, sprite(0.0));
        c.push_canvas2d(1, sprite(16.0));
        c.push_canvas2d(1, sprite(32.0));

        let packet = c.build_frame_packet(true).unwrap();
        let FrameOp::CanvasBatch(p) = &packet.ops()[1] else {
            panic!("expected CanvasBatch");
        };
        let cmds = &p.commands;
        assert_eq!(cmds.len(), 1, "three adjacent draws must fold into one");
        let Canvas2DCmd::DrawImageBatch { draws } = &cmds[0] else {
            panic!("expected DrawImageBatch, got {:?}", cmds[0]);
        };
        assert_eq!(
            draws.iter().map(|d| d.dx).collect::<Vec<_>>(),
            vec![0.0, 16.0, 32.0]
        );
    }

    /// The whole soundness argument is adjacency: a state command between
    /// two draws means they no longer share a paint, so the run must break.
    #[test]
    fn a_state_change_between_draws_breaks_the_run() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, sprite(0.0));
        c.push_canvas2d(1, Canvas2DCmd::SetGlobalAlpha { alpha: 0.5 });
        c.push_canvas2d(1, sprite(16.0));

        let packet = c.build_frame_packet(true).unwrap();
        let FrameOp::CanvasBatch(p) = &packet.ops()[1] else {
            panic!("expected CanvasBatch");
        };
        let cmds = &p.commands;
        assert_eq!(cmds.len(), 3, "the alpha change must stay between them");
        assert!(matches!(cmds[0], Canvas2DCmd::DrawImage { .. }));
        assert!(matches!(cmds[1], Canvas2DCmd::SetGlobalAlpha { .. }));
        assert!(matches!(cmds[2], Canvas2DCmd::DrawImage { .. }));
    }

    /// Two canvases interleaving draws must not fold across each other --
    /// each canvas has its own state and its own surface.
    #[test]
    fn draws_on_different_canvases_never_fold_together() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, sprite(0.0));
        c.push_canvas2d(2, sprite(16.0));
        c.push_canvas2d(1, sprite(32.0));

        let packet = c.build_frame_packet(true).unwrap();
        let mut per_canvas: Vec<usize> = Vec::new();
        for op in packet.ops() {
            if let FrameOp::CanvasBatch(p) = op {
                per_canvas.push(p.commands.len());
            }
        }
        assert_eq!(
            per_canvas.iter().sum::<usize>(),
            3,
            "canvas 1's two draws are not adjacent -- canvas 2 sits between them"
        );
    }

    /// A GL batch between two draws splits the canvas segment, and the fold
    /// must not reach across that split -- the draws would then be submitted
    /// before GL work the content ordered ahead of the second one.
    ///
    /// The adjacency argument is about the *command stream*, and a segment
    /// boundary is a discontinuity in it that the per-canvas check alone does
    /// not see. Today the property holds structurally -- the fold only ever
    /// looks at `seg.commands.last_mut()`, so it cannot reach past the segment
    /// it is in -- which means this characterises the boundary rather than
    /// guarding a decision. It is here so that a future fold that indexes
    /// canvases instead of segments fails loudly.
    #[test]
    fn a_gl_batch_between_draws_breaks_the_run() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, sprite(0.0));
        c.push_gl(GLCmd::Clear {
            canvas_id: shared::protocol::render_cmd::CanvasId::from(2u32),
            bit_field: 0x4000,
        });
        c.push_canvas2d(1, sprite(16.0));

        let packet = c.build_frame_packet(true).unwrap();
        let mut draw_entries = 0usize;
        let mut canvas_batches = 0usize;
        for op in packet.ops() {
            if let FrameOp::CanvasBatch(p) = op {
                canvas_batches += 1;
                for cmd in p.commands.iter() {
                    match cmd {
                        Canvas2DCmd::DrawImage { .. } => draw_entries += 1,
                        Canvas2DCmd::DrawImageBatch { draws } => draw_entries += draws.len(),
                        _ => {}
                    }
                }
            }
        }
        assert_eq!(draw_entries, 2, "both draws must survive");
        assert!(
            canvas_batches >= 2,
            "the GL batch must still split the 2D work into two segments, \
             got {canvas_batches}"
        );
    }

    /// Folding must not stop the collector noticing memory growth, or a
    /// sprite storm could pin the JS heap past the auto-flush budget.
    #[test]
    fn folded_draws_still_count_against_the_flush_budget() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, sprite(0.0));
        let after_one = c.pending_bytes;
        c.push_canvas2d(1, sprite(16.0));
        assert!(
            c.pending_bytes > after_one,
            "an absorbed draw still consumes memory"
        );
    }

    #[test]
    fn canvas2d_draw_image_tracks_dirty_rect() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::DrawImage {
                image_id: 1,
                sx: 0.0,
                sy: 0.0,
                sw: 32.0,
                sh: 32.0,
                dx: 50.0,
                dy: 60.0,
                dw: 32.0,
                dh: 32.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let ops = packet.ops();
        let dirty = match &ops[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        let d = dirty.expect("DrawImage must mark dirty");
        assert!((d.x - 50.0).abs() < 0.001);
        assert!((d.y - 60.0).abs() < 0.001);
    }

    // ── Scissor hint poisoning tests ──

    #[test]
    fn fill_text_poisons_dirty_rect_to_none() {
        let mut c = UnifiedFrameCollector::new();
        // FillRect first — establishes a valid dirty_rect
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );
        // FillText — bounds unknown, must poison
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillText {
                text: "hi".into(),
                x: 0.0,
                y: 0.0,
                max_width: f32::INFINITY,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "FillText must poison dirty_rect to None");
    }

    #[test]
    fn path_fill_poisons_dirty_rect_to_none() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );
        c.push_canvas2d(1, Canvas2DCmd::Fill);

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "path Fill must poison dirty_rect to None");
    }

    #[test]
    fn clip_poisons_dirty_rect_to_none() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );
        c.push_canvas2d(1, Canvas2DCmd::Clip);

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "Clip must poison dirty_rect to None");
    }

    #[test]
    fn rect_after_poison_stays_none() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Fill); // poison
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        ); // should NOT recover

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "once poisoned, dirty_rect stays None");
    }

    #[test]
    fn stroke_rect_poisons_dirty_rect() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::StrokeRect {
                x: 10.0,
                y: 20.0,
                w: 100.0,
                h: 50.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(
            dirty.is_none(),
            "StrokeRect must poison (lineWidth unknown JS-side)"
        );
    }

    #[test]
    fn set_transform_poisons_dirty_rect() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(
            1,
            Canvas2DCmd::SetTransform {
                a: 2.0,
                b: 0.0,
                c: 0.0,
                d: 2.0,
                e: 0.0,
                f: 0.0,
            },
        );
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "SetTransform must poison scissor hint");
    }

    #[test]
    fn translate_poisons_dirty_rect() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::Translate { x: 10.0, y: 10.0 });
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "Translate must poison scissor hint");
    }

    #[test]
    fn set_shadow_blur_poisons_dirty_rect() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::SetShadowBlur { blur: 5.0 });
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "SetShadowBlur must poison scissor hint");
    }

    #[test]
    fn set_line_width_poisons_dirty_rect() {
        let mut c = UnifiedFrameCollector::new();
        c.push_canvas2d(1, Canvas2DCmd::SetLineWidth { width: 4.0 });
        c.push_canvas2d(
            1,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        );

        let packet = c.build_frame_packet(true).unwrap();
        let dirty = match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            _ => panic!("expected CanvasBatch"),
        };
        assert!(dirty.is_none(), "SetLineWidth must poison scissor hint");
    }

    /// **Every pushed command must reach a segment — the property the four
    /// `if let Some(FrameSegment::…)` sites could silently fail.**
    ///
    /// Those sites established "the last segment is of this kind" and then
    /// re-derived it, with nothing on the `else`: a command whose segment lookup
    /// missed was dropped, after `pending_bytes` had already counted it. Now the
    /// lookup lives in `gl_segment` / `canvas2d_segment` and asserts instead.
    ///
    /// The assertion cannot be triggered from outside the type — the fields it
    /// depends on are private, which is exactly why it is an invariant and not a
    /// runtime condition. What a test *can* pin is the observable consequence:
    /// across every segment-transition shape, the number of commands that come
    /// back out equals the number pushed in. A regression that reintroduced a
    /// silent miss would show up here as a short count.
    #[test]
    fn every_pushed_command_lands_in_a_segment() {
        // One case per transition the segment tracker can make: first push,
        // same-kind repeat, canvas switch, 2D→GL, GL→2D, and back again.
        let pushes: Vec<(u32, Canvas2DCmd)> = vec![
            (1, a_rect()),
            (1, a_rect()),
            (2, a_rect()),
            (2, a_rect()),
            (1, a_rect()),
        ];

        let mut c = UnifiedFrameCollector::new();
        let mut expected_2d = 0usize;
        let mut expected_gl = 0usize;
        for (i, (canvas, cmd)) in pushes.into_iter().enumerate() {
            c.push_canvas2d(canvas, cmd);
            expected_2d += 1;
            // Interleave GL work so the collector keeps changing segment kind.
            if i % 2 == 0 {
                c.push_gl(clear_command());
                c.push_gl_fast(clear_command());
                expected_gl += 2;
            }
        }

        let packet = c.build_frame_packet(true).expect("a packet");
        let mut got_2d = 0usize;
        let mut got_gl = 0usize;
        for op in packet.ops() {
            match op {
                FrameOp::CanvasBatch(p) => got_2d += p.commands.len(),
                FrameOp::GlBatch(b) => got_gl += b.commands.len(),
                _ => {}
            }
        }
        assert_eq!(
            got_2d, expected_2d,
            "{} of {expected_2d} Canvas2D commands reached a segment",
            got_2d
        );
        assert_eq!(
            got_gl, expected_gl,
            "{} of {expected_gl} GL commands reached a segment",
            got_gl
        );
    }

    /// **A run of plain Canvas2D draws must be able to trip the soft byte
    /// budget, which for eighteen of them it could not.**
    ///
    /// `AUTO_FLUSH_SOFT_BUDGET_BYTES` exists so a JS turn cannot pin unbounded
    /// pending commands while the render thread works — its own doc names the
    /// case. But the check lived in `webgl::maybe_auto_flush`, which the
    /// hand-written Canvas2D ops (`op_fill_rect`, `op_move_to`, `op_arc`, the
    /// rest) never called: they pushed and returned. Only the eight
    /// `batched_op!`-generated ops asked, and they paid a second `OpState`
    /// borrow — a `BTreeMap` descent, ~15 ns — to ask.
    ///
    /// Both now go through `context2d::with_collector`, which reads
    /// `should_auto_flush` on the borrow the push already holds. The check is
    /// free, so every op can have it.
    ///
    /// This test owns the collector half of that claim: that pushing ordinary
    /// scalar draws does drive `pending_bytes` to the budget and flip
    /// `should_auto_flush`. Whether each op calls the funnel is a fact about
    /// `context2d.rs` and is asserted there.
    #[test]
    fn a_run_of_plain_rect_draws_reaches_the_auto_flush_budget() {
        let mut c = UnifiedFrameCollector::new();
        assert!(
            !c.should_auto_flush(),
            "an empty collector must not ask for a flush"
        );

        // The cheapest possible draw, repeated. If even this reaches the budget,
        // the guard is reachable from any op — which is the point.
        let mut pushes = 0u64;
        while !c.should_auto_flush() {
            c.push_canvas2d(
                1,
                Canvas2DCmd::FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
            );
            pushes += 1;
            assert!(
                pushes < 5_000_000,
                "5M scalar fillRects did not reach the {AUTO_FLUSH_SOFT_BUDGET_BYTES}-byte \
                 budget; either the accounting stopped counting them or the \
                 budget is unreachable from a plain draw"
            );
        }

        // Worth recording what the reachable depth actually is: this is how many
        // un-flushed scalar draws a JS turn could hold before the guard fires.
        assert!(
            c.approx_pending_bytes() >= AUTO_FLUSH_SOFT_BUDGET_BYTES,
            "should_auto_flush tripped at {} bytes, below the budget",
            c.approx_pending_bytes()
        );
        println!("  budget reached after {pushes} scalar fillRect pushes");

        // And a flush clears it, so the guard is not sticky — a frame that
        // flushes mid-way keeps drawing rather than flushing on every push.
        let _ = c.build_frame_packet(true);
        assert!(
            !c.should_auto_flush(),
            "the budget stayed tripped after a flush, so every later push would \
             cut another barrier"
        );
    }

    /// Read the segment's scissor hint out of a built packet.
    fn hint_after(commands: Vec<Canvas2DCmd>) -> Option<shared::protocol::DirtyRect> {
        let mut c = UnifiedFrameCollector::new();
        for cmd in commands {
            c.push_canvas2d(1, cmd);
        }
        let packet = c.build_frame_packet(true).expect("a packet");
        match &packet.ops()[1] {
            FrameOp::CanvasBatch(p) => p.dirty_rect,
            other => panic!("expected CanvasBatch, got {other:?}"),
        }
    }

    fn a_rect() -> Canvas2DCmd {
        Canvas2DCmd::FillRect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        }
    }

    /// **`globalCompositeOperation` writes outside the source geometry, so it
    /// must poison — and before this it fell through to `_ => {}`.**
    ///
    /// Under `copy` a `fillRect(0,0,10,10)` clears the whole rest of the canvas;
    /// `destination-out`, `source-in` and `xor` likewise change pixels the rect
    /// never covers. A 10x10 hint would scissor that away and leave the stale
    /// content on screen.
    ///
    /// The render thread's `state_allows_partial` rejects any non-source-over
    /// blend and would have caught it. That does not make this arm redundant: it
    /// is the only layer that sees the *command*, and a hint that is wrong on
    /// arrival relies on a gate in another crate continuing to agree.
    #[test]
    fn set_composite_operation_poisons_dirty_rect() {
        let hint = hint_after(vec![
            // 9 = `copy`, per the encoding documented on the variant: the
            // mode that clears everything the source does not cover.
            Canvas2DCmd::SetCompositeOperation { op: 9 },
            a_rect(),
        ]);
        assert!(
            hint.is_none(),
            "a composite mode change must poison the scissor hint; got {hint:?}"
        );
    }

    /// A resize re-specifies the backing store, which clears the canvas. The
    /// reallocation is unconditional so it cannot itself be scissored away, but
    /// a small hint would let the presented window keep older content outside it
    /// while the canvas behind went transparent.
    #[test]
    fn resize_canvas_poisons_dirty_rect() {
        let hint = hint_after(vec![
            Canvas2DCmd::ResizeCanvas {
                w: Some(320),
                h: Some(200),
            },
            a_rect(),
        ]);
        assert!(
            hint.is_none(),
            "a canvas resize must poison the scissor hint; got {hint:?}"
        );
    }

    /// The commands that genuinely cannot widen a rect bound must still *not*
    /// poison — otherwise the fail-safe default would have been achieved by
    /// throwing partial updates away entirely, which is not a fix.
    ///
    /// One representative per group in the match: path building, paint style,
    /// stroke geometry, text attribute, save/restore, and a read.
    #[test]
    fn commands_with_no_bounds_effect_keep_the_scissor_hint() {
        for (label, cmd) in [
            ("path building", Canvas2DCmd::BeginPath),
            ("paint style", Canvas2DCmd::SetGlobalAlpha { alpha: 1.0 }),
            (
                "stroke geometry",
                Canvas2DCmd::SetLineDashOffset { offset: 2.0 },
            ),
            (
                "text attribute",
                Canvas2DCmd::SetTextAlign {
                    align: shared::protocol::render_cmd::TextAlign::Start,
                },
            ),
            ("state stack", Canvas2DCmd::Save),
            ("state stack", Canvas2DCmd::Restore),
        ] {
            let hint = hint_after(vec![cmd, a_rect()]);
            assert_eq!(
                hint.map(|r| (r.x, r.y, r.width, r.height)),
                Some((0.0, 0.0, 10.0, 10.0)),
                "{label}: a command with no bounds effect discarded the hint"
            );
        }
    }
}

// ── Section 7.3: zero steady-state allocation on the render command path ────

#[cfg(test)]
mod steady_state_allocation {
    use super::*;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};

    const WARMUP: usize = 4;
    const MEASURED: usize = 64;

    /// A command with no heap payload — the majority of a WebGL frame by count,
    /// and the only kind `push_gl_fast` accepts.
    fn scalar_gl_command() -> GLCmd {
        GLCmd::Clear {
            canvas_id: 1,
            bit_field: 0x4000,
        }
    }

    fn gl_segment_headroom(collector: &UnifiedFrameCollector) -> Option<usize> {
        match collector.segments.last() {
            Some(FrameSegment::GL(seg)) => Some(seg.commands.capacity() - seg.commands.len()),
            _ => None,
        }
    }

    /// Open a GL segment and drive it until it has `headroom` unused slots.
    ///
    /// **The reservation is established here rather than by cycling frames
    /// through the command-vector pool, and that is deliberate.** The pool is
    /// process-global and `cargo test` runs this binary's tests concurrently, so
    /// a gate that depended on getting *its own* recycled vector back would fail
    /// whenever another test took it first — a flaky gate, which is worse than
    /// none. The pool's own reuse property is covered where it can be tested
    /// deterministically, against a private instance, by
    /// `command_vec_pool::tests::recycled_vector_reuses_its_allocation`. What is
    /// left for these gates is the question that test cannot answer: whether the
    /// collector reaches the heap on an enqueue into capacity it already holds.
    ///
    /// Each push either consumes a reserved slot or doubles the capacity, so the
    /// loop converges — **while consecutive GL commands share a segment.** The
    /// bound is what happens when they stop: a collector that opened a segment
    /// per command would leave the newest one at its minimum capacity forever,
    /// and an unbounded loop would then allocate until the process died rather
    /// than report. A fixture that cannot establish its precondition has to say
    /// so; hanging the suite is not a test result.
    fn reserve_gl_segment_headroom(collector: &mut UnifiedFrameCollector, headroom: usize) {
        // Generous: reaching `headroom` needs about `headroom` pushes plus the
        // doublings, so anything near this bound means the invariant is gone.
        let ceiling = headroom * 4 + 64;
        for _ in 0..ceiling {
            if gl_segment_headroom(collector).is_some_and(|slack| slack >= headroom) {
                return;
            }
            collector.push_gl_fast(scalar_gl_command());
        }
        panic!(
            "could not reserve {headroom} free slots in the open GL segment after \
             {ceiling} commands: consecutive GL commands are no longer sharing one \
             segment, so every enqueue takes a fresh vector from the pool"
        );
    }

    /// Hand the accumulated commands back the way the render thread does, so a
    /// gate does not leave the shared pool poorer than it found it.
    ///
    /// Dropping the packet is the whole of it now: every vector it carries is a
    /// loan, and so is the op vector holding them. The earlier version of this
    /// helper walked the ops, emptied each command vector and called the pool by
    /// hand — which is precisely the shape that made a forgotten return
    /// invisible in production code.
    fn end_frame(collector: &mut UnifiedFrameCollector) {
        drop(collector.build_frame_packet(true));
    }

    /// Section 7.3, on the per-event unit of the render command path: one `gl.*`
    /// call from content becomes one command in the open segment. Cocos and
    /// three.js emit hundreds of these per frame, so this is the highest-rate
    /// event the engine handles.
    ///
    /// What it measures is everything `push_gl_fast` does around the push — the
    /// segment lookup, the pending-byte accounting and the peak record — none of
    /// which may reach the heap once the segment holds capacity.
    #[test]
    fn steady_state_gl_command_enqueue_never_reaches_the_heap() {
        let mut collector = UnifiedFrameCollector::new();
        reserve_gl_segment_headroom(&mut collector, WARMUP + MEASURED);

        assert_no_steady_state_allocation(
            Burst {
                path: "frame_collector: per-command GL enqueue into an open segment",
                warmup: WARMUP,
                measured: MEASURED,
            },
            |_| collector.push_gl_fast(scalar_gl_command()),
        );

        end_frame(&mut collector);
    }

    /// Section 7.3, on the set the packet builder gathers its `Materialize`
    /// targets into. One entry per distinct Canvas2D target in the run — one per
    /// text label on the UI this collector was profiled against — and nothing
    /// caps how many a scene has. Above the set's inline capacity a per-frame
    /// set allocates and frees on the thread running the game on every frame of
    /// that scene; one that spans frames pays it once.
    ///
    /// **Asserted on the retained capacity rather than measured by a burst, and
    /// that is not a weaker gate here.** A burst over `build_frame_packet` would
    /// measure the packet's own pooled op vector too, and this binary's several
    /// hundred other tests take from that pool concurrently — the interference
    /// `reserve_gl_segment_headroom` above exists to avoid, which is why the
    /// whole-frame burst lives in an integration test of its own over in
    /// `migo-shared`. That pool is unreachable from here, but the property this
    /// item is about is exactly "the second frame does not allocate again",
    /// which the retained capacity states directly and deterministically.
    #[test]
    fn a_frame_wider_than_the_pending_set_pays_its_spill_once() {
        const DISTINCT: u32 =
            shared::protocol::canvas_id_set::CANVAS_ID_SET_INLINE_CAPACITY as u32 * 2;

        let mut collector = UnifiedFrameCollector::new();
        let wide_frame = |c: &mut UnifiedFrameCollector| {
            // Distinct canvases, so each opens its own Canvas2D segment and all
            // of them are pending at once when the GL segment closes the run.
            for canvas in 0..DISTINCT {
                c.push_canvas2d(canvas, Canvas2DCmd::Save);
            }
            c.push_gl_fast(scalar_gl_command());
            end_frame(c);
        };

        wide_frame(&mut collector);
        assert!(
            collector.pending_2d.spilled(),
            "the frame gathered its pending canvases somewhere else, so the set that \
             spans frames never saw them and every frame pays the spill"
        );
        let reached = collector.pending_2d.capacity();
        assert!(reached >= DISTINCT as usize);

        wide_frame(&mut collector);

        assert_eq!(
            collector.pending_2d.capacity(),
            reached,
            "the second frame grew the set again, so the spill is paid per frame"
        );
    }

    /// Building a packet hands the *segments* to the builder. It must not hand
    /// over the list that held them: that list is refilled from empty on the very
    /// next frame, so surrendering its allocation means the first push of every
    /// frame allocates one again, for the life of the process.
    ///
    /// Not folded into a burst gate, because the two would not be measuring the
    /// same thing. A frame boundary still reaches the heap for the frame
    /// packet's own op vector, which is pooled nowhere and is recorded as its own
    /// task — so a burst across a frame cycle cannot assert zero yet, while this
    /// property can be asserted exactly.
    #[test]
    fn building_a_packet_keeps_the_segment_lists_allocation() {
        let mut collector = UnifiedFrameCollector::new();
        // Several segments, so the list has grown a real allocation to keep.
        for _ in 0..4 {
            collector.push_canvas2d(1, Canvas2DCmd::Save);
            collector.push_gl_fast(scalar_gl_command());
        }
        let reserved = collector.segments.capacity();
        assert!(reserved >= 8, "fixture must produce a list worth keeping");

        end_frame(&mut collector);

        assert_eq!(
            collector.segments.capacity(),
            reserved,
            "the frame gave up the segment list's allocation, so the next frame's \
             first push has to allocate it again"
        );
        assert!(
            collector.segments.is_empty(),
            "the segments themselves must still be handed to the packet"
        );
    }

    /// Section 7.3, on the batched half of the same path: `op_submit_render_stream`
    /// takes a vector from the pool, decodes a stream into it and hands it to
    /// `append_gl_batch`, which appends and recycles. That is one event per
    /// submit, and Pixi reaches it twice a frame with every draw batched behind
    /// it — so it is the path a real game's frame actually takes.
    ///
    /// Its own gate rather than a shared one with the enqueue above: they are
    /// different calls, and a burst covering both could not say which reached the
    /// heap.
    ///
    /// **The batch vectors come from a reservoir built before the burst, not from
    /// `take_gl_command_vec`, and that is the same rule `reserve_gl_segment_headroom`
    /// explains.** The first version of this gate did call the pool inside the
    /// measured window, on the reasoning that it recycles a vector every iteration
    /// and would get one back. That reasoning is wrong under concurrency: a
    /// concurrent test taking the pool's last vector leaves `take` with nothing to
    /// hand back, so it allocates a fresh minimum-capacity one. It cost a green
    /// standalone run and one failure under load — a single 1408-byte allocation,
    /// which is `GL_COMMAND_VEC_INITIAL_CAPACITY * size_of::<GLCmd>()` exactly.
    /// What is measured here is therefore the append and the recycle; whether the
    /// pool hands capacity back is `command_vec_pool`'s own property, tested there
    /// against a private instance where it can be deterministic.
    #[test]
    fn steady_state_gl_batch_append_never_reaches_the_heap() {
        const BATCH: usize = 8;

        let mut collector = UnifiedFrameCollector::new();
        reserve_gl_segment_headroom(&mut collector, (WARMUP + MEASURED) * BATCH);

        // One vector per iteration, each already holding its batch's capacity, so
        // the body neither allocates nor grows.
        let mut reservoir: Vec<PooledVec<GLCmd>> = Vec::with_capacity(WARMUP + MEASURED);
        for _ in 0..WARMUP + MEASURED {
            reservoir.push(PooledVec::from(Vec::with_capacity(BATCH)));
        }

        assert_no_steady_state_allocation(
            Burst {
                path: "frame_collector: batched GL submit append and vector recycle",
                warmup: WARMUP,
                measured: MEASURED,
            },
            |_| {
                let mut commands = reservoir
                    .pop()
                    .expect("the reservoir holds one vector per iteration");
                for _ in 0..BATCH {
                    commands.push(scalar_gl_command());
                }
                let approx = BATCH * std::mem::size_of::<GLCmd>();
                collector.append_gl_batch(commands, approx)
            },
        );

        end_frame(&mut collector);
    }
}
