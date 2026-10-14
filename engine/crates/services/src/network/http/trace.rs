//! What a request's journey measured: the connection it travelled on and the phases of the exchange.
//!
//! The mini-game reference answers a request with these as its `profile`: epoch milliseconds per phase, the peer,
//! whether the socket was reused, the protocol, the connection's RTT. Each is defined -- a reused connection's DNS and
//! connect times equal the fetch start, a plain connection's TLS times are zero -- so each is measured here or is not
//! reported; nothing is filled in.
//!
//! Phases are monotonic [`Instant`]s, turned into wall-clock milliseconds through one anchor taken once per process
//! ([`epoch_ms`]), so the phases of one request keep their order and spacing whatever the wall clock does meanwhile.

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use http::Version;

use super::tcp_info::SocketProbe;

/// `at` in milliseconds since the Unix epoch, as the reference reports a phase.
pub fn epoch_ms(at: Instant) -> f64 {
    static ANCHOR: OnceLock<(Instant, f64)> = OnceLock::new();
    let (anchor, wall_ms) = *ANCHOR.get_or_init(|| {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |since| since.as_secs_f64() * 1000.0);
        (Instant::now(), wall)
    });
    if at >= anchor {
        wall_ms + (at - anchor).as_secs_f64() * 1000.0
    } else {
        wall_ms - (anchor - at).as_secs_f64() * 1000.0
    }
}

/// One measured phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Instant,
    pub end: Instant,
}

impl Span {
    pub(crate) fn since(start: Instant) -> Self {
        Self {
            start,
            end: Instant::now(),
        }
    }
}

/// How a connection was made, and what it has carried since.
///
/// One per connection, shared by every response it carries (hyper hands each response the connection's extras).
#[derive(Debug)]
pub struct ConnectionTrace {
    /// The name lookup, or `None` where there was none: an IP-literal host, or a proxy that resolves it.
    pub dns: Option<Span>,
    /// TCP establishment -- through a proxy, to the proxy and through its tunnel.
    pub connect: Span,
    /// The TLS handshake, or `None` for a plain connection.
    pub tls: Option<Span>,
    /// Who the socket is connected to: the server, or the proxy.
    pub peer: SocketAddr,
    pub local: SocketAddr,
    pub proxied: bool,
    /// What TLS ALPN chose, or HTTP/1.1 for a plain connection.
    pub protocol: Version,
    served: AtomicU32,
    socket: SocketProbe,
}

impl ConnectionTrace {
    pub(crate) fn new(made: Established, socket: SocketProbe) -> Self {
        Self {
            dns: made.dns,
            connect: made.connect,
            tls: made.tls,
            peer: made.peer,
            local: made.local,
            proxied: made.proxied,
            protocol: made.protocol,
            served: AtomicU32::new(0),
            socket,
        }
    }

    /// When the connection was ready to carry a request: after TLS, or after TCP for a plain one.
    pub fn established(&self) -> Instant {
        self.tls.map_or(self.connect.end, |tls| tls.end)
    }

    /// Count one exchange on this connection; true for its first. The first exchange is the one that waited for the
    /// connection to be made; every later one reused it.
    pub(crate) fn claim(&self) -> bool {
        self.served.fetch_add(1, Ordering::Relaxed) == 0
    }

    /// The socket's smoothed round-trip time now, where the platform reports one and the socket is still open.
    pub fn rtt(&self) -> Option<Duration> {
        self.socket.rtt()
    }
}

/// How a connection was established: what the connector measured while making it.
pub(crate) struct Established {
    pub(crate) dns: Option<Span>,
    pub(crate) connect: Span,
    pub(crate) tls: Option<Span>,
    pub(crate) peer: SocketAddr,
    pub(crate) local: SocketAddr,
    pub(crate) proxied: bool,
    pub(crate) protocol: Version,
}

/// The phases of one exchange, from the call to the last byte of the body.
#[derive(Debug, Clone)]
pub struct RequestTiming {
    /// When the request was sent: before any redirect, connection or queue.
    pub fetch_start: Instant,
    /// From the first redirect response to the request that followed the last one, where every hop stayed on the
    /// request's host -- the reference counts only same-host redirects.
    pub redirect: Option<Span>,
    /// The connection the final response came on, if it came on one (a `data:` URL does not).
    pub connection: Option<std::sync::Arc<ConnectionTrace>>,
    /// Whether that connection existed before this request needed it.
    pub socket_reused: bool,
    /// When the final request began to be written: when its connection was ready, or at once on a reused one.
    pub request_start: Instant,
    /// When the final request's body was written in full.
    pub request_end: Instant,
    /// When the final response's head arrived.
    pub response_start: Instant,
    /// When its body ended, once it has.
    pub response_end: Option<Instant>,
    /// The final request's HTTP message: its head as serialized, plus its body.
    pub bytes_sent: u64,
    /// The final response's HTTP message: its head as serialized, plus the body read so far.
    pub bytes_received: u64,
    pub version: Version,
}

impl RequestTiming {
    /// The timing of an answer that needed no connection (a `data:` URL): every phase at the fetch start.
    pub fn local(at: Instant, bytes_received: u64) -> Self {
        Self {
            fetch_start: at,
            redirect: None,
            connection: None,
            socket_reused: false,
            request_start: at,
            request_end: at,
            response_start: at,
            response_end: Some(at),
            bytes_sent: 0,
            bytes_received,
            version: Version::HTTP_11,
        }
    }
}
