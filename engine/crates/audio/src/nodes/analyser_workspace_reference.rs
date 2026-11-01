//! Test-only frozen analyser before workspace ownership changes.
use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use super::AudioNodeProcessor;

/// AnalyserNode: provides real-time frequency and time-domain analysis.
///
/// Passes audio through unchanged while capturing samples for FFT analysis.
pub struct AnalyserNode {
    id: AudioNodeId,
    fft_size: usize,
    min_decibels: f32,
    max_decibels: f32,
    smoothing_time_constant: f32,
    /// Circular time-domain buffer (mono, downmixed from input)
    time_domain_buffer: Vec<f32>,
    write_pos: usize,
    channels: u32,
    // --- Cached FFT workspace (avoids allocation per query) ---
    fft_re: Vec<f64>,
    fft_im: Vec<f64>,
    /// Pre-computed Blackman window coefficients (recomputed on fft_size change)
    window: Vec<f64>,
    /// Pre-computed twiddle factors, `exp(-2*PI*i*k/n)` for `k` in `0..n/2`.
    ///
    /// The butterfly used to derive each factor from the previous one by complex
    /// multiplication. That recurrence loses precision as it goes, and the loss
    /// grows with the transform: by the 32768-point size the spec allows, the
    /// factors near the end of a pass are visibly off, which shows up as a raised
    /// noise floor across the whole spectrum. A table is exact at every index and
    /// is built once per `fftSize` change.
    twiddles: Vec<(f64, f64)>,
    /// Previous frame's magnitudes, for `smoothingTimeConstant`.
    smoothed: Vec<f64>,
}

impl AnalyserNode {
    pub fn new(id: AudioNodeId, channels: u32) -> Self {
        let fft_size = 2048; // Default per spec
        let window = Self::compute_blackman_window(fft_size);
        let twiddles = Self::compute_twiddles(fft_size);
        Self {
            id,
            fft_size,
            min_decibels: -100.0,
            max_decibels: -30.0,
            smoothing_time_constant: 0.8,
            time_domain_buffer: vec![0.0; fft_size],
            write_pos: 0,
            channels,
            fft_re: vec![0.0; fft_size],
            fft_im: vec![0.0; fft_size],
            window,
            twiddles,
            smoothed: vec![0.0; fft_size / 2],
        }
    }

    pub fn set_fft_size(&mut self, size: usize) {
        // Must be power of 2, between 32 and 32768
        let size = size.next_power_of_two().clamp(32, 32768);
        if size != self.fft_size {
            self.fft_size = size;
            self.time_domain_buffer.resize(size, 0.0);
            self.write_pos = 0;
            // Resize cached buffers and recompute window
            self.fft_re.resize(size, 0.0);
            self.fft_im.resize(size, 0.0);
            self.window = Self::compute_blackman_window(size);
            self.twiddles = Self::compute_twiddles(size);
            // The history is meaningless at a new resolution.
            self.smoothed.clear();
            self.smoothed.resize(size / 2, 0.0);
        }
    }

    pub fn set_min_decibels(&mut self, v: f32) {
        self.min_decibels = v;
    }

    pub fn set_max_decibels(&mut self, v: f32) {
        self.max_decibels = v;
    }

    pub fn set_smoothing_time_constant(&mut self, v: f32) {
        self.smoothing_time_constant = v.clamp(0.0, 1.0);
    }

    /// Get the current time domain data as byte values (0-255)
    pub fn get_byte_time_domain_data(&self) -> Vec<u8> {
        let mut result = vec![0u8; self.fft_size];
        let buf_len = self.time_domain_buffer.len();
        for (i, sample) in result.iter_mut().enumerate() {
            let pos = (self.write_pos + i) % buf_len;
            // Map [-1, 1] to [0, 255]
            let val = ((self.time_domain_buffer[pos] + 1.0) * 128.0).clamp(0.0, 255.0);
            *sample = val as u8;
        }
        result
    }

    /// Get the current time domain data as float values
    pub fn get_float_time_domain_data(&self) -> Vec<f32> {
        let mut result = vec![0.0f32; self.fft_size];
        let buf_len = self.time_domain_buffer.len();
        for (i, sample) in result.iter_mut().enumerate() {
            let pos = (self.write_pos + i) % buf_len;
            *sample = self.time_domain_buffer[pos];
        }
        result
    }

    /// Radix-2 Cooley-Tukey in-place FFT, using a pre-computed twiddle table.
    fn fft(re: &mut [f64], im: &mut [f64], twiddles: &[(f64, f64)]) {
        let n = re.len();
        if n <= 1 {
            return;
        }
        debug_assert!(n.is_power_of_two());
        debug_assert_eq!(twiddles.len(), n / 2);

        // Bit-reversal permutation
        let mut j = 0usize;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j ^= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }

