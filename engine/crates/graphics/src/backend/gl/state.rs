//! Canvas2D drawing state (Skia-native, GL-free).
//!
//! Mirrors the [WHATWG Canvas 2D §2.4 drawing state][spec] so that a single
//! `Canvas2DState` value captures everything `SkPaint` / `SkCanvas` needs to
//! honour `save()` / `restore()`, minus the bits Skia tracks on its own:
//!
//!   * **CTM** (current transformation matrix) — maintained by `SkCanvas`
//!     via `save()`/`restore()`/`concat()` / `translate()` / `scale()` etc.
//!   * **Clip region** — same, `SkCanvas::clip_*()` rides the save stack.
//!
//! The state struct is therefore a pure Rust value object with a tiny
//! save/restore stack.  It is unit-testable without a GPU context — this
//! module is exercised heavily by `backend::gl::state::tests` below.
//!
//! ## Design notes
//!
//! * **Styles are lazily resolved.**  `FillStyleKind` / `StrokeStyleKind`
//!   keep the high-level description (colour / gradient stops / pattern
//!   image handle); the `SkPaint` / `SkShader` is built at draw time.  This
//!   matches Chrome/Blink and keeps `save()` cheap (no `Shader::clone()`).
//! * **globalAlpha** is *not* folded into `FillStyleKind::Color` here;
//!   modulation happens at paint-build time via
//!   [`crate::backend::gl::color::to_sk_color4f_modulated`].
//! * **Shadow** is represented as a `Shadow` struct; an `ImageFilter` is
//!   built lazily by the handler when a draw call sees a non-trivial shadow.
//! * The [Canvas 2D spec default drawing state][spec-defaults] is encoded
//!   in `Default for Canvas2DState`.  Tests below pin every default down so
//!   regressions are caught by CI, not by a user on device.
//!
//! [spec]: https://html.spec.whatwg.org/multipage/canvas.html#the-canvas-state
//! [spec-defaults]: https://html.spec.whatwg.org/multipage/canvas.html#reset-the-rendering-context-to-its-default-state

use std::sync::Arc;

use shared::protocol::color::Color;
use shared::protocol::render_cmd::{
    GradientStop, GradientType, TextAlign, TextBaseline, TextDirection,
};
use skia_safe::{BlendMode, FilterMode, MipmapMode, PaintCap, PaintJoin, SamplingOptions};

/// Fill (or stroke) style referenced by a [`Canvas2DState`].
///
/// See WHATWG Canvas 2D §3 "fillStyle".  Pattern entries carry the image
/// registry id only; the `SkImage` is resolved by the handler at draw time
/// so this enum stays GL-free.
#[derive(Debug, Clone, PartialEq)]
pub enum StyleKind {
    Color(Color),
    LinearGradient {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stops: Arc<Vec<GradientStop>>,
    },
    RadialGradient {
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
        stops: Arc<Vec<GradientStop>>,
    },
    ConicGradient {
        cx: f32,
        cy: f32,
        start_angle: f32,
        stops: Arc<Vec<GradientStop>>,
    },
    Pattern {
        image_id: u32,
        repeat_x: bool,
        repeat_y: bool,
    },
}

impl StyleKind {
    /// Build a [`StyleKind`] from the shared protocol's gradient shape +
    /// colour stops.  Extracted because both fill and stroke variants go
    /// through the exact same decode path.
    pub fn from_gradient(
        kind: GradientType,
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        _r1: f32,
        stops: Vec<GradientStop>,
    ) -> Self {
        let stops = Arc::new(stops);
        match kind {
            GradientType::Linear => StyleKind::LinearGradient {
                x0,
                y0,
                x1,
                y1,
                stops,
            },
            GradientType::Radial => StyleKind::RadialGradient {
                x0,
                y0,
                r0,
                x1,
                y1,
                r1: _r1,
                stops,
            },
            GradientType::Conic => StyleKind::ConicGradient {
                cx: x0,
                cy: y0,
                start_angle: x1,
                stops,
            },
        }
    }
}

/// Shadow parameters, matching the four shadow attributes in Canvas 2D §4
/// "drawing images / shapes".  Stored as-is; the handler converts to a
/// `SkImageFilter::blur` at draw time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub blur: f32,
    pub color: Color,
    pub offset_x: f32,
    pub offset_y: f32,
}

