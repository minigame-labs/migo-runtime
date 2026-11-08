use super::*;

#[path = "range_buffer_tests.rs"]
mod range_buffer;
use crate::streaming::{StreamMsg, StreamingState};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) fn wav(rate: u32, channels: u16, frames: usize) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut out,
        hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..frames {
        for ch in 0..channels {
            writer
                .write_sample(((frame * 137 + usize::from(ch) * 31) % 12001) as i16 - 6000)
                .unwrap();
        }
    }
    writer.finalize().unwrap();
    out.into_inner()
}
fn cases() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("wave-tiny", wav(8000, 1, 17)),
        ("wave-stereo", wav(44100, 2, 9737)),
        (
            "vorbis-tiny",
            include_bytes!("../decoder/ogg-fixtures/0.ogg").to_vec(),
        ),
        (
            "vorbis-stereo",
            include_bytes!("../decoder/ogg-fixtures/1.ogg").to_vec(),
        ),
        (
            "vorbis-mono",
            include_bytes!("../decoder/ogg-fixtures/2.ogg").to_vec(),
        ),
        (
            "vorbis-eight",
            include_bytes!("../decoder/ogg-fixtures/max-window-8.ogg").to_vec(),
        ),
        (
            "adts-mono",
            include_bytes!("../decoder/aac-fixtures/adts-0.aac").to_vec(),
        ),
        (
            "adts-stereo",
            include_bytes!("../decoder/aac-fixtures/adts-1.aac").to_vec(),
        ),
        (
            "adts-tiny",
            include_bytes!("../decoder/aac-fixtures/adts-2.aac").to_vec(),
        ),
        ("mp3", crate::mp3_fixture::stream(6)),
        (
            "vorbis-cross",
            include_bytes!("../decoder/ogg-fixtures/cross.ogg").to_vec(),
        ),
    ]
}
fn decode(data: &[u8], target: u32, chunk: usize) -> EngineResult<Vec<f32>> {
    let mut decoder = AutoDecoder::new(target);
    let mut offset = 0;
    let mut result = Vec::new();
    let mut pcm = Vec::with_capacity(32768);
    let address = pcm.as_ptr();
    let capacity = pcm.capacity();
    let mut want_input = true;
    for _ in 0..2_000_000 {
        let end = if want_input {
            (offset + chunk).min(data.len())
        } else {
            offset
        };
        let eof = offset == data.len();
        let step = decoder.step(&data[offset..end], eof, &mut pcm, &AtomicBool::new(false))?;
        assert_eq!(
            (pcm.as_ptr(), pcm.capacity()),
            (address, capacity),
            "stream codec replaced or grew the publication loan allocation"
        );
        assert!(
            step.consumed <= end - offset,
            "stream codec consumed beyond the offered suffix"
        );
        if !pcm.is_empty() {
            assert!(
                step.channels > 0 && pcm.len().is_multiple_of(step.channels as usize),
                "packet publication split a channel frame"
            );
            assert_eq!(
                step.sample_rate, target,
                "packet publication lost the submitted target"
            );
        }
        offset += step.consumed;
        result.extend_from_slice(&pcm);
        pcm.clear();
        match step.progress {
            Progress::Finished => {
                assert_eq!(offset, data.len());
                return Ok(result);
            }
            Progress::NeedInput => want_input = true,
            Progress::Yield => want_input = false,
            _ => panic!("unexpected sequential stream state"),
        }
    }
    panic!("stream codec failed to make bounded progress")
}

