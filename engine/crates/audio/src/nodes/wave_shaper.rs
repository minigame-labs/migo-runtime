use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use super::AudioNodeProcessor;

/// Oversample mode for WaveShaperNode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OversampleType {
    None,
    TwoX,
    FourX,
}

impl OversampleType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "2x" => Self::TwoX,
            "4x" => Self::FourX,
            _ => Self::None,
        }
    }

    #[inline]
    fn factor(self) -> usize {
        match self {
            Self::None => 1,
            Self::TwoX => 2,
            Self::FourX => 4,
        }
    }
}

/// Half-band-ish FIR length used for both interpolation and decimation.
///
/// 32 taps of a Blackman-windowed sinc gives about 60 dB of stopband rejection,
/// which is what makes oversampling worth doing at all: too short a filter lets
/// the images it exists to remove straight back through.
const FIR_TAPS: usize = 32;

/// Windowed-sinc lowpass at `cutoff` (cycles/sample), scaled by `gain`.
fn design_lowpass(cutoff: f64, gain: f64) -> Vec<f32> {
    let centre = (FIR_TAPS - 1) as f64 / 2.0;
    let mut taps: Vec<f64> = (0..FIR_TAPS)
        .map(|i| {
            let n = i as f64 - centre;
            let sinc = if n.abs() < 1e-12 {
                2.0 * cutoff
            } else {
                (std::f64::consts::TAU * cutoff * n).sin() / (std::f64::consts::PI * n)
            };
            let phase = std::f64::consts::TAU * i as f64 / (FIR_TAPS - 1) as f64;
            let window = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
            sinc * window
        })
        .collect();
    // Normalise to unit DC gain before applying the interpolation gain, so the
    // filter cannot change the level of the signal it is only meant to band-limit.
    let dc: f64 = taps.iter().sum();
    if dc.abs() > 1e-12 {
        for tap in taps.iter_mut() {
            *tap = *tap / dc * gain;
        }
    }
    taps.into_iter().map(|t| t as f32).collect()
}

/// A single channel's FIR delay line.
#[derive(Clone, Default)]
struct FirState {
    history: [f32; FIR_TAPS],
    cursor: usize,
}

impl FirState {
    #[inline]
    fn push_and_filter(&mut self, sample: f32, taps: &[f32]) -> f32 {
        self.history[self.cursor] = sample;
        let mut acc = 0.0f32;
        // Walk backwards from the newest sample so tap 0 multiplies it.
        let mut index = self.cursor;
        for tap in taps.iter() {
            acc += *tap * self.history[index];
            index = if index == 0 { FIR_TAPS - 1 } else { index - 1 };
        }
        self.cursor = (self.cursor + 1) % FIR_TAPS;
        acc
    }
}

/// WaveShaperNode: applies a non-linear distortion using a transfer function.
///
/// The `curve` is a Float32Array lookup table. Input values in [-1, 1] are
/// mapped to indices in the curve array with linear interpolation.
///
/// `oversample` is honoured. It used to be accepted, stored and ignored, which is
/// worse than not offering it: distortion generates harmonics above Nyquist by
/// construction, so the setting whose entire job is to keep them from folding back
/// as inharmonic tones silently did nothing.
pub struct WaveShaperNode {
    id: AudioNodeId,
    curve: Option<Vec<f32>>,
    oversample: OversampleType,
    /// Interpolation and decimation filters, rebuilt when the factor changes.
    up_taps: Vec<f32>,
    down_taps: Vec<f32>,
    up_state: Vec<FirState>,
    down_state: Vec<FirState>,
    /// Oversampled working buffer, `factor` samples per input frame.
    scratch: Vec<f32>,
    output_connected: bool,
}

impl WaveShaperNode {
    pub fn new(id: AudioNodeId) -> Self {
        Self {
            id,
            curve: None,
            oversample: OversampleType::None,
            up_taps: Vec::new(),
            down_taps: Vec::new(),
            up_state: Vec::new(),
            down_state: Vec::new(),
            scratch: Vec::new(),
            output_connected: false,
        }
    }

    pub fn set_curve(&mut self, curve: Option<Vec<f32>>) {
        self.curve = curve;
    }

    pub(crate) fn retire_curve(
        &mut self,
        host: Option<&crate::node_retirement::Host>,
    ) -> shared::error::EngineResult<()> {
        let bytes = self
            .curve
            .as_ref()
            .map_or(0, |curve| curve.capacity() * std::mem::size_of::<f32>());
        crate::node_retirement::retire_replacement(&mut self.curve, host, bytes)
    }

