use shared::error::EngineResult;

use crate::decoder::DecodedAudio;

/// Resample audio to target sample rate if needed.
///
/// Whole files use the bounded mature band-limited pipeline. Device conversion
/// uses the same mature kernels through its separately owned continuous stream.
pub fn resample_if_needed(
    audio: DecodedAudio,
    target_sample_rate: u32,
) -> EngineResult<DecodedAudio> {
    let mut job = crate::resample_job::ResampleJob::new(audio, target_sample_rate)?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    while !job.step(&cancel)? {}
    job.finish()
}

#[cfg(test)]
pub(crate) mod fixture {
    use super::DecodedAudio;
    pub(crate) fn audio(rate: u32, channels: u32, frames: usize) -> DecodedAudio {
        DecodedAudio {
            samples: (0..frames * channels as usize)
                .map(|i| ((i * 37 % 257) as f32 - 128.0) / 128.0)
                .collect(),
            sample_rate: rate,
            channels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::DecodedAudio;

    /// Whole-file SRC must retain every output sample through all bounded
    /// partitions. Filter start/tail semantics now come from the mature kernel.
    #[test]
    fn upsample_ramp_has_no_chunk_boundary_glitch() {
        let input_rate = 24_000;
        let output_rate = 48_000; // integer 2x upsampling
        let n = 20_000; // > 2 * CHUNK_FRAMES so several chunk boundaries are crossed
        let samples: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let audio = DecodedAudio {
            samples,
            sample_rate: input_rate,
            channels: 1,
        };

        let expected = crate::resample_job::tests::reference(&audio, output_rate);
        let out = resample_if_needed(audio, output_rate).unwrap();
        assert_eq!(
            out.samples, expected,
            "all ramp samples, including filter boundaries, match the mature zero-extended reference"
        );
        assert_eq!(out.frame_count(), n * 2);
    }

    /// A within-budget input that upsamples past the PCM budget must be rejected
    /// *before* the giant output buffer is allocated (the guard runs first).
    #[test]
    fn resample_rejects_over_budget_upsampling() {
        // 600k mono frames (~2.4 MB) upsampled 256x (3kHz -> 768kHz) => ~153M
        // output samples, above the 64 MiB / 4 = 16M-sample budget.
        let input_frames = 600_000usize;
        let audio = DecodedAudio {
            samples: vec![0.0f32; input_frames],
            sample_rate: 3_000,
            channels: 1,
        };
        assert!(
            resample_if_needed(audio, 768_000).is_err(),
            "over-budget upsample must be rejected"
        );
    }

    /// A deferred tail is a shortened buffer, and a shortened loop restarts early
    /// -- a step discontinuity at the seam, audible as a click on every repeat.
    #[test]
    fn the_final_frames_are_emitted_rather_than_deferred_forever() {
        let audio = DecodedAudio {
            samples: vec![1.0f32; 100],
            sample_rate: 24_000,
            channels: 1,
        };
        let expected = crate::resample_job::tests::reference(&audio, 48000);
        let out = resample_if_needed(audio, 48_000).unwrap();
        assert_eq!(
            out.frame_count(),
            200,
            "the complete source duration must be returned"
        );
        assert_eq!(
            out.samples, expected,
            "band-limited EOF uses the mature zero-extension policy"
        );
    }
}
