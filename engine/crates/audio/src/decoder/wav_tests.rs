use super::*;
use std::sync::Arc;

// Exact previous product reader, kept only as an independent compatibility oracle.
#[path = "wav_reference.rs"]
mod legacy;

fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend(tag);
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    if !data.len().is_multiple_of(2) {
        out.push(0);
    }
}
fn header(
    bits: u16,
    storage: u16,
    valid: Option<u16>,
    channels: u16,
    format: u16,
    rate: u32,
) -> Vec<u8> {
    let align = channels * storage / 8;
    let mut fmt = Vec::new();
    fmt.extend(if valid.is_some() { 0xfffe_u16 } else { format }.to_le_bytes());
    fmt.extend(channels.to_le_bytes());
    fmt.extend(rate.to_le_bytes());
    fmt.extend((rate * u32::from(align)).to_le_bytes());
    fmt.extend(align.to_le_bytes());
    fmt.extend(bits.to_le_bytes());
    if let Some(valid) = valid {
        fmt.extend(22_u16.to_le_bytes());
        fmt.extend(valid.to_le_bytes());
        fmt.extend(0_u32.to_le_bytes());
        fmt.extend(u32::from(format).to_le_bytes());
        fmt.extend([0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]);
    }
    fmt
}
fn file(fmt: &[u8], payload: &[u8], metadata: usize) -> Vec<u8> {
    let mut out = b"RIFF\0\0\0\0WAVE".to_vec();
    if metadata != 0 {
        chunk(&mut out, b"JUNK", &vec![0xa3; metadata]);
    }
    chunk(&mut out, b"fmt ", fmt);
    chunk(&mut out, b"data", payload);
    let len = out.len() as u32 - 8;
    out[4..8].copy_from_slice(&len.to_le_bytes());
    out
}
fn pcm(frames: usize, channels: u16) -> (Vec<u8>, Vec<f32>) {
    let values: Vec<i16> = (0..frames * channels as usize)
        .map(|n| (n as i32).wrapping_mul(11939).wrapping_add(107) as i16)
        .collect();
    (
        values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        values.iter().map(|v| f32::from(*v) / 32768.0).collect(),
    )
}
fn normal(frames: usize, channels: u16) -> Vec<u8> {
    file(
        &header(16, 16, None, channels, 1, 48000),
        &pcm(frames, channels).0,
        0,
    )
}
fn trace() -> Trace {
    TRACE.with(|t| std::mem::take(&mut *t.borrow_mut()))
}
fn step<B: AsRef<[u8]>>(job: &mut WavDecodeJob<B>) -> EngineResult<bool> {
    trace();
    let result = job.step(&AtomicBool::new(false));
    let t = trace();
    assert!(t.parser_calls <= 1 && t.input_view <= 65536 && t.decoded_samples <= 8192);
    assert!(t.reservations == 0 || (t.parser_calls == 0 && t.decoded_samples == 0));
    result
}
fn run<B: AsRef<[u8]>>(mut job: WavDecodeJob<B>) -> EngineResult<DecodedAudio> {
    for _ in 0..131100 {
        if step(&mut job)? {
            return job.finish();
        }
    }
    panic!("WAVE job made no bounded progress")
}
fn same(actual: &DecodedAudio, expected: &DecodedAudio) {
    assert_eq!(
        (actual.sample_rate, actual.channels, actual.samples.len()),
        (
            expected.sample_rate,
            expected.channels,
            expected.samples.len()
        )
    );
    assert!(
        actual
            .samples
            .iter()
            .zip(&expected.samples)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "every interleaved PCM bit must match"
    );
}

#[test]
fn all_integer_widths_and_channel_counts_keep_complete_legacy_pcm() {
    for channels in [1, 2, 4, 6, 8, 16, 32] {
        for bits in [8, 16, 24, 32] {
            for extended in [false, true] {
                let payload: Vec<u8> = (0..channels as usize * bits as usize / 8 * 117)
                    .map(|n| (n * 19 + 33) as u8)
                    .collect();
                let data = file(
                    &header(bits, bits, extended.then_some(bits), channels, 1, 48000),
                    &payload,
                    35,
                );
                let reference = file(
                    &header(bits, bits, extended.then_some(bits), channels, 1, 48000),
                    &payload,
                    0,
                );
                let expected = legacy::decode(&reference).unwrap();
                let actual = run(WavDecodeJob::new(data.clone(), None).unwrap()).unwrap();
                same(&actual, &expected);
                same(&crate::decoder::decode(&data).unwrap(), &expected);
            }
        }
    }
}