#[test]
fn partitioned_product_codecs_preserve_complete_pcm_and_loan_identity() {
    let mut outputs = 0;
    let mut samples = 0;
    for (name, data) in cases() {
        let native = crate::decoder::decode(&data).unwrap();
        for (variant, target) in [native.sample_rate, 16000, 48000].into_iter().enumerate() {
            let expected = crate::decoder::decode_for_rate(&data, target).unwrap();
            for chunk in [7, 137, 65536] {
                let actual=decode(&data,target,chunk).unwrap_or_else(|e|panic!("product progressive codec failed complete PCM: {name}/{target}/{chunk}: {e}"));
                assert_eq!(
                    actual, expected.samples,
                    "product progressive complete PCM changed: {name}/{target}/{chunk}"
                );
                outputs += 1;
                samples += actual.len();
                if std::env::var_os("MIGO_DUMP_PROGRESSIVE_PCM").is_some() {
                    println!(
                        "MIGO_PROGRESSIVE_PCM {name}/{variant}/{target}/{chunk} {:?}",
                        actual.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                    );
                }
            }
        }
    }
    println!("PROGRESSIVE_COMPLETE_COMPARISON {outputs} {samples}");
}

#[test]
fn one_byte_headers_and_large_tags_never_use_tag_payload_as_format() {
    let original = include_bytes!("../decoder/aac-fixtures/adts-1.aac");
    let expected = crate::decoder::decode_for_rate(original, 48000).unwrap();
    for body in [128, 65536, 3 * 1024 * 1024] {
        let mut bytes = b"ID3\x04\0\0".to_vec();
        bytes.extend([
            (body >> 21) as u8 & 127,
            (body >> 14) as u8 & 127,
            (body >> 7) as u8 & 127,
            body as u8 & 127,
        ]);
        bytes.resize(10 + body, 0);
        bytes[10..14].copy_from_slice(b"OggS");
        bytes.extend(original);
        assert_eq!(
            decode(&bytes, 48000, 8192)
                .unwrap_or_else(|_| panic!("ADTS metadata was decoded or dispatched as audio")),
            expected.samples,
            "ADTS metadata changed complete PCM"
        );
    }
    for (name, bytes) in cases().into_iter().filter(|(n, _)| n.ends_with("tiny")) {
        assert_eq!(
            decode(&bytes, 48000, 1).unwrap(),
            crate::decoder::decode_for_rate(&bytes, 48000)
                .unwrap()
                .samples,
            "one-byte stream lost its prefix: {name}"
        );
    }
    let tag = b"ID3\x04\0\0\0\0\0\0";
    let mut boundary = tag.repeat(32);
    boundary.extend(original);
    assert_eq!(
        decode(&boundary, 48000, 7).unwrap(),
        expected.samples,
        "32 legal tag extents changed progressive PCM"
    );
    assert_eq!(
        crate::decoder::decode_for_rate(&boundary, 48000)
            .unwrap_or_else(|_| panic!("whole-file tag boundary disagrees with streaming"))
            .samples,
        expected.samples,
        "whole-file tag boundary disagrees with streaming"
    );
    let mut exceeded = tag.repeat(33);
    exceeded.extend(original);
    assert!(
        decode(&exceeded, 48000, 7).is_err(),
        "progressive metadata tag budget was ignored"
    );
    let mut decoder = AutoDecoder::new(48000);
    let mut pcm = Vec::with_capacity(32768);
    let huge = b"ID3\x04\0\0\x7f\x7f\x7f\x7f";
    let step = decoder
        .step(huge, false, &mut pcm, &AtomicBool::new(false))
        .unwrap();
    assert!(
        decoder
            .step(
                &huge[step.consumed..],
                false,
                &mut pcm,
                &AtomicBool::new(false)
            )
            .is_err(),
        "oversized metadata was not refused at its header"
    );
}

#[test]
fn extreme_rates_and_32_channels_keep_the_current_cascade_and_exact_tail() {
    for (rate, channels, frames, target) in [(768000, 1, 8193, 3000), (3000, 32, 17, 768000)] {
        let bytes = wav(rate, channels, frames);
        let expected = crate::decoder::decode_for_rate(&bytes, target).unwrap();
        let actual = decode(&bytes, target, 137)
            .unwrap_or_else(|e| panic!("progressive cascade failed its supported format: {e}"));
        assert_eq!(
            actual, expected.samples,
            "progressive extreme-ratio PCM or final tail changed"
        );
    }
}

