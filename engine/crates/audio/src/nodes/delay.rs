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
        let requested = max_delay_time.max(0.001).min(179.0); // Web Audio spec max
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
            automation: Vec::new(),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn run(node: &mut DelayNode, input: &[f32], sample_rate: u32) -> Vec<f32> {
        let mut out = vec![0.0f32; input.len()];
        node.process(input, &mut out, sample_rate, 1, 0.0);
        out
    }

    #[test]
    fn a_zero_delay_passes_the_input_through() {
        let mut node = DelayNode::new(1, 0.1, 48_000, 1);
        let input: Vec<f32> = (0..16).map(|i| i as f32).collect();
        assert_eq!(run(&mut node, &input, 48_000), input);
    }

    #[test]
    fn a_whole_sample_delay_shifts_by_exactly_that_many_frames() {
        let sample_rate = 48_000;
        let mut node = DelayNode::new(1, 0.1, sample_rate, 1);
        // Three frames of delay. Exact in seconds is not exact in f32, so the
        // impulse may straddle two taps by a rounding epsilon.
        node.delay_time.set_value(3.0 / sample_rate as f32);

        let mut input = vec![0.0f32; 8];
        input[0] = 1.0;
        let out = run(&mut node, &input, sample_rate);

        assert!(
            (out[3] - 1.0).abs() < 1e-4,
            "the impulse must land three frames later: {out:?}"
        );
        let elsewhere: f32 = out
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 3)
            .map(|(_, s)| s.abs())
            .sum();
        assert!(elsewhere < 1e-4, "and nowhere else: {out:?}");
    }

    /// Rounding the delay to whole samples made a half-sample delay identical to
    /// either its floor or its ceiling, which is what turned a delayTime sweep into
    /// a staircase. A fractional delay must land between the two.
    #[test]
    fn a_fractional_delay_interpolates_between_taps() {
        let sample_rate = 48_000;
        let mut node = DelayNode::new(1, 0.1, sample_rate, 1);
        node.delay_time.set_value(1.5 / sample_rate as f32);

        let mut input = vec![0.0f32; 8];
        input[0] = 1.0;
        let out = run(&mut node, &input, sample_rate);

        assert!(
            (out[1] - 0.5).abs() < 1e-3 && (out[2] - 0.5).abs() < 1e-3,
            "a 1.5-frame delay must split the impulse across taps 1 and 2: {out:?}"
        );
    }

    /// `delayTime` is a-rate. Sampling it once per block froze the delay at the
    /// block's start time, so a ramp only stepped at block boundaries.
    ///
    /// The probe is exact rather than approximate: a delay that grows by one frame
    /// per frame keeps the read position pinned to the first sample written, so a
    /// rising input must come out constant. Evaluated once per block the delay
    /// would be zero throughout and the output would be the input.
    #[test]
    fn delay_time_is_evaluated_per_sample() {
        let sample_rate = 48_000;
        let mut node = DelayNode::new(1, 0.1, sample_rate, 1);
        node.delay_time.set_value_at_time(0.0, 0.0);
        node.delay_time
            .linear_ramp_to_value_at_time(8.0 / sample_rate as f32, 8.0 / sample_rate as f64);

        let input: Vec<f32> = (1..=8).map(|i| i as f32).collect();
        let out = run(&mut node, &input, sample_rate);

        for (i, &sample) in out.iter().enumerate() {
            assert!(
                (sample - 1.0).abs() < 1e-3,
                "frame {i} read {sample}; a per-sample delay must track the write \
                 cursor and keep reading the first sample. Got {out:?}"
            );
        }
    }

    /// The aggregate ledger must reject delay buffer creation when it is near
    /// its ceiling. The refusal must happen before the allocation: if the ledger
    /// is at 99 bytes of a 100-byte ceiling, a ~384-byte minimum delay buffer
    /// cannot be granted and `new_with_aggregate` must return `Err` without
    /// ever calling `vec!`.
    #[test]
    fn delay_buffer_refused_when_aggregate_is_at_ceiling() {
        let ledger = Arc::new(crate::limits::AudioAggregateLedger::new(100));
        // Fill 99 bytes so only 1 byte remains — too little for any delay buffer.
        let _filler = ledger
            .try_reserve(99, "test-filler")
            .expect("filler fits within 100-byte test ledger");

        let result = DelayNode::new_with_aggregate(1, 0.001, 48_000, 1, &ledger);
        assert!(
            result.is_err(),
            "delay buffer creation must be refused when the aggregate ledger is full"
        );
    }

    /// The aggregate permit must be released exactly once when the node is
    /// dropped, leaving the ledger back at zero.
    #[test]
    fn delay_node_teardown_releases_aggregate_permit() {
        let ledger = Arc::new(crate::limits::AudioAggregateLedger::new(1024 * 1024));

        let node = DelayNode::new_with_aggregate(1, 0.001, 48_000, 1, &ledger)
            .expect("0.001 s mono delay fits within 1 MiB test ledger");

        let bytes_held = ledger.used_bytes();
        assert!(
            bytes_held > 0,
            "aggregate must show the delay buffer as reserved while the node lives"
        );

        drop(node);
        assert_eq!(
            ledger.used_bytes(),
            0,
            "aggregate must return to zero when the delay node is dropped"
        );
    }
}
