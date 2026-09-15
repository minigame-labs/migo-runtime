//! Canvas2D command dispatcher — the render-thread-facing glue between
//! a [`shared::protocol::render_cmd::Canvas2DCmd`] stream and the Skia
//! [`crate::backend::gl::surface::Canvas2DContext`] that owns the
//! `SkSurface` for a given canvas.
//!
//! This module exists to keep `render_thread.rs` at a reasonable size and
//! to localise the bits of state the dispatcher needs (the shared
//! `TextContext` + per-frame scratch).  It deliberately mimics the old
//! `Renderer2d` API surface so the render loop doesn't need to be
//! rewritten for the migration.

use shared::error::EngineResult;
use shared::protocol::render_cmd::{Canvas2DCmd, CanvasId};

use crate::damage_effect::DamageEffect;
use crate::{CanvasManager, canvas::BackingSizeOwner};

/// Renderer-side shim around a shared [`TextContext`](crate::backend::gl::text::TextContext).
///
/// The struct itself is stateless beyond the font registry; the actual
/// Canvas2D state (paints, paths, CTM, …) lives per-canvas inside
/// [`crate::backend::gl::surface::Canvas2DContext`].  This layout mirrors
/// the old femtovg `Renderer2d` so `render_thread.rs` can treat the
/// dispatcher as a drop-in replacement.
pub(crate) struct Renderer2d {
    /// F-2: shared ownership with the JS-thread measurer.
    /// `fillText` / `strokeText` acquire this mutex for the
    /// duration of a paint (on the render thread), and the
    /// JS-thread `op_measure_text_flat` acquires it for each
    /// measurement.  The mutex is contended only on cache-miss
    /// paths where the shaping is the real cost anyway; steady-
    /// state `measureText` calls from JS hit the JS-side LRU
    /// first (R-10) and never touch this lock at all.
    pub(crate) text: crate::text_measurer_impl::SharedTextContext,
}

impl Renderer2d {
    /// Construct with an externally-built shared `TextContext`
    /// (F-2).  Used by `RenderThread::spawn`, which builds the
    /// pair off-thread so the `SharedTextMeasurer` half can be
    /// published before the render loop starts.
    pub(crate) fn from_shared_text(shared: crate::text_measurer_impl::SharedTextContext) -> Self {
        Self { text: shared }
    }

