//! Content's UDP socket, as the host holds it.
//!
//! One bound socket is one resource in the session's table, and content drives
//! it with six calls: bind, connect, send, set the TTL, take the next datagram,
//! close. Every destination is held to the policy `fetch` is held to, and every
//! resolved address is run through the address filter -- so multicast, private
//! and link-local targets are refused before a datagram reaches the kernel,
//! whether content named them by host or by literal. A sender is held to the same
//! rule: content hears only those it could send to.
//!
//! Broadcast is refused outright: the filter already blocks 255.255.255.255 and
//! every multicast range, so the flag has no legitimate target and would only
//! serve to escape those checks. Fan-out goes through a server.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tracing::debug;

use shared::network_policy::{NetworkChannel, NetworkPolicy};

use crate::ServiceError;

use super::address_filter::{LanAccess, is_blocked_datagram_address, is_public};
use super::gate::{self, GateKind};
use super::resources::{CancelFlag, ResourceId, ResourceTable};
use super::sockets::{
    AddrMeta, BACKGROUND_THROTTLE, ReceiveScratch, addr_family, checked_port, join_host_port,
    resolve_first,
};

/// The largest single receive.
///
/// A datagram arrives in one `recv_from` and bytes past the buffer end are
/// dropped with no truncation flag, so this must hold the largest UDP payload
/// there is: 65507 over IPv4, 65527 over IPv6.
const RECV_CAPACITY: usize = 65536;

/// The receive side: the scratch a datagram lands in, and the one peer whose
/// address string is kept.
///
/// The cache is a single entry and is replaced when the peer differs, so
/// `remoteAddress` is always the datagram's actual sender rather than a stale
/// value -- and a socket talking to one peer, which is what a game does,
/// formats its address once.
struct Receive {
    scratch: ReceiveScratch,
    last_peer: Option<(SocketAddr, Arc<str>)>,
}

/// One bound socket.
///
/// The socket itself is shared by send, connect and the TTL; the receive state
/// is taken exclusively by a read, so two reads cannot race the scratch. A read
/// takes the receive state and then the socket, and nothing else takes the
/// receive state, so the two cannot deadlock.
pub struct UdpSock {
    socket: UdpSocket,
    rx: Mutex<Receive>,
    cancel: Arc<CancelFlag>,
    local: AddrMeta,
    inbound: Inbound,
}

/// Which senders' datagrams reach content. The socket listens on every
/// interface, so without this anything that can reach the port -- a process on
/// the device, a host on the LAN -- would be heard where content could not
/// send to it.
#[derive(Clone, Copy, Debug)]
enum Inbound {
    /// The senders this socket could send to: a public one only when the udp
    /// channel grants a domain, one on the local network only when the local
    /// network is granted, the device itself never.
    Policy { lan: LanAccess, public: bool },
    /// Every sender. Tests talk over the loopback, which the rule refuses.
    #[cfg(test)]
    Any,
}

impl Inbound {
    fn admits(self, peer: &SocketAddr) -> bool {
        match self {
            Self::Policy { lan, public } => {
                if is_public(peer.ip()) {
                    public
                } else {
                    !is_blocked_datagram_address(peer, lan)
                }
            }
            #[cfg(test)]
            Self::Any => true,
        }
    }
}

