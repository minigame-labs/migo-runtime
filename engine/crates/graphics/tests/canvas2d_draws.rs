//! Canvas2D draw-path / transform / gradient / composite / shadow
//! golden-image tests.
//!
//! All tests render through the same raster harness as `canvas2d_rect.rs`.
//! Because AA edges are in play, most tests use `GoldenCfg::loose_aa`;
//! exact-match asserts pin pixel values at a handful of well-known
//! coordinates so drift can be distinguished from total failure.

#[path = "common/mod.rs"]
mod common;

use common::golden::{GoldenCfg, assert_matches_golden};
use common::harness::{read_pixels_rgba8, with_raster_surface};

use shared::protocol::color::Color as ProtoColor;
use shared::protocol::render_cmd::Canvas2DCmd::{self, *};
use shared::protocol::render_cmd::{GradientStop, GradientType};

use graphics::backend::gl::canvas::Canvas2DRenderer;
use graphics::backend::gl::paint::NullPatternResolver;

fn apply(ctx: &mut Canvas2DRenderer, canvas: &skia_safe::Canvas, cmds: &[Canvas2DCmd]) {
    for c in cmds {
        ctx.apply(canvas, c, &NullPatternResolver);
    }
}

// ====== Image smoothing ====================================================
//
// `imageSmoothingEnabled` is drawing state. What the renderer does with it is pick `drawImage`'s sampling; the
// pixels that follow from the sampling are checked end to end in migo-conformance (`canvas2d-image-smoothing`),
// because a scaled texture needs a GPU context and these tests run on a raster surface. What is checked here is
// the half that has no GPU in it: the command lands in the state, and the state is saved, restored and reset the
// way the specification says.

fn sampling_filter(ctx: &Canvas2DRenderer) -> skia_safe::FilterMode {
    ctx.state.image_sampling_options().filter
}

#[test]
fn image_smoothing_defaults_on_and_the_command_turns_it_off_and_on() {
    let mut c = Canvas2DRenderer::new();
    assert_eq!(
        sampling_filter(&c),
        skia_safe::FilterMode::Linear,
        "smoothing is on by default"
    );
    let (w, h) = (4, 4);
    with_raster_surface(w, h, |s| {
        apply(&mut c, s.canvas(), &[SetImageSmoothing { enabled: false }]);
        assert_eq!(sampling_filter(&c), skia_safe::FilterMode::Nearest);
        assert_eq!(
            c.state.image_sampling_options().mipmap,
            skia_safe::MipmapMode::None,
            "nearest is nearest all the way down: no mip level to blend through"
        );
        apply(&mut c, s.canvas(), &[SetImageSmoothing { enabled: true }]);
        assert_eq!(sampling_filter(&c), skia_safe::FilterMode::Linear);
    });
}

#[test]
fn image_smoothing_is_saved_and_restored_with_the_rest_of_the_state() {
    let mut c = Canvas2DRenderer::new();
    with_raster_surface(4, 4, |s| {
        apply(
            &mut c,
            s.canvas(),
            &[
                SetImageSmoothing { enabled: false },
                Save,
                SetImageSmoothing { enabled: true },
                Save,
                SetImageSmoothing { enabled: false },
            ],
        );
        assert_eq!(
            sampling_filter(&c),
            skia_safe::FilterMode::Nearest,
            "innermost"
        );
        apply(&mut c, s.canvas(), &[Restore]);
        assert_eq!(
            sampling_filter(&c),
            skia_safe::FilterMode::Linear,
            "the middle level's setting comes back"
        );
        apply(&mut c, s.canvas(), &[Restore]);
        assert_eq!(
            sampling_filter(&c),
            skia_safe::FilterMode::Nearest,
            "and the outer one's"
        );
    });
}

