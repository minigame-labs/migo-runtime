//! Content's TCP socket, as the host holds it.
//!
//! One connection is one resource in the session's table, and content drives it
//! with four calls: connect, take the next event, write, close. The policy a
//! raw socket is held to is the one `fetch` and the WebSocket are held to --
//! otherwise a game that cannot reach a host through `fetch()` would reach it
//! through `createTCPSocket()`, which is the same host by another name.
//!
//! The receive path is one allocation per socket, not per read: the scratch a
//! read fills is retained (`sockets::ReceiveScratch`) and each event copies
//! exactly the bytes that arrived.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use migo_io::pools::{ByteTicket, IoPools};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tracing::debug;

use shared::op_state::NetworkPolicy;

use crate::ServiceError;

use super::gate::{self, GateKind};
use super::resources::{CancelFlag, ResourceId, ResourceTable};
use super::sockets::{
    AddrMeta, BACKGROUND_THROTTLE, ReceiveScratch, checked_port, join_host_port, resolve_first,
};

/// The largest single read. 64 KiB balances the syscall against the scratch
/// each socket retains; not changed without measuring on a device.
const RECV_CAPACITY: usize = 65536;

/// The deadline for one write, including waiting for the writer: a stalled
/// reader must not hold the writer forever. A liveness bound, not a
/// performance claim.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The connect deadline when content named none, and the most it may name.
const DEFAULT_CONNECT_SECONDS: u32 = 2;
const MAX_CONNECT_SECONDS: u32 = 300;

/// The read half and the scratch it fills, together under one lock: two reads
/// serialize on it, and a cancelled read drops it.
struct Receive {
    reader: tokio::io::ReadHalf<TcpStream>,
    scratch: ReceiveScratch,
}

/// One open connection.
pub struct TcpConn {
    rx: Mutex<Receive>,
    tx: Mutex<tokio::io::WriteHalf<TcpStream>>,
    cancel: Arc<CancelFlag>,
    /// This connection's write deadline. Held per connection rather than read
    /// from the constant at each write so a test can stall a real socket
    /// without waiting out the production bound.
    write_timeout: Duration,
    local: AddrMeta,
    remote: AddrMeta,
}

impl TcpConn {
    /// Split a connected stream into the halves a session holds, and read both
    /// ends once: an event then clones an `Arc<str>` rather than formatting a
    /// `SocketAddr` again per message.
    fn over(stream: TcpStream, write_timeout: Duration, fallback_remote: SocketAddr) -> Self {
        let local_addr = stream
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let remote_addr = stream.peer_addr().unwrap_or(fallback_remote);
        let (reader, writer) = tokio::io::split(stream);
        Self {
            rx: Mutex::new(Receive {
                reader,
                scratch: ReceiveScratch::new(RECV_CAPACITY),
            }),
            tx: Mutex::new(writer),
            cancel: Arc::new(CancelFlag::default()),
            write_timeout,
            local: AddrMeta::new(&local_addr),
            remote: AddrMeta::new(&remote_addr),
        }
    }

