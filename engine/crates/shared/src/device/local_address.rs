//! The device's address on its local network, as `getLocalIPAddress` reports
//! it: the active network's address, and its netmask. The engine reads it
//! from the operating system on every platform, so every platform answers it
//! the same way and no host has anything to report.
//!
//! The active network is the one the routing table picks, not the first
//! interface listed: a connected UDP socket sends nothing, but the system gives
//! it the source address of the route it would take -- the network the device
//! is on now, not a VPN's tunnel, a container bridge or an idle cellular link
//! that happens to come first. The destination is an address reserved for
//! documentation (RFC 5737, RFC 3849), which only a default route reaches and
//! nothing answers.
//!
//! The answer is a LAN address, so IPv4 where the active network has one: the
//! IPv4 route's source; or, on a network whose only default route is IPv6 --
//! which some networks are, with an IPv4 address on the same link all the
//! same -- the IPv4 address of the interface the IPv6 route leaves by. An IPv6
//! address only on a network with no IPv4 at all.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Where the device is on its local network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAddress {
    pub address: IpAddr,
    pub netmask: IpAddr,
}

impl LocalAddress {
    /// `{"localip", "netmask"}`, the API's result.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"localip":"{}","netmask":"{}"}}"#,
            self.address, self.netmask
        )
    }
}

/// One address of one of the system's interfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InterfaceAddress {
    /// The interface, by the system's name or index for it.
    interface: String,
    address: IpAddr,
    prefix: u8,
}

/// One network the device is on: an address one of its interfaces has, and
/// the length of that interface's prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNetwork {
    pub address: IpAddr,
    pub prefix: u8,
}

impl LocalNetwork {
    /// Whether `ip` is on this network: the same family, and the same first
    /// `prefix` bits.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.address, ip) {
            (IpAddr::V4(own), IpAddr::V4(other)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix.min(32)))
                    .unwrap_or(0);
                u32::from(own) & mask == u32::from(other) & mask
            }
            (IpAddr::V6(own), IpAddr::V6(other)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix.min(128)))
                    .unwrap_or(0);
                u128::from(own) & mask == u128::from(other) & mask
            }
            _ => false,
        }
    }
}

/// How old the list [`networks`] answers may be. A datagram path asks for every
/// datagram and listing interfaces is a system call, while a device moves
/// between networks seconds apart at the quickest.
const NETWORKS_MAX_AGE: Duration = Duration::from_secs(1);

/// Every network the device is on, from every address of every interface --
/// what decides whether an address is on its local network or is the device
/// itself. Empty where the system does not say. At most [`NETWORKS_MAX_AGE`] old.
pub fn networks() -> Arc<[LocalNetwork]> {
    static LISTED: Mutex<Option<(Instant, Arc<[LocalNetwork]>)>> = Mutex::new(None);
    let now = Instant::now();
    // Held across the listing, so callers arriving together list once.
    let mut listed = LISTED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, networks)) = listed.as_ref()
        && now.duration_since(*at) < NETWORKS_MAX_AGE
    {
        return Arc::clone(networks);
    }
    let networks: Arc<[LocalNetwork]> = os::addresses()
        .into_iter()
        .map(|entry| LocalNetwork {
            address: entry.address,
            prefix: entry.prefix,
        })
        .collect();
    *listed = Some((now, Arc::clone(&networks)));
    networks
}

/// The device's local address, or why it has none.
pub fn query() -> Result<LocalAddress, String> {
    let v4 = routed(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 9)),
    );
    let v6 = routed(
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 9)),
    );
    if let (Err(error), Err(_)) = (&v4, &v6) {
        return Err(format!("the device is on no network ({error})"));
    }
    let addresses = os::addresses();
    let chosen = choose(v4.ok(), v6.ok(), &addresses)
        .ok_or_else(|| "no network interface has the address the system routes from".to_owned())?;
    Ok(LocalAddress {
        address: chosen.address,
        netmask: netmask(chosen.address, chosen.prefix),
    })
}

