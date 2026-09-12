//! OpenGL ES 3.0 backend: Skia Canvas2D + WebGL state-tracked dispatcher.
//!
//! Modules:
//!
//!   * [`blend_mode`] — Canvas2D composite-operation code → [`BlendMode`] map.
//!   * [`canvas`] — `Canvas2DRenderer` + command dispatcher.
//!   * [`color`] — protocol `Color` → Skia `Color4f`, with globalAlpha modulation.
//!   * [`image_store`] — texture-id → Skia `SkImage` registry.
//!   * [`paint`] — build `SkPaint` (+ shader, shadow filter, dash effect) from state.
//!   * [`path`] — Canvas2D-semantics incremental path builder over `SkPathBuilder`.
//!   * [`state`] — `Canvas2DState` value type + save/restore stack.
//!   * [`surface`] — `Canvas2DContext` (SkSurface + DirectContext, one per canvas).
//!   * [`text`] — SkParagraph-backed text stack (font registration, fill/stroke, measure).
//!   * [`text_attrs`] — `TextAlign` / `TextBaseline` anchor-offset math.
//!
//! [`BlendMode`]: skia_safe::BlendMode

pub mod blend_mode;
pub mod canvas;
pub mod color;
pub mod effect_cache;
pub mod image_store;
pub mod paint;
pub mod path;
pub(crate) mod readback;
#[cfg(test)]
pub(crate) mod readback_test_gl;
pub mod state;
pub(crate) mod state_tracker;
pub mod surface;
pub mod text;
pub mod text_attrs;
pub(crate) mod texture_copy;
pub(crate) mod uniform_cache;
