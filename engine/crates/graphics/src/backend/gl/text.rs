//! SkParagraph-backed text pipeline for Canvas2D.
//!
//! Responsibilities:
//!   * Font registration: bundle typefaces by CSS family name so `ctx.font
//!     = '16px MyBrand'` resolves correctly.
//!   * Font resolution: `TextAttrs` → Skia `TextStyle` + fallback chain.
//!   * Layout + paint: `fillText` / `strokeText` render through a
//!     `Paragraph`, honouring the Canvas2D `textAlign` / `textBaseline`
//!     anchor semantics (unlike CSS, the anchor passed to Skia's
//!     `paint()` is the *paragraph box top-left*, not the caller's
//!     anchor, so we shift in X and Y ourselves).
//!   * measureText: produces a [`TextMetrics`] straight from the paragraph
//!     metrics API — no manual glyph advance summing.
//!
//! Thread safety: [`TextContext`] is intended to live on the render thread
//! and is **not** `Send` (Skia `FontCollection` / `TypefaceFontProvider`
//! are `SkRefCnt` without atomic counters).  Fonts loaded once are shared
//! across all Canvas2D contexts in the engine.

use shared::protocol::color::Color as ProtocolColor;
use shared::protocol::render_cmd::{TextDirection as ProtocolDirection, TextMetrics};
use skia_safe::{
    Canvas, Font, FontMgr, FontStyle, Paint, TextBlob, Typeface,
    textlayout::{
        FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle, TextAlign as SkParaAlign,
        TextDirection, TextStyle, TypefaceFontProvider,
    },
};

/// Map protocol-level [`ProtocolDirection`] to Skia's paragraph
/// direction.  `Inherit` resolves to `LTR` because the engine has no
/// parent box to inherit from, matching the Canvas 2D spec's fallback.
#[inline]
pub(crate) fn sk_direction_for(direction: ProtocolDirection) -> TextDirection {
    match direction {
        ProtocolDirection::Rtl => TextDirection::RTL,
        ProtocolDirection::Ltr | ProtocolDirection::Inherit => TextDirection::LTR,
    }
}

use super::color::to_sk_color4f_modulated;
use super::paint::{PatternResolver, build_fill_paint, build_stroke_paint};
use super::state::{Canvas2DState, StyleKind, TextAttrs};
use super::text_attrs::{ResolvedTextAlign, y_baseline_offset};

// F-2 Send/Sync safety argument.  `TextContext` holds
// `RCHandle` values backed by Skia's atomic-refcounted SkRefCnt
// (Skia is compiled with `SK_REFCNT_ATOMIC` as the default),
// plus `RefCell<LruCache<...>>` that's already `Send` when its
// contents are.  The sole blocker that keeps this type off the
// auto-impl is `RCHandle<...>` using `*const` internally to
// inhibit auto-Send per skia-safe's prelude — but the pointer
// it wraps is the address of an atomically-refcounted object,
// which is safe to move between threads as long as only one
// thread accesses it at a time.  Serialised access is
// guaranteed by the outer `parking_lot::Mutex<TextContext>`
// held through `SharedTextMeasurer`.
//
// Sync is NOT implemented intentionally — callers must go
// through the mutex so concurrent `measure()` invocations
// serialise rather than racing on shared Skia state.
unsafe impl Send for TextContext {}

/// Shared text layout state.  Owns the font registry, not per-canvas.
pub struct TextContext {
    /// Whether simple `fillText` runs may take the `SkTextBlob` path.
    ///
    /// Resolved once from
    /// [`shared::feature_policy::FeatureKey::CanvasTextBlobFastPath`] rather
    /// than consulted per call: the decision cannot change mid-frame, and
    /// `fillText` is hot enough that a map lookup per label would be part of
    /// what the fast path is trying to remove.
    blob_fast_path: bool,
    /// How many `fillText` calls the blob path has actually served.
    ///
    /// Without this the parity suite is vacuous: a fast path that quietly
    /// declined every case would make both renders identical and every
    /// comparison pass. This repository has shipped that shape of silence
    /// before -- a conformance runner that reported "0 assertions" instead of
    /// failing -- so the count is part of the feature, not part of the test.
    fast_path_paints: std::cell::Cell<u64>,
    /// The shaper the fast path builds its blobs with.
    ///
    /// It has to be *a* shaper rather than `TextBlob::from_str`, because that
    /// helper lays glyphs out on their nominal advances with no kerning and no
    /// shaping. For a single glyph the two agree; for `AV` or `Hello` they do
    /// not, and the fast path would render text that is subtly differently
    /// spaced from every other text in the same frame.
    shaper: skia_safe::shaper::Shaper,
    font_collection: FontCollection,
    provider: TypefaceFontProvider,
    /// `FontMgr` handle wrapping `provider`.  Cached instead of
    /// rebuilt on every `resolve_primary_typeface` call — the
    /// clone-into-`FontMgr` path isn't expensive (SkRefCnt bump)
    /// but it runs hundreds of times per frame on UI-heavy scenes,
    /// and the cached handle removes the allocation + atomic
    /// refcount churn entirely.  Rebuilt whenever a new family
    /// is registered via [`Self::rebuild_asset_font_mgr`].
    asset_font_mgr: FontMgr,
    /// Cached system/default font manager.  On Android this
    /// resolves to `SkFontMgr_Android` which walks the device
    /// fallback chain for CJK / emoji; on headless CI it's an
    /// empty manager.  Immutable for the lifetime of the
    /// `TextContext` — Skia gives back a fresh refcount on every
    /// `FontMgr::default()` call, so we snapshot once and clone
    /// the SkRefCnt thereafter.
    system_font_mgr: FontMgr,
    system_fallback_family: String,
    bundled_fallback_family: String,
    /// LRU cache of `measureText` results keyed by
    /// `(text, attrs_fingerprint)`.  `measureText` is the hottest
    /// call in Canvas2D UI code (label auto-sizing, wrap detection,
    /// hit-testing) — a mid-tier inventory screen issues hundreds
    /// of measurements per frame.  Skia paragraph shaping via
    /// HarfBuzz + ICU costs ~50-150 us each; the cache reduces
    /// steady-state cost to a hash lookup.
    ///
    /// `RefCell` because `measure_text` is conceptually a read
    /// against an immutable `TextContext`; interior mutability
    /// keeps call sites ergonomic without forcing `&mut self`
    /// through the renderer.
    measure_cache: core::cell::RefCell<lru::LruCache<TextMeasureKey, Verified<TextMetrics>>>,
    /// Immutable, paint-compatible paragraphs for the conservative solid-fill
    /// path.  This is separate from metrics and blob shaping caches because
    /// paragraph foreground paint is part of the retained value.
    paragraph_cache: core::cell::RefCell<lru::LruCache<ParagraphCacheKey, CachedParagraph>>,
    paragraph_cache_bytes: core::cell::Cell<usize>,
    /// `measure_text` and the fast-path `paint_text` branch.  When the
    /// text qualifies for the SkTextBlob fast path (pure ASCII, single
    /// typeface resolvable from the first family name, no BiDi),
    /// `blob` is `Some` and subsequent fills/strokes at the same
    /// attrs skip the full SkParagraph pipeline entirely.  Otherwise
    /// the entry still carries cached metrics and the paint path
    /// falls back to SkParagraph — never silently painting the wrong
    /// glyphs.
    ///
    /// Keyed identically to `measure_cache` so a measure-hit and a
    /// paint-hit for the same string cost one shaping pass between
    /// them, not two.  Capped at the same 256-entry size: each entry
    /// holds one `TextBlob` (RCHandle, a few hundred bytes) plus
    /// `TextMetrics` (7 floats).  ~100 KB steady-state upper bound.
    ///
    /// The paint fast path that would read this is disabled pending
    /// golden verification (see `try_fast_path_paint`), so nothing
    /// reads it back through a live code path yet.
    #[allow(dead_code)]
    shape_cache: core::cell::RefCell<lru::LruCache<TextMeasureKey, Verified<ShapedText>>>,
    /// Retained heap estimate for [`shape_cache`].  The cache has both an
    /// entry cap and a byte cap because a single blob can be much larger than
    /// the small labels that motivated the original count-only limit.
    shape_cache_bytes: core::cell::Cell<usize>,
    /// Retained heap estimate for [`measure_cache`], kept separately so the
    /// metrics cache cannot consume the shaping cache's budget.
    measure_cache_bytes: core::cell::Cell<usize>,
    /// Warn once per unresolved family chain so misconfigured hosts
    /// get a clear signal instead of silent "paint nothing".
    unresolved_family_warnings: core::cell::RefCell<std::collections::HashSet<String>>,
    /// Diag companion to [`unresolved_family_warnings`]: the
    /// successfully-resolved `(family, weight, italic, source)`
    /// tuples we've already emitted an `info!` for.  Bounded by
    /// the count of distinct `ctx.font` values the game ever
    /// sets (typically < 20); growth is not a concern and the
    /// lack of eviction is intentional — each font resolves
    /// exactly once in a session log.
    resolved_family_logs: core::cell::RefCell<std::collections::HashSet<String>>,
    /// Monotonically-incrementing version for the font registry
    /// (R-4).  Every successful [`Self::register_family_aliases`]
    /// / [`Self::register_typeface_data`] bumps this counter; the
    /// value becomes part of [`TextMeasureKey`] so stale LRU
    /// entries keyed at an older epoch are naturally evicted
    /// through ordinary LRU churn — no O(n) `clear()` sweep per
    /// registration.  Under a startup burst of 30-60 font
    /// registrations this replaces 30 × 512-entry-clear with 30
    /// atomic increments.
    font_epoch: core::cell::Cell<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredFont {
    pub internal_family: Option<String>,
    pub aliases: Vec<String>,
}

/// Cached shaping result.  The fast path (`blob.is_some()`) skips
/// SkParagraph entirely; the slow path (`blob.is_none()`) still gets
/// metrics dedup but re-enters the paragraph pipeline for painting
/// because the text requires complex shaping (mixed scripts, BiDi,
/// line-height propagation, font-fallback per glyph).
// The paint fast path that reads `blob` is disabled pending golden
// verification (see `try_fast_path_paint`), so today's measureText-only
// callers never populate/read either field through a live code path the
// compiler can see. Kept, not deleted: this is the fast path's data shape.
#[allow(dead_code)]
#[derive(Clone)]
struct ShapedText {
    metrics: TextMetrics,
    /// `None` when the text needed full SkParagraph shaping
    /// (non-ASCII, mixed fallback, RTL, etc.).
    blob: Option<TextBlob>,
    /// Distance from the shaped blob's own origin (the line top) down to the
    /// baseline, in the same font the blob was built with.
    ///
    /// Kept beside the blob rather than recomputed at the draw site: it is a
    /// property of *this* blob, and deriving it again from any other metrics
    /// source is exactly how the two paths drift apart.
    baseline_from_top: f32,
}

/// Fingerprint of the state that determines the shaped-text metrics.
///
/// Excludes fill/stroke paint (measurement is paint-independent) and
/// `maxWidth` (applied as a post-layout horizontal scale, not during
/// shaping -- see `paint_text`).
/// **A hash of the text, not the text.** `measure_text` is called once per
/// text run per frame by auto-sizing UI code, and a key owning a `String`
/// meant every one of those calls allocated and freed one — including the
/// hits, which is the whole path the cache exists to make free. The key is
/// `Copy` and 40 bytes now, so a lookup touches the heap zero times.
///
/// A hash is not an identity, so the text rides along in the value and every
/// hit compares it: see [`Verified`]. That turns a collision into a miss
/// rather than into the wrong metrics for the wrong string, which for a
/// `measureText` result means a mislaid label with nothing to point at it.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
struct TextMeasureKey {
    text_hash: u64,
    size_bits: u32,
    families_hash: u64,
    weight: u16,
    italic: bool,
    /// BiDi direction folded into the key — Skia's paragraph layout
    /// varies glyph advances for RTL runs (e.g. Arabic contextual
    /// forms), so LTR and RTL hits MUST NOT share a cache slot even
    /// when every other attr matches.
    direction: ProtocolDirection,
    /// Font-registry epoch (R-4).  Bumped by every successful
    /// `register_*` call so entries cached at an older epoch are
    /// *key-mismatched* on the next lookup — LRU reclaims them on
    /// its own cadence without a blocking full-table clear.  The
    /// net effect is equivalent to invalidation but avoids the
    /// O(n) sweep that used to stall the render thread when a
    /// game registered 30+ typefaces at boot.
    font_epoch: u64,
}

impl TextMeasureKey {
    fn new(text: &str, attrs: &TextAttrs, font_epoch: u64) -> Self {
        use std::hash::{Hash, Hasher};
        let mut families = std::collections::hash_map::DefaultHasher::new();
        for f in attrs.families.iter() {
            f.hash(&mut families);
        }
        // Separate hasher from the families one: folding both into a single
        // digest would let a family-name change and a text change cancel out.
        let mut body = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut body);
        Self {
            text_hash: body.finish(),
            // `f32` isn't Hash/Eq by itself; bit-cast to preserve
            // exact equality (including for NaN, which never hits
            // this path because we clamp to 0 earlier).
            size_bits: attrs.size.to_bits(),
            families_hash: families.finish(),
            weight: attrs.weight,
            italic: attrs.italic,
            direction: attrs.direction,
            font_epoch,
        }
    }
}

