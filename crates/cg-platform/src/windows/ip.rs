//! IP Helper calls on cellglide0, never netsh. Every function takes our adapter's LUID, refuses
//! any LUID whose GUID is not cellglide0's before calling anything, and
//! touches nothing else: `flush` enumerates the system tables and deletes only rows whose
//! `InterfaceLuid` is ours, and no table-wide flush API is ever called.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use cg_core::tunnel::{MTU, WINDOWS_ADAPTER_GUID};
use windows::Win32::Foundation::{ERROR_NOT_FOUND, NO_ERROR, WIN32_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceLuidToGuid, CreateIpForwardEntry2, CreateUnicastIpAddressEntry,
    DeleteIpForwardEntry2, DeleteUnicastIpAddressEntry, FreeMibTable, GetIpForwardTable2,
    GetIpInterfaceEntry, GetUnicastIpAddressTable, InitializeIpForwardEntry,
    InitializeIpInterfaceEntry, InitializeUnicastIpAddressEntry, MIB_IPFORWARD_ROW2,
    MIB_IPFORWARD_TABLE2, MIB_IPINTERFACE_ROW, MIB_UNICASTIPADDRESS_ROW,
    MIB_UNICASTIPADDRESS_TABLE, SetIpInterfaceEntry,
};
use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows::Win32::Networking::WinSock::{
    ADDRESS_FAMILY, AF_INET, AF_INET6, AF_UNSPEC, IN_ADDR, IN_ADDR_0, IN6_ADDR, IN6_ADDR_0,
    IpDadStatePreferred, IpPrefixOriginManual, MIB_IPPROTO_NETMGMT, RouterDiscoveryDisabled,
    SOCKADDR_IN, SOCKADDR_IN6, SOCKADDR_INET,
};
use windows::core::GUID;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    fn address_family(self) -> ADDRESS_FAMILY {
        match self {
            Family::V4 => AF_INET,
            Family::V6 => AF_INET6,
        }
    }
}

/// MTU 1500, interface metric 0 (not automatic), no DAD, no router discovery.
pub fn configure_interface(luid: u64, family: Family) -> io::Result<()> {
    check_luid(luid)?;
    let mut row = MIB_IPINTERFACE_ROW::default();
    // SAFETY: the row is a valid, writable MIB_IPINTERFACE_ROW.
    unsafe { InitializeIpInterfaceEntry(&mut row) };
    row.Family = family.address_family();
    row.InterfaceLuid = NET_LUID_LH { Value: luid };
    // SAFETY: as above; Family and InterfaceLuid identify the row to read.
    win32(unsafe { GetIpInterfaceEntry(&mut row) })?;
    if row_luid(row.InterfaceLuid) != luid {
        return Err(io::Error::other(format!(
            "GetIpInterfaceEntry returned LUID {:#x} for {luid:#x}",
            row_luid(row.InterfaceLuid)
        )));
    }
    row.NlMtu = MTU;
    row.UseAutomaticMetric = false;
    row.Metric = 0;
    row.DadTransmits = 0;
    row.RouterDiscoveryBehavior = RouterDiscoveryDisabled;
    if family == Family::V4 {
        // SetIpInterfaceEntry rejects IPv4 rows with a site prefix length (MSDN).
        row.SitePrefixLength = 0;
    }
    // SAFETY: the row came from GetIpInterfaceEntry for our LUID.
    win32(unsafe { SetIpInterfaceEntry(&mut row) })
}

/// A manual unicast address, usable at once (DAD is off on our interface).
pub fn add_address(luid: u64, address: IpAddr, prefix: u8) -> io::Result<()> {
    if prefix > max_prefix(address) {
        return Err(invalid(format!("prefix /{prefix} too long for {address}")));
    }
    check_luid(luid)?;
    let mut row = MIB_UNICASTIPADDRESS_ROW::default();
    // SAFETY: the row is a valid, writable MIB_UNICASTIPADDRESS_ROW.
    unsafe { InitializeUnicastIpAddressEntry(&mut row) };
    row.InterfaceLuid = NET_LUID_LH { Value: luid };
    row.Address = sockaddr(address);
    row.OnLinkPrefixLength = prefix;
    row.DadState = IpDadStatePreferred;
    // SAFETY: the row is initialized and names our LUID.
    win32(unsafe { CreateUnicastIpAddressEntry(&row) })
}

