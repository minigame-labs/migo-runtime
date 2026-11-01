use super::*;
use crate::decoder::mp3::take_native_trace;

const MUSIC: &[u8] = include_bytes!("fixtures/music.mp3");
const MUSIC_PCM: &[u8] = include_bytes!("fixtures/music.i16le");

fn frozen(encoded: &[u8], pcm: &[u8], rate: u32, channels: u32) -> (Vec<u8>, DecodedAudio) {
    (
        encoded.to_vec(),
        DecodedAudio {
            samples: pcm
                .chunks_exact(2)
                .map(|v| f32::from(i16::from_le_bytes(v.try_into().unwrap())) / 32768.0)
                .collect(),
            sample_rate: rate,
            channels,
        },
    )
}
fn drain(mut job: Mp3DecodeJob<Vec<u8>>) -> DecodedAudio {
    for _ in 0..100_000 {
        take_native_trace();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let trace = take_native_trace();
        assert!(trace.calls <= 4 && trace.bytes <= 8192 && trace.max_scan <= 4096);
        if done {
            return job.finish().unwrap();
        }
    }
    panic!("MP3 presentation job must make bounded progress");
}

#[test]
fn gapless_product_dispatch_preserves_full_frozen_pcm_and_exact_target_lengths() {
    let cases = [
        (MUSIC, MUSIC_PCM, 48000, 2, 480137),
        (
            include_bytes!("fixtures/tiny-47-mono.mp3").as_slice(),
            include_bytes!("fixtures/tiny-47-mono.i16le").as_slice(),
            48000,
            1,
            47,
        ),
        (
            include_bytes!("fixtures/tiny-47-stereo.mp3").as_slice(),
            include_bytes!("fixtures/tiny-47-stereo.i16le").as_slice(),
            44100,
            2,
            47,
        ),
        (
            include_bytes!("fixtures/gapless-mpeg2.mp3").as_slice(),
            include_bytes!("fixtures/gapless-mpeg2.i16le").as_slice(),
            22050,
            1,
            9737,
        ),
    ];
    for (encoded, pcm, rate, channels, frames) in cases {
        let (_, expected) = frozen(encoded, pcm, rate, channels);
        assert_eq!(expected.samples.len(), frames * channels as usize);
        for target in [rate, 8000, 22050, 44100, 48000, 96000] {
            let actual = crate::decoder::decode_for_rate(encoded, target).unwrap();
            let count = (frames as u64 * target as u64).div_ceil(rate as u64) as usize;
            assert_eq!(
                (actual.samples.len(), actual.channels, actual.sample_rate),
                (count * channels as usize, channels, target)
            );
            let (_, source) = frozen(encoded, pcm, rate, channels);
            let reference = crate::resampler::resample_if_needed(source, target).unwrap();
            assert!(
                actual.samples == reference.samples,
                "all converted PCM must match the frozen source through the existing DSP"
            );
        }
        let actual = crate::decoder::decode(encoded).unwrap();
        assert!(
            actual.samples == expected.samples,
            "all native PCM must match the independent frozen oracle"
        );
        assert!(actual.samples.iter().any(|v| *v != 0.0));
    }
}

#[test]
fn known_presentation_reserves_once_in_its_own_step_and_never_copies_final_pcm() {
    let mut job = Mp3DecodeJob::new(MUSIC.to_vec(), None);
    for _ in 0..1000 {
        let Phase::Decode(owner) = &job.phase else {
            panic!("metadata must precede PCM")
        };
        if owner.decoder.presentation_samples() == Some(Some(960274)) {
            break;
        }
        job.step(&AtomicBool::new(false)).unwrap();
    }
    let Phase::Decode(owner) = &job.phase else {
        panic!("metadata phase")
    };
    assert_eq!(owner.decoder.presentation_samples(), Some(Some(960274)));
    assert!(!owner.hint_checked && owner.metadata.is_none());
    take_native_trace();
    let before = migo_alloc_probe::thread_counts();
    assert!(!job.step(&AtomicBool::new(false)).unwrap());
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        take_native_trace().calls,
        0,
        "reservation must not decode a frame"
    );
    assert!(after.bytes_allocated - before.bytes_allocated >= 960274 * 4);
    let Phase::Decode(owner) = &job.phase else {
        panic!("reservation phase")
    };
    assert!(owner.hint_checked && owner.metadata.is_none() && !owner.collector.needs_reservation());
    for _ in 0..100_000 {
        let before = migo_alloc_probe::thread_counts();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            after.allocations, before.allocations,
            "known PCM reuses one allocation through sealing"
        );
        assert_eq!(after.reallocations, before.reallocations);
        if done {
            assert_eq!(job.finish().unwrap().samples.len(), 960274);
            return;
        }
    }
    panic!("known presentation did not finish");
}