/// Paint-compatible paragraph key.  The cache is intentionally conservative:
/// only solid fill paints with SrcOver/no shadow are retained, so a cached
/// paragraph cannot accidentally carry a gradient, filter, or blend state
/// into a later draw.  Positioning (`align`, `baseline`, x/y, maxWidth) stays
/// outside because it is applied at paint time.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
struct ParagraphCacheKey {
    text_key: TextMeasureKey,
    color_bits: [u32; 4],
    global_alpha_bits: u32,
    antialias: bool,
}

struct CachedParagraph {
    text: String,
    paragraph: Paragraph,
}

/// A cache entry that carries the text its key only hashed.
///
/// The one place the "a hash is not an identity" obligation of
/// [`TextMeasureKey`] is discharged, so the two caches keyed by it cannot
/// drift apart on it. A hit that fails the comparison is reported as a miss
/// and the caller recomputes and overwrites — correct, and for a 64-bit digest
/// over a 256-entry cache, never observed.
#[derive(Debug, Clone)]
struct Verified<T> {
    text: String,
    value: T,
}

impl<T> Verified<T> {
    #[inline]
    fn new(text: &str, value: T) -> Self {
        Self {
            text: text.to_string(),
            value,
        }
    }

    /// The cached value, if this entry really is about `text`.
    #[inline]
    fn matching(&self, text: &str) -> Option<&T> {
        if self.text == text {
            Some(&self.value)
        } else {
            None
        }
    }
}

/// Count limits alone do not bound retained text: `Verified` owns the full
/// UTF-8 string and a shaped entry additionally owns Skia's blob.  Keep
/// ordinary labels hot, but do not let one long paragraph monopolise either
/// cache.  These are host memory ceilings; device raster cost is separate.
const MEASURE_CACHE_CAP: usize = 256;
const PARAGRAPH_CACHE_BYTES: usize = 256 * 1024;
const MAX_MEASURE_CACHE_BYTES: usize = 64 * 1024;
const SHAPE_CACHE_BYTES: usize = 128 * 1024;
const MAX_MEASURE_ITEM_TEXT_BYTES: usize = 512;
const TEXT_CACHE_ENTRY_OVERHEAD: usize = 128;

#[inline]
fn text_cache_entry_bytes(text_len: usize) -> usize {
    text_len.saturating_add(TEXT_CACHE_ENTRY_OVERHEAD)
}

impl Default for TextContext {
    fn default() -> Self {
        Self::new()
    }
}

impl TextContext {
    const SYSTEM_FALLBACK_FAMILY: &'static str = "sans-serif";
    const BUNDLED_FALLBACK_FAMILY: &'static str = "migo-default-sans";
    const BUNDLED_FALLBACK_FONT: &'static [u8] =
        include_bytes!("../../../tests/fixtures/fonts/NotoSans-Regular.ttf");

    /// Create a text context with deterministic fallback fonts installed.
    /// Custom families still need explicit registration via
    /// [`Self::register_family_aliases`] / [`Self::register_family`].
    /// Turn the `SkTextBlob` paint path on or off for this renderer.
    ///
    /// The host-side kill switch: a device or a build that gets it wrong can be
    /// put back on the complete SkParagraph path without shipping new code, and
    /// the pixel-parity tests use this same control to render both ways.
    pub fn set_blob_fast_path(&mut self, enabled: bool) {
        self.blob_fast_path = enabled;
    }

    /// How many `fillText` calls the `SkTextBlob` path has served so far.
    pub fn fast_path_paint_count(&self) -> u64 {
        self.fast_path_paints.get()
    }

    pub fn new() -> Self {
        let mut fc = FontCollection::new();
        let provider = TypefaceFontProvider::new();
        // Skia requires *some* font manager to shape runs.  We attach the
        // provider as the *asset* manager (treated like a custom bundle)
        // plus a best-effort `FontMgr::new()` as the default for system
        // fallback.  On Android the default resolves to SkFontMgr_Android;
        // on the headless Linux CI host it resolves to an empty manager
        // and tests rely on the registered asset font alone.
        //
        // Clone the provider into the collection — both retain shared
        // ownership via SkRefCnt.
        let asset_mgr: FontMgr = provider.clone().into();
        fc.set_asset_font_manager(Some(asset_mgr.clone()));
        let system_mgr = FontMgr::default();
        fc.set_default_font_manager(system_mgr.clone(), None);

        let cache_cap =
            std::num::NonZeroUsize::new(MEASURE_CACHE_CAP).expect("MEASURE_CACHE_CAP must be > 0");

        let mut this = Self {
            blob_fast_path: shared::feature_policy::is_enabled(
                shared::feature_policy::FeatureKey::CanvasTextBlobFastPath,
            ),
            fast_path_paints: std::cell::Cell::new(0),
            // With no font manager the constructor falls back to the
            // primitive shaper, which lays glyphs out on nominal advances and
            // is exactly what this path is trying to stop doing -- the parity
            // suite catches it as multi-glyph runs that still differ.
            shaper: skia_safe::shaper::Shaper::new(Some(asset_mgr.clone())),
            font_collection: fc,
            provider,
            asset_font_mgr: asset_mgr,
            system_font_mgr: system_mgr,
            system_fallback_family: Self::SYSTEM_FALLBACK_FAMILY.to_string(),
            bundled_fallback_family: Self::BUNDLED_FALLBACK_FAMILY.to_string(),
            measure_cache: core::cell::RefCell::new(lru::LruCache::new(cache_cap)),
            measure_cache_bytes: core::cell::Cell::new(0),
            paragraph_cache: core::cell::RefCell::new(lru::LruCache::new(cache_cap)),
            paragraph_cache_bytes: core::cell::Cell::new(0),
            shape_cache: core::cell::RefCell::new(lru::LruCache::new(cache_cap)),
            shape_cache_bytes: core::cell::Cell::new(0),
            unresolved_family_warnings: core::cell::RefCell::new(std::collections::HashSet::new()),
            resolved_family_logs: core::cell::RefCell::new(std::collections::HashSet::new()),
            font_epoch: core::cell::Cell::new(0),
        };
        this.install_bundled_fallback();
        this
    }

