use std::any::Any;

use shared::protocol::audio_cmd::AudioNodeId;

use super::AudioNodeProcessor;

#[path = "analyser_workspace.rs"]
mod workspace;

/// AnalyserNode: provides real-time frequency and time-domain analysis.
///
/// Passes audio through unchanged while capturing samples for FFT analysis.
pub struct AnalyserNode {
    id: AudioNodeId,
    fft_size: usize,
    min_decibels: f32,
    max_decibels: f32,
    smoothing_time_constant: f32,
    write_pos: usize,
    channels: u32,
    workspace: crate::node_retirement::Value<workspace::Workspace>,
    workspace_host: Option<crate::node_retirement::Host>,
}

impl AnalyserNode {
    #[cfg(test)]
    pub fn new(id: AudioNodeId, channels: u32) -> Self {
        Self::new_with_retirement(id, channels, None).expect("unmanaged analyser admission")
    }

    pub(crate) fn new_with_retirement(
        id: AudioNodeId,
        channels: u32,
        host: Option<&crate::node_retirement::Host>,
    ) -> shared::error::EngineResult<Self> {
        let fft_size = 2048;
        let reservation = workspace::Workspace::reserve(host, fft_size)?;
        let workspace = reservation.finish(workspace::Workspace::new(fft_size));
        Ok(Self {
            id,
            fft_size,
            min_decibels: -100.0,
            max_decibels: -30.0,
            smoothing_time_constant: 0.8,
            write_pos: 0,
            channels,
            workspace,
            workspace_host: host.cloned(),
        })
    }
    pub(crate) fn prepare_workspace(
        &mut self,
        host: &crate::node_retirement::Host,
    ) -> shared::error::EngineResult<()> {
        if let Some(previous) = &self.workspace_host
            && !previous.shares_physical_ledger(host)
        {
            return Err(shared::error::EngineError::from_detail(
                shared::error::ErrorCode::InvalidArgument,
                "analyser workspace belongs to another physical ledger",
            ));
        }
        self.workspace
            .prepare_with_bytes(host, self.workspace.capacity_bytes())?;
        self.workspace_host = Some(host.clone());
        Ok(())
    }
    pub(crate) fn fft_size(&self) -> usize {
        self.fft_size
    }
    #[cfg(test)]
    pub fn set_fft_size(&mut self, size: usize) {
        if let Err(error) = self.try_set_fft_size(size) {
            tracing::warn!("FFT workspace admission rejected: {error}");
        }
    }
    pub(crate) fn try_set_fft_size(&mut self, size: usize) -> shared::error::EngineResult<()> {
        let size = size.next_power_of_two().clamp(32, 32768);
        if size == self.fft_size {
            return Ok(());
        }
        let next =
            workspace::Workspace::replacement(&self.workspace, size, self.workspace_host.as_ref())?;
        let previous = std::mem::replace(&mut self.workspace, next);
        self.fft_size = size;
        self.write_pos = 0;
        previous.retire_replacement();
        Ok(())
    }

    #[allow(dead_code)]
    pub fn set_min_decibels(&mut self, v: f32) {
        self.min_decibels = v;
    }

    #[allow(dead_code)]
    pub fn set_max_decibels(&mut self, v: f32) {
        self.max_decibels = v;
    }

    pub fn set_smoothing_time_constant(&mut self, v: f32) {
        self.smoothing_time_constant = v.clamp(0.0, 1.0);
    }

    /// Get the current time domain data as byte values (0-255)
    pub fn get_byte_time_domain_data(&self) -> Vec<u8> {
        let mut result = vec![0u8; self.fft_size];
        let buf_len = self.workspace.time_domain_buffer.len();
        for i in 0..self.fft_size {
            let pos = (self.write_pos + i) % buf_len;
            // Map [-1, 1] to [0, 255]
            let val = ((self.workspace.time_domain_buffer[pos] + 1.0) * 128.0).clamp(0.0, 255.0);
            result[i] = val as u8;
        }
        result
    }

