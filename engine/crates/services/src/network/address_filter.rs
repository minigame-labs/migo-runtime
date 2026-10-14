//! Network address filtering for SSRF prevention.
//!
//! Shared by `fetch`, `WebSocket`, raw TCP and raw UDP so the runtime
//! has a **single** notion of which addresses content may reach: publicly
//! reachable unicast, and the other devices on the device's own local network.
//!
//! Blocks wherever it is:
//! - Loopback / link-local / unspecified / broadcast
//! - Carrier-Grade NAT
//! - IPv6 link-local
//! - IPv4-mapped IPv6 pointing at any of the above
//! - IPv4 multicast 224.0.0.0/4 and IPv6 multicast ff00::/8
//! - RFC 5737 / 3849 documentation ranges
//! - RFC 2544 benchmarking (198.18.0.0/15)
//! - RFC 1112 reserved future 240.0.0.0/4
//!
//! Blocks unless it is another device on the device's local network:
//! - RFC 1918 private and IPv6 unique-local addresses
//!
//! The local network is what the reference platform opens -- "the non-local
//! IPs on the local network" -- so a game can play on a LAN; a private address
//! anywhere else, and the device itself, stay out of reach.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use shared::device::local_address::{self, LocalNetwork};

/// Returns `true` if the address may not be reached from game JS code.
///
/// A private address is allowed only on the device's own local network and only when it is not the device itself:
/// the reference platform allows "the non-local IPs on the local network" (局域网内的非本机 IP) -- which is how games
/// play on a LAN -- and nothing else private. Everything else this module lists is refused wherever it is.
pub fn is_blocked_address(addr: &SocketAddr) -> bool {
    // The interfaces are listed only for a private address: a system call, and most addresses are public.
    is_blocked_given(addr, local_address::networks)
}

/// [`is_blocked_address`] on a device whose networks are `networks`.
#[cfg(test)]
pub(crate) fn is_blocked_on(addr: &SocketAddr, networks: &[LocalNetwork]) -> bool {
    is_blocked_given(addr, || networks.to_vec())
}

/// The rule, with the device's networks asked for only when an address's class needs them.
fn is_blocked_given(addr: &SocketAddr, networks: impl FnOnce() -> Vec<LocalNetwork>) -> bool {
    match class_of(addr.ip()) {
        Class::Public => false,
        Class::Reserved => true,
        Class::Private => !on_local_network(addr.ip(), &networks()),
    }
}

/// Whether `ip` is in a private range -- which [`is_blocked_address`] allows only on the device's local network. An
/// allowed private address is therefore another device on that network.
pub(crate) fn is_private(ip: IpAddr) -> bool {
    matches!(class_of(ip), Class::Private)
}

enum Class {
    /// Publicly reachable unicast.
    Public,
    /// A private range: allowed on the device's local network.
    Private,
    /// Never: loopback, link-local, broadcast, multicast, and the ranges reserved for documentation and testing.
    Reserved,
}

fn class_of(ip: IpAddr) -> Class {
    match canonical(ip) {
        IpAddr::V4(ip) => class_v4(&ip),
        IpAddr::V6(ip) => class_v6(&ip),
    }
}

/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) as the IPv4 address it embeds, so `::ffff:127.0.0.1` is judged as
/// `127.0.0.1` and a mapped LAN address as the LAN address it is.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn class_v4(ip: &Ipv4Addr) -> Class {
    if ip.is_loopback()            // 127.0.0.0/8
        || ip.is_link_local()      // 169.254.0.0/16 (includes cloud metadata 169.254.169.254)
        || ip.is_broadcast()       // 255.255.255.255
        || ip.is_unspecified()     // 0.0.0.0
        || is_cgn(ip)              // 100.64.0.0/10 (Carrier-Grade NAT)
        || is_multicast_v4(ip)     // 224.0.0.0/4 (RFC 5771) — unicast-only policy
        || is_documentation_v4(ip) // TEST-NET-1/2/3 (RFC 5737)
        || is_benchmark_v4(ip)     // 198.18.0.0/15 (RFC 2544)
        || is_reserved_future_v4(ip)
    // 240.0.0.0/4 (RFC 1112)
    {
        Class::Reserved
    } else if ip.is_private() {
        // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
        Class::Private
    } else {
        Class::Public
    }
}

