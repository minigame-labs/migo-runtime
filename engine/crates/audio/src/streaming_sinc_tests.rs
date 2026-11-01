use super::*;
use crate::{resample_job::tests::reference, resampler::fixture::audio};

fn collect(input: &crate::decoder::DecodedAudio, target: u32, partition: usize) -> Vec<f32> {
    let mut stream = Stream::new(input.sample_rate, target, input.channels, None).unwrap();
    let mut result = Vec::new();
    let mut output = Vec::with_capacity(STEP_SAMPLES);
    let mut position = 0;
    for _ in 0..2_000_000 {
        let end = (position + partition * input.channels as usize).min(input.samples.len());
        let offered = &input.samples[position..end];
        let preparing = stream.is_preparing();
        output.clear();
        bandlimited::take_trace();
        let before = migo_alloc_probe::thread_counts();
        let step = stream
            .step(
                offered,
                position == input.samples.len(),
                &mut output,
                &AtomicBool::new(false),
            )
            .unwrap();
        let after = migo_alloc_probe::thread_counts();
        let trace = bandlimited::take_trace();
        assert!(
            trace.calls <= 1 && trace.input_samples <= 8192 && trace.output_samples <= 8192,
            "incremental kernel work exceeds one step"
        );
        assert!(step.consumed <= offered.len() && step.consumed <= 8192 && output.len() <= 8192);
        if !preparing {
            assert_eq!(
                after.allocations, before.allocations,
                "incremental processing allocated"
            );
            assert_eq!(
                after.reallocations, before.reallocations,
                "incremental processing reallocated"
            );
        }
        position += step.consumed;
        result.extend_from_slice(&output);
        if step.progress == bandlimited::Progress::Finished {
            assert_eq!(position, input.samples.len());
            return result;
        }
    }
    panic!("incremental SRC failed to complete");
}

#[test]
fn unknown_partitions_match_every_raw_library_sample_and_original_clock() {
    for (source, target, channels, frames) in [
        (48000, 16000, 1, 9737),
        (48000, 16000, 2, 0),
        (44100, 48000, 2, 1025),
        (48001, 3000, 32, 16),
        (768000, 3000, 2, 4097),
        (96000, 8000, 1, 1001),
        (1, 16385, 2, 1),
        (65537, 1, 2, 513),
        (48000, 48000, 2, 1025),
    ] {
        let input = audio(source, channels, frames);
        let expected = reference(&input, target);
        for partition in [1, 3, 127, 1024] {
            let result = collect(&input, target, partition);
            assert_eq!(result.len(), expected.len());
            assert!(
                result
                    .iter()
                    .zip(&expected)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "partition PCM differs: {source}->{target}, {partition}"
            );
            assert_eq!(
                result.len() as u128,
                (frames as u128 * target as u128).div_ceil(source as u128) * channels as u128
            );
        }
    }
}

#[test]
fn incremental_setup_initializes_only_one_layer_without_consuming_input() {
    let mut stream = Stream::new(768000, 3000, 2, None).unwrap();
    let mut output = Vec::with_capacity(STEP_SAMPLES);
    for count in 1..=4 {
        bandlimited::take_trace();
        let step = stream
            .step(&[0.25, 0.5], false, &mut output, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            stream.prepared_layers(),
            count,
            "only one streaming layer per step"
        );
        assert_eq!(step.consumed, 0);
        assert!(output.is_empty());
        assert_eq!(bandlimited::take_trace().calls, 0);
    }
    assert!(!stream.is_preparing());
}

#[test]
fn incremental_cancellation_frees_all_scratch_and_is_sticky() {
    for stop in [0, 1, 4, 8] {
        let mut stream = Stream::new(768000, 3000, 2, None).unwrap();
        let input = audio(768000, 2, 97);
        let mut output = Vec::with_capacity(STEP_SAMPLES);
        let mut offset = 0;
        for _ in 0..stop {
            offset += stream
                .step(
                    &input.samples[offset..],
                    offset == input.samples.len(),
                    &mut output,
                    &AtomicBool::new(false),
                )
                .unwrap()
                .consumed;
            output.clear();
        }
        let before = migo_alloc_probe::thread_counts();
        assert_eq!(
            stream
                .step(&[], false, &mut output, &AtomicBool::new(true))
                .unwrap_err()
                .code,
            ErrorCode::Cancelled
        );
        let after = migo_alloc_probe::thread_counts();
        assert!(
            after.bytes_freed > before.bytes_freed,
            "stream cancellation retained its scratch"
        );
        assert!(stream.layers.is_empty() && stream.layers.capacity() == 0);
        assert!(stream.rates.is_empty() && stream.rates.capacity() == 0);
        assert!(output.is_empty());
        assert_eq!(
            stream
                .step(&[], true, &mut output, &AtomicBool::new(false))
                .unwrap_err()
                .code,
            ErrorCode::Cancelled
        );
    }
}

