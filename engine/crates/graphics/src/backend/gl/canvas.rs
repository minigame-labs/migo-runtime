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
use skia_safe::{Canvas, ClipOp, Matrix, Paint, PaintCap, PaintJoin, PathFillType, Rect as SkRect};

use super::blend_mode::blend_mode_from_code;
use super::paint::{
    PatternResolver, build_clear_paint, build_fill_paint, build_stroke_geometry_paint,
    build_stroke_paint,
};
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

/// What the current default path's points are relative to.
///
/// The specification transforms each point by the matrix current when it is added, so a path built under one transform
/// and filled under another fills where it was built. The path is kept in the current user space -- the space Skia draws
/// it in, so the common case costs nothing -- and a change of transform while it holds points is owed to it as one
/// matrix, paid when the path is next added to or used: `fill(); restore(); beginPath()`, the common shape, never pays.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PathSpace {
    /// The current user space.
    User,
    /// The user space of an earlier transform, this matrix to canvas coordinates.
    Earlier(Matrix),
    /// Canvas coordinates: the transform is singular, so there is no user space to keep the path in.
    Canvas,
}

/// A `Path2D`'s segments built into a path, under a fill rule.
fn path2d(segments: &[u32], even_odd: bool) -> skia_safe::Path {
    let mut built = CanvasPath::new();
    built.append_segments(segments);
    built.snapshot().with_fill_type(if even_odd {
        PathFillType::EvenOdd
    } else {
        PathFillType::Winding
    })
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
    /// What the current default path's points are relative to (see [`PathSpace`]).
    path_space: PathSpace,
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
            path_space: PathSpace::User,
            image_paint_cache: None,
        }
    }

    /// The current transformation matrix as Skia's.
    fn ctm(&self) -> Matrix {
        let [a, b, c, d, e, f] = self.state.ctm;
        Matrix::new_all(a, c, e, b, d, f, 0.0, 0.0, 1.0)
    }

    /// The transform is about to change: a path that holds points stays where they are, in the space of the transform
    /// they were added under, until it is next used (`settle_path`).
    fn transform_changing(&mut self) {
        if self.path_space == PathSpace::User && self.path.verb_count() > 0 {
            self.path_space = PathSpace::Earlier(self.ctm());
        }
    }

    /// The transform changed: back to the one the path's points are in -- a `restore()` after a `save()` and a
    /// `translate()` -- leaves nothing owed.
    fn transform_changed(&mut self) {
        if self.path_space == PathSpace::Earlier(self.ctm()) {
            self.path_space = PathSpace::User;
        }
    }

    /// Bring the current default path into the current user space, which is where Skia draws it, or into canvas
    /// coordinates when the transform cannot be inverted.
    fn settle_path(&mut self) {
        let ctm = self.ctm();
        match self.path_space {
            PathSpace::User => {}
            PathSpace::Earlier(earlier) => match ctm.invert() {
                Some(inverse) => {
                    self.path.transform(&Matrix::concat(&inverse, &earlier));
                    self.path_space = PathSpace::User;
                }
                None => {
                    self.path.transform(&earlier);
                    self.path_space = PathSpace::Canvas;
                }
            },
            PathSpace::Canvas => {
                if let Some(inverse) = ctm.invert() {
                    self.path.transform(&inverse);
                    self.path_space = PathSpace::User;
                }
            }
        }
    }

    /// Add to the current default path: each point through the transform current now, as the specification has it.
    /// `continues` says whether the segment goes on from the current point (a line, a curve, an arc) rather than starting
    /// subpaths of its own (`moveTo`, `rect`, `roundRect`).
    fn build_path(&mut self, continues: bool, add: impl FnOnce(&mut CanvasPath)) {
        self.settle_path();
        if self.path_space == PathSpace::Canvas {
            // The transform is singular, so there is no user space to add in: the segment is made in user space and
            // moved to canvas coordinates, where the path is kept until the transform can be inverted again.
            let mut segment = CanvasPath::new();
            add(&mut segment);
            let ctm = self.ctm();
            self.path.append_transformed(&segment, &ctm, continues);
        } else {
            add(&mut self.path);
        }
    }

    /// Ready the current default path to be drawn or clipped with, in the current user space; false when the transform
    /// is singular, under which the path draws nothing and clips nothing.
    fn path_for_use(&mut self) -> bool {
        self.settle_path();
        self.path_space == PathSpace::User
    }

    /// `isPointInPath` / `isPointInStroke`: whether `(x, y)` -- canvas coordinates, not through the transform -- is
    /// inside the path, under `even_odd` or nonzero, or inside the outline of its stroke under the current line styles.
    /// The path is the current default path, or `segments` -- a `Path2D` -- drawn through the transform. Points on an edge
    /// count as inside. A singular transform answers false, as there is no path to be inside of.
    pub fn hit_test(
        &mut self,
        segments: Option<&[u32]>,
        x: f32,
        y: f32,
        stroke: bool,
        even_odd: bool,
    ) -> bool {
        if !x.is_finite() || !y.is_finite() {
            return false;
        }
        let ctm = self.ctm();
        if ctm.invert().is_none() {
            return false;
        }
        let path = match segments {
            Some(words) => {
                let mut built = CanvasPath::new();
                built.append_segments(words);
                built.snapshot()
            }
            None => {
                if !self.path_for_use() {
                    return false;
                }
                self.path.snapshot()
            }
        };
        let mut area = if stroke {
            // The stroke is traced in user space -- the line width is in user units -- and then moved to the canvas,
            // as it is drawn. `ctm` sets how finely its curves are cut.
            let mut outline = skia_safe::Path::default();
            if !skia_safe::path_utils::fill_path_with_paint(
                &path,
                &build_stroke_geometry_paint(&self.state),
                &mut outline,
                None,
                ctm,
            ) {
                return false;
            }
            outline
        } else {
            path.with_fill_type(if even_odd {
                PathFillType::EvenOdd
            } else {
                PathFillType::Winding
            })
        };
        area = area.with_transform(&ctm);
        area.contains((x, y))
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
                self.path_space = PathSpace::User;
                false
            }
            ClosePath => {
                self.path.close_path();
                false
            }
            MoveTo { x, y } => {
                self.build_path(false, |path| path.move_to(*x, *y));
                false
            }
            LineTo { x, y } => {
                self.build_path(true, |path| path.line_to(*x, *y));
                false
            }
            QuadraticCurveTo { cpx, cpy, x, y } => {
                self.build_path(true, |path| path.quadratic_to(*cpx, *cpy, *x, *y));
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
                self.build_path(true, |path| {
                    path.bezier_to(*cp1x, *cp1y, *cp2x, *cp2y, *x, *y)
                });
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
                self.build_path(true, |path| {
                    path.arc(*x, *y, *radius, *start_angle, *end_angle, *counterclockwise)
                });
                false
            }
            ArcTo {
                x1,
                y1,
                x2,
                y2,
                radius,
            } => {
                self.build_path(true, |path| path.arc_to(*x1, *y1, *x2, *y2, *radius));
                false
            }
            Rect { x, y, w, h } => {
                self.build_path(false, |path| path.rect(*x, *y, *w, *h));
                false
            }
            RoundRect { x, y, w, h, radii } => {
                self.build_path(false, |path| path.round_rect(*x, *y, *w, *h, *radii));
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
                self.build_path(true, |path| {
                    path.ellipse(
                        *x,
                        *y,
                        *radius_x,
                        *radius_y,
                        *rotation,
                        *start_angle,
                        *end_angle,
                        *counterclockwise,
                    )
                });
                false
            }

            // ---- Path-based drawing -------------------------------
            Fill | FillEvenOdd | Stroke | Clip | ClipEvenOdd if !self.path_for_use() => false,
            Fill => {
                let paint = build_fill_paint(&self.state, resolver);
                let path = self.path.snapshot();
                canvas.draw_path(&path, &paint);
                true
            }
            // The same path under the other winding rule: the holes of an even-odd icon stay holes.
            FillEvenOdd => {
                let paint = build_fill_paint(&self.state, resolver);
                let path = self.path.snapshot().with_fill_type(PathFillType::EvenOdd);
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
            ClipEvenOdd => {
                let path = self.path.snapshot().with_fill_type(PathFillType::EvenOdd);
                canvas.clip_path(&path, ClipOp::Intersect, true);
                false
            }
            // A `Path2D`: its own coordinates, drawn through the transform; the current default path is not touched. A
            // singular transform draws and clips nothing.
            FillPath { .. } | StrokePath { .. } | ClipPath { .. }
                if self.ctm().invert().is_none() =>
            {
                false
            }
            FillPath { path, even_odd } => {
                let paint = build_fill_paint(&self.state, resolver);
                let path = path2d(path, *even_odd);
                canvas.draw_path(&path, &paint);
                true
            }
            StrokePath { path } => {
                let paint = build_stroke_paint(&self.state, resolver);
                canvas.draw_path(&path2d(path, false), &paint);
                true
            }
            ClipPath { path, even_odd } => {
                canvas.clip_path(&path2d(path, *even_odd), ClipOp::Intersect, true);
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
            // Read by every `drawImage` through `Canvas2DState::image_sampling_options`, and saved and
            // restored with the rest of the state because it is a field of it.
            SetImageSmoothing { enabled } => {
                self.state.image_smoothing = *enabled;
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
                self.transform_changing();
                let popped_attrs = self.stack.pop(&mut self.state);
                if popped_attrs {
                    canvas.restore();
                }
                self.transform_changed();
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
                self.transform_changing();
                self.state.ctm_set([*a, *b, *c, *d, *e, *f]);
                let m = Matrix::new_all(*a, *c, *e, *b, *d, *f, 0.0, 0.0, 1.0);
                canvas.set_matrix(&skia_safe::M44::from(m));
                self.transform_changed();
                false
            }
            ResetTransform => {
                self.transform_changing();
                self.state.ctm_reset();
                canvas.reset_matrix();
                self.transform_changed();
                false
            }
            Translate { x, y } => {
                // Translate matrix is [1, 0, 0, 1, tx, ty].
                self.transform_changing();
                self.state.ctm_concat([1.0, 0.0, 0.0, 1.0, *x, *y]);
                canvas.translate((*x, *y));
                self.transform_changed();
                false
            }
            Rotate { angle } => {
                // Rotate matrix is [cos, sin, -sin, cos, 0, 0]; the
                // exact shear values are what the axis-aligned test
                // relies on, so we compute them once here.
                let (s, c_) = (angle.sin(), angle.cos());
                self.transform_changing();
                self.state.ctm_concat([c_, s, -s, c_, 0.0, 0.0]);
                canvas.rotate(angle.to_degrees(), None);
                self.transform_changed();
                false
            }
            Scale { x, y } => {
                // Scale matrix is [sx, 0, 0, sy, 0, 0]; uniform or
                // mirrored scales keep shear terms at zero so
                // `ctm_is_axis_aligned()` stays true.
                self.transform_changing();
                self.state.ctm_concat([*x, 0.0, 0.0, *y, 0.0, 0.0]);
                canvas.scale((*x, *y));
                self.transform_changed();
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
            HitTest { .. } => {
                // A reply variant, like `MeasureText` below: the dispatcher answers it (`hit_test`).
                tracing::warn!(
                    "Canvas2DCmd::HitTest reached `apply_env` -- dispatcher layering regressed \
                     (expected intercept in canvas2d_dispatcher)"
                );
                false
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
            // `putImageData` replaces the bitmap's pixels: Skia's `writePixels` ignores the matrix, the clip and the
            // paint (so `globalAlpha`, the composite operation and the shadow), which is what the specification asks
            // of it, and converts from the unpremultiplied RGBA `ImageData` holds to what the surface stores.
            PutImageData {
                x,
                y,
                width,
                height,
                pixels,
            } => {
                let (Ok(w), Ok(h)) = (i32::try_from(*width), i32::try_from(*height)) else {
                    return false;
                };
                if w == 0 || h == 0 {
                    return false;
                }
                let info = skia_safe::ImageInfo::new(
                    (w, h),
                    skia_safe::ColorType::RGBA8888,
                    skia_safe::AlphaType::Unpremul,
                    None,
                );
                canvas.write_pixels(&info, pixels, *width as usize * 4, (*x, *y))
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
            CaptureImage { .. } => {
                tracing::warn!(
                    "Canvas2DCmd::CaptureImage reached `apply_env` — dispatcher \
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
        self.path_space = PathSpace::User;
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

#[cfg(test)]
mod put_image_data_tests {
    use super::*;
    use crate::backend::gl::paint::NullPatternResolver;

    fn read(surface: &mut skia_safe::Surface, x: i32, y: i32) -> [u8; 4] {
        let info = skia_safe::ImageInfo::new(
            (1, 1),
            skia_safe::ColorType::RGBA8888,
            skia_safe::AlphaType::Unpremul,
            None,
        );
        let mut px = [0u8; 4];
        assert!(surface.read_pixels(&info, &mut px, 4, (x, y)));
        px
    }

    fn apply(renderer: &mut Canvas2DRenderer, surface: &mut skia_safe::Surface, cmd: Canvas2DCmd) {
        let env = DrawEnv {
            canvas: surface.canvas(),
            text: None,
            resolver: &NullPatternResolver,
        };
        renderer.apply_env(&env, &cmd);
    }

    /// `putImageData` replaces pixels: whatever the drawing state is -- a transform, a clip, `globalAlpha`, a composite
    /// operation -- it neither moves, clips, fades nor combines the pixels it writes (HTML Standard, "putImageData").
    #[test]
    fn put_image_data_ignores_the_drawing_state() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((16, 16)).unwrap();
        let mut renderer = Canvas2DRenderer::new();
        // Paint the whole canvas blue, then make every part of the state hostile.
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::SetFillStyle {
                color: ProtocolColor::rgb(0, 0, 255),
            },
        );
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::FillRect {
                x: 0.0,
                y: 0.0,
                w: 16.0,
                h: 16.0,
            },
        );
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::Translate { x: 5.0, y: 5.0 },
        );
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::Scale { x: 3.0, y: 3.0 },
        );
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::SetGlobalAlpha { alpha: 0.25 },
        );
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::SetCompositeOperation { op: 5 }, // destination-in: it would erase most of what is under the write
        );
        apply(&mut renderer, &mut surface, Canvas2DCmd::BeginPath);
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
        );
        apply(&mut renderer, &mut surface, Canvas2DCmd::Clip);

        // Two pixels: opaque red, and half-transparent green (unpremultiplied bytes, as ImageData holds them).
        let pixels = vec![255, 0, 0, 255, 0, 255, 0, 128];
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::PutImageData {
                x: 2,
                y: 3,
                width: 2,
                height: 1,
                pixels,
            },
        );
        assert_eq!(
            read(&mut surface, 2, 3),
            [255, 0, 0, 255],
            "opaque, in place, unclipped"
        );
        let green = read(&mut surface, 3, 3);
        assert!(
            green[0] == 0 && green[1] >= 254 && green[2] == 0 && (127..=129).contains(&green[3]),
            "half-transparent green replaces the blue (it is not blended with it): {green:?}"
        );
        assert_eq!(
            read(&mut surface, 4, 3),
            [0, 0, 255, 255],
            "next to the write nothing changed"
        );
    }

    /// A rectangle that hangs over the canvas's edge writes the part that is inside, and one wholly outside writes nothing.
    #[test]
    fn put_image_data_is_clipped_to_the_canvas_edges() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((4, 4)).unwrap();
        let mut renderer = Canvas2DRenderer::new();
        let pixels: Vec<u8> = (0..16).flat_map(|i| [i as u8 * 10, 0, 0, 255]).collect();
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::PutImageData {
                x: -2,
                y: -2,
                width: 4,
                height: 4,
                pixels,
            },
        );
        // pixel (2,2) of the image (index 10) lands at (0,0)
        assert_eq!(read(&mut surface, 0, 0), [100, 0, 0, 255]);
        assert_eq!(read(&mut surface, 1, 1), [150, 0, 0, 255]);
        assert_eq!(
            read(&mut surface, 2, 2),
            [0, 0, 0, 0],
            "past the image: untouched"
        );
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::PutImageData {
                x: 40,
                y: 40,
                width: 1,
                height: 1,
                pixels: vec![1, 2, 3, 255],
            },
        );
        assert_eq!(
            read(&mut surface, 3, 3),
            [0, 0, 0, 0],
            "wholly outside: nothing"
        );
    }

    /// A buffer that is not `width * height * 4` bytes writes nothing rather than something shifted.
    #[test]
    fn put_image_data_with_a_short_buffer_writes_nothing() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((4, 4)).unwrap();
        let mut renderer = Canvas2DRenderer::new();
        apply(
            &mut renderer,
            &mut surface,
            Canvas2DCmd::PutImageData {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
                pixels: vec![9; 8],
            },
        );
        assert_eq!(read(&mut surface, 0, 0), [0, 0, 0, 0]);
    }
}

