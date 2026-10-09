//! Thread-safe measureText façade.
//!
//! The render thread's `TextContext` owns HarfBuzz + ICU + Skia
//! shaping state that is naturally on the render side for
//! `fillText` / `strokeText` (both need a live GL canvas).  But
//! `measureText` / `getTextLineHeight` — the hot path for UI
//! auto-sizing — never touches GL; making JS go through a
//! cross-thread RPC just to read a cached paragraph metric is
//! pure latency.
//!
//! This module exposes the trait the JS-thread measure op goes
//! through, plus a lightweight shared-handle type, so:
//!
//!   * `shared` stays free of `skia-safe` (no cycle into
//!     `graphics` would be possible otherwise).
//!   * `graphics` provides the concrete implementation (registered
//!     at render-thread startup) and hides the Skia types behind
//!     the trait.
//!   * `runtime-v8` holds an `Arc<dyn TextMeasurer>` on
//!     `CanvasOpState` and skips the RenderCommand round-trip for
//!     every `op_measure_text*` call, falling back to the
//!     existing sync-op path only when the handle is missing
//!     (older tests / custom embedders).
//!
//! Font state parity: the trait takes **parsed** font attrs on
//! every call (family, size, weight, italic) so the JS side can
//! drive measurement without having to keep the CSS `ctx.font`
//! parser consistent with the graphics crate.  The concrete
//! implementation owns a `parking_lot::Mutex<TextContext>`
//! internally, and `op_load_font` dispatches through the trait
//! to keep JS and render-thread views of the font registry in
//! sync.

use crate::protocol::render_cmd::TextMetrics;

/// Thread-safe measurement handle.
///
/// Implementations are expected to wrap an internally-mutable
/// shaping context (typically `parking_lot::Mutex<TextContext>`)
/// so the trait can be `Send + Sync` even when the underlying
/// Skia handles aren't individually thread-safe — serialised
/// access through the mutex is what makes it OK to move between
/// threads.
///
/// The trait is **intentionally minimal**: only the operations
/// the JS hot path actually needs (measure + line-height + font
/// registration).  Adding new methods is a wire-format break
/// between `shared` and the `graphics` impl, which is why
/// `#[non_exhaustive]`-style guarantees aren't offered.
pub trait TextMeasurer: Send + Sync + 'static {
    /// Measure `text` using the given font descriptor.
    ///
    /// `families` is the whole CSS family list, head first
    /// (`ctx.font` post-split), and it is resolved exactly as a
    /// `fillText` of the same font resolves it. It used to be the
    /// head alone, so a list whose first name the device lacks
    /// (`"Microsoft YaHei", serif`) measured one face and painted
    /// another, and text laid out from the measurement did not fit
    /// what was drawn. `weight` and `italic` come from the
    /// shorthand parser.  Returns the same `TextMetrics`
    /// shape the `Canvas2DCmd::MeasureText` path produces, so
    /// the JS side doesn't have to branch on which path served
    /// the metric.
    fn measure(
        &self,
        text: &str,
        families: &std::sync::Arc<Vec<String>>,
        font_size: f32,
        weight: u16,
        italic: bool,
    ) -> TextMetrics;

    /// Line-height helper paralleling `RenderCommand::GetTextLineHeight`.
    fn line_height(
        &self,
        families: &std::sync::Arc<Vec<String>>,
        font_size: f32,
        weight: u16,
        italic: bool,
    ) -> f32;

    /// Register a font byte blob under one or more aliases.
    /// Returns the canonical family name (typically the font's
    /// internal `name` table entry) on success, or `None` on
    /// parse failure.
    fn register_font(&self, aliases: &[String], bytes: &[u8]) -> Option<String>;

    /// Convenience overload that takes a raw CSS font
    /// shorthand and forwards it through [`crate::css_font::
    /// parse_css_font`] so callers on the JS thread don't need
    /// their own parser.  Default impl so existing implementors
    /// pick it up automatically.
    fn measure_css(&self, text: &str, css_font: &str) -> TextMetrics {
        let f = parse_family_list(css_font);
        self.measure(text, &f.families, f.size, f.weight, f.italic)
    }

    /// Companion to [`Self::measure_css`] — same parse flow
    /// but for `getTextLineHeight`.
    fn line_height_css(&self, css_font: &str) -> f32 {
        let f = parse_family_list(css_font);
        self.line_height(&f.families, f.size, f.weight, f.italic)
    }
}

