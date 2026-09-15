//! Canvas2D text rendering — golden pixel tests.
//!
//! Exercised scenarios (all via the CPU raster harness):
//!   * `fillText` — ASCII + CJK + emoji glyph coverage
//!   * `textAlign` — start/center/right anchor positioning
//!   * `textBaseline` — alphabetic/top/bottom offsets
//!   * `strokeText` — outlined glyphs
//!   * `measureText` — width, ascent, descent
//!   * `font-size` and `font-weight` style application
//!
//! Tests that depend on specific shaping (CJK/emoji) require additional
//! test fonts and are currently gated behind `#[ignore]` until a compact
//! subset ships in `tests/fixtures/fonts/`.

#[path = "common/mod.rs"]
mod common;

// Apple has no use for these: the one golden comparison in this file is
// `#[cfg(not(target_vendor = "apple"))]`, for a reason stated at that call.
#[cfg(not(target_vendor = "apple"))]
use common::golden::{GoldenCfg, assert_matches_golden};
use common::harness::{read_pixels_rgba8, with_raster_surface};

use shared::protocol::color::Color as ProtoColor;
use shared::protocol::render_cmd::Canvas2DCmd::{self, *};
use shared::protocol::render_cmd::{TextAlign, TextBaseline};

use graphics::backend::gl::canvas::{Canvas2DRenderer, DrawEnv};
use graphics::backend::gl::paint::NullPatternResolver;
use graphics::backend::gl::text::TextContext;

const NOTO_SANS: &[u8] = include_bytes!("fixtures/fonts/NotoSans-Regular.ttf");

/// Test-local scaffold that registers the bundled test font under the
/// `test-noto` family name.  Tests should use `families: ["test-noto",
/// "sans-serif"]` in their commands so the first-resolving family is
/// always our bundled typeface — independent of whatever system fonts
/// happen to be installed on the CI machine.
fn with_text_env<T>(f: impl FnOnce(&TextContext) -> T) -> T {
    let mut text = TextContext::new();
    assert!(text.register_family("test-noto", NOTO_SANS));
    f(&text)
}

fn apply_env(
    ctx: &mut Canvas2DRenderer,
    env: &DrawEnv<'_, NullPatternResolver>,
    cmds: &[Canvas2DCmd],
) {
    for c in cmds {
        ctx.apply_env(env, c);
    }
}

/// `SetFont` is just a CSS-font string at the protocol layer; until the
/// parser arrives (tracked by a separate protocol PR) tests modify
/// `Canvas2DRenderer::state.text` directly.  This helper keeps that local
/// to tests so production code never reaches into private state.
fn set_font(ctx: &mut Canvas2DRenderer, size: f32) {
    ctx.state.text.families = std::sync::Arc::new(vec!["test-noto".into(), "sans-serif".into()]);
    ctx.state.text.size = size;
}

#[test]
fn fill_text_draws_non_empty_pixels() {
    let (w, h) = (128, 32);
    let buf = with_raster_surface(w, h, |surface| {
        surface.canvas().clear(skia_safe::Color::WHITE);
        with_text_env(|text| {
            let resolver = NullPatternResolver;
            let env = DrawEnv {
                canvas: surface.canvas(),
                text: Some(text),
                resolver: &resolver,
            };
            let mut ctx = Canvas2DRenderer::new();
            set_font(&mut ctx, 20.0);
            apply_env(
                &mut ctx,
                &env,
                &[
                    SetFillStyle {
                        color: ProtoColor::rgb(0, 0, 0),
                    },
                    FillText {
                        text: "Hello".into(),
                        x: 8.0,
                        y: 24.0,
                        max_width: f32::INFINITY,
                    },
                ],
            );
            read_pixels_rgba8(surface)
        })
    });

    // Verify SOME pixels are not white (glyphs were rasterised).
    let non_white = buf
        .chunks_exact(4)
        .filter(|c| c[0] < 250 || c[1] < 250 || c[2] < 250)
        .count();
    assert!(
        non_white > 20,
        "expected >=20 non-white pixels for rendered text, got {non_white}"
    );
    // The golden is a rasteriser's, not a renderer's, and text is the one thing
    // in this suite that differs between them. The outlines are identical
    // everywhere -- the font is bundled, not the system's -- but hinting, gamma
    // and edge coverage are the scaler's, and Skia's scaler is FreeType on Linux
    // and Android and CoreText on Apple.
    //
    // Measured 2026-09-11, macOS 26.6 against the committed golden: 300 of 4096
    // pixels outside a tolerance of 2, max delta 203, every one of them a glyph
    // edge (a first mismatch of [214,214,214] where the golden has white).
    // Comparing there would assert that CoreText is FreeType.
    //
    // What is NOT skipped is everything that is about this renderer rather than
    // that scaler: the non-white count above, and the two positioning tests
    // below, which assert where the ink lands relative to the anchor and pass on
    // every platform. A per-platform golden was considered and rejected for now:
    // it would have to be generated on the CI runner rather than on a developer's
    // Mac -- this lane's runner is arm64 macOS 15 and the machine that would
    // produce it is Intel macOS 26 -- so it would be a golden nobody could
    // regenerate from the machine that failed it.
    #[cfg(not(target_vendor = "apple"))]
    assert_matches_golden(
        "canvas2d_fill_text_hello",
        w as u32,
        h as u32,
        &buf,
        GoldenCfg::loose_aa(w as u32, h as u32),
    );
}

