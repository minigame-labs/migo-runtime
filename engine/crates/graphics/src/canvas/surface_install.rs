use crate::surface_binding::RecreateKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallPolicy {
    Skip,
    FastResize,
    FullRecreate,
}

/// Who chose an onscreen canvas's backing-store size.
///
/// The engine derives a default from the surface for a canvas the content never
/// sized, so that default has to be re-derived every time the surface changes; a
/// size the content chose is the content's and the engine must never move it.
/// `canvas.width = N` is what promotes a canvas to [`Self::Content`], and it is a
/// latch -- a surface change cannot take a canvas back.
///
/// The JS half records the same promotion (`_sizedByContent` in
/// `web/03_canvas.js`) to decide whether it may adopt a new size, and the two
/// have to agree: `op_get_canvas_info` reports the backing store, so a render
/// side that moved a content-owned buffer would leave the content drawing in
/// coordinates its own canvas no longer has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackingSizeOwner {
    Engine,
    Content,
}

/// The backing store the engine gives an onscreen canvas the content has not
/// sized, in CSS pixels.
///
/// Logical rather than physical: a DPR-naive engine (Pixi/Phaser created with
/// resolution 1) sizes its GL viewport to the logical window, so a logical
/// backing is filled and the swap-time blit upscales it to the surface — exactly
/// how a browser CSS-scales a logical canvas to fill the display — instead of
/// leaving it rendering into a corner of an over-sized physical buffer. A
/// DPR-aware engine (Cocos) sets `canvas.width = logical * dpr`, which takes
/// ownership and restores the physical, bypass-eligible size.
///
/// One function for one rule: a fresh install, a same-surface resize and a
/// surface recreate all have to answer this identically, and the recreate path
/// answering it differently is what left a rotated canvas describing the surface
/// the app was suspended on.
pub(crate) fn engine_default_backing(surface: (u32, u32), pixel_ratio: f32) -> (u32, u32) {
    let (width, height) = surface;
    if pixel_ratio > 1.0 {
        (
            ((width as f32 / pixel_ratio).round() as u32).max(1),
            ((height as f32 / pixel_ratio).round() as u32).max(1),
        )
    } else {
        (width.max(1), height.max(1))
    }
}

/// The buffer a fixed-size window surface should have, given the onscreen
/// canvas's DrawingBuffer and the window it fills.
///
/// The DrawingBuffer when it fits inside the window, and the window otherwise.
/// Every present fills the whole window with the whole canvas, stretched, so a
/// buffer the canvas's size shows exactly what a window-sized one does once the
/// compositor scales it -- which is how a browser composites a canvas -- while
/// the copy into it becomes 1:1 (or, under bypass, disappears) and the window
/// system's drawables shrink with it. A DPR-naive game's canvas is a ninth of a
/// 3x screen, so the three drawables of a triple-buffered layer shrink ninefold:
/// 38 MiB to 4 on an iPhone XS Max.
///
/// A canvas larger than the window in either direction keeps the window's size:
/// a bigger buffer shows no more pixels and costs more than the downscaling
/// blit. No DrawingBuffer means content draws straight into the window, so the
/// window's size is the only one that exists.
pub(crate) fn window_buffer_size(
    drawing_buffer: Option<(u32, u32)>,
    window: (u32, u32),
) -> (u32, u32) {
    match drawing_buffer {
        Some((width, height)) if width <= window.0 && height <= window.1 => (width, height),
        _ => window,
    }
}