#[test]
fn a_context_reset_returns_smoothing_to_its_default() {
    // A canvas resize resets the context, per spec; the JavaScript side resets its shadow to match.
    let mut c = Canvas2DRenderer::new();
    with_raster_surface(4, 4, |s| {
        apply(&mut c, s.canvas(), &[SetImageSmoothing { enabled: false }]);
        assert_eq!(sampling_filter(&c), skia_safe::FilterMode::Nearest);
    });
    c.reset();
    assert_eq!(sampling_filter(&c), skia_safe::FilterMode::Linear);
}

// ====== Path fill / stroke / clip ==========================================

#[test]
fn path_fill_triangle_is_inside_bbox_only() {
    let (w, h) = (32, 32);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetFillStyle {
                    color: ProtoColor::rgb(0, 128, 0),
                },
                BeginPath,
                MoveTo { x: 16.0, y: 4.0 },
                LineTo { x: 28.0, y: 28.0 },
                LineTo { x: 4.0, y: 28.0 },
                ClosePath,
                Fill,
            ],
        );
        read_pixels_rgba8(s)
    });

    // Centroid pixel should be green; outside triangle should be white.
    let centroid = ((18 * 32 + 16) * 4) as usize;
    assert_eq!(buf[centroid + 1] > 0 && buf[centroid] < 10, true);
    let corner = 0;
    assert_eq!(&buf[corner..corner + 4], &[255, 255, 255, 255]);
    assert_matches_golden(
        "canvas2d_path_fill_triangle",
        w as u32,
        h as u32,
        &buf,
        GoldenCfg::loose_aa(w as u32, h as u32),
    );
}

#[test]
fn path_stroke_has_correct_line_cap() {
    // Render two horizontal lines with the same thickness but different
    // caps, and spot-check that `round` extends past the segment endpoint
    // while `butt` does not.
    let (w, h) = (64, 32);
    let buf_butt = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetStrokeStyle {
                    color: ProtoColor::rgb(0, 0, 0),
                },
                SetLineWidth { width: 8.0 },
                SetLineCap { cap: 0 },
                BeginPath,
                MoveTo { x: 12.0, y: 16.0 },
                LineTo { x: 52.0, y: 16.0 },
                Stroke,
            ],
        );
        read_pixels_rgba8(s)
    });
    let buf_round = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetStrokeStyle {
                    color: ProtoColor::rgb(0, 0, 0),
                },
                SetLineWidth { width: 8.0 },
                SetLineCap { cap: 1 },
                BeginPath,
                MoveTo { x: 12.0, y: 16.0 },
                LineTo { x: 52.0, y: 16.0 },
                Stroke,
            ],
        );
        read_pixels_rgba8(s)
    });
    // Sample 1 pixel outside the butt end (should stay white) and inside
    // the round cap at the same coordinate (should be dark).
    let px_idx = ((16 * 64 + 10) * 4) as usize;
    assert_eq!(buf_butt[px_idx], 255, "butt cap should not extend here");
    assert!(
        buf_round[px_idx] < 128,
        "round cap should paint here, got r={}",
        buf_round[px_idx]
    );
}

#[test]
fn clip_intersects_fillrect_to_subregion() {
    let (w, h) = (32, 32);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                // Clip to left half.
                BeginPath,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 16.0,
                    h: 32.0,
                },
                Clip,
                // Fill entire canvas red; only the left half should take.
                SetFillStyle {
                    color: ProtoColor::rgb(255, 0, 0),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 32.0,
                    h: 32.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    let left = ((16 * 32 + 8) * 4) as usize;
    let right = ((16 * 32 + 24) * 4) as usize;
    assert_eq!(&buf[left..left + 4], &[255, 0, 0, 255]);
    assert_eq!(&buf[right..right + 4], &[255, 255, 255, 255]);
}

// ====== Fill rule ==========================================================
//
// Two nested squares drawn in the same direction: the nonzero rule fills the middle (winding 2), the even-odd rule
// leaves it a hole. `FillEvenOdd` / `ClipEvenOdd` are the records `fill("evenodd")` / `clip("evenodd")` become.

fn nested_squares() -> Vec<Canvas2DCmd> {
    vec![
        BeginPath,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 40.0,
        },
        Rect {
            x: 10.0,
            y: 10.0,
            w: 20.0,
            h: 20.0,
        },
    ]
}