#[test]
fn text_align_shifts_x_anchor() {
    // At `textAlign=right` with the same x/y anchor, text should render
    // entirely to the LEFT of x (so pixels at x+8 are white).
    let (w, h) = (200, 32);
    let with_align = |align: TextAlign| -> Vec<u8> {
        with_raster_surface(w, h, |surface| {
            surface.canvas().clear(skia_safe::Color::WHITE);
            with_text_env(|text| {
                let resolver = NullPatternResolver;
                let env = DrawEnv {
                    canvas: surface.canvas(),
                    text: Some(text),
                    resolver: &resolver,
                };
                let mut ctx = Canvas2DRenderer::new();
                set_font(&mut ctx, 18.0);
                apply_env(
                    &mut ctx,
                    &env,
                    &[
                        SetTextAlign { align },
                        SetFillStyle {
                            color: ProtoColor::rgb(0, 0, 0),
                        },
                        FillText {
                            text: "Right".into(),
                            x: 100.0,
                            y: 24.0,
                            max_width: f32::INFINITY,
                        },
                    ],
                );
                read_pixels_rgba8(surface)
            })
        })
    };

    let left_buf = with_align(TextAlign::Left);
    let right_buf = with_align(TextAlign::Right);

    // Each row has `w` pixels × 4 bytes.  Compute the horizontal centre
    // of mass of dark pixels across all rows — that indicates where the
    // text actually landed.
    let dark_centroid_x = |buf: &[u8]| -> f32 {
        let mut sum_x = 0f64;
        let mut n = 0u32;
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                if buf[i] < 150 {
                    sum_x += x as f64;
                    n += 1;
                }
            }
        }
        if n == 0 {
            f32::NAN
        } else {
            (sum_x / n as f64) as f32
        }
    };

    let left_cx = dark_centroid_x(&left_buf);
    let right_cx = dark_centroid_x(&right_buf);

    assert!(left_cx.is_finite(), "align=left rendered no visible text");
    assert!(right_cx.is_finite(), "align=right rendered no visible text");
    // align=right must shift the text LEFT relative to align=left by the
    // full text width, so the right-variant centroid is strictly lower.
    assert!(
        right_cx < left_cx - 20.0,
        "align=right centroid {right_cx} should be well left of align=left {left_cx}"
    );
}

