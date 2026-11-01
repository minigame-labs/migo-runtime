//! Complete PCM before timing; direct mature whole-buffer calls share the exact
//! kernel parameters and original-duration cascade policy with the bounded job.
use crate::{decoder::DecodedAudio, resample_job::ResampleJob};
use rubato::{Resampler, audioadapter_buffers::direct::InterleavedSlice};
use std::{hint::black_box, sync::atomic::AtomicBool};
fn cpu_ns() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the initialized timespec is writable for this synchronous call.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut t) },
        0
    );
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}
fn reference(mut audio: DecodedAudio, target: u32) -> DecodedAudio {
    let frames = audio.frame_count();
    let source = audio.sample_rate;
    let channels = audio.channels as usize;
    let mut plan = Vec::new();
    let mut rate = source;
    while u64::from(rate) > 4 * u64::from(target) {
        rate = rate.div_ceil(4);
        plan.push(rate);
    }
    plan.push(target);
    for output in plan {
        let ratio = f64::from(output) / f64::from(audio.sample_rate);
        let quantum = (((8192 / channels - 10) as f64 / ratio).floor() as usize).min(128);
        assert!(
            quantum > 0,
            "timed cases use constant-input quanta, where the mature all-buffer helper is applicable"
        );
        let mut core = rubato::Async::<f32>::new_sinc(
            ratio,
            1.,
            &rubato::SincInterpolationParameters::default(),
            quantum,
            channels,
            rubato::FixedAsync::Input,
        )
        .unwrap();
        let view = InterleavedSlice::new(&audio.samples, channels, audio.frame_count()).unwrap();
        let mut samples = core
            .process_all(&view, audio.frame_count(), None)
            .unwrap()
            .take_data();
        let count =
            (frames as u64 * u64::from(output)).div_ceil(u64::from(source)) as usize * channels;
        assert!(samples.len() >= count);
        samples.truncate(count);
        audio = DecodedAudio {
            samples,
            sample_rate: output,
            channels: channels as u32,
        };
    }
    audio
}
#[test]
#[ignore = "isolated same-quality CPU/RSS experiment"]
fn same_profile_product_and_mature_src() {
    let case = std::env::var("MIGO_SRC_BENCH_CASE").unwrap();
    let mode = std::env::var("MIGO_SRC_BENCH_MODE").unwrap();
    let loops: usize = std::env::var("MIGO_SRC_BENCH_LOOPS")
        .unwrap()
        .parse()
        .unwrap();
    let (source, target, channels, frames): (u32, u32, u32, usize) = match case.as_str() {
        "long" => (48000, 44100, 2, 480137),
        "tiny" => (44100, 48000, 2, 47),
        "down" => (768000, 3000, 2, 768001),
        "wide" => (48000, 96000, 32, 9737),
        _ => panic!("unknown case"),
    };
    let original: Vec<f32> = (0..frames * channels as usize)
        .map(|i| ((i * 37 % 257) as f32 - 128.) / 128.)
        .collect();
    let decode = || {
        let input = DecodedAudio {
            samples: original.clone(),
            sample_rate: source,
            channels,
        };
        if mode == "product" {
            let mut job = ResampleJob::new(input, target).unwrap();
            while !job.step(&AtomicBool::new(false)).unwrap() {}
            job.finish().unwrap()
        } else {
            reference(input, target)
        }
    };
    let golden = std::env::var("MIGO_SRC_BENCH_GOLDEN").unwrap();
    if mode == "golden" {
        let data = decode();
        let bytes: Vec<_> = data.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(golden, bytes).unwrap();
        return;
    }
    assert!(["product", "mature"].contains(&mode.as_str()));
    let check = decode();
    let expected = std::fs::read(golden).unwrap();
    assert_eq!(check.sample_rate, target);
    assert_eq!(check.channels, channels);
    assert_eq!(check.samples.len() * 4, expected.len());
    for (v, bytes) in check.samples.iter().zip(expected.chunks_exact(4)) {
        assert_eq!(
            v.to_bits(),
            f32::from_le_bytes(bytes.try_into().unwrap()).to_bits()
        );
    }
    drop((check, expected));
    for _ in 0..3 {
        drop(black_box(decode()));
    }
    let start = cpu_ns();
    for _ in 0..loops {
        drop(black_box(decode()));
    }
    let elapsed = cpu_ns() - start;
    println!(
        "MIGO_SRC_BENCH {{\"case\":\"{case}\",\"mode\":\"{mode}\",\"loops\":{loops},\"samples\":{},\"cpu_ns\":{elapsed},\"pcm_checked\":true}}",
        (frames as u64 * u64::from(target)).div_ceil(u64::from(source)) * u64::from(channels)
    );
}