#[test]
fn codec_errors_and_cancellation_remain_terminal_without_pcm() {
    let mut trailer = wav(48000, 2, 128);
    let expected = crate::decoder::decode_for_rate(&trailer, 48000).unwrap();
    trailer.extend(b"LIST\x0c\0\0\0INFOISFT\0\0\0\0");
    let size = (trailer.len() - 8) as u32;
    trailer[4..8].copy_from_slice(&size.to_le_bytes());
    assert_eq!(
        decode(&trailer, 48000, 137).unwrap(),
        expected.samples,
        "valid WAVE trailing metadata changed PCM"
    );
    let mut too_much = wav(48000, 2, 128);
    too_much.resize(too_much.len() + 4 * 1024 * 1024 + 1, 0);
    assert!(
        decode(&too_much, 48000, 8192).is_err(),
        "progressive WAVE metadata budget was ignored"
    );
    let small = wav(48000, 2, 128);
    let mut leading = small[..12].to_vec();
    leading.extend(b"JUNK");
    leading.extend((4_u32 * 1024 * 1024).to_le_bytes());
    leading.resize(leading.len() + 4 * 1024 * 1024, 0);
    leading.extend(&small[12..]);
    let size = (leading.len() - 8) as u32;
    leading[4..8].copy_from_slice(&size.to_le_bytes());
    assert!(
        decode(&leading, 48000, 8192).is_err(),
        "progressive WAVE metadata budget was ignored"
    );
    for (_, bytes) in cases().into_iter().filter(|(n, _)| *n != "mp3") {
        for stop in [0, 3, 17, 111] {
            let mut d = AutoDecoder::new(48000);
            let mut pcm = Vec::with_capacity(32768);
            let mut offset = 0;
            for _ in 0..stop {
                let end = (offset + 137).min(bytes.len());
                let step = d
                    .step(
                        &bytes[offset..end],
                        false,
                        &mut pcm,
                        &AtomicBool::new(false),
                    )
                    .unwrap();
                offset += step.consumed;
                pcm.clear();
            }
            let result = d.step(&[], false, &mut pcm, &AtomicBool::new(true));
            assert!(
                matches!(
                    result,
                    Ok(Step {
                        progress: Progress::Cancelled,
                        ..
                    })
                ),
                "progressive codec ignored cancellation"
            );
            assert!(
                d.codec.is_none(),
                "progressive codec cancellation retained its owner"
            );
            assert!(pcm.is_empty());
            assert!(
                matches!(d.step(&[],false,&mut pcm,&AtomicBool::new(false)),Err(e) if e.code==ErrorCode::Cancelled),
                "progressive cancellation did not remain terminal"
            );
        }
    }
    for bytes in [
        b"ID3\x04\0\0\x7f\x7f\x7f\x7f".to_vec(),
        include_bytes!("../decoder/aac-fixtures/1-front.m4a").to_vec(),
    ] {
        assert!(
            decode(&bytes, 48000, 7).is_err(),
            "unsupported or oversized stream published successful PCM"
        );
    }
}