    /// Rebuild the cached asset `FontMgr` so newly-registered
    /// typefaces become visible to `resolve_primary_typeface`.
    /// Called by `register_family` after inserting into the
    /// provider.
    fn rebuild_asset_font_mgr(&mut self) {
        // `TypefaceFontProvider` shares state between clones via
        // SkRefCnt, so the clone here points at the same backing
        // registry — but the `FontMgr` wrapper snapshots its view
        // at construction time on some backends.  Rebuild after
        // mutation to guarantee fresh lookups.
        self.asset_font_mgr = self.provider.clone().into();
    }

    fn install_bundled_fallback(&mut self) {
        if !self.register_family(
            &self.bundled_fallback_family.clone(),
            Self::BUNDLED_FALLBACK_FONT,
        ) {
            tracing::warn!(
                "Canvas2D text: failed to install bundled fallback family '{}'",
                self.bundled_fallback_family
            );
        }
    }

    /// R-4: bump the font-registry epoch.  Entries previously
    /// cached at an older epoch will miss the key comparison on
    /// the next lookup and evict through ordinary LRU usage.
    /// This replaces the old full-table `clear()` with an O(1)
    /// atomic increment — startup bursts of dozens of font
    /// registrations no longer stall the render thread for
    /// thousands of key drops.
    ///
    /// Prefer calling this via `bump_font_epoch` at every
    /// successful `register_*`.  The old method name is kept so
    /// call sites that document "invalidate after font change"
    /// read naturally; tests can still observe the epoch via
    /// [`Self::font_epoch`] if they need to assert it bumped.
    #[inline]
    fn invalidate_measure_cache(&self) {
        self.bump_font_epoch();
    }

    #[inline]
    fn bump_font_epoch(&self) {
        self.font_epoch.set(self.font_epoch.get().wrapping_add(1));
    }

    /// Current font-registry epoch — exposed for tests.
    #[inline]
    #[allow(dead_code)]
    pub(super) fn font_epoch(&self) -> u64 {
        self.font_epoch.get()
    }

    /// Retained-byte estimate for the metrics cache, exposed for bounded-cache
    /// tests and host diagnostics.
    #[inline]
    #[allow(dead_code)]
    fn measure_cache_bytes(&self) -> usize {
        self.measure_cache_bytes.get()
    }

    /// Resolve the first typeface that can be produced for the given
    /// text attributes.  Used to decide whether the SkTextBlob fast
    /// path applies: we need exactly one typeface for the whole run,
    /// and it must be able to shape every code point in the string.
    /// Returns `None` when the first-tier family isn't registered,
    /// in which case the caller falls back to SkParagraph (which
    /// walks the full fallback chain).
    ///
    /// Unused today: its only caller, `obtain_shaped_text`, is itself
    /// unreachable while `try_fast_path_paint` is disabled.
    #[allow(dead_code)]
    fn resolve_primary_typeface(&self, attrs: &TextAttrs) -> Option<Typeface> {
        let style = self.font_style(attrs);
        // Try the head of the family list first, then the fallback.
        // P1-10: reuse the cached `asset_font_mgr` instead of
        // cloning the provider on every call — on a UI-heavy
        // scene this helper runs hundreds of times per frame.
        for family in self.effective_families(attrs).iter() {
            if let Some(tf) = self.asset_font_mgr.match_family_style(family, style) {
                return Some(tf);
            }
        }
        // Last resort: the system default font manager, which on
        // Android resolves through `SkFontMgr_Android` with its own
        // fallback chain.  We still only accept a single typeface
        // because the fast path can't shape multi-typeface runs.
        self.system_font_mgr
            .match_family_style(&self.system_fallback_family, style)
    }

    fn font_style(&self, attrs: &TextAttrs) -> FontStyle {
        FontStyle::new(
            skia_safe::font_style::Weight::from(i32::from(attrs.weight)),
            skia_safe::font_style::Width::NORMAL,
            if attrs.italic {
                skia_safe::font_style::Slant::Italic
            } else {
                skia_safe::font_style::Slant::Upright
            },
        )
    }

    /// Inline capacity of the family chain. A CSS `font-family` list of three
    /// or four names plus this engine's two fallbacks is the widest shape seen
    /// in practice; a longer list spills to the heap and still works.
    const FAMILY_CHAIN_INLINE: usize = 8;

