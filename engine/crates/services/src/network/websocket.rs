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

use futures::StreamExt;
use http::HeaderValue;
use http::header::HeaderName;
use tokio::sync::Mutex;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::debug;

use shared::network_policy::{NetworkChannel, NetworkPolicy};

use crate::ServiceError;

use super::gate::{self, GateKind};
use super::resources::{CancelFlag, ResourceId, ResourceTable};
use super::sockets::{BACKGROUND_THROTTLE, join_host_port};
use super::websocket_deflate;
use super::websocket_transport::{ReadStep, Receiver, Sender};

type WsTransport = MaybeTlsStream<tokio::net::TcpStream>;

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

#[derive(Clone, Copy, Debug)]
pub struct ConnectOptions {
    pub timeout_ms: Option<u32>,
    pub tcp_no_delay: bool,
    pub per_message_deflate: bool,
    /// A network cancel handle issued before starting the async connection.
    /// Closing it stops DNS/TCP/handshake waits before a socket id exists.
    pub cancel_rid: Option<ResourceId>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            timeout_ms: None,
            tcp_no_delay: true,
            per_message_deflate: false,
            cancel_rid: None,
        }
    }
}

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(WS_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(WS_MAX_FRAME_BYTES))
        .write_buffer_size(WS_WRITE_BUFFER_BYTES)
        .max_write_buffer_size(WS_MAX_WRITE_BUFFER_BYTES)
}

async fn finish_handshake<F, T, E>(
    handshake: F,
    secure: bool,
    slot: tokio::sync::OwnedSemaphorePermit,
    pools: migo_io::pools::IoPools,
    deadline: tokio::time::Instant,
) -> Result<(T, tokio::sync::OwnedSemaphorePermit), ServiceError>
where
    F: std::future::Future<Output = Result<T, E>> + Send + 'static,
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    // A native verifier can outlive the cancelled caller. Its connection must
    // continue to count against the quota until the offloaded poll returns.
    let guarded = async move {
        let result = handshake.await;
        (result, slot)
    };
    let handshake = async move {
        if secure {
            migo_io::polling::run(guarded, pools, migo_io::task::PoolKind::Tls, 1024 * 1024)
                .await
                .map_err(|error| generic(format!("WebSocket handshake executor failed: {error}")))
        } else {
            // Plain WS never waits behind native certificate verification.
            Ok(guarded.await)
        }
    };
    let (result, slot) = tokio::time::timeout_at(deadline, handshake)
        .await
        .map_err(|_| generic("WebSocket handshake timeout"))??;
    let value = result.map_err(|error| generic(format!("WebSocket handshake failed: {error}")))?;
    Ok((value, slot))
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
    tx: Mutex<Sender<WsTransport>>,
    rx: Mutex<Receiver<WsTransport>>,
    cancel: Arc<CancelFlag>,
    pools: migo_io::pools::IoPools,
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
    options: ConnectOptions,
    pools: migo_io::pools::IoPools,
) -> Result<WsHandshake, ServiceError> {
    let cancel = options
        .cancel_rid
        .map(|rid| {
            resources
                .cancel_flag(rid)
                .ok_or_else(|| generic("WebSocket connect cancel handle is not open"))
        })
        .transpose()?;
    let connecting = create_inner(policy, resources, url, protocols, headers, options, pools);
    if let Some(cancel) = cancel {
        cancel
            .until_cancelled(connecting)
            .await
            .ok_or_else(|| generic("WebSocket connect cancelled"))?
    } else {
        connecting.await
    }
}

