//! Content's WebSocket, as the host holds it.
//!
//! One connection is one resource in the session's table (`resources`), and
//! content drives it with four calls: connect, take the next event, send,
//! close. The embedded runtime wraps the id in a thin deno resource so its ops
//! find it in process; the external session's producer names the same id on the
//! service stream. The connection itself, its limits and its policy are here,
//! once, so a game reaches the same servers under the same rules whichever
//! execution it runs in.
//!
//! # Why an event is taken rather than delivered
//!
//! A WebSocket in the embedded runtime is a promise per event: the engine's
//! JavaScript awaits `op_ws_next_event` and awaits the next one when that
//! settles. Keeping that shape on the external lane costs nothing extra -- the
//! answer travels on the same stream a `fetch` body does -- and it keeps the
//! backpressure content already has: a game that stops asking stops being sent
//! messages, rather than filling a queue nobody drains.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use http::HeaderValue;
use http::header::HeaderName;
use tokio::sync::Mutex;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::debug;

use shared::op_state::NetworkPolicy;

use crate::ServiceError;

use super::gate::{self, GateKind};
use super::resources::{CancelFlag, ResourceId, ResourceTable};

type WsStream = tokio_tungstenite::WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Per-connection limits.
///
/// tungstenite's defaults are 64 MiB message / 16 MiB frame, tuned for desktop
/// servers. A mobile game runtime cannot reserve that much per connection, so
/// these are far lower; a game that needs large payloads fragments them itself.
pub const WS_MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub const WS_MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub const WS_WRITE_BUFFER_BYTES: usize = 128 * 1024;
pub const WS_MAX_WRITE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// How long a send or a close waits before it is given up on.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The connect deadline when content named none.
const DEFAULT_CONNECT_TIMEOUT_MS: u32 = 60_000;

/// Delay before each poll while the app is in the background: the socket stays
/// connected and delivery is deferred, rather than spinning a backgrounded game.
pub const BACKGROUND_THROTTLE: std::time::Duration = std::time::Duration::from_millis(500);

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(WS_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(WS_MAX_FRAME_BYTES))
        .write_buffer_size(WS_WRITE_BUFFER_BYTES)
        .max_write_buffer_size(WS_MAX_WRITE_BUFFER_BYTES)
}

/// Handshake-critical headers the client controls itself.
///
/// Content supplying these would corrupt the upgrade, or override the
/// subprotocol negotiated through the `protocols` argument, so they are
/// dropped. Names compare against `HeaderName::as_str()`, always lowercase.
fn is_reserved_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "upgrade"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-accept"
            | "sec-websocket-protocol"
            | "sec-websocket-extensions"
    )
}

/// One open connection.
///
/// The two halves are locked separately: a send must not wait behind the read
/// that is parked waiting for the server's next message, which is the normal
/// state of a connection content is listening on.
pub struct WebSocketConn {
    tx: Mutex<SplitSink<WsStream, Message>>,
    rx: Mutex<SplitStream<WsStream>>,
    cancel: Arc<CancelFlag>,
}

impl WebSocketConn {
    /// Cancel whatever is in flight. Called when the resource is closed.
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// What a connection answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsHandshake {
    pub rid: ResourceId,
    pub protocol: String,
    pub extensions: String,
}

/// One event from a connection, in the shape the engine's `WebSocket` facade
/// reads: a message carries either text or bytes, never both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsEvent {
    Text(String),
    Binary(Vec<u8>),
    Error(String),
    Close { code: u16, reason: String },
}

