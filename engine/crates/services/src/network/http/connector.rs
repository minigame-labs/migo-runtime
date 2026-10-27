//! How the engine makes a connection: the name looked up through the address policy, the TCP connection, the TLS
//! handshake -- each timed -- and, through a proxy, the tunnel.
//!
//! It is the engine's own rather than an HTTP library's because what a connection is made of is what a request's
//! profile reports: the boundary between TCP and TLS, the connection a response actually came on, the socket to ask for
//! its RTT. A library's connector keeps those to itself.
//!
//! Pooling, HTTP/1.1 and HTTP/2 are hyper's (`hyper_util`'s legacy client, which reqwest was built on). The connection
//! hands hyper a [`ConnectionTrace`] as its extra, and hyper gives that to every response the connection carries.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use http::{Uri, Version};
use hyper_util::client::legacy::connect::dns::Name;
use hyper_util::client::legacy::connect::proxy::Tunnel;
use hyper_util::client::legacy::connect::{Connected, Connection, HttpConnector};
use hyper_util::client::proxy::matcher::{Intercept, Matcher};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use super::tcp_info::{self, SocketGuard};
use super::trace::{ConnectionTrace, Established, Span};

pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// TCP + TLS establishment bound: mobile networks stall at connect time behind captive portals, so this is capped on
/// its own rather than left to the operating system's SYN retry window. The request's own timeout still bounds the
/// whole exchange.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// What the resolver may answer.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AddressRule {
    /// Only publicly reachable unicast addresses ([`crate::network::address_filter`]), never the local network: every
    /// channel this client carries is one `gate::lan_access` refuses it to. `operation` names the caller in the
    /// refusal.
    Public { operation: &'static str },
    /// Anything, the loopback included: for tests that serve on it, never in a build that ships.
    #[cfg(test)]
    Any,
}

impl AddressRule {
    /// Why `address` may not be connected to, if it may not.
    fn refusal(self, address: &SocketAddr) -> Option<String> {
        match self {
            Self::Public { operation } => crate::network::address_filter::is_blocked_address(
                address,
                crate::network::address_filter::LanAccess::Refused,
            )
            .then(|| {
                format!(
                    "{operation}: connection to {} is not allowed (private/loopback address)",
                    address.ip()
                )
            }),
            #[cfg(test)]
            Self::Any => None,
        }
    }
}

pub(crate) struct ConnectorConfig {
    pub(crate) rule: AddressRule,
    pub(crate) tls: crate::network::tls::ClientTls,
    pub(crate) proxies: Arc<Matcher>,
}

#[derive(Clone)]
pub(crate) struct Connector {
    inner: Arc<ConnectorConfig>,
}

impl Connector {
    pub(crate) fn new(config: ConnectorConfig) -> Self {
        Self {
            inner: Arc::new(config),
        }
    }
}

impl tower_service::Service<Uri> for Connector {
    type Response = Conn;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Conn, BoxError>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let config = Arc::clone(&self.inner);
        Box::pin(async move { connect(&config, dst).await })
    }
}

async fn connect(config: &ConnectorConfig, dst: Uri) -> Result<Conn, BoxError> {
    let https = match dst.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        other => return Err(format!("unsupported scheme {other:?}").into()),
    };
    let host = dst.host().ok_or("URL has no host")?.to_string();
    let bare_host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = dst.port_u16().unwrap_or(if https { 443 } else { 80 });
    let tls_config = if https {
        Some(config.tls.config().await?)
    } else {
        None
    };

    let (tcp, dns, connect_span, proxied) = match config.proxies.intercept(&dst) {
        Some(proxy) => {
            let (tcp, span) = through_proxy(&proxy, &dst, https).await?;
            (tcp, None, span, true)
        }
        None => {
            let (addresses, dns) = resolve(config.rule, &bare_host, port).await?;
            let started = Instant::now();
            let tcp = tcp_connector(StaticAddresses(Arc::new(addresses)))
                .call_async_uri(dst.clone())
                .await?;
            (tcp, dns, Span::since(started), false)
        }
    };

    let peer = tcp.peer_addr()?;
    let local = tcp.local_addr()?;
    let (probe, guard) = tcp_info::watch(&tcp);

    if !https {
        let made = Established {
            dns,
            connect: connect_span,
            tls: None,
            peer,
            local,
            proxied,
            protocol: Version::HTTP_11,
        };
        let trace = ConnectionTrace::new(made, probe);
        return Ok(Conn::new(guard, Box::new(tcp), trace, proxied));
    }

    let started = Instant::now();
    let server_name = ServerName::try_from(bare_host)
        .map_err(|error| format!("invalid TLS server name: {error}"))?;
    let handshake = TlsConnector::from(tls_config.ok_or("missing HTTPS configuration")?)
        .connect(server_name, tcp);
    // Platform certificate APIs may do blocking filesystem/JVM/OCSP work.
    // Only one handshake poll runs per admitted job; socket waits free it.
    let handshake = migo_io::polling::run(
        handshake,
        config.tls.pools(),
        migo_io::task::PoolKind::Tls,
        1024 * 1024,
    );
    let connected = tokio::time::timeout(CONNECT_TIMEOUT, handshake)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
    let tls = connected?;
    let tls_span = Span::since(started);
    let protocol = match tls.get_ref().1.alpn_protocol() {
        Some(b"h2") => Version::HTTP_2,
        _ => Version::HTTP_11,
    };
    let made = Established {
        dns,
        connect: connect_span,
        tls: Some(tls_span),
        peer,
        local,
        proxied,
        protocol,
    };
    let trace = ConnectionTrace::new(made, probe);
    // Through a proxy, an HTTPS request travels a tunnel the server's TLS ends at, so it is written in origin form
    // like a direct one; only plain HTTP through a proxy is written in absolute form.
    Ok(Conn::new(guard, Box::new(tls), trace, false))
}

