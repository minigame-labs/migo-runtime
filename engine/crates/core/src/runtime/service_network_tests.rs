//! The network ops as the producer calls them: arguments in wire shape,
//! answers in the shape `network.mjs` rebuilds the op's object from.
//!
//! A `data:` URL is the whole path without a connection -- build, send, read,
//! close -- so it is what these run, beside the refusals that must happen
//! before anything is built.

use super::*;

fn policy(whitelist: &[&str], enforce_https: bool) -> NetworkPolicy {
    NetworkPolicy {
        domain_whitelist: whitelist.iter().map(|host| (*host).to_string()).collect(),
        enforce_https,
    }
}

struct Session {
    network: NetworkBinding,
    scheduler: IoScheduler,
    executor: tokio::runtime::Runtime,
}

impl Session {
    fn new(policy: NetworkPolicy) -> Self {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread executor");
        Self {
            network: NetworkBinding::new(
                policy,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                executor.handle().clone(),
            ),
            scheduler: IoScheduler::new(7301),
            executor,
        }
    }

    /// `op_fetch`'s ten arguments, as the producer writes them for a GET with
    /// no body and no client of its own.
    fn fetch(&self, url: &str) -> Result<OwnedValue, ServiceError> {
        call_sync(
            &self.network,
            &self.scheduler,
            id::op_fetch,
            vec![
                OwnedValue::Bytes(b"GET".to_vec()),
                OwnedValue::Str(url.to_string()),
                OwnedValue::Array(Vec::new()),
                OwnedValue::Null,
                OwnedValue::Bool(false),
                OwnedValue::Null,
                OwnedValue::Null,
                OwnedValue::U32(30_000),
                OwnedValue::Bool(false),
                OwnedValue::Bool(false),
            ],
        )
    }

    fn call_async(&self, op: u32, args: Vec<OwnedValue>) -> Result<OwnedValue, ServiceError> {
        let future = call_async(&self.network, &self.scheduler, op, args)?;
        self.executor.block_on(future)
    }
}

fn array(value: &OwnedValue) -> &[OwnedValue] {
    match value {
        OwnedValue::Array(items) => items,
        other => panic!("expected an array, not {other:?}"),
    }
}

fn u32_at(value: &OwnedValue, index: usize) -> u32 {
    match &array(value)[index] {
        OwnedValue::U32(number) => *number,
        other => panic!("expected a u32 at {index}, not {other:?}"),
    }
}

/// The whole path: the handles, the head, the body in the chunks the caller
/// asked for, and the end of the body as an empty answer.
#[test]
fn a_data_url_is_built_sent_and_read_through_the_service() {
    let session = Session::new(policy(&["allowed.example"], true));
    let handles = session
        .fetch("data:text/plain;base64,aGVsbG8gd29ybGQ=")
        .expect("a data: URL needs no connection and no allow list");
    // `FetchReturn`: the request, and nothing to abort.
    assert_eq!(array(&handles).len(), 2);
    assert_eq!(array(&handles)[1], OwnedValue::Null);
    let request_rid = u32_at(&handles, 0);

    let answer = session
        .call_async(id::op_fetch_send, vec![OwnedValue::U32(request_rid)])
        .expect("the send answers");
    let fields = array(&answer);
    assert_eq!(fields.len(), 9, "FetchResponse has nine fields");
    assert_eq!(fields[0], OwnedValue::U32(200));
    assert_eq!(fields[1], OwnedValue::Str("OK".to_string()));
    assert_eq!(
        array(&fields[2])[0],
        OwnedValue::Array(vec![
            OwnedValue::Bytes(b"content-type".to_vec()),
            OwnedValue::Bytes(b"text/plain".to_vec()),
        ]),
        "a header crosses as the bytes ByteString would have made of it"
    );
    assert_eq!(fields[8], OwnedValue::Null, "no error");
    let response_rid = u32_at(&answer, 4);

    // Read as the engine's `ReadableStream` does: a bounded buffer, until the
    // answer is empty.
    let mut body = Vec::new();
    loop {
        let chunk = session
            .call_async(
                id::core_read,
                vec![OwnedValue::U32(response_rid), OwnedValue::U32(4)],
            )
            .expect("a read answers");
        match chunk {
            OwnedValue::Bytes(bytes) => {
                assert!(bytes.len() <= 4, "a read answers at most what was asked");
                if bytes.is_empty() {
                    break;
                }
                body.extend_from_slice(&bytes);
            }
            other => panic!("a read answers bytes, not {other:?}"),
        }
    }
    assert_eq!(body, b"hello world");

    command(&session.network, id::core_close, vec![OwnedValue::U32(response_rid)])
        .expect("closing an open handle");
    let error = session
        .call_async(
            id::core_read,
            vec![OwnedValue::U32(response_rid), OwnedValue::U32(4)],
        )
        .expect_err("a closed body is not readable");
    assert!(error.message.contains("is not open"), "{}", error.message);
}

