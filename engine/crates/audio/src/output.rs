//! Audio output: the ring buffer, the real-time render logic, and one device backend.
//!
//! The device half is per platform and the render half is not, so the render half lives
//! here and each backend uses it. Two copies of a real-time callback is how one of them
//! silently stops matching the allocation gates at the bottom of this file -- and
//! OpenHarmony needed a second backend only because its userspace is musl with no ALSA
//! and cpal has no OHAudio support, not because anything about the buffering differs.

use shared::channel::{AudioNotifier, ThreadWakeup};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use ringbuf::HeapCons;
use ringbuf::traits::{Consumer, Observer};

/// The device backend for this target. Selected on `target_env` rather than a feature,
/// because which audio API exists is a property of the platform and not a choice a
/// caller gets to make.
#[cfg(all(target_os = "linux", target_env = "ohos"))]
#[path = "output_ohaudio.rs"]
mod backend;
#[cfg(target_os = "android")]
#[path = "output_oboe.rs"]
mod backend;
#[cfg(not(any(target_os = "android", all(target_os = "linux", target_env = "ohos"))))]
#[path = "output_cpal.rs"]
mod backend;
#[path = "output_policy.rs"]
mod policy;
#[path = "output_silent.rs"]
mod silent;

#[cfg(all(test, target_os = "linux", not(target_env = "ohos")))]
#[path = "output_benchmarks.rs"]
mod benchmarks;

use backend::AudioOutput as DeviceOutput;
pub use silent::SilentOutput;

/// Where the mix goes: the platform's output device, or -- with none to open -- [`SilentOutput`], which consumes it
/// at a device's rate and plays nothing.
///
/// So an output always exists. A machine with no device still runs the game's audio as the reference platform
/// does -- its time advances, its sources end, it is only not heard -- and the audio thread, while there is something
/// to hear, keeps looking for a device and moves the mix to one when it appears.
pub enum AudioOutput {
    Device(DeviceOutput),
    Silent(SilentOutput),
}

macro_rules! each_output {
    ($self:expr, $output:ident => $body:expr) => {
        match $self {
            AudioOutput::Device($output) => $body,
            AudioOutput::Silent($output) => $body,
        }
    };
}

impl AudioOutput {
    /// The default device, or silence when none opens.
    pub fn new() -> Self {
        match Self::open_device() {
            Ok(output) => output,
            Err(error) => {
                tracing::warn!(
                    "no audio output device ({error}); the mix plays into silence until one appears"
                );
                // Running from the start, as a device's stream is once opened.
                let mut silent = SilentOutput::new();
                silent.resume_stream();
                Self::Silent(silent)
            }
        }
    }

    /// The default device, and nothing else: what the audio thread tries while it plays into silence.
    pub fn open_device() -> shared::error::EngineResult<Self> {
        DeviceOutput::new().map(Self::Device)
    }

    /// SilentOutput pumps on this owner thread; attaching a notifier there
    /// would create a self-wakeup loop. Device recovery calls this again for
    /// the replacement stream, whose sync state has a fresh lifetime.
    pub fn bind_wakeup(&self, wakeup: &ThreadWakeup) {
        if let Self::Device(device) = self {
            device.sync().bind_wakeup(wakeup);
        }
    }

    #[inline]
    pub fn is_silent(&self) -> bool {
        matches!(self, Self::Silent(_))
    }

    /// Once per pass of the audio thread: silence consumes the time that passed; a device consumes on its own.
    #[inline]
    pub fn pump(&mut self) {
        if let Self::Silent(silent) = self {
            silent.pump();
        }
    }

    #[inline]
    pub fn sample_rate(&self) -> u32 {
        each_output!(self, output => output.sample_rate())
    }

    #[inline]
    pub fn channels(&self) -> u32 {
        each_output!(self, output => output.channels())
    }

    #[inline]
    pub fn write(&mut self, samples: &[f32]) -> usize {
        each_output!(self, output => output.write(samples))
    }

    #[inline]
    pub fn available(&self) -> usize {
        each_output!(self, output => output.available())
    }

    #[inline]
    pub fn buffered(&self) -> usize {
        each_output!(self, output => output.buffered())
    }

    #[inline]
    pub fn high_watermark(&self) -> usize {
        each_output!(self, output => output.high_watermark())
    }

    #[inline]
    pub fn needs_data(&self) -> bool {
        each_output!(self, output => output.needs_data())
    }

