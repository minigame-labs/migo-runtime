use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use super::AudioNodeProcessor;

/// IIRFilterNode: generic IIR filter with user-provided coefficients.
///
/// Implements the transfer function:
///   H(z) = (b0 + b1*z^-1 + ... + bM*z^-M) / (1 + a1*z^-1 + ... + aN*z^-N)
///
/// Note: a0 is always 1 (normalized). The `feedback` array includes a0.
///
/// Uses circular buffers for history to avoid O(N) per-sample shifting.
pub struct IIRFilterNode {
    id: AudioNodeId,
    /// Feedforward coefficients (numerator)
    feedforward: Vec<f64>,
    /// Feedback coefficients (denominator), a[0] is normalized to 1.0
    feedback: Vec<f64>,
    /// Per-channel input circular buffer
    x_history: Vec<Vec<f64>>,
    /// Per-channel output circular buffer
    y_history: Vec<Vec<f64>>,
    /// Current write position in circular buffers (shared across channels)
    x_write_pos: usize,
    y_write_pos: usize,
    channels: u32,
}

impl IIRFilterNode {
    pub fn new(id: AudioNodeId, feedforward: Vec<f64>, feedback: Vec<f64>, channels: u32) -> Self {
        let ff_len = feedforward.len().max(1);
        let fb_len = feedback.len().max(1);

        // Normalize by a[0]
        let a0 = if feedback.is_empty() {
            1.0
        } else {
            feedback[0]
        };
        let inv_a0 = if a0.abs() > 1e-20 { 1.0 / a0 } else { 1.0 };

        let norm_ff: Vec<f64> = feedforward.iter().map(|&v| v * inv_a0).collect();
        let norm_fb: Vec<f64> = feedback.iter().map(|&v| v * inv_a0).collect();

        let ch = channels.max(1) as usize;
        let x_history = vec![vec![0.0; ff_len]; ch];
        let y_history = vec![vec![0.0; fb_len]; ch];

        Self {
            id,
            feedforward: norm_ff,
            feedback: norm_fb,
            x_history,
            y_history,
            x_write_pos: 0,
            y_write_pos: 0,
            channels,
        }
    }

    /// Compute frequency response H(e^{jw}) for an array of frequencies.
    /// H(z) = sum(b[k]*z^{-k}) / sum(a[k]*z^{-k})
    pub fn get_frequency_response(
        &self,
        sample_rate: f64,
        frequencies: &[f32],
    ) -> (Vec<f32>, Vec<f32>) {
        let len = frequencies.len();
        let mut mag_response = Vec::with_capacity(len);
        let mut phase_response = Vec::with_capacity(len);

        for &freq_hz in frequencies {
            let omega = std::f64::consts::TAU * freq_hz as f64 / sample_rate;

            // Numerator: sum b[k] * e^{-jkw}
            let mut num_re = 0.0;
            let mut num_im = 0.0;
            for (k, &bk) in self.feedforward.iter().enumerate() {
                let angle = k as f64 * omega;
                num_re += bk * angle.cos();
                num_im -= bk * angle.sin();
            }

            // Denominator: sum a[k] * e^{-jkw}
            let mut den_re = 0.0;
            let mut den_im = 0.0;
            for (k, &ak) in self.feedback.iter().enumerate() {
                let angle = k as f64 * omega;
                den_re += ak * angle.cos();
                den_im -= ak * angle.sin();
            }

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
}

impl AudioNodeProcessor for IIRFilterNode {
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
        _channels: u32,
        _current_time: f64,
    ) -> usize {
        let channels = self.channels.max(1) as usize;
        let frames = if inputs.is_empty() {
            output.len() / channels
        } else {
            inputs.len().min(output.len()) / channels
        };
        if frames == 0 {
            return 0;
        }
        let len = frames * channels;
        let ff = &self.feedforward;
        let fb = &self.feedback;
        let ff_len = ff.len();
        let fb_len = fb.len();

        // Degenerate filter with no coefficients: pass input through unchanged.
        // For zero input, its output is simply silence.
        if ff_len == 0 || fb_len == 0 {
            for (idx, sample) in output[..len].iter_mut().enumerate() {
                *sample = inputs.get(idx).copied().unwrap_or(0.0);
            }
            return frames;
        }

        for frame in 0..frames {
            for ch in 0..channels {
                let idx = frame * channels + ch;
                let x0 = inputs.get(idx).copied().unwrap_or(0.0) as f64;

                // Write input to circular buffer (O(1) instead of O(N) shift)
                let xh = &mut self.x_history[ch];
                if !xh.is_empty() {
                    let xpos = self.x_write_pos % ff_len;
                    xh[xpos] = x0;
                }

                // Compute output: sum(b[i]*x[n-i]) - sum(a[i]*y[n-i]) for i>=1
                let mut y0 = 0.0;
                for i in 0..ff_len {
                    let ri = (self.x_write_pos + ff_len - i) % ff_len;
                    y0 += ff[i] * xh[ri];
                }
                let yh = &self.y_history[ch];
                for i in 1..fb_len {
                    let ri = (self.y_write_pos + fb_len - i) % fb_len;
                    y0 -= fb[i] * yh[ri];
                }

                // Write output to circular buffer
                let yh = &mut self.y_history[ch];
                if !yh.is_empty() {
                    let ypos = self.y_write_pos % fb_len;
                    yh[ypos] = y0;
                }

                output[idx] = y0 as f32;
            }

            self.x_write_pos = self.x_write_pos.wrapping_add(1);
            self.y_write_pos = self.y_write_pos.wrapping_add(1);
        }

        frames
    }

    fn has_tail_audio(&self) -> bool {
        self.x_history
            .iter()
            .chain(self.y_history.iter())
            .flatten()
            .any(|sample| sample.abs() > 1e-10)
    }
}
