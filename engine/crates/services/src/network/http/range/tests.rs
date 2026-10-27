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
}
struct Server {
    url: Url,
    task: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<String>>>,
    arrived: Arc<Notify>,
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
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let observed = observed.clone();
            let signal = signal.clone();
            children.spawn(async move {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    match socket.read_u8().await { Ok(b) => request.push(b), Err(_) => return }
                    assert!(request.len() < 8192);
                }
                let text = String::from_utf8(request).unwrap().to_ascii_lowercase();
                observed.lock().unwrap().push(text.clone());
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
                if matches!(reply, Reply::Short | Reply::SlowShort) { end = end.min(start + 3); }
                if matches!(reply, Reply::SlowShort) { tokio::time::sleep(Duration::from_millis(25)).await; }
                if matches!(reply, Reply::Redirect) && !text.starts_with("get /changed ") {
                    let _ = socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /changed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    return;
                }
                let mut tag = "ETag: \"v1\"\r\n";
                if matches!(reply, Reply::ChangedTag | Reply::ChangedFull) { tag = "ETag: \"v2\"\r\n"; }
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
                if matches!(reply, Reply::Multipart) { head.push_str("Content-Type: multipart/byteranges; boundary=x\r\n"); }
                let chunked = matches!(reply, Reply::Chunked | Reply::BodyShort | Reply::BodyLong);
                let count = (declared_end - declared_start + 1) as usize;
                head.push_str(&if chunked { "Transfer-Encoding: chunked\r\n\r\n".to_owned() }
                    else { format!("Content-Length: {count}\r\n\r\n") });
                if socket.write_all(head.as_bytes()).await.is_err() { return; }
                if matches!(reply, Reply::HangBody) { std::future::pending::<()>().await; }
                let count = count - usize::from(matches!(reply, Reply::BodyShort)) + usize::from(matches!(reply, Reply::BodyLong));
                let body = payload(start, count);
                for bytes in body.chunks(3) {
                    if chunked && socket.write_all(format!("{:x}\r\n", bytes.len()).as_bytes()).await.is_err() { return; }
                    if socket.write_all(bytes).await.is_err() { return; }
                    if chunked && socket.write_all(b"\r\n").await.is_err() { return; }
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
