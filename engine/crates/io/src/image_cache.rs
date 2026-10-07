//! Decoded-image LRU with a W-TinyLFU admission filter.
//!
//! Caches decoded RGBA image data so repeated `drawImage` calls
//! don't re-run the decode + resize pipeline.  Keyed on
//! `(path, mount_generation, target_w, target_h)` so a hot-update
//! (generation bump) or a different pre-resize size naturally
//! invalidates stale slots.
//!
//! # W-TinyLFU admission
//!
//! A plain size-bounded LRU loses badly when a scene scrolls through
//! dozens of one-shot images — they fill the cache and evict the
//! handful of truly-hot sprites the UI keeps drawing. This module
//! fronts the LRU with a [`CountMinSketch`] and applies the Caffeine
//! admission rule: when the LRU is full, a newcomer is only admitted
//! if its estimated frequency is ≥ the current LRU victim's. Hot
//! items "earn their way in" after being referenced at least once
//! more than the current victim; genuinely one-shot items never
//! displace a warm entry.
//!
//! The rule collapses to "always admit" until the cache is full, so
//! first-touch cost matches the old LRU exactly.
//!
//! # Memory-pressure trim
//!
//! Android's `ComponentCallbacks2.onTrimMemory` is wired through
//! `shared::protocol::host_cmd::OnMemoryWarning`; the host loop
//! translates the level into an [`ImageCache::trim`] call that
//! drops a fraction of the cached bytes. Levels follow Android's
//! scheme exactly: 5 / 10 / 15 → running moderate / low / critical;
//! 20 → UI hidden; 40 / 60 / 80 → background / moderate / complete.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::LazyLock;

use lru::LruCache;
use parking_lot::Mutex;
use shared::protocol::io_cmd::NormalizedImage;

use crate::count_min_sketch::CountMinSketch;

/// Default maximum cache size: 64 MB
const DEFAULT_MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Default maximum number of cached entries
const DEFAULT_MAX_ENTRIES: usize = 256;

/// Structured cache key: `(real_path, mount_generation, target_w, target_h)`.
///
/// `target_w = target_h = 0` means "full resolution" — the path most
/// Canvas `drawImage` hits.  Non-zero targets correspond to
/// `createImageBitmap` / `drawImage(…, dw, dh)` paths that request a
/// specific pre-resize, so those get their own cache slots instead of
/// hammering the decode+resize pipeline each frame.
pub type ImageCacheKey = (String, u64, u32, u32);

/// Convenience constructor for the common full-resolution key.
#[inline]
pub fn full_res_key(path: String, generation: u64) -> ImageCacheKey {
    (path, generation, 0, 0)
}

/// Convenience: key for a specific pre-resized variant.  `target_w` or
/// `target_h` == 0 is normalised to 0 to match the "full res" sentinel.
#[inline]
pub fn resized_key(path: String, generation: u64, target_w: u32, target_h: u32) -> ImageCacheKey {
    (path, generation, target_w, target_h)
}

/// Report an unpin with no pin behind it.
///
/// Not a panic: the likeliest cause is refcount accounting drift in
/// `runtime-v8::rendering::image::cache`, and taking the process down for it would
/// trade a leaked pin for a crash. Warn once so the bug is visible and keep going.
fn unpin_without_pin_is_a_bug(key: &ImageCacheKey) {
    shared::warn_once!(
        path = key.0.as_str(),
        gen = key.1,
        tw = key.2,
        th = key.3,
        "image_cache unpin without matching pin (accounting bug suspected)"
    );
}

/// Cached image entry.
///
/// `owners` records which Sessions have asked for these bytes, and it is the
/// reason this type is no longer handed out: [`ImageCache::get`] returns the
/// `NormalizedImage` instead, whose clone is an `Arc` bump and two `u32`. That
/// call sits on the `texImage2D` frame path, so cloning a `Vec` alongside it
/// would put a heap allocation there — what Section 7.3 forbids.
///
/// One entry can have several owners, because two games loading the same file
/// share one decoded copy on purpose. Bytes are therefore attributed to every
/// owner rather than split between them: per-Session totals can exceed the
/// resident total, and that overlap is the sharing working.
struct CachedImage {
    image: NormalizedImage,
    size_bytes: usize,
    owners: Vec<i32>,
    /// Live aliases holding these bytes. A non-zero count makes the entry immune
    /// to LRU eviction, admission rejection, trim, and clear.
    ///
    /// On the entry rather than in a `HashMap<ImageCacheKey, u32>` beside the cache,
    /// for the reason the text texture cache moved its own: a separately keyed map
    /// needs an *owned* key to record a pin and drops it again when the count falls
    /// to zero, so an alias taken and released — a sprite pool recycling images —
    /// paid a `String` clone and a free per event. Keyed by the entry, a pin is a
    /// field. [`ImageCache::reservations`] holds the counts this field cannot,
    /// which are the ones for keys that are not resident yet.
    pins: u32,
}

impl CachedImage {
    fn new(image: NormalizedImage, session: i32) -> Self {
        let size_bytes = image.rgba.len();
        let mut owners = Vec::with_capacity(1);
        owners.push(session);
        Self {
            image,
            size_bytes,
            owners,
            pins: 0,
        }
    }

    #[inline]
    fn owned_by(&self, session: i32) -> bool {
        self.owners.contains(&session)
    }

    /// Record `session` as depending on these bytes. Allocates only the first
    /// time a given Session touches a given entry, never in steady state.
    #[inline]
    fn add_owner(&mut self, session: i32) {
        if !self.owned_by(session) {
            self.owners.push(session);
        }
    }
}

/// One Session's own lookup outcomes. Kept per Session because a shared
/// aggregate let one game watch another's asset loading.
#[derive(Default, Clone, Copy)]
struct SessionCounters {
    hits: u64,
    misses: u64,
}

/// Cache statistics for monitoring.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub entries: usize,
    pub size_bytes: usize,
    pub max_bytes: usize,
    pub hits: u64,
    pub misses: u64,
    /// Count of admission-filter rejections. Each rejection avoided
    /// an eviction that W-TinyLFU judged worse than keeping the
    /// current LRU victim. Ratio vs `misses` is a proxy for cache
    /// churn savings; a high value means the workload is heavy on
    /// one-shot images and the admission filter is doing real work.
    pub admissions_rejected: u64,
    /// Count of bytes evicted specifically by an
    /// [`ImageCache::trim`] call (memory-pressure driven), as
    /// opposed to routine LRU eviction. Useful for confirming the
    /// `onTrimMemory` hook actually ran after a spike.
    pub trim_bytes_released: u64,
}

impl CacheStats {
    /// Calculate hit rate (0.0 - 1.0).
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Trim levels mirroring Android `ComponentCallbacks2.TRIM_MEMORY_*`.
/// The host translates the raw integer into one of these variants
/// before calling [`ImageCache::trim`] so the cache isn't coupled
/// to Android-specific constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimLevel {
    /// `TRIM_MEMORY_RUNNING_MODERATE` (5) — mild pressure, drop a
    /// quarter of the cache.
    RunningModerate,
    /// `TRIM_MEMORY_RUNNING_LOW` (10) — drop half.
    RunningLow,
    /// `TRIM_MEMORY_RUNNING_CRITICAL` (15) — drop everything.
    RunningCritical,
    /// `TRIM_MEMORY_UI_HIDDEN` (20) — app fully obscured; drop half.
    UiHidden,
    /// `TRIM_MEMORY_BACKGROUND` / `MODERATE` / `COMPLETE` (40/60/80)
    /// — backgrounded; drop everything, we're not drawing.
    Background,
}

impl TrimLevel {
    /// Decode Android's raw integer level into a [`TrimLevel`].
    /// Unknown values map to the closest documented bucket so new
    /// Android releases don't silently disable the hook.
    pub fn from_android(raw: i32) -> Self {
        match raw {
            5 => TrimLevel::RunningModerate,
            10 => TrimLevel::RunningLow,
            15 => TrimLevel::RunningCritical,
            20 => TrimLevel::UiHidden,
            40 | 60 | 80 => TrimLevel::Background,
            // Anything higher than critical collapses to the most
            // aggressive policy we have; anything lower is "mild".
            n if n >= 15 => TrimLevel::RunningCritical,
            _ => TrimLevel::RunningModerate,
        }
    }

