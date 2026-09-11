//! Memoisation for the two expensive effect objects that every
//! Canvas2D drawing pass builds: the drop-shadow `ImageFilter` and
//! the dash `PathEffect`.
//!
//! Why a cache at all?  Both effect kinds are reference-counted
//! Skia handles (`RCHandle<SkImageFilter>` / `RCHandle<SkPathEffect>`),
//! and both are deterministic pure functions of their parameters:
//!
//!   * `drop_shadow(dx, dy, sigma_x, sigma_y, color)` → same
//!     `SkImageFilter` every time.
//!   * `dash(intervals, phase)` → same `SkPathEffect` every time.
//!
//! Games tend to set the same shadow on every button label, and the
//! same dash pattern on every selection outline.  Rebuilding the
//! effect on every paint pays the Skia constructor plus one
//! allocation per call; with a small LRU we pay that once per
//! distinct parameter tuple and hand back a cheap `Arc`-like
//! refcount bump on every subsequent hit.
//!
//! The caches live in thread-local storage because Skia effect
//! handles are not `Send` (they lean on Skia's own per-thread
//! reference counting).  Cap both at 32 entries — typical games use
//! a handful of shadow / dash configs plus occasional one-offs, so
//! the hit rate on small caps is essentially 100% while steady-state
//! memory stays in single-digit KB.

use std::cell::{Cell, RefCell};
use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;
use skia_safe::{ImageFilter, PathEffect, Point, Shader, TileMode, gradient_shader, image_filters};

use shared::protocol::render_cmd::GradientStop;

const SHADOW_CACHE_CAP: usize = 32;
const DASH_CACHE_CAP: usize = 32;
const GRADIENT_CACHE_CAP: usize = 64;
const SHADOW_CACHE_BYTES: usize = 8 * 1024;
const DASH_CACHE_BYTES: usize = 8 * 1024;
const GRADIENT_CACHE_BYTES: usize = 16 * 1024;
const MAX_DASH_CACHED_INTERVALS: usize = 64;
const MAX_GRADIENT_CACHED_STOPS: usize = 64;
const EFFECT_CACHE_ENTRY_OVERHEAD: usize = 128;
const SHADOW_ENTRY_BYTES: usize = EFFECT_CACHE_ENTRY_OVERHEAD;

/// Shadow-filter cache key.  All four parameters are bit-casted to
/// preserve exact equality: `f32::to_bits` is a lossless round-trip
/// except for the NaN payload, which never shows up on Canvas 2D
/// shadow parameters (JS coerces via `Number` which normalises
/// NaN to one pattern).
/// **Exact, not hashed.** Every parameter of the filter gets its own field, so
/// two distinct shadows can never share a key and the cache needs no
/// verification step.
///
/// The two sigma channels used to be folded into one `u32` by a `mix_u32`
/// helper whose own comment called it "reversible-ish". Two `u32`s do not fit in
/// one, so collisions existed by pigeonhole, and for any pair of `sigma_x`
/// values a colliding `sigma_y` can be solved for directly — the rotate and the
/// wrapping add are both bijections. On a collision the cache hands back an
/// `ImageFilter` built for a different blur or offset, which paints a wrong
/// shadow with no error anywhere.
///
/// Its two sibling caches in this file take the other route: hash the key and
/// re-verify the contents on hit, with a comment explaining that a collision
/// would otherwise return the wrong effect. They have to, because their keys
/// cover variable-length data. A shadow's parameters are five fixed-width
/// scalars, so the key can simply be all of them — which is also less code than
/// either alternative, and removes the only caller of `mix_u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ShadowKey {
    /// Premultiplied `SkColor` (ARGB u32).  Already embeds
    /// `global_alpha` modulation, so two calls with the same logical
    /// shadow colour but different alpha get different keys (which
    /// is correct — the filter bakes the colour in).
    color: u32,
    sigma_x_bits: u32,
    sigma_y_bits: u32,
    dx_bits: u32,
    dy_bits: u32,
}

/// Dash-effect cache key.  `intervals_hash` folds the full slice
/// into a 64-bit hash (FxHash is cheap and ordered so `[4, 2]` and
/// `[2, 4]` hash differently).  Collisions are tolerated silently
/// because the cache is a best-effort optimisation: on collision
/// we'd hand back the wrong `PathEffect`, so we ALSO store a copy
/// of the intervals in the value and re-verify on lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DashKey {
    intervals_hash: u64,
    phase_bits: u32,
}

struct DashEntry {
    intervals: Vec<f32>,
    phase: f32,
    effect: PathEffect,
}