        // Butterfly passes. Each factor is read from the table rather than derived
        // from its predecessor, so precision does not decay across a pass.
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let stride = n / len;
            let mut i = 0;
            while i < n {
                for k in 0..half {
                    let (w_re, w_im) = twiddles[k * stride];
                    let u_re = re[i + k];
                    let u_im = im[i + k];
                    let v_re = re[i + k + half] * w_re - im[i + k + half] * w_im;
                    let v_im = re[i + k + half] * w_im + im[i + k + half] * w_re;
                    re[i + k] = u_re + v_re;
                    im[i + k] = u_im + v_im;
                    re[i + k + half] = u_re - v_re;
                    im[i + k + half] = u_im - v_im;
                }
                i += len;
            }
            len <<= 1;
        }
    }

    /// `exp(-2*PI*i*k/n)` for `k` in `0..n/2`, computed directly at every index.
    fn compute_twiddles(n: usize) -> Vec<(f64, f64)> {
        (0..n / 2)
            .map(|k| {
                let angle = -std::f64::consts::TAU * k as f64 / n as f64;
                (angle.cos(), angle.sin())
            })
            .collect()
    }

    /// Pre-compute Blackman window coefficients (called once per fft_size change).
    fn compute_blackman_window(n: usize) -> Vec<f64> {
        let inv_n = 1.0 / (n - 1).max(1) as f64;
        (0..n)
            .map(|i| {
                0.42 - 0.5 * (std::f64::consts::TAU * i as f64 * inv_n).cos()
                    + 0.08 * (2.0 * std::f64::consts::TAU * i as f64 * inv_n).cos()
            })
            .collect()
    }

    /// Prepare windowed time-domain data into cached FFT buffers and run FFT.
    fn run_fft(&mut self) {
        let buf_len = self.time_domain_buffer.len();
        let n = self.fft_size;

        // Fill re[] with windowed time-domain data; zero im[]
        for i in 0..n {
            let pos = (self.write_pos + i) % buf_len;
            self.fft_re[i] = self.time_domain_buffer[pos] as f64 * self.window[i];
            self.fft_im[i] = 0.0;
        }

        Self::fft(&mut self.fft_re, &mut self.fft_im, &self.twiddles);
        self.apply_smoothing();
    }

    /// Blend this frame's magnitudes with the previous frame's.
    ///
    /// **`smoothingTimeConstant` used to be stored and never read.** It is what
    /// makes a spectrum visualiser readable rather than a flicker, and the spec
    /// defines the frequency data as the smoothed magnitudes, not the raw ones --
    /// so ignoring it was both a missing feature and a wrong answer.
    fn apply_smoothing(&mut self) {
        let bins = self.fft_size / 2;
        if self.smoothed.len() != bins {
            self.smoothed.clear();
            self.smoothed.resize(bins, 0.0);
        }
        let tau = self.smoothing_time_constant.clamp(0.0, 1.0) as f64;
        let inv_fft = 1.0 / self.fft_size as f64;
        for bin in 0..bins {
            let magnitude =
                (self.fft_re[bin] * self.fft_re[bin] + self.fft_im[bin] * self.fft_im[bin]).sqrt()
                    * inv_fft;
            self.smoothed[bin] = tau * self.smoothed[bin] + (1.0 - tau) * magnitude;
        }
    }

    /// Get frequency domain data in bytes (0-255), mapped from dB scale.
    pub fn get_byte_frequency_data(&mut self) -> Vec<u8> {
        let freq_bins = self.fft_size / 2;

        self.run_fft();

        let mut result = vec![0u8; freq_bins];
        let min_db = self.min_decibels as f64;
        let max_db = self.max_decibels as f64;
        let range = max_db - min_db;

        for (i, sample) in result.iter_mut().enumerate() {
            let magnitude = self.smoothed[i];
            let db = if magnitude > 1e-20 {
                20.0 * magnitude.log10()
            } else {
                min_db
            };
            let normalized = ((db - min_db) / range).clamp(0.0, 1.0);
            *sample = (normalized * 255.0) as u8;
        }
        result
    }

    /// Get frequency domain data as float dB values.
    pub fn get_float_frequency_data(&mut self) -> Vec<f32> {
        let freq_bins = self.fft_size / 2;

        self.run_fft();

        let mut result = vec![0.0f32; freq_bins];
        let min_db = self.min_decibels as f64;

        for (i, sample) in result.iter_mut().enumerate() {
            let magnitude = self.smoothed[i];
            let db = if magnitude > 1e-20 {
                20.0 * magnitude.log10()
            } else {
                min_db
            };
            *sample = db.max(min_db) as f32;
        }
        result
    }
}

impl AudioNodeProcessor for AnalyserNode {
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
        let len = inputs.len().min(output.len());
        if len == 0 {
            return 0;
        }

        // Pass through: copy input to output
        output[..len].copy_from_slice(&inputs[..len]);

        // Capture time-domain data (downmix to mono for analysis)
        let channels = self.channels.max(1) as usize;
        let frames = len / channels;
        let buf_len = self.time_domain_buffer.len();

        for frame in 0..frames {
            // Downmix to mono: average all channels
            let mut mono = 0.0f32;
            for ch in 0..channels {
                mono += inputs[frame * channels + ch];
            }
            mono /= channels as f32;

            self.time_domain_buffer[self.write_pos] = mono;
            self.write_pos = (self.write_pos + 1) % buf_len;
        }

        frames
    }
}