    /// Apply a single Canvas2D command.
    ///
    /// Returns `Ok(was_render)` where `was_render = true` means the
    /// command produced an observable framebuffer change (render-thread
    /// uses this to decide whether a present is needed).  `Ok(false)`
    /// covers pure state mutation and path building.
    pub(crate) fn handle_command(
        &mut self,
        cm: &mut CanvasManager,
        canvas_id: CanvasId,
        cmd: Canvas2DCmd,
    ) -> EngineResult<bool> {
        match cmd {
            // CreateContext2D fast path — init the Skia surface in
            // FIFO order with the surrounding command stream.  No reply
            // channel: callers know `canvas_id` already (they passed it
            // in), and any subsequent Canvas2D command on this canvas
            // is serialised behind this op via the render command
            // channel's FIFO semantics, so a race against surface
            // construction is impossible.  An init failure is logged
            // and the canvas stays uninitialised — later draws hit
            // `get_2d_context_mut`'s `NotFound` error, matching the
            // pre-existing failure shape.
            Canvas2DCmd::CreateContext2D => {
                cm.init_skia_for_canvas(canvas_id)?;
                Ok(false)
            }
            // In-band resize: keeps surface dimension changes serialised
            // with the surrounding `FillText` / `TexImage2DFromCanvas2D`
            // commands.  Errors are logged but not propagated — a failed
            // resize leaves the surface at its previous size, matching
            // browser behaviour for the pathological case where the OS
            // refuses a new pbuffer (allocation failure / oversize).
            Canvas2DCmd::ResizeCanvas { w, h } => {
                if let Err(e) = cm.resize_canvas(canvas_id, w, h, BackingSizeOwner::Content) {
                    tracing::warn!(
                        "Canvas2DCmd::ResizeCanvas failed: canvas={:?}, w={:?}, h={:?}, err={}",
                        canvas_id,
                        w,
                        h,
                        e
                    );
                }
                Ok(false)
            }
            // Every failure here has to reach `resp`, and until 2026-09-11 none
            // of them did: three `?` operators returned early with the responder
            // still unanswered, so the caller -- a JavaScript `getImageData`
            // blocked on this reply -- was told
            // "internal error (render op responder dropped without sending a
            // reply (likely a handler forgot to call ...))", a message about the
            // plumbing, while the actual reason went to the host's render-event
            // channel, which is not where the caller is looking.
            //
            // Measured on macOS: a read outside the canvas reported
            // `Canvas2D getImageData read_pixels failed` to the host and the
            // dropped-responder text to the content, and the two had to be
            // matched up by hand across two log streams.
            //
            // Answering and returning `Ok(false)` is `MeasureText`'s shape three
            // arms below, and for the same reason: a synchronous op's failure
            // belongs to the caller that is waiting for it, not to the host as a
            // render error. `MeasureText` had it right and this arm did not.
            Canvas2DCmd::GetImageData {
                x,
                y,
                width,
                height,
                resp,
            } => {
                match cm
                    .make_current_needed(canvas_id)
                    .and_then(|()| cm.get_2d_context_mut(canvas_id))
                    .and_then(|ctx| ctx.read_image_data(x, y, width, height))
                {
                    Ok(pixels) => resp.ok(pixels),
                    Err(e) => resp.err(e),
                }
                Ok(false)
            }
            Canvas2DCmd::ReadSnapshotPixels { snapshot_id, resp } => {
                crate::render_diagnostics::bump_canvas2d_snapshot_forced_readback();
                let pixels = cm
                    .read_canvas2d_snapshot_pixels(snapshot_id)
                    .unwrap_or_default();
                let _ = resp.send(Ok(pixels));
                Ok(false)
            }
            Canvas2DCmd::CaptureSnapshot {
                x,
                y,
                width,
                height,
                snapshot_id,
                cache_key,
            } => {
                let id = match cm.snapshot_canvas2d_region_with_id(
                    canvas_id,
                    x,
                    y,
                    width,
                    height,
                    snapshot_id,
                ) {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(
                            "snapshot_canvas2d_region_with_id failed for canvas {:?}: {}",
                            canvas_id,
                            e
                        );
                        0
                    }
                };
                if id == 0 {
                    crate::render_diagnostics::bump_canvas2d_snapshot_fallback();
                    // Cache record path was conditional on a successful
                    // snapshot — if capture failed, drop the key on the
                    // floor.  The downstream `TexImage2DFromSnapshot`
                    // will also fail (and fall back to the JS-side legacy
                    // path), and JS won't see the cache populated; on
                    // the next attempt the cache miss recurs but eventually
                    // a successful snapshot populates it.
                    let _ = cache_key;
                } else {
                    crate::render_diagnostics::bump_canvas2d_snapshot_taken();
                    if let Some(key) = cache_key {
                        cm.mark_snapshot_for_text_cache(snapshot_id, key);
                    }
                }
                Ok(false)
            }
            // MeasureText must be intercepted here: the downstream
            // `apply_with_images` path borrows the command by reference, so
            // its `MeasureText { resp, .. }` arm can't actually consume
            // `resp` and reply.  Routing the measurement through the
            // dispatcher lets us read the per-canvas `TextAttrs` from the
            // renderer's own state and send the metrics back on the reply
            // channel before `Canvas2DCmd` is dropped — without this, the
            // sender side is dropped silently and the JS op fails with
            // "channel disconnected", which collapses every text layout to
            // zero-width metrics (symptom: fonts don't render at all).
            Canvas2DCmd::MeasureText { text, resp } => {
                let metrics = match cm.make_current_needed(canvas_id) {
                    Ok(()) => match cm.get_2d_context_mut(canvas_id) {
                        Ok(ctx) => {
                            // Lock the shared TextContext only for the
                            // duration of the measurement call; JS side
                            // may contend but cache-hit paths are sub-μs.
                            let mut text_ctx = self.text.lock();
                            text_ctx.get().measure_text(&text, &ctx.renderer.state.text)
                        }
                        Err(e) => {
                            resp.err(e);
                            return Ok(false);
                        }
                    },
                    Err(e) => {
                        resp.err(e);
                        return Ok(false);
                    }
                };
                resp.ok(metrics);
                Ok(false)
            }
            cmd => {
                // Everything else routes through the per-canvas handler.
                // Split-borrow `cm` so the handler can see both the 2D context
                // (mutable, owns the Skia surface + GrDirectContext) and the
                // shared image store (immutable, holds the GL texture table)
                // at the same time — this is what turns `drawImage` from a
                // no-op into a real rasterised draw.
                cm.make_current_needed(canvas_id)?;
                let (ctx, image_store) = cm.split_2d_and_images(canvas_id)?;
                // Q6: only commands that execute shaping/painting acquire the
                // shared text mutex, and only AFTER the split borrow succeeds so
                // a split error never takes the mutex. The classification is
                // exhaustive in the command's defining crate, so adding a future
                // variant requires an explicit lock decision at compile time. The
                // guard then covers exactly the `apply_with_images`/`apply_env`
                // text execution; text state setters remain ordered commands but
                // do not consult TextContext.
                let mut text_guard = cmd.requires_text_context().then(|| self.text.lock());
                let text_ctx = text_guard.as_mut().map(|guard| &*guard.get());
                Ok(ctx.apply_with_images(&cmd, text_ctx, image_store))
            }
        }
    }

    /// Legacy per-layer dirty bit hook.  Skia's own `GrDirectContext`
    /// tracks atlas / tile invalidation, so this is a no-op for now —
    /// kept to preserve the render-loop call sites unchanged.
    pub(crate) fn clear_dirty_layer(&mut self, _canvas_id: CanvasId) {}
}

