//! Isolated same-codec host experiment. Only one exact test executes per process.
use std::{hint::black_box, mem::MaybeUninit};
unsafe extern "C" {
    fn migo_mp3_test_full_reader(
        data: *const u8,
        size: usize,
        output: *mut i16,
        capacity: usize,
        actual: *mut usize,
    ) -> i32;
}
const MUSIC: &[u8] = include_bytes!(concat!(env!("MIGO_MP3_TEST_FIXTURES"), "/music.mp3"));
const MUSIC_PCM: &[u8] = include_bytes!(concat!(env!("MIGO_MP3_TEST_FIXTURES"), "/music.i16le"));
fn cpu_ns() -> u64 {
    let mut stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut stamp) },
        0
    );
    stamp.tv_sec as u64 * 1_000_000_000 + stamp.tv_nsec as u64
}
fn mature(input: &[u8], expected: usize) -> Vec<f32> {
    let capacity = expected + 2304;
    let mut raw: Vec<MaybeUninit<i16>> = Vec::with_capacity(capacity);
    // SAFETY: MaybeUninit may remain uninitialized; C writes only within the
    // explicit capacity. Read only the actual range the library initialized.
    unsafe { raw.set_len(capacity) };
    let mut actual = 0;
    let status = unsafe {
        migo_mp3_test_full_reader(
            input.as_ptr(),
            input.len(),
            raw.as_mut_ptr().cast(),
            capacity,
            &mut actual,
        )
    };
    assert_eq!(status, 0);
    assert_eq!(actual, expected);
    let samples = unsafe { std::slice::from_raw_parts(raw.as_ptr().cast::<i16>(), actual) };
    samples.iter().map(|&v| f32::from(v) / 32768.0).collect()
}
#[test]
#[ignore = "isolated CPU/RSS experiment; explicit exact invocation only"]
fn same_codec_product_and_mature_reader() {
    let case = std::env::var("MIGO_MP3_BENCH_CASE").unwrap();
    let mode = std::env::var("MIGO_MP3_BENCH_MODE").unwrap();
    let loops: usize = std::env::var("MIGO_MP3_BENCH_LOOPS")
        .unwrap()
        .parse()
        .unwrap();
    let (input, frozen): (&[u8], &[u8]) = match case.as_str() {
        "long" => (MUSIC, MUSIC_PCM),
        "tiny" => (
            include_bytes!(concat!(env!("MIGO_MP3_TEST_FIXTURES"), "/tiny-47-mono.mp3")),
            include_bytes!(concat!(
                env!("MIGO_MP3_TEST_FIXTURES"),
                "/tiny-47-mono.i16le"
            )),
        ),
        "mpeg2" => (
            include_bytes!(concat!(
                env!("MIGO_MP3_TEST_FIXTURES"),
                "/gapless-mpeg2.mp3"
            )),
            include_bytes!(concat!(
                env!("MIGO_MP3_TEST_FIXTURES"),
                "/gapless-mpeg2.i16le"
            )),
        ),
        "untagged" => (
            include_bytes!(concat!(env!("MIGO_MP3_TEST_FIXTURES"), "/stereo-cbr.mp3")),
            include_bytes!(concat!(env!("MIGO_MP3_TEST_FIXTURES"), "/stereo-cbr.f32le")),
        ),
        _ => panic!("unknown benchmark case"),
    };
    let expected = frozen.len() / if case == "untagged" { 4 } else { 2 };
    let decode = || match mode.as_str() {
        "product" => crate::decoder::decode(input).unwrap().samples,
        "mature" => mature(input, expected),
        _ => panic!("unknown benchmark mode"),
    };
    // Complete PCM is checked in every fresh process, before measuring its
    // selected arm. The other arm never runs here, preserving peak-RSS scope.
    let check = decode();
    assert_eq!(check.len(), expected);
    for (index, &actual) in check.iter().enumerate() {
        let reference = if case == "untagged" {
            f32::from_le_bytes(frozen[index * 4..index * 4 + 4].try_into().unwrap())
        } else {
            f32::from(i16::from_le_bytes(
                frozen[index * 2..index * 2 + 2].try_into().unwrap(),
            )) / 32768.0
        };
        assert_eq!(actual.to_bits(), reference.to_bits());
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
        "MIGO_MP3_BENCH {{\"case\":\"{}\",\"mode\":\"{}\",\"loops\":{},\"samples\":{},\"cpu_ns\":{},\"pcm_checked\":true}}",
        case, mode, loops, expected, elapsed
    );
}
