use super::*;
use crate::{
    decoder::mp3::{file::tests::expected_fixture, take_native_trace},
    mp3_fixture,
};

const CBR: &[u8] = include_bytes!("fixtures/stereo-cbr.mp3");
const VBR: &[u8] = include_bytes!("fixtures/mono-vbr.mp3");
const CBR_PCM: &[u8] = include_bytes!("fixtures/stereo-cbr.f32le");
const VBR_PCM: &[u8] = include_bytes!("fixtures/mono-vbr.f32le");

fn reference(
    encoded: &[u8],
    frozen: &[u8],
    rate: u32,
    channels: u32,
    target: Option<u32>,
) -> DecodedAudio {
    let pcm = expected_fixture(encoded, frozen);
    let raw = DecodedAudio {
        samples: pcm
            .chunks_exact(4)
            .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
            .collect(),
        sample_rate: rate,
        channels,
    };
    match target {
        Some(target) => crate::resampler::resample_if_needed(raw, target).unwrap(),
        None => raw,
    }
}

fn drain<B: AsRef<[u8]>>(mut job: Mp3DecodeJob<B>) -> DecodedAudio {
    for _ in 0..100_000 {
        take_native_trace();
        let phase = stage(&job);
        let preparing_dsp = matches!(&job.phase,Phase::Resample(job) if job.is_preparing());
        let before = migo_alloc_probe::thread_counts();
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let after = migo_alloc_probe::thread_counts();
        if phase == 1 || (phase == 3 && !preparing_dsp) {
            assert_eq!(
                after.allocations, before.allocations,
                "finalization and prepared DSP reuse their output buffers"
            );
            assert_eq!(after.reallocations, before.reallocations);
        }
        let trace = take_native_trace();
        assert!(trace.calls <= 4 && trace.bytes <= 8192 && trace.max_scan <= 4096);
        if done {
            assert!(job.step(&AtomicBool::new(false)).unwrap());
            return job.finish().unwrap();
        }
    }
    panic!("MP3 job made no bounded progress");
}

#[test]
fn raw_and_targeted_jobs_preserve_exact_nonzero_pcm_and_metadata() {
    for (encoded, frozen, rate, channels) in [(CBR, CBR_PCM, 44_100, 2), (VBR, VBR_PCM, 22_050, 1)]
    {
        for target in [
            None,
            Some(8_000),
            Some(22_050),
            Some(44_100),
            Some(48_000),
            Some(96_000),
        ] {
            let expected = reference(encoded, frozen, rate, channels, target);
            let actual = drain(Mp3DecodeJob::new(encoded, target));
            assert_eq!(actual.samples, expected.samples);
            assert_eq!(actual.sample_rate, expected.sample_rate);
            assert_eq!(actual.channels, channels);
        }
    }
}

fn stage<B>(job: &Mp3DecodeJob<B>) -> usize {
    match job.phase {
        Phase::Decode(_) => 0,
        Phase::Flatten(..) => 1,
        Phase::Decoded(_) => 2,
        Phase::Resample(_) => 3,
        Phase::Complete(_) => 4,
        Phase::Failed(_) => 5,
    }
}