#[cfg(test)]
mod path_semantics_tests {
    use super::*;
    use crate::backend::gl::paint::NullPatternResolver;
    use frame_wire::canvas2d::path2d;

    fn apply(renderer: &mut Canvas2DRenderer, surface: &mut skia_safe::Surface, cmd: Canvas2DCmd) {
        let env = DrawEnv {
            canvas: surface.canvas(),
            text: None,
            resolver: &NullPatternResolver,
        };
        renderer.apply_env(&env, &cmd);
    }

    fn run(cmds: Vec<Canvas2DCmd>) -> (Canvas2DRenderer, skia_safe::Surface) {
        let mut surface = skia_safe::surfaces::raster_n32_premul((200, 200)).unwrap();
        let mut renderer = Canvas2DRenderer::new();
        for cmd in cmds {
            apply(&mut renderer, &mut surface, cmd);
        }
        (renderer, surface)
    }

    fn alpha(surface: &mut skia_safe::Surface, x: i32, y: i32) -> u8 {
        let info = skia_safe::ImageInfo::new(
            (1, 1),
            skia_safe::ColorType::RGBA8888,
            skia_safe::AlphaType::Unpremul,
            None,
        );
        let mut px = [0u8; 4];
        assert!(surface.read_pixels(&info, &mut px, 4, (x, y)));
        px[3]
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Canvas2DCmd {
        Canvas2DCmd::Rect { x, y, w, h }
    }

    /// Segments of a `Path2D`, as the facade encodes them: floats as their bits, flags as 0 or 1.
    fn segments(ops: &[(u32, &[f32])]) -> Vec<u32> {
        let mut words = Vec::new();
        for (op, args) in ops {
            words.push(*op);
            let flags = path2d::arguments(*op).map_or(&[][..], |(_, flags)| flags);
            words.extend(args.iter().enumerate().map(|(i, a)| {
                if flags.contains(&i) {
                    *a as u32
                } else {
                    a.to_bits()
                }
            }));
        }
        assert!(path2d::is_valid(&words));
        words
    }

    /// Each point of the current default path goes through the transform current when it was added: a rectangle added
    /// at the origin and filled after a translate fills at the origin.
    #[test]
    fn a_path_stays_where_the_transform_was_when_it_was_built() {
        let (mut renderer, mut surface) = run(vec![
            Canvas2DCmd::BeginPath,
            rect(0.0, 0.0, 10.0, 10.0),
            Canvas2DCmd::Translate { x: 50.0, y: 0.0 },
            rect(0.0, 0.0, 10.0, 10.0),
            Canvas2DCmd::Fill,
        ]);
        assert_eq!(
            alpha(&mut surface, 5, 5),
            255,
            "the first rectangle, built at the origin"
        );
        assert_eq!(
            alpha(&mut surface, 55, 5),
            255,
            "the second, built after the translate"
        );
        assert_eq!(alpha(&mut surface, 105, 5), 0, "nothing translated twice");
        assert!(renderer.hit_test(None, 5.0, 5.0, false, false));
        assert!(renderer.hit_test(None, 55.0, 5.0, false, false));
        assert!(!renderer.hit_test(None, 105.0, 5.0, false, false));
    }

    /// A transform that comes back before the path is used owes it nothing, and one that does not is paid once.
    #[test]
    fn a_save_and_restore_around_a_path_costs_it_nothing() {
        let (mut renderer, _) = run(vec![
            Canvas2DCmd::BeginPath,
            rect(10.0, 10.0, 10.0, 10.0),
            Canvas2DCmd::Save,
            Canvas2DCmd::Scale { x: 3.0, y: 3.0 },
            Canvas2DCmd::Restore,
        ]);
        assert_eq!(renderer.path_space, PathSpace::User);
        assert!(renderer.hit_test(None, 15.0, 15.0, false, false));
        let (mut renderer, _) = run(vec![
            Canvas2DCmd::BeginPath,
            rect(10.0, 10.0, 10.0, 10.0),
            Canvas2DCmd::Scale { x: 3.0, y: 3.0 },
        ]);
        assert!(matches!(renderer.path_space, PathSpace::Earlier(_)));
        assert!(
            renderer.hit_test(None, 15.0, 15.0, false, false),
            "still where it was built"
        );
        assert!(!renderer.hit_test(None, 45.0, 45.0, false, false));
        assert_eq!(renderer.path_space, PathSpace::User, "paid when used");
    }

    /// Under a singular transform there is no path to be inside of, and nothing is drawn; the points added meanwhile
    /// are kept where the transform put them, and the path is usable again once the transform is.
    #[test]
    fn a_singular_transform_draws_and_hits_nothing() {
        let (mut renderer, mut surface) = run(vec![
            Canvas2DCmd::BeginPath,
            rect(0.0, 0.0, 20.0, 20.0),
            Canvas2DCmd::Scale { x: 0.0, y: 0.0 },
            Canvas2DCmd::Fill,
        ]);
        assert_eq!(alpha(&mut surface, 5, 5), 0);
        assert!(!renderer.hit_test(None, 5.0, 5.0, false, false));
        apply(&mut renderer, &mut surface, Canvas2DCmd::ResetTransform);
        assert!(
            renderer.hit_test(None, 5.0, 5.0, false, false),
            "the rectangle, still where it was built"
        );
    }

    /// A point on an edge is inside; the even-odd rule leaves the hole of two nested rectangles empty, nonzero fills it.
    #[test]
    fn hit_tests_take_edges_and_the_fill_rule() {
        let (mut renderer, _) = run(vec![
            Canvas2DCmd::BeginPath,
            rect(0.0, 0.0, 100.0, 100.0),
            rect(25.0, 25.0, 50.0, 50.0),
        ]);
        assert!(
            renderer.hit_test(None, 0.0, 50.0, false, false),
            "the left edge"
        );
        assert!(
            renderer.hit_test(None, 50.0, 50.0, false, false),
            "nonzero: the hole is filled"
        );
        assert!(
            !renderer.hit_test(None, 50.0, 50.0, false, true),
            "even-odd: the hole is empty"
        );
        assert!(renderer.hit_test(None, 10.0, 50.0, false, true));
        assert!(!renderer.hit_test(None, f32::NAN, 50.0, false, false));
    }

    /// A stroke's outline is the line styles' -- width, caps -- traced in user space and then transformed.
    #[test]
    fn a_stroke_is_hit_by_its_outline_under_the_line_styles() {
        let (mut renderer, _) = run(vec![
            Canvas2DCmd::SetLineWidth { width: 10.0 },
            Canvas2DCmd::BeginPath,
            Canvas2DCmd::MoveTo { x: 20.0, y: 50.0 },
            Canvas2DCmd::LineTo { x: 120.0, y: 50.0 },
        ]);
        assert!(renderer.hit_test(None, 70.0, 54.0, true, false));
        assert!(!renderer.hit_test(None, 70.0, 56.0, true, false));
        assert!(
            !renderer.hit_test(None, 17.0, 50.0, true, false),
            "butt caps end at the end"
        );
        let (mut renderer, _) = run(vec![
            Canvas2DCmd::SetLineWidth { width: 10.0 },
            Canvas2DCmd::SetLineCap { cap: 2 },
            Canvas2DCmd::Scale { x: 2.0, y: 2.0 },
            Canvas2DCmd::BeginPath,
            Canvas2DCmd::MoveTo { x: 20.0, y: 50.0 },
            Canvas2DCmd::LineTo { x: 60.0, y: 50.0 },
        ]);
        assert!(
            renderer.hit_test(None, 80.0, 109.0, true, false),
            "a width of 10 under a scale of 2 is 20"
        );
        assert!(
            renderer.hit_test(None, 32.0, 100.0, true, false),
            "square caps reach past the end"
        );
    }

    /// A `Path2D` is in its own coordinates and drawn through the transform current when it is used; the current default
    /// path is not touched.
    #[test]
    fn a_path2d_is_drawn_through_the_transform_of_its_use() {
        let square = segments(&[(path2d::RECT, &[0.0, 0.0, 10.0, 10.0])]);
        let (mut renderer, mut surface) = run(vec![
            Canvas2DCmd::BeginPath,
            rect(150.0, 150.0, 10.0, 10.0),
            Canvas2DCmd::Translate { x: 100.0, y: 0.0 },
            Canvas2DCmd::FillPath {
                path: square.clone(),
                even_odd: false,
            },
        ]);
        assert_eq!(alpha(&mut surface, 105, 5), 255);
        assert_eq!(alpha(&mut surface, 5, 5), 0);
        assert!(renderer.hit_test(Some(&square), 105.0, 5.0, false, false));
        assert!(!renderer.hit_test(Some(&square), 5.0, 5.0, false, false));
        assert!(
            renderer.hit_test(None, 155.0, 155.0, false, false),
            "the default path is where it was"
        );
    }

    /// `roundRect` cuts its corners, scales radii that would overlap, and mirrors a negative size without moving it.
    #[test]
    fn round_rects_cut_corners_and_mirror() {
        let radii = [20.0; 8];
        let (mut renderer, _) = run(vec![
            Canvas2DCmd::BeginPath,
            Canvas2DCmd::RoundRect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
                radii,
            },
        ]);
        assert!(
            !renderer.hit_test(None, 2.0, 2.0, false, false),
            "the corner is cut"
        );
        assert!(renderer.hit_test(None, 50.0, 2.0, false, false));
        let (mut renderer, _) = run(vec![
            Canvas2DCmd::BeginPath,
            Canvas2DCmd::RoundRect {
                x: 100.0,
                y: 0.0,
                w: -100.0,
                h: 100.0,
                radii: [80.0; 8],
            },
        ]);
        assert!(
            renderer.hit_test(None, 50.0, 50.0, false, false),
            "a negative width spans to the left"
        );
        assert!(
            !renderer.hit_test(None, 1.0, 1.0, false, false),
            "radii of 80 are scaled to 50 and meet"
        );
    }