/// A route to `cidr` on our interface: on-link (unspecified next hop), metric 0.
pub fn add_route(luid: u64, cidr: &str) -> io::Result<()> {
    let (prefix, length) = parse_cidr(cidr).ok_or_else(|| invalid(format!("bad route {cidr}")))?;
    check_luid(luid)?;
    let next_hop = match prefix {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let mut row = MIB_IPFORWARD_ROW2::default();
    // SAFETY: the row is a valid, writable MIB_IPFORWARD_ROW2.
    unsafe { InitializeIpForwardEntry(&mut row) };
    row.InterfaceLuid = NET_LUID_LH { Value: luid };
    row.DestinationPrefix.Prefix = sockaddr(prefix);
    row.DestinationPrefix.PrefixLength = length;
    row.NextHop = sockaddr(next_hop);
    row.Metric = 0;
    // SAFETY: the row is initialized and names our LUID.
    win32(unsafe { CreateIpForwardEntry2(&row) })
}

/// Deletes the routes and addresses we added on our LUID: routes with protocol NetMgmt
/// and addresses with a manual prefix origin. The stack's own routes and link-local
/// addresses on our interface go with the adapter. Keeps going past a failed delete (and
/// past a failed route step, to the addresses) and returns the first error.
pub fn flush(luid: u64) -> io::Result<()> {
    check_luid(luid)?;
    let routes = delete_routes(luid);
    let addresses = delete_addresses(luid);
    routes.and(addresses)
}

/// Deletes only the routes we added on our LUID (protocol NetMgmt), keeping the addresses.
pub fn flush_routes(luid: u64) -> io::Result<()> {
    check_luid(luid)?;
    delete_routes(luid)
}

/// Collects the first failure of a run of deletes. ERROR_NOT_FOUND means already gone
/// (deleting one row can take others with it).
#[derive(Default)]
struct FirstError(Option<io::Error>);

impl FirstError {
    fn note(&mut self, result: WIN32_ERROR) {
        if result != NO_ERROR && result != ERROR_NOT_FOUND && self.0.is_none() {
            self.0 = Some(io::Error::from_raw_os_error(result.0 as i32));
        }
    }

    fn into_result(self) -> io::Result<()> {
        self.0.map_or(Ok(()), Err)
    }
}

/// Only after `check_luid`.
fn delete_routes(luid: u64) -> io::Result<()> {
    let mut routes: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
    // SAFETY: on success the function stores a table we free with FreeMibTable.
    win32(unsafe { GetIpForwardTable2(AF_UNSPEC, &mut routes) })?;
    let routes = MibTable(routes);
    // SAFETY: the table holds NumEntries rows and lives until `routes` drops.
    let rows = unsafe {
        std::slice::from_raw_parts((*routes.0).Table.as_ptr(), (*routes.0).NumEntries as usize)
    };
    let mut first = FirstError::default();
    for row in rows {
        if row_luid(row.InterfaceLuid) != luid || row.Protocol != MIB_IPPROTO_NETMGMT {
            continue;
        }
        // SAFETY: the row came from the system's table and names our LUID.
        first.note(unsafe { DeleteIpForwardEntry2(row) });
    }
    first.into_result()
}

/// Only after `check_luid`.
fn delete_addresses(luid: u64) -> io::Result<()> {
    let mut addresses: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
    // SAFETY: on success the function stores a table we free with FreeMibTable.
    win32(unsafe { GetUnicastIpAddressTable(AF_UNSPEC, &mut addresses) })?;
    let addresses = MibTable(addresses);
    // SAFETY: the table holds NumEntries rows and lives until `addresses` drops.
    let rows = unsafe {
        std::slice::from_raw_parts(
            (*addresses.0).Table.as_ptr(),
            (*addresses.0).NumEntries as usize,
        )
    };
    let mut first = FirstError::default();
    for row in rows {
        if row_luid(row.InterfaceLuid) != luid || row.PrefixOrigin != IpPrefixOriginManual {
            continue;
        }
        // SAFETY: the row came from the system's table and names our LUID.
        first.note(unsafe { DeleteUnicastIpAddressEntry(row) });
    }
    first.into_result()
}

/// `"a.b.c.d/n"` or `"x::/n"` into an address and a prefix length within range.
pub fn parse_cidr(cidr: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = cidr.split_once('/')?;
    let address: IpAddr = address.parse().ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    (prefix <= max_prefix(address)).then_some((address, prefix))
}

fn max_prefix(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

/// Refuses, before any call that could act on it, every LUID that is not cellglide0's:
/// zero, one that names no interface, and any interface whose GUID is not our fixed one.
fn check_luid(luid: u64) -> io::Result<()> {
    if luid == 0 {
        return Err(invalid("zero interface LUID".to_owned()));
    }
    let mut guid = GUID::zeroed();
    // SAFETY: both pointers are valid for the call.
    let found = unsafe { ConvertInterfaceLuidToGuid(&NET_LUID_LH { Value: luid }, &mut guid) };
    if found != NO_ERROR {
        return Err(invalid(format!(
            "LUID {luid:#x} names no interface ({})",
            io::Error::from_raw_os_error(found.0 as i32)
        )));
    }
    if guid.to_u128() != WINDOWS_ADAPTER_GUID {
        return Err(invalid(format!(
            "LUID {luid:#x} is not cellglide0 (GUID {guid:?})"
        )));
    }
    Ok(())
}

fn row_luid(luid: NET_LUID_LH) -> u64 {
    // SAFETY: NET_LUID is a 64-bit union; every bit pattern is a valid u64.
    unsafe { luid.Value }
}

fn sockaddr(address: IpAddr) -> SOCKADDR_INET {
    match address {
        IpAddr::V4(v4) => SOCKADDR_INET {
            Ipv4: SOCKADDR_IN {
                sin_family: AF_INET,
                sin_addr: IN_ADDR {
                    S_un: IN_ADDR_0 {
                        // Network byte order, as the octets already are.
                        S_addr: u32::from_ne_bytes(v4.octets()),
                    },
                },
                ..Default::default()
            },
        },
        IpAddr::V6(v6) => SOCKADDR_INET {
            Ipv6: SOCKADDR_IN6 {
                sin6_family: AF_INET6,
                sin6_addr: IN6_ADDR {
                    u: IN6_ADDR_0 { Byte: v6.octets() },
                },
                ..Default::default()
            },
        },
    }
}

/// A table from `Get*Table2`, freed with `FreeMibTable`.
struct MibTable<T>(*mut T);

impl<T> Drop for MibTable<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from an IP Helper table function.
            unsafe { FreeMibTable(self.0.cast()) };
        }
    }
}

