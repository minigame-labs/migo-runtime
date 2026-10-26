use super::*;
use crate::{
    resampled_streaming::{Resampled, core},
    streaming_transport::Decoder,
};
use rubato::{Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice};

#[tokio::test(flavor = "current_thread")]
async fn http_resampling_and_bypass_match_library_timeline_with_exact_done() {
    for (name, target) in [
        ("cbr-44100-1.mp3", 48000),
        ("vbr-48000-2.mp3", 44100),
        ("mpeg25-8000-2.mp3", 48000),
        ("tiny.mp3", 44100),
        ("long.mp3", 48000),
    ] {
        let data = fixture(name);
        let decoded =
            crate::candidate::decode(&data, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize).unwrap();
        let frames = (decoded.samples.len() as u128 / decoded.channels as u128 * target as u128)
            .div_ceil(decoded.sample_rate as u128) as usize;
        let expected = if decoded.sample_rate == target {
            decoded.samples.clone()
        } else {
            let source = InterleavedSlice::new(
                &decoded.samples,
                decoded.channels as usize,
                decoded.samples.len() / decoded.channels as usize,
            )
            .unwrap();
            let reference = core(decoded.sample_rate, target, decoded.channels)
                .unwrap()
                .process_all(
                    &source,
                    decoded.samples.len() / decoded.channels as usize,
                    None,
                )
                .unwrap();
            let mut output = Vec::with_capacity(frames * decoded.channels as usize);
            for frame in 0..frames {
                for ch in 0..decoded.channels as usize {
                    output.push(reference.read_sample(ch, frame).unwrap());
                }
            }
            output
        };
        for chunk_size in [137, 100000] {
            let state = StreamingState::new();
            let (url, serving, _) = server(data.clone(), chunk_size, Reply::Chunked).await;
            let (tx, mut rx) = crate::streaming::stream_channel();
            let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), move || {
                Resampled::new(target)
            }));
            let mut actual = Vec::new();
            let mut ready = 0;
            let mut done = 0;
            while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
                match message {
                    StreamMsg::Ready {
                        sample_rate,
                        channels,
                    } => {
                        ready += 1;
                        assert_eq!((sample_rate, channels), (target, decoded.channels));
                    }
                    StreamMsg::Samples(pcm) => {
                        assert_eq!((ready, done), (1, 0));
                        assert!(pcm.len() <= crate::bandlimited::MAX_SCRATCH_SAMPLES);
                        actual.extend_from_slice(&pcm);
                    }
                    StreamMsg::Done => done += 1,
                    StreamMsg::Error(error) => panic!("{name}/{target}/{chunk_size}: {error}"),
                }
            }
            finished(task).await;
            finished(serving).await;
            assert_eq!((ready, done), (1, 1));
            assert_eq!(actual, expected, "{name}/{target}/{chunk_size}");
            assert!(state.download_complete.load(Ordering::Acquire));
        }
    }
}

#[test]
fn bounded_codec_and_resampler_steps_have_no_steady_rust_allocation() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let source = fixture("long.mp3");
    let mut decoder = Resampled::new(44100);
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let cancel = AtomicBool::new(false);
    let mut offset = 0;
    let mut samples = 0;
    assert_no_steady_state_allocation(
        Burst {
            path: "MP3->sinc composed bounded decode/resample step",
            warmup: 32,
            measured: 64,
        },
        |_| {
            out.clear();
            let step = decoder
                .step(&source[offset..], false, &mut out, &cancel)
                .unwrap();
            offset += step.consumed;
            assert!(offset <= source.len());
            assert_ne!(step.progress, Progress::Finished);
            samples += out.len();
        },
    );
    assert!(samples > 0);
}