    /// An SVG arc and `addPath` with a transform, as a `Path2D` built from a path string and another path has them.
    #[test]
    fn svg_arcs_and_added_paths_build() {
        let half_disc = segments(&[
            (path2d::MOVE_TO, &[0.0, 50.0]),
            (
                path2d::SVG_ARC_TO,
                &[50.0, 50.0, 0.0, 0.0, 1.0, 100.0, 50.0],
            ),
            (path2d::CLOSE_PATH, &[]),
        ]);
        let (mut renderer, _) = run(vec![]);
        assert!(
            renderer.hit_test(Some(&half_disc), 50.0, 20.0, false, false),
            "sweep 1 bows upwards"
        );
        assert!(!renderer.hit_test(Some(&half_disc), 50.0, 80.0, false, false));
        let mut added = vec![path2d::ADD_PATH];
        added.extend(
            [1.0f32, 0.0, 0.0, 1.0, 0.0, 100.0]
                .iter()
                .map(|v| v.to_bits()),
        );
        added.push(half_disc.len() as u32);
        added.extend(&half_disc);
        assert!(path2d::is_valid(&added));
        assert!(
            renderer.hit_test(Some(&added), 50.0, 120.0, false, false),
            "moved down by its transform"
        );
        assert!(!renderer.hit_test(Some(&added), 50.0, 20.0, false, false));
    }