/// Damage classifier with a conservative partial-damage fast path.
///
/// Returns one of:
///   * `NoDamage` for pure state / path-building commands,
///   * `OnscreenRect { … }` for rect-shaped paints that we can bound
///     tightly AND the state gate [`state_allows_partial`] permits,
///   * `FullSurface` for everything else.
///
/// The partial-damage subset is intentionally narrow: it only fires
/// for `fillRect` / `clearRect` / `strokeRect` / `drawImage` when the
/// current 2D state guarantees the paint stays inside the declared
/// rect.  Any property that could extend or shift the paint (shadow,
/// non-source-over composite, `globalAlpha != 1`, non-axis-aligned
/// CTM, any path clip) forces a full-surface declaration.  The cost
/// of a false-positive partial rect would be subtle residual pixels
/// outside the damage region, which is never acceptable.
///
/// The function takes the *current* `Canvas2DState`; the render thread
/// already re-evaluates this per command so we get up-to-date CTM /
/// shadow / blend info without plumbing anything extra.
#[inline]
pub(crate) fn classify_draw_damage(
    cmd: &Canvas2DCmd,
    state: &crate::backend::gl::state::Canvas2DState,
) -> DamageEffect {
    use Canvas2DCmd::*;
    // Pure state / path-building commands never modify the framebuffer.
    match cmd {
        BeginPath
        | ClosePath
        | MoveTo { .. }
        | LineTo { .. }
        | QuadraticCurveTo { .. }
        | BezierCurveTo { .. }
        | Arc { .. }
        | ArcTo { .. }
        | Rect { .. }
        | Ellipse { .. }
        | SetFillStyle { .. }
        | SetStrokeStyle { .. }
        | SetLineWidth { .. }
        | SetLineCap { .. }
        | SetLineJoin { .. }
        | SetMiterLimit { .. }
        | SetGlobalAlpha { .. }
        | SetCompositeOperation { .. }
        | SetLineDash { .. }
        | SetLineDashOffset { .. }
        | SetShadowBlur { .. }
        | SetShadowColor { .. }
        | SetShadowOffsetX { .. }
        | SetShadowOffsetY { .. }
        | SetFillStyleGradient { .. }
        | SetStrokeStyleGradient { .. }
        | SetFillStylePattern { .. }
        | SetStrokeStylePattern { .. }
        | SetFont { .. }
        | SetTextAlign { .. }
        | SetTextBaseline { .. }
        | SetTextDirection { .. }
        | Save
        | Restore
        | SetTransform { .. }
        | ResetTransform
        | Translate { .. }
        | Rotate { .. }
        | Scale { .. }
        | Clip
        | MeasureText { .. }
        | GetImageData { .. }
        | CaptureSnapshot { .. }
        | ReadSnapshotPixels { .. }
        | CreateContext2D
        | ResizeCanvas { .. } => return DamageEffect::NoDamage,
        _ => {}
    }

    // Fast-path: tight bounding rects.  Only fire when the global
    // state is safe (no shadow / filter / alpha modulation / non
    // source-over composite / non-axis-aligned CTM / user clip).
    if !state_allows_partial(state) {
        return DamageEffect::FullSurface;
    }

    // All rect-producing commands operate in OBJECT SPACE coords;
    // the damage region must be the DEVICE SPACE bounding box, so
    // every fast-path branch routes through `rect_damage_in_space`
    // which applies `state.ctm` before floor/ceil.
    match cmd {
        FillRect { x, y, w, h } | ClearRect { x, y, w, h } => {
            rect_damage_in_space(state, *x, *y, *w, *h)
        }
        StrokeRect { x, y, w, h } => {
            // Canvas 2D `strokeRect` paints a 1-lineWidth band
            // centered on the rect perimeter.  Expand the OBJECT
            // SPACE rect by half the line width, then map through
            // the CTM.  (The object-space expansion is correct
            // because `lineWidth` is in user-space coords — see
            // the Canvas 2D spec on lineWidth units.)
            let half = state.line_width * 0.5;
            rect_damage_in_space(
                state,
                *x - half,
                *y - half,
                *w + state.line_width,
                *h + state.line_width,
            )
        }
        DrawImage { dx, dy, dw, dh, .. } => rect_damage_in_space(state, *dx, *dy, *dw, *dh),
        DrawImageBatch { draws } => {
            // Union the sub-rects in OBJECT SPACE first, then CTM
            // the single union rect once.  This produces the same
            // device-space bbox as CTM-ing each sub-rect and
            // unioning (true because CTM is axis-aligned in this
            // branch; state_allows_partial gated that).
            //
            // Each entry MUST be normalised to `(l <= r, t <= b)`
            // before unioning: Canvas 2D allows negative dw/dh
            // (flips the image), and a naive `min/max` of un-
            // normalised corners loses non-overlapping regions —
            // e.g. one entry at x=[50,100] + another at x=[0,10]
            // would produce a bbox of [0,50], silently dropping
            // the [50,100] paint.
            let mut acc: Option<(f32, f32, f32, f32)> = None;
            for d in draws.iter() {
                // Skip obviously degenerate / non-finite entries
                // so they can't poison the accumulator with NaN.
                if !(d.dx.is_finite() && d.dy.is_finite() && d.dw.is_finite() && d.dh.is_finite()) {
                    continue;
                }
                let (l, r) = if d.dw < 0.0 {
                    (d.dx + d.dw, d.dx)
                } else {
                    (d.dx, d.dx + d.dw)
                };
                let (t, b) = if d.dh < 0.0 {
                    (d.dy + d.dh, d.dy)
                } else {
                    (d.dy, d.dy + d.dh)
                };
                if !(r > l && b > t) {
                    continue; // zero-area entry paints nothing
                }
                acc = Some(match acc {
                    Some((l0, t0, r0, b0)) => (l0.min(l), t0.min(t), r0.max(r), b0.max(b)),
                    None => (l, t, r, b),
                });
            }
            match acc {
                Some((l, t, r, b)) => rect_damage_in_space(state, l, t, r - l, b - t),
                None => DamageEffect::NoDamage,
            }
        }
        // Everything else (path fill/stroke, text, putImageData …)
        // still falls back to full surface.  Bounding-box extraction
        // for those needs the Skia path / layout, which we don't
        // want to run twice per draw.
        _ => DamageEffect::FullSurface,
    }
}

