//! `Std.Net.interfaceAddresses` (Lean 4.34.0 `lean_uv_interface_addresses`,
//! `src/runtime/uv/net_addr.cpp`, over libuv 1.48's
//! `uv_interface_addresses` for Linux, `src/unix/linux.c`), over
//! `getifaddrs` (nix).
//!
//! The entries, in `getifaddrs`'s order, are the IPv4 and IPv6 addresses of
//! interfaces that are up and running (`IFF_UP` and `IFF_RUNNING`), with
//! the netmask, whether the interface is a loopback one (`IFF_LOOPBACK`),
//! and the hardware address of the interface's link-layer entry (an alias
//! `eth0:1` takes `eth0`'s; zeros when there is none). A failure of
//! `getifaddrs` is Lean's `invalidArgument` "failed to get interface
//! addresses" (`EINVAL`).

use crate::io::IoError;
use nix::ifaddrs::InterfaceAddress as Raw;
use nix::net::if_::InterfaceFlags;
use nix::sys::socket::{AddressFamily, SockaddrLike};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Lean's `Std.Net.InterfaceAddress`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceAddress {
    pub name: String,
    pub physical_address: [u8; 6],
    pub is_loopback: bool,
    pub address: IpAddr,
    pub netmask: IpAddr,
}

/// One `getifaddrs` entry, as plain data.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) up_running: bool,
    pub(crate) loopback: bool,
    /// The entry's address: an IP address, a link-layer address, or another
    /// family (`None` with `other`).
    pub(crate) ip: Option<IpAddr>,
    pub(crate) link: Option<[u8; 6]>,
    pub(crate) has_addr: bool,
    pub(crate) netmask: Option<IpAddr>,
}

fn ip_of(s: &nix::sys::socket::SockaddrStorage) -> Option<IpAddr> {
    if let Some(v4) = s.as_sockaddr_in() {
        return Some(IpAddr::V4(v4.ip()));
    }
    s.as_sockaddr_in6().map(|v6| IpAddr::V6(v6.ip()))
}

fn entry(r: &Raw) -> Entry {
    let f = r.flags;
    let addr = r.address.as_ref();
    let link = addr
        .filter(|a| a.family() == Some(AddressFamily::Packet))
        .and_then(|a| a.as_link_addr())
        .and_then(|l| l.addr());
    Entry {
        name: r.interface_name.clone(),
        up_running: f.contains(InterfaceFlags::IFF_UP) && f.contains(InterfaceFlags::IFF_RUNNING),
        loopback: f.contains(InterfaceFlags::IFF_LOOPBACK),
        ip: addr.and_then(ip_of),
        link: if addr.is_some_and(|a| a.family() == Some(AddressFamily::Packet)) {
            Some(link.unwrap_or([0; 6]))
        } else {
            None
        },
        has_addr: addr.is_some(),
        netmask: r.netmask.as_ref().and_then(ip_of),
    }
}

/// `uv_interface_addresses` and Lean's conversion, on the entries (pure, for
/// the tests): `uv__ifaddr_exclude` keeps up-and-running entries with an
/// address, the IP ones as addresses and the link-layer ones for their
/// hardware address; Lean keeps the IPv4 and IPv6 addresses.
pub(crate) fn from_entries(es: &[Entry]) -> Vec<InterfaceAddress> {
    let mut out: Vec<InterfaceAddress> = Vec::new();
    for e in es {
        // `uv__ifaddr_exclude(ent, UV__EXCLUDE_IFADDR)`
        if !e.up_running || !e.has_addr || e.link.is_some() {
            continue;
        }
        let Some(address) = e.ip else {
            // another family: libuv lists it, Lean skips it
            continue;
        };
        // Lean reads the netmask as an address of the address's family
        let netmask = match (address, e.netmask) {
            (IpAddr::V4(_), Some(IpAddr::V4(m))) => IpAddr::V4(m),
            (IpAddr::V6(_), Some(IpAddr::V6(m))) => IpAddr::V6(m),
            (IpAddr::V4(_), _) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            (IpAddr::V6(_), _) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        out.push(InterfaceAddress {
            name: e.name.clone(),
            physical_address: [0; 6],
            is_loopback: e.loopback,
            address,
            netmask,
        });
    }
    // the hardware addresses: every link-layer entry that is up and running
    // gives its address to the entries of its name, or of an alias of it
    // (`name:...`); a later one wins
    for e in es {
        let Some(mac) = e.link else {
            continue;
        };
        if !e.up_running {
            continue;
        }
        for a in out.iter_mut() {
            let n = e.name.as_bytes();
            let an = a.name.as_bytes();
            if an.starts_with(n) && (an.len() == n.len() || an[n.len()] == b':') {
                a.physical_address = mac;
            }
        }
    }
    out
}

/// `Std.Net.interfaceAddresses` (`lean_uv_interface_addresses`).
pub fn interface_addresses() -> Result<Vec<InterfaceAddress>, IoError> {
    let raw = nix::ifaddrs::getifaddrs().map_err(|_| {
        IoError::InvalidArgument(None, 22, "failed to get interface addresses".to_owned())
    })?;
    let es: Vec<Entry> = raw.map(|r| entry(&r)).collect();
    Ok(from_entries(&es))
}
