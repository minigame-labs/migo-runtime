use super::*;
use std::sync::Arc;

const LIMIT: usize = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize;
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_OGG_FIXTURES").unwrap()).join(name),
    )
    .unwrap()
}
fn scan_to_end(data: &[u8], format: Format, target: u32) -> (Outcome, u32) {
    let mut scan = Scan::new(format).unwrap();
    for _ in 0..=SCAN_STEPS {
        let (offset, window, calls) = (scan.offset, scan.window, scan.calls);
        let outcome = scan.step(data, target, LIMIT).unwrap();
        assert!(scan.calls - calls <= 1, "one library operation per yield");
        assert!(scan.offset >= offset && scan.offset - offset <= window);
        assert!(scan.window <= 65536 && scan.window <= window + INPUT_BYTES);
        if !matches!(outcome, Outcome::Yield) {
            return (outcome, scan.calls);
        }
    }
    panic!("duration preflight exceeded its bounded calls");
}
fn drain<B: AsRef<[u8]>>(mut job: WholeFileJob<B>, expected: &[f32], known: bool) {
    let mut reservations = 0;
    let mut flatten = 0;
    for _ in 0..MAX_STEPS {
        let (scanning, calls, reserve) = match &job.phase {
            Phase::Decode(owner) => (
                owner.scan.is_some(),
                owner.scan.as_ref().map(|s| s.calls).unwrap_or(0),
                owner.collector.needs_reservation(),
            ),
            _ => (false, 0, false),
        };
        flatten += usize::from(matches!(job.phase, Phase::Flatten(..)));
        let done = job.step(&AtomicBool::new(false)).unwrap();
        if scanning {
            let Phase::Decode(owner) = &job.phase else {
                panic!("preflight must yield before decoding");
            };
            assert_eq!(
                (owner.offset, owner.samples, owner.head, owner.pcm.len()),
                (0, 0, 0, 0)
            );
            assert!(!done);
            if calls == 0 {
                assert!(
                    owner.scan.as_ref().is_some_and(|scan| scan.calls == 1),
                    "first framing operation must yield before scanning subsequent packets"
                );
            }
            if let Some(scan) = &owner.scan {
                assert!(scan.calls - calls <= 1);
            }
        }
        if reserve {
            reservations += 1;
            let Phase::Decode(owner) = &job.phase else {
                unreachable!()
            };
            assert!(
                owner.scan.is_none(),
                "preflight state retires before PCM allocation"
            );
            assert_eq!((owner.offset, owner.samples), (0, 0));
            assert!(!done);
        }
        if done {
            assert_eq!(reservations, usize::from(known));
            assert_eq!(
                flatten,
                if known {
                    1
                } else {
                    expected.len().div_ceil(8192).max(1)
                }
            );
            assert_eq!(job.finish().unwrap().samples, expected);
            return;
        }
    }
    panic!("duration job did not finish");
}

#[test]
fn trimmed_library_durations_keep_independent_frame_counts_and_exact_pcm() {
    for (name, frames, rate, channels, known) in [
        ("0.ogg", 17, 44100, 1, true),
        ("1.ogg", 9737, 48000, 2, true),
        ("2.ogg", 9737, 22050, 1, true),
        ("cross.ogg", 240137, 48000, 6, true),
        ("large.ogg", 9737, 48000, 2, false),
        ("max-window-8.ogg", 8192, 48000, 8, true),
    ] {
        let data = fixture(name);
        let raw = crate::candidate::decode(&data, LIMIT).unwrap();
        assert_eq!(
            (raw.sample_rate, raw.channels, raw.samples.len()),
            (rate, channels, frames * channels as usize)
        );
        for target in [rate, if rate == 44100 { 48000 } else { 44100 }] {
            let (outcome, _) = scan_to_end(&data, Format::Vorbis, target);
            let samples = frames * target as usize;
            let samples = samples.div_ceil(rate as usize) * channels as usize;
            if known {
                assert!(
                    matches!(outcome,Outcome::Complete{samples:n,channels:c} if n==samples && c==channels)
                );
            } else {
                assert!(matches!(outcome, Outcome::Fallback));
            }
            let expected = super::super::tests::resampled(&raw.samples, rate, target, channels);
            drain(
                WholeFileJob::new(data.as_slice(), Format::Vorbis, target, LIMIT).unwrap(),
                &expected,
                known,
            );
        }
    }
}