    pub fn set_oversample(&mut self, oversample: OversampleType) {
        if oversample == self.oversample {
            return;
        }
        self.oversample = oversample;
        let factor = oversample.factor();
        if factor == 1 {
            self.up_taps.clear();
            self.down_taps.clear();
            self.up_state.clear();
            self.down_state.clear();
            self.scratch.clear();
            return;
        }
        // Cutoff is the original Nyquist expressed in the oversampled rate. The
        // interpolator also makes up the (1/factor) level lost to zero-stuffing.
        let cutoff = 0.5 / factor as f64;
        self.up_taps = design_lowpass(cutoff, factor as f64);
        self.down_taps = design_lowpass(cutoff, 1.0);
        self.up_state.clear();
        self.down_state.clear();
    }

    /// Prepare the context's first device quantum while handling its command.
    /// Keep lengths and filter history unchanged; process initializes new state.
    pub(crate) fn prepare_oversampling(&mut self, channels: u32) {
        let factor = self.oversample.factor();
        if factor == 1 {
            return;
        }
        let channels = channels.max(1) as usize;
        self.up_state
            .reserve(channels.saturating_sub(self.up_state.len()));
        self.down_state
            .reserve(channels.saturating_sub(self.down_state.len()));
        self.scratch.reserve(
            (crate::audio_thread::RENDER_QUANTUM_FRAMES * factor)
                .saturating_sub(self.scratch.len()),
        );
    }

    fn zero_input_response(&self) -> f32 {
        self.curve
            .as_ref()
            .map_or(0.0, |curve| Self::shape(curve, 0.0))
    }

    /// Apply waveshaping transfer function using linear interpolation
    #[inline]
    fn shape(curve: &[f32], input: f32) -> f32 {
        if curve.is_empty() {
            return input;
        }

        let len = curve.len();
        if len == 1 {
            return curve[0];
        }

        // Map input [-1, 1] to curve index [0, len-1]
        let clamped = input.clamp(-1.0, 1.0);
        let normalized = (clamped + 1.0) * 0.5; // [0, 1]
        let index_f = normalized * (len - 1) as f32;

        let index_low = (index_f as usize).min(len - 2);
        let index_high = index_low + 1;
        let frac = index_f - index_low as f32;

        // Linear interpolation
        curve[index_low] * (1.0 - frac) + curve[index_high] * frac
    }
}

