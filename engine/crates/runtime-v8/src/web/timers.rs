// web/timers.rs
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use deno_core::{OpState, op2};
use shared::op_state::HostOpState;
use tokio::time::Instant;

pub struct StartTime(Instant);

impl Default for StartTime {
    /// `performance.now()` counts from the process time origin, the same instant the frame
    /// timestamps handed to `requestAnimationFrame` callbacks count from (`shared::time_origin`),
    /// so the two are on one timeline as they are in a browser.
    fn default() -> Self {
        Self(Instant::from_std(shared::time_origin::process_origin()))
    }
}

impl std::ops::Deref for StartTime {
    type Target = Instant;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Write elapsed time since StartTime into `buf` as:
/// - u32 seconds (little-endian)
/// - u32 subsec_nanos (little-endian)
#[op2(fast)]
pub fn op_now(state: &mut OpState, #[buffer] buf: &mut [u8]) {
    let start_time = state.borrow::<StartTime>();
    let elapsed = start_time.elapsed();

    let seconds = elapsed.as_secs() as u32;
    let subsec_nanos = elapsed.subsec_nanos();

    if buf.len() >= 8 {
        buf[0..4].copy_from_slice(&seconds.to_le_bytes());
        buf[4..8].copy_from_slice(&subsec_nanos.to_le_bytes());
    }
}

/// Return the timer-specific lifecycle level. This is separate from network
/// throttling because foreground timer delivery may wait for a live Surface.
#[op2(fast)]
pub fn op_timer_is_backgrounded(state: &mut OpState) -> bool {
    state
        .borrow::<HostOpState>()
        .timer_backgrounded
        .load(Ordering::Acquire)
}

/// Return current Unix timestamp in microseconds as a f64.
#[op2(fast)]
#[number]
pub fn op_now_us() -> u64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    dur.as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::StartTime;

    /// `performance.now()` and the animation-frame timestamps count from one instant. A session's
    /// `StartTime` made at any later moment still reads the process clock, not a clock of its own:
    /// otherwise a game that began loading before the runtime existed (or a second session in the
    /// same process) would have `performance.now()` and `ts` on different timelines.
    #[test]
    fn a_start_time_counts_from_the_process_origin_not_from_its_own_creation() {
        let first = shared::time_origin::process_origin();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let start = StartTime::default();
        let from_start = start.elapsed().as_secs_f64() * 1000.0;
        let from_origin = shared::time_origin::elapsed_ms();
        assert!(
            from_start >= 19.0,
            "a StartTime made 20 ms after the origin reads {from_start} ms, so it restarted the clock"
        );
        assert!(
            (from_start - from_origin).abs() < 5.0,
            "{from_start} vs {from_origin}"
        );
        assert_eq!(shared::time_origin::process_origin(), first);
    }
}