    /// Get the current time domain data as float values
    pub fn get_float_time_domain_data(&self) -> Vec<f32> {
        let mut result = vec![0.0f32; self.fft_size];
        let buf_len = self.workspace.time_domain_buffer.len();
        for i in 0..self.fft_size {
            let pos = (self.write_pos + i) % buf_len;
            result[i] = self.workspace.time_domain_buffer[pos];
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
        let buf_len = self.workspace.time_domain_buffer.len();
        let n = self.fft_size;

        // Fill re[] with windowed time-domain data; zero im[]
        for i in 0..n {
            let pos = (self.write_pos + i) % buf_len;
            self.workspace.fft_re[i] =
                self.workspace.time_domain_buffer[pos] as f64 * self.workspace.window[i];
            self.workspace.fft_im[i] = 0.0;
        }

        let workspace = &mut *self.workspace;
        Self::fft(
            &mut workspace.fft_re,
            &mut workspace.fft_im,
            &workspace.twiddles,
        );
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
        if self.workspace.smoothed.len() != bins {
            self.workspace.smoothed.clear();
            self.workspace.smoothed.resize(bins, 0.0);
        }
        let tau = self.smoothing_time_constant.clamp(0.0, 1.0) as f64;
        let inv_fft = 1.0 / self.fft_size as f64;
        for bin in 0..bins {
            let magnitude = (self.workspace.fft_re[bin] * self.workspace.fft_re[bin]
                + self.workspace.fft_im[bin] * self.workspace.fft_im[bin])
                .sqrt()
                * inv_fft;
            self.workspace.smoothed[bin] =
                tau * self.workspace.smoothed[bin] + (1.0 - tau) * magnitude;
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

        for i in 0..freq_bins {
            let magnitude = self.workspace.smoothed[i];
            let db = if magnitude > 1e-20 {
                20.0 * magnitude.log10()
            } else {
                min_db
            };
            let normalized = ((db - min_db) / range).clamp(0.0, 1.0);
            result[i] = (normalized * 255.0) as u8;
        }
        result
    }

    /// Get frequency domain data as float dB values.
    pub fn get_float_frequency_data(&mut self) -> Vec<f32> {
        let freq_bins = self.fft_size / 2;

        self.run_fft();

        let mut result = vec![0.0f32; freq_bins];
        let min_db = self.min_decibels as f64;

        for i in 0..freq_bins {
            let magnitude = self.workspace.smoothed[i];
            let db = if magnitude > 1e-20 {
                20.0 * magnitude.log10()
            } else {
                min_db
            };
            result[i] = db.max(min_db) as f32;
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
        let buf_len = self.workspace.time_domain_buffer.len();

        for frame in 0..frames {
            // Downmix to mono: average all channels
            let mut mono = 0.0f32;
            for ch in 0..channels {
                mono += inputs[frame * channels + ch];
            }
            mono /= channels as f32;

            self.workspace.time_domain_buffer[self.write_pos] = mono;
            self.write_pos = (self.write_pos + 1) % buf_len;
        }

        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed_with_sine(node: &mut AnalyserNode, frequency: f64, sample_rate: f64, blocks: usize) {
        let frames = node.fft_size;
        let mut input = vec![0.0f32; frames];
        let mut phase = 0.0f64;
        let step = std::f64::consts::TAU * frequency / sample_rate;
        for _ in 0..blocks {
            for sample in input.iter_mut() {
                *sample = phase.sin() as f32;
                phase += step;
            }
            let mut out = vec![0.0f32; frames];
            node.process(&input, &mut out, sample_rate as u32, 1, 0.0);
        }
    }

    /// A tone must land in the bin it belongs to. This is the property the twiddle
    /// recurrence degraded: derived factors drift, which smears energy out of the
    /// correct bin and lifts the floor everywhere else.
    #[test]
    fn a_pure_tone_concentrates_in_its_own_bin() {
        let sample_rate = 48_000.0;
        let mut node = AnalyserNode::new(1, 1);
        node.set_fft_size(2048);
        node.set_smoothing_time_constant(0.0); // measure this frame only
        // Exactly on bin 64: 64 * 48000 / 2048.
        let frequency = 64.0 * sample_rate / 2048.0;
        fed_with_sine(&mut node, frequency, sample_rate, 4);

        let spectrum = node.get_float_frequency_data();
        let peak = spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(bin, _)| bin)
            .unwrap();
        assert!(
            (peak as i64 - 64).abs() <= 1,
            "the tone should peak at bin 64, peaked at {peak}"
        );
    }

    /// `smoothingTimeConstant` was stored, marked dead code, and never read. The
    /// spec defines the frequency data as the *smoothed* magnitudes, so ignoring it
    /// was a wrong answer and not just a missing nicety.
    #[test]
    fn the_smoothing_time_constant_changes_the_result() {
        let sample_rate = 48_000.0;
        let frequency = 64.0 * sample_rate / 2048.0;

        let mut unsmoothed = AnalyserNode::new(1, 1);
        unsmoothed.set_smoothing_time_constant(0.0);
        fed_with_sine(&mut unsmoothed, frequency, sample_rate, 1);
        let sharp = unsmoothed.get_float_frequency_data();

        // A high constant keeps most of the previous (silent) frame, so the first
        // measurement of a tone must come out quieter.
        let mut smoothed = AnalyserNode::new(1, 1);
        smoothed.set_smoothing_time_constant(0.95);
        fed_with_sine(&mut smoothed, frequency, sample_rate, 1);
        let smooth = smoothed.get_float_frequency_data();

        assert!(
            smooth[64] < sharp[64] - 1.0,
            "smoothing must attenuate a newly arrived tone: {} vs {}",
            smooth[64],
            sharp[64]
        );
    }

    #[test]
    fn resizing_the_fft_resets_the_smoothing_history_to_match() {
        let mut node = AnalyserNode::new(1, 1);
        node.set_fft_size(512);
        assert_eq!(node.workspace.smoothed.len(), 256);
        assert_eq!(node.workspace.twiddles.len(), 256);
        node.set_fft_size(4096);
        assert_eq!(node.workspace.smoothed.len(), 2048);
        assert_eq!(node.workspace.twiddles.len(), 2048);
    }
}

#[cfg(test)]
#[path = "analyser_workspace_tests.rs"]
mod workspace_tests;
