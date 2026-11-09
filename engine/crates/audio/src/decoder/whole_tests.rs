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
        include_bytes!("aac-fixtures/adts-1.aac").to_vec(),
        include_bytes!("aac-fixtures/2-tail-reverse.m4a").to_vec(),
    ]
}
fn drain(mut job: WholeDecodeJob) -> DecodedAudio {
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
            let actual = drain(WholeDecodeJob::new(bytes.clone(), Some(target)).unwrap());
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
            assert!(
                matches!(job.step(&AtomicBool::new(true)), Err(e) if e.code == ErrorCode::Cancelled),
                "product AAC cancellation was not forwarded"
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

#[test]
fn aac_adts_reaches_the_product_dispatcher() {
    let input = include_bytes!("aac-fixtures/adts-1.aac");
    let audio = crate::decoder::decode_for_rate(input, 44100)
        .unwrap_or_else(|_| panic!("product dispatcher rejected AAC-LC ADTS"));
    assert_eq!(
        (audio.sample_rate, audio.channels, audio.frame_count()),
        (44100, 2, 11264)
    );
    assert!(audio.samples.iter().any(|s| s.abs() > 0.01));
}

#[test]
fn m4a_reaches_the_product_dispatcher() {
    let input = include_bytes!("aac-fixtures/1-tail.m4a");
    let audio = crate::decoder::decode_for_rate(input, 44100)
        .unwrap_or_else(|_| panic!("product dispatcher rejected AAC-LC M4A"));
    assert_eq!(
        (audio.sample_rate, audio.channels, audio.frame_count()),
        (44100, 2, 9737)
    );
    assert!(audio.samples.iter().any(|s| s.abs() > 0.01));
}

fn f32le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
        .collect()
}

fn i16le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|x| f32::from(i16::from_le_bytes(x.try_into().unwrap())) / 32768.0)
        .collect()
}

#[test]
fn every_format_decodes_to_its_frozen_independent_reference() {
    // MP3: the original minimp3 output, bit for bit (the same core decoder).
    for (name, encoded, frozen) in [
        (
            "stereo-cbr",
            include_bytes!("mp3/fixtures/stereo-cbr.mp3").as_slice(),
            f32le(include_bytes!("mp3/fixtures/stereo-cbr.f32le")),
        ),
        (
            "mono-vbr",
            include_bytes!("mp3/fixtures/mono-vbr.mp3"),
            f32le(include_bytes!("mp3/fixtures/mono-vbr.f32le")),
        ),
        (
            "music",
            include_bytes!("mp3/fixtures/music.mp3"),
            i16le(include_bytes!("mp3/fixtures/music.i16le")),
        ),
        (
            "tiny-47-mono",
            include_bytes!("mp3/fixtures/tiny-47-mono.mp3"),
            i16le(include_bytes!("mp3/fixtures/tiny-47-mono.i16le")),
        ),
        (
            "tiny-47-stereo",
            include_bytes!("mp3/fixtures/tiny-47-stereo.mp3"),
            i16le(include_bytes!("mp3/fixtures/tiny-47-stereo.i16le")),
        ),
        (
            "gapless-mpeg2",
            include_bytes!("mp3/fixtures/gapless-mpeg2.mp3"),
            i16le(include_bytes!("mp3/fixtures/gapless-mpeg2.i16le")),
        ),
    ] {
        let audio = crate::decoder::decode(encoded).unwrap();
        assert_eq!(audio.samples.len(), frozen.len(), "{name}: length");
        // The references were decoded on x86_64, where they are bit exact.
        // minimp3's NEON synthesis orders its float sums differently from the
        // SSE one, so on aarch64 a few samples in ten thousand round to the
        // neighbouring 16-bit value (measured on a device: 219 of 960274 for
        // `music`, all one step). Bit-exactness against minimp3 itself on the
        // running architecture is the oracle tests' job (mp3_source_tests).
        let step = if cfg!(target_arch = "x86_64") {
            0.0
        } else {
            1.0 / 32768.0
        };
        assert!(
            audio
                .samples
                .iter()
                .zip(&frozen)
                .all(|(a, b)| (a - b).abs() <= step),
            "{name}: PCM differs from the original decoder"
        );
    }
    // Vorbis: the frozen mature decoder, to float rounding.
    for (name, encoded, frozen) in [
        (
            "0",
            include_bytes!("ogg-fixtures/0.ogg").as_slice(),
            include_bytes!("ogg-fixtures/0.f32le").as_slice(),
        ),
        (
            "1",
            include_bytes!("ogg-fixtures/1.ogg"),
            include_bytes!("ogg-fixtures/1.f32le"),
        ),
        (
            "2",
            include_bytes!("ogg-fixtures/2.ogg"),
            include_bytes!("ogg-fixtures/2.f32le"),
        ),
    ] {
        let audio = crate::decoder::decode(encoded).unwrap();
        let frozen = f32le(frozen);
        assert_eq!(audio.samples.len(), frozen.len(), "Vorbis {name}: length");
        let max = audio
            .samples
            .iter()
            .zip(&frozen)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(max < 1e-6, "Vorbis {name}: PCM differs by {max}");
    }
    // AAC: an independent decoder, so equal within codec tolerance.
    for number in 0..3 {
        let encoded = match number {
            0 => include_bytes!("aac-fixtures/adts-0.aac").as_slice(),
            1 => include_bytes!("aac-fixtures/adts-1.aac"),
            _ => include_bytes!("aac-fixtures/adts-2.aac"),
        };
        let frozen = f32le(match number {
            0 => include_bytes!("aac-fixtures/adts-0.f32le").as_slice(),
            1 => include_bytes!("aac-fixtures/adts-1.f32le"),
            _ => include_bytes!("aac-fixtures/adts-2.f32le"),
        });
        let audio = crate::decoder::decode(encoded).unwrap();
        assert_eq!(audio.samples.len(), frozen.len(), "ADTS {number}: length");
        let (mut square, mut maximum) = (0.0, 0.0_f64);
        for (actual, expected) in audio.samples.iter().zip(&frozen) {
            let error = f64::from(actual - expected).abs();
            square += error * error;
            maximum = maximum.max(error);
        }
        assert!(
            maximum < 0.01 && (square / frozen.len() as f64).sqrt() < 0.001,
            "ADTS {number}: PCM differs from the independent decoder"
        );
    }
}

#[test]
fn every_m4a_layout_keeps_its_format_and_presentation_length() {
    for (name, encoded, rate, channels, frames) in crate::decoder::aac_fixture_cases() {
        let audio = crate::decoder::decode(encoded).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(
            (audio.sample_rate, audio.channels, audio.frame_count()),
            (*rate, *channels, *frames),
            "{name}"
        );
    }
}