    /// After `addPath` the path goes on from the added path's last point, in its last subpath, as every engine has it: a
    /// line drawn next extends an open subpath into a bigger shape, and after a closed one starts a subpath at its start.
    #[test]
    fn the_next_segment_after_an_added_path_goes_on_from_its_last_point() {
        let elbow = segments(&[
            (path2d::MOVE_TO, &[0.0, 0.0]),
            (path2d::LINE_TO, &[100.0, 0.0]),
            (path2d::LINE_TO, &[100.0, 100.0]),
        ]);
        let mut words = vec![path2d::ADD_PATH];
        words.extend(
            [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0]
                .iter()
                .map(|v| v.to_bits()),
        );
        words.push(elbow.len() as u32);
        words.extend(&elbow);
        words.extend(segments(&[(path2d::LINE_TO, &[0.0, 100.0])]));
        let (mut renderer, _) = run(vec![]);
        assert!(
            renderer.hit_test(Some(&words), 90.0, 50.0, false, false),
            "the added triangle"
        );
        assert!(
            renderer.hit_test(Some(&words), 10.0, 50.0, false, false),
            "the line after it extends the added subpath to a square's fourth corner"
        );

        let square = segments(&[(path2d::RECT, &[0.0, 0.0, 50.0, 50.0])]);
        let mut words = vec![path2d::ADD_PATH];
        words.extend(
            [1.0f32, 0.0, 0.0, 1.0, 100.0, 100.0]
                .iter()
                .map(|v| v.to_bits()),
        );
        words.push(square.len() as u32);
        words.extend(&square);
        words.extend(segments(&[
            (path2d::LINE_TO, &[200.0, 100.0]),
            (path2d::LINE_TO, &[200.0, 0.0]),
        ]));
        assert!(
            renderer.hit_test(Some(&words), 110.0, 95.0, false, false),
            "after a closed subpath the next one starts at its first point, (100, 100) under the transform -- not at its \
             last, (100, 150)"
        );
    }

    /// An ellipse's arc continues the subpath the line to its start is in, so a pie closes through the centre.
    #[test]
    fn an_ellipse_continues_its_subpath() {
        let (mut renderer, _) = run(vec![
            Canvas2DCmd::BeginPath,
            Canvas2DCmd::MoveTo { x: 50.0, y: 50.0 },
            Canvas2DCmd::Ellipse {
                x: 50.0,
                y: 50.0,
                radius_x: 40.0,
                radius_y: 40.0,
                rotation: 0.0,
                start_angle: 0.0,
                end_angle: std::f32::consts::FRAC_PI_2,
                counterclockwise: false,
            },
            Canvas2DCmd::ClosePath,
        ]);
        assert!(
            renderer.hit_test(None, 60.0, 60.0, false, false),
            "inside the pie, on the centre's side of the chord"
        );
    }
}
