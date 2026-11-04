use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use crate::param::AudioParamTimeline;

use super::AudioNodeProcessor;

/// ConstantSourceNode: outputs a constant value from its `offset` AudioParam.
///
/// Useful for automation — the offset can be automated and connected to
/// other nodes' AudioParam inputs for complex modulation.
pub struct ConstantSourceNode {
    id: AudioNodeId,
    offset: AudioParamTimeline,
    started: bool,
    stopped: bool,
    start_when: f64,
    stop_when: f64,
}

impl ConstantSourceNode {
    pub fn new(id: AudioNodeId) -> Self {
        Self {
            id,
            offset: AudioParamTimeline::new(1.0, f32::MIN, f32::MAX),
            started: false,
            stopped: false,
            start_when: 0.0,
            stop_when: f64::INFINITY,
        }
    }

    pub fn start(&mut self, when: f64) {
        if self.started || self.stopped {
            return;
        }
        self.started = true;
        self.start_when = when;
    }

    pub fn stop(&mut self, when: f64) {
        if self.stopped {
            return;
        }
        self.stop_when = when;
    }
}

impl AudioNodeProcessor for ConstantSourceNode {
    fn id(&self) -> AudioNodeId {
        self.id
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn is_finished(&self) -> bool {
        self.stopped
    }

    fn is_producing(&self) -> bool {
        self.started && !self.stopped
    }

    fn process(
        &mut self,
        _inputs: &[f32],
        output: &mut [f32],
        sample_rate: u32,
        channels: u32,
        current_time: f64,
    ) -> usize {
        let channels = channels.max(1) as usize;
        let frames = output.len() / channels;
        let clock = crate::render_time::RenderTime::new(current_time, sample_rate);
        let active = clock.active(self.start_when, self.stop_when, frames);
        let stopped = clock.stopped(self.stop_when, frames);
        if self.stopped || !self.started || active.is_empty() {
            output[..frames * channels].fill(0.0);
            self.stopped |= stopped;
            return 0;
        }
        output[..active.start * channels].fill(0.0);
        output[active.end * channels..frames * channels].fill(0.0);
        let output = &mut output[active.start * channels..active.end * channels];
        let frames = active.len();
        let current_time = clock.at(active.start);

        if self.offset.has_automation() {
            let mut automation = [0.0; crate::audio_thread::RENDER_QUANTUM_FRAMES];
            let mut block = self.offset.values_block(current_time, sample_rate);
            for (frame, samples) in output.chunks_exact_mut(channels).enumerate() {
                let index = frame % automation.len();
                if index == 0 {
                    let count = (frames - frame).min(automation.len());
                    self.offset
                        .compute_values_from(&mut block, frame, &mut automation[..count]);
                }
                samples.fill(automation[index]);
            }
        } else {
            let value = self.offset.compute_value(current_time);
            output.fill(value);
        }
        self.stopped |= stopped;
        frames
    }

    fn prepare_params(
        &mut self,
        host: &crate::node_retirement::Host,
    ) -> shared::error::EngineResult<()> {
        self.offset.prepare_storage(host)
    }

    fn get_param_mut(&mut self, name: &str) -> Option<&mut AudioParamTimeline> {
        match name {
            "offset" => Some(&mut self.offset),
            _ => None,
        }
    }
}
