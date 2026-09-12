//! Central limits for audio PCM allocation.
//!
//! Both the WebAudio `createBuffer` path and the `decodeAudioData` decode path
//! must agree on how large a single decoded/allocated PCM buffer may get. This
//! module is the single source of truth so the native side, the decoders, and
//! the JS shims (which mirror these constants) stay consistent — preventing
//! both integer-overflow-sized `createBuffer` requests and decode bombs (a tiny
//! compressed file expanding into gigabytes of PCM).

use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};

use shared::error::{EngineError, EngineResult, ErrorCode};

use crate::decoder::DecodedAudio;

/// Hard ceiling on a single decoded/allocated PCM buffer (interleaved f32).
///
/// 64 MiB is about 2.91 minutes of 48 kHz stereo f32. Longer BGM must use
/// the bounded streaming window; this keeps decode and WebAudio allocation safe.
pub const MAX_AUDIO_PCM_BYTES: u64 = 64 * 1024 * 1024;

/// The same ceiling expressed in interleaved f32 samples (all channels).
pub const MAX_AUDIO_PCM_SAMPLES: u64 = MAX_AUDIO_PCM_BYTES / 4;

/// Aggregate retained PCM ceiling for one AudioContext.
pub const MAX_CONTEXT_RETAINED_PCM_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_CONTEXT_RETAINED_PCM_BUFFERS: usize = 512;

/// Aggregate retained PCM ceiling shared by every AudioContext in the process.
pub const MAX_PROCESS_RETAINED_PCM_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_PROCESS_RETAINED_PCM_BUFFERS: usize = 2048;

/// Default cache capacity, shared with the aggregate ledger.
pub const MAX_AUDIO_CACHE_BYTES: usize = MAX_CONTEXT_RETAINED_PCM_BYTES;

/// Default process-wide physical PCM ceiling.
///
/// This is derived only from existing per-domain ceilings. Hosts that have a
/// tighter memory policy can pass an explicit ledger to the audio components;
/// the native process default remains deterministic and reviewable.
pub const MAX_AUDIO_AGGREGATE_PHYSICAL_BYTES: usize = MAX_PROCESS_RETAINED_PCM_BYTES
    + MAX_CONTEXT_RETAINED_PCM_BYTES
    + (MAX_AUDIO_PCM_BYTES as usize * 2);

/// Maximum channel count accepted for a buffer (Web Audio permits up to 32).
pub const MAX_AUDIO_CHANNELS: u32 = 32;

/// Valid `createBuffer` sample-rate range in Hz (Web Audio: 3000..=768000).
pub const MIN_SAMPLE_RATE: u32 = 3_000;
pub const MAX_SAMPLE_RATE: u32 = 768_000;

/// Validate `createBuffer`-style parameters and the resulting PCM footprint.
///
/// Uses checked arithmetic so an overflowing `length * channels * 4` is rejected
/// with a structured error instead of panicking (debug) or wrapping to a
/// bogus-but-smaller allocation (release).
#[cfg(test)]
pub fn validate_buffer_alloc(channels: u32, length: u32, sample_rate: u32) -> EngineResult<()> {
    validated_buffer_alloc_bytes(channels, length, sample_rate).map(|_| ())
}

/// Validate a createBuffer request and return its retained interleaved PCM bytes.
pub(crate) fn validated_buffer_alloc_bytes(
    channels: u32,
    length: u32,
    sample_rate: u32,
) -> EngineResult<usize> {
    if channels == 0 || channels > MAX_AUDIO_CHANNELS {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!(
                "invalid channel count {} (must be 1..={})",
                channels, MAX_AUDIO_CHANNELS
            ),
        ));
    }
    if sample_rate < MIN_SAMPLE_RATE || sample_rate > MAX_SAMPLE_RATE {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!(
                "invalid sample rate {} (must be {}..={})",
                sample_rate, MIN_SAMPLE_RATE, MAX_SAMPLE_RATE
            ),
        ));
    }
    if length == 0 {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            "buffer length must be >= 1",
        ));
    }

    let bytes = (length as u64)
        .checked_mul(channels as u64)
        .and_then(|samples| samples.checked_mul(4))
        .ok_or_else(|| {
            EngineError::from_detail(ErrorCode::InvalidArgument, "buffer size overflow")
        })?;

    if bytes > MAX_AUDIO_PCM_BYTES {
        return Err(EngineError::from_detail(
            ErrorCode::InvalidArgument,
            format!(
                "buffer of {} bytes exceeds the {} byte PCM budget",
                bytes, MAX_AUDIO_PCM_BYTES
            ),
        ));
    }

    usize::try_from(bytes).map_err(|_| {
        EngineError::from_detail(ErrorCode::InvalidArgument, "buffer size does not fit usize")
    })
}