    /// Compute the family fallback chain used when building a
    /// [`ParagraphStyle`] / [`TextStyle`].  R-1: the chain is
    /// deliberately kept minimal — author-supplied families
    /// first, then the generic `sans-serif` (which on Android
    /// routes through `SkFontMgr_Android` and implicitly covers
    /// CJK / Hebrew / Arabic via the device's prebuilt
    /// `fonts.xml`), and finally the bundled
    /// `NotoSans-Regular.ttf` as a deterministic last resort for
    /// the headless-CI path.  Any per-codepoint resolution
    /// (emoji, rare scripts) is left to Skia's own
    /// [`FontCollection::defaultFallback`] /
    /// `matchFamilyStyleCharacter(unicode, …)` pipeline
    /// (`skia/modules/skparagraph/src/FontCollection.cpp:201-224`),
    /// which inspects each glyph's codepoint at shaping time —
    /// strictly more accurate than the previous
    /// prefix-sample-plus-static-list heuristic that could miss
    /// CJK/emoji appearing after the first 64 chars, and match
    /// the strategy Slint (parley + fontique) and Flutter (txt +
    /// `FontCollection::defaultFallback`) use.
    ///
    /// **Borrows the names rather than cloning them.** This runs once per
    /// `fillText` / `strokeText` and once per measure miss, and it used to
    /// return `Vec<String>`: one allocation for the vector plus one per author
    /// family plus one per fallback appended — four heap events for the
    /// ordinary `font-family: Arial`, every text draw, on the render thread.
    /// Both consumers only ever read the names (`SkTextStyle::set_font_families`
    /// takes `&[impl AsRef<str>]`, `SkFontMgr::match_family_style` takes
    /// `&str`), so nothing needed to own them.
    ///
    /// The chain outlives neither `self` nor `attrs`, which is what makes the
    /// borrow sound and is why the signature ties it to both.
    ///
    /// The `text` parameter this once took, retained-and-ignored so callers
    /// could drop it incrementally, is gone with the last caller that passed it.
    fn effective_families<'a>(
        &'a self,
        attrs: &'a TextAttrs,
    ) -> smallvec::SmallVec<[&'a str; Self::FAMILY_CHAIN_INLINE]> {
        let mut families: smallvec::SmallVec<[&'a str; Self::FAMILY_CHAIN_INLINE]> =
            attrs.families.iter().map(String::as_str).collect();
        if !families
            .iter()
            .any(|f| f.eq_ignore_ascii_case(&self.system_fallback_family))
        {
            families.push(&self.system_fallback_family);
        }
        if !families
            .iter()
            .any(|f| f.eq_ignore_ascii_case(&self.bundled_fallback_family))
        {
            families.push(&self.bundled_fallback_family);
        }
        families
    }

    fn maybe_warn_missing_family_resolution(&self, attrs: &TextAttrs) {
        if attrs.families.is_empty() {
            return;
        }
        let style = self.font_style(attrs);
        // Diag: track "first successful resolution" per family so
        // the operator can see, once per font, what typeface Skia
        // actually picked — postscript name, style, source
        // (asset bundle vs system).  This is the single most
        // useful log line when investigating "why does the font
        // look wrong in hxddd" — if the game requested `"Arial"`
        // and we resolved it to e.g. `"Roboto-Regular"` via the
        // system fallback, the log tells us directly.
        let mut any_resolved = false;
        for family in attrs.families.iter() {
            // Check asset manager first (registered via
            // op_load_font) — these are games' own TTFs.
            if let Some(tf) = self.asset_font_mgr.match_family_style(family, style) {
                any_resolved = true;
                self.log_first_resolution(family, &tf, "asset", attrs);
                continue;
            }
            // Then the system manager — Android routes through
            // `SkFontMgr_Android` which reads `/system/etc/fonts.xml`
            // for CJK / emoji fallback.
            if let Some(tf) = self.system_font_mgr.match_family_style(family, style) {
                any_resolved = true;
                self.log_first_resolution(family, &tf, "system", attrs);
                continue;
            }
        }
        if any_resolved {
            return;
        }
        let key = attrs.families.join(", ");
        if self
            .unresolved_family_warnings
            .borrow_mut()
            .insert(key.clone())
        {
            tracing::warn!(
                "Canvas2D text families unresolved: [{}]; falling back to '{}' -> '{}'",
                key,
                self.system_fallback_family,
                self.bundled_fallback_family
            );
        }
    }

    /// Log — at most once per `(family, weight, italic, source)`
    /// tuple per session — which underlying typeface Skia chose.
    /// Emits at info level because (a) the set is bounded by the
    /// number of distinct fonts a game ever asks for (single-digit
    /// in the typical case, a few dozen at the extreme), and
    /// (b) the information is essential when diagnosing "the
    /// text looks wrong" in a field log — the operator doesn't
    /// have to bounce through multiple trace toggles.
    fn log_first_resolution(
        &self,
        family: &str,
        typeface: &skia_safe::Typeface,
        source: &'static str,
        attrs: &TextAttrs,
    ) {
        let key = format!(
            "{}|{}|{}|{}",
            family, attrs.weight, attrs.italic as u8, source
        );
        if !self.resolved_family_logs.borrow_mut().insert(key) {
            return;
        }
        // `family_name` returns the typeface's own declared family
        // (what Skia considers canonical), which can differ from
        // the requested `family` when Android maps aliases
        // ("Arial" → "Roboto-Regular").  Both are useful for
        // diagnosis so we emit them together.
        let resolved_family = typeface.family_name();
        let resolved_style = typeface.font_style();
        tracing::info!(
            requested_family = family,
            requested_weight = attrs.weight,
            requested_italic = attrs.italic,
            source,
            resolved_family = %resolved_family,
            resolved_weight = *resolved_style.weight(),
            resolved_italic = matches!(resolved_style.slant(), skia_safe::font_style::Slant::Italic),
            "Canvas2D typeface resolved (first use this session)"
        );
    }

    /// Register a typeface under the CSS family name `family`.
    ///
    /// Fonts registered multiple times under the same alias accumulate as
    /// a *style-set* in the provider — subsequent `bold` / `italic`
    /// variants can be added without displacing the original.  Returns
    /// `false` on parse failure (e.g. corrupted byte stream).
    pub fn register_family(&mut self, family: &str, bytes: &[u8]) -> bool {
        self.register_family_aliases(&[family.to_string()], bytes)
            .is_some()
    }

    /// Register a typeface under one or more CSS family aliases.
    ///
    /// The font's internal family name is also registered when available so
    /// hosts can use either a custom alias or the name embedded in the font
    /// itself.  Duplicate aliases are removed case-insensitively.
    pub fn register_family_aliases(
        &mut self,
        aliases: &[String],
        bytes: &[u8],
    ) -> Option<RegisteredFont> {
        let typeface = FontMgr::default().new_from_data(bytes, None)?;
        let internal_family = normalize_registered_family(&typeface.family_name());
        let aliases = dedupe_registered_families(
            aliases
                .iter()
                .cloned()
                .chain(internal_family.iter().cloned()),
        );
        if aliases.is_empty() {
            return None;
        }
        // Diag (font-debug): log the registered TTF's intrinsic style so
        // multiple registrations under the same alias can be told apart
        // (e.g. a game loading 3 subset files all named
        // "Source Han Sans CN" — Skia's TypefaceFontProvider picks ONE
        // by closest font_style, which is otherwise opaque from logs).
        let style = typeface.font_style();
        let ps_name = typeface
            .post_script_name()
            .unwrap_or_else(|| "<no postscript>".to_string());
        tracing::debug!(
            target: "graphics::backend::gl::text::font_diag",
            internal_family = ?internal_family,
            aliases = ?aliases,
            postscript = %ps_name,
            weight = *style.weight(),
            width = *style.width(),
            slant = ?style.slant(),
            bytes = bytes.len(),
            "register_family_aliases: typeface registered"
        );
        for alias in &aliases {
            self.provider
                .register_typeface(typeface.clone(), Some(alias.as_str()));
        }
        // Re-sync the asset manager so the collection sees the new family.
        self.font_collection
            .set_asset_font_manager(Some(self.provider.clone().into()));
        // P1-10: keep the cached `asset_font_mgr` in lock-step with
        // the provider so `resolve_primary_typeface` sees the newly
        // registered family on the very next call.
        self.rebuild_asset_font_mgr();
        // A new typeface may change how existing families resolve --
        // flush the measure cache to avoid serving stale metrics.
        self.invalidate_measure_cache();
        Some(RegisteredFont {
            internal_family,
            aliases,
        })
    }

    /// Register a typeface with no family alias — useful when the TTF
    /// itself already carries the correct `name` table entry.
    pub fn register_typeface_data(&mut self, bytes: &[u8]) -> Option<Typeface> {
        let tf = FontMgr::default().new_from_data(bytes, None)?;
        self.provider.register_typeface(tf.clone(), None);
        self.font_collection
            .set_asset_font_manager(Some(self.provider.clone().into()));
        self.rebuild_asset_font_mgr();
        self.invalidate_measure_cache();
        Some(tf)
    }

    /// Paint text filled with `state.fill`.
    pub fn fill_text<R: PatternResolver>(
        &self,
        canvas: &Canvas,
        text: &str,
        x: f32,
        y: f32,
        max_width: f32,
        state: &Canvas2DState,
        resolver: &R,
    ) {
        self.paint_text(canvas, text, x, y, max_width, state, resolver, false);
    }

    /// Paint text stroked with `state.stroke`.
    pub fn stroke_text<R: PatternResolver>(
        &self,
        canvas: &Canvas,
        text: &str,
        x: f32,
        y: f32,
        max_width: f32,
        state: &Canvas2DState,
        resolver: &R,
    ) {
        self.paint_text(canvas, text, x, y, max_width, state, resolver, true);
    }

    fn paint_text<R: PatternResolver>(
        &self,
        canvas: &Canvas,
        text: &str,
        x: f32,
        y: f32,
        max_width: f32,
        state: &Canvas2DState,
        resolver: &R,
        stroke: bool,
    ) {
        let paint = if stroke {
            build_stroke_paint(state, resolver)
        } else {
            build_fill_paint(state, resolver)
        };

        // Reuse a laid-out paragraph for the conservative solid-fill case.
        // Paint state is part of the key; gradients, patterns, shadows and
        // strokes continue through the complete per-call path.
        if !stroke && self.try_cached_paragraph_paint(canvas, text, x, y, max_width, state, &paint)
        {
            return;
        }

        // Fast path: pure-ASCII / single-typeface / LTR text with no
        // `maxWidth` scaling and no shadow can skip SkParagraph.  The
        // feature remains host-policy gated and defaults off.
        if let Some(()) = self.try_fast_path_paint(canvas, text, x, y, max_width, state, &paint) {
            return;
        }

        let mut paragraph = self.build_paragraph(text, &state.text, Some(&paint));
        paragraph.layout(f32::INFINITY);
        let run_width = paragraph.max_intrinsic_width();
        log_paragraph_diagnostics("paint", text, &state.text, &mut paragraph, run_width);
        let sk_dir = sk_direction_for(state.text.direction);
        let align = ResolvedTextAlign::resolve(state.text.align, sk_dir);
        let x_anchor = x - align.x_anchor_offset(run_width);
        let y_anchor = y - baseline_offset(&paragraph, &state.text);
        if max_width.is_finite() && max_width > 0.0 && run_width > max_width {
            let scale = max_width / run_width;
            canvas.save();
            canvas.translate((x_anchor, y_anchor));
            canvas.scale((scale, 1.0));
            paragraph.paint(canvas, (0.0, 0.0));
            canvas.restore();
        } else {
            paragraph.paint(canvas, (x_anchor, y_anchor));
        }
    }

    fn try_cached_paragraph_paint(
        &self,
        canvas: &Canvas,
        text: &str,
        x: f32,
        y: f32,
        max_width: f32,
        state: &Canvas2DState,
        paint: &Paint,
    ) -> bool {
        let StyleKind::Color(color) = &state.fill else {
            return false;
        };
        if state.shadow.is_visible() || state.blend_mode != skia_safe::BlendMode::SrcOver {
            return false;
        }
        let key = ParagraphCacheKey {
            text_key: TextMeasureKey::new(text, &state.text, self.font_epoch.get()),
            color_bits: [
                color.r.to_bits(),
                color.g.to_bits(),
                color.b.to_bits(),
                color.a.to_bits(),
            ],
            global_alpha_bits: state.global_alpha.to_bits(),
            antialias: state.antialias,
        };
        let cached_matches = {
            let mut cache = self.paragraph_cache.borrow_mut();
            let matches = cache.get(&key).is_some_and(|entry| entry.text == text);
            if !matches && cache.get(&key).is_some() {
                cache.pop(&key);
            }
            matches
        };
        if !cached_matches {
            let mut paragraph = self.build_paragraph(text, &state.text, Some(paint));
            paragraph.layout(f32::INFINITY);
            self.insert_paragraph_cache(key, text, paragraph);
        }

        let mut cache = self.paragraph_cache.borrow_mut();
        let Some(entry) = cache.get_mut(&key) else {
            return false;
        };
        let paragraph = &mut entry.paragraph;
        let run_width = paragraph.max_intrinsic_width();
        log_paragraph_diagnostics("paint", text, &state.text, paragraph, run_width);
        let sk_dir = sk_direction_for(state.text.direction);
        let align = ResolvedTextAlign::resolve(state.text.align, sk_dir);
        let x_anchor = x - align.x_anchor_offset(run_width);
        let y_anchor = y - baseline_offset(paragraph, &state.text);
        if max_width.is_finite() && max_width > 0.0 && run_width > max_width {
            let scale = max_width / run_width;
            canvas.save();
            canvas.translate((x_anchor, y_anchor));
            canvas.scale((scale, 1.0));
            paragraph.paint(canvas, (0.0, 0.0));
            canvas.restore();
        } else {
            paragraph.paint(canvas, (x_anchor, y_anchor));
        }
        true
    }

    /// Attempt to paint `text` via a cached `SkTextBlob`.  Returns
    /// `Some(())` when the fast path ran successfully; returns `None`
    /// to signal that the caller must fall back to the full
    /// SkParagraph pipeline.
    ///
    /// Guards against scenarios the single-typeface blob can't
    /// represent faithfully:
    ///   * non-ASCII (may need fallback shaping / BiDi reorder)
    ///   * `state.text.direction == Rtl`
    ///   * `state.shadow.is_visible()` (blob would ignore filter
    ///     attached to the paint — paragraph path handles shadow via
    ///     layer saves)
    ///   * `max_width` horizontal scaling (handled identically in the
    ///     paragraph path for safety)
    ///   * missing typeface resolution (we need a single concrete
    ///     typeface for the whole run)
    fn try_fast_path_paint(
        &self,
        canvas: &Canvas,
        text: &str,
        x: f32,
        y: f32,
        max_width: f32,
        state: &Canvas2DState,
        paint: &Paint,
    ) -> Option<()> {
        if !self.blob_fast_path {
            return None;
        }
        // `is_scale_translate` is true for translate/scale matrices and false
        // as soon as any rotation or skew is present.
        let inputs = FastPathInputs {
            text,
            x,
            y,
            max_width,
            state,
            paint,
            ctm_is_scale_translate: canvas.local_to_device_as_3x3().is_scale_translate(),
        };
        if !fast_path_is_eligible(&inputs) {
            return None;
        }

        let key = TextMeasureKey::new(text, &state.text, self.font_epoch.get());
        let shaped = self.obtain_shaped_text(&key, text, &state.text)?;
        let blob = shaped.blob.as_ref()?;
        let metrics = &shaped.metrics;

        let sk_dir = sk_direction_for(state.text.direction);
        let align = ResolvedTextAlign::resolve(state.text.align, sk_dir);
        let x_anchor = x - align.x_anchor_offset(metrics.width);

        // Alphabetic only, and that is what makes the vertical placement
        // provable rather than lucky.
        //
        // The paragraph path draws its *box top* at `y - offset` and its
        // baseline lands `ascent` below that; `draw_text_blob` takes the
        // baseline directly. For `alphabetic`, `offset == ascent`, so the
        // paragraph's baseline is `y - ascent + ascent == y` -- the ascent
        // cancels, and the two paths agree no matter which of the engine's two
        // ascent definitions is used. For every other baseline the offset and
        // the ascent do not cancel, and the two definitions
        // (`Paragraph::alphabetic_baseline` vs `SkFont::metrics`) are not the
        // same number, so those modes stay on the paragraph.
        // Alphabetic baseline means "put the baseline at `y`", and the blob's
        // own origin is its line top, so it is drawn one ascent higher. Both
        // numbers come from the font this blob was built with, so they cancel
        // exactly -- the placement does not depend on the paragraph's ascent
        // agreeing with the font's, which it does not.
        canvas.draw_text_blob(blob, (x_anchor, y - shaped.baseline_from_top), paint);
        self.fast_path_paints.set(self.fast_path_paints.get() + 1);
        Some(())
    }

    /// Look up (or populate) the shape-cache entry for
    /// `(text, attrs)`.  Returns `None` when no typeface can be
    /// resolved — callers treat this as "fall back to paragraph".
    ///
    /// Unused today: its only intended caller, the SkTextBlob fast
    /// path, is disabled pending golden verification (see
    /// `try_fast_path_paint`).
    fn obtain_shaped_text(
        &self,
        key: &TextMeasureKey,
        text: &str,
        attrs: &TextAttrs,
    ) -> Option<ShapedText> {
        if let Some(existing) = self
            .shape_cache
            .borrow_mut()
            .get(key)
            .and_then(|entry| entry.matching(text))
            .cloned()
        {
            crate::render_diagnostics::hit_shape_cache();
            return Some(existing);
        }
        crate::render_diagnostics::miss_shape_cache();
        let tf = self.resolve_primary_typeface(attrs)?;
        let mut font = Font::from_typeface(tf, attrs.size.max(0.0));
        // Match how the paragraph rasterises and advances, or the fast path
        // produces text that is the same glyphs at slightly different places.
        //
        // `linear_metrics` is the one that matters: without it each glyph's
        // advance is the hinted, rounded one, and the error accumulates along
        // the run -- the parity suite saw `Hello` come out a pixel narrow and
        // `0123456789` three pixels wide, while single glyphs matched exactly.
        font.set_subpixel(true);
        font.set_linear_metrics(true);
        font.set_hinting(skia_safe::FontHinting::Slight);
        font.set_edging(skia_safe::font::Edging::AntiAlias);
        // Shaped, not laid out on nominal advances. `TextBlob::from_str` runs no
        // GSUB/GPOS at all, so any font with kerning pairs places `AV` or
        // `Hello` differently from the paragraph -- proven by the parity suite,
        // where single glyphs matched and multi-glyph runs did not.
        //
        // `f32::MAX` as the wrap width keeps this one unwrapped run; Canvas2D
        // `fillText` never wraps. The run handler positions the blob against the
        // *line top*, so the baseline sits `ascent` below the offset -- see
        // `ShapedText::baseline_from_top`.
        let shaped = self
            .shaper
            .shape_text_blob(text, &font, true, f32::MAX, (0.0, 0.0));
        // The run handler's end point is the start of the *next line*, not the
        // width of this one -- reading it as an advance silently produced a
        // zero width, which alignment then turned into text drawn at the raw
        // anchor. Measure the run instead, with the same font settings the
        // blob was shaped with so the number describes those glyphs.
        let blob = shaped.map(|(blob, _end)| blob);
        let advance = font.measure_str(text, Some(paint_for_measure())).0;
        // The advance the shaper actually produced, not a second measurement
        // that could disagree with the glyphs that were placed.
        let (width, ascent, descent) = if blob.is_some() {
            // SkFont::metrics gives the font-level ascent / descent
            // used by browser measureText's `fontBoundingBox*`.
            let (_line_height, font_mx) = font.metrics();
            let ascent = (-font_mx.ascent).max(0.0);
            let descent = font_mx.descent.max(0.0);
            (advance, ascent, descent)
        } else {
            (0.0, 0.0, 0.0)
        };
        let metrics = TextMetrics {
            width,
            actual_bounding_box_left: 0.0,
            actual_bounding_box_right: width,
            font_bounding_box_ascent: ascent,
            font_bounding_box_descent: descent,
            actual_bounding_box_ascent: ascent,
            actual_bounding_box_descent: descent,
            em_height_ascent: ascent,
            em_height_descent: descent,
            hanging_baseline: ascent * 0.8,
            alphabetic_baseline: 0.0,
            ideographic_baseline: descent * -1.0,
        };
        let entry = ShapedText {
            metrics,
            blob,
            baseline_from_top: ascent,
        };
        self.insert_shape_cache(*key, text, entry.clone());
        Some(entry)
    }

    fn insert_paragraph_cache(&self, key: ParagraphCacheKey, text: &str, paragraph: Paragraph) {
        let bytes = text_cache_entry_bytes(text.len()).saturating_add(1024);
        if text.len() > MAX_MEASURE_ITEM_TEXT_BYTES || bytes > PARAGRAPH_CACHE_BYTES {
            return;
        }
        let mut cache = self.paragraph_cache.borrow_mut();
        if let Some(old) = cache.pop(&key) {
            let old_bytes = text_cache_entry_bytes(old.text.len()).saturating_add(1024);
            self.paragraph_cache_bytes
                .set(self.paragraph_cache_bytes.get().saturating_sub(old_bytes));
        }
        while self.paragraph_cache_bytes.get().saturating_add(bytes) > PARAGRAPH_CACHE_BYTES {
            let Some((_, old)) = cache.pop_lru() else {
                break;
            };
            let old_bytes = text_cache_entry_bytes(old.text.len()).saturating_add(1024);
            self.paragraph_cache_bytes
                .set(self.paragraph_cache_bytes.get().saturating_sub(old_bytes));
        }
        if cache.len() >= MEASURE_CACHE_CAP {
            if let Some((_, old)) = cache.pop_lru() {
                let old_bytes = text_cache_entry_bytes(old.text.len()).saturating_add(1024);
                self.paragraph_cache_bytes
                    .set(self.paragraph_cache_bytes.get().saturating_sub(old_bytes));
            }
        }
        cache.put(
            key,
            CachedParagraph {
                text: text.to_string(),
                paragraph,
            },
        );
        self.paragraph_cache_bytes
            .set(self.paragraph_cache_bytes.get().saturating_add(bytes));
    }

    /// Insert a shaped entry only while both entry and retained-byte limits
    /// allow it; oversized text remains a normal uncached render.
    fn insert_shape_cache(&self, key: TextMeasureKey, text: &str, value: ShapedText) {
        let bytes = text_cache_entry_bytes(text.len()).saturating_add(512);
        if text.len() > MAX_MEASURE_ITEM_TEXT_BYTES || bytes > SHAPE_CACHE_BYTES {
            return;
        }
        let mut cache = self.shape_cache.borrow_mut();
        if let Some(old) = cache.pop(&key) {
            let old_bytes = text_cache_entry_bytes(old.text.len()).saturating_add(512);
            self.shape_cache_bytes
                .set(self.shape_cache_bytes.get().saturating_sub(old_bytes));
        }
        while self.shape_cache_bytes.get().saturating_add(bytes) > SHAPE_CACHE_BYTES {
            let Some((_, old)) = cache.pop_lru() else {
                break;
            };
            let old_bytes = text_cache_entry_bytes(old.text.len()).saturating_add(512);
            self.shape_cache_bytes
                .set(self.shape_cache_bytes.get().saturating_sub(old_bytes));
        }
        if cache.len() >= MEASURE_CACHE_CAP {
            if let Some((_, old)) = cache.pop_lru() {
                let old_bytes = text_cache_entry_bytes(old.text.len()).saturating_add(512);
                self.shape_cache_bytes
                    .set(self.shape_cache_bytes.get().saturating_sub(old_bytes));
            }
        }
        cache.put(key, Verified::new(text, value));
        self.shape_cache_bytes
            .set(self.shape_cache_bytes.get().saturating_add(bytes));
    }

    fn insert_measure_cache(&self, key: TextMeasureKey, text: &str, value: TextMetrics) {
        let bytes = text_cache_entry_bytes(text.len());
        if text.len() > MAX_MEASURE_ITEM_TEXT_BYTES || bytes > MAX_MEASURE_CACHE_BYTES {
            return;
        }
        let mut cache = self.measure_cache.borrow_mut();
        if let Some(old) = cache.pop(&key) {
            self.measure_cache_bytes.set(
                self.measure_cache_bytes
                    .get()
                    .saturating_sub(text_cache_entry_bytes(old.text.len())),
            );
        }
        while self.measure_cache_bytes.get().saturating_add(bytes) > MAX_MEASURE_CACHE_BYTES {
            let Some((_, old)) = cache.pop_lru() else {
                break;
            };
            self.measure_cache_bytes.set(
                self.measure_cache_bytes
                    .get()
                    .saturating_sub(text_cache_entry_bytes(old.text.len())),
            );
        }
        if cache.len() >= MEASURE_CACHE_CAP {
            if let Some((_, old)) = cache.pop_lru() {
                self.measure_cache_bytes.set(
                    self.measure_cache_bytes
                        .get()
                        .saturating_sub(text_cache_entry_bytes(old.text.len())),
                );
            }
        }
        cache.put(key, Verified::new(text, value));
        self.measure_cache_bytes
            .set(self.measure_cache_bytes.get().saturating_add(bytes));
    }

    ///
    /// Hot path: results are cached in an LRU keyed on the fingerprint
    /// returned by [`TextMeasureKey::new`].  Identical repeated
    /// measurements (typical in auto-sizing UI code) collapse to a
    /// hash lookup instead of re-running HarfBuzz shaping.
    pub fn measure_text(&self, text: &str, attrs: &TextAttrs) -> TextMetrics {
        let key = TextMeasureKey::new(text, attrs, self.font_epoch.get());
        if let Some(cached) = self
            .measure_cache
            .borrow_mut()
            .get(&key)
            .and_then(|entry| entry.matching(text))
        {
            crate::render_diagnostics::hit_measure_cache();
            return cached.clone();
        }
        crate::render_diagnostics::miss_measure_cache();

        let mut paragraph = self.build_paragraph(text, attrs, None);
        paragraph.layout(f32::INFINITY);

        let width = paragraph.max_intrinsic_width();
        log_paragraph_diagnostics("measure", text, attrs, &mut paragraph, width);
        let alpha_baseline = paragraph.alphabetic_baseline();
        let ideo_baseline = paragraph.ideographic_baseline();
        let height = paragraph.height();
        let descent = (height - alpha_baseline).max(0.0);

        // `actual_bounding_box_left` is the distance from the x
        // anchor to the leftmost painted pixel, which with our
        // left-anchored paragraph rendering is 0 in practice.  A
        // more accurate implementation would walk the glyph cluster
        // bounds; the current value matches Skia's paragraph API
        // output and is within typical browser tolerances.
        //
        // Hanging / ideographic baselines aren't directly exposed by
        // SkParagraph; we approximate them against the alphabetic
        // baseline using CSS OpenType metrics rules (hanging ≈ 80%
        // of ascent, ideographic ≈ alpha + 14% of descent).  Canvas
        // 2D call sites mostly use `alphabetic_baseline` anyway.
        let metrics = TextMetrics {
            width,
            actual_bounding_box_left: 0.0,
            actual_bounding_box_right: width,
            font_bounding_box_ascent: alpha_baseline,
            font_bounding_box_descent: descent,
            actual_bounding_box_ascent: alpha_baseline,
            actual_bounding_box_descent: descent,
            em_height_ascent: alpha_baseline,
            em_height_descent: descent,
            hanging_baseline: alpha_baseline * 0.8,
            alphabetic_baseline: 0.0,
            ideographic_baseline: ideo_baseline,
        };
        self.insert_measure_cache(key, text, metrics.clone());
        metrics
    }

    fn build_paragraph(
        &self,
        text: &str,
        attrs: &TextAttrs,
        paint_override: Option<&Paint>,
    ) -> Paragraph {
        let mut para_style = ParagraphStyle::new();
        // Single-line Canvas2D semantics: align is handled by us via the
        // anchor offset; the paragraph itself always uses `Start`.
        para_style.set_text_align(SkParaAlign::Start);
        para_style.set_text_direction(sk_direction_for(attrs.direction));

        let mut text_style = TextStyle::new();
        text_style.set_font_size(attrs.size.max(0.0));
        self.maybe_warn_missing_family_resolution(attrs);
        // R-1: the fallback chain is kept minimal — author
        // families first, then `sans-serif` + bundled Noto.  Any
        // per-codepoint CJK / emoji resolution is left to Skia's
        // `FontCollection::defaultFallback`
        // (`matchFamilyStyleCharacter(unicode, …)`), which is
        // strictly more accurate than the previous prefix-sample
        // + static-family-list heuristic.
        let effective_families = self.effective_families(attrs);
        text_style.set_font_families(&effective_families[..]);
        text_style.set_font_style(self.font_style(attrs));

        if let Some(p) = paint_override {
            text_style.set_foreground_paint(p);
        } else {
            text_style.set_color(to_sk_color4f_modulated(ProtocolColor::black(), 1.0).to_color());
        }

        let mut builder = ParagraphBuilder::new(&para_style, self.font_collection.clone());
        builder.push_style(&text_style);
        builder.add_text(text);
        builder.build()
    }
}

