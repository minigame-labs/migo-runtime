//! Executes the production transport, HTTP client, off-worker and PCM pool.
use crate::streaming::{StreamMsg, StreamingState};
use crate::streaming_transport::{self as transport, Decoder, Progress, Step};
use shared::{
    error::{EngineError, EngineResult, ErrorCode},
    network_policy::NetworkPolicy,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

const PATIENCE: Duration = Duration::from_secs(3);

#[cfg(feature = "bandlimited-probe")]
#[path = "resampled_transport_contracts.rs"]
mod resampling;

#[cfg(feature = "bandlimited-probe")]
#[path = "player_transport_contracts.rs"]
mod playback;

#[cfg(feature = "wave-streaming-probe")]
#[path = "wave_transport_contracts.rs"]
mod wave;

#[cfg(feature = "adts-streaming-probe")]
#[path = "adts_transport_contracts.rs"]
mod adts;

#[cfg(feature = "vorbis-streaming-probe")]
#[path = "vorbis_transport_contracts.rs"]
mod vorbis;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(std::env::var_os("W09_STREAM_FIXTURES").unwrap()).join(name))
        .unwrap()
}

struct Push(crate::push_decoder::Decoder, Arc<AtomicUsize>);
impl Decoder for Push {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        self.1.fetch_add(1, Ordering::SeqCst);
        let error = |message| EngineError::from_detail(ErrorCode::IoError, message);
        let consumed = if eof {
            self.0.finish_input();
            0
        } else {
            self.0.push(input).map_err(error)?
        };
        let progress = match self.0.drain(pcm, cancel).map_err(error)? {
            crate::push_decoder::Progress::NeedInput => Progress::NeedInput,
            #[cfg(feature = "m4a-streaming-probe")]
            crate::push_decoder::Progress::NeedSource => {
                return Err(error("MP3 requested source IO"));
            }
            crate::push_decoder::Progress::Yield => Progress::Yield,
            crate::push_decoder::Progress::Finished => Progress::Finished,
            crate::push_decoder::Progress::Cancelled => Progress::Cancelled,
        };
        Ok(Step {
            consumed,
            progress,
            sample_rate: self.0.rate,
            channels: self.0.channels,
        })
    }
}

fn client() -> crate::network::http::HttpClient {
    crate::network::http::HttpClientBuilder::new(
        "migo-streaming-probe",
        false,
        &NetworkPolicy {
            allow_plain_http: true,
            ..NetworkPolicy::unrestricted()
        },
        "audio-stream",
        migo_io::pools::IoPools::new(0),
    )
    .no_proxy()
    .any_address()
    .build()
    .unwrap()
}

#[derive(Clone, Copy)]
enum Reply {
    Chunked,
    Fixed,
    TruncatedBody,
    HangHeaders,
    HangBody,
    NotFound,
}

async fn server(
    data: Vec<u8>,
    chunk_size: usize,
    reply: Reply,
) -> (String, tokio::task::JoinHandle<()>, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://localhost:{}/audio.mp3",
        listener.local_addr().unwrap().port()
    );
    let (arrived, observed) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        if matches!(reply, Reply::HangHeaders) {
            let _ = arrived.send(());
            std::future::pending::<()>().await;
            return;
        }
        let head = match reply {
            Reply::Chunked => {
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                    .to_owned()
            }
            Reply::NotFound => {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned()
            }
            _ => format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
                    + if matches!(reply, Reply::TruncatedBody | Reply::HangBody) {
                        500
                    } else {
                        0
                    }
            ),
        };
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let _ = arrived.send(());
        if matches!(reply, Reply::HangBody) {
            std::future::pending::<()>().await;
        } else if !matches!(reply, Reply::NotFound) {
            for chunk in data.chunks(chunk_size) {
                if matches!(reply, Reply::Chunked)
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
                if matches!(reply, Reply::Chunked) && socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
                tokio::task::yield_now().await;
            }
            if matches!(reply, Reply::Chunked) {
                let _ = socket.write_all(b"0\r\n\r\n").await;
            }
        }
    });
    (url, task, observed)
}

// The scheduler owns the CPU scope for the entire task, including cancellation.
async fn run_on_cpu<D: Decoder>(
    client: crate::network::http::HttpClient,
    url: String,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    make_decoder: impl FnOnce() -> D + Send + 'static,
) {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    transport::run(owner.audio_cpu(), client, url, tx, state, make_decoder).await;
    drop(owner);
}

