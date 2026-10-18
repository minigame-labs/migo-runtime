//! Network address filtering for SSRF prevention.
//!
//! Shared by `fetch`, `WebSocket`, raw TCP and raw UDP so the runtime has a
//! **single** notion of which addresses content may reach: [`Reach`], where an
//! address is from the device. Every rule here is read off it.
//!
//! Never, wherever it is:
//! - the device itself, by any of its addresses
//! - loopback, link-local, "this network" 0.0.0.0/8, broadcast and multicast
//!   (a datagram on a granted local network excepted)
//! - Carrier-Grade NAT 100.64.0.0/10
//! - the IETF's special-purpose ranges no host on the internet has: protocol
//!   assignments 192.0.0.0/24, the 6to4 relay anycast 192.88.99.0/24,
//!   documentation (RFC 5737, RFC 3849), benchmarking (RFC 2544, 2001:2::/48),
//!   240.0.0.0/4, discard-only 100::/64, ORCHID, Teredo, local-use NAT64
//!   64:ff9b:1::/48 and the deprecated site-local fec0::/10
//! - an address that embeds an IPv4 address -- mapped, compatible, the NAT64
//!   well-known prefix, 6to4 -- is judged as the IPv4 address it embeds
//!
//! Only when the policy grants the local network: another device on one of the
//! device's own networks, whatever its address's class. On IPv6 that is a global
//! address as often as a unique-local one, so the address range alone cannot
//! say -- the device's networks do.
//!
//! The local network is what the reference platform opens -- "the non-local
//! IPs on the local network" -- so a game can play on a LAN. Here it is opened
//! only when the host grants it ([`LanAccess::LocalNetwork`]), and only for the
//! socket, TCP and UDP channels.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use shared::device::local_address::{self, LocalNetwork};

/// Whether other devices on the device's own local network may be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanAccess {
    /// No other device on the local network. What a request, download or upload always gets, and every channel by
    /// default.
    Refused,
    /// Another device on one of the device's networks; for a datagram, also the limited broadcast address and
    /// multicast groups.
    LocalNetwork,
}

/// Where an address is, from the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Never content's to reach: the device itself, the reserved ranges, and a private address on none of the
    /// device's networks.
    Never,
    /// Another device on one of the device's own networks -- or, for a datagram, a broadcast or multicast group.
    LocalNetwork,
    /// A server on the internet, which content reaches by name.
    Internet,
}

impl Reach {
    /// Whether an address this far away may not be reached under `lan`.
    pub(crate) fn blocked(self, lan: LanAccess) -> bool {
        match self {
            Reach::Never => true,
            Reach::LocalNetwork => lan == LanAccess::Refused,
            Reach::Internet => false,
        }
    }
}

/// Where `ip` is for a stream or a request: never a broadcast or multicast group.
pub(crate) fn stream_reach(ip: IpAddr) -> Reach {
    reach_given(ip, false, local_address::networks)
}

/// Where `ip` is for a datagram, which may also be sent to the local network's broadcast address and multicast
/// groups -- the reference lets `UDPSocket.send` reach "any IP", broadcast included, which is how a game finds its
/// peers on a LAN.
pub(crate) fn datagram_reach(ip: IpAddr) -> Reach {
    reach_given(ip, true, local_address::networks)
}

/// Returns `true` if a stream or request may not reach `addr` under `lan`.
pub fn is_blocked_address(addr: &SocketAddr, lan: LanAccess) -> bool {
    stream_reach(addr.ip()).blocked(lan)
}

/// [`is_blocked_address`] for a datagram.
pub fn is_blocked_datagram_address(addr: &SocketAddr, lan: LanAccess) -> bool {
    datagram_reach(addr.ip()).blocked(lan)
}

/// [`stream_reach`] or [`datagram_reach`] on a device whose networks are `networks`.
#[cfg(test)]
pub(crate) fn reach_on(ip: IpAddr, datagram: bool, networks: &[LocalNetwork]) -> Reach {
    reach_given(ip, datagram, || Arc::from(networks))
}

/// [`is_blocked_address`] on a device whose networks are `networks`.
#[cfg(test)]
pub(crate) fn is_blocked_on(addr: &SocketAddr, networks: &[LocalNetwork], lan: LanAccess) -> bool {
    reach_on(addr.ip(), false, networks).blocked(lan)
}

