//! The per-context Canvas2D render loop.
//!
//! A [`Canvas2DRenderer`] owns the mutable drawing state (`Canvas2DState`),
//! the save/restore stack, and the in-flight `CanvasPath`.  It *does not*
//! own an `SkCanvas` — the canvas is passed in per-call so the same
//! context can render to CPU raster surfaces (tests) or the GPU-backed
//! onscreen `SkSurface` (production) without refactoring.
//!
//! Side-effecting commands that produce a reply (MeasureText / GetImageData)
//! are not handled here; the upper layer dispatches them directly against
//! the surface before routing the reply channel.
//!
//! The handler is *non-exhaustive* (see `Canvas2DCmd`): unknown variants
//! are logged and ignored rather than panicking, so adding a new command
//! upstream does not break builds of older backend revisions.

use shared::protocol::color::Color as ProtocolColor;
use shared::protocol::render_cmd::{Canvas2DCmd, GradientType, TextAlign, TextBaseline};
use skia_safe::{Canvas, ClipOp, Matrix, Paint, PaintCap, PaintJoin, Rect as SkRect};

use super::blend_mode::blend_mode_from_code;
use super::paint::{PatternResolver, build_clear_paint, build_fill_paint, build_stroke_paint};
use super::path::CanvasPath;
use super::state::{Canvas2DState, Shadow, StateStack, StyleKind};
use super::text::TextContext;

/// Bundle of render-time resources passed alongside each command.
///
/// Factored out so the handler stays a single entry point regardless of
/// which resources a specific command consults: plain `fillRect` only
/// uses `canvas`; `fillText` additionally requires `text`; `drawImage` /
/// `Pattern` gradient uses `resolver`.
///
/// `'e` bounds the lifetime of the environment borrow, ensuring that
/// `apply()` does not retain any reference past the call.
pub struct DrawEnv<'e, R: PatternResolver> {
    pub canvas: &'e Canvas,
    pub text: Option<&'e TextContext>,
    pub resolver: &'e R,
}

/// Owns the Canvas2D state for one `CanvasRenderingContext2D`.
pub struct Canvas2DRenderer {
    /// Whether consecutive same-image sprite runs may be merged into one
    /// `SkCanvas::drawAtlas`.
    ///
    /// Resolved once from
    /// [`shared::feature_policy::FeatureKey::CanvasDrawAtlas`]; a batch is far
    /// too hot to consult a policy map per draw.
    pub(crate) draw_atlas: bool,
    /// Whether an atlas run has already been reported.
    ///
    /// One line per process, not per batch: the question it answers -- did this
    /// path actually run, or did every batch fall back? -- is answered once, and
    /// a per-batch log on a sprite-heavy frame would be its own performance
    /// problem. Without it an A/B that merges nothing looks exactly like an A/B
    /// that merges correctly.
    pub(crate) draw_atlas_reported: std::cell::Cell<bool>,
    pub state: Canvas2DState,
    pub stack: StateStack,
    pub path: CanvasPath,
    /// Single-slot `SkPaint` cache keyed by the compact
    /// [`ImagePaintKey`] encoding of the draw-relevant state
    /// (anti-alias flag, blend mode, global alpha quantised to
    /// u8, and a "no visible shadow" bit).  A hit returns a
    /// clone of the cached `Paint`; `skia_safe::Paint` is an
    /// `RCHandle`, so the clone is a refcount bump.
    ///
    /// Target workload: UI bursts that issue hundreds of
    /// `drawImage` / `fillRect` with identical styling.  A
    /// single-slot cache is the simplest thing that collapses
    /// that burst to one real construction; the ~1 bit of
    /// accuracy loss (alpha quantisation) is below display
    /// resolution.
    image_paint_cache: Option<(ImagePaintKey, Paint)>,
}

/// Compact key for [`Canvas2DRenderer::image_paint_cache`].
///
/// Packed to fit in a `u32` so the equality check is a single
/// register compare.  Fields (LSB first):
///
/// * bit 0 : anti-alias flag
/// * bits 1-6 : blend mode discriminant (5 bits is enough for
///   every Skia `BlendMode`; we reserve a sixth for safety)
/// * bits 8-15 : `global_alpha` quantised to u8 (0..=255)
///
/// The remaining bits are zero; reserving them now means adding
/// new inputs (e.g. colour filter presence) later doesn't break
/// callers that compare by value.
#[derive(Copy, Clone, PartialEq, Eq)]
struct ImagePaintKey(u32);

