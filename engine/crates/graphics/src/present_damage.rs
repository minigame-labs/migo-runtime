// present_damage.rs — pure, dependency-free damage/blit plan model.
// No `use crate::…`, no Android/EGL/GL/Skia, no external crates.
// Can be tested standalone: rustc --edition 2024 --test present_damage.rs

// ── Public types ──────────────────────────────────────────────────────────────

/// A non-empty, clipped, lower-left pixel rectangle.
///
/// All constructor math is checked or widened to i64 so no integer overflow is
/// possible.  An invalid/empty geometry returns `None`; the caller must fall
/// back to `DamageRegion::FullSurface`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    // Private, so [`DamageRect::new`] is the only way in and "a `DamageRect`
    // exists" means "its geometry is valid".
    //
    // They were `pub`, and the gap that opened is worth recording: `from_rect`
    // documented itself as returning `FullSurface` on invalid geometry and never
    // checked, because with public fields anyone could build a zero- or
    // negative-sized rect without passing through the constructor. Such a rect
    // reaches `glBlitFramebuffer` as an empty box, which repairs nothing — so
    // the region it claimed to cover keeps the previous frame's pixels, with no
    // GL error and nothing in a log.
    //
    // Production never did build one (the one caller guards on
    // `DamageRect::new`), so this closes a latent hole rather than a live bug.
    // The point is that the invariant is now the type's rather than the caller's:
    // there is no longer a `from_rect` contract to get wrong.
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl DamageRect {
    /// Returns `None` if `width` or `height` <= 0, or if the right/top edge
    /// overflows `i32`.
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        // Check x + width and y + height do not overflow i32.
        let right = (x as i64).checked_add(width as i64)?;
        let top = (y as i64).checked_add(height as i64)?;
        if right > i32::MAX as i64 || top > i32::MAX as i64 {
            return None;
        }
        Some(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// Lower-left origin and extent, in surface pixels.
    ///
    /// One accessor rather than four, because every consumer wants all of them
    /// — `drawing_buffer` turns them straight into blit corners — and four calls
    /// invite reading three and computing the fourth wrong.
    #[inline]
    pub fn xywh(self) -> (i32, i32, i32, i32) {
        (self.x, self.y, self.width, self.height)
    }

    /// Right edge (exclusive), always valid after construction.
    #[inline]
    fn right(self) -> i32 {
        self.x + self.width
    }

    /// Top edge (exclusive, lower-left coords), always valid after
    /// construction.
    #[inline]
    fn top(self) -> i32 {
        self.y + self.height
    }

    /// Returns true if `other` is fully contained within `self`.
    fn contains(self, other: Self) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.top() >= other.top()
    }

    /// Smallest AABB that covers both rectangles.
    fn union_aabb(self, other: Self) -> Option<Self> {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let top = self.top().max(other.top());
        let width = i32::try_from((right as i64) - (x as i64)).ok()?;
        let height = i32::try_from((top as i64) - (y as i64)).ok()?;
        Self::new(x, y, width, height)
    }
}

// ── DamageRegion ─────────────────────────────────────────────────────────────

/// A damage region: either the full surface, or 1-4 discrete lower-left rects.
///
/// The `Partial` variant is always non-empty (`len >= 1`) and never exceeds
/// capacity 4.  Once the union causes effective rects to exceed 4, the region
/// collapses to a single AABB and stays collapsed (sticky) for all future
/// unions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DamageRegion {
    /// Entire surface is damaged (or eligibility is unknown/failed).
    FullSurface,
    /// 1-4 discrete rectangles. No heap allocation.
    Partial(PartialRegion),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialRegion {
    rects: [DamageRect; 4],
    len: u8,
    /// True once a union forced a 5th rect and we collapsed to one AABB.
    collapsed: bool,
}

// Safe placeholder rect that is never exposed when `len < index`.
const PLACEHOLDER: DamageRect = DamageRect {
    x: 0,
    y: 0,
    width: 1,
    height: 1,
};

impl PartialRegion {
    /// Construct a region with exactly one rect.
    fn single(r: DamageRect) -> Self {
        Self {
            rects: [r, PLACEHOLDER, PLACEHOLDER, PLACEHOLDER],
            len: 1,
            collapsed: false,
        }
    }

    /// Slice of the live rectangles.
    pub fn rects(&self) -> &[DamageRect] {
        &self.rects[..self.len as usize]
    }

    /// Whether this region was collapsed to one AABB due to exceeding 4 rects.
    #[allow(dead_code)]
    pub fn is_collapsed(&self) -> bool {
        self.collapsed
    }
}

impl DamageRegion {
    /// Single-rect partial region.
    ///
    /// No geometry check, and none needed: [`DamageRect`]'s fields are private,
    /// so the only way to hold one is through `DamageRect::new`, which rejects
    /// non-positive extents and edges that overflow `i32`. This used to promise
    /// "returns `FullSurface` on invalid geometry" and never checked — a promise
    /// that mattered because the fields were public. Making the type carry the
    /// invariant removed the need for the promise rather than the promise's
    /// falsehood.
    pub fn from_rect(r: DamageRect) -> Self {
        DamageRegion::Partial(PartialRegion::single(r))
    }

    /// Union `self` with another region according to the sticky bounded rules.
    ///
    /// - Exact duplicates and fully-contained rects are dropped.
    /// - Partial overlaps are kept as discrete rects.
    /// - On overflow (would need >4 rects), collapse to one AABB (sticky).
    /// - `FullSurface` union with anything => `FullSurface`.
    pub fn union(self, other: DamageRegion) -> DamageRegion {
        match (self, other) {
            (DamageRegion::FullSurface, _) | (_, DamageRegion::FullSurface) => {
                DamageRegion::FullSurface
            }
            (DamageRegion::Partial(mut a), DamageRegion::Partial(b)) => {
                for &r in b.rects() {
                    let Some(next) = union_rect_into(a, r) else {
                        return DamageRegion::FullSurface;
                    };
                    a = next;
                }
                DamageRegion::Partial(a)
            }
        }
    }

    /// Returns the rects slice if this is a `Partial` region.
    pub fn rects(&self) -> Option<&[DamageRect]> {
        match self {
            DamageRegion::FullSurface => None,
            DamageRegion::Partial(p) => Some(p.rects()),
        }
    }

    /// Return one AABB for stats/legacy consumers. `None` means the region is
    /// full-surface or its discrete span cannot be represented by `DamageRect`.
    pub fn bounding_rect(&self) -> Option<DamageRect> {
        let mut rects = self.rects()?.iter().copied();
        let mut aabb = rects.next()?;
        for rect in rects {
            aabb = aabb.union_aabb(rect)?;
        }
        Some(aabb)
    }

    pub fn is_full_surface(&self) -> bool {
        matches!(self, DamageRegion::FullSurface)
    }
}

