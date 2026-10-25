//! The client against servers it runs itself: HTTP/1.1 and HTTP/2, over TCP and TLS, direct and through a proxy.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use http::{HeaderValue, Method, Request, StatusCode, Version};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper_util::client::proxy::matcher::Matcher;
use hyper_util::rt::{TokioExecutor, TokioIo};
use shared::network_policy::NetworkPolicy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use super::*;
use crate::network::gate::GateKind;

type ServerBody = BoxBody<Bytes, Infallible>;

fn text(status: StatusCode, body: impl Into<Bytes>) -> http::Response<ServerBody> {
    http::Response::builder()
        .status(status)
        .body(Full::new(body.into()).boxed())
        .unwrap()
}

fn redirect(status: StatusCode, location: &str) -> http::Response<ServerBody> {
    http::Response::builder()
        .status(status)
        .header("location", location)
        .body(Full::new(Bytes::new()).boxed())
        .unwrap()
}

/// What the test server answers, by path.
async fn route(request: Request<Incoming>) -> http::Response<ServerBody> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or("").to_string();
    let headers = request.headers().clone();
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let header = |name: &str| {
        headers
            .get(name)
            .map_or_else(String::new, |value| value.to_str().unwrap().to_string())
    };
    match path.as_str() {
        "/accept-encoding" => text(StatusCode::OK, header("accept-encoding")),
        path if path.starts_with("/encoded/") => encoded_reply(path).await,
        "/hello" => text(StatusCode::OK, "hello"),
        "/echo" => text(
            StatusCode::OK,
            format!("{method} {}", String::from_utf8_lossy(&body)),
        ),
        "/headers" => text(
            StatusCode::OK,
            format!(
                "authorization={} referer={} cookie={}",
                header("authorization"),
                header("referer"),
                header("cookie")
            ),
        ),
        "/upload" => text(
            StatusCode::OK,
            format!(
                "{}\n{}",
                header("content-type"),
                String::from_utf8_lossy(&body)
            ),
        ),
        "/same-host" => redirect(StatusCode::FOUND, "/hello"),
        "/twice" => redirect(StatusCode::MOVED_PERMANENTLY, "/same-host"),
        // To the same host on the port the query names: another origin.
        "/other-port" => redirect(
            StatusCode::FOUND,
            &format!(
                "http://localhost:{}/headers",
                query.strip_prefix("to=").unwrap_or("0")
            ),
        ),
        "/post-302" => redirect(StatusCode::FOUND, "/echo"),
        "/post-307" => redirect(StatusCode::TEMPORARY_REDIRECT, "/echo"),
        "/to-blocked" => redirect(StatusCode::FOUND, "http://blocked.example/x"),
        "/slow-head" => {
            tokio::time::sleep(Duration::from_secs(3)).await;
            text(StatusCode::OK, "late")
        }
        "/slow-body" => {
            let first = futures::stream::once(async {
                Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"first")))
            });
            let stalled = first.chain(futures::stream::pending());
            http::Response::new(BodyExt::boxed(StreamBody::new(stalled)))
        }
        _ => text(StatusCode::NOT_FOUND, "no such path"),
    }
}

async fn encode_body(coding: &str, input: &[u8]) -> Vec<u8> {
    use async_compression::tokio::bufread::{BrotliEncoder, GzipEncoder};
    let mut output = Vec::new();
    match coding {
        "gzip" => {
            GzipEncoder::new(input)
                .read_to_end(&mut output)
                .await
                .unwrap();
        }
        "br" => {
            BrotliEncoder::new(input)
                .read_to_end(&mut output)
                .await
                .unwrap();
        }
        _ => unreachable!(),
    }
    output
}