impl ImagePaintKey {
    #[inline]
    fn from_state(state: &Canvas2DState) -> Option<Self> {
        // Shadow filters depend on 5 floats + a colour + offset;
        // caching them here would require a far wider key.  The
        // `effect_cache` module already caches the inner
        // `ImageFilter`, so we opt out of the paint cache entirely
        // when a shadow is visible rather than grow the key.
        if state.shadow.is_visible() {
            return None;
        }
        let aa = if state.antialias { 1u32 } else { 0 };
        let blend = state.blend_mode as u32 & 0x3F;
        let alpha = (state.global_alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u32 & 0xFF;
        Some(ImagePaintKey(aa | (blend << 1) | (alpha << 8)))
    }
}

impl Default for Canvas2DRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas2DRenderer {
    /// Turn sprite-run merging on or off for this renderer.
    ///
    /// The host-side kill switch, and what the pixel-parity tests use to render
    /// a batch both ways.
    pub fn set_draw_atlas(&mut self, enabled: bool) {
        self.draw_atlas = enabled;
    }

    pub fn new() -> Self {
        Self {
            draw_atlas: shared::feature_policy::is_enabled(
                shared::feature_policy::FeatureKey::CanvasDrawAtlas,
            ),
            draw_atlas_reported: std::cell::Cell::new(false),
            state: Canvas2DState::default(),
            stack: StateStack::new(),
            path: CanvasPath::new(),
            image_paint_cache: None,
        }
    }

    /// Look up or build the `SkPaint` used for `drawImage` /
    /// `drawImageBatch` under the current state.  Returns a
    /// refcounted clone of the cached instance on hit, or a
    /// freshly built one (and stores it for the next call) on
    /// miss.
    ///
    /// Workload assumption: UI-heavy pages issue long bursts of
    /// draws with identical paint parameters.  A 1-slot cache
    /// collapses those bursts to a single build; larger caches
    /// don't pay off without also adding eviction machinery.
    #[inline]
    pub(crate) fn acquire_image_paint(&mut self, build: impl FnOnce() -> Paint) -> Paint {
        let Some(key) = ImagePaintKey::from_state(&self.state) else {
            // Shadow path: effect_cache already memoises the inner
            // ImageFilter, so a full rebuild here is cheap and
            // keeps the cache key narrow.
            return build();
        };
        if let Some((cached_key, cached)) = &self.image_paint_cache {
            if *cached_key == key {
                return cached.clone();
            }
        }
        let paint = build();
        self.image_paint_cache = Some((key, paint.clone()));
        paint
    }

    /// Apply one Canvas2D command.  Returns `true` when the command caused
    /// an observable change to the target surface (a draw was issued or
    /// pixels were cleared); `false` for pure state mutation and path
    /// building.  The render thread uses the boolean to decide whether
    /// the canvas requires a present.
    ///
    /// Legacy form for callers that do not execute text commands. No dummy
    /// [`TextContext`] is constructed: non-text dispatch has no dependency on
    /// the font registry or shaping caches.
    pub fn apply<R: PatternResolver>(
        &mut self,
        canvas: &Canvas,
        cmd: &Canvas2DCmd,
        resolver: &R,
    ) -> bool {
        let env = DrawEnv {
            canvas,
            text: None,
            resolver,
        };
        self.apply_env(&env, cmd)
    }