impl Shadow {
    /// Canvas 2D spec defaults: fully transparent black, zero blur/offset —
    /// i.e. draws have no shadow unless explicitly enabled.
    #[inline]
    pub const fn none() -> Self {
        Self {
            blur: 0.0,
            color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }

    /// Returns `true` if the shadow is visible for the current draw.
    ///
    /// Per spec the shadow is drawn only when the shadow colour is non-
    /// transparent **and** at least one of `blur`, `offset_x`, `offset_y`
    /// is non-zero.  Zero-offset zero-blur shadows are spec'd as no-ops
    /// even if the colour is opaque — matches Chrome.
    #[inline]
    pub fn is_visible(&self) -> bool {
        let has_color = self.color.a > 0.0;
        let has_extent = self.blur > 0.0 || self.offset_x != 0.0 || self.offset_y != 0.0;
        has_color && has_extent
    }
}

/// Text-layout attributes (font + align + baseline).
///
/// Typeface resolution is handled separately by the font manager; this just
/// carries the *descriptor* so the state can be snapshotted cheaply.
#[derive(Debug, Clone, PartialEq)]
pub struct TextAttrs {
    /// `font-size` in CSS px.
    pub size: f32,
    /// Parsed family list, head-first (e.g. `["Helvetica", "sans-serif"]`).
    pub families: Arc<Vec<String>>,
    /// CSS `font-weight`, 1..=1000 (400 = normal, 700 = bold).
    pub weight: u16,
    /// CSS `font-style: italic | oblique`.
    pub italic: bool,
    pub align: TextAlign,
    pub baseline: TextBaseline,
    /// BiDi reorder direction.  Defaults to `Inherit`, which the text
    /// pipeline resolves to `Ltr` (the engine has no parent box).
    pub direction: TextDirection,
}

impl Default for TextAttrs {
    /// Canvas 2D default: `10px sans-serif`, `start` align, `alphabetic`
    /// baseline, normal weight, upright, inherit direction.
    fn default() -> Self {
        Self {
            size: 10.0,
            families: Arc::new(vec!["sans-serif".to_string()]),
            weight: 400,
            italic: false,
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            direction: TextDirection::Inherit,
        }
    }
}

/// Full drawing state for one 2D context.  See module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct Canvas2DState {
    pub fill: StyleKind,
    pub stroke: StyleKind,

    pub line_width: f32,
    pub line_cap: PaintCap,
    pub line_join: PaintJoin,
    pub miter_limit: f32,

    pub line_dash: Arc<Vec<f32>>,
    pub line_dash_offset: f32,

    pub global_alpha: f32,
    pub blend_mode: BlendMode,

    pub shadow: Shadow,
    pub text: TextAttrs,
    /// Enables/disables default anti-aliasing for fills/strokes.
    pub antialias: bool,
    /// Image-smoothing (bilinear) flag — affects `drawImage` when scaling.
    pub image_smoothing: bool,
    /// Current transformation matrix stored as the SVG-style 2x3
    /// affine `[a, b, c, d, e, f]`:
    ///
    /// ```text
    ///   | a  c  e |
    ///   | b  d  f |   ( third row = [0, 0, 1] implicit )
    ///   | 0  0  1 |
    /// ```
    ///
    /// Maintained in parallel with `SkCanvas`'s own CTM so the
    /// damage classifier (`canvas2d_dispatcher::classify_draw_damage`)
    /// can transform rectangles from object space to device space
    /// WITHOUT cracking open the Skia canvas handle.  Values are
    /// kept in sync by the transform handlers in `canvas.rs`; any
    /// divergence is a bug.
    ///
    /// `ctm_is_axis_aligned()` replaces the previous sticky
    /// `ctm_non_axis_aligned` boolean so `setTransform(1,0,0,1,0,0)`
    /// after a `rotate()` correctly re-enables the partial-damage
    /// fast path.
    pub ctm: [f32; 6],
}

/// Identity matrix constant.
pub const CTM_IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

impl Canvas2DState {
    /// Axis-aligned CTM shear test — `b` and `c` (the off-diagonal
    /// components) must both be zero for a rect's bounding box to
    /// remain a rect after transform.  Uses a small epsilon so
    /// floating-point drift in long transform chains doesn't
    /// accidentally lose the fast path.
    #[inline]
    pub fn ctm_is_axis_aligned(&self) -> bool {
        const EPS: f32 = 1e-6;
        self.ctm[1].abs() < EPS && self.ctm[2].abs() < EPS
    }

