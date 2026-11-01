use super::*;
use std::sync::Arc;

fn fixture(name: &str) -> Vec<u8> {
    let bytes: &[u8] = match name {
        "0" => include_bytes!("ogg-fixtures/0.ogg"),
        "1" => include_bytes!("ogg-fixtures/1.ogg"),
        "2" => include_bytes!("ogg-fixtures/2.ogg"),
        "cross" => include_bytes!("ogg-fixtures/cross.ogg"),
        "max-window-8" => include_bytes!("ogg-fixtures/max-window-8.ogg"),
        _ => unreachable!(),
    };
    bytes.to_vec()
}
fn drain(mut job: OggDecodeJob<Vec<u8>>) -> DecodedAudio {
    for _ in 0..MAX_STEPS {
        if job.step(&AtomicBool::new(false)).unwrap() {
            return job.finish().unwrap();
        }
    }
    panic!("owned Vorbis did not complete")
}
fn phase<B>(job: &OggDecodeJob<B>) -> &'static str {
    match &job.phase {
        Phase::Decode(owner) if owner.scan.is_some() => "scan",
        Phase::Decode(owner) if owner.collector.needs_reservation() => "reserve",
        Phase::Decode(owner) => match owner.action {
            Action::Parse => "parse",
            Action::Packet { .. } if owner.codec.is_none() => "configure",
            Action::Packet { .. } => "packet",
            Action::Copy { frame: 0, .. } => "copy-first",
            Action::Copy { .. } => "copy-later",
        },
        Phase::Flatten(..) => "flatten",
        Phase::Decoded(_) => "decoded",
        Phase::Resample(_) => "src",
        Phase::Complete(_) => "complete",
        Phase::Failed(_) => "failed",
    }
}

#[test]
fn owned_steps_preserve_every_legacy_sample_and_submitted_rate() {
    for name in ["0", "1", "2", "cross", "max-window-8"] {
        let bytes = fixture(name);
        let expected = super::super::reference::decode(&bytes).unwrap();
        let actual = drain(OggDecodeJob::new(bytes.clone(), None).unwrap());
        assert_eq!(
            (actual.sample_rate, actual.channels),
            (expected.sample_rate, expected.channels)
        );
        assert_eq!(actual.samples, expected.samples, "{name} native PCM");
        // Converted long/multichannel behavior is also covered by the common
        // SRC matrices; keep this codec test focused on actual short tails.
        if matches!(name, "0" | "1" | "2") {
            for target in [44100, 48000] {
                let converted =
                    crate::resampler::resample_if_needed(expected.clone(), target).unwrap();
                let actual = drain(OggDecodeJob::new(bytes.clone(), Some(target)).unwrap());
                assert_eq!(actual.sample_rate, target);
                assert_eq!(actual.channels, converted.channels);
                assert_eq!(actual.samples, converted.samples, "{name} at {target}");
            }
        }
    }
}

#[test]
fn parser_codec_and_multichannel_pcm_copies_yield_separately() {
    for name in ["cross", "max-window-8"] {
        let mut job = OggDecodeJob::new(fixture(name), None).unwrap();
        let mut calls = (0, 0, 0);
        let mut split = false;
        let mut max_copy_allocation = 0;
        for _ in 0..MAX_STEPS {
            TRACE.with(|t| *t.borrow_mut() = Trace::default());
            let before = migo_alloc_probe::thread_counts();
            let done = job.step(&AtomicBool::new(false)).unwrap();
            let after = migo_alloc_probe::thread_counts();
            let t = TRACE.with(|t| std::mem::take(&mut *t.borrow_mut()));
            assert!(
                t.parser_calls + t.codec_calls <= 1,
                "two library operations in one step"
            );
            assert!(t.input_view <= 65536 && t.copied_samples <= 8192);
            if t.copied_samples != 0 {
                assert_eq!(
                    t.parser_calls + t.codec_calls,
                    0,
                    "PCM copy must yield from codec work"
                );
                let allocated = after.bytes_allocated - before.bytes_allocated;
                max_copy_allocation = max_copy_allocation.max(allocated);
                // One 64KiB page plus the bounded descriptor deque. At the
                // 64MiB PCM cap, 1024 Vec descriptors need 24KiB on x64.
                assert!(
                    allocated <= 96 * 1024,
                    "copy allocated more than a fixed page and bounded descriptors: {allocated}"
                );
            }
            calls.0 += t.parser_calls;
            calls.1 += t.codec_calls;
            calls.2 += usize::from(t.copied_samples != 0);
            split |= phase(&job) == "copy-later";
            if done {
                break;
            }
        }
        assert!(calls.0 > 1 && calls.1 > 1 && calls.2 > 1);
        if name == "max-window-8" {
            assert!(
                split,
                "long eight-channel blocks need multiple bounded copies"
            );
        }
        assert!(!job.finish().unwrap().samples.is_empty());
        println!("{name} maximum per-copy allocation: {max_copy_allocation} bytes");
    }
}

