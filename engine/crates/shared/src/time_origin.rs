//! The one instant that `performance.now()` and every `requestAnimationFrame` timestamp count from.
//!
//! A browser gives content a single time origin: `performance.now()`, the timestamp handed to an animation
//! frame callback and `performance.timeOrigin` are on one timeline, so `ts - performance.now()` means
//! something and a game that stamps `performance.now()` at load and subtracts it from the first frame's
//! timestamp gets a small positive number. The runtime had two: `performance.now()` counted from the moment
//! the JavaScript runtime was created, and the frame timestamp from the first vsync the display delivered
//! (or from the render thread's start where there is no display clock). The first frame of a game that had
//! spent two seconds loading read `ts = 0` against `performance.now() = 2000`, so a loop that began with
//! `dt = ts - performance.now()` had a negative first step and an animation timeline anchored to
//! `performance.now()` started two seconds behind.
//!
//! The origin is per process, not per session: the clocks only have to agree with each other, and a
//! second session in one process sees `performance.now()` continue from where the first left off, which a
//! monotonic clock allows.

use std::sync::OnceLock;
use std::time::Instant;

static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// The origin, fixed the first time anything asks. Whatever starts first -- the render thread or the
/// JavaScript runtime -- fixes it, so a timestamp is never before it.
pub fn process_origin() -> Instant {
    *ORIGIN.get_or_init(Instant::now)
}

/// Milliseconds from the origin to `at`, saturating at zero for an instant before it.
pub fn ms_since_origin(at: Instant) -> f64 {
    at.saturating_duration_since(process_origin()).as_secs_f64() * 1000.0
}

/// Milliseconds from the origin to now.
pub fn elapsed_ms() -> f64 {
    ms_since_origin(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_origin_is_fixed_once_and_the_clock_runs_from_it() {
        let first = process_origin();
        assert_eq!(
            process_origin(),
            first,
            "a second ask must not move the origin"
        );
        let a = elapsed_ms();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = elapsed_ms();
        assert!(a >= 0.0 && b - a >= 4.0, "{a} then {b}");
    }

    #[test]
    fn an_instant_before_the_origin_is_zero_not_a_panic() {
        let origin = process_origin();
        let before = origin
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or(origin);
        assert_eq!(ms_since_origin(before), 0.0);
    }

    #[test]
    fn every_thread_sees_the_same_origin() {
        let here = process_origin();
        let there = std::thread::spawn(process_origin).join().unwrap();
        assert_eq!(here, there);
    }
}
