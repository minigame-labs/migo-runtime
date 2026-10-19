use super::*;
use shared::network_policy::{DomainPattern, DomainPolicy};

/// `allowed` on the request channel, or every domain on every channel when it is empty; plain HTTP only when
/// `https_only` is false.
fn policy(allowed: &[&str], https_only: bool) -> NetworkPolicy {
    let mut policy = if allowed.is_empty() {
        NetworkPolicy::unrestricted()
    } else {
        NetworkPolicy {
            request: DomainPolicy::Allowlist(
                allowed
                    .iter()
                    .map(|host| DomainPattern::parse(host).unwrap())
                    .collect(),
            ),
            ..NetworkPolicy::default()
        }
    };
    policy.allow_plain_http = !https_only;
    policy
}

struct Session {
    policy: NetworkPolicy,
    client: Client,
    resources: ResourceTable,
    pools: IoPools,
}

impl Session {
    fn new(policy: NetworkPolicy) -> Self {
        let client =
            super::super::client::create_policy_http_client("migo", false, &policy, "fetch")
                .expect("a client");
        Self::with_client(policy, client)
    }

    /// A session whose client is `configure`d after the policy's own settings: the
    /// environment's proxies are the machine's, so a test that means to be direct, or
    /// to be proxied, says which.
    fn configured(
        policy: NetworkPolicy,
        configure: impl FnOnce(
            super::super::http::HttpClientBuilder,
        ) -> super::super::http::HttpClientBuilder,
    ) -> Self {
        let builder = super::super::client::policy_client_builder("migo", false, &policy, "fetch");
        let client = configure(builder).build().expect("a client");
        Self::with_client(policy, client)
    }

    fn with_client(policy: NetworkPolicy, client: Client) -> Self {
        Self {
            policy,
            client,
            resources: ResourceTable::new(),
            pools: IoPools::new(4242),
        }
    }

    fn env(&self) -> FetchEnv<'_> {
        FetchEnv {
            policy: &self.policy,
            client: &self.client,
            resources: &self.resources,
            pools: &self.pools,
            allow_host: false,
        }
    }

    fn fetch(&self, method: &str, url: &str) -> Result<FetchHandles, ServiceError> {
        self.fetch_as(FetchKind::Request, method, url)
    }

    fn fetch_as(
        &self,
        kind: FetchKind,
        method: &str,
        url: &str,
    ) -> Result<FetchHandles, ServiceError> {
        fetch(
            self.env(),
            kind,
            method.as_bytes(),
            url,
            &[],
            RequestBody::None,
            30_000,
            false,
        )
    }
}

#[test]
fn a_request_the_policy_refuses_is_never_built() {
    let session = Session::new(policy(&["allowed.example"], true));
    let error = session
        .fetch("GET", "https://blocked.example/a")
        .expect_err("the allow list refuses it");
    assert!(
        error.message.contains("is not in the request allowlist"),
        "{}",
        error.message
    );
    assert!(
        session.resources.is_empty(),
        "a refused request leaves no handle behind"
    );

    let error = session
        .fetch("GET", "http://allowed.example/a")
        .expect_err("plain HTTP is not allowed");
    assert!(
        error.message.contains("plain-text http:// is not allowed"),
        "{}",
        error.message
    );
    assert!(session.resources.is_empty());
}

#[test]
fn a_built_request_answers_a_handle_and_the_one_that_aborts_it() {
    let session = Session::new(policy(&[], false));
    let handles = session.fetch("GET", "https://allowed.example/a").unwrap();
    let cancel = handles.cancel_handle_rid.expect("an HTTP request aborts");
    assert_ne!(handles.request_rid, cancel);
    assert_eq!(session.resources.len(), 2);

    // The abort is a close of that handle, and the send then answers that it
    // was cancelled rather than connecting.
    session.resources.close(cancel);
    let error = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fetch_send(
            &session.resources,
            &session.pools,
            handles.request_rid,
        ))
        .expect_err("an aborted request is not sent");
    assert_eq!(error.message, "request was cancelled");
}