    fn endpoints(&self) -> TcpEndpoints {
        TcpEndpoints {
            local: self.local.clone(),
            remote: self.remote.clone(),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Both ends of a connection, as content is told them.
#[derive(Clone, Debug)]
pub struct TcpEndpoints {
    pub local: AddrMeta,
    pub remote: AddrMeta,
}

/// What a connect answered.
#[derive(Clone, Debug)]
pub struct TcpConnected {
    pub rid: ResourceId,
    pub endpoints: TcpEndpoints,
}

/// One event from a connection.
#[derive(Clone, Debug)]
pub enum TcpEvent {
    /// Bytes from the peer, with both ends of the connection they came over.
    Message {
        data: Box<[u8]>,
        endpoints: TcpEndpoints,
    },
    Error(String),
    /// The peer closed, or this end did.
    Close,
}

/// Connect, and answer the handle content drives the socket with.
///
/// The policy is enforced before the name is resolved, and the resolver refuses
/// a name that answers with any address in a blocked range.
pub async fn connect(
    policy: &NetworkPolicy,
    resources: &ResourceTable,
    address: &str,
    port: u32,
    timeout_secs: u32,
) -> Result<TcpConnected, ServiceError> {
    let port = checked_port(port)?;
    let seconds = if timeout_secs == 0 {
        DEFAULT_CONNECT_SECONDS
    } else {
        timeout_secs.min(MAX_CONNECT_SECONDS)
    };
    debug!("TCP connect: {address}:{port} (timeout={seconds}s)");

    gate::enforce_host(address, port, policy, GateKind::TcpSocket)
        .map_err(ServiceError::generic)?;

    let deadline = Duration::from_secs(u64::from(seconds));
    let target = join_host_port(address, port);
    let resolved = tokio::time::timeout(deadline, resolve_first(&target))
        .await
        .map_err(|_| generic(format!("connect:fail timeout after {seconds}s")))?
        .map_err(|error| generic(format!("connect:fail resolve error: {error}")))?;

    let stream = tokio::time::timeout(deadline, TcpStream::connect(resolved))
        .await
        .map_err(|_| generic(format!("connect:fail timeout after {seconds}s")))?
        .map_err(|error| generic(format!("connect:fail {error}")))?;
    // Lower latency, which is what a game socket is for.
    let _ = stream.set_nodelay(true);

    let connection = TcpConn::over(stream, WRITE_TIMEOUT, resolved);
    let endpoints = connection.endpoints();
    let rid = resources.add_tcp(Arc::new(connection));
    debug!(
        "TCP connected, rid={rid}, local={}, remote={}",
        endpoints.local.address, endpoints.remote.address
    );

    Ok(TcpConnected { rid, endpoints })
}

/// The next event: bytes, a failure, or the close.
pub async fn next_event(
    resources: &ResourceTable,
    rid: ResourceId,
    backgrounded: &AtomicBool,
) -> Result<TcpEvent, ServiceError> {
    let connection = resources.tcp(rid)?;
    let cancel = Arc::clone(&connection.cancel);
    let event = async {
        if backgrounded.load(Ordering::Relaxed) {
            tokio::time::sleep(BACKGROUND_THROTTLE).await;
        }
        let mut rx = connection.rx.lock().await;
        let rx = &mut *rx;
        // The disjoint borrow lets the read fill the scratch while holding the
        // reader; both end with this block, so the copy below can run.
        let read = {
            let Receive { reader, scratch } = rx;
            reader.read(scratch.as_mut_slice()).await
        };
        match read {
            Ok(0) => TcpEvent::Close,
            // Exactly the filled prefix: no trailing bytes from a longer
            // earlier read reach content.
            Ok(count) => TcpEvent::Message {
                data: rx.scratch.copy_filled(count),
                endpoints: connection.endpoints(),
            },
            Err(error) => TcpEvent::Error(error.to_string()),
        }
    };
    cancel
        .until_cancelled(event)
        .await
        .ok_or_else(|| generic("TCPSocket closed"))
}

/// Write text or bytes. The bytes are charged to the session's budget before
/// the write, so a game cannot queue more than the host will hold.
pub async fn write(
    resources: &ResourceTable,
    pools: &IoPools,
    rid: ResourceId,
    text: Option<String>,
    bytes: Option<Vec<u8>>,
) -> Result<(), ServiceError> {
    let payload = match (text, bytes) {
        (Some(text), _) => text.into_bytes(),
        (None, Some(bytes)) => bytes,
        (None, None) => return Err(type_error("write:fail no data provided")),
    };
    let connection = resources.tcp(rid)?;
    let ticket = reserve_write_bytes(pools, payload.len())?;
    write_payload(&connection, payload, ticket).await
}

/// The write itself: the payload and its byte ticket are held by the future,
/// so a cancelled write releases both rather than pinning them behind a stalled
/// peer.
async fn write_payload(
    connection: &TcpConn,
    payload: Vec<u8>,
    ticket: ByteTicket,
) -> Result<(), ServiceError> {
    let cancel = Arc::clone(&connection.cancel);
    let written = async {
        let _held = ticket;
        match tokio::time::timeout(connection.write_timeout, async {
            connection.tx.lock().await.write_all(&payload).await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(generic(format!("write:fail {error}"))),
            Err(_) => Err(generic("write:fail deadline exceeded")),
        }
    };
    cancel
        .until_cancelled(written)
        .await
        .unwrap_or_else(|| Err(generic("TCPSocket closed")))
}

fn reserve_write_bytes(pools: &IoPools, count: usize) -> Result<ByteTicket, ServiceError> {
    pools
        .reserve_bytes(count as u64)
        .map_err(|error| generic(format!("write:fail byte limit: {error}")))
}

/// Close the socket. The read in flight is cancelled and both halves drop, so
/// the connection is shut down rather than left to a garbage collector.
pub fn close(resources: &ResourceTable, rid: ResourceId) -> Result<(), ServiceError> {
    // Named first, so closing something that is not a socket is the refusal the
    // embedded op gives rather than a silent no-op.
    resources.tcp(rid)?;
    resources.close(rid);
    Ok(())
}

fn generic(message: impl Into<String>) -> ServiceError {
    ServiceError::generic(message)
}

fn type_error(message: impl Into<String>) -> ServiceError {
    ServiceError::classed("TypeError", message)
}

#[cfg(test)]
#[path = "tcp_tests.rs"]
mod tests;