/// The rule, with the device's networks asked for only when an address's class leaves the question open.
fn reach_given(
    ip: IpAddr,
    datagram: bool,
    networks: impl FnOnce() -> Arc<[LocalNetwork]>,
) -> Reach {
    let ip = canonical(ip);
    if datagram && is_group(ip) {
        return Reach::LocalNetwork;
    }
    let class = match ip {
        IpAddr::V4(ip) => class_v4(&ip),
        IpAddr::V6(ip) => class_v6(&ip),
    };
    if class == Class::Reserved {
        return Reach::Never;
    }
    let networks = networks();
    if networks
        .iter()
        .any(|network| canonical(network.address) == ip)
    {
        return Reach::Never;
    }
    if networks
        .iter()
        .any(|network| network.prefix > 0 && network.contains(ip))
    {
        return Reach::LocalNetwork;
    }
    match class {
        Class::Public => Reach::Internet,
        Class::Private | Class::Reserved => Reach::Never,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Publicly reachable unicast.
    Public,
    /// A private range: another device on the local network, when the local network is granted and it is on one of
    /// the device's networks; never otherwise.
    Private,
    /// Never: loopback, link-local, broadcast, multicast, and the special-purpose ranges.
    Reserved,
}

/// The IPv4 address an IPv6 address embeds, if it is one of the forms that do -- so `::ffff:127.0.0.1` is judged as
/// `127.0.0.1`, and `64:ff9b::a00:1` as the `10.0.0.1` a NAT64 gateway would deliver it to.
fn canonical(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    let segments = v6.segments();
    let low = Ipv4Addr::from(u32::from(segments[6]) << 16 | u32::from(segments[7]));
    if let Some(v4) = v6.to_ipv4_mapped() {
        IpAddr::V4(v4)
    } else if segments[..6] == [0; 6] && !v6.is_loopback() && !v6.is_unspecified() {
        // The deprecated compatible form `::a.b.c.d`, which `::1` and `::` are not: they are IPv6's own loopback and
        // unspecified addresses, judged as themselves.
        IpAddr::V4(low)
    } else if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        // NAT64's well-known prefix, 64:ff9b::/96 (RFC 6052).
        IpAddr::V4(low)
    } else if segments[0] == 0x2002 {
        // 6to4, 2002::/16 (RFC 3056): the IPv4 address follows the prefix.
        IpAddr::V4(Ipv4Addr::from(
            u32::from(segments[1]) << 16 | u32::from(segments[2]),
        ))
    } else {
        ip
    }
}

/// A broadcast or multicast group.
fn is_group(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_broadcast() || ip.is_multicast(),
        IpAddr::V6(ip) => ip.is_multicast(),
    }
}