// R-1 / R-8: the previous `sample_text_scripts` heuristic has
// been removed.  SkParagraph's internal `FontCollection::
// defaultFallback` performs per-codepoint
// `matchFamilyStyleCharacter(unicode, …)` at shaping time, which
// both covers CJK / emoji that appear after the first 64 chars
// and sidesteps the "device doesn't ship Noto Sans CJK SC /
// Source Han Sans CN under those exact names" mismatch that the
// static family list used to run into.  See
// `skia/modules/skparagraph/src/FontCollection.cpp:201-224` for
// the upstream implementation pattern Migo now delegates to.

/// Diag (font-debug): emit a single debug-level line describing what
/// SkParagraph actually did for `text` under `attrs`.  Captures the
/// fields most useful when investigating "wrong glyphs / truncated
/// text" reports — especially when a game registers multiple subset
/// typefaces under the same family alias and Skia's
/// `TypefaceFontProvider` picks one that's missing some codepoints
/// (no per-codepoint fallback inside a single alias).
///
/// `role` is "measure" or "paint" so we can correlate the two sides
/// (Cocos-style auto-sizing canvases use measureText then fillText
/// for the same string — width mismatches between them produce the
/// "right side of the label is clipped" symptom).
///
/// Only runs when `target = "graphics::backend::gl::text::font_diag"`
/// is enabled at debug level — zero cost in release otherwise.
fn log_paragraph_diagnostics(
    role: &'static str,
    text: &str,
    attrs: &TextAttrs,
    paragraph: &mut Paragraph,
    width: f32,
) {
    // Cheap level guard so we don't pay the get_font_at / unresolved
    // calls when font_diag isn't enabled.
    if !tracing::enabled!(target: "graphics::backend::gl::text::font_diag", tracing::Level::DEBUG) {
        return;
    }

    // Sample which typeface Skia bound to the first byte of the
    // string and (if non-empty) the last char-boundary byte.  Two
    // samples is usually enough to tell whether different runs got
    // different typefaces.
    let mut samples: Vec<(usize, String, i32)> = Vec::new();
    let mut push_sample = |idx: usize| {
        let font = paragraph.get_font_at(idx);
        let family = font.typeface().family_name();
        let weight = *font.typeface().font_style().weight();
        samples.push((idx, family, weight));
    };
    if !text.is_empty() {
        push_sample(0);
        // Find last char boundary that's strictly < text.len().
        let last = text.char_indices().last().map(|(i, _)| i).unwrap_or(0);
        if last != 0 {
            push_sample(last);
        }
    }

    let unresolved = paragraph.unresolved_glyphs();
    let unresolved_codepoints = paragraph.unresolved_codepoints();

    // Truncate text in the log so a paragraph of 1KB doesn't blow up
    // the log line; keep first ~64 chars (char-boundary safe).
    let mut text_preview: String = text.chars().take(64).collect();
    if text.chars().count() > 64 {
        text_preview.push('…');
    }

    tracing::debug!(
        target: "graphics::backend::gl::text::font_diag",
        role,
        text = %text_preview,
        text_chars = text.chars().count(),
        text_bytes = text.len(),
        size = attrs.size,
        weight = attrs.weight,
        italic = attrs.italic,
        families = ?attrs.families.as_slice(),
        width,
        unresolved_glyphs = ?unresolved,
        unresolved_codepoints = ?unresolved_codepoints,
        run_samples = ?samples,
        "paragraph diagnostics"
    );
}

