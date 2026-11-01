use super::*;
use std::sync::{Arc, atomic::Ordering};

fn inputs() -> Vec<Vec<u8>> {
    let mut wave = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut wave,
        hound::WavSpec {
            channels: 2,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for n in 0..1300 {
        writer.write_sample((n * 7) as i16).unwrap();
    }
    writer.finalize().unwrap();
    vec![
        wave.into_inner(),
        include_bytes!("ogg-fixtures/1.ogg").to_vec(),
        crate::mp3_fixture::stream(4),
    ]
}
fn drain<B: AsRef<[u8]>>(mut job: WholeDecodeJob<B>) -> DecodedAudio {
    for _ in 0..1_048_576 {
        if job.step(&AtomicBool::new(false)).unwrap() {
            return job.finish().unwrap();
        }
    }
    panic!("product decode did not complete")
}
#[test]
fn all_product_formats_keep_complete_pcm_and_the_submitted_target() {
    for bytes in inputs() {
        let native = crate::decoder::decode(&bytes).unwrap();
        for target in [native.sample_rate, 16000, 48000] {
            let expected = crate::resampler::resample_if_needed(native.clone(), target).unwrap();
            let actual = drain(WholeDecodeJob::new(bytes.as_slice(), Some(target)).unwrap());
            assert_eq!(
                (actual.sample_rate, actual.channels),
                (target, expected.channels)
            );
            assert_eq!(actual.samples, expected.samples);
            let direct = crate::decoder::decode_for_rate(&bytes, target).unwrap();
            assert_eq!(direct.samples, actual.samples);
        }
    }
    for bytes in [vec![], b"RI".to_vec(), b"abcd".to_vec()] {
        assert!(WholeDecodeJob::new(bytes, Some(48000)).is_err());
    }
    assert!(WholeDecodeJob::new(inputs().remove(0), Some(0)).is_err());
}

struct Input(Arc<Vec<u8>>);
impl AsRef<[u8]> for Input {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
#[test]
fn cancellation_and_incomplete_finish_release_each_product_owner() {
    for bytes in inputs() {
        for stop in [0, 1, 7] {
            let input = Arc::new(bytes.clone());
            let weak = Arc::downgrade(&input);
            let mut job = WholeDecodeJob::new(Input(input), Some(48000)).unwrap();
            for _ in 0..stop {
                assert!(!job.step(&AtomicBool::new(false)).unwrap());
            }
            assert_eq!(
                job.step(&AtomicBool::new(true)).unwrap_err().code,
                ErrorCode::Cancelled
            );
            assert!(weak.upgrade().is_none());
            assert_eq!(
                job.step(&AtomicBool::new(false)).unwrap_err().code,
                ErrorCode::Cancelled
            );
            assert_eq!(job.finish().unwrap_err().code, ErrorCode::Cancelled);
        }
        let input = Arc::new(bytes);
        let weak = Arc::downgrade(&input);
        assert_eq!(
            WholeDecodeJob::new(Input(input), Some(48000))
                .unwrap()
                .finish()
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn actual_product_jobs_preserve_state_on_background_cpu_handoffs() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    for bytes in inputs() {
        let expected = crate::decoder::decode_for_rate(&bytes, 48000).unwrap();
        runtime.block_on(async {
            let mut owner = crate::off_worker::OffWorker::new(None, host.background_audio_cpu());
            let (next, initialized) = owner
                .with(move |job| {
                    *job = Some(WholeDecodeJob::new(bytes, Some(48000))?);
                    Ok::<_, EngineError>(())
                })
                .await
                .unwrap();
            initialized.unwrap();
            owner = next;
            let cancel = Arc::new(AtomicBool::new(false));
            let mut steps = 0;
            loop {
                let flag = Arc::clone(&cancel);
                let (next, done) = owner
                    .with(move |job| job.as_mut().unwrap().step(&flag))
                    .await
                    .unwrap();
                owner = next;
                steps += 1;
                if done.unwrap() {
                    break;
                }
                assert_eq!(host.audio_cpu().run(|| 17).await.unwrap(), 17);
            }
            assert!(steps > 2);
            assert!(!cancel.load(Ordering::Acquire));
            let (_, output) = owner
                .with(|job| job.take().unwrap().finish())
                .await
                .unwrap();
            assert_eq!(output.unwrap().samples, expected.samples);
        });
    }
}