    /// Bytes the cache may keep at this level, as a ceiling on the budget.
    ///
    /// A ceiling, deliberately, rather than a fraction of *current* usage. The
    /// pressure signal is delivered per Session — a host app running two games calls
    /// `notifyMemoryWarning` once for each from a single Android `onTrimMemory` — and
    /// this cache is shared between them, so a "release a quarter of what is left"
    /// rule compounded: N Sessions released `1 - (1 - f)^N`, about 58% at moderate
    /// pressure with three games instead of 25%. Against a ceiling the second and
    /// third calls find the cache already under it and do nothing, so the level means
    /// the same thing however many Sessions relay it.
    ///
    /// It also stops a cache sitting well inside its budget from being churned: at
    /// 20 MB of a 64 MB budget, moderate pressure asks for nothing, where the old
    /// rule paid for 5 MB of re-decodes to relieve nothing that mattered.
    fn retained_bytes(self, budget: usize) -> usize {
        match self {
            TrimLevel::RunningModerate => budget / 4 * 3,
            TrimLevel::RunningLow | TrimLevel::UiHidden => budget / 2,
            // Nothing is retained under critical or background pressure; pinned
            // entries survive anyway, since dropping a live alias's bytes trades a
            // memory saving for a black texture.
            TrimLevel::RunningCritical | TrimLevel::Background => 0,
        }
    }
}

/// Decoded-image cache with TinyLFU admission.
///
/// # Pinning (live-vs-cached split)
///
/// Production showed that relying on pure LRU semantics corrupts
/// rendering when a caller still *actively references* an entry that
/// just fell off the tail.  The failure mode is specific to GL
/// texture uploads: `op_tex_image_2d_from_image` reads the decoded
/// RGBA from here, and on a miss it silently no-ops — the
/// caller-allocated GL texture stays uninitialised and samples as
/// solid black on every device we've profiled.  Games routinely
/// keep image handles alive for the duration of a scene (often
/// several seconds of cold LRU tail time), which is more than enough
/// for the admission filter to rotate the entry out.
///
/// Industry convention for this class of cache is a *live-vs-cached*
/// split (Flutter `ImageCache`, Chromium `MemoryCache`, Android
/// `BitmapPool` / Glide's active-resource map).  Active resources
/// are reference-counted and NEVER evictable; only idle resources
/// participate in LRU bookkeeping.  The field below implements that
/// split: a non-zero pin count promotes the entry to the "live"
/// set.  Eviction and trim both skip pinned entries; callers lift
/// the pin via [`Self::unpin`] exactly when the upstream reference
/// count reaches zero (see `runtime-v8/src/rendering/image/cache.rs`).
///
/// When the cache is at its byte budget and every remaining entry
/// is pinned, new inserts still succeed — the invariant "bytes for
/// actively referenced images are always available" is stronger
/// than the soft byte budget, because the alternative is a black
/// frame on screen.  The over-budget state resolves itself the
/// moment any alias is released.  Eviction of pinned entries during
/// `clear` / `trim` is intentionally impossible — the host has no
/// way to know which entries are still live, and rendering
/// correctness trumps the budget.
pub struct ImageCache {
    cache: LruCache<ImageCacheKey, CachedImage>,
    current_size: usize,
    max_size: usize,
    hits: u64,
    misses: u64,
    /// Frequency oracle used by the admission filter.  Reset on
    /// `clear`; size follows the cache's entry budget.
    sketch: CountMinSketch,
    admissions_rejected: u64,
    trim_bytes_released: u64,
    /// Pins taken for keys that are **not resident**, which is the one count
    /// [`CachedImage::pins`] cannot hold.
    ///
    /// The pin path establishes an alias before the decode finishes, so a pin
    /// routinely arrives before the bytes do (`begin_load` → decode → `insert`).
    /// A reservation records that intent and [`ImageCache::insert`] adopts it, so
    /// the newly resident entry arrives pre-pinned.
    ///
    /// Recording one costs an owned key, which is why the resident case does not
    /// go through here: a reservation is taken once per decode, beside a decode
    /// that allocates the bitmap itself, whereas pinning a resident entry is the
    /// per-event path Section 7.3 governs.
    ///
    /// **A key is in exactly one home.** A reservation is removed the moment the
    /// entry becomes resident, and handed back if that entry is later evicted with
    /// pins still on it.
    reservations: HashMap<ImageCacheKey, u32>,
    /// Per-Session lookup outcomes, beside the process aggregate rather than
    /// replacing it: the aggregate still describes the one cache that exists,
    /// while a game may only be told about its own traffic.
    sessions: HashMap<i32, SessionCounters>,
}

