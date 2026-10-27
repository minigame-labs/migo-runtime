use crate::bandlimited::{Progress, Stream};
use rubato::{
    Async, Fft, FixedAsync, FixedSync, Resampler, SincInterpolationParameters,
    audioadapter_buffers::direct::InterleavedSlice,
};

#[global_allocator]
static COUNTING: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

fn sinc(input: u32, output: u32, channels: usize) -> Async<f32> {
    Async::new_sinc(
        output as f64 / input as f64,
        1.0,
        &SincInterpolationParameters::default(),
        128,
        channels,
        FixedAsync::Input,
    )
    .unwrap()
}
fn fft(input: u32, output: u32, channels: usize) -> Fft<f32> {
    Fft::new(
        input as usize,
        output as usize,
        128,
        channels,
        FixedSync::Both,
    )
    .unwrap()
}
fn signal(frames: usize, rate: u32, channels: usize, frequency: f64) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            (0..channels).map(move |ch| {
                (0.5 * ((frame as f64 * frequency * std::f64::consts::TAU / rate as f64)
                    + ch as f64 * 0.7)
                    .sin()) as f32
            })
        })
        .collect()
}

fn decode<R: Resampler<f32>>(stream: &mut Stream<R>, input: &[f32], chunk: usize) -> Vec<f32> {
    let mut output = Vec::new();
    let mut scratch = Vec::with_capacity(stream.output_capacity());
    for chunk in input.chunks(chunk) {
        let mut offset = 0;
        while offset < chunk.len() {
            scratch.clear();
            let step = stream.step(&chunk[offset..], false, &mut scratch).unwrap();
            assert!(step.consumed > 0);
            offset += step.consumed;
            assert_eq!(step.progress, Progress::NeedInput);
            output.extend_from_slice(&scratch);
        }
    }
    for _ in 0..10_000 {
        scratch.clear();
        let step = stream.step(&[], true, &mut scratch).unwrap();
        assert_eq!(step.consumed, 0);
        output.extend_from_slice(&scratch);
        if step.progress == Progress::Finished {
            scratch.clear();
            assert_eq!(
                stream.step(&[], true, &mut scratch).unwrap().progress,
                Progress::Finished
            );
            assert!(scratch.is_empty(), "EOF flushed the tail twice");
            return output;
        }
        assert_eq!(step.progress, Progress::Yield);
    }
    panic!("resampler did not finish");
}

fn compare<R: Resampler<f32>>(
    mut library: R,
    adapter: R,
    input_rate: u32,
    output_rate: u32,
    channels: usize,
    frames: usize,
) {
    let source = signal(frames, input_rate, channels, 997.0);
    let view = InterleavedSlice::new(&source, channels, frames).unwrap();
    let reference = library.process_all(&view, frames, None).unwrap();
    let expected_frames =
        (frames as u128 * output_rate as u128).div_ceil(input_rate as u128) as usize;
    // The convenience library API uses floating-point ceil for length. Migo's
    // declared rates use an integer ceil; compare the shared timeline exactly.
    use rubato::audioadapter::Adapter;
    assert!(reference.frames() >= expected_frames);
    let mut expected = vec![0.0; expected_frames * channels];
    for frame in 0..expected_frames {
        for channel in 0..channels {
            expected[frame * channels + channel] = reference.read_sample(channel, frame).unwrap();
        }
    }
    let mut stream = Stream::new(adapter, input_rate, output_rate, channels as u32).unwrap();
    for chunk in [1, 17, 511, 8192] {
        stream.reset();
        let actual = decode(&mut stream, &source, chunk * channels);
        assert_eq!(
            actual.len(),
            expected.len(),
            "duration {input_rate}->{output_rate}/{frames}"
        );
        assert_eq!(
            actual, expected,
            "timeline {input_rate}->{output_rate}/{frames}/chunk{chunk}"
        );
    }
}

#[test]
fn tiny_partial_long_chunks_and_reset_equal_library_time_aligned_output() {
    for (input, output) in [
        (44100, 48000),
        (48000, 44100),
        (48000, 16000),
        (8000, 48000),
    ] {
        for channels in [1, 2] {
            for frames in [1, 17, 127, 128, 511, 1023, 4097] {
                compare(
                    sinc(input, output, channels),
                    sinc(input, output, channels),
                    input,
                    output,
                    channels,
                    frames,
                );
                compare(
                    fft(input, output, channels),
                    fft(input, output, channels),
                    input,
                    output,
                    channels,
                    frames,
                );
            }
        }
    }
}

#[test]
fn impulse_start_tail_duration_and_channel_separation_are_preserved() {
    for (input, output) in [(44100, 48000), (48000, 16000)] {
        for position in [0, 2000, 4095] {
            let mut source = vec![0.0; 4096 * 2];
            source[position * 2] = 1.0;
            let mut stream = Stream::new(sinc(input, output, 2), input, output, 2).unwrap();
            let actual = decode(&mut stream, &source, 274);
            assert_eq!(
                actual.len(),
                (4096usize * output as usize).div_ceil(input as usize) * 2
            );
            assert!(
                actual.chunks_exact(2).all(|frame| frame[1] == 0.0),
                "channel leakage"
            );
            let peak = actual
                .chunks_exact(2)
                .enumerate()
                .max_by(|(_, a), (_, b)| a[0].abs().total_cmp(&b[0].abs()))
                .unwrap()
                .0;
            let expected = (position as f64 * output as f64 / input as f64).round() as usize;
            assert!(
                peak.abs_diff(expected) <= 1,
                "impulse delay {position}: {peak} != {expected}"
            );
            assert!(
                actual.iter().any(|sample| sample.abs() > 0.05),
                "impulse was lost at an edge"
            );
        }
    }
}

