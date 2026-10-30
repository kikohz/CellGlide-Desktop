//! The speed test's Wi‑Fi/Ethernet leg on this machine: the uplink chosen from the route table
//! is bound by its (possibly localized) alias, and the connection really leaves from that
//! interface's address. Needs no administrator, but needs this machine's Wi‑Fi or Ethernet
//! online; it opens one TLS connection to Cloudflare and sends no request, never through the
//! phone. Run with `cargo test -p cg-tools --test windows_bind -- --ignored --nocapture`.
#![cfg(windows)]

use cg_tools::speed::{Route, Server};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use windows::Win32::Foundation::NO_ERROR;
use windows::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceAliasToLuid, FreeMibTable, GetUnicastIpAddressTable, MIB_UNICASTIPADDRESS_TABLE,
};
use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC};
use windows::core::PCWSTR;

#[tokio::test]
#[ignore]
async fn binds_by_localized_alias() {
    let alias = cg_tools::uplink::physical_uplink().expect("a physical uplink");
    eprintln!("uplink: {alias}");
    let stream = cg_tools::speed::connect(&Route::Interface(alias.clone()), &Server::cloudflare())
        .await
        .unwrap();
    let local = stream.local_addr().unwrap().ip();
    eprintln!("local address: {local}");
    assert!(
        addresses_of(&alias).contains(&local),
        "{local} not on {alias}"
    );
}

/// The unicast addresses (`GetUnicastIpAddressTable`) on the interface named `alias`.
fn addresses_of(alias: &str) -> Vec<IpAddr> {
    let wide: Vec<u16> = alias.encode_utf16().chain([0]).collect();
    let mut luid = NET_LUID_LH::default();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `luid` is writable.
    let found = unsafe { ConvertInterfaceAliasToLuid(PCWSTR(wide.as_ptr()), &mut luid) };
    assert_eq!(found, NO_ERROR, "{alias} names no interface");
    // SAFETY: NET_LUID_LH is a union over one u64.
    let luid = unsafe { luid.Value };

    let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
    // SAFETY: on success the table is freed below with FreeMibTable.
    assert_eq!(
        unsafe { GetUnicastIpAddressTable(AF_UNSPEC, &mut table) },
        NO_ERROR
    );
    // SAFETY: the table holds NumEntries rows until FreeMibTable.
    let rows = unsafe {
        std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
    };
    let addresses = rows
        .iter()
        // SAFETY: NET_LUID_LH is a union over one u64.
        .filter(|row| unsafe { row.InterfaceLuid.Value } == luid)
        // SAFETY: si_family says which member of the SOCKADDR_INET union is valid.
        .filter_map(|row| unsafe {
            match row.Address.si_family {
                AF_INET => Some(IpAddr::V4(Ipv4Addr::from(
                    row.Address.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes(),
                ))),
                AF_INET6 => Some(IpAddr::V6(Ipv6Addr::from(
                    row.Address.Ipv6.sin6_addr.u.Byte,
                ))),
                _ => None,
            }
        })
        .collect();
    // SAFETY: the table came from GetUnicastIpAddressTable and `rows` is no longer used.
    unsafe { FreeMibTable(table.cast()) };
    addresses
}
