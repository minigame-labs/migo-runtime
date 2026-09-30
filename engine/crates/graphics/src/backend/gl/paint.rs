//! Build Skia [`Paint`] objects from a [`Canvas2DState`] snapshot.
//!
//! Canvas2D fillStyle / strokeStyle / globalAlpha / shadow / blend-mode all
//! translate into SkPaint settings; gradients and patterns additionally
//! require an `SkShader`.  We intentionally rebuild the paint per draw
//! rather than caching it on the state — SkPaint is a value type (cheap
//! copies, no allocation for the common flat-colour case) and rebuilding
//! avoids the "state ⇆ cache coherency" complexity that a stale cache
//! would introduce.

use super::color::{to_sk_color4f, to_sk_color4f_modulated};
use super::state::{Canvas2DState, StyleKind};
use skia_safe::{BlendMode, Paint, Shader};

/// Build a Skia `Shader` from the fill/stroke `StyleKind`, if any.
///
/// Returns `None` when the kind is a flat colour, or when the gradient has
/// fewer than two stops (spec-invalid, silently suppressed to match browser
/// behaviour), or when a pattern image is not yet loaded.
pub fn build_shader<R>(kind: &StyleKind, global_alpha: f32, image_resolver: &R) -> Option<Shader>
where
    R: PatternResolver,
{
    match kind {
        StyleKind::Color(_) => None,
        StyleKind::LinearGradient {
            x0,
            y0,
            x1,
            y1,
            stops,
        } => super::effect_cache::get_or_build_linear_gradient(
            *x0,
            *y0,
            *x1,
            *y1,
            stops,
            global_alpha,
        ),
        StyleKind::RadialGradient {
            x0,
            y0,
            r0,
            x1,
            y1,
            r1,
            stops,
        } => super::effect_cache::get_or_build_radial_gradient(
            *x0,
            *y0,
            *r0,
            *x1,
            *y1,
            *r1,
            stops,
            global_alpha,
        ),
        StyleKind::ConicGradient {
            cx,
            cy,
            start_angle,
            stops,
        } => super::effect_cache::get_or_build_conic_gradient(
            *cx,
            *cy,
            *start_angle,
            stops,
            global_alpha,
        ),
        StyleKind::Pattern {
            image_id,
            repeat_x,
            repeat_y,
        } => image_resolver.resolve_pattern(*image_id, *repeat_x, *repeat_y, global_alpha),
    }
}

/// Abstraction over image registry lookup, factored out so the handler
/// core can be unit-tested without a real GPU image store.
pub trait PatternResolver {
    fn resolve_pattern(
        &self,
        image_id: u32,
        repeat_x: bool,
        repeat_y: bool,
        global_alpha: f32,
    ) -> Option<Shader>;
}

/// Test-only resolver that always reports "image not loaded".
pub struct NullPatternResolver;

impl PatternResolver for NullPatternResolver {
    fn resolve_pattern(&self, _: u32, _: bool, _: bool, _: f32) -> Option<Shader> {
        None
    }
}

// Gradient builders live in `super::effect_cache` so their shader
// outputs are memoised across `build_shader` calls.  See
// `effect_cache::get_or_build_linear_gradient` and siblings.