struct Input(Arc<Vec<u8>>);
impl AsRef<[u8]> for Input {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
#[test]
fn cancellation_at_all_owned_phases_retires_input_pcm_and_stays_terminal() {
    for target in [
        "scan",
        "reserve",
        "parse",
        "configure",
        "packet",
        "copy-first",
        "copy-later",
        "flatten",
        "decoded",
        "src",
        "complete",
    ] {
        let input = Arc::new(fixture("max-window-8"));
        let weak = Arc::downgrade(&input);
        let mut job = OggDecodeJob::new(Input(input), Some(44100)).unwrap();
        for _ in 0..MAX_STEPS {
            if phase(&job) == target {
                break;
            }
            if job.step(&AtomicBool::new(false)).unwrap() {
                assert_eq!(target, "complete", "phase {target} missing");
                break;
            }
        }
        assert_eq!(phase(&job), target);
        TRACE.with(|t| *t.borrow_mut() = Trace::default());
        let before = migo_alloc_probe::thread_counts();
        assert_eq!(
            job.step(&AtomicBool::new(true)).unwrap_err().code,
            ErrorCode::Cancelled
        );
        let t = TRACE.with(|t| std::mem::take(&mut *t.borrow_mut()));
        assert_eq!(t.parser_calls + t.codec_calls + t.copied_samples, 0);
        assert!(
            weak.upgrade().is_none(),
            "encoded source retained after cancel at {target}"
        );
        assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
        assert_eq!(
            job.step(&AtomicBool::new(false)).unwrap_err().code,
            ErrorCode::Cancelled
        );
        assert_eq!(job.finish().unwrap_err().code, ErrorCode::Cancelled);
    }
}

#[test]
fn input_retires_before_final_collection_copy_and_src() {
    let input = Arc::new(fixture("cross"));
    let weak = Arc::downgrade(&input);
    let mut job = OggDecodeJob::new(Input(input), Some(44100)).unwrap();
    let mut retired = false;
    for _ in 0..MAX_STEPS {
        let done = job.step(&AtomicBool::new(false)).unwrap();
        if matches!(
            job.phase,
            Phase::Flatten(..) | Phase::Decoded(_) | Phase::Resample(_) | Phase::Complete(_)
        ) {
            assert!(weak.upgrade().is_none());
            retired = true;
        }
        if done {
            break;
        }
    }
    assert!(retired);
    assert_eq!(
        job.finish().unwrap().frame_count(),
        (240137_usize * 44100).div_ceil(48000)
    );
}

#[test]
fn parser_and_total_work_limits_are_sticky_and_release_every_owner() {
    for parser_limit in [false, true] {
        let input = Arc::new(fixture("1"));
        let weak = Arc::downgrade(&input);
        let mut job = OggDecodeJob::new(Input(input), None).unwrap();
        if parser_limit {
            let Phase::Decode(owner) = &mut job.phase else {
                unreachable!()
            };
            owner.scan = None;
            owner.calls = MAX_PARSER_CALLS;
        } else {
            job.steps = MAX_STEPS;
        }
        assert_eq!(
            job.step(&AtomicBool::new(false)).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert!(weak.upgrade().is_none());
        assert_eq!(
            job.step(&AtomicBool::new(false)).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(job.finish().unwrap_err().code, ErrorCode::InvalidArgument);
    }
}

#[test]
fn incomplete_finish_and_output_errors_never_publish_a_partial_buffer() {
    assert!(OggDecodeJob::new(fixture("1"), Some(0)).is_err());
    assert_eq!(
        OggDecodeJob::new(fixture("1"), None)
            .unwrap()
            .finish()
            .unwrap_err()
            .code,
        ErrorCode::Internal
    );
    let input = Arc::new(fixture("1"));
    let weak = Arc::downgrade(&input);
    let mut job = OggDecodeJob::with_limit(Input(input), None, 9737 * 2 - 1).unwrap();
    if let Phase::Decode(owner) = &mut job.phase {
        owner.scan = None;
    }
    let error = loop {
        match job.step(&AtomicBool::new(false)) {
            Ok(false) => {}
            Ok(true) => panic!("over-budget PCM escaped"),
            Err(error) => break error,
        }
    };
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        job.step(&AtomicBool::new(false)).unwrap_err().code,
        error.code
    );
    assert_eq!(job.finish().unwrap_err().code, error.code);
}

#[test]
fn final_flatten_and_completion_transfer_the_reserved_pcm_without_cloning() {
    let mut job = OggDecodeJob::new(fixture("1"), None).unwrap();
    let pointer = loop {
        let flatten = matches!(job.phase, Phase::Flatten(..));
        let before = migo_alloc_probe::thread_counts();
        assert!(!job.step(&AtomicBool::new(false)).unwrap());
        let after = migo_alloc_probe::thread_counts();
        if flatten {
            assert_eq!(after.allocations, before.allocations);
            assert_eq!(after.reallocations, before.reallocations);
        }
        if let Phase::Decoded(audio) = &job.phase {
            break audio.samples.as_ptr();
        }
    };
    let before = migo_alloc_probe::thread_counts();
    assert!(job.step(&AtomicBool::new(false)).unwrap());
    let output = job.finish().unwrap();
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(after.allocations, before.allocations);
    assert_eq!(after.reallocations, before.reallocations);
    assert_eq!(output.samples.as_ptr(), pointer);
    assert_eq!(output.samples.len(), 9737 * 2);
}

#[test]
fn nonzero_long_blocks_preserve_the_absolute_cursor_and_every_channel() {
    use symphonia_core::audio::{AsGenericAudioBufferRef, AudioBuffer, AudioSpec, Channels};
    for (channels, known) in [(6, false), (6, true), (8, false), (8, true)] {
        let frames = 4107;
        let mut block =
            AudioBuffer::<f32>::new(AudioSpec::new(48000, Channels::Discrete(channels)), frames);
        block
            .render_with(Some(frames), |frame, planes| {
                for (channel, plane) in planes.iter_mut().enumerate() {
                    plane[frame] = (frame * channels as usize + channel) as f32 / 32768. - 0.5;
                }
                Ok(())
            })
            .unwrap();
        let mut collector =
            PcmCollector::with_expected_samples(known.then_some(frames * channels as usize));
        if collector.needs_reservation() {
            assert!(!collector.step().unwrap());
        }
        let mut scratch = [0.; 8192];
        let mut cursor = 0;
        let mut done = false;
        for _ in 0..8 {
            let previous = cursor;
            done = copy_block(
                block.as_generic_audio_buffer_ref(),
                &mut cursor,
                &mut scratch[..if known { 0 } else { 8192 }],
                &mut collector,
            )
            .unwrap();
            assert_eq!(cursor, (previous + 8192 / channels as usize).min(frames));
            if done {
                break;
            }
        }
        assert!(done);
        collector.seal().unwrap();
        while !collector.step().unwrap() {}
        let samples = collector.finish().unwrap();
        let expected: Vec<_> = (0..frames * channels as usize)
            .map(|n| n as f32 / 32768. - 0.5)
            .collect();
        assert_eq!(samples, expected, "nonzero {channels}-channel long block");
    }
}

#[test]
fn known_and_fallback_collection_preserve_pcm_and_reservation_steps() {
    for name in ["0", "1", "cross", "max-window-8"] {
        let data = fixture(name);
        let expected = super::super::reference::decode(&data).unwrap();
        for fallback in [false, true] {
            let mut job = OggDecodeJob::new(data.clone(), None).unwrap();
            if fallback {
                let Phase::Decode(owner) = &mut job.phase else {
                    unreachable!()
                };
                owner.scan.as_mut().unwrap().calls = 4096;
            }
            let mut reservation = false;
            for _ in 0..MAX_STEPS {
                let reserve = phase(&job) == "reserve";
                if reserve {
                    let Phase::Decode(owner) = &job.phase else {
                        unreachable!()
                    };
                    assert_eq!(owner.offset, 0);
                    assert!(owner.codec.is_none());
                    TRACE.with(|t| *t.borrow_mut() = Trace::default());
                }
                let done = job.step(&AtomicBool::new(false)).unwrap();
                if reserve {
                    let t = TRACE.with(|t| std::mem::take(&mut *t.borrow_mut()));
                    assert_eq!(t.parser_calls + t.codec_calls + t.copied_samples, 0);
                    reservation = true;
                }
                if done {
                    break;
                }
            }
            assert_eq!(reservation, !fallback);
            assert_eq!(job.finish().unwrap().samples, expected.samples);
        }
    }
}

#[test]
fn duration_quota_rejects_before_codec_or_pcm_reservation() {
    let input = Arc::new(fixture("1"));
    let weak = Arc::downgrade(&input);
    let mut job = OggDecodeJob::with_limit(Input(input), None, 9737 * 2 - 1).unwrap();
    let error = loop {
        assert_ne!(phase(&job), "reserve");
        TRACE.with(|t| *t.borrow_mut() = Trace::default());
        let result = job.step(&AtomicBool::new(false));
        assert_eq!(TRACE.with(|t| t.borrow().codec_calls), 0);
        match result {
            Ok(false) => {}
            Ok(true) => panic!("duration quota was ignored"),
            Err(error) => break error,
        }
    };
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        job.step(&AtomicBool::new(false)).unwrap_err().code,
        error.code
    );
    assert_eq!(job.finish().unwrap_err().code, error.code);
}