/// The active network's LAN address among `addresses`, given the source
/// addresses of the IPv4 and IPv6 default routes, either of which may be
/// absent.
fn choose(
    v4: Option<IpAddr>,
    v6: Option<IpAddr>,
    addresses: &[InterfaceAddress],
) -> Option<&InterfaceAddress> {
    let owner = |address: IpAddr| addresses.iter().find(|entry| entry.address == address);
    if let Some(found) = v4.and_then(owner) {
        return Some(found);
    }
    let found = v6.and_then(owner)?;
    addresses
        .iter()
        .find(|entry| entry.interface == found.interface && entry.address.is_ipv4())
        .or(Some(found))
}

/// The source address the system picks for a datagram from `local` to `to`.
fn routed(local: SocketAddr, to: SocketAddr) -> io::Result<IpAddr> {
    let socket = UdpSocket::bind(local)?;
    socket.connect(to)?;
    let address = socket.local_addr()?.ip();
    if address.is_unspecified() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no source address",
        ));
    }
    Ok(address)
}

/// The netmask of a prefix `prefix` bits long, in `address`'s family.
fn netmask(address: IpAddr, prefix: u8) -> IpAddr {
    match address {
        IpAddr::V4(_) => {
            let ones = u32::from(prefix.min(32));
            IpAddr::V4(Ipv4Addr::from(u32::MAX.checked_shl(32 - ones).unwrap_or(0)))
        }
        IpAddr::V6(_) => {
            let ones = u32::from(prefix.min(128));
            IpAddr::V6(Ipv6Addr::from(
                u128::MAX.checked_shl(128 - ones).unwrap_or(0),
            ))
        }
    }
}

/// How many leading ones `mask` has; `None` when its ones are not contiguous,
/// which no interface's netmask is.
#[cfg(unix)]
fn prefix_of(mask: IpAddr) -> Option<u8> {
    let (bits, width) = match mask {
        IpAddr::V4(mask) => (u128::from(u32::from(mask)) << 96, 32),
        IpAddr::V6(mask) => (u128::from(mask), 128),
    };
    let ones = bits.leading_ones();
    (ones <= width && bits.checked_shl(ones).unwrap_or(0) == 0).then_some(ones as u8)
}

#[cfg(unix)]
mod os {
    use std::ffi::CStr;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::InterfaceAddress;

    /// Every internet address of every interface, from the system's list.
    pub(super) fn addresses() -> Vec<InterfaceAddress> {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs fills `list` with a list that freeifaddrs below
        // releases; nothing read from it outlives this function.
        if unsafe { libc::getifaddrs(&mut list) } != 0 {
            return Vec::new();
        }
        let mut addresses = Vec::new();
        let mut entry = list;
        while !entry.is_null() {
            // SAFETY: an entry of the list getifaddrs returned, alive until
            // freeifaddrs.
            let interface = unsafe { &*entry };
            entry = interface.ifa_next;
            // SAFETY: the entry's address is null or a sockaddr of its family.
            let Some(address) = (unsafe { ip_of(interface.ifa_addr) }) else {
                continue;
            };
            // SAFETY: the mask is null or a sockaddr; it is read in the
            // address's family, since some systems leave the mask's own unset.
            let Some(prefix) =
                (unsafe { mask_of(interface.ifa_netmask, address) }).and_then(super::prefix_of)
            else {
                continue;
            };
            if interface.ifa_name.is_null() {
                continue;
            }
            // SAFETY: a NUL-terminated name, alive until freeifaddrs.
            let name = unsafe { CStr::from_ptr(interface.ifa_name) };
            addresses.push(InterfaceAddress {
                interface: name.to_string_lossy().into_owned(),
                address,
                prefix,
            });
        }
        // SAFETY: `list` is what getifaddrs returned, freed once.
        unsafe { libc::freeifaddrs(list) };
        addresses
    }