fn spawn(
    url: String,
    state: Arc<StreamingState>,
    steps: Arc<AtomicUsize>,
) -> (mpsc::Receiver<StreamMsg>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = crate::streaming::stream_channel();
    let io_thread = std::thread::current().id();
    let task = tokio::spawn(run_on_cpu(client(), url, tx, state, move || {
        assert_ne!(
            std::thread::current().id(),
            io_thread,
            "codec construction ran on the IO executor"
        );
        Push(crate::push_decoder::Decoder::new(None), steps)
    }));
    (rx, task)
}

async fn finished(task: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(PATIENCE, task)
        .await
        .expect("transport did not terminate")
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn real_http_chunks_partial_feed_eof_and_pcm_equal_whole_file() {
    for name in [
        "cbr-44100-1.mp3",
        "vbr-48000-2.mp3",
        "mpeg2-22050-1.mp3",
        "mpeg25-8000-2.mp3",
        "no-xing.mp3",
        "no-xing-vbr.mp3",
        "tiny.mp3",
        "long.mp3",
    ] {
        let data = fixture(name);
        let expected =
            crate::candidate::decode(&data, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize).unwrap();
        for (chunk_size, reply) in [
            (137, Reply::Chunked),
            (8192, Reply::Fixed),
            (100_000, Reply::Chunked),
        ] {
            let state = StreamingState::new();
            let (url, serving, _) = server(data.clone(), chunk_size, reply).await;
            let (mut rx, task) = spawn(url, state.clone(), Arc::new(AtomicUsize::new(0)));
            let mut actual = Vec::new();
            let mut ready = 0;
            let mut done = 0;
            while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
                match message {
                    StreamMsg::Ready {
                        sample_rate,
                        channels,
                        ..
                    } => {
                        ready += 1;
                        assert_eq!(
                            (sample_rate, channels),
                            (expected.sample_rate, expected.channels)
                        );
                        assert_eq!(done, 0);
                    }
                    StreamMsg::Samples { pcm, .. } => {
                        assert_eq!((ready, done), (1, 0));
                        assert!(pcm.len() <= crate::push_decoder::SAMPLES_PER_STEP);
                        actual.extend_from_slice(&pcm);
                    }
                    StreamMsg::Done { .. } => {
                        assert_eq!(ready, 1);
                        done += 1;
                    }
                    StreamMsg::Error { message: error, .. } => {
                        panic!("{name}/{chunk_size}: {error}")
                    }
                }
            }
            finished(task).await;
            finished(serving).await;
            assert_eq!((ready, done), (1, 1));
            assert_eq!(actual, expected.samples, "{name}/{chunk_size}");
            assert_eq!(
                state.bytes_downloaded.load(Ordering::Acquire),
                data.len() as u64
            );
            assert!(state.download_complete.load(Ordering::Acquire));
        }
    }
}