#[test]
fn a_method_or_url_content_cannot_have_meant_is_a_type_error() {
    let session = Session::new(policy(&[], false));
    for (method, url, message) in [
        (
            "BAD METHOD",
            "https://allowed.example/",
            "Invalid HTTP method",
        ),
        ("GET", "not a url", "Invalid URL"),
        ("GET", "ftp://allowed.example/", "SchemeNotSupported"),
        ("GET", "blob:abc", "BlobNotFound"),
    ] {
        let error = session.fetch(method, url).expect_err(message);
        assert_eq!(error.class, "TypeError", "{message}");
        assert_eq!(error.message, message);
    }
    assert!(session.resources.is_empty());
}

/// A download is held to the download channel: a domain granted only for requests is refused to it, and the refusal
/// names the API and the channel. This is the service end of the kind the engine's JavaScript passes.
#[test]
fn a_download_is_held_to_the_download_channel_not_the_request_one() {
    let session = Session::new(policy(&["api.example.com"], true));
    session
        .fetch_as(FetchKind::Request, "GET", "https://api.example.com/a")
        .expect("the request channel grants it");
    let error = session
        .fetch_as(FetchKind::Download, "GET", "https://api.example.com/a")
        .expect_err("the download channel does not");
    assert_eq!(
        error.message,
        "downloadFile: domain 'api.example.com' is not in the download allowlist"
    );
    assert!(FetchKind::parse("upload").is_err());
}

/// A `data:` URL is answered without a connection -- and so without a cancel
/// handle, because there is nothing to abort.
#[test]
fn a_data_url_is_answered_from_its_own_bytes() {
    let session = Session::new(policy(&["allowed.example"], true));
    let handles = session
        .fetch("GET", "data:text/plain;base64,aGVsbG8=")
        .unwrap();
    assert_eq!(handles.cancel_handle_rid, None);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let answer = runtime
        .block_on(fetch_send(
            &session.resources,
            &session.pools,
            handles.request_rid,
        ))
        .unwrap();
    assert_eq!(answer.status, 200);
    assert_eq!(answer.status_text, "OK");
    assert!(
        answer
            .headers
            .iter()
            .any(|(name, value)| name == b"content-type" && value == b"text/plain"),
        "{:?}",
        answer.headers
    );

    let body = runtime
        .block_on(read(&session.resources, answer.response_rid, 64 * 1024))
        .unwrap();
    assert_eq!(&body[..], b"hello");
    let end = runtime
        .block_on(read(&session.resources, answer.response_rid, 64 * 1024))
        .unwrap();
    assert!(end.is_empty(), "an empty read is the end of the body");
}

/// Reads are bounded by what the caller asked for: content pulls a large body
/// through a 64 KiB buffer, and one read never answers more than it.
#[test]
fn a_read_answers_at_most_what_was_asked_for() {
    let session = Session::new(policy(&[], false));
    let handles = session
        .fetch("GET", "data:text/plain;base64,aGVsbG8gd29ybGQ=")
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let answer = runtime
        .block_on(fetch_send(
            &session.resources,
            &session.pools,
            handles.request_rid,
        ))
        .unwrap();
    let mut read_back = Vec::new();
    loop {
        let chunk = runtime
            .block_on(read(&session.resources, answer.response_rid, 4))
            .unwrap();
        assert!(chunk.len() <= 4);
        if chunk.is_empty() {
            break;
        }
        read_back.extend_from_slice(&chunk);
    }
    assert_eq!(read_back, b"hello world");
}

#[test]
fn a_send_happens_once_and_a_closed_body_reads_nothing() {
    let session = Session::new(policy(&[], false));
    let handles = session.fetch("GET", "data:text/plain,hi").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let answer = runtime
        .block_on(fetch_send(
            &session.resources,
            &session.pools,
            handles.request_rid,
        ))
        .unwrap();
    let again = runtime.block_on(fetch_send(
        &session.resources,
        &session.pools,
        handles.request_rid,
    ));
    assert!(
        again.unwrap_err().message.contains("is not open"),
        "a request is sent once"
    );

    session.resources.close(answer.response_rid);
    let error = runtime
        .block_on(read(&session.resources, answer.response_rid, 16))
        .expect_err("a closed body is not readable");
    assert!(error.message.contains("is not open"), "{}", error.message);
}