/// A CSS `font` string as the measurer needs it: the whole family list, parsed
/// by the parser the paint side uses, so a measurement and the `fillText` that
/// follows it resolve the same list.
///
/// Input that parser rejects goes through the lenient one, which has always
/// answered something for any string; its single family is the whole list.
struct FamilyListFont {
    families: std::sync::Arc<Vec<String>>,
    size: f32,
    weight: u16,
    italic: bool,
}

fn parse_family_list(css_font: &str) -> FamilyListFont {
    if let Some(p) = crate::css_font_shorthand::parse_font_shorthand(css_font) {
        return FamilyListFont {
            families: std::sync::Arc::new(p.families),
            size: p.size_px,
            weight: p.weight,
            italic: p.italic,
        };
    }
    let p = crate::css_font::parse_css_font(css_font);
    FamilyListFont {
        families: std::sync::Arc::new(vec![p.family]),
        size: p.size,
        weight: p.weight,
        italic: p.italic,
    }
}

/// Process-wide shared handle.  Cheap to `Clone`: single
/// refcount bump; every clone dispatches through the same
/// underlying mutex-guarded context.
pub type SharedTextMeasurer = std::sync::Arc<dyn TextMeasurer>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn zero_metrics() -> TextMetrics {
        TextMetrics {
            width: 0.0,
            actual_bounding_box_left: 0.0,
            actual_bounding_box_right: 0.0,
            actual_bounding_box_ascent: 0.0,
            actual_bounding_box_descent: 0.0,
            font_bounding_box_ascent: 0.0,
            font_bounding_box_descent: 0.0,
            em_height_ascent: 0.0,
            em_height_descent: 0.0,
            hanging_baseline: 0.0,
            alphabetic_baseline: 0.0,
            ideographic_baseline: 0.0,
        }
    }

    /// Records what the default `*_css` methods hand to the implementation.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<(Vec<String>, f32, u16, bool)>>,
    }

    impl TextMeasurer for Recorder {
        fn measure(
            &self,
            _text: &str,
            families: &Arc<Vec<String>>,
            font_size: f32,
            weight: u16,
            italic: bool,
        ) -> TextMetrics {
            self.seen
                .lock()
                .unwrap()
                .push((families.to_vec(), font_size, weight, italic));
            zero_metrics()
        }

        fn line_height(
            &self,
            families: &Arc<Vec<String>>,
            font_size: f32,
            weight: u16,
            italic: bool,
        ) -> f32 {
            self.seen
                .lock()
                .unwrap()
                .push((families.to_vec(), font_size, weight, italic));
            0.0
        }

        fn register_font(&self, _aliases: &[String], _bytes: &[u8]) -> Option<String> {
            None
        }
    }

    /// A measurement must resolve the list a `fillText` of the same font does.
    /// It used to receive the head alone, so `"Microsoft YaHei", serif` measured
    /// one face where it painted another.
    #[test]
    fn a_measurement_gets_the_whole_family_list() {
        let r = Recorder::default();
        r.measure_css(
            "x",
            "italic bold 18px 'Microsoft YaHei', \"Noto Serif\", serif",
        );
        r.line_height_css("14px Arial, sans-serif");
        let seen = r.seen.lock().unwrap();
        assert_eq!(
            seen[0],
            (
                vec![
                    "Microsoft YaHei".to_string(),
                    "Noto Serif".to_string(),
                    "serif".to_string()
                ],
                18.0,
                700,
                true
            )
        );
        assert_eq!(
            seen[1].0,
            vec!["Arial".to_string(), "sans-serif".to_string()]
        );
    }

    /// Input the paint-side parser rejects still measures, through the lenient
    /// parser, as it always did.
    #[test]
    fn input_the_shorthand_parser_rejects_still_measures() {
        let r = Recorder::default();
        r.measure_css("x", "");
        let seen = r.seen.lock().unwrap();
        assert_eq!(seen[0].0, vec!["sans-serif".to_string()]);
        assert_eq!(seen[0].1, 10.0);
    }
}