    /// The address a sockaddr holds, for the two internet families.
    ///
    /// # Safety
    /// `address` is null or points to a sockaddr of the family it names.
    unsafe fn ip_of(address: *const libc::sockaddr) -> Option<IpAddr> {
        if address.is_null() {
            return None;
        }
        match i32::from(unsafe { (*address).sa_family }) {
            libc::AF_INET => {
                let address = unsafe { &*address.cast::<libc::sockaddr_in>() };
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                    address.sin_addr.s_addr,
                ))))
            }
            libc::AF_INET6 => {
                let address = unsafe { &*address.cast::<libc::sockaddr_in6>() };
                Some(IpAddr::V6(Ipv6Addr::from(address.sin6_addr.s6_addr)))
            }
            _ => None,
        }
    }

    /// The netmask `mask` holds for an address of `of`'s family.
    ///
    /// Apple's systems size a netmask by its `sa_len`, which stops after the
    /// last non-zero byte, and may leave its family unset: the bytes it has
    /// are copied into a zeroed sockaddr of the address's family and read
    /// from there.
    ///
    /// # Safety
    /// `mask` is null or points to a sockaddr of at least its own length.
    unsafe fn mask_of(mask: *const libc::sockaddr, of: IpAddr) -> Option<IpAddr> {
        if mask.is_null() {
            return None;
        }
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let full = match of {
            IpAddr::V4(_) => std::mem::size_of::<libc::sockaddr_in>(),
            IpAddr::V6(_) => std::mem::size_of::<libc::sockaddr_in6>(),
        };
        #[cfg(target_vendor = "apple")]
        let length = usize::from(unsafe { (*mask).sa_len }).min(full);
        #[cfg(not(target_vendor = "apple"))]
        let length = full;
        // SAFETY: `length` bytes are the mask's own, and `storage` is larger
        // than either sockaddr.
        unsafe {
            std::ptr::copy_nonoverlapping(
                mask.cast::<u8>(),
                std::ptr::addr_of_mut!(storage).cast::<u8>(),
                length,
            );
        }
        let storage = std::ptr::addr_of!(storage);
        Some(match of {
            IpAddr::V4(_) => {
                let mask = unsafe { &*storage.cast::<libc::sockaddr_in>() };
                IpAddr::V4(Ipv4Addr::from(u32::from_be(mask.sin_addr.s_addr)))
            }
            IpAddr::V6(_) => {
                let mask = unsafe { &*storage.cast::<libc::sockaddr_in6>() };
                IpAddr::V6(Ipv6Addr::from(mask.sin6_addr.s6_addr))
            }
        })
    }
}