    /// Pre-multiply the CTM by a 2x3 affine: `self.ctm = self.ctm * m`.
    /// All Canvas 2D transform ops (`translate` / `rotate` / `scale`)
    /// are post-concatenations in matrix terms — they apply to points
    /// before the existing CTM — which is the same as pre-multiplying
    /// the CTM by the op's matrix.
    #[inline]
    pub fn ctm_concat(&mut self, m: [f32; 6]) {
        let [a, b, c, d, e, f] = self.ctm;
        let [a2, b2, c2, d2, e2, f2] = m;
        self.ctm = [
            a * a2 + c * b2,
            b * a2 + d * b2,
            a * c2 + c * d2,
            b * c2 + d * d2,
            a * e2 + c * f2 + e,
            b * e2 + d * f2 + f,
        ];
    }

    /// Replace the CTM wholesale — semantics of `ctx.setTransform`.
    #[inline]
    pub fn ctm_set(&mut self, m: [f32; 6]) {
        self.ctm = m;
    }

    /// Reset to identity — `ctx.resetTransform()` / `save`-less
    /// `setTransform(1,0,0,1,0,0)`.
    #[inline]
    pub fn ctm_reset(&mut self) {
        self.ctm = CTM_IDENTITY;
    }

    /// Transform an axis-aligned object-space rect into device-space
    /// bounding box.  Caller MUST verify `ctm_is_axis_aligned()` first
    /// — sheared / rotated matrices produce a parallelogram whose
    /// bbox is strictly larger than naive per-corner min/max.
    ///
    /// Handles negative scale (reflection) correctly by taking min/max
    /// after transform rather than assuming top-left.
    ///
    /// Returns `None` if any corner is non-finite (NaN / Inf CTM).
    #[inline]
    pub fn map_axis_aligned_rect(
        &self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) -> Option<(f32, f32, f32, f32)> {
        debug_assert!(
            self.ctm_is_axis_aligned(),
            "map_axis_aligned_rect called on non-axis-aligned CTM"
        );
        let [a, _, _, d, e, f] = self.ctm;
        let x0 = a * x + e;
        let y0 = d * y + f;
        let x1 = a * (x + w) + e;
        let y1 = d * (y + h) + f;
        let (lx, rx) = (x0.min(x1), x0.max(x1));
        let (ty, by) = (y0.min(y1), y0.max(y1));
        if !(lx.is_finite() && rx.is_finite() && ty.is_finite() && by.is_finite()) {
            return None;
        }
        Some((lx, ty, rx - lx, by - ty))
    }
}

impl Default for Canvas2DState {
    /// Full set of WHATWG Canvas 2D defaults.
    ///
    /// Any change here is a user-visible behaviour change and MUST be
    /// paired with a test update in `tests::defaults_match_whatwg_spec`.
    fn default() -> Self {
        Self {
            fill: StyleKind::Color(Color::black()),
            stroke: StyleKind::Color(Color::black()),

            line_width: 1.0,
            line_cap: PaintCap::Butt,
            line_join: PaintJoin::Miter,
            miter_limit: 10.0,

            line_dash: Arc::new(Vec::new()),
            line_dash_offset: 0.0,

            global_alpha: 1.0,
            blend_mode: BlendMode::SrcOver,

            shadow: Shadow::none(),
            text: TextAttrs::default(),
            antialias: true,
            image_smoothing: true,
            ctm: CTM_IDENTITY,
        }
    }
}

impl Canvas2DState {
    /// Return `true` if the *fill* side must go through a shader (gradient
    /// or pattern) rather than a flat colour.  Exposed so the handler can
    /// short-circuit paint construction on the hot path.
    #[inline]
    pub fn fill_needs_shader(&self) -> bool {
        !matches!(&self.fill, StyleKind::Color(_))
    }

    /// Sibling of [`Self::fill_needs_shader`] for the stroke side.
    #[inline]
    pub fn stroke_needs_shader(&self) -> bool {
        !matches!(&self.stroke, StyleKind::Color(_))
    }

    /// Return the Skia [`SamplingOptions`] that Canvas 2D `drawImage` must use
    /// for the current drawing state.
    ///
    /// `imageSmoothingEnabled = false` → nearest-neighbour, no mipmap.
    /// `imageSmoothingEnabled = true`  → bilinear with nearest-mip, matching
    /// Chromium's default (mipmap filter is not part of the Canvas 2D spec
    /// until `imageSmoothingQuality = "high"`, which this engine does not
    /// yet expose).
    ///
    /// This is the single source of truth for image sampling in the Canvas
    /// 2D renderer. Every draw path — single `DrawImage`, `DrawImageBatch`
    /// atlas run, and `DrawImageBatch` individual fallback — MUST call this
    /// helper instead of hardcoding a `SamplingOptions`. Divergence between
    /// paths causes observable pixel differences when adjacent draws are
    /// merged into a batch (audit finding R3).
    #[inline]
    pub fn image_sampling_options(&self) -> SamplingOptions {
        if self.image_smoothing {
            SamplingOptions::new(FilterMode::Linear, MipmapMode::Nearest)
        } else {
            SamplingOptions::new(FilterMode::Nearest, MipmapMode::None)
        }
    }
}