    /// Apply one command against a full [`DrawEnv`].  Required for any
    /// text-related opcode; non-text commands ignore `env.text`.
    pub fn apply_env<R: PatternResolver>(
        &mut self,
        env: &DrawEnv<'_, R>,
        cmd: &Canvas2DCmd,
    ) -> bool {
        let canvas = env.canvas;
        let resolver = env.resolver;
        use Canvas2DCmd::*;
        match cmd {
            // ---- Path building -------------------------------------
            BeginPath => {
                self.path.reset();
                false
            }
            ClosePath => {
                self.path.close_path();
                false
            }
            MoveTo { x, y } => {
                self.path.move_to(*x, *y);
                false
            }
            LineTo { x, y } => {
                self.path.line_to(*x, *y);
                false
            }
            QuadraticCurveTo { cpx, cpy, x, y } => {
                self.path.quadratic_to(*cpx, *cpy, *x, *y);
                false
            }
            BezierCurveTo {
                cp1x,
                cp1y,
                cp2x,
                cp2y,
                x,
                y,
            } => {
                self.path.bezier_to(*cp1x, *cp1y, *cp2x, *cp2y, *x, *y);
                false
            }
            Arc {
                x,
                y,
                radius,
                start_angle,
                end_angle,
                counterclockwise,
            } => {
                self.path
                    .arc(*x, *y, *radius, *start_angle, *end_angle, *counterclockwise);
                false
            }
            ArcTo {
                x1,
                y1,
                x2,
                y2,
                radius,
            } => {
                self.path.arc_to(*x1, *y1, *x2, *y2, *radius);
                false
            }
            Rect { x, y, w, h } => {
                self.path.rect(*x, *y, *w, *h);
                false
            }
            Ellipse {
                x,
                y,
                radius_x,
                radius_y,
                rotation,
                start_angle,
                end_angle,
                counterclockwise,
            } => {
                self.path.ellipse(
                    *x,
                    *y,
                    *radius_x,
                    *radius_y,
                    *rotation,
                    *start_angle,
                    *end_angle,
                    *counterclockwise,
                );
                false
            }

            // ---- Path-based drawing -------------------------------
            Fill => {
                let paint = build_fill_paint(&self.state, resolver);
                let path = self.path.snapshot();
                canvas.draw_path(&path, &paint);
                true
            }
            Stroke => {
                let paint = build_stroke_paint(&self.state, resolver);
                let path = self.path.snapshot();
                canvas.draw_path(&path, &paint);
                true
            }
            Clip => {
                let path = self.path.snapshot();
                canvas.clip_path(&path, ClipOp::Intersect, true);
                false
            }

            // ---- Rectangle primitives -----------------------------
            FillRect { x, y, w, h } => {
                let paint = build_fill_paint(&self.state, resolver);
                canvas.draw_rect(SkRect::from_xywh(*x, *y, *w, *h), &paint);
                true
            }
            StrokeRect { x, y, w, h } => {
                let paint = build_stroke_paint(&self.state, resolver);
                canvas.draw_rect(SkRect::from_xywh(*x, *y, *w, *h), &paint);
                true
            }
            ClearRect { x, y, w, h } => {
                let paint = build_clear_paint();
                canvas.draw_rect(SkRect::from_xywh(*x, *y, *w, *h), &paint);
                true
            }

            // ---- Style setters (pure state) -----------------------
            SetFillStyle { color } => {
                self.state.fill = StyleKind::Color(*color);
                false
            }
            SetStrokeStyle { color } => {
                self.state.stroke = StyleKind::Color(*color);
                false
            }
            SetLineWidth { width } => {
                // Canvas spec: ignore negative/zero/NaN/Inf values.
                if width.is_finite() && *width > 0.0 {
                    self.state.line_width = *width;
                }
                false
            }
            SetLineCap { cap } => {
                self.state.line_cap = match cap {
                    0 => PaintCap::Butt,
                    1 => PaintCap::Round,
                    2 => PaintCap::Square,
                    _ => PaintCap::Butt,
                };
                false
            }
            SetLineJoin { join } => {
                self.state.line_join = match join {
                    0 => PaintJoin::Miter,
                    1 => PaintJoin::Round,
                    2 => PaintJoin::Bevel,
                    _ => PaintJoin::Miter,
                };
                false
            }
            SetMiterLimit { limit } => {
                if limit.is_finite() && *limit > 0.0 {
                    self.state.miter_limit = *limit;
                }
                false
            }
            SetGlobalAlpha { alpha } => {
                if alpha.is_finite() && (0.0..=1.0).contains(alpha) {
                    self.state.global_alpha = *alpha;
                }
                false
            }
            SetCompositeOperation { op } => {
                self.state.blend_mode = blend_mode_from_code(*op);
                false
            }
            SetLineDash { segments } => {
                // Spec: odd-length dash arrays double up to even length.
                //
                // `extend_from_within` rather than `extend_from_slice(&d.clone())`:
                // the clone existed only to dodge the borrow of `d` while
                // extending it, and it bought a whole second vector to do it.
                let doubled = segments.len() % 2 == 1;
                let mut d = Vec::with_capacity(if doubled {
                    segments.len() * 2
                } else {
                    segments.len()
                });
                d.extend_from_slice(segments);
                if doubled {
                    d.extend_from_within(..);
                }
                self.state.line_dash = std::sync::Arc::new(d);
                false
            }
            SetLineDashOffset { offset } => {
                if offset.is_finite() {
                    self.state.line_dash_offset = *offset;
                }
                false
            }
            SetShadowBlur { blur } => {
                if blur.is_finite() && *blur >= 0.0 {
                    self.state.shadow.blur = *blur;
                }
                false
            }
            SetShadowColor { color } => {
                self.state.shadow.color = *color;
                false
            }
            SetShadowOffsetX { offset } => {
                if offset.is_finite() {
                    self.state.shadow.offset_x = *offset;
                }
                false
            }
            SetShadowOffsetY { offset } => {
                if offset.is_finite() {
                    self.state.shadow.offset_y = *offset;
                }
                false
            }
            SetFillStyleGradient {
                gradient_type,
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
                stops,
            } => {
                self.state.fill = StyleKind::from_gradient(
                    *gradient_type,
                    *x0,
                    *y0,
                    *r0,
                    *x1,
                    *y1,
                    *r1,
                    stops.clone(),
                );
                false
            }
            SetStrokeStyleGradient {
                gradient_type,
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
                stops,
            } => {
                self.state.stroke = StyleKind::from_gradient(
                    *gradient_type,
                    *x0,
                    *y0,
                    *r0,
                    *x1,
                    *y1,
                    *r1,
                    stops.clone(),
                );
                false
            }
            SetFillStylePattern {
                image_id,
                repeat_x,
                repeat_y,
            } => {
                self.state.fill = StyleKind::Pattern {
                    image_id: *image_id,
                    repeat_x: *repeat_x,
                    repeat_y: *repeat_y,
                };
                false
            }
            SetStrokeStylePattern {
                image_id,
                repeat_x,
                repeat_y,
            } => {
                self.state.stroke = StyleKind::Pattern {
                    image_id: *image_id,
                    repeat_x: *repeat_x,
                    repeat_y: *repeat_y,
                };
                false
            }
            SetFont { font } => {
                apply_parsed_font(&mut self.state, font);
                false
            }
            SetTextAlign { align } => {
                self.state.text.align = *align;
                false
            }
            SetTextBaseline { baseline } => {
                self.state.text.baseline = *baseline;
                false
            }
            SetTextDirection { direction } => {
                self.state.text.direction = *direction;
                false
            }

            // ---- State stack --------------------------------------
            Save => {
                // Snapshot attribute state AND the SkCanvas (CTM + clip).
                self.stack.push(&self.state);
                canvas.save();
                false
            }
            Restore => {
                // Pop both sides.  Canvas spec: silent no-op when the
                // stack is empty.
                let popped_attrs = self.stack.pop(&mut self.state);
                if popped_attrs {
                    canvas.restore();
                }
                false
            }

            // ---- CTM mutators -------------------------------------
            //
            // Every branch below does TWO things:
            //   1. Update our shadow `state.ctm` so the damage
            //      classifier and partial-damage gate see the same
            //      transform Skia does.
            //   2. Forward the operation to `SkCanvas` for the
            //      actual drawing transform.
            //
            // The shadow and SkCanvas MUST stay in sync; save/restore
            // handles this naturally via `Canvas2DState::clone`.
            SetTransform { a, b, c, d, e, f } => {
                self.state.ctm_set([*a, *b, *c, *d, *e, *f]);
                let m = Matrix::new_all(*a, *c, *e, *b, *d, *f, 0.0, 0.0, 1.0);
                canvas.set_matrix(&skia_safe::M44::from(m));
                false
            }
            ResetTransform => {
                self.state.ctm_reset();
                canvas.reset_matrix();
                false
            }
            Translate { x, y } => {
                // Translate matrix is [1, 0, 0, 1, tx, ty].
                self.state.ctm_concat([1.0, 0.0, 0.0, 1.0, *x, *y]);
                canvas.translate((*x, *y));
                false
            }
            Rotate { angle } => {
                // Rotate matrix is [cos, sin, -sin, cos, 0, 0]; the
                // exact shear values are what the axis-aligned test
                // relies on, so we compute them once here.
                let (s, c_) = (angle.sin(), angle.cos());
                self.state.ctm_concat([c_, s, -s, c_, 0.0, 0.0]);
                canvas.rotate(angle.to_degrees(), None);
                false
            }
            Scale { x, y } => {
                // Scale matrix is [sx, 0, 0, sy, 0, 0]; uniform or
                // mirrored scales keep shear terms at zero so
                // `ctm_is_axis_aligned()` stays true.
                self.state.ctm_concat([*x, 0.0, 0.0, *y, 0.0, 0.0]);
                canvas.scale((*x, *y));
                false
            }

            // ---- Text (stubbed in P4; real impl in P5) ------------
            FillText {
                text,
                x,
                y,
                max_width,
            } => {
                let text_ctx = env
                    .text
                    .expect("FillText routed without the shared TextContext");
                text_ctx.fill_text(canvas, text, *x, *y, *max_width, &self.state, resolver);
                true
            }
            StrokeText {
                text,
                x,
                y,
                max_width,
            } => {
                let text_ctx = env
                    .text
                    .expect("StrokeText routed without the shared TextContext");
                text_ctx.stroke_text(canvas, text, *x, *y, *max_width, &self.state, resolver);
                true
            }
            MeasureText { .. } => {
                // Sync reply variant — routed through `canvas2d_dispatcher`
                // which already handled the `resp` by the time control
                // reaches this backend.  Reaching here means the dispatcher
                // layering invariant is broken; the drop-safe `RenderCmdResp`
                // will still report `ErrorCode::Internal` to the caller
                // when the outer `Canvas2DCmd` is freed, but a warning
                // makes the misrouting visible in logs so the regression
                // is caught.
                tracing::warn!(
                    "Canvas2DCmd::MeasureText reached `apply_env` — dispatcher \
                     layering regressed (expected intercept in canvas2d_dispatcher)"
                );
                false
            }

            // ---- Images (implemented in P4 but needs resolver) ----
            DrawImage { .. } | DrawImageBatch { .. } => {
                // Route through the `PatternResolver` trait or a dedicated
                // image-draw trait in Phase 4b; for now drop so the
                // command stream stays valid.
                false
            }
            GetImageData { .. } => {
                tracing::warn!(
                    "Canvas2DCmd::GetImageData reached `apply_env` — dispatcher \
                     layering regressed"
                );
                false
            }
            CaptureSnapshot { .. } => {
                tracing::warn!(
                    "Canvas2DCmd::CaptureSnapshot reached `apply_env` — dispatcher \
                     layering regressed"
                );
                false
            }
            ReadSnapshotPixels { .. } => {
                tracing::warn!(
                    "Canvas2DCmd::ReadSnapshotPixels reached `apply_env` — dispatcher \
                     layering regressed"
                );
                false
            }

            CreateContext2D => {
                tracing::warn!(
                    "Canvas2DCmd::CreateContext2D reached `apply_env` — \
                     dispatcher layering regressed"
                );
                false
            }

            // `Canvas2DCmd` is `#[non_exhaustive]`, so rustc forces a
            // catch-all.  Keep the arm, but emit a structured warning
            // when hit so forgotten-variant regressions surface in
            // logs instead of silently rendering nothing.
            other => {
                tracing::warn!(
                    "Canvas2DCmd variant not handled in apply_env: {:?}",
                    std::mem::discriminant(other)
                );
                false
            }
        }
    }