impl ImageCache {
    /// Create a new cache with default settings.
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_CACHE_BYTES)
    }

    /// Create a new cache with custom limits.
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        // A zero-entry cap would panic in LruCache::new; clamp to
        // `NonZeroUsize::MIN` (= 1) so misconfiguration degrades to
        // a thrashing-but-safe one-slot cache instead of a process
        // abort.
        let cap = NonZeroUsize::new(max_entries).unwrap_or(NonZeroUsize::MIN);
        Self {
            cache: LruCache::new(cap),
            current_size: 0,
            max_size: max_bytes,
            hits: 0,
            misses: 0,
            sketch: CountMinSketch::new_for_capacity(max_entries),
            admissions_rejected: 0,
            trim_bytes_released: 0,
            reservations: HashMap::new(),
            sessions: HashMap::new(),
        }
    }

    /// Increment the pin count for `key`.  While any pin is live
    /// the entry is exempt from LRU eviction, admission rejection,
    /// trim, and `clear`.  Idempotent — each call must be paired
    /// with exactly one [`Self::unpin`] call.
    ///
    /// Safe to call before the entry exists in the cache: the pin
    /// is recorded ahead of time so a subsequent `insert(key, _)`
    /// arrives pre-pinned.  This matters for the
    /// begin_load → decode → insert sequence, where the alias
    /// (and therefore the pin intent) is established before the
    /// bytes finish decoding.
    pub fn pin(&mut self, key: &ImageCacheKey) {
        // Resident first, and it is the case that must not reach the heap: this is
        // the per-event half of the path, and the entry already owns a copy of the
        // key. Only the not-yet-resident case pays for a reservation's owned key.
        let count = match self.cache.peek_mut(key) {
            Some(resident) => {
                resident.pins = resident.pins.saturating_add(1);
                resident.pins
            }
            None => {
                let count = self.reservations.entry(key.clone()).or_insert(0);
                *count += 1;
                *count
            }
        };
        // Diag trace: pin/unpin traffic is tightly coupled to
        // alias lifecycle, so a log stream of pin transitions is
        // invaluable when investigating "my texture black-screened
        // but the Image was still alive" bugs.  Trace-level
        // because frequency tracks image-create cadence (dozens
        // per scene load), so off by default.
        tracing::trace!(
            path = key.0.as_str(),
            gen = key.1,
            tw = key.2,
            th = key.3,
            pin_count = count,
            "image_cache pin"
        );
    }

    /// Decrement the pin count for `key`.  When the count hits
    /// zero the entry falls back to regular LRU eligibility — it
    /// is NOT evicted immediately.  The next eviction pass
    /// (insert over-budget or trim) may reclaim it if it is the
    /// LRU tail.
    ///
    /// No-op when the key has no pin recorded; this matches the
    /// "extra unpin" defensive case without panicking.
    pub fn unpin(&mut self, key: &ImageCacheKey) {
        let new_count = match self.cache.peek_mut(key) {
            Some(resident) if resident.pins > 0 => {
                resident.pins -= 1;
                resident.pins
            }
            Some(_) => {
                // Resident but unpinned: the same accounting drift the branch
                // below reports, seen through the other home.
                unpin_without_pin_is_a_bug(key);
                return;
            }
            None => match self.reservations.get_mut(key) {
                Some(count) => {
                    *count = count.saturating_sub(1);
                    let remaining = *count;
                    if remaining == 0 {
                        self.reservations.remove(key);
                    }
                    remaining
                }
                None => {
                    unpin_without_pin_is_a_bug(key);
                    return;
                }
            },
        };
        tracing::trace!(
            path = key.0.as_str(),
            gen = key.1,
            tw = key.2,
            th = key.3,
            pin_count = new_count,
            "image_cache unpin"
        );
    }

    /// Test helper / diagnostic: current pin count for `key`, 0 if
    /// absent.  Not intended for hot-path use — looks up in a
    /// separate HashMap and takes an extra lock boundary.
    #[inline]
    #[allow(dead_code)]
    pub fn pin_count(&self, key: &ImageCacheKey) -> u32 {
        match self.cache.peek(key) {
            Some(resident) => resident.pins,
            None => self.reservations.get(key).copied().unwrap_or(0),
        }
    }

    /// Remove the LRU-tail entry that is NOT currently pinned, if
    /// any exists.  Returns the freed byte count.
    ///
    /// When every remaining entry is pinned, returns `None` and
    /// leaves the cache unchanged — the caller must accept
    /// over-budget state rather than evict a live resource.
    fn pop_unpinned_lru(&mut self) -> Option<usize> {
        // `iter()` walks MRU → LRU; `.rev()` flips to LRU → MRU
        // so the first unpinned key we find is the coldest
        // evictable entry.  O(N) worst case but N ≤ 256 by
        // design, so the scan is cheap vs the memcpy a decode
        // would cost if we got eviction wrong.
        let victim_key = self
            .cache
            .iter()
            .rev()
            .find(|(_, v)| v.pins == 0)
            .map(|(k, _)| k.clone())?;
        let victim = self.cache.pop(&victim_key)?;
        self.current_size = self.current_size.saturating_sub(victim.size_bytes);
        Some(victim.size_bytes)
    }

    /// Get an image from cache on behalf of `session`. Hitting a key still bumps
    /// the frequency counter so long-lived entries keep their admission
    /// advantage.
    ///
    /// A hit records `session` as an owner. Reading an entry another game decoded
    /// is exactly how sharing pays off, and it also makes this Session depend on
    /// those bytes — so `clear_for_session` must not drop them while it still
    /// does, and `stats_for_session` should count them.
    pub fn get(&mut self, key: &ImageCacheKey, session: i32) -> Option<NormalizedImage> {
        // Frequency accounting runs for every lookup, hit or miss,
        // so the sketch reflects "how popular is this key" not "how
        // often did it hit the cache" — the latter would feedback-
        // lock cold-but-hot paths out forever.
        self.sketch.increment(key);
        let counters = self.sessions.entry(session).or_default();
        match self.cache.get_mut(key) {
            Some(cached) => {
                self.hits += 1;
                counters.hits += 1;
                cached.add_owner(session);
                Some(cached.image.clone())
            }
            None => {
                self.misses += 1;
                counters.misses += 1;
                None
            }
        }
    }

    /// Check whether an image key is currently cached without affecting hit/miss stats.
    pub fn contains(&self, key: &ImageCacheKey) -> bool {
        self.cache.contains(key)
    }

    /// Insert an image into cache. Applies the W-TinyLFU admission
    /// rule when the cache is at its size budget:
    ///
    ///  1. The newcomer's CM-sketch frequency is read (post-
    ///     increment) from the unified sketch.
    ///  2. The current LRU victim's frequency is read.
    ///  3. If the newcomer's frequency is strictly less than the
    ///     victim's, the newcomer is rejected (not cached this call);
    ///     a rejection counter ticks up. The sketch still sees the
    ///     access, so a repeated request will cross the threshold.
    ///  4. Otherwise evict LRU victims until there's room, then
    ///     insert.
    pub fn insert(&mut self, key: ImageCacheKey, image: NormalizedImage, session: i32) {
        let new_freq = self.sketch.increment(&key);
        let mut cached = CachedImage::new(image, session);
        let new_size = cached.size_bytes;

        // Refuse unusable entries up front.
        if new_size > self.max_size {
            return;
        }

        // Over-budget case: consult the admission filter before we
        // evict anything. This is the point where the W-TinyLFU
        // "window" concept collapses to "don't evict a warm entry
        // to make room for a cold one."
        //
        // Pinned-vs-unpinned reshuffle: the admission filter check
        // still compares against the LRU tail, but admission-
        // rejection is only safe for *unpinned* newcomers.  An
        // incoming entry whose key is already pinned (e.g. the
        // begin_load flow pinned it moments ago) must succeed
        // unconditionally — rejecting it would leave the texture
        // upload with no bytes to read later, which is the exact
        // black-texture regression the pin mechanism exists to
        // prevent.  We also skip the admission check when the key
        // is already resident (replace-in-place path).
        // Adopt whichever home holds this key's pins. Taken *before* the eviction
        // loop below for the same reason the owners are: that loop can pop this very
        // key, and reading afterwards would find nothing.
        cached.pins = self.reservations.remove(&key).unwrap_or(0)
            + self.cache.peek(&key).map_or(0, |resident| resident.pins);
        let newcomer_pinned = cached.pins > 0;
        // Re-decoding a resident key must not forget who already depended on it,
        // and the owners are carried across **here**, before the eviction loop
        // below. That loop can pop this very key when it is the coldest unpinned
        // entry — two Sessions finishing a decode of the same image into a cache
        // with no spare room is enough — and reading the owners afterwards would
        // then find nothing and silently drop the first Session's claim, leaving
        // its bytes evictable by the second Session's `clear_for_session`.
        let already_resident = match self.cache.peek(&key) {
            Some(resident) => {
                for owner in &resident.owners {
                    cached.add_owner(*owner);
                }
                true
            }
            None => false,
        };
        if self.current_size + new_size > self.max_size && !newcomer_pinned && !already_resident {
            if let Some((victim_key, victim)) = self.cache.peek_lru() {
                // Only count the true LRU tail as the victim when
                // it's not pinned — a pinned tail item doesn't
                // represent a real eviction candidate and its
                // sketch count shouldn't gate newcomers.
                let victim_pinned = victim.pins > 0;
                let victim_freq = if victim_pinned {
                    0
                } else {
                    self.sketch.estimate(victim_key)
                };
                if !victim_pinned && new_freq < victim_freq {
                    // The sketch says the newcomer is colder than
                    // the item we'd have to evict. Drop it on the
                    // floor — on retry its sketch count will have
                    // advanced, which is how cold-but-hot paths
                    // eventually earn their way in.
                    self.admissions_rejected += 1;
                    shared::stats::io_metrics_global()
                        .image_cache_admissions_rejected
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
            }
        }

        // Evict only unpinned tail entries.  When every remaining
        // entry is pinned the cache legitimately sits over its
        // byte budget until some upstream alias releases — the
        // over-budget state is the least-bad option, since the
        // alternative is dropping bytes the app is actively
        // using.
        while self.current_size + new_size > self.max_size {
            if self.pop_unpinned_lru().is_none() {
                // All remaining entries are pinned; we must
                // insert anyway to keep the live-set invariant.
                // Rate-limited info (not warn) so the operator
                // sees the over-budget condition without noise
                // on every admission — a steady-state game
                // that legitimately holds more pinned bytes
                // than `max_size` should log this at most once
                // per 5 seconds.
                shared::info_rate_limited!(
                    std::time::Duration::from_secs(5),
                    current_bytes = self.current_size,
                    max_bytes = self.max_size,
                    incoming_bytes = new_size,
                    pinned_entries = self.pinned_entry_count(),
                    total_entries = self.cache.len(),
                    "image_cache over budget: all remaining entries pinned"
                );
                break;
            }
        }

        // `push` returns a displaced pair in two different situations, and they are
        // not interchangeable: the previous value under *this* key, whose pins were
        // adopted above, or — when the LRU is at its **entry** cap — the tail under
        // some other key, whose pins would otherwise vanish. Asking the cache which
        // one is about to happen is exact; `already_resident` is not, because the
        // eviction loop above may have popped this key in between.
        let displaces_this_key = self.cache.contains(&key);

        // Insert and update size (handle potential replacement).
        if let Some((displaced_key, displaced)) = self.cache.push(key, cached) {
            self.current_size = self.current_size.saturating_sub(displaced.size_bytes);
            // The entry cap lives inside `LruCache` and cannot be taught about pins,
            // so give a tail entry's pins back to the reservation table: the alias is
            // still live and a re-insert must arrive pinned, which is the behaviour a
            // pin map keyed beside the cache used to provide for free. Paying an
            // owned key here is the trade that table exists for, on a path that runs
            // only when a pinned entry is displaced by entry count.
            if !displaces_this_key && displaced.pins > 0 {
                *self.reservations.entry(displaced_key).or_insert(0) += displaced.pins;
            }
        }
        self.current_size += new_size;
    }

    /// Resident entries a live alias is holding.
    fn pinned_entry_count(&self) -> usize {
        self.cache.iter().filter(|(_, v)| v.pins > 0).count()
    }

    /// Clear the entire cache except pinned entries.  Resets the
    /// frequency sketch too — on a full clear there's no history
    /// worth keeping around.
    ///
    /// Pinned entries survive because the app still holds live
    /// references to them; wiping them out would leave dangling
    /// aliases whose later `texImage2D` upload would black-screen.
    /// Not reachable from a Session, and deliberately so: this discards entries
    /// every live game may be depending on, which is why the game-visible
    /// `ImageCache.clear()` routes through [`Self::clear_for_session`] instead.
    /// Retained at crate visibility for the cache's own tests.
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        // Fast path: no pins → full clear as before. Reservations are counts for
        // keys that are *not* resident, so they never keep an entry here.
        if self.pinned_entry_count() == 0 {
            self.cache.clear();
            self.current_size = 0;
            self.sketch.reset();
            return;
        }

        // Walk entries and drop only the non-pinned ones.  We
        // collect the to-drop key list up front to avoid mutating
        // the LRU under its own iterator.
        let drop_keys: Vec<ImageCacheKey> = self
            .cache
            .iter()
            .filter(|(_, v)| v.pins == 0)
            .map(|(k, _)| k.clone())
            .collect();
        for k in drop_keys {
            if let Some(removed) = self.cache.pop(&k) {
                self.current_size = self.current_size.saturating_sub(removed.size_bytes);
            }
        }
        // Pinned entries are still "warm" by definition; keep the
        // sketch counts so the partial clear doesn't force them
        // to re-earn their admission.
    }

    /// Drop `session`'s claim on every entry, and evict the entries left with no
    /// claim at all.
    ///
    /// This backs the game-visible `ImageCache.clear()`. A game may discard what
    /// it is holding; it may not discard what another game is holding, which the
    /// process-wide [`Self::clear`] did. An entry two games own therefore survives
    /// with the caller's ownership dropped, and pins still win over eviction for
    /// the same reason they do everywhere else: the alternative is a live alias
    /// whose next upload reads no bytes and renders black.
    pub fn clear_for_session(&mut self, session: i32) {
        let orphaned: Vec<ImageCacheKey> = self
            .cache
            .iter()
            .filter(|(_, v)| v.owned_by(session) && v.owners.len() == 1 && v.pins == 0)
            .map(|(k, _)| k.clone())
            .collect();

        for (_, entry) in self.cache.iter_mut() {
            entry.owners.retain(|owner| *owner != session);
        }

        for key in orphaned {
            if let Some(removed) = self.cache.pop(&key) {
                self.current_size = self.current_size.saturating_sub(removed.size_bytes);
            }
        }
        // The sketch is left alone. Its counts describe the shared cache, and the
        // entries other games still own have earned their admission.
    }

    /// Forget `session` entirely at teardown: its claims and its counters.
    ///
    /// Nothing is evicted. The bytes are context-independent and shared on
    /// purpose, so a later Session loading the same unchanged file should still
    /// get them for free; they age out through the LRU like any other entry.
    pub fn release_session(&mut self, session: i32) {
        for (_, entry) in self.cache.iter_mut() {
            entry.owners.retain(|owner| *owner != session);
        }
        self.sessions.remove(&session);
    }

    /// Release a fraction of the cache in response to OS memory
    /// pressure. Returns the number of bytes actually freed.
    ///
    /// Eviction walks the LRU tail first, so the freshest entries
    /// survive longer. On `TrimLevel::Background` / `RunningCritical`
    /// the cache is fully cleared (same as [`clear`] but leaves the
    /// sketch intact — the app may come back to the foreground and
    /// the learned frequency distribution is still useful).
    pub fn trim(&mut self, level: TrimLevel) -> usize {
        if self.cache.is_empty() {
            return 0;
        }

        let start_size = self.current_size;
        let target_size = level.retained_bytes(self.max_size);
        while self.current_size > target_size {
            if self.pop_unpinned_lru().is_none() {
                // Only pinned entries remain; cannot free more without violating the
                // live-reference invariant. Pinned bytes are what an alias's next
                // `texImage2D` will read, so the alternative to keeping them over
                // budget is a black texture — the same contract Flutter's
                // `ImageCache.clear()` exposes.
                break;
            }
        }
        let freed = start_size.saturating_sub(self.current_size);
        self.trim_bytes_released += freed as u64;
        // Saturating, not wrapping. This counter is cumulative and its wire
        // slot is a fixed 4 bytes, and `TrimLevel::Background` retains nothing,
        // so each trip through the background can charge it the whole budget --
        // 64 background transitions at the 64 MiB default is enough to wrap it.
        // A plain `fetch_add` would then report a near-zero total to whoever is
        // reading it precisely because they are chasing memory pressure. Pinned
        // at u32::MAX it reads as "at least 4 GiB", which is the honest answer;
        // widening the field would move a published offset in the stats blob.
        let freed_u32 = u32::try_from(freed).unwrap_or(u32::MAX);
        let _ = shared::stats::io_metrics_global()
            .image_cache_trim_bytes
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |total| Some(total.saturating_add(freed_u32)),
            );
        freed
    }

    /// What `session` is told about the cache.
    ///
    /// `hits` and `misses` are its own lookups. `entries` and `size_bytes` cover
    /// the entries it owns, counting a shared entry's bytes in full for each
    /// owner: two games holding one 4 MB atlas are each told 4 MB, so per-Session
    /// totals can exceed the resident total. Splitting the bytes would need an
    /// arbitrary rule, and under-reporting what a game depends on is the more
    /// misleading of the two. `max_bytes` is the one shared budget, reported as
    /// such.
    ///
    /// Nothing here varies with another Session's traffic, which is the point:
    /// the process-wide figures let one game observe another's asset loading.
    pub fn stats_for_session(&self, session: i32) -> CacheStats {
        let counters = self.sessions.get(&session).copied().unwrap_or_default();
        let (entries, size_bytes) = self
            .cache
            .iter()
            .filter(|(_, v)| v.owned_by(session))
            .fold((0usize, 0usize), |(n, bytes), (_, v)| {
                (n + 1, bytes + v.size_bytes)
            });
        CacheStats {
            entries,
            size_bytes,
            max_bytes: self.max_size,
            hits: counters.hits,
            misses: counters.misses,
            admissions_rejected: 0,
            trim_bytes_released: 0,
        }
    }

    /// Process-wide statistics, for diagnostics that are allowed to see the whole
    /// cache. Never hand these to a game: see [`Self::stats_for_session`].
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.cache.len(),
            size_bytes: self.current_size,
            max_bytes: self.max_size,
            hits: self.hits,
            misses: self.misses,
            admissions_rejected: self.admissions_rejected,
            trim_bytes_released: self.trim_bytes_released,
        }
    }
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Global image cache instance.
static GLOBAL_CACHE: LazyLock<Mutex<ImageCache>> = LazyLock::new(|| Mutex::new(ImageCache::new()));