/// Gradient-shader cache key.  Combines the geometric parameters
/// (packed as f32 bit patterns) with an identity-level reference
/// to the stop list (`Arc::as_ptr`) and the global alpha.
///
/// Using the `Arc` pointer address means two `ctx.createLinearGradient`
/// calls that share the same stop vector hit the cache on the second
/// use -- the JS façade always clones the `Arc<Vec<GradientStop>>`
/// inside `StyleKind`, so repeatedly assigning the same gradient
/// object to `fillStyle` produces pointer-equal keys.  Distinct
/// gradient objects with identical stops pay one build then hit.
/// Retaining the Arc in [`GradientEntry`] prevents address reuse while a
/// key is resident, so the hit path can trust identity and avoid a stop scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GradientKey {
    kind: u8, // 0 = linear, 1 = radial, 2 = conic
    /// Up to 7 f32 parameters as bit patterns; unused slots are 0.
    /// Linear: [x0, y0, x1, y1, 0, 0, 0]
    /// Radial: [x0, y0, r0, x1, y1, r1, 0]
    /// Conic:  [cx, cy, start_angle, 0, 0, 0, 0]
    geom_bits: [u32; 7],
    alpha_bits: u32,
    stops_addr: usize,
    stops_len: u32,
}

struct GradientEntry {
    /// Retaining this immutable revision makes `stops_addr` collision-safe:
    /// its allocation cannot be recycled while the cache entry is alive.
    /// Hits can therefore use identity instead of re-comparing every stop.
    stops_arc: Arc<Vec<GradientStop>>,
    shader: Shader,
}

#[inline]
fn dash_entry_bytes(intervals_len: usize) -> usize {
    intervals_len
        .saturating_mul(std::mem::size_of::<f32>())
        .saturating_add(EFFECT_CACHE_ENTRY_OVERHEAD)
}

#[inline]
fn gradient_entry_bytes(stops_len: usize) -> usize {
    stops_len
        .saturating_mul(std::mem::size_of::<GradientStop>())
        .saturating_add(EFFECT_CACHE_ENTRY_OVERHEAD)
}

thread_local! {
    static SHADOW_CACHE: RefCell<LruCache<ShadowKey, ImageFilter>> = RefCell::new(
        LruCache::new(NonZeroUsize::new(SHADOW_CACHE_CAP).expect("cap > 0"))
    );
    static DASH_CACHE: RefCell<LruCache<DashKey, DashEntry>> = RefCell::new(
        LruCache::new(NonZeroUsize::new(DASH_CACHE_CAP).expect("cap > 0"))
    );
    static GRADIENT_CACHE: RefCell<LruCache<GradientKey, GradientEntry>> = RefCell::new(
        LruCache::new(NonZeroUsize::new(GRADIENT_CACHE_CAP).expect("cap > 0"))
    );
    static SHADOW_CACHE_BYTES_RETAINED: Cell<usize> = const { Cell::new(0) };
    static DASH_CACHE_BYTES_RETAINED: Cell<usize> = const { Cell::new(0) };
    static GRADIENT_CACHE_BYTES_RETAINED: Cell<usize> = const { Cell::new(0) };
}

/// Fetch or build a drop-shadow `ImageFilter`.  Returns `None` when
/// Skia's `image_filters::drop_shadow` constructor itself returns
/// `None` (typically an OOM on the raw filter graph; should never
/// happen in practice but we propagate instead of panicking).
pub fn get_or_build_drop_shadow(
    color: u32,
    sigma_x: f32,
    sigma_y: f32,
    dx: f32,
    dy: f32,
) -> Option<ImageFilter> {
    let key = ShadowKey {
        color,
        sigma_x_bits: sigma_x.to_bits(),
        sigma_y_bits: sigma_y.to_bits(),
        dx_bits: dx.to_bits(),
        dy_bits: dy.to_bits(),
    };
    SHADOW_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if let Some(hit) = cache.get(&key) {
            crate::render_diagnostics::hit_shadow_filter_cache();
            return Some(hit.clone());
        }
        crate::render_diagnostics::miss_shadow_filter_cache();
        let filter = image_filters::drop_shadow(
            (dx, dy),
            (sigma_x, sigma_y),
            skia_safe::Color::from(color),
            None,
            None,
            None,
        )?;
        let bytes = SHADOW_ENTRY_BYTES;
        if let Some(_) = cache.pop(&key) {
            SHADOW_CACHE_BYTES_RETAINED.with(|counter| {
                counter.set(counter.get().saturating_sub(bytes));
            });
        }
        while SHADOW_CACHE_BYTES_RETAINED.with(|counter| counter.get().saturating_add(bytes))
            > SHADOW_CACHE_BYTES
        {
            let Some((_old_key, _old)) = cache.pop_lru() else {
                break;
            };
            SHADOW_CACHE_BYTES_RETAINED.with(|counter| {
                counter.set(counter.get().saturating_sub(bytes));
            });
        }
        if cache.len() >= SHADOW_CACHE_CAP {
            if cache.pop_lru().is_some() {
                SHADOW_CACHE_BYTES_RETAINED.with(|counter| {
                    counter.set(counter.get().saturating_sub(bytes));
                });
            }
        }
        cache.put(key, filter.clone());
        SHADOW_CACHE_BYTES_RETAINED.with(|counter| {
            counter.set(counter.get().saturating_add(bytes));
        });
        Some(filter)
    })
}

