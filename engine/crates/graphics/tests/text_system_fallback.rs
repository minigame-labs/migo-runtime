//! A character the named fonts lack is drawn in a system font that has it, on Linux.
//!
//! The engine's Skia was built with fontconfig turned off, for a reason that stopped being true
//! once Linux shipped: "we ship SkFontMgr_Android on device and bundled fonts for headless tests".
//! What took its place is Skia's directory font manager, which lists /usr/share/fonts by name and
//! never answers "which face has this character" -- so SkParagraph's per-character fallback found
//! nothing, and every script the bundled Latin face lacks came out as boxes. A Cocos Creator game's
//! start button read as two of them, with Chinese fonts installed.
//!
//! The system here is one this test makes: a fontconfig configuration whose only font is
//! `fallback-probe.ttf`, a solid em square at U+5F00 in a family no text names. Drawn from it, the
//! character is exactly one em wide and fills its box; drawn as the bundled font's .notdef, it is
//! narrower and hollow. Its own binary, because fontconfig reads its configuration once per process.
#![cfg(all(target_os = "linux", not(target_env = "ohos")))]

mod common;

use std::sync::Arc;

use common::harness::{read_pixels_rgba8, with_raster_surface};
use graphics::backend::gl::canvas::{Canvas2DRenderer, DrawEnv};
use graphics::backend::gl::paint::NullPatternResolver;
use graphics::backend::gl::state::TextAttrs;
use graphics::backend::gl::text::TextContext;
use shared::protocol::color::Color as ProtoColor;
use shared::protocol::render_cmd::CanvasFont;
use shared::protocol::render_cmd::Canvas2DCmd::{FillText, SetFillStyle, SetFont};

const NOTO_SANS: &[u8] = include_bytes!("fixtures/fonts/NotoSans-Regular.ttf");
const PROBE: &[u8] = include_bytes!("fixtures/fonts/fallback-probe.ttf");
const SIZE: f32 = 40.0;

/// Point fontconfig at a system whose only font is the probe, before anything reads it.
fn only_the_probe_is_installed() {
    let root = std::env::temp_dir().join(format!("migo-text-fallback-{}", std::process::id()));
    let fonts = root.join("fonts");
    std::fs::create_dir_all(&fonts).unwrap();
    std::fs::write(fonts.join("fallback-probe.ttf"), PROBE).unwrap();
    let config = root.join("fonts.conf");
    std::fs::write(
        &config,
        format!(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n<fontconfig>\n  <dir>{}</dir>\n  <cachedir>{}</cachedir>\n</fontconfig>\n",
            fonts.display(),
            root.join("cache").display()
        ),
    )
    .unwrap();
    // SAFETY: the one test in this binary, before it starts a thread or touches fontconfig.
    unsafe { std::env::set_var("FONTCONFIG_FILE", &config) };
}

#[test]
fn a_character_the_named_fonts_lack_is_drawn_from_a_system_font_that_has_it() {
    only_the_probe_is_installed();
    let mut text = TextContext::new();
    assert!(text.register_family("test-noto", NOTO_SANS));
    let families = Arc::new(vec!["test-noto".to_string(), "sans-serif".to_string()]);

    let attrs = TextAttrs {
        size: SIZE,
        families: families.clone(),
        ..TextAttrs::default()
    };
    let width = text.measure_text("\u{5F00}", &attrs).width;
    assert!(
        (width - SIZE).abs() < 0.5,
        "U+5F00 measures {width} px at {SIZE} px: not the probe's one-em square, so it was not found in the system"
    );

    let (w, h) = (64, 64);
    let pixels = with_raster_surface(w, h, |surface| {
        surface.canvas().clear(skia_safe::Color::WHITE);
        let resolver = NullPatternResolver;
        let env = DrawEnv { canvas: surface.canvas(), text: Some(&text), resolver: &resolver };
        let mut ctx = Canvas2DRenderer::new();
        for cmd in [
            SetFont { font: CanvasFont { size: SIZE, weight: 400, italic: false, families: families.clone() } },
            SetFillStyle { color: ProtoColor::rgb(0, 0, 0) },
            FillText { text: "\u{5F00}".into(), x: 8.0, y: 48.0, max_width: f32::INFINITY },
        ] {
            ctx.apply_env(&env, &cmd);
        }
        read_pixels_rgba8(surface)
    });
    // The square spans x 8..48 and, at an ascent of 0.8 em, y 16..48: 1,280 px, nearly all ink.
    let ink = pixels.chunks_exact(4).filter(|p| p[0] < 64 && p[1] < 64 && p[2] < 64).count();
    assert!(ink > 1_100, "{ink} dark pixels: the character was not drawn from the probe's solid square");
}