async fn encoded_reply(path: &str) -> http::Response<ServerBody> {
    let variant = path.strip_prefix("/encoded/").unwrap();
    let plain = if variant == "bomb" {
        vec![0; 4 * 1024 * 1024]
    } else {
        b"mini-game payload: abcdefghijklmnopqrstuvwxyz\n".repeat(2048)
    };
    let coding = match variant {
        "br" => "br",
        "mixed" | "repeated-headers" => "gzip, br",
        "unknown" => "future-codec",
        "too-many" => "gzip, gzip, gzip",
        _ => "gzip",
    };
    let mut encoded = if coding == "future-codec" {
        plain.clone()
    } else {
        encode_body("gzip", &plain).await
    };
    if variant == "br" {
        encoded = encode_body("br", &plain).await;
    }
    if matches!(variant, "mixed" | "repeated-headers") {
        encoded = encode_body("br", &encoded).await;
    }
    if variant == "members" {
        encoded.extend(encode_body("gzip", &plain).await);
    }
    if variant == "truncated" {
        encoded.truncate(encoded.len() - 7);
    }
    let mut response = text(
        if variant == "range" {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        },
        encoded,
    );
    response
        .headers_mut()
        .insert("content-encoding", HeaderValue::from_static(coding));
    if variant == "repeated-headers" {
        response
            .headers_mut()
            .insert("content-encoding", HeaderValue::from_static("gzip"));
        response
            .headers_mut()
            .append("content-encoding", HeaderValue::from_static("br"));
    }
    if variant == "range" {
        response
            .headers_mut()
            .insert("content-range", HeaderValue::from_static("bytes 0-99/999"));
    }
    if variant == "slow" {
        let first = futures::stream::once(async {
            Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"\x1f\x8b\x08\x00")))
        });
        let stalled = first.chain(futures::stream::pending());
        *response.body_mut() = BodyExt::boxed(StreamBody::new(stalled));
    }
    response
}

#[tokio::test]
async fn content_decoding_preserves_payload_and_measures_wire_separately_for_h1_and_h2() {
    for h2 in [false, true] {
        let (tls, root) = tls_pair(h2);
        let server = serve(Some(tls)).await;
        let client = HttpClientBuilder::new(
            "migo-test",
            h2,
            &policy(),
            "fetch",
            migo_io::pools::IoPools::new(0),
        )
        .no_proxy()
        .any_address()
        .add_root(root)
        .build()
        .unwrap();
        for coding in ["gzip", "br", "mixed", "repeated-headers", "members"] {
            let url = Url::parse(&format!(
                "https://localhost:{}/encoded/{coding}",
                server.port()
            ))
            .unwrap();
            let response = client.get(GateKind::Fetch, url).send().await.unwrap();
            assert_eq!(
                response.version(),
                if h2 {
                    Version::HTTP_2
                } else {
                    Version::HTTP_11
                }
            );
            assert!(!response.headers().contains_key("content-encoding"));
            assert!(!response.headers().contains_key("content-length"));
            assert_eq!(response.content_length(), None);
            let (body, response) = body_of(response).await;
            let expected = "mini-game payload: abcdefghijklmnopqrstuvwxyz\n"
                .repeat(if coding == "members" { 4096 } else { 2048 });
            assert_eq!(body, expected, "{coding}");
            assert_eq!(response.decoded_body_bytes(), expected.len() as u64);
            assert!(response.wire_body_bytes() < response.decoded_body_bytes());
            assert!(response.timing().bytes_received > response.wire_body_bytes());
            assert!(response.timing().bytes_received < response.decoded_body_bytes());
        }
    }
}

