use super::*;
use crate::network::http::HttpClientBuilder;
use shared::network_policy::NetworkPolicy;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

fn client() -> HttpClient {
    HttpClientBuilder::new(
        "range-contract",
        false,
        &NetworkPolicy {
            allow_plain_http: true,
            ..NetworkPolicy::unrestricted()
        },
        "audio",
        migo_io::pools::IoPools::new(0),
    )
    .no_proxy()
    .any_address()
    .build()
    .unwrap()
}
fn payload(start: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (start.wrapping_add(i as u64).wrapping_mul(37) ^ 0x5a) as u8)
        .collect()
}
#[derive(Clone, Copy)]
enum Reply {
    Fixed,
    Chunked,
    Short,
    UnknownTotal,
    WrongStart,
    WrongEnd,
    ChangedTag,
    MissingTag,
    ChangedLength,
    Ignored,
    ChangedFull,
    Gzip,
    Multipart,
    DuplicateRange,
    MissingRange,
    BodyShort,
    BodyLong,
    HangHeaders,
    HangBody,
    SlowShort,
    Redirect,
    GzipInitial,
    OtherCoding,
    Streamed,
    Drip,
    HoldTail,
    ChangedAfterFirst,
}
struct Server {
    url: Url,
    task: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<String>>>,
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn start_server(reply: Reply, length: u64, initial_chunked: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://localhost:{}/asset",
        listener.local_addr().unwrap().port()
    ))
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let arrived = Arc::new(Notify::new());
    let signal = arrived.clone();
    let release = Arc::new(Notify::new());
    let unblock = release.clone();
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let observed = observed.clone();
            let signal = signal.clone();
            let unblock = unblock.clone();
            children.spawn(async move {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    match socket.read_u8().await { Ok(b) => request.push(b), Err(_) => return }
                    assert!(request.len() < 8192);
                }
                let text = String::from_utf8(request).unwrap().to_ascii_lowercase();
                let ordinal = {
                    let mut requests = observed.lock().unwrap();
                    requests.push(text.clone());
                    requests.len()
                };
                let range = text.lines().find_map(|line| line.strip_prefix("range: bytes="));
                let Some(range) = range else {
                    if matches!(reply, Reply::GzipInitial) {
                        let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Encoding: gzip\r\nContent-Length: 20\r\nConnection: close\r\n\r\n").await;
                        let _ = socket.write_all(&[31,139,8,0,0,0,0,0,0,3,3,0,0,0,0,0,0,0,0,0]).await;
                        return;
                    }
                    let head = if initial_chunked {
                        "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_owned()
                    } else {
                        format!("HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n")
                    };
                    if socket.write_all(head.as_bytes()).await.is_ok() {
                        // Only the head is used to pin a virtual large object.
                        let body = payload(0, length.min(512) as usize);
                        if initial_chunked {
                            let _ = socket.write_all(format!("{:x}\r\n", body.len()).as_bytes()).await;
                            let _ = socket.write_all(&body).await;
                            let _ = socket.write_all(b"\r\n0\r\n\r\n").await;
                        } else { let _ = socket.write_all(&body).await; }
                    }
                    return;
                };
                signal.notify_one();
                let (start,end) = range.trim().split_once('-').unwrap();
                let start = start.parse::<u64>().unwrap();
                let mut end = end.parse::<u64>().unwrap();
                assert!(text.contains("if-range: \"v1\"\r\n"));
                assert!(text.contains("accept-encoding: identity\r\n"));
                if matches!(reply, Reply::HangHeaders) { std::future::pending::<()>().await; }
                if matches!(reply, Reply::Short | Reply::SlowShort | Reply::ChangedAfterFirst) { end = end.min(start + 3); }
                if matches!(reply, Reply::SlowShort) { tokio::time::sleep(Duration::from_millis(25)).await; }
                if matches!(reply, Reply::Redirect) && !text.starts_with("get /changed ") {
                    let _ = socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /changed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    return;
                }
                let mut tag = "ETag: \"v1\"\r\n";
                if matches!(reply, Reply::ChangedTag | Reply::ChangedFull) || (matches!(reply, Reply::ChangedAfterFirst) && ordinal > 2) { tag = "ETag: \"v2\"\r\n"; }
                if matches!(reply, Reply::MissingTag) { tag = ""; }
                if matches!(reply, Reply::Ignored | Reply::ChangedFull) {
                    let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\n{tag}Content-Length: 10485760\r\nConnection: close\r\n\r\n").as_bytes()).await;
                    std::future::pending::<()>().await;
                }
                let total = if matches!(reply, Reply::UnknownTotal) { "*".to_owned() }
                    else { (length + u64::from(matches!(reply, Reply::ChangedLength))).to_string() };
                let declared_start = start - u64::from(matches!(reply, Reply::WrongStart));
                let declared_end = end - u64::from(matches!(reply, Reply::WrongStart))
                    + u64::from(matches!(reply, Reply::WrongEnd));
                let range = format!("Content-Range: bytes {declared_start}-{declared_end}/{total}\r\n");
                let mut head = format!("HTTP/1.1 206 Partial Content\r\n{tag}Connection: close\r\n");
                if !matches!(reply, Reply::MissingRange) { head.push_str(&range); }
                if matches!(reply, Reply::DuplicateRange) { head.push_str(&range); }
                if matches!(reply, Reply::Gzip) { head.push_str("Content-Encoding: gzip\r\n"); }
                if matches!(reply, Reply::OtherCoding) { head.push_str("Content-Encoding: snappy\r\n"); }
                if matches!(reply, Reply::Multipart) { head.push_str("Content-Type: multipart/byteranges; boundary=x\r\n"); }
                let chunked = matches!(reply, Reply::Chunked | Reply::BodyShort | Reply::BodyLong | Reply::HoldTail | Reply::Drip);
                let count = (declared_end - declared_start + 1) as usize;
                head.push_str(&if chunked { "Transfer-Encoding: chunked\r\n\r\n".to_owned() }
                    else { format!("Content-Length: {count}\r\n\r\n") });
                if socket.write_all(head.as_bytes()).await.is_err() { return; }
                if matches!(reply, Reply::HangBody) { std::future::pending::<()>().await; }
                let count = count - usize::from(matches!(reply, Reply::BodyShort)) + usize::from(matches!(reply, Reply::BodyLong));
                let step = if matches!(reply, Reply::Streamed) { 8192 } else if matches!(reply, Reply::BodyLong) { 1 } else { 3 };
                for offset in (0..count).step_by(step) {
                    let body = payload(start + offset as u64, step.min(count - offset));
                    let bytes = body.as_slice();
                    if chunked && socket.write_all(format!("{:x}\r\n", bytes.len()).as_bytes()).await.is_err() { return; }
                    if socket.write_all(bytes).await.is_err() { return; }
                    if chunked && socket.write_all(b"\r\n").await.is_err() { return; }
                    if matches!(reply, Reply::Drip) { tokio::time::sleep(Duration::from_millis(20)).await; }
                    if matches!(reply, Reply::HoldTail) && offset == 0 && count > step { unblock.notified().await; }
                }
                // Overlong data must be rejected before it enters the result
                // buffer, without waiting for the peer to finish its body.
                if matches!(reply, Reply::BodyLong) { std::future::pending::<()>().await; }
                if chunked { let _ = socket.write_all(b"0\r\n\r\n").await; }
            });
        }
    });
    Server {
        url,
        task,
        requests,
        arrived,
        release,
    }
}
async fn reader(server: &Server, limits: Limits) -> Reader {
    let client = client();
    let response = client
        .get(GateKind::AudioStream, server.url.clone())
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .await
        .unwrap();
    let identity = Identity::capture(&response).unwrap();
    drop(response);
    Reader::new(client, GateKind::AudioStream, identity, limits)
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

#[tokio::test]
async fn decoded_initial_response_cannot_be_pinned_even_after_its_headers_are_normalized() {
    let server = start_server(Reply::GzipInitial, 512, false).await;
    let mut response = client()
        .get(GateKind::AudioStream, server.url.clone())
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .await
        .unwrap();
    assert!(!response.headers().contains_key(CONTENT_ENCODING));
    assert!(!response.headers().contains_key(CONTENT_LENGTH));
    assert!(matches!(
        Identity::capture(&response),
        Err(Error::InvalidResponse(_))
    ));
    while response.chunk().await.unwrap().is_some() {}
    assert!(matches!(
        Identity::capture(&response),
        Err(Error::InvalidResponse(_))
    ));
}

#[test]
fn range_headers_reject_invalid_coordinates_and_keep_u64_positions() {
    for valid in [
        "bytes 0-15/16",
        "BYTES 4294967300-4294967309/5000000000",
        "bytes 0-7/*",
    ] {
        assert!(
            ContentRange::parse(&HeaderValue::from_str(valid).unwrap()).is_ok(),
            "{valid}"
        );
    }
    for bad in [
        "items 0-1/8",
        "bytes */8",
        "bytes 0-8/8",
        "bytes 9-1/20",
        "bytes +1-2/8",
        "bytes 0-18446744073709551615/*",
        "bytes 0-1/18446744073709551616",
        "bytes 0-1/8, bytes 2-3/8",
        "bytes 0-1/0",
    ] {
        assert!(
            ContentRange::parse(&HeaderValue::from_str(bad).unwrap()).is_err(),
            "{bad}"
        );
    }
}
#[test]
fn capture_requires_a_strong_validator_and_identity_encoding() {
    for tag in [b"\"v1\"".as_slice(), b"\"\xff\"", b"\"a\\b\""] {
        let response = Response::local(
            Url::parse("https://example.test/asset").unwrap(),
            http::Response::builder()
                .header(ETAG, HeaderValue::from_bytes(tag).unwrap())
                .header(CONTENT_LENGTH, "8")
                .body(vec![0; 8])
                .unwrap(),
        );
        assert_eq!(Identity::capture(&response).unwrap().length(), Some(8));
    }
    for tag in [None, Some("W/\"v1\""), Some("v1"), Some("\"bad\"quote\"")] {
        let mut builder = http::Response::builder();
        if let Some(tag) = tag {
            builder = builder.header(ETAG, tag);
        }
        let response = Response::local(
            Url::parse("https://example.test/asset").unwrap(),
            builder.body(vec![]).unwrap(),
        );
        assert!(matches!(
            Identity::capture(&response),
            Err(Error::NoStrongValidator)
        ));
    }
    let response = Response::local(
        Url::parse("https://example.test/asset").unwrap(),
        http::Response::builder()
            .header(ETAG, "\"v1\"")
            .header(CONTENT_ENCODING, "gzip")
            .body(vec![])
            .unwrap(),
    );
    assert!(matches!(
        Identity::capture(&response),
        Err(Error::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn actual_http_fixed_chunked_partial_and_unknown_lengths_keep_exact_bytes() {
    for mode in [
        Reply::Fixed,
        Reply::Chunked,
        Reply::Short,
        Reply::UnknownTotal,
    ] {
        for initial_chunked in [false, true] {
            let server = start_server(mode, 512, initial_chunked).await;
            let mut source = reader(&server, Limits::default()).await;
            assert_eq!(
                source.length(),
                if initial_chunked { None } else { Some(512) }
            );
            assert_eq!(
                source.read_exact_at(37, 61, deadline()).await.unwrap(),
                payload(37, 61)
            );
            assert_eq!(source.bytes_received(), 61);
            assert_eq!(
                source.length(),
                if initial_chunked && matches!(mode, Reply::UnknownTotal) {
                    None
                } else {
                    Some(512)
                }
            );
            assert_eq!(
                source.read_exact_at(7, 19, deadline()).await.unwrap(),
                payload(7, 19)
            );
            assert_eq!(source.bytes_received(), 80);
            let requests = server.requests.lock().unwrap();
            assert_eq!(requests.len() as u32, source.requests() + 1);
            assert!(
                requests
                    .iter()
                    .all(|r| r.contains("accept-encoding: identity\r\n"))
            );
        }
    }
}

#[tokio::test]
async fn initial_identity_length_does_not_change_after_body_is_consumed() {
    for chunked in [false, true] {
        let server = start_server(Reply::Fixed, 512, chunked).await;
        let mut response = client()
            .get(GateKind::AudioStream, server.url.clone())
            .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
            .send()
            .await
            .unwrap();
        let expected = if chunked { None } else { Some(512) };
        assert_eq!(Identity::capture(&response).unwrap().length(), expected);
        while response.chunk().await.unwrap().is_some() {}
        assert_eq!(Identity::capture(&response).unwrap().length(), expected);
    }
}
#[tokio::test]
async fn actual_http_virtual_large_object_does_not_truncate_absolute_offsets() {
    let offset = (1_u64 << 34) + 73;
    let server = start_server(Reply::Fixed, offset + 512, false).await;
    let mut source = reader(&server, Limits::default()).await;
    assert_eq!(
        source.read_exact_at(offset, 31, deadline()).await.unwrap(),
        payload(offset, 31)
    );
    assert!(server.requests.lock().unwrap()[1].contains(&format!("range: bytes={offset}-")));
}
#[tokio::test]
async fn actual_http_identity_extent_and_body_failures_never_return_partial_data() {
    for mode in [
        Reply::WrongStart,
        Reply::WrongEnd,
        Reply::ChangedTag,
        Reply::MissingTag,
        Reply::ChangedLength,
        Reply::Gzip,
        Reply::OtherCoding,
        Reply::Multipart,
        Reply::DuplicateRange,
        Reply::MissingRange,
        Reply::BodyShort,
        Reply::BodyLong,
        Reply::Redirect,
    ] {
        let server = start_server(mode, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        assert!(
            tokio::time::timeout(
                Duration::from_secs(1),
                source.read_exact_at(7, 16, deadline())
            )
            .await
            .expect("invalid extents must fail before waiting for the response end")
            .is_err()
        );
        assert_eq!(
            source.requests(),
            1,
            "invalid body must fail its first request"
        );
        assert_eq!(
            source.read_exact_at(7, 16, deadline()).await,
            Err(Error::PreviouslyFailed)
        );
    }
}
#[tokio::test]
async fn ignored_range_and_changed_full_object_are_classified_before_reading_the_body() {
    for (mode, error) in [
        (Reply::Ignored, Error::RangeUnsupported),
        (Reply::ChangedFull, Error::RepresentationChanged),
    ] {
        let server = start_server(mode, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                source.read_exact_at(7, 16, deadline())
            )
            .await
            .unwrap(),
            Err(error)
        );
        assert_eq!(source.bytes_received(), 0);
    }
}
#[tokio::test]
async fn actual_http_read_byte_request_and_object_budgets_are_independent() {
    for limits in [
        Limits {
            max_read_bytes: 15,
            ..Limits::default()
        },
        Limits {
            max_total_bytes: 15,
            ..Limits::default()
        },
        Limits {
            max_requests: 0,
            ..Limits::default()
        },
    ] {
        let server = start_server(Reply::Fixed, 512, false).await;
        let mut source = reader(&server, limits).await;
        assert_eq!(
            source.read_exact_at(7, 16, deadline()).await,
            Err(Error::Limit)
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }
    for (offset, len) in [(500, 16), (u64::MAX, 1), (0, 0)] {
        let server = start_server(Reply::Fixed, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        assert_eq!(
            source.read_exact_at(offset, len, deadline()).await,
            Err(Error::Limit)
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }
    let server = start_server(Reply::Short, 512, false).await;
    let mut source = reader(
        &server,
        Limits {
            max_requests: 2,
            ..Limits::default()
        },
    )
    .await;
    assert_eq!(
        source.read_exact_at(7, 16, deadline()).await,
        Err(Error::Limit)
    );
    assert_eq!(source.bytes_received(), 8);
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    let server = start_server(Reply::Fixed, 512, false).await;
    let mut source = reader(
        &server,
        Limits {
            max_total_bytes: 24,
            ..Limits::default()
        },
    )
    .await;
    source.read_exact_at(7, 16, deadline()).await.unwrap();
    assert_eq!(
        source.read_exact_at(7, 16, deadline()).await,
        Err(Error::Limit)
    );
    assert_eq!(source.requests(), 1);
}
#[tokio::test]
async fn cancellation_and_one_absolute_deadline_end_pending_headers_body_and_partial_retries() {
    for mode in [Reply::HangHeaders, Reply::HangBody] {
        let server = start_server(mode, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        {
            let read = source.read_exact_at(7, 16, deadline());
            tokio::pin!(read);
            tokio::select! { result = &mut read => panic!("pending response completed: {result:?}"),
            _ = server.arrived.notified() => {} }
        }
        assert_eq!(
            source.read_exact_at(7, 16, deadline()).await,
            Err(Error::PreviouslyFailed)
        );
    }
    for mode in [Reply::HangHeaders, Reply::HangBody, Reply::SlowShort] {
        let server = start_server(mode, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        let start = Instant::now();
        assert_eq!(
            source
                .read_exact_at(7, 64, start + Duration::from_millis(70))
                .await,
            Err(Error::Timeout)
        );
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}

async fn drain<R: std::borrow::BorrowMut<Reader>>(
    stream: &mut super::stream::MediaStream<R>,
) -> Result<u64, Error> {
    let mut count = 0;
    loop {
        let position = stream.position();
        let Some(chunk) = stream.chunk().await? else {
            break;
        };
        assert_eq!(chunk.as_ref(), payload(position, chunk.len()));
        count += chunk.len() as u64;
    }
    Ok(count)
}

#[tokio::test]
async fn media_stream_exceeds_control_budgets_without_collecting_or_truncating_positions() {
    let offset = (1_u64 << 34) + 73;
    let len = 6 * 1024 * 1024 + 37;
    let server = start_server(Reply::Streamed, offset + len + 128, false).await;
    let mut source = reader(
        &server,
        Limits {
            max_read_bytes: 16,
            max_total_bytes: 16,
            max_requests: 2,
        },
    )
    .await;
    assert_eq!(
        source.read_exact_at(7, 8, deadline()).await.unwrap(),
        payload(7, 8)
    );
    {
        let mut stream = source
            .stream_at(offset, len, StreamLimits::default())
            .unwrap();
        assert_eq!(drain(&mut stream).await.unwrap(), len);
        assert_eq!(stream.position(), offset + len);
        assert_eq!(stream.bytes_received(), len);
        assert_eq!(stream.requests(), 1);
        assert_eq!(stream.chunk().await.unwrap(), None);
    }
    assert_eq!(source.bytes_received(), 8);
    assert_eq!(source.requests(), 1);
    assert_eq!(
        source.read_exact_at(9, 8, deadline()).await.unwrap(),
        payload(9, 8)
    );
    assert_eq!(source.bytes_received(), 16);
    assert_eq!(source.requests(), 2);
}

#[tokio::test]
async fn media_short_responses_continue_exactly_and_unknown_total_is_not_object_eof() {
    for mode in [
        Reply::Fixed,
        Reply::Chunked,
        Reply::Short,
        Reply::UnknownTotal,
    ] {
        for chunked in [false, true] {
            let server = start_server(mode, 512, chunked).await;
            let mut source = reader(&server, Limits::default()).await;
            let mut stream = source.stream_at(37, 61, StreamLimits::default()).unwrap();
            assert_eq!(drain(&mut stream).await.unwrap(), 61);
            assert_eq!(stream.position(), 98);
            assert_eq!(stream.bytes_received(), 61);
            assert_eq!(
                stream.requests(),
                if matches!(mode, Reply::Short) { 16 } else { 1 }
            );
            assert_eq!(
                stream.object_length(),
                if chunked && matches!(mode, Reply::UnknownTotal) {
                    None
                } else {
                    Some(512)
                }
            );
        }
    }
}

#[tokio::test]
async fn media_rejects_headers_body_faults_and_version_changes_before_retry_or_success() {
    for mode in [
        Reply::WrongStart,
        Reply::WrongEnd,
        Reply::ChangedTag,
        Reply::MissingTag,
        Reply::ChangedLength,
        Reply::Gzip,
        Reply::OtherCoding,
        Reply::Multipart,
        Reply::DuplicateRange,
        Reply::MissingRange,
        Reply::BodyShort,
        Reply::BodyLong,
        Reply::Redirect,
        Reply::ChangedAfterFirst,
    ] {
        let server = start_server(mode, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        {
            let mut stream = source.stream_at(7, 16, StreamLimits::default()).unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), drain(&mut stream))
                    .await
                    .expect("invalid media must not wait for untrusted body EOF")
                    .is_err()
            );
            assert_eq!(
                stream.requests(),
                if matches!(mode, Reply::ChangedAfterFirst) {
                    2
                } else {
                    1
                }
            );
            assert_eq!(
                tokio::time::timeout(Duration::from_millis(500), stream.chunk())
                    .await
                    .expect("a failed or cancelled media read must fail immediately"),
                Err(Error::PreviouslyFailed)
            );
        }
        assert_eq!(
            source.read_exact_at(7, 16, deadline()).await,
            Err(Error::PreviouslyFailed)
        );
    }
    for (mode, expected) in [
        (Reply::Ignored, Error::RangeUnsupported),
        (Reply::ChangedFull, Error::RepresentationChanged),
    ] {
        let server = start_server(mode, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        let mut stream = source.stream_at(7, 16, StreamLimits::default()).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), stream.chunk())
                .await
                .unwrap(),
            Err(expected)
        );
        assert_eq!(stream.bytes_received(), 0);
    }
}

#[tokio::test]
async fn media_first_chunk_and_backpressure_do_not_wait_for_or_expire_the_body() {
    let server = start_server(Reply::HoldTail, 512, false).await;
    let mut source = reader(&server, Limits::default()).await;
    {
        let mut stream = source
            .stream_at(
                7,
                16,
                StreamLimits {
                    header_timeout: Duration::from_millis(100),
                    idle_timeout: Duration::from_millis(100),
                    ..StreamLimits::default()
                },
            )
            .unwrap();
        let first = tokio::time::timeout(Duration::from_millis(500), stream.chunk())
            .await
            .expect("first bytes must arrive before the held tail")
            .unwrap()
            .unwrap();
        assert_eq!(first.as_ref(), payload(7, first.len()));
        assert!(stream.position() < 23);
        tokio::time::sleep(Duration::from_millis(160)).await;
        server.release.notify_one();
        assert_eq!(drain(&mut stream).await.unwrap() + first.len() as u64, 16);
    }
    // Dropping a completed chunk's stream does not poison later control reads.
    let mut stream = source.stream_at(7, 16, StreamLimits::default()).unwrap();
    assert!(stream.chunk().await.unwrap().is_some());
    drop(stream);
    server.release.notify_one();
    assert_eq!(
        source.read_exact_at(7, 3, deadline()).await.unwrap(),
        payload(7, 3)
    );
}

#[tokio::test]
async fn media_request_count_extent_and_timeout_limits_fail_without_unbounded_requests() {
    let server = start_server(Reply::Short, 512, false).await;
    let mut source = reader(&server, Limits::default()).await;
    let mut stream = source
        .stream_at(
            7,
            16,
            StreamLimits {
                max_requests: 2,
                ..StreamLimits::default()
            },
        )
        .unwrap();
    assert_eq!(drain(&mut stream).await, Err(Error::Limit));
    assert_eq!(stream.bytes_received(), 8);
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    drop(stream);
    for (offset, len, limits) in [
        (500, 16, StreamLimits::default()),
        (u64::MAX, 1, StreamLimits::default()),
        (0, 0, StreamLimits::default()),
        (
            0,
            1,
            StreamLimits {
                header_timeout: Duration::ZERO,
                ..StreamLimits::default()
            },
        ),
        (
            0,
            1,
            StreamLimits {
                idle_timeout: Duration::ZERO,
                ..StreamLimits::default()
            },
        ),
    ] {
        let mut source = reader(&server, Limits::default()).await;
        let before = server.requests.lock().unwrap().len();
        assert!(matches!(
            source.stream_at(offset, len, limits),
            Err(Error::Limit)
        ));
        assert_eq!(server.requests.lock().unwrap().len(), before);
        assert!(matches!(
            source.stream_at(7, 16, StreamLimits::default()),
            Err(Error::PreviouslyFailed)
        ));
    }
}

#[tokio::test]
async fn media_header_idle_and_cancel_deadlines_are_separate_from_duration() {
    for mode in [Reply::HangHeaders, Reply::HangBody] {
        let server = start_server(mode, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        let start = Instant::now();
        let mut stream = source
            .stream_at(
                7,
                16,
                StreamLimits {
                    header_timeout: Duration::from_millis(70),
                    idle_timeout: Duration::from_millis(70),
                    ..StreamLimits::default()
                },
            )
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(500), stream.chunk())
                .await
                .expect("header and idle timers must both interrupt pending IO"),
            Err(Error::Timeout)
        );
        assert!(start.elapsed() < Duration::from_millis(500));
    }
    for mode in [Reply::HangHeaders, Reply::HangBody] {
        let server = start_server(mode, 512, false).await;
        let mut source = reader(&server, Limits::default()).await;
        let mut stream = source.stream_at(7, 16, StreamLimits::default()).unwrap();
        {
            let read = stream.chunk();
            tokio::pin!(read);
            tokio::select! { result = &mut read => panic!("pending media completed: {result:?}"),
            _ = server.arrived.notified() => {} }
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(500), stream.chunk())
                .await
                .expect("a failed or cancelled media read must fail immediately"),
            Err(Error::PreviouslyFailed)
        );
        drop(stream);
        assert_eq!(
            source.read_exact_at(7, 16, deadline()).await,
            Err(Error::PreviouslyFailed)
        );
    }
    let server = start_server(Reply::Drip, 512, false).await;
    let mut source = reader(&server, Limits::default()).await;
    let start = Instant::now();
    let mut stream = source
        .stream_at(
            7,
            32,
            StreamLimits {
                header_timeout: Duration::from_millis(100),
                idle_timeout: Duration::from_millis(100),
                ..StreamLimits::default()
            },
        )
        .unwrap();
    assert_eq!(drain(&mut stream).await.unwrap(), 32);
    assert!(
        start.elapsed() > Duration::from_millis(100),
        "header timeout must not become whole-media timeout"
    );
}

#[tokio::test]
async fn owned_media_returns_the_same_pinned_reader_and_its_control_budget() {
    let server = start_server(Reply::Chunked, 512, true).await;
    let mut source = reader(
        &server,
        Limits {
            max_read_bytes: 8,
            max_total_bytes: 16,
            max_requests: 2,
        },
    )
    .await;
    source.read_exact_at(7, 8, deadline()).await.unwrap();
    assert_eq!(source.length(), Some(512));
    let mut stream = source
        .into_stream_at(37, 61, StreamLimits::default())
        .unwrap();
    assert_eq!(drain(&mut stream).await.unwrap(), 61);
    assert_eq!(stream.reader().bytes_received(), 8);
    assert_eq!(stream.reader().requests(), 1);
    let mut source = stream.into_reader();
    assert_eq!(source.length(), Some(512));
    assert_eq!(
        source.read_exact_at(9, 8, deadline()).await.unwrap(),
        payload(9, 8)
    );
    let mut stream = source
        .into_stream_at(10, 32, StreamLimits::default())
        .unwrap();
    assert_eq!(drain(&mut stream).await.unwrap(), 32);
    let mut source = stream.into_reader();
    assert_eq!(
        source.read_exact_at(9, 1, deadline()).await,
        Err(Error::Limit)
    );
}

#[tokio::test]
async fn cancelled_owned_media_returns_a_failed_reader_without_retrying_io() {
    for mode in [Reply::HangHeaders, Reply::HangBody] {
        let server = start_server(mode, 512, false).await;
        let source = reader(&server, Limits::default()).await;
        let mut stream = source
            .into_stream_at(7, 16, StreamLimits::default())
            .unwrap();
        {
            let read = stream.chunk();
            tokio::pin!(read);
            tokio::select! {result = &mut read => panic!("pending media completed: {result:?}"),
            _ = server.arrived.notified() => {} }
        }
        let requests = server.requests.lock().unwrap().len();
        let mut source = stream.into_reader();
        assert_eq!(
            source.read_exact_at(7, 16, deadline()).await,
            Err(Error::PreviouslyFailed)
        );
        assert!(matches!(
            source.into_stream_at(7, 16, StreamLimits::default()),
            Err(Error::PreviouslyFailed)
        ));
        assert_eq!(server.requests.lock().unwrap().len(), requests);
    }
}