fn rms(source: &[f32]) -> f64 {
    (source
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / source.len() as f64)
        .sqrt()
}

#[test]
fn passband_is_preserved_and_out_of_band_energy_is_rejected() {
    for frequency in [1000.0, 20000.0] {
        let source = signal(48000, 48000, 1, frequency);
        let mut sinc_stream = Stream::new(sinc(48000, 16000, 1), 48000, 16000, 1).unwrap();
        let mut fft_stream = Stream::new(fft(48000, 16000, 1), 48000, 16000, 1).unwrap();
        for (name, result) in [
            ("sinc", decode(&mut sinc_stream, &source, 137)),
            ("fft", decode(&mut fft_stream, &source, 137)),
        ] {
            let level = rms(&result[512..result.len() - 512]);
            if frequency < 8000.0 {
                assert!(
                    (level - 0.5_f64 / 2.0_f64.sqrt()).abs() < 0.005,
                    "{name} passband {level}"
                );
            } else {
                let rejection = 20.0 * (level / (0.5 / 2.0_f64.sqrt())).log10();
                assert!(rejection < -85.0, "{name} alias rejection {rejection} dB");
            }
        }
    }
}

fn no_allocations<R: Resampler<f32>>(core: R, path: &'static str) {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let stride = core.input_frames_next() * 2;
    let input = signal(stride / 2 * 64, 44100, 2, 997.0);
    let mut stream = Stream::new(core, 44100, 48000, 2).unwrap();
    let mut output = Vec::with_capacity(stream.output_capacity());
    let mut frames = 0;
    assert_no_steady_state_allocation(
        Burst {
            path,
            warmup: 8,
            measured: 48,
        },
        |iteration| {
            output.clear();
            let chunk = &input[iteration * stride..(iteration + 1) * stride];
            let step = stream.step(chunk, false, &mut output).unwrap();
            assert_eq!(step.consumed, chunk.len());
            frames += output.len();
        },
    );
    assert!(frames > 0);
}

#[test]
fn bounded_preallocated_steps_never_allocate_after_construction() {
    no_allocations(
        sinc(44100, 48000, 2),
        "Rubato sinc input/buffer/filter/output",
    );
    no_allocations(
        fft(44100, 48000, 2),
        "Rubato FFT input/buffer/filter/output",
    );
}

fn compare_appended<R: Resampler<f32>>(first: R, second: R) {
    let stride = first.input_frames_next() * 2;
    let source = signal(stride / 2 * 8, 44100, 2, 997.0);
    let mut individual = Stream::new(first, 44100, 48000, 2).unwrap();
    let mut grouped = Stream::new(second, 44100, 48000, 2).unwrap();
    let mut one = Vec::with_capacity(individual.output_capacity());
    let mut expected = Vec::new();
    let mut actual = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let capacity = actual.capacity();
    let pointer = actual.as_ptr();
    for chunk in source.chunks_exact(stride) {
        one.clear();
        individual.step(chunk, false, &mut one).unwrap();
        expected.extend_from_slice(&one);
        grouped.append_step(chunk, false, &mut actual).unwrap();
    }
    assert_eq!(actual, expected);
    assert_eq!(actual.capacity(), capacity);
    assert_eq!(actual.as_ptr(), pointer);
    // A full destination must be rejected before the library advances or an
    // extend can grow it, and failure discards all PCM in this publication batch.
    actual.resize(capacity, 0.0);
    assert!(
        grouped
            .append_step(&source[..stride], false, &mut actual)
            .is_err()
    );
    assert!(actual.is_empty());
    assert_eq!(actual.capacity(), capacity);
    assert!(grouped.append_step(&[], true, &mut actual).is_err());
}

#[test]
fn appended_quanta_equal_individual_steps_and_respect_destination_capacity() {
    compare_appended(sinc(44100, 48000, 2), sinc(44100, 48000, 2));
    compare_appended(fft(44100, 48000, 2), fft(44100, 48000, 2));
}

#[test]
fn malformed_protocol_budget_and_sticky_failure_are_rejected() {
    assert!(Stream::new(sinc(48000, 44100, 2), 44100, 48000, 2).is_err());
    let mut stream = Stream::new(sinc(44100, 48000, 2), 44100, 48000, 2).unwrap();
    let mut output = Vec::with_capacity(stream.output_capacity());
    assert!(stream.step(&[0.0], false, &mut output).is_err());
    assert!(stream.step(&[], true, &mut output).is_err());
    assert!(output.is_empty());
    stream.reset();
    assert_eq!(
        stream.step(&[], true, &mut output).unwrap().progress,
        Progress::Finished
    );
    assert!(stream.step(&[0.0, 0.0], false, &mut output).is_err());
    for args in [
        (0, 48000, 2),
        (44100, 0, 2),
        (44100, 48000, 0),
        (44100, 48000, 33),
    ] {
        assert!(crate::bandlimited::validate_format(args.0, args.1, args.2).is_err());
    }
    assert!(
        Stream::new(sinc(8000, 192000, 32), 8000, 192000, 32).is_err(),
        "unbounded scratch accepted"
    );
}