/// Save/restore stack for [`Canvas2DState`].
///
/// The WHATWG Canvas 2D spec requires `save()` and `restore()` to be
/// perfectly symmetric — every `save()` MUST snapshot *all* drawing
/// state, and the matching `restore()` MUST pop it.  We used to cap
/// the stack at 32 entries "for allocator pressure", but that silently
/// desynchronised our logical state from the `SkCanvas` CTM/clip
/// stack: past depth 32, `save()` dropped the snapshot yet still
/// called `canvas.save()`; the matching `restore()` saw an empty
/// local stack, skipped `canvas.restore()`, and **leaked a CTM/clip
/// frame forever**.  The fix is to let the stack grow unbounded;
/// `Vec`'s natural amortised growth is cheap compared to a single
/// `SkPaint` draw, and pathological scripts that save a million
/// times have worse problems than a 32-byte `Vec` entry.
///
/// `Arc` sharing inside [`Canvas2DState`] (line_dash, families,
/// gradient stops) keeps each snapshot to ~200 bytes of genuinely
/// owned data.
///
/// **Future refinement note (P3-2 persistent blocks)** — a more
/// aggressive design would split `Canvas2DState` into orthogonal
/// sub-blocks (`transform/clip`, `stroke/dash`, `fill`, `text`,
/// `shadow`) and wrap each in its own `Arc`, so a `save()` becomes
/// `N` pointer clones instead of a ~200-byte struct copy.  That
/// refactor is intentionally deferred until profiling shows
/// `clone` is a hotspot: the current `Vec<Canvas2DState>` already
/// costs only 4 × f32 of true-own copy per push because every
/// heap-backed field is `Arc<_>`.  If you pick it up, keep the
/// `save()` → mutate → `restore()` round-trip allocation-free on
/// the common no-mutate-then-restore path that games emit 10s of
/// thousands of times per frame (sprite batches).
#[derive(Debug, Default)]
pub struct StateStack {
    /// Saved state snapshots, LIFO.  Unbounded — see type doc.
    saved: Vec<Canvas2DState>,
}

impl StateStack {
    pub fn new() -> Self {
        Self {
            saved: Vec::with_capacity(8),
        }
    }

    /// Number of snapshots currently on the stack (equals the number of
    /// `save()` calls that have no matching `restore()`).
    #[inline]
    pub fn depth(&self) -> usize {
        self.saved.len()
    }

    /// Push a snapshot of `state`.  Always succeeds — the stack has
    /// no artificial cap because an asymmetric `save()` / `restore()`
    /// pair would desync us from `SkCanvas`.
    pub fn push(&mut self, state: &Canvas2DState) {
        self.saved.push(state.clone());
    }

    /// Pop the most recent snapshot into `state`.
    ///
    /// Returns `true` if a snapshot existed, `false` on under-flow
    /// (`restore()` before `save()` — a legal Canvas 2D no-op).
    pub fn pop(&mut self, state: &mut Canvas2DState) -> bool {
        match self.saved.pop() {
            Some(snapshot) => {
                *state = snapshot;
                true
            }
            None => false,
        }
    }

