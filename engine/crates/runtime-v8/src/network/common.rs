use std::net::SocketAddr;
use std::sync::Arc;

use deno_error::JsErrorBox;

/// Delay inserted before each poll iteration when the app is backgrounded.
pub(super) const BACKGROUND_THROTTLE: std::time::Duration = std::time::Duration::from_millis(500);

/// Narrow a JS-supplied port (arrives as `u32`) to `u16`, rejecting
/// out-of-range values instead of silently wrapping. `70000 as u16`
/// is `4464` — a *different* port — so raw sockets must reject rather
/// than connect somewhere the caller never asked for.
pub(super) fn checked_port(port: u32) -> Result<u16, JsErrorBox> {
    u16::try_from(port)
        .map_err(|_| JsErrorBox::type_error(format!("port {} out of range (0-65535)", port)))
}

/// Build a `host:port` string for `tokio::net::lookup_host`, bracketing
/// bare IPv6 literals (`::1` -> `[::1]:port`). Without the brackets the
/// literal's own colons make the string ambiguous and resolution fails,
/// so raw IPv6-literal connect/send targets would never work.
pub(super) fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    }
}

/// Resolve a host:port string to the first SocketAddr.
///
/// Rejects addresses in private/loopback/link-local ranges to prevent SSRF.
/// Checks ALL resolved addresses — if any points to a blocked range the
/// entire resolution is rejected (prevents mixed public/private DNS responses).
pub(super) async fn resolve_first(addr: &str) -> Result<SocketAddr, std::io::Error> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host(addr).await?.collect();
    let first = *addrs.first().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            format!("could not resolve '{}'", addr),
        )
    })?;
    for resolved in &addrs {
        if migo_services::network::address_filter::is_blocked_address(resolved) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "connection to {} is not allowed (private/loopback address)",
                    resolved.ip()
                ),
            ));
        }
    }
    Ok(first)
}

/// Return "IPv4" or "IPv6" for a SocketAddr as a `&'static str`, so callers
/// cache/copy it instead of allocating a `String` per event.
pub(super) fn addr_family(addr: &SocketAddr) -> &'static str {
    match addr {
        SocketAddr::V4(_) => "IPv4",
        SocketAddr::V6(_) => "IPv6",
    }
}

/// Cached, cheaply-clonable address metadata for one socket endpoint. Built
/// once at connect/bind so the hot receive path clones an `Arc<str>` and copies
/// two scalars instead of re-formatting a `SocketAddr` (heap `String`) per event.
#[derive(Clone)]
pub(super) struct AddrMeta {
    pub address: Arc<str>,
    pub family: &'static str,
    pub port: u16,
}

impl AddrMeta {
    pub(super) fn new(addr: &SocketAddr) -> Self {
        Self {
            address: Arc::from(addr.ip().to_string()),
            family: addr_family(addr),
            port: addr.port(),
        }
    }
}

/// Reusable receive scratch. Allocated and zeroed **once** per socket receive
/// state and reused across reads; each event copies only the filled prefix into
/// a fresh exact-length `Box<[u8]>` for `ToJsBuffer`. No `unsafe` /
/// uninitialized memory. This is one exact n-byte alloc+copy per event, not
/// end-to-end zero-copy — it removes the repeated 64 KiB allocate+zero.
pub(super) struct ReceiveScratch {
    buf: Box<[u8]>,
}