impl UdpSock {
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// What a bind answered.
#[derive(Clone, Debug)]
pub struct UdpBound {
    pub rid: ResourceId,
    pub local: AddrMeta,
}

/// One event from a socket.
#[derive(Clone, Debug)]
pub enum UdpEvent {
    /// A datagram, with the peer it came from and the socket it arrived on.
    Message {
        data: Box<[u8]>,
        remote: AddrMeta,
        local: AddrMeta,
    },
    Error(String),
}

/// Bind a local port. `port` of zero takes whatever the system assigns.
///
/// A bound socket hears whoever can reach its port, so a policy under which
/// content could send to no one gets no socket, and the one it does get hears
/// only the senders it could send to.
pub fn bind(
    policy: &NetworkPolicy,
    resources: &ResourceTable,
    port: u32,
    socket_type: &str,
) -> Result<UdpBound, ServiceError> {
    let lan = gate::lan_access(policy, NetworkChannel::Udp);
    let public = !policy.channel(NetworkChannel::Udp).grants_nothing();
    if !public && lan == LanAccess::Refused {
        return Err(generic(
            "bind:fail UDPSocket: the policy grants the udp channel nothing".to_string(),
        ));
    }
    bind_hearing(
        resources,
        port,
        socket_type,
        Inbound::Policy { lan, public },
    )
}

fn bind_hearing(
    resources: &ResourceTable,
    port: u32,
    socket_type: &str,
    inbound: Inbound,
) -> Result<UdpBound, ServiceError> {
    let port = checked_port(port)?;
    let address: SocketAddr = if socket_type == "udp6" {
        format!("[::]:{port}")
    } else {
        format!("0.0.0.0:{port}")
    }
    .parse()
    .map_err(|error| generic(format!("bind:fail {error}")))?;
    debug!("UDP bind: {address} (type={socket_type})");

    let socket = std::net::UdpSocket::bind(address)
        .map_err(|error| generic(format!("bind:fail {error}")))?;
    socket
        .set_nonblocking(true)
        .map_err(|error| generic(format!("bind:fail set_nonblocking: {error}")))?;
    let socket = UdpSocket::from_std(socket)
        .map_err(|error| generic(format!("bind:fail from_std: {error}")))?;
    let local_addr = socket
        .local_addr()
        .map_err(|error| generic(format!("bind:fail {error}")))?;
    // Formatted once: an event clones the `Arc<str>` rather than formatting the
    // same address per datagram.
    let local = AddrMeta::new(&local_addr);

    let rid = resources.add_udp(Arc::new(UdpSock {
        socket,
        rx: Mutex::new(Receive {
            scratch: ReceiveScratch::new(RECV_CAPACITY),
            last_peer: None,
        }),
        cancel: Arc::new(CancelFlag::default()),
        local: local.clone(),
        inbound,
    }));
    debug!("UDP bound, rid={rid}, local={local_addr}");
    Ok(UdpBound { rid, local })
}

/// Set the socket's default destination, which is what an unaddressed write
/// goes to.
pub async fn connect(
    policy: &NetworkPolicy,
    resources: &ResourceTable,
    rid: ResourceId,
    address: &str,
    port: u32,
) -> Result<(), ServiceError> {
    let port = checked_port(port)?;
    let address = gate::enforce_host(address, port, policy, GateKind::UdpSocket)
        .map_err(ServiceError::generic)?;
    let socket = resources.udp(rid)?;

    let target = join_host_port(&address, port);
    debug!("UDP connect: rid={rid}, target={target}");
    let lan = gate::lan_access(policy, NetworkChannel::Udp);
    let resolved = resolve_first(&target, |resolved| {
        super::address_filter::is_blocked_datagram_address(resolved, lan)
    })
    .await
    .map_err(|error| generic(format!("connect:fail {error}")))?;
    socket
        .socket
        .connect(resolved)
        .await
        .map_err(|error| generic(format!("connect:fail {error}")))?;
    debug!("UDP connect: success, rid={rid}, addr={resolved}");
    Ok(())
}

/// Send one datagram.
#[allow(clippy::too_many_arguments)]
pub async fn send(
    policy: &NetworkPolicy,
    resources: &ResourceTable,
    rid: ResourceId,
    address: &str,
    port: u32,
    text: Option<String>,
    bytes: Option<Vec<u8>>,
    offset: u32,
    length: u32,
    set_broadcast: bool,
) -> Result<(), ServiceError> {
    let port = checked_port(port)?;
    let address = gate::enforce_host(address, port, policy, GateKind::UdpSocket)
        .map_err(ServiceError::generic)?;
    let socket = resources.udp(rid)?;

    let target = join_host_port(&address, port);
    debug!("UDP send: rid={rid}, target={target}");
    // Every DNS answer goes through the address filter's datagram rule: the
    // stream rule (documentation, benchmarking, reserved, link-local, loopback,
    // private refused unless the local network is granted) with the broadcast
    // address and multicast groups allowed on a granted local network, as the
    // reference allows them.
    let lan = gate::lan_access(policy, NetworkChannel::Udp);
    let resolved = resolve_first(&target, |resolved| {
        super::address_filter::is_blocked_datagram_address(resolved, lan)
    })
    .await
    .map_err(|error| generic(format!("send:fail resolve error: {error}")))?;

    let payload = match (text, bytes) {
        (Some(text), _) => text.into_bytes(),
        (None, Some(bytes)) => {
            let (start, end) = send_range(bytes.len(), offset as usize, length as usize)
                .map_err(|why| generic(format!("send:fail {why}")))?;
            bytes[start..end].to_vec()
        }
        (None, None) => return Err(type_error("send:fail no data provided")),
    };

    // SO_BROADCAST: asked for (setBroadcast), or implied by the limited
    // broadcast address, which the reference sends to without it. A directed
    // broadcast (a LAN's x.y.z.255) without setBroadcast is refused by the
    // kernel, as the reference refuses it.
    let limited_broadcast = matches!(resolved.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast());
    if set_broadcast || limited_broadcast {
        socket
            .socket
            .set_broadcast(true)
            .map_err(|error| generic(format!("send:fail broadcast: {error}")))?;
    }
    debug!("UDP send: {} bytes to {resolved}", payload.len());
    socket
        .socket
        .send_to(&payload, resolved)
        .await
        .map_err(|error| generic(format!("send:fail {error}")))?;
    Ok(())
}

/// Set the IP_TTL option.
pub fn set_ttl(resources: &ResourceTable, rid: ResourceId, ttl: u32) -> Result<(), ServiceError> {
    resources
        .udp(rid)?
        .socket
        .set_ttl(ttl)
        .map_err(|error| generic(format!("setTTL:fail {error}")))
}

/// The next datagram, or the failure reading for one gave.
pub async fn next_event(
    resources: &ResourceTable,
    rid: ResourceId,
    backgrounded: &AtomicBool,
) -> Result<UdpEvent, ServiceError> {
    let socket = resources.udp(rid)?;
    let cancel = Arc::clone(&socket.cancel);
    let event = async {
        if backgrounded.load(Ordering::Relaxed) {
            tokio::time::sleep(BACKGROUND_THROTTLE).await;
        }
        let mut rx = socket.rx.lock().await;
        let rx = &mut *rx;
        loop {
            match socket.socket.recv_from(rx.scratch.as_mut_slice()).await {
                Ok((count, peer)) => {
                    // The cached peer was admitted when it was cached, so a socket
                    // talking to one peer lists the device's networks once.
                    let known = rx
                        .last_peer
                        .as_ref()
                        .is_some_and(|(cached, _)| *cached == peer);
                    if !known && !socket.inbound.admits(&peer) {
                        debug!("UDP: dropped a datagram from {peer}, which content may not reach");
                        continue;
                    }
                    break UdpEvent::Message {
                        // Exactly the filled prefix: datagram boundaries are kept and
                        // no bytes from a longer earlier datagram reach content.
                        data: rx.scratch.copy_filled(count),
                        remote: AddrMeta {
                            address: peer_address(&mut rx.last_peer, peer),
                            family: addr_family(&peer),
                            port: peer.port(),
                        },
                        local: socket.local.clone(),
                    };
                }
                Err(error) => break UdpEvent::Error(error.to_string()),
            }
        }
    };
    cancel
        .until_cancelled(event)
        .await
        .ok_or_else(|| generic("UDPSocket closed"))
}

/// Close the socket: the read in flight is cancelled and the socket drops.
pub fn close(resources: &ResourceTable, rid: ResourceId) -> Result<(), ServiceError> {
    resources.udp(rid)?;
    resources.close(rid);
    Ok(())
}

/// The peer's address, reusing the cached string when the peer has not changed
/// and replacing it when it has.
fn peer_address(cache: &mut Option<(SocketAddr, Arc<str>)>, peer: SocketAddr) -> Arc<str> {
    if let Some((cached, address)) = cache.as_ref()
        && *cached == peer
    {
        return Arc::clone(address);
    }
    let address: Arc<str> = Arc::from(peer.ip().to_string());
    *cache = Some((peer, Arc::clone(&address)));
    address
}

/// The `[start, end)` of an outbound payload for `offset` and `length`.
///
/// Out of bounds is refused rather than clamped: clamping quietly sent fewer
/// bytes than the caller asked for, and `offset + length` can overflow.
/// `length == 0` means "from `offset` to the end".
fn send_range(len: usize, offset: usize, length: usize) -> Result<(usize, usize), &'static str> {
    if offset > len {
        return Err("offset out of bounds");
    }
    let end = if length == 0 {
        len
    } else {
        let end = offset.checked_add(length).ok_or("length overflow")?;
        if end > len {
            return Err("offset+length out of bounds");
        }
        end
    };
    Ok((offset, end))
}

fn generic(message: impl Into<String>) -> ServiceError {
    ServiceError::generic(message)
}

fn type_error(message: impl Into<String>) -> ServiceError {
    ServiceError::classed("TypeError", message)
}

#[cfg(test)]
#[path = "udp_tests.rs"]
mod tests;