/// If a visible shadow is present in `state`, install a drop-shadow
/// `ImageFilter` on `paint`.
///
/// Canvas2D's shadow is a drop-shadow (rendered *behind* the primary
/// draw).  We convert `shadowBlur` to sigma via `sigma = blur / 2` which
/// matches Chrome's interpretation of the CSS "blur length" as
/// `2 x stddev`.
///
/// Two rules of the specification (HTML, "shadows") decide the numbers:
///
///  * `shadowOffsetX/Y` and `shadowBlur` are in device pixels and are **not
///    affected by the current transformation matrix**. A Skia image filter
///    runs in the canvas's local space, so the device values are carried
///    back through the inverse of the matrix's linear part first. Handed over
///    as they were, a context under `scale(2, 2)` -- every device-pixel-ratio
///    game -- drew its shadow twice as far and twice as soft as a browser.
///  * The shadow's alpha is the silhouette's alpha times the shadow colour's
///    alpha, and `globalAlpha` is applied once, to the result. The silhouette
///    already carries `globalAlpha` (it is the paint's own alpha), so the
///    shadow colour must not be scaled by it again: at `globalAlpha = 0.5` the
///    shadow came out at a quarter.
pub fn apply_shadow_to_paint(paint: &mut Paint, state: &Canvas2DState) {
    if !state.shadow.is_visible() {
        return;
    }
    let [a, b, c, d, _, _] = state.ctm;
    let det = a * d - b * c;
    // A singular matrix flattens the drawing to a line or a point; there is
    // nothing for a shadow to be cast from.
    if !det.is_finite() || det.abs() < f32::EPSILON {
        return;
    }
    let (dx, dy) = (state.shadow.offset_x, state.shadow.offset_y);
    let local_dx = (d * dx - c * dy) / det;
    let local_dy = (a * dy - b * dx) / det;
    let sigma = state.shadow.blur * 0.5;
    let sigma_x = sigma / a.hypot(b);
    let sigma_y = sigma / c.hypot(d);
    let color = to_sk_color4f(state.shadow.color).to_color();
    // Shared filter pool: drop-shadow reuse is very high because
    // games set the same shadow on every label / button-container
    // for a whole screen.  `get_or_build_drop_shadow` hands back a
    // refcounted clone, no allocation on cache hit.
    if let Some(filter) = super::effect_cache::get_or_build_drop_shadow(
        // `skia_safe::Color` is a `#[repr(transparent)]` newtype
        // over `SkColor` (aka ARGB u32).  Transmuting through the
        // `native_transmutable!` machinery is idiomatic in skia-safe.
        // Easier: rebuild the u32 from the alpha/red/green/blue
        // channels the Color struct exposes.
        (color.a() as u32) << 24
            | (color.r() as u32) << 16
            | (color.g() as u32) << 8
            | (color.b() as u32),
        sigma_x,
        sigma_y,
        local_dx,
        local_dy,
    ) {
        paint.set_image_filter(filter);
    }
}

/// Build a `Paint` preset for the Canvas2D *fill* side.
pub fn build_fill_paint<R: PatternResolver>(state: &Canvas2DState, resolver: &R) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(state.antialias);
    paint.set_style(skia_safe::paint::Style::Fill);
    paint.set_blend_mode(state.blend_mode);

    match &state.fill {
        StyleKind::Color(c) => {
            let c4f = to_sk_color4f_modulated(*c, state.global_alpha);
            paint.set_color4f(c4f, None);
        }
        _ => {
            if let Some(shader) = build_shader(&state.fill, state.global_alpha, resolver) {
                paint.set_shader(shader);
                apply_style_alpha(&mut paint, &state.fill, state.global_alpha);
            } else {
                paint.set_color4f(
                    to_sk_color4f(shared::protocol::color::Color::transparent()),
                    None,
                );
            }
        }
    }
    apply_shadow_to_paint(&mut paint, state);
    paint
}

/// Apply `globalAlpha` on a paint whose colour comes from a shader.
///
/// Gradient stops already bake `globalAlpha` into their per-stop
/// colours via [`to_sk_color4f_modulated`] — applying paint.alpha on
/// top would double-modulate.  Pattern shaders, however, sample
/// through an image and have no such opportunity; without this hook
/// `fillStyle = ctx.createPattern(...)` used to render at full
/// opacity regardless of `ctx.globalAlpha`, a silent spec deviation
/// caught by the 2026-04 rendering review.
#[inline]
fn apply_style_alpha(paint: &mut Paint, kind: &StyleKind, global_alpha: f32) {
    if matches!(kind, StyleKind::Pattern { .. }) {
        // SkPaint's alpha multiplies the shader output, exactly what
        // Canvas 2D globalAlpha demands for pattern fills/strokes.
        paint.set_alpha_f(global_alpha);
    }
}

