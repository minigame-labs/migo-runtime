//! Full-result controls over the actual pull, bounded job and product entries.
//! PCM verification/dumps run separately from the timed decode-and-drop loop.
use crate::{
    decoder::DecodedAudio,
    whole_file_candidate::{Format, WholeFileJob},
};
use std::{hint::black_box, io::Write, sync::atomic::AtomicBool, time::Instant};

const LIMIT: usize = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize;

fn pull(data: &[u8], target: u32) -> Result<DecodedAudio, String> {
    let raw = crate::candidate::decode(data, LIMIT)?;
    crate::bandlimited::validate_format(raw.sample_rate, target, raw.channels)?;
    if raw.sample_rate == target {
        return Ok(raw);
    }
    let count = (raw.frame_count() as u128 * u128::from(target))
        .div_ceil(u128::from(raw.sample_rate))
        * u128::from(raw.channels);
    if count > LIMIT as u128 {
        return Err("pull benchmark resampling exceeds the PCM budget".into());
    }
    let mut resampler = crate::bandlimited::Stream::new(
        crate::resampled_streaming::core(raw.sample_rate, target, raw.channels)
            .map_err(|error| error.to_string())?,
        raw.sample_rate,
        target,
        raw.channels,
    )?;
    let mut samples = Vec::with_capacity(count as usize);
    let mut scratch = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let mut position = 0;
    for _ in 0..131_072 {
        scratch.clear();
        let step = resampler.append_step(
            &raw.samples[position..],
            position == raw.samples.len(),
            &mut scratch,
        )?;
        position += step.consumed;
        if samples.len() + scratch.len() > count as usize {
            return Err("pull benchmark sinc exceeded the declared tail".into());
        }
        samples.extend_from_slice(&scratch);
        if step.progress == crate::bandlimited::Progress::Finished {
            if position != raw.samples.len() || samples.len() != count as usize {
                return Err("pull benchmark sinc did not finish the complete input/tail".into());
            }
            return Ok(DecodedAudio {
                samples,
                sample_rate: target,
                channels: raw.channels,
            });
        }
    }
    Err("pull benchmark DSP work budget exceeded".into())
}

pub fn decode(mode: &str, format: &str, data: &[u8], target: u32) -> Result<DecodedAudio, String> {
    match mode {
        "product" => {
            crate::decoder::decode_for_rate(data, target).map_err(|error| error.to_string())
        }
        "pull" => pull(data, target),
        "job" => {
            let format = match format {
                "mp3" => Format::Mp3,
                "wave" => Format::Wave,
                "vorbis" => Format::Vorbis,
                "adts" => Format::Adts,
                "m4a" => Format::M4a,
                _ => return Err("unknown benchmark input format".into()),
            };
            let mut job = WholeFileJob::new(data, format, target, LIMIT)
                .map_err(|error| error.to_string())?;
            loop {
                if job
                    .step(&AtomicBool::new(false))
                    .map_err(|error| error.to_string())?
                {
                    return job.finish().map_err(|error| error.to_string());
                }
            }
        }
        _ => Err("unknown benchmark arm".into()),
    }
}