#[test]
fn padded_24_bit_pcm_uses_mature_storage_shift_without_changing_numeric_values() {
    let values = [-8388608_i32, -8388607, -257, -1, 0, 1, 257, 8388607];
    let payload: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let data = file(&header(24, 32, None, 1, 1, 48000), &payload, 0);
    let expected = DecodedAudio {
        samples: values.iter().map(|v| *v as f32 / 8388608.0).collect(),
        sample_rate: 48000,
        channels: 1,
    };
    same(&decode(&data).unwrap(), &expected);
    same(&legacy::decode(&data).unwrap(), &expected);
}

#[test]
fn extensible_valid_bits_use_the_canonical_left_aligned_pcm_scale() {
    // WAVE extensible stores valid bits at the most significant end of the container.
    // The old reader divided these 32-bit containers by 2^23 and amplified by 256.
    let values = [i32::MIN, -0x12345600, -256, 0, 256, 0x12345600, 0x7fffff00];
    let data = file(
        &header(32, 32, Some(24), 1, 1, 48000),
        &values
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
        0,
    );
    let expected = DecodedAudio {
        samples: values.iter().map(|v| *v as f32 / 2147483648.0).collect(),
        sample_rate: 48000,
        channels: 1,
    };
    same(&decode(&data).unwrap(), &expected);
    assert!(legacy::decode(&data).unwrap().samples != expected.samples);
    let values = [-8388608_i32, -4096, 0, 16, 8388592];
    let payload: Vec<_> = values
        .iter()
        .flat_map(|v| v.to_le_bytes()[..3].to_vec())
        .collect();
    let data = file(&header(24, 24, Some(20), 1, 1, 48000), &payload, 0);
    same(
        &decode(&data).unwrap(),
        &DecodedAudio {
            samples: values.iter().map(|v| *v as f32 / 8388608.0).collect(),
            sample_rate: 48000,
            channels: 1,
        },
    );
}

#[test]
fn float_payloads_and_zero_valid_bits_preserve_every_sample_bit() {
    let samples = [
        -3.5_f32,
        -1.,
        -0.,
        0.,
        0.5,
        1.,
        3.,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::from_bits(0x7fc12345),
    ];
    let payload: Vec<_> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
    for valid in [None, Some(0), Some(32)] {
        let data = file(&header(32, 32, valid, 1, 3, 48000), &payload, 0);
        let expected = DecodedAudio {
            samples: samples.to_vec(),
            sample_rate: 48000,
            channels: 1,
        };
        same(&decode(&data).unwrap(), &expected);
        same(&legacy::decode(&data).unwrap(), &expected);
    }
    let data = file(
        &header(32, 32, Some(0), 32, 1, 48000),
        &vec![0x65; 32 * 4 * 47],
        0,
    );
    same(&decode(&data).unwrap(), &legacy::decode(&data).unwrap());
}

#[test]
fn target_rate_dispatch_matches_the_existing_dsp_and_exact_tail() {
    for channels in [1, 2, 6, 32] {
        for frames in [1, 47, 9737] {
            let data = normal(frames, channels);
            for rate in [8000, 22050, 44100, 48000, 96000] {
                let expected =
                    crate::resampler::resample_if_needed(legacy::decode(&data).unwrap(), rate)
                        .unwrap();
                let actual = crate::decoder::decode_for_rate(&data, rate).unwrap();
                assert_eq!(
                    actual.samples.len(),
                    (frames as u64 * u64::from(rate)).div_ceil(48000) as usize * channels as usize
                );
                same(&actual, &expected);
            }
        }
    }
    for rate in [1, 2999, 768001] {
        let data = file(&header(16, 16, None, 1, 1, rate), &pcm(47, 1).0, 0);
        same(&decode(&data).unwrap(), &legacy::decode(&data).unwrap());
    }
}