/// The headers a request may not carry, and the ones it gains.
#[test]
fn content_cannot_set_the_headers_that_would_forge_a_hop() {
    let headers = [
        (b"x-forwarded-for".to_vec(), b"10.0.0.1".to_vec()),
        (b"host".to_vec(), b"internal".to_vec()),
        (b"content-length".to_vec(), b"99".to_vec()),
        (b"range".to_vec(), b"bytes=0-9".to_vec()),
        (b"x-game".to_vec(), b"mine".to_vec()),
    ];
    let map = header_map(&headers, false, false).unwrap();
    assert!(!map.contains_key("x-forwarded-for"));
    assert!(!map.contains_key(HOST), "a forged Host is dropped");
    assert!(
        !map.contains_key(CONTENT_LENGTH),
        "the body's length is ours"
    );
    assert_eq!(map.get("x-game").unwrap(), "mine");
    assert_eq!(
        map.get(ACCEPT_ENCODING).unwrap(),
        "identity",
        "a range request must not be re-encoded whole"
    );
    assert_eq!(map.get(CACHE_CONTROL).unwrap(), "no-cache");
    assert_eq!(map.get(PRAGMA).unwrap(), "no-cache");

    // A client the host built to allow it keeps the Host header, and a cached
    // request gains no directives of ours.
    let map = header_map(&headers, true, true).unwrap();
    assert_eq!(map.get(HOST).unwrap(), "internal");
    assert!(!map.contains_key(CACHE_CONTROL));
    assert!(!map.contains_key(PRAGMA));
}

#[test]
fn a_header_that_is_not_one_is_refused_by_name() {
    let error = header_map(&[(b"not a header".to_vec(), b"x".to_vec())], false, false)
        .expect_err("the name is not a header name");
    assert_eq!(error.class, "TypeError");
    assert_eq!(error.message, "Invalid Header");
    let error = header_map(&[(b"x-ok".to_vec(), vec![0x7f])], false, false)
        .expect_err("the value is not a header value");
    assert_eq!(error.message, "Invalid Header Value");
}

/// A body larger than the session's whole byte budget is refused before the
/// response resource exists, so nothing is allocated and no reader is handed
/// out; and the credit a response holds comes back when it is dropped.
#[test]
fn a_declared_body_is_admitted_before_its_reader_exists() {
    let pools = IoPools::new(9501);
    let error = match reserve_response_bytes(&pools, Some(256 * 1024 * 1024 + 1)) {
        Ok(_) => panic!("an oversized response must be refused before its resource"),
        Err(error) => error,
    };
    assert!(
        error.message.contains("byte limit") && error.message.contains("requested"),
        "{}",
        error.message
    );

    let pools = IoPools::new(9502);
    let ticket = reserve_response_bytes(&pools, Some(4096)).unwrap();
    let response = Response::local(
        Url::parse("data:text/plain,body").unwrap(),
        http::Response::builder()
            .status(http::StatusCode::OK)
            .body(b"body".to_vec())
            .unwrap(),
    );
    let now = std::time::Instant::now();
    let queue = super::super::http::Span {
        start: now,
        end: now,
    };
    let body = ResponseBody::new(response, Some(4096), Some(ticket), now, queue, None);
    assert_eq!(body.size(), Some(4096));
    drop(body);
    reserve_response_bytes(&pools, Some(4096))
        .expect("dropping the response returns its byte credit");
}

/// A body with no declared length is admitted against the buffered ceiling the
/// engine's JavaScript enforces, not against nothing.
#[test]
fn a_body_that_declares_no_length_is_charged_the_buffered_ceiling() {
    let pools = IoPools::new(9503);
    let _held = reserve_response_bytes(&pools, None).expect("the first fits");
    assert_eq!(MAX_BUFFERED_BODY_BYTES, 32 * 1024 * 1024);
}

/// A listener on the loopback that answers one connection with `reply` and hands back the request head it read.
fn loopback_server(reply: &'static str) -> LoopbackServer {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().unwrap().port();
    let (sent, received) = std::sync::mpsc::channel();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        // A connection that never comes is the point of the refusing tests: the thread polls until
        // the test says it has looked.
        listener.set_nonblocking(true).expect("a pollable listener");
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(_) if !stopped.load(std::sync::atomic::Ordering::Relaxed) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).unwrap_or(0) == 0 {
                break;
            }
            head.push(byte[0]);
        }
        let _ = sent.send(String::from_utf8_lossy(&head).into_owned());
        let _ = stream.write_all(reply.as_bytes());
    });
    LoopbackServer {
        port,
        received,
        stop,
        handle,
    }
}

