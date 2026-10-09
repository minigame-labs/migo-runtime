//! Per-session cache of rasterized text textures.
//!
//! Cocos re-renders the same labels every frame and every shop open
//! ("立即购买", "12000金币", etc.) through the
//! `offscreen Canvas2D → fillText → texImage2D(canvas)` pattern.
//! On each repaint we pay ~4 ms of Skia glyph layout + atlas
//! rasterization + snapshot blit + texImage upload, multiplied by
//! dozens of labels per scene.
//!
//! This module caches the **final GL texture** produced for each
//! distinct `(text, font, size, color, align, baseline, canvas-size)`
//! tuple, keyed within one Session so subsequent shop opens and
//! sub-views hit the cache.
//!
//! # Session scope
//!
//! Every Session builds its own EGL display and context: one
//! `CanvasManager` per host render thread, `create_pbuffer_context`
//! with no sharing between managers.  A GL texture name therefore only
//! means anything inside the context that minted it, so the cache is
//! **per host/session**, not per process — see [`SessionTextCache`] and
//! [`text_cache_for_host`].  Consequences of that scoping, each of
//! which a test in this module pins down:
//!
//! - Two games drawing identical text at identical size, weight,
//!   colour and canvas dimensions land on the same *logical* key but in
//!   different caches, so neither can receive a name minted in the
//!   other's context nor delete a name the other still draws with.
//! - The lock a per-frame `fillText` acquires is the session's own, so
//!   there is no cross-session lock on a per-event path.
//! - The byte budget and trim levels are accounted per session, so one
//!   game's eviction pressure cannot evict another game's entries.
//! - The font generation counter is per session, so one game reloading
//!   a font does not invalidate another game's cached text.
//!
//! # Design summary
//!
//! - Storage: `LruCache<TextCacheKey, _>` keyed on the value-equal
//!   `TextCacheKey`, byte- and entry-bounded with LRU eviction on
//!   insert.
//! - Pin / refcount: entries currently in flight (JS-side recognized
//!   a hit and emitted a `TexImageSource` (text-cache source) command) are
//!   pinned so the LRU cannot evict them out from under the render
//!   thread before the command executes.  The count is stored on the
//!   entry, unlike `io::image_cache`'s parallel map: a pin lasts one
//!   frame, so a separately keyed record would own — and therefore
//!   allocate and free — a `TextCacheKey`'s two strings on every hit.
//! - Eviction returns GL texture ids to the caller (via the
//!   `evicted_textures` Vec on `insert` and via `trim`'s return
//!   value); GL `glDeleteTextures` happens on the render thread,
//!   never inside the cache.
//! - One instance per host, reached through [`text_cache_for_host`] at
//!   session bring-up.  Access from both the JS thread (`peek` / `pin`
//!   to record a hit) and the render thread (`insert` / `get` / `trim`)
//!   under that session's own `parking_lot::Mutex`.
//!
//! # Texture ownership invariant
//!
//! A texture id inserted under a `TextCacheKey` is owned by its
//! session's cache until:
//! - it is removed by LRU eviction during a later `insert` (the
//!   removed id is returned to the caller for deletion), or
//! - the cache is cleared / trimmed (returned ids → caller deletes),
//!   or
//! - the session ends and [`unregister_text_cache`] drops the cache.
//!
//! Pinned entries cannot be evicted; an `insert` whose key is already
//! pinned simply replaces the previous entry under that key (the old
//! id is returned for deletion, the pin transfers to the new entry).

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use lru::LruCache;
use parking_lot::{Mutex, RwLock};

use crate::protocol::render_cmd::{TextAlign, TextBaseline};

/// Default byte budget.  Text textures are small (typical label
/// ~10–50 KB at 1× DPR), so the byte budget caps faster than the
/// entry count in practice.  Tunable; the on-trim path frees as
/// needed under memory pressure.
const DEFAULT_MAX_BYTES: usize = 128 * 1024 * 1024;

/// Default entry count.  A few thousand labels is well above what a
/// single game session realistically renders.
const DEFAULT_MAX_ENTRIES: usize = 8192;

