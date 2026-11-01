//! Frame times on the context's integer sample grid. Recover only an exact
//! round-trip grid origin; arbitrary caller times keep their fractional origin.
use std::ops::Range;

pub(crate) struct RenderTime {
    origin: f64,
    rate: f64,
    grid: bool,
}

impl RenderTime {
    pub(crate) fn new(time: f64, rate: u32) -> Self {
        let rate = rate.max(1) as f64;
        let frame = (time * rate).round();
        let grid = frame / rate == time;
        Self {
            origin: if grid { frame } else { time },
            rate,
            grid,
        }
    }

    pub(crate) fn at(&self, frame: usize) -> f64 {
        if self.grid {
            (self.origin + frame as f64) / self.rate
        } else {
            self.origin + frame as f64 * (1.0 / self.rate)
        }
    }

    fn first_at(&self, time: f64, frames: usize) -> usize {
        let (mut left, mut right) = (0, frames);
        while left < right {
            let middle = left + (right - left) / 2;
            if self.at(middle) < time {
                left = middle + 1;
            } else {
                right = middle;
            }
        }
        left
    }

    pub(crate) fn active(&self, start: f64, stop: f64, frames: usize) -> Range<usize> {
        let end = self.first_at(stop, frames);
        self.first_at(start, frames).min(end)..end
    }

    pub(crate) fn stopped(&self, stop: f64, frames: usize) -> bool {
        stop <= self.at(frames)
    }
}