    /// Reset the context to fresh defaults.  Used on `resetContext()` and
    /// after onscreen surface recreation.
    pub fn reset(&mut self) {
        self.state = Canvas2DState::default();
        // Context reset is a lifecycle boundary, unlike beginPath(): release
        // path high-water storage instead of retaining a previous workload's
        // pathological allocation for the next context.
        self.stack = StateStack::new();
        self.path.shrink();
    }

    /// Apply a Canvas2D shadow to the given paint if the current shadow is
    /// visible.  Exposed so `fill` / `stroke` paths can opt in at draw
    /// time without paying the cost when shadows are off (the common case).
    pub fn maybe_apply_shadow(_paint: &mut skia_safe::Paint, shadow: &Shadow) -> bool {
        if !shadow.is_visible() {
            return false;
        }
        // TODO(P4b): install a blur + drop-shadow SkImageFilter on `paint`.
        true
    }
}

/// Unused-import silencer for imports that only matter in the stubs above.
fn _force_use_imports() {
    let _ = ProtocolColor::black();
    let _ = TextAlign::Start;
    let _ = TextBaseline::Alphabetic;
    let _ = GradientType::Linear;
}

/// Apply a CSS `font` shorthand to a Canvas2D state.
///
/// Extracted so both the dispatch path and unit tests exercise the
/// identical code: the test seam ensures `SetFont` can never regress
/// to the previous "silently ignored" behaviour.  An unparseable
/// shorthand leaves `state.text` untouched, matching Blink's "invalid
/// font assignment is a no-op" policy.
pub(crate) fn apply_parsed_font(state: &mut Canvas2DState, font: &str) {
    if let Some(parsed) = shared::css_font_shorthand::parse_font_shorthand(font) {
        // Diag: parsed OK.  Logged at trace because SetFont can
        // fire once per UI element per frame in Cocos Creator
        // games; trace keeps the hot path free unless the
        // operator actively asks for it via RUST_LOG.
        tracing::trace!(
            raw = font,
            family = parsed.families.first().map(String::as_str).unwrap_or(""),
            families_len = parsed.families.len(),
            size = parsed.size_px,
            weight = parsed.weight,
            italic = parsed.italic,
            "SetFont parsed"
        );
        state.text.size = parsed.size_px;
        state.text.weight = parsed.weight;
        state.text.italic = parsed.italic;
        state.text.families = std::sync::Arc::new(parsed.families);
    } else {
        // Invalid CSS font shorthand per WHATWG; the state stays
        // at the previous value (browser-equivalent no-op).  We
        // warn *once per distinct source location* because a game
        // that keeps sending the same bad string would otherwise
        // flood logcat — but the first occurrence is worth
        // surfacing because it usually points at a typo or a
        // parser gap we haven't closed yet.
        shared::warn_once!(
            raw = font,
            "SetFont rejected: unparseable CSS font shorthand"
        );
    }
}

#[cfg(test)]
mod set_font_tests {
    use super::*;

    #[test]
    fn apply_parsed_font_updates_size_and_family() {
        let mut state = Canvas2DState::default();
        apply_parsed_font(
            &mut state,
            "italic bold 24px 'Noto Sans CJK SC', sans-serif",
        );
        assert_eq!(state.text.size, 24.0);
        assert_eq!(state.text.weight, 700);
        assert!(state.text.italic);
        assert_eq!(
            &*state.text.families,
            &vec!["Noto Sans CJK SC".to_string(), "sans-serif".to_string()]
        );
    }

    #[test]
    fn apply_parsed_font_preserves_state_on_invalid_input() {
        let mut state = Canvas2DState::default();
        let before = state.text.clone();
        // No size token → invalid per CSS; must be silent no-op.
        apply_parsed_font(&mut state, "bold serif");
        assert_eq!(state.text, before);
    }

    #[test]
    fn apply_parsed_font_handles_pt_units() {
        let mut state = Canvas2DState::default();
        apply_parsed_font(&mut state, "12pt Helvetica");
        // 12pt == 16px at 96dpi
        assert!((state.text.size - 16.0).abs() < 1e-3);
        assert_eq!(&*state.text.families, &vec!["Helvetica".to_string()]);
    }
}
