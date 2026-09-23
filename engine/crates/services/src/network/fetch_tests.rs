use super::*;
use crate::network::gate::GateKind;

fn policy(whitelist: &[&str], enforce_https: bool) -> NetworkPolicy {
    NetworkPolicy {
        domain_whitelist: whitelist.iter().map(|host| (*host).to_string()).collect(),
        enforce_https,
    }
}

struct Session {
    policy: NetworkPolicy,
    client: Client,
    resources: ResourceTable,
    pools: IoPools,
}

impl Session {
    fn new(policy: NetworkPolicy) -> Self {
        let client = super::super::client::create_policy_http_client(
            "migo",
            false,
            &policy,
            GateKind::FetchRedirect,
            "fetch",
        )
        .expect("a client");
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
        fetch(
            self.env(),
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
        error.message.contains("is not in the allowed list"),
        "{}",
        error.message
    );
    assert!(
        session.resources.is_empty(),
        "a refused request leaves no handle behind"
    );

    let error = session
        .fetch("GET", "http://allowed.example/a")
        .expect_err("HTTPS is enforced");
    assert!(error.message.contains("HTTPS"), "{}", error.message);
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
    let response = Response::from(
        http::Response::builder()
            .status(http::StatusCode::OK)
            .body(reqwest::Body::from("body"))
            .unwrap(),
    );
    let body = ResponseBody::new(response, Some(4096), Some(ticket));
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