pub fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("product, pull or job");
    let format = args.next().expect("codec/container");
    let path = args.next().expect("input file");
    let target: u32 = args.next().unwrap().parse().unwrap();
    let iterations: usize = args.next().unwrap().parse().unwrap();
    let dump = args.next();
    assert!(args.next().is_none());
    assert!((1..=1024).contains(&iterations));
    assert!(std::fs::metadata(&path).unwrap().len() <= 16 * 1024 * 1024);
    let data = std::fs::read(path).unwrap();
    if let Some(path) = dump {
        let audio = decode(&mode, &format, &data, target).unwrap();
        assert!(audio.samples.iter().all(|sample| sample.is_finite()));
        let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        for sample in &audio.samples {
            file.write_all(&sample.to_le_bytes()).unwrap();
        }
        file.flush().unwrap();
        println!(
            "{}",
            serde_json::json!({"verification_only": true, "mode": mode,
            "sample_rate": audio.sample_rate, "channels": audio.channels,
            "frames": audio.frame_count(), "samples": audio.samples.len(),
            "pcm_capacity_samples": audio.samples.capacity()})
        );
        return;
    }
    let mut durations = Vec::with_capacity(iterations);
    let mut shape = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let audio = decode(&mode, &format, black_box(&data), target).unwrap();
        let current = (audio.sample_rate, audio.channels, audio.frame_count());
        assert_eq!(*shape.get_or_insert(current), current);
        black_box(&audio.samples);
        drop(audio);
        durations.push(started.elapsed().as_nanos() as u64);
    }
    let cold = durations[0];
    let total: u64 = durations.iter().sum();
    durations.sort_unstable();
    let (rate, channels, frames) = shape.unwrap();
    println!(
        "{}",
        serde_json::json!({"verification_only": false, "mode": mode,
        "iterations": iterations, "decode_and_drop_total_ns": total,
        "cold_decode_and_drop_ns": cold, "median_operation_ns": durations[iterations / 2],
        "sample_rate": rate, "channels": channels, "frames_per_iteration": frames,
        "encoded_bytes": data.len()})
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::PathBuf::from(std::env::var_os("W09_COMPLETE_FIXTURES").unwrap()).join(name),
        )
        .unwrap()
    }
    #[test]
    fn real_complete_controls_keep_all_five_formats_and_sinc_tails_bit_exact() {
        for (name, format, rate, channels, frames) in [
            ("1.wav", "wave", 44100, 2, 9737),
            ("stereo-cbr.mp3", "mp3", 44100, 2, 6912),
            ("adts-0.aac", "adts", 32000, 1, 8192),
            ("1-tail.m4a", "m4a", 44100, 2, 9737),
        ] {
            let data = fixture(name);
            for target in [rate, 48000] {
                let expected = decode("pull", format, &data, target).unwrap();
                let actual = decode("job", format, &data, target).unwrap();
                assert_eq!((actual.sample_rate, actual.channels), (target, channels));
                assert_eq!(
                    actual.frame_count(),
                    (frames * target as usize).div_ceil(rate as usize)
                );
                assert_eq!(actual.samples, expected.samples, "{name}/{target}");
            }
        }
        let data = std::fs::read(
            std::path::PathBuf::from(std::env::var_os("W09_OGG_FIXTURES").unwrap()).join("1.ogg"),
        )
        .unwrap();
        assert_eq!(
            decode("pull", "vorbis", &data, 44100).unwrap().samples,
            decode("job", "vorbis", &data, 44100).unwrap().samples
        );
    }

    fn high_frequency_wave() -> Vec<u8> {
        let count = 4096_u32;
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + count * 2).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16_u32.to_le_bytes());
        data.extend_from_slice(&1_u16.to_le_bytes());
        data.extend_from_slice(&1_u16.to_le_bytes());
        data.extend_from_slice(&48000_u32.to_le_bytes());
        data.extend_from_slice(&96000_u32.to_le_bytes());
        data.extend_from_slice(&2_u16.to_le_bytes());
        data.extend_from_slice(&16_u16.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&(count * 2).to_le_bytes());
        for index in 0..count {
            data.extend_from_slice(&[0_i16, 16384, 0, -16384][index as usize % 4].to_le_bytes());
        }
        data
    }
    #[test]
    fn product_and_candidate_sinc_controls_share_alias_rejection() {
        let data = high_frequency_wave();
        let product = decode("product", "wave", &data, 16000).unwrap();
        let pull = decode("pull", "wave", &data, 16000).unwrap();
        let job = decode("job", "wave", &data, 16000).unwrap();
        assert_eq!(
            (product.frame_count(), pull.frame_count(), job.frame_count()),
            (1366, 1366, 1366)
        );
        let rms = |audio: &DecodedAudio| {
            let steady = &audio.samples[256..audio.samples.len() - 256];
            (steady
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / steady.len() as f64)
                .sqrt()
        };
        assert!(
            rms(&product) < 0.001,
            "product control must execute the selected band-limited path"
        );
        assert!(
            rms(&pull) < 0.001,
            "candidate control must retain bandlimiting"
        );
        assert_eq!(pull.samples, job.samples);
        assert_eq!(product.samples, pull.samples);
    }
    #[test]
    fn controls_fail_closed_for_unknown_arms_bad_rates_and_corrupt_input() {
        let data = high_frequency_wave();
        assert!(decode("unknown", "wave", &data, 48000).is_err());
        assert!(decode("job", "unknown", &data, 48000).is_err());
        for mode in ["product", "pull", "job"] {
            assert!(decode(mode, "wave", &data, 0).is_err());
            assert!(decode(mode, "wave", b"RIFF", 48000).is_err());
        }
    }
}
