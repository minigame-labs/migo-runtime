//! Isolated playback-rate decision probe; no production player is selected here.
use migo_playback_rate_probe::bandlimited::{Progress, Stream};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Adjustable, Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters};
use std::{f64::consts::TAU, hint::black_box, time::Instant};

const SAMPLES: usize = 8192;
#[derive(Clone, Copy, Debug)]
enum Mode {
    Input,
    Output,
}
fn output_frames(n: usize, source: u32, target: u32) -> usize {
    (n as u128 * target as u128).div_ceil(source as u128) as usize
}
fn kernel(mode: Mode, source: u32, target: u32, channels: usize) -> Async<f32> {
    let ratio = target as f64 / source as f64;
    let (fixed, quantum) = match mode {
        Mode::Input => (
            FixedAsync::Input,
            128.min(((SAMPLES / channels - 10) as f64 / ratio).floor() as usize),
        ),
        Mode::Output => (
            FixedAsync::Output,
            128.min(((SAMPLES / channels - 130) as f64 * ratio).floor() as usize),
        ),
    };
    assert!(quantum > 0);
    let core = Async::new_sinc(
        ratio,
        1.,
        &SincInterpolationParameters::default(),
        quantum,
        channels,
        fixed,
    )
    .unwrap();
    assert!(
        core.input_frames_max() * channels <= SAMPLES
            && core.output_frames_max() * channels <= SAMPLES,
        "rate kernel exceeds original per-step sample budget"
    );
    #[cfg(test)]
    assert_mode(&core, mode);
    core
}
fn fixed_side(core: &Async<f32>) -> &'static str {
    let actual = format!("{core:?}");
    if actual.contains("fixed: Input") {
        "input"
    } else if actual.contains("fixed: Output") {
        "output"
    } else {
        panic!("mature kernel has unknown fixed side")
    }
}
#[cfg(test)]
fn assert_mode(core: &Async<f32>, mode: Mode) {
    let expected = match mode {
        Mode::Input => "input",
        Mode::Output => "output",
    };
    assert_eq!(
        fixed_side(core),
        expected,
        "mature kernel uses wrong fixed side"
    );
}

fn cpu_ns() -> u128 {
    let mut clock = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // On success clock_gettime initializes the complete native timespec.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, clock.as_mut_ptr()) };
    assert_eq!(status, 0);
    let clock = unsafe { clock.assume_init() };
    clock.tv_sec as u128 * 1_000_000_000 + clock.tv_nsec as u128
}