/// Whether an interleaved sample count (in-progress or final) is within budget.
///
/// Used by the decoders to abort a decode bomb before it grows unbounded.
#[inline]
pub fn pcm_samples_within_budget(sample_count: usize) -> bool {
    sample_count as u64 <= MAX_AUDIO_PCM_SAMPLES
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PcmUsageSnapshot {
    pub bytes: usize,
    pub buffers: usize,
}

/// Process-wide physical PCM ledger shared by every audio allocation domain.
#[derive(Debug)]
pub(crate) struct AudioAggregateLedger {
    max_bytes: usize,
    bytes: AtomicUsize,
}

impl AudioAggregateLedger {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            bytes: AtomicUsize::new(0),
        }
    }

    pub(crate) fn process_global() -> Arc<Self> {
        static PROCESS_LEDGER: LazyLock<Arc<AudioAggregateLedger>> = LazyLock::new(|| {
            Arc::new(AudioAggregateLedger::new(
                MAX_AUDIO_AGGREGATE_PHYSICAL_BYTES,
            ))
        });
        Arc::clone(&PROCESS_LEDGER)
    }

    #[cfg(test)]
    pub(crate) fn used_bytes(&self) -> usize {
        self.bytes.load(Ordering::Acquire)
    }

    pub(crate) fn try_reserve(
        self: &Arc<Self>,
        bytes: usize,
        scope: &'static str,
    ) -> EngineResult<AudioAggregatePermit> {
        self.bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.max_bytes)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    format!("{scope} aggregate physical PCM limit exceeded"),
                )
            })?;
        Ok(AudioAggregatePermit {
            ledger: Arc::clone(self),
            bytes,
        })
    }
}

#[derive(Debug)]
pub(crate) struct AudioAggregatePermit {
    ledger: Arc<AudioAggregateLedger>,
    bytes: usize,
}

impl Drop for AudioAggregatePermit {
    fn drop(&mut self) {
        self.ledger.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl AudioAggregatePermit {
    pub(crate) fn try_grow_to(
        &mut self,
        new_bytes: usize,
        scope: &'static str,
    ) -> EngineResult<()> {
        if new_bytes <= self.bytes {
            return Ok(());
        }
        let additional = new_bytes - self.bytes;
        self.ledger
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(additional)
                    .filter(|next| *next <= self.ledger.max_bytes)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    format!("{scope} aggregate physical PCM limit exceeded"),
                )
            })?;
        self.bytes = new_bytes;
        Ok(())
    }

    pub(crate) fn shrink_to(&mut self, new_bytes: usize) {
        if new_bytes < self.bytes {
            self.ledger
                .bytes
                .fetch_sub(self.bytes - new_bytes, Ordering::AcqRel);
            self.bytes = new_bytes;
        }
    }
}

/// One atomic aggregate counter. Process usage is shared across audio threads;
/// context usage uses the same implementation so permits can outlive the map.
#[derive(Debug)]
pub(crate) struct PcmUsage {
    max_bytes: usize,
    max_buffers: usize,
    bytes: AtomicUsize,
    buffers: AtomicUsize,
}

impl PcmUsage {
    pub(crate) fn new(max_bytes: usize, max_buffers: usize) -> Self {
        Self {
            max_bytes,
            max_buffers,
            bytes: AtomicUsize::new(0),
            buffers: AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> PcmUsageSnapshot {
        PcmUsageSnapshot {
            bytes: self.bytes.load(Ordering::Acquire),
            buffers: self.buffers.load(Ordering::Acquire),
        }
    }

    fn try_reserve(
        self: &Arc<Self>,
        bytes: usize,
        scope: &'static str,
    ) -> EngineResult<PcmUsagePermit> {
        self.buffers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(1).filter(|next| *next <= self.max_buffers)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    format!("{scope} retained PCM buffer count limit exceeded"),
                )
            })?;