#[cfg(windows)]
mod os {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6,
    };

    use super::InterfaceAddress;

    /// Every unicast address of every adapter, with its on-link prefix.
    pub(super) fn addresses() -> Vec<InterfaceAddress> {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        // The documented starting size; grown to what the call asks for.
        let mut size: u32 = 15 * 1024;
        let mut buffer: Vec<u64> = Vec::new();
        for _ in 0..4 {
            buffer.resize((size as usize).div_ceil(8), 0);
            // SAFETY: `buffer` is `size` bytes, 8-aligned, as the call requires.
            let result = unsafe {
                GetAdaptersAddresses(
                    u32::from(AF_UNSPEC),
                    flags,
                    std::ptr::null(),
                    buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                    &mut size,
                )
            };
            if result == ERROR_BUFFER_OVERFLOW {
                continue;
            }
            if result != ERROR_SUCCESS {
                return Vec::new();
            }
            let mut addresses = Vec::new();
            let mut adapter = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            while !adapter.is_null() {
                // SAFETY: the adapter list the call wrote into `buffer`.
                let current = unsafe { &*adapter };
                // SAFETY: the union's index form.
                let index = unsafe { current.Anonymous1.Anonymous.IfIndex };
                let mut unicast = current.FirstUnicastAddress;
                while !unicast.is_null() {
                    // SAFETY: as above.
                    let entry = unsafe { &*unicast };
                    // SAFETY: a sockaddr of the family it names.
                    if let Some(address) = unsafe { ip_of(entry.Address.lpSockaddr) } {
                        addresses.push(InterfaceAddress {
                            interface: index.to_string(),
                            address,
                            prefix: entry.OnLinkPrefixLength,
                        });
                    }
                    unicast = entry.Next;
                }
                adapter = current.Next;
            }
            return addresses;
        }
        Vec::new()
    }

    /// # Safety
    /// `address` is null or points to a sockaddr of the family it names.
    unsafe fn ip_of(address: *const SOCKADDR) -> Option<IpAddr> {
        if address.is_null() {
            return None;
        }
        match unsafe { (*address).sa_family } {
            AF_INET => {
                let address = unsafe { &*address.cast::<SOCKADDR_IN>() };
                // SAFETY: the union's address form.
                let raw = unsafe { address.sin_addr.S_un.S_addr };
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(raw))))
            }
            AF_INET6 => {
                let address = unsafe { &*address.cast::<SOCKADDR_IN6>() };
                // SAFETY: the union's byte form.
                Some(IpAddr::V6(Ipv6Addr::from(unsafe {
                    address.sin6_addr.u.Byte
                })))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A network contains the addresses that share its prefix, in its own family, and nothing at a longer distance.
    #[test]
    fn a_network_contains_the_addresses_its_prefix_covers() {
        let lan = LocalNetwork {
            address: "10.0.2.15".parse().unwrap(),
            prefix: 24,
        };
        assert!(lan.contains("10.0.2.2".parse().unwrap()));
        assert!(lan.contains("10.0.2.255".parse().unwrap()));
        assert!(!lan.contains("10.0.3.2".parse().unwrap()));
        assert!(
            !lan.contains("::ffff:10.0.2.2".parse().unwrap()),
            "another family"
        );
        let ula = LocalNetwork {
            address: "fd00::15".parse().unwrap(),
            prefix: 64,
        };
        assert!(ula.contains("fd00::2".parse().unwrap()));
        assert!(!ula.contains("fd00:0:0:1::2".parse().unwrap()));
    }

    /// The system's networks include the loopback's, which every system has.
    #[cfg(unix)]
    #[test]
    fn the_system_lists_its_networks() {
        assert!(
            networks()
                .iter()
                .any(|network| network.address.is_loopback())
        );
    }

    /// A datagram path asks for every datagram, so a second ask within the age
    /// is answered from the first listing rather than by the system again.
    #[test]
    fn networks_are_listed_once_within_their_age() {
        let first = networks();
        assert!(Arc::ptr_eq(&first, &networks()));
    }

    fn at(interface: &str, address: &str, prefix: u8) -> InterfaceAddress {
        InterfaceAddress {
            interface: interface.to_owned(),
            address: address.parse().unwrap(),
            prefix,
        }
    }

    fn chosen(
        v4: Option<&str>,
        v6: Option<&str>,
        addresses: &[InterfaceAddress],
    ) -> Option<String> {
        choose(
            v4.map(|a| a.parse().unwrap()),
            v6.map(|a| a.parse().unwrap()),
            addresses,
        )
        .map(|entry| format!("{} {}/{}", entry.interface, entry.address, entry.prefix))
    }

    /// The IPv4 route's source, whichever interface it is on and whatever is
    /// listed before it -- the Linux VM's eth0 over its docker0 bridge.
    #[test]
    fn the_ipv4_route_s_source_is_the_answer() {
        let addresses = [
            at("lo", "127.0.0.1", 8),
            at("docker0", "172.17.0.1", 16),
            at("eth0", "192.168.5.1", 24),
            at("eth0", "fd00::1", 64),
        ];
        assert_eq!(
            chosen(Some("192.168.5.1"), Some("fd00::1"), &addresses).as_deref(),
            Some("eth0 192.168.5.1/24")
        );
    }

    /// A network whose only default route is IPv6, with an IPv4 address on the
    /// same link -- the Android emulator's Wi-Fi: the LAN address is that IPv4
    /// address, not the IPv6 one and not another interface's.
    #[test]
    fn an_ipv6_only_route_answers_its_interface_s_ipv4_address() {
        let addresses = [
            at("eth0", "10.0.2.15", 24),
            at("wlan0", "10.0.2.16", 24),
            at("wlan0", "fe80::a8ce:ab04:67d6:2ac0", 64),
            at("wlan0", "fec0::d1b1:b2ae:62e0:1cc5", 64),
        ];
        assert_eq!(
            chosen(None, Some("fec0::d1b1:b2ae:62e0:1cc5"), &addresses).as_deref(),
            Some("wlan0 10.0.2.16/24")
        );
    }

    /// No IPv4 on the active network at all: its IPv6 address. A route no
    /// interface owns, or none at all: nothing.
    #[test]
    fn an_ipv6_only_network_answers_ipv6_and_an_unowned_route_nothing() {
        let addresses = [at("en0", "2001:db8:1::5", 64), at("en1", "192.168.1.9", 24)];
        assert_eq!(
            chosen(None, Some("2001:db8:1::5"), &addresses).as_deref(),
            Some("en0 2001:db8:1::5/64")
        );
        assert_eq!(chosen(Some("10.9.9.9"), None, &addresses), None);
        assert_eq!(chosen(None, None, &addresses), None);
    }

    #[test]
    fn a_prefix_is_its_netmask_in_the_address_s_family() {
        let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(netmask(v4, 24).to_string(), "255.255.255.0");
        assert_eq!(netmask(v4, 20).to_string(), "255.255.240.0");
        assert_eq!(netmask(v4, 32).to_string(), "255.255.255.255");
        assert_eq!(netmask(v4, 0).to_string(), "0.0.0.0");
        assert_eq!(netmask(v6, 64).to_string(), "ffff:ffff:ffff:ffff::");
        assert_eq!(netmask(v6, 0).to_string(), "::");
    }

    #[cfg(unix)]
    #[test]
    fn a_netmask_is_its_prefix_when_its_ones_lead() {
        assert_eq!(prefix_of("255.255.255.0".parse().unwrap()), Some(24));
        assert_eq!(prefix_of("255.255.255.255".parse().unwrap()), Some(32));
        assert_eq!(prefix_of("0.0.0.0".parse().unwrap()), Some(0));
        assert_eq!(
            prefix_of("ffff:ffff:ffff:ffff::".parse().unwrap()),
            Some(64)
        );
        assert_eq!(prefix_of("255.0.255.0".parse().unwrap()), None);
    }

    /// On this machine: an interface's own address, neither loopback nor
    /// unspecified, with a netmask of its family -- or, on a machine with no
    /// network at all, the answer says so.
    #[test]
    fn the_device_s_address_is_an_interface_s() {
        match query() {
            Ok(local) => {
                assert!(
                    !local.address.is_loopback() && !local.address.is_unspecified(),
                    "{local:?}"
                );
                assert_eq!(local.address.is_ipv4(), local.netmask.is_ipv4());
                assert!(
                    os::addresses()
                        .iter()
                        .any(|entry| entry.address == local.address)
                );
                let json: serde_json::Value = serde_json::from_str(&local.to_json()).unwrap();
                assert_eq!(json["localip"], local.address.to_string());
                assert_eq!(json["netmask"], local.netmask.to_string());
            }
            Err(reason) => assert!(
                reason.starts_with("the device is on no network"),
                "{reason}"
            ),
        }
    }
}
