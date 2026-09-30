//! Canvas2D `globalCompositeOperation` ↔ Skia `BlendMode` mapping.
//!
//! The JS layer serialises [Canvas 2D spec compositing modes][spec] as a
//! stable `u8` code (see `_COMPOSITE_OPS` in
//! `engine/crates/runtime-v8/rendering/webgl/02_2d_context.js`).  This module
//! is the single source of truth translating that opcode into a
//! [`skia_safe::BlendMode`] at the render-thread boundary.
//!
//! Unknown / future codes fall back to `SrcOver` (the spec default) rather
//! than crashing — consistent with browser behaviour for unrecognised values.
//!
//! Notably:
//!   * `"lighter"`  → `BlendMode::Plus` (Skia calls additive blending "plus",
//!                    the spec calls it "lighter")
//!   * `"copy"`     → `BlendMode::Src`
//!
//! The 16 advanced non-separable / hue-chroma modes (`multiply`, `screen`,
//! `hue`, `saturation`, `color`, `luminosity`, etc.) are passed straight
//! through to Skia; the 11-entry legacy table used by femtovg is a strict
//! subset of what we expose here.
//!
//! [spec]: https://html.spec.whatwg.org/multipage/canvas.html#compositing

use skia_safe::BlendMode;

/// All 26 HTML Canvas 2D compositing operations, indexed by stable `u8` code.
///
/// Invariants verified by `tests::table_is_complete_and_ordered`:
///
/// * `TABLE[0]`  is the HTML default `"source-over"` → `BlendMode::SrcOver`
/// * indices `0..=10` match the legacy femtovg table exactly, so JS code that
///   predates the expansion still behaves correctly
/// * entries `11..=25` match the spec order of the 15 advanced modes
const TABLE: &[(&str, BlendMode); 26] = &[
    // Porter-Duff (11, indices 0..=10) -----------------------------------
    ("source-over", BlendMode::SrcOver),
    ("source-in", BlendMode::SrcIn),
    ("source-out", BlendMode::SrcOut),
    ("source-atop", BlendMode::SrcATop),
    ("destination-over", BlendMode::DstOver),
    ("destination-in", BlendMode::DstIn),
    ("destination-out", BlendMode::DstOut),
    ("destination-atop", BlendMode::DstATop),
    ("lighter", BlendMode::Plus),
    ("copy", BlendMode::Src),
    ("xor", BlendMode::Xor),
    // Advanced separable (11..=21) ---------------------------------------
    ("multiply", BlendMode::Multiply),
    ("screen", BlendMode::Screen),
    ("overlay", BlendMode::Overlay),
    ("darken", BlendMode::Darken),
    ("lighten", BlendMode::Lighten),
    ("color-dodge", BlendMode::ColorDodge),
    ("color-burn", BlendMode::ColorBurn),
    ("hard-light", BlendMode::HardLight),
    ("soft-light", BlendMode::SoftLight),
    ("difference", BlendMode::Difference),
    ("exclusion", BlendMode::Exclusion),
    // Non-separable / hue-chroma (22..=25) -------------------------------
    ("hue", BlendMode::Hue),
    ("saturation", BlendMode::Saturation),
    ("color", BlendMode::Color),
    ("luminosity", BlendMode::Luminosity),
];

/// Decode a compositing-operation opcode into a Skia [`BlendMode`].
///
/// Unknown codes return `BlendMode::SrcOver` — the spec default and the same
/// behaviour browsers exhibit when assigned an unrecognised string.
#[inline]
pub fn blend_mode_from_code(op: u8) -> BlendMode {
    TABLE
        .get(op as usize)
        .map(|(_, m)| *m)
        .unwrap_or(BlendMode::SrcOver)
}

/// Decode a compositing-operation opcode into its spec name.
/// Returns `"source-over"` for unknown codes.
///
/// Exposed primarily for tracing / debug tooling.
#[inline]
pub fn name_from_code(op: u8) -> &'static str {
    TABLE
        .get(op as usize)
        .map(|(n, _)| *n)
        .unwrap_or("source-over")
}

/// Whether `mode` changes the destination *outside* the shape being drawn.
///
/// HTML composites every draw as if the shape were an infinite bitmap that is
/// transparent beyond it. For most operators a transparent source leaves the
/// destination alone, so only the shape's own pixels matter. For these five it
/// does not -- `source-in`, `source-out`, `destination-in`, `destination-atop`
/// and `copy` all produce transparent black where the source is transparent --
/// so drawing a small shape under one of them clears everything else the clip
/// allows. `destination-in` with a circle is how a circular crop is made.
/// Skia applies a draw only within the geometry's coverage, so these draws need
/// the layer in [`in_full_canvas_layer`] to reach the rest of the canvas.
#[inline]
pub fn is_full_canvas_composite(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::SrcIn
            | BlendMode::SrcOut
            | BlendMode::DstIn
            | BlendMode::DstATop
            | BlendMode::Src
    )
}