        if self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.max_bytes)
            })
            .is_err()
        {
            self.buffers.fetch_sub(1, Ordering::AcqRel);
            return Err(EngineError::from_detail(
                ErrorCode::InputSaturated,
                format!("{scope} retained PCM byte limit exceeded"),
            ));
        }

        Ok(PcmUsagePermit {
            usage: Arc::clone(self),
            bytes,
        })
    }
}

#[derive(Debug)]
struct PcmUsagePermit {
    usage: Arc<PcmUsage>,
    bytes: usize,
}

impl Drop for PcmUsagePermit {
    fn drop(&mut self) {
        self.usage.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.usage.buffers.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PcmUsagePermit {
    fn try_grow_to(&mut self, new_bytes: usize, scope: &'static str) -> EngineResult<()> {
        if new_bytes <= self.bytes {
            return Ok(());
        }
        let additional = new_bytes - self.bytes;
        self.usage
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(additional)
                    .filter(|next| *next <= self.usage.max_bytes)
            })
            .map_err(|_| {
                EngineError::from_detail(
                    ErrorCode::InputSaturated,
                    format!("{scope} retained PCM byte limit exceeded"),
                )
            })?;
        self.bytes = new_bytes;
        Ok(())
    }

    fn shrink_to(&mut self, new_bytes: usize) {
        if new_bytes >= self.bytes {
            return;
        }
        self.usage
            .bytes
            .fetch_sub(self.bytes - new_bytes, Ordering::AcqRel);
        self.bytes = new_bytes;
    }
}

#[derive(Clone)]
pub(crate) struct PcmBudget {
    context: Arc<PcmUsage>,
    process: Arc<PcmUsage>,
    aggregate: Option<Arc<AudioAggregateLedger>>,
}

impl PcmBudget {
    #[cfg(test)]
    pub(crate) fn new(context: Arc<PcmUsage>, process: Arc<PcmUsage>) -> Self {
        Self {
            context,
            process,
            aggregate: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_aggregate(
        context: Arc<PcmUsage>,
        process: Arc<PcmUsage>,
        aggregate: Arc<AudioAggregateLedger>,
    ) -> Self {
        Self {
            context,
            process,
            aggregate: Some(aggregate),
        }
    }

    pub(crate) fn for_context() -> Self {
        static PROCESS_USAGE: OnceLock<Arc<PcmUsage>> = OnceLock::new();
        let process = Arc::clone(PROCESS_USAGE.get_or_init(|| {
            Arc::new(PcmUsage::new(
                MAX_PROCESS_RETAINED_PCM_BYTES,
                MAX_PROCESS_RETAINED_PCM_BUFFERS,
            ))
        }));
        let context = Arc::new(PcmUsage::new(
            MAX_CONTEXT_RETAINED_PCM_BYTES,
            MAX_CONTEXT_RETAINED_PCM_BUFFERS,
        ));
        Self {
            context,
            process,
            aggregate: Some(AudioAggregateLedger::process_global()),
        }
    }

    pub(crate) fn reserve(&self, bytes: usize) -> EngineResult<RetainedPcmPermit> {
        if bytes as u64 > MAX_AUDIO_PCM_BYTES {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                format!(
                    "buffer of {bytes} bytes exceeds the {MAX_AUDIO_PCM_BYTES} byte PCM budget"
                ),
            ));
        }
        let context = self.context.try_reserve(bytes, "AudioContext")?;
        let process = self.process.try_reserve(bytes, "process")?;
        let aggregate = match &self.aggregate {
            Some(ledger) => Some(ledger.try_reserve(bytes, "audio")?),
            None => None,
        };
        Ok(RetainedPcmPermit {
            _context: context,
            _process: process,
            _aggregate: aggregate,
        })
    }
}

#[derive(Debug)]
pub(crate) struct RetainedPcmPermit {
    _context: PcmUsagePermit,
    _process: PcmUsagePermit,
    _aggregate: Option<AudioAggregatePermit>,
}