/// Cache key.  `Hash` + `Eq` derive over all fields; `f32` is mapped
/// to its bit pattern (`to_bits`) for hashing so two `NaN` values
/// would compare equal (acceptable — cocos never passes NaN sizes).
///
/// Field set is the **minimum** required for correctness: dropping
/// any one of these means two different renderings collapse to one
/// cache slot.  Adding fields (e.g. stroke color, shadow) would
/// reduce hit rate, so kept tight until the whitelist expands to
/// cover those.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextCacheKey {
    /// The text string passed to `fillText`.
    pub text: String,
    /// The raw `font` CSS string the caller set on the Canvas2D
    /// context.  We intentionally key on the unresolved request
    /// string rather than the resolved family because cocos always
    /// passes the same request string for the same intent;
    /// resolution happens on the render thread and would force a
    /// sync round-trip if we wanted the resolved name on the JS
    /// side.  Duplicates from "Arial, sans-serif" vs "Arial" are
    /// acceptable — they're rare and self-limiting.
    pub font_request: String,
    /// `f32::to_bits()` of the font size in CSS pixels.
    pub font_size_bits: u32,
    /// Font weight; 400 for regular, 700 for bold.
    pub font_weight: u16,
    pub italic: bool,
    /// RGBA u32 (premultiplied alpha not applied here — Skia handles
    /// alpha in its paint pipeline).
    pub fill_color: u32,
    pub text_align: TextAlign,
    pub text_baseline: TextBaseline,
    /// Canvas dimensions in device pixels.  The produced texture has
    /// the same dimensions; two canvases of different sizes calling
    /// fillText with otherwise identical state produce different
    /// textures (different padding around the glyph run).
    pub canvas_w: u32,
    pub canvas_h: u32,
    /// Font generation snapshot, taken from the owning session's
    /// [`SessionTextCache::font_generation`].  Rolls forward when *this
    /// session* reloads a font; old-generation entries become
    /// unreachable.  Another session's reload does not affect it.
    pub font_generation: u64,
}

/// Resident cache entry.  `texture_id` is the GL name owned by the
/// cache (not by any `ImageStore` / `image_registry`).  Render thread
/// is the only mutator; JS-side callers read `width`/`height` for
/// sanity checks but never touch GL state.
#[derive(Debug, Clone)]
pub struct CachedTextEntry {
    pub texture_id: u32,
    pub width: u32,
    pub height: u32,
    /// Resident byte cost for budget accounting (RGBA8 → 4 × w × h).
    pub size_bytes: usize,
}

/// Trim levels from `ComponentCallbacks2.onTrimMemory`, mirroring
/// `io::image_cache::TrimLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimLevel {
    RunningModerate,
    RunningLow,
    RunningCritical,
    UiHidden,
    Background,
}

impl TrimLevel {
    pub fn from_android(raw: i32) -> Self {
        match raw {
            5 => TrimLevel::RunningModerate,
            10 => TrimLevel::RunningLow,
            15 => TrimLevel::RunningCritical,
            20 => TrimLevel::UiHidden,
            40 | 60 | 80 => TrimLevel::Background,
            n if n >= 15 => TrimLevel::RunningCritical,
            _ => TrimLevel::RunningModerate,
        }
    }

    /// Bytes the cache may keep at this level, as a ceiling on the budget.
    ///
    /// A ceiling rather than a fraction of *current* usage, matching
    /// `io::image_cache`. This cache is per session, so it never had the
    /// compounding that forced the change there — a signal relayed by N Sessions
    /// trims N separate caches once each. The reading is wrong for the other reason:
    /// a fraction of current usage churns a cache already well inside its budget,
    /// paying a re-rasterization of every label dropped to relieve a few hundred
    /// kilobytes the OS was not asking for. And two caches under one pressure signal
    /// should not interpret its levels differently.
    fn retained_bytes(self, budget: usize) -> usize {
        match self {
            TrimLevel::RunningModerate => budget / 4 * 3,
            TrimLevel::RunningLow | TrimLevel::UiHidden => budget / 2,
            TrimLevel::RunningCritical | TrimLevel::Background => 0,
        }
    }
}

/// Snapshot of cache statistics for diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextCacheStats {
    pub entries: usize,
    pub size_bytes: usize,
    pub max_bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub trim_bytes_released: u64,
}

impl TextCacheStats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

pub struct TextTextureCache {
    cache: LruCache<TextCacheKey, Resident>,
    current_size_bytes: usize,
    max_size_bytes: usize,
    hits: u64,
    misses: u64,
    trim_bytes_released: u64,
}

/// A resident entry and its pin count.
///
/// The pin count lives here rather than in a `HashMap<TextCacheKey, u32>` beside
/// the cache because a pin's lifetime is one frame: JS pins on a `fillText` hit and
/// the render thread unpins after the copy. A separately keyed map therefore has to
/// own a `TextCacheKey` — two heap strings — created and destroyed on every hit,
/// which is per-event allocation on a steady hot path. Keyed by the entry, a pin is
/// an increment.
struct Resident {
    entry: CachedTextEntry,
    pins: u32,
}