#[test]
fn text_baseline_shifts_y_anchor() {
    // textBaseline=top anchors the TOP of the text box at y, so the glyph
    // appears BELOW the anchor.  textBaseline=bottom anchors the BOTTOM
    // of the text box at y, glyph appears ABOVE.  Compare both variants.
    let (w, h) = (64, 64);
    let with_baseline = |bl: TextBaseline| -> Vec<u8> {
        with_raster_surface(w, h, |surface| {
            surface.canvas().clear(skia_safe::Color::WHITE);
            with_text_env(|text| {
                let resolver = NullPatternResolver;
                let env = DrawEnv {
                    canvas: surface.canvas(),
                    text: Some(text),
                    resolver: &resolver,
                };
                let mut ctx = Canvas2DRenderer::new();
                set_font(&mut ctx, 18.0);
                apply_env(
                    &mut ctx,
                    &env,
                    &[
                        SetTextBaseline { baseline: bl },
                        SetFillStyle {
                            color: ProtoColor::rgb(0, 0, 0),
                        },
                        FillText {
                            text: "X".into(),
                            x: 24.0,
                            y: 32.0,
                            max_width: f32::INFINITY,
                        },
                    ],
                );
                read_pixels_rgba8(surface)
            })
        })
    };

    let top = with_baseline(TextBaseline::Top);
    let bottom = with_baseline(TextBaseline::Bottom);

    // Count non-white pixels in the upper vs lower half of the canvas.
    let half = (h / 2) * w * 4;
    let darken =
        |slice: &[u8]| -> u32 { slice.chunks_exact(4).filter(|c| c[0] < 220).count() as u32 };
    let top_upper = darken(&top[..half as usize]);
    let top_lower = darken(&top[half as usize..]);
    let bot_upper = darken(&bottom[..half as usize]);
    let bot_lower = darken(&bottom[half as usize..]);

    assert!(
        top_lower > top_upper,
        "baseline=top should put glyph below anchor (upper={top_upper}, lower={top_lower})"
    );
    assert!(
        bot_upper > bot_lower,
        "baseline=bottom should put glyph above anchor (upper={bot_upper}, lower={bot_lower})"
    );
}

#[test]
fn stroke_text_produces_outline_not_fill() {
    let (w, h) = (80, 32);
    let stroke_buf = with_raster_surface(w, h, |surface| {
        surface.canvas().clear(skia_safe::Color::WHITE);
        with_text_env(|text| {
            let resolver = NullPatternResolver;
            let env = DrawEnv {
                canvas: surface.canvas(),
                text: Some(text),
                resolver: &resolver,
            };
            let mut ctx = Canvas2DRenderer::new();
            set_font(&mut ctx, 24.0);
            apply_env(
                &mut ctx,
                &env,
                &[
                    SetStrokeStyle {
                        color: ProtoColor::rgb(0, 0, 0),
                    },
                    SetLineWidth { width: 1.0 },
                    StrokeText {
                        text: "O".into(),
                        x: 8.0,
                        y: 26.0,
                        max_width: f32::INFINITY,
                    },
                ],
            );
            read_pixels_rgba8(surface)
        })
    });

    // At least some dark pixels (the outline).
    let dark = stroke_buf.chunks_exact(4).filter(|c| c[0] < 60).count();
    assert!(
        dark >= 8,
        "stroke outline should produce ≥8 dark pixels, got {dark}"
    );
}

#[test]
fn measure_text_tracks_characters() {
    with_text_env(|text| {
        let attrs = graphics::backend::gl::state::TextAttrs {
            size: 20.0,
            families: std::sync::Arc::new(vec!["test-noto".into(), "sans-serif".into()]),
            weight: 400,
            italic: false,
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            direction: shared::protocol::render_cmd::TextDirection::Inherit,
        };
        let short = text.measure_text("i", &attrs);
        let long = text.measure_text("Lorem ipsum dolor sit amet", &attrs);
        assert!(long.width > short.width * 10.0);
        assert_eq!(text.measure_text("", &attrs).width, 0.0);
    });
}

#[test]
fn max_width_scales_long_text_horizontally() {
    // FillText with maxWidth < intrinsic width should fit inside.
    let (w, h) = (256, 32);
    let buf = with_raster_surface(w, h, |surface| {
        surface.canvas().clear(skia_safe::Color::WHITE);
        with_text_env(|text| {
            let resolver = NullPatternResolver;
            let env = DrawEnv {
                canvas: surface.canvas(),
                text: Some(text),
                resolver: &resolver,
            };
            let mut ctx = Canvas2DRenderer::new();
            set_font(&mut ctx, 24.0);
            apply_env(
                &mut ctx,
                &env,
                &[
                    SetFillStyle {
                        color: ProtoColor::rgb(0, 0, 0),
                    },
                    FillText {
                        text: "The quick brown fox jumps over the lazy dog".into(),
                        x: 8.0,
                        y: 26.0,
                        max_width: 120.0,
                    },
                ],
            );
            read_pixels_rgba8(surface)
        })
    });

    // All non-white pixels must sit within x=0..(8+120+padding) = x<140.
    let mut rightmost_dark = 0i32;
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if buf[i] < 200 {
                if x > rightmost_dark {
                    rightmost_dark = x;
                }
            }
        }
    }
    assert!(
        rightmost_dark < 140,
        "text should be scaled to fit in maxWidth=120, rightmost dark px at x={rightmost_dark}"
    );
}