/// Build a `Paint` preset for the Canvas2D *stroke* side.
pub fn build_stroke_paint<R: PatternResolver>(state: &Canvas2DState, resolver: &R) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(state.antialias);
    paint.set_style(skia_safe::paint::Style::Stroke);
    paint.set_stroke_width(state.line_width.max(0.0));
    paint.set_stroke_cap(state.line_cap);
    paint.set_stroke_join(state.line_join);
    paint.set_stroke_miter(state.miter_limit);
    paint.set_blend_mode(state.blend_mode);

    match &state.stroke {
        StyleKind::Color(c) => {
            let c4f = to_sk_color4f_modulated(*c, state.global_alpha);
            paint.set_color4f(c4f, None);
        }
        _ => {
            if let Some(shader) = build_shader(&state.stroke, state.global_alpha, resolver) {
                paint.set_shader(shader);
                apply_style_alpha(&mut paint, &state.stroke, state.global_alpha);
            } else {
                paint.set_color4f(
                    to_sk_color4f(shared::protocol::color::Color::transparent()),
                    None,
                );
            }
        }
    }

    if !state.line_dash.is_empty() {
        if let Some(effect) =
            super::effect_cache::get_or_build_dash(&state.line_dash, state.line_dash_offset)
        {
            paint.set_path_effect(effect);
        }
    }
    apply_shadow_to_paint(&mut paint, state);
    paint
}

/// `clearRect`'s Paint: writes transparent black with blend mode Clear,
/// which sets the destination to fully transparent regardless of current
/// state.
pub fn build_clear_paint() -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(false);
    paint.set_style(skia_safe::paint::Style::Fill);
    paint.set_color4f(skia_safe::Color4f::new(0.0, 0.0, 0.0, 0.0), None);
    paint.set_blend_mode(BlendMode::Clear);
    paint
}

#[cfg(test)]
mod tests {
    use super::super::state::{Canvas2DState, Shadow, StyleKind};
    use super::*;
    use shared::protocol::color::Color as ProtocolColor;
    use shared::protocol::render_cmd::GradientStop;
    use skia_safe::Color;

    /// A 1x1 read of a raster surface as premultiplied RGBA.
    fn pixel(surface: &mut skia_safe::Surface, x: i32, y: i32) -> [u8; 4] {
        let info = skia_safe::ImageInfo::new(
            (1, 1),
            skia_safe::ColorType::RGBA8888,
            skia_safe::AlphaType::Premul,
            None,
        );
        let mut px = [0u8; 4];
        assert!(surface.read_pixels(&info, &mut px, 4, (x, y)));
        px
    }

    /// Draw a red `size` square at the origin of a `scale`-scaled 64x64 surface
    /// with a hard black shadow `offset` device pixels to the right, exactly as
    /// the renderer does (the canvas carries the matrix, the state records it,
    /// the paint carries the filter).
    fn shadowed_square(
        scale: f32,
        offset: f32,
        global_alpha: f32,
        size: f32,
    ) -> skia_safe::Surface {
        let mut s = Canvas2DState::default();
        s.ctm = [scale, 0.0, 0.0, scale, 0.0, 0.0];
        s.global_alpha = global_alpha;
        s.fill = StyleKind::Color(ProtocolColor::rgb(255, 0, 0));
        s.shadow = Shadow {
            blur: 0.0,
            color: ProtocolColor::black(),
            offset_x: offset,
            offset_y: 0.0,
        };
        let paint = build_fill_paint(&s, &NullPatternResolver);
        let mut surface = skia_safe::surfaces::raster_n32_premul((64, 64)).unwrap();
        surface.canvas().scale((scale, scale));
        surface
            .canvas()
            .draw_rect(skia_safe::Rect::from_xywh(0.0, 0.0, size, size), &paint);
        surface
    }

    /// The offset is in device pixels whatever the matrix: a 10x10 pixel square
    /// drawn under `scale(2, 2)` with `shadowOffsetX = 10` casts its shadow to
    /// device x 10..20, not 20..30.
    #[test]
    fn shadow_offset_is_in_device_pixels_not_user_space() {
        let mut surface = shadowed_square(2.0, 10.0, 1.0, 5.0);
        assert_eq!(pixel(&mut surface, 5, 5), [255, 0, 0, 255], "the shape");
        assert_eq!(pixel(&mut surface, 15, 5), [0, 0, 0, 255], "the shadow");
        assert_eq!(pixel(&mut surface, 25, 5), [0, 0, 0, 0], "not doubled");
    }

    /// The same rule at the identity matrix is unchanged, and at `scale(0.5)` the
    /// offset is still device pixels.
    #[test]
    fn shadow_offset_at_other_scales() {
        let mut surface = shadowed_square(1.0, 10.0, 1.0, 10.0);
        assert_eq!(pixel(&mut surface, 15, 5), [0, 0, 0, 255]);
        let mut surface = shadowed_square(0.5, 10.0, 1.0, 20.0);
        // a 10x10 pixel square; its shadow at device x 10..20
        assert_eq!(pixel(&mut surface, 5, 5), [255, 0, 0, 255]);
        assert_eq!(pixel(&mut surface, 15, 5), [0, 0, 0, 255]);
        assert_eq!(pixel(&mut surface, 25, 5), [0, 0, 0, 0]);
    }

