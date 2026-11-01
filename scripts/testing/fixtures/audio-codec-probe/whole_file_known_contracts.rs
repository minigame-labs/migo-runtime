use super::*;
use std::sync::Arc;

const LIMIT: usize = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize;
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_COMPLETE_FIXTURES").unwrap()).join(name),
    )
    .unwrap()
}
fn reference(data: &[u8], target: u32) -> DecodedAudio {
    let raw = crate::candidate::decode(data, LIMIT).unwrap();
    DecodedAudio {
        samples: super::tests::resampled(&raw.samples, raw.sample_rate, target, raw.channels),
        sample_rate: target,
        channels: raw.channels,
    }
}
fn observe<B: AsRef<[u8]>>(
    mut job: WholeFileJob<B>,
    expected: &DecodedAudio,
    known: bool,
) -> usize {
    if !known && let Phase::Decode(owner) = &mut job.phase {
        owner.scan = None;
    }
    let mut reservations = 0;
    let mut copies = 0;
    let mut final_steps = 0;
    let baseline = migo_alloc_probe::thread_counts().live_bytes();
    let mut peak = 0;
    for _ in 0..MAX_STEPS {
        let (reserve, copy, head, offset) = match &job.phase {
            Phase::Decode(owner) => (
                owner.collector.needs_reservation(),
                owner.head < owner.pcm.len(),
                owner.head,
                owner.offset,
            ),
            _ => (false, false, 0, 0),
        };
        let finalizing = matches!(job.phase, Phase::Flatten(..));
        let before = migo_alloc_probe::thread_counts();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let after = migo_alloc_probe::thread_counts();
        peak = peak.max((after.live_bytes() - baseline).max(0) as usize);
        if reserve {
            reservations += 1;
            assert!(!done);
            assert_eq!(after.allocations - before.allocations, 2);
            assert_eq!(after.reallocations, before.reallocations);
            assert_eq!(
                after.bytes_allocated - before.bytes_allocated,
                (expected.samples.len() * 4 + std::mem::size_of::<Vec<f32>>()) as u64
            );
            let Phase::Decode(owner) = &job.phase else {
                unreachable!()
            };
            assert_eq!((owner.head, owner.offset, owner.samples), (head, offset, 0));
        } else if copy {
            copies += 1;
            let Phase::Decode(owner) = &job.phase else {
                unreachable!()
            };
            assert!(owner.head > head && owner.head - head <= 8192);
            if known {
                assert_eq!(after.allocations, before.allocations);
                assert_eq!(after.reallocations, before.reallocations);
            }
        }
        if finalizing {
            final_steps += 1;
            assert_eq!(after.allocations, before.allocations);
            assert_eq!(after.reallocations, before.reallocations);
        }
        if done {
            let actual = job.finish().unwrap();
            assert_eq!(
                (actual.sample_rate, actual.channels),
                (expected.sample_rate, expected.channels)
            );
            assert_eq!(actual.samples, expected.samples);
            assert_eq!(reservations, usize::from(known));
            assert!(copies >= actual.samples.len().div_ceil(8192));
            if known {
                assert_eq!(final_steps, 1);
            } else {
                assert!(final_steps >= actual.samples.len().div_ceil(8192));
            }
            return peak;
        }
    }
    panic!("declared-length whole-file job did not finish");
}

#[test]
fn known_wave_mp3_and_m4a_reserve_once_then_copy_complete_pcm_without_allocation() {
    for (name, format) in [
        ("1.wav", Format::Wave),
        ("known-gapless.mp3", Format::Mp3),
        ("1-tail.m4a", Format::M4a),
        ("2-tail-reverse.m4a", Format::M4a),
    ] {
        let data = fixture(name);
        for target in [44100, 48000] {
            let expected = reference(&data, target);
            observe(
                WholeFileJob::new(data.as_slice(), format, target, LIMIT).unwrap(),
                &expected,
                true,
            );
        }
    }
}