    #[inline]
    pub fn sync(&self) -> &AudioSync {
        each_output!(self, output => output.sync())
    }

    /// Silence never fails; a device can.
    #[inline]
    pub fn is_alive(&self) -> bool {
        match self {
            Self::Device(device) => device.is_alive(),
            Self::Silent(_) => true,
        }
    }

    #[cfg(target_os = "ios")]
    pub fn resume_interrupted_stream(&self) {
        if let Self::Device(device) = self {
            device.resume_interrupted_stream();
        }
    }

    pub fn pause_stream(&mut self) -> bool {
        each_output!(self, output => output.pause_stream())
    }

    pub fn resume_stream(&mut self) -> bool {
        each_output!(self, output => output.resume_stream())
    }
}

/// Ring buffer size in sample frames (per channel)
/// 8192 frames at 48kHz = ~170ms buffer
pub(crate) const RING_BUFFER_FRAMES: usize = 8192;

/// Web Audio graph quantum. Device bursts size the reserve, while submissions
/// remain aligned to this independent graph scheduling boundary.
pub(crate) const RENDER_QUANTUM_FRAMES: usize = 128;

/// Low watermark - when buffer falls below this, signal to refill
/// 2048 frames at 48kHz = ~42ms
pub(crate) const LOW_WATERMARK_FRAMES: usize = 2048;

/// High watermark - the refill *target*. A refill pass fills only up to here,
/// not to the top of the ring, so a freshly triggered sound isn't queued behind
/// ~150ms of already-buffered audio. 4096 frames at 48kHz = ~85ms, which still
/// leaves 2x the low-watermark margin against underruns.
pub(crate) const HIGH_WATERMARK_FRAMES: usize = 4096;

/// Lightweight signaling for callback-driven audio.
/// Uses atomic flag instead of Condvar for lower overhead.
///
/// # Atomic ordering model
///
/// Atomic fields serve different synchronization purposes and intentionally
/// use different memory orderings:
///
/// - **`needs_data`** (`Release` on store, `AcqRel` on swap): This flag
///   establishes a happens-before relationship between the audio callback
///   (producer of the signal) and the audio thread (consumer).  When the
///   callback stores `true` with `Release`, all preceding ring buffer reads
///   (pop_slice, occupied_len) are visible to the audio thread after it
///   observes `true` via the `AcqRel` swap in `check_and_clear`.  This
///   ensures the audio thread sees the correct buffer state before refilling.
///
/// - **`buffer_level`** (`Relaxed`): This is a best-effort hint used for
///   monitoring and the `needs_data()` heuristic on `AudioOutput`.  It does
///   not guard any shared data -- the ring buffer itself is lock-free and
///   the producer/consumer halves are on separate threads.  A slightly stale
///   value is harmless (worst case: one extra or one missed refill cycle).
///   Using `Relaxed` avoids unnecessary memory fences on the hot audio
///   callback path.
///
/// - **`stream_error`** (`Release` on store, `Acquire` on load): The error
///   callback stores `true` with `Release` to ensure the error state is
///   visible to the audio thread checking `is_alive()` with `Acquire`.
#[derive(Clone)]
pub struct AudioSync {
    inner: Arc<AudioSyncState>,
}

struct AudioSyncState {
    needs_data: AtomicBool,
    buffer_level: AtomicUsize,
    max_callback: AtomicUsize,
    callback_wakeup_enabled: AtomicBool,
    notifier: OnceLock<AudioNotifier>,
    policy: OnceLock<policy::RefillPolicy>,
    software_underrun: AtomicBool,
    software_underruns: AtomicUsize,
}