impl AudioNodeProcessor for WaveShaperNode {
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
        _sample_rate: u32,
        channels: u32,
        _current_time: f64,
    ) -> usize {
        let ch = channels.max(1) as usize;
        let len =
            if inputs.is_empty() && (self.has_tail_audio() || self.zero_input_response() != 0.0) {
                output.len() / ch * ch
            } else {
                inputs.len().min(output.len())
            };
        if len == 0 {
            return 0;
        }
        let frames = len / ch;

        let Some(curve) = self.curve.take() else {
            // No curve: pass through
            output[..len].copy_from_slice(&inputs[..len]);
            return frames;
        };

        let factor = self.oversample.factor();
        if factor == 1 {
            for (i, sample) in output[..len].iter_mut().enumerate() {
                *sample = Self::shape(&curve, inputs.get(i).copied().unwrap_or(0.0));
            }
            self.curve = Some(curve);
            return frames;
        }

        // Per-channel filter state, allocated when the channel count is first seen.
        if self.up_state.len() < ch {
            self.up_state.resize(ch, FirState::default());
            self.down_state.resize(ch, FirState::default());
        }
        if self.scratch.len() < frames * factor {
            self.scratch.resize(frames * factor, 0.0);
        }

        for channel in 0..ch {
            // Upsample: zero-stuff, then interpolate.
            for frame in 0..frames {
                let sample = inputs.get(frame * ch + channel).copied().unwrap_or(0.0);
                for phase in 0..factor {
                    let stuffed = if phase == 0 { sample } else { 0.0 };
                    self.scratch[frame * factor + phase] =
                        self.up_state[channel].push_and_filter(stuffed, &self.up_taps);
                }
            }

            // Shape at the higher rate, where the harmonics it generates have room.
            for sample in self.scratch[..frames * factor].iter_mut() {
                *sample = Self::shape(&curve, *sample);
            }

            // Decimate: filter every oversampled sample, keep one in `factor`.
            for frame in 0..frames {
                let mut kept = 0.0;
                for phase in 0..factor {
                    let filtered = self.down_state[channel]
                        .push_and_filter(self.scratch[frame * factor + phase], &self.down_taps);
                    if phase == 0 {
                        kept = filtered;
                    }
                }
                output[frame * ch + channel] = kept;
            }
        }

        self.curve = Some(curve);
        frames
    }

    fn has_tail_audio(&self) -> bool {
        if self.oversample == OversampleType::None || self.curve.is_none() {
            return false;
        }
        // Both FIRs have finite history. Exact comparison retains tiny tails;
        // for biased curves the downstream steady state is shape(0), not zero.
        let baseline = self.zero_input_response();
        self.up_state
            .iter()
            .any(|state| state.history.iter().any(|&sample| sample != 0.0))
            || self
                .down_state
                .iter()
                .any(|state| state.history.iter().any(|&sample| sample != baseline))
    }

    fn is_producing(&self) -> bool {
        self.has_tail_audio() || (self.output_connected && self.zero_input_response() != 0.0)
    }

    fn set_output_connected(&mut self, connected: bool) {
        self.output_connected = connected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hard-clipping curve, which is the classic aliasing generator.
    fn clip_curve() -> Vec<f32> {
        (0..1024)
            .map(|i| {
                let x = i as f32 / 1023.0 * 2.0 - 1.0;
                (x * 4.0).clamp(-1.0, 1.0)
            })
            .collect()
    }

    fn shaped(oversample: OversampleType, input: &[f32]) -> Vec<f32> {
        let mut node = WaveShaperNode::new(1);
        node.set_curve(Some(clip_curve()));
        node.set_oversample(oversample);
        let mut out = vec![0.0f32; input.len()];
        node.process(input, &mut out, 48_000, 1, 0.0);
        out
    }

    /// Energy at the mirror of a distortion product is aliasing. Hard-clipping a
    /// tone near Nyquist/3 generates a third harmonic above Nyquist, which folds
    /// down; oversampling has to remove most of it. The observable proxy is the
    /// difference between the two settings: if `oversample` is ignored, they are
    /// bit-identical.
    #[test]
    fn oversampling_changes_the_result_rather_than_being_ignored() {
        let sample_rate = 48_000.0;
        let frequency = 7_000.0;
        let input: Vec<f32> = (0..1024)
            .map(|i| {
                (std::f64::consts::TAU * frequency * i as f64 / sample_rate).sin() as f32 * 0.8
            })
            .collect();

        let plain = shaped(OversampleType::None, &input);
        let two_x = shaped(OversampleType::TwoX, &input);
        let four_x = shaped(OversampleType::FourX, &input);

        let differs = |a: &[f32], b: &[f32]| {
            a.iter()
                .zip(b.iter())
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f32, f32::max)
        };
        assert!(
            differs(&plain, &two_x) > 1e-3,
            "2x oversampling must actually filter, not pass the setting through"
        );
        assert!(
            differs(&plain, &four_x) > 1e-3,
            "4x oversampling must actually filter"
        );
    }

    /// Oversampling must not change the level or polarity of what it filters: a
    /// signal inside the curve's linear region has to survive round-tripping
    /// through the interpolate/decimate pair.
    #[test]
    fn oversampling_preserves_a_signal_inside_the_linear_region() {
        // Gentle curve: identity over [-1, 1].
        let identity: Vec<f32> = (0..1024).map(|i| i as f32 / 1023.0 * 2.0 - 1.0).collect();
        let sample_rate = 48_000.0;
        let input: Vec<f32> = (0..2048)
            .map(|i| (std::f64::consts::TAU * 200.0 * i as f64 / sample_rate).sin() as f32 * 0.5)
            .collect();

        let mut node = WaveShaperNode::new(1);
        node.set_curve(Some(identity));
        node.set_oversample(OversampleType::TwoX);
        let mut out = vec![0.0f32; input.len()];
        node.process(&input, &mut out, 48_000, 1, 0.0);

        // Skip the filter's group delay, then compare peak levels.
        let settled = &out[FIR_TAPS * 2..];
        let peak_in = input.iter().copied().fold(0.0f32, |m, s| m.max(s.abs()));
        let peak_out = settled.iter().copied().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (peak_out - peak_in).abs() < 0.05,
            "level must survive oversampling: {peak_out} vs {peak_in}"
        );
    }

    #[test]
    fn without_a_curve_the_node_passes_through_at_every_oversample_setting() {
        for mode in [
            OversampleType::None,
            OversampleType::TwoX,
            OversampleType::FourX,
        ] {
            let mut node = WaveShaperNode::new(1);
            node.set_oversample(mode);
            let input: Vec<f32> = (0..16).map(|i| i as f32).collect();
            let mut out = vec![0.0f32; input.len()];
            node.process(&input, &mut out, 48_000, 1, 0.0);
            assert_eq!(out, input, "{mode:?} without a curve must pass through");
        }
    }
}