/// Insert one rect into a `PartialRegion`, applying deduplication and sticky
/// AABB collapse when needed. Returns `None` when the combined AABB cannot be
/// represented by `DamageRect`; callers must fail closed to full-surface damage.
fn union_rect_into(mut region: PartialRegion, new_rect: DamageRect) -> Option<PartialRegion> {
    // If already collapsed to one AABB, expand it.
    if region.collapsed {
        let aabb = region.rects[0].union_aabb(new_rect)?;
        region.rects[0] = aabb;
        return Some(region);
    }

    let live = region.rects();

    // Drop if it is fully contained by any existing rect.
    for &existing in live {
        if existing.contains(new_rect) {
            return Some(region);
        }
    }

    // Remove existing rects that are fully contained by new_rect.
    let mut buf: [DamageRect; 4] = [PLACEHOLDER; 4];
    let mut buf_len: u8 = 0;
    for &existing in live {
        if !new_rect.contains(existing) {
            buf[buf_len as usize] = existing;
            buf_len += 1;
        }
    }

    // Check for exact duplicate.
    let already_present = buf[..buf_len as usize].iter().any(|&r| r == new_rect);
    if already_present {
        return Some(PartialRegion {
            rects: buf,
            len: buf_len,
            collapsed: false,
        });
    }

    if buf_len < 4 {
        buf[buf_len as usize] = new_rect;
        buf_len += 1;
        Some(PartialRegion {
            rects: buf,
            len: buf_len,
            collapsed: false,
        })
    } else {
        // Would need a fifth slot → collapse to AABB.
        let mut aabb = buf[0];
        for i in 1..buf_len as usize {
            aabb = aabb.union_aabb(buf[i])?;
        }
        aabb = aabb.union_aabb(new_rect)?;
        Some(PartialRegion {
            rects: [aabb, PLACEHOLDER, PLACEHOLDER, PLACEHOLDER],
            len: 1,
            collapsed: true,
        })
    }
}

// ── PresentDamageHistory ─────────────────────────────────────────────────────

/// Ring-buffer of up to 4 successfully-presented current-frame regions.
///
/// Entry 0 is the most-recently pushed.  Capacity matches the maximum
/// buffer age a driver can return (4 is generous; EGL implementations
/// typically stay at 2-3 but the spec has no upper bound).
pub struct PresentDamageHistory {
    // Index 0 = newest; up to 4 entries.
    entries: [Option<DamageRegion>; 4],
    len: usize,
}

impl PresentDamageHistory {
    pub const fn new() -> Self {
        Self {
            entries: [None, None, None, None],
            len: 0,
        }
    }

    /// Record the current-frame region after a successful swap.
    /// Oldest entry is dropped when capacity is exceeded.
    pub fn push(&mut self, region: DamageRegion) {
        // Shift older entries towards the end to make room at index 0 for newest.
        if self.len < 4 {
            self.len += 1;
        }
        for i in (1..self.len).rev() {
            self.entries[i] = self.entries[i - 1].take();
        }
        self.entries[0] = Some(region);
    }

    /// Number of stored entries.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Clear all history (surface resize, context loss, etc.).
    pub fn clear(&mut self) {
        self.entries = [None, None, None, None];
        self.len = 0;
    }

    /// Compute the repair region for the given buffer age.
    ///
    /// - age <= 0 → `FullSurface`
    /// - age 1 → return `current` directly (no history needed)
    /// - age N → union `current` with the newest `N-1` history entries
    /// - fewer than `N-1` history entries → `FullSurface`
    /// - `current` or any consumed entry is `FullSurface` → `FullSurface`
    pub fn resolve_with_age(&self, current: &DamageRegion, buffer_age: i32) -> DamageRegion {
        if buffer_age <= 0 {
            return DamageRegion::FullSurface;
        }
        if buffer_age == 1 {
            return current.clone();
        }
        // Need age-1 history entries.
        let needed = (buffer_age - 1) as usize;
        if self.len < needed {
            return DamageRegion::FullSurface;
        }
        // Start with current and union each required history entry.
        let mut repair = current.clone();
        for i in 0..needed {
            let entry = self.entries[i]
                .as_ref()
                .expect("len guarantees entry present");
            repair = repair.union(entry.clone());
            if repair.is_full_surface() {
                return DamageRegion::FullSurface;
            }
        }
        repair
    }
}

// ── PresentDamagePlan ─────────────────────────────────────────────────────────

/// Output of the eligibility gate.
///
/// `current` is always the real current-frame damage (for history recording).
/// `repair` is the age-expanded buffer damage; `FullSurface` when any
/// eligibility check fails.
#[derive(Debug, Clone)]
pub struct PresentDamagePlan {
    pub current: DamageRegion,
    pub repair: DamageRegion,
}

/// Build a `PresentDamagePlan` from current damage + eligibility parameters.
///
/// Eligibility fails (repair=FullSurface) when:
/// - `age_supported` is false
/// - `buffer_age <= 0`
/// - `!db_matches_surface` (dimension mismatch)
/// - `!dest_single_sample` (multisampled window surface)
/// - history shortage for the requested age
/// - `current` is `FullSurface`
/// - any history entry consumed is `FullSurface`
pub fn build_present_plan(
    current: DamageRegion,
    history: &PresentDamageHistory,
    age_supported: bool,
    buffer_age: i32,
    db_matches_surface: bool,
    dest_single_sample: bool,
) -> PresentDamagePlan {
    let repair = if !age_supported || buffer_age <= 0 || !db_matches_surface || !dest_single_sample
    {
        DamageRegion::FullSurface
    } else if current.is_full_surface() {
        DamageRegion::FullSurface
    } else {
        history.resolve_with_age(&current, buffer_age)
    };
    PresentDamagePlan { current, repair }
}

// ── EXT-vs-KHR demotion helper ────────────────────────────────────────────────

/// The region to hand `eglSwapBuffersWithDamage`, which is **not** the region
/// handed to `eglSetDamageRegionKHR`.
///
/// The two extensions answer different questions and this plan carries both
/// answers, one field apart:
///
/// * `repair` (to `eglSetDamageRegionKHR`) is *buffer* damage -- "the region I
///   am about to rewrite in this back buffer", widened by buffer age so the
///   aged contents get repaired.
/// * `current` (to `eglSwapBuffersWithDamage`) is *surface* damage -- "the
///   region that differs from the frame you last showed", which is what lets
///   the compositor skip recompositing the rest.
///
/// Submitting `repair` to swap is not a visual bug: it is a superset, so the
/// frame still looks right, and every pixel test passes. It just quietly gives
/// back most of the compositor saving the call was made for -- which is why the
/// choice is a named function with a test rather than a field access at the
/// call site.
pub fn swap_damage_region(plan: &PresentDamagePlan) -> &DamageRegion {
    &plan.current
}