/// Run `draw` into a layer and composite the layer over the whole clip with
/// `mode`.
///
/// `draw` must paint with `SrcOver` (the layer is empty, so that is "the shape,
/// alone"); the layer is then applied to everything the clip allows, transparent
/// pixels included, which is what makes an unbounded operator reach beyond the
/// shape. The current matrix and clip are the layer's own, so both apply exactly
/// as they do to an ordinary draw.
pub fn in_full_canvas_layer<T>(
    canvas: &skia_safe::Canvas,
    mode: BlendMode,
    draw: impl FnOnce() -> T,
) -> T {
    begin_full_canvas_layer(canvas, mode);
    let painted = draw();
    canvas.restore();
    painted
}

/// The opening half of [`in_full_canvas_layer`], for a caller that cannot hold
/// the canvas borrow across its draw. Must be paired with `canvas.restore()`.
pub fn begin_full_canvas_layer(canvas: &skia_safe::Canvas, mode: BlendMode) {
    let mut layer_paint = skia_safe::Paint::default();
    layer_paint.set_blend_mode(mode);
    canvas.save_layer(&skia_safe::canvas::SaveLayerRec::default().paint(&layer_paint));
}

/// Number of valid compositing-operation codes (0..`OP_COUNT`).
pub const OP_COUNT: u8 = TABLE.len() as u8;

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::BlendMode::*;

    #[test]
    fn table_is_complete_and_ordered() {
        // The 26-entry table must stay in sync with the WHATWG Canvas 2D spec
        // section "compositing" order, AND indices 0..=10 must stay byte-for-
        // byte identical to the legacy 11-entry femtovg table so that JS
        // bytecode compiled against the old numbering continues to work.
        let names: Vec<&str> = TABLE.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            &names[..11],
            &[
                "source-over",
                "source-in",
                "source-out",
                "source-atop",
                "destination-over",
                "destination-in",
                "destination-out",
                "destination-atop",
                "lighter",
                "copy",
                "xor",
            ]
        );
        assert_eq!(TABLE.len(), OP_COUNT as usize);
        assert_eq!(OP_COUNT, 26);
    }

    #[test]
    fn source_over_is_the_default() {
        assert_eq!(blend_mode_from_code(0), SrcOver);
        assert_eq!(name_from_code(0), "source-over");
    }

    #[test]
    fn lighter_maps_to_plus_not_screen() {
        // Canvas "lighter" is additive saturated blend (Skia `Plus`), NOT
        // the separable "screen" mode.  Regression check for a common bug.
        assert_eq!(blend_mode_from_code(8), Plus);
        assert_eq!(name_from_code(8), "lighter");
        assert_ne!(blend_mode_from_code(8), Screen);
    }

    #[test]
    fn copy_maps_to_src_not_clear() {
        assert_eq!(blend_mode_from_code(9), Src);
        assert_eq!(name_from_code(9), "copy");
    }

    #[test]
    fn porter_duff_modes_11_entries() {
        use BlendMode as B;
        let expected = [
            B::SrcOver,
            B::SrcIn,
            B::SrcOut,
            B::SrcATop,
            B::DstOver,
            B::DstIn,
            B::DstOut,
            B::DstATop,
            B::Plus,
            B::Src,
            B::Xor,
        ];
        for (op, mode) in expected.iter().enumerate() {
            assert_eq!(
                blend_mode_from_code(op as u8),
                *mode,
                "op code {op} ({}) mismatched",
                name_from_code(op as u8),
            );
        }
    }

    #[test]
    fn advanced_separable_modes_11_to_21() {
        use BlendMode as B;
        let expected = [
            (11, B::Multiply),
            (12, B::Screen),
            (13, B::Overlay),
            (14, B::Darken),
            (15, B::Lighten),
            (16, B::ColorDodge),
            (17, B::ColorBurn),
            (18, B::HardLight),
            (19, B::SoftLight),
            (20, B::Difference),
            (21, B::Exclusion),
        ];
        for (op, mode) in expected {
            assert_eq!(blend_mode_from_code(op), mode);
        }
    }

    #[test]
    fn non_separable_hue_chroma_modes_22_to_25() {
        use BlendMode as B;
        assert_eq!(blend_mode_from_code(22), B::Hue);
        assert_eq!(blend_mode_from_code(23), B::Saturation);
        assert_eq!(blend_mode_from_code(24), B::Color);
        assert_eq!(blend_mode_from_code(25), B::Luminosity);
    }

    #[test]
    fn unknown_opcodes_fall_back_to_source_over() {
        for op in [26u8, 27, 42, 100, 200, 255] {
            assert_eq!(
                blend_mode_from_code(op),
                SrcOver,
                "op={op} should fall back to SrcOver",
            );
            assert_eq!(name_from_code(op), "source-over");
        }
    }

    #[test]
    fn all_names_unique() {
        let mut seen = std::collections::HashSet::new();
        for (n, _) in TABLE.iter() {
            assert!(seen.insert(*n), "duplicate name {n}");
        }
    }

    #[test]
    fn legacy_11_entry_prefix_matches_femtovg_composite_operation_order() {
        // femtovg::CompositeOperation numeric order (from femtovg 0.22 source):
        //   0 SourceOver, 1 SourceIn, 2 SourceOut, 3 SourceAtop,
        //   4 DestinationOver, 5 DestinationIn, 6 DestinationOut,
        //   7 DestinationAtop, 8 Lighter, 9 Copy, 10 Xor
        // This test locks that prefix in; breaking it silently corrupts any
        // JS bytecode that was compiled assuming the old numbering.
        let legacy_names = [
            "source-over",
            "source-in",
            "source-out",
            "source-atop",
            "destination-over",
            "destination-in",
            "destination-out",
            "destination-atop",
            "lighter",
            "copy",
            "xor",
        ];
        for (op, expected_name) in legacy_names.iter().enumerate() {
            assert_eq!(name_from_code(op as u8), *expected_name);
        }
    }

    /// Raster-surface behaviour of the layer: a blue 20x20 destination at the
    /// origin, a red 20x20 source at (10,10), each operator's three regions.
    #[test]
    fn unbounded_operators_reach_outside_the_shape() {
        let read = |surface: &mut skia_safe::Surface, x: i32, y: i32| -> [u8; 4] {
            let info = skia_safe::ImageInfo::new(
                (1, 1),
                skia_safe::ColorType::RGBA8888,
                skia_safe::AlphaType::Unpremul,
                None,
            );
            let mut px = [0u8; 4];
            assert!(surface.read_pixels(&info, &mut px, 4, (x, y)));
            px
        };
        const T: [u8; 4] = [0, 0, 0, 0];
        const BLUE: [u8; 4] = [0, 0, 255, 255];
        const RED: [u8; 4] = [255, 0, 0, 255];
        // (mode, destination-only, overlap, source-only)
        let cases = [
            (SrcIn, T, RED, T),
            (SrcOut, T, T, RED),
            (DstIn, T, BLUE, T),
            (DstATop, T, BLUE, RED),
            (Src, T, RED, RED),
        ];
        for (mode, dst_only, overlap, src_only) in cases {
            assert!(is_full_canvas_composite(mode), "{mode:?}");
            let mut surface = skia_safe::surfaces::raster_n32_premul((40, 40)).unwrap();
            let canvas = surface.canvas();
            let mut p = skia_safe::Paint::default();
            p.set_color(skia_safe::Color::from_argb(255, 0, 0, 255));
            canvas.draw_rect(skia_safe::Rect::from_xywh(0.0, 0.0, 20.0, 20.0), &p);
            in_full_canvas_layer(canvas, mode, || {
                let mut p = skia_safe::Paint::default();
                p.set_color(skia_safe::Color::from_argb(255, 255, 0, 0));
                canvas.draw_rect(skia_safe::Rect::from_xywh(10.0, 10.0, 20.0, 20.0), &p);
            });
            assert_eq!(
                read(&mut surface, 5, 5),
                dst_only,
                "{mode:?} destination only"
            );
            assert_eq!(read(&mut surface, 15, 15), overlap, "{mode:?} overlap");
            assert_eq!(read(&mut surface, 25, 25), src_only, "{mode:?} source only");
        }
    }

    /// The rest are bounded: a transparent source leaves the destination alone,
    /// so they must not pay for a layer.
    #[test]
    fn bounded_operators_are_left_to_the_ordinary_draw() {
        for mode in [
            SrcOver, DstOver, DstOut, SrcATop, Xor, Plus, Multiply, Screen, Darken, Lighten,
            Difference, Exclusion,
        ] {
            assert!(!is_full_canvas_composite(mode), "{mode:?}");
        }
    }

    /// A clip limits the reach: outside it nothing changes, even for `copy`.
    #[test]
    fn the_layer_respects_the_clip() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((40, 40)).unwrap();
        let canvas = surface.canvas();
        let mut p = skia_safe::Paint::default();
        p.set_color(skia_safe::Color::from_argb(255, 0, 0, 255));
        canvas.draw_rect(skia_safe::Rect::from_xywh(0.0, 0.0, 40.0, 40.0), &p);
        canvas.save();
        canvas.clip_rect(
            skia_safe::Rect::from_xywh(10.0, 10.0, 20.0, 20.0),
            None,
            None,
        );
        in_full_canvas_layer(canvas, Src, || {
            let mut p = skia_safe::Paint::default();
            p.set_color(skia_safe::Color::from_argb(255, 255, 0, 0));
            canvas.draw_rect(skia_safe::Rect::from_xywh(12.0, 12.0, 5.0, 5.0), &p);
        });
        canvas.restore();
        let info = skia_safe::ImageInfo::new(
            (1, 1),
            skia_safe::ColorType::RGBA8888,
            skia_safe::AlphaType::Unpremul,
            None,
        );
        let mut px = [0u8; 4];
        surface.read_pixels(&info, &mut px, 4, (5, 5));
        assert_eq!(px, [0, 0, 255, 255], "outside the clip is untouched");
        surface.read_pixels(&info, &mut px, 4, (25, 25));
        assert_eq!(
            px,
            [0, 0, 0, 0],
            "inside the clip, outside the shape, copy clears"
        );
        surface.read_pixels(&info, &mut px, 4, (14, 14));
        assert_eq!(px, [255, 0, 0, 255], "the shape");
    }
}
