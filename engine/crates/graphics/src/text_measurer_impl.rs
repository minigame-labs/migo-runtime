//! Concrete [`shared::text_measurer::TextMeasurer`] implementation
//! backed by the graphics crate's `TextContext` (F-2).
//!
//! Architecture: a single `Arc<parking_lot::Mutex<TextContext>>`
//! is owned by the render thread and cloned into the JS side via
//! `CanvasOpState::text_measurer`.  Every call enters the mutex
//! for the duration of a measurement; the shaping pipeline is
//! already O(50-150 μs) per miss and sub-μs per cache hit, so
//! even under a worst-case JS + render-thread contention burst
//! the mutex wait time is dwarfed by the work it guards.
//!
//! Font registration (`register_font`) goes through the same
//! mutex, which keeps the shared `font_epoch` counter
//! consistent between the render thread's `fillText` shaping
//! and the JS-thread `measureText` lookup.  JS side also bumps
//! `globalThis.__migoFontEpoch` immediately so its per-canvas
//! LRU cache (R-10) invalidates within the same tick —
//! `register_font` on the shared context and the JS-side bump
//! are wired from the same `loadFont` call, so ordering is
//! deterministic.

use std::sync::Arc;

use parking_lot::Mutex;
use shared::protocol::render_cmd::{TextAlign, TextBaseline, TextDirection, TextMetrics};
use shared::text_measurer::{SharedTextMeasurer, TextMeasurer};

use crate::backend::gl::state::TextAttrs;
use crate::backend::gl::text::TextContext;

/// Public handle the render thread hands to `CanvasOpState`.  A
/// process-wide `Arc` keeps the context alive for the lifetime
/// of the engine; dropping all clones tears down the
/// `TextContext` together with its Skia font managers and LRU
/// caches.
pub type SharedTextContext = Arc<Mutex<LazyTextContext>>;

/// A `TextContext` that is not built until something needs it.
///
/// Building one costs 35-41 ms on an arm64 device: `FontMgr::default()`
/// resolves to `SkFontMgr_Android`, which parses `/system/etc/fonts.xml` and
/// enumerates the system families, and the bundled fallback face is parsed on
/// top of that. That was being paid on the host thread inside
/// `RenderThread::spawn`, *before* the render thread existed -- so every
/// session delayed the start of EGL/Skia initialization by that much, and then
/// blocked waiting for it. Sessions that never draw a glyph paid it too.
///
/// The wrapper exists rather than an `Option` field inside `TextContext`
/// because it makes the deferral impossible to get wrong: there is no way to
/// reach the context except through [`LazyTextContext::get`], so no call site
/// can observe a half-built one. The render thread calls `get` itself once its
/// GPU capabilities are published (see `RenderThread`), which is still long
/// before any game code runs -- the cost moves off the critical path instead of
/// moving into the first `fillText`.
pub struct LazyTextContext(Option<TextContext>);

impl LazyTextContext {
    /// A handle that has not built its context yet.
    pub fn deferred() -> Self {
        Self(None)
    }

    /// The context, building it on first use.
    pub fn get(&mut self) -> &mut TextContext {
        self.0.get_or_insert_with(TextContext::new)
    }

    /// Whether the context has been built. Diagnostics and tests only.
    pub fn is_built(&self) -> bool {
        self.0.is_some()
    }
}

/// Wrap a `TextContext` behind the process-wide mutex and
/// hand back a type-erased `SharedTextMeasurer` pointing at it.
/// The render thread also keeps the same `Arc<Mutex<_>>`
/// directly so `fillText` / `strokeText` can borrow the
/// context mutably without going through the trait object.
pub fn into_shared_measurer(ctx: TextContext) -> (SharedTextContext, SharedTextMeasurer) {
    shared_measurer(LazyTextContext(Some(ctx)))
}

/// The same pair, with the `TextContext` not built yet.
///
/// See [`LazyTextContext`] for why the engine starts here.
pub fn deferred_shared_measurer() -> (SharedTextContext, SharedTextMeasurer) {
    shared_measurer(LazyTextContext::deferred())
}