/// Connect, and answer the handle content drives the connection with.
///
/// The policy is enforced before anything network-visible happens -- before
/// DNS, before TCP, before TLS -- and every resolved address is checked, then
/// connected to directly: resolving once and connecting to the name again would
/// leave a window for the answer to change between the check and the connect.
pub async fn create(
    policy: &NetworkPolicy,
    resources: &ResourceTable,
    url: &str,
    protocols: &[String],
    headers: &[(String, String)],
    timeout_ms: Option<u32>,
) -> Result<WsHandshake, ServiceError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let started = std::time::Instant::now();
    debug!("WebSocket connect: {url}");

    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_millis(u64::from(match timeout_ms {
            Some(ms) if ms > 0 => ms,
            _ => DEFAULT_CONNECT_TIMEOUT_MS,
        }));

    let parsed = url::Url::parse(url)
        .map_err(|error| type_error(format!("Invalid WebSocket URL: {error}")))?;
    gate::enforce(&parsed, policy, GateKind::WebSocket).map_err(ServiceError::generic)?;

    // Post-gate: `into_client_request` re-validates the URL's shape, not the
    // policy.
    let mut request = url
        .into_client_request()
        .map_err(|error| generic(format!("Invalid WebSocket URL: {error}")))?;
    let scheme = request.uri().scheme_str().unwrap_or("").to_string();

    let Some(host) = request.uri().host().map(str::to_string) else {
        return Err(generic("WebSocket URL has no host"));
    };
    let port = request
        .uri()
        .port_u16()
        .unwrap_or(if scheme == "wss" { 443 } else { 80 });
    let addresses = resolve(&host, port, deadline).await?;

    // The same header filter `fetch` applies, so content cannot forge a `Host`
    // (vhost and routing bypass), a forwarding header (SSRF amplification, a
    // leaked credential) or the handshake's own headers. A blocked header is
    // dropped rather than refused, as it is for `fetch`.
    let request_headers = request.headers_mut();
    for (key, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(value),
        ) && name.as_str() != "host"
            && !super::fetch::is_blocked_header(&name)
            && !is_reserved_header(&name)
        {
            request_headers.insert(name, value);
        }
    }
    if !protocols.is_empty()
        && let Ok(value) = HeaderValue::from_str(&protocols.join(", "))
    {
        request_headers.insert("Sec-WebSocket-Protocol", value);
    }

    let stream = connect(&addresses, deadline).await?;
    let _ = stream.set_nodelay(true);

    let handshake =
        tokio_tungstenite::client_async_tls_with_config(request, stream, Some(ws_config()), None);
    let (stream, response) = tokio::time::timeout_at(deadline, handshake)
        .await
        .map_err(|_| generic("WebSocket handshake timeout"))?
        .map_err(|error| generic(format!("WebSocket handshake failed: {error}")))?;

    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let (protocol, extensions) = (
        header("Sec-WebSocket-Protocol"),
        header("Sec-WebSocket-Extensions"),
    );

    let (tx, rx) = stream.split();
    let rid = resources.add_web_socket(Arc::new(WebSocketConn {
        tx: Mutex::new(tx),
        rx: Mutex::new(rx),
        cancel: Arc::new(CancelFlag::default()),
    }));

    shared::stats::io_metrics_global()
        .record_op(shared::stats::OpClass::WsConnect, started.elapsed());
    debug!("WebSocket connected, rid={rid}");

    Ok(WsHandshake {
        rid,
        protocol,
        extensions,
    })
}

/// Every address the host resolves to, refused entirely if any one of them is
/// in a blocked range: a mixed public/private answer is how a name is used to
/// reach the inside of a network.
async fn resolve(
    host: &str,
    port: u16,
    deadline: tokio::time::Instant,
) -> Result<Vec<std::net::SocketAddr>, ServiceError> {
    let target = join_host_port(host, port);
    let addresses: Vec<std::net::SocketAddr> =
        tokio::time::timeout_at(deadline, tokio::net::lookup_host(&target))
            .await
            .map_err(|_| generic("WebSocket DNS resolve timeout"))?
            .map_err(|error| generic(format!("WebSocket DNS resolve failed: {error}")))?
            .collect();
    if addresses.is_empty() {
        return Err(generic("WebSocket DNS resolve returned no addresses"));
    }
    for address in &addresses {
        if super::address_filter::is_blocked_address(address) {
            return Err(generic(format!(
                "WebSocket connection to {} is not allowed (private/loopback address)",
                address.ip()
            )));
        }
    }
    Ok(addresses)
}