    /// Number of snapshot slots currently allocated in the backing `Vec`
    /// (reflects the high-water capacity, not just live depth).
    ///
    /// Exposed so callers can observe and reason about the high-water mark
    /// before deciding whether to call [`shrink_to_fit`](Self::shrink_to_fit).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.saved.capacity()
    }

    /// Release `Vec` over-capacity accumulated from a previous deep-save
    /// burst, bringing the retained allocation back toward the live depth.
    ///
    /// Call at context reset, canvas resize, canvas destroy, or a
    /// low-memory event.  Does **not** affect the live saved-state
    /// semantics — only excess heap capacity is released; any snapshots
    /// still on the stack are retained unmodified.
    #[inline]
    pub fn shrink_to_fit(&mut self) {
        self.saved.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- save / restore is allocation-free, and stays that way ---------

    /// Section 7.3 on `save()` / `restore()`.
    ///
    /// **This gate protects a property that a single added field would break.**
    /// [`Canvas2DState`] is scalars plus `Arc`s — `line_dash`,
    /// `text.families`, and the gradient `stops` inside [`StyleKind`] — so the
    /// snapshot [`StateStack::push`] takes is a fixed-size copy and a handful
    /// of refcount bumps, never a heap event. Give any of those fields a bare
    /// `Vec` or `String` and every `save()` a UI issues, which is one per
    /// styled draw, silently buys and frees a block on the render thread. The
    /// type's own doc cannot enforce that; this can.
    ///
    /// The stack's `Vec` is warmed outside the burst: it is built
    /// `with_capacity(8)` and grows once per new maximum nesting depth, which
    /// is startup cost rather than steady state.
    #[test]
    fn a_steady_state_save_and_restore_never_reaches_the_heap() {
        let mut state = Canvas2DState::default();
        let mut stack = StateStack::new();

        // A dash pattern and a gradient fill, so the Arc-carrying fields are
        // populated rather than left at their scalar defaults — a snapshot of
        // an all-default state would not exercise them.
        state.line_dash = Arc::new(vec![4.0, 2.0]);
        state.fill = StyleKind::LinearGradient {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
            stops: Arc::new(vec![GradientStop {
                offset: 0.0,
                color: Color::black(),
            }]),
        };

        // Reach the deepest nesting the burst will use, so the stack's own
        // growth is paid before measurement.
        const DEPTH: usize = 8;
        for _ in 0..DEPTH {
            stack.push(&state);
        }
        for _ in 0..DEPTH {
            stack.pop(&mut state);
        }

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "canvas2d: save/restore snapshot at nesting depth 8",
                warmup: 4,
                measured: 64,
            },
            |_| {
                for _ in 0..DEPTH {
                    stack.push(&state);
                }
                let mut popped = 0u32;
                for _ in 0..DEPTH {
                    if stack.pop(&mut state) {
                        popped += 1;
                    }
                }
                popped
            },
        );
    }

    /// The snapshot has to be a real snapshot: a `restore()` must put back the
    /// values the matching `save()` saw, including the ones behind an `Arc`.
    /// A gate on allocation alone would pass an implementation that shared
    /// state instead of copying it.
    #[test]
    fn restore_puts_back_every_field_the_save_captured() {
        let mut state = Canvas2DState::default();
        let mut stack = StateStack::new();

        state.line_width = 7.0;
        state.global_alpha = 0.25;
        state.line_dash = Arc::new(vec![1.0, 2.0, 3.0]);
        state.text.size = 42.0;
        state.ctm = [2.0, 0.0, 0.0, 2.0, 10.0, 20.0];
        let saved = state.clone();

        stack.push(&state);
        // Change everything the snapshot covers.
        state.line_width = 1.0;
        state.global_alpha = 1.0;
        state.line_dash = Arc::new(Vec::new());
        state.text.size = 10.0;
        state.ctm = CTM_IDENTITY;

        assert!(stack.pop(&mut state), "the stack had a snapshot to pop");
        assert_eq!(state, saved, "restore did not put the saved state back");
    }

    // ---- defaults ------------------------------------------------------
    #[test]
    fn defaults_match_whatwg_spec() {
        let s = Canvas2DState::default();

        // Styles: black opaque
        assert_eq!(s.fill, StyleKind::Color(Color::black()));
        assert_eq!(s.stroke, StyleKind::Color(Color::black()));

        // Stroke attributes
        assert_eq!(s.line_width, 1.0);
        assert!(matches!(s.line_cap, PaintCap::Butt));
        assert!(matches!(s.line_join, PaintJoin::Miter));
        assert_eq!(s.miter_limit, 10.0);
        assert!(s.line_dash.is_empty());
        assert_eq!(s.line_dash_offset, 0.0);

        // Compositing
        assert_eq!(s.global_alpha, 1.0);
        assert_eq!(s.blend_mode, BlendMode::SrcOver);

        // Shadow: off
        assert!(!s.shadow.is_visible());

        // Text
        assert_eq!(s.text.size, 10.0);
        assert_eq!(&*s.text.families, &vec!["sans-serif".to_string()]);
        assert_eq!(s.text.weight, 400);
        assert!(!s.text.italic);
        assert!(matches!(s.text.align, TextAlign::Start));
        assert!(matches!(s.text.baseline, TextBaseline::Alphabetic));

        // Smoothing + anti-aliasing
        assert!(s.antialias);
        assert!(s.image_smoothing);
    }

    #[test]
    fn fill_needs_shader_for_gradients_and_patterns() {
        let mut s = Canvas2DState::default();
        assert!(!s.fill_needs_shader());

        s.fill = StyleKind::LinearGradient {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 0.0,
            stops: std::sync::Arc::new(vec![]),
        };
        assert!(s.fill_needs_shader());

        s.fill = StyleKind::Pattern {
            image_id: 42,
            repeat_x: true,
            repeat_y: true,
        };
        assert!(s.fill_needs_shader());
    }

    // ---- shadow visibility --------------------------------------------
    #[test]
    fn transparent_shadow_is_not_visible_even_with_blur() {
        let s = Shadow {
            blur: 10.0,
            color: Color::transparent(),
            offset_x: 5.0,
            offset_y: 5.0,
        };
        assert!(!s.is_visible());
    }

    #[test]
    fn opaque_zero_shadow_is_not_visible() {
        // Matches Chrome: a non-transparent shadow colour with blur=0 and
        // offsets=(0,0) produces NO shadow (it would just overpaint itself).
        let s = Shadow {
            blur: 0.0,
            color: Color::black(),
            offset_x: 0.0,
            offset_y: 0.0,
        };
        assert!(!s.is_visible());
    }

    #[test]
    fn opaque_shadow_with_blur_is_visible() {
        let s = Shadow {
            blur: 4.0,
            color: Color::black(),
            offset_x: 0.0,
            offset_y: 0.0,
        };
        assert!(s.is_visible());
    }

    #[test]
    fn opaque_shadow_with_offset_only_is_visible() {
        let s = Shadow {
            blur: 0.0,
            color: Color::black(),
            offset_x: 2.0,
            offset_y: 0.0,
        };
        assert!(s.is_visible());
    }

    // ---- StyleKind ----------------------------------------------------
    #[test]
    fn gradient_type_linear_drops_r_inputs() {
        let k = StyleKind::from_gradient(
            GradientType::Linear,
            0.0,
            0.0,
            99.0, // r0 ignored
            100.0,
            0.0,
            88.0, // r1 ignored
            vec![],
        );
        assert!(matches!(k, StyleKind::LinearGradient { .. }));
    }

    #[test]
    fn gradient_type_radial_preserves_r_inputs() {
        let k = StyleKind::from_gradient(
            GradientType::Radial,
            10.0,
            20.0,
            30.0,
            40.0,
            50.0,
            60.0,
            vec![],
        );
        match k {
            StyleKind::RadialGradient { r0, r1, .. } => {
                assert_eq!(r0, 30.0);
                assert_eq!(r1, 60.0);
            }
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn gradient_type_conic_encodes_start_angle_in_x1() {
        let k = StyleKind::from_gradient(
            GradientType::Conic,
            50.0, // cx
            60.0, // cy
            0.0,
            std::f32::consts::FRAC_PI_2, // start angle in x1 (90°)
            0.0,
            0.0,
            vec![],
        );
        match k {
            StyleKind::ConicGradient {
                cx,
                cy,
                start_angle,
                ..
            } => {
                assert_eq!(cx, 50.0);
                assert_eq!(cy, 60.0);
                assert!((start_angle - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
            }
            _ => panic!("wrong kind"),
        }
    }

    // ---- save / restore ------------------------------------------------
    #[test]
    fn restore_without_save_is_noop() {
        let mut stack = StateStack::new();
        let mut state = Canvas2DState::default();
        let before = state.clone();
        assert!(!stack.pop(&mut state));
        assert_eq!(state, before);
    }

    #[test]
    fn save_restore_roundtrip_preserves_state() {
        let mut stack = StateStack::new();
        let mut state = Canvas2DState::default();

        stack.push(&state);
        assert_eq!(stack.depth(), 1);

        // Mutate
        state.line_width = 5.5;
        state.global_alpha = 0.25;
        state.blend_mode = BlendMode::Plus;
        state.fill = StyleKind::Color(Color::rgb(10, 20, 30));
        state.text.size = 42.0;

        assert!(stack.pop(&mut state));
        assert_eq!(stack.depth(), 0);
        assert_eq!(state, Canvas2DState::default());
    }

    #[test]
    fn save_captures_independent_snapshot() {
        // Verifies the stack does not hold a reference to the live state.
        let mut stack = StateStack::new();
        let mut state = Canvas2DState::default();

        state.line_width = 3.0;
        stack.push(&state);

        state.line_width = 7.0;
        assert_eq!(state.line_width, 7.0);

        stack.pop(&mut state);
        assert_eq!(state.line_width, 3.0);
    }

    #[test]
    fn stack_grows_unbounded_and_pops_symmetrically() {
        // Regression for the WHATWG save/restore symmetry bug: the old
        // code capped depth at 32, causing silently-dropped snapshots
        // past that limit and a permanent CTM/clip desync against
        // `SkCanvas`.  This test deliberately pushes well past 32 and
        // asserts that *every* pop returns the matching snapshot.
        let mut stack = StateStack::new();
        let base = Canvas2DState::default();
        let mut state = base.clone();

        // Push a hundred distinct snapshots, each mutating `line_width`
        // so we can verify stack identity on pop.
        const N: usize = 128;
        for i in 0..N {
            state.line_width = i as f32 + 1.0;
            stack.push(&state);
        }
        assert_eq!(stack.depth(), N);

        // Pop in reverse and check each restored line_width.
        for i in (0..N).rev() {
            assert!(stack.pop(&mut state), "pop {} must succeed", i);
            assert_eq!(state.line_width, i as f32 + 1.0);
        }
        assert_eq!(stack.depth(), 0);
        // A final pop must report underflow (Canvas 2D spec says
        // `restore()` on an empty stack is a silent no-op).
        assert!(!stack.pop(&mut state));
    }

    #[test]
    fn stack_push_is_symmetric_under_deep_nesting() {
        // Verifies: for N consecutive pushes followed by N pops, the
        // state returned at each pop exactly matches the snapshot
        // captured at the corresponding push — no dropped frames.
        let mut stack = StateStack::new();
        let mut state = Canvas2DState::default();
        let mut expected: Vec<f32> = Vec::new();
        for i in 0..200 {
            state.line_width = (i as f32) * 0.5;
            expected.push(state.line_width);
            stack.push(&state);
        }
        while let Some(expect) = expected.pop() {
            assert!(stack.pop(&mut state));
            assert_eq!(state.line_width, expect);
        }
        assert_eq!(stack.depth(), 0);
    }

    #[test]
    fn save_shares_heap_with_live_state() {
        // Regression: `Canvas2DState::clone` (called by `StateStack::push`)
        // must NOT deep-copy line_dash / text.families / gradient stops —
        // they live in `Arc<Vec<_>>` specifically so every `ctx.save()`
        // costs a refcount bump rather than the old per-sprite heap copy.
        let mut state = Canvas2DState::default();
        state.line_dash = std::sync::Arc::new(vec![4.0, 2.0, 1.0, 2.0]);
        state.text = TextAttrs {
            size: 12.0,
            families: std::sync::Arc::new(vec![
                "Helvetica".into(),
                "Arial".into(),
                "sans-serif".into(),
            ]),
            weight: 400,
            italic: false,
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            direction: TextDirection::Inherit,
        };

        let mut stack = StateStack::new();
        stack.push(&state);

        // After push, the stack holds one `Arc` clone of each heap vec.
        assert_eq!(std::sync::Arc::strong_count(&state.line_dash), 2);
        assert_eq!(std::sync::Arc::strong_count(&state.text.families), 2);
    }

    #[test]
    fn gradient_stops_are_shared_across_save_snapshots() {
        // A fill style with a gradient is saved/restored thousands of
        // times per frame in particle systems; the Arc'd stop list
        // means we never clone its N colour stops.
        let mut state = Canvas2DState::default();
        let stops_vec = vec![
            GradientStop {
                offset: 0.0,
                color: Color::black(),
            },
            GradientStop {
                offset: 1.0,
                color: Color::white(),
            },
        ];
        state.fill = StyleKind::from_gradient(
            GradientType::Linear,
            0.0,
            0.0,
            0.0,
            100.0,
            0.0,
            0.0,
            stops_vec,
        );
        // Walk the Arc through a `push` + `pop` and check the refcount
        // observed from the *inside* of the enum never exceeded 2.
        let mut stack = StateStack::new();
        stack.push(&state);
        if let StyleKind::LinearGradient { stops, .. } = &state.fill {
            assert_eq!(std::sync::Arc::strong_count(stops), 2);
        } else {
            panic!("wrong kind");
        }
        let mut popped = Canvas2DState::default();
        stack.pop(&mut popped);
        // `state.fill` still holds its Arc; `popped.fill` holds another
        // Arc clone from the snapshot; total count = 2.
        if let (
            StyleKind::LinearGradient { stops: a, .. },
            StyleKind::LinearGradient { stops: b, .. },
        ) = (&state.fill, &popped.fill)
        {
            assert!(std::sync::Arc::ptr_eq(a, b), "save must keep the same Arc");
        } else {
            panic!("wrong kind after pop");
        }
    }

    // ---- P3-2 latent-COW evidence -----------------------------------

    #[test]
    fn deep_save_scales_heap_cost_by_arc_refcount_only() {
        // Pin the P3-2 claim in the StateStack doc comment: for
        // a 1000-deep save stack with identical state, the
        // heap-backed Arc<Vec<_>> fields are SHARED, not deep-
        // copied.  We observe this as `Arc::strong_count` growing
        // linearly with depth — which means each push cost ONE
        // refcount bump (atomic inc) rather than N bytes of vec
        // cloning.  A future persistent-data-structure rewrite
        // would turn every clone into O(1) cost; this test
        // proves the *current* design is already within a small
        // constant factor of that ideal, so the rewrite is
        // correctly deferred.
        let mut state = Canvas2DState::default();
        state.line_dash = std::sync::Arc::new(vec![4.0, 2.0, 1.0, 2.0]);
        state.text = TextAttrs {
            size: 12.0,
            families: std::sync::Arc::new(vec![
                "Helvetica".into(),
                "Arial".into(),
                "sans-serif".into(),
            ]),
            weight: 400,
            italic: false,
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            direction: TextDirection::Inherit,
        };

        let mut stack = StateStack::new();
        let depth = 1000;
        for _ in 0..depth {
            stack.push(&state);
        }
        // 1 live + `depth` snapshots on the stack, all sharing
        // the same Arc.  The original Arc in `state` itself is
        // the +1 at the end — total = 1 + depth.
        assert_eq!(std::sync::Arc::strong_count(&state.line_dash), depth + 1,);
        assert_eq!(
            std::sync::Arc::strong_count(&state.text.families),
            depth + 1,
        );

        // Popping half the stack must drop the Arc refcount by
        // exactly that many (LIFO, no orphaning).
        let mut scratch = state.clone();
        // `scratch.clone()` bumped the Arc by 2 more; back it out.
        drop(scratch.line_dash.clone());
        drop(scratch);
        let scratch = &mut state.clone();
        for _ in 0..(depth / 2) {
            assert!(stack.pop(scratch));
        }
        // After popping half, refcount = state_arc + remaining_snapshots + scratch_arc.
        assert_eq!(
            std::sync::Arc::strong_count(&state.line_dash),
            (depth / 2) + 1 + 1,
        );
    }

    #[test]
    fn deep_save_restore_preserves_order() {
        // LIFO semantics: last-saved state is first-restored.
        let mut stack = StateStack::new();
        let mut state = Canvas2DState::default();

        for i in 1..=4 {
            state.line_width = i as f32;
            stack.push(&state);
        }
        // After 4 pushes the live state is line_width=4.0
        // The stack (bottom → top) is 1.0, 2.0, 3.0, 4.0

        // Mutate beyond the last push, then restore -- should get 4.0 back.
        state.line_width = 99.0;
        stack.pop(&mut state);
        assert_eq!(state.line_width, 4.0);

        stack.pop(&mut state);
        assert_eq!(state.line_width, 3.0);

        stack.pop(&mut state);
        assert_eq!(state.line_width, 2.0);

        stack.pop(&mut state);
        assert_eq!(state.line_width, 1.0);

        assert_eq!(stack.depth(), 0);
    }
    #[test]
    fn save_stack_high_water_released_by_shrink() {
        let mut stack = StateStack::new();
        let mut state = Canvas2DState::default();
        const DEEP: usize = 200;
        for i in 0..DEEP {
            state.line_width = i as f32 + 1.0;
            stack.push(&state);
        }
        for _ in 0..DEEP {
            assert!(stack.pop(&mut state));
        }
        assert_eq!(stack.depth(), 0);
        let high = stack.capacity();
        assert!(
            high >= DEEP,
            "Vec capacity should still be at least {DEEP}, got {high}"
        );
        stack.shrink_to_fit();
        let low = stack.capacity();
        assert!(
            low < high / 4,
            "shrink_to_fit must release high-water capacity: before={high}, after={low}"
        );
    }

    #[test]
    fn image_sampling_options_match_canvas_smoothing_defaults() {
        let smooth = Canvas2DState::default().image_sampling_options();
        assert_eq!(smooth.filter, FilterMode::Linear);
        assert_eq!(smooth.mipmap, MipmapMode::Nearest);

        let mut state = Canvas2DState::default();
        state.image_smoothing = false;
        let nearest = state.image_sampling_options();
        assert_eq!(nearest.filter, FilterMode::Nearest);
        assert_eq!(nearest.mipmap, MipmapMode::None);
    }
}