/// Transform an object-space rect through the current CTM, then
/// snap outward to pixel bounds for the damage region.
///
/// Must only be called when `state_allows_partial(state)` is true:
/// callers rely on that gate to ensure `ctm_is_axis_aligned()`
/// holds, without which the per-corner min/max below under-reports
/// the actual paint coverage.
#[inline]
fn rect_damage_in_space(
    state: &crate::backend::gl::state::Canvas2DState,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> DamageEffect {
    // Reject non-finite coords / dimensions up front; any NaN or
    // infinity at this stage would propagate into the device-space
    // bbox and corrupt the compositor's dirty rects.
    if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
        return DamageEffect::NoDamage;
    }
    // Canvas 2D spec: negative width/height paint LEFT/UP from the
    // origin (see fillRect / drawImage spec).  Normalise into the
    // canonical `(x, y, |w|, |h|)` box so the damage classifier
    // correctly reports the covered region — a naive `w > 0 && h > 0`
    // guard would report NoDamage for `fillRect(10, 10, -5, -5)` and
    // leave those pixels ghosted on next compositor pass.
    let (x, w) = if w < 0.0 { (x + w, -w) } else { (x, w) };
    let (y, h) = if h < 0.0 { (y + h, -h) } else { (y, h) };
    // Degenerate zero-area rect paints nothing — genuine NoDamage.
    if w <= 0.0 || h <= 0.0 {
        return DamageEffect::NoDamage;
    }
    // Map to device space.  `map_axis_aligned_rect` debug-asserts
    // the shear predicate; the `state_allows_partial` gate guards
    // it in release builds.
    let Some((mx, my, mw, mh)) = state.map_axis_aligned_rect(x, y, w, h) else {
        return DamageEffect::FullSurface;
    };
    // Post-transform dimensions can still end up negative when the
    // CTM embeds a reflection (e.g. `scale(-1, 1)`).  Fall back to
    // min/max rather than `.floor()/.ceil()` assuming canonical
    // layout, since the latter would produce an empty rect.
    let left = mx.min(mx + mw).floor() as i32;
    let top = my.min(my + mh).floor() as i32;
    let right = mx.max(mx + mw).ceil() as i32;
    let bottom = my.max(my + mh).ceil() as i32;
    DamageEffect::OnscreenRect {
        x: left,
        y: top,
        width: (right - left).max(0),
        height: (bottom - top).max(0),
    }
}