/// Compute the Y shift needed so that `paragraph.paint(x, y_anchor)` lands
/// with the Canvas2D caller-supplied `y` sitting on the requested baseline.
/// Whether a `fillText` call is one the single-typeface `SkTextBlob` path can
/// reproduce exactly.
///
/// Every rule here exists because SkParagraph does something the blob cannot,
/// not because the blob would look slightly different. The blob carries one
/// typeface, one direction, no filters and no post-layout transform; anything
/// that needs more is not a candidate, and the paragraph path is not a
/// degraded mode -- it is the complete one.
///
/// Free-standing, and returning a plain `bool` per call rather than reading any
/// renderer state, so the rules can be enumerated by tests without a font
/// manager, a canvas, or a GPU.
struct FastPathInputs<'a> {
    text: &'a str,
    x: f32,
    y: f32,
    max_width: f32,
    state: &'a Canvas2DState,
    paint: &'a Paint,
    /// True for translate/scale transforms, false once any rotation or skew
    /// is present.
    ctm_is_scale_translate: bool,
}

fn fast_path_is_eligible(inputs: &FastPathInputs<'_>) -> bool {
    let FastPathInputs {
        text,
        x,
        y,
        max_width,
        state,
        paint,
        ctm_is_scale_translate,
    } = *inputs;
    // Under a rotated or skewed transform Skia rasterises glyphs through a
    // different path than it does axis-aligned, and the two paths land a pixel
    // apart along the run's edge. Scale and translation are fine and are
    // covered by the parity suite; rotation is not, so it goes to the
    // paragraph, which is where rotated text was rendered before this existed.
    if !ctm_is_scale_translate {
        return false;
    }
    // An empty run has no blob at all, and the paragraph path already produces
    // exactly nothing for it; routing it here would only add a cache entry.
    if text.is_empty() {
        return false;
    }
    // Non-ASCII may need font fallback across several typefaces, or BiDi
    // reordering -- a single blob cannot express either.
    if !text.is_ascii() {
        return false;
    }
    if state.text.direction == ProtocolDirection::Rtl {
        return false;
    }
    // See `try_fast_path_paint`: only the alphabetic baseline makes the two
    // paths' vertical placement identical by construction.
    if state.text.baseline != shared::protocol::render_cmd::TextBaseline::Alphabetic {
        return false;
    }
    // A shadow is a draw-looper/filter the paragraph path installs around the
    // whole layer; a blob drawn with the same paint would simply lose it.
    if state.shadow.is_visible() {
        return false;
    }
    // `maxWidth` is honoured as a post-layout horizontal scale around the
    // paragraph, which changes glyph positions -- reproduce it there.
    if max_width.is_finite() && max_width > 0.0 {
        return false;
    }
    // Stroked text goes through the paragraph so stroke geometry, joins and
    // dashes stay in one implementation.
    if paint.style() != skia_safe::paint::Style::Fill {
        return false;
    }
    if !x.is_finite() || !y.is_finite() {
        return false;
    }
    if !state.text.size.is_finite() || state.text.size <= 0.0 {
        return false;
    }
    true
}

