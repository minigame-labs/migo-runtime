//! Actual HTTP, shared CPU, bounded Rubato and PCM publication for WAVE.
use super::*;
use symphonia::core as symphonia_core;
include!("wave_pcm_contracts.rs");
use crate::resampled_streaming::{WaveResampled, core};
use rubato::{Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice};

fn reference(source: &[f32], channels: u32, target: u32) -> Vec<f32> {
    if target == 44100 {
        return source.to_vec();
    }
    let input_frames = source.len() / channels as usize;
    let source = InterleavedSlice::new(source, channels as usize, input_frames).unwrap();
    let output = core(44100, target, channels)
        .unwrap()
        .process_all(&source, input_frames, None)
        .unwrap();
    let frames = (input_frames as u64 * target as u64).div_ceil(44100) as usize;
    let mut expected = Vec::with_capacity(frames * channels as usize);
    for i in 0..frames {
        for ch in 0..channels as usize {
            expected.push(output.read_sample(ch, i).unwrap());
        }
    }
    expected
}
async fn receive(file: Vec<u8>, target: u32, channels: u32, fragment: usize) -> Vec<f32> {
    let state = StreamingState::new();
    let (url, serving, _) = server(file, fragment, Reply::Chunked).await;
    let (tx, mut rx) = crate::streaming::stream_channel();
    let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), move || {
        WaveResampled::new(target)
    }));
    let (mut ready, mut done) = (0, 0);
    let mut actual = Vec::new();
    while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
        match message {
            StreamMsg::Ready {
                sample_rate,
                channels: received,
            } => {
                ready += 1;
                assert_eq!((sample_rate, received), (target, channels));
            }
            StreamMsg::Samples(pcm) => {
                assert_eq!((ready, done), (1, 0));
                assert!(pcm.len() <= crate::bandlimited::MAX_SCRATCH_SAMPLES);
                actual.extend_from_slice(&pcm);
            }
            StreamMsg::Done => done += 1,
            StreamMsg::Error(error) => panic!("WAVE transport error: {error}"),
        }
    }
    finished(task).await;
    finished(serving).await;
    assert_eq!((ready, done), (1, 1));
    assert!(state.download_complete.load(Ordering::Acquire));
    actual
}

#[tokio::test(flavor = "current_thread")]
async fn http_wave_profiles_resampling_and_bypass_match_exact_pcm_timeline() {
    let mut cases = 0;
    for (bits, float, channels, target) in [
        (8, false, 1, 44100),
        (16, false, 2, 48000),
        (24, false, 1, 48000),
        (32, false, 2, 44100),
        (32, true, 2, 48000),
        (16, false, 6, 48000),
        (16, false, 26, 48000),
    ] {
        for frames in [17, 9737] {
            let (file, source) = fixture(bits, float, channels, frames);
            let expected = reference(&source, channels as u32, target);
            for fragment in [137, 65536] {
                let actual = receive(file.clone(), target, channels as u32, fragment).await;
                assert_eq!(
                    actual, expected,
                    "{bits}/{float}/{channels}/{target}/{frames}/{fragment}"
                );
                cases += 1;
            }
        }
    }
    println!("W09_HTTP_WAVE {cases} exact PCM/timeline comparisons");
    assert_eq!(cases, 28);
}

#[tokio::test(flavor = "current_thread")]
async fn wave_first_frame_arrives_before_server_releases_the_remaining_body() {
    let (file, expected) = fixture(16, false, 2, 9737);
    let prefix = file.len() - expected.len() * 2 + 4;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}/wave", listener.local_addr().unwrap());
    let release = Arc::new(tokio::sync::Notify::new());
    let blocked = release.clone();
    let serving = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 4096];
        let mut used = 0;
        loop {
            used += socket.read(&mut request[used..]).await.unwrap();
            if request[..used].windows(4).any(|p| p == b"\r\n\r\n") {
                break;
            }
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
        WaveResampled::new(44100)
    }));
    assert!(matches!(
        tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap(),
        Some(StreamMsg::Ready {
            sample_rate: 44100,
            channels: 2
        })
    ));
    match tokio::time::timeout(PATIENCE, rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        StreamMsg::Samples(pcm) => assert_eq!(&*pcm, &expected[..2]),
        _ => panic!("expected first PCM frame"),
    }
    assert!(!state.download_complete.load(Ordering::Acquire));
    state.cancel();
    tokio::time::timeout(PATIENCE, task).await.unwrap().unwrap();
    release.notify_waiters();
    serving.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_wave_delivers_error_without_done() {
    let (file, _) = fixture(16, false, 2, 17);
    let mut magic = file.clone();
    magic[0] = b'X';
    let mut channels = file.clone();
    let fmt = channels.windows(4).position(|p| p == b"fmt ").unwrap() + 8;
    channels[fmt + 2..fmt + 4].copy_from_slice(&65535_u16.to_le_bytes());
    let mut truncated = file;
    truncated.pop();
    for (data, invalid_header) in [(magic, true), (channels, true), (truncated, false)] {
        let (url, serving, _) = server(data, 65536, Reply::Fixed).await;
        let state = StreamingState::new();
        let (tx, mut rx) = crate::streaming::stream_channel();
        let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), || {
            WaveResampled::new(44100)
        }));
        let mut errors = 0;
        while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
            match message {
                StreamMsg::Error(_) => errors += 1,
                StreamMsg::Ready { .. } | StreamMsg::Samples(_) => {
                    assert!(!invalid_header, "invalid header published PCM");
                    assert_eq!(errors, 0, "published PCM after error");
                }
                StreamMsg::Done => panic!("truncated WAVE reported success"),
            }
        }
        assert_eq!(errors, 1);
        finished(task).await;
        finished(serving).await;
        assert!(!state.download_complete.load(Ordering::Acquire));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn large_optional_wave_metadata_streams_without_holding_the_whole_prefix() {
    let (file, expected) = fixture(16, false, 1, 2317);
    let mut body = b"INFO".to_vec();
    chunk(&mut body, b"INAM", &vec![b'x'; 2 * 1024 * 1024]);
    let mut large = file[..12].to_vec();
    chunk(&mut large, b"LIST", &body);
    large.extend_from_slice(&file[12..]);
    let len = large.len() as u32 - 8;
    large[4..8].copy_from_slice(&len.to_le_bytes());
    assert_eq!(receive(large, 44100, 1, 4096).await, expected);
}