impl AudioSync {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(AudioSyncState {
                needs_data: AtomicBool::new(false),
                buffer_level: AtomicUsize::new(0),
                max_callback: AtomicUsize::new(0),
                callback_wakeup_enabled: AtomicBool::new(false),
                notifier: OnceLock::new(),
                policy: OnceLock::new(),
                software_underrun: AtomicBool::new(false),
                software_underruns: AtomicUsize::new(0),
            }),
        }
    }

    pub(super) fn configure_device(&self, channels: usize, hint_frames: usize) {
        let policy = policy::RefillPolicy::new(channels, hint_frames, self.max_callback());
        assert!(
            self.inner.policy.set(policy).is_ok(),
            "one policy per device route"
        );
    }

    pub(crate) fn refresh_device_policy(&self) {
        if let Some(policy) = self.inner.policy.get() {
            let underrun = self.inner.software_underrun.swap(false, Ordering::Acquire);
            let previous = policy.high();
            policy.refresh(self.max_callback(), underrun);
            if underrun && policy.high() > previous {
                // Repeated misses at the cap do not log. Formatting remains
                // on the owner, never on the device callback.
                tracing::debug!(
                    software_underruns = self.software_underrun_count(),
                    target_samples = policy.high(),
                    "audio software ring reserve increased"
                );
            }
        }
    }

    pub(crate) fn software_underrun_count(&self) -> usize {
        self.inner.software_underruns.load(Ordering::Relaxed)
    }

    fn device_high_watermark(&self) -> usize {
        self.inner
            .policy
            .get()
            .expect("configured device policy")
            .high()
    }

    fn low_watermark(&self, fallback: usize) -> usize {
        self.inner
            .policy
            .get()
            .map_or(fallback, policy::RefillPolicy::low)
    }

    fn record_underrun(&self) {
        if self.inner.callback_wakeup_enabled.load(Ordering::Acquire) {
            self.inner
                .software_underruns
                .fetch_add(1, Ordering::Relaxed);
            self.inner.software_underrun.store(true, Ordering::Release);
        }
    }

    /// Install only on device output, on the control thread. A callback that
    /// ran earlier retained its needs_data hint and never initialized a parker.
    fn bind_wakeup(&self, wakeup: &ThreadWakeup) {
        if let Some(notifier) = wakeup.audio_notifier() {
            let _ = self.inner.notifier.set(notifier);
        }
    }

    /// Inactive/silent hardware must not wake the producer on every zero-fill.
    pub(crate) fn set_callback_wakeup_enabled(&self, enabled: bool) {
        self.inner
            .callback_wakeup_enabled
            .store(enabled, Ordering::Release);
    }

    /// Signal that buffer needs data (called from callback)
    #[inline]
    fn signal_need_data(&self) {
        self.inner.needs_data.store(true, Ordering::Release);
        if self.inner.callback_wakeup_enabled.load(Ordering::Acquire)
            && let Some(notifier) = self.inner.notifier.get()
        {
            notifier.notify();
        }
    }

    /// Check and clear the needs_data flag
    #[inline]
    pub fn check_and_clear(&self) -> bool {
        self.inner.needs_data.swap(false, Ordering::AcqRel)
    }

    #[inline]
    fn update_level(&self, level: usize) {
        self.inner.buffer_level.store(level, Ordering::Relaxed);
    }

    /// Get current buffer fill level
    #[inline]
    pub fn buffer_level(&self) -> usize {
        self.inner.buffer_level.load(Ordering::Relaxed)
    }

    /// Record the sample count of a hardware callback request (running max).
    #[inline]
    fn observe_callback(&self, len: usize) {
        self.inner.max_callback.fetch_max(len, Ordering::Relaxed);
    }

    /// Largest hardware callback request seen so far (samples); 0 until the first callback.
    #[inline]
    pub fn max_callback(&self) -> usize {
        self.inner.max_callback.load(Ordering::Relaxed)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use ringbuf::traits::{Producer, Split};
    use ringbuf::{HeapProd, HeapRb};

    const WARMUP: usize = 8;
    const MEASURED: usize = 64;

    /// Deliberately larger than the 4096-sample conversion buffer this callback
    /// used to pre-size, so a device that asks for more than the code guessed is
    /// what the gates run against.
    const CALLBACK_SAMPLES: usize = 8192;

    /// A callback wired to its own ring, filled, with the producer kept alive.
    ///
    /// The producer is returned rather than dropped because a dropped producer
    /// closes the ring, and a gate that measured a closed ring would be measuring
    /// the wrong thing.
    fn filled_callback(ring_samples: usize) -> (HeapProd<f32>, OutputCallback) {
        let (mut producer, consumer) = HeapRb::<f32>::new(ring_samples).split();
        let block = vec![0.25f32; ring_samples];
        producer.push_slice(&block);
        (
            producer,
            OutputCallback::new(consumer, AudioSync::new(), ring_samples / 4),
        )
    }

    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    #[test]
    #[ignore = "explicit LLVM RTSan native callback contract"]
    fn native_realtime_callback_matrix() {
        let probe = migo_alloc_probe::RealtimeProbe::load().unwrap();
        let case = std::env::var("MIGO_RTSAN_CASE").unwrap();
        let wake = ThreadWakeup::new_audio();
        let (_producer, mut native) = filled_callback(128);
        let (_converted_producer, mut converted) = filled_callback(128);
        native.sync.configure_device(1, 128);
        converted.sync.configure_device(1, 128);
        native.sync.bind_wakeup(&wake);
        converted.sync.bind_wakeup(&wake);
        native.sync.set_callback_wakeup_enabled(true);
        converted.sync.set_callback_wakeup_enabled(true);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let consumer = if case.starts_with("parked-") {
            let waiter = wake.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let consumer = std::thread::spawn(move || {
                waiter.bind_audio_thread();
                ready_tx
                    .send(std::fs::read_link("/proc/thread-self").unwrap())
                    .unwrap();
                waiter.wait();
                done_tx.send(()).unwrap();
            });
            let task = ready_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            // Observe the actual kernel wait instead of guessing how much
            // sleep suffices on an overloaded CI runner. The independent
            // trace must additionally observe its FUTEX_WAKE slow path.
            let wchan = std::path::Path::new("/proc").join(task).join("wchan");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !std::fs::read_to_string(&wchan).unwrap().contains("futex") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "consumer never parked"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Some(consumer)
        } else {
            assert_eq!(case, "cold-callbacks");
            wake.bind_audio_thread();
            None
        };
        migo_alloc_probe::on_native_thread(move || {
            let mut native_out = [0.0f32; 128];
            let mut converted_out = [0i16; 1025];
            probe.run_traced(|| {
                if case == "parked-converted-callback" {
                    converted.render_converted(&mut converted_out);
                    converted.render_converted(&mut converted_out);
                } else {
                    native.render_native(&mut native_out);
                    native.render_native(&mut native_out);
                    converted.render_converted(&mut converted_out);
                }
            });
        })
        .unwrap();
        if let Some(consumer) = consumer {
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            consumer.join().unwrap();
        } else {
            assert!(wake.wait_timeout(std::time::Duration::ZERO));
        }
    }

    #[test]
    fn idle_zero_fill_does_not_grow_reserve_and_active_misses_are_coalesced() {
        let (_producer, mut callback) = filled_callback(256);
        callback.sync.configure_device(2, 128);
        let mut out = [0.0f32; 256];
        callback.render_native(&mut out);
        for _ in 0..10 {
            callback.render_native(&mut out);
            callback.sync.refresh_device_policy();
        }
        assert_eq!(callback.sync.device_high_watermark(), 512);
        assert_eq!(callback.sync.software_underrun_count(), 0);
        callback.sync.set_callback_wakeup_enabled(true);
        for _ in 0..10 {
            callback.render_native(&mut out);
        }
        callback.sync.refresh_device_policy();
        assert_eq!(callback.sync.device_high_watermark(), 768);
        assert_eq!(callback.sync.software_underrun_count(), 10);
        callback.sync.refresh_device_policy();
        assert_eq!(callback.sync.device_high_watermark(), 768);
    }

    #[test]
    fn converted_callback_grows_reserve_once_even_across_scratch_chunks() {
        let (_producer, mut callback) = filled_callback(128);
        callback.sync.configure_device(1, 1025);
        callback.sync.set_callback_wakeup_enabled(true);
        let mut out = [0i16; 1025];
        callback.render_converted(&mut out);
        assert_eq!(out[128..], [0; 1025 - 128]);
        callback.sync.refresh_device_policy();
        assert_eq!(callback.sync.device_high_watermark(), 3200);
        assert_eq!(callback.sync.software_underrun_count(), 1);
    }

    #[test]
    fn exact_one_burst_remaining_requests_refill_before_ring_is_empty() {
        let wake = ThreadWakeup::new_audio();
        wake.bind_audio_thread();
        let (_producer, mut callback) = filled_callback(512);
        callback.sync.configure_device(2, 128);
        callback.sync.bind_wakeup(&wake);
        callback.sync.set_callback_wakeup_enabled(true);
        callback.render_native(&mut [0.0; 256]);
        assert_eq!(callback.sync.buffer_level(), 256);
        assert!(wake.wait_timeout(std::time::Duration::ZERO));
    }

    #[test]
    fn device_callback_wakes_only_for_active_low_watermark_work() {
        let wake = ThreadWakeup::new_audio();
        wake.bind_audio_thread();
        let (mut producer, mut callback) = filled_callback(512);
        callback.sync.bind_wakeup(&wake);
        let mut out = [0.0f32; 512];
        callback.render_native(&mut out); // Disabled during initial/idle output.
        assert!(callback.sync.check_and_clear());
        assert!(!wake.wait_timeout(std::time::Duration::ZERO));
        callback.sync.set_callback_wakeup_enabled(true);
        producer.push_slice(&out);
        callback.render_native(&mut out[..128]); // Still above low watermark.
        assert!(!wake.wait_timeout(std::time::Duration::ZERO));
        callback.render_native(&mut out);
        assert!(wake.wait_timeout(std::time::Duration::ZERO));
        assert!(callback.sync.check_and_clear());
        callback.sync.set_callback_wakeup_enabled(false);
        callback.render_native(&mut out);
        assert!(!wake.wait_timeout(std::time::Duration::ZERO));
    }

    #[test]
    fn binding_after_an_early_callback_keeps_the_refill_hint() {
        let wake = ThreadWakeup::new_audio();
        wake.bind_audio_thread();
        let (_producer, mut callback) = filled_callback(128);
        let mut out = [0.0f32; 128];
        callback.render_native(&mut out);
        callback.sync.bind_wakeup(&wake);
        callback.sync.set_callback_wakeup_enabled(true);
        assert!(callback.sync.check_and_clear());
        callback.render_native(&mut out);
        assert!(wake.wait_timeout(std::time::Duration::ZERO));
    }

    #[test]
    fn silent_pumping_does_not_create_a_self_wakeup_loop() {
        let wake = ThreadWakeup::new_audio();
        wake.bind_audio_thread();
        let mut output = AudioOutput::Silent(SilentOutput::new());
        output.bind_wakeup(&wake);
        output.sync().set_callback_wakeup_enabled(true);
        let now = std::time::Instant::now();
        if let AudioOutput::Silent(silent) = &mut output {
            silent.resume_at(now);
            silent.pump_until(now + std::time::Duration::from_millis(10));
        }
        assert!(
            output.sync().check_and_clear(),
            "the silent render path was exercised"
        );
        assert!(
            !wake.wait_timeout(std::time::Duration::ZERO),
            "a silent pump already runs on the producer and must not wake itself"
        );
    }

    #[test]
    fn first_native_and_converted_callbacks_with_notification_never_touch_the_heap() {
        let mut fleet: Vec<_> = (0..WARMUP + MEASURED)
            .map(|_| {
                let native_wake = ThreadWakeup::new_audio();
                let converted_wake = ThreadWakeup::new_audio();
                native_wake.bind_audio_thread();
                converted_wake.bind_audio_thread();
                let (native_producer, native) = filled_callback(CALLBACK_SAMPLES);
                let (converted_producer, converted) = filled_callback(CALLBACK_SAMPLES);
                native.sync.bind_wakeup(&native_wake);
                converted.sync.bind_wakeup(&converted_wake);
                native.sync.set_callback_wakeup_enabled(true);
                converted.sync.set_callback_wakeup_enabled(true);
                (
                    native_wake,
                    converted_wake,
                    native_producer,
                    converted_producer,
                    native,
                    converted,
                )
            })
            .collect();
        let fleet = migo_alloc_probe::on_native_thread(move || {
            let mut native_out = vec![0.0f32; CALLBACK_SAMPLES];
            let mut converted_out = vec![0i16; CALLBACK_SAMPLES];
            assert_no_steady_state_allocation(
                Burst {
                    path: "cold native/converted callback including audio wake notification",
                    warmup: WARMUP,
                    measured: MEASURED,
                },
                |index| {
                    fleet[index].4.render_native(&mut native_out);
                    fleet[index].5.render_converted(&mut converted_out);
                },
            );
            fleet
        })
        .unwrap();
        // Only the registered owner may consume the pending notification.
        for (native, converted, ..) in &fleet {
            assert!(native.wait_timeout(std::time::Duration::ZERO));
            assert!(converted.wait_timeout(std::time::Duration::ZERO));
        }
    }

    /// The steady-state allocation gate, on the hardware callback.
    ///
    /// One iteration is one device callback plus the refill that keeps the ring
    /// from draining, which is the pair that actually repeats forever. Both
    /// device shapes are covered in the same burst: the native `f32` path pops
    /// straight into the device buffer, the integer path converts, and only the
    /// second one ever owned a buffer.
    #[test]
    fn a_steady_state_output_callback_never_reaches_the_heap() {
        let ring_samples = CALLBACK_SAMPLES * 2;
        let (mut native_producer, mut native) = filled_callback(ring_samples);
        let (mut converted_producer, mut converted) = filled_callback(ring_samples);

        // The reservoir is built before the measured window, so the burst body
        // does not take from a pool it does not control.
        let refill = vec![0.25f32; CALLBACK_SAMPLES];
        let mut native_out = vec![0.0f32; CALLBACK_SAMPLES];
        let mut converted_out = vec![0i16; CALLBACK_SAMPLES];

        assert_no_steady_state_allocation(
            Burst {
                path: "audio: one hardware output callback, native and converted",
                warmup: WARMUP,
                measured: MEASURED,
            },
            |_| {
                native.render_native(&mut native_out);
                native_producer.push_slice(&refill);

                converted.render_converted(&mut converted_out);
                converted_producer.push_slice(&refill);
            },
        );
    }

    /// The same property, but from cold — which is the one that was broken.
    ///
    /// A steady-state burst cannot see this defect, and saying why is the point:
    /// the conversion buffer only ever grew, so the one `realloc` a device buffer
    /// larger than 4096 samples caused happened during the warm-up and the
    /// measured window was clean. That is not an acceptable answer here. This
    /// callback runs on a real-time-scheduled thread, where the first call is as
    /// deadline-bound as the ten-thousandth, and stream recovery and route
    /// changes rebuild it with a buffer size nobody chose.
    ///
    /// So every iteration gets a callback that has never run, all of them built
    /// before the measured window. That needs no new mechanism: a burst over a
    /// fleet of one-shot callbacks measures first calls exactly.
    #[test]
    fn an_output_callback_never_reaches_the_heap_on_its_very_first_call() {
        let mut fleet: Vec<(HeapProd<f32>, OutputCallback)> = (0..WARMUP + MEASURED)
            .map(|_| filled_callback(CALLBACK_SAMPLES))
            .collect();
        let mut out = vec![0i16; CALLBACK_SAMPLES];

        assert_no_steady_state_allocation(
            Burst {
                path: "audio: a hardware output callback's first call, at a device buffer size \
                       larger than any the callback pre-sized for",
                warmup: WARMUP,
                measured: MEASURED,
            },
            |iteration| {
                fleet[iteration].1.render_converted(&mut out);
            },
        );
    }

    /// The conversion must still be the conversion. A chunked scratch that
    /// silenced or reordered samples would pass both gates above and be inaudible
    /// to them, so the boundary between chunks is asserted directly.
    #[test]
    fn chunked_conversion_matches_the_ring_across_chunk_boundaries() {
        // Not a multiple of the chunk size, so the final partial chunk is covered.
        let samples = CONVERT_CHUNK_SAMPLES * 2 + 7;
        let source: Vec<f32> = (0..samples)
            .map(|i| (i as f32 / samples as f32) * 2.0 - 1.0)
            .collect();

        let (mut producer, consumer) = HeapRb::<f32>::new(samples).split();
        producer.push_slice(&source);
        let mut callback = OutputCallback::new(consumer, AudioSync::new(), 0);

        let mut out = vec![0i16; samples];
        callback.render_converted(&mut out);

        for (index, (&want, &got)) in source.iter().zip(out.iter()).enumerate() {
            assert_eq!(
                got,
                i16::from_normalized(want),
                "sample {index} converted wrong across a chunk boundary"
            );
        }
    }

    /// A callback handed more than the ring holds must pad with silence rather
    /// than leave the device buffer at whatever it contained.
    #[test]
    fn an_underrun_pads_the_rest_of_the_device_buffer_with_silence() {
        let available = CONVERT_CHUNK_SAMPLES + 3;
        let (mut producer, consumer) = HeapRb::<f32>::new(available).split();
        producer.push_slice(&vec![1.0f32; available]);
        let mut callback = OutputCallback::new(consumer, AudioSync::new(), usize::MAX);

        let mut out = vec![0xAAu16.wrapping_mul(3); available * 2];
        callback.render_converted(&mut out);

        assert!(
            out[..available]
                .iter()
                .all(|&s| s == u16::from_normalized(1.0)),
            "the buffered samples must reach the device buffer"
        );
        assert!(
            out[available..]
                .iter()
                .all(|&s| s == u16::from_normalized(0.0)),
            "an underrun must pad with silence, not leave stale device samples"
        );
        assert!(
            callback.sync.check_and_clear(),
            "an underrun must ask the audio thread for a refill"
        );
    }
}