/// Connect to the first address that answers, within the deadline.
///
/// A dual-stack host may list a dead address first -- an unreachable IPv6 is
/// the common one -- and connecting only to the first would fail where a later
/// one works.
async fn connect(
    addresses: &[std::net::SocketAddr],
    deadline: tokio::time::Instant,
) -> Result<tokio::net::TcpStream, ServiceError> {
    let mut last = String::from("no address");
    for address in addresses {
        match tokio::time::timeout_at(deadline, tokio::net::TcpStream::connect(*address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last = error.to_string(),
            Err(_) => {
                last = "timeout".to_string();
                break;
            }
        }
    }
    Err(generic(format!("WebSocket TCP connect failed: {last}")))
}

/// `host:port` for a resolver, with a bare IPv6 literal bracketed: without the
/// brackets its own colons make the string ambiguous and it never resolves.
fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// The next event content has not seen yet.
///
/// Pings and pongs are the protocol's own and are answered by the stream, so
/// they are read past rather than handed to content. A connection that ended
/// without a close frame is the 1006 every WebSocket implementation reports.
pub async fn next_event(
    resources: &ResourceTable,
    rid: ResourceId,
    backgrounded: &AtomicBool,
) -> Result<WsEvent, ServiceError> {
    let connection = resources.web_socket(rid)?;
    let cancel = Arc::clone(&connection.cancel);
    let event = async {
        let mut rx = connection.rx.lock().await;
        loop {
            if backgrounded.load(Ordering::Relaxed) {
                tokio::time::sleep(BACKGROUND_THROTTLE).await;
            }
            match rx.next().await {
                Some(Ok(Message::Text(text))) => return WsEvent::Text(text.to_string()),
                // `Vec::<u8>::from(Bytes)` takes the allocation when the
                // `Bytes` is uniquely owned and full length, and copies
                // otherwise: opportunistic, not guaranteed.
                Some(Ok(Message::Binary(data))) => return WsEvent::Binary(Vec::from(data)),
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame
                        .map(|frame| (frame.code.into(), frame.reason.to_string()))
                        .unwrap_or((1005, String::new()));
                    return WsEvent::Close { code, reason };
                }
                Some(Err(error)) => return WsEvent::Error(error.to_string()),
                None => {
                    return WsEvent::Close {
                        code: 1006,
                        reason: String::new(),
                    };
                }
            }
        }
    };
    cancel
        .until_cancelled(event)
        .await
        .ok_or_else(|| generic("WebSocket closed"))
}

/// Send text or bytes. One of the two, as the facade's `send` passes one.
pub async fn send(
    resources: &ResourceTable,
    rid: ResourceId,
    text: Option<String>,
    bytes: Option<Vec<u8>>,
) -> Result<(), ServiceError> {
    let message = match (text, bytes) {
        (Some(text), _) => Message::Text(text.into()),
        (None, Some(bytes)) => Message::Binary(bytes.into()),
        (None, None) => return Err(type_error("No data provided")),
    };
    write(resources, rid, message, "send").await
}

/// Send the close frame. The resource stays until content closes it, so the
/// close's own answer still reaches the caller.
pub async fn close(
    resources: &ResourceTable,
    rid: ResourceId,
    code: u16,
    reason: String,
) -> Result<(), ServiceError> {
    let frame = tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: code.into(),
        reason: reason.into(),
    };
    write(resources, rid, Message::Close(Some(frame)), "close").await
}

async fn write(
    resources: &ResourceTable,
    rid: ResourceId,
    message: Message,
    what: &str,
) -> Result<(), ServiceError> {
    let connection = resources.web_socket(rid)?;
    let cancel = Arc::clone(&connection.cancel);
    let written = async {
        match tokio::time::timeout(WRITE_TIMEOUT, async {
            connection.tx.lock().await.send(message).await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(generic(format!("WebSocket {what} failed: {error}"))),
            Err(_) => Err(generic(format!("WebSocket {what} timeout"))),
        }
    };
    cancel
        .until_cancelled(written)
        .await
        .unwrap_or_else(|| Err(generic("WebSocket closed")))
}

fn generic(message: impl Into<String>) -> ServiceError {
    ServiceError::generic(message)
}

fn type_error(message: impl Into<String>) -> ServiceError {
    ServiceError::classed("TypeError", message)
}

#[cfg(test)]
#[path = "websocket_tests.rs"]
mod tests;
