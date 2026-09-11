//! Incremental path builder mirroring Canvas2D's `CanvasPath` semantics
//! atop Skia's [`skia_safe::PathBuilder`].
//!
//! Skia separates *mutable* path construction (`PathBuilder`) from
//! *immutable* drawing primitives (`Path`).  We wrap a `PathBuilder` and
//! expose Canvas2D-style incremental methods; callers snapshot a `Path`
//! via [`CanvasPath::snapshot`] before passing to `Canvas::draw_path` /
//! `Canvas::clip_path`.
//!
//! Semantic adjustments over `PathBuilder`:
//!
//!   * Canvas `arc(cx, cy, r, startAngle, endAngle, ccw)` allows any angle
//!     (positive or negative) and sweeps in the direction dictated by
//!     `ccw`.  `PathBuilder::arc_to` takes a bounding rect + start +
//!     sweep; we normalise angles in [`normalise_arc`] first.
//!   * Canvas `ellipse(...)` adds rotation; Skia has no rotated-ellipse
//!     primitive, so we build an arc subpath in the unit circle space
//!     then concatenate it with a pre-transform.
//!   * Canvas `arcTo(x1, y1, x2, y2, r)` degenerates to `lineTo(x1, y1)`
//!     when the three points are collinear or `r == 0`; Skia handles
//!     that correctly, but we track `has_current_point` ourselves to
//!     honour the spec's implicit-moveTo rules.

use skia_safe::{Matrix, Path, PathDirection, Point, Rect, path_builder::PathBuilder};

use std::f32::consts::TAU;

/// Canvas2D-style incremental path builder around a Skia [`PathBuilder`].
pub struct CanvasPath {
    inner: PathBuilder,
    has_current_point: bool,
    /// Start point of the current subpath; used for `close_path` /
    /// implicit `moveTo` after close.
    subpath_start: Option<Point>,
}

impl Default for CanvasPath {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CanvasPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasPath")
            .field("has_current_point", &self.has_current_point)
            .field("subpath_start", &self.subpath_start)
            .finish_non_exhaustive()
    }
}

impl CanvasPath {
    pub fn new() -> Self {
        Self {
            inner: PathBuilder::new(),
            has_current_point: false,
            subpath_start: None,
        }
    }

    /// Clear the current path while retaining Skia's internal arrays.
    ///
    /// `beginPath()` is a hot operation for content that rebuilds the same
    /// geometry every frame; Skia's reset preserves the builder's storage so
    /// this avoids reallocating on the next path.  Call [`Self::shrink`] at
    /// destroy, resize, or a real low-memory event when that high-water
    /// storage is no longer useful.
    pub fn reset(&mut self) {
        self.inner.reset();
        self.has_current_point = false;
        self.subpath_start = None;
    }

    /// Release retained Skia path storage and return to a fresh builder.
    ///
    /// This is intentionally separate from [`Self::reset`]: normal
    /// `beginPath()` calls should reuse storage, while lifecycle and
    /// low-memory paths must be able to drop a pathological high-water mark.
    pub fn shrink(&mut self) {
        self.inner = PathBuilder::new();
        self.has_current_point = false;
        self.subpath_start = None;
    }

    /// Number of points currently retained by the mutable path.
    #[inline]
    pub fn point_count(&self) -> usize {
        self.inner.count_points()
    }

    /// Number of verbs currently retained by the mutable path.
    #[inline]
    pub fn verb_count(&self) -> usize {
        self.inner.verbs().len()
    }

    /// Snapshot the current state as an immutable `Path` ready for drawing.
    /// The builder is unchanged, so further edits continue to affect the
    /// current path (matching Canvas2D's "current default path" model).
    pub fn snapshot(&self) -> Path {
        self.inner.snapshot()
    }

    #[inline]
    pub fn has_current_point(&self) -> bool {
        self.has_current_point
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.inner.move_to(Point::new(x, y));
        self.has_current_point = true;
        self.subpath_start = Some(Point::new(x, y));
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        if !self.has_current_point {
            self.move_to(x, y);
        }
        self.inner.line_to(Point::new(x, y));
    }