/// Pure cold-path policy. Reuse is permitted only when backend-specific native
/// equivalence has already been proven and no generation/force boundary
/// requires a fresh EGLSurface.
pub(crate) fn classify_surface_install(
    kind: RecreateKind,
    native_equivalent: bool,
    installed_size: Option<(u32, u32)>,
    requested_size: Option<(u32, u32)>,
    has_onscreen_2d: bool,
    owes_2d_restore: bool,
    force_recreate: bool,
) -> InstallPolicy {
    if force_recreate
        || kind != RecreateKind::SameGeneration
        || !native_equivalent
        || installed_size.is_none()
        || requested_size.is_none()
    {
        return InstallPolicy::FullRecreate;
    }

    if installed_size == requested_size {
        // Skipping is only safe when the install owes nothing. A canvas whose
        // 2D context was torn down by an earlier event still looks
        // same-window/same-size, and skipping leaves it without a context for
        // the rest of the session -- the content holds the object it got from
        // `getContext('2d')` and never asks again, so nothing would rebuild it.
        //
        // `owes_2d_restore` rather than `!has_onscreen_2d`: a WebGL-only canvas
        // has no 2D context and never will, and must keep taking this fast
        // path. The distinction is "never had one" versus "had one and lost
        // it", which is exactly what the absent-context check cannot make.
        if owes_2d_restore {
            InstallPolicy::FullRecreate
        } else {
            InstallPolicy::Skip
        }
    } else if has_onscreen_2d {
        InstallPolicy::FastResize
    } else {
        InstallPolicy::FullRecreate
    }
}

#[cfg(test)]
mod tests {
    use crate::surface_binding::RecreateKind;

    use super::{
        InstallPolicy, classify_surface_install, engine_default_backing, window_buffer_size,
    };

    /// A canvas that fits takes the buffer at its own size -- the DPR-naive
    /// case, a ninth of a 3x screen, and the one the rule exists for -- down to a
    /// canvas matching the window exactly, which is the same answer either way.
    #[test]
    fn a_canvas_that_fits_the_window_is_the_buffer() {
        assert_eq!(
            window_buffer_size(Some((414, 896)), (1242, 2688)),
            (414, 896)
        );
        assert_eq!(
            window_buffer_size(Some((960, 640)), (2688, 1242)),
            (960, 640)
        );
        assert_eq!(
            window_buffer_size(Some((1242, 2688)), (1242, 2688)),
            (1242, 2688)
        );
        assert_eq!(window_buffer_size(Some((1, 1)), (1242, 2688)), (1, 1));
    }

    /// Larger than the window in either direction keeps the window: a bigger
    /// buffer shows no more pixels. One axis over is enough, since the present
    /// stretches the whole canvas over the whole window regardless of aspect.
    #[test]
    fn a_canvas_larger_than_the_window_in_either_direction_keeps_the_window() {
        assert_eq!(
            window_buffer_size(Some((2484, 5376)), (1242, 2688)),
            (1242, 2688)
        );
        assert_eq!(
            window_buffer_size(Some((1280, 720)), (1242, 2688)),
            (1242, 2688)
        );
        assert_eq!(
            window_buffer_size(Some((720, 2700)), (1242, 2688)),
            (1242, 2688)
        );
    }

    /// No DrawingBuffer means content draws into the window itself, so there is
    /// no other size to take.
    #[test]
    fn without_a_drawing_buffer_the_buffer_is_the_window() {
        assert_eq!(window_buffer_size(None, (1242, 2688)), (1242, 2688));
    }

    /// The engine's own default is the surface in CSS pixels.
    ///
    /// The rounding matters rather than being incidental: 1080/2.75 is 392.72, and
    /// content comparing `canvas.width` against `getSystemInfoSync().windowWidth`
    /// only ever agrees if both round the same ratio the same way.
    #[test]
    fn the_engine_default_backing_is_the_surface_in_css_pixels() {
        assert_eq!(engine_default_backing((1080, 2340), 2.75), (393, 851));
        assert_eq!(engine_default_backing((2204, 1080), 2.75), (801, 393));
    }