/// The policy refuses before anything is built, and the producer's `fetch`
/// facade turns that throw into the failure content sees.
#[test]
fn a_host_the_policy_refuses_is_never_built() {
    let session = Session::new(policy(&["allowed.example"], true));
    let error = session
        .fetch("https://blocked.example/a")
        .expect_err("the allow list refuses it");
    assert_eq!(error.class, "Error");
    assert!(
        error.message.contains("blocked.example") && error.message.contains("allowed list"),
        "{}",
        error.message
    );
    assert!(
        session.network.resources().is_empty(),
        "a refused request leaves no handle behind"
    );
}

/// Aborting is a close of the cancel handle, and the send after it answers
/// that rather than connecting.
#[test]
fn closing_the_cancel_handle_stops_the_send() {
    let session = Session::new(policy(&[], false));
    let handles = session.fetch("https://allowed.example/slow").unwrap();
    let request_rid = u32_at(&handles, 0);
    let cancel_rid = u32_at(&handles, 1);

    command(&session.network, id::core_try_close, vec![OwnedValue::U32(cancel_rid)])
        .expect("abort() closes the cancel handle");
    let error = session
        .call_async(id::op_fetch_send, vec![OwnedValue::U32(request_rid)])
        .expect_err("an aborted request is not sent");
    assert_eq!(error.message, "request was cancelled");
}

/// `close` refuses a handle that names nothing, as deno's does; `tryClose` is
/// the form content calls when it does not care.
#[test]
fn the_two_closes_differ_on_a_handle_that_names_nothing() {
    let session = Session::new(policy(&[], false));
    let error = command(&session.network, id::core_close, vec![OwnedValue::U32(4242)])
        .expect_err("close names a handle that must be open");
    assert!(error.message.contains("4242"), "{}", error.message);
    command(&session.network, id::core_try_close, vec![OwnedValue::U32(4242)])
        .expect("tryClose says nothing about a handle that is gone");
}

/// The two arguments the engine's JavaScript never passes: a client of its own
/// and a body resource. Refused rather than ignored, so a producer that starts
/// passing one is told.
#[test]
fn a_client_or_a_body_resource_is_refused_rather_than_ignored() {
    let session = Session::new(policy(&[], false));
    for index in [3usize, 6] {
        let mut args = vec![
            OwnedValue::Bytes(b"GET".to_vec()),
            OwnedValue::Str("https://allowed.example/a".to_string()),
            OwnedValue::Array(Vec::new()),
            OwnedValue::Null,
            OwnedValue::Bool(false),
            OwnedValue::Null,
            OwnedValue::Null,
            OwnedValue::U32(30_000),
            OwnedValue::Bool(false),
            OwnedValue::Bool(false),
        ];
        args[index] = OwnedValue::U32(1);
        let error = call_sync(&session.network, &session.scheduler, id::op_fetch, args)
            .expect_err("this lane has neither");
        assert!(error.message.contains("op_fetch takes no"), "{}", error.message);
    }
}

/// A header list the producer could not have written is a `TypeError` naming
/// the op, as every other malformed call is.
#[test]
fn a_header_list_that_is_not_pairs_is_a_type_error() {
    let session = Session::new(policy(&[], false));
    let mut args = vec![
        OwnedValue::Bytes(b"GET".to_vec()),
        OwnedValue::Str("https://allowed.example/a".to_string()),
        OwnedValue::Array(vec![OwnedValue::Str("not-a-pair".to_string())]),
        OwnedValue::Null,
        OwnedValue::Bool(false),
        OwnedValue::Null,
        OwnedValue::Null,
        OwnedValue::U32(30_000),
        OwnedValue::Bool(false),
        OwnedValue::Bool(false),
    ];
    let error = call_sync(&session.network, &session.scheduler, id::op_fetch, args.clone())
        .expect_err("a header is a pair of byte strings");
    assert_eq!(error.class, "TypeError");
    assert!(error.message.contains("op_fetch"), "{}", error.message);

    args[2] = OwnedValue::Array(vec![OwnedValue::Array(vec![
        OwnedValue::Bytes(b"x-game".to_vec()),
        OwnedValue::Bytes(b"mine".to_vec()),
    ])]);
    call_sync(&session.network, &session.scheduler, id::op_fetch, args)
        .expect("a pair of byte strings is a header");
}