impl Default for TextTextureCache {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_BYTES)
    }
}

impl TextTextureCache {
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        let cap = NonZeroUsize::new(max_entries).unwrap_or(NonZeroUsize::MIN);
        Self {
            cache: LruCache::new(cap),
            current_size_bytes: 0,
            max_size_bytes: max_bytes,
            hits: 0,
            misses: 0,
            trim_bytes_released: 0,
        }
    }

    /// Lookup without mutating LRU order — used as a cheap "would
    /// this hit" probe from JS without committing to use it yet.
    /// Returns a clone of the entry so the caller can release the
    /// lock immediately.
    pub fn peek(&self, key: &TextCacheKey) -> Option<CachedTextEntry> {
        self.cache.peek(key).map(|resident| resident.entry.clone())
    }

    /// Lookup AND promote to MRU.  Bumps the hit counter.  Render
    /// thread calls this when consuming a hit.
    pub fn get(&mut self, key: &TextCacheKey) -> Option<CachedTextEntry> {
        if let Some(resident) = self.cache.get(key) {
            let entry = resident.entry.clone();
            self.hits += 1;
            Some(entry)
        } else {
            self.misses += 1;
            None
        }
    }

    /// Increment the pin count for `key`.  Pinned entries are exempt
    /// from LRU eviction in `insert` / `trim` / `clear`.
    ///
    /// Returns whether the pin took. Only a resident entry can be pinned, so a key
    /// with nothing behind it is refused rather than recorded for a future occupant.
    /// Callers pin under the same lock guard as the lookup that found the entry, so
    /// a refusal means the caller pinned something it had not looked up.
    #[must_use = "a refused pin leaves the entry evictable while the caller draws with it"]
    pub fn pin(&mut self, key: &TextCacheKey) -> bool {
        match self.cache.peek_mut(key) {
            Some(resident) => {
                resident.pins = resident.pins.saturating_add(1);
                true
            }
            None => false,
        }
    }

    /// Decrement the pin count.  Tolerant of extra unpins, and of a key that is no
    /// longer resident (no-op rather than panic).
    pub fn unpin(&mut self, key: &TextCacheKey) {
        if let Some(resident) = self.cache.peek_mut(key) {
            resident.pins = resident.pins.saturating_sub(1);
        }
    }

    pub fn pin_count(&self, key: &TextCacheKey) -> u32 {
        self.cache.peek(key).map_or(0, |resident| resident.pins)
    }

    /// Insert a freshly-rendered entry under `key`.  Returns the list
    /// of GL texture ids the caller must delete on the render thread
    /// — these are LRU victims and any prior occupant of `key`.
    /// Refuses to insert oversized entries (silently drops; texture
    /// id is returned for deletion).
    #[must_use = "texture ids in the returned Vec must be deleted on the render thread"]
    pub fn insert(&mut self, key: TextCacheKey, entry: CachedTextEntry) -> Vec<u32> {
        let mut evicted: Vec<u32> = Vec::new();

        // Oversized inserts: never store, hand the texture back for
        // immediate deletion.
        if entry.size_bytes > self.max_size_bytes {
            evicted.push(entry.texture_id);
            return evicted;
        }

        // Evict LRU tail until we have room (or only pinned entries
        // remain — in which case the cache is allowed to sit
        // over-budget, matching `io::image_cache`'s contract).
        while self.current_size_bytes + entry.size_bytes > self.max_size_bytes {
            if let Some(victim_key) = self
                .cache
                .iter()
                .rev()
                .find(|(_, resident)| resident.pins == 0)
                .map(|(k, _)| k.clone())
            {
                if let Some(victim) = self.cache.pop(&victim_key) {
                    self.current_size_bytes = self
                        .current_size_bytes
                        .saturating_sub(victim.entry.size_bytes);
                    evicted.push(victim.entry.texture_id);
                }
            } else {
                break;
            }
        }

        // Insert.  `LruCache::push` evicts the absolute tail if at
        // the entry-count cap; that eviction is independent of our
        // byte-budget logic and may displace a pinned entry, which
        // we must avoid.  Manually evict an unpinned tail first if
        // we're at the entry cap.
        if self.cache.len() >= self.cache.cap().get() && !self.cache.contains(&key) {
            if let Some(victim_key) = self
                .cache
                .iter()
                .rev()
                .find(|(_, resident)| resident.pins == 0)
                .map(|(k, _)| k.clone())
            {
                if let Some(victim) = self.cache.pop(&victim_key) {
                    self.current_size_bytes = self
                        .current_size_bytes
                        .saturating_sub(victim.entry.size_bytes);
                    evicted.push(victim.entry.texture_id);
                }
            }
            // If every entry is pinned and we're at the cap, allow
            // the LRU's own eviction in `push` below to fire; it
            // may displace a pinned entry but the over-pin
            // condition is itself indicating misuse.  In normal
            // operation pins are short-lived (one in-flight
            // command), so the at-cap-all-pinned case is
            // pathological.
        }

        let new_size = entry.size_bytes;
        // A replacement inherits the outgoing entry's pins: the in-flight command
        // that took them is still going to draw from this key.
        let pins = self.cache.peek(&key).map_or(0, |resident| resident.pins);
        if let Some((_, old)) = self.cache.push(key, Resident { entry, pins }) {
            self.current_size_bytes = self.current_size_bytes.saturating_sub(old.entry.size_bytes);
            evicted.push(old.entry.texture_id);
        }
        self.current_size_bytes += new_size;

        evicted
    }

    /// Memory-pressure trim.  Returns the texture ids the caller must
    /// delete on the render thread.  Pinned entries are preserved
    /// (same contract as `io::image_cache::trim`).
    #[must_use]
    pub fn trim(&mut self, level: TrimLevel) -> Vec<u32> {
        let mut evicted: Vec<u32> = Vec::new();
        if self.cache.is_empty() {
            return evicted;
        }

        let start_size = self.current_size_bytes;
        let target = level.retained_bytes(self.max_size_bytes);

        while self.current_size_bytes > target {
            let victim_key = self
                .cache
                .iter()
                .rev()
                .find(|(_, resident)| resident.pins == 0)
                .map(|(k, _)| k.clone());
            // Only pinned entries left: a pin means a `TexImageSource` (text-cache source)
            // command is still in flight for that texture, so the cache sits over
            // its ceiling until the command executes.
            let Some(k) = victim_key else { break };
            if let Some(victim) = self.cache.pop(&k) {
                self.current_size_bytes = self
                    .current_size_bytes
                    .saturating_sub(victim.entry.size_bytes);
                evicted.push(victim.entry.texture_id);
            } else {
                break;
            }
        }

        let freed = start_size.saturating_sub(self.current_size_bytes);
        self.trim_bytes_released = self.trim_bytes_released.saturating_add(freed as u64);
        evicted
    }

    pub fn stats(&self) -> TextCacheStats {
        TextCacheStats {
            entries: self.cache.len(),
            size_bytes: self.current_size_bytes,
            max_bytes: self.max_size_bytes,
            hits: self.hits,
            misses: self.misses,
            trim_bytes_released: self.trim_bytes_released,
        }
    }
}