#[test]
fn a_real_wave_larger_than_the_whole_file_limit_uses_bounded_steady_storage() {
    let frames = crate::limits::MAX_AUDIO_PCM_SAMPLES as u32 + 129;
    let bytes = frames * 2;
    let mut header = b"RIFF".to_vec();
    header.extend((bytes + 36).to_le_bytes());
    header.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    header.extend(48000_u32.to_le_bytes());
    header.extend(96000_u32.to_le_bytes());
    header.extend(b"\x02\0\x10\0data");
    header.extend(bytes.to_le_bytes());
    let mut d = AutoDecoder::new(48000);
    let body = [0_u8, 32].repeat(4096);
    let mut pcm = Vec::with_capacity(32768);
    let pointer = pcm.as_ptr();
    let capacity = pcm.capacity();
    let mut offset = 0;
    let mut produced = 0;
    for _ in 0..1_000_000 {
        let data = if offset < header.len() {
            &header[offset..]
        } else {
            let n = ((header.len() + bytes as usize) - offset).min(body.len());
            &body[..n]
        };
        let eof = data.is_empty();
        let before = migo_alloc_probe::thread_counts();
        let step = d
            .step(data, eof, &mut pcm, &AtomicBool::new(false))
            .unwrap();
        let after = migo_alloc_probe::thread_counts();
        if produced > 131072 {
            assert_eq!(
                after.allocation_events() - before.allocation_events(),
                0,
                "steady progressive WAVE allocated or reallocated"
            );
        }
        assert_eq!((pcm.as_ptr(), pcm.capacity()), (pointer, capacity));
        assert!(
            pcm.iter().all(|s| *s == 0.25),
            "long progressive PCM changed"
        );
        produced += pcm.len();
        pcm.clear();
        offset += step.consumed;
        if step.progress == Progress::Finished {
            assert_eq!(
                produced, frames as usize,
                "streaming incorrectly inherited the whole-file PCM limit"
            );
            return;
        }
    }
    panic!("long progressive WAVE exceeded its step bound");
}

#[tokio::test]
async fn original_cpu_cancel_retires_actual_packet_allocations_after_the_running_step() {
    for (_, data) in [cases().remove(1), cases().remove(3), cases().remove(7)] {
        let host = migo_io::scheduler::IoScheduler::new(0);
        let peer = migo_io::scheduler::IoScheduler::new(0);
        let owner = crate::off_worker::OffWorker::new(AutoDecoder::new(48000), host.audio_cpu());
        let (owner, watches) = owner
            .with(move |decoder| {
                let mut offset = 0;
                let mut pcm = Vec::with_capacity(32768);
                for _ in 0..4096 {
                    let step = decoder
                        .step(&data[offset..], false, &mut pcm, &AtomicBool::new(false))
                        .unwrap();
                    offset += step.consumed;
                    pcm.clear();
                    if step.sample_rate != 0 {
                        return match decoder.codec.as_ref().unwrap() {
                            Codec::Wave(d) => d.watches(),
                            Codec::Vorbis(d) => d.watches(),
                            Codec::Adts(d) => d.watches(),
                            Codec::Mp3(_) | Codec::M4a(_) => unreachable!(),
                        };
                    }
                }
                panic!("product packet owner did not initialize")
            })
            .await
            .unwrap();
        let (started, waiting) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let task = tokio::spawn(owner.with(move |decoder| {
            started.send(()).unwrap();
            held.recv_timeout(WAIT).unwrap();
            let mut pcm = Vec::with_capacity(32768);
            let result = decoder.step(&[], false, &mut pcm, &flag);
            assert!(
                matches!(&result, Ok(step) if step.progress == Progress::Cancelled)
                    || matches!(&result, Err(error) if error.code == ErrorCode::Cancelled),
                "CPU packet cancellation was ignored"
            );
            std::thread::current().name().map(str::to_owned)
        }));
        tokio::task::spawn_blocking(move || waiting.recv_timeout(WAIT).unwrap())
            .await
            .unwrap();
        cancelled.store(true, Ordering::Release);
        assert!(
            watches.iter().all(|(w, _)| w.freed_bytes().is_none()),
            "packet buffers freed before the original running CPU returned"
        );
        assert_eq!(peer.audio_cpu().run(|| 17).await.unwrap(), 17);
        release.send(()).unwrap();
        let (owner, name) = task.await.unwrap().unwrap();
        assert_ne!(name.as_deref(), Some("audio-stream"));
        assert!(
            watches.iter().all(|(w, n)| w.freed_bytes() == Some(*n)),
            "original CPU did not actually free packet capacity bytes"
        );
        drop(owner);
    }
}

