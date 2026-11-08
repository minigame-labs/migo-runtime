use super::*;
#[path = "streaming_http_control_tests.rs"]
pub(crate) mod controls;
#[path = "streaming_http_mp3_tests.rs"]
mod mp3_controls;
#[path = "streaming_http_prepare_tests.rs"]
mod preparation;
#[path = "streaming_range_control_tests.rs"]
mod range_controls;
#[path = "streaming_http_vorbis_tests.rs"]
mod vorbis_controls;
#[path = "streaming_http_wave_tests.rs"]
mod wave_controls;
use crate::streaming_transport::Decoder;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const WAIT: Duration = Duration::from_secs(20);
const TAIL: &[u8] = include_bytes!("decoder/aac-fixtures/1-tail.m4a");
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    Normal,
    Changed,
    Offset,
    Ignored,
    Truncated,
    NoTag,
    Weak,
    Chunked,
    Gzip,
    UnknownLength,
    OriginalShort,
}
pub(crate) struct Server {
    proxy: String,
    task: tokio::task::JoinHandle<()>,
    ranges: Arc<AtomicUsize>,
    initial_eof: Arc<AtomicUsize>,
    behavior: Arc<AtomicUsize>,
    hold_body: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    pub(crate) fn range_requests(&self) -> usize {
        self.ranges.load(Ordering::Acquire)
    }
    fn range_fault(&self, mode: Mode) {
        self.behavior.store(
            match mode {
                Mode::Changed => 1,
                Mode::Offset => 2,
                Mode::Truncated => 3,
                _ => 0,
            },
            Ordering::Release,
        );
    }
    //0=no pause;1=before body;2=after first body chunk. Production never sees this fixture state.
    fn pause_range_body(&self, at: usize) {
        self.hold_body.store(at, Ordering::Release);
    }
    pub(crate) fn client(&self, pools: migo_io::pools::IoPools) -> HttpClient {
        migo_services::network::http::HttpClientBuilder::new(
            "migo-indexed-http-test",
            false,
            &shared::network_policy::NetworkPolicy {
                allow_plain_http: true,
                ..shared::network_policy::NetworkPolicy::unrestricted()
            },
            "audio-stream",
            pools,
        )
        .proxies(
            hyper_util::client::proxy::matcher::Matcher::builder()
                .http(&self.proxy)
                .build(),
        )
        .build()
        .unwrap()
    }
}
// Test fixture writer: gzip with one uncompressed DEFLATE block.
fn gzip(data: &[u8]) -> Vec<u8> {
    assert!(data.len() < 65536);
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255, 1];
    let n = data.len() as u16;
    out.extend(n.to_le_bytes());
    out.extend((!n).to_le_bytes());
    out.extend(data);
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ ((0u32.wrapping_sub(crc & 1)) & 0xedb8_8320);
        }
    }
    out.extend((!crc).to_le_bytes());
    out.extend((data.len() as u32).to_le_bytes());
    out
}
pub(crate) async fn server(data: Vec<u8>, mode: Mode, hold: bool, short: bool) -> Server {
    server_with_initial_prefix(data, mode, hold.then_some(4096), short).await
}
pub(super) async fn server_with_initial_prefix(
    data: Vec<u8>,
    mode: Mode,
    initial_prefix: Option<usize>,
    short: bool,
) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let ranges = Arc::new(AtomicUsize::new(0));
    let count = ranges.clone();
    let initial_eof = Arc::new(AtomicUsize::new(0));
    let eof = initial_eof.clone();
    let data = Arc::new(if mode == Mode::Gzip {
        gzip(&data)
    } else {
        data
    });
    let behavior = Arc::new(AtomicUsize::new(0));
    let hold_body = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (behavior_run, hold_run, entered_run, release_run) = (
        behavior.clone(),
        hold_body.clone(),
        entered.clone(),
        release.clone(),
    );
    let task =
        tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (data, count, eof) = (data.clone(), count.clone(), eof.clone());
                let (behavior, hold_body, entered, release) = (
                    behavior_run.clone(),
                    hold_run.clone(),
                    entered_run.clone(),
                    release_run.clone(),
                );
                children.spawn(async move {
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    match socket.read_u8().await {
                        Ok(b) => header.push(b),
                        Err(_) => return,
                    }
                    assert!(header.len() < 8192);
                }
                let text = String::from_utf8(header).unwrap().to_ascii_lowercase();
                if text.lines().next().unwrap().contains("/sequential ") {
                    assert!(text.contains("accept-encoding: gzip, br\r\n"));
                } else {
                    assert!(text.contains("accept-encoding: identity\r\n"));
                }
                let mode = match behavior.load(Ordering::Acquire) {
                    1 => Mode::Changed,
                    2 => Mode::Offset,
                    3 => Mode::Truncated,
                    _ => mode,
                };
                let range = text.lines().find_map(|s| s.strip_prefix("range: bytes="));
                let (start, end, status, extent) = if let Some(range) = range {
                    count.fetch_add(1, Ordering::AcqRel);
                    assert!(text.contains("if-range: \"fixed\"\r\n"));
                    let (a, z) = range.split_once('-').unwrap();
                    let start = a.parse::<usize>().unwrap();
                    let mut end = z.parse::<usize>().unwrap();
                    if short {
                        end = end.min(start + 4095);
                    }
                    if mode == Mode::Ignored {
                        (0, data.len() - 1, "200 OK".to_owned(), String::new())
                    } else {
                        let offset = start + usize::from(mode == Mode::Offset);
                        (
                            start,
                            end,
                            "206 Partial Content".to_owned(),
                            format!(
                                "Content-Range: bytes {offset}-{end}/{}\r\n",
                                if mode == Mode::UnknownLength {
                                    "*".to_owned()
                                } else {
                                    data.len().to_string()
                                }
                            ),
                        )
                    }
                } else {
                    (0, data.len() - 1, "200 OK".to_owned(), String::new())
                };
                let tag = if matches!(
                    mode,
                    Mode::NoTag | Mode::Chunked | Mode::Gzip | Mode::OriginalShort
                ) {
                    ""
                } else if mode == Mode::Weak {
                    "ETag: W/\"fixed\"\r\n"
                } else if range.is_some() && mode == Mode::Changed {
                    "ETag: \"changed\"\r\n"
                } else {
                    "ETag: \"fixed\"\r\n"
                };
                let chunked =
                    range.is_none() && matches!(mode, Mode::Chunked | Mode::UnknownLength);
                let framing = if chunked {
                    "Transfer-Encoding: chunked\r\n".to_owned()
                } else {
                    format!(
                        "Content-Length: {}\r\n",
                        end - start
                            + 1
                            + usize::from(range.is_none() && mode == Mode::OriginalShort) * 17
                    )
                };
                let encoding = if mode == Mode::Gzip {
                    "Content-Encoding: gzip\r\n"
                } else {
                    ""
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\n{tag}{extent}{framing}{encoding}Connection: close\r\n\r\n"
                );
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let end = if range.is_none() {
                    initial_prefix.map_or(end+1, |n| (start+n).min(end+1))
                } else { end+1 };
                let end = if range.is_some() && mode == Mode::Truncated {
                    end - 1
                } else {
                    end
                };
                if range.is_some() && hold_body.load(Ordering::Acquire) == 1 {
                    entered.notify_one();
                    release.notified().await;
                }
                for (i, chunk) in data[start..end].chunks(137).enumerate() {
                    if range.is_some() && i == 1 && hold_body.load(Ordering::Acquire) == 2 {
                        entered.notify_one();
                        release.notified().await;
                    }

                    if chunked
                        && socket
                            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                            .await
                            .is_err()
                    {
                        return;
                    }
                    if socket.write_all(chunk).await.is_err() {
                        return;
                    }
                    if chunked && socket.write_all(b"\r\n").await.is_err() {
                        return;
                    }
                }
                if chunked && socket.write_all(b"0\r\n\r\n").await.is_err() {
                    return;
                }
                if range.is_none() && initial_prefix.is_some() {
                    std::future::pending::<()>().await;
                }
                if range.is_none() {
                    eof.fetch_add(1, Ordering::Release);
                }
            });
            }
        });
    Server {
        proxy,
        task,
        ranges,
        initial_eof,
        behavior,
        hold_body,
        entered,
        release,
    }
}
fn start(
    server: &Server,
    host: &migo_io::scheduler::IoScheduler,
    rate: u32,
) -> (tokio::sync::mpsc::Receiver<StreamMsg>, Arc<StreamingState>) {
    let state = StreamingState::new();
    let rx = crate::streaming::start_streaming_download(
        host.audio_cpu(),
        server.client(host.pools().clone()),
        "http://indexed-product-fixture.invalid/wrong.mp3".into(),
        state.clone(),
        rate,
    );
    (rx, state)
}
async fn collect(rx: &mut mpsc::Receiver<StreamMsg>) -> Result<Vec<f32>, String> {
    let mut pcm = Vec::new();
    let (mut ready, mut done) = (0, 0);
    while let Some(event) = tokio::time::timeout(WAIT, rx.recv())
        .await
        .expect("indexed HTTP publication stalled")
    {
        match event {
            StreamMsg::Ready { .. } => ready += 1,
            StreamMsg::Samples { pcm: data, .. } => {
                assert_eq!((ready, done), (1, 0));
                pcm.extend_from_slice(&data);
            }
            StreamMsg::Done { .. } => done += 1,
            StreamMsg::Error { message, .. } => {
                assert_eq!(done, 0, "failed HTTP source published Done");
                return Err(message);
            }
        }
    }
    assert_eq!(
        (ready, done),
        (1, 1),
        "indexed HTTP lost ordered Ready/Done"
    );
    Ok(pcm)
}
fn cases() -> Vec<(&'static str, &'static [u8])> {
    macro_rules! case {
        ($n:literal) => {
            (
                $n,
                include_bytes!(concat!("decoder/aac-fixtures/", $n, ".m4a")).as_slice(),
            )
        };
    }
    vec![
        case!("0-front"),
        case!("0-tail"),
        case!("1-front"),
        case!("1-tail"),
        case!("1-reorder"),
        case!("1-split"),
        case!("1-co64"),
        case!("2-front"),
        case!("2-tail"),
        case!("2-reverse"),
        case!("2-reorder"),
        case!("2-co64"),
        case!("2-tail-reverse"),
        case!("2-multi"),
        case!("2-tail-multi"),
        case!("2-split"),
        case!("edit-raw"),
        case!("edit-211"),
        case!("edit-1537"),
        case!("edit-version-one"),
    ]
}
// Test fixture transformation only; product parsing always uses the library.
pub(crate) fn padded(data: &[u8], gap: u32) -> Vec<u8> {
    fn offsets(data: &mut [u8], gap: u32) {
        let mut pos = 0;
        while pos < data.len() {
            let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            match &data[pos + 4..pos + 8] {
                b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                    offsets(&mut data[pos + 8..pos + len], gap)
                }
                b"stco" | b"co64" => {
                    let wide = &data[pos + 4..pos + 8] == b"co64";
                    let size = if wide { 8 } else { 4 };
                    let n =
                        u32::from_be_bytes(data[pos + 12..pos + 16].try_into().unwrap()) as usize;
                    for i in 0..n {
                        let a = pos + 16 + i * size;
                        if wide {
                            let old = u64::from_be_bytes(data[a..a + 8].try_into().unwrap());
                            data[a..a + 8].copy_from_slice(&(old + u64::from(gap)).to_be_bytes());
                        } else {
                            let old = u32::from_be_bytes(data[a..a + 4].try_into().unwrap());
                            data[a..a + 4].copy_from_slice(&(old + gap).to_be_bytes());
                        }
                    }
                }
                _ => (),
            }
            pos += len;
        }
    }
    let mut result = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let span = AtomSpan::read(&data[pos..], pos as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        let end = span.end.unwrap() as usize;
        if span.kind == AtomKind::MediaData {
            result.extend(((end - pos) as u32 + gap + 65536).to_be_bytes());
            result.extend(b"mdat");
            result.resize(result.len() + gap as usize, 0);
            result.extend(&data[pos + 8..end]);
            result.resize(result.len() + 65536, 0);
        } else {
            let mut part = data[pos..end].to_vec();
            if span.kind == AtomKind::Movie {
                offsets(&mut part, gap);
            }
            result.extend(part);
        }
        pos = end;
    }
    result
}
#[tokio::test]
async fn product_http_m4a_preserves_every_pcm_bit_front_tail_reorder_co64_and_edits() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    for (name, data) in cases() {
        for rate in [16000, 48000] {
            let server = server(data.to_vec(), Mode::Normal, false, false).await;
            let (mut rx, state) = start(&server, &host, rate);
            let actual = collect(&mut rx).await.unwrap_or_else(|e| {
                panic!("product HTTP M4A failed complete PCM: {name}/{rate}: {e}")
            });
            let expected = crate::decoder::decode_for_rate(data, rate).unwrap();
            assert_eq!(
                actual.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
                expected
                    .samples
                    .iter()
                    .map(|s| s.to_bits())
                    .collect::<Vec<_>>(),
                "HTTP M4A complete PCM bits changed: {name}/{rate}"
            );
            assert!(state.download_complete.load(Ordering::Acquire));
            if std::env::var_os("MIGO_DUMP_HTTP_M4A_PCM").is_some() {
                println!(
                    "MIGO_HTTP_M4A_PCM {name}/{rate} {:?}",
                    actual.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
                );
            }
        }
    }
}
#[tokio::test]
async fn range_tail_plays_beyond_whole_file_limit_without_waiting_for_original_body_eof() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    for name in ["1-tail", "1-reorder", "1-co64", "2-tail-reverse"] {
        let data = cases().into_iter().find(|(n, _)| *n == name).unwrap().1;
        let large = padded(data, 20 * 1024 * 1024);
        assert!(large.len() > 16 * 1024 * 1024);
        let server = server(large, Mode::Normal, name.contains("tail"), false).await;
        let (mut rx, _) = start(&server, &host, 48000);
        let actual = collect(&mut rx)
            .await
            .unwrap_or_else(|e| panic!("large indexed HTTP M4A failed: {name}: {e}"));
        assert_eq!(
            actual,
            crate::decoder::decode_for_rate(data, 48000)
                .unwrap()
                .samples,
            "large HTTP M4A full PCM or tail changed"
        );
        if name.contains("tail") {
            assert!(
                server.ranges.load(Ordering::Acquire) > 0,
                "large tail M4A never reached real Range IO"
            );
            assert_eq!(
                server.initial_eof.load(Ordering::Acquire),
                0,
                "HTTP M4A waited for full original download"
            );
        }
    }
}
#[tokio::test]
async fn changed_ignored_shifted_short_or_unpinned_ranges_cannot_publish_success() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    for mode in [
        Mode::Changed,
        Mode::Offset,
        Mode::Ignored,
        Mode::Truncated,
        Mode::NoTag,
    ] {
        let server = server(padded(TAIL, 256 * 1024), mode, true, false).await;
        let (mut rx, state) = start(&server, &host, 48000);
        assert!(
            collect(&mut rx).await.is_err(),
            "invalid HTTP representation published successful PCM"
        );
        assert!(!state.download_complete.load(Ordering::Acquire));
        assert!(
            server.ranges.load(Ordering::Acquire) <= 1,
            "invalid Range retried or started an unpinned fallback"
        );
    }
}
#[tokio::test]
async fn valid_short_media_responses_continue_to_the_real_object_eof() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let server = server(padded(TAIL, 256 * 1024), Mode::Normal, true, true).await;
    let (mut rx, _) = start(&server, &host, 16000);
    let actual = collect(&mut rx)
        .await
        .expect("valid short 206 lost media continuation");
    assert_eq!(
        actual,
        crate::decoder::decode_for_rate(TAIL, 16000)
            .unwrap()
            .samples,
        "short 206 changed complete PCM"
    );
    assert!(server.ranges.load(Ordering::Acquire) > 8);
}
#[tokio::test]
async fn admitted_index_input_is_held_until_the_original_cpu_really_frees_it() {
    const N: usize = 2 * 1024 * 1024;
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(1071, 4, N as u64);
    let peer = host.peer_for_admission_probe(1072);
    let client = server(TAIL.to_vec(), Mode::Normal, false, false)
        .await
        .client(host.pools().clone());
    let input = Input::new(&client, N).unwrap();
    let watch = migo_alloc_probe::DeallocationWatch::new(input.data.as_ptr()).unwrap();
    assert!(
        Input::new(&client, 1).is_err(),
        "HTTP index bypassed original byte admission"
    );
    let owner = OffWorker::new((), host.audio_cpu());
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let job = tokio::spawn(owner.with(move |_| {
        entered.send(()).unwrap();
        held.recv_timeout(WAIT).unwrap();
        assert!(
            std::thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
        );
        let before = migo_alloc_probe::thread_counts();
        drop(input);
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= N as u64,
            "HTTP index did not really free on original CPU"
        );
    }));
    tokio::task::spawn_blocking(move || waiting.recv_timeout(WAIT).unwrap())
        .await
        .unwrap();
    let retained = watch.freed_bytes().is_none() && Input::new(&client, 1).is_err();
    assert_eq!(peer.audio_cpu().run(|| 19).await.unwrap(), 19);
    release.send(()).unwrap();
    let (owner, ()) = job.await.unwrap().unwrap();
    assert!(
        retained,
        "HTTP index storage or credit returned before original CPU drop"
    );
    assert_eq!(
        watch.freed_bytes(),
        Some(N),
        "original HTTP index pointer was not freed"
    );
    assert!(Input::new(&client, N).is_ok());
    drop(owner);
}
#[tokio::test]
async fn sequential_decoder_keeps_m4a_refusal_and_cancellation_terminal() {
    let mut decoder = AutoDecoder::new(48000);
    let mut pcm = Vec::with_capacity(32768);
    assert!(
        decoder
            .step(
                TAIL,
                false,
                &mut pcm,
                &std::sync::atomic::AtomicBool::new(false)
            )
            .is_err(),
        "sequential decoder silently accepted indexed M4A"
    );
    let mut decoder = AutoDecoder::http(48000);
    decoder
        .step(
            TAIL,
            false,
            &mut pcm,
            &std::sync::atomic::AtomicBool::new(true),
        )
        .unwrap();
    assert!(
        decoder
            .step(
                TAIL,
                false,
                &mut pcm,
                &std::sync::atomic::AtomicBool::new(false)
            )
            .is_err()
    );
    let source = server(TAIL.to_vec(), Mode::Normal, false, false).await;
    let host = migo_io::scheduler::IoScheduler::new(0);
    let state = StreamingState::new();
    let (tx, mut rx) = crate::streaming::stream_channel();
    let operation = tokio::spawn(crate::streaming_transport::run(
        host.audio_cpu(),
        source.client(host.pools().clone()),
        "http://indexed-product-fixture.invalid/sequential".into(),
        tx,
        state,
        || AutoDecoder::new(48000),
    ));
    let cause = collect(&mut rx)
        .await
        .expect_err("sequential HTTP source silently accepted indexed M4A");
    assert!(
        cause.contains("M4A requires the indexed streaming source"),
        "sequential HTTP refusal did not reach the decoder: {cause}"
    );
    operation.await.unwrap();
}