fn shared_measurer(ctx: LazyTextContext) -> (SharedTextContext, SharedTextMeasurer) {
    let shared = Arc::new(Mutex::new(ctx));
    let measurer: SharedTextMeasurer = Arc::new(TextMeasurerAdapter {
        inner: shared.clone(),
    });
    (shared, measurer)
}

/// Newtype adapter so the `impl TextMeasurer` doesn't appear
/// on `Arc<Mutex<TextContext>>` directly (which would conflict
/// with other downstream blanket impls).
struct TextMeasurerAdapter {
    inner: SharedTextContext,
}

impl TextMeasurer for TextMeasurerAdapter {
    fn measure(
        &self,
        text: &str,
        families: &Arc<Vec<String>>,
        font_size: f32,
        weight: u16,
        italic: bool,
    ) -> TextMetrics {
        let attrs = build_attrs(families, font_size, weight, italic);
        let mut ctx = self.inner.lock();
        ctx.get().measure_text(text, &attrs)
    }

    fn line_height(
        &self,
        families: &Arc<Vec<String>>,
        font_size: f32,
        weight: u16,
        italic: bool,
    ) -> f32 {
        let attrs = build_attrs(families, font_size, weight, italic);
        let mut ctx = self.inner.lock();
        let m = ctx.get().measure_text(" ", &attrs);
        let line = m.font_bounding_box_ascent + m.font_bounding_box_descent;
        if line > 0.0 { line } else { font_size * 1.2 }
    }

    fn register_font(&self, aliases: &[String], bytes: &[u8]) -> Option<String> {
        let mut ctx = self.inner.lock();
        ctx.get()
            .register_family_aliases(aliases, bytes)
            .and_then(|reg| {
                reg.internal_family
                    .or_else(|| reg.aliases.into_iter().next())
            })
    }
}

/// The attributes a `fillText` of the same font builds: the author's whole
/// family list, with the engine's fallbacks appended by the text context the
/// same way for both.
fn build_attrs(
    families: &Arc<Vec<String>>,
    font_size: f32,
    weight: u16,
    italic: bool,
) -> TextAttrs {
    TextAttrs {
        size: font_size,
        families: families.clone(),
        weight,
        italic,
        align: TextAlign::Start,
        baseline: TextBaseline::Alphabetic,
        direction: TextDirection::Inherit,
    }
}

#[cfg(test)]
mod tests {
    use super::{LazyTextContext, deferred_shared_measurer, into_shared_measurer};
    use crate::backend::gl::text::TextContext;

    /// The whole point of the wrapper: nothing is built until asked for.
    ///
    /// Building the context parses the system font configuration and the
    /// bundled fallback face -- 35-41 ms on an arm64 device -- and it used to
    /// happen on the host thread before the render thread was even spawned. A
    /// regression here is invisible in behaviour and costs that much on every
    /// session, so it is asserted rather than trusted.
    #[test]
    fn deferred_measurer_builds_nothing_until_used() {
        let (shared, measurer) = deferred_shared_measurer();
        assert!(
            !shared.lock().is_built(),
            "deferred_shared_measurer must not build the text context"
        );

        let _ = measurer.measure(
            "hello",
            &std::sync::Arc::new(vec!["sans-serif".to_string()]),
            16.0,
            400,
            false,
        );

        assert!(
            shared.lock().is_built(),
            "measuring through the shared handle must build the context"
        );
    }

    /// `get` is the only door in, and it is idempotent.
    #[test]
    fn get_builds_once_and_returns_the_same_context() {
        let mut lazy = LazyTextContext::deferred();
        assert!(!lazy.is_built());
        let first = lazy.get() as *const TextContext;
        assert!(lazy.is_built());
        let second = lazy.get() as *const TextContext;
        assert_eq!(first, second, "get must not rebuild the context");
    }

    /// The eager constructor still hands back a built context, so callers that
    /// have one already (tests, tools) are unaffected by the deferral.
    #[test]
    fn eager_constructor_is_already_built() {
        let (shared, _measurer) = into_shared_measurer(TextContext::new());
        assert!(shared.lock().is_built());
    }
}