/// Whether this frame should be presented with surface damage, and which rects
/// to submit if so.
///
/// Split out from the EGL call because that call needs a live display and this
/// decision does not: every way of getting it wrong -- submitting after a
/// driver rejection, submitting a whole-surface region that says nothing,
/// submitting an empty one -- is decided here, where a host test can see it.
///
/// A full-surface frame deliberately returns `None`. Passing the whole surface
/// as damage is legal but tells the compositor only what it already assumed,
/// at the cost of building and passing a rect array to say it.
pub fn surface_damage_to_submit<'a>(
    plan: &'a PresentDamagePlan,
    extension_available: bool,
    previously_rejected: bool,
) -> Option<&'a [DamageRect]> {
    if !extension_available || previously_rejected {
        return None;
    }
    match swap_damage_region(plan).rects() {
        Some(rects) if !rects.is_empty() => Some(rects),
        _ => None,
    }
}

/// Adjust repair after `eglSetDamageRegionKHR` declaration failure.
///
/// If `EGL_EXT_buffer_age` is independently advertised, the aged buffer
/// contents are available regardless of the KHR declaration, so the partial
/// repair can be kept.
///
/// If only `EGL_KHR_partial_update` provides age semantics and its declaration
/// failed, the spec says contents outside the declared region are undefined;
/// we must fall back to `FullSurface`.
pub fn repair_after_declaration_failure(
    plan: PresentDamagePlan,
    ext_buffer_age_independently_advertised: bool,
) -> PresentDamagePlan {
    if ext_buffer_age_independently_advertised {
        plan
    } else {
        PresentDamagePlan {
            current: plan.current,
            repair: DamageRegion::FullSurface,
        }
    }
}

// ── BlitPlan ──────────────────────────────────────────────────────────────────

/// How the DrawingBuffer should be blitted to the window surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlitPlan {
    /// Full-surface blit using GL_LINEAR (legacy / scaled path).
    Full { linear: bool },
    /// 1-4 identity-coordinate rects using GL_NEAREST.
    Rects(BlitRects),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlitRects {
    rects: [DamageRect; 4],
    len: u8,
}

impl BlitRects {
    /// Slice of the rects to blit.
    pub fn rects(&self) -> &[DamageRect] {
        &self.rects[..self.len as usize]
    }
}

