use crate::{
    format_source::Format,
    http_source::{self, Kind},
    isomp4_source_contracts::{self as indexed, Config, Fault},
    local_source_contracts::{self as local, Received},
    spool_contracts::{self as spool, Reply},
    streaming::{StreamMsg, StreamingState, stream_channel},
    streaming_transport,
};
use migo_io::scheduler::IoScheduler;
use std::{
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
        &format!("auto-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        4 * 1024 * 1024,
    )
}
async fn receive(owner: IoScheduler, plan: http_source::Prepared, target: u32) -> Received {
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    let check = state.clone();
    let task = tokio::spawn(async move {
        let result = plan.run(&owner, target, &tx, &state).await;
        streaming_transport::publish_result(&tx, &state, result).await;
    });
    let mut got = Received {
        pcm: Vec::new(),
        ready: 0,
        done: 0,
        errors: Vec::new(),
    };
    while let Some(msg) = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
    {
        match msg {
            StreamMsg::Ready { sample_rate, .. } => {
                assert_eq!(sample_rate, target);
                got.ready += 1;
            }
            StreamMsg::Samples(p) => {
                assert_eq!((got.ready, got.done, got.errors.len()), (1, 0, 0));
                got.pcm.extend_from_slice(&p);
            }
            StreamMsg::Done => got.done += 1,
            StreamMsg::Error(e) => got.errors.push(e),
        }
    }
    task.await.unwrap();
    assert_eq!(
        check.download_complete.load(Ordering::Acquire),
        got.done == 1
    );
    got
}
async fn prepare(
    url: String,
    owner: &IoScheduler,
    format: Option<Format>,
    limit: crate::spool_source::Limits,
) -> http_source::Prepared {
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    let plan = http_source::http(spool::client(), url, owner, format, limit, &tx, &state)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rx.try_recv().is_err(),
        "source preparation published decoder output"
    );
    plan
}
fn exact(got: Received, data: &[u8], target: u32) {
    assert_eq!(
        (got.ready, got.done, got.errors.len()),
        (1, 1, 0),
        "{:?}",
        got.errors
    );
    assert_eq!(got.pcm, local::reference(data, target).0);
}
#[tokio::test]
async fn known_sequential_formats_keep_one_response_and_do_not_create_spool() {
    let mut count = 0;
    for (format, name) in [
        (Format::Mp3, "3.mp3"),
        (Format::Wave, "3.wav"),
        (Format::Adts, "3.aac"),
        (Format::Ogg, "2.ogg"),
    ] {
        let data = std::fs::read(local::assets().join(name)).unwrap();
        for chunked in [false, true] {
            for tag in [false, true] {
                for target in [44100, 48000] {
                    let server = spool::server(data.clone(), Reply::framing(chunked, tag)).await;
                    let owner = IoScheduler::new(0);
                    let limit = limits();
                    let dir = limit.dir.clone();
                    let plan = prepare(server.url.clone(), &owner, Some(format), limit).await;
                    assert_eq!(plan.kind(), Kind::Sequential);
                    assert!(!dir.exists());
                    exact(receive(owner, plan, target).await, &data, target);
                    assert_eq!(server.requests.load(Ordering::Acquire), 1);
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 32);
    println!("W09_HTTP_SELECT_SEQUENTIAL_PCM {count}");
}
#[tokio::test]
async fn m4a_actual_range_capability_or_original_response_spool_preserves_pcm() {
    let mut count = 0;
    for name in [
        "0-front.m4a",
        "1-tail.m4a",
        "2-reverse.m4a",
        "2-tail-reverse.m4a",
    ] {
        let data = indexed::fixture(name);
        for chunked in [false, true] {
            for mode in 0..3 {
                for target in [44100, 48000] {
                    let server = indexed::start_server(
                        data.clone(),
                        Config {
                            chunked,
                            no_tag: mode == 1,
                            fault: if mode == 2 {
                                Fault::IgnoredRange
                            } else {
                                Fault::None
                            },
                            ..Config::default()
                        },
                    )
                    .await;
                    let owner = IoScheduler::new(0);
                    let limit = limits();
                    let dir = limit.dir.clone();
                    let plan =
                        prepare(server.url.to_string(), &owner, Some(Format::M4a), limit).await;
                    assert_eq!(
                        plan.kind(),
                        if mode == 0 { Kind::Range } else { Kind::Spool }
                    );
                    if mode == 0 {
                        assert_eq!(plan.range_requests(), Some(1));
                        assert!(!dir.exists());
                    } else {
                        assert!(dir.exists());
                        assert_eq!(server.ranges.lock().unwrap().len(), usize::from(mode == 2));
                    }
                    exact(receive(owner, plan, target).await, &data, target);
                    assert_eq!(
                        server.full_requests.load(Ordering::Acquire),
                        1,
                        "fallback fetched another full object"
                    );
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 48);
    println!("W09_HTTP_SELECT_M4A_PCM {count}");
}
#[tokio::test]
async fn unknown_containers_use_single_bounded_spool_then_library_identification() {
    let mut assets = vec![];
    for name in ["0.mp3", "0.wav", "0.aac", "0.ogg"] {
        assets.push(std::fs::read(local::assets().join(name)).unwrap());
    }
    assets.push(indexed::fixture("1-tail.m4a"));
    let mut count = 0;
    for data in assets {
        for chunked in [false, true] {
            for target in [44100, 48000] {
                let server = spool::server(data.clone(), Reply::framing(chunked, true)).await;
                let owner = IoScheduler::new(0);
                let plan = prepare(server.url.clone(), &owner, None, limits()).await;
                assert_eq!(plan.kind(), Kind::Spool);
                exact(receive(owner, plan, target).await, &data, target);
                assert_eq!(server.requests.load(Ordering::Acquire), 1);
                count += 1;
            }
        }
    }
    assert_eq!(count, 20);
    println!("W09_HTTP_SELECT_UNKNOWN_PCM {count}");
}
#[tokio::test]
async fn changed_or_incomplete_range_is_an_error_without_fallback_or_decoder() {
    for fault in [Fault::ChangedTag, Fault::BodyShort] {
        let server = indexed::start_server(
            indexed::fixture("1-tail.m4a"),
            Config {
                fault,
                ..Config::default()
            },
        )
        .await;
        let owner = IoScheduler::new(0);
        let limit = limits();
        let dir = limit.dir.clone();
        let (tx, mut rx) = stream_channel();
        let state = StreamingState::new();
        let result = http_source::http(
            spool::client(),
            server.url.to_string(),
            &owner,
            Some(Format::M4a),
            limit,
            &tx,
            &state,
        )
        .await;
        assert!(result.is_err());
        assert!(!dir.exists());
        assert!(rx.try_recv().is_err());
        assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
        assert_eq!(server.ranges.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn unread_complete_response_is_required_before_admission() {
    use migo_services::network::{http::Response, url::Url};
    for case in 0..3 {
        let mut raw = http::Response::builder().status(if case == 0 { 206 } else { 200 });
        if case == 1 {
            raw = raw.header(http::header::CONTENT_RANGE, "bytes 0-3/4");
        }
        let mut response = Response::local(
            Url::parse("http://localhost/unknown").unwrap(),
            raw.body(b"data".to_vec()).unwrap(),
        );
        if case == 2 {
            response.chunk().await.unwrap();
        }
        let owner = IoScheduler::new(0);
        let limit = limits();
        let dir = limit.dir.clone();
        let (tx, mut rx) = stream_channel();
        let state = StreamingState::new();
        assert!(
            http_source::response(
                spool::client(),
                response,
                &owner,
                Some(Format::Mp3),
                limit,
                &tx,
                &state
            )
            .await
            .is_err()
        );
        assert!(!dir.exists());
        assert!(rx.try_recv().is_err());
    }
}
#[tokio::test]
async fn cancellation_receiver_and_domain_close_end_pending_range_before_admission() {
    for stop in 0..3 {
        let server = indexed::start_server(
            indexed::fixture("1-tail.m4a"),
            Config {
                fault: Fault::HangHeaders,
                ..Config::default()
            },
        )
        .await;
        let owner = Arc::new(IoScheduler::new(0));
        let limit = limits();
        let dir = limit.dir.clone();
        let (tx, rx) = stream_channel();
        let state = StreamingState::new();
        let worker = owner.clone();
        let s = state.clone();
        let url = server.url.to_string();
        let task = tokio::spawn(async move {
            http_source::http(
                spool::client(),
                url,
                &worker,
                Some(Format::M4a),
                limit,
                &tx,
                &s,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), server.index_arrived.notified())
            .await
            .unwrap();
        match stop {
            0 => state.cancel(),
            1 => drop(rx),
            _ => owner.close(),
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
        assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    }
}
#[tokio::test]
async fn fallback_keeps_original_absolute_deadline_and_disk_limit() {
    let server = indexed::start_server(
        indexed::fixture("1-tail.m4a"),
        Config {
            fault: Fault::IgnoredRange,
            hold: Some(0),
            ..Config::default()
        },
    )
    .await;
    let owner = Arc::new(IoScheduler::new(0));
    let mut limit = limits();
    limit.deadline = Instant::now() + Duration::from_millis(150);
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let s = state.clone();
    let o = owner.clone();
    let url = server.url.to_string();
    let task = tokio::spawn(async move {
        http_source::http(spool::client(), url, &o, Some(Format::M4a), limit, &tx, &s).await
    });
    tokio::time::timeout(Duration::from_secs(1), server.index_arrived.notified())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    server.release.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    let data = indexed::fixture("1-tail.m4a");
    let server = spool::server(data.clone(), Reply::framing(false, false)).await;
    let mut limit = limits();
    limit.max_bytes = data.len() as u64 - 1;
    let dir = limit.dir.clone();
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    assert!(
        http_source::http(
            spool::client(),
            server.url.clone(),
            &owner,
            Some(Format::M4a),
            limit,
            &tx,
            &state
        )
        .await
        .is_err()
    );
    assert!(!dir.exists());
    assert!(rx.try_recv().is_err());
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn queued_unknown_probe_stops_on_playback_cancel_and_receiver_exit() {
    use migo_io::task::{PoolKind, PriorityClass};
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    for close in [false, true] {
        let data = std::fs::read(local::assets().join("0.mp3")).unwrap();
        let server = spool::server(data, Reply::framing(false, false)).await;
        let owner = IoScheduler::isolated_for_admission_probe(957, 1, 4 * 1024 * 1024);
        let plan = prepare(server.url.clone(), &owner, None, limits()).await;
        let (started, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let blocker = owner
            .pools()
            .submit_async(PoolKind::Fs, PriorityClass::ForegroundAsync, move || {
                started.send(()).unwrap();
                wait.recv().unwrap();
            })
            .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let (tx, rx) = stream_channel();
        let state = StreamingState::new();
        let mut pending = Box::pin(plan.run(&owner, 48000, &tx, &state));
        assert!(matches!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        if close {
            drop(rx);
        } else {
            state.cancel();
        }
        let stopped = tokio::time::timeout(Duration::from_millis(250), pending).await;
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        owner
            .pools()
            .run(PoolKind::Fs, PriorityClass::Background, || ())
            .unwrap();
        assert!(
            stopped.is_ok(),
            "cancelled probe waited for a queued filesystem job"
        );
        assert!(stopped.unwrap().is_ok());
        assert!(state.is_cancelled());
    }
}
#[tokio::test]
async fn range_failure_after_selection_never_reselects_spool() {
    let server = indexed::start_server(
        indexed::fixture("2-buffered-miss.m4a"),
        Config {
            media_fault: Fault::IgnoredRange,
            ..Config::default()
        },
    )
    .await;
    let owner = IoScheduler::new(0);
    let limit = limits();
    let dir = limit.dir.clone();
    let plan = prepare(server.url.to_string(), &owner, Some(Format::M4a), limit).await;
    assert_eq!(plan.kind(), Kind::Range);
    let got = receive(owner, plan, 48000).await;
    assert_eq!((got.done, got.errors.len()), (0, 1));
    assert!(
        server.ranges.lock().unwrap().len() > 1,
        "no post-admission range was attempted"
    );
    assert!(!dir.exists());
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn decoded_m4a_uses_one_decoded_response_without_byte_ranges() {
    use std::io::Write;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let data = indexed::fixture("1-tail.m4a");
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&data).unwrap();
    let body = gzip.finish().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/wrong.mp3", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        while !raw.ends_with(b"\r\n\r\n") {
            raw.push(socket.read_u8().await.unwrap());
            assert!(raw.len() < 8192);
        }
        assert!(
            !String::from_utf8(raw)
                .unwrap()
                .to_ascii_lowercase()
                .contains("range:")
        );
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nETag: \"one\"\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
    });
    let owner = IoScheduler::new(0);
    let plan = prepare(url, &owner, Some(Format::M4a), limits()).await;
    assert_eq!(plan.kind(), Kind::Spool);
    exact(receive(owner, plan, 48000).await, &data, 48000);
    task.await.unwrap();
}