#[test]
fn known_pcm_configuration_and_reservation_yield_then_transfer_without_allocation() {
    let data = normal(100137, 2);
    let mut job = WavDecodeJob::new(data, None).unwrap();
    for _ in 0..20 {
        if matches!(&job.phase, Phase::Decode(owner) if owner.codec.is_none() && owner.parser.track().is_some())
        {
            break;
        }
        step(&mut job).unwrap();
    }
    trace();
    assert!(!job.step(&AtomicBool::new(false)).unwrap());
    let t = trace();
    assert_eq!(t.parser_calls, 0, "codec configuration is a separate step");
    assert!(
        matches!(&job.phase, Phase::Decode(owner) if owner.codec.is_some() && owner.collector.needs_reservation() && owner.samples==0)
    );
    trace();
    let before = migo_alloc_probe::thread_counts();
    assert!(!job.step(&AtomicBool::new(false)).unwrap());
    let after = migo_alloc_probe::thread_counts();
    let t = trace();
    assert_eq!(
        (t.reservations, t.parser_calls, t.decoded_samples),
        (1, 0, 0)
    );
    assert!(after.bytes_allocated - before.bytes_allocated >= 100137 * 2 * 4);
    for _ in 0..1000 {
        let before = migo_alloc_probe::thread_counts();
        let done = step(&mut job).unwrap();
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            after.allocations, before.allocations,
            "known PCM must transfer through EOF without allocating or cloning"
        );
        assert_eq!(after.reallocations, before.reallocations);
        if done {
            assert_eq!(job.finish().unwrap().samples, pcm(100137, 2).1);
            return;
        }
    }
    panic!("known PCM did not finish")
}

