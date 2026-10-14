//! The output when there is no device to play to: the mix is consumed at the rate a device would consume it, and
//! not heard.
//!
//! A machine with no output device -- a headless server, a VM, a desktop with its output unplugged -- is not a game
//! whose audio failed. The reference platform makes an AudioContext there like anywhere else; its time advances,
//! its sources end when they would, an InnerAudioContext reaches `ended`. Refusing to start the audio thread instead
//! made every audio object a game asked for an `AudioError` and stopped games that never meant to be heard.
//!
//! No thread stands in for the device. The audio thread already wakes on a short tick while there is something to
//! hear, so it calls [`SilentOutput::pump`] once per pass, and the pump hands the device's callback -- the same
//! [`OutputCallback`] a device runs, so the ring, the watermarks and the refill signal behave exactly as with one --
//! however much time has passed since the last pass. The scratch it renders into is a fixed stack buffer: like the
//! device callback, the steady state reaches no heap.

use std::time::Instant;

use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::{HeapProd, HeapRb};

use super::{
    AudioSync, CONVERT_CHUNK_SAMPLES, HIGH_WATERMARK_FRAMES, LOW_WATERMARK_FRAMES, OutputCallback,
    RING_BUFFER_FRAMES,
};

/// The rate the mix runs at with no device to ask: the common output rate, so a device that appears later is most
/// likely to take the same one and the contexts need not renegotiate.
pub(crate) const SILENT_SAMPLE_RATE: u32 = 48_000;
pub(crate) const SILENT_CHANNELS: u32 = 2;

/// Most frames one pump consumes. A device that is not serviced for longer than its ring lasts does not catch up
/// afterwards -- the time it missed is an underrun -- and neither does this.
const MAX_FRAMES_PER_PUMP: u64 = RING_BUFFER_FRAMES as u64;

pub struct SilentOutput {
    producer: HeapProd<f32>,
    callback: OutputCallback,
    sync: AudioSync,
    low_watermark_samples: usize,
    high_watermark_samples: usize,
    /// When consumption started, or `None` while paused: a pause stops the clock as it stops a device's callback.
    started: Option<Instant>,
    /// Frames consumed since `started`. Consumption is measured from one origin rather than pass to pass, so the
    /// rounding of each pass does not accumulate into drift.
    consumed: u64,
}

impl SilentOutput {
    pub fn new() -> Self {
        let channels = SILENT_CHANNELS as usize;
        let (producer, consumer) = HeapRb::<f32>::new(RING_BUFFER_FRAMES * channels).split();
        let sync = AudioSync::new();
        let low_watermark_samples = LOW_WATERMARK_FRAMES * channels;
        Self {
            producer,
            callback: OutputCallback::new(consumer, sync.clone(), low_watermark_samples),
            sync,
            low_watermark_samples,
            high_watermark_samples: HIGH_WATERMARK_FRAMES * channels,
            started: None,
            consumed: 0,
        }
    }

    #[inline]
    pub fn sample_rate(&self) -> u32 {
        SILENT_SAMPLE_RATE
    }

    #[inline]
    pub fn channels(&self) -> u32 {
        SILENT_CHANNELS
    }

    /// Consume what a device would have consumed since the last pump.
    pub fn pump(&mut self) {
        self.pump_until(Instant::now());
    }

    /// [`pump`](Self::pump) at a given moment, which is what lets a test drive the clock.
    pub(crate) fn pump_until(&mut self, now: Instant) {
        let Some(started) = self.started else {
            return;
        };
        let elapsed = now.saturating_duration_since(started).as_nanos();
        let due = u64::try_from(elapsed * u128::from(SILENT_SAMPLE_RATE) / 1_000_000_000)
            .unwrap_or(u64::MAX);
        let mut frames = due.saturating_sub(self.consumed);
        if frames > MAX_FRAMES_PER_PUMP {
            self.consumed = due - MAX_FRAMES_PER_PUMP;
            frames = MAX_FRAMES_PER_PUMP;
        }
        self.consumed += frames;

        let mut scratch = [0.0f32; CONVERT_CHUNK_SAMPLES];
        let mut samples = frames as usize * SILENT_CHANNELS as usize;
        while samples > 0 {
            let chunk = samples.min(CONVERT_CHUNK_SAMPLES);
            self.callback.render_native(&mut scratch[..chunk]);
            samples -= chunk;
        }
    }

    #[inline]
    pub fn write(&mut self, samples: &[f32]) -> usize {
        self.producer.push_slice(samples)
    }