impl RetainedPcmPermit {
    pub(crate) fn try_grow_to(&mut self, new_bytes: usize) -> EngineResult<()> {
        if new_bytes as u64 > MAX_AUDIO_PCM_BYTES {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                format!(
                    "buffer capacity of {new_bytes} bytes exceeds the {MAX_AUDIO_PCM_BYTES} byte PCM budget"
                ),
            ));
        }
        let previous = self._context.bytes;
        self._context.try_grow_to(new_bytes, "AudioContext")?;
        if let Err(error) = self._process.try_grow_to(new_bytes, "process") {
            self._context.shrink_to(previous);
            return Err(error);
        }
        if let Some(aggregate) = &mut self._aggregate {
            if let Err(error) = aggregate.try_grow_to(new_bytes, "audio") {
                self._context.shrink_to(previous);
                self._process.shrink_to(previous);
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn try_resize_to(&mut self, new_bytes: usize) -> EngineResult<()> {
        if new_bytes >= self._context.bytes {
            return self.try_grow_to(new_bytes);
        }
        self._context.shrink_to(new_bytes);
        self._process.shrink_to(new_bytes);
        if let Some(aggregate) = &mut self._aggregate {
            aggregate.shrink_to(new_bytes);
        }
        Ok(())
    }
}

/// PCM plus its aggregate-budget permits. Every native holder shares this Arc;
/// accounting returns only when the final holder drops it.
pub(crate) struct RetainedAudio {
    audio: DecodedAudio,
    _permit: RetainedPcmPermit,
}

impl RetainedAudio {
    pub(crate) fn try_new(audio: DecodedAudio, budget: &PcmBudget) -> EngineResult<Self> {
        let bytes = pcm_bytes(audio.samples.capacity())?;
        let permit = budget.reserve(bytes)?;
        Ok(Self {
            audio,
            _permit: permit,
        })
    }

    pub(crate) fn from_reserved(audio: DecodedAudio, permit: RetainedPcmPermit) -> Self {
        Self {
            audio,
            _permit: permit,
        }
    }

    pub(crate) fn try_clone_with_budget(&self, budget: &PcmBudget) -> EngineResult<Self> {
        let original_capacity_bytes = pcm_bytes(self.audio.samples.capacity())?;
        let mut permit = budget.reserve(original_capacity_bytes)?;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(self.audio.samples.len())
            .map_err(|_| {
                EngineError::from_detail(ErrorCode::OutOfMemory, "retained PCM allocation failed")
            })?;
        samples.extend_from_slice(&self.audio.samples);
        let cloned_capacity_bytes = pcm_bytes(samples.capacity())?;
        permit.try_resize_to(cloned_capacity_bytes)?;
        Ok(Self::from_reserved(
            DecodedAudio {
                samples,
                sample_rate: self.audio.sample_rate,
                channels: self.audio.channels,
            },
            permit,
        ))
    }

    pub(crate) fn audio_mut(&mut self) -> &mut DecodedAudio {
        &mut self.audio
    }
}

impl Deref for RetainedAudio {
    type Target = DecodedAudio;

    fn deref(&self) -> &Self::Target {
        &self.audio
    }
}

pub(crate) fn pcm_bytes(sample_count: usize) -> EngineResult<usize> {
    sample_count
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| {
            EngineError::from_detail(ErrorCode::InvalidArgument, "PCM sample byte size overflow")
        })
}

