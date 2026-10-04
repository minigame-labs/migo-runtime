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

/// Milliseconds from the origin to `nanos`, a reading of the platform's monotonic clock -- the clock a host's
/// frame timestamps are on (`migo_session_notify_vsync`): `CLOCK_MONOTONIC` on Android, Linux and OpenHarmony
/// (Choreographer's frame time, `steady_clock`), the uptime clock on Apple platforms (`CACurrentMediaTime`, a
/// display link's timestamps), the performance counter on Windows.
///
/// Converted through how long ago the reading was, taken against a reading of the same clock now, so the host's
/// clock and the one `Instant` reads only have to run at the same rate, not share a zero. A reading from the future
/// is now: a frame cannot have begun after it was reported. One from before the origin is zero, as
/// [`ms_since_origin`] has it.
pub fn ms_since_origin_of_monotonic_nanos(nanos: i64) -> f64 {
    let reported = Instant::now();
    let ago = monotonic_nanos().saturating_sub(nanos).max(0) as u64;
    ms_since_origin(
        reported
            .checked_sub(std::time::Duration::from_nanos(ago))
            .unwrap_or_else(process_origin),
    )
}

/// Now, on the platform's monotonic clock (see [`ms_since_origin_of_monotonic_nanos`]), in nanoseconds.
pub fn monotonic_nanos() -> i64 {
    #[cfg(unix)]
    {
        // The uptime clock on Apple platforms: mach_absolute_time in nanoseconds, which `CACurrentMediaTime` reads
        // in seconds.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `now` is a valid timespec for the call to fill, and the clock exists on the platform.
        unsafe { libc::clock_gettime(CLOCK, &mut now) };
        now.tv_sec as i64 * 1_000_000_000 + now.tv_nsec as i64
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };
        let (mut count, mut frequency) = (0i64, 0i64);
        // SAFETY: both write one i64 through a valid pointer, and cannot fail on any Windows that runs this.
        unsafe {
            QueryPerformanceCounter(&mut count);
            QueryPerformanceFrequency(&mut frequency);
        }
        (count as i128 * 1_000_000_000 / frequency.max(1) as i128) as i64
    }
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

    /// A frame timestamp on the platform's monotonic clock lands where it happened on the process timeline: one
    /// read now is now, one from 30 ms ago is 30 ms ago, one from the future is now, and one from before the origin
    /// is zero.
    #[test]
    fn a_monotonic_reading_lands_where_it_happened() {
        let _ = process_origin();
        std::thread::sleep(std::time::Duration::from_millis(40));
        let now = monotonic_nanos();
        let before = elapsed_ms();
        let at_now = ms_since_origin_of_monotonic_nanos(now);
        let after = elapsed_ms();
        assert!(
            before - 1.0 <= at_now && at_now <= after,
            "{before} <= {at_now} <= {after}"
        );

        let earlier = ms_since_origin_of_monotonic_nanos(now - 30_000_000);
        assert!(
            (at_now - earlier - 30.0).abs() < 1.0,
            "{at_now} - {earlier}"
        );

        let ahead = ms_since_origin_of_monotonic_nanos(monotonic_nanos() + 25_000_000);
        assert!(
            ahead <= elapsed_ms(),
            "a reading from the future is not after now: {ahead}"
        );

        assert_eq!(ms_since_origin_of_monotonic_nanos(0), 0.0);
    }

    /// The clock `Instant` reads and the one hosts report frames on run together: two readings of each, a while
    /// apart, measure the same interval.
    #[test]
    fn the_monotonic_clock_runs_with_instant() {
        let (instant, raw) = (Instant::now(), monotonic_nanos());
        std::thread::sleep(std::time::Duration::from_millis(50));
        let (instant_elapsed, raw_elapsed) =
            (instant.elapsed().as_nanos() as i64, monotonic_nanos() - raw);
        assert!(
            (instant_elapsed - raw_elapsed).abs() < 2_000_000,
            "{instant_elapsed} vs {raw_elapsed}"
        );
    }

    #[test]
    fn every_thread_sees_the_same_origin() {
        let here = process_origin();
        let there = std::thread::spawn(process_origin).join().unwrap();
        assert_eq!(here, there);
    }
}