/// True when the 2D state allows the damage classifier to emit a
/// tight rect for rectangle-shaped paints.  Any of the following
/// forces `FullSurface`:
///   * non-identity CTM except for integer translations (rotations,
///     skew, non-axis-aligned scale can move pixels anywhere),
///   * active shadow (shadow paints outside the rect),
///   * non source-over composite (copy / destination-out / xor
///     invalidate pixels beyond the source rect),
///   * `globalAlpha != 1` (OK actually — still bounded — but we
///     forbid it for safety margin until we validate),
///   * non-trivial stroke join at high miter limit (miter spikes
///     exceed the half-width expansion).
///
/// Scissor / clip tracking isn't exposed through the handler
/// interface yet, so user clips also fall back to full surface by
/// way of the CTM check (clips typically go alongside non-identity
/// transforms in real code).
#[inline]
pub(crate) fn state_allows_partial(state: &crate::backend::gl::state::Canvas2DState) -> bool {
    use skia_safe::BlendMode;
    if state.shadow.is_visible() {
        return false;
    }
    if state.blend_mode != BlendMode::SrcOver {
        return false;
    }
    if (state.global_alpha - 1.0).abs() > 1e-6 {
        return false;
    }
    // Miter joins can spike past half-line-width; only allow
    // rectangle stroke damage when the miter limit is at/below the
    // spec default, or the join is round/bevel (which clip cleanly).
    if matches!(state.line_join, skia_safe::PaintJoin::Miter) && state.miter_limit > 10.0 {
        return false;
    }
    // Non-axis-aligned CTM (rotation, skew, off-diagonal
    // `setTransform`) can project a rectangle to arbitrary coverage,
    // so the classifier can't bound the damage tightly.  Pure
    // translation and axis-aligned scale (including negative /
    // reflection) are fine — they keep the bounding box a bbox.
    //
    // Derived from the live CTM every call, so `setTransform(1,0,0,1,0,0)`
    // after a `rotate()` correctly re-enables the fast path (the
    // previous implementation's sticky boolean left it stuck on).
    if !state.ctm_is_axis_aligned() {
        return false;
    }
    true
}

#[cfg(test)]
// The 0.7071068 literals are the entries of a 45-degree rotation matrix, laid
// out as `[cos, sin, -sin, cos, 0, 0]` so the matrix is readable as a matrix.
// Swapping them for FRAC_1_SQRT_2 would name the number and hide the shape,
// and these tests are about the shape.
#[allow(clippy::approx_constant)]
mod partial_damage_tests {
    use super::*;
    use crate::backend::gl::state::Canvas2DState;
    use crate::damage_effect::DamageEffect;
    use shared::protocol::render_cmd::Canvas2DCmd;

    fn base() -> Canvas2DState {
        Canvas2DState::default()
    }

    #[test]
    fn state_allows_partial_default_is_true() {
        assert!(state_allows_partial(&base()));
    }

    #[test]
    fn shadow_disables_partial() {
        let mut s = base();
        s.shadow.blur = 4.0;
        s.shadow.color = shared::protocol::color::Color::black();
        assert!(!state_allows_partial(&s));
    }

    #[test]
    fn non_source_over_disables_partial() {
        let mut s = base();
        s.blend_mode = skia_safe::BlendMode::Plus;
        assert!(!state_allows_partial(&s));
    }

    #[test]
    fn global_alpha_lt_1_disables_partial() {
        let mut s = base();
        s.global_alpha = 0.5;
        assert!(!state_allows_partial(&s));
    }

    #[test]
    fn rotated_ctm_disables_partial() {
        let mut s = base();
        // 45-degree rotation: ctm = [cos, sin, -sin, cos, 0, 0].
        let (sin45, cos45) = (0.7071068_f32, 0.7071068_f32);
        s.ctm = [cos45, sin45, -sin45, cos45, 0.0, 0.0];
        assert!(!state_allows_partial(&s));
    }

    #[test]
    fn setTransform_back_to_identity_re_enables_partial() {
        // Regression: the old `ctm_non_axis_aligned` was sticky,
        // so `rotate()` then `setTransform(1,0,0,1,0,0)` left the
        // flag permanently stuck on.  Now derived from the live
        // matrix so a `setTransform` back to an axis-aligned
        // matrix restores the fast path.
        let mut s = base();
        s.ctm = [0.7071068, 0.7071068, -0.7071068, 0.7071068, 0.0, 0.0];
        assert!(!state_allows_partial(&s));
        s.ctm_set([1.0, 0.0, 0.0, 1.0, 100.0, 50.0]);
        assert!(state_allows_partial(&s));
    }

    #[test]
    fn fill_rect_yields_onscreen_rect_when_safe() {
        let s = base();
        let cmd = Canvas2DCmd::FillRect {
            x: 10.0,
            y: 20.0,
            w: 50.0,
            h: 30.0,
        };
        let d = classify_draw_damage(&cmd, &s);
        assert_eq!(
            d,
            DamageEffect::OnscreenRect {
                x: 10,
                y: 20,
                width: 50,
                height: 30
            }
        );
    }