/// Derive a `BlitPlan` from the repair region and surface/sample properties.
///
/// Partial (NEAREST, identity coords) requires:
/// - `repair` is `Partial`
/// - `db_matches_surface` (same-size: source == destination, no Y flip)
/// - `dest_single_sample` (no MSAA on the window surface)
///
/// Otherwise: `Full { linear: true }`.
pub fn blit_plan(
    repair: &DamageRegion,
    db_matches_surface: bool,
    dest_single_sample: bool,
) -> BlitPlan {
    match repair {
        DamageRegion::Partial(p) if db_matches_surface && dest_single_sample => {
            let mut rects = [PLACEHOLDER; 4];
            let len = p.len;
            rects[..len as usize].copy_from_slice(p.rects());
            BlitPlan::Rects(BlitRects { rects, len })
        }
        // **NEAREST when the copy is 1:1, and that is the common case.**
        //
        // This arm used to hardcode `linear: true`, so `linear: false` was never
        // produced and a full-surface blit always asked for bilinear filtering —
        // including when the DrawingBuffer and the window surface are the same
        // size, which is a straight copy. A bilinear full-screen blit makes the
        // driver read four texels per output pixel where one would do, and on
        // tiled mobile GPUs it can leave the fast blit path altogether.
        //
        // This is not a rare arm. It is taken every frame on a device without
        // `EGL_KHR_partial_update` / `EGL_EXT_buffer_age`, and every frame for
        // any content whose damage is full-surface — which is most 3D games,
        // since they clear the whole screen. At 1080x2400 that is the single
        // largest per-frame bandwidth item in the engine.
        //
        // `blit_from_surface` in `drawing_buffer` has always picked the filter
        // this way for the reverse copy. The rare path had it right and the
        // per-frame path did not.
        //
        // LINEAR is still required when the sizes differ: `glBlitFramebuffer`
        // rejects a scaling blit asking for NEAREST on some drivers, which is
        // the reason `blit_from_surface` records for the same choice.
        _ => BlitPlan::Full {
            linear: !db_matches_surface,
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> DamageRect {
        DamageRect::new(x, y, w, h).expect("valid rect")
    }

    #[test]
    fn surface_damage_is_submitted_only_when_it_says_something_new() {
        let history = PresentDamageHistory::new();
        let partial = build_present_plan(
            DamageRegion::from_rect(r(40, 40, 10, 10)),
            &history,
            true,
            1,
            true,
            true,
        );
        let full = build_present_plan(DamageRegion::FullSurface, &history, true, 1, true, true);

        assert_eq!(
            surface_damage_to_submit(&partial, true, false),
            Some(&[r(40, 40, 10, 10)][..]),
            "a partial frame is exactly the case the extension exists for"
        );
        assert_eq!(
            surface_damage_to_submit(&full, true, false),
            None,
            "a whole-surface region tells the compositor what it already assumed"
        );
        assert_eq!(
            surface_damage_to_submit(&partial, false, false),
            None,
            "no extension, no submission"
        );
        assert_eq!(
            surface_damage_to_submit(&partial, true, true),
            None,
            "a driver that rejected the call once is not asked again"
        );
    }

    /// Swap gets `current`, the damage declaration gets `repair`. They are two
    /// different regions and the plan holds both, so the only thing standing
    /// between them is which field the call site reads -- and reading the wrong
    /// one produces a correct-looking frame.
    #[test]
    fn swap_receives_surface_damage_not_the_buffer_repair_region() {
        // A history with an older frame makes `repair` strictly wider than
        // `current`: buffer age 2 means the back buffer also needs the region
        // damaged two frames ago repaired.
        let mut history = PresentDamageHistory::new();
        history.push(DamageRegion::from_rect(r(0, 0, 10, 10)));
        history.push(DamageRegion::from_rect(r(90, 90, 10, 10)));

        let current = DamageRegion::from_rect(r(40, 40, 10, 10));
        let plan = build_present_plan(current, &history, true, 2, true, true);

        let submitted = swap_damage_region(&plan);
        assert_eq!(
            submitted.rects(),
            Some(&[r(40, 40, 10, 10)][..]),
            "swap must be told only what changed since the last presented frame"
        );
        assert_ne!(
            submitted.bounding_rect(),
            plan.repair.bounding_rect(),
            "this test is worthless unless repair and current actually differ here"
        );
    }

    /// **`DamageRect::new` is the only way to hold one, so its rejections are
    /// the whole geometry invariant.**
    ///
    /// Every rect that reaches `glBlitFramebuffer` as a repair box comes through
    /// here. A non-positive extent blits nothing, which leaves the region it
    /// claimed to cover holding the previous frame's pixels — no GL error,
    /// nothing in a log, just a stale patch. An overflowing edge is worse: the
    /// arithmetic that computes blit corners wraps.
    ///
    /// This was previously only half the story, because the fields were public
    /// and a caller could skip the constructor. They are private now, which is
    /// what makes this test cover the type rather than one path into it.
    #[test]
    fn a_rect_can_only_exist_if_its_geometry_is_usable() {
        // Degenerate extents: a blit of these copies nothing.
        assert!(DamageRect::new(0, 0, 0, 10).is_none(), "zero width");
        assert!(DamageRect::new(0, 0, 10, 0).is_none(), "zero height");
        assert!(DamageRect::new(0, 0, -1, 10).is_none(), "negative width");
        assert!(DamageRect::new(0, 0, 10, -1).is_none(), "negative height");

        // Edges that overflow `i32`: `right()`/`top()` are plain adds after
        // construction, so a rect that got in here would wrap them.
        assert!(
            DamageRect::new(i32::MAX, 0, 1, 1).is_none(),
            "x + width overflows"
        );
        assert!(
            DamageRect::new(0, i32::MAX, 1, 1).is_none(),
            "y + height overflows"
        );
        assert!(
            DamageRect::new(1, 1, i32::MAX, i32::MAX).is_none(),
            "both edges overflow"
        );

        // The largest rect that still fits is accepted — the check must bound
        // the edge, not the extent.
        let biggest = DamageRect::new(0, 0, i32::MAX, i32::MAX).expect("edges land exactly on MAX");
        assert_eq!(biggest.xywh(), (0, 0, i32::MAX, i32::MAX));

        // And a negative origin is fine: surfaces use lower-left coordinates and
        // a damage rect may legitimately start left of or below the origin after
        // a transform, so long as its edges are representable.
        let offscreen = DamageRect::new(-50, -20, 100, 40).expect("negative origin is valid");
        assert_eq!(offscreen.xywh(), (-50, -20, 100, 40));
    }

    fn partial(rects: &[DamageRect]) -> DamageRegion {
        assert!(!rects.is_empty() && rects.len() <= 4);
        let mut region = DamageRegion::from_rect(rects[0]);
        for &r in &rects[1..] {
            region = region.union(DamageRegion::from_rect(r));
        }
        region
    }

    // ── DamageRect validation ────────────────────────────────────────────────

    #[test]
    fn empty_width_fails() {
        assert!(DamageRect::new(0, 0, 0, 10).is_none());
    }

    #[test]
    fn empty_height_fails() {
        assert!(DamageRect::new(0, 0, 10, 0).is_none());
    }

    #[test]
    fn negative_width_fails() {
        assert!(DamageRect::new(0, 0, -1, 10).is_none());
    }

    #[test]
    fn negative_height_fails() {
        assert!(DamageRect::new(0, 0, 10, -1).is_none());
    }

    #[test]
    fn overflow_right_edge_fails() {
        assert!(DamageRect::new(i32::MAX, 0, 1, 1).is_none());
    }

    #[test]
    fn overflow_top_edge_fails() {
        assert!(DamageRect::new(0, i32::MAX, 1, 1).is_none());
    }

    #[test]
    fn valid_rect_succeeds() {
        let rect = DamageRect::new(10, 20, 100, 200).unwrap();
        assert_eq!(rect.x, 10);
        assert_eq!(rect.y, 20);
        assert_eq!(rect.width, 100);
        assert_eq!(rect.height, 200);
    }

    // ── age 0 / query failure / unsupported → FullSurface ────────────────────

    #[test]
    fn age_zero_gives_full_repair() {
        let h = PresentDamageHistory::new();
        let current = partial(&[r(0, 0, 100, 100)]);
        let plan = build_present_plan(current, &h, true, 0, true, true);
        assert!(plan.repair.is_full_surface());
    }

    #[test]
    fn negative_age_gives_full_repair() {
        let h = PresentDamageHistory::new();
        let current = partial(&[r(0, 0, 100, 100)]);
        let plan = build_present_plan(current, &h, true, -1, true, true);
        assert!(plan.repair.is_full_surface());
    }

    #[test]
    fn unsupported_age_gives_full_repair() {
        let h = PresentDamageHistory::new();
        let current = partial(&[r(0, 0, 100, 100)]);
        let plan = build_present_plan(current, &h, false, 1, true, true);
        assert!(plan.repair.is_full_surface());
    }

    #[test]
    fn history_shortage_gives_full_repair() {
        let mut h = PresentDamageHistory::new();
        h.push(partial(&[r(0, 0, 50, 50)]));
        let current = partial(&[r(10, 10, 20, 20)]);
        let plan = build_present_plan(current, &h, true, 3, true, true);
        assert!(plan.repair.is_full_surface());
    }

    #[test]
    fn full_current_gives_full_repair() {
        let mut h = PresentDamageHistory::new();
        h.push(partial(&[r(0, 0, 50, 50)]));
        let plan = build_present_plan(DamageRegion::FullSurface, &h, true, 1, true, true);
        assert!(plan.repair.is_full_surface());
    }

    #[test]
    fn full_history_entry_poisons_repair() {
        let mut h = PresentDamageHistory::new();
        h.push(DamageRegion::FullSurface);
        let current = partial(&[r(0, 0, 10, 10)]);
        let plan = build_present_plan(current, &h, true, 2, true, true);
        assert!(plan.repair.is_full_surface());
    }

    // ── current is always preserved ──────────────────────────────────────────

    #[test]
    fn current_preserved_when_repair_full() {
        let h = PresentDamageHistory::new();
        let current = partial(&[r(5, 5, 30, 30)]);
        let plan = build_present_plan(current.clone(), &h, true, 0, true, true);
        assert!(plan.repair.is_full_surface());
        assert_eq!(plan.current, current);
    }

    // ── KHR-only vs EXT-independent demotion ─────────────────────────────────

    #[test]
    fn khr_only_declaration_failure_forces_full() {
        let h = PresentDamageHistory::new();
        let current = partial(&[r(0, 0, 100, 100)]);
        let plan = build_present_plan(current, &h, true, 1, true, true);
        let demoted = repair_after_declaration_failure(plan, false);
        assert!(demoted.repair.is_full_surface());
    }

    #[test]
    fn ext_independent_declaration_failure_keeps_partial() {
        let h = PresentDamageHistory::new();
        let current = partial(&[r(0, 0, 100, 100)]);
        let plan = build_present_plan(current, &h, true, 1, true, true);
        let kept = repair_after_declaration_failure(plan, true);
        assert!(!kept.repair.is_full_surface());
    }

    // ── age 1 preserves 1-4 current rects ────────────────────────────────────

    #[test]
    fn age_1_single_rect() {
        let h = PresentDamageHistory::new();
        let rect = r(10, 10, 50, 50);
        let current = DamageRegion::from_rect(rect);
        let plan = build_present_plan(current, &h, true, 1, true, true);
        let repair_rects = plan.repair.rects().expect("partial repair");
        assert_eq!(repair_rects, &[rect]);
    }

    #[test]
    fn age_1_four_rects_preserved() {
        let h = PresentDamageHistory::new();
        let rects = [
            r(0, 0, 10, 10),
            r(20, 0, 10, 10),
            r(0, 20, 10, 10),
            r(20, 20, 10, 10),
        ];
        let current = partial(&rects);
        let plan = build_present_plan(current, &h, true, 1, true, true);
        let repair_rects = plan.repair.rects().expect("partial repair");
        assert_eq!(repair_rects.len(), 4);
    }

    // ── age 2/3 unions exactly the newest required frames ────────────────────

    #[test]
    fn age_2_unions_current_with_newest_history() {
        let mut h = PresentDamageHistory::new();
        let hist_rect = r(50, 50, 10, 10);
        let curr_rect = r(0, 0, 10, 10);
        h.push(partial(&[hist_rect]));
        let current = partial(&[curr_rect]);
        let plan = build_present_plan(current, &h, true, 2, true, true);
        let repair_rects = plan.repair.rects().expect("partial repair");
        assert_eq!(repair_rects.len(), 2);
        assert!(repair_rects.contains(&curr_rect));
        assert!(repair_rects.contains(&hist_rect));
    }

    #[test]
    fn age_3_unions_current_with_two_newest_history() {
        let mut h = PresentDamageHistory::new();
        let h0 = r(0, 0, 10, 10);
        let h1 = r(20, 20, 10, 10);
        let older = r(200, 200, 5, 5);
        h.push(partial(&[older]));
        h.push(partial(&[h1]));
        h.push(partial(&[h0]));
        let curr_rect = r(40, 40, 10, 10);
        let current = partial(&[curr_rect]);
        let plan = build_present_plan(current, &h, true, 3, true, true);
        let repair_rects = plan.repair.rects().expect("partial repair");
        assert!(repair_rects.contains(&curr_rect));
        assert!(repair_rects.contains(&h0));
        assert!(repair_rects.contains(&h1));
        assert!(!repair_rects.contains(&older));
    }

    // ── >4 rects collapse to sticky AABB ─────────────────────────────────────

    #[test]
    fn fifth_rect_collapses_to_aabb() {
        let r1 = r(0, 0, 10, 10);
        let r2 = r(20, 0, 10, 10);
        let r3 = r(0, 20, 10, 10);
        let r4 = r(20, 20, 10, 10);
        let r5 = r(40, 40, 10, 10);

        let region = partial(&[r1, r2, r3, r4]);
        let region = region.union(DamageRegion::from_rect(r5));

        let p = match &region {
            DamageRegion::Partial(p) => p,
            _ => panic!("expected Partial"),
        };
        assert!(p.is_collapsed());
        assert_eq!(p.rects().len(), 1);
        let aabb = p.rects()[0];
        assert!(aabb.x <= 0 && aabb.y <= 0);
        assert!(aabb.right() >= 50 && aabb.top() >= 50);
    }

    #[test]
    fn post_collapse_union_stays_collapsed_does_not_refragment() {
        let r1 = r(0, 0, 10, 10);
        let r2 = r(20, 0, 10, 10);
        let r3 = r(0, 20, 10, 10);
        let r4 = r(20, 20, 10, 10);
        let r5 = r(40, 40, 10, 10);

        let region = partial(&[r1, r2, r3, r4]);
        let region = region.union(DamageRegion::from_rect(r5));
        let region = region.union(DamageRegion::from_rect(r(100, 100, 5, 5)));
        let region = region.union(DamageRegion::from_rect(r(200, 200, 5, 5)));

        let p = match &region {
            DamageRegion::Partial(p) => p,
            _ => panic!("expected Partial"),
        };
        assert!(p.is_collapsed(), "must stay collapsed");
        assert_eq!(p.rects().len(), 1, "must remain one AABB");
        let aabb = p.rects()[0];
        assert!(aabb.right() >= 205 && aabb.top() >= 205);
    }

    #[test]
    fn unrepresentable_aabb_span_fails_closed_to_full_surface() {
        let rects = [
            r(i32::MIN, 0, 1, 1),
            r(i32::MAX - 1, 0, 1, 1),
            r(0, 10, 1, 1),
            r(0, 20, 1, 1),
            r(0, 30, 1, 1),
        ];

        let region = partial(&rects[..4]).union(DamageRegion::from_rect(rects[4]));

        assert_eq!(region, DamageRegion::FullSurface);
    }

    #[test]
    fn unrepresentable_discrete_region_has_no_stats_aabb() {
        let region = partial(&[r(i32::MIN, 0, 1, 1), r(i32::MAX - 1, 0, 1, 1)]);

        assert!(region.bounding_rect().is_none());
    }

    // ── containment deduplication ─────────────────────────────────────────────

    #[test]
    fn contained_rect_dropped() {
        let big = r(0, 0, 100, 100);
        let small = r(10, 10, 10, 10);
        let region = partial(&[big]);
        let region = region.union(DamageRegion::from_rect(small));
        let rects = region.rects().expect("partial");
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0], big);
    }

    #[test]
    fn exact_duplicate_dropped() {
        let rect = r(5, 5, 20, 20);
        let region = DamageRegion::from_rect(rect);
        let region = region.union(DamageRegion::from_rect(rect));
        let rects = region.rects().expect("partial");
        assert_eq!(rects.len(), 1);
    }

    #[test]
    fn new_rect_containing_existing_absorbs_it() {
        let small = r(10, 10, 5, 5);
        let big = r(0, 0, 100, 100);
        let region = DamageRegion::from_rect(small);
        let region = region.union(DamageRegion::from_rect(big));
        let rects = region.rects().expect("partial");
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0], big);
    }

    // ── history mechanics ─────────────────────────────────────────────────────

    #[test]
    fn history_advances_only_on_push() {
        let mut h = PresentDamageHistory::new();
        assert_eq!(h.len(), 0);
        h.push(partial(&[r(0, 0, 10, 10)]));
        assert_eq!(h.len(), 1);
        h.push(partial(&[r(10, 10, 10, 10)]));
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn history_clears_on_reset() {
        let mut h = PresentDamageHistory::new();
        h.push(partial(&[r(0, 0, 10, 10)]));
        h.push(partial(&[r(20, 20, 10, 10)]));
        h.clear();
        assert_eq!(h.len(), 0);
    }

    #[test]
    fn history_evicts_oldest_when_at_capacity() {
        let mut h = PresentDamageHistory::new();
        h.push(partial(&[r(0, 0, 5, 5)]));
        h.push(partial(&[r(10, 0, 5, 5)]));
        h.push(partial(&[r(20, 0, 5, 5)]));
        h.push(partial(&[r(30, 0, 5, 5)]));
        h.push(partial(&[r(40, 0, 5, 5)]));
        assert_eq!(h.len(), 4);
        let current = partial(&[r(50, 0, 5, 5)]);
        let repair = h.resolve_with_age(&current, 5);
        assert!(!repair.is_full_surface());
        let repair_rects = repair.rects().expect("partial");
        assert!(!repair_rects.contains(&r(0, 0, 5, 5)));
    }

    #[test]
    fn history_newest_entry_is_at_index_zero() {
        let mut h = PresentDamageHistory::new();
        h.push(partial(&[r(0, 0, 10, 10)]));
        h.push(partial(&[r(99, 99, 1, 1)]));
        let current = partial(&[r(50, 50, 5, 5)]);
        let repair = h.resolve_with_age(&current, 2);
        let rects = repair.rects().expect("partial");
        assert!(rects.contains(&r(99, 99, 1, 1)));
    }

    // ── scaling / multisample → Full BlitPlan ────────────────────────────────

    /// A size mismatch means the blit scales, and a scaling blit needs LINEAR —
    /// `glBlitFramebuffer` rejects a scaling NEAREST on some drivers.
    #[test]
    fn size_mismatch_gives_a_scaling_full_blit() {
        let repair = partial(&[r(0, 0, 100, 100)]);
        assert_eq!(
            blit_plan(&repair, false, true),
            BlitPlan::Full { linear: true }
        );
    }

    /// **The filter must follow the geometry, not the reason for falling back.**
    ///
    /// Both cases below fail partial-repair eligibility for reasons that have
    /// nothing to do with size — a multisampled destination, and damage that
    /// covers everything — and in both the DrawingBuffer still matches the
    /// surface. So the blit is a straight 1:1 copy and must ask for NEAREST.
    ///
    /// These two asserted `linear: true` before, which is how the engine came to
    /// bilinear-filter a full-screen copy on every frame of any device without
    /// the buffer-age extensions, and on every frame of any content that clears
    /// the whole screen. Four texels read per output pixel where one would do.
    #[test]
    fn a_same_size_full_blit_uses_nearest_whatever_forced_it() {
        // Multisampled destination: partial repair is ineligible, size is not the
        // reason.
        let repair = partial(&[r(0, 0, 100, 100)]);
        assert_eq!(
            blit_plan(&repair, true, false),
            BlitPlan::Full { linear: false },
            "a multisample fallback scaled nothing, so it must not filter"
        );

        // Full-surface damage: nothing to repair partially, still a 1:1 copy.
        assert_eq!(
            blit_plan(&DamageRegion::FullSurface, true, true),
            BlitPlan::Full { linear: false },
            "full-surface damage on a matching buffer is still a straight copy"
        );

        // And when the sizes really do differ, both fallbacks scale.
        assert_eq!(
            blit_plan(&DamageRegion::FullSurface, false, true),
            BlitPlan::Full { linear: true }
        );
        assert_eq!(
            blit_plan(&repair, false, false),
            BlitPlan::Full { linear: true }
        );
    }

    // ── same-size partial → Rects NEAREST ────────────────────────────────────

    #[test]
    fn same_size_single_sample_gives_rect_blit_plan() {
        let rect = r(10, 10, 50, 50);
        let repair = DamageRegion::from_rect(rect);
        let plan = blit_plan(&repair, true, true);
        match plan {
            BlitPlan::Rects(br) => {
                assert_eq!(br.rects(), &[rect]);
            }
            other => panic!("expected Rects, got {other:?}"),
        }
    }

    #[test]
    fn rect_blit_coords_are_identical_source_and_dest_no_y_flip() {
        let rects_in = [r(0, 0, 10, 10), r(50, 50, 20, 20)];
        let repair = partial(&rects_in);
        let plan = blit_plan(&repair, true, true);
        match plan {
            BlitPlan::Rects(br) => {
                let out = br.rects();
                for expected in &rects_in {
                    assert!(
                        out.contains(expected),
                        "rect {expected:?} missing from blit plan"
                    );
                }
            }
            other => panic!("expected Rects, got {other:?}"),
        }
    }

    // ── build_present_plan eligibility checks ─────────────────────────────────

    #[test]
    fn size_mismatch_in_plan_gives_full_repair() {
        let mut h = PresentDamageHistory::new();
        h.push(partial(&[r(0, 0, 10, 10)]));
        let current = partial(&[r(0, 0, 10, 10)]);
        let plan = build_present_plan(current, &h, true, 2, false, true);
        assert!(plan.repair.is_full_surface());
    }

    #[test]
    fn multisample_in_plan_gives_full_repair() {
        let mut h = PresentDamageHistory::new();
        h.push(partial(&[r(0, 0, 10, 10)]));
        let current = partial(&[r(0, 0, 10, 10)]);
        let plan = build_present_plan(current, &h, true, 2, true, false);
        assert!(plan.repair.is_full_surface());
    }

    // ── full-surface union ─────────────────────────────────────────────────────

    #[test]
    fn full_union_anything_stays_full() {
        let region = DamageRegion::FullSurface;
        let other = partial(&[r(0, 0, 100, 100)]);
        assert_eq!(region.union(other), DamageRegion::FullSurface);
    }

    #[test]
    fn anything_union_full_stays_full() {
        let region = partial(&[r(0, 0, 100, 100)]);
        assert_eq!(
            region.union(DamageRegion::FullSurface),
            DamageRegion::FullSurface
        );
    }
}

