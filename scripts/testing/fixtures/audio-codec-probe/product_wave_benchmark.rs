//! Isolated CPU/RSS experiment, one arm per fresh process and complete PCM checks.
use crate::decoder::DecodedAudio;
use std::hint::black_box;
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
fn value(n: usize, bits: u16) -> i32 {
    if bits == 16 {
        (n.wrapping_mul(11939).wrapping_add(107) as i16) as i32
    } else {
        ((n.wrapping_mul(11939).wrapping_add(107) as i32) << 8) >> 8
    }
}
#[test]
#[ignore = "isolated host CPU/RSS experiment; invoke exactly one test"]
fn product_and_exact_legacy_wave_reader() {
    let case = std::env::var("MIGO_WAVE_BENCH_CASE").unwrap();
    let mode = std::env::var("MIGO_WAVE_BENCH_MODE").unwrap();
    let loops: usize = std::env::var("MIGO_WAVE_BENCH_LOOPS")
        .unwrap()
        .parse()
        .unwrap();
    let (frames, channels, bits): (usize, u16, u16) = match case.as_str() {
        "long" => (480137, 2, 16),
        "tiny" => (47, 1, 16),
        "wide" => (48000, 32, 16),
        "s24" => (480137, 2, 24),
        _ => panic!("unknown case"),
    };
    let bytes = channels * bits / 8;
    let count = frames * channels as usize;
    let mut input = Vec::with_capacity(44 + count * bits as usize / 8);
    input.extend(b"RIFF");
    input.extend((36 + count as u32 * bits as u32 / 8).to_le_bytes());
    input.extend(b"WAVEfmt ");
    input.extend(16_u32.to_le_bytes());
    input.extend(1_u16.to_le_bytes());
    input.extend(channels.to_le_bytes());
    input.extend(48000_u32.to_le_bytes());
    input.extend((48000_u32 * bytes as u32).to_le_bytes());
    input.extend(bytes.to_le_bytes());
    input.extend(bits.to_le_bytes());
    input.extend(b"data");
    input.extend((count as u32 * bits as u32 / 8).to_le_bytes());
    for n in 0..count {
        input.extend(&value(n, bits).to_le_bytes()[..bits as usize / 8]);
    }
    let decode = || match mode.as_str() {
        "product" => crate::decoder::decode(&input).unwrap(),
        "mature" => legacy::decode(&input).unwrap(),
        _ => panic!("unknown mode"),
    };
    let check = decode();
    assert_eq!(
        (check.samples.len(), check.sample_rate, check.channels),
        (count, 48000, channels as u32)
    );
    for (n, v) in check.samples.iter().enumerate() {
        assert_eq!(
            v.to_bits(),
            (value(n, bits) as f32 / if bits == 16 { 32768. } else { 8388608. }).to_bits()
        );
    }
    drop(check);
    for _ in 0..3 {
        drop(black_box(decode()));
    }
    let start = cpu_ns();
    for _ in 0..loops {
        drop(black_box(decode()));
    }
    let elapsed = cpu_ns() - start;
    println!(
        "MIGO_WAVE_BENCH {{\"case\":\"{case}\",\"mode\":\"{mode}\",\"loops\":{loops},\"samples\":{count},\"cpu_ns\":{elapsed},\"pcm_checked\":true}}"
    );
}