async fn create_inner(
    policy: &NetworkPolicy,
    resources: &ResourceTable,
    url: &str,
    protocols: &[String],
    headers: &[(String, String)],
    options: ConnectOptions,
    pools: migo_io::pools::IoPools,
) -> Result<WsHandshake, ServiceError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let started = std::time::Instant::now();
    debug!("WebSocket connect: {url}");

    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_millis(u64::from(match options.timeout_ms {
            Some(ms) if ms > 0 => ms,
            _ => DEFAULT_CONNECT_TIMEOUT_MS,
        }));

    let parsed = super::url::Url::parse(url)
        .map_err(|error| type_error(format!("Invalid WebSocket URL: {error}")))?;
    gate::enforce(&parsed, policy, GateKind::WebSocket).map_err(ServiceError::generic)?;
    // RFC6455 section 3 also excludes an explicitly empty fragment.
    if parsed.fragment().is_some() {
        return Err(type_error("WebSocket URL must not contain a fragment"));
    }
    // The reference lets five WebSocket connections exist at once; one still
    // opening is one of them.
    let slot = resources.web_socket_slot().ok_or_else(|| {
        generic(format!(
            "at most {} WebSocket connections may be open at once",
            super::resources::MAX_WEB_SOCKETS
        ))
    })?;

    // Built from the URL the gate read, not content's string: two parsers given
    // the same text are two chances to disagree about its host.
    let mut request = parsed
        .as_str()
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
    let tls = if scheme == "wss" {
        let config = resources.web_socket_tls(policy.tls_trust, pools.clone());
        let config = tokio::time::timeout_at(deadline, config.config())
            .await
            .map_err(|_| generic("WebSocket TLS configuration timeout"))?
            .map_err(|error| generic(format!("WebSocket TLS configuration failed: {error}")))?;
        Some(tokio_tungstenite::Connector::Rustls(config))
    } else {
        None
    };
    let addresses = resolve(
        &host,
        port,
        gate::lan_access(policy, NetworkChannel::Socket),
        deadline,
    )
    .await?;

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
    if options.per_message_deflate {
        request_headers.insert(
            "Sec-WebSocket-Extensions",
            HeaderValue::from_static(websocket_deflate::OFFER),
        );
    }

    let stream = super::http::connect_vetted_tcp(addresses, &host, port, deadline)
        .await
        .map_err(|error| generic(format!("WebSocket TCP connect failed: {error}")))?;
    stream
        .set_nodelay(options.tcp_no_delay)
        .map_err(|error| generic(format!("WebSocket TCP_NODELAY failed: {error}")))?;

    let config = ws_config()
        .external_deflate(options.per_message_deflate)
        .external_deflate_max_wire_size(Some(websocket_deflate::encoded_limit(
            WS_MAX_MESSAGE_BYTES,
        )));
    let handshake =
        tokio_tungstenite::client_async_tls_with_config(request, stream, Some(config), tls);
    let ((stream, response), slot) =
        finish_handshake(handshake, scheme == "wss", slot, pools.clone(), deadline).await?;

    let values: Result<Vec<_>, _> = response
        .headers()
        .get_all("Sec-WebSocket-Extensions")
        .iter()
        .map(|value| value.to_str())
        .collect();
    let values = values.map_err(|_| generic("Invalid WebSocket extension response"))?;
    let parameters =
        websocket_deflate::negotiate(options.per_message_deflate, values.iter().copied())
            .map_err(|error| generic(error.to_string()))?;

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
    let slot = Arc::new(slot);
    let rid = resources.add_web_socket(Arc::new(WebSocketConn {
        pools: pools.clone(),
        tx: Mutex::new(Sender::new(tx, parameters, pools.clone(), slot.clone())),
        rx: Mutex::new(Receiver::new(rx, parameters, pools, slot)),
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
    lan: super::address_filter::LanAccess,
    deadline: tokio::time::Instant,
) -> Result<Vec<std::net::SocketAddr>, ServiceError> {
    let target = join_host_port(host, port);
    tokio::time::timeout_at(
        deadline,
        super::sockets::resolve_all(&target, |address| {
            super::address_filter::is_blocked_address(address, lan)
        }),
    )
    .await
    .map_err(|_| generic("WebSocket DNS resolve timeout"))?
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            generic(format!("WebSocket {error}"))
        } else {
            generic(format!("WebSocket DNS resolve failed: {error}"))
        }
    })
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
        let mut throttle = true;
        loop {
            if throttle && backgrounded.load(Ordering::Relaxed) {
                tokio::time::sleep(BACKGROUND_THROTTLE).await;
            }
            match rx.next().await? {
                ReadStep::Event(event) => return Ok(event),
                ReadStep::Control => {
                    throttle = true;
                    tokio::task::yield_now().await;
                }
                ReadStep::Fragment => throttle = false,
            }
        }
    };
    let result = cancel
        .until_cancelled(event)
        .await
        .ok_or_else(|| generic("WebSocket closed"))?;
    if result.is_err() {
        connection.cancel();
    }
    result
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
    if message.len() > WS_MAX_MESSAGE_BYTES {
        return Err(generic("WebSocket message exceeds 8 MiB"));
    }
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
    let deadline = tokio::time::Instant::now() + WRITE_TIMEOUT;
    // Admit queued payloads before they wait for the writer, and transfer the
    // ticket with an offloaded message if its caller abandons the wait.
    let payload = connection
        .pools
        .reserve_bytes(message.len() as u64)
        .map_err(|error| generic(format!("WebSocket send budget: {error}")))?;
    let written = async {
        match tokio::time::timeout_at(deadline, async {
            let mut sender = connection.tx.lock().await;
            let result = sender.send_admitted(message, deadline, payload).await;
            if result.is_err() && sender.is_closed() {
                connection.cancel();
            }
            result
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(generic(format!(
                "WebSocket {what} failed: {}",
                error.message
            ))),
            Err(_) => {
                connection.cancel();
                Err(generic(format!("WebSocket {what} timeout")))
            }
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
