use super::*;
use std::sync::Arc;

#[path = "wave_pcm_contracts.rs"]
mod wave;

const LIMIT: usize = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize;

fn stage<B>(job: &WholeFileJob<B>) -> usize {
    match &job.phase {
        Phase::Decode(owner) if owner.collector.needs_reservation() => 6,
        Phase::Decode(owner) if owner.head < owner.pcm.len() => 1,
        Phase::Decode(owner) if owner.ended => 2,
        Phase::Decode(_) => 0,
        Phase::Flatten(..) => 3,
        Phase::Complete(_) => 4,
        Phase::Failed(_) => 5,
    }
}
fn offset<B>(job: &WholeFileJob<B>) -> usize {
    match &job.phase {
        Phase::Decode(owner) => owner.offset,
        _ => 0,
    }
}
fn drain<B: AsRef<[u8]>>(mut job: WholeFileJob<B>) -> DecodedAudio {
    let mut flatten_steps = 0;
    let mut declared = false;
    let mut copy_steps = 0;
    for _ in 0..MAX_STEPS {
        let before_stage = stage(&job);
        let old_offset = offset(&job);
        let old_head = match &job.phase {
            Phase::Decode(owner) => owner.head,
            _ => 0,
        };
        let before = migo_alloc_probe::thread_counts();
        declared |= before_stage == 6;
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let after = migo_alloc_probe::thread_counts();
        if before_stage == 3 {
            flatten_steps += 1;
            assert_eq!(after.allocations, before.allocations);
            assert_eq!(after.reallocations, before.reallocations);
        }
        if let Phase::Decode(owner) = &job.phase {
            assert!(
                owner.offset - old_offset <= 8192,
                "actual admitted input must stay bounded"
            );
            if before_stage == 1 {
                assert!(owner.head - old_head <= 8192);
                assert!(after.bytes_allocated - before.bytes_allocated <= 131_072);
                copy_steps += usize::from(owner.head > old_head);
                if declared {
                    assert_eq!(after.allocations, before.allocations);
                    assert_eq!(after.reallocations, before.reallocations);
                }
            }
            if before_stage == 6 {
                assert_eq!(owner.head, old_head);
                assert_eq!(owner.offset, old_offset);
                assert!(!owner.collector.needs_reservation());
            }
        }
        if done {
            assert!(job.step(&AtomicBool::new(false)).unwrap());
            let output = job.finish().unwrap();
            assert!(
                flatten_steps
                    >= if declared {
                        1
                    } else {
                        output.samples.len().div_ceil(8192).max(1)
                    },
                "finalization must leave a scheduling boundary per copy budget"
            );
            assert!(copy_steps >= output.samples.len().div_ceil(8192));
            return output;
        }
    }
    panic!("whole-file job exhausted its bounded steps");
}

pub(super) fn resampled(samples: &[f32], input: u32, output: u32, channels: u32) -> Vec<f32> {
    if input == output {
        return samples.to_vec();
    }
    let mut sampler = crate::bandlimited::Stream::new(
        crate::resampled_streaming::core(input, output, channels).unwrap(),
        input,
        output,
        channels,
    )
    .unwrap();
    let mut out = Vec::new();
    let mut scratch = Vec::with_capacity(SCRATCH_SAMPLES);
    let mut offset = 0;
    for _ in 0..100_000 {
        scratch.clear();
        let step = sampler
            .append_step(&samples[offset..], offset == samples.len(), &mut scratch)
            .unwrap();
        offset += step.consumed;
        out.extend_from_slice(&scratch);
        if step.progress == crate::bandlimited::Progress::Finished {
            assert_eq!(offset, samples.len());
            assert_eq!(
                out.len(),
                (samples.len() / channels as usize * output as usize).div_ceil(input as usize)
                    * channels as usize
            );
            return out;
        }
    }
    panic!("reference Rubato did not finish");
}
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_OGG_FIXTURES").unwrap()).join(name),
    )
    .unwrap()
}