/// The addresses `host` resolves to, each one allowed by `rule`, and how long the lookup took -- `None` for an IP
/// literal, which is not looked up. An IP literal is the gate's to refuse, before a request is built.
async fn resolve(
    rule: AddressRule,
    host: &str,
    port: u16,
) -> Result<(Vec<SocketAddr>, Option<Span>), BoxError> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok((vec![SocketAddr::new(ip, port)], None));
    }
    let started = Instant::now();
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    let span = Span::since(started);
    // Every address, not the first: a name that answers one public and one private address must not be connected
    // to at all, or happy eyeballs could pick the private one.
    if let Some(refusal) = addresses.iter().find_map(|address| rule.refusal(address)) {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::PermissionDenied,
            refusal,
        )));
    }
    if addresses.is_empty() {
        return Err(format!("{host} resolved to no address").into());
    }
    Ok((addresses, Some(span)))
}

/// The TCP connector every connection is made with: happy eyeballs across the resolved addresses, TCP_NODELAY, keep
/// alive probing, the connect timeout.
fn tcp_connector<R>(resolver: R) -> HttpConnector<R> {
    let mut tcp = base_tcp_connector(resolver, CONNECT_TIMEOUT);
    tcp.set_keepalive(Some(Duration::from_secs(15)));
    tcp.set_keepalive_interval(Some(Duration::from_secs(15)));
    tcp.set_keepalive_retries(Some(3));
    #[cfg(any(target_os = "android", target_os = "linux"))]
    tcp.set_tcp_user_timeout(Some(Duration::from_secs(30)));
    tcp
}

fn base_tcp_connector<R>(resolver: R, timeout: Duration) -> HttpConnector<R> {
    let mut tcp = HttpConnector::new_with_resolver(resolver);
    tcp.enforce_http(false);
    tcp.set_nodelay(true);
    tcp.set_connect_timeout(Some(timeout));
    tcp
}

/// Reuse hyper-util's happy-eyeballs driver after the caller has checked
/// every DNS answer. The static resolver cannot resolve the name again.
/// WSS keeps its original deadline and does not inherit HTTP's idle probes.
pub(in crate::network) async fn connect_vetted_tcp(
    addresses: Vec<SocketAddr>,
    host: &str,
    port: u16,
    deadline: tokio::time::Instant,
) -> Result<TcpStream, BoxError> {
    let remaining = deadline
        .checked_duration_since(tokio::time::Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "TCP connect deadline expired"))?;
    if addresses.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no vetted TCP addresses").into());
    }
    let target = Uri::builder()
        .scheme("http")
        .authority(crate::network::sockets::join_host_port(host, port))
        .path_and_query("/")
        .build()?;
    let connecting =
        base_tcp_connector(StaticAddresses(Arc::new(addresses)), remaining).call_async_uri(target);
    tokio::time::timeout_at(deadline, connecting)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TCP connect timed out"))?
}

#[cfg(test)]
mod vetted_tcp_tests {
    use super::*;

    #[tokio::test]
    async fn an_unreachable_answer_does_not_hide_a_later_vetted_address() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live = listener.local_addr().unwrap();
        let dead = SocketAddr::new("127.0.0.2".parse().unwrap(), live.port());
        let socket = connect_vetted_tcp(
            vec![dead, live],
            // This name cannot use public DNS; all answers are the exact
            // test-vetted addresses above, as on the production WSS path.
            "already-vetted.invalid",
            live.port(),
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(socket.peer_addr().unwrap(), live);
        assert!(socket.nodelay().unwrap());
    }

