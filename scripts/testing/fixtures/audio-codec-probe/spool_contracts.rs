//! Single-response HTTP spool -> existing indexed source/CPU/DSP/player.
use crate::{
    isomp4_file::IndexedFile,
    resampled_streaming::{M4aResampled, core},
    spool_source::{self, Limits},
    streaming::{StreamMsg, StreamingState, stream_channel},
    streaming_transport::{self as transport},
};
use migo_io::{scheduler::IoScheduler, spool::SpoolBudget};
use rubato::{Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice};
use shared::network_policy::NetworkPolicy;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    time::Instant,
};
#[derive(Default)]
pub(crate) struct Reply {
    pub chunked: bool,
    pub tag: bool,
    pub(crate) hold: Option<usize>,
    cut: Option<usize>,
}
impl Reply {
    pub(crate) fn framing(chunked: bool, tag: bool) -> Self {
        Self {
            chunked,
            tag,
            ..Self::default()
        }
    }
}
pub(crate) struct Server {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
    pub requests: Arc<AtomicUsize>,
    pub(crate) held: Arc<Notify>,
    pub(crate) release: Arc<Notify>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) async fn server(data: Vec<u8>, reply: Reply) -> Server {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}/audio", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let held = Arc::new(Notify::new());
    let signal = held.clone();
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = vec![];
        while !raw.ends_with(b"\r\n\r\n") {
            raw.push(socket.read_u8().await.unwrap());
            assert!(raw.len() < 8192);
        }
        let request = String::from_utf8(raw).unwrap().to_ascii_lowercase();
        assert!(!request.contains("range:"));
        assert!(request.contains("accept-encoding: identity"));
        counted.fetch_add(1, Ordering::Release);
        let framing = if reply.chunked {
            "Transfer-Encoding: chunked\r\n".into()
        } else {
            format!("Content-Length: {}\r\n", data.len())
        };
        let tag = if reply.tag { "ETag: \"one\"\r\n" } else { "" };
        socket
            .write_all(
                format!("HTTP/1.1 200 OK\r\n{framing}{tag}Connection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let cut = reply.cut.unwrap_or(data.len());
        let mut at = 0;
        while at < cut {
            if reply.hold == Some(at) {
                signal.notify_one();
                gate.notified().await;
            }
            let end = (at + 4096)
                .min(cut)
                .min(reply.hold.filter(|n| *n > at).unwrap_or(cut));
            let bytes = &data[at..end];
            if reply.chunked
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
            if reply.chunked && socket.write_all(b"\r\n").await.is_err() {
                return;
            }
            at = end;
        }
        if reply.chunked {
            let _ = socket.write_all(b"0\r\n\r\n").await;
        }
    });
    Server {
        url,
        task,
        requests,
        held,
        release,
    }
}
pub(crate) fn client() -> crate::network::http::HttpClient {
    crate::network::http::HttpClientBuilder::new(
        "spool-contract",
        false,
        &NetworkPolicy {
            allow_plain_http: true,
            ..NetworkPolicy::unrestricted()
        },
        "spool",
        migo_io::pools::IoPools::new(0),
    )
    .no_proxy()
    .any_address()
    .build()
    .unwrap()
}
fn data(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name)).unwrap()
}
pub(crate) fn limits(name: &str, max_bytes: u64) -> Limits {
    Limits {
        dir: PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap())
            .parent()
            .unwrap()
            .join(format!("http-spool-{name}-{}", std::process::id())),
        budget: SpoolBudget::new(max_bytes * 2, 4).unwrap(),
        max_bytes,
        deadline: Instant::now() + Duration::from_secs(10),
        trace: None,
    }
}
fn reference(file: &[u8], target: u32) -> Vec<f32> {
    let audio = crate::candidate::decode(file, 16_777_216).unwrap();
    if target == audio.sample_rate {
        return audio.samples;
    }
    let ch = audio.channels as usize;
    let n = audio.samples.len() / ch;
    let input = InterleavedSlice::new(&audio.samples, ch, n).unwrap();
    let output = core(audio.sample_rate, target, audio.channels)
        .unwrap()
        .process_all(&input, n, None)
        .unwrap();
    let frames = (n as u64 * target as u64).div_ceil(audio.sample_rate as u64) as usize;
    let mut result = Vec::with_capacity(frames * ch);
    for f in 0..frames {
        for c in 0..ch {
            result.push(output.read_sample(c, f).unwrap());
        }
    }
    result
}
async fn produce(
    server_url: String,
    l: Limits,
    target: u32,
    tx: tokio::sync::mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
) {
    let owner = IoScheduler::new(0);
    let result = match spool_source::http(client(), server_url, &owner, l, &tx, &state).await {
        Ok(Some(reader)) => {
            let len = reader.len();
            transport::run_source(
                owner.audio_cpu(),
                IndexedFile::from_reader(reader),
                Some(len),
                &tx,
                &state,
                move |n| M4aResampled::with_object_len(target, n),
            )
            .await
        }
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };
    transport::publish_result(&tx, &state, result).await;
}
#[tokio::test]
async fn no_range_no_validator_and_unknown_lengths_spool_one_representation() {
    let mut count = 0;
    for name in [
        "0-front.m4a",
        "1-tail.m4a",
        "2-reverse.m4a",
        "2-tail-multi.m4a",
    ] {
        for chunked in [false, true] {
            for tag in [false, true] {
                for target in [44100, 48000] {
                    let bytes = data(name);
                    let expected = reference(&bytes, target);
                    let s = server(
                        bytes,
                        Reply {
                            chunked,
                            tag,
                            ..Default::default()
                        },
                    )
                    .await;
                    let state = StreamingState::new();
                    let (tx, mut rx) = stream_channel();
                    let task = tokio::spawn(produce(
                        s.url.clone(),
                        limits(name, 4 * 1024 * 1024),
                        target,
                        tx,
                        state.clone(),
                    ));
                    let (mut ready, mut done) = (0, 0);
                    let mut pcm = vec![];
                    while let Some(msg) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                        .await
                        .unwrap()
                    {
                        match msg {
                            StreamMsg::Ready { sample_rate, .. } => {
                                assert_eq!(sample_rate, target);
                                ready += 1
                            }
                            StreamMsg::Samples { pcm: v, .. } => {
                                assert_eq!((ready, done), (1, 0));
                                pcm.extend_from_slice(&v)
                            }
                            StreamMsg::Done { .. } => done += 1,
                            StreamMsg::Error { message: e, .. } => {
                                panic!("{name}/{chunked}/{tag}/{target}: {e}")
                            }
                        }
                    }
                    task.await.unwrap();
                    assert_eq!((ready, done), (1, 1));
                    assert_eq!(pcm, expected);
                    assert!(state.download_complete.load(Ordering::Acquire));
                    assert_eq!(s.requests.load(Ordering::Acquire), 1);
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 32);
    println!("W09_HTTP_SPOOL_PCM {count} exact comparisons");
}
#[tokio::test]
async fn no_ready_or_pcm_is_published_until_the_spool_is_complete() {
    let bytes = data("1-tail.m4a");
    let len = bytes.len();
    let s = server(
        bytes,
        Reply {
            chunked: true,
            hold: Some(4096.min(len - 1)),
            ..Default::default()
        },
    )
    .await;
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let task = tokio::spawn(produce(
        s.url.clone(),
        limits("held", 4 * 1024 * 1024),
        48000,
        tx,
        state.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(2), s.held.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(40), rx.recv())
            .await
            .is_err(),
        "spool published before EOF"
    );
    s.release.notify_one();
    let mut done = 0;
    while let Some(msg) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        match msg {
            StreamMsg::Done { .. } => done += 1,
            StreamMsg::Error { message: e, .. } => panic!("{e}"),
            _ => {}
        }
    }
    task.await.unwrap();
    assert_eq!(done, 1);
}
#[tokio::test]
async fn over_limit_truncation_and_deadlines_report_error_without_ready_or_done() {
    for (reply, max) in [
        (Reply::default(), 1),
        (
            Reply {
                chunked: true,
                ..Default::default()
            },
            1024,
        ),
        (
            Reply {
                cut: Some(100),
                ..Default::default()
            },
            4 * 1024 * 1024,
        ),
    ] {
        let s = server(data("1-tail.m4a"), reply).await;
        let state = StreamingState::new();
        let (tx, mut rx) = stream_channel();
        let task = tokio::spawn(produce(
            s.url.clone(),
            limits("bad", max),
            48000,
            tx,
            state.clone(),
        ));
        let mut errors = 0;
        while let Some(msg) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
        {
            match msg {
                StreamMsg::Error { message: _, .. } => errors += 1,
                _ => panic!("failed spool published audio"),
            }
        }
        task.await.unwrap();
        assert_eq!(errors, 1);
        assert!(!state.download_complete.load(Ordering::Acquire));
    }
    let owner = IoScheduler::new(0);
    let l = limits("deadline", 4096);
    let mut l = Limits {
        deadline: Instant::now(),
        ..l
    };
    let state = StreamingState::new();
    let (tx, _rx) = stream_channel();
    let s = server(data("1-tail.m4a"), Reply::default()).await;
    assert!(
        spool_source::http(client(), s.url.clone(), &owner, l.clone(), &tx, &state)
            .await
            .is_err()
    );
    l.deadline = Instant::now() + Duration::from_secs(1);
    owner.domain().close_all();
    assert!(
        spool_source::http(client(), s.url.clone(), &owner, l, &tx, &state)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn cancellation_and_receiver_drop_abort_download_without_decoder_output() {
    for close in [false, true] {
        let s = server(
            data("1-tail.m4a"),
            Reply {
                chunked: true,
                hold: Some(4096),
                ..Default::default()
            },
        )
        .await;
        let state = StreamingState::new();
        let (tx, rx) = stream_channel();
        let task = tokio::spawn(produce(
            s.url.clone(),
            limits("cancel", 4 * 1024 * 1024),
            48000,
            tx,
            state.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(2), s.held.notified())
            .await
            .unwrap();
        if close {
            drop(rx)
        } else {
            state.cancel()
        };
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(!state.download_complete.load(Ordering::Acquire));
    }
    let s = server(
        data("1-tail.m4a"),
        Reply {
            chunked: true,
            hold: Some(4096),
            ..Default::default()
        },
    )
    .await;
    let url = s.url.clone();
    let owner = IoScheduler::new(0);
    let domain = owner.domain();
    let state = StreamingState::new();
    let (tx, _rx) = stream_channel();
    let trace = Arc::new(spool_source::Trace::default());
    let mut l = limits("domain", 4 * 1024 * 1024);
    l.trace = Some(trace.clone());
    let task =
        tokio::spawn(
            async move { spool_source::http(client(), url, &owner, l, &tx, &state).await },
        );
    tokio::time::timeout(Duration::from_secs(2), s.held.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let notified = trace.waiting.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if trace.written.load(Ordering::Acquire) >= 4096 {
                break;
            }
            notified.await;
        }
    })
    .await
    .expect("client must reach the held next-body wait after writing the prefix");
    domain.close_all();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn already_consumed_response_is_rejected_before_private_file_creation() {
    for chunked in [false, true] {
        let s = server(
            data("1-tail.m4a"),
            Reply {
                chunked,
                ..Default::default()
            },
        )
        .await;
        let mut response = client()
            .get(
                crate::network::gate::GateKind::AudioStream,
                crate::network::url::Url::parse(&s.url).unwrap(),
            )
            .header(
                http::header::ACCEPT_ENCODING,
                http::HeaderValue::from_static("identity"),
            )
            .send()
            .await
            .unwrap();
        assert!(!response.chunk().await.unwrap().unwrap().is_empty());
        assert!(response.decoded_body_bytes() > 0);
        let owner = IoScheduler::new(0);
        let state = StreamingState::new();
        let (tx, _rx) = stream_channel();
        let l = limits("consumed", 4 * 1024 * 1024);
        let dir = l.dir.clone();
        assert!(!dir.exists());
        assert!(
            spool_source::response(response, &owner, l, &tx, &state)
                .await
                .is_err()
        );
        assert!(
            !dir.exists(),
            "consumed response performed spool filesystem work"
        );
    }
}