    pub fn close_path(&mut self) {
        if self.has_current_point {
            self.inner.close();
            if let Some(start) = self.subpath_start {
                self.has_current_point = true;
                self.subpath_start = Some(start);
            }
        }
    }

    pub fn quadratic_to(&mut self, cpx: f32, cpy: f32, x: f32, y: f32) {
        if !self.has_current_point {
            self.move_to(cpx, cpy);
        }
        self.inner.quad_to(Point::new(cpx, cpy), Point::new(x, y));
    }

    pub fn bezier_to(&mut self, cp1x: f32, cp1y: f32, cp2x: f32, cp2y: f32, x: f32, y: f32) {
        if !self.has_current_point {
            self.move_to(cp1x, cp1y);
        }
        self.inner.cubic_to(
            Point::new(cp1x, cp1y),
            Point::new(cp2x, cp2y),
            Point::new(x, y),
        );
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let r = Rect::from_xywh(x, y, w, h);
        self.inner.add_rect(r, Some(PathDirection::CW), Some(0));
        self.has_current_point = true;
        self.subpath_start = Some(Point::new(x, y));
    }

    pub fn arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        start_angle: f32,
        end_angle: f32,
        ccw: bool,
    ) {
        if radius <= 0.0 {
            self.line_to(cx, cy);
            return;
        }

        let (start, sweep) = normalise_arc(start_angle, end_angle, ccw);

        let sx = cx + radius * start.cos();
        let sy = cy + radius * start.sin();
        if self.has_current_point {
            self.inner.line_to(Point::new(sx, sy));
        } else {
            self.inner.move_to(Point::new(sx, sy));
            self.subpath_start = Some(Point::new(sx, sy));
            self.has_current_point = true;
        }

        let bounds = Rect::from_ltrb(cx - radius, cy - radius, cx + radius, cy + radius);

        // Skia's `arc_to` treats `sweep >= 360` as a degenerate case and
        // may produce only the starting point.  Canvas spec requires a
        // full oval, so split into two ~180° sweeps when near 2π.
        let sweep_deg = sweep.to_degrees();
        let start_deg = start.to_degrees();
        if sweep_deg.abs() >= 360.0 - 1e-3 {
            let half = sweep_deg * 0.5;
            self.inner.arc_to(bounds, start_deg, half, false);
            self.inner.arc_to(bounds, start_deg + half, half, false);
        } else {
            self.inner.arc_to(bounds, start_deg, sweep_deg, false);
        }
    }

    pub fn arc_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, r: f32) {
        if !self.has_current_point {
            self.move_to(x1, y1);
        }
        self.inner
            .arc_to_tangent(Point::new(x1, y1), Point::new(x2, y2), r);
    }

    pub fn ellipse(
        &mut self,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        rotation: f32,
        start: f32,
        end: f32,
        ccw: bool,
    ) {
        if rx <= 0.0 || ry <= 0.0 {
            self.line_to(cx, cy);
            return;
        }

        let (start, sweep) = normalise_arc(start, end, ccw);

        // Start point of the arc in user space (after the rotation).
        let cs = start.cos();
        let sn = start.sin();
        let rot_cos = rotation.cos();
        let rot_sin = rotation.sin();
        let px = cx + rot_cos * rx * cs - rot_sin * ry * sn;
        let py = cy + rot_sin * rx * cs + rot_cos * ry * sn;

        if self.has_current_point {
            self.inner.line_to(Point::new(px, py));
        } else {
            self.inner.move_to(Point::new(px, py));
        }

        // Build the arc in unit-circle space then transform into user
        // space via a pre-matrix.
        let mut sub = PathBuilder::new();
        let unit = Rect::from_ltrb(-1.0, -1.0, 1.0, 1.0);
        sub.arc_to(unit, start.to_degrees(), sweep.to_degrees(), true);

        let mut m = Matrix::new_identity();
        m.post_scale((rx, ry), None);
        m.post_rotate(rotation.to_degrees(), None);
        m.post_translate((cx, cy));
        let transformed = sub.snapshot_and_transform(&m);
        self.inner.add_path(&transformed);

        self.has_current_point = true;
        if self.subpath_start.is_none() {
            self.subpath_start = Some(Point::new(px, py));
        }
    }
}