    #[inline]
    pub fn available(&self) -> usize {
        self.producer.vacant_len()
    }

    #[inline]
    pub fn buffered(&self) -> usize {
        self.producer.occupied_len()
    }

    #[inline]
    pub fn high_watermark(&self) -> usize {
        self.high_watermark_samples
            .max(self.sync.max_callback().saturating_mul(2))
    }

    #[inline]
    pub fn needs_data(&self) -> bool {
        self.sync.buffer_level() < self.low_watermark_samples
    }

    #[inline]
    pub fn sync(&self) -> &AudioSync {
        &self.sync
    }

    pub fn pause_stream(&mut self) -> bool {
        self.started = None;
        self.consumed = 0;
        true
    }

    /// Starts the clock at once, as a device's stream starts calling back when it is played.
    pub fn resume_stream(&mut self) -> bool {
        self.resume_at(Instant::now());
        true
    }

    pub(crate) fn resume_at(&mut self, now: Instant) {
        if self.started.is_none() {
            self.started = Some(now);
            self.consumed = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use std::time::Duration;

    fn filled(output: &mut SilentOutput, samples: usize) {
        let block = vec![0.5f32; samples];
        assert_eq!(output.write(&block), samples, "the fixture fits the ring");
    }

    /// Consumed at the device's rate, measured from the start: ten milliseconds is 480 frames of stereo.
    #[test]
    fn time_consumes_the_mix_at_the_device_rate() {
        let mut output = SilentOutput::new();
        filled(&mut output, 8192);
        let start = Instant::now();
        output.resume_at(start);

        output.pump_until(start + Duration::from_millis(10));
        assert_eq!(output.buffered(), 8192 - 960);
        output.pump_until(start + Duration::from_millis(20));
        assert_eq!(output.buffered(), 8192 - 1920);
    }

    /// Paused, the clock stands still, and nothing is consumed while it does; resumed, it counts from the resume.
    #[test]
    fn a_paused_output_consumes_nothing() {
        let mut output = SilentOutput::new();
        filled(&mut output, 4096);
        output.pump_until(Instant::now() + Duration::from_secs(1));
        assert_eq!(
            output.buffered(),
            4096,
            "an output never started consumes nothing"
        );

        let start = Instant::now();
        output.resume_at(start);
        output.pump_until(start + Duration::from_millis(5));
        assert_eq!(output.buffered(), 4096 - 480);
        output.pause_stream();
        output.pump_until(start + Duration::from_secs(5));
        assert_eq!(output.buffered(), 4096 - 480);

        let again = start + Duration::from_secs(6);
        output.resume_at(again);
        output.pump_until(again + Duration::from_millis(5));
        assert_eq!(output.buffered(), 4096 - 960);
    }

    /// Below the low watermark the pump asks for a refill, as the device callback does: the audio thread's refill is
    /// driven by the same signal either way.
    #[test]
    fn a_drained_ring_asks_for_a_refill() {
        let mut output = SilentOutput::new();
        filled(&mut output, 4096);
        let start = Instant::now();
        output.resume_at(start);
        assert!(!output.sync().check_and_clear());

        output.pump_until(start + Duration::from_millis(40));
        assert!(output.needs_data());
        assert!(output.sync().check_and_clear());
    }

    /// A pump after a long stall consumes at most a ring's worth: the missed time is an underrun, not a backlog to
    /// burn through.
    #[test]
    fn a_long_stall_is_an_underrun_not_a_backlog() {
        let mut output = SilentOutput::new();
        let start = Instant::now();
        output.resume_at(start);
        let stalled = start + Duration::from_secs(60);
        output.pump_until(stalled);
        filled(&mut output, 4096);
        output.pump_until(stalled + Duration::from_millis(5));
        assert_eq!(
            output.buffered(),
            4096 - 480,
            "after a stall the next pass consumes only the time since it"
        );
    }

    /// The pump stands where a device callback does, so it is held to the same rule: no heap in the steady state.
    #[test]
    fn a_steady_state_pump_never_reaches_the_heap() {
        let mut output = SilentOutput::new();
        let refill = vec![0.25f32; 960];
        let start = Instant::now();
        output.resume_at(start);
        assert_no_steady_state_allocation(
            Burst {
                path: "audio: one silent-output pump and the refill after it",
                warmup: 8,
                measured: 64,
            },
            |iteration| {
                output.pump_until(start + Duration::from_millis(10 * (iteration as u64 + 1)));
                output.write(&refill);
            },
        );
    }
}
