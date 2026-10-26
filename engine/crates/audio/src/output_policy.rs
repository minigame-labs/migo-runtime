//! Bounded software-ring reserve. Hardware buffering remains the backend's
//! responsibility (for example Oboe LatencyTuner). Only a missing software
//! sample increases this reserve; idle/startup zero-fill does not.
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{RENDER_QUANTUM_FRAMES, RING_BUFFER_FRAMES};

pub(super) struct RefillPolicy {
    channels: usize,
    hint_frames: usize,
    reserve_bursts: AtomicUsize,
    low_samples: AtomicUsize,
    high_samples: AtomicUsize,
}

impl RefillPolicy {
    pub(super) fn new(channels: usize, hint_frames: usize, observed_samples: usize) -> Self {
        assert!(channels > 0);
        let policy = Self {
            channels,
            hint_frames,
            reserve_bursts: AtomicUsize::new(2),
            low_samples: AtomicUsize::new(0),
            high_samples: AtomicUsize::new(0),
        };
        policy.refresh(observed_samples, false);
        policy
    }

    /// Single audio owner only. Route replacement constructs a fresh policy;
    /// do not oscillate the reserve down while a stream remains open.
    pub(super) fn refresh(&self, observed_samples: usize, underrun: bool) {
        let reserve = self.reserve_bursts.load(Ordering::Relaxed);
        let reserve = if underrun {
            (reserve + 1).min(8)
        } else {
            reserve
        };
        self.reserve_bursts.store(reserve, Ordering::Relaxed);
        let burst = observed_samples
            .div_ceil(self.channels)
            .max(self.hint_frames)
            .clamp(RENDER_QUANTUM_FRAMES, RING_BUFFER_FRAMES);
        let target = burst.saturating_mul(reserve);
        let high = target
            .div_ceil(RENDER_QUANTUM_FRAMES)
            .saturating_mul(RENDER_QUANTUM_FRAMES)
            .min(RING_BUFFER_FRAMES);
        let low = burst
            .saturating_mul(reserve - 1)
            .min(high - RENDER_QUANTUM_FRAMES);
        // The callback only needs the independent low threshold. All changes
        // are monotonic for a stream; no mixed multi-field snapshot is needed.
        self.low_samples
            .store(low * self.channels, Ordering::Relaxed);
        self.high_samples
            .store(high * self.channels, Ordering::Relaxed);
    }

    #[inline]
    pub(super) fn low(&self) -> usize {
        self.low_samples.load(Ordering::Relaxed)
    }

    #[inline]
    pub(super) fn high(&self) -> usize {
        self.high_samples.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_burst_and_observed_callback_control_reserve_not_sample_rate() {
        let policy = RefillPolicy::new(2, 192, 0);
        assert_eq!((policy.low(), policy.high()), (192 * 2, 384 * 2));
        policy.refresh(480 * 2, false);
        assert_eq!((policy.low(), policy.high()), (480 * 2, 1024 * 2));
        let observed_at_open = RefillPolicy::new(2, 0, 480 * 2);
        assert_eq!(observed_at_open.high(), policy.high());
    }

    #[test]
    fn software_miss_adds_reserve_and_a_new_route_resets_it() {
        let policy = RefillPolicy::new(2, 128, 0);
        for bursts in 3..=8 {
            policy.refresh(256, true);
            assert_eq!(policy.high(), bursts * 256);
            assert_eq!(policy.low(), (bursts - 1) * 256);
        }
        for _ in 0..1000 {
            policy.refresh(256, true);
        }
        assert_eq!(policy.high(), 8 * 256);
        assert_eq!(RefillPolicy::new(2, 128, 0).high(), 2 * 256);
    }

    #[test]
    fn unknown_odd_and_hostile_hints_keep_targets_reachable_and_bounded() {
        for channels in [1, 2, 6, 8] {
            for hint in [0, 1, 127, 128, 192, 480, 8192, usize::MAX] {
                for observed in [0, 1, 255, 4097, usize::MAX] {
                    let policy = RefillPolicy::new(channels, hint, observed);
                    for _ in 0..10 {
                        assert!(policy.low() < policy.high());
                        assert!(policy.high() <= RING_BUFFER_FRAMES * channels);
                        assert_eq!(policy.high() % (RENDER_QUANTUM_FRAMES * channels), 0);
                        assert!(policy.high() - policy.low() >= RENDER_QUANTUM_FRAMES * channels);
                        policy.refresh(observed, true);
                    }
                }
            }
        }
    }
}