    #[test]
    fn fill_rect_with_shadow_falls_back_to_full_surface() {
        let mut s = base();
        s.shadow.blur = 10.0;
        s.shadow.color = shared::protocol::color::Color::black();
        let cmd = Canvas2DCmd::FillRect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        assert_eq!(classify_draw_damage(&cmd, &s), DamageEffect::FullSurface);
    }

    #[test]
    fn clear_rect_yields_onscreen_rect() {
        let cmd = Canvas2DCmd::ClearRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        assert_eq!(
            classify_draw_damage(&cmd, &base()),
            DamageEffect::OnscreenRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            }
        );
    }

    #[test]
    fn stroke_rect_expands_by_half_line_width() {
        let mut s = base();
        s.line_width = 4.0;
        let cmd = Canvas2DCmd::StrokeRect {
            x: 10.0,
            y: 10.0,
            w: 20.0,
            h: 20.0,
        };
        // Outer bounds: (8, 8) to (32, 32) → 24x24.
        assert_eq!(
            classify_draw_damage(&cmd, &s),
            DamageEffect::OnscreenRect {
                x: 8,
                y: 8,
                width: 24,
                height: 24,
            }
        );
    }

    #[test]
    fn path_fill_always_falls_back_to_full_surface() {
        // Path drawing isn't in the safe subset (bounding box
        // extraction would require the Skia path, which the
        // classifier intentionally doesn't run).
        let cmd = Canvas2DCmd::Fill;
        assert_eq!(
            classify_draw_damage(&cmd, &base()),
            DamageEffect::FullSurface
        );
    }

    #[test]
    fn draw_image_yields_dst_rect_damage() {
        let cmd = Canvas2DCmd::DrawImage {
            image_id: 42,
            sx: 0.0,
            sy: 0.0,
            sw: 32.0,
            sh: 32.0,
            dx: 100.0,
            dy: 200.0,
            dw: 64.0,
            dh: 32.0,
        };
        assert_eq!(
            classify_draw_damage(&cmd, &base()),
            DamageEffect::OnscreenRect {
                x: 100,
                y: 200,
                width: 64,
                height: 32
            }
        );
    }

    // ---- CTM mapping regression tests (P0-1) ------------------------
    //
    // Bug: damage classifier used to return object-space rects,
    // ignoring the current transform.  `translate(100,100)` then
    // `fillRect(10,10,50,50)` SHOULD paint at device (110,110)-
    // (160,160), but the classifier reported (10,10)-(60,60) — so
    // the compositor repainted the wrong tile, and the moved rect
    // either flickered or ghosted.
    //
    // These tests pin the corrected behaviour: every partial-damage
    // branch now maps through `state.ctm` before floor/ceil.

    fn fill_rect_cmd(x: f32, y: f32, w: f32, h: f32) -> Canvas2DCmd {
        Canvas2DCmd::FillRect { x, y, w, h }
    }

    fn expect_rect(d: DamageEffect, x: i32, y: i32, w: i32, h: i32) {
        assert_eq!(
            d,
            DamageEffect::OnscreenRect {
                x,
                y,
                width: w,
                height: h,
            }
        );
    }

    #[test]
    fn translate_shifts_damage_rect() {
        let mut s = base();
        // translate(100, 100)
        s.ctm_concat([1.0, 0.0, 0.0, 1.0, 100.0, 100.0]);
        expect_rect(
            classify_draw_damage(&fill_rect_cmd(10.0, 20.0, 50.0, 30.0), &s),
            110,
            120,
            50,
            30,
        );
    }

    #[test]
    fn scale_expands_damage_rect() {
        let mut s = base();
        // scale(2, 3)
        s.ctm_concat([2.0, 0.0, 0.0, 3.0, 0.0, 0.0]);
        expect_rect(
            classify_draw_damage(&fill_rect_cmd(5.0, 10.0, 20.0, 10.0), &s),
            10,
            30,
            40,
            30,
        );
    }

    #[test]
    fn translate_then_scale_composes() {
        let mut s = base();
        s.ctm_concat([1.0, 0.0, 0.0, 1.0, 7.0, 11.0]); // translate(7, 11)
        s.ctm_concat([2.0, 0.0, 0.0, 2.0, 0.0, 0.0]); // scale(2, 2)
        // Canvas 2D semantics: transforms accumulate left-to-right,
        // so subsequent paints are first scaled, then translated.
        // rect(0, 0, 10, 10) → scaled to (0,0,20,20) → translated
        // by (7, 11) → (7, 11, 20, 20).
        expect_rect(
            classify_draw_damage(&fill_rect_cmd(0.0, 0.0, 10.0, 10.0), &s),
            7,
            11,
            20,
            20,
        );
    }

    #[test]
    fn setTransform_replaces_not_concatenates() {
        let mut s = base();
        s.ctm_concat([1.0, 0.0, 0.0, 1.0, 50.0, 50.0]); // translate
        s.ctm_set([2.0, 0.0, 0.0, 2.0, 0.0, 0.0]); // setTransform replaces
        // The earlier translate must NOT leak into the result.
        expect_rect(
            classify_draw_damage(&fill_rect_cmd(0.0, 0.0, 10.0, 10.0), &s),
            0,
            0,
            20,
            20,
        );
    }

    #[test]
    fn reset_transform_clears_ctm() {
        let mut s = base();
        s.ctm_concat([2.0, 0.0, 0.0, 2.0, 100.0, 100.0]);
        s.ctm_reset();
        expect_rect(
            classify_draw_damage(&fill_rect_cmd(0.0, 0.0, 10.0, 10.0), &s),
            0,
            0,
            10,
            10,
        );
    }

    #[test]
    fn negative_scale_reflection_stays_axis_aligned() {
        // scale(-1, 1) mirrors horizontally — the bbox still ends
        // up a rect, just flipped.  `map_axis_aligned_rect` has
        // to use min/max after transform for this to work.
        let mut s = base();
        s.ctm_concat([-1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        expect_rect(
            classify_draw_damage(&fill_rect_cmd(10.0, 20.0, 50.0, 30.0), &s),
            -60,
            20,
            50,
            30,
        );
    }

    #[test]
    fn stroke_rect_with_translate_expands_then_translates() {
        let mut s = base();
        s.line_width = 4.0;
        s.ctm_concat([1.0, 0.0, 0.0, 1.0, 100.0, 100.0]);
        // Object-space expansion by half line width → (8, 8, 24, 24),
        // then translate by (100, 100) → (108, 108, 24, 24).
        expect_rect(
            classify_draw_damage(
                &Canvas2DCmd::StrokeRect {
                    x: 10.0,
                    y: 10.0,
                    w: 20.0,
                    h: 20.0,
                },
                &s,
            ),
            108,
            108,
            24,
            24,
        );
    }

    #[test]
    fn draw_image_with_scale_expands_dst_rect() {
        let mut s = base();
        s.ctm_concat([2.0, 0.0, 0.0, 2.0, 10.0, 10.0]);
        let cmd = Canvas2DCmd::DrawImage {
            image_id: 1,
            sx: 0.0,
            sy: 0.0,
            sw: 32.0,
            sh: 32.0,
            dx: 5.0,
            dy: 5.0,
            dw: 20.0,
            dh: 20.0,
        };
        // dst (5,5,20,20) → scale(2) → (10,10,40,40) → translate(10,10)
        //   → (20,20,40,40).
        expect_rect(classify_draw_damage(&cmd, &s), 20, 20, 40, 40);
    }

    #[test]
    fn rotated_ctm_falls_back_to_full_surface() {
        let mut s = base();
        s.ctm = [0.7071068, 0.7071068, -0.7071068, 0.7071068, 0.0, 0.0];
        assert_eq!(
            classify_draw_damage(&fill_rect_cmd(0.0, 0.0, 10.0, 10.0), &s),
            DamageEffect::FullSurface,
        );
    }

    #[test]
    fn non_finite_ctm_falls_back_to_full_surface() {
        // Pathological CTM (e.g. post-matrix-inverse numerical
        // failure) must not produce NaN damage rects.
        let mut s = base();
        s.ctm = [f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0];
        assert_eq!(
            classify_draw_damage(&fill_rect_cmd(0.0, 0.0, 10.0, 10.0), &s),
            DamageEffect::FullSurface,
        );
    }

    // ---- negative-dimension normalisation (P1-4) --------------------
    //
    // Canvas 2D allows negative width/height, which the spec defines
    // as painting left/up from the origin.  Prior to this commit the
    // damage classifier returned `NoDamage` for those commands and
    // left the "flipped" pixels ghosted on next compositor pass.

    #[test]
    fn fill_rect_negative_width_normalises_to_canonical_box() {
        // `fillRect(10, 10, -5, -5)` paints the box (5, 5) — (10, 10).
        let cmd = fill_rect_cmd(10.0, 10.0, -5.0, -5.0);
        expect_rect(classify_draw_damage(&cmd, &base()), 5, 5, 5, 5);
    }

    #[test]
    fn draw_image_negative_dst_flip_damages_canonical_box() {
        // `drawImage(img, 0,0,32,32, 100, 100, -50, -50)` flips the
        // image and paints (50, 50) — (100, 100).
        let cmd = Canvas2DCmd::DrawImage {
            image_id: 1,
            sx: 0.0,
            sy: 0.0,
            sw: 32.0,
            sh: 32.0,
            dx: 100.0,
            dy: 100.0,
            dw: -50.0,
            dh: -50.0,
        };
        expect_rect(classify_draw_damage(&cmd, &base()), 50, 50, 50, 50);
    }

    #[test]
    fn nan_rect_dimensions_fall_back_to_no_damage() {
        // Defence against JS passing `Number.NaN` through setters.
        // The old guard was just `w > 0 && h > 0` which evaluated
        // to false for NaN (OK) but also false for negative w — the
        // latter was a false NoDamage.  New guard checks finiteness
        // explicitly and normalises negatives.
        let cmd = fill_rect_cmd(0.0, 0.0, f32::NAN, 10.0);
        assert_eq!(classify_draw_damage(&cmd, &base()), DamageEffect::NoDamage,);
    }

    #[test]
    fn zero_area_rect_paints_nothing() {
        // `fillRect(x, y, 0, 10)` is well-formed but paints no
        // pixels — damage should stay empty, not turn into a
        // 0-wide rectangle that the compositor still processes.
        let cmd = fill_rect_cmd(10.0, 10.0, 0.0, 10.0);
        assert_eq!(classify_draw_damage(&cmd, &base()), DamageEffect::NoDamage,);
    }

    #[test]
    fn draw_image_batch_with_disjoint_negative_flips_preserves_union() {
        // Regression: union previously mis-handled flipped entries.
        // Entry A paints [50, 100] (via dw=-50 from dx=100).
        // Entry B paints [0, 10].  True union bbox is [0, 100].
        let cmd = Canvas2DCmd::DrawImageBatch {
            draws: vec![
                shared::protocol::render_cmd::DrawImageEntry {
                    image_id: 1,
                    sx: 0.0,
                    sy: 0.0,
                    sw: 10.0,
                    sh: 10.0,
                    dx: 100.0,
                    dy: 10.0,
                    dw: -50.0,
                    dh: 10.0,
                },
                shared::protocol::render_cmd::DrawImageEntry {
                    image_id: 1,
                    sx: 0.0,
                    sy: 0.0,
                    sw: 10.0,
                    sh: 10.0,
                    dx: 0.0,
                    dy: 10.0,
                    dw: 10.0,
                    dh: 10.0,
                },
            ],
        };
        expect_rect(classify_draw_damage(&cmd, &base()), 0, 10, 100, 10);
    }

    #[test]
    fn draw_image_batch_skips_non_finite_entries() {
        // One NaN entry must not poison the accumulator; the other
        // entry still contributes a legitimate rect.
        let cmd = Canvas2DCmd::DrawImageBatch {
            draws: vec![
                shared::protocol::render_cmd::DrawImageEntry {
                    image_id: 1,
                    sx: 0.0,
                    sy: 0.0,
                    sw: 10.0,
                    sh: 10.0,
                    dx: f32::NAN,
                    dy: 0.0,
                    dw: 10.0,
                    dh: 10.0,
                },
                shared::protocol::render_cmd::DrawImageEntry {
                    image_id: 1,
                    sx: 0.0,
                    sy: 0.0,
                    sw: 10.0,
                    sh: 10.0,
                    dx: 0.0,
                    dy: 0.0,
                    dw: 10.0,
                    dh: 10.0,
                },
            ],
        };
        expect_rect(classify_draw_damage(&cmd, &base()), 0, 0, 10, 10);
    }

    #[test]
    fn reflection_ctm_keeps_device_rect_positive() {
        // `scale(-1, -1)` with `fillRect(10, 20, 50, 30)` paints
        // (-60, -50)-(-10, -20).  After normalisation the damage
        // must be the positive-extent bbox (-60, -50, 50, 30).
        let mut s = base();
        s.ctm_concat([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]);
        expect_rect(
            classify_draw_damage(&fill_rect_cmd(10.0, 20.0, 50.0, 30.0), &s),
            -60,
            -50,
            50,
            30,
        );
    }

    #[test]
    fn draw_image_batch_unions_then_transforms() {
        let mut s = base();
        s.ctm_concat([1.0, 0.0, 0.0, 1.0, 100.0, 100.0]);
        let cmd = Canvas2DCmd::DrawImageBatch {
            draws: vec![
                shared::protocol::render_cmd::DrawImageEntry {
                    image_id: 1,
                    sx: 0.0,
                    sy: 0.0,
                    sw: 10.0,
                    sh: 10.0,
                    dx: 0.0,
                    dy: 0.0,
                    dw: 10.0,
                    dh: 10.0,
                },
                shared::protocol::render_cmd::DrawImageEntry {
                    image_id: 1,
                    sx: 0.0,
                    sy: 0.0,
                    sw: 10.0,
                    sh: 10.0,
                    dx: 20.0,
                    dy: 20.0,
                    dw: 10.0,
                    dh: 10.0,
                },
            ],
        };
        // Object-space union: (0,0)-(30,30) = (0,0,30,30).
        // After translate(100,100): (100,100,30,30).
        expect_rect(classify_draw_damage(&cmd, &s), 100, 100, 30, 30);
    }
}