#[test]
fn wave_jobs_match_independent_numeric_pcm_and_the_existing_sinc_tail() {
    let mut count = 0;
    for (bits, floating) in [
        (8, false),
        (16, false),
        (24, false),
        (32, false),
        (32, true),
    ] {
        for channels in [1, 2, 6, 26] {
            for frames in [17, 2317] {
                let (file, pcm) = wave::fixture(bits, floating, channels, frames);
                for target in [44100, 48000] {
                    let expected = resampled(&pcm, 44100, target, channels as u32);
                    let actual = drain(
                        WholeFileJob::new(file.as_slice(), Format::Wave, target, LIMIT).unwrap(),
                    );
                    assert_eq!(
                        actual.samples, expected,
                        "{bits}/{floating}/{channels}/{frames}/{target}"
                    );
                    assert_eq!(
                        (actual.sample_rate, actual.channels),
                        (target, channels as u32)
                    );
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 80);
}

#[test]
fn vorbis_jobs_keep_exact_independent_frame_counts_cross_pages_and_long_windows() {
    let mut count = 0;
    for (name, frames, rate, channels) in [
        ("0.ogg", 17, 44100, 1),
        ("1.ogg", 9737, 48000, 2),
        ("2.ogg", 9737, 22050, 1),
        ("cross.ogg", 240137, 48000, 6),
        ("large.ogg", 9737, 48000, 2),
        ("max-window-8.ogg", 8192, 48000, 8),
    ] {
        let data = fixture(name);
        let reference = crate::candidate::decode(&data, LIMIT).unwrap();
        assert_eq!(
            (
                reference.sample_rate,
                reference.channels,
                reference.samples.len()
            ),
            (rate, channels, frames * channels as usize)
        );
        for target in [rate, if rate == 44100 { 48000 } else { 44100 }] {
            let expected = resampled(&reference.samples, rate, target, channels);
            let actual =
                drain(WholeFileJob::new(data.as_slice(), Format::Vorbis, target, LIMIT).unwrap());
            assert_eq!(actual.samples, expected, "{name}/{target}");
            assert_eq!((actual.sample_rate, actual.channels), (target, channels));
            count += 1;
        }
    }
    assert_eq!(count, 12);
}

#[test]
fn only_complete_jobs_can_publish_pcm_and_empty_streams_keep_the_codec_refusal() {
    for wanted in 0..4 {
        let (file, _) = wave::fixture(16, false, 2, 49_169);
        let mut job = WholeFileJob::new(file, Format::Wave, 48000, LIMIT).unwrap();
        for _ in 0..MAX_STEPS {
            if stage(&job) == wanted {
                break;
            }
            job.step(&AtomicBool::new(false)).unwrap();
        }
        assert_eq!(stage(&job), wanted);
        assert!(job.finish().is_err());
    }
    let (file, _) = wave::fixture(16, false, 2, 0);
    let mut job = WholeFileJob::new(file, Format::Wave, 44100, LIMIT).unwrap();
    let mut refused = false;
    for _ in 0..MAX_STEPS {
        match job.step(&AtomicBool::new(false)) {
            Err(error) => {
                assert_eq!(error.code, ErrorCode::IoError);
                refused = true;
                break;
            }
            Ok(true) => panic!("no-sample codec refusal must not become successful empty PCM"),
            Ok(false) => {}
        }
    }
    assert!(refused && job.finish().is_err());
}

#[test]
fn cancellation_at_all_owned_phases_releases_storage_and_stays_terminal() {
    for wanted in 0..5 {
        let (file, _) = wave::fixture(16, false, 2, 49_169);
        let input: Arc<[u8]> = file.into();
        let weak = Arc::downgrade(&input);
        let mut job = WholeFileJob::new(input, Format::Wave, 48000, LIMIT).unwrap();
        for _ in 0..MAX_STEPS {
            if stage(&job) == wanted {
                break;
            }
            job.step(&AtomicBool::new(false)).unwrap();
        }
        assert_eq!(stage(&job), wanted);
        if wanted == 1 {
            assert!(!job.step(&AtomicBool::new(false)).unwrap());
        }
        if wanted >= 3 {
            assert!(
                weak.upgrade().is_none(),
                "input must retire before final publication"
            );
        }
        let before = migo_alloc_probe::thread_counts();
        assert!(
            matches!(job.step(&AtomicBool::new(true)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
        assert!(weak.upgrade().is_none());
        assert!(
            matches!(job.step(&AtomicBool::new(false)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(matches!(job.finish(), Err(e) if e.code == ErrorCode::Cancelled));
    }
    // The unknown-length path still has a real partially copied final buffer.
    let input: Arc<[u8]> = fixture("1.ogg").into();
    let weak = Arc::downgrade(&input);
    let mut job = WholeFileJob::new(input, Format::Vorbis, 44100, LIMIT).unwrap();
    if let Phase::Decode(owner) = &mut job.phase {
        owner.scan = None;
    }
    while !matches!(job.phase, Phase::Flatten(..)) {
        assert!(!job.step(&AtomicBool::new(false)).unwrap());
    }
    assert!(weak.upgrade().is_none());
    assert!(!job.step(&AtomicBool::new(false)).unwrap());
    let before = migo_alloc_probe::thread_counts();
    assert!(
        matches!(job.step(&AtomicBool::new(true)), Err(error) if error.code == ErrorCode::Cancelled)
    );
    assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
    assert!(matches!(job.finish(), Err(error) if error.code == ErrorCode::Cancelled));
}

#[test]
fn quota_codec_and_work_errors_retire_input_without_publishing_partial_pcm() {
    let (wave, pcm) = wave::fixture(16, false, 2, 9737);
    let exact = resampled(&pcm, 44100, 48000, 2).len();
    assert_eq!(
        drain(WholeFileJob::new(wave.clone(), Format::Wave, 48000, exact).unwrap())
            .samples
            .len(),
        exact
    );
    let mut broken = fixture("1.ogg");
    broken.pop();
    for (file, format, limit, budget) in [
        (wave.clone(), Format::Wave, exact - 1, MAX_STEPS),
        (broken, Format::Vorbis, LIMIT, MAX_STEPS),
        (fixture("1.ogg"), Format::Vorbis, 1, MAX_STEPS),
        (wave, Format::Wave, LIMIT, 1),
    ] {
        let input: Arc<[u8]> = file.into();
        let weak = Arc::downgrade(&input);
        let mut job = WholeFileJob::new(input, format, 48000, limit).unwrap();
        job.remaining_steps = budget;
        let mut code = None;
        for _ in 0..MAX_STEPS {
            match job.step(&AtomicBool::new(false)) {
                Err(e) => {
                    code = Some(e.code);
                    break;
                }
                Ok(true) => panic!("invalid/truncated/budgeted input cannot complete"),
                Ok(false) => {}
            }
        }
        let code = code.expect("bounded failure was not observed");
        assert_eq!(stage(&job), 5);
        assert!(weak.upgrade().is_none());
        assert!(matches!(job.step(&AtomicBool::new(false)),Err(e)if e.code==code));
        assert!(matches!(job.finish(),Err(e)if e.code==code));
    }
}

#[test]
fn constructor_rejects_oversized_input_and_invalid_rates_before_codec_storage() {
    let input: Arc<[u8]> = vec![0; 16 * 1024 * 1024 + 1].into();
    let weak = Arc::downgrade(&input);
    let before = migo_alloc_probe::thread_counts();
    assert!(WholeFileJob::new(input, Format::Vorbis, 48000, LIMIT).is_err());
    assert!(weak.upgrade().is_none());
    assert!(migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 8192);
    for rate in [0, 2999, 768001] {
        assert!(WholeFileJob::new(Vec::new(), Format::Wave, rate, LIMIT).is_err());
    }
    let (file, pcm) = wave::fixture(16, false, 1, 17);
    assert_eq!(
        drain(WholeFileJob::new(file, Format::Wave, 44100, usize::MAX).unwrap()).samples,
        pcm
    );
}

#[test]
fn metadata_and_final_copy_yield_without_changing_nonzero_samples() {
    let (clean, pcm) = wave::fixture(16, false, 2, 49_169);
    let mut file = clean[..12].to_vec();
    file.extend_from_slice(b"JUNK");
    file.extend_from_slice(&(2_u32 * 1024 * 1024).to_le_bytes());
    file.resize(file.len() + 2 * 1024 * 1024, 0);
    file.extend_from_slice(&clean[12..]);
    file.extend_from_slice(b"JUNK");
    file.extend_from_slice(&(128_u32 * 1024).to_le_bytes());
    file.resize(file.len() + 128 * 1024, 0);
    let length = file.len() as u32 - 8;
    file[4..8].copy_from_slice(&length.to_le_bytes());
    assert_eq!(
        drain(WholeFileJob::new(file, Format::Wave, 44100, LIMIT).unwrap()).samples,
        pcm
    );
    // Unknown presentation lengths must keep the paged, bounded final copy.
    let file = fixture("1.ogg");
    let expected = crate::candidate::decode(&file, LIMIT).unwrap();
    let mut job = WholeFileJob::new(file, Format::Vorbis, 48000, LIMIT).unwrap();
    if let Phase::Decode(owner) = &mut job.phase {
        owner.scan = None;
    }
    assert_eq!(drain(job).samples, expected.samples);
}

#[test]
fn real_background_cpu_handoffs_keep_pcm_exact_and_host_close_retires_the_owner() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    let (file, pcm) = wave::fixture(24, false, 2, 2317);
    let input: Arc<[u8]> = file.into();
    let weak = Arc::downgrade(&input);
    let host = migo_io::scheduler::IoScheduler::new(0);
    let actual = runtime.block_on(async {
        let job = WholeFileJob::new(input, Format::Wave, 48000, LIMIT).unwrap();
        let mut owner = crate::off_worker::OffWorker::new(Some(job), host.background_audio_cpu());
        loop {
            let (next, result) = owner
                .with(|job| job.as_mut().unwrap().step(&AtomicBool::new(false)))
                .await
                .unwrap();
            owner = next;
            if result.unwrap() {
                break;
            }
            assert_eq!(host.audio_cpu().run(|| 321).await.unwrap(), 321);
        }
        let (_, output) = owner
            .with(|job| job.take().unwrap().finish().unwrap())
            .await
            .unwrap();
        output
    });
    assert!(weak.upgrade().is_none());
    assert_eq!(actual.samples, resampled(&pcm, 44100, 48000, 2));
    let (file, _) = wave::fixture(16, false, 2, 2317);
    let input: Arc<[u8]> = file.into();
    let weak = Arc::downgrade(&input);
    runtime.block_on(async {
        let mut job = WholeFileJob::new(input, Format::Wave, 48000, LIMIT).unwrap();
        job.step(&AtomicBool::new(false)).unwrap();
        let owner = crate::off_worker::OffWorker::new(job, host.background_audio_cpu());
        host.close();
        assert!(owner.with(|_| ()).await.is_err());
    });
    assert!(weak.upgrade().is_none());
}