/// Every op this module claims is one it answers, and no op is claimed by two
/// of the three shapes -- the dispatch reads them in order, so an op in two
/// sets would be answered by whichever came first.
#[test]
fn every_claimed_op_is_answered_in_exactly_one_shape() {
    let claimed: Vec<u32> = super::super::service_ops::ALL
        .iter()
        .map(|(_, id)| *id)
        .filter(|op| is_sync(*op) || is_async(*op) || is_command(*op))
        .collect();
    assert_eq!(
        claimed.len(),
        20,
        "fetch's three, the three core members, the socket's four and the ten raw-socket ops"
    );
    for op in claimed {
        let shapes = u8::from(is_sync(op)) + u8::from(is_async(op)) + u8::from(is_command(op));
        assert_eq!(shapes, 1, "op {op} is claimed by {shapes} shapes");
    }
}

/// `prefetchDns` takes its hostnames as JSON, and a list that is not JSON is
/// the `TypeError` the embedded op throws.
#[test]
fn prefetch_dns_refuses_a_list_that_is_not_json() {
    let session = Session::new(policy(&["allowed.example"], true));
    let error = command(
        &session.network,
        id::op_prefetch_dns,
        vec![OwnedValue::Str("not json".to_string())],
    )
    .expect_err("the hostnames are a JSON array");
    assert_eq!(error.class, "TypeError");
    assert!(
        error.message.starts_with("prefetchDns: invalid JSON"),
        "{}",
        error.message
    );
}

/// A UDP socket binds, is refused a destination the policy does not allow, and
/// is closed -- the three shapes that need no peer.
#[test]
fn a_udp_socket_binds_and_is_held_to_the_policy() {
    let session = Session::new(policy(&["allowed.example"], false));
    let bound = call_sync(
        &session.network,
        &session.scheduler,
        id::op_udp_bind,
        vec![OwnedValue::U32(0), OwnedValue::Str("udp4".to_string())],
    )
    .expect("a system-assigned port");
    let fields = array(&bound);
    assert_eq!(fields.len(), 4, "UdpBindResult has four fields");
    assert_ne!(u32_at(&bound, 1), 0, "the system assigned a port");
    assert_eq!(fields[3], OwnedValue::Str("IPv4".to_string()));
    let rid = u32_at(&bound, 0);

    let error = session
        .call_async(
            id::op_udp_send,
            vec![
                OwnedValue::U32(rid),
                OwnedValue::Str("blocked.example".to_string()),
                OwnedValue::U32(9000),
                OwnedValue::Str("hi".to_string()),
                OwnedValue::Null,
                OwnedValue::U32(0),
                OwnedValue::U32(0),
                OwnedValue::Bool(false),
            ],
        )
        .expect_err("the allow list refuses it");
    assert!(
        error.message.contains("blocked.example"),
        "{}",
        error.message
    );

    command(&session.network, id::op_udp_set_ttl, vec![OwnedValue::U32(rid), OwnedValue::U32(8)])
        .expect("the TTL is the socket's to set");
    command(&session.network, id::op_udp_close, vec![OwnedValue::U32(rid)])
        .expect("closing an open socket");
    let error = command(&session.network, id::op_udp_close, vec![OwnedValue::U32(rid)])
        .expect_err("a closed socket names nothing");
    assert!(error.message.contains("is not open"), "{}", error.message);
}

/// A TCP connect is held to the same policy, and a handle that is not a socket
/// is refused by name rather than misread.
#[test]
fn a_tcp_connect_is_held_to_the_policy() {
    let session = Session::new(policy(&["allowed.example"], false));
    let error = session
        .call_async(
            id::op_tcp_connect,
            vec![
                OwnedValue::Str("blocked.example".to_string()),
                OwnedValue::U32(443),
                OwnedValue::U32(1),
            ],
        )
        .expect_err("the allow list refuses it");
    assert!(
        error.message.contains("blocked.example"),
        "{}",
        error.message
    );

    let bound = call_sync(
        &session.network,
        &session.scheduler,
        id::op_udp_bind,
        vec![OwnedValue::U32(0), OwnedValue::Str("udp4".to_string())],
    )
    .expect("a UDP socket");
    let error = command(
        &session.network,
        id::op_tcp_close,
        vec![OwnedValue::U32(u32_at(&bound, 0))],
    )
    .expect_err("a UDP socket is not a TCP one");
    assert!(
        error.message.contains("not a TCP socket"),
        "{}",
        error.message
    );
}