fn client(server: &Server) -> crate::streaming::HttpClient {
    migo_services::network::http::HttpClientBuilder::new(
        "migo-product-stream-test",
        false,
        &shared::network_policy::NetworkPolicy {
            allow_plain_http: true,
            ..shared::network_policy::NetworkPolicy::unrestricted()
        },
        "audio-stream",
        migo_io::pools::IoPools::new(0),
    )
    .proxies(
        hyper_util::client::proxy::matcher::Matcher::builder()
            .http(server.proxy.as_str())
            .build(),
    )
    .build()
    .unwrap()
}
const WAIT: Duration = Duration::from_secs(15);
struct Server {
    url: String,
    proxy: String,
    task: tokio::task::JoinHandle<()>,
    release: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.release.take();
        self.task.abort();
    }
}
async fn server(
    data: Vec<u8>,
    chunk: usize,
    chunked: bool,
    hold: Option<usize>,
    truncated: bool,
) -> Server {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let url = "http://product-stream-fixture.invalid/deliberately-wrong.mp3".to_owned();
    let (release, released) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            match socket.read_u8().await {
                Ok(x) => header.push(x),
                Err(_) => return,
            };
            assert!(header.len() < 8192);
        }
        let headers = if chunked {
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_owned()
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len() + usize::from(truncated) * 17
            )
        };
        if socket.write_all(headers.as_bytes()).await.is_err() {
            return;
        }
        let mut sent = 0;
        let mut released = Some(released);
        for bytes in data.chunks(chunk) {
            if hold.is_some_and(|n| sent >= n)
                && let Some(wait) = released.take()
            {
                let _ = wait.await;
            }
            if chunked
                && socket
                    .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                    .await
                    .is_err()
            {
                return;
            }
            if socket.write_all(bytes).await.is_err() {
                return;
            }
            if chunked && socket.write_all(b"\r\n").await.is_err() {
                return;
            }
            sent += bytes.len();
        }
        if chunked && !truncated {
            let _ = socket.write_all(b"0\r\n\r\n").await;
        }
    });
    Server {
        url,
        proxy,
        task,
        release: Some(release),
    }
}
async fn message(rx: &mut tokio::sync::mpsc::Receiver<StreamMsg>) -> Option<StreamMsg> {
    tokio::time::timeout(WAIT, rx.recv())
        .await
        .expect("product stream publication stalled")
}

#[tokio::test]
async fn real_http_payload_selects_the_product_codec_and_preserves_all_pcm() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    for (name, data) in cases() {
        for target in [16000, 48000] {
            let expected = crate::decoder::decode_for_rate(&data, target).unwrap();
            for chunked in [false, true] {
                let source = server(data.clone(), 137, chunked, None, false).await;
                let state = StreamingState::new();
                let mut rx = crate::streaming::start_streaming_download(
                    host.audio_cpu(),
                    client(&source),
                    source.url.clone(),
                    state.clone(),
                    target,
                );
                let mut actual = Vec::new();
                let (mut ready, mut done) = (0, 0);
                while let Some(event) = message(&mut rx).await {
                    match event {
                        StreamMsg::Ready {
                            sample_rate,
                            channels,
                            ..
                        } => {
                            ready += 1;
                            assert_eq!(
                                (sample_rate, channels),
                                (target, expected.channels),
                                "HTTP stream lost submitted format"
                            );
                        }
                        StreamMsg::Samples { pcm, .. } => {
                            assert_eq!((ready, done), (1, 0));
                            actual.extend_from_slice(&pcm);
                        }
                        StreamMsg::Done { .. } => done += 1,
                        StreamMsg::Error { message, .. } => {
                            panic!("real product HTTP codec failed: {name}: {message}")
                        }
                    }
                }
                assert_eq!(
                    (ready, done),
                    (1, 1),
                    "HTTP product stream lost its ordered Ready/Done"
                );
                assert_eq!(
                    actual, expected.samples,
                    "HTTP product complete PCM changed: {name}/{chunked}"
                );
                assert!(state.download_complete.load(Ordering::Acquire));
            }
        }
    }
}