#[cfg(feature = "adts-streaming-probe")]
#[test]
fn adts_packet_durations_reserve_exact_output_at_native_and_converted_rates() {
    let path = std::path::PathBuf::from(std::env::var_os("W09_COMPLETE_FIXTURES").unwrap());
    let rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.join("elementary.json")).unwrap()).unwrap();
    let mut cases = 0;
    for row in rows.as_array().unwrap() {
        let name = row["file"].as_str().unwrap();
        if !name.ends_with(".aac") {
            continue;
        }
        let data = std::fs::read(path.join(name)).unwrap();
        let raw = crate::candidate::decode(&data, LIMIT).unwrap();
        let frames = row["frames"].as_u64().unwrap() as usize;
        assert_eq!(raw.samples.len(), frames * raw.channels as usize);
        for target in [raw.sample_rate, 44100] {
            let (outcome, _) = scan_to_end(&data, Format::Adts, target);
            let samples = (frames * target as usize).div_ceil(raw.sample_rate as usize)
                * raw.channels as usize;
            assert!(
                matches!(outcome,Outcome::Complete{samples:n,channels} if n==samples && channels==raw.channels)
            );
            let expected =
                super::super::tests::resampled(&raw.samples, raw.sample_rate, target, raw.channels);
            drain(
                WholeFileJob::new(data.as_slice(), Format::Adts, target, LIMIT).unwrap(),
                &expected,
                true,
            );
            cases += 1;
        }
    }
    assert_eq!(cases, 6);
}

#[test]
fn startup_byte_and_operation_limits_fall_back_without_codec_work_or_pcm_loss() {
    let data = fixture("large.ogg");
    let (outcome, calls) = scan_to_end(&data, Format::Vorbis, 48000);
    assert!(matches!(outcome, Outcome::Fallback));
    assert!(calls < 32, "large comments must not be parsed twice");
    let data = fixture("1.ogg");
    for (calls, frames) in [(128, 0), (4096, 1)] {
        let mut job = WholeFileJob::new(data.as_slice(), Format::Vorbis, 48000, LIMIT).unwrap();
        let Phase::Decode(owner) = &mut job.phase else {
            unreachable!()
        };
        let scan = owner.scan.as_mut().unwrap();
        scan.calls = calls;
        scan.frames = frames;
        assert!(!job.step(&AtomicBool::new(false)).unwrap());
        let Phase::Decode(owner) = &job.phase else {
            unreachable!()
        };
        assert!(owner.scan.is_none());
        assert_eq!(
            (owner.offset, owner.samples, owner.expected_samples),
            (0, 0, None)
        );
        let raw = crate::candidate::decode(&data, LIMIT).unwrap();
        drain(job, &raw.samples, false);
    }
}

#[test]
fn cancellation_during_scan_at_completion_and_reservation_frees_owned_input() {
    for stage in 0..4 {
        let input: Arc<[u8]> = fixture("cross.ogg").into();
        let weak = Arc::downgrade(&input);
        let mut job = WholeFileJob::new(input, Format::Vorbis, 44100, LIMIT).unwrap();
        if stage == 1 {
            for _ in 0..8 {
                assert!(!job.step(&AtomicBool::new(false)).unwrap());
            }
        }
        if stage >= 2 {
            for _ in 0..MAX_STEPS {
                if matches!(&job.phase,Phase::Decode(owner) if owner.collector.needs_reservation())
                {
                    break;
                }
                assert!(!job.step(&AtomicBool::new(false)).unwrap());
            }
            assert!(
                matches!(&job.phase,Phase::Decode(owner) if owner.collector.needs_reservation())
            );
            if stage == 3 {
                assert!(!job.step(&AtomicBool::new(false)).unwrap());
            }
        }
        let before = migo_alloc_probe::thread_counts();
        assert!(matches!(job.step(&AtomicBool::new(true)),Err(e) if e.code==ErrorCode::Cancelled));
        assert!(weak.upgrade().is_none());
        assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
        assert!(matches!(job.step(&AtomicBool::new(false)),Err(e) if e.code==ErrorCode::Cancelled));
        assert!(job.finish().is_err());
    }
}

#[test]
fn quota_and_corrupt_truncated_or_chained_input_fail_before_pcm_reservation() {
    let good = fixture("1.ogg");
    let mut corrupt = good.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    let mut truncated = good.clone();
    truncated.pop();
    let mut chained = good.clone();
    chained.extend_from_slice(&good);
    for (data, limit) in [
        (good, 1),
        (corrupt, LIMIT),
        (truncated, LIMIT),
        (chained, LIMIT),
    ] {
        let input: Arc<[u8]> = data.into();
        let weak = Arc::downgrade(&input);
        let mut job = WholeFileJob::new(input, Format::Vorbis, 48000, limit).unwrap();
        let mut failed = false;
        for _ in 0..SCAN_STEPS + 2 {
            assert!(
                matches!(&job.phase,Phase::Decode(owner) if owner.samples==0 && owner.offset==0 && !owner.collector.needs_reservation())
            );
            match job.step(&AtomicBool::new(false)) {
                Err(e) => {
                    assert_eq!(e.code, ErrorCode::InvalidArgument);
                    failed = true;
                    break;
                }
                Ok(true) => panic!("invalid preflight cannot publish PCM"),
                Ok(false) => {}
            }
        }
        assert!(failed);
        assert!(weak.upgrade().is_none());
        assert!(
            matches!(job.step(&AtomicBool::new(false)),Err(e) if e.code==ErrorCode::InvalidArgument)
        );
        assert!(job.finish().is_err());
    }
}