fn class_v4(ip: &Ipv4Addr) -> Class {
    let o = ip.octets();
    if o[0] == 0                            // 0.0.0.0/8, "this network" (RFC 791)
        || ip.is_loopback()                 // 127.0.0.0/8
        || ip.is_link_local()               // 169.254.0.0/16, cloud metadata included
        || ip.is_broadcast()                // 255.255.255.255
        || ip.is_multicast()                // 224.0.0.0/4
        || (o[0] == 100 && (o[1] & 0xC0) == 64) // 100.64.0.0/10, Carrier-Grade NAT (RFC 6598)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24, IETF protocol assignments (RFC 6890)
        || ip.is_documentation()            // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 (RFC 5737)
        || (o[0] == 192 && o[1] == 88 && o[2] == 99) // 192.88.99.0/24, 6to4 relay anycast (RFC 7526)
        || (o[0] == 198 && (o[1] & 0xFE) == 18) // 198.18.0.0/15, benchmarking (RFC 2544)
        || (o[0] & 0xF0) == 0xF0
    // 240.0.0.0/4, reserved (RFC 1112)
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
    let s = ip.segments();
    if ip.is_loopback()                     // ::1
        || ip.is_unspecified()              // ::
        || (s[0] & 0xFFC0) == 0xFE80        // fe80::/10, link-local
        || (s[0] & 0xFFC0) == 0xFEC0        // fec0::/10, site-local (deprecated, RFC 3879)
        || ip.is_multicast()                // ff00::/8
        || (s[0] == 0x2001 && s[1] == 0x0DB8) // 2001:db8::/32, documentation (RFC 3849)
        || (s[0] == 0x2001 && s[1] == 0)    // 2001::/32, Teredo (RFC 4380)
        || (s[0] == 0x2001 && s[1] == 2 && s[2] == 0) // 2001:2::/48, benchmarking (RFC 5180)
        || (s[0] == 0x2001 && matches!(s[1] & 0xFFF0, 0x0010 | 0x0020)) // ORCHID, 2001:10::/28 and 2001:20::/28
        || s[..4] == [0x0100, 0, 0, 0]      // 100::/64, discard-only (RFC 6666)
        || s[..3] == [0x64, 0xff9b, 1]
    // 64:ff9b:1::/48, local-use NAT64 (RFC 8215)
    {
        Class::Reserved
    } else if (s[0] & 0xFE00) == 0xFC00 {
        // fc00::/7, unique-local
        Class::Private
    } else {
        Class::Public
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAN: LanAccess = LanAccess::LocalNetwork;
    const NO_LAN: LanAccess = LanAccess::Refused;

    /// The rule on a device on no local network, so what these assertions say does not depend on the network the
    /// machine running them is on. (It shadows the glob-imported function on purpose.)
    fn is_blocked_address(addr: &SocketAddr) -> bool {
        is_blocked_on(addr, &[], LAN)
    }

    fn network(address: IpAddr, prefix: u8) -> LocalNetwork {
        LocalNetwork { address, prefix }
    }

    /// The reference allows the non-local IPs on the local network: an emulator at 10.0.2.15/24 reaches its host at
    /// 10.0.2.2, but not itself, nor another network's private address.
    #[test]
    fn a_private_address_on_the_local_network_is_allowed_unless_it_is_the_device() {
        let lan = [network(IpAddr::V4(Ipv4Addr::new(10, 0, 2, 15)), 24)];
        assert!(!is_blocked_on(&sa4(10, 0, 2, 2, 8080), &lan, LAN));
        assert!(
            is_blocked_on(&sa4(10, 0, 2, 15, 8080), &lan, LAN),
            "the device itself"
        );
        assert!(
            is_blocked_on(&sa4(10, 0, 3, 1, 8080), &lan, LAN),
            "another network"
        );
        assert!(is_blocked_on(&sa4(192, 168, 1, 1, 80), &lan, LAN));
        // The rest stays refused on any network.
        assert!(is_blocked_on(&sa4(127, 0, 0, 1, 80), &lan, LAN));
        assert!(is_blocked_on(&sa4(169, 254, 169, 254, 80), &lan, LAN));
    }

    /// Without the local network granted, the same peer -- and every other private address, of either family -- is
    /// refused, and a datagram may not be broadcast or multicast.
    #[test]
    fn without_the_local_network_no_private_address_is_reachable() {
        let lan = [
            network(IpAddr::V4(Ipv4Addr::new(10, 0, 2, 15)), 24),
            network(
                IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 0x15)),
                64,
            ),
        ];
        for peer in [
            sa4(10, 0, 2, 2, 8080),
            sa6([0xfd00, 0, 0, 0, 0, 0, 0, 2], 80),
            sa6([0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0202], 80),
        ] {
            assert!(!is_blocked_on(&peer, &lan, LAN), "{peer} granted");
            assert!(is_blocked_on(&peer, &lan, NO_LAN), "{peer} refused");
        }
        assert!(!is_blocked_on(&sa4(8, 8, 8, 8, 443), &lan, NO_LAN));
        for group in [sa4(255, 255, 255, 255, 9), sa4(224, 0, 0, 251, 5353)] {
            assert!(is_blocked_datagram_address(&group, NO_LAN), "{group}");
        }
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
            &lan,
            LAN
        ));
        assert!(
            is_blocked_on(&sa6([0xfd00, 0, 0, 1, 0, 0, 0, 2], 80), &lan, LAN),
            "another /64"
        );
        assert!(
            !is_blocked_on(&sa6([0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0502], 80), &lan, LAN),
            "::ffff:192.168.5.2"
        );
        assert!(
            is_blocked_on(&sa6([0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001], 80), &lan, LAN),
            "::ffff:127.0.0.1"
        );
    }

    /// A datagram may go to the limited broadcast address and to multicast groups when the local network is granted;
    /// a stream may not, and neither may reach the loopback.
    #[test]
    fn a_datagram_may_be_broadcast_or_multicast() {
        for allowed in [
            sa4(255, 255, 255, 255, 9),
            sa4(224, 0, 0, 251, 5353),
            sa6([0xff02, 0, 0, 0, 0, 0, 0, 0xfb], 5353),
        ] {
            assert!(!is_blocked_datagram_address(&allowed, LAN), "{allowed}");
            assert!(
                super::is_blocked_address(&allowed, LAN),
                "a stream to {allowed}"
            );
        }
        assert!(is_blocked_datagram_address(&sa4(127, 0, 0, 1, 9), LAN));
        assert!(!is_blocked_datagram_address(&sa4(8, 8, 8, 8, 53), LAN));
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    /// The device and its neighbours are told by the device's networks, not by the address range. On IPv6 the
    /// device and every device beside it on Wi-Fi have global addresses, and so does a device given a public IPv4:
    /// read by range, an allowed domain pointed at the phone's own address reached its services.
    #[test]
    fn the_device_and_its_neighbours_are_known_by_its_networks_whatever_the_range() {
        let networks = [
            network(ip("2001:470:1:2::15"), 64),
            network(ip("81.2.69.15"), 24),
        ];
        for (address, reach) in [
            ("2001:470:1:2::15", Reach::Never),
            ("2001:470:1:2::99", Reach::LocalNetwork),
            ("2001:470:1:3::1", Reach::Internet),
            ("81.2.69.15", Reach::Never),
            ("81.2.69.20", Reach::LocalNetwork),
            ("81.2.70.1", Reach::Internet),
            ("::ffff:81.2.69.15", Reach::Never),
        ] {
            assert_eq!(reach_on(ip(address), false, &networks), reach, "{address}");
            assert_eq!(
                reach_on(ip(address), true, &networks),
                reach,
                "{address} by datagram"
            );
        }
        assert!(Reach::LocalNetwork.blocked(NO_LAN));
        assert!(!Reach::LocalNetwork.blocked(LAN));
    }

    /// An IPv6 address that carries an IPv4 one is the IPv4 address it carries: a NAT64 gateway or a 6to4 relay
    /// delivers it there. The forms that cannot be read that way are refused outright.
    #[test]
    fn an_address_that_embeds_an_ipv4_one_is_judged_as_it() {
        for (address, reach) in [
            ("64:ff9b::a00:1", Reach::Never),
            ("64:ff9b::c0a8:101", Reach::Never),
            ("64:ff9b::808:808", Reach::Internet),
            ("2002:c0a8:101::1", Reach::Never),
            ("2002:a9fe:a9fe::1", Reach::Never),
            ("2002:808:808::1", Reach::Internet),
            ("::7f00:1", Reach::Never),
            ("::ffff:a9fe:a9fe", Reach::Never),
            ("64:ff9b:1::808:808", Reach::Never),
            ("2001:0:4136:e378:8000:63bf:3fff:fdd2", Reach::Never),
        ] {
            assert_eq!(reach_on(ip(address), false, &[]), reach, "{address}");
        }
    }

    /// The special-purpose ranges no internet host has, at their edges.
    #[test]
    fn special_purpose_ranges_are_never_reached_and_their_neighbours_are() {
        for never in [
            "0.1.2.3",
            "192.0.0.8",
            "192.0.0.255",
            "192.88.99.1",
            "fec0::1",
            "100::1",
            "2001:2::1",
            "2001:10::1",
            "2001:2f:ffff::1",
            "2001:1f::1",
        ] {
            assert_eq!(reach_on(ip(never), false, &[]), Reach::Never, "{never}");
        }
        for internet in [
            "1.0.0.1",
            "192.0.1.1",
            "192.88.98.1",
            "100:0:0:1::1",
            "2001:3::1",
            "2001:30::1",
            "2001:4860:4860::8888",
        ] {
            assert_eq!(
                reach_on(ip(internet), false, &[]),
                Reach::Internet,
                "{internet}"
            );
        }
    }

    /// A network of prefix 0 is not a local network: it would make every address local.
    #[test]
    fn a_zero_length_prefix_makes_nothing_local() {
        let everything = [network(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 0)];
        assert!(is_blocked_on(&sa4(192, 168, 1, 1, 80), &everything, LAN));
    }

    /// Through the device's real interfaces: what does not depend on them.
    #[test]
    fn the_real_rule_refuses_the_loopback_and_allows_the_public() {
        for lan in [NO_LAN, LAN] {
            assert!(super::is_blocked_address(&sa4(127, 0, 0, 1, 80), lan));
            assert!(!super::is_blocked_address(&sa4(8, 8, 8, 8, 443), lan));
        }
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