fn win32(result: WIN32_ERROR) -> io::Result<()> {
    if result == NO_ERROR {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result.0 as i32))
    }
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::NetworkManagement::IpHelper::ConvertInterfaceIndexToLuid;

    #[test]
    fn parses_the_tunnel_routes() {
        assert_eq!(
            parse_cidr("0.0.0.0/1"),
            Some(("0.0.0.0".parse().unwrap(), 1))
        );
        assert_eq!(parse_cidr("8000::/1"), Some(("8000::".parse().unwrap(), 1)));
        assert_eq!(parse_cidr("10.0.0.0"), None);
        assert_eq!(parse_cidr("1.2.3.4/33"), None);
    }

    #[test]
    fn parses_every_tunnel_constant() {
        use cg_core::tunnel::{IPV4_ROUTES, IPV6_ROUTES};
        for route in IPV4_ROUTES.iter().chain(&IPV6_ROUTES) {
            assert!(parse_cidr(route).is_some(), "{route}");
        }
        assert_eq!(parse_cidr("::/129"), None);
        assert_eq!(parse_cidr("::/x"), None);
        assert_eq!(parse_cidr("nope/1"), None);
    }

    #[test]
    fn refuses_a_zero_luid_before_calling_anything() {
        let kind = |result: io::Result<()>| result.unwrap_err().kind();
        assert_eq!(
            kind(configure_interface(0, Family::V4)),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            kind(add_address(0, IpAddr::V4(Ipv4Addr::LOCALHOST), 32)),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(kind(add_route(0, "::/1")), io::ErrorKind::InvalidInput);
        assert_eq!(kind(flush(0)), io::ErrorKind::InvalidInput);
    }

    /// The loopback interface (index 1) is real but not ours: refused, by the check alone
    /// (this test never calls a function that could change the loopback interface).
    #[test]
    fn refuses_another_interfaces_luid() {
        let mut loopback = NET_LUID_LH::default();
        // SAFETY: `loopback` is writable.
        win32(unsafe { ConvertInterfaceIndexToLuid(1, &mut loopback) }).unwrap();
        let loopback = row_luid(loopback);
        assert_ne!(loopback, 0);
        let error = check_luid(loopback).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
        assert!(error.to_string().contains("not cellglide0"), "{error}");
    }

    #[test]
    fn refuses_bad_input_before_calling_anything() {
        let kind = |result: io::Result<()>| result.unwrap_err().kind();
        assert_eq!(kind(add_route(1, "10.0.0.0")), io::ErrorKind::InvalidInput);
        assert_eq!(
            kind(add_address(1, IpAddr::V4(Ipv4Addr::LOCALHOST), 33)),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn sockaddr_is_in_network_order() {
        let v4 = sockaddr("198.18.0.1".parse().unwrap());
        // SAFETY: written as Ipv4 just above.
        unsafe {
            assert_eq!(v4.si_family, AF_INET);
            assert_eq!(v4.Ipv4.sin_addr.S_un.S_un_b.s_b1, 198);
            assert_eq!(v4.Ipv4.sin_addr.S_un.S_un_b.s_b4, 1);
        }
        let v6 = sockaddr("fd6c:6779:6465::1".parse().unwrap());
        // SAFETY: written as Ipv6 just above.
        unsafe {
            assert_eq!(v6.si_family, AF_INET6);
            assert_eq!(v6.Ipv6.sin6_addr.u.Byte[0], 0xfd);
            assert_eq!(v6.Ipv6.sin6_addr.u.Byte[15], 1);
        }
    }
}
