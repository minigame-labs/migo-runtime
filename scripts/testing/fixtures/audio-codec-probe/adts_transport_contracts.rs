//! Actual HTTP -> shared CPU -> AAC-LC -> bounded sinc/bypass -> player.
use super::*;
use crate::resampled_streaming::{AdtsResampled, core};
use rubato::{Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice};
use symphonia_codec_aac::streaming::{AdtsPacketizer, PacketStep};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(std::env::var_os("W09_ADTS_FIXTURES").unwrap()).join(name)).unwrap()
}
fn decoded(file: &[u8]) -> crate::decoder::DecodedAudio {
    crate::candidate::decode(file, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize).unwrap()
}
fn reference(audio: &crate::decoder::DecodedAudio, target: u32) -> Vec<f32> {
    if audio.sample_rate == target {
        return audio.samples.clone();
    }
    let channels = audio.channels as usize;
    let input_frames = audio.samples.len() / channels;
    let source = InterleavedSlice::new(&audio.samples, channels, input_frames).unwrap();
    let output = core(audio.sample_rate, target, audio.channels)
        .unwrap()
        .process_all(&source, input_frames, None)
        .unwrap();
    let frames = (input_frames as u64 * target as u64).div_ceil(audio.sample_rate as u64) as usize;
    let mut expected = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in 0..channels {
            expected.push(output.read_sample(ch, i).unwrap());
        }
    }
    expected
}
fn first_frame(file: &[u8]) -> &[u8] {
    let mut parser = AdtsPacketizer::new(Default::default());
    let PacketStep::Packet { consumed, .. } = parser.next(file, false).unwrap() else {
        panic!("fixture must start with ADTS");
    };
    &file[..consumed]
}
async fn receive(
    file: Vec<u8>,
    target: u32,
    channels: u32,
    fragment: usize,
    reply: Reply,
) -> Vec<f32> {
    let state = StreamingState::new();
    let (url, serving, _) = server(file, fragment, reply).await;
    let (tx, mut rx) = crate::streaming::stream_channel();
    let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), move || {
        AdtsResampled::new(target)
    }));
    let (mut ready, mut done) = (0, 0);
    let mut actual = Vec::new();
    while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
        match message {
            StreamMsg::Ready {
                sample_rate,
                channels: received,
                ..
            } => {
                ready += 1;
                assert_eq!((sample_rate, received), (target, channels));
            }
            StreamMsg::Samples { pcm, .. } => {
                assert_eq!((ready, done), (1, 0));
                assert!(pcm.len() <= crate::bandlimited::MAX_SCRATCH_SAMPLES);
                actual.extend_from_slice(&pcm);
            }
            StreamMsg::Done { .. } => done += 1,
            StreamMsg::Error { message: error, .. } => panic!("ADTS transport error: {error}"),
        }
    }
    finished(task).await;
    finished(serving).await;
    assert_eq!((ready, done), (1, 1));
    assert!(state.download_complete.load(Ordering::Acquire));
    actual
}