#[tokio::test]
async fn real_http_publishes_pcm_before_body_eof_and_truncation_never_sends_done() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let mut metadata_wave = wav(8000, 1, 17);
    let mut metadata = b"INFOISFT".to_vec();
    metadata.extend(8192_u32.to_le_bytes());
    metadata.resize(metadata.len() + 8192, 0);
    metadata_wave.extend(b"LIST");
    metadata_wave.extend((metadata.len() as u32).to_le_bytes());
    metadata_wave.extend(metadata);
    let size = (metadata_wave.len() - 8) as u32;
    metadata_wave[4..8].copy_from_slice(&size.to_le_bytes());
    for (name, data) in [
        ("wave-with-metadata-tail", metadata_wave),
        cases().remove(1),
        (
            "vorbis-cross",
            include_bytes!("../decoder/ogg-fixtures/cross.ogg").to_vec(),
        ),
        cases().remove(7),
    ] {
        let mut source = server(data.clone(), 137, true, Some(data.len() / 2), false).await;
        let state = StreamingState::new();
        let mut rx = crate::streaming::start_streaming_download(
            host.audio_cpu(),
            client(&source),
            source.url.clone(),
            state.clone(),
            48000,
        );
        loop {
            match message(&mut rx).await {
                Some(StreamMsg::Samples { pcm, .. }) => {
                    assert!(!pcm.is_empty());
                    assert!(
                        !source.task.is_finished(),
                        "progressive decoder waited for complete body"
                    );
                    assert!(!state.download_complete.load(Ordering::Acquire));
                    break;
                }
                Some(StreamMsg::Error { message, .. }) => {
                    panic!("product first PCM failed: {name}: {message}")
                }
                Some(StreamMsg::Done { .. }) | None => {
                    panic!("product first PCM was withheld until EOF")
                }
                _ => {}
            }
        }
        source.release.take().unwrap().send(()).unwrap();
        while message(&mut rx).await.is_some() {}
        let source = server(data, 137, false, None, true).await;
        let state = StreamingState::new();
        let mut rx = crate::streaming::start_streaming_download(
            host.audio_cpu(),
            client(&source),
            source.url.clone(),
            state.clone(),
            48000,
        );
        let (mut errors, mut done) = (0, 0);
        while let Some(event) = message(&mut rx).await {
            match event {
                StreamMsg::Error { .. } => errors += 1,
                StreamMsg::Done { .. } => done += 1,
                _ => {}
            }
        }
        assert_eq!(
            (errors, done),
            (1, 0),
            "truncated product HTTP body published successful Done"
        );
        assert!(!state.download_complete.load(Ordering::Acquire));
    }
}