#[tokio::test]
async fn negotiation_range_unknown_and_bodyless_headers_keep_their_meaning() {
    let server = serve(None).await;
    let client = client(false);
    let fetch = |path| client.get(GateKind::Fetch, url(server, path));
    let response = fetch("/accept-encoding").send().await.unwrap();
    assert_eq!(body_of(response).await.0, "gzip, br");
    let response = fetch("/accept-encoding")
        .header(RANGE, HeaderValue::from_static("bytes=0-99"))
        .send()
        .await
        .unwrap();
    assert_eq!(body_of(response).await.0, "identity");
    let response = fetch("/accept-encoding")
        .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
        .send()
        .await
        .unwrap();
    assert_eq!(body_of(response).await.0, "identity");
    for path in ["/encoded/range", "/encoded/unknown"] {
        let mut response = fetch(path).send().await.unwrap();
        assert!(response.headers().contains_key("content-encoding"));
        assert!(response.content_length().is_some());
        let bytes = response.chunk().await.unwrap().unwrap();
        assert!(!bytes.is_empty());
    }
    // A server that ignores Range still cannot silently change resume coordinates.
    let mut response = fetch("/encoded/gzip")
        .header(RANGE, HeaderValue::from_static("bytes=0-99"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert_eq!(&response.chunk().await.unwrap().unwrap()[..2], b"\x1f\x8b");
    let mut response = client
        .request(GateKind::Fetch, Method::HEAD, url(server, "/encoded/gzip"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert!(response.chunk().await.unwrap().is_none());
}

#[tokio::test]
async fn bad_compression_fails_once_and_fuses_the_response_stream() {
    let server = serve(None).await;
    let client = client(false);
    for path in ["/encoded/truncated", "/encoded/bomb"] {
        let mut response = client
            .get(GateKind::Fetch, url(server, path))
            .send()
            .await
            .unwrap();
        let mut delivered = 0;
        loop {
            match response.chunk().await {
                Ok(Some(bytes)) => {
                    delivered += bytes.len();
                    assert!(delivered < 4 * 1024 * 1024);
                }
                Ok(None) => panic!("{path}: invalid compressed response was accepted"),
                Err(error) => {
                    assert_eq!(error.kind(), ErrorKind::Body);
                    break;
                }
            }
        }
        assert!(response.chunk().await.unwrap().is_none());
        assert!(
            response.timing().response_end.is_none(),
            "a failed body is not a completed network sample"
        );
    }
    let error = client
        .get(GateKind::Fetch, url(server, "/encoded/too-many"))
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("too many Content-Encoding"));
    let response = client
        .get(GateKind::Fetch, url(server, "/encoded/truncated"))
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let mut errors = 0;
    while let Some(next) = stream.next().await {
        if next.is_err() {
            errors += 1;
        }
        assert!(errors <= 1);
    }
    assert_eq!(errors, 1);
}

#[tokio::test]
async fn a_stalled_compressed_body_obeys_the_original_request_deadline() {
    let server = serve(None).await;
    let mut response = client(false)
        .get(GateKind::Fetch, url(server, "/encoded/slow"))
        .timeout(Duration::from_millis(100))
        .send()
        .await
        .unwrap();
    let error = response.chunk().await.unwrap_err();
    assert!(error.is_timeout(), "{error}");
    assert!(response.chunk().await.unwrap().is_none());
    assert!(response.timing().response_end.is_none());
}

/// A server on the loopback, HTTP/1.1 or HTTP/2 as the connection negotiates, over TLS when `tls` is given.
async fn serve(tls: Option<Arc<rustls::ServerConfig>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let own = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let tls = tls.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |request| async move {
                    Ok::<_, Infallible>(route(request).await)
                });
                let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                match tls {
                    Some(config) => {
                        let Ok(stream) =
                            tokio_rustls::TlsAcceptor::from(config).accept(socket).await
                        else {
                            return;
                        };
                        let _ = builder
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                    }
                    None => {
                        let _ = builder
                            .serve_connection(TokioIo::new(socket), service)
                            .await;
                    }
                }
            });
        }
    });
    own
}

/// A certificate authority-less TLS setup: a self-signed certificate for `localhost`, which the client trusts as a
/// root.
fn tls_pair(
    alpn_h2: bool,
) -> (
    Arc<rustls::ServerConfig>,
    rustls::pki_types::CertificateDer<'static>,
) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = certified.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(certified.key_pair.serialize_der().into());
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.clone()], key)
    .unwrap();
    config.alpn_protocols = if alpn_h2 {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    (Arc::new(config), cert)
}

/// Any domain over plain HTTP: these tests are about the client, and the client gates only the redirects it follows.
fn policy() -> NetworkPolicy {
    NetworkPolicy {
        allow_plain_http: true,
        ..NetworkPolicy::unrestricted()
    }
}