    /// `globalAlpha` scales the shadow once: half alpha is a half-transparent
    /// shadow (premultiplied 0,0,0,128), not a quarter.
    #[test]
    fn shadow_takes_global_alpha_once() {
        let mut surface = shadowed_square(1.0, 10.0, 0.5, 10.0);
        let shadow = pixel(&mut surface, 15, 5);
        assert!(
            (i32::from(shadow[3]) - 128).abs() <= 2,
            "shadow alpha {} at globalAlpha 0.5, want about 128",
            shadow[3]
        );
        let shape = pixel(&mut surface, 5, 5);
        assert!(
            (i32::from(shape[3]) - 128).abs() <= 2,
            "shape alpha {}",
            shape[3]
        );
    }

    #[test]
    fn build_fill_paint_uses_color_when_style_is_flat() {
        let mut s = Canvas2DState::default();
        s.fill = StyleKind::Color(ProtocolColor::rgb(10, 20, 30));
        s.global_alpha = 1.0;
        let p = build_fill_paint(&s, &NullPatternResolver);
        assert!(p.shader().is_none());
        assert_eq!(p.style(), skia_safe::paint::Style::Fill);
    }

    #[test]
    fn build_stroke_paint_carries_line_attributes() {
        let mut s = Canvas2DState::default();
        s.stroke = StyleKind::Color(ProtocolColor::black());
        s.line_width = 5.0;
        s.miter_limit = 3.5;
        s.line_cap = skia_safe::PaintCap::Round;
        s.line_join = skia_safe::PaintJoin::Bevel;
        s.line_dash = std::sync::Arc::new(vec![4.0, 2.0]);
        s.line_dash_offset = 1.0;

        let p = build_stroke_paint(&s, &NullPatternResolver);
        assert_eq!(p.style(), skia_safe::paint::Style::Stroke);
        assert_eq!(p.stroke_width(), 5.0);
        assert_eq!(p.stroke_miter(), 3.5);
        assert_eq!(p.stroke_cap(), skia_safe::PaintCap::Round);
        assert_eq!(p.stroke_join(), skia_safe::PaintJoin::Bevel);
        assert!(p.path_effect().is_some(), "dash effect must be installed");
    }

    #[test]
    fn build_stroke_paint_no_dash_when_empty() {
        let mut s = Canvas2DState::default();
        s.stroke = StyleKind::Color(ProtocolColor::black());
        s.line_dash = std::sync::Arc::new(Vec::new());
        let p = build_stroke_paint(&s, &NullPatternResolver);
        assert!(p.path_effect().is_none());
    }

    #[test]
    fn build_shader_returns_none_for_flat_color() {
        let s = StyleKind::Color(ProtocolColor::black());
        let sh = build_shader(&s, 1.0, &NullPatternResolver);
        assert!(sh.is_none());
    }

    #[test]
    fn build_shader_rejects_single_stop_gradient() {
        let s = StyleKind::LinearGradient {
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 0.0,
            stops: std::sync::Arc::new(vec![GradientStop {
                offset: 0.5,
                color: ProtocolColor::black(),
            }]),
        };
        assert!(build_shader(&s, 1.0, &NullPatternResolver).is_none());
    }

    #[test]
    fn build_shader_accepts_two_stop_linear() {
        let s = StyleKind::LinearGradient {
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 0.0,
            stops: std::sync::Arc::new(vec![
                GradientStop {
                    offset: 0.0,
                    color: ProtocolColor::rgb(255, 0, 0),
                },
                GradientStop {
                    offset: 1.0,
                    color: ProtocolColor::rgb(0, 0, 255),
                },
            ]),
        };
        assert!(build_shader(&s, 1.0, &NullPatternResolver).is_some());
    }

    #[test]
    fn pattern_returns_none_from_null_resolver() {
        let s = StyleKind::Pattern {
            image_id: 1,
            repeat_x: true,
            repeat_y: true,
        };
        assert!(build_shader(&s, 1.0, &NullPatternResolver).is_none());
    }

