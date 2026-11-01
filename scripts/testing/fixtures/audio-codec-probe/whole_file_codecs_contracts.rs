use super::*;
use std::sync::Arc;

const LIMIT: usize = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize;
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_COMPLETE_FIXTURES").unwrap()).join(name),
    )
    .unwrap()
}
fn source_stage<B>(job: &WholeFileJob<B>) -> usize {
    match &job.phase {
        Phase::Decode(owner) => match &owner.codec {
            Codec::M4a(file) => file.source_stage(),
            _ => 0,
        },
        _ => 0,
    }
}
fn run<B: AsRef<[u8]>>(job: WholeFileJob<B>) -> DecodedAudio {
    run_observed(job, false)
}
fn run_observed<B: AsRef<[u8]>>(mut job: WholeFileJob<B>, mut declared: bool) -> DecodedAudio {
    let mut flatten = 0;
    for _ in 0..MAX_STEPS {
        let old_source = source_stage(&job);
        let old_offset = match &job.phase {
            Phase::Decode(owner) => owner.offset,
            _ => 0,
        };
        let was_flatten = matches!(job.phase, Phase::Flatten(..));
        declared |=
            matches!(&job.phase, Phase::Decode(owner) if owner.collector.needs_reservation());
        let before = migo_alloc_probe::thread_counts();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        if let Phase::Decode(owner) = &job.phase
            && old_source == 0
        {
            assert!(owner.offset >= old_offset && owner.offset - old_offset <= 8192);
        }
        if was_flatten {
            flatten += 1;
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(after.allocations, before.allocations);
            assert_eq!(after.reallocations, before.reallocations);
        }
        if done {
            let audio = job.finish().unwrap();
            assert!(
                flatten
                    >= if declared {
                        1
                    } else {
                        audio.samples.len().div_ceil(8192).max(1)
                    }
            );
            return audio;
        }
    }
    panic!("complete codec job made no bounded progress");
}
fn compare(data: &[u8], format: Format, rate: u32, channels: u32, frames: usize) {
    let raw = crate::candidate::decode(data, LIMIT).unwrap();
    assert_eq!(
        (raw.sample_rate, raw.channels, raw.samples.len()),
        (rate, channels, frames * channels as usize)
    );
    assert!(raw.samples.iter().any(|sample| sample.abs() > 0.001));
    for target in [rate, if rate == 44100 { 48000 } else { 44100 }] {
        let expected = super::tests::resampled(&raw.samples, rate, target, channels);
        let actual = run(WholeFileJob::new(data, format, target, LIMIT).unwrap());
        assert_eq!((actual.sample_rate, actual.channels), (target, channels));
        assert_eq!(
            actual.samples, expected,
            "complete PCM and sinc tail at {rate}->{target}"
        );
    }
}

#[test]
fn mp3_and_adts_jobs_match_pull_pcm_independent_counts_and_sinc_tail() {
    let rows: serde_json::Value = serde_json::from_slice(&fixture("elementary.json")).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 5);
    for row in rows.as_array().unwrap() {
        let name = row["file"].as_str().unwrap();
        let format = if name.ends_with(".mp3") {
            Format::Mp3
        } else {
            Format::Adts
        };
        compare(
            &fixture(name),
            format,
            row["rate"].as_u64().unwrap() as u32,
            row["channels"].as_u64().unwrap() as u32,
            row["frames"].as_u64().unwrap() as usize,
        );
    }
}

#[test]
fn m4a_jobs_match_exact_presentation_across_front_tail_reordered_and_multi_media() {
    let rows: serde_json::Value = serde_json::from_slice(&fixture("complete-m4a.json")).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 19);
    for row in rows.as_array().unwrap() {
        let name = row["file"].as_str().unwrap();
        if let Some(start) = row["slice_start"].as_u64() {
            let raw = crate::candidate::decode(&fixture("edit-raw.m4a"), LIMIT).unwrap();
            let edited = crate::candidate::decode(&fixture(name), LIMIT).unwrap();
            let channels = row["channels"].as_u64().unwrap() as usize;
            let frames = row["frames"].as_u64().unwrap() as usize;
            assert_eq!(
                edited.samples,
                raw.samples[start as usize * channels..(start as usize + frames) * channels]
            );
        }
        compare(
            &fixture(name),
            Format::M4a,
            row["rate"].as_u64().unwrap() as u32,
            row["channels"].as_u64().unwrap() as u32,
            row["frames"].as_u64().unwrap() as usize,
        );
    }
}