fn client(http2: bool) -> HttpClient {
    HttpClientBuilder::new(
        "migo-test",
        http2,
        &policy(),
        "fetch",
        migo_io::pools::IoPools::new(0),
    )
    .no_proxy()
    .any_address()
    .build()
    .unwrap()
}

fn url(address: SocketAddr, path: &str) -> Url {
    Url::parse(&format!("http://localhost:{}{path}", address.port())).unwrap()
}

async fn body_of(mut response: Response) -> (String, Response) {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        body.extend_from_slice(&chunk);
    }
    (String::from_utf8(body).unwrap(), response)
}

/// A first request pays for the lookup and the connection, and its phases are in order; a second request on the same
/// client reuses the connection, and says so.
#[tokio::test]
async fn a_request_measures_its_phases_and_a_second_reuses_the_connection() {
    let server = serve(None).await;
    let client = client(false);

    let response = client
        .get(GateKind::Fetch, url(server, "/hello"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(
        response.remote_addr().map(|peer| peer.port()),
        Some(server.port())
    );
    let (body, response) = body_of(response).await;
    assert_eq!(body, "hello");

    let timing = response.timing().clone();
    let trace = timing.connection.clone().expect("it came on a connection");
    let dns = trace.dns.expect("localhost was looked up");
    assert!(timing.fetch_start <= dns.start && dns.end <= trace.connect.start);
    assert!(trace.connect.end <= timing.request_start);
    assert!(trace.tls.is_none());
    assert!(!timing.socket_reused);
    assert!(timing.request_start <= timing.response_start);
    assert!(timing.response_start <= timing.response_end.expect("the body was read to its end"));
    assert!(
        timing.bytes_received > 5,
        "the head and the five bytes of the body"
    );
    assert!(timing.bytes_sent > 0);

    let again = client
        .get(GateKind::Fetch, url(server, "/hello"))
        .send()
        .await
        .unwrap();
    let (_, again) = body_of(again).await;
    assert!(again.timing().socket_reused);
    assert!(Arc::ptr_eq(
        again.timing().connection.as_ref().unwrap(),
        &trace
    ));
}

/// The production resolver refuses a name that answers the loopback, before connecting, and says why.
#[tokio::test]
async fn the_policy_resolver_refuses_the_loopback_before_connecting() {
    let server = serve(None).await;
    let client = HttpClientBuilder::new(
        "migo-test",
        false,
        &policy(),
        "fetch",
        migo_io::pools::IoPools::new(0),
    )
    .no_proxy()
    .build()
    .unwrap();
    let error = client
        .get(GateKind::Fetch, url(server, "/hello"))
        .send()
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.starts_with("error sending request for url"),
        "{message}"
    );
    // `localhost` answers ::1 and 127.0.0.1; whichever is refused first is named.
    assert!(
        message.contains("is not allowed (private/loopback address)"),
        "{message}"
    );
}

/// A same-host redirect is followed and timed; the response says where it came from.
#[tokio::test]
async fn same_host_redirects_are_followed_and_timed() {
    let server = serve(None).await;
    let response = client(false)
        .get(GateKind::Fetch, url(server, "/twice"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.url().path(), "/hello");
    let redirect = response.timing().redirect.expect("two same-host redirects");
    assert!(response.timing().fetch_start <= redirect.start && redirect.start <= redirect.end);
    assert!(redirect.end <= response.timing().response_start);
    assert_eq!(body_of(response).await.0, "hello");
}

/// A redirect to another origin -- here the same host on another port -- drops the credentials; the `Referer` names
/// the URL that redirected. The host is the same, so the redirect is timed.
#[tokio::test]
async fn a_cross_origin_redirect_drops_credentials() {
    let server = serve(None).await;
    let other = serve(None).await;
    let response = client(false)
        .get(
            GateKind::Fetch,
            url(server, &format!("/other-port?to={}", other.port())),
        )
        .header(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        )
        .header(http::header::COOKIE, HeaderValue::from_static("a=b"))
        .send()
        .await
        .unwrap();
    assert!(response.timing().redirect.is_some());
    let (body, _) = body_of(response).await;
    assert_eq!(
        body,
        format!(
            "authorization= referer=http://localhost:{}/other-port?to={} cookie=",
            server.port(),
            other.port()
        )
    );
}

/// 302 turns a POST into a GET without its body; 307 resends it as it was.
#[tokio::test]
async fn redirects_rewrite_the_method_as_the_status_says() {
    let server = serve(None).await;
    let client = client(false);
    let found = client
        .post(GateKind::Fetch, url(server, "/post-302"))
        .body(b"payload".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(body_of(found).await.0, "GET ");
    let temporary = client
        .post(GateKind::Fetch, url(server, "/post-307"))
        .body(b"payload".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(body_of(temporary).await.0, "POST payload");
}

/// A redirect the policy refuses is an error that says so, and nothing is sent to the refused host.
#[tokio::test]
async fn a_redirect_the_policy_refuses_is_an_error() {
    let server = serve(None).await;
    let allowed_only =
        NetworkPolicy::from_json(r#"{"request":["allowed.example"],"allowPlainHttp":true}"#)
            .unwrap();
    let client = HttpClientBuilder::new(
        "migo-test",
        false,
        &allowed_only,
        "fetch",
        migo_io::pools::IoPools::new(0),
    )
    .no_proxy()
    .any_address()
    .build()
    .unwrap();
    let error = client
        .get(GateKind::Fetch, url(server, "/to-blocked"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Redirect);
    assert!(
        error.to_string().contains(
            "fetch: redirect rejected: domain 'blocked.example' is not in the request allowlist"
        ),
        "{error}"
    );
}

/// One client, two requests to the same redirect: each hop is gated on the channel its request was made on, so a
/// download is held to the download allowlist even through the client a `fetch` also uses.
#[tokio::test]
async fn a_redirect_stays_on_the_channel_its_request_was_made_on() {
    let server = serve(None).await;
    let request_only =
        NetworkPolicy::from_json(r#"{"request":["blocked.example"],"allowPlainHttp":true}"#)
            .unwrap();
    let client = HttpClientBuilder::new(
        "migo-test",
        false,
        &request_only,
        "fetch",
        migo_io::pools::IoPools::new(0),
    )
    .no_proxy()
    .any_address()
    .build()
    .unwrap();
    let download = client
        .get(GateKind::Download, url(server, "/to-blocked"))
        .send()
        .await
        .unwrap_err();
    assert_eq!(download.kind(), ErrorKind::Redirect, "{download}");
    assert!(
        download.to_string().contains(
            "downloadFile: redirect rejected: domain 'blocked.example' is not in the download allowlist"
        ),
        "{download}"
    );
    // The request channel grants the hop, so the same redirect is followed -- and fails only at the network.
    let fetch = client
        .get(GateKind::Fetch, url(server, "/to-blocked"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap_err();
    assert_ne!(fetch.kind(), ErrorKind::Redirect, "{fetch}");
}

/// The timeout bounds the whole exchange: a head that does not come in time, and a body that stops coming.
#[tokio::test]
async fn the_timeout_bounds_the_head_and_the_body() {
    let server = serve(None).await;
    let client = client(false);
    let error = client
        .get(GateKind::Fetch, url(server, "/slow-head"))
        .timeout(Duration::from_millis(200))
        .send()
        .await
        .unwrap_err();
    assert!(error.is_timeout(), "{error}");

    let mut response = client
        .get(GateKind::Fetch, url(server, "/slow-body"))
        .timeout(Duration::from_millis(300))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.chunk().await.unwrap().as_deref(),
        Some(&b"first"[..])
    );
    assert!(response.chunk().await.unwrap_err().is_timeout());
}

/// Over TLS the handshake is its own phase, between the connection and the request; ALPN decides HTTP/2 where the
/// client offers it, and HTTP/1.1 where it does not.
#[tokio::test]
async fn tls_is_timed_and_alpn_chooses_the_protocol() {
    let (server_tls, root) = tls_pair(true);
    let server = serve(Some(server_tls)).await;
    let https = Url::parse(&format!("https://localhost:{}/hello", server.port())).unwrap();
    for (http2, expected) in [(true, Version::HTTP_2), (false, Version::HTTP_11)] {
        let client = HttpClientBuilder::new(
            "migo-test",
            http2,
            &policy(),
            "fetch",
            migo_io::pools::IoPools::new(0),
        )
        .no_proxy()
        .any_address()
        .add_root(root.clone())
        .build()
        .unwrap();
        let response = client
            .get(GateKind::Fetch, https.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.version(), expected);
        let trace = response.timing().connection.clone().unwrap();
        let tls = trace.tls.expect("a TLS connection has a handshake");
        assert!(trace.connect.end <= tls.start && tls.end <= response.timing().request_start);
        assert_eq!(trace.protocol, expected);
        assert_eq!(body_of(response).await.0, "hello");
    }
}

/// A certificate no root vouches for is refused.
#[tokio::test]
async fn an_untrusted_certificate_is_refused() {
    let (server_tls, _) = tls_pair(false);
    let server = serve(Some(server_tls)).await;
    let https = Url::parse(&format!("https://localhost:{}/hello", server.port())).unwrap();
    let error = client(false)
        .get(GateKind::Fetch, https)
        .send()
        .await
        .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("certificate"),
        "{error}"
    );
}

/// What a proxy on the loopback saw, and a stand-in for one: CONNECT is tunnelled to `upstream`; anything else is
/// answered with a fixed reply.
async fn proxy(upstream: SocketAddr) -> (SocketAddr, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let own = listener.local_addr().unwrap();
    let (seen, heads) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8(head).unwrap();
                let _ = seen.send(head.clone());
                if head.starts_with("CONNECT ") {
                    socket
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    let mut server = TcpStream::connect(upstream).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut server).await;
                } else {
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\nconnection: close\r\n\r\nproxied")
                        .await;
                }
            });
        }
    });
    (own, heads)
}

/// Plain HTTP goes to the proxy in absolute form, with the proxy's credentials, for a host the client never looks
/// up -- and through a proxy the policy resolver is not consulted, so a proxy on the loopback works.
#[tokio::test]
async fn plain_http_is_written_to_the_proxy_in_absolute_form() {
    let (proxy, mut heads) = proxy("127.0.0.1:9".parse().unwrap()).await;
    let matcher = Matcher::builder()
        .all(format!("http://user:pass@127.0.0.1:{}", proxy.port()))
        .build();
    let client = HttpClientBuilder::new(
        "migo-test",
        false,
        &policy(),
        "fetch",
        migo_io::pools::IoPools::new(0),
    )
    .proxies(matcher)
    .build()
    .unwrap();
    let response = client
        .get(
            GateKind::Fetch,
            Url::parse("http://public.example/a").unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.remote_addr(), Some(proxy));
    assert!(response.timing().connection.as_ref().unwrap().proxied);
    assert!(response.timing().connection.as_ref().unwrap().dns.is_none());
    assert_eq!(body_of(response).await.0, "proxied");
    let head = heads.recv().await.unwrap();
    assert!(
        head.starts_with("GET http://public.example/a HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.to_lowercase()
            .contains("proxy-authorization: basic dxnlcjpwyxnz"),
        "{head}"
    );
}

/// HTTPS is tunnelled: the proxy sees a CONNECT, and TLS runs end to end inside it.
#[tokio::test]
async fn https_is_tunnelled_through_the_proxy() {
    let (server_tls, root) = tls_pair(false);
    let server = serve(Some(server_tls)).await;
    let (proxy, mut heads) = proxy(server).await;
    let matcher = Matcher::builder()
        .all(format!("http://127.0.0.1:{}", proxy.port()))
        .build();
    let client = HttpClientBuilder::new(
        "migo-test",
        false,
        &policy(),
        "fetch",
        migo_io::pools::IoPools::new(0),
    )
    .proxies(matcher)
    .add_root(root)
    .build()
    .unwrap();
    let response = client
        .get(
            GateKind::Fetch,
            Url::parse(&format!("https://localhost:{}/hello", server.port())).unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert!(response.timing().connection.as_ref().unwrap().tls.is_some());
    assert_eq!(body_of(response).await.0, "hello");
    let head = heads.recv().await.unwrap();
    assert!(
        head.starts_with(&format!("CONNECT localhost:{} HTTP/1.1\r\n", server.port())),
        "{head}"
    );
}

/// A multipart form arrives as the server reads one: its boundary in the content type, its parts in order.
#[tokio::test]
async fn a_multipart_form_is_sent_with_its_boundary() {
    let server = serve(None).await;
    let file = futures::stream::iter([Ok(Bytes::from_static(b"ABC"))]);
    let form = Form::new()
        .part(
            "file",
            Part::stream(file, Some(3))
                .file_name("a.txt")
                .mime("text/plain"),
        )
        .text("k", "v");
    let response = client(false)
        .post(GateKind::Fetch, url(server, "/upload"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    let (body, response) = body_of(response).await;
    let (content_type, sent) = body.split_once('\n').unwrap();
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .expect(content_type);
    assert!(sent.starts_with(&format!("--{boundary}\r\n")), "{sent}");
    assert!(
        sent.contains("filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nABC\r\n"),
        "{sent}"
    );
    assert!(sent.ends_with(&format!("--{boundary}--\r\n")), "{sent}");
    assert!(response.timing().bytes_sent >= sent.len() as u64);
}

/// A local answer (a `data:` URL) has no connection and every phase at the fetch start.
#[tokio::test]
async fn a_local_answer_has_no_connection() {
    let mut response = Response::local(
        Url::parse("data:text/plain,hi").unwrap(),
        http::Response::new(b"hi".to_vec()),
    );
    assert_eq!(response.content_length(), Some(2));
    assert_eq!(response.chunk().await.unwrap().as_deref(), Some(&b"hi"[..]));
    assert_eq!(response.chunk().await.unwrap(), None);
    let timing = response.timing();
    assert!(timing.connection.is_none() && !timing.socket_reused);
    assert_eq!(timing.response_end, Some(timing.fetch_start));
}

/// Methods other than the common ones pass through as they are.
#[tokio::test]
async fn a_request_carries_its_method() {
    let server = serve(None).await;
    let response = client(false)
        .request(
            GateKind::Fetch,
            Method::from_bytes(b"PURGE").unwrap(),
            url(server, "/echo"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(body_of(response).await.0, "PURGE ");
}

/// A public HTTPS server, through the machine's own proxy settings if it has any: the Mozilla roots verify a real
/// chain, ALPN negotiates with a real server, and the production resolver allows a public address. Needs the
/// internet, so it runs only when asked (`--ignored`).
#[tokio::test]
#[ignore = "needs the internet"]
async fn a_public_https_server_answers() {
    for http2 in [true, false] {
        let client = HttpClientBuilder::new(
            "migo-test",
            http2,
            &policy(),
            "fetch",
            migo_io::pools::IoPools::new(0),
        )
        .build()
        .unwrap();
        let response = client
            .get(GateKind::Fetch, Url::parse("https://example.com/").unwrap())
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let trace = response.timing().connection.clone().unwrap();
        assert!(trace.tls.is_some());
        let expected = if http2 {
            Version::HTTP_2
        } else {
            Version::HTTP_11
        };
        assert_eq!(response.version(), expected);
        eprintln!(
            "http2={http2} peer={} proxied={} dns={:?} connect={:?} tls={:?} rtt={:?}",
            trace.peer,
            trace.proxied,
            trace.dns.map(|span| span.end - span.start),
            trace.connect.end - trace.connect.start,
            trace.tls.map(|span| span.end - span.start),
            trace.rtt()
        );
        assert!(body_of(response).await.0.contains("Example Domain"));
    }
}