/// One session's text texture cache plus its font generation counter.
///
/// Handed out as an `Arc` so both entry points — the JS thread through
/// `CanvasOpState`, the render thread through `CanvasManager` — take the
/// handle once at session bring-up and hold it for the session's life.
/// A `fillText` therefore locks only *this* session's `Mutex`; two
/// sessions rendering text concurrently never contend, which is what
/// the "no cross-session lock on a per-event path" rule requires.
pub struct SessionTextCache {
    cache: Mutex<TextTextureCache>,
    /// Per-session font generation.  Bumped by this session's render
    /// thread on `LoadFont`, read by this session's JS thread when it
    /// builds a `TextCacheKey`.  An atomic rather than cache state so
    /// the read never blocks behind the cache lock.
    font_generation: AtomicU64,
}

impl Default for SessionTextCache {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_BYTES)
    }
}

impl std::fmt::Debug for SessionTextCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTextCache")
            .field("font_generation", &self.font_generation())
            .finish_non_exhaustive()
    }
}

impl SessionTextCache {
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            cache: Mutex::new(TextTextureCache::with_limits(max_entries, max_bytes)),
            font_generation: AtomicU64::new(1),
        }
    }

    /// Acquire this session's cache.  The budget and LRU behind this
    /// lock belong to this session alone, so one game's eviction
    /// pressure cannot evict another game's entries.
    #[inline]
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, TextTextureCache> {
        self.cache.lock()
    }

    /// Read this session's current generation token without bumping it.
    /// JS uses this when constructing a `TextCacheKey`.
    #[inline]
    pub fn font_generation(&self) -> u64 {
        self.font_generation.load(Ordering::Relaxed)
    }

    /// Roll this session's generation forward, making every entry keyed
    /// under the previous value unreachable **for this session only**.
    /// Called by this session's render thread on font load / replace.
    #[inline]
    pub fn bump_font_generation(&self) -> u64 {
        self.font_generation.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Shared per-session handle.
pub type SharedTextCache = std::sync::Arc<SessionTextCache>;

/// Per-host registry.  Mirrors the `console_log` and `stats` per-host
/// registries: an outer `RwLock<HashMap<host_id, Arc<_>>>` touched only
/// at session bring-up and teardown, never on a render path — the
/// per-event paths hold the `Arc` they took at bring-up.
static SESSION_CACHES: LazyLock<RwLock<HashMap<i32, SharedTextCache>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Get (or create) the text texture cache belonging to `host_id`.
///
/// Total by construction, so neither entry point has to care which of
/// them reaches the session first: the render thread and the JS thread
/// each call this once during bring-up and keep the returned `Arc`.
///
/// A texture name inserted through one host's handle is unreachable
/// through any other host's handle.  That is the whole point: each
/// Session builds its own EGL display and context (one `CanvasManager`
/// per host render thread, `create_pbuffer_context` with no sharing
/// between managers), so a GL name is meaningful only inside the
/// context that minted it.
pub fn text_cache_for_host(host_id: i32) -> SharedTextCache {
    if let Some(existing) = SESSION_CACHES.read().get(&host_id) {
        return existing.clone();
    }
    SESSION_CACHES
        .write()
        .entry(host_id)
        .or_insert_with(|| std::sync::Arc::new(SessionTextCache::default()))
        .clone()
}

/// Drop `host_id`'s cache at session teardown so neither its bytes nor
/// its GL names — both meaningless once the session's EGL context is
/// gone — are retained for process life.  Returns the handle if one was
/// registered, for callers that still want to drain it.
pub fn unregister_text_cache(host_id: i32) -> Option<SharedTextCache> {
    SESSION_CACHES.write().remove(&host_id)
}

/// Whether `host_id` currently has a registered cache.  Diagnostics and
/// teardown assertions only.
pub fn text_cache_registered(host_id: i32) -> bool {
    SESSION_CACHES.read().contains_key(&host_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use migo_contention_probe::{PATIENCE, PerEventPath, assert_completes_while_locked};

    fn k(text: &str, color: u32) -> TextCacheKey {
        TextCacheKey {
            text: text.into(),
            font_request: "Arial 14px".into(),
            font_size_bits: 14.0f32.to_bits(),
            font_weight: 400,
            italic: false,
            fill_color: color,
            text_align: TextAlign::Start,
            text_baseline: TextBaseline::Alphabetic,
            canvas_w: 128,
            canvas_h: 32,
            font_generation: 1,
        }
    }

    fn entry(id: u32, bytes: usize) -> CachedTextEntry {
        CachedTextEntry {
            texture_id: id,
            width: 128,
            height: 32,
            size_bytes: bytes,
        }
    }

    #[test]
    fn insert_then_get_roundtrip() {
        let mut c = TextTextureCache::with_limits(16, 1024 * 1024);
        let key = k("hi", 0xffffffff);
        assert!(c.insert(key.clone(), entry(1, 1000)).is_empty());
        let got = c.get(&key).expect("hit");
        assert_eq!(got.texture_id, 1);
        let s = c.stats();
        assert_eq!(s.entries, 1);
        assert_eq!(s.size_bytes, 1000);
        assert_eq!(s.hits, 1);
    }

    #[test]
    fn lru_evicts_oldest_unpinned() {
        let mut c = TextTextureCache::with_limits(16, 2_000);
        let _ = c.insert(k("a", 1), entry(1, 1000));
        let _ = c.insert(k("b", 2), entry(2, 1000));
        let evicted = c.insert(k("c", 3), entry(3, 1000));
        assert_eq!(evicted, vec![1]); // "a" was LRU tail
        assert!(c.get(&k("a", 1)).is_none());
        assert!(c.get(&k("b", 2)).is_some());
        assert!(c.get(&k("c", 3)).is_some());
    }

    #[test]
    fn pinned_entry_survives_pressure() {
        let mut c = TextTextureCache::with_limits(16, 2_000);
        let pinned = k("p", 1);
        let _ = c.insert(pinned.clone(), entry(1, 1000));
        assert!(c.pin(&pinned));
        let _ = c.insert(k("b", 2), entry(2, 1000));
        // Inserting "c" needs room. The LRU tail is the pinned "p", but the
        // pin protects it, so eviction skips past it to the unpinned "b" —
        // the cache stays within budget by dropping "b" instead of displacing
        // the pin (see the evict-unpinned-until-room contract on `insert`).
        let evicted = c.insert(k("c", 3), entry(3, 1000));
        assert_eq!(evicted, vec![2]); // unpinned "b" was the victim
        assert!(c.peek(&pinned).is_some()); // pinned "p" survived
        assert!(c.get(&k("b", 2)).is_none());
    }

    #[test]
    fn oversized_entry_dropped_immediately() {
        let mut c = TextTextureCache::with_limits(16, 1024);
        let evicted = c.insert(k("big", 0), entry(99, 4096));
        assert_eq!(evicted, vec![99]);
        assert_eq!(c.stats().entries, 0);
    }

    #[test]
    fn unpin_idempotent_at_zero() {
        let mut c = TextTextureCache::with_limits(4, 1024);
        let key = k("a", 0);
        let _ = c.insert(key.clone(), entry(1, 100));
        c.unpin(&key); // no-op
        assert!(c.pin(&key));
        c.unpin(&key);
        c.unpin(&key); // tolerated
        assert_eq!(c.pin_count(&key), 0);
    }

    #[test]
    fn a_pin_on_a_key_with_no_resident_entry_is_refused() {
        // The pin count lives on the entry, so there is nothing to hold a pin for a
        // key the cache has never seen. Refusing is what makes that visible: a
        // silently accepted pin would read as protection that does not exist.
        let mut c = TextTextureCache::with_limits(4, 1024);
        let key = k("absent", 0);
        assert!(!c.pin(&key));
        assert_eq!(c.pin_count(&key), 0);
    }

    #[test]
    fn replacing_a_pinned_entry_carries_the_pin_to_its_successor() {
        // The in-flight command that took the pin is still going to draw from this
        // key, so a replacement that started at zero pins would let the very next
        // insert evict the texture out from under it.
        let mut c = TextTextureCache::with_limits(2, 4096);
        let key = k("live", 0);
        let _ = c.insert(key.clone(), entry(1, 1000));
        assert!(c.pin(&key));

        let displaced = c.insert(key.clone(), entry(2, 1000));
        assert_eq!(
            displaced,
            vec![1],
            "the old texture is handed back for deletion"
        );
        assert_eq!(c.pin_count(&key), 1);

        let evicted = c.trim(TrimLevel::Background);
        assert!(
            evicted.is_empty(),
            "a carried pin still protects the successor"
        );
        assert_eq!(c.peek(&key).expect("still resident").texture_id, 2);
    }

    #[test]
    fn moderate_pressure_reads_as_a_ceiling_not_a_share_of_what_is_resident() {
        // A level names how much the cache may keep, so a cache already inside that
        // ceiling is asked for nothing and a repeated signal cannot compound. The
        // per-frame cost of getting this wrong is re-rasterizing every label dropped.
        let mut c = TextTextureCache::with_limits(64, 8_000);
        for i in 0..2u32 {
            let _ = c.insert(k(&format!("small{i}"), i), entry(i, 1_000));
        }
        assert!(
            c.trim(TrimLevel::RunningModerate).is_empty(),
            "a cache holding 2 KB of an 8 KB budget must not be churned"
        );

        for i in 2..8u32 {
            let _ = c.insert(k(&format!("small{i}"), i), entry(i, 1_000));
        }
        assert_eq!(c.stats().size_bytes, 8_000);
        let first = c.trim(TrimLevel::RunningModerate);
        assert!(!first.is_empty());
        assert_eq!(c.stats().size_bytes, 6_000);
        assert!(
            c.trim(TrimLevel::RunningModerate).is_empty(),
            "the same level asked for more the second time, so it is being read as a \
             share of what is left rather than as a ceiling"
        );
    }

    #[test]
    fn trim_running_low_releases_about_half() {
        let mut c = TextTextureCache::with_limits(16, 8 * 1000);
        for i in 0..8u32 {
            let _ = c.insert(k(&format!("k{i}"), i), entry(100 + i, 1000));
        }
        let before = c.stats().size_bytes;
        let evicted = c.trim(TrimLevel::RunningLow);
        let after = c.stats().size_bytes;
        assert!(!evicted.is_empty());
        assert_eq!(before - after, (evicted.len() * 1000));
        assert!(after <= before / 2 + 1000);
    }

    #[test]
    fn trim_background_keeps_pinned() {
        let mut c = TextTextureCache::with_limits(8, 4 * 1000);
        let pinned = k("live", 0);
        let _ = c.insert(pinned.clone(), entry(1, 1000));
        assert!(c.pin(&pinned));
        let _ = c.insert(k("idle", 0), entry(2, 1000));
        let evicted = c.trim(TrimLevel::Background);
        assert_eq!(evicted, vec![2]);
        assert!(c.peek(&pinned).is_some());
    }

    #[test]
    fn font_generation_advances() {
        let c = text_cache_for_host(9_001);
        let g0 = c.font_generation();
        let g1 = c.bump_font_generation();
        assert!(g1 > g0);
        assert_eq!(c.font_generation(), g1);
    }

    // ── Per-session isolation ────────────────────────────────────────────────
    //
    // Each Session owns its own EGL display + context (one `CanvasManager`
    // per host render thread, `create_pbuffer_context` with no sharing), so
    // a GL texture name is only meaningful inside the context that minted
    // it.  A cache reachable across sessions therefore hands session B a
    // name that means nothing in B's context, or lets B delete a name A is
    // still drawing with.  These tests pin that isolation down.

    /// Two host ids must never see one another's registrations, even
    /// across a re-run of the same test binary.  Distinct per test so the
    /// registry can be asserted without cross-test interference.
    fn unique_host_pair() -> (i32, i32) {
        use std::sync::atomic::AtomicI32;
        static NEXT: AtomicI32 = AtomicI32::new(9_000);
        let base = NEXT.fetch_add(2, Ordering::Relaxed);
        (base, base + 1)
    }

    struct HostCacheGuard(i32);

    impl Drop for HostCacheGuard {
        fn drop(&mut self) {
            unregister_text_cache(self.0);
        }
    }

    #[test]
    fn two_sessions_do_not_share_texture_ids_for_the_same_logical_key() {
        let (a_id, b_id) = unique_host_pair();
        let (_ga, _gb) = (HostCacheGuard(a_id), HostCacheGuard(b_id));
        let a = text_cache_for_host(a_id);
        let b = text_cache_for_host(b_id);

        // The identical logical key: same text, font, size, weight, colour,
        // align, baseline and canvas dimensions.  This is exactly the
        // collision two games rendering the same label hit.
        let key = k("立即购买", 0xffffffff);

        // Session A rasterizes it into texture name 41 in A's context.
        assert!(a.lock().insert(key.clone(), entry(41, 4_096)).is_empty());
        // Session B rasterizes the same label into name 77 in B's context.
        // 77 and 41 are unrelated names in unrelated GL namespaces.
        assert!(
            b.lock().insert(key.clone(), entry(77, 4_096)).is_empty(),
            "session B's insert must not evict or replace session A's entry"
        );

        assert_eq!(
            a.lock()
                .get(&key)
                .expect("session A keeps its own entry")
                .texture_id,
            41,
            "session A must still resolve to the texture name minted in A's context"
        );
        assert_eq!(
            b.lock()
                .get(&key)
                .expect("session B keeps its own entry")
                .texture_id,
            77,
            "session B must resolve to its own texture name, never A's"
        );

        // Neither side may observe the other's bytes either.
        assert_eq!(a.lock().stats().entries, 1);
        assert_eq!(b.lock().stats().entries, 1);
    }

    #[test]
    fn font_generation_is_per_session() {
        let (a_id, b_id) = unique_host_pair();
        let (_ga, _gb) = (HostCacheGuard(a_id), HostCacheGuard(b_id));
        let a = text_cache_for_host(a_id);
        let b = text_cache_for_host(b_id);

        let b_gen_before = b.font_generation();
        let mut b_key = k("shared label", 0x11223344);
        b_key.font_generation = b_gen_before;
        let _ = b.lock().insert(b_key.clone(), entry(5, 1_000));

        // Session A reloads a font.  Only A's generation may roll forward.
        let a_gen = a.bump_font_generation();
        assert!(a_gen > 1, "session A's own generation must advance");
        assert_eq!(
            b.font_generation(),
            b_gen_before,
            "one game reloading a font must not roll another game's generation forward"
        );
        assert!(
            b.lock().get(&b_key).is_some(),
            "session B's cached text must survive session A's font reload"
        );
    }

    #[test]
    fn eviction_pressure_is_scoped_to_one_session() {
        let (a_id, b_id) = unique_host_pair();
        let (_ga, _gb) = (HostCacheGuard(a_id), HostCacheGuard(b_id));
        let a = text_cache_for_host(a_id);
        let b = text_cache_for_host(b_id);

        let b_key = k("b-only", 0xdeadbeef);
        let _ = b.lock().insert(b_key.clone(), entry(200, 1_000));

        // Session A churns well past any shared budget.  Its own entries
        // may be evicted; B's may not, and no id A hands back for deletion
        // may be one of B's.
        let mut a_evicted: Vec<u32> = Vec::new();
        for i in 0..64u32 {
            a_evicted.extend(
                a.lock()
                    .insert(k(&format!("a{i}"), i), entry(1_000 + i, 1_000)),
            );
        }
        assert!(
            !a_evicted.contains(&200),
            "session A's eviction pressure returned session B's texture id \
             200 for deletion: {a_evicted:?}"
        );
        assert!(
            b.lock().get(&b_key).is_some(),
            "session A's eviction pressure evicted session B's entry"
        );

        // Trim is the other pressure path and must be scoped the same way.
        let trimmed = a.lock().trim(TrimLevel::Background);
        assert!(
            !trimmed.contains(&200),
            "session A's trim returned session B's texture id: {trimmed:?}"
        );
        assert!(
            b.lock().get(&b_key).is_some(),
            "session A's trim evicted session B's entry"
        );
    }

    #[test]
    fn dropping_one_session_cache_leaves_the_other_intact() {
        let (a_id, b_id) = unique_host_pair();
        let _gb = HostCacheGuard(b_id);
        let a = text_cache_for_host(a_id);
        let b = text_cache_for_host(b_id);

        let key = k("survivor", 0x0f0f0f0f);
        let _ = a.lock().insert(key.clone(), entry(11, 2_000));
        let _ = b.lock().insert(key.clone(), entry(22, 2_000));

        // Session A ends.
        let removed = unregister_text_cache(a_id).expect("session A was registered");
        drop(removed);
        drop(a);
        assert!(
            !text_cache_registered(a_id),
            "session A's cache must be dropped at teardown, not retained for process life"
        );

        assert_eq!(
            b.lock().get(&key).expect("session B unaffected").texture_id,
            22,
            "dropping session A's cache must not disturb session B's entries"
        );
        assert!(text_cache_registered(b_id));

        // A fresh session reusing A's id starts empty — no texture name
        // from the dead EGL context survives into it.
        let _ga = HostCacheGuard(a_id);
        let reborn = text_cache_for_host(a_id);
        assert!(
            reborn.lock().peek(&key).is_none(),
            "a new session must not inherit the previous session's texture names"
        );
    }

    /// Steady-state allocation gate, on the path `op_text_cache_peek_pin` takes for every
    /// `fillText` that hits: lock this session's cache, look the entry up, pin it
    /// for the render thread, release the pin.
    ///
    /// Key construction is excluded on purpose. The op receives its `String`s from
    /// V8, so the key's two heap strings are already paid for at the boundary; what
    /// this measures is whether the cache adds any more.
    #[test]
    fn steady_state_text_cache_hit_never_reaches_the_heap() {
        let session = SessionTextCache::with_limits(16, 1024 * 1024);
        let key = k("steady", 0xffff_ffff);
        assert!(
            session
                .lock()
                .insert(key.clone(), entry(7, 1000))
                .is_empty()
        );

        assert_no_steady_state_allocation(
            Burst {
                path: "text_texture_cache: per-fillText lock, peek, pin and unpin on a hit",
                warmup: 4,
                measured: 64,
            },
            |_| {
                let mut cache = session.lock();
                let hit = cache.peek(&key).expect("a resident entry stays resident");
                assert!(cache.pin(&key));
                assert_eq!(cache.pin_count(&key), 1);
                cache.unpin(&key);
                hit.texture_id
            },
        );
    }

    /// The steady-state requirement: no per-event path acquires a lock shared beyond its own session.
    ///
    /// Task 0.16 freed the render path of the session registry by resolving the handle
    /// once at bring-up, and that freedom has been *structural* ever since — which
    /// the contention gate explicitly does not accept. This holds the registry against a frame.
    #[test]
    fn a_per_frame_text_cache_hit_does_not_reach_the_session_registry() {
        let (host_id, _spare) = unique_host_pair();
        let _guard = HostCacheGuard(host_id);
        // The one acquisition a session is allowed: bring-up, before any frame.
        let session = text_cache_for_host(host_id);
        let key = k("held", 0x0102_0304);
        assert!(
            session
                .lock()
                .insert(key.clone(), entry(11, 1000))
                .is_empty()
        );

        let texture_id = assert_completes_while_locked(
            PerEventPath {
                path: "SessionTextCache lock, peek, pin and unpin on a fillText hit",
                shared_lock: "text_texture_cache SESSION_CACHES",
                patience: PATIENCE,
            },
            &SESSION_CACHES,
            move || {
                let mut cache = session.lock();
                let hit = cache.peek(&key).map(|entry| entry.texture_id);
                assert!(cache.pin(&key));
                cache.unpin(&key);
                hit
            },
        );

        // An operation that found nothing would satisfy the gate without ever
        // exercising the frame path.
        assert_eq!(texture_id, Some(11));
    }
}
