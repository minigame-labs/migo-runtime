//! Test-only mature variable-rate prototype; production players are unchanged.
use std::{ffi::c_void, ptr::NonNull};
#[repr(C)]
struct Data {
    input: *const f32,
    output: *mut f32,
    input_frames: libc::c_long,
    output_frames: libc::c_long,
    used: libc::c_long,
    produced: libc::c_long,
    eof: libc::c_int,
    ratio: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct Stats {
    live: usize,
    usable: usize,
    allocations: usize,
    frees: usize,
    calls: usize,
    last_input: usize,
}
unsafe extern "C" {
    fn src_new(kind: libc::c_int, channels: libc::c_int, error: *mut libc::c_int) -> *mut c_void;
    fn src_delete(owner: *mut c_void) -> *mut c_void;
    fn src_reset(owner: *mut c_void) -> libc::c_int;
    fn src_set_ratio(owner: *mut c_void, ratio: f64) -> libc::c_int;
    fn probe_process(owner: *mut c_void, data: *mut Data) -> libc::c_int;
    fn probe_begin();
    fn probe_end();
    fn probe_counts() -> Stats;
    fn probe_reference(
        input: *const f32,
        frames: usize,
        channels: usize,
        rates: *const u32,
        rate_count: usize,
        block: usize,
        output: *mut f32,
        capacity: usize,
    ) -> usize;
}
fn native<T>(f: impl FnOnce() -> T) -> T {
    unsafe { probe_begin() };
    let result = f();
    unsafe { probe_end() };
    result
}
fn counts() -> Stats {
    unsafe { probe_counts() }
}
fn checked_ratio(ratio: f64) -> Result<f64, &'static str> {
    if !ratio.is_finite() || !(1. / 256. ..=256.).contains(&ratio) {
        return Err("ratio must be finite and within the mature domain");
    }
    Ok(ratio)
}
struct Owner(NonNull<c_void>);
impl Owner {
    fn new(channels: usize) -> Self {
        assert!((1..=32).contains(&channels));
        let mut error = 0;
        let pointer = native(|| unsafe { src_new(0, channels as libc::c_int, &mut error) });
        assert_eq!(error, 0);
        Self(NonNull::new(pointer).expect("native constructor failed"))
    }
    fn ratio(&mut self, ratio: f64) -> Result<(), &'static str> {
        let ratio = checked_ratio(ratio)?;
        if native(|| unsafe { src_set_ratio(self.0.as_ptr(), ratio) }) != 0 {
            return Err("native ratio rejected");
        }
        Ok(())
    }
    fn reset(&mut self) {
        assert_eq!(native(|| unsafe { src_reset(self.0.as_ptr()) }), 0);
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        assert!(native(|| unsafe { src_delete(self.0.as_ptr()) }).is_null());
    }
}
#[derive(Clone, Copy)]
struct Policy<'a> {
    rates: &'a [u32],
    block: usize,
}
impl Policy<'_> {
    fn units(self, frame: usize) -> u32 {
        self.rates[(frame / self.block) % self.rates.len()]
    }
    fn endpoint(self, frames: usize) -> usize {
        if self.rates.len() == 1 {
            return (frames as u128 * 1000).div_ceil(self.rates[0] as u128) as usize;
        }
        let mut remaining = frames as u128 * 1000;
        let mut n = 0;
        while remaining > 0 {
            remaining = remaining.saturating_sub(self.units(n) as u128);
            n += 1;
        }
        n
    }
}
// Zero extension supplies the mature filter's lookahead. Only the independent
// source clock determines presentation duration; native EOF is not that clock.
fn render(
    owner: &mut Owner,
    input: &[f32],
    channels: usize,
    policy: Policy<'_>,
    offer: usize,
) -> Vec<f32> {
    let mut result = vec![0.; policy.endpoint(input.len() / channels) * channels];
    render_into(owner, input, channels, policy, offer, &mut result);
    result
}
// Reuse the same output allocation for the CPU comparison.
fn render_into(
    owner: &mut Owner,
    input: &[f32],
    channels: usize,
    policy: Policy<'_>,
    offer: usize,
    result: &mut [f32],
) {
    let frames = input.len() / channels;
    let end = policy.endpoint(frames);
    assert_eq!(result.len(), end * channels);
    let zero = [0.; 8192];
    let (mut read, mut written, mut padding) = (0, 0, 0);
    let native_before = counts();
    #[cfg(test)]
    let rust_before = migo_alloc_probe::thread_counts().allocation_events();
    while written < end {
        let padded = read == frames;
        let available = if padded {
            offer.min(zero.len() / channels)
        } else {
            offer.min(frames - read)
        };
        let capacity = (end - written)
            .min(128)
            .min(policy.block - written % policy.block);
        owner.ratio(1000. / policy.units(written) as f64).unwrap();
        let mut data = Data {
            input: if padded {
                zero.as_ptr()
            } else {
                input[read * channels..].as_ptr()
            },
            output: result[written * channels..].as_mut_ptr(),
            input_frames: available as libc::c_long,
            output_frames: capacity as libc::c_long,
            used: 0,
            produced: 0,
            eof: 0,
            ratio: 1000. / policy.units(written) as f64,
        };
        assert_eq!(
            native(|| unsafe { probe_process(owner.0.as_ptr(), &mut data) }),
            0
        );
        assert_eq!(
            counts().last_input,
            available,
            "native received a different input offer"
        );
        assert!(
            counts().last_input * channels <= 8192,
            "native input offer exceeded the original sample budget"
        );
        assert!(
            data.used >= 0
                && data.produced >= 0
                && data.used as usize <= available
                && data.produced as usize <= capacity
        );
        assert!(
            data.used > 0 || data.produced > 0,
            "native SRC stalled before authoritative endpoint"
        );
        if padded {
            padding += data.used as usize;
        } else {
            read += data.used as usize;
        }
        written += data.produced as usize;
        assert!(
            padding <= 8192,
            "native zero extension exceeded the bounded allowance"
        );
    }
    #[cfg(test)]
    assert_eq!(
        migo_alloc_probe::thread_counts().allocation_events(),
        rust_before,
        "variable-rate Rust hot path allocated"
    );
    let native_after = counts();

    assert_eq!(
        native_after.allocations, native_before.allocations,
        "variable-rate native hot path allocated"
    );
    assert_eq!(native_after.live, native_before.live);
    assert_eq!(
        read, frames,
        "variable-rate source ingestion did not reach EOF"
    );
}
fn tone(frames: usize, channels: usize, frequency: f64) -> Vec<f32> {
    (0..frames)
        .flat_map(|i| {
            (0..channels).map(move |c| {
                (0.25 * (std::f64::consts::TAU * frequency * i as f64 + c as f64 * 0.37).sin()
                    / (c + 1) as f64) as f32
            })
        })
        .collect()
}
fn reference(input: &[f32], channels: usize, policy: Policy<'_>) -> Vec<f32> {
    let frames = input.len() / channels;
    let capacity = frames * 2 + 8192;
    let mut out = vec![0.; capacity * channels];
    let n = unsafe {
        probe_reference(
            input.as_ptr(),
            frames,
            channels,
            policy.rates.as_ptr(),
            policy.rates.len(),
            policy.block,
            out.as_mut_ptr(),
            capacity,
        )
    };
    out.truncate(n * channels);
    out
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !args.is_empty() {
        benchmark(&args);
        return;
    }
    let data = tone(9737, 2, 0.03125);
    let policy = Policy {
        rates: &[1000, 2000, 503, 500, 1333],
        block: 128,
    };
    let before = counts();
    let mut owner = Owner::new(2);
    let native_bytes = counts().live - before.live;
    let got = render(&mut owner, &data, 2, policy, 137);
    assert_eq!(got, reference(&data, 2, policy));
    owner.reset();
    drop(owner);
    assert_eq!(counts().live, before.live);
    let panic = if cfg!(panic = "abort") {
        "abort"
    } else {
        "unwind"
    };
    println!(
        "{{\"output_frames\":{},\"native_bytes\":{native_bytes},\"released\":true,\"panic\":\"{panic}\"}}",
        got.len() / 2
    );
}
fn process_cpu_ns() -> u128 {
    let mut clock = std::mem::MaybeUninit::<libc::timespec>::uninit();
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, clock.as_mut_ptr()) },
        0
    );
    let clock = unsafe { clock.assume_init() };
    clock.tv_sec as u128 * 1_000_000_000 + clock.tv_nsec as u128
}
fn benchmark(args: &[String]) {
    assert_eq!(
        args.len(),
        5,
        "usage: probe small|wide units channels frames iterations"
    );
    let channels = args[2].parse::<usize>().unwrap();
    let units = args[1].parse::<u32>().unwrap();
    let frames = args[3].parse::<usize>().unwrap();
    let iterations = args[4].parse::<usize>().unwrap();
    assert!(
        [1, 2, 8, 32].contains(&channels)
            && (units == 0 || (500..=2000).contains(&units))
            && frames > 0
            && iterations > 0
    );
    let offer = match args[0].as_str() {
        "small" => 128,
        "wide" => 8192 / channels,
        _ => panic!("unknown input offer"),
    };
    let fixed = [units];
    let policy = Policy {
        rates: if units == 0 {
            &[1000, 2000, 503, 500, 1333]
        } else {
            &fixed
        },
        block: 128,
    };
    let input = tone(frames, channels, 0.03125);
    let before = counts();
    let mut owner = Owner::new(channels);
    let native_bytes = counts().live - before.live;
    let mut output = render(&mut owner, &input, channels, policy, offer);
    let expected = reference(&input, channels, policy);
    assert_eq!(output.len(), expected.len());
    let first_difference = output
        .iter()
        .zip(&expected)
        .position(|(a, b)| a.to_bits() != b.to_bits());
    let max_difference = output
        .iter()
        .zip(&expected)
        .map(|(a, b)| (*a - *b).abs())
        .fold(0f32, f32::max);
    assert!(
        first_difference.is_none(),
        "benchmark complete PCM differs: first={first_difference:?}, max={max_difference}, units={units}, channels={channels}, offer={offer}"
    );
    drop(expected);
    let before = counts();
    let cpu_start = process_cpu_ns();
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        owner.reset();
        render_into(
            &mut owner,
            std::hint::black_box(&input),
            channels,
            policy,
            offer,
            &mut output,
        );
        std::hint::black_box(&output);
    }
    let elapsed_ns = start.elapsed().as_nanos();
    let cpu_ns = process_cpu_ns() - cpu_start;
    let after = counts();
    assert_eq!(after.allocations, before.allocations);
    assert_eq!(
        after.last_input, offer,
        "native benchmark did not receive the selected offer"
    );
    let panic = if cfg!(panic = "abort") {
        "abort"
    } else {
        "unwind"
    };
    let checksum = output.iter().map(|x| *x as f64).sum::<f64>();
    println!(
        "{{\"mode\":\"{}\",\"actual_native_offer\":{},\"units\":{units},\"channels\":{channels},\"input_frames\":{frames},\"output_frames\":{},\"iterations\":{iterations},\"native_bytes\":{native_bytes},\"native_calls\":{},\"cpu_ns\":{cpu_ns},\"elapsed_ns\":{elapsed_ns},\"panic\":\"{panic}\",\"checksum\":{checksum}}}",
        args[0],
        after.last_input,
        output.len() / channels,
        after.calls - before.calls
    );
}
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();
#[cfg(test)]
mod tests {
    use super::*;
    fn dump(pcm: &[f32], name: &str) {
        if let Some(dir) = std::env::var_os("MIGO_VARIABLE_RATE_PCM_DIR") {
            let bytes = pcm.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>();
            std::fs::write(
                std::path::Path::new(&dir).join(format!("{name}.f32le")),
                bytes,
            )
            .unwrap();
        }
    }
    fn same(got: &[f32], want: &[f32]) {
        assert_eq!(
            got.len(),
            want.len(),
            "variable-rate endpoint disagrees with independent C clock"
        );
        assert!(
            got.iter()
                .zip(want)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "variable-rate complete native PCM changed"
        );
    }
    #[test]
    fn fixed_complete_pcm_matches_independent_full_source() {
        for channels in [1, 2, 8, 32] {
            for units in [500, 503, 997, 1000, 1001, 1333, 1999, 2000] {
                for frames in [1, 17, 127, 129, 9737] {
                    let data = tone(frames, channels, 0.03125);
                    let policy = Policy {
                        rates: &[units],
                        block: 128,
                    };
                    let want = reference(&data, channels, policy);
                    for offer in [1, 137, 8192 / channels] {
                        let got = render(&mut Owner::new(channels), &data, channels, policy, offer);
                        same(&got, &want);
                        if offer == 137 {
                            dump(&got, &format!("fixed-{channels}-{units}-{frames}-actual"));
                            dump(
                                &want,
                                &format!("fixed-{channels}-{units}-{frames}-reference"),
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn dynamic_complete_pcm_and_clock_survive_fragmentation() {
        for channels in [1, 2, 8, 32] {
            for block in [128, 4096] {
                for frames in [1, 129, 9737, 24017] {
                    let data = tone(frames, channels, 0.03125);
                    let policy = Policy {
                        rates: &[1000, 2000, 500, 1333, 503],
                        block,
                    };
                    let want = reference(&data, channels, policy);
                    for offer in [1, 137, 8192 / channels] {
                        let got = render(&mut Owner::new(channels), &data, channels, policy, offer);
                        same(&got, &want);
                        if offer == 137 {
                            dump(&got, &format!("dynamic-{channels}-{block}-{frames}-actual"));
                            dump(
                                &want,
                                &format!("dynamic-{channels}-{block}-{frames}-reference"),
                            );
                        }
                    }
                }
            }
        }
        for (channels, frames) in [(2, 480137), (32, 240137)] {
            let data = tone(frames, channels, 0.03125);
            let policy = Policy {
                rates: &[1000, 2000, 503, 500, 1333],
                block: 128,
            };
            let want = reference(&data, channels, policy);
            for offer in [128, 8192 / channels] {
                let got = render(&mut Owner::new(channels), &data, channels, policy, offer);
                same(&got, &want);
                dump(&got, &format!("compact-{channels}-{frames}-{offer}-actual"));
                dump(
                    &want,
                    &format!("compact-{channels}-{frames}-{offer}-reference"),
                );
            }
        }
    }
    fn gain(pcm: &[f32], frequency: f64, phase: f64) -> f64 {
        let (mut ss, mut cc, mut sc, mut ys, mut yc) = (0., 0., 0., 0., 0.);
        for (i, x) in pcm.iter().enumerate() {
            let angle = std::f64::consts::TAU * frequency * i as f64 + phase;
            let (sin, cos) = angle.sin_cos();
            ss += sin * sin;
            cc += cos * cos;
            sc += sin * cos;
            ys += *x as f64 * sin;
            yc += *x as f64 * cos;
        }
        let determinant = ss * cc - sc * sc;
        ((ys * cc - yc * sc) / determinant).hypot((yc * ss - ys * sc) / determinant) / 0.25
    }
    #[test]
    fn variable_cutoff_preserves_high_end_and_rejects_aliases() {
        let policy = Policy {
            rates: &[1000, 2000, 1000, 500, 2000],
            block: 4096,
        };
        for frequency in [0.03125, 0.375, 0.47] {
            let data = tone(48000, 1, frequency);
            let got = render(&mut Owner::new(1), &data, 1, policy, 137);
            let mut source_units = 0u64;
            for (segment, pcm) in got.chunks(4096).enumerate() {
                let units = policy.rates[segment % policy.rates.len()];
                if pcm.len() > 2048 {
                    let part = &pcm[1024..pcm.len() - 1024];
                    if frequency * units as f64 / 1000. > 0.5 {
                        let rms = (part.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
                            / part.len() as f64)
                            .sqrt();
                        assert!(
                            rms / (0.25 / 2f64.sqrt()) < 0.0001,
                            "dynamic native cutoff failed alias rejection"
                        );
                    } else {
                        let g = gain(
                            part,
                            frequency * units as f64 / 1000.,
                            std::f64::consts::TAU
                                * frequency
                                * (source_units as f64 + 1024. * units as f64)
                                / 1000.,
                        );
                        assert!(
                            (g - 1.).abs() < 0.001,
                            "dynamic native cutoff lost passband: {g}, frequency={frequency}, units={units}, segment={segment}"
                        );
                    }
                }
                source_units += pcm.len() as u64 * units as u64;
            }
            dump(&got, &format!("quality-{frequency}-actual"));
        }
    }
    #[test]
    fn native_actual_capacity_and_all_three_pointers_are_released() {
        for channels in [1, 2, 8, 32] {
            let before = counts();
            let owner = Owner::new(channels);
            let active = counts();
            assert_eq!(active.allocations - before.allocations, 3);
            assert!(
                active.live - before.live > channels * 100000,
                "native history capacity was mistaken for PCM length"
            );
            assert!(active.usable >= active.live);
            drop(owner);
            let after = counts();
            assert_eq!(
                after.live, before.live,
                "native kernel heap remained after destruction"
            );
            assert_eq!(after.usable, before.usable);
            assert_eq!(after.frees - before.frees, 3);
        }
    }
    #[test]
    fn ratio_and_processing_calls_have_no_rust_or_native_allocations() {
        let data = tone(9737, 8, 0.03125);
        let mut owner = Owner::new(8);
        let policy = Policy {
            rates: &[1000, 2000, 503, 500, 1333],
            block: 128,
        };
        let got = render(&mut owner, &data, 8, policy, 137);
        same(&got, &reference(&data, 8, policy));
    }
    #[test]
    fn reset_reuses_native_capacity_and_restores_every_sample() {
        let data = tone(9737, 2, 0.03125);
        let policy = Policy {
            rates: &[503, 2000, 500],
            block: 128,
        };
        let mut owner = Owner::new(2);
        let first = render(&mut owner, &data, 2, policy, 137);
        let before = counts();
        owner.reset();
        let after = counts();
        assert_eq!(before.allocations, after.allocations);
        assert_eq!(before.live, after.live);
        let second = render(&mut owner, &data, 2, policy, 17);
        same(&first, &second);
    }
    #[test]
    fn benchmark_mode_reaches_the_actual_native_input() {
        for (mode, expected) in [("small", 128), ("wide", 4096)] {
            let args = [mode, "1333", "2", "9737", "1"].map(str::to_owned);
            benchmark(&args);
            assert_eq!(
                counts().last_input,
                expected,
                "benchmark mode did not reach native input"
            );
        }
    }
    #[test]
    fn invalid_ratio_is_rejected_before_native_state_changes() {
        let mut owner = Owner::new(1);
        for ratio in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.,
            -1.,
            1. / 257.,
            257.,
        ] {
            let before = counts();
            assert!(
                owner.ratio(ratio).is_err(),
                "invalid native ratio was accepted"
            );
            assert_eq!(counts(), before);
        }
        owner.ratio(0.5).unwrap();
        owner.ratio(2.).unwrap();
    }
}