#[tokio::test(flavor = "current_thread")]
async fn wave_audio_extent_end_still_checks_the_http_tail() {
    let (mut file, expected) = fixture(16, false, 2, 17);
    chunk(&mut file, b"JUNK", &vec![0; 128 * 1024]);
    let riff_size = file.len() as u32 - 8;
    file[4..8].copy_from_slice(&riff_size.to_le_bytes());
    assert_eq!(receive(file.clone(), 44100, 2, 137).await, expected);
    let (url, serving, _) = server(file, 137, Reply::TruncatedBody).await;
    let state = StreamingState::new();
    let (tx, mut rx) = crate::streaming::stream_channel();
    let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), || {
        WaveResampled::new(44100)
    }));
    let mut errors = 0;
    while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
        match message {
            StreamMsg::Error(_) => errors += 1,
            StreamMsg::Done => panic!("audio extent end hid truncated HTTP body"),
            _ => assert_eq!(errors, 0, "publication after terminal error"),
        }
    }
    assert_eq!(errors, 1);
    assert!(!state.download_complete.load(Ordering::Acquire));
    finished(task).await;
    finished(serving).await;
}

#[tokio::test(flavor = "current_thread")]
async fn wave_player_pcm_end_and_pause_match_the_numeric_oracle() {
    use super::playback::{HASH_SEED, hash_samples, play_format};
    for (bits, float, channels, target) in [
        (8, false, 1, 44100),
        (16, false, 2, 48000),
        (24, false, 1, 48000),
        (32, false, 2, 44100),
        (32, true, 2, 48000),
        (16, false, 6, 48000),
        (16, false, 26, 48000),
    ] {
        for frames in [17, 9737] {
            let (file, source) = fixture(bits, float, channels, frames);
            let expected = reference(&source, channels as u32, target);
            let (url, serving, _) = server(file, 8192, Reply::Chunked).await;
            let row = play_format::<16, crate::push_decoder::Wave>(
                "wave-profile",
                target,
                channels as u32,
                expected.len() / channels as usize,
                false,
                false,
                client(),
                (url, serving),
            )
            .await;
            assert_eq!(
                row["pcm_hash"],
                format!("{:016x}", hash_samples(HASH_SEED, &expected))
            );
        }
    }
    let (file, source) = fixture(16, false, 2, 97371);
    let expected = reference(&source, 2, 48000);
    let (url, serving, _) = server(file, 137, Reply::Chunked).await;
    let row = play_format::<16, crate::push_decoder::Wave>(
        "wave-pause",
        48000,
        2,
        expected.len() / 2,
        true,
        false,
        client(),
        (url, serving),
    )
    .await;
    assert_eq!(row["pauses"], 1);
    assert_eq!(
        row["pcm_hash"],
        format!("{:016x}", hash_samples(HASH_SEED, &expected))
    );
    println!(
        "W09_WAVE_PLAYER 15 numeric/resampling timeline comparisons, including pause/backpressure"
    );
}

// Generate a long encoded body on demand, so neither the test server nor the
// player retains an encoded whole-file fixture. The only payload storage in
// this server is the small header and one fixed zero block.
async fn zero_stream(frames: usize) -> (String, tokio::task::JoinHandle<()>) {
    let (mut header, _) = fixture(16, false, 1, 1);
    header.truncate(header.len() - 2);
    let bytes = u32::try_from(frames * 2).unwrap();
    let riff_size = header.len() as u32 - 8 + bytes;
    header[4..8].copy_from_slice(&riff_size.to_le_bytes());
    let end = header.len();
    header[end - 4..].copy_from_slice(&bytes.to_le_bytes());
    let fmt = header.windows(4).position(|p| p == b"fmt ").unwrap() + 8;
    header[fmt + 4..fmt + 8].copy_from_slice(&8000_u32.to_le_bytes());
    header[fmt + 8..fmt + 12].copy_from_slice(&16000_u32.to_le_bytes());
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}/wave", listener.local_addr().unwrap());
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
                    header.len() + bytes as usize
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(&header).await.unwrap();
        let block = [0_u8; 8192];
        let mut remaining = bytes as usize;
        while remaining > 0 {
            let count = remaining.min(block.len());
            if socket.write_all(&block[..count]).await.is_err() {
                return;
            }
            remaining -= count;
            tokio::task::yield_now().await;
        }
    });
    (url, serving)
}

#[tokio::test(flavor = "current_thread")]
async fn long_wave_http_player_crosses_cache_quota_with_bounded_pcm() {
    const FRAMES: usize = 17_600_000;
    assert!(FRAMES as u64 > crate::limits::MAX_AUDIO_PCM_SAMPLES);
    let stream = zero_stream(FRAMES).await;
    let row = super::playback::play_format::<16, crate::push_decoder::Wave>(
        "generated-wave-2200s",
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
    println!("W09_LONG_WAVE_PLAYER {row}");
}
