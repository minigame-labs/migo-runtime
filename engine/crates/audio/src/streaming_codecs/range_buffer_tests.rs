//! Actual HTTP fills a pre-admitted allocation; the original CPU retires it.
use super::*;
use crate::off_worker::OffWorker;
use migo_services::network::{gate::GateKind, http::range, url::Url};
use std::future::Future;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn byte(at: u64) -> u8 {
    (at.wrapping_mul(37) ^ 0x5a) as u8
}
async fn ranged_server() -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            children.spawn(async move {
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    match socket.read_u8().await { Ok(x)=>header.push(x), Err(_)=>return }
                    assert!(header.len() < 8192);
                }
                let request = String::from_utf8(header).unwrap().to_ascii_lowercase();
                let Some(range) = request.lines().find_map(|line| line.strip_prefix("range: bytes=")) else {
                    let _=socket.write_all(b"HTTP/1.1 200 OK\r\nETag: \"fixed\"\r\nContent-Length: 5000000000\r\nConnection: close\r\n\r\n").await;
                    return;
                };
                assert!(request.contains("if-range: \"fixed\"\r\n"));
                assert!(request.contains("accept-encoding: identity\r\n"));
                let (start,end)=range.split_once('-').unwrap();
                let start=start.parse::<u64>().unwrap();let end=end.parse::<u64>().unwrap();
                let length=end-start+1;
                let head=format!("HTTP/1.1 206 Partial Content\r\nETag: \"fixed\"\r\nContent-Range: bytes {start}-{end}/5000000000\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
                if socket.write_all(head.as_bytes()).await.is_err(){return}
                let mut out=[0;8192];let mut at=start;
                while at<=end {
                    let n=(end-at+1).min(out.len() as u64) as usize;
                    for (i,value) in out[..n].iter_mut().enumerate(){*value=byte(at+i as u64);}
                    if socket.write_all(&out[..n]).await.is_err(){return}
                    at+=n as u64;
                }
            });
        }
    });
    Server {
        url: "http://range-buffer-fixture.invalid/control".into(),
        proxy,
        task,
        release: None,
    }
}

#[tokio::test]
async fn admitted_http_index_uses_original_capacity_without_another_full_body_vec_and_frees_on_cpu()
{
    const CAP: usize = 4 * 1024 * 1024;
    const LENGTH: usize = 2 * 1024 * 1024;
    const GUARD: usize = 32;
    const OFFSET: u64 = 4_294_967_319;
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(1063, 4, CAP as u64);
    let peer = host.peer_for_admission_probe(1064);
    let ticket = host.pools().reserve_bytes(CAP as u64).unwrap();
    let mut data = Vec::with_capacity(CAP);
    data.resize(LENGTH + 2 * GUARD, 0xa5);
    assert_eq!(data.capacity(), CAP);
    let pointer = data.as_ptr();
    let watch = migo_alloc_probe::DeallocationWatch::new(pointer).unwrap();
    let server = ranged_server().await;
    let client = client(&server);
    let response = client
        .get(GateKind::AudioStream, Url::parse(&server.url).unwrap())
        .header(
            "accept-encoding".parse().unwrap(),
            "identity".parse().unwrap(),
        )
        .send()
        .await
        .unwrap();
    let identity = range::Identity::capture(&response).unwrap();
    drop(response);
    let mut reader = range::Reader::new(
        client,
        GateKind::AudioStream,
        identity,
        range::Limits::default(),
    );
    let mut max_poll_bytes = 0;
    {
        let operation = reader.read_exact_into(
            OFFSET,
            &mut data[GUARD..GUARD + LENGTH],
            tokio::time::Instant::now() + WAIT,
        );
        tokio::pin!(operation);
        std::future::poll_fn(|cx| {
            let before = migo_alloc_probe::thread_counts();
            let result = operation.as_mut().poll(cx);
            max_poll_bytes = max_poll_bytes
                .max(migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated);
            result
        })
        .await
        .unwrap();
    }
    assert!(
        data[GUARD..GUARD + LENGTH]
            .iter()
            .enumerate()
            .all(|(i, value)| *value == byte(OFFSET + i as u64)),
        "borrowed HTTP control bytes changed"
    );
    assert!(
        data[..GUARD]
            .iter()
            .chain(&data[GUARD + LENGTH..])
            .all(|v| *v == 0xa5)
    );
    assert_eq!(
        data.as_ptr(),
        pointer,
        "HTTP replaced the admitted allocation"
    );
    assert_eq!(data.capacity(), CAP);
    assert!(
        max_poll_bytes < (LENGTH / 2) as u64,
        "range read allocated another full-size index buffer: {max_poll_bytes}"
    );
    assert_eq!(reader.bytes_received(), LENGTH as u64);
    let (started, entered) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let owner = OffWorker::new((), host.audio_cpu());
    let task = tokio::spawn(owner.with(move |_| {
        started.send(()).unwrap();
        blocked.recv_timeout(WAIT).unwrap();
        let before = migo_alloc_probe::thread_counts();
        drop(data);
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= CAP as u64,
            "original CPU did not actually free the admitted HTTP allocation"
        );
        assert!(
            std::thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
        );
        drop(ticket);
    }));
    tokio::task::spawn_blocking(move || entered.recv_timeout(WAIT).unwrap())
        .await
        .unwrap();
    let held = watch.freed_bytes().is_none() && host.pools().reserve_bytes(1).is_err();
    let progress = peer.audio_cpu().run(|| 17).await.unwrap();
    release.send(()).unwrap();
    let (owner, ()) = task.await.unwrap().unwrap();
    assert!(
        held,
        "HTTP allocation or its capacity credit retired before the original CPU returned"
    );
    assert_eq!(progress, 17);
    assert_eq!(
        watch.freed_bytes(),
        Some(CAP),
        "original HTTP allocation capacity was not really destroyed"
    );
    assert!(host.pools().reserve_bytes(CAP as u64).is_ok());
    drop(owner);
    println!(
        "MIGO_RANGE_BUFFER actual_capacity={CAP} data_length={} control_length={LENGTH} max_poll_allocated={max_poll_bytes} original_cpu_free=true",
        LENGTH + 2 * GUARD
    );
}
