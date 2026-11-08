use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use crate::param::AudioParamTimeline;

use super::AudioNodeProcessor;

/// DynamicsCompressorNode: applies dynamic range compression.
///
/// Uses an envelope follower and gain reduction to compress
/// audio that exceeds the threshold.
pub struct DynamicsCompressorNode {
    id: AudioNodeId,
    threshold: AudioParamTimeline,
    knee: AudioParamTimeline,
    ratio: AudioParamTimeline,
    attack: AudioParamTimeline,
    release: AudioParamTimeline,
    /// Current gain reduction in dB (read-only, negative value)
    reduction: f32,
    /// Internal envelope follower state
    envelope: f32,
    channels: u32,
}

impl DynamicsCompressorNode {
    pub fn new(id: AudioNodeId, channels: u32) -> Self {
        Self {
            id,
            threshold: AudioParamTimeline::new(-24.0, -100.0, 0.0),
            knee: AudioParamTimeline::new(30.0, 0.0, 40.0),
            ratio: AudioParamTimeline::new(12.0, 1.0, 20.0),
            attack: AudioParamTimeline::new(0.003, 0.0, 1.0),
            release: AudioParamTimeline::new(0.25, 0.0, 1.0),
            reduction: 0.0,
            envelope: 0.0,
            channels,
        }
    }

    pub fn reduction(&self) -> f32 {
        self.reduction
    }
}

impl AudioNodeProcessor for DynamicsCompressorNode {
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
        let len = inputs.len().min(output.len());
        if len == 0 {
            return 0;
        }

        let channels = self.channels.max(1) as usize;
        let frames = len / channels;
        let sr = sample_rate as f32;

        let threshold_db = self.threshold.compute_value(current_time);
        let knee_db = self.knee.compute_value(current_time);
        let ratio = self.ratio.compute_value(current_time).max(1.0);
        let attack = self.attack.compute_value(current_time).max(0.0001);
        let release = self.release.compute_value(current_time).max(0.0001);

        // Compute attack/release coefficients
        let attack_coeff = (-1.0 / (attack * sr)).exp();
        let release_coeff = (-1.0 / (release * sr)).exp();

        let half_knee = knee_db * 0.5;

        for frame in 0..frames {
            // Find peak across all channels for this frame
            let mut peak = 0.0f32;
            for ch in 0..channels {
                let sample = inputs[frame * channels + ch].abs();
                if sample > peak {
                    peak = sample;
                }
            }

            // Convert to dB
            let input_db = if peak > 1e-10 {
                20.0 * peak.log10()
            } else {
                -200.0
            };

            // Compute gain reduction using soft-knee characteristic
            let over_db = input_db - threshold_db;
            let gain_reduction_db = if over_db <= -half_knee {
                0.0
            } else if over_db >= half_knee {
                // Above knee: full compression
                over_db * (1.0 - 1.0 / ratio)
            } else {
                // In knee region: quadratic interpolation
                let x = over_db + half_knee;
                (x * x) / (4.0 * knee_db.max(0.001)) * (1.0 - 1.0 / ratio)
            };

            // Envelope follower
            let target = gain_reduction_db;
            if target > self.envelope {
                self.envelope = attack_coeff * self.envelope + (1.0 - attack_coeff) * target;
            } else {
                self.envelope = release_coeff * self.envelope + (1.0 - release_coeff) * target;
            }

            // Convert dB gain reduction to linear gain.
            // 10^(-env/20) = e^(-env * ln10/20) — exp() is ~4x faster than powf() on ARM.
            const LN10_OVER_20: f32 = std::f32::consts::LN_10 / 20.0;
            let gain = (-self.envelope * LN10_OVER_20).exp();

            // Apply gain to all channels
            for ch in 0..channels {
                let idx = frame * channels + ch;
                output[idx] = inputs[idx] * gain;
            }
        }

        self.reduction = -self.envelope;

        frames
    }

    fn get_param_mut(&mut self, name: &str) -> Option<&mut AudioParamTimeline> {
        match name {
            "threshold" => Some(&mut self.threshold),
            "knee" => Some(&mut self.knee),
            "ratio" => Some(&mut self.ratio),
            "attack" => Some(&mut self.attack),
            "release" => Some(&mut self.release),
            _ => None,
        }
    }
}
