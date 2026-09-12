use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use crate::param::AudioParamTimeline;

use super::AudioNodeProcessor;

/// BiquadFilter types (Robert Bristow-Johnson's Audio EQ Cookbook)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BiquadFilterType {
    Lowpass,
    Highpass,
    Bandpass,
    Lowshelf,
    Highshelf,
    Peaking,
    Notch,
    Allpass,
}

impl BiquadFilterType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "highpass" => Self::Highpass,
            "bandpass" => Self::Bandpass,
            "lowshelf" => Self::Lowshelf,
            "highshelf" => Self::Highshelf,
            "peaking" => Self::Peaking,
            "notch" => Self::Notch,
            "allpass" => Self::Allpass,
            _ => Self::Lowpass,
        }
    }
}

/// Biquad filter state per channel
#[derive(Clone, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

pub struct BiquadFilterNode {
    id: AudioNodeId,
    filter_type: BiquadFilterType,
    frequency: AudioParamTimeline,
    detune: AudioParamTimeline,
    q: AudioParamTimeline,
    gain: AudioParamTimeline,
    // Biquad coefficients
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    // Per-channel filter state
    states: Vec<BiquadState>,
    // Cache to avoid recomputing coefficients every frame
    last_freq: f32,
    last_q: f32,
    last_gain: f32,
    last_type: BiquadFilterType,
    last_detune: f32,
}

impl BiquadFilterNode {
    pub fn new(id: AudioNodeId, channels: u32, sample_rate: u32) -> Self {
        // Nyquist for the rate actually in use. Hardcoding 22050 is only right
        // at 44.1 kHz; at the 48 kHz most devices run it clamped away the top
        // 2 kHz of the legal range.
        let nyquist = (sample_rate.max(2) / 2) as f32;
        let mut node = Self {
            id,
            filter_type: BiquadFilterType::Lowpass,
            frequency: AudioParamTimeline::new(350.0, 0.0, nyquist),
            detune: AudioParamTimeline::new(0.0, -153600.0, 153600.0),
            q: AudioParamTimeline::new(1.0, 0.0001, 1000.0),
            gain: AudioParamTimeline::new(0.0, -40.0, 40.0),
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            states: vec![BiquadState::default(); channels as usize],
            last_freq: -1.0,
            last_q: -1.0,
            last_gain: f32::NAN,
            last_type: BiquadFilterType::Lowpass,
            last_detune: f32::NAN,
        };
        node.compute_coefficients(sample_rate.max(1) as f64, 0.0); // recomputed on first process
        node
    }

    pub fn set_type(&mut self, t: BiquadFilterType) {
        self.filter_type = t;
        // Force coefficient recomputation
        self.last_freq = -1.0;
    }

    /// Compute frequency response for an array of frequencies.
    /// Evaluates H(e^{jω}) = (b0 + b1·e^{-jω} + b2·e^{-2jω}) / (1 + a1·e^{-jω} + a2·e^{-2jω})
    /// at each frequency. Updates coefficients first to ensure they're current.
    pub fn get_frequency_response(
        &mut self,
        sample_rate: f64,
        frequencies: &[f32],
    ) -> (Vec<f32>, Vec<f32>) {
        // Ensure coefficients are up-to-date
        self.compute_coefficients(sample_rate, 0.0);

        let len = frequencies.len();
        let mut mag_response = Vec::with_capacity(len);
        let mut phase_response = Vec::with_capacity(len);

        for &freq_hz in frequencies {
            let omega = std::f64::consts::TAU * freq_hz as f64 / sample_rate;

            // e^{-jω} components
            let cos1 = omega.cos();
            let sin1 = omega.sin();
            let cos2 = (2.0 * omega).cos();
            let sin2 = (2.0 * omega).sin();

            // Numerator: b0 + b1·e^{-jω} + b2·e^{-2jω}
            let num_re = self.b0 + self.b1 * cos1 + self.b2 * cos2;
            let num_im = -(self.b1 * sin1 + self.b2 * sin2);

            // Denominator: 1 + a1·e^{-jω} + a2·e^{-2jω}
            let den_re = 1.0 + self.a1 * cos1 + self.a2 * cos2;
            let den_im = -(self.a1 * sin1 + self.a2 * sin2);

            // H = num / den (complex division)
            let den_mag_sq = den_re * den_re + den_im * den_im;
            if den_mag_sq < 1e-20 {
                mag_response.push(0.0);
                phase_response.push(0.0);
                continue;
            }

            let h_re = (num_re * den_re + num_im * den_im) / den_mag_sq;
            let h_im = (num_im * den_re - num_re * den_im) / den_mag_sq;

            let magnitude = (h_re * h_re + h_im * h_im).sqrt();
            let phase = h_im.atan2(h_re);

            mag_response.push(magnitude as f32);
            phase_response.push(phase as f32);
        }

        (mag_response, phase_response)
    }

