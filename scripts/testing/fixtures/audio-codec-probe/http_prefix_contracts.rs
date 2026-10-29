//! Observe actual held HTTP bodies, payload pointers, CPU workers and budgets.
use crate::{
    http_prefix,
    http_source::{self, Kind},
    local_source_contracts as local,
    replay_body::{Body, Replayed},
    source_chunk::BodyChunk,
    spool_contracts as spool,
    streaming::{StreamMsg, StreamingState, stream_channel},
    streaming_transport::{
        self as transport, Decoder, HttpSource, Sequential, Source, SourceRead, Step,
    },
};
use bytes::Bytes;
use migo_io::{
    scheduler::IoScheduler,
    task::{PoolKind, PriorityClass},
};
use migo_services::network::{http::Response, url::Url};
use shared::error::EngineResult;
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn limits() -> crate::spool_source::Limits {
    spool::limits(
        &format!("prefix-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        4 * 1024 * 1024,
    )
}
fn response(data: Vec<u8>) -> Response {
    Response::local(
        Url::parse("http://127.0.0.1:1/wrong.mp3").unwrap(),
        http::Response::builder()
            .status(200)
            .header("ETag", "\"one\"")
            .body(data)
            .unwrap(),
    )
}
#[derive(Default)]
struct Got {
    pcm: Vec<f32>,
    ready: usize,
    done: usize,
    errors: Vec<String>,
}
fn accept(got: &mut Got, msg: StreamMsg) {
    match msg {
        StreamMsg::Ready { .. } => got.ready += 1,
        StreamMsg::Samples(p) => {
            assert_eq!((got.ready, got.done, got.errors.len()), (1, 0, 0));
            got.pcm.extend_from_slice(&p);
        }
        StreamMsg::Done => got.done += 1,
        StreamMsg::Error(e) => got.errors.push(e),
    }
}
async fn drain(mut rx: tokio::sync::mpsc::Receiver<StreamMsg>, mut got: Got) -> Got {
    while let Some(msg) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        accept(&mut got, msg);
    }
    got
}
fn exact(got: Got, data: &[u8], rate: u32) {
    assert_eq!(
        (got.ready, got.done, got.errors.len()),
        (1, 1, 0),
        "{:?}",
        got.errors
    );
    assert_eq!(got.pcm, local::reference(data, rate).0);
}
async fn play(owner: IoScheduler, plan: http_source::Prepared, rate: u32) -> Got {
    let (tx, rx) = stream_channel();
    let state = StreamingState::new();
    let task = tokio::spawn(async move {
        let r = plan.run(&owner, rate, &tx, &state).await;
        transport::publish_result(&tx, &state, r).await;
    });
    let got = drain(rx, Got::default()).await;
    task.await.unwrap();
    got
}
#[tokio::test]
async fn unknown_sequential_formats_publish_pcm_before_the_original_body_eof() {
    let mut count = 0;
    for name in ["3.mp3", "3.wav", "3.aac", "2.ogg"] {
        for chunked in [false, true] {
            for target in [44100, 48000] {
                let data = std::fs::read(local::assets().join(name)).unwrap();
                let hold = (data.len() - 1).min(32768);
                let mut reply = spool::Reply::framing(chunked, false);
                reply.hold = Some(hold);
                let server = spool::server(data.clone(), reply).await;
                let limit = limits();
                let dir = limit.dir.clone();
                let owner = IoScheduler::new(0);
                let (tx, mut rx) = stream_channel();
                let state = StreamingState::new();
                let observed = state.clone();
                let url = server.url.clone();
                let task = tokio::spawn(async move {
                    let result = async {
                        if let Some(plan) = http_source::http(
                            spool::client(),
                            url,
                            &owner,
                            None,
                            limit,
                            &tx,
                            &state,
                        )
                        .await?
                        {
                            assert_eq!(plan.kind(), Kind::Sequential);
                            plan.run(&owner, target, &tx, &state).await?;
                        }
                        Ok(())
                    }
                    .await;
                    transport::publish_result(&tx, &state, result).await;
                });
                tokio::time::timeout(Duration::from_secs(2), server.held.notified())
                    .await
                    .unwrap();
                let mut got = Got::default();
                let early = tokio::time::timeout(Duration::from_secs(2), async {
                    while got.pcm.is_empty() {
                        accept(&mut got, rx.recv().await.expect("no early PCM"));
                    }
                })
                .await;
                let complete = observed.download_complete.load(Ordering::Acquire);
                server.release.notify_one();
                assert!(early.is_ok(), "{name}/{chunked} waited for full body");
                assert!(!complete);
                assert!(!dir.exists());
                let got = drain(rx, got).await;
                task.await.unwrap();
                exact(got, &data, target);
                assert_eq!(server.requests.load(Ordering::Acquire), 1);
                count += 1;
            }
        }
    }
    assert_eq!(count, 16);
    println!("W09_PREFIX_EARLY_PCM {count} exact comparisons before EOF");
}
#[tokio::test]
async fn complete_m4a_prefix_uses_cached_bytes_without_disk_or_range_get() {
    let mut count = 0;
    for name in ["0-front.m4a", "1-tail.m4a", "2-reverse.m4a"] {
        for target in [44100, 48000] {
            let data = crate::isomp4_source_contracts::fixture(name);
            assert!(data.len() as u64 <= http_prefix::MAX_PREFIX_BYTES);
            let owner = IoScheduler::new(0);
            let limit = limits();
            let dir = limit.dir.clone();
            let (tx, _rx) = stream_channel();
            let state = StreamingState::new();
            let plan = http_source::response(
                spool::client(),
                response(data.clone()),
                &owner,
                None,
                limit,
                &tx,
                &state,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(plan.kind(), Kind::Buffered);
            assert!(!dir.exists());
            exact(play(owner, plan, target).await, &data, target);
            count += 1;
        }
    }
    assert_eq!(count, 6);
    println!("W09_PREFIX_CACHED_PCM {count} exact comparisons, zero GET");
}
#[tokio::test]
async fn oversized_prefix_spools_the_exact_consumed_prefix_once() {
    for chunked in [false, true] {
        for target in [44100, 48000] {
            let data = std::fs::read(std::env::var_os("W09_PREFIX_PADDED").unwrap()).unwrap();
            // Verify encoded bytes independently of a decoder ignoring padding.
            let raw_server =
                spool::server(data.clone(), spool::Reply::framing(chunked, false)).await;
            let owner = IoScheduler::new(0);
            let raw = spool::client()
                .get(
                    migo_services::network::gate::GateKind::AudioStream,
                    Url::parse(&raw_server.url).unwrap(),
                )
                .header(
                    http::header::ACCEPT_ENCODING,
                    http::HeaderValue::from_static("identity"),
                )
                .send()
                .await
                .unwrap();
            let (tx, _rx) = stream_channel();
            let state = StreamingState::new();
            let (body, format) = http_prefix::probe(
                Body::new(raw).unwrap(),
                &owner,
                Instant::now() + Duration::from_secs(5),
                &tx,
                &state,
            )
            .await
            .unwrap()
            .unwrap();
            assert!(format.is_none());
            let mut reader = crate::spool_source::body(body, &owner, limits(), &tx, &state)
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            while let Some(chunk) = reader.chunk().await.unwrap() {
                bytes.extend_from_slice(chunk.as_ref());
            }
            assert_eq!(bytes, data, "spool altered the consumed prefix");
            let server = spool::server(data.clone(), spool::Reply::framing(chunked, false)).await;
            let owner = IoScheduler::new(0);
            let (tx, _rx) = stream_channel();
            let state = StreamingState::new();
            let limit = limits();
            let dir = limit.dir.clone();
            let plan = http_source::http(
                spool::client(),
                server.url.clone(),
                &owner,
                None,
                limit,
                &tx,
                &state,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(plan.kind(), Kind::Spool);
            assert!(dir.exists());
            exact(play(owner, plan, target).await, &data, target);
            assert_eq!(server.requests.load(Ordering::Acquire), 1);
        }
    }
    println!("W09_PREFIX_FALLBACK_PCM 4 exact comparisons");
}
struct Dummy;
impl Decoder for Dummy {
    fn step(
        &mut self,
        _: &[u8],
        _: bool,
        _: &mut Vec<f32>,
        _: &std::sync::atomic::AtomicBool,
    ) -> EngineResult<Step> {
        unreachable!()
    }
}
#[tokio::test]
async fn tiny_http_frames_hit_the_read_count_bound_and_keep_exact_fallback_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let audio = std::fs::read(local::assets().join("0.mp3")).unwrap();
    let mut data = b"ID3\x04\0\0\0\0\x10\0".to_vec();
    data.resize(10 + 2048, 0);
    data.extend_from_slice(&audio);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/wrong.wav", listener.local_addr().unwrap());
    let sent = data.clone();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.set_nodelay(true).unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        assert!(
            !String::from_utf8(request)
                .unwrap()
                .to_ascii_lowercase()
                .contains("range:")
        );
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for byte in sent {
            socket
                .write_all(&[b'1', b'\r', b'\n', byte, b'\r', b'\n'])
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
        socket.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let owner = IoScheduler::new(0);
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let plan = http_source::http(spool::client(), url, &owner, None, limits(), &tx, &state)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.kind(), Kind::Spool);
    exact(play(owner, plan, 48000).await, &data, 48000);
    task.await.unwrap();
    println!("W09_PREFIX_TINY_PCM 1 exact comparison");
}
#[tokio::test]
async fn replay_keeps_payload_pointer_order_and_credit_after_source_drop() {
    let owner = IoScheduler::isolated_for_admission_probe(972, 1, 64);
    let pools = owner.pools();
    let a = BodyChunk::new(Bytes::from(vec![1, 2, 3]), pools).unwrap();
    let slice = a.slice(1, 3);
    assert_eq!(
        slice.as_ref().as_ptr(),
        a.as_ref()[1..].as_ptr(),
        "prefix slice copied payload"
    );
    drop(slice);
    let pointer = a.as_ref().as_ptr();
    let b = BodyChunk::new(Bytes::from(vec![4, 5, 6]), pools).unwrap();
    let mut prefix = VecDeque::new();
    prefix.push_back(a);
    prefix.push_back(b);
    let source =
        <Sequential as HttpSource<Dummy>>::new(spool::client(), response(vec![7, 8, 9])).unwrap();
    let mut source = Replayed { source, prefix };
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let first = <Replayed<Sequential> as Source<Dummy>>::read(&mut source, &tx, &state)
        .await
        .unwrap();
    let SourceRead::Data(first) = first else {
        panic!("no prefix");
    };
    assert_eq!(first.as_ref(), [1, 2, 3]);
    assert_eq!(first.as_ref().as_ptr(), pointer);
    for expected in [[4, 5, 6], [7, 8, 9]] {
        let next = <Replayed<Sequential> as Source<Dummy>>::read(&mut source, &tx, &state)
            .await
            .unwrap();
        let SourceRead::Data(next) = next else {
            panic!("lost bytes");
        };
        assert_eq!(next.as_ref(), expected);
    }
    drop(source);
    assert!(pools.reserve_bytes(64).is_err(), "live prefix lost credit");
    drop(first);
    assert!(pools.reserve_bytes(64).is_ok());
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        http_prefix::HOOK.lock().unwrap().take();
    }
}
#[tokio::test]
async fn cancelled_active_cpu_probe_keeps_credit_until_worker_exit_and_stops_reads() {
    for close in [false, true] {
        let owner = Arc::new(IoScheduler::isolated_for_admission_probe(
            973,
            1,
            256 * 1024,
        ));
        let (start, wait) = std::sync::mpsc::channel();
        let hook = Arc::new(http_prefix::Hook {
            started: tokio::sync::Notify::new(),
            reads: AtomicUsize::new(0),
            release: std::sync::Mutex::new(wait),
        });
        *http_prefix::HOOK.lock().unwrap() = Some(hook.clone());
        let _reset = Reset;
        let (tx, rx) = stream_channel();
        let state = StreamingState::new();
        let s = state.clone();
        let o = owner.clone();
        let body = response(std::fs::read(local::assets().join("0.mp3")).unwrap());
        let limit = limits();
        let task = tokio::spawn(async move {
            http_source::response(spool::client(), body, &o, None, limit, &tx, &s).await
        });
        tokio::time::timeout(Duration::from_secs(2), hook.started.notified())
            .await
            .unwrap();
        assert!(owner.pools().reserve_bytes(256 * 1024).is_err());
        if close {
            drop(rx);
        } else {
            state.cancel();
        }
        let result = tokio::time::timeout(Duration::from_millis(250), task).await;
        let held = owner.pools().reserve_bytes(256 * 1024).is_err();
        start.send(()).unwrap();
        owner
            .pools()
            .run(PoolKind::Fs, PriorityClass::Background, || ())
            .unwrap();
        assert!(result.unwrap().unwrap().unwrap().is_none());
        assert!(
            held,
            "cancelled active parser returned payload credit early"
        );
        assert_eq!(hook.reads.load(Ordering::Relaxed), 0);
        assert!(owner.pools().reserve_bytes(256 * 1024).is_ok());
    }
}
#[tokio::test]
async fn late_cpu_probe_checks_original_deadline_before_read_without_waiter_repoll() {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    let owner = IoScheduler::isolated_for_admission_probe(974, 1, 256 * 1024);
    let (start, wait) = std::sync::mpsc::channel();
    let hook = Arc::new(http_prefix::Hook {
        started: tokio::sync::Notify::new(),
        reads: AtomicUsize::new(0),
        release: std::sync::Mutex::new(wait),
    });
    *http_prefix::HOOK.lock().unwrap() = Some(hook.clone());
    let _reset = Reset;
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let body = Body::new(response(
        std::fs::read(local::assets().join("0.mp3")).unwrap(),
    ))
    .unwrap();
    let mut pending = Box::pin(http_prefix::probe(
        body,
        &owner,
        Instant::now() + Duration::from_millis(80),
        &tx,
        &state,
    ));
    assert!(matches!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    tokio::time::timeout(Duration::from_secs(2), hook.started.notified())
        .await
        .unwrap();
    std::thread::sleep(Duration::from_millis(120));
    start.send(()).unwrap();
    owner
        .pools()
        .run(PoolKind::Fs, PriorityClass::Background, || ())
        .unwrap();
    assert_eq!(
        hook.reads.load(Ordering::Relaxed),
        0,
        "expired parser ran before timeout waiter repoll"
    );
    assert!(pending.await.is_err());
}
#[tokio::test]
async fn partial_prefix_body_cancel_deadline_and_scratch_admission_fail_without_disk() {
    for stop in 0..3 {
        let data = std::fs::read(local::assets().join("3.mp3")).unwrap();
        let mut reply = spool::Reply::framing(false, false);
        reply.hold = Some(0);
        let server = spool::server(data, reply).await;
        let owner = Arc::new(IoScheduler::new(0));
        let (tx, rx) = stream_channel();
        let state = StreamingState::new();
        let s = state.clone();
        let o = owner.clone();
        let mut limit = limits();
        let dir = limit.dir.clone();
        if stop == 2 {
            limit.deadline = Instant::now() + Duration::from_millis(80);
        }
        let url = server.url.clone();
        let task = tokio::spawn(async move {
            http_source::http(spool::client(), url, &o, None, limit, &tx, &s).await
        });
        tokio::time::timeout(Duration::from_secs(2), server.held.notified())
            .await
            .unwrap();
        match stop {
            0 => state.cancel(),
            1 => drop(rx),
            _ => {}
        }
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        if stop == 2 {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_none());
        }
        assert!(!dir.exists());
    }
    let owner = IoScheduler::isolated_for_admission_probe(975, 1, 128 * 1024 - 1);
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let limit = limits();
    let dir = limit.dir.clone();
    assert!(
        http_source::response(
            spool::client(),
            response(std::fs::read(local::assets().join("0.mp3")).unwrap()),
            &owner,
            None,
            limit,
            &tx,
            &state
        )
        .await
        .is_err()
    );
    assert!(!dir.exists());
    assert!(owner.pools().reserve_bytes(128 * 1024 - 1).is_ok());
}