#[test]
fn unknown_length_uses_paged_collection_and_keeps_the_complete_suffix() {
    let (payload, expected) = pcm(100137, 2);
    let mut data = file(&header(16, 16, None, 2, 1, 48000), &payload, 0);
    data[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    data[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let actual = run(WavDecodeJob::new(data, None).unwrap()).unwrap();
    assert_eq!(actual.samples, expected);
}

#[test]
fn metadata_skips_and_pcm_packets_obey_actual_per_step_limits() {
    let data = file(
        &header(8, 8, None, 2, 1, 48000),
        &vec![0x65; 100137 * 2],
        1024 * 1024 + 3,
    );
    let mut job = WavDecodeJob::new(data, None).unwrap();
    let mut max_samples = 0;
    let mut skipped = 0;
    for _ in 0..10000 {
        let offset = match &job.phase {
            Phase::Decode(owner) => owner.offset,
            _ => usize::MAX,
        };
        trace();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let t = trace();
        assert!(t.parser_calls <= 1 && t.input_view <= 8192 && t.decoded_samples <= 8192);
        if (20..1024 * 1024 + 23).contains(&offset) {
            assert!(t.consumed <= 4096);
            skipped += t.consumed;
        }
        max_samples = max_samples.max(t.decoded_samples);
        if done {
            assert!(skipped >= 1024 * 1024);
            assert!(max_samples > 0 && max_samples <= 8192);
            assert_eq!(job.finish().unwrap().samples.len(), 200274);
            return;
        }
    }
    panic!("metadata progress")
}

fn stage(job: &WavDecodeJob<Arc<[u8]>>) -> usize {
    match &job.phase {
        Phase::Decode(owner) if owner.samples > 0 => 5,
        Phase::Decode(owner) if owner.codec.is_some() && !owner.collector.needs_reservation() => 4,
        Phase::Decode(owner) if owner.codec.is_some() => 3,
        Phase::Decode(owner) if owner.parser.track().is_some() => 2,
        Phase::Decode(owner) if owner.offset > 0 => 1,
        Phase::Decode(_) => 0,
        Phase::Flatten(..) => 6,
        Phase::Decoded(_) => 7,
        Phase::Resample(_) => 8,
        Phase::Complete(_) => 9,
        Phase::Failed(_) => 10,
    }
}
#[test]
fn cancellation_at_every_owned_phase_releases_input_and_stays_terminal() {
    for wanted in 0..=9 {
        let input: Arc<[u8]> = normal(100137, 2).into();
        let weak = Arc::downgrade(&input);
        let mut job = WavDecodeJob::new(input, Some(44100)).unwrap();
        for _ in 0..2000 {
            if stage(&job) == wanted {
                break;
            }
            step(&mut job).unwrap();
        }
        assert_eq!(stage(&job), wanted);
        assert_eq!(
            job.step(&AtomicBool::new(true)).unwrap_err().code,
            ErrorCode::Cancelled
        );
        assert!(
            weak.upgrade().is_none(),
            "cancel must retire the input at phase {wanted}"
        );
        assert!(matches!(job.phase, Phase::Failed(ErrorCode::Cancelled)));
        assert_eq!(step(&mut job).unwrap_err().code, ErrorCode::Cancelled);
        assert_eq!(job.finish().unwrap_err().code, ErrorCode::Cancelled);
    }
}

#[test]
fn declared_and_actual_output_limits_reject_before_partial_pcm_can_escape() {
    for unknown in [false, true] {
        let mut data = normal(100137, 2);
        if unknown {
            data[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
            data[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        let input: Arc<[u8]> = data.into();
        let weak = Arc::downgrade(&input);
        let mut job = WavDecodeJob::with_limit(input, None, 100).unwrap();
        let mut decoded = 0;
        let mut error = None;
        for _ in 0..1000 {
            trace();
            let result = job.step(&AtomicBool::new(false));
            decoded += trace().decoded_samples;
            match result {
                Err(e) => {
                    error = Some(e);
                    break;
                }
                Ok(true) => break,
                Ok(false) => {}
            }
        }
        let error = error.expect("output over its caller limit must fail");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        if unknown {
            assert!(decoded > 0 && decoded <= 8192);
        } else {
            assert_eq!(decoded, 0);
        }
        assert!(weak.upgrade().is_none());
        assert_eq!(step(&mut job).unwrap_err().code, error.code);
        assert!(job.finish().is_err());
    }
    let mut job = WavDecodeJob::new(normal(47, 1), None).unwrap();
    if let Phase::Decode(owner) = &mut job.phase {
        owner.steps = MAX_STEPS;
    }
    assert!(step(&mut job).is_err());
    assert!(job.finish().is_err());
}

#[test]
fn truncated_empty_unsupported_and_oversized_files_never_publish_pcm() {
    let mut cases = vec![Vec::new(), b"RIFFbad".to_vec(), normal(0, 1)];
    let mut truncated = normal(100137, 2);
    truncated.truncate(truncated.len() - 3);
    cases.push(truncated);
    for (bits, channels, format) in [
        (64, 1, 3),
        (16, 0, 1),
        (16, 33, 1),
        (16, 1, 6),
        (16, 1, 7),
        (16, 1, 2),
    ] {
        cases.push(file(
            &header(bits, bits, None, channels, format, 48000),
            &[0; 128],
            0,
        ));
    }
    let mut huge_fmt = header(16, 16, None, 1, 1, 48000);
    huge_fmt.resize(65537, 0);
    cases.push(file(&huge_fmt, &[0; 128], 0));
    for data in cases {
        assert!(run(WavDecodeJob::new(data, None).unwrap()).is_err());
    }
    assert!(WavDecodeJob::new(vec![0; 16 * 1024 * 1024 + 1], None).is_err());
    assert!(WavDecodeJob::new(normal(1, 1), Some(0)).is_err());
    assert!(
        WavDecodeJob::new(normal(47, 1), None)
            .unwrap()
            .finish()
            .is_err()
    );
}

#[test]
fn large_format_fields_are_bounded_and_trailing_chunks_are_ignored() {
    let mut fmt = header(16, 16, None, 2, 1, 48000);
    fmt.resize(65536, 0);
    let data = file(&fmt, &pcm(47, 2).0, 0);
    let mut job = WavDecodeJob::new(data, None).unwrap();
    let mut calls = 0;
    let mut failed = false;
    for _ in 0..32 {
        calls += 1;
        match step(&mut job) {
            Err(_) => {
                failed = true;
                break;
            }
            Ok(true) => panic!("invalid enlarged PCM format accepted"),
            Ok(false) => {}
        }
    }
    assert!(
        failed && calls >= 8,
        "bounded window must grow in separate parser calls"
    );
    let mut data = normal(47, 2);
    chunk(&mut data, b"JUNK", &[5; 31]);
    let actual = run(WavDecodeJob::new(data, None).unwrap()).unwrap();
    assert_eq!(actual.samples, pcm(47, 2).1);
}