/// Get reference to global cache.
pub fn global_cache() -> parking_lot::MutexGuard<'static, ImageCache> {
    GLOBAL_CACHE.lock()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Session these mechanics tests load as. They exercise LRU, admission and
    /// pin behaviour, none of which is per-Session; the tests that *are* about
    /// Session isolation name their own ids.
    const ONE_SESSION: i32 = 1;
    use shared::protocol::io_cmd::NormalizedImage;
    use std::sync::Arc;

    fn rgba(width: u32, height: u32) -> NormalizedImage {
        NormalizedImage {
            width,
            height,
            rgba: Arc::new(vec![0u8; (width * height * 4) as usize]),
        }
    }

    #[test]
    fn full_res_and_resized_keys_do_not_collide() {
        // Regression: the same source path at full-res vs a specific
        // pre-resized dimension used to share a cache slot (resizes
        // were never cached at all), forcing the decoder to re-run
        // every frame.  With the 4-tuple key, each variant gets its
        // own slot and survives across draws.
        let mut cache = ImageCache::with_limits(16, 16 * 1024 * 1024);
        let full = full_res_key("/code/sprite.png".into(), 3);
        let r128 = resized_key("/code/sprite.png".into(), 3, 128, 128);
        let r64 = resized_key("/code/sprite.png".into(), 3, 64, 64);

        cache.insert(full.clone(), rgba(256, 256), ONE_SESSION);
        cache.insert(r128.clone(), rgba(128, 128), ONE_SESSION);
        cache.insert(r64.clone(), rgba(64, 64), ONE_SESSION);

        assert_eq!(cache.get(&full, ONE_SESSION).unwrap().width, 256);
        assert_eq!(cache.get(&r128, ONE_SESSION).unwrap().width, 128);
        assert_eq!(cache.get(&r64, ONE_SESSION).unwrap().width, 64);
        let stats = cache.stats();
        assert_eq!(stats.entries, 3);
        assert!(stats.hits >= 3);
    }

    #[test]
    fn generation_bump_evicts_all_sizes() {
        // A mount-generation bump (asset hot-reload) must invalidate
        // every variant of the path, which is naturally handled by
        // keying on generation: gen=9 entries simply never collide
        // with gen=10 lookups.
        let mut cache = ImageCache::with_limits(16, 16 * 1024 * 1024);
        cache.insert(
            full_res_key("/code/t.png".into(), 9),
            rgba(32, 32),
            ONE_SESSION,
        );
        cache.insert(
            resized_key("/code/t.png".into(), 9, 16, 16),
            rgba(16, 16),
            ONE_SESSION,
        );

        assert!(
            cache
                .get(&full_res_key("/code/t.png".into(), 10), ONE_SESSION)
                .is_none()
        );
        assert!(
            cache
                .get(&resized_key("/code/t.png".into(), 10, 16, 16), ONE_SESSION)
                .is_none()
        );
    }

    #[test]
    fn below_budget_inserts_never_rejected() {
        // The admission filter must only kick in when the cache is
        // full. Empty-cache inserts always succeed.
        let mut cache = ImageCache::with_limits(16, 1024 * 1024);
        cache.insert(full_res_key("/a.png".into(), 1), rgba(16, 16), ONE_SESSION);
        cache.insert(full_res_key("/b.png".into(), 1), rgba(16, 16), ONE_SESSION);
        let s = cache.stats();
        assert_eq!(s.entries, 2);
        assert_eq!(s.admissions_rejected, 0);
    }

    #[test]
    fn admission_keeps_hot_entry_across_cold_scans() {
        // Scenario: one hot image, many one-shot images, cache can
        // hold ~3 images worth of bytes.  Under plain LRU the hot
        // image would be evicted on the third cold touch; with
        // TinyLFU it must survive as long as `hot`'s frequency
        // sketch exceeds the cold items'.
        let img_bytes = 64 * 64 * 4;
        let cap_bytes = 4 * img_bytes; // room for ~4 images
        let mut cache = ImageCache::with_limits(16, cap_bytes);

        let hot = full_res_key("/hot.png".into(), 1);
        // Warm up "hot": multiple accesses bump its sketch count.
        for _ in 0..8 {
            let _ = cache.get(&hot, ONE_SESSION); // miss, bumps sketch
        }
        cache.insert(hot.clone(), rgba(64, 64), ONE_SESSION);
        // Each subsequent get bumps hot's count further.
        for _ in 0..8 {
            let _ = cache.get(&hot, ONE_SESSION);
        }

        // Flood with cold one-shot images.
        for i in 0..200u32 {
            let k = full_res_key(format!("/cold_{i}.png"), 1);
            cache.insert(k, rgba(64, 64), ONE_SESSION);
        }

        // The hot entry should still be cached; the admission
        // filter rejected most of the cold inserts once the hot
        // entry's frequency dominated.
        let s = cache.stats();
        assert!(
            cache.contains(&hot),
            "hot entry evicted by cold scan (rejections={}, entries={})",
            s.admissions_rejected,
            s.entries
        );
        assert!(
            s.admissions_rejected > 0,
            "no cold inserts were rejected by admission"
        );
    }

    /// One Android `onTrimMemory` reaches this cache once per live Session, because
    /// the host app relays it through each `GameSession` and the cache is shared. A
    /// level that meant "release a quarter of what is left" therefore compounded --
    /// three games turned a 25% request into about 58%, evicting a working set the OS
    /// never asked for.
    #[test]
    fn a_pressure_signal_relayed_by_every_session_trims_once() {
        let img_bytes = 32 * 32 * 4;
        let budget = 8 * img_bytes;
        let mut cache = ImageCache::with_limits(16, budget);
        for i in 0..8u32 {
            cache.insert(
                full_res_key(format!("/p{i}.png"), 1),
                rgba(32, 32),
                ONE_SESSION,
            );
        }
        assert_eq!(
            cache.stats().size_bytes,
            budget,
            "the fixture must start full"
        );

        let first = cache.trim(TrimLevel::RunningModerate);
        let after_first = cache.stats().size_bytes;
        assert!(
            first > 0,
            "the first relay of the signal must actually free bytes"
        );

        // The second and third Sessions relay the same signal.
        let second = cache.trim(TrimLevel::RunningModerate);
        let third = cache.trim(TrimLevel::RunningModerate);
        assert_eq!(
            (second, third),
            (0, 0),
            "the same pressure signal freed more each time another Session relayed \
             it, so N games multiply one request into N"
        );
        assert_eq!(
            cache.stats().size_bytes,
            after_first,
            "repeated relays of one signal must leave the cache where the first put it"
        );
        assert_eq!(
            after_first,
            budget / 4 * 3,
            "moderate pressure must land on its ceiling, not on a fraction of \
             whatever happened to be resident"
        );
    }

    /// The other half of reading a level as a ceiling: a cache already well inside
    /// its budget is asked for nothing. Evicting a quarter of it would buy the OS a
    /// few megabytes and cost a re-decode of every entry dropped.
    #[test]
    fn moderate_pressure_leaves_a_cache_inside_its_budget_alone() {
        let img_bytes = 32 * 32 * 4;
        let mut cache = ImageCache::with_limits(16, 8 * img_bytes);
        for i in 0..2u32 {
            cache.insert(
                full_res_key(format!("/q{i}.png"), 1),
                rgba(32, 32),
                ONE_SESSION,
            );
        }
        let before = cache.stats().size_bytes;
        assert_eq!(cache.trim(TrimLevel::RunningModerate), 0);
        assert_eq!(cache.stats().size_bytes, before);
    }

    /// The cumulative trim counter must saturate, never wrap.
    ///
    /// `image_cache_trim_bytes` is a process-wide `AtomicU32` and
    /// `TrimLevel::Background` retains nothing, so roughly 64 background
    /// transitions at the 64 MiB default budget are enough to carry it past
    /// `u32::MAX`. Wrapping there would report a near-zero total to exactly the
    /// person reading it because they are chasing memory pressure.
    ///
    /// The assertion is "never goes backwards" rather than an exact value on
    /// purpose: the counter is a shared singleton other tests in this binary
    /// also add to, and monotonicity is the property that distinguishes a
    /// saturating add from a wrapping one without depending on test ordering.
    #[test]
    fn trim_byte_counter_saturates_instead_of_wrapping() {
        use std::sync::atomic::Ordering;
        let counter = &shared::stats::io_metrics_global().image_cache_trim_bytes;
        // Park it just under the ceiling. Nothing in the codebase ever
        // decrements this counter, so from here any decrease is a wrap.
        let seed = u32::MAX - 1024;
        counter.store(seed, Ordering::Relaxed);

        let img_bytes = 32 * 32 * 4; // 4 KiB, comfortably more than the 1 KiB headroom
        let mut cache = ImageCache::with_limits(16, 8 * img_bytes);
        cache.insert(
            full_res_key("/wrap.png".into(), 1),
            rgba(32, 32),
            ONE_SESSION,
        );
        assert_eq!(cache.trim(TrimLevel::Background), img_bytes);

        let after = counter.load(Ordering::Relaxed);
        assert!(
            after >= seed,
            "cumulative trim counter wrapped: {after} < seed {seed}"
        );
        assert_eq!(after, u32::MAX, "should have pinned at the ceiling");
    }

    /// Aggressive levels stay absolute: everything unpinned goes, whatever the
    /// resident size was.
    #[test]
    fn background_pressure_still_empties_an_underfull_cache() {
        let img_bytes = 32 * 32 * 4;
        let mut cache = ImageCache::with_limits(16, 8 * img_bytes);
        cache.insert(full_res_key("/r.png".into(), 1), rgba(32, 32), ONE_SESSION);
        assert_eq!(cache.trim(TrimLevel::Background), img_bytes);
        assert_eq!(cache.stats().size_bytes, 0);
    }

    #[test]
    fn trim_running_low_frees_about_half() {
        let img_bytes = 32 * 32 * 4;
        let mut cache = ImageCache::with_limits(16, 8 * img_bytes);
        for i in 0..8u32 {
            cache.insert(
                full_res_key(format!("/p{i}.png"), 1),
                rgba(32, 32),
                ONE_SESSION,
            );
        }
        let before = cache.stats().size_bytes;
        let freed = cache.trim(TrimLevel::RunningLow);
        let after = cache.stats().size_bytes;
        assert_eq!(before - after, freed);
        // Half of `before` ± one entry's worth.
        let target = before / 2;
        let tolerance = img_bytes;
        assert!(
            after <= target + tolerance,
            "after={after} too high for target {target}"
        );
        assert_eq!(cache.stats().trim_bytes_released as usize, freed);
    }

    #[test]
    fn trim_background_clears_entries_but_keeps_sketch() {
        let mut cache = ImageCache::with_limits(16, 4 * 1024 * 1024);
        for _ in 0..10 {
            let _ = cache.get(&full_res_key("/hot.png".into(), 1), ONE_SESSION);
        }
        cache.insert(
            full_res_key("/hot.png".into(), 1),
            rgba(64, 64),
            ONE_SESSION,
        );

        let freed = cache.trim(TrimLevel::Background);
        let s = cache.stats();
        assert!(freed > 0);
        assert_eq!(s.entries, 0);
        assert_eq!(s.size_bytes, 0);

        // Sketch retained: a fresh insert of the hot key still sees
        // a non-zero frequency, so its re-admission is still
        // privileged on the next spike.
        let freq = cache.sketch.estimate(&full_res_key("/hot.png".into(), 1));
        assert!(freq > 0, "background trim wiped the frequency sketch");
    }

    #[test]
    fn trim_level_from_android_covers_known_codes() {
        assert_eq!(TrimLevel::from_android(5), TrimLevel::RunningModerate);
        assert_eq!(TrimLevel::from_android(10), TrimLevel::RunningLow);
        assert_eq!(TrimLevel::from_android(15), TrimLevel::RunningCritical);
        assert_eq!(TrimLevel::from_android(20), TrimLevel::UiHidden);
        assert_eq!(TrimLevel::from_android(40), TrimLevel::Background);
        assert_eq!(TrimLevel::from_android(60), TrimLevel::Background);
        assert_eq!(TrimLevel::from_android(80), TrimLevel::Background);
        // Unknown high code → closest known bucket is Critical.
        assert_eq!(TrimLevel::from_android(100), TrimLevel::RunningCritical);
    }

    #[test]
    fn clear_resets_sketch() {
        let mut cache = ImageCache::with_limits(4, 1024 * 1024);
        for _ in 0..10 {
            let _ = cache.get(&full_res_key("/k.png".into(), 1), ONE_SESSION);
        }
        cache.clear();
        assert_eq!(cache.sketch.estimate(&full_res_key("/k.png".into(), 1)), 0);
    }

    #[test]
    fn oversized_image_is_silently_dropped() {
        // One image bigger than the whole cache must neither be
        // stored nor trigger an eviction storm.
        let mut cache = ImageCache::with_limits(4, 1024);
        cache.insert(
            full_res_key("/big.png".into(), 1),
            rgba(64, 64),
            ONE_SESSION,
        ); // 16KB
        let s = cache.stats();
        assert_eq!(s.entries, 0);
        assert_eq!(s.size_bytes, 0);
    }

    /// Pin count prevents eviction even under LRU pressure.
    /// Regression test for the hxddd black-block bug: before the
    /// pin mechanism, a scene-load burst of 200 one-shot sprites
    /// would evict pinned-but-still-active textures and make them
    /// sample as black on the next `texImage2D` upload.
    #[test]
    fn pinned_entries_survive_lru_pressure() {
        let img_bytes = 64 * 64 * 4; // 16 KB
        let cap_bytes = 4 * img_bytes; // room for ~4 images
        let mut cache = ImageCache::with_limits(32, cap_bytes);

        let live = full_res_key("/avatar.png".into(), 1);
        cache.insert(live.clone(), rgba(64, 64), ONE_SESSION);
        cache.pin(&live);
        assert_eq!(cache.pin_count(&live), 1);

        // Flood with cold images (each the full image size so
        // they actually compete for bytes).  Crucially, the hot
        // sketch is NOT pre-warmed — under pure admission filter
        // the pinned entry would still face eviction pressure,
        // but the pin must override the filter decision.
        for i in 0..200u32 {
            let k = full_res_key(format!("/cold_{i}.png"), 1);
            cache.insert(k, rgba(64, 64), ONE_SESSION);
        }

        assert!(
            cache.contains(&live),
            "pinned entry evicted by LRU flood — black-texture regression returns"
        );

        // Unpin → entry becomes a regular LRU candidate again.
        // It should still be present (most recently used by us
        // via `contains`), but one more cold scan can now evict
        // it normally.
        cache.unpin(&live);
        assert_eq!(cache.pin_count(&live), 0);
        for i in 200..400u32 {
            let k = full_res_key(format!("/cold_{i}.png"), 1);
            cache.insert(k, rgba(64, 64), ONE_SESSION);
        }
        assert!(
            !cache.contains(&live),
            "unpinned entry should be evictable under pressure"
        );
    }

    /// `trim(Background)` is allowed to drop unpinned
    /// entries but must keep pinned ones.  This protects live
    /// avatar / HUD textures through an `onTrimMemory` storm.
    #[test]
    fn trim_preserves_pinned_entries() {
        let mut cache = ImageCache::with_limits(16, 4 * 1024 * 1024);

        let pinned = full_res_key("/live.png".into(), 1);
        let idle = full_res_key("/idle.png".into(), 1);
        cache.insert(pinned.clone(), rgba(64, 64), ONE_SESSION);
        cache.insert(idle.clone(), rgba(64, 64), ONE_SESSION);
        cache.pin(&pinned);

        let freed = cache.trim(TrimLevel::Background);
        // Exactly the idle entry's bytes should be released.
        let idle_bytes = 64 * 64 * 4;
        assert_eq!(freed, idle_bytes);
        assert!(cache.contains(&pinned));
        assert!(!cache.contains(&idle));

        // `clear` under the same contract.
        cache.clear();
        assert!(cache.contains(&pinned));
    }

    /// Pinning an absent key is valid (records intent), and
    /// a subsequent `insert` of that key arrives pre-pinned.
    #[test]
    fn pin_absent_key_is_honoured_on_later_insert() {
        let mut cache = ImageCache::with_limits(4, 2 * 64 * 64 * 4);
        let k = full_res_key("/pre.png".into(), 1);
        cache.pin(&k);
        cache.insert(k.clone(), rgba(64, 64), ONE_SESSION);

        // Evict-flood: the pinned entry must survive.
        for i in 0..50u32 {
            let kc = full_res_key(format!("/flood_{i}.png"), 1);
            cache.insert(kc, rgba(64, 64), ONE_SESSION);
        }
        assert!(cache.contains(&k));
    }

    // ── Per-Session attribution ─────────────────────────────────────────────
    //
    // This cache is shared between Sessions on purpose: its entries are decoded
    // RGBA under a key carrying the file's real identity, so two games loading one
    // asset hold one copy. What each game may *do* to it, and be told about it, is
    // the part that has to be its own.

    fn key(path: &str) -> ImageCacheKey {
        full_res_key(path.to_string(), 1)
    }

    #[test]
    fn one_games_clear_keeps_what_another_game_is_using() {
        let mut cache = ImageCache::new();
        let (a, b) = (11, 22);
        cache.insert(key("/a-only.png"), rgba(16, 16), a);
        cache.insert(key("/b-only.png"), rgba(16, 16), b);

        // Reached from game script through `ImageCache.clear()`.
        cache.clear_for_session(a);

        assert!(
            !cache.contains(&key("/a-only.png")),
            "a game clearing its own cache must actually drop its own entries"
        );
        assert!(
            cache.contains(&key("/b-only.png")),
            "one game's script cleared another game's decoded bytes"
        );
    }

    #[test]
    fn clearing_spares_an_entry_the_other_game_also_loaded() {
        let mut cache = ImageCache::new();
        let (a, b) = (33, 44);
        let shared = key("/shared-atlas.png");
        cache.insert(shared.clone(), rgba(32, 32), a);
        // B loading the same file is served the copy A decoded -- that is the
        // sharing working -- and depends on those bytes from here on.
        assert!(cache.get(&shared, b).is_some());

        cache.clear_for_session(a);
        assert!(
            cache.contains(&shared),
            "an entry two games hold was dropped when only one of them cleared"
        );

        // With A's claim gone, B clearing is the last claim and the bytes may go.
        cache.clear_for_session(b);
        assert!(!cache.contains(&shared));
    }

    #[test]
    fn a_game_is_told_only_about_its_own_traffic() {
        let mut cache = ImageCache::new();
        let (a, b) = (55, 66);
        cache.insert(key("/a1.png"), rgba(16, 16), a);
        cache.insert(key("/b1.png"), rgba(16, 16), b);
        cache.insert(key("/b2.png"), rgba(16, 16), b);
        // One hit and one miss for B, none for A.
        assert!(cache.get(&key("/b1.png"), b).is_some());
        assert!(cache.get(&key("/absent.png"), b).is_none());

        let seen_by_a = cache.stats_for_session(a);
        assert_eq!(seen_by_a.entries, 1, "game A sees another game's entries");
        assert_eq!(
            seen_by_a.hits, 0,
            "game A is told about lookups it never made"
        );
        assert_eq!(
            seen_by_a.misses, 0,
            "game A can watch another game's cache misses"
        );
        assert_eq!(
            seen_by_a.size_bytes,
            16 * 16 * 4,
            "game A's byte total includes bytes only another game asked for"
        );

        let seen_by_b = cache.stats_for_session(b);
        assert_eq!(seen_by_b.entries, 2);
        assert_eq!(seen_by_b.hits, 1);
        assert_eq!(seen_by_b.misses, 1);
    }

    #[test]
    fn shared_bytes_are_reported_in_full_to_each_owner() {
        // The decided semantics, asserted rather than left implicit: an entry two
        // games own has no non-arbitrary split, so each is told the whole size and
        // per-Session totals legitimately exceed the resident total. Under-reporting
        // what a game depends on would be the more misleading choice.
        let mut cache = ImageCache::new();
        let (a, b) = (77, 88);
        let shared = key("/one-copy.png");
        cache.insert(shared.clone(), rgba(64, 64), a);
        assert!(cache.get(&shared, b).is_some());

        let bytes = 64 * 64 * 4;
        assert_eq!(cache.stats_for_session(a).size_bytes, bytes);
        assert_eq!(cache.stats_for_session(b).size_bytes, bytes);
        assert_eq!(
            cache.stats().size_bytes,
            bytes,
            "the resident total must stay one copy, whatever the owners are told"
        );
    }

    #[test]
    fn a_departed_game_stops_being_charged_but_its_bytes_stay() {
        let mut cache = ImageCache::new();
        let (a, b) = (99, 111);
        let shared = key("/survives-teardown.png");
        cache.insert(shared.clone(), rgba(32, 32), a);
        assert!(cache.get(&shared, b).is_some());
        cache.insert(key("/a-alone.png"), rgba(32, 32), a);

        cache.release_session(a);

        assert!(
            cache.contains(&shared) && cache.contains(&key("/a-alone.png")),
            "teardown evicted decoded bytes; they are context-independent and a \
             later session loading the same unchanged file should get them free"
        );
        let orphaned = cache.stats_for_session(a);
        assert_eq!(
            orphaned.entries, 0,
            "a departed game is still charged bytes"
        );
        assert_eq!(orphaned.size_bytes, 0);
        assert_eq!(
            cache.stats_for_session(b).entries,
            1,
            "one game's teardown disturbed another's attribution"
        );
    }

    #[test]
    fn a_pinned_entry_survives_its_owners_clear() {
        // Same precedence as everywhere else in this cache: a pin means a live alias
        // will read these bytes, and the alternative to keeping them is a texture
        // upload that finds nothing and renders black.
        let mut cache = ImageCache::new();
        let a = 123;
        let pinned = key("/live.png");
        cache.pin(&pinned);
        cache.insert(pinned.clone(), rgba(16, 16), a);

        cache.clear_for_session(a);
        assert!(cache.contains(&pinned));
    }

    #[test]
    fn a_replaced_entry_keeps_the_first_games_claim_even_under_pressure() {
        // Two Sessions both miss, both decode, and the second insert lands on a key
        // the first already made resident. With no spare room the eviction loop can
        // pop that very key before the replacement goes in, and if the owners are
        // read after the loop the first game's claim vanishes -- leaving its bytes
        // for the second game's `clear_for_session` to evict.
        let one_image = 32 * 32 * 4;
        let mut cache = ImageCache::with_limits(16, one_image);
        let (a, b) = (211, 222);
        let contended = key("/both-decoded-it.png");

        cache.insert(contended.clone(), rgba(32, 32), a);
        cache.insert(contended.clone(), rgba(32, 32), b);

        cache.clear_for_session(b);
        assert!(
            cache.contains(&contended),
            "the first game's claim was lost when its entry was replaced, so the \
             second game's clear evicted bytes the first is still using"
        );
        assert_eq!(cache.stats_for_session(a).entries, 1);
    }

    /// A pin lives on the entry while the entry is resident and in the reservation
    /// table otherwise, and never in both. Nothing else in this cache is allowed to
    /// depend on which one it is, so the transfer has to leave the other empty.
    #[test]
    fn a_key_never_holds_its_pins_in_both_homes() {
        let mut cache = ImageCache::with_limits(8, 4 * 1024 * 1024);
        let k = key("/two-homes.png");

        // Pinned before the decode lands: the reservation table is the only home.
        cache.pin(&k);
        assert_eq!(cache.reservations.get(&k).copied(), Some(1));
        assert_eq!(cache.pin_count(&k), 1);

        // Resident: the entry takes the count over, and the reservation is gone.
        cache.insert(k.clone(), rgba(16, 16), ONE_SESSION);
        assert!(
            !cache.reservations.contains_key(&k),
            "an adopted reservation left a second count behind, so an unpin can \
             retire one home while the other keeps the entry unevictable forever"
        );
        assert_eq!(cache.pin_count(&k), 1);

        // Pinning again while resident must not reopen the other home.
        cache.pin(&k);
        assert!(!cache.reservations.contains_key(&k));
        assert_eq!(cache.pin_count(&k), 2);

        // A re-decode of a key that is resident *and* pinned — two Sessions both
        // finishing a decode of one image, which this cache shares on purpose —
        // replaces the entry under it. The pins move to the replacement; the copy
        // that was displaced must not leave a count behind as well.
        cache.insert(k.clone(), rgba(16, 16), ONE_SESSION);
        assert_eq!(
            cache.pin_count(&k),
            2,
            "a replacement dropped the pins of the aliases still holding the key"
        );
        assert!(
            !cache.reservations.contains_key(&k),
            "the displaced copy left its pins in the other home too, so the key \
             carries a count no unpin can ever retire and the entry never becomes \
             evictable again"
        );

        cache.unpin(&k);
        cache.unpin(&k);
        assert_eq!(cache.pin_count(&k), 0);
        assert!(!cache.reservations.contains_key(&k));
    }

    /// The LRU's *entry* cap lives inside `LruCache` and cannot be taught about
    /// pins, so it can displace an entry a live alias is holding. The pin has to
    /// survive that, because the alias still exists and the re-decode that follows
    /// must arrive pinned — which is what a pin map keyed beside the cache gave for
    /// free, and what moving the count onto the entry would otherwise have lost.
    #[test]
    fn a_pin_survives_the_entry_cap_displacing_what_it_held() {
        // Bytes are generous on purpose: this is the entry cap acting, not the
        // byte budget, whose eviction already refuses pinned entries.
        let mut cache = ImageCache::with_limits(2, 4 * 1024 * 1024);
        let live = key("/held.png");

        cache.insert(live.clone(), rgba(16, 16), ONE_SESSION);
        cache.pin(&live);
        cache.insert(key("/second.png"), rgba(16, 16), ONE_SESSION);
        cache.insert(key("/third.png"), rgba(16, 16), ONE_SESSION);

        assert!(
            !cache.contains(&live),
            "the entry cap is what this test needs to fire; it did not"
        );
        assert_eq!(
            cache.pin_count(&live),
            1,
            "the alias is still live, so its pin must outlive the entry the cap took"
        );

        // The re-decode arrives pre-pinned, so a byte-budget flood cannot take it.
        cache.insert(live.clone(), rgba(16, 16), ONE_SESSION);
        assert_eq!(cache.pin_count(&live), 1);
        assert!(!cache.reservations.contains_key(&live));
        cache.clear();
        assert!(
            cache.contains(&live),
            "a re-decode under a surviving pin came back unpinned"
        );
    }

    // ── Section 7.3: zero steady-state allocation ───────────────────────────

    /// Section 7.3, on the path `op_tex_image_2d_from_image` takes for every draw
    /// whose bytes come from this cache: `resolve_cached_image_rgba` looks the key
    /// up and hands back the decoded RGBA.
    ///
    /// Key construction is excluded on purpose, exactly as the text cache's gate
    /// excludes it: the caller already holds the key. What this measures is whether
    /// the *cache* adds a heap event to a hit -- the frequency increment, the
    /// per-Session counters, and the per-owner attribution against the entry's
    /// `owners` vector, which task 0.16 recorded as unmeasured.
    ///
    /// Two owners, not one, because `add_owner` scans that vector: a gate with a
    /// single owner would pass a `Vec` that grew on every hit.
    #[test]
    fn steady_state_image_cache_hit_never_reaches_the_heap() {
        let mut cache = ImageCache::with_limits(16, 4 * 1024 * 1024);
        let hot = key("/steady.png");
        cache.insert(hot.clone(), rgba(16, 16), 401);
        assert!(cache.get(&hot, 402).is_some());

        migo_alloc_probe::assert_no_steady_state_allocation(
            migo_alloc_probe::Burst {
                path: "io::image_cache: per-draw lookup on a hit, with owner attribution",
                warmup: 4,
                measured: 64,
            },
            |_| {
                let hit = cache
                    .get(&hot, 402)
                    .expect("a resident entry stays resident");
                hit.width
            },
        );
    }

    /// Section 7.3, on the path an alias takes across its own lifetime: `begin_load`
    /// pins the decoded bytes when it hands out an alias, and the alias's release
    /// unpins them. A game recycling image aliases -- a sprite pool, a scrolling
    /// list -- runs this pair per event.
    ///
    /// The pair is measured as a pair because that is the shape of the defect: a pin
    /// map keyed beside the entry needs an owned key to record the pin and drops it
    /// again when the count reaches zero, so the allocation and the free are one
    /// round trip rather than growth.
    /// Section 7.3's steady-state *growth* requirement, on the reservation table.
    ///
    /// This is the pin path's other half. The resident case above must not reach the
    /// heap at all; the *non*-resident case must, because a reservation records the
    /// pin beside the cache and needs an owned key to do it. So no burst can be
    /// written over it, and the only available question is whether the round trip
    /// gives the key back.
    ///
    /// The reservation table is the one structure in this cache that
    /// `current_size` does not account for, which makes it the one place growth can
    /// hide from every budget test *and* from the public API: a reservation left
    /// behind at a count of zero is indistinguishable from an absent one through
    /// `pin_count`, and invisible to `size_bytes`. Only the bytes show it.
    #[test]
    fn a_reservation_round_trip_gives_back_the_key_it_took() {
        let mut cache = ImageCache::with_limits(16, 4 * 1024 * 1024);

        migo_alloc_probe::assert_no_steady_state_growth(
            migo_alloc_probe::Cycle {
                path: "io::image_cache: pin and unpin a key that is not resident",
                warmup: 4,
                measured: 64,
            },
            |iteration| {
                // A distinct key per iteration, all the same length, so the bytes a
                // balanced round trip takes and returns are equal and the measured
                // net is exactly zero rather than approximately so. Repeating one
                // key would let a table that never released it still look balanced,
                // because the second pin of a live reservation allocates nothing.
                let absent = key(&format!("/absent{iteration:06}.png"));
                cache.pin(&absent);
                let pinned = cache.pin_count(&absent);
                cache.unpin(&absent);
                pinned
            },
        );
    }

    /// The same requirement one level up: a cache at its byte budget must give back
    /// what each admission takes.
    ///
    /// Measured against the allocator rather than against `current_size`, which is
    /// the point — the cache's own accounting counts an entry's pixels and nothing
    /// else, so a guard that reads it cannot see growth in what it does not count.
    #[test]
    fn a_cache_at_its_budget_does_not_grow_as_entries_turn_over() {
        const ENTRY_BYTES: usize = 16 * 16 * 4;
        let mut cache = ImageCache::with_limits(1024, ENTRY_BYTES * 8);

        migo_alloc_probe::assert_no_steady_state_growth(
            migo_alloc_probe::Cycle {
                path: "io::image_cache: admit one entry at the byte budget",
                // Long enough for the byte budget to bind, so the measured window
                // sees turnover rather than the cache legitimately filling.
                warmup: 32,
                measured: 64,
            },
            |iteration| {
                cache.insert(
                    key(&format!("/turnover{iteration:06}.png")),
                    rgba(16, 16),
                    501,
                );
            },
        );
    }
}