#[tokio::test(flavor = "current_thread")]
async fn http_adts_profiles_match_exact_pcm_timeline() {
    let mut count = 0;
    for i in 0..4 {
        let file = data(&format!("{i}.aac"));
        let audio = decoded(&file);
        for target in [
            audio.sample_rate,
            if audio.sample_rate == 48000 {
                44100
            } else {
                48000
            },
        ] {
            let expected = reference(&audio, target);
            for fragment in [1, 137, 65536] {
                for reply in [Reply::Fixed, Reply::Chunked] {
                    assert_eq!(
                        receive(file.clone(), target, audio.channels, fragment, reply).await,
                        expected,
                        "{i}/{target}/{fragment}"
                    );
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 48);
    println!("W09_HTTP_ADTS {count} exact PCM/timeline comparisons");
}

#[tokio::test(flavor = "current_thread")]
async fn first_adts_packet_arrives_before_server_releases_remaining_body() {
    let file = data("1.aac");
    let audio = decoded(&file);
    let prefix = first_frame(&file).len();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}/adts", listener.local_addr().unwrap());
    let release = Arc::new(tokio::sync::Notify::new());
    let blocked = release.clone();
    let serving = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    file.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(&file[..prefix]).await.unwrap();
        blocked.notified().await;
    });
    let state = StreamingState::new();
    let (tx, mut rx) = crate::streaming::stream_channel();
    let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), || {
        AdtsResampled::new(48000)
    }));
    assert!(matches!(
        tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap(),
        Some(StreamMsg::Ready {
            sample_rate: 48000,
            channels: 2,
            ..
        })
    ));
    match tokio::time::timeout(PATIENCE, rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        StreamMsg::Samples { pcm, .. } => assert_eq!(&*pcm, &audio.samples[..1024 * 2]),
        _ => panic!("expected first AAC block"),
    }
    assert!(!state.download_complete.load(Ordering::Acquire));
    state.cancel();
    finished(task).await;
    release.notify_waiters();
    serving.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_adts_and_http_truncation_report_error_without_done() {
    let file = data("1.aac");
    let mut wrong_profile = file.clone();
    wrong_profile[2] &= 0x3f;
    let mut header_tail = file.clone();
    header_tail.extend_from_slice(&[0xff, 0xf1]);
    let mut payload_tail = file.clone();
    payload_tail.pop();
    for (file, reply, invalid_header) in [
        (wrong_profile, Reply::Fixed, true),
        (header_tail, Reply::Fixed, false),
        (payload_tail, Reply::Chunked, false),
        (file, Reply::TruncatedBody, false),
    ] {
        let (url, serving, _) = server(file, 137, reply).await;
        let state = StreamingState::new();
        let (tx, mut rx) = crate::streaming::stream_channel();
        let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), || {
            AdtsResampled::new(44100)
        }));
        let mut errors = 0;
        while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
            match message {
                StreamMsg::Error { message: _, .. } => errors += 1,
                StreamMsg::Done { .. } => panic!("invalid AAC/HTTP published Done"),
                _ => {
                    assert!(!invalid_header, "invalid profile published audio");
                    assert_eq!(errors, 0, "publication after error");
                }
            }
        }
        assert_eq!(errors, 1);
        assert!(!state.download_complete.load(Ordering::Acquire));
        finished(task).await;
        finished(serving).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn large_id3_prefix_is_skipped_incrementally_before_adts() {
    let file = data("1.aac");
    let audio = decoded(&file);
    let expected = reference(&audio, 44100);
    let len = 2 * 1024 * 1024;
    let mut tagged = b"ID3\x04\0\0".to_vec();
    for shift in [21, 14, 7, 0] {
        tagged.push(((len >> shift) & 0x7f) as u8);
    }
    tagged.resize(tagged.len() + len, 0xff);
    tagged.extend_from_slice(&file);
    assert_eq!(
        receive(tagged, 44100, audio.channels, 4096, Reply::Chunked).await,
        expected
    );
}

#[tokio::test(flavor = "current_thread")]
async fn adts_player_matches_reference_through_pause_and_end() {
    use super::playback::{HASH_SEED, hash_samples, play_format};
    let mut cases = 0;
    for i in 0..4 {
        let file = data(&format!("{i}.aac"));
        let audio = decoded(&file);
        for target in [
            audio.sample_rate,
            if audio.sample_rate == 48000 {
                44100
            } else {
                48000
            },
        ] {
            let expected = reference(&audio, target);
            let (url, serving, _) = server(file.clone(), 137, Reply::Chunked).await;
            let row = play_format::<16, crate::push_decoder::Adts>(
                "adts-profile",
                target,
                audio.channels,
                expected.len() / audio.channels as usize,
                i == 3,
                false,
                client(),
                (url, serving),
            )
            .await;
            assert_eq!(
                row["pcm_hash"],
                format!("{:016x}", hash_samples(HASH_SEED, &expected))
            );
            if i == 3 {
                assert_eq!(row["pauses"], 1);
            }
            cases += 1;
        }
    }
    assert_eq!(cases, 8);
    println!("W09_ADTS_PLAYER {cases} timeline comparisons, including pause/backpressure");
}

