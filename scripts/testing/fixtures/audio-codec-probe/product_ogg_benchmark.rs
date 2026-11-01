//! One arm per process, with a frozen original-reader PCM oracle before timing.
use crate::decoder::{DecodedAudio, whole::WholeDecodeJob};
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
#[test]
#[ignore = "isolated same-codec whole-file CPU/RSS experiment"]
fn product_and_original_vorbis_reader() {
    let case = std::env::var("MIGO_OGG_BENCH_CASE").unwrap();
    let mode = std::env::var("MIGO_OGG_BENCH_MODE").unwrap();
    let loops: usize = std::env::var("MIGO_OGG_BENCH_LOOPS")
        .unwrap()
        .parse()
        .unwrap();
    let (original, frames, channels, source, target): (&[u8], usize, usize, usize, usize) =
        match case.as_str() {
            "long" => (
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/decoder/ogg-fixtures/cross.ogg"
                )),
                240137,
                6,
                48000,
                48000,
            ),
            "tiny" => (
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/decoder/ogg-fixtures/0.ogg"
                )),
                17,
                1,
                44100,
                44100,
            ),
            "wide" => (
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/decoder/ogg-fixtures/max-window-8.ogg"
                )),
                8192,
                8,
                48000,
                48000,
            ),
            "convert" => (
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/decoder/ogg-fixtures/1.ogg"
                )),
                9737,
                2,
                48000,
                44100,
            ),
            _ => panic!("unknown case"),
        };
    let decode = || -> DecodedAudio {
        let input = original.to_vec();
        if mode == "product" {
            let mut job = WholeDecodeJob::new(input, Some(target as u32)).unwrap();
            while !job.step(&AtomicBool::new(false)).unwrap() {}
            job.finish().unwrap()
        } else {
            let native = super::reference::decode(&input).unwrap();
            drop(input);
            crate::resampler::resample_if_needed(native, target as u32).unwrap()
        }
    };
    let golden = std::env::var("MIGO_OGG_BENCH_GOLDEN").unwrap();
    if mode == "golden" {
        let data = decode();
        let bytes: Vec<_> = data.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(golden, bytes).unwrap();
        return;
    }
    assert!(["product", "mature"].contains(&mode.as_str()));
    let check = decode();
    assert_eq!(
        (check.sample_rate, check.channels, check.samples.len()),
        (
            target as u32,
            channels as u32,
            (frames * target).div_ceil(source) * channels
        )
    );
    let expected = std::fs::read(golden).unwrap();
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
        "MIGO_OGG_BENCH {{\"case\":\"{case}\",\"mode\":\"{mode}\",\"loops\":{loops},\"samples\":{},\"cpu_ns\":{elapsed},\"pcm_checked\":true}}",
        (frames * target).div_ceil(source) * channels
    );
}