fn class_v6(ip: &Ipv6Addr) -> Class {
    if ip.is_loopback()            // ::1
        || ip.is_unspecified()     // ::
        || is_ipv6_link_local(ip)  // fe80::/10
        || is_multicast_v6(ip)     // ff00::/8 — unicast-only policy
        || is_documentation_v6(ip)
    // 2001:db8::/32 (RFC 3849)
    {
        Class::Reserved
    } else if is_unique_local(ip) {
        // fc00::/7
        Class::Private
    } else {
        Class::Public
    }
}

/// Whether a private `ip` is on one of the device's networks and is not the device itself.
fn on_local_network(ip: IpAddr, networks: &[LocalNetwork]) -> bool {
    let ip = canonical(ip);
    !networks.iter().any(|network| network.address == ip)
        && networks
            .iter()
            .any(|network| network.prefix > 0 && network.contains(ip))
}

/// Carrier-Grade NAT: 100.64.0.0/10
fn is_cgn(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (octets[1] & 0xC0) == 64
}

/// IPv6 Unique Local Address: fc00::/7
fn is_unique_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xFE00) == 0xFC00
}

/// IPv6 Link-Local: fe80::/10
fn is_ipv6_link_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xFFC0) == 0xFE80
}

/// IPv4 multicast: 224.0.0.0/4 (RFC 5771).
///
/// A small-game runtime that allows outbound multicast would let JS
/// reach LAN discovery services, SSDP/mDNS neighbours, and routing
/// daemons — none of which are legitimate targets.
fn is_multicast_v4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    (o[0] & 0xF0) == 0xE0
}

/// IPv4 documentation ranges (RFC 5737): 192.0.2.0/24, 198.51.100.0/24,
/// 203.0.113.0/24. These must never appear on the public internet, so
/// a resolution into one of these ranges is either a misconfigured DNS
/// response or an exfiltration trick.
fn is_documentation_v4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
}

/// IPv4 benchmarking: 198.18.0.0/15 (RFC 2544).
fn is_benchmark_v4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 198 && (o[1] & 0xFE) == 18
}

/// IPv4 reserved future use: 240.0.0.0/4 (RFC 1112, excluding
/// 255.255.255.255 which `is_broadcast` already covers).
fn is_reserved_future_v4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    (o[0] & 0xF0) == 0xF0 && !ip.is_broadcast()
}

/// IPv6 multicast: ff00::/8 (RFC 4291).
fn is_multicast_v6(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xFF00) == 0xFF00
}