#[test]
fn cancellation_at_every_owned_phase_releases_storage_and_cannot_resume() {
    for wanted in 0..5 {
        let input: std::sync::Arc<[u8]> = mp3_fixture::stream(64).into();
        let weak = std::sync::Arc::downgrade(&input);
        let mut job = Mp3DecodeJob::new(input, Some(48_000));
        for _ in 0..100_000 {
            if stage(&job) == wanted {
                break;
            }
            job.step(&AtomicBool::new(false)).unwrap();
        }
        assert_eq!(stage(&job), wanted);
        if wanted == 1 || wanted == 3 {
            assert!(!job.step(&AtomicBool::new(false)).unwrap());
        }
        let before = migo_alloc_probe::thread_counts();
        assert!(
            matches!(job.step(&AtomicBool::new(true)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(migo_alloc_probe::thread_counts().bytes_freed > before.bytes_freed);
        assert!(weak.upgrade().is_none());
        assert_eq!(stage(&job), 5);
        assert!(
            matches!(job.step(&AtomicBool::new(false)), Err(e) if e.code == ErrorCode::Cancelled)
        );
        assert!(matches!(job.finish(), Err(e) if e.code == ErrorCode::Cancelled));
    }
}

#[test]
fn codec_and_dsp_errors_retire_the_job_without_returning_partial_pcm() {
    for (input, target) in [(b"ID3bad".to_vec(), None), (CBR.to_vec(), Some(0))] {
        let input: std::sync::Arc<[u8]> = input.into();
        let weak = std::sync::Arc::downgrade(&input);
        let mut job = Mp3DecodeJob::new(input, target);
        let mut failed = false;
        for _ in 0..100_000 {
            match job.step(&AtomicBool::new(false)) {
                Err(e) => {
                    assert_eq!(e.code, ErrorCode::InvalidArgument);
                    failed = true;
                    break;
                }
                Ok(true) => panic!("invalid input or rate cannot complete"),
                Ok(false) => {}
            }
        }
        assert!(failed);
        assert!(weak.upgrade().is_none());
        assert_eq!(stage(&job), 5);
        assert!(
            matches!(job.step(&AtomicBool::new(false)), Err(e) if e.code == ErrorCode::InvalidArgument)
        );
        assert!(job.finish().is_err());
    }
}

#[test]
fn complete_pcm_is_unavailable_before_all_collection_and_dsp_phases_finish() {
    for wanted in 0..4 {
        let mut job = Mp3DecodeJob::new(mp3_fixture::stream(8), Some(48_000));
        for _ in 0..100_000 {
            if stage(&job) == wanted {
                break;
            }
            job.step(&AtomicBool::new(false)).unwrap();
        }
        assert_eq!(stage(&job), wanted);
        assert!(
            job.finish().is_err(),
            "only the complete phase can publish PCM"
        );
    }
}

#[test]
fn actual_rate_dispatch_uses_collection_and_dsp_in_the_same_mp3_job() {
    TRACE.with(|trace| trace.set((0, 0)));
    let actual = crate::decoder::decode_for_rate(CBR, 48_000).unwrap();
    let phases = TRACE.with(|trace| trace.get());
    assert!(
        phases.0 > 1 && phases.1 > 1,
        "product dispatch must step both collection and DSP"
    );
    assert_eq!(
        actual.samples,
        reference(CBR, CBR_PCM, 44_100, 2, Some(48_000)).samples
    );
    TRACE.with(|trace| trace.set((0, 0)));
    let actual = crate::decoder::decode(CBR).unwrap();
    let phases = TRACE.with(|trace| trace.get());
    assert!(phases.0 > 1 && phases.1 == 0);
    assert_eq!(
        actual.samples,
        reference(CBR, CBR_PCM, 44_100, 2, None).samples
    );
}

#[test]
fn full_job_keeps_exclusive_ownership_across_real_audio_cpu_handoffs() {
    let input: std::sync::Arc<[u8]> = CBR.into();
    let weak = std::sync::Arc::downgrade(&input);
    let host = migo_io::scheduler::IoScheduler::new(0);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    let output = runtime.block_on(async {
        let mut owner = crate::off_worker::OffWorker::new(
            Some(Mp3DecodeJob::new(input, Some(48_000))),
            host.background_audio_cpu(),
        );
        let mut steps = 0;
        loop {
            let (next, result) = owner
                .with(|job| job.as_mut().unwrap().step(&AtomicBool::new(false)))
                .await
                .unwrap();
            owner = next;
            steps += 1;
            if result.unwrap() {
                break;
            }
            assert_eq!(host.audio_cpu().run(|| 456).await.unwrap(), 456);
        }
        assert!(steps > 6);
        let (_, output) = owner
            .with(|job| job.take().unwrap().finish().unwrap())
            .await
            .unwrap();
        output
    });
    assert!(weak.upgrade().is_none());
    assert_eq!(
        output.samples,
        reference(CBR, CBR_PCM, 44_100, 2, Some(48_000)).samples
    );
}

#[test]
fn finalization_storage_is_released_before_the_resampled_buffer_is_allocated() {
    let frames = (crate::limits::MAX_AUDIO_PCM_SAMPLES * 44_100
        / 48_000
        / mp3_fixture::SAMPLES_PER_FRAME as u64) as usize;
    let baseline = migo_alloc_probe::thread_counts();
    let mut job = Mp3DecodeJob::new(mp3_fixture::stream(frames), Some(48_000));
    let mut peak = 0_i128;
    loop {
        let done = job.step(&AtomicBool::new(false)).unwrap();
        let now = migo_alloc_probe::thread_counts();
        let live = (now.bytes_allocated - baseline.bytes_allocated) as i128
            - (now.bytes_freed - baseline.bytes_freed) as i128;
        peak = peak.max(live);
        assert!(
            peak <= 130 * 1024 * 1024,
            "pages must retire before DSP, live Rust bytes {peak}"
        );
        if done {
            break;
        }
    }
    assert!(
        peak >= 64 * 1024 * 1024,
        "measurement must include real full-size PCM storage"
    );
    let output = job.finish().unwrap();
    assert!(output.samples.len() as u64 <= crate::limits::MAX_AUDIO_PCM_SAMPLES);
    assert_eq!(output.sample_rate, 48_000);
    assert!(output.samples.iter().all(|sample| *sample == 0.0));
}