// Repeat a real encoded silent AAC frame on demand. The encoded and decoded
// whole stream are never retained by this server or the virtual player.
async fn silent_stream(packets: usize) -> (String, tokio::task::JoinHandle<()>) {
    let file = data("silence-8000.aac");
    let audio = decoded(&file);
    assert_eq!((audio.sample_rate, audio.channels), (8000, 1));
    assert!(audio.samples.iter().all(|v| v.abs() < 1e-7));
    let frame = first_frame(&file).to_vec();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}/adts", listener.local_addr().unwrap());
    let serving = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    frame.len() * packets
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        // Batch writes only in this fixed-size server buffer, to avoid turning
        // localhost packet scheduling into the virtual consumer's bottleneck.
        let per_batch = (8192 / frame.len()).max(1);
        let mut batch = Vec::with_capacity(per_batch * frame.len());
        for _ in 0..per_batch {
            batch.extend_from_slice(&frame);
        }
        let mut remaining = packets;
        while remaining > 0 {
            let n = remaining.min(per_batch);
            if socket.write_all(&batch[..n * frame.len()]).await.is_err() {
                return;
            }
            remaining -= n;
            tokio::task::yield_now().await;
        }
    });
    (url, serving)
}
#[tokio::test(flavor = "current_thread")]
async fn long_adts_http_player_exceeds_cache_quota_with_bounded_pcm() {
    const PACKETS: usize = 17200;
    const FRAMES: usize = PACKETS * 1024;
    assert!(FRAMES as u64 > crate::limits::MAX_AUDIO_PCM_SAMPLES);
    let stream = silent_stream(PACKETS).await;
    let row = super::playback::play_format::<16, crate::push_decoder::Adts>(
        "generated-adts-2201.6s",
        8000,
        1,
        FRAMES,
        false,
        true,
        client(),
        stream,
    )
    .await;
    assert!(row["player_peak_pcm_capacity_bytes"].as_u64().unwrap() < 4 * 1024 * 1024);
    println!("W09_LONG_ADTS_PLAYER {row}");
}

#[test]
fn bounded_adts_decode_resample_steps_have_no_steady_rust_allocation() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let file = data("3.aac");
    let mut decoder = AdtsResampled::new(44100);
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let cancel = AtomicBool::new(false);
    let (mut offset, mut samples) = (0, 0);
    assert_no_steady_state_allocation(
        Burst {
            path: "AAC-LC->sinc bounded step",
            warmup: 32,
            measured: 64,
        },
        |_| {
            out.clear();
            let step = decoder
                .step(&file[offset..], false, &mut out, &cancel)
                .unwrap();
            offset += step.consumed;
            assert!(offset <= file.len());
            assert_ne!(step.progress, Progress::Finished);
            samples += out.len();
        },
    );
    assert!(samples > 0);
}

struct Observed(AdtsResampled, Arc<AtomicUsize>);
impl Decoder for Observed {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0.step(input, eof, pcm, cancel)
    }
}
#[tokio::test(flavor = "current_thread")]
async fn adts_queue_backpressure_and_cancel_stop_cpu_steps() {
    let (url, serving, _) = server(data("3.aac"), 65536, Reply::Chunked).await;
    let state = StreamingState::new();
    let (tx, rx) = crate::streaming::stream_channel();
    let steps = Arc::new(AtomicUsize::new(0));
    let count = steps.clone();
    let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), move || {
        Observed(AdtsResampled::new(44100), count)
    }));
    tokio::time::timeout(PATIENCE, async {
        while rx.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let before = steps.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        steps.load(Ordering::SeqCst),
        before,
        "full queue kept decoding AAC"
    );
    state.cancel();
    finished(task).await;
    drop(rx);
    finished(serving).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(steps.load(Ordering::SeqCst), before);
}