fn stage(job: &Mp3DecodeJob<std::sync::Arc<[u8]>>) -> usize {
    match &job.phase {
        Phase::Decode(owner) if owner.metadata.is_some() => 4,
        Phase::Decode(owner) if owner.hint_checked => 3,
        Phase::Decode(owner) if owner.decoder.presentation_samples().is_some() => 2,
        Phase::Decode(_) => 0,
        Phase::Flatten(..) => 5,
        Phase::Decoded(_) => 6,
        Phase::Resample(_) => 7,
        Phase::Complete(_) => 8,
        Phase::Failed(_) => 9,
    }
}
#[test]
fn gapless_cancellation_at_bootstrap_reservation_decode_dsp_and_completion_releases_input() {
    for wanted in 0..9 {
        let input: std::sync::Arc<[u8]> = MUSIC.into();
        let weak = std::sync::Arc::downgrade(&input);
        let mut job = Mp3DecodeJob::new(input, Some(96000));
        if wanted == 1 {
            assert!(!job.step(&AtomicBool::new(false)).unwrap());
        } else {
            for _ in 0..100_000 {
                if stage(&job) == wanted {
                    break;
                }
                job.step(&AtomicBool::new(false)).unwrap();
            }
            assert_eq!(stage(&job), wanted);
        }
        take_native_trace();
        assert!(
            matches!(job.step(&AtomicBool::new(true)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(weak.upgrade().is_none());
        assert_eq!(
            take_native_trace().calls,
            0,
            "cancellation must not scan/decode"
        );
        assert!(
            matches!(job.step(&AtomicBool::new(false)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(matches!(job.finish(), Err(e) if e.code == ErrorCode::Cancelled));
    }
}

#[test]
fn truncated_declared_presentation_and_pcm_quota_never_publish_a_partial_result() {
    let mut huge = MUSIC.to_vec();
    let xing = huge
        .windows(4)
        .position(|v| v == b"Info" || v == b"Xing")
        .unwrap();
    huge[xing + 8..xing + 12].copy_from_slice(&u32::MAX.to_be_bytes());
    for (input, quota) in [(MUSIC[..MUSIC.len() / 2].to_vec(), false), (huge, true)] {
        let input: std::sync::Arc<[u8]> = input.into();
        let weak = std::sync::Arc::downgrade(&input);
        let before = migo_alloc_probe::thread_counts();
        let mut job = Mp3DecodeJob::new(input, None);
        let mut failed = false;
        for _ in 0..100_000 {
            match job.step(&AtomicBool::new(false)) {
                Err(e) => {
                    assert_eq!(e.code, ErrorCode::InvalidArgument);
                    failed = true;
                    break;
                }
                Ok(true) => panic!("invalid declared PCM cannot complete"),
                Ok(false) => {}
            }
        }
        assert!(failed && weak.upgrade().is_none());
        if quota {
            assert!(
                migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated
                    < 128 * 1024,
                "known invalid quota must fail before reserving PCM"
            );
        }
        assert!(matches!(job.finish(), Err(e) if e.code == ErrorCode::InvalidArgument));
    }
}

#[test]
fn a_source_format_change_retires_the_owned_job_without_partial_success() {
    let cbr = include_bytes!("fixtures/stereo-cbr.mp3");
    let head = super::super::id3v2_tag_length(cbr).unwrap();
    let mut input = cbr[..head].to_vec();
    // More than the mature native probe's ten matching headers establishes
    // the initial format; a short prefix may legitimately be skipped as junk.
    for _ in 0..3 {
        input.extend_from_slice(&cbr[head..]);
    }
    input.extend_from_slice(include_bytes!("fixtures/mono-vbr.mp3"));
    let mut job = Mp3DecodeJob::new(input, None);
    for _ in 0..10000 {
        match job.step(&AtomicBool::new(false)) {
            Err(e) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(job.finish().is_err());
                return;
            }
            Ok(true) => panic!("mixed source formats cannot publish one PCM buffer"),
            Ok(false) => {}
        }
    }
    panic!("format error did not retire");
}

#[test]
fn direct_gapless_owned_job_matches_the_frozen_native_reference() {
    let (encoded, expected) = frozen(MUSIC, MUSIC_PCM, 48000, 2);
    let actual = drain(Mp3DecodeJob::new(encoded, None));
    assert!(actual.samples == expected.samples);
}