#[test]
fn every_m4a_source_phase_can_cancel_without_retaining_input_or_partial_pcm() {
    for wanted in 1..=3 {
        let input: Arc<[u8]> = fixture("2-tail-reverse.m4a").into();
        let weak = Arc::downgrade(&input);
        let mut job = WholeFileJob::new(input, Format::M4a, 44100, LIMIT).unwrap();
        for _ in 0..MAX_STEPS {
            if source_stage(&job) == wanted {
                break;
            }
            assert!(!job.step(&AtomicBool::new(false)).unwrap());
        }
        assert_eq!(source_stage(&job), wanted);
        assert!(weak.upgrade().is_some());
        let before = migo_alloc_probe::thread_counts();
        assert!(
            matches!(job.step(&AtomicBool::new(true)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(weak.upgrade().is_none());
        assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
        assert!(
            matches!(job.step(&AtomicBool::new(false)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(matches!(job.finish(), Err(e) if e.code == ErrorCode::Cancelled));
    }
}

#[test]
fn m4a_index_scans_yield_after_each_library_atom_without_copying_the_body() {
    let data = fixture("valid-many.m4a");
    let raw = crate::candidate::decode(&data, LIMIT).unwrap();
    let mut job = WholeFileJob::new(data, Format::M4a, raw.sample_rate, LIMIT).unwrap();
    let mut scans = 0;
    for _ in 0..MAX_STEPS {
        let stage = source_stage(&job);
        let before = migo_alloc_probe::thread_counts();
        assert!(!job.step(&AtomicBool::new(false)).unwrap());
        if stage == 1 {
            scans += 1;
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(after.allocations, before.allocations);
            assert_eq!(after.reallocations, before.reallocations);
        }
        if source_stage(&job) == 2 {
            break;
        }
    }
    assert_eq!(
        scans, 25,
        "24 padding atoms and the movie header each yield once"
    );
    assert_eq!(run(job).samples, raw.samples);
}

#[test]
fn invalid_m4a_sources_fail_with_bounded_work_and_release_all_storage() {
    for name in [
        "bad-many.m4a",
        "bad-metadata.m4a",
        "bad-index.m4a",
        "bad-header.m4a",
        "bad-fragment.m4a",
        "bad-missing.m4a",
    ] {
        let input: Arc<[u8]> = fixture(name).into();
        let weak = Arc::downgrade(&input);
        let mut job = WholeFileJob::new(input, Format::M4a, 44100, LIMIT).unwrap();
        let mut failed = false;
        for _ in 0..512 {
            let before_source = source_stage(&job);
            match job.step(&AtomicBool::new(false)) {
                Err(e) => {
                    assert!(
                        before_source > 0,
                        "{name}: refusal must occur in the bounded source resolver"
                    );
                    if name == "bad-index.m4a" {
                        assert_eq!(before_source, 1, "oversized index refused before parsing");
                    }
                    assert_eq!(e.code, ErrorCode::InvalidArgument, "{name}");
                    assert!(weak.upgrade().is_none());
                    assert!(
                        matches!(job.step(&AtomicBool::new(false)), Err(next) if next.code == e.code)
                    );
                    assert!(matches!(job.finish(), Err(next) if next.code == e.code));
                    failed = true;
                    break;
                }
                Ok(true) => panic!("invalid source completed: {name}"),
                Ok(false) => {}
            }
        }
        assert!(failed, "bounded source refusal not reached: {name}");
    }
}

#[test]
fn m4a_output_quota_is_exact_and_owned_input_retires_before_publication() {
    let data = fixture("2-tail-reverse.m4a");
    let raw = crate::candidate::decode(&data, LIMIT).unwrap();
    let expected = super::tests::resampled(&raw.samples, raw.sample_rate, 44100, raw.channels);
    let input: Arc<[u8]> = data.clone().into();
    let weak = Arc::downgrade(&input);
    let mut job = WholeFileJob::new(input, Format::M4a, 44100, expected.len()).unwrap();
    let mut declared = false;
    while !matches!(job.phase, Phase::Flatten(..)) {
        declared |=
            matches!(&job.phase, Phase::Decode(owner) if owner.collector.needs_reservation());
        assert!(!job.step(&AtomicBool::new(false)).unwrap());
    }
    assert!(
        weak.upgrade().is_none(),
        "codec, input and scratch retire before final publication"
    );
    assert!(declared);
    assert_eq!(run_observed(job, declared).samples, expected);
    let input: Arc<[u8]> = data.into();
    let weak = Arc::downgrade(&input);
    let mut job = WholeFileJob::new(input, Format::M4a, 44100, expected.len() - 1).unwrap();
    let mut failed = false;
    for _ in 0..MAX_STEPS {
        match job.step(&AtomicBool::new(false)) {
            Err(e) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                failed = true;
                break;
            }
            Ok(true) => panic!("quota must not publish partial output"),
            Ok(false) => {}
        }
    }
    assert!(failed && weak.upgrade().is_none() && job.finish().is_err());
}

#[test]
fn background_m4a_source_steps_allow_foreground_work_and_keep_pcm_exact() {
    let data = fixture("2-tail-multi.m4a");
    let raw = crate::candidate::decode(&data, LIMIT).unwrap();
    let expected = super::tests::resampled(&raw.samples, raw.sample_rate, 44100, raw.channels);
    let host = migo_io::scheduler::IoScheduler::new(0);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    let actual = runtime.block_on(async {
        let mut owner = crate::off_worker::OffWorker::new(
            Some(WholeFileJob::new(data, Format::M4a, 44100, LIMIT).unwrap()),
            host.background_audio_cpu(),
        );
        let mut source_steps = 0;
        loop {
            let (returned, (done, source)) = owner
                .with(|slot| {
                    let job = slot.as_mut().unwrap();
                    let source = source_stage(job);
                    (job.step(&AtomicBool::new(false)).unwrap(), source)
                })
                .await
                .unwrap();
            owner = returned;
            if source != 0 {
                source_steps += 1;
                assert_eq!(host.audio_cpu().run(|| 321).await.unwrap(), 321);
            }
            if done {
                let (_, audio) = owner
                    .with(|slot| slot.take().unwrap().finish().unwrap())
                    .await
                    .unwrap();
                assert!(source_steps >= 6);
                break audio;
            }
        }
    });
    assert_eq!(actual.samples, expected);
    host.close();
}