#[tokio::test]
async fn real_http_backpressure_and_cancel_stop_source_reads_with_full_pcm_loans() {
    let data = wav(48000, 2, 240137);
    let source = server(data.clone(), 8192, true, None, false).await;
    let host = migo_io::scheduler::IoScheduler::new(0);
    let state = StreamingState::new();
    let mut rx = crate::streaming::start_streaming_download(
        host.audio_cpu(),
        client(&source),
        source.url.clone(),
        state.clone(),
        48000,
    );
    let end = Instant::now() + WAIT;
    while !state.publication_waiting.load(Ordering::Acquire)
        || rx.len() < crate::streaming::STREAM_CHANNEL_CAPACITY
    {
        assert!(Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let before = state.bytes_downloaded.load(Ordering::Acquire);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        state.bytes_downloaded.load(Ordering::Acquire),
        before,
        "full PCM channel did not backpressure the source"
    );
    assert!(before < data.len() as u64);
    state.cancel();
    let mut done = 0;
    while let Some(event) = message(&mut rx).await {
        if matches!(event, StreamMsg::Done { .. }) {
            done += 1;
        }
    }
    assert_eq!(done, 0, "cancelled product stream published Done");
    assert!(!state.download_complete.load(Ordering::Acquire));
}

#[tokio::test]
async fn actual_player_receives_the_selected_codec_through_source_identity_and_pause() {
    for (name, data) in [cases().remove(1), cases().remove(3), cases().remove(7)] {
        let expected = crate::decoder::decode_for_rate(&data, 48000).unwrap();
        let source = server(data, 137, true, None, false).await;
        let host = migo_io::scheduler::IoScheduler::new(0);
        let state = StreamingState::new();
        let rx = crate::streaming::start_streaming_download(
            host.audio_cpu(),
            client(&source),
            source.url.clone(),
            state.clone(),
            48000,
        );
        let mut player = crate::inner_audio::InnerAudioPlayer::new(31, expected.channels);
        let shared = player.shared.clone();
        shared.set_autoplay(true);
        let load = player.begin_load_for(101).unwrap();
        player
            .complete_stream_load(load, source.url.clone(), rx, state)
            .unwrap();
        let mut actual = Vec::new();
        let mut block = vec![0.; 128 * expected.channels as usize];
        let end = Instant::now() + WAIT;
        let mut paused = false;
        while actual.len() < expected.samples.len() {
            assert!(
                Instant::now() < end,
                "product player made no progress: {name}"
            );
            player.poll_stream();
            if !paused && !actual.is_empty() {
                player.pause();
                let position = shared.position_frames();
                let mut sentinel = [0.125; 16];
                player.process(&mut sentinel);
                assert_eq!(sentinel, [0.125; 16], "paused product player wrote PCM");
                assert_eq!(shared.position_frames(), position);
                player.play();
                paused = true;
            }
            block.fill(0.);
            let before = shared.position_frames();
            let active = player.process(&mut block);
            let count = if active {
                shared.position_frames().saturating_sub(before) as usize
            } else if shared.state() == crate::inner_audio::PlaybackState::Stopped
                && shared.is_loaded()
            {
                (expected.samples.len() - actual.len()) / expected.channels as usize
            } else {
                0
            };
            assert!(count <= 128);
            actual.extend_from_slice(&block[..count * expected.channels as usize]);
            if count == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        assert_eq!(
            actual, expected.samples,
            "actual player complete PCM changed: {name}"
        );
        player.stop();
        assert_eq!(shared.position_frames(), 0);
    }
}

#[test]
#[ignore = "explicit O3 A/B microbenchmark; correctness is in the ordinary suite"]
fn packet_buffer_transfer_benchmark() {
    for (name, data) in [cases().remove(1), cases().remove(3), cases().remove(7)] {
        let native = crate::decoder::decode(&data).unwrap();
        let expected = decode(&data, native.sample_rate, 8192).unwrap();
        let begin = Instant::now();
        let mut count = 0;
        for _ in 0..300 {
            let pcm = decode(std::hint::black_box(&data), native.sample_rate, 8192).unwrap();
            assert_eq!(pcm, expected);
            count += std::hint::black_box(pcm.len());
        }
        println!(
            "PACKET_TRANSFER_BENCH {name} {} {count}",
            begin.elapsed().as_nanos()
        );
    }
}

mod http_wave_retirement {
    use super::*;
    use crate::off_worker::OffWorker;
    use crate::streaming_sinc::STEP_SAMPLES;
    use crate::streaming_transport::{Decoder, Progress};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[tokio::test]
    async fn http_wave_eof_retains_actual_capacities_until_original_cpu_really_destroys_them() {
        let host = migo_io::scheduler::IoScheduler::new(1061);
        let peer = migo_io::scheduler::IoScheduler::new(1062);
        let slot = OffWorker::new(None::<AutoDecoder>, host.audio_cpu());
        let (slot, watches) = slot
            .with(|slot| {
                let mut data = crate::streaming_codecs::tests::wav(44100, 2, 9737);
                data.extend(b"LIST");
                data.extend(37u32.to_le_bytes());
                data.resize(data.len() + 38, 0);
                let length = (data.len() - 8) as u32;
                data[4..8].copy_from_slice(&length.to_le_bytes());
                let mut d = AutoDecoder::http_retained(16000, data.len() as u64);
                let mut pos = 0;
                let mut source_eof = false;
                let mut out = Vec::with_capacity(STEP_SAMPLES);
                for _ in 0..10000 {
                    out.clear();
                    let end = (pos + 1024).min(data.len());
                    let step = d
                        .step(
                            &data[pos..end],
                            source_eof,
                            &mut out,
                            &AtomicBool::new(false),
                        )
                        .unwrap();
                    pos += step.consumed;
                    if step.progress == Progress::NeedInput && pos == data.len() {
                        source_eof = true;
                    }
                    assert_ne!(step.progress, Progress::NeedSource);
                    if step.progress == Progress::Finished {
                        break;
                    }
                }
                let Some(Codec::Wave(wave)) = d.codec.as_mut() else {
                    panic!("HTTP WAVE EOF lost retained decoder")
                };
                let o = wave
                    .owner
                    .as_ref()
                    .expect("EOF dropped the retained source decoder");
                assert!(
                    o.decoder.input_len() > 0
                        && o.decoder.input_len()
                            < crate::decoder::aac::incremental::ENCODED_CAPACITY,
                    "WAVE EOF lost actual retained capacity"
                );
                assert_eq!(
                    o.decoder.input_allocation().1,
                    crate::decoder::aac::incremental::ENCODED_CAPACITY
                );
                let (request, _, terminal) = wave
                    .reposition_wave(0, None)
                    .expect("WAVE EOF restart did not preserve retained input coordinates");
                assert!(
                    request.is_none() && !terminal,
                    "retained WAVE EOF restart unexpectedly reread source"
                );
                let watches = wave.watches();
                *slot = Some(d);
                watches
            })
            .await
            .unwrap();
        let bytes = watches.iter().map(|(_, n)| *n as u64).sum::<u64>();
        let (start, started) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let task = tokio::spawn(slot.with(move |slot| {
            start.send(()).unwrap();
            blocked
                .recv_timeout(std::time::Duration::from_secs(15))
                .unwrap();
            let mut out = Vec::with_capacity(STEP_SAMPLES);
            let before = migo_alloc_probe::thread_counts();
            assert_eq!(
                slot.as_mut()
                    .unwrap()
                    .step(&[], false, &mut out, &flag)
                    .unwrap()
                    .progress,
                Progress::Cancelled
            );
            assert!(
                migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= bytes,
                "original CPU did not really free HTTP WAVE retained buffers"
            );
            assert!(
                std::thread::current()
                    .name()
                    .unwrap_or("")
                    .starts_with("Migo-IO-")
            );
        }));
        tokio::task::spawn_blocking(move || {
            started
                .recv_timeout(std::time::Duration::from_secs(15))
                .unwrap()
        })
        .await
        .unwrap();
        cancel.store(true, Ordering::Release);
        let held = watches.iter().all(|(w, _)| w.freed_bytes().is_none());
        assert_eq!(peer.audio_cpu().run(|| 17).await.unwrap(), 17);
        release.send(()).unwrap();
        let (slot, ()) = task.await.unwrap().unwrap();
        assert!(held, "retained replay freed before original CPU returned");
        assert!(
            watches.iter().all(|(w, n)| w.freed_bytes() == Some(*n)),
            "retained buffer capacity was not actually destroyed"
        );
        drop(slot);
    }
}