/// Normalise Canvas2D arc angles into `(start, sweep)` where `sweep` is
/// bounded by ±2π, sign indicating sweep direction.  Used by both `arc`
/// and `ellipse`.
/// **The order of the two `ccw` arms is load-bearing.**
///
/// WHATWG's full-circle condition is asymmetric: clockwise needs
/// `endAngle - startAngle >= 2π`, anticlockwise needs
/// `startAngle - endAngle >= 2π` — that is, `delta <= -2π`. The
/// anticlockwise arm below tests `delta.abs() >= TAU`, which would also catch
/// `delta >= 2π`; it does not, because the `delta > 0.0` arm takes those first.
/// So `abs()` only ever sees `delta <= -2π`, which is exactly the spec's
/// condition.
///
/// Swap the two and `arc(…, 0, 3π, ccw = true)` becomes a full circle instead of
/// the π sweep the spec asks for. `normalise_arc_ccw_past_a_full_turn_is_not_a_
/// full_circle` pins it.
fn normalise_arc(start: f32, end: f32, ccw: bool) -> (f32, f32) {
    let delta = end - start;
    let sweep = if ccw {
        if delta > 0.0 {
            -(TAU - delta.rem_euclid(TAU))
        } else if delta.abs() >= TAU {
            -TAU
        } else {
            delta
        }
    } else if delta < 0.0 {
        TAU - (-delta).rem_euclid(TAU)
    } else if delta >= TAU {
        TAU
    } else {
        delta
    };
    (start, sweep.clamp(-TAU, TAU))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Snapshot + copy bounds into an owned value so the snapshot `Path`'s
    /// borrow of the bounds rect lifetime-wise doesn't leak into callers.
    fn bounds(p: &CanvasPath) -> Rect {
        *p.snapshot().bounds()
    }

    #[test]
    fn move_and_line_create_subpath() {
        let mut p = CanvasPath::new();
        p.move_to(10.0, 20.0);
        p.line_to(50.0, 60.0);
        assert!(p.has_current_point());
        let b = bounds(&p);
        assert!((b.left - 10.0).abs() < 1e-4);
        assert!((b.right - 50.0).abs() < 1e-4);
    }

    #[test]
    fn line_to_without_move_implicitly_moves() {
        let mut p = CanvasPath::new();
        p.line_to(7.0, 8.0);
        assert!(p.has_current_point());
    }

    #[test]
    fn rect_bounds_match_input() {
        let mut p = CanvasPath::new();
        p.rect(5.0, 10.0, 20.0, 30.0);
        let b = bounds(&p);
        assert!((b.left - 5.0).abs() < 1e-4);
        assert!((b.top - 10.0).abs() < 1e-4);
        assert!((b.right - 25.0).abs() < 1e-4);
        assert!((b.bottom - 40.0).abs() < 1e-4);
    }

    #[test]
    fn arc_bounds_include_full_circle() {
        let mut p = CanvasPath::new();
        p.arc(50.0, 50.0, 40.0, 0.0, TAU, false);
        let b = bounds(&p);
        assert!((b.left - 10.0).abs() < 0.5, "left {}", b.left);
        assert!((b.right - 90.0).abs() < 0.5);
        assert!((b.top - 10.0).abs() < 0.5);
        assert!((b.bottom - 90.0).abs() < 0.5);
    }

    #[test]
    fn arc_zero_radius_becomes_line_to_center() {
        let mut p = CanvasPath::new();
        p.move_to(0.0, 0.0);
        p.arc(10.0, 10.0, 0.0, 0.0, PI, false);
        let b = bounds(&p);
        assert!((b.left - 0.0).abs() < 1e-4);
        assert!((b.right - 10.0).abs() < 1e-4);
    }

    #[test]
    /// **The asymmetry in WHATWG's full-circle rule, and the branch order that
    /// encodes it.**
    ///
    /// Clockwise is a full circle when `end - start >= 2π`; anticlockwise when
    /// `start - end >= 2π`. So `delta = +3π` is a full circle *only* clockwise —
    /// anticlockwise it is an ordinary sweep to `3π mod 2π = π`, taken the short
    /// way round, which is `-π`.
    ///
    /// The `ccw` arms get this right by ordering alone: `delta > 0.0` is tested
    /// before `delta.abs() >= TAU`, so the `abs()` test only ever sees negative
    /// deltas. Nothing else enforces that, and no test covered the case where it
    /// matters — the three neighbouring tests all use `|delta| <= 2π`.
    #[test]
    fn normalise_arc_ccw_past_a_full_turn_is_not_a_full_circle() {
        // Anticlockwise, more than a full turn in the *positive* direction.
        let (_, sw) = normalise_arc(0.0, 3.0 * PI, true);
        assert!(
            (sw + PI).abs() < 1e-4,
            "expected a -π sweep, got {sw}; the ccw arms were reordered and \
             `delta.abs() >= TAU` swallowed a positive delta as a full circle"
        );

        // Clockwise with the same delta *is* a full circle, per the other half
        // of the rule.
        let (_, cw) = normalise_arc(0.0, 3.0 * PI, false);
        assert!(
            (cw - TAU).abs() < 1e-4,
            "clockwise past a full turn must be a full circle, got {cw}"
        );

        // And the genuinely anticlockwise full turn still is one.
        let (_, ccw_full) = normalise_arc(0.0, -3.0 * PI, true);
        assert!(
            (ccw_full + TAU).abs() < 1e-4,
            "anticlockwise past a full turn the other way must be a full circle, \
             got {ccw_full}"
        );
    }

    #[test]
    fn normalise_arc_full_cw_sweep_is_2pi() {
        let (_, sw) = normalise_arc(0.0, TAU, false);
        assert!((sw - TAU).abs() < 1e-4);
    }

    #[test]
    fn normalise_arc_full_ccw_sweep_is_minus_2pi() {
        let (_, sw) = normalise_arc(0.0, -TAU, true);
        assert!((sw + TAU).abs() < 1e-4);
    }

    #[test]
    fn normalise_arc_inverted_end_with_ccw_goes_shortway() {
        let (_, sw) = normalise_arc(0.0, PI, true);
        assert!(sw < 0.0, "sw={sw} should be negative");
        assert!((sw + PI).abs() < 1e-4);
    }

    #[test]
    fn close_path_on_empty_is_noop() {
        let mut p = CanvasPath::new();
        p.close_path();
        assert!(!p.has_current_point());
        assert!(p.snapshot().is_empty());
    }

    #[test]
    fn reset_clears_state() {
        let mut p = CanvasPath::new();
        p.move_to(1.0, 2.0);
        p.line_to(3.0, 4.0);
        assert!(p.has_current_point());
        p.reset();
        assert!(!p.has_current_point());
        assert!(p.snapshot().is_empty());
    }

    #[test]
    fn arc_to_requires_no_initial_move() {
        let mut p = CanvasPath::new();
        p.arc_to(10.0, 0.0, 10.0, 10.0, 5.0);
        assert!(p.has_current_point());
    }

    #[test]
    fn quadratic_without_move_starts_at_control_point() {
        let mut p = CanvasPath::new();
        p.quadratic_to(5.0, 5.0, 10.0, 10.0);
        assert!(p.has_current_point());
    }

    #[test]
    fn bezier_without_move_starts_at_first_control_point() {
        let mut p = CanvasPath::new();
        p.bezier_to(1.0, 2.0, 3.0, 4.0, 5.0, 6.0);
        assert!(p.has_current_point());
    }

    #[test]
    fn ellipse_zero_radius_is_line_to_center() {
        let mut p = CanvasPath::new();
        p.move_to(0.0, 0.0);
        p.ellipse(20.0, 20.0, 0.0, 10.0, 0.0, 0.0, PI, false);
        let b = bounds(&p);
        assert!(b.right <= 20.0 + 1e-3);
    }
    // ---- T-M2 high-water and builder-reuse tests -------------------------

    /// **HIGH-WATER**: After building a large path, calling `reset()` must
    /// clear the accumulated complexity, and `shrink()` must bring it down
    /// to zero even if `reset()` retained Skia's internal buffer for reuse.
    ///
    /// This is a host-half T-M2 guard: path complexity must not persist
    /// across a canvas `beginPath` or an explicit reset event.
    #[test]
    fn high_water_path_shrink_releases_capacity() {
        let mut p = CanvasPath::new();
        // Build a large path (500 move+line pairs = 1000+ operations).
        for i in 0..500_u32 {
            p.move_to(i as f32, 0.0);
            p.line_to(i as f32 + 0.5, 1.0);
        }
        assert!(
            p.point_count() >= 500,
            "expected >= 500 points before reset, got {}",
            p.point_count()
        );
        assert!(p.verb_count() >= 500, "expected >= 500 verbs before reset");

        // After reset(), all content must be cleared (high-water reachable
        // via a fresh beginPath).
        p.reset();
        assert_eq!(p.point_count(), 0, "reset must clear all points");
        assert_eq!(p.verb_count(), 0, "reset must clear all verbs");
        assert!(!p.has_current_point(), "reset must clear has_current_point");
        assert!(p.snapshot().is_empty(), "reset must produce an empty path");

        // After shrink(), a fresh PathBuilder is installed, releasing the
        // Skia-internal buffer.  The counts are still zero.
        p.shrink();
        assert_eq!(p.point_count(), 0, "shrink must leave point count zero");
        assert_eq!(p.verb_count(), 0, "shrink must leave verb count zero");
        assert!(!p.has_current_point());
    }

    /// **SEMANTICS**: After `reset()`, adding the same geometry must produce
    /// identical bounds and point counts as a freshly constructed builder.
    ///
    /// This pins the invariant that the in-place Skia reset fully clears
    /// internal state: reusing the buffer must not leak geometry from the
    /// previous path into the new one.
    #[test]
    fn builder_reuse_semantics_match_fresh_builder() {
        fn add_geometry(p: &mut CanvasPath) {
            p.move_to(0.0, 0.0);
            p.line_to(10.0, 0.0);
            p.line_to(10.0, 10.0);
            p.close_path();
            p.arc(50.0, 50.0, 20.0, 0.0, TAU, false);
            p.rect(100.0, 100.0, 30.0, 20.0);
        }

        // Fresh builder baseline.
        let mut fresh = CanvasPath::new();
        add_geometry(&mut fresh);
        let bounds_fresh = bounds(&fresh);
        let points_fresh = fresh.point_count();
        let verbs_fresh = fresh.verb_count();
        assert!(points_fresh > 0);

        // Pollute a builder, then reset and rebuild.
        let mut reused = CanvasPath::new();
        reused.move_to(999.0, 999.0);
        reused.line_to(-999.0, -999.0);
        reused.reset();
        add_geometry(&mut reused);

        assert_eq!(
            reused.point_count(),
            points_fresh,
            "reused builder must have same point count as fresh builder"
        );
        assert_eq!(
            reused.verb_count(),
            verbs_fresh,
            "reused builder must have same verb count as fresh builder"
        );
        let bounds_reused = bounds(&reused);
        assert!(
            (bounds_reused.left - bounds_fresh.left).abs() < 1e-3,
            "left mismatch: fresh={}, reused={}",
            bounds_fresh.left,
            bounds_reused.left
        );
        assert!((bounds_reused.right - bounds_fresh.right).abs() < 1e-3);
        assert!((bounds_reused.top - bounds_fresh.top).abs() < 1e-3);
        assert!((bounds_reused.bottom - bounds_fresh.bottom).abs() < 1e-3);
    }
}
