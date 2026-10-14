//! The one window measurement a host publishes, and the one service that
//! reports it to content.
//!
//! Two implementations of this rule existed. The C ABI carried a seqlock so a
//! host could re-attach a resized surface and have `migo.getSystemInfoSync()`
//! follow it; the desktop platforms carried a construction-time snapshot that
//! could never be updated, with a comment saying so. Both serialised the same
//! `WindowInfo`, field for field, so the only real difference was that one
//! could change and the other could not — which is the shape of defect this
//! repository keeps finding rather than a design.
//!
//! Content works in CSS pixels, so `pixel_ratio` is what keeps physical and
//! logical apart. Reporting physical pixels as CSS pixels lays a game out at
//! the wrong scale while every pixel still lands exactly where the engine put
//! it, which is why it reads as a content bug rather than a host one.

use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering, fence},
};

use crate::{
    protocol::error::ServiceError,
    services::{SystemInfoService, app_info::HostAppInfo},
    surface::{PixelRatio, SafeArea, WindowInfo},
};

/// One coherent physical-window measurement supplied by a host.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostWindowMetrics {
    width_pixels: u32,
    height_pixels: u32,
    pixel_ratio: PixelRatio,
}

impl HostWindowMetrics {
    #[inline]
    pub const fn new(width_pixels: u32, height_pixels: u32, pixel_ratio: PixelRatio) -> Self {
        Self {
            width_pixels,
            height_pixels,
            pixel_ratio,
        }
    }

    #[inline]
    pub const fn width_pixels(self) -> u32 {
        self.width_pixels
    }

    #[inline]
    pub const fn height_pixels(self) -> u32 {
        self.height_pixels
    }

    #[inline]
    pub const fn pixel_ratio(self) -> PixelRatio {
        self.pixel_ratio
    }
}

/// A single-writer seqlock over the window measurement.
///
/// Surface transitions are serialised by the host's own surface state machine,
/// while JS may query window information concurrently. Atomics avoid taking a
/// lock from V8 and guarantee that width, height and DPR always come from one
/// update rather than from two.
#[derive(Debug)]
pub struct HostWindowState {
    sequence: AtomicU64,
    width_pixels: AtomicU32,
    height_pixels: AtomicU32,
    pixel_ratio_bits: AtomicU32,
}

impl HostWindowState {
    pub fn new(metrics: HostWindowMetrics) -> Self {
        Self {
            sequence: AtomicU64::new(0),
            width_pixels: AtomicU32::new(metrics.width_pixels),
            height_pixels: AtomicU32::new(metrics.height_pixels),
            pixel_ratio_bits: AtomicU32::new(metrics.pixel_ratio.get().to_bits()),
        }
    }

    /// Publish all fields as one logical update and return the old snapshot, so
    /// a caller whose own commit is refused can roll the change back.
    pub fn replace(&self, metrics: HostWindowMetrics) -> HostWindowMetrics {
        let previous = self.snapshot();
        let prior = self.sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(prior & 1, 0, "window updates must be serialized");
        self.width_pixels
            .store(metrics.width_pixels, Ordering::Relaxed);
        self.height_pixels
            .store(metrics.height_pixels, Ordering::Relaxed);
        self.pixel_ratio_bits
            .store(metrics.pixel_ratio.get().to_bits(), Ordering::Relaxed);
        self.sequence.fetch_add(1, Ordering::Release);
        previous
    }

    pub fn snapshot(&self) -> HostWindowMetrics {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let width_pixels = self.width_pixels.load(Ordering::Relaxed);
            let height_pixels = self.height_pixels.load(Ordering::Relaxed);
            let pixel_ratio_bits = self.pixel_ratio_bits.load(Ordering::Relaxed);
            // The validating load only means anything if the three reads above
            // cannot drift past it. An acquire *load* is the wrong tool: it
            // stops later work from being hoisted above itself, while what has
            // to be forbidden here is the opposite direction -- an earlier
            // relaxed load sinking below it, which would let this return a
            // width from one update and a height from the next while the
            // sequence still compared equal. An acquire fence is a load-load
            // barrier over the reads that precede it, which is exactly the
            // `smp_rmb()` in the kernel's `read_seqretry`. x86 cannot reorder
            // two loads, so without this the tear is unobservable there and
            // reachable only on the aarch64 targets (Android, OpenHarmony).
            fence(Ordering::Acquire);
            let after = self.sequence.load(Ordering::Relaxed);
            if before == after {
                // Unreachable by construction rather than by luck: every store
                // to `pixel_ratio_bits` comes from an already-validated
                // `PixelRatio`, and a `u32` store cannot tear, so whichever
                // update this read lands on carries a valid pattern.
                let pixel_ratio = PixelRatio::new(f32::from_bits(pixel_ratio_bits))
                    .expect("HostWindowState stores only validated ratios");
                return HostWindowMetrics {
                    width_pixels,
                    height_pixels,
                    pixel_ratio,
                };
            }
        }
    }
}