#[test]
fn reservation_cancellation_before_after_and_during_copy_retires_the_owned_job() {
    for stage in 0..3 {
        let input: Arc<[u8]> = fixture("2-tail-reverse.m4a").into();
        let weak = Arc::downgrade(&input);
        let mut job = WholeFileJob::new(input, Format::M4a, 44100, LIMIT).unwrap();
        for _ in 0..MAX_STEPS {
            if matches!(&job.phase, Phase::Decode(owner) if owner.collector.needs_reservation()) {
                break;
            }
            assert!(!job.step(&AtomicBool::new(false)).unwrap());
        }
        assert!(matches!(&job.phase, Phase::Decode(owner) if owner.collector.needs_reservation()));
        if stage > 0 {
            assert!(!job.step(&AtomicBool::new(false)).unwrap());
        }
        if stage > 1 {
            for _ in 0..MAX_STEPS {
                if matches!(&job.phase, Phase::Decode(owner) if owner.samples > 0) {
                    break;
                }
                assert!(!job.step(&AtomicBool::new(false)).unwrap());
            }
            assert!(matches!(&job.phase, Phase::Decode(owner) if owner.samples > 0));
        }
        assert!(weak.upgrade().is_some());
        let before = migo_alloc_probe::thread_counts();
        assert!(
            matches!(job.step(&AtomicBool::new(true)), Err(error) if error.code == ErrorCode::Cancelled)
        );
        assert!(weak.upgrade().is_none());
        assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
        assert!(
            matches!(job.step(&AtomicBool::new(false)), Err(error) if error.code == ErrorCode::Cancelled)
        );
        assert!(matches!(job.finish(), Err(error) if error.code == ErrorCode::Cancelled));
    }
}

#[test]
fn unknown_and_late_lengths_keep_collected_pages_and_bounded_finalization() {
    let data = fixture("adts-0.aac");
    let expected = reference(&data, 48000);
    observe(
        WholeFileJob::new(data.as_slice(), Format::Adts, 48000, LIMIT).unwrap(),
        &expected,
        false,
    );
    let data = fixture("1.wav");
    let expected = reference(&data, 48000);
    let mut job = WholeFileJob::new(data, Format::Wave, 48000, LIMIT).unwrap();
    for _ in 0..MAX_STEPS {
        if matches!(&job.phase, Phase::Decode(owner) if owner.collector.needs_reservation()) {
            break;
        }
        assert!(!job.step(&AtomicBool::new(false)).unwrap());
    }
    let Phase::Decode(owner) = &mut job.phase else {
        unreachable!()
    };
    assert!(owner.collector.needs_reservation());
    // Model a header without an early duration, then a later declared timeline.
    owner.collector = PcmCollector::new();
    let mut late = false;
    let mut latched = false;
    let mut final_steps = 0;
    for _ in 0..MAX_STEPS {
        if let Phase::Decode(owner) = &mut job.phase {
            if !late && owner.samples > 0 {
                owner.expected_samples = None;
                late = true;
            }
            if late && owner.expected_samples.is_some() {
                latched = true;
            }
            assert!(
                !owner.collector.needs_reservation(),
                "late metadata must not replace collected PCM"
            );
        }
        final_steps += usize::from(matches!(job.phase, Phase::Flatten(..)));
        if job.step(&AtomicBool::new(false)).unwrap() {
            assert!(late && latched);
            assert!(final_steps >= expected.samples.len().div_ceil(8192));
            assert_eq!(job.finish().unwrap().samples, expected.samples);
            return;
        }
    }
    panic!("late duration did not finish through the original PCM pages");
}

#[test]
fn known_and_unknown_storage_obey_the_existing_requested_rust_byte_ceiling() {
    for (name, format, known) in [
        ("2-tail-reverse.m4a", Format::M4a, true),
        ("adts-0.aac", Format::Adts, false),
    ] {
        let data = fixture(name);
        let expected = reference(&data, 44100);
        let peak = observe(
            WholeFileJob::new(data.as_slice(), format, 44100, LIMIT).unwrap(),
            &expected,
            known,
        );
        let pcm = expected.samples.len() * 4;
        assert!(peak <= pcm * if known { 1 } else { 2 } + 2 * 1024 * 1024);
        assert!(peak < 144 * 1024 * 1024);
    }
}
