//! Test-only frozen implementation before bounded stack automation.
use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use crate::param::AudioParamTimeline;

use super::AudioNodeProcessor;

pub struct GainNode {
    id: AudioNodeId,
    gain: AudioParamTimeline,
    /// Reusable buffer for per-sample automation values
    automation_buf: Vec<f32>,
}

impl GainNode {
    pub fn new(id: AudioNodeId) -> Self {
        Self {
            id,
            gain: AudioParamTimeline::new(1.0, -3.4028235e38, 3.4028235e38),
            automation_buf: Vec::new(),
        }
    }

    pub fn set_gain(&mut self, value: f32) {
        self.gain.set_value(value);
    }

    pub fn gain_value(&self) -> f32 {
        self.gain.value()
    }
}

impl AudioNodeProcessor for GainNode {
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
        channels: u32,
        current_time: f64,
    ) -> usize {
        let len = inputs.len().min(output.len());
        if len == 0 {
            return 0;
        }

        let ch = channels.max(1) as usize;
        let frames = len / ch;

        if self.gain.has_automation() {
            // Per-sample automation for zipper-free transitions
            if self.automation_buf.len() < frames {
                self.automation_buf.resize(frames, 0.0);
            }
            self.gain.compute_values(
                current_time,
                &mut self.automation_buf[..frames],
                sample_rate,
            );
            for frame in 0..frames {
                let g = self.automation_buf[frame];
                for c in 0..ch {
                    let idx = frame * ch + c;
                    if idx < len {
                        output[idx] = inputs[idx] * g;
                    }
                }
            }
        } else {
            let gain = self.gain.value();

            // Optimization: skip multiply if gain is 1.0
            if (gain - 1.0).abs() < f32::EPSILON {
                output[..len].copy_from_slice(&inputs[..len]);
            } else {
                for i in 0..len {
                    output[i] = inputs[i] * gain;
                }
            }
        }

        frames
    }

    fn get_param_mut(&mut self, name: &str) -> Option<&mut AudioParamTimeline> {
        match name {
            "gain" => {
                // Parameter commands prepare the first a-rate device quantum.
                // Static gains keep their empty scratch until automation is used.
                self.automation_buf.reserve(
                    crate::audio_thread::RENDER_QUANTUM_FRAMES
                        .saturating_sub(self.automation_buf.len()),
                );
                Some(&mut self.gain)
            }
            _ => None,
        }
    }
}