/// Fetch or build a dash `PathEffect`.  Returns `None` when Skia
/// rejects the intervals (empty list, odd count, negative entries).
///
/// The returned effect is an RC handle — cloning is a refcount bump,
/// never a deep copy.
pub fn get_or_build_dash(intervals: &[f32], phase: f32) -> Option<PathEffect> {
    if intervals.is_empty() {
        return None;
    }
    if intervals.len() > MAX_DASH_CACHED_INTERVALS {
        return PathEffect::dash(intervals, phase);
    }
    let intervals_hash = hash_intervals(intervals);
    let key = DashKey {
        intervals_hash,
        phase_bits: phase.to_bits(),
    };
    DASH_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        // On cache hit, re-verify intervals and phase — a 64-bit
        // hash collision would otherwise silently serve the wrong
        // dash, a class of bug that's painful to diagnose.
        if let Some(entry) = cache.get(&key) {
            if entry.intervals == intervals && entry.phase.to_bits() == phase.to_bits() {
                crate::render_diagnostics::hit_dash_effect_cache();
                return Some(entry.effect.clone());
            }
            // Hash collision with different payload — fall through to miss.
        }
        crate::render_diagnostics::miss_dash_effect_cache();
        let effect = PathEffect::dash(intervals, phase)?;
        let bytes = dash_entry_bytes(intervals.len());
        if let Some(old) = cache.pop(&key) {
            DASH_CACHE_BYTES_RETAINED.with(|counter| {
                counter.set(
                    counter
                        .get()
                        .saturating_sub(dash_entry_bytes(old.intervals.len())),
                );
            });
        }
        while DASH_CACHE_BYTES_RETAINED.with(|counter| counter.get().saturating_add(bytes))
            > DASH_CACHE_BYTES
        {
            let Some((_old_key, old)) = cache.pop_lru() else {
                break;
            };
            DASH_CACHE_BYTES_RETAINED.with(|counter| {
                counter.set(
                    counter
                        .get()
                        .saturating_sub(dash_entry_bytes(old.intervals.len())),
                );
            });
        }
        if cache.len() >= DASH_CACHE_CAP {
            if let Some((_old_key, old)) = cache.pop_lru() {
                DASH_CACHE_BYTES_RETAINED.with(|counter| {
                    counter.set(
                        counter
                            .get()
                            .saturating_sub(dash_entry_bytes(old.intervals.len())),
                    );
                });
            }
        }
        cache.put(
            key,
            DashEntry {
                intervals: intervals.to_vec(),
                phase,
                effect: effect.clone(),
            },
        );
        DASH_CACHE_BYTES_RETAINED.with(|counter| {
            counter.set(counter.get().saturating_add(bytes));
        });
        Some(effect)
    })
}

/// Fetch or build a linear-gradient `Shader`.  Caches on
/// (gradient kind, geometric parameters, stop-list identity, global
/// alpha).  See [`GradientKey`] for the cache-hit re-verification
/// strategy on `Arc::as_ptr` collisions.
pub fn get_or_build_linear_gradient(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    stops: &std::sync::Arc<Vec<GradientStop>>,
    global_alpha: f32,
) -> Option<Shader> {
    let key = GradientKey {
        kind: 0,
        geom_bits: [
            x0.to_bits(),
            y0.to_bits(),
            x1.to_bits(),
            y1.to_bits(),
            0,
            0,
            0,
        ],
        alpha_bits: global_alpha.to_bits(),
        stops_addr: std::sync::Arc::as_ptr(stops) as usize,
        stops_len: stops.len() as u32,
    };
    lookup_or_build_gradient(key, stops, || {
        let (colors, positions) = build_colors_positions(stops, global_alpha)?;
        gradient_shader::linear(
            (Point::new(x0, y0), Point::new(x1, y1)),
            gradient_shader::GradientShaderColors::Colors(&colors),
            Some(&positions[..]),
            TileMode::Clamp,
            None,
            None,
        )
    })
}

