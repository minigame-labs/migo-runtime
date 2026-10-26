//! Real HTTP/CPU/sinc/player composition for borrowed and assembled Vorbis.
use super::*;
use crate::resampled_streaming::{VorbisResampled, core};
use rubato::{Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice};
use symphonia_format_ogg::streaming::{PacketStep, PagePacketizer, PageStep, VorbisPacketizer};
fn data(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(std::env::var_os("W09_OGG_FIXTURES").unwrap()).join(name)).unwrap()
}
fn decoded(file: &[u8]) -> crate::decoder::DecodedAudio {
    crate::candidate::decode(file, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize).unwrap()
}
fn reference(audio: &crate::decoder::DecodedAudio, target: u32) -> Vec<f32> {
    if target == audio.sample_rate {
        return audio.samples.clone();
    }
    let frames = audio.samples.len() / audio.channels as usize;
    let source = InterleavedSlice::new(&audio.samples, audio.channels as usize, frames).unwrap();
    let output = core(audio.sample_rate, target, audio.channels)
        .unwrap()
        .process_all(&source, frames, None)
        .unwrap();
    let count = (frames as u64 * target as u64).div_ceil(audio.sample_rate as u64) as usize;
    let mut result = Vec::with_capacity(count * audio.channels as usize);
    for frame in 0..count {
        for channel in 0..audio.channels as usize {
            result.push(output.read_sample(channel, frame).unwrap());
        }
    }
    result
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
        VorbisResampled::new(target)
    }));
    let (mut ready, mut done) = (0, 0);
    let mut actual = Vec::new();
    while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
        match message {
            StreamMsg::Ready {
                sample_rate,
                channels: got,
            } => {
                ready += 1;
                assert_eq!((sample_rate, got), (target, channels));
            }
            StreamMsg::Samples(pcm) => {
                assert_eq!((ready, done), (1, 0));
                assert!(pcm.len() <= crate::bandlimited::MAX_SCRATCH_SAMPLES);
                actual.extend_from_slice(&pcm);
            }
            StreamMsg::Done => done += 1,
            StreamMsg::Error(error) => panic!("Vorbis transport: {error}"),
        }
    }
    finished(task).await;
    finished(serving).await;
    assert_eq!((ready, done), (1, 1));
    assert!(state.download_complete.load(Ordering::Acquire));
    actual
}
#[tokio::test(flavor = "current_thread")]
async fn http_vorbis_profiles_match_exact_pcm_timeline() {
    let mut count = 0;
    for name in [
        "0.ogg",
        "1.ogg",
        "2.ogg",
        "3.ogg",
        "cross.ogg",
        "max-window-8.ogg",
    ] {
        let file = data(name);
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
                        "{name}/{target}/{fragment}"
                    );
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 72);
    println!("W09_HTTP_VORBIS {count} exact PCM/timeline comparisons");
}
fn first_audio_page_end(file: &[u8]) -> usize {
    let mut parser = PagePacketizer::new();
    let mut position = 0;
    loop {
        let PageStep::Page { consumed, page } = parser.next(&file[position..], false).unwrap()
        else {
            panic!()
        };
        position += consumed;
        if page.num_packets() >= 2
            && page
                .packets()
                .next()
                .is_some_and(|p| p.first().is_some_and(|b| b & 1 == 0))
        {
            return position;
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn first_vorbis_pcm_arrives_before_remaining_body_is_released() {
    let file = data("3.ogg");
    let audio = decoded(&file);
    let prefix = first_audio_page_end(&file);
    assert!(prefix < file.len());
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}/vorbis", listener.local_addr().unwrap());
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
        VorbisResampled::new(48000)
    }));
    assert!(matches!(
        tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap(),
        Some(StreamMsg::Ready {
            sample_rate: 48000,
            channels: 6
        })
    ));
    match tokio::time::timeout(PATIENCE, rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        StreamMsg::Samples(pcm) => {
            assert!(!pcm.is_empty());
            assert_eq!(&*pcm, &audio.samples[..pcm.len()]);
        }
        _ => panic!("expected valid first PCM"),
    }
    assert!(!state.download_complete.load(Ordering::Acquire));
    state.cancel();
    finished(task).await;
    release.notify_waiters();
    serving.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn bad_vorbis_crc_eos_chain_and_http_never_publish_done() {
    let file = data("1.ogg");
    let mut bad_crc = file.clone();
    bad_crc[22] ^= 1;
    let mut truncated = file.clone();
    truncated.pop();
    let mut chain = file.clone();
    chain.extend_from_slice(&file);
    for (file, reply, invalid_header) in [
        (bad_crc, Reply::Fixed, true),
        (truncated, Reply::Chunked, false),
        (chain, Reply::Fixed, false),
        (file, Reply::TruncatedBody, false),
    ] {
        let (url, serving, _) = server(file, 137, reply).await;
        let state = StreamingState::new();
        let (tx, mut rx) = crate::streaming::stream_channel();
        let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), || {
            VorbisResampled::new(44100)
        }));
        let mut errors = 0;
        while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
            match message {
                StreamMsg::Error(_) => errors += 1,
                StreamMsg::Done => panic!("invalid Ogg/HTTP published Done"),
                _ => {
                    assert!(!invalid_header);
                    assert_eq!(errors, 0);
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
async fn large_vorbis_comment_streams_without_assembly() {
    let expected = reference(&decoded(&data("clean.ogg")), 44100);
    assert_eq!(
        receive(data("large.ogg"), 44100, 2, 4096, Reply::Chunked).await,
        expected
    );
}
#[tokio::test(flavor = "current_thread")]
async fn vorbis_player_matches_reference_pause_and_ended() {
    use super::playback::{HASH_SEED, hash_samples, play_format};
    let mut cases = 0;
    for name in [
        "0.ogg",
        "1.ogg",
        "2.ogg",
        "3.ogg",
        "cross.ogg",
        "max-window-8.ogg",
    ] {
        let file = data(name);
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
            let row = play_format::<16, crate::push_decoder::Vorbis>(
                "vorbis-profile",
                target,
                audio.channels,
                expected.len() / audio.channels as usize,
                name == "3.ogg",
                false,
                client(),
                (url, serving),
            )
            .await;
            assert_eq!(
                row["pcm_hash"],
                format!("{:016x}", hash_samples(HASH_SEED, &expected)),
                "{name}/{target}"
            );
            if name == "3.ogg" {
                assert_eq!(row["pauses"], 1);
            }
            cases += 1;
        }
    }
    assert_eq!(cases, 12);
    println!("W09_VORBIS_PLAYER {cases} exact timeline comparisons");
}
fn silent_page(template: &[u8], sequence: u32, flags: u8, granule: u64, packets: usize) -> Vec<u8> {
    use symphonia::core::{checksum::Crc32, io::Monitor};
    assert!((1..=255).contains(&packets));
    let mut result = template[..27].to_vec();
    result[5] = flags;
    result[26] = packets as u8;
    result[6..14].copy_from_slice(&granule.to_le_bytes());
    result[18..22].copy_from_slice(&sequence.to_le_bytes());
    result[22..26].fill(0);
    result.extend(std::iter::repeat_n(1, packets));
    result.extend(std::iter::repeat_n(0x0e, packets));
    let mut crc = Crc32::new(0);
    crc.process_buf_bytes(&result);
    result[22..26].copy_from_slice(&crc.crc().to_le_bytes());
    result
}
async fn silent_stream() -> (usize, (String, tokio::task::JoinHandle<()>)) {
    let file = data("silence-8000.ogg");
    let audio = decoded(&file);
    assert_eq!((audio.sample_rate, audio.channels), (8000, 1));
    assert!(audio.samples.iter().all(|v| *v == 0.0));
    let mut parser = VorbisPacketizer::new(Default::default());
    let mut prefix = 0;
    loop {
        match parser.next(&file[prefix..], false).unwrap() {
            PacketStep::Discard(n) => prefix += n,
            PacketStep::Packet { .. } => break,
            _ => panic!("missing silent audio"),
        }
    }
    let params = parser
        .track()
        .unwrap()
        .codec_params
        .as_ref()
        .unwrap()
        .audio()
        .unwrap();
    let quantum = params.max_frames_per_packet.unwrap() as usize;
    let valid_packets = 17_600_000_usize.div_ceil(quantum);
    let frames = valid_packets * quantum;
    let primer = silent_page(&file[prefix..], 2, 0, 0, 1);
    let mut encoded_length = prefix + primer.len();
    let mut remaining = valid_packets;
    while remaining > 0 {
        let n = remaining.min(255);
        encoded_length += 27 + 2 * n;
        remaining -= n;
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}/vorbis", listener.local_addr().unwrap());
    let serving = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {encoded_length}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        socket.write_all(&file[..prefix]).await.unwrap();
        socket.write_all(&primer).await.unwrap();
        let (mut remaining, mut sent, mut sequence) = (valid_packets, 0, 3);
        while remaining > 0 {
            let n = remaining.min(255);
            remaining -= n;
            sent += n;
            let page = silent_page(
                &file[prefix..],
                sequence,
                if remaining == 0 { 4 } else { 0 },
                (sent * quantum) as u64,
                n,
            );
            sequence += 1;
            if socket.write_all(&page).await.is_err() {
                return;
            }
            tokio::task::yield_now().await;
        }
    });
    (frames, (url, serving))
}
#[tokio::test(flavor = "current_thread")]
async fn long_vorbis_http_player_has_bounded_pcm_beyond_cache_quota() {
    let (frames, stream) = silent_stream().await;
    assert!(frames as u64 > crate::limits::MAX_AUDIO_PCM_SAMPLES);
    let row = super::playback::play_format::<16, crate::push_decoder::Vorbis>(
        "generated-vorbis-2200s",
        8000,
        1,
        frames,
        false,
        true,
        client(),
        stream,
    )
    .await;
    assert!(row["player_peak_pcm_capacity_bytes"].as_u64().unwrap() < 4 * 1024 * 1024);
    // Includes setup construction, but rejects per-packet allocation in a
    // 68k-packet stream (the former allocated EOF path exceeded 200k events).
    assert!(row["inside_step_rust_allocations"].as_u64().unwrap() < 4096);
    println!("W09_LONG_VORBIS_PLAYER {row}");
}
#[test]
fn bounded_vorbis_resampling_has_no_steady_rust_allocation() {
    let file = data("3.ogg");
    let mut decoder = VorbisResampled::new(44100);
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let cancel = AtomicBool::new(false);
    let mut offset = 0;
    let mut step = || {
        out.clear();
        let result = decoder
            .step(&file[offset..], false, &mut out, &cancel)
            .unwrap();
        offset += result.consumed;
        assert_ne!(result.progress, Progress::Finished);
        out.len()
    };
    for _ in 0..16 {
        step();
    }
    let before = migo_alloc_probe::thread_counts().allocation_events();
    let mut count = 0;
    for _ in 0..32 {
        count += step();
    }
    let after = migo_alloc_probe::thread_counts().allocation_events();
    assert_eq!(after - before, 0);
    assert!(count > 0);
}
struct Observed(VorbisResampled, Arc<AtomicUsize>);
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
async fn vorbis_backpressure_and_cancel_stop_cpu_steps() {
    let (url, serving, _) = server(data("3.ogg"), 65536, Reply::Chunked).await;
    let state = StreamingState::new();
    let (tx, rx) = crate::streaming::stream_channel();
    let steps = Arc::new(AtomicUsize::new(0));
    let count = steps.clone();
    let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), move || {
        Observed(VorbisResampled::new(44100), count)
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
    assert_eq!(steps.load(Ordering::SeqCst), before);
    state.cancel();
    finished(task).await;
    drop(rx);
    finished(serving).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(steps.load(Ordering::SeqCst), before);
}