fn pixel(buf: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
    let i = (y * w + x) * 4;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

#[test]
fn fill_even_odd_leaves_the_middle_a_hole_and_nonzero_fills_it() {
    let render = |rule: Canvas2DCmd| {
        with_raster_surface(48, 48, |s| {
            s.canvas().clear(skia_safe::Color::WHITE);
            let mut c = Canvas2DRenderer::new();
            let mut cmds = vec![SetFillStyle {
                color: ProtoColor::rgb(255, 0, 0),
            }];
            cmds.extend(nested_squares());
            cmds.push(rule);
            apply(&mut c, s.canvas(), &cmds);
            read_pixels_rgba8(s)
        })
    };
    let nonzero = render(Fill);
    assert_eq!(
        pixel(&nonzero, 48, 20, 20),
        [255, 0, 0, 255],
        "nonzero fills the middle"
    );
    assert_eq!(pixel(&nonzero, 48, 5, 20), [255, 0, 0, 255], "and the ring");

    let even_odd = render(FillEvenOdd);
    assert_eq!(
        pixel(&even_odd, 48, 20, 20),
        [255, 255, 255, 255],
        "even-odd leaves a hole"
    );
    assert_eq!(
        pixel(&even_odd, 48, 5, 20),
        [255, 0, 0, 255],
        "and fills the ring"
    );
    assert_eq!(
        pixel(&even_odd, 48, 45, 45),
        [255, 255, 255, 255],
        "outside is untouched"
    );
}

#[test]
fn clip_even_odd_clips_out_the_hole() {
    let render = |rule: Canvas2DCmd| {
        with_raster_surface(48, 48, |s| {
            s.canvas().clear(skia_safe::Color::WHITE);
            let mut c = Canvas2DRenderer::new();
            let mut cmds = nested_squares();
            cmds.push(rule);
            cmds.push(SetFillStyle {
                color: ProtoColor::rgb(255, 0, 0),
            });
            cmds.push(FillRect {
                x: 0.0,
                y: 0.0,
                w: 48.0,
                h: 48.0,
            });
            apply(&mut c, s.canvas(), &cmds);
            read_pixels_rgba8(s)
        })
    };
    let nonzero = render(Clip);
    assert_eq!(
        pixel(&nonzero, 48, 20, 20),
        [255, 0, 0, 255],
        "a nonzero clip keeps the middle"
    );
    let even_odd = render(ClipEvenOdd);
    assert_eq!(
        pixel(&even_odd, 48, 20, 20),
        [255, 255, 255, 255],
        "an even-odd clip excludes the hole"
    );
    assert_eq!(
        pixel(&even_odd, 48, 5, 20),
        [255, 0, 0, 255],
        "and keeps the ring"
    );
}

// ====== Transform stack ===================================================

#[test]
fn translate_shifts_origin() {
    let (w, h) = (32, 32);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                Translate { x: 8.0, y: 8.0 },
                SetFillStyle {
                    color: ProtoColor::rgb(0, 0, 0),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 4.0,
                    h: 4.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    // Rect should appear at (8..12, 8..12).  Check two corners:
    let inside = ((9 * 32 + 9) * 4) as usize;
    let outside = ((2 * 32 + 2) * 4) as usize;
    assert_eq!(&buf[inside..inside + 4], &[0, 0, 0, 255]);
    assert_eq!(&buf[outside..outside + 4], &[255, 255, 255, 255]);
}

#[test]
fn rotate_90_degrees_spins_rect() {
    let (w, h) = (32, 32);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                Translate { x: 16.0, y: 16.0 },
                Rotate {
                    angle: std::f32::consts::FRAC_PI_2,
                },
                SetFillStyle {
                    color: ProtoColor::rgb(0, 0, 0),
                },
                // Rect from (-8, -1) → (8, 1) rotates 90° → (-1, -8) → (1, 8)
                FillRect {
                    x: -8.0,
                    y: -1.0,
                    w: 16.0,
                    h: 2.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    // After rotation, rect is vertical through x=16. Spot-check a pixel
    // at (16, 8) (should be black) and (8, 16) (should be white now).
    let vert = ((8 * 32 + 16) * 4) as usize;
    let horz = ((16 * 32 + 8) * 4) as usize;
    assert_eq!(&buf[vert..vert + 4], &[0, 0, 0, 255]);
    assert_eq!(&buf[horz..horz + 4], &[255, 255, 255, 255]);
}

#[test]
fn save_restore_pops_transforms() {
    let (w, h) = (32, 32);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                Save,
                Translate { x: 16.0, y: 16.0 },
                Restore,
                // After restore, origin back to (0,0); rect drawn at (0..4, 0..4)
                SetFillStyle {
                    color: ProtoColor::rgb(0, 0, 0),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 4.0,
                    h: 4.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    let origin = ((1 * 32 + 1) * 4) as usize;
    assert_eq!(&buf[origin..origin + 4], &[0, 0, 0, 255]);
}

#[test]
fn reset_transform_undoes_translate() {
    let (w, h) = (32, 32);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                Translate { x: 10.0, y: 10.0 },
                ResetTransform,
                SetFillStyle {
                    color: ProtoColor::rgb(0, 0, 0),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 4.0,
                    h: 4.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    // After reset, rect at (0..4, 0..4).
    let origin = 0;
    assert_eq!(&buf[origin..origin + 4], &[0, 0, 0, 255]);
}

// ====== Gradients =========================================================

#[test]
fn linear_gradient_left_red_right_blue() {
    let (w, h) = (64, 16);
    let stops = vec![
        GradientStop {
            offset: 0.0,
            color: ProtoColor::rgb(255, 0, 0),
        },
        GradientStop {
            offset: 1.0,
            color: ProtoColor::rgb(0, 0, 255),
        },
    ];
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetFillStyleGradient {
                    gradient_type: GradientType::Linear,
                    x0: 0.0,
                    y0: 0.0,
                    r0: 0.0,
                    x1: 64.0,
                    y1: 0.0,
                    r1: 0.0,
                    stops,
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 64.0,
                    h: 16.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    // Far left red, far right blue, midpoint roughly 50/50.
    let left = 0;
    let right = ((8 * 64 + 63) * 4) as usize;
    let mid = ((8 * 64 + 32) * 4) as usize;
    assert!(buf[left] > 240, "left r={}", buf[left]);
    assert!(buf[right + 2] > 240, "right b={}", buf[right + 2]);
    assert!(buf[mid] > 80 && buf[mid + 2] > 80);
    assert_matches_golden(
        "canvas2d_linear_gradient_red_blue",
        w as u32,
        h as u32,
        &buf,
        GoldenCfg::loose_aa(w as u32, h as u32),
    );
}

#[test]
fn radial_gradient_center_out() {
    let (w, h) = (48, 48);
    let stops = vec![
        GradientStop {
            offset: 0.0,
            color: ProtoColor::rgb(255, 255, 0),
        },
        GradientStop {
            offset: 1.0,
            color: ProtoColor::rgb(0, 0, 0),
        },
    ];
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::BLACK);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetFillStyleGradient {
                    gradient_type: GradientType::Radial,
                    x0: 24.0,
                    y0: 24.0,
                    r0: 0.0,
                    x1: 24.0,
                    y1: 24.0,
                    r1: 20.0,
                    stops,
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 48.0,
                    h: 48.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    // Center pixel should be strongly yellow (high R + G, low B).
    let c = ((24 * 48 + 24) * 4) as usize;
    assert!(buf[c] > 220 && buf[c + 1] > 220 && buf[c + 2] < 40);
    // Corner far from center should be ~black.
    let corner = 0;
    assert!(buf[corner] < 20);
}

// ====== Composite operations ==============================================

#[test]
fn composite_destination_over_preserves_existing() {
    // Paint red, then paint blue underneath with destination-over; the
    // red should stay on top.
    let (w, h) = (8, 8);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::TRANSPARENT);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetFillStyle {
                    color: ProtoColor::rgb(255, 0, 0),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 8.0,
                    h: 8.0,
                },
                SetCompositeOperation { op: 4 }, // destination-over
                SetFillStyle {
                    color: ProtoColor::rgb(0, 0, 255),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 8.0,
                    h: 8.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    // Expected: red stays visible.  With destination-over and an opaque
    // red destination, the blue would only apply where alpha was zero —
    // which is nowhere, so we stay red.
    let idx = 0;
    assert_eq!(&buf[idx..idx + 4], &[255, 0, 0, 255]);
}

#[test]
fn composite_xor_cuts_intersection() {
    // Two overlapping opaque rects with XOR: the overlap is fully erased.
    let (w, h) = (16, 16);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::TRANSPARENT);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetFillStyle {
                    color: ProtoColor::rgb(255, 0, 0),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 16.0,
                },
                SetCompositeOperation { op: 10 }, // xor
                SetFillStyle {
                    color: ProtoColor::rgb(0, 0, 255),
                },
                FillRect {
                    x: 6.0,
                    y: 0.0,
                    w: 10.0,
                    h: 16.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    // Overlap region (6..10) should be fully transparent.
    for x in 6..10 {
        let idx = ((8 * 16 + x) * 4) as usize;
        assert_eq!(buf[idx + 3], 0, "x={x} should be transparent");
    }
    // Left-only region should be red, right-only should be blue.
    let left = ((8 * 16 + 2) * 4) as usize;
    let right = ((8 * 16 + 14) * 4) as usize;
    assert_eq!(buf[left], 255);
    assert_eq!(buf[right + 2], 255);
}

#[test]
fn composite_lighter_adds_colors() {
    let (w, h) = (8, 8);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::BLACK);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetFillStyle {
                    color: ProtoColor::rgb(128, 0, 0),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 8.0,
                    h: 8.0,
                },
                SetCompositeOperation { op: 8 }, // lighter (Plus)
                SetFillStyle {
                    color: ProtoColor::rgb(0, 128, 0),
                },
                FillRect {
                    x: 0.0,
                    y: 0.0,
                    w: 8.0,
                    h: 8.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    let idx = 0;
    // Lighter = src + dst (saturated at 255). 128 + 0 for R, 0 + 128 for G.
    assert!((buf[idx] as i32 - 128).abs() <= 2, "r={}", buf[idx]);
    assert!((buf[idx + 1] as i32 - 128).abs() <= 2, "g={}", buf[idx + 1]);
}

// ====== Shadow ============================================================

#[test]
fn shadow_extends_drawing_and_is_offset() {
    let (w, h) = (32, 32);
    let buf = with_raster_surface(w, h, |s| {
        s.canvas().clear(skia_safe::Color::WHITE);
        let mut c = Canvas2DRenderer::new();
        apply(
            &mut c,
            s.canvas(),
            &[
                SetFillStyle {
                    color: ProtoColor::rgb(255, 0, 0),
                },
                SetShadowColor {
                    color: ProtoColor::rgb(0, 0, 0),
                },
                SetShadowBlur { blur: 4.0 },
                SetShadowOffsetX { offset: 4.0 },
                SetShadowOffsetY { offset: 4.0 },
                FillRect {
                    x: 8.0,
                    y: 8.0,
                    w: 8.0,
                    h: 8.0,
                },
            ],
        );
        read_pixels_rgba8(s)
    });
    // Shadow should be visible at (20, 20) — below-right of the rect.
    let shadow_idx = ((20 * 32 + 20) * 4) as usize;
    assert!(
        buf[shadow_idx] < 230,
        "shadow should darken (20,20), got r={}",
        buf[shadow_idx]
    );
    assert_matches_golden(
        "canvas2d_shadow_drop",
        w as u32,
        h as u32,
        &buf,
        GoldenCfg::loose_aa(w as u32, h as u32),
    );
}