impl ReceiveScratch {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0u8; capacity].into_boxed_slice(),
        }
    }

    /// Mutable view of the whole scratch for a single `read` / `recv_from`.
    pub(super) fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buf
    }

    /// Copy exactly the first `n` filled bytes into a fresh exact-length
    /// `Box<[u8]>`. `n` is clamped to the scratch length so a malformed
    /// internal length can never read out of bounds or panic.
    pub(super) fn copy_filled(&self, n: usize) -> Box<[u8]> {
        let end = n.min(self.buf.len());
        self.buf[..end].to_vec().into_boxed_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_port_accepts_full_u16_range() {
        assert_eq!(checked_port(0).unwrap(), 0);
        assert_eq!(checked_port(80).unwrap(), 80);
        assert_eq!(checked_port(65535).unwrap(), 65535);
    }

    #[test]
    fn checked_port_rejects_out_of_range_instead_of_wrapping() {
        // Regression: `70000 as u16` silently becomes 4464. Reject it.
        assert!(checked_port(65536).is_err());
        assert!(checked_port(70000).is_err());
        assert!(checked_port(u32::MAX).is_err());
    }

    #[test]
    fn join_host_port_brackets_bare_ipv6_literal() {
        assert_eq!(join_host_port("::1", 80), "[::1]:80");
        assert_eq!(join_host_port("2001:db8::1", 443), "[2001:db8::1]:443");
    }

    #[test]
    fn join_host_port_leaves_already_bracketed_ipv6() {
        assert_eq!(join_host_port("[::1]", 80), "[::1]:80");
    }

    #[test]
    fn join_host_port_passes_through_ipv4_and_hostname() {
        assert_eq!(join_host_port("1.2.3.4", 80), "1.2.3.4:80");
        assert_eq!(join_host_port("example.com", 443), "example.com:443");
    }

    #[test]
    fn addr_family_maps_v4_and_v6() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        assert_eq!(
            addr_family(&SocketAddr::from((Ipv4Addr::LOCALHOST, 80))),
            "IPv4"
        );
        assert_eq!(
            addr_family(&SocketAddr::from((Ipv6Addr::LOCALHOST, 80))),
            "IPv6"
        );
    }

    #[test]
    fn addr_meta_caches_family_port_and_clonable_arc() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        let v4 = AddrMeta::new(&SocketAddr::from((Ipv4Addr::new(1, 2, 3, 4), 1234)));
        assert_eq!(&*v4.address, "1.2.3.4");
        assert_eq!(v4.family, "IPv4");
        assert_eq!(v4.port, 1234);

        let v6 = AddrMeta::new(&SocketAddr::from((Ipv6Addr::LOCALHOST, 443)));
        assert_eq!(v6.family, "IPv6");
        assert_eq!(&*v6.address, "::1");

        // Cloning a cached endpoint shares the same allocation (Arc refcount
        // bump), not a re-formatted string — the per-event reuse contract.
        let cloned = v4.clone();
        assert!(Arc::ptr_eq(&v4.address, &cloned.address));
    }

    #[test]
    fn receive_scratch_reuses_allocation_and_copies_exact_bytes() {
        let mut s = ReceiveScratch::new(65536);
        let base = s.as_mut_slice().as_ptr();
        assert_eq!(s.as_mut_slice().len(), 65536);

        s.as_mut_slice()[..5].copy_from_slice(&[1, 2, 3, 4, 5]);
        let first = s.copy_filled(5);
        assert_eq!(&*first, &[1, 2, 3, 4, 5]);
        // Scratch allocation retained across the copy (pointer + capacity stable).
        assert_eq!(s.as_mut_slice().as_ptr(), base);
        assert_eq!(s.as_mut_slice().len(), 65536);

        // A shorter second read reuses the same scratch; the produced payload
        // is exact with NO trailing zero bytes leaking from the earlier read.
        s.as_mut_slice()[..3].copy_from_slice(&[9, 9, 9]);
        let second = s.copy_filled(3);
        assert_eq!(&*second, &[9, 9, 9]);
        assert_eq!(s.as_mut_slice().as_ptr(), base);

        // Empty datagram → 0-length payload.
        assert_eq!(&*s.copy_filled(0), &[] as &[u8]);
        // Malformed over-length is clamped, never panics / reads OOB.
        assert_eq!(s.copy_filled(usize::MAX).len(), 65536);
    }
}