struct LoopbackServer {
    port: u16,
    received: std::sync::mpsc::Receiver<String>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl LoopbackServer {
    /// Whether a connection arrived within `wait`, and the head of its request.
    fn request_within(&self, wait: Duration) -> Option<String> {
        self.received.recv_timeout(wait).ok()
    }

    fn finish(self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.handle.join().unwrap();
    }
}

const OK_REPLY: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// A response the machine's proxy delivers is delivered.
///
/// The check that used to sit in `fetch_send` compared the address the response came from against the
/// blocked ranges. Through a proxy that address is the proxy's: 127.0.0.1 for a local one (Clash, a
/// corporate agent), a private address for a corporate one. With any proxy in the environment, every
/// request failed with "connection to 127.0.0.1 is not allowed" after the server had answered it.
#[test]
fn a_response_delivered_by_a_proxy_on_the_loopback_is_delivered() {
    let server = loopback_server(OK_REPLY);
    let proxy = hyper_util::client::proxy::matcher::Matcher::builder()
        .http(format!("http://127.0.0.1:{}", server.port))
        .build();
    let session = Session::configured(policy(&[], false), |builder| builder.proxies(proxy));

    let handles = session.fetch("GET", "http://public.example/a").unwrap();
    let answer = block_on(fetch_send(
        &session.resources,
        &session.pools,
        handles.request_rid,
    ))
    .expect("the proxy's answer is the answer");
    assert_eq!(answer.status, 200);

    // It went through the proxy: the proxy read an absolute-form request line, for a host the machine
    // never resolved.
    let head = server
        .request_within(Duration::from_secs(5))
        .expect("the proxy was asked");
    assert!(
        head.starts_with("GET http://public.example/a HTTP/1.1"),
        "{head}"
    );
    server.finish();
}

/// What the removed check guarded is still guarded, without it: a name that resolves to the loopback
/// is refused by the client's own resolver before any connection is made, and the listener behind it
/// never sees one.
#[test]
fn a_name_that_resolves_to_the_loopback_is_refused_before_it_connects() {
    let server = loopback_server(OK_REPLY);
    let session = Session::configured(policy(&[], false), |builder| builder.no_proxy());

    let handles = session
        .fetch("GET", &format!("http://localhost:{}/a", server.port))
        .unwrap();
    let error = block_on(fetch_send(
        &session.resources,
        &session.pools,
        handles.request_rid,
    ))
    .expect_err("a loopback destination is refused");
    assert!(
        error.message.contains("error sending request")
            || error
                .message
                .contains("not allowed (private/loopback address)"),
        "{}",
        error.message
    );
    assert!(
        server.request_within(Duration::from_millis(600)).is_none(),
        "the listener must never see a connection"
    );
    server.finish();
}

/// An IP literal never reaches the resolver (hyper bypasses it), so the gate refuses it when the
/// request is built, and the listener never sees a connection either.
#[test]
fn an_ip_literal_on_the_loopback_is_refused_when_the_request_is_built() {
    let server = loopback_server(OK_REPLY);
    let session = Session::configured(policy(&[], false), |builder| builder.no_proxy());
    let error = session
        .fetch("GET", &format!("http://127.0.0.1:{}/a", server.port))
        .expect_err("a loopback literal is refused");
    assert!(error.message.contains("not allowed"), "{}", error.message);
    assert!(server.request_within(Duration::from_millis(600)).is_none());
    server.finish();
}

/// Read a response to its end, then take its profile, as content does before it closes the response.
async fn profile_after_reading(session: &Session, url: &str) -> serde_json::Value {
    let handles = session.fetch("GET", url).unwrap();
    let answer = fetch_send(&session.resources, &session.pools, handles.request_rid)
        .await
        .expect("an answer");
    while !read(&session.resources, answer.response_rid, 64 * 1024)
        .await
        .unwrap()
        .is_empty()
    {}
    profile(&session.resources, answer.response_rid)
        .await
        .unwrap()
}

/// An answer that needed no connection: every phase at the fetch start, after the call; no peer and no round trip;
/// what was received counted.
#[test]
fn a_data_url_profile_has_every_phase_at_its_fetch_start() {
    let session = Session::new(policy(&[], false));
    let profile = block_on(profile_after_reading(&session, "data:text/plain,hello"));
    let fetch_start = profile["fetchStart"].as_u64().unwrap();
    assert!(profile["invokeStart"].as_u64().unwrap() <= fetch_start);
    for phase in [
        "queueStart",
        "queueEnd",
        "domainLookUpStart",
        "domainLookUpEnd",
        "connectStart",
        "connectEnd",
        "requestStart",
        "requestEnd",
        "responseStart",
        "responseEnd",
    ] {
        assert_eq!(
            profile[phase].as_u64(),
            Some(fetch_start),
            "{phase}: {profile}"
        );
    }
    for zero in [
        "SSLconnectionStart",
        "SSLconnectionEnd",
        "redirectStart",
        "redirectEnd",
        "sendBytesCount",
        "port",
    ] {
        assert_eq!(profile[zero].as_u64(), Some(0), "{zero}: {profile}");
    }
    assert_eq!(profile["rtt"].as_f64(), Some(0.0));
    assert_eq!(profile["peerIP"], "");
    assert_eq!(profile["protocol"], "unknown");
    assert_eq!(profile["socketReused"], false);
    assert_eq!(profile["usingHighPerformanceMode"], false);
    assert!(profile["receivedBytedCount"].as_u64().unwrap() >= 5);
}

/// Through a connection: the phases in the reference's order, the peer, the protocol, the bytes, and estimates the
/// response itself has fed.
#[test]
fn a_profile_through_a_connection_orders_its_phases() {
    let server = loopback_server(OK_REPLY);
    let proxy = hyper_util::client::proxy::matcher::Matcher::builder()
        .http(format!("http://127.0.0.1:{}", server.port))
        .build();
    let session = Session::configured(policy(&[], false), |builder| builder.proxies(proxy));
    let profile = block_on(profile_after_reading(&session, "http://public.example/a"));
    let at = |name: &str| {
        profile[name]
            .as_u64()
            .unwrap_or_else(|| panic!("{name}: {profile}"))
    };
    let order = [
        "invokeStart",
        "fetchStart",
        "connectStart",
        "connectEnd",
        "requestStart",
        "responseStart",
        "responseEnd",
    ];
    for pair in order.windows(2) {
        assert!(
            at(pair[0]) <= at(pair[1]),
            "{} after {}: {profile}",
            pair[0],
            pair[1]
        );
    }
    // Through a proxy nothing was looked up: the lookup is the fetch start.
    assert_eq!(at("domainLookUpStart"), at("fetchStart"));
    assert_eq!(at("SSLconnectionStart"), 0);
    assert_eq!(profile["peerIP"], "127.0.0.1");
    assert_eq!(at("port"), u64::from(server.port));
    assert_eq!(profile["protocol"], "http1.1");
    assert_eq!(profile["socketReused"], false);
    assert!(at("sendBytesCount") > 0 && at("receivedBytedCount") > 2);
    assert_ne!(
        profile["httpRttEstimate"].as_f64(),
        Some(-1.0),
        "the response is a sample: {profile}"
    );
    assert!((2..=5).contains(&at("estimate_nettype")));
    server.finish();
}

/// Ten requests run at once, as the reference allows; the eleventh waits its turn, starts when one of the ten is
/// closed, and reports the wait as its queue.
#[test]
fn the_eleventh_request_waits_for_one_of_ten_to_end() {
    let session = Session::new(policy(&[], false));
    block_on(async {
        let mut open = Vec::new();
        for _ in 0..super::super::resources::MAX_CONCURRENT_REQUESTS {
            let handles = session.fetch("GET", "data:text/plain,a").unwrap();
            let answer = fetch_send(&session.resources, &session.pools, handles.request_rid)
                .await
                .unwrap();
            open.push(answer.response_rid);
        }
        let eleventh = session.fetch("GET", "data:text/plain,b").unwrap();
        let waiting = fetch_send(&session.resources, &session.pools, eleventh.request_rid);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut waiting)
                .await
                .is_err(),
            "an eleventh request ran while ten were open"
        );
        session.resources.close(open[0]);
        let answer = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("it runs once one of the ten is closed")
            .unwrap();
        while !read(&session.resources, answer.response_rid, 1024)
            .await
            .unwrap()
            .is_empty()
        {}
        let profile = profile(&session.resources, answer.response_rid)
            .await
            .unwrap();
        let (start, end) = (
            profile["queueStart"].as_u64().unwrap(),
            profile["queueEnd"].as_u64().unwrap(),
        );
        assert!(end >= start + 100, "it waited at least 100 ms: {profile}");
        assert!(end <= profile["fetchStart"].as_u64().unwrap());
    });
}
