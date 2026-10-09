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
    /// what was drawn. All four are the facade's reading of
    /// `ctx.font`, as `op_set_font` sends it.  Returns the same `TextMetrics`
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
}

/// Process-wide shared handle.  Cheap to `Clone`: single
/// refcount bump; every clone dispatches through the same
/// underlying mutex-guarded context.
pub type SharedTextMeasurer = std::sync::Arc<dyn TextMeasurer>;
