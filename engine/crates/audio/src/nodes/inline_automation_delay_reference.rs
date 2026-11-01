//! Test-only frozen implementation before bounded stack automation.
use std::any::Any;

use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::protocol::audio_cmd::AudioNodeId;

use crate::limits::{AudioAggregateLedger, AudioAggregatePermit};
use crate::param::AudioParamTimeline;

use super::AudioNodeProcessor;

/// DelayNode: fractional-delay circular buffer.
///
/// Delays the input by `delayTime`, which is an a-rate parameter: it is evaluated
/// per sample, and the read position is interpolated between the two nearest taps.
/// Both halves are needed for the flanging the API exists to support. Rounding the
/// delay to whole samples and sampling the parameter once per block turned a glide
/// into a staircase -- each step a discontinuity in the delay line, heard as
/// zipper noise rather than a sweep.
pub struct DelayNode {
    id: AudioNodeId,
    delay_time: AudioParamTimeline,
    /// Circular buffer, interleaved, `frames * channels` samples.
    buffer: Vec<f32>,
    /// Aggregate permit held for the circular buffer's physical bytes.
    _delay_permit: AudioAggregatePermit,
    /// Write position, in frames.
    write_frame: usize,
    /// Frames in the circular buffer.
    frames: usize,
    /// Maximum delay in seconds (set at creation time)
    max_delay: f32,
    channels: usize,
    /// Reusable per-sample automation values, like `GainNode`'s.
    automation: Vec<f32>,
}

impl DelayNode {
    pub fn new(id: AudioNodeId, max_delay_time: f32, sample_rate: u32, channels: u32) -> Self {
        Self::new_with_aggregate(
            id,
            max_delay_time,
            sample_rate,
            channels,
            &AudioAggregateLedger::process_global(),
        )
        .expect("process-wide audio aggregate ledger rejected delay buffer")
    }

    pub(crate) fn new_with_aggregate(
        id: AudioNodeId,
        max_delay_time: f32,
        sample_rate: u32,
        channels: u32,
        aggregate: &std::sync::Arc<AudioAggregateLedger>,
    ) -> EngineResult<Self> {
        // Per-node delay-buffer memory cap. Web Audio allows maxDelayTime up to
        // 180s, which at 48kHz stereo f32 is ~68MB for a single node; cap the
        // allocation and shrink max_delay to fit the budget.
        const MAX_DELAY_BYTES: usize = 16 * 1024 * 1024;
        let ch = channels.max(1) as usize;
        let requested = max_delay_time.max(0.001);
        let requested = requested.min(179.0); // Web Audio spec max
        let bytes_per_sec = sample_rate as f64 * ch as f64 * std::mem::size_of::<f32>() as f64;
        let budget_secs = (MAX_DELAY_BYTES as f64 / bytes_per_sec.max(1.0)) as f32;
        let max_delay = requested.min(budget_secs);
        if max_delay < requested {
            tracing::warn!(
                "DelayNode {}: maxDelayTime {:.1}s exceeds the {}MB per-node budget; clamped to {:.1}s",
                id,
                requested,
                MAX_DELAY_BYTES / (1024 * 1024),
                max_delay
            );
        }
        // One frame of headroom so the interpolator's second tap at the maximum
        // delay is still inside the buffer rather than wrapping onto the sample
        // just written.
        let frames = (max_delay as f64 * sample_rate as f64) as usize + 2;
        let buffer_samples = frames.checked_mul(ch).ok_or_else(|| {
            EngineError::from_detail(ErrorCode::InvalidArgument, "delay buffer size overflow")
        })?;
        let buffer_bytes = buffer_samples
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| {
                EngineError::from_detail(ErrorCode::InvalidArgument, "delay buffer size overflow")
            })?;
        let delay_permit = aggregate.try_reserve(buffer_bytes, "delay")?;
        Ok(Self {
            id,
            delay_time: AudioParamTimeline::new(0.0, 0.0, max_delay),
            buffer: vec![0.0; buffer_samples],
            _delay_permit: delay_permit,
            write_frame: 0,
            frames,
            max_delay,
            channels: ch,
            automation: Vec::with_capacity(crate::audio_thread::RENDER_QUANTUM_FRAMES),
        })
    }

    /// Read channel `ch` a fractional number of frames behind the write cursor.
    #[inline]
    fn read_interpolated(&self, delay_frames: f32, ch: usize) -> f32 {
        let whole = delay_frames.floor();
        let frac = delay_frames - whole;
        let back = whole as usize;
        // `write_frame` still holds the sample written this iteration, so a delay
        // of zero reads it straight back.
        let tap0 = (self.write_frame + self.frames - back % self.frames) % self.frames;
        let tap1 = (tap0 + self.frames - 1) % self.frames;
        let a = self.buffer[tap0 * self.channels + ch];
        let b = self.buffer[tap1 * self.channels + ch];
        a + (b - a) * frac
    }
}

impl AudioNodeProcessor for DelayNode {
    fn id(&self) -> AudioNodeId {
        self.id
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn process(
        &mut self,
        inputs: &[f32],
        output: &mut [f32],
        sample_rate: u32,
        _channels: u32,
        current_time: f64,
    ) -> usize {
        let channels = self.channels;
        if self.buffer.is_empty() || channels == 0 {
            return 0;
        }
        // An effect remains renderable after its source is pruned: zero input
        // must advance the delay line so buffered samples can drain instead of
        // being cut off at the source's end.
        let frames = if inputs.is_empty() {
            output.len() / channels
        } else {
            inputs.len().min(output.len()) / channels
        };
        if frames == 0 {
            return 0;
        }
        let len = frames * channels;

        // a-rate: one value per frame. The maximum is one frame short of the
        // buffer so the interpolator's older tap cannot wrap past the newest
        // sample.
        let max_delay_frames = (self.frames - 2) as f32;
        if self.automation.len() < frames {
            self.automation.resize(frames, 0.0);
        }
        self.delay_time
            .compute_values(current_time, &mut self.automation[..frames], sample_rate);

        let sr = sample_rate.max(1) as f32;
        for frame in 0..frames {
            let delay_frames = (self.automation[frame].clamp(0.0, self.max_delay) * sr)
                .clamp(0.0, max_delay_frames);
            let base = frame * channels;
            let write_base = self.write_frame * channels;
            for ch in 0..channels {
                self.buffer[write_base + ch] = inputs.get(base + ch).copied().unwrap_or(0.0);
            }
            for ch in 0..channels {
                output[base + ch] = self.read_interpolated(delay_frames, ch);
            }

            self.write_frame = (self.write_frame + 1) % self.frames;
        }

        len / channels
    }

    fn has_tail_audio(&self) -> bool {
        self.buffer.iter().any(|sample| sample.abs() > 1e-10)
    }

    fn get_param_mut(&mut self, name: &str) -> Option<&mut AudioParamTimeline> {
        match name {
            "delayTime" => Some(&mut self.delay_time),
            _ => None,
        }
    }
}