#[tokio::test]
async fn oversized_tail_index_is_refused_before_the_body_or_allocation() {
    let mut data = TAIL.to_vec();
    let mut pos = 0;
    loop {
        let span = AtomSpan::read(&data[pos..], pos as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        if span.kind == AtomKind::Movie {
            break;
        }
        pos = span.end.unwrap() as usize;
    }
    let length = 2 * 1024 * 1024 + 1;
    data[pos..pos + 4].copy_from_slice(&(length as u32).to_be_bytes());
    data.resize(pos + length, 0);
    let server = server(data, Mode::Normal, true, false).await;
    let host = migo_io::scheduler::IoScheduler::new(0);
    let (mut rx, _) = start(&server, &host, 48000);
    let cause = collect(&mut rx)
        .await
        .expect_err("oversized HTTP index was accepted");
    assert!(
        cause.contains("HTTP M4A index budget exceeded before body read"),
        "oversized HTTP index did not fail before allocation/body: {cause}"
    );
    assert_eq!(
        server.ranges.load(Ordering::Acquire),
        2,
        "oversized HTTP index reached its unadmitted body read"
    );
}

#[tokio::test]
async fn indexed_packets_are_checked_against_actual_mdat_and_failure_stays_terminal() {
    let server = server(TAIL.to_vec(), Mode::Normal, false, false).await;
    let host = migo_io::scheduler::IoScheduler::new(0);
    let client = server.client(host.pools().clone());
    let response = client
        .get(
            GateKind::AudioStream,
            migo_services::network::url::Url::parse(
                "http://indexed-product-fixture.invalid/extents",
            )
            .unwrap(),
        )
        .header(
            "accept-encoding".parse().unwrap(),
            "identity".parse().unwrap(),
        )
        .send()
        .await
        .unwrap();
    let mut source = IndexedHttp::new(client, response).unwrap();
    let packet = IndexedPacket {
        offset: 8,
        size: 1,
        track_id: 0,
        pts: symphonia_core::units::Timestamp::ZERO,
        dur: symphonia_core::units::Duration::ZERO,
        trim_start: symphonia_core::units::Duration::ZERO,
        trim_end: symphonia_core::units::Duration::ZERO,
    };
    assert!(
        source.media(packet, Instant::now() + WAIT).await.is_err(),
        "HTTP source accepted packet bytes outside actual mdat"
    );
    let state = StreamingState::new();
    let (tx, _rx) = crate::streaming::stream_channel();
    source.failed = true;
    assert!(
        source.read(&tx, &state).await.is_err(),
        "failed indexed HTTP source resumed on EOF"
    );
}

#[tokio::test]
async fn actual_player_receives_range_m4a_with_complete_pcm_source_identity_and_pause() {
    for (name, data) in cases()
        .into_iter()
        .filter(|(n, _)| ["1-tail", "1-reorder", "2-tail-reverse", "edit-1537"].contains(n))
    {
        let expected = crate::decoder::decode_for_rate(data, 48000).unwrap();
        let source = server(
            padded(data, 256 * 1024),
            Mode::Normal,
            name.contains("tail"),
            false,
        )
        .await;
        let host = migo_io::scheduler::IoScheduler::new(0);
        let state = StreamingState::new();
        let rx = crate::streaming::start_streaming_download(
            host.audio_cpu(),
            source.client(host.pools().clone()),
            "http://indexed-product-fixture.invalid/player".into(),
            state.clone(),
            48000,
        );
        let mut player = crate::inner_audio::InnerAudioPlayer::new(31, expected.channels);
        let shared = player.shared.clone();
        shared.set_autoplay(true);
        let load = player.begin_load_for(101).unwrap();
        player
            .complete_stream_load(
                load,
                "http://indexed-product-fixture.invalid/player".into(),
                rx,
                state,
            )
            .unwrap();
        let mut actual = Vec::new();
        let mut block = vec![0.; 128 * expected.channels as usize];
        let end = std::time::Instant::now() + WAIT;
        let mut paused = false;
        while actual.len() < expected.samples.len() {
            assert!(
                std::time::Instant::now() < end,
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
            "actual HTTP M4A player complete PCM changed: {name}"
        );
        player.stop();
        assert_eq!(shared.position_frames(), 0);
    }
}

// Transport partitions are independent of packetizer acceptance. Exercise a
// large real HTTP prefix even when hyper happens to yield small socket frames.
// This adapter only joins test chunks; all framing, identity, IO and resumption
// still go through the production IndexedHttp and the original CPU driver.
struct Coalesced(IndexedHttp);
impl Source<AutoDecoder> for Coalesced {
    type Request = HttpRequest;
    fn pending(decoder: &AutoDecoder) -> EngineResult<HttpRequest> {
        IndexedHttp::pending(decoder)
    }
    async fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        let mut bytes = Vec::new();
        while bytes.len() < 65536 {
            match self.0.read(tx, state).await? {
                SourceRead::Data(chunk) => bytes.extend_from_slice(chunk.as_ref()),
                SourceRead::End => break,
            }
        }
        Ok(if bytes.is_empty() {
            SourceRead::End
        } else {
            SourceRead::Data(bytes)
        })
    }
    async fn resume(
        &mut self,
        request: HttpRequest,
        decoder: OffWorker<Option<AutoDecoder>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<AutoDecoder>>> {
        self.0.resume(request, decoder, tx, state).await
    }
}
impl HttpSource<AutoDecoder> for Coalesced {
    fn request(request: RequestBuilder) -> RequestBuilder {
        IndexedHttp::request(request)
    }
    fn new(client: HttpClient, response: Response) -> EngineResult<Self> {
        IndexedHttp::new(client, response).map(Self)
    }
}
#[tokio::test]
async fn source_reposition_discards_large_unaccepted_http_suffix_and_preserves_all_pcm() {
    let server = server(padded(TAIL, 256 * 1024), Mode::Normal, false, false).await;
    let host = migo_io::scheduler::IoScheduler::new(0);
    let state = StreamingState::new();
    let (tx, mut rx) = crate::streaming::stream_channel();
    let operation = tokio::spawn(crate::streaming_transport::run_with_source::<
        AutoDecoder,
        Coalesced,
    >(
        host.audio_cpu(),
        server.client(host.pools().clone()),
        "http://indexed-product-fixture.invalid/partitions".into(),
        tx,
        state,
        |_| AutoDecoder::http(48000),
    ));
    let actual = collect(&mut rx)
        .await
        .expect("coalesced HTTP M4A failed complete PCM");
    assert_eq!(
        actual,
        crate::decoder::decode_for_rate(TAIL, 48000)
            .unwrap()
            .samples,
        "coalesced HTTP M4A complete PCM or tail changed"
    );
    operation.await.unwrap();
}

#[path = "streaming_mp3_http_tests.rs"]
mod mp3_gapless;