/// IPv6 documentation: 2001:db8::/32 (RFC 3849).
fn is_documentation_v6(ip: &Ipv6Addr) -> bool {
    let seg = ip.segments();
    seg[0] == 0x2001 && seg[1] == 0x0DB8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule on a device on no local network, so what these assertions say does not depend on the network the
    /// machine running them is on. (It shadows the glob-imported function on purpose.)
    fn is_blocked_address(addr: &SocketAddr) -> bool {
        is_blocked_on(addr, &[])
    }

    fn network(address: IpAddr, prefix: u8) -> LocalNetwork {
        LocalNetwork { address, prefix }
    }

    /// The reference allows the non-local IPs on the local network: an emulator at 10.0.2.15/24 reaches its host at
    /// 10.0.2.2, but not itself, nor another network's private address.
    #[test]
    fn a_private_address_on_the_local_network_is_allowed_unless_it_is_the_device() {
        let lan = [network(IpAddr::V4(Ipv4Addr::new(10, 0, 2, 15)), 24)];
        assert!(!is_blocked_on(&sa4(10, 0, 2, 2, 8080), &lan));
        assert!(
            is_blocked_on(&sa4(10, 0, 2, 15, 8080), &lan),
            "the device itself"
        );
        assert!(
            is_blocked_on(&sa4(10, 0, 3, 1, 8080), &lan),
            "another network"
        );
        assert!(is_blocked_on(&sa4(192, 168, 1, 1, 80), &lan));
        // The rest stays refused on any network.
        assert!(is_blocked_on(&sa4(127, 0, 0, 1, 80), &lan));
        assert!(is_blocked_on(&sa4(169, 254, 169, 254, 80), &lan));
    }

    /// The same for IPv6's unique local addresses, and for an IPv4 LAN address written in its mapped IPv6 form.
    #[test]
    fn unique_local_and_mapped_lan_addresses_follow_the_same_rule() {
        let lan = [
            network(
                IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 0x15)),
                64,
            ),
            network(IpAddr::V4(Ipv4Addr::new(192, 168, 5, 15)), 24),
        ];
        assert!(!is_blocked_on(
            &sa6([0xfd00, 0, 0, 0, 0, 0, 0, 2], 80),
            &lan
        ));
        assert!(
            is_blocked_on(&sa6([0xfd00, 0, 0, 1, 0, 0, 0, 2], 80), &lan),
            "another /64"
        );
        assert!(
            !is_blocked_on(&sa6([0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0502], 80), &lan),
            "::ffff:192.168.5.2"
        );
        assert!(
            is_blocked_on(&sa6([0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001], 80), &lan),
            "::ffff:127.0.0.1"
        );
    }

    /// A network of prefix 0 is not a local network: it would make every address local.
    #[test]
    fn a_zero_length_prefix_makes_nothing_local() {
        let everything = [network(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 0)];
        assert!(is_blocked_on(&sa4(192, 168, 1, 1, 80), &everything));
    }

    /// Through the device's real interfaces: what does not depend on them.
    #[test]
    fn the_real_rule_refuses_the_loopback_and_allows_the_public() {
        assert!(super::is_blocked_address(&sa4(127, 0, 0, 1, 80)));
        assert!(!super::is_blocked_address(&sa4(8, 8, 8, 8, 443)));
    }

    fn sa4(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), port)
    }

    fn sa6(segments: [u16; 8], port: u16) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                segments[4],
                segments[5],
                segments[6],
                segments[7],
            )),
            port,
        )
    }

    // -- IPv4 blocked --

    #[test]
    fn blocks_loopback_127_0_0_1() {
        assert!(is_blocked_address(&sa4(127, 0, 0, 1, 80)));
    }

    #[test]
    fn blocks_loopback_127_x() {
        assert!(is_blocked_address(&sa4(127, 255, 255, 255, 80)));
    }

    #[test]
    fn blocks_private_10() {
        assert!(is_blocked_address(&sa4(10, 0, 0, 1, 8080)));
    }

    #[test]
    fn blocks_private_172_16() {
        assert!(is_blocked_address(&sa4(172, 16, 0, 1, 443)));
    }

    #[test]
    fn blocks_private_172_31() {
        assert!(is_blocked_address(&sa4(172, 31, 255, 255, 443)));
    }

    #[test]
    fn blocks_private_192_168() {
        assert!(is_blocked_address(&sa4(192, 168, 1, 1, 22)));
    }

    #[test]
    fn blocks_link_local() {
        assert!(is_blocked_address(&sa4(169, 254, 1, 1, 80)));
    }

    #[test]
    fn blocks_cloud_metadata() {
        assert!(is_blocked_address(&sa4(169, 254, 169, 254, 80)));
    }

    #[test]
    fn blocks_broadcast() {
        assert!(is_blocked_address(&sa4(255, 255, 255, 255, 80)));
    }

    #[test]
    fn blocks_unspecified() {
        assert!(is_blocked_address(&sa4(0, 0, 0, 0, 80)));
    }

    #[test]
    fn blocks_cgn_100_64() {
        assert!(is_blocked_address(&sa4(100, 64, 0, 1, 80)));
    }

    #[test]
    fn blocks_cgn_100_127() {
        assert!(is_blocked_address(&sa4(100, 127, 255, 254, 80)));
    }

    // -- IPv4 allowed --

    #[test]
    fn allows_public_8_8_8_8() {
        assert!(!is_blocked_address(&sa4(8, 8, 8, 8, 443)));
    }

    #[test]
    fn allows_public_1_1_1_1() {
        assert!(!is_blocked_address(&sa4(1, 1, 1, 1, 53)));
    }

    #[test]
    fn blocks_documentation_203() {
        // RFC 5737 reserves 203.0.113.0/24 for documentation.
        assert!(is_blocked_address(&sa4(203, 0, 113, 1, 80)));
    }

    #[test]
    fn blocks_documentation_192_0_2() {
        assert!(is_blocked_address(&sa4(192, 0, 2, 42, 80)));
    }

    #[test]
    fn blocks_documentation_198_51_100() {
        assert!(is_blocked_address(&sa4(198, 51, 100, 5, 80)));
    }

    #[test]
    fn blocks_benchmark_198_18() {
        assert!(is_blocked_address(&sa4(198, 18, 0, 1, 80)));
    }

    #[test]
    fn blocks_benchmark_198_19() {
        assert!(is_blocked_address(&sa4(198, 19, 255, 254, 80)));
    }

    #[test]
    fn blocks_reserved_future_240() {
        assert!(is_blocked_address(&sa4(240, 0, 0, 1, 80)));
    }

    #[test]
    fn blocks_multicast_224() {
        assert!(is_blocked_address(&sa4(224, 0, 0, 1, 80)));
    }

    #[test]
    fn blocks_multicast_239() {
        assert!(is_blocked_address(&sa4(239, 255, 255, 255, 80)));
    }

    #[test]
    fn allows_public_198_17_before_benchmark() {
        assert!(!is_blocked_address(&sa4(198, 17, 255, 254, 80)));
    }

    #[test]
    fn allows_public_223_just_below_multicast() {
        assert!(!is_blocked_address(&sa4(223, 255, 255, 254, 80)));
    }

    #[test]
    fn allows_172_outside_private() {
        // 172.32.0.0 is NOT in 172.16.0.0/12
        assert!(!is_blocked_address(&sa4(172, 32, 0, 1, 80)));
    }

    #[test]
    fn allows_100_outside_cgn() {
        // 100.128.0.0 is NOT in 100.64.0.0/10
        assert!(!is_blocked_address(&sa4(100, 128, 0, 1, 80)));
    }

    // -- IPv6 blocked --

    #[test]
    fn blocks_ipv6_loopback() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 80);
        assert!(is_blocked_address(&addr));
    }

    #[test]
    fn blocks_ipv6_unspecified() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 80);
        assert!(is_blocked_address(&addr));
    }

    #[test]
    fn blocks_ipv6_link_local() {
        assert!(is_blocked_address(&sa6([0xfe80, 0, 0, 0, 0, 0, 0, 1], 80)));
    }

    #[test]
    fn blocks_ipv6_unique_local_fc() {
        assert!(is_blocked_address(&sa6([0xfc00, 0, 0, 0, 0, 0, 0, 1], 80)));
    }

    #[test]
    fn blocks_ipv6_unique_local_fd() {
        assert!(is_blocked_address(&sa6([0xfd00, 0, 0, 0, 0, 0, 0, 1], 80)));
    }

    // -- IPv4-mapped IPv6 --

    #[test]
    fn blocks_ipv4_mapped_loopback() {
        // ::ffff:127.0.0.1
        assert!(is_blocked_address(&sa6(
            [0, 0, 0, 0, 0, 0xFFFF, 0x7F00, 0x0001],
            80
        )));
    }

    #[test]
    fn blocks_ipv4_mapped_private_10() {
        // ::ffff:10.0.0.1
        assert!(is_blocked_address(&sa6(
            [0, 0, 0, 0, 0, 0xFFFF, 0x0A00, 0x0001],
            80
        )));
    }

    #[test]
    fn blocks_ipv4_mapped_link_local() {
        // ::ffff:169.254.169.254 (cloud metadata)
        assert!(is_blocked_address(&sa6(
            [0, 0, 0, 0, 0, 0xFFFF, 0xA9FE, 0xA9FE],
            80
        )));
    }

    #[test]
    fn allows_ipv4_mapped_public() {
        // ::ffff:8.8.8.8
        assert!(!is_blocked_address(&sa6(
            [0, 0, 0, 0, 0, 0xFFFF, 0x0808, 0x0808],
            443
        )));
    }

    // -- IPv6 allowed --

    #[test]
    fn blocks_ipv6_documentation_range() {
        // 2001:db8::/32 is RFC 3849 documentation, must not be routed.
        assert!(is_blocked_address(&sa6(
            [0x2001, 0x0db8, 0, 0, 0, 0, 0, 1],
            80
        )));
    }

    #[test]
    fn blocks_ipv6_multicast() {
        assert!(is_blocked_address(&sa6([0xff02, 0, 0, 0, 0, 0, 0, 1], 80)));
    }

    #[test]
    fn allows_ipv6_global_unicast() {
        assert!(!is_blocked_address(&sa6(
            [0x2600, 0x1f18, 0, 0, 0, 0, 0, 1],
            443
        )));
    }
}