    /// At or below one device pixel per CSS pixel the surface *is* the default.
    ///
    /// Dividing anyway would inflate the buffer above the surface on a ratio below
    /// 1, which no display asks for and every blit would then downscale.
    #[test]
    fn a_ratio_of_one_or_less_leaves_the_backing_at_the_surface() {
        assert_eq!(engine_default_backing((1000, 700), 1.0), (1000, 700));
        assert_eq!(engine_default_backing((1000, 700), 0.5), (1000, 700));
    }

    /// No canvas may be zero in either dimension: a zero-sized FBO is
    /// incomplete, and the ratio can round a thin surface to nothing.
    #[test]
    fn the_default_backing_is_never_zero() {
        assert_eq!(engine_default_backing((1, 1), 4.0), (1, 1));
        assert_eq!(engine_default_backing((0, 0), 1.0), (1, 1));
    }

    #[test]
    fn surface_install_policy_preserves_fast_paths_without_cross_generation_reuse() {
        let old_size = (1080, 1920);
        let new_size = (1920, 1080);

        assert_eq!(
            classify_surface_install(
                RecreateKind::SameGeneration,
                true,
                Some(old_size),
                Some(old_size),
                true,
                false,
                false,
            ),
            InstallPolicy::Skip,
        );
        assert_eq!(
            classify_surface_install(
                RecreateKind::SameGeneration,
                true,
                Some(old_size),
                Some(new_size),
                true,
                false,
                false,
            ),
            InstallPolicy::FastResize,
        );
        assert_eq!(
            classify_surface_install(
                RecreateKind::NewGeneration,
                true,
                Some(old_size),
                Some(old_size),
                true,
                false,
                false,
            ),
            InstallPolicy::FullRecreate,
        );
    }

    #[test]
    fn surface_install_policy_fails_to_full_recreate_when_reuse_is_not_proven() {
        let size = (1080, 1920);
        for (kind, equivalent, has_2d, force) in [
            (RecreateKind::Initial, false, false, false),
            (RecreateKind::SameGeneration, false, true, false),
            (RecreateKind::SameGeneration, true, false, false),
            (RecreateKind::SameGeneration, true, true, true),
        ] {
            assert_eq!(
                classify_surface_install(
                    kind,
                    equivalent,
                    Some(size),
                    Some((size.0 + 1, size.1)),
                    has_2d,
                    false,
                    force,
                ),
                InstallPolicy::FullRecreate,
            );
        }
    }

    /// An outstanding 2D restore forbids the same-size fast path.
    ///
    /// A canvas whose 2D context was torn down by an earlier event still looks
    /// same-window and same-size, so every other input says "skip". Skipping
    /// leaves it with no context for the rest of the session: the content holds
    /// the object `getContext('2d')` gave it and never asks again, so nothing
    /// would ever rebuild it. Measured cost when this went wrong on a real
    /// surface path: ~900k `2d context not found` per 8s, frozen picture, game
    /// still running at 60fps.
    #[test]
    fn an_outstanding_2d_restore_forbids_the_same_size_fast_path() {
        let size = (1080, 1920);
        assert_eq!(
            classify_surface_install(
                RecreateKind::SameGeneration,
                true,
                Some(size),
                Some(size),
                false,
                true,
                false,
            ),
            InstallPolicy::FullRecreate,
            "an install that owes a 2D restore must not skip",
        );
    }

    /// ...and a canvas that never had a 2D context still takes it.
    ///
    /// This is the distinction the absent-context check cannot make: "never had
    /// one" (WebGL-only content, the common case) versus "had one and lost it".
    /// Keying the skip on `!has_onscreen_2d` instead of the outstanding
    /// obligation would put every WebGL resume through a full recreate.
    #[test]
    fn a_canvas_that_never_had_a_2d_context_still_takes_the_fast_path() {
        let size = (1080, 1920);
        assert_eq!(
            classify_surface_install(
                RecreateKind::SameGeneration,
                true,
                Some(size),
                Some(size),
                false,
                false,
                false,
            ),
            InstallPolicy::Skip,
            "WebGL-only content owes nothing and must keep the fast path",
        );
    }
}