/// Fetch or build a radial gradient (two-point conical under the hood,
/// matching Canvas 2D's `createRadialGradient`).
pub fn get_or_build_radial_gradient(
    x0: f32,
    y0: f32,
    r0: f32,
    x1: f32,
    y1: f32,
    r1: f32,
    stops: &std::sync::Arc<Vec<GradientStop>>,
    global_alpha: f32,
) -> Option<Shader> {
    let key = GradientKey {
        kind: 1,
        geom_bits: [
            x0.to_bits(),
            y0.to_bits(),
            r0.to_bits(),
            x1.to_bits(),
            y1.to_bits(),
            r1.to_bits(),
            0,
        ],
        alpha_bits: global_alpha.to_bits(),
        stops_addr: std::sync::Arc::as_ptr(stops) as usize,
        stops_len: stops.len() as u32,
    };
    lookup_or_build_gradient(key, stops, || {
        let (colors, positions) = build_colors_positions(stops, global_alpha)?;
        gradient_shader::two_point_conical(
            Point::new(x0, y0),
            r0,
            Point::new(x1, y1),
            r1,
            gradient_shader::GradientShaderColors::Colors(&colors),
            Some(&positions[..]),
            TileMode::Clamp,
            None,
            None,
        )
    })
}

/// Fetch or build a conic (sweep) gradient.
pub fn get_or_build_conic_gradient(
    cx: f32,
    cy: f32,
    start_angle_rad: f32,
    stops: &std::sync::Arc<Vec<GradientStop>>,
    global_alpha: f32,
) -> Option<Shader> {
    let key = GradientKey {
        kind: 2,
        geom_bits: [
            cx.to_bits(),
            cy.to_bits(),
            start_angle_rad.to_bits(),
            0,
            0,
            0,
            0,
        ],
        alpha_bits: global_alpha.to_bits(),
        stops_addr: std::sync::Arc::as_ptr(stops) as usize,
        stops_len: stops.len() as u32,
    };
    lookup_or_build_gradient(key, stops, || {
        let (colors, positions) = build_colors_positions(stops, global_alpha)?;
        let start_deg = start_angle_rad.to_degrees();
        let end_deg = start_deg + 360.0;
        gradient_shader::sweep(
            Point::new(cx, cy),
            gradient_shader::GradientShaderColors::Colors(&colors),
            Some(&positions[..]),
            TileMode::Clamp,
            Some((start_deg, end_deg)),
            None,
            None,
        )
    })
}

#[inline]
fn lookup_or_build_gradient<F: FnOnce() -> Option<Shader>>(
    key: GradientKey,
    stops: &Arc<Vec<GradientStop>>,
    build: F,
) -> Option<Shader> {
    if stops.len() > MAX_GRADIENT_CACHED_STOPS {
        return build();
    }
    GRADIENT_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        // The key includes the Arc allocation address and length.  Because
        // the entry retains the Arc below, that address cannot be recycled
        // while this key is resident; a hit is therefore O(1), with no stop
        // slice comparison.
        if let Some(entry) = cache.get(&key) {
            crate::render_diagnostics::hit_gradient_cache();
            return Some(entry.shader.clone());
        }
        crate::render_diagnostics::miss_gradient_cache();
        let shader = build()?;
        let bytes = gradient_entry_bytes(stops.len());
        if let Some(old) = cache.pop(&key) {
            GRADIENT_CACHE_BYTES_RETAINED.with(|counter| {
                counter.set(
                    counter
                        .get()
                        .saturating_sub(gradient_entry_bytes(old.stops_arc.len())),
                );
            });
        }
        while GRADIENT_CACHE_BYTES_RETAINED.with(|counter| counter.get().saturating_add(bytes))
            > GRADIENT_CACHE_BYTES
        {
            let Some((_old_key, old)) = cache.pop_lru() else {
                break;
            };
            GRADIENT_CACHE_BYTES_RETAINED.with(|counter| {
                counter.set(
                    counter
                        .get()
                        .saturating_sub(gradient_entry_bytes(old.stops_arc.len())),
                );
            });
        }
        if cache.len() >= GRADIENT_CACHE_CAP {
            if let Some((_old_key, old)) = cache.pop_lru() {
                GRADIENT_CACHE_BYTES_RETAINED.with(|counter| {
                    counter.set(
                        counter
                            .get()
                            .saturating_sub(gradient_entry_bytes(old.stops_arc.len())),
                    );
                });
            }
        }
        cache.put(
            key,
            GradientEntry {
                stops_arc: Arc::clone(stops),
                shader: shader.clone(),
            },
        );
        GRADIENT_CACHE_BYTES_RETAINED.with(|counter| {
            counter.set(counter.get().saturating_add(bytes));
        });
        Some(shader)
    })
}

#[inline]
fn build_colors_positions(
    stops: &[GradientStop],
    global_alpha: f32,
) -> Option<(Vec<skia_safe::Color>, Vec<f32>)> {
    if stops.len() < 2 {
        return None;
    }
    // Same modulation the old `stops_to_colors_positions` did: bake
    // globalAlpha into each stop's colour so Skia's gradient
    // samples pick it up automatically.
    let colors = stops
        .iter()
        .map(|s| super::color::to_sk_color4f_modulated(s.color, global_alpha).to_color())
        .collect::<Vec<_>>();
    let positions = stops.iter().map(|s| s.offset.clamp(0.0, 1.0)).collect();
    Some((colors, positions))
}