/// Reports the window the host described, and what the host says about
/// itself (its id, as `host.appId`), and nothing else.
///
/// Every other `SystemInfoService` method keeps its default: a host-driven
/// platform has no device model, benchmark level or system settings to report
/// here, and inventing plausible ones would be worse than saying so.
#[derive(Debug)]
pub struct HostWindowInfo {
    window: Arc<HostWindowState>,
    app: HostAppInfo,
}

impl HostWindowInfo {
    pub fn new(window: Arc<HostWindowState>) -> Self {
        Self {
            window,
            app: HostAppInfo::default(),
        }
    }

    /// What the host says about itself.
    pub fn with_app(mut self, app: HostAppInfo) -> Self {
        self.app = app;
        self
    }
}

impl SystemInfoService for HostWindowInfo {
    fn host_app_info(&self) -> HostAppInfo {
        self.app.clone()
    }

    fn get_window_info_json(&self) -> Result<String, ServiceError> {
        let metrics = self.window.snapshot();
        let physical_width = metrics.width_pixels as f32;
        let physical_height = metrics.height_pixels as f32;
        // A host-presented window has no status bar and no display cutout, so
        // the safe area is the whole window -- which is what zero insets mean.
        let info = WindowInfo {
            pixel_ratio: metrics.pixel_ratio.get(),
            screen_width: physical_width,
            screen_height: physical_height,
            window_width: physical_width,
            window_height: physical_height,
            status_bar_height: 0.0,
            screen_top: 0.0,
            safe_area: SafeArea {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            },
        }
        .to_logical();
        serde_json::to_string(&info)
            .map_err(|error| ServiceError::system(format!("getWindowInfo:fail serialize: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ratio(value: f32) -> PixelRatio {
        PixelRatio::new(value).expect("test ratio must be valid")
    }

    fn window_info(metrics: HostWindowMetrics) -> serde_json::Value {
        let json = HostWindowInfo::new(Arc::new(HostWindowState::new(metrics)))
            .get_window_info_json()
            .expect("window info must serialise");
        serde_json::from_str(&json).expect("window info must be valid JSON")
    }

    /// The plain case: content must see the window it actually has rather than
    /// the zeroes it saw before any host described one.
    #[test]
    fn a_host_window_is_reported_to_content() {
        let info = window_info(HostWindowMetrics::new(720, 1280, ratio(1.0)));
        assert_eq!(info["window_width"], 720.0);
        assert_eq!(info["window_height"], 1280.0);
        assert_eq!(info["pixel_ratio"], 1.0);
    }

    /// A 2x host presents a 1440x2560 surface and content must be told
    /// 720x1280. Handing it the physical extent is the classic way a game ends
    /// up laid out at the wrong scale.
    #[test]
    fn a_hidpi_window_is_reported_in_css_pixels() {
        let info = window_info(HostWindowMetrics::new(1440, 2560, ratio(2.0)));
        assert_eq!(info["window_width"], 720.0);
        assert_eq!(info["window_height"], 1280.0);
        assert_eq!(info["pixel_ratio"], 2.0);
    }

    /// The screen a desktop game is told about is its own window. Reporting the
    /// desktop's extent would make a windowed game size itself to the display.
    #[test]
    fn the_screen_is_the_window() {
        let info = window_info(HostWindowMetrics::new(800, 600, ratio(1.0)));
        assert_eq!(info["screen_width"], info["window_width"]);
        assert_eq!(info["screen_height"], info["window_height"]);
    }

    /// The property the desktop platforms did not have: a service already handed
    /// to content follows a later host update. Without this, a resized window
    /// keeps reporting its start-up size and content lays itself out for a
    /// window that is no longer there.
    #[test]
    fn a_published_update_is_visible_through_a_service_already_created() {
        let state = Arc::new(HostWindowState::new(HostWindowMetrics::new(
            720,
            1280,
            ratio(1.0),
        )));
        let service = HostWindowInfo::new(Arc::clone(&state));

        let previous = state.replace(HostWindowMetrics::new(1000, 700, ratio(1.0)));
        assert_eq!(previous, HostWindowMetrics::new(720, 1280, ratio(1.0)));

        let json = service
            .get_window_info_json()
            .expect("window info must serialise");
        let info: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(info["window_width"], 1000.0);
        assert_eq!(info["window_height"], 700.0);
    }

    /// A reader must never combine fields from two different updates.
    #[test]
    fn readers_never_observe_fields_from_different_updates() {
        let state = Arc::new(HostWindowState::new(HostWindowMetrics::new(
            2,
            4,
            ratio(1.0),
        )));
        let writer_state = Arc::clone(&state);
        let writer = std::thread::spawn(move || {
            for step in 0..20_000u32 {
                let side = 2 + (step % 8) * 2;
                writer_state.replace(HostWindowMetrics::new(side, side * 2, ratio(1.0)));
            }
        });

        for _ in 0..20_000 {
            let seen = state.snapshot();
            assert_eq!(
                seen.height_pixels(),
                seen.width_pixels() * 2,
                "width and height came from different updates"
            );
        }
        writer.join().expect("writer thread");
    }
}