/// An `http(s)://` image source is fetched by the session's own client, under
/// the policy every other request is held to -- and a host the policy refuses
/// is refused here as it is for `fetch`.
#[tokio::test]
async fn an_image_source_is_held_to_the_policy_fetch_is_held_to() {
    let executor = tokio::runtime::Handle::current();
    let network = NetworkBinding::new(
        policy(&["allowed.example"], true),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        executor,
    );
    let (policy, client) = network.image_client().expect("the session has a client");
    let error =
        migo_services::network::image_source::fetch_http_image(&policy, &client, "https://blocked.example/a.png")
            .await
            .expect_err("the allow list refuses it");
    assert_eq!(error.code, shared::error::ErrorCode::PermissionDenied);
    assert!(
        error
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("blocked.example"),
        "{error:?}"
    );

    // And a URL that is not one never reaches the client.
    let error = migo_services::network::image_source::fetch_http_image(&policy, &client, "not a url")
        .await
        .expect_err("a URL is a URL");
    assert_eq!(error.code, shared::error::ErrorCode::InvalidArgument);
}

/// The socket events, as the host writes them and the producer rebuilds them.
///
/// A socket cannot ride the record-and-replay harness the fetch calls do: the
/// replay would need a server, and every address a test server could listen on
/// is one the address filter refuses -- which is the filter working. So the one
/// thing the two halves must agree about, the event's shape, is pinned by a
/// fixture this writes and `test/socket-events.test.mjs` reads.
///
/// Regenerate with `MIGO_WS_EVENTS_BLESS=1 cargo test -p migo-core
/// --no-default-features --features external-frames the_producer_s_socket_events`.
#[test]
fn the_producer_s_socket_events_are_the_ones_this_writes() {
    let cases = [
        ("text", WsEvent::Text("hello".to_string())),
        ("binary", WsEvent::Binary(vec![0, 127, 255])),
        ("error", WsEvent::Error("connection reset".to_string())),
        (
            "close",
            WsEvent::Close {
                code: 1006,
                reason: String::new(),
            },
        ),
        (
            "close with a reason",
            WsEvent::Close {
                code: 1000,
                reason: "bye".to_string(),
            },
        ),
    ];
    let written: Vec<serde_json::Value> = cases
        .into_iter()
        .map(|(name, event)| {
            serde_json::json!({ "name": name, "wire": json_of(&ws_event(event)) })
        })
        .collect();

    let peer = AddrMeta {
        address: std::sync::Arc::from("93.184.216.34"),
        family: "IPv4",
        port: 9000,
    };
    let here = AddrMeta {
        address: std::sync::Arc::from("10.0.0.2"),
        family: "IPv4",
        port: 51000,
    };
    let endpoints = migo_services::network::tcp::TcpEndpoints {
        local: here.clone(),
        remote: peer.clone(),
    };
    let tcp_cases = [
        (
            "message",
            TcpEvent::Message {
                data: Box::from(&b"pong"[..]),
                endpoints: endpoints.clone(),
            },
        ),
        ("error", TcpEvent::Error("connection reset".to_string())),
        ("close", TcpEvent::Close),
    ];
    let tcp_written: Vec<serde_json::Value> = tcp_cases
        .into_iter()
        .map(|(name, event)| {
            serde_json::json!({ "name": name, "wire": json_of(&tcp_event(event)) })
        })
        .collect();

    let udp_cases = [
        (
            "message",
            UdpEvent::Message {
                data: Box::from(&b"ping"[..]),
                remote: peer,
                local: here,
            },
        ),
        ("error", UdpEvent::Error("no route to host".to_string())),
    ];
    let udp_written: Vec<serde_json::Value> = udp_cases
        .into_iter()
        .map(|(name, event)| {
            serde_json::json!({ "name": name, "wire": json_of(&udp_event(event)) })
        })
        .collect();

    let answers = serde_json::json!({
        "events": written,
        "tcp": tcp_written,
        "udp": udp_written,
    });

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../platforms/apple/WebContent/PerformancePlus/test/fixtures/ws-event-answers.json");
    let rendered = serde_json::to_string_pretty(&answers).expect("JSON") + "\n";
    if std::env::var_os("MIGO_WS_EVENTS_BLESS").is_some() {
        std::fs::write(&path, &rendered).expect("write the fixture");
        return;
    }
    let checked_in = std::fs::read_to_string(&path).expect("the fixture is checked in");
    assert_eq!(
        checked_in, rendered,
        "the socket events changed; regenerate with MIGO_WS_EVENTS_BLESS=1"
    );
}

/// An `OwnedValue` as JSON, for the fixture: the shapes a socket event uses.
fn json_of(value: &OwnedValue) -> serde_json::Value {
    match value {
        OwnedValue::U32(number) => serde_json::json!(number),
        OwnedValue::Str(text) => serde_json::json!(text),
        OwnedValue::Bytes(bytes) => serde_json::json!(bytes),
        OwnedValue::Array(items) => {
            serde_json::Value::Array(items.iter().map(json_of).collect())
        }
        other => panic!("a socket event carries no {other:?}"),
    }
}
