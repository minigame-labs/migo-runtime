use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use crate::param::AudioParamTimeline;

use super::AudioNodeProcessor;

pub struct GainNode {
    id: AudioNodeId,
    gain: AudioParamTimeline,
}

impl GainNode {
    pub fn new(id: AudioNodeId) -> Self {
        Self {
            id,
            gain: AudioParamTimeline::new(1.0, -3.4028235e38, 3.4028235e38),
        }
    }

    pub fn set_gain(&mut self, value: f32) {
        self.gain.set_value(value);
    }

    #[allow(dead_code)]
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
            let mut automation_buf = [0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];
            let mut block = self.gain.values_block(current_time, sample_rate);
            for frame in 0..frames {
                let index = frame % automation_buf.len();
                if index == 0 {
                    let count = (frames - frame).min(automation_buf.len());
                    self.gain
                        .compute_values_from(&mut block, frame, &mut automation_buf[..count]);
                }
                let g = automation_buf[index];
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

    fn prepare_params(
        &mut self,
        host: &crate::node_retirement::Host,
    ) -> shared::error::EngineResult<()> {
        self.gain.prepare_storage(host)
    }

    fn get_param_mut(&mut self, name: &str) -> Option<&mut AudioParamTimeline> {
        match name {
            "gain" => Some(&mut self.gain),
            _ => None,
        }
    }
}