    /// Compute biquad coefficients using Robert Bristow-Johnson's formulas
    fn compute_coefficients(&mut self, sample_rate: f64, current_time: f64) {
        let freq = self.frequency.compute_value(current_time);
        let q = self.q.compute_value(current_time);
        let db_gain = self.gain.compute_value(current_time);
        let detune_val = self.detune.compute_value(current_time);
        let ft = self.filter_type;

        // Skip if nothing changed (detune included — it shifts f0 below)
        if freq == self.last_freq
            && q == self.last_q
            && db_gain == self.last_gain
            && ft == self.last_type
            && detune_val == self.last_detune
        {
            return;
        }
        self.last_freq = freq;
        self.last_q = q;
        self.last_gain = db_gain;
        self.last_type = ft;
        self.last_detune = detune_val;

        let detune = detune_val as f64;
        let f0 = (freq as f64) * (2.0f64).powf(detune / 1200.0);
        let w0 = std::f64::consts::TAU * f0 / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q.max(0.0001) as f64);
        let a_db = 10.0f64.powf(db_gain as f64 / 40.0);

        let (b0, b1, b2, a0, a1, a2) = match ft {
            BiquadFilterType::Lowpass => {
                let b1 = 1.0 - cos_w0;
                let b0 = b1 / 2.0;
                let b2 = b0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadFilterType::Highpass => {
                let b1 = -(1.0 + cos_w0);
                let b0 = (1.0 + cos_w0) / 2.0;
                let b2 = b0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadFilterType::Bandpass => {
                let b0 = alpha;
                let b1 = 0.0;
                let b2 = -alpha;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadFilterType::Lowshelf => {
                let two_sqrt_a_alpha = 2.0 * a_db.sqrt() * alpha;
                let b0 = a_db * ((a_db + 1.0) - (a_db - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = 2.0 * a_db * ((a_db - 1.0) - (a_db + 1.0) * cos_w0);
                let b2 = a_db * ((a_db + 1.0) - (a_db - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a_db + 1.0) + (a_db - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = -2.0 * ((a_db - 1.0) + (a_db + 1.0) * cos_w0);
                let a2 = (a_db + 1.0) + (a_db - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadFilterType::Highshelf => {
                let two_sqrt_a_alpha = 2.0 * a_db.sqrt() * alpha;
                let b0 = a_db * ((a_db + 1.0) + (a_db - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = -2.0 * a_db * ((a_db - 1.0) + (a_db + 1.0) * cos_w0);
                let b2 = a_db * ((a_db + 1.0) + (a_db - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a_db + 1.0) - (a_db - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = 2.0 * ((a_db - 1.0) - (a_db + 1.0) * cos_w0);
                let a2 = (a_db + 1.0) - (a_db - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadFilterType::Peaking => {
                let b0 = 1.0 + alpha * a_db;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 - alpha * a_db;
                let a0 = 1.0 + alpha / a_db;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha / a_db;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadFilterType::Notch => {
                let b0 = 1.0;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadFilterType::Allpass => {
                let b0 = 1.0 - alpha;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 + alpha;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
        };

        // Normalize coefficients
        let inv_a0 = 1.0 / a0;
        self.b0 = b0 * inv_a0;
        self.b1 = b1 * inv_a0;
        self.b2 = b2 * inv_a0;
        self.a1 = a1 * inv_a0;
        self.a2 = a2 * inv_a0;
    }
}

impl AudioNodeProcessor for BiquadFilterNode {
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
        let channels = self.states.len().max(1);
        // Once an upstream source ends, process zero input through the
        // difference equation so the filter's stored state can decay naturally.
        let frames = if inputs.is_empty() {
            output.len() / channels
        } else {
            inputs.len().min(output.len()) / channels
        };
        if frames == 0 {
            return 0;
        }

        // Recompute coefficients if params changed (k-rate: sampled once per block)
        self.compute_coefficients(sample_rate as f64, current_time);

        let (b0, b1, b2, a1, a2) = (self.b0, self.b1, self.b2, self.a1, self.a2);

        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let x0 = inputs.get(idx).copied().unwrap_or(0.0) as f64;
                let state = &mut self.states[ch];

                // Direct Form I
                let y0 = b0 * x0 + b1 * state.x1 + b2 * state.x2 - a1 * state.y1 - a2 * state.y2;

                state.x2 = state.x1;
                state.x1 = x0;
                state.y2 = state.y1;
                state.y1 = y0;

                output[idx] = y0 as f32;
            }
        }

        frames
    }

    fn has_tail_audio(&self) -> bool {
        self.states.iter().any(|state| {
            [state.x1, state.x2, state.y1, state.y2]
                .iter()
                .any(|sample| sample.abs() > 1e-10)
        })
    }

    fn get_param_mut(&mut self, name: &str) -> Option<&mut AudioParamTimeline> {
        match name {
            "frequency" => Some(&mut self.frequency),
            "detune" => Some(&mut self.detune),
            "Q" => Some(&mut self.q),
            "gain" => Some(&mut self.gain),
            _ => None,
        }
    }
}