// ── Wiring source-contract guards ──────────────────────────────────────────────
//
// The manager/DrawingBuffer wiring cannot be unit-tested at runtime on this host
// (the full `graphics` test binary fails to link without EGL/freetype/fontconfig).
// The pure decision logic above IS runtime-tested; these guards lock the
// remaining wiring invariants that the pure tests cannot observe — that the
// manager consumes `repair` (never `current`) for the EGL declaration and blit,
// records `current` (never `repair`) into history, queries the destination
// sample config + the independent EXT flag, and clears the pending plan on every
// lifecycle reset; and that the blit consumes a `BlitPlan`, restores scissor once,
// and never invalidates the framebuffer. `include_str!` keeps them host-runnable
// (paths resolve relative to this file under both `rustc --test` and cargo).
#[cfg(test)]
mod wiring_source_guards {
    const MGR: &str = include_str!("canvas/manager/mod.rs");
    const DB: &str = include_str!("canvas/manager/drawing_buffer.rs");

    fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .expect("function signature must exist");
        let source = &source[start..];
        let open = source.find('{').expect("function body must open");
        let mut depth = 0usize;
        for (offset, ch) in source[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &source[open + 1..open + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("function body must close");
    }

    #[test]
    fn manager_uses_present_damage_history_not_legacy() {
        assert!(
            MGR.contains("PresentDamageHistory"),
            "manager must store the multi-rect PresentDamageHistory"
        );
        assert!(
            !MGR.contains("damage_tracker::DamageHistory"),
            "legacy AABB-only DamageHistory must be removed from the manager"
        );
    }

    #[test]
    fn manager_uses_keyed_pending_plan() {
        assert!(
            MGR.contains("pending_present_plan"),
            "manager must cache a keyed PresentDamagePlan"
        );
        assert!(
            !MGR.contains("pending_declared_damage"),
            "legacy tuple cache must be replaced by the keyed plan"
        );
        assert!(
            MGR.contains("pending_present_plan = Some((id,"),
            "declare_frame_damage must cache the plan keyed by canvas id"
        );
    }

    #[test]
    fn manager_blit_consumes_repair_and_history_records_current() {
        // The blit's region argument must be `repair`, never `current`. Scan the
        // window after the single `blit_plan(` call so the guard is robust to
        // rustfmt wrapping the arguments across lines.
        let call = MGR.find("blit_plan(").expect("swap must call blit_plan");
        let window = &MGR[call..(call + 160).min(MGR.len())];
        assert!(
            window.contains("&plan.repair"),
            "blit_plan's region argument must be the repair region"
        );
        assert!(
            !window.contains("plan.current"),
            "the blit must never be derived from the current region"
        );
        // Which region enters the history is no longer a text question: the
        // choice moved inside `commit_present_outcome`, which receives the whole
        // plan, and `history_records_the_frames_own_damage_and_never_the_age_
        // expanded_repair` drives it against a real ring buffer with the two
        // regions made distinguishable.
    }

    #[test]
    fn manager_queries_config_samples_and_independent_ext_flag() {
        assert!(
            MGR.contains("EGL_SAMPLE_BUFFERS") && MGR.contains("dest_single_sample"),
            "manager must query the selected EGL config sample buffers/samples"
        );
        assert!(
            MGR.contains("repair_after_declaration_failure") && MGR.contains("has_ext_buffer_age"),
            "declaration failure must demote via the independent EGL_EXT_buffer_age flag"
        );
    }

    #[test]
    fn manager_clears_pending_plan_on_every_lifecycle_reset() {
        let clears = MGR.matches("self.pending_present_plan = None;").count();
        assert!(
            clears >= 4,
            "every history-clear lifecycle site must also clear the pending plan (found {clears})"
        );
    }

    #[test]
    fn manager_makes_target_surface_current_before_age_query_and_declaration() {
        let body = function_body(MGR, "fn prepare_present_plan");
        let make_current = body
            .find("make_current_needed(id)")
            .expect("present-plan preparation must make the target surface current");
        let age_query = body
            .find("query_surface")
            .expect("present-plan preparation must query buffer age");
        assert!(
            make_current < age_query,
            "the target surface must be current before querying age or declaring damage"
        );
    }

    #[test]
    fn onscreen_surface_recreate_carries_the_2d_drawing_state_across() {
        // JS shadows every Canvas2D state setter and skips re-sending a value it
        // believes is current, so the render-side state machine is the
        // authoritative half of a pair. A context rebuilt at spec defaults
        // desynchronises them for good: the content never re-sends its
        // fillStyle, and every later fill paints the default opaque black. The
        // symptom is a black screen on resume with JS still drawing, the
        // context healthy, and blit and swap both reporting success -- nothing
        // downstream can catch it, so the wiring is asserted here.
        //
        // The capture lives in the *teardown*, not in the install. On Android
        // those are separate events: `surfaceDestroyed` takes the context away
        // when the app is backgrounded and `surfaceCreated` arrives whenever
        // the user returns. An install that asks `contexts_2d` whether this
        // canvas had a 2D context is asking after the answer was destroyed --
        // it reads `false`, skips the rebuild, and the game paints into nothing
        // for the rest of its life. Measured on a Mate30 Pro: ~900k
        // `2d context not found` per 8s with rAF still running at 60fps.
        let teardown = function_body(MGR, "fn destroy_onscreen_internal");
        let stash = teardown
            .find("self.stash_onscreen_2d_restore(id)")
            .expect("teardown must record what the next install owes the content");
        let drop = teardown
            .find("self.drop_2d_context(id")
            .expect("teardown must drop the 2D context");
        assert!(
            stash < drop,
            "the 2D drawing state must be recorded before the context is dropped"
        );

        let helper = function_body(MGR, "fn stash_onscreen_2d_restore");
        assert!(
            helper.contains("drawing_state()"),
            "the recorded obligation must carry the drawing state, not just a flag"
        );

        let create = function_body(MGR, "pub(crate) fn create_onscreen");
        let take = create.find("self.onscreen_2d_restore.take()").expect(
            "the install must discharge the obligation a teardown recorded -- deriving it \
                 from `contexts_2d` here reads a context an earlier surfaceDestroyed \
                 already removed",
        );
        let reinit = create
            .find("context_2d_impl::init_skia_for_canvas")
            .expect("surface recreate must re-create the 2D context");
        let adopt = create
            .find("adopt_drawing_state")
            .expect("surface recreate must restore the captured 2D drawing state");
        assert!(
            take < reinit && reinit < adopt,
            "the obligation must be read first, then the context rebuilt, then the state adopted"
        );
    }

    #[test]
    fn onscreen_drawing_buffer_resize_invalidates_present_state() {
        let resize = function_body(MGR, "pub(crate) fn resize_canvas");
        let start = resize
            .find("drawing_buffer::resize")
            .expect("DrawingBuffer resize policy must exist");
        let end = resize[start..]
            .find("self.evaluate_bypass()")
            .expect("onscreen resize policy must re-evaluate bypass");
        let branch = &resize[start..start + end];
        assert!(
            branch.contains("self.damage_history.clear()"),
            "DrawingBuffer resize must invalidate buffer-age history"
        );
        assert!(
            branch.contains("self.pending_present_plan = None;"),
            "DrawingBuffer resize must discard any plan built for the old storage"
        );
        assert!(
            branch.contains("DamageEffect::FullSurface"),
            "the first present after DrawingBuffer resize must be full"
        );
    }

    #[test]
    fn same_window_surface_resize_updates_physical_extent() {
        let create = function_body(MGR, "pub(crate) fn create_onscreen");
        let start = create
            .find("CanvasManager::create_onscreen fast resize")
            .expect("same-window surface resize branch must exist");
        let end = create[start..]
            .find("Validate the client API")
            .expect("fast resize branch must precede full surface recreation");
        let branch = &create[start..start + end];
        assert!(
            branch.contains("entry.physical_width =") && branch.contains("entry.physical_height ="),
            "surface-driven fast resize must update the dimensions used by present/blit"
        );
    }

    /// A resize the content did not ask for must not reset its 2D state.
    ///
    /// `Canvas2DContext::resize` returns the state machine to spec defaults --
    /// right for `canvas.width = N`, wrong for a surface the platform swapped
    /// underneath a running game. The JS setters de-duplicate against a shadow
    /// no surface change clears, so a context that came back at defaults stays
    /// there: the content believes its fill style is already in force and never
    /// sends it again. Nothing reports an error; the game just draws in black.
    ///
    /// Asserted over EVERY caller rather than the one that was wrong, because
    /// whoever adds the next surface-driven resize will not have read this.
    #[test]
    fn a_resize_the_content_did_not_ask_for_keeps_its_2d_drawing_state() {
        let helper = function_body(MGR, "fn resize_canvas_for_surface_change");
        let capture = helper
            .find("drawing_state()")
            .expect("the surface-driven resize must capture the drawing state");
        let resize = helper
            .find("self.resize_canvas(")
            .expect("the surface-driven resize must delegate the resize itself");
        let adopt = helper
            .find("adopt_drawing_state")
            .expect("the surface-driven resize must give the state back");
        assert!(
            capture < resize && resize < adopt,
            "capture the state, resize, then adopt it back -- in that order"
        );

        // Every surface-install path that resizes must go through that helper.
        // `create_onscreen`'s fast path called `resize_canvas` directly and is
        // why this test exists.
        let production = MGR
            .split_once("#[cfg(test)]")
            .map(|(before, _)| before)
            .expect("mod.rs must have a test module boundary to cut at");
        let allowed = [
            "fn resize_canvas_for_surface_change",
            // The content-driven entry point: resetting is what the spec says
            // `canvas.width = N` does, and the JS shadow resets to match.
            "pub(crate) fn resize_canvas",
        ];
        let mut offenders = Vec::new();
        for (index, _) in production.match_indices("fn ") {
            let tail = &production[index..];
            let Some(name_end) = tail.find(['(', '<']) else {
                continue;
            };
            let signature = &tail[..name_end];
            if allowed.iter().any(|a| a.ends_with(signature)) {
                continue;
            }
            let body_end = tail.find("\n    }").unwrap_or(tail.len());
            let body = &tail[..body_end];
            if body.contains("self.resize_canvas(") {
                offenders.push(signature.trim().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "these resize a canvas without carrying the content's 2D drawing \
             state across, so it keeps drawing with values the render side no \
             longer has: {offenders:?}"
        );
    }

    /// Adopting the state must put the transform back on the Skia canvas.
    ///
    /// `Canvas2DState::ctm` is a mirror of `SkCanvas`'s CTM, not the thing Skia
    /// draws with, and a replacement surface starts at identity. Copying the
    /// mirror alone leaves the damage classifier transforming rectangles with a
    /// matrix Skia is not using -- and silently drops a transform the content
    /// set once at start-up, which is the same de-duplication trap in a field
    /// that looks like bookkeeping.
    #[test]
    fn adopting_2d_state_reapplies_the_transform_to_skia() {
        const SURFACE: &str = include_str!("backend/gl/surface.rs");
        let adopt = function_body(SURFACE, "pub fn adopt_drawing_state");
        assert!(
            adopt.contains("state.ctm"),
            "adopting the state must read the transform out of it"
        );
        assert!(
            adopt.contains("set_matrix"),
            "adopting the state must re-apply the transform to the Skia canvas, \
             which comes up at identity behind a replacement surface"
        );
    }

    // `swap_failure_preserves_accumulated_damage_for_retry` and
    // `blit_failure_poison_is_propagated_to_present_history` stood here. Both
    // read the source text of `swap_buffers_no_restore` and asserted the byte
    // offsets of `.swap_buffers(`, `self.damage.reset()` and `blit_succeeded`
    // were in order — inspection wearing a test's clothing, on the presentation
    // path, which is what ledger 0.56 named them as. The properties are real and
    // are orderings, so `swap_buffers_no_restore` now takes the swap outcome as
    // data and hands the bookkeeping to `commit_present_outcome`, whose three
    // branches are driven directly against a real `FrameDamageAccumulator` and
    // `PresentDamageHistory` in `canvas::manager`'s tests. What is left here is
    // only what genuinely has no host-observable behaviour: the wiring of one
    // function's calls to another's, where the effect needs a GL context.
    #[test]
    fn drawing_buffer_blit_reports_whether_every_repair_write_succeeded() {
        // Still text, because the alternative is a live GL context. Narrowed to
        // the signature: a blit that cannot report failure makes the poison
        // branch above unreachable, and no host test can see that.
        let declaration = DB
            .find("pub(crate) fn blit_to_surface")
            .expect("DrawingBuffer blit function must exist");
        let signature_end = DB[declaration..]
            .find('{')
            .expect("DrawingBuffer blit function body must open");
        assert!(
            DB[declaration..declaration + signature_end].contains("-> bool"),
            "DrawingBuffer blit must report whether every repair write succeeded"
        );
    }

    #[test]
    fn bypass_transition_invalidates_present_history() {
        let body = function_body(MGR, "pub(crate) fn evaluate_bypass");
        assert!(
            body.contains("self.damage_history.clear()")
                && body.contains("self.pending_present_plan = None;")
                && body.contains("DamageEffect::FullSurface"),
            "switching between direct-FBO and DrawingBuffer presentation must force a full boundary"
        );
    }

    #[test]
    fn drawing_buffer_consumes_blit_plan_with_correct_filters() {
        assert!(
            DB.contains("BlitPlan"),
            "blit_to_surface must consume a BlitPlan"
        );
        assert!(
            DB.contains("BlitPlan::Rects") && DB.contains("BlitPlan::Full"),
            "blit must handle both the rect and full variants"
        );
        assert!(
            DB.contains("glow::NEAREST"),
            "identity partial rect blits must use NEAREST"
        );
        assert!(
            DB.contains("glow::LINEAR"),
            "the full/scaled fallback must keep LINEAR filtering"
        );
    }

    #[test]
    fn drawing_buffer_no_invalidate_single_scissor_restore() {
        assert!(
            !DB.contains("invalidate_framebuffer") && !DB.contains("invalidate_sub_framebuffer"),
            "must not invalidate the framebuffer — buffer-age repair needs persistent pixels"
        );
        // Per blitting function, not per file. Each disables the scissor test
        // for its own blit -- `glBlitFramebuffer` writes through it, and games
        // leave one enabled over a sub-window box -- and each must restore it
        // from exactly one epilogue that every exit path reaches. Counting
        // across the file made adding a second blit look like a second restore
        // inside the first one, which is the thing this actually guards.
        for signature in [
            "pub(crate) fn blit_to_surface",
            "pub(crate) fn blit_from_surface",
        ] {
            let body = function_body(DB, signature);
            let restores = body.matches("enable(glow::SCISSOR_TEST)").count();
            assert_eq!(
                restores, 1,
                "{signature}: scissor enable must be restored from exactly one cleanup \
                 epilogue (found {restores})"
            );
            assert!(
                body.contains("disable(glow::SCISSOR_TEST)"),
                "{signature}: a blit must not write through the game's scissor box"
            );
        }
    }

    /// The snapshot the readback signal documents must actually be taken.
    ///
    /// `signal_default_fbo_readback` has described a reverse blit since the flag
    /// was introduced, and for that whole time the function only set the flag:
    /// the first `readPixels` on a context that had been bypassing the
    /// DrawingBuffer returned an empty buffer for pixels the game had just
    /// drawn. Pinned against the source because the effect needs a GL context.
    #[test]
    fn readback_signal_snapshots_the_surface_before_leaving_bypass() {
        let body = function_body(MGR, "pub(crate) fn signal_default_fbo_readback");
        let snapshot = body
            .find("blit_from_surface")
            .expect("leaving bypass for a readback must snapshot the surface");
        let latch = body
            .find("self.needs_default_fbo_readback = true;")
            .expect("the readback latch must remain");
        assert!(
            snapshot < latch,
            "the snapshot must be taken while bypass is still on, i.e. before the latch \
             that turns it off"
        );
    }
}