/// Empty the caches.  Never strictly necessary (entries are bounded)
/// but useful from tests that want deterministic refcount assertions
/// or from a future `freeGpuResources` integration.
pub fn clear_all() {
    SHADOW_CACHE.with(|c| c.borrow_mut().clear());
    DASH_CACHE.with(|c| c.borrow_mut().clear());
    GRADIENT_CACHE.with(|c| c.borrow_mut().clear());
    SHADOW_CACHE_BYTES_RETAINED.with(|counter| counter.set(0));
    DASH_CACHE_BYTES_RETAINED.with(|counter| counter.set(0));
    GRADIENT_CACHE_BYTES_RETAINED.with(|counter| counter.set(0));
}

fn hash_intervals(intervals: &[f32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for v in intervals {
        v.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::protocol::color::Color as ProtocolColor;

    /// **Two distinct shadows must never share a key**, because the shadow cache
    /// is the one cache here with no content re-verification — a shared key
    /// means the second shadow silently gets the first one's filter.
    ///
    /// The sigmas used to be folded into a single `u32` by a "reversible-ish"
    /// mix, which cannot be injective: two `u32`s do not fit in one. Every field
    /// now stands on its own, so this test is checking that the struct still has
    /// a field per parameter rather than checking a hash function's luck.
    ///
    /// Keys rather than filters, deliberately: building a real `ImageFilter`
    /// needs Skia and would test Skia's constructor instead of the key.
    #[test]
    fn every_shadow_parameter_changes_the_cache_key() {
        let base = ShadowKey {
            color: 0xFF00_0000,
            sigma_x_bits: 2.0f32.to_bits(),
            sigma_y_bits: 2.0f32.to_bits(),
            dx_bits: 3.0f32.to_bits(),
            dy_bits: 4.0f32.to_bits(),
        };

        // One field at a time, so a key that dropped any single parameter fails
        // on exactly that line.
        let variants = [
            (
                "color",
                ShadowKey {
                    color: 0xFF00_00FF,
                    ..base
                },
            ),
            (
                "sigma_x",
                ShadowKey {
                    sigma_x_bits: 2.5f32.to_bits(),
                    ..base
                },
            ),
            (
                "sigma_y",
                ShadowKey {
                    sigma_y_bits: 2.5f32.to_bits(),
                    ..base
                },
            ),
            (
                "dx",
                ShadowKey {
                    dx_bits: 3.5f32.to_bits(),
                    ..base
                },
            ),
            (
                "dy",
                ShadowKey {
                    dy_bits: 4.5f32.to_bits(),
                    ..base
                },
            ),
        ];
        for (field, variant) in variants {
            assert_ne!(
                variant, base,
                "changing {field} left the cache key unchanged, so a shadow \
                 differing only in {field} would be served the wrong filter"
            );
        }

        // Anisotropic blur: `sigma_x != sigma_y` must be distinguishable from
        // both isotropic keys. Canvas 2D only ever asks for equal sigmas, but the
        // function takes them separately and the key has to mean what it says.
        let iso_x = ShadowKey {
            sigma_y_bits: 2.0f32.to_bits(),
            ..base
        };
        let aniso = ShadowKey {
            sigma_x_bits: 2.0f32.to_bits(),
            sigma_y_bits: 7.0f32.to_bits(),
            ..base
        };
        let swapped = ShadowKey {
            sigma_x_bits: 7.0f32.to_bits(),
            sigma_y_bits: 2.0f32.to_bits(),
            ..base
        };
        assert_ne!(aniso, iso_x);
        assert_ne!(
            aniso, swapped,
            "the two sigma channels are interchangeable in the key, so a blur \
             wide in x reads as one wide in y"
        );

        // And the key must be a plain tuple of its fields: equal fields, equal
        // key, or the cache would miss on every repeat of the same shadow.
        assert_eq!(base, ShadowKey { ..base });
    }

    /// **The key must be populated from the arguments, not just shaped
    /// correctly.**
    ///
    /// `every_shadow_parameter_changes_the_cache_key` builds `ShadowKey` values
    /// by hand, so it proves the struct discriminates — and stays green if
    /// `get_or_build_drop_shadow` fills a field from the wrong argument, which is
    /// the mistake the old `mix_u32` packing invited. Verified by making
    /// `sigma_y_bits` read `sigma_x`: the hand-built test passed unchanged.
    ///
    /// This one goes through the real function and counts entries, which is the
    /// only handle on cache identity skia-safe offers (an `RCHandle` exposes no
    /// pointer equality). Five distinct shadows must leave five entries; any
    /// parameter the key drops shows up as a smaller count.
    #[test]
    fn distinct_shadows_reach_the_cache_as_distinct_entries() {
        clear_all();
        const COLOR: u32 = 0xFF00_0000;

        // Base, then one argument changed at a time — including `sigma_y` alone,
        // which is the channel the packed key could lose.
        let built = [
            get_or_build_drop_shadow(COLOR, 4.0, 4.0, 2.0, 2.0),
            get_or_build_drop_shadow(COLOR, 5.0, 4.0, 2.0, 2.0), // sigma_x
            get_or_build_drop_shadow(COLOR, 4.0, 5.0, 2.0, 2.0), // sigma_y only
            get_or_build_drop_shadow(COLOR, 4.0, 4.0, 3.0, 2.0), // dx
            get_or_build_drop_shadow(COLOR, 4.0, 4.0, 2.0, 3.0), // dy
        ];
        for (i, filter) in built.iter().enumerate() {
            assert!(filter.is_some(), "shadow {i} failed to build");
        }

        SHADOW_CACHE.with(|c| {
            assert_eq!(
                c.borrow().len(),
                5,
                "five distinct shadows produced fewer entries, so the key drops \
                 one of the five parameters and some shadow is being served \
                 another's filter"
            );
        });

        // Repeating the anisotropic one must hit rather than add a sixth.
        let _again = get_or_build_drop_shadow(COLOR, 4.0, 5.0, 2.0, 2.0);
        SHADOW_CACHE.with(|c| {
            assert_eq!(
                c.borrow().len(),
                5,
                "a repeat of an existing shadow added an entry, so the key is \
                 not stable across identical calls"
            );
        });
    }

    #[test]
    fn shadow_cache_hits_on_identical_params() {
        clear_all();
        let color = 0xFF00_0000u32;
        let a = get_or_build_drop_shadow(color, 4.0, 4.0, 2.0, 2.0).expect("build");
        let b = get_or_build_drop_shadow(color, 4.0, 4.0, 2.0, 2.0).expect("build");
        // RC handles should point at the same native pointer when
        // served from cache.  skia-safe doesn't expose raw ptr
        // equality, but `Clone` of an RCHandle bumps the refcount,
        // so we can at least verify the cache doesn't grow past 1
        // entry after two identical requests.
        SHADOW_CACHE.with(|c| {
            assert_eq!(
                c.borrow().len(),
                1,
                "two identical requests produced two entries"
            );
        });
        let _ = (a, b);
    }

    #[test]
    fn shadow_cache_distinguishes_colour_and_sigma() {
        clear_all();
        let _ = get_or_build_drop_shadow(0xFF00_0000, 4.0, 4.0, 2.0, 2.0).unwrap();
        let _ = get_or_build_drop_shadow(0xFFFF_0000, 4.0, 4.0, 2.0, 2.0).unwrap();
        let _ = get_or_build_drop_shadow(0xFF00_0000, 8.0, 8.0, 2.0, 2.0).unwrap();
        let _ = get_or_build_drop_shadow(0xFF00_0000, 4.0, 4.0, 5.0, 2.0).unwrap();
        SHADOW_CACHE.with(|c| {
            assert_eq!(
                c.borrow().len(),
                4,
                "each distinct param tuple must occupy its own slot"
            );
        });
    }

    #[test]
    fn dash_cache_hits_on_identical_intervals_and_phase() {
        clear_all();
        let _ = get_or_build_dash(&[4.0, 2.0], 0.0).unwrap();
        let _ = get_or_build_dash(&[4.0, 2.0], 0.0).unwrap();
        DASH_CACHE.with(|c| {
            assert_eq!(c.borrow().len(), 1);
        });
    }

    #[test]
    fn dash_cache_preserves_interval_order() {
        clear_all();
        // [4, 2] and [2, 4] render differently; the cache MUST
        // NOT alias them even if a naive multiset hash would.
        let _ = get_or_build_dash(&[4.0, 2.0], 0.0).unwrap();
        let _ = get_or_build_dash(&[2.0, 4.0], 0.0).unwrap();
        DASH_CACHE.with(|c| {
            assert_eq!(c.borrow().len(), 2);
        });
    }

    #[test]
    fn dash_rejects_empty_intervals() {
        clear_all();
        assert!(get_or_build_dash(&[], 0.0).is_none());
    }

    #[test]
    fn dash_cache_distinguishes_phase() {
        clear_all();
        let _ = get_or_build_dash(&[4.0, 2.0], 0.0).unwrap();
        let _ = get_or_build_dash(&[4.0, 2.0], 1.0).unwrap();
        DASH_CACHE.with(|c| {
            assert_eq!(c.borrow().len(), 2);
        });
    }

    // ---- metrics wiring (P1-3a/b + P1-5) --------------------------

    /// Run `body` with a fresh current-thread `DebugStats` sink installed and
    /// clean it up afterwards. TLS keeps parallel tests isolated.
    fn with_sink<F: FnOnce(&std::sync::Arc<shared::stats::DebugStats>)>(f: F) {
        use crate::render_diagnostics;
        render_diagnostics::uninstall_for_tests();
        let stats = std::sync::Arc::new(shared::stats::DebugStats::default());
        render_diagnostics::install(stats.clone());
        f(&stats);
        render_diagnostics::uninstall_for_tests();
    }

    #[test]
    fn shadow_cache_miss_and_hit_each_bump_respective_metric() {
        with_sink(|stats| {
            use std::sync::atomic::Ordering;
            clear_all();

            // First call: miss.
            let _ = get_or_build_drop_shadow(0xFF00_0000, 4.0, 4.0, 2.0, 2.0).unwrap();
            crate::render_diagnostics::flush_frame();
            assert_eq!(stats.shadow_filter_misses.load(Ordering::Relaxed), 1);
            assert_eq!(stats.shadow_filter_hits.load(Ordering::Relaxed), 0);

            // Second call with same params: hit.
            let _ = get_or_build_drop_shadow(0xFF00_0000, 4.0, 4.0, 2.0, 2.0).unwrap();
            crate::render_diagnostics::flush_frame();
            assert_eq!(stats.shadow_filter_misses.load(Ordering::Relaxed), 1);
            assert_eq!(stats.shadow_filter_hits.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn dash_cache_miss_and_hit_each_bump_respective_metric() {
        with_sink(|stats| {
            use std::sync::atomic::Ordering;
            clear_all();

            let _ = get_or_build_dash(&[4.0, 2.0], 0.0).unwrap();
            crate::render_diagnostics::flush_frame();
            assert_eq!(stats.dash_effect_misses.load(Ordering::Relaxed), 1);
            assert_eq!(stats.dash_effect_hits.load(Ordering::Relaxed), 0);

            let _ = get_or_build_dash(&[4.0, 2.0], 0.0).unwrap();
            crate::render_diagnostics::flush_frame();
            assert_eq!(stats.dash_effect_misses.load(Ordering::Relaxed), 1);
            assert_eq!(stats.dash_effect_hits.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn distinct_params_all_register_as_misses() {
        with_sink(|stats| {
            use std::sync::atomic::Ordering;
            clear_all();

            // Three distinct shadow tuples ⇒ 3 misses, 0 hits.
            let _ = get_or_build_drop_shadow(0xFF00_0000, 4.0, 4.0, 2.0, 2.0).unwrap();
            let _ = get_or_build_drop_shadow(0xFFFF_0000, 4.0, 4.0, 2.0, 2.0).unwrap();
            let _ = get_or_build_drop_shadow(0xFF00_0000, 8.0, 8.0, 2.0, 2.0).unwrap();
            crate::render_diagnostics::flush_frame();
            assert_eq!(stats.shadow_filter_misses.load(Ordering::Relaxed), 3);
            assert_eq!(stats.shadow_filter_hits.load(Ordering::Relaxed), 0);
        });
    }

    // ---- Gradient cache -------------------------------------------

    fn simple_stops() -> std::sync::Arc<Vec<GradientStop>> {
        use shared::protocol::color::Color as ProtocolColor;
        std::sync::Arc::new(vec![
            GradientStop {
                offset: 0.0,
                color: ProtocolColor::rgb(255, 0, 0),
            },
            GradientStop {
                offset: 1.0,
                color: ProtocolColor::rgb(0, 0, 255),
            },
        ])
    }

    #[test]
    fn linear_gradient_hits_on_identical_params_and_same_arc() {
        clear_all();
        let stops = simple_stops();
        let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 1.0).expect("build");
        // Same Arc + same geometry must hit.
        let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 1.0).expect("build");
        GRADIENT_CACHE.with(|c| {
            assert_eq!(c.borrow().len(), 1, "identical request must not grow cache");
        });
    }

    #[test]
    fn linear_gradient_distinguishes_geometry() {
        clear_all();
        let stops = simple_stops();
        let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 1.0).unwrap();
        let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 50.0, &stops, 1.0).unwrap();
        let _ = get_or_build_linear_gradient(10.0, 0.0, 100.0, 0.0, &stops, 1.0).unwrap();
        GRADIENT_CACHE.with(|c| {
            assert_eq!(c.borrow().len(), 3);
        });
    }

    #[test]
    fn gradient_distinguishes_global_alpha() {
        clear_all();
        let stops = simple_stops();
        let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 1.0).unwrap();
        let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 0.5).unwrap();
        GRADIENT_CACHE.with(|c| {
            assert_eq!(c.borrow().len(), 2);
        });
    }

    #[test]
    fn radial_and_conic_gradient_build() {
        clear_all();
        let stops = simple_stops();
        assert!(
            get_or_build_radial_gradient(0.0, 0.0, 10.0, 50.0, 50.0, 100.0, &stops, 1.0).is_some()
        );
        assert!(get_or_build_conic_gradient(50.0, 50.0, 0.0, &stops, 1.0).is_some());
    }

    #[test]
    fn gradient_cache_miss_and_hit_each_bump_metric() {
        with_sink(|stats| {
            use std::sync::atomic::Ordering;
            clear_all();
            let stops = simple_stops();

            // First call: miss.
            let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 1.0).expect("build");
            crate::render_diagnostics::flush_frame();
            assert_eq!(stats.gradient_misses.load(Ordering::Relaxed), 1);
            assert_eq!(stats.gradient_hits.load(Ordering::Relaxed), 0);

            // Second call with same Arc + same geometry: hit.
            let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 1.0).expect("build");
            crate::render_diagnostics::flush_frame();
            assert_eq!(stats.gradient_misses.load(Ordering::Relaxed), 1);
            assert_eq!(stats.gradient_hits.load(Ordering::Relaxed), 1);
        });
    }

    // ── T-M1 / E-P2 byte-ceiling and revision-identity tests ───────
    // These are intentionally written before the byte-bounded cache
    // implementation.  They document the RED contract for oversized
    // values, retained-byte accounting, and immutable gradient revisions.

    #[test]
    fn shadow_cache_byte_total_stays_under_budget() {
        clear_all();
        for i in 0..SHADOW_CACHE_CAP + 8 {
            let _ = get_or_build_drop_shadow(0xFF00_0000, 1.0, 1.0, i as f32, 0.0);
        }
        assert!(
            SHADOW_CACHE_BYTES_RETAINED.with(|c| c.get()) <= SHADOW_CACHE_BYTES,
            "retained shadow-cache bytes exceeded the byte ceiling"
        );
        SHADOW_CACHE.with(|c| assert_eq!(c.borrow().len(), SHADOW_CACHE_CAP));
    }

    /// A dash payload above the single-item limit is still built, but is not
    /// retained by the LRU.  The render result must not depend on caching.
    #[test]
    fn dash_cache_oversized_intervals_not_retained() {
        clear_all();
        // Use an even count so Skia accepts the dash; the cache gate is the
        // behaviour under test, not interval-shape validation.
        let intervals = vec![2.0_f32; MAX_DASH_CACHED_INTERVALS + 2];
        assert!(get_or_build_dash(&intervals, 0.0).is_some());
        DASH_CACHE.with(|c| assert_eq!(c.borrow().len(), 0));
    }

    /// A gradient with too many stops is rendered normally but not retained.
    #[test]
    fn gradient_cache_oversized_stops_not_retained() {
        clear_all();
        let stops = std::sync::Arc::new(
            (0..=MAX_GRADIENT_CACHED_STOPS)
                .map(|i| GradientStop {
                    offset: i as f32 / MAX_GRADIENT_CACHED_STOPS as f32,
                    color: ProtocolColor::rgb(255, 0, 0),
                })
                .collect::<Vec<_>>(),
        );
        assert!(get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 1.0).is_some());
        GRADIENT_CACHE.with(|c| assert_eq!(c.borrow().len(), 0));
    }

    /// Many ordinary dash entries must stay under the retained-byte ceiling,
    /// even though the entry count remains below the ordinary LRU capacity.
    #[test]
    fn dash_cache_byte_total_stays_under_budget() {
        clear_all();
        for phase in 0..50 {
            let intervals = [2.0_f32; 32];
            let _ = get_or_build_dash(&intervals, phase as f32);
        }
        assert!(
            DASH_CACHE_BYTES_RETAINED.with(|c| c.get()) <= DASH_CACHE_BYTES,
            "retained dash-cache bytes exceeded the byte ceiling"
        );
        DASH_CACHE.with(|c| {
            assert_eq!(
                c.borrow().len(),
                DASH_CACHE_CAP,
                "ordinary dash entries must retain the existing entry-count cap"
            );
        });
    }

    /// The cache entry retains the immutable Arc revision itself.  This is
    /// what makes the address in `GradientKey` collision-safe and lets a hit
    /// avoid re-comparing every stop.
    #[test]
    fn gradient_hit_retains_arc_revision_identity() {
        clear_all();
        let stops = simple_stops();
        let _ = get_or_build_linear_gradient(0.0, 0.0, 100.0, 0.0, &stops, 1.0);
        let key = GradientKey {
            kind: 0,
            geom_bits: [
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                100.0_f32.to_bits(),
                0,
                0,
                0,
                0,
            ],
            alpha_bits: 1.0_f32.to_bits(),
            stops_addr: std::sync::Arc::as_ptr(&stops) as usize,
            stops_len: stops.len() as u32,
        };
        GRADIENT_CACHE.with(|c| {
            let cache = c.borrow();
            let entry = cache.peek(&key).expect("gradient must be cached");
            assert_eq!(
                std::sync::Arc::as_ptr(&entry.stops_arc),
                std::sync::Arc::as_ptr(&stops),
                "cache must retain the Arc revision, not a copied stop Vec"
            );
        });
    }
}