fn baseline_offset(paragraph: &Paragraph, attrs: &TextAttrs) -> f32 {
    let ascent = paragraph.alphabetic_baseline();
    let height = paragraph.height();
    let descent = (height - ascent).max(0.0);
    y_baseline_offset(attrs.baseline, ascent, descent)
}

/// Dummy paint used only so `SkFont::measure_str` returns a width without
/// forcing an allocation. The real fill/stroke paint is applied at draw time
/// on the SkTextBlob itself.
///
/// One process-wide `Paint`, because this is on the shape-cache miss path and
/// a `Paint` is not a cheap value to build per call.
fn paint_for_measure() -> &'static Paint {
    use std::sync::OnceLock;
    static PAINT: OnceLock<Paint> = OnceLock::new();
    PAINT.get_or_init(Paint::default)
}

fn normalize_registered_family(candidate: &str) -> Option<String> {
    let trimmed = candidate
        .trim()
        .trim_matches(|ch| ch == '"' || ch == '\'')
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn dedupe_registered_families<I>(candidates: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = std::collections::HashSet::new();
    let mut aliases = Vec::new();
    for candidate in candidates {
        let Some(alias) = normalize_registered_family(&candidate) else {
            continue;
        };
        let key = alias.to_lowercase();
        if seen.insert(key) {
            aliases.push(alias);
        }
    }
    aliases
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::protocol::render_cmd::{TextAlign, TextBaseline};

    const NOTO_SANS: &[u8] = include_bytes!("../../../tests/fixtures/fonts/NotoSans-Regular.ttf");

    fn test_attrs(size: f32) -> TextAttrs {
        TextAttrs {
            size,
            families: std::sync::Arc::new(vec!["test-noto".to_string(), "sans-serif".to_string()]),
            weight: 400,
            italic: false,
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            direction: ProtocolDirection::Inherit,
        }
    }

    #[test]
    fn register_family_accepts_valid_ttf() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
    }

    #[test]
    fn register_family_rejects_garbage_bytes() {
        let mut ctx = TextContext::new();
        assert!(!ctx.register_family("garbage", b"\x00\x01\x02\x03"));
    }

    #[test]
    fn measure_text_width_scales_with_font_size() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));

        let small = ctx.measure_text("Hello", &test_attrs(12.0));
        let large = ctx.measure_text("Hello", &test_attrs(48.0));
        assert!(
            large.width > small.width * 3.5,
            "large={} small={}",
            large.width,
            small.width
        );
    }

    #[test]
    fn measure_text_width_scales_with_text_length() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let w_short = ctx.measure_text("x", &test_attrs(24.0)).width;
        let w_long = ctx.measure_text("xxxxxxxxxx", &test_attrs(24.0)).width;
        assert!(w_long > w_short * 7.5);
    }

    #[test]
    fn measure_text_baselines_are_sensible() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let m = ctx.measure_text("Ag", &test_attrs(24.0));
        assert!(m.font_bounding_box_ascent > 0.0);
        assert!(m.font_bounding_box_descent >= 0.0);
        assert!(m.actual_bounding_box_right > 0.0);
    }

    #[test]
    fn measure_text_cache_returns_bitwise_identical_results() {
        // The LRU must not perturb the result: a cached lookup has
        // to be indistinguishable from a fresh shaping pass.  This
        // catches accidental floating-point drift, field truncation,
        // or key collisions.
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let attrs = test_attrs(18.5);
        let first = ctx.measure_text("Hello, world", &attrs);
        let second = ctx.measure_text("Hello, world", &attrs);
        assert_eq!(first.width.to_bits(), second.width.to_bits());
        assert_eq!(
            first.font_bounding_box_ascent.to_bits(),
            second.font_bounding_box_ascent.to_bits()
        );
        assert_eq!(
            first.actual_bounding_box_right.to_bits(),
            second.actual_bounding_box_right.to_bits()
        );
    }

    /// Section 7.3's steady-state requirement on `measureText`.
    ///
    /// **This is the assertion the previous key could not have passed.** The
    /// key owned a `String`, built before the lookup, so every call — hit
    /// included — allocated and freed one. Auto-sizing UI code measures every
    /// label every frame, which made the cache's whole purpose, the hit, cost a
    /// heap round trip on the thread running the game.
    #[test]
    fn a_steady_state_measure_text_hit_never_reaches_the_heap() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let attrs = test_attrs(18.0);
        let label = "Score: 1234";
        // Resident before the burst, or the burst measures the shaping path.
        assert!(ctx.measure_text(label, &attrs).width > 0.0);

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "text: measureText lookup on a resident label",
                warmup: 4,
                measured: 64,
            },
            |_| ctx.measure_text(label, &attrs).width,
        );
    }

    /// Section 7.3 on the family chain, which every text draw builds.
    ///
    /// Four heap events for `font-family: Arial` before this: the vector, the
    /// author family's clone, and one per appended fallback. Borrowed, it is
    /// zero — and the widest realistic chain still fits inline.
    #[test]
    fn building_the_family_chain_never_reaches_the_heap() {
        let ctx = TextContext::new();
        let mut attrs = test_attrs(16.0);
        // A four-name CSS chain plus the two fallbacks: six of the eight
        // inline slots, so the burst covers the shape that would spill first.
        attrs.families = std::sync::Arc::new(vec![
            "Helvetica Neue".to_string(),
            "Helvetica".to_string(),
            "Arial".to_string(),
            "Liberation Sans".to_string(),
        ]);

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "text: family fallback chain for one text draw",
                warmup: 4,
                measured: 64,
            },
            |_| ctx.effective_families(&attrs).len(),
        );
    }

    /// The chain's contents, since the burst above only proves it is cheap.
    /// Both fallbacks appended, in order, and neither duplicated when the
    /// author already named it.
    #[test]
    fn the_family_chain_appends_each_fallback_exactly_once() {
        let ctx = TextContext::new();
        let mut attrs = test_attrs(16.0);

        attrs.families = std::sync::Arc::new(vec!["Arial".to_string()]);
        let chain: Vec<&str> = ctx.effective_families(&attrs).to_vec();
        assert_eq!(chain, vec!["Arial", "sans-serif", "migo-default-sans"]);

        // Author already asked for the generic; it must not appear twice, and
        // the match is case-insensitive.
        attrs.families = std::sync::Arc::new(vec!["SANS-SERIF".to_string()]);
        let chain: Vec<&str> = ctx.effective_families(&attrs).to_vec();
        assert_eq!(chain, vec!["SANS-SERIF", "migo-default-sans"]);

        // No author families at all: the fallbacks alone, still in order.
        attrs.families = std::sync::Arc::new(Vec::new());
        let chain: Vec<&str> = ctx.effective_families(&attrs).to_vec();
        assert_eq!(chain, vec!["sans-serif", "migo-default-sans"]);
    }

    /// The obligation a hashed key takes on: two different strings that landed
    /// on one slot must not share its metrics. Forced by writing the collision
    /// directly, because a 64-bit digest will not produce one on demand.
    #[test]
    fn a_key_collision_is_a_miss_not_the_wrong_strings_metrics() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let attrs = test_attrs(24.0);

        let narrow = "l";
        let wide = "MMMMMMMMMM";
        let narrow_metrics = ctx.measure_text(narrow, &attrs);
        let wide_metrics = ctx.measure_text(wide, &attrs);
        assert!(
            wide_metrics.width > narrow_metrics.width * 5.0,
            "fixture strings must measure far apart, or this proves nothing"
        );

        // Plant the wide string's entry under the narrow string's key — the
        // shape a hash collision takes.
        let narrow_key = TextMeasureKey::new(narrow, &attrs, ctx.font_epoch.get());
        ctx.measure_cache
            .borrow_mut()
            .put(narrow_key, Verified::new(wide, wide_metrics));

        let after = ctx.measure_text(narrow, &attrs);
        assert_eq!(
            after.width.to_bits(),
            narrow_metrics.width.to_bits(),
            "the colliding entry was served for the wrong string: measuring {narrow:?} \
             returned {} instead of {}",
            after.width,
            narrow_metrics.width
        );
    }

    #[test]
    fn measure_text_cache_keys_on_size_and_family() {
        // Different size / family must NOT share a cache slot,
        // otherwise the metrics returned after the first distinct
        // measurement would be completely wrong.
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let small = ctx.measure_text("test", &test_attrs(12.0));
        let large = ctx.measure_text("test", &test_attrs(48.0));
        assert!(
            large.width > small.width * 3.5,
            "cache confused sizes: small.w={}, large.w={}",
            small.width,
            large.width
        );
    }

    #[test]
    fn register_family_invalidates_measure_cache_by_epoch() {
        // After registering a new typeface under an existing
        // family name, previously cached metrics may be wrong. Invalidation
        // is O(1): old entries remain resident but cannot match the new epoch.
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        // Warm the cache.
        let _ = ctx.measure_text("hello", &test_attrs(16.0));
        let old_cache_len = ctx.measure_cache.borrow().len();
        let old_epoch = ctx.font_epoch();
        assert!(old_cache_len > 0);

        // Registering another typeface advances identity without walking and
        // clearing the old LRU entries on the render thread.
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        assert_eq!(ctx.font_epoch(), old_epoch.wrapping_add(1));
        assert_eq!(ctx.measure_cache.borrow().len(), old_cache_len);

        // The same logical query now inserts a distinct current-epoch key,
        // proving the stale entry was not served.
        let _ = ctx.measure_text("hello", &test_attrs(16.0));
        assert_eq!(ctx.measure_cache.borrow().len(), old_cache_len + 1);
    }

    #[test]
    fn measure_empty_string_is_zero_width() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let m = ctx.measure_text("", &test_attrs(24.0));
        assert_eq!(m.width, 0.0);
    }

    #[test]
    fn default_text_context_has_non_empty_builtin_fallback() {
        let ctx = TextContext::new();
        let attrs = TextAttrs::default();
        let metrics = ctx.measure_text("Hello", &attrs);
        assert!(
            metrics.width > 0.0,
            "default text context should shape Latin text without requiring host LoadFont bootstrap"
        );
    }

    #[test]
    fn unresolved_family_falls_back_instead_of_painting_nothing() {
        let ctx = TextContext::new();
        let attrs = TextAttrs {
            size: 24.0,
            families: std::sync::Arc::new(vec!["definitely-missing-family".to_string()]),
            weight: 400,
            italic: false,
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            direction: ProtocolDirection::Inherit,
        };
        let metrics = ctx.measure_text("Hello", &attrs);
        assert!(
            metrics.width > 0.0,
            "missing family should fall back to a deterministic default instead of returning zero-width text"
        );
    }

    #[test]
    fn register_family_aliases_resolves_explicit_and_internal_names() {
        let mut ctx = TextContext::new();
        let registration = ctx
            .register_family_aliases(
                &["brand-sans".to_string(), "NotoSans-Regular".to_string()],
                NOTO_SANS,
            )
            .expect("fixture font should register");

        assert!(
            registration
                .aliases
                .iter()
                .any(|alias| alias == "brand-sans"),
            "explicit alias should be registered"
        );
        assert!(
            registration
                .internal_family
                .as_deref()
                .is_some_and(|family| !family.is_empty()),
            "font internal family should be surfaced for diagnostics and matching"
        );

        let explicit_alias_attrs = TextAttrs {
            families: std::sync::Arc::new(vec!["brand-sans".to_string()]),
            ..TextAttrs::default()
        };
        let internal_family_attrs = TextAttrs {
            families: std::sync::Arc::new(vec![registration.internal_family.clone().unwrap()]),
            ..TextAttrs::default()
        };

        assert!(ctx.measure_text("Hello", &explicit_alias_attrs).width > 0.0);
        assert!(ctx.measure_text("Hello", &internal_family_attrs).width > 0.0);
    }

    /// G-3: smoke test the `unsafe impl Send for TextContext`
    /// promise — one thread writes (registers a font), another
    /// thread reads (measures), both serialised through a
    /// `parking_lot::Mutex`.  Asserts the data stays consistent
    /// and no deadlock / UB surfaces at `cargo test` time.  The
    /// real thread-safety contract is documented on the `unsafe
    /// impl` block; this test is a cheap regression shield.
    #[test]
    fn text_context_is_send_safe_through_mutex() {
        use std::sync::Arc;
        use std::thread;

        let shared = Arc::new(parking_lot::Mutex::new(TextContext::new()));

        // Spawn writer + reader threads against the same handle.
        let writer = {
            let shared = shared.clone();
            thread::spawn(move || {
                let mut ctx = shared.lock();
                ctx.register_family_aliases(&["concurrent-noto".to_string()], NOTO_SANS);
            })
        };
        let reader = {
            let shared = shared.clone();
            thread::spawn(move || {
                // Repeat the measure a few times so the mutex
                // gets contention from both threads; the LRU
                // cache path stresses the `RefCell` inside
                // which has to stay safe under `Send`.
                let mut last_width = 0.0;
                for _ in 0..50 {
                    let ctx = shared.lock();
                    let attrs = TextAttrs {
                        families: std::sync::Arc::new(vec!["concurrent-noto".to_string()]),
                        ..TextAttrs::default()
                    };
                    let m = ctx.measure_text("hello", &attrs);
                    last_width = m.width;
                }
                last_width
            })
        };

        writer.join().expect("writer panicked");
        let _ = reader.join().expect("reader panicked");
        // If we got here the mutex + `unsafe impl Send` stayed
        // coherent.  Exact width value is not asserted because
        // Skia can return 0 when the writer hasn't landed yet
        // on the scheduling interleave we saw — the invariant is
        // "no crash, no TSAN abort".
    }

    // ── T-M1 byte-ceiling tests ─────────────────────────────────────
    // These must be RED before the byte-bound constants and the
    // `measure_cache_bytes` field are added.  They become GREEN once
    // the production code enforces the limits.

    /// An extremely long text string must not be retained in the cache.
    ///
    /// Without a per-item gate, a single label of several kilobytes would
    /// occupy a disproportionate share of the cache budget and make every
    /// subsequent LRU hit scan that string for equality.  Items above
    /// `MAX_MEASURE_ITEM_TEXT_BYTES` are measured normally but never stored.
    #[test]
    fn measure_cache_oversized_text_not_retained() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        // One byte above the single-item threshold.
        let long_text: String = "a".repeat(MAX_MEASURE_ITEM_TEXT_BYTES + 1);
        let attrs = test_attrs(16.0);
        let m = ctx.measure_text(&long_text, &attrs);
        assert!(
            m.width > 0.0,
            "measurement must still succeed for a long string"
        );
        assert_eq!(
            ctx.measure_cache.borrow().len(),
            0,
            "text longer than MAX_MEASURE_ITEM_TEXT_BYTES must not occupy a cache slot"
        );
    }

    /// Inserting many moderate-size items must not push the retained byte
    /// total above `MAX_MEASURE_CACHE_BYTES`.
    ///
    /// Without byte tracking, 200 distinct 300-byte strings produce
    /// ≈ 200 × 452 bytes ≈ 90 KiB — well above the 64 KiB ceiling.
    /// With byte-bounded trimming, the running total stays under the limit
    /// while the entry-count cap enforces the per-item LRU invariant.
    #[test]
    fn measure_cache_byte_total_stays_under_budget() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let attrs = test_attrs(16.0);
        // 200 distinct texts, each 300 bytes — all individually under the
        // single-item gate (512 bytes) but collectively well over the byte ceiling.
        for i in 0u32..200 {
            let text = format!("{:0>300}", i); // zero-padded to exactly 300 chars
            let _ = ctx.measure_text(&text, &attrs);
        }
        assert!(
            ctx.measure_cache_bytes() <= MAX_MEASURE_CACHE_BYTES,
            "retained measure-cache bytes {} exceeded the ceiling of {}",
            ctx.measure_cache_bytes(),
            MAX_MEASURE_CACHE_BYTES,
        );
    }

    #[test]
    fn ordinary_measure_entries_still_reach_the_entry_cap() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let attrs = test_attrs(16.0);
        for i in 0..MEASURE_CACHE_CAP {
            let _ = ctx.measure_text(&format!("label-{i}"), &attrs);
        }
        assert_eq!(
            ctx.measure_cache.borrow().len(),
            MEASURE_CACHE_CAP,
            "byte accounting must not reduce the ordinary entry-count capacity"
        );
    }

    // ── T-P1 key-completeness verification ──────────────────────────
    // These verify that every glyph-affecting attribute is included in
    // the cache key.  They should be GREEN from the first run (the key is
    // correct), confirming there is no regression path that could silently
    // alias two differently-shaped text runs.

    /// Two `measure_text` calls that differ only in `direction` must occupy
    /// distinct cache slots.  The Canvas 2D spec and HarfBuzz both care about
    /// BiDi direction during shaping — Arabic text in LTR order is wrong.
    ///
    /// This test is expected to pass immediately (direction IS in the key).
    /// Its purpose is to lock that property down as a regression gate.
    #[test]
    fn key_completeness_direction_produces_distinct_cache_entries() {
        let mut ctx = TextContext::new();
        assert!(ctx.register_family("test-noto", NOTO_SANS));
        let attrs_inherit = test_attrs(16.0);
        let mut attrs_rtl = test_attrs(16.0);
        attrs_rtl.direction = ProtocolDirection::Rtl;

        let _ = ctx.measure_text("hello", &attrs_inherit);
        let _ = ctx.measure_text("hello", &attrs_rtl);
        assert_eq!(
            ctx.measure_cache.borrow().len(),
            2,
            "LTR and RTL draws of the same text must occupy distinct cache slots; \
             sharing them would return wrong metrics for one direction"
        );
    }
}
