//! Frozen original complete callback kernel; independent native HeapRb ownership.
use crate::output::AudioSync;
use ringbuf::{
    HeapCons,
    traits::{Consumer, Observer},
};

/// Samples converted per pass through the stack scratch buffer.
///
/// 512 f32 is 2 KiB of stack, which every real-time audio callback thread has
/// (AAudio, ALSA and CoreAudio all give their callback threads a stack measured
/// in tens of kilobytes). Chunking is what removes the heap from the callback:
/// the scratch is sized by this constant rather than by whatever the device asks
/// for, so no device buffer size can make the callback allocate.
const CONVERT_CHUNK_SAMPLES: usize = 512;

/// A device sample format the ring's `f32` can be written as.
///
/// A trait rather than three copies of the callback: the conversion is the only
/// difference between the integer formats, and it monomorphises away.
pub(crate) trait FromNormalizedF32: Copy {
    fn from_normalized(sample: f32) -> Self;
}

impl FromNormalizedF32 for i16 {
    #[inline]
    fn from_normalized(sample: f32) -> Self {
        (sample.clamp(-1.0, 1.0) * 32767.0) as Self
    }
}

impl FromNormalizedF32 for u16 {
    #[inline]
    fn from_normalized(sample: f32) -> Self {
        // Centred at 32768, matching the unsigned PCM convention.
        ((sample.clamp(-1.0, 1.0) * 32767.0) + 32768.0) as Self
    }
}

/// Everything the hardware callback does, as a named type rather than a closure.
///
/// **This exists so the real-time region can be measured.** The callback runs on
/// a thread the platform schedules as real-time -- `SCHED_FIFO` on Android -- and
/// an allocation there is not slow, it is a missed deadline heard as a dropout,
/// because the allocator can block behind a thread that is not real-time
/// scheduled at all. The allocation gate enforces this with a test, and a
/// closure handed to `build_output_stream` cannot be reached without a device.
///
/// The type is also what removes the duplication: three near-identical callbacks
/// differing only in the sample conversion collapse into two methods.
pub(crate) struct OutputCallback {
    consumer: HeapCons<f32>,
    sync: AudioSync,
    low_watermark: usize,
}

impl OutputCallback {
    /// Built by a backend, which owns the consumer half of the ring.
    pub(crate) fn new(consumer: HeapCons<f32>, sync: AudioSync, low_watermark: usize) -> Self {
        Self {
            consumer,
            sync,
            low_watermark,
        }
    }

    /// The device speaks the ring's own format: pop straight into its buffer.
    pub(crate) fn render_native(&mut self, data: &mut [f32]) {
        self.sync.observe_callback(data.len());

        let read = self.consumer.pop_slice(data);
        if read < data.len() {
            data[read..].fill(0.0);
            self.sync.record_underrun();
        }

        self.report_depth();
    }

    /// The device speaks an integer format: convert through a fixed stack
    /// scratch, a chunk at a time.
    ///
    /// The scratch used to be a `Vec<f32>` pre-sized to 4096 samples and grown
    /// with `resize` whenever the device asked for more. A device is under no
    /// obligation to request the same number of samples every callback, or a
    /// number this code guessed -- AAudio's `numFrames` varies across route
    /// changes and stream recovery, and cpal's ALSA backend sizes each callback
    /// from the period space actually available -- so that `resize` was a
    /// `realloc` inside a real-time callback. A fixed scratch cannot be grown,
    /// so the failure mode is gone rather than made unlikely.
    pub(crate) fn render_converted<T: FromNormalizedF32>(&mut self, data: &mut [T]) {
        self.sync.observe_callback(data.len());

        let mut scratch = [0.0f32; CONVERT_CHUNK_SAMPLES];
        let mut underrun = false;
        for out in data.chunks_mut(CONVERT_CHUNK_SAMPLES) {
            let scratch = &mut scratch[..out.len()];
            let read = self.consumer.pop_slice(scratch);
            if read < scratch.len() {
                scratch[read..].fill(0.0);
                underrun = true;
            }
            for (dst, &sample) in out.iter_mut().zip(scratch.iter()) {
                *dst = T::from_normalized(sample);
            }
        }

        if underrun {
            self.sync.record_underrun();
        }

        self.report_depth();
    }

    /// Publish the post-callback ring depth and ask for a refill if it is low.
    #[inline]
    fn report_depth(&self) {
        let remaining = self.consumer.occupied_len();
        self.sync.update_level(remaining);

        if remaining <= self.sync.low_watermark(self.low_watermark) {
            self.sync.signal_need_data();
        }
    }
}