    #[tokio::test]
    async fn an_expired_deadline_cannot_start_another_connection() {
        let result = connect_vetted_tcp(
            vec!["127.0.0.1:1".parse().unwrap()],
            "already-vetted.invalid",
            1,
            tokio::time::Instant::now() - Duration::from_secs(1),
        )
        .await;
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}

trait CallUri {
    fn call_async_uri(
        self,
        dst: Uri,
    ) -> Pin<Box<dyn Future<Output = Result<TcpStream, BoxError>> + Send>>;
}

impl<R> CallUri for HttpConnector<R>
where
    HttpConnector<R>: tower_service::Service<Uri, Response = TokioIo<TcpStream>> + Send + 'static,
    <HttpConnector<R> as tower_service::Service<Uri>>::Future: Send,
    <HttpConnector<R> as tower_service::Service<Uri>>::Error: Into<BoxError>,
{
    fn call_async_uri(
        mut self,
        dst: Uri,
    ) -> Pin<Box<dyn Future<Output = Result<TcpStream, BoxError>> + Send>> {
        Box::pin(async move {
            let io = tower_service::Service::call(&mut self, dst)
                .await
                .map_err(Into::into)?;
            Ok(io.into_inner())
        })
    }
}

/// A resolver that answers addresses already looked up and vetted.
#[derive(Clone)]
struct StaticAddresses(Arc<Vec<SocketAddr>>);

impl tower_service::Service<Name> for StaticAddresses {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = std::future::Ready<Result<Self::Response, io::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _name: Name) -> Self::Future {
        std::future::ready(Ok(self.0.as_ref().clone().into_iter()))
    }
}

/// The connection to `dst` through `proxy`, and how long establishing it took.
///
/// A proxy is the machine owner's egress, not something content names, so the engine resolves and connects to it
/// without the address policy -- a proxy on the loopback is the common case -- and never resolves `dst` itself.
/// HTTPS is tunnelled with CONNECT; plain HTTP is written to the proxy in absolute form.
///
/// So the address rule does not reach past a proxy: an allowed domain that resolves to a private address is
/// connected to if the proxy connects to it. Resolving `dst` here first would break the networks a proxy exists
/// for, where the client cannot resolve outside names at all, and would still not bind what the proxy resolves.
/// DATA-HANDLING.md states the exception.
async fn through_proxy(
    proxy: &Intercept,
    dst: &Uri,
    https: bool,
) -> Result<(TcpStream, Span), BoxError> {
    let proxy_uri = proxy.uri().clone();
    if proxy_uri.scheme_str() != Some("http") {
        return Err(format!("unsupported proxy scheme {:?}", proxy_uri.scheme_str()).into());
    }
    let started = Instant::now();
    let direct = tcp_connector(hyper_util::client::legacy::connect::dns::GaiResolver::new());
    let tcp = if https {
        let mut tunnel = Tunnel::new(proxy_uri, direct);
        if let Some(auth) = proxy.basic_auth() {
            tunnel = tunnel.with_auth(auth.clone());
        }
        let io = tower_service::Service::call(&mut tunnel, dst.clone()).await?;
        io.into_inner()
    } else {
        direct.call_async_uri(proxy_uri).await?
    };
    Ok((tcp, Span::since(started)))
}

/// A byte stream hyper can drive.
pub(crate) trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// One connection: its stream, and its trace.
pub(crate) struct Conn {
    // Declared before the stream: fields drop in order, and the probe's slot must be emptied before the socket closes.
    _guard: SocketGuard,
    io: TokioIo<Box<dyn Io>>,
    trace: Arc<ConnectionTrace>,
    absolute_form: bool,
}

impl Conn {
    fn new(
        guard: SocketGuard,
        io: Box<dyn Io>,
        trace: ConnectionTrace,
        absolute_form: bool,
    ) -> Self {
        Self {
            _guard: guard,
            io: TokioIo::new(io),
            trace: Arc::new(trace),
            absolute_form,
        }
    }
}

impl Connection for Conn {
    fn connected(&self) -> Connected {
        let connected = Connected::new()
            .proxy(self.absolute_form)
            .extra(Arc::clone(&self.trace));
        if self.trace.protocol == Version::HTTP_2 {
            connected.negotiated_h2()
        } else {
            connected
        }
    }
}

impl hyper::rt::Read for Conn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for Conn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }
}