pub(crate) fn try_allocate_zeroed_pcm(sample_count: usize) -> EngineResult<Vec<f32>> {
    let mut samples = Vec::new();
    samples.try_reserve_exact(sample_count).map_err(|_| {
        EngineError::from_detail(ErrorCode::OutOfMemory, "retained PCM allocation failed")
    })?;
    samples.resize(sample_count, 0.0);
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn accepts_normal_buffer() {
        // 1 second of 48 kHz stereo — trivially within budget.
        assert!(validate_buffer_alloc(2, 48_000, 48_000).is_ok());
    }

    #[test]
    fn rejects_zero_channels() {
        assert!(validate_buffer_alloc(0, 48_000, 48_000).is_err());
    }

    #[test]
    fn rejects_too_many_channels() {
        assert!(validate_buffer_alloc(MAX_AUDIO_CHANNELS + 1, 48_000, 48_000).is_err());
    }

    #[test]
    fn rejects_zero_length() {
        assert!(validate_buffer_alloc(2, 0, 48_000).is_err());
    }

    #[test]
    fn rejects_bad_sample_rate() {
        assert!(validate_buffer_alloc(2, 100, 0).is_err());
        assert!(validate_buffer_alloc(2, 100, MAX_SAMPLE_RATE + 1).is_err());
    }

    #[test]
    fn rejects_length_channels_overflow() {
        // 8 channels * 600M frames * 4 bytes = 19.2 GB. The naive `(length *
        // channels) as usize` in u32 space overflows; validation must reject.
        assert!(validate_buffer_alloc(8, 600_000_000, 48_000).is_err());
        // Extreme: u32::MAX frames.
        assert!(validate_buffer_alloc(2, u32::MAX, 48_000).is_err());
    }

    #[test]
    fn rejects_over_budget_but_non_overflowing() {
        // 2ch * 200M frames * 4 = 1.6 GB: fits in u64 without overflow but
        // still exceeds the 64 MiB budget.
        assert!(validate_buffer_alloc(2, 200_000_000, 48_000).is_err());
    }

    #[test]
    fn pcm_samples_budget_boundary() {
        assert_eq!(MAX_AUDIO_PCM_BYTES, 64 * 1024 * 1024);
        assert!(pcm_samples_within_budget(MAX_AUDIO_PCM_SAMPLES as usize));
        assert!(!pcm_samples_within_budget(
            MAX_AUDIO_PCM_SAMPLES as usize + 1
        ));
    }

    #[test]
    fn create_buffer_accepts_exact_sixty_four_mib_and_rejects_the_next_f32() {
        let exact_frames = MAX_AUDIO_PCM_SAMPLES as u32;
        assert!(validate_buffer_alloc(1, exact_frames, 48_000).is_ok());
        let error = validate_buffer_alloc(1, exact_frames + 1, 48_000)
            .expect_err("one f32 beyond the 64 MiB PCM ceiling must fail");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn retained_pcm_budget_rolls_back_context_reservation_when_process_is_full() {
        let process = Arc::new(PcmUsage::new(4, 1));
        let first_context = Arc::new(PcmUsage::new(16, 2));
        let second_context = Arc::new(PcmUsage::new(16, 2));
        let first_budget = PcmBudget::new(Arc::clone(&first_context), Arc::clone(&process));
        let second_budget = PcmBudget::new(Arc::clone(&second_context), Arc::clone(&process));
        let _held = first_budget
            .reserve(4)
            .expect("fixture fills process budget");

        let error = second_budget
            .reserve(4)
            .expect_err("process count limit must reject another retained buffer");

        assert_eq!(error.code, ErrorCode::InputSaturated);
        assert_eq!(second_context.snapshot(), PcmUsageSnapshot::default());
        assert_eq!(
            process.snapshot(),
            PcmUsageSnapshot {
                bytes: 4,
                buffers: 1
            }
        );
    }

    #[test]
    fn retained_pcm_permit_releases_exact_bytes_and_count_on_drop() {
        let process = Arc::new(PcmUsage::new(32, 4));
        let context = Arc::new(PcmUsage::new(16, 2));
        let budget = PcmBudget::new(Arc::clone(&context), Arc::clone(&process));

        let permit = budget.reserve(12).expect("within both limits");
        assert_eq!(
            context.snapshot(),
            PcmUsageSnapshot {
                bytes: 12,
                buffers: 1
            }
        );
        assert_eq!(
            process.snapshot(),
            PcmUsageSnapshot {
                bytes: 12,
                buffers: 1
            }
        );

        drop(permit);
        assert_eq!(context.snapshot(), PcmUsageSnapshot::default());
        assert_eq!(process.snapshot(), PcmUsageSnapshot::default());
    }

    #[test]
    fn retained_pcm_growth_rolls_back_context_bytes_when_process_rejects_delta() {
        let process = Arc::new(PcmUsage::new(4, 2));
        let context = Arc::new(PcmUsage::new(8, 2));
        let budget = PcmBudget::new(Arc::clone(&context), Arc::clone(&process));
        let mut permit = budget.reserve(4).unwrap();

        let error = permit
            .try_grow_to(8)
            .expect_err("process byte ceiling must reject the capacity delta");

        assert_eq!(error.code, ErrorCode::InputSaturated);
        assert_eq!(context.snapshot().bytes, 4);
        assert_eq!(process.snapshot().bytes, 4);
        drop(permit);
        assert_eq!(context.snapshot(), PcmUsageSnapshot::default());
        assert_eq!(process.snapshot(), PcmUsageSnapshot::default());
    }
}