fn tone(frames: usize, channels: usize, frequency: f64) -> Vec<f32> {
    (0..frames)
        .flat_map(|i| {
            (0..channels).map(move |c| {
                (0.25 * (TAU * frequency * i as f64 + c as f64 * 0.37).sin() / (c + 1) as f64)
                    as f32
            })
        })
        .collect()
}
fn render(
    core: Async<f32>,
    source: u32,
    target: u32,
    channels: usize,
    data: &[f32],
    offer: usize,
) -> Vec<f32> {
    let frames = data.len() / channels;
    let end = output_frames(frames, source, target);
    let mut owner = Stream::new(core, source, target, channels as u32)
        .unwrap()
        .with_output_end(end as u64);
    let mut out = Vec::with_capacity(owner.output_capacity());
    let mut result = Vec::with_capacity(end * channels);
    let mut pos = 0;
    #[cfg(test)]
    let allocations = migo_alloc_probe::thread_counts().allocation_events();
    for _ in 0..2_000_000 {
        out.clear();
        let last = data.len().min(pos + offer * channels);
        let step = owner
            .step(&data[pos..last], pos == data.len(), &mut out)
            .unwrap();
        assert!(
            step.consumed <= SAMPLES && out.len() <= SAMPLES,
            "rate step exceeds original work bound"
        );
        pos += step.consumed;
        result.extend_from_slice(&out);
        if step.progress == Progress::Finished {
            assert_eq!(pos, data.len());
            assert_eq!(
                result.len(),
                end * channels,
                "rate presentation lost its exact endpoint"
            );
            #[cfg(test)]
            assert_eq!(
                migo_alloc_probe::thread_counts().allocation_events(),
                allocations,
                "prepared rate adapter allocated in processing"
            );
            return result;
        }
    }
    panic!("rate probe exceeded deterministic step bound")
}
// Independent full-source indexing oracle: no adapter staging or EOF machine.
fn direct(mut core: Async<f32>, data: &[f32], channels: usize, count: usize) -> Vec<f32> {
    let frames = data.len() / channels;
    let delay = core.output_delay();
    let max = delay + count + core.output_frames_max() * 4;
    let mut data_out = vec![0.; max * channels];
    let src = InterleavedSlice::new(data, channels, frames).unwrap();
    let mut dst = InterleavedSlice::new_mut(&mut data_out, channels, max).unwrap();
    let (mut pos, mut output) = (0, 0);
    while pos < frames || output < delay + count {
        let need = core.input_frames_next();
        let take = (frames - pos).min(need);
        let index = Indexing::new()
            .input_offset(pos)
            .output_offset(output)
            .partial_len(take);
        let (used, n) = core
            .process_into_buffer(&src, &mut dst, Some(&index))
            .unwrap();
        assert_eq!(used, need);
        pos += take;
        output += n;
        assert!(output <= max, "mature reference exceeded output buffer");
    }
    data_out[delay * channels..(delay + count) * channels].to_vec()
}
fn rms(data: &[f32]) -> f64 {
    let margin = 1024.min(data.len() / 4);
    let part = &data[margin..data.len() - margin];
    (part.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / part.len() as f64).sqrt()
}
fn gain(data: &[f32], frequency: f64) -> f64 {
    let margin = 1024.min(data.len() / 4);
    let n = data.len() - 2 * margin;
    let (mut a, mut b) = (0., 0.);
    for (i, x) in data[margin..data.len() - margin].iter().enumerate() {
        let phase = TAU * frequency * (i + margin) as f64;
        a += *x as f64 * phase.sin();
        b += *x as f64 * phase.cos();
    }
    2. * a.hypot(b) / n as f64 / 0.25
}
fn shortcuts() -> (f64, f64, f64, f64) {
    let data = tone(48000, 1, 0.375);
    let good = direct(kernel(Mode::Output, 2000, 1000, 1), &data, 1, 24000);
    let mut dynamic = Async::new_sinc(
        1.,
        2.,
        &SincInterpolationParameters::default(),
        128,
        1,
        FixedAsync::Output,
    )
    .unwrap();
    dynamic.set_resample_ratio(0.5, false).unwrap();
    let bad = direct(dynamic, &data, 1, 24000);
    let mut worst = Async::new_sinc(
        0.5,
        4.,
        &SincInterpolationParameters::default(),
        128,
        1,
        FixedAsync::Output,
    )
    .unwrap();
    worst.set_resample_ratio(1., false).unwrap();
    let narrow = direct(worst, &data, 1, 48000);
    let unity = direct(kernel(Mode::Output, 1000, 1000, 1), &data, 1, 48000);
    (
        rms(&good) / rms(&data),
        rms(&bad) / rms(&data),
        gain(&narrow, 0.375),
        gain(&unity, 0.375),
    )
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("quality") {
        let (good, bad, narrow, unity) = shortcuts();
        println!(
            "{{\"fixed_alias_ratio\":{good},\"ratio_only_alias_ratio\":{bad},\"worst_cutoff_unity_gain\":{narrow},\"proper_unity_gain\":{unity}}}"
        );
        return;
    }
    assert_eq!(
        args.len(),
        5,
        "usage: probe input|output units channels frames iterations, or quality"
    );
    let mode = match args[0].as_str() {
        "input" => Mode::Input,
        "output" => Mode::Output,
        _ => panic!("unknown rate mode"),
    };
    let units = args[1].parse::<u32>().unwrap();
    let channels = args[2].parse::<usize>().unwrap();
    let frames = args[3].parse::<usize>().unwrap();
    let iterations = args[4].parse::<usize>().unwrap();
    assert!(
        (500..=2000).contains(&units)
            && [1, 2, 8, 32].contains(&channels)
            && frames > 0
            && iterations > 0
    );
    let data = tone(frames, channels, 0.03125);
    let checked = render(
        kernel(mode, units, 1000, channels),
        units,
        1000,
        channels,
        &data,
        137,
    );
    let reference = direct(
        kernel(mode, units, 1000, channels),
        &data,
        channels,
        output_frames(frames, units, 1000),
    );
    assert_eq!(
        checked.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        reference.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
    drop(checked);
    drop(reference);
    let begin = Instant::now();
    let mut core = kernel(mode, units, 1000, channels);
    let setup = begin.elapsed().as_nanos();
    let actual_side = fixed_side(&core);
    let count = output_frames(frames, units, 1000);
    let max = core
        .process_all_needed_output_len(frames)
        .max(core.output_delay() + count + core.output_frames_max());
    let mut out = vec![0.; max * channels];
    let input = InterleavedSlice::new(&data, channels, frames).unwrap();
    let mut dst = InterleavedSlice::new_mut(&mut out, channels, max).unwrap();
    core.process_all_into_buffer(&input, &mut dst, frames, None)
        .unwrap();
    let cpu_begin = cpu_ns();
    let started = Instant::now();
    for _ in 0..iterations {
        core.reset();
        let (used, produced) = core
            .process_all_into_buffer(black_box(&input), &mut dst, frames, None)
            .unwrap();
        assert_eq!((used, produced), (frames, count));
        black_box(&dst);
    }
    let nanos = started.elapsed().as_nanos();
    let cpu = cpu_ns() - cpu_begin;
    let panic_strategy = if cfg!(panic = "abort") {
        "abort"
    } else {
        "unwind"
    };
    let checksum = out[..count * channels]
        .iter()
        .map(|x| *x as f64)
        .sum::<f64>();
    println!(
        "{{\"mode\":\"{}\",\"fixed_side\":\"{actual_side}\",\"units\":{units},\"channels\":{channels},\"input_frames\":{frames},\"output_frames\":{count},\"iterations\":{iterations},\"setup_ns\":{setup},\"elapsed_ns\":{nanos},\"cpu_ns\":{cpu},\"panic\":\"{panic_strategy}\",\"checksum\":{checksum}}}",
        args[0]
    );
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();
#[cfg(test)]
mod tests {
    use super::*;
    fn cursor_frames(frames: usize, source: u32, target: u32) -> usize {
        let mut position = 0u128;
        let mut count = 0;
        while position < frames as u128 * target as u128 {
            position += source as u128;
            count += 1;
        }
        count
    }
    fn dump(pcm: &[f32], name: &str) {
        if let Some(dir) = std::env::var_os("MIGO_PLAYBACK_RATE_PCM_DIR") {
            let bytes = pcm.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
            std::fs::write(
                std::path::Path::new(&dir).join(format!("{name}.f32le")),
                bytes,
            )
            .unwrap();
        }
    }

    #[test]
    fn fixed_rates_keep_all_pcm_exactly_equal_to_the_full_mature_oracle() {
        for channels in [1, 2, 8, 32] {
            for units in [500, 503, 997, 1000, 1001, 1333, 1999, 2000] {
                for frames in [1, 17, 127, 129, 9737] {
                    let data = tone(frames, channels, 0.03125);
                    let count = output_frames(frames, units, 1000);
                    assert_eq!(
                        count,
                        cursor_frames(frames, units, 1000),
                        "rate clock is not the exact rational endpoint"
                    );
                    for mode in [Mode::Input, Mode::Output] {
                        let want =
                            direct(kernel(mode, units, 1000, channels), &data, channels, count);
                        for offer in [1, 137, 8192] {
                            let got = render(
                                kernel(mode, units, 1000, channels),
                                units,
                                1000,
                                channels,
                                &data,
                                offer,
                            );
                            assert_eq!(
                                got.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                                want.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                                "complete fixed-rate mature PCM changed"
                            );
                            if offer == 137 {
                                dump(
                                    &got,
                                    &format!("{channels}-{units}-{frames}-{mode:?}-actual"),
                                );
                                dump(
                                    &want,
                                    &format!("{channels}-{units}-{frames}-{mode:?}-reference"),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn fixed_sides_have_the_same_quality_and_complete_time_origin() {
        for channels in [1, 2, 8, 32] {
            for units in [503, 1000, 1333, 2000] {
                let data = tone(9737, channels, 0.03125);
                let n = output_frames(9737, units, 1000);
                let input = direct(
                    kernel(Mode::Input, units, 1000, channels),
                    &data,
                    channels,
                    n,
                );
                let output = direct(
                    kernel(Mode::Output, units, 1000, channels),
                    &data,
                    channels,
                    n,
                );
                assert!(
                    input
                        .iter()
                        .zip(&output)
                        .all(|(a, b)| a.is_finite() && b.is_finite() && (*a - *b).abs() < 0.0001),
                    "fixed sides have unequal complete PCM or time origin"
                );
            }
        }
    }
    #[test]
    fn octave_detune_and_buffer_context_ratios_keep_exact_duration() {
        for (source, target) in [
            (44100 * 1000, 48000 * 1000),
            (44100 * 2000, 48000 * 1000),
            (48000 * 500, 44100 * 2000),
        ] {
            let data = tone(9737, 2, 0.03125);
            for mode in [Mode::Input, Mode::Output] {
                let got = render(
                    kernel(mode, source, target, 2),
                    source,
                    target,
                    2,
                    &data,
                    17,
                );
                let want = direct(
                    kernel(mode, source, target, 2),
                    &data,
                    2,
                    output_frames(9737, source, target),
                );
                assert_eq!(got, want);
                dump(&got, &format!("detune-{source}-{target}-{mode:?}-actual"));
                dump(
                    &want,
                    &format!("detune-{source}-{target}-{mode:?}-reference"),
                );
            }
        }
    }
    #[test]
    fn fixed_rate_sinc_preserves_passband_and_rejects_aliases() {
        for mode in [Mode::Input, Mode::Output] {
            for units in [500, 1000, 1250, 1333, 2000] {
                let low = tone(48000, 1, 0.03125);
                let got = render(kernel(mode, units, 1000, 1), units, 1000, 1, &low, 137);
                let g = gain(&got, 0.03125 * units as f64 / 1000.);
                assert!((g - 1.).abs() < 0.001, "rate sinc lost its passband: {g}");
                if units > 1000 {
                    let frequency = (0.5 * 1000. / units as f64 + 0.5) / 2.;
                    let high = tone(48000, 1, frequency);
                    let got = render(kernel(mode, units, 1000, 1), units, 1000, 1, &high, 137);
                    let alias = rms(&got) / rms(&high);
                    assert!(alias < 0.0001, "rate sinc failed alias rejection: {alias}");
                }
            }
        }
    }
    #[test]
    fn ratio_only_and_worst_cutoff_shortcuts_are_explicitly_rejected() {
        let (good, bad, narrow, unity) = shortcuts();
        assert!(
            good < 0.0001 && bad > 0.5,
            "ratio-only shortcut was mistaken for bandlimited playback"
        );
        assert!(
            narrow < 0.001 && unity > 0.99,
            "worst-cutoff shortcut was mistaken for transparent unity"
        );
    }
    #[test]
    fn prepared_kernel_calls_and_ratio_changes_do_not_allocate_or_grow() {
        for mode in [Mode::Input, Mode::Output] {
            let mut core = kernel(mode, 1333, 1000, 32);
            let frames = core.input_frames_max();
            let input_data = vec![0.; frames * 32];
            let mut output = vec![0.; core.output_frames_max() * 32];
            let src = InterleavedSlice::new(&input_data, 32, frames).unwrap();
            let out_frames = core.output_frames_max();
            let mut dst = InterleavedSlice::new_mut(&mut output, 32, out_frames).unwrap();
            core.process_into_buffer(&src, &mut dst, None).unwrap();
            let before = migo_alloc_probe::thread_counts();
            for _ in 0..128 {
                core.process_into_buffer(&src, &mut dst, None).unwrap();
            }
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(
                after.allocation_events(),
                before.allocation_events(),
                "prepared rate kernel allocated in processing"
            );
        }
        let mut core = Async::<f32>::new_sinc(
            1.,
            2.,
            &SincInterpolationParameters::default(),
            128,
            2,
            FixedAsync::Output,
        )
        .unwrap();
        let before = migo_alloc_probe::thread_counts();
        for units in 500..=2000 {
            core.set_resample_ratio(1000. / units as f64, false)
                .unwrap();
        }
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            after.allocation_events(),
            before.allocation_events(),
            "ratio update allocated in processing"
        );
    }
    #[test]
    fn kernel_actual_heap_and_caller_capacity_are_released() {
        let before = migo_alloc_probe::thread_counts();
        let core = kernel(Mode::Output, 2000, 1000, 32);
        let built = migo_alloc_probe::thread_counts();
        assert!(built.live_bytes() > before.live_bytes());
        drop(core);
        let freed = migo_alloc_probe::thread_counts();
        assert_eq!(
            freed.live_bytes(),
            before.live_bytes(),
            "rate probe retained kernel heap after destruction"
        );
        let mut out = Vec::<f32>::with_capacity(4096);
        let capacity = out.capacity();
        out.push(0.);
        let watch = migo_alloc_probe::DeallocationWatch::new(out.as_ptr().cast()).unwrap();
        assert!(capacity > out.len());
        drop(out);
        assert_eq!(watch.freed_bytes(), Some(capacity * 4));
    }
}