#[test]
fn invalid_frames_early_eof_capacity_and_clock_overflows_retire_the_owner() {
    for format in [
        (0, 48000, 1),
        (48000, 0, 1),
        (48000, 48000, 0),
        (48000, 48000, 33),
    ] {
        assert!(Stream::new(format.0, format.1, format.2, None).is_err());
    }
    for (source, target, channels, end, input, eof, capacity) in [
        (48000, 16000, 2, None, vec![0.5], false, 8192),
        (48000, 16000, 1, None, vec![0.5], true, 8192),
        (48000, 48000, 1, Some(0), vec![0.5], false, 8192),
        (48000, 48000, 1, Some(2), vec![], true, 8192),
        (48000, 48000, 1, None, vec![0.5, 0.25], false, 1),
    ] {
        let mut stream = Stream::new(source, target, channels, end).unwrap();
        let mut output = Vec::with_capacity(capacity);
        assert_eq!(
            stream
                .step(&input, eof, &mut output, &AtomicBool::new(false))
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        assert!(output.is_empty() && stream.layers.is_empty());
        assert_eq!(
            stream
                .step(&[], true, &mut output, &AtomicBool::new(false))
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    let mut stream = Stream::new(48000, 48000, 1, None).unwrap();
    stream.input_frames = u64::MAX;
    let mut output = Vec::with_capacity(STEP_SAMPLES);
    assert!(
        stream
            .step(&[0.5], false, &mut output, &AtomicBool::new(false))
            .is_err()
    );
    let mut stream = Stream::new(1, 65537, 1, None).unwrap();
    stream.input_frames = u64::MAX;
    assert!(
        stream
            .step(&[], true, &mut output, &AtomicBool::new(false))
            .is_err()
    );
}

#[test]
fn a_continuous_stream_has_no_whole_file_pcm_or_total_step_limit() {
    let mut stream = Stream::new(48000, 48000, 1, None).unwrap();
    let input = vec![0.25; 8192];
    let mut output = Vec::with_capacity(8192);
    let iterations = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize / 8192 + 1;
    for _ in 0..iterations {
        output.clear();
        let before = migo_alloc_probe::thread_counts();
        let step = stream
            .step(&input, false, &mut output, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(step.consumed, input.len());
        assert_eq!(output, input);
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(after.allocations, before.allocations);
        assert_eq!(after.reallocations, before.reallocations);
    }
    for _ in 0..262145 {
        output.clear();
        assert_eq!(
            stream
                .step(&input[..1], false, &mut output, &AtomicBool::new(false))
                .unwrap()
                .consumed,
            1
        );
    }
    output.clear();
    let step = stream
        .step(&[], true, &mut output, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(step.progress, bandlimited::Progress::Finished);
    assert!(stream.input_frames > crate::limits::MAX_AUDIO_PCM_SAMPLES);
    assert_eq!(stream.input_frames, stream.output_frames);
}

#[test]
fn endpoint_binding_rejects_retraction_and_cancel_overrides_completion() {
    let mut filter =
        bandlimited::Stream::new(kernel(48000, 16000, 1).unwrap(), 48000, 16000, 1).unwrap();
    let mut output = Vec::with_capacity(STEP_SAMPLES);
    for _ in 0..8 {
        filter.step(&[0.25; 128], false, &mut output).unwrap();
        output.clear();
    }
    assert!(
        filter.set_output_end(0).is_err(),
        "published PCM was retracted"
    );
    let mut stream = Stream::new(48000, 48000, 1, None).unwrap();
    stream
        .step(&[0.25], false, &mut output, &AtomicBool::new(false))
        .unwrap();
    stream
        .step(&[], true, &mut output, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        stream
            .step(&[], true, &mut output, &AtomicBool::new(false))
            .unwrap()
            .progress,
        bandlimited::Progress::Finished
    );
    assert_eq!(
        stream
            .step(&[], true, &mut output, &AtomicBool::new(true))
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
}