    /// Test-only PatternResolver that returns a transparent-black
    /// 1x1 shader so we can observe the paint's alpha channel
    /// without needing a real image.
    struct StubPatternResolver;
    impl PatternResolver for StubPatternResolver {
        fn resolve_pattern(
            &self,
            _image_id: u32,
            _repeat_x: bool,
            _repeat_y: bool,
            _global_alpha: f32,
        ) -> Option<Shader> {
            // Solid-colour shader backs the pattern path; its colour
            // doesn't matter for the alpha assertion below — we
            // only care that paint.alpha_f() reflects globalAlpha.
            Some(skia_safe::shaders::color(Color::WHITE))
        }
    }

    #[test]
    fn pattern_fill_applies_global_alpha_to_paint() {
        // Regression: `SkiaPatternResolver::resolve_pattern` drops
        // `global_alpha` on the floor (`let _ = global_alpha`); the
        // paint side is now responsible for folding globalAlpha in
        // via `set_alpha_f`.  Without this, `fillStyle =
        // ctx.createPattern(img, 'repeat')` paints at full opacity
        // regardless of `ctx.globalAlpha = 0.4` — a silent spec
        // deviation.
        let mut s = Canvas2DState::default();
        s.fill = StyleKind::Pattern {
            image_id: 42,
            repeat_x: true,
            repeat_y: true,
        };
        s.global_alpha = 0.4;
        let p = build_fill_paint(&s, &StubPatternResolver);
        assert!(p.shader().is_some(), "pattern shader must be installed");
        // Skia stores alpha as u8 0..=255; 0.4 * 255 ~= 102.
        let alpha = p.alpha();
        assert!(
            alpha >= 100 && alpha <= 105,
            "expected alpha ~= 102 for globalAlpha=0.4; got {alpha}"
        );
    }

    #[test]
    fn pattern_stroke_applies_global_alpha_to_paint() {
        let mut s = Canvas2DState::default();
        s.stroke = StyleKind::Pattern {
            image_id: 42,
            repeat_x: false,
            repeat_y: false,
        };
        s.global_alpha = 0.25;
        let p = build_stroke_paint(&s, &StubPatternResolver);
        assert!(p.shader().is_some());
        let alpha = p.alpha();
        assert!(
            alpha >= 60 && alpha <= 68,
            "expected alpha ~= 64 for globalAlpha=0.25; got {alpha}"
        );
    }

    #[test]
    fn gradient_fill_does_not_apply_paint_alpha_double() {
        // Gradient stops bake globalAlpha into their per-stop colours
        // via `to_sk_color4f_modulated`; applying paint.alpha on top
        // would double-modulate.  This test pins the non-pattern
        // branch's invariant: paint.alpha stays at the default for
        // gradient fills.
        let mut s = Canvas2DState::default();
        s.fill = StyleKind::LinearGradient {
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 0.0,
            stops: std::sync::Arc::new(vec![
                GradientStop {
                    offset: 0.0,
                    color: ProtocolColor::rgb(255, 0, 0),
                },
                GradientStop {
                    offset: 1.0,
                    color: ProtocolColor::rgb(0, 0, 255),
                },
            ]),
        };
        s.global_alpha = 0.4;
        let p = build_fill_paint(&s, &NullPatternResolver);
        assert!(p.shader().is_some());
        assert_eq!(p.alpha(), 255, "paint.alpha should be default for gradient");
    }

    #[test]
    fn clear_paint_uses_blend_mode_clear() {
        let p = build_clear_paint();
        assert_eq!(p.as_blend_mode(), Some(BlendMode::Clear));
    }

    #[test]
    fn fill_paint_carries_shadow_filter_when_visible() {
        let mut s = Canvas2DState::default();
        s.shadow = Shadow {
            blur: 4.0,
            color: ProtocolColor::black(),
            offset_x: 2.0,
            offset_y: 2.0,
        };
        let p = build_fill_paint(&s, &NullPatternResolver);
        assert!(p.image_filter().is_some(), "shadow ImageFilter missing");
    }

    #[test]
    fn fill_paint_has_no_shadow_filter_when_shadow_is_off() {
        let s = Canvas2DState::default();
        let p = build_fill_paint(&s, &NullPatternResolver);
        assert!(p.image_filter().is_none());
    }
}