async fn filled(rx: &mpsc::Receiver<StreamMsg>) {
    tokio::time::timeout(PATIENCE, async {
        while rx.len() != crate::streaming::STREAM_CHANNEL_CAPACITY {
            assert!(!rx.is_closed(), "stream ended before applying backpressure");
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn four_slot_backpressure_stops_decode_and_cancellation_releases_full_queue() {
    let state = StreamingState::new();
    let steps = Arc::new(AtomicUsize::new(0));
    let (url, serving, _) = server(fixture("long.mp3"), 100_000, Reply::Fixed).await;
    let (mut rx, task) = spawn(url, state.clone(), steps.clone());
    filled(&rx).await;
    // One decode may already have started when Ready + three Samples fill the
    // channel; after that send blocks, so no later decode may be scheduled.
    tokio::time::sleep(Duration::from_millis(30)).await;
    let before = steps.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        steps.load(Ordering::SeqCst),
        before,
        "decode outran the full PCM queue"
    );
    state.cancel();
    finished(task).await;
    serving.abort();
    assert!(!state.download_complete.load(Ordering::Acquire));
    while let Some(message) = rx.recv().await {
        assert!(!matches!(
            message,
            StreamMsg::Done { .. } | StreamMsg::Error { message: _, .. }
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn consumer_drop_and_header_body_cancellation_terminate_actual_http_tasks() {
    for reply in [Reply::HangHeaders, Reply::HangBody] {
        for drop_consumer in [false, true] {
            let state = StreamingState::new();
            let (url, serving, observed) = server(Vec::new(), 1, reply).await;
            let (rx, task) = spawn(url, state.clone(), Arc::new(AtomicUsize::new(0)));
            tokio::time::timeout(PATIENCE, observed)
                .await
                .unwrap()
                .unwrap();
            if drop_consumer {
                drop(rx);
            } else {
                state.cancel();
            }
            finished(task).await;
            serving.abort();
            assert!(!state.download_complete.load(Ordering::Acquire));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn http_status_body_truncation_and_codec_errors_never_publish_done() {
    for (data, reply) in [
        (fixture("long.mp3"), Reply::TruncatedBody),
        (Vec::new(), Reply::NotFound),
        (b"not MPEG data".to_vec(), Reply::Fixed),
        (fixture("long.mp3")[..500].to_vec(), Reply::Fixed),
    ] {
        let state = StreamingState::new();
        let (url, serving, _) = server(data, 137, reply).await;
        let (mut rx, task) = spawn(url, state.clone(), Arc::new(AtomicUsize::new(0)));
        let mut errors = 0;
        while let Some(message) = tokio::time::timeout(PATIENCE, rx.recv()).await.unwrap() {
            match message {
                StreamMsg::Done { .. } => panic!("failed stream sent Done"),
                StreamMsg::Error { message: _, .. } => errors += 1,
                _ => assert_eq!(errors, 0),
            }
        }
        finished(task).await;
        finished(serving).await;
        assert_eq!(errors, 1);
        assert!(!state.download_complete.load(Ordering::Acquire));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_messages_cancel_while_queue_is_full() {
    for terminal in [
        StreamMsg::Done {
            revision: crate::streaming::StreamRevision::INITIAL,
        },
        StreamMsg::Error {
            revision: crate::streaming::StreamRevision::INITIAL,
            message: "decode failed".into(),
        },
    ] {
        let (tx, _rx) = crate::streaming::stream_channel();
        for _ in 0..crate::streaming::STREAM_CHANNEL_CAPACITY {
            tx.try_send(StreamMsg::Done {
                revision: crate::streaming::StreamRevision::INITIAL,
            })
            .unwrap_or_else(|_| panic!("channel capacity changed"));
        }
        let state = StreamingState::new();
        let waiter_state = state.clone();
        let task = tokio::spawn(async move { transport::send(&tx, &waiter_state, terminal).await });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        state.cancel();
        assert!(
            !tokio::time::timeout(PATIENCE, task)
                .await
                .expect("terminal send ignored cancellation")
                .unwrap()
        );
    }
}

struct Paused {
    started: Option<oneshot::Sender<()>>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    dropped: Arc<AtomicBool>,
}
impl Drop for Paused {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}
impl Decoder for Paused {
    fn step(&mut self, _: &[u8], _: bool, _: &mut Vec<f32>, _: &AtomicBool) -> EngineResult<Step> {
        let _ = self.started.take().unwrap().send(());
        let (lock, wake) = &*self.gate;
        let _ = wake
            .wait_timeout_while(
                lock.lock().unwrap(),
                Duration::from_millis(800),
                |released| !*released,
            )
            .unwrap();
        Ok(Step {
            consumed: 0,
            progress: Progress::Cancelled,
            sample_rate: 0,
            channels: 0,
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cpu_step_leaves_http_worker_free_and_cancel_does_not_wait_for_join() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let dropped = Arc::new(AtomicBool::new(false));
    let (started, observed) = oneshot::channel();
    let (url, serving, _) = server(vec![0; 10], 10, Reply::Fixed).await;
    let (tx, _rx) = crate::streaming::stream_channel();
    let state = StreamingState::new();
    let inside_gate = gate.clone();
    let inside_dropped = dropped.clone();
    let before = Instant::now();
    let task = tokio::spawn(run_on_cpu(client(), url, tx, state.clone(), move || {
        Paused {
            started: Some(started),
            gate: inside_gate,
            dropped: inside_dropped,
        }
    }));
    tokio::time::timeout(PATIENCE, observed)
        .await
        .unwrap()
        .unwrap();
    // The second session must get headers on this same single IO worker while
    // the first session's decoder is deliberately held on a blocking thread.
    let (other, other_server, _) = server(Vec::new(), 1, Reply::Fixed).await;
    let result = client()
        .get(
            crate::network::gate::GateKind::AudioStream,
            crate::network::url::Url::parse(&other).unwrap(),
        )
        .send()
        .await;
    let io_elapsed = before.elapsed();
    state.cancel();
    let cancel_result = tokio::time::timeout(Duration::from_millis(200), task).await;
    let was_still_running = !dropped.load(Ordering::Acquire);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    serving.abort();
    finished(other_server).await;
    assert!(result.is_ok(), "co-tenant HTTP failed: {result:?}");
    assert!(
        io_elapsed < Duration::from_millis(500),
        "CPU blocked the IO executor: {io_elapsed:?}"
    );
    cancel_result
        .expect("cancel waited for the CPU join")
        .unwrap();
    assert!(
        was_still_running,
        "test never observed cancellation during the held CPU step"
    );
    tokio::time::timeout(PATIENCE, async {
        while !dropped.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
