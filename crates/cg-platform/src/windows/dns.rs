//! cellglide0's own DNS servers, through `SetInterfaceDnsSettings` (Windows 10 2004+).
//! Only our adapter's fixed GUID is accepted: no NRPT, no other adapter.

use std::io;
use std::net::IpAddr;

use cg_core::tunnel::WINDOWS_ADAPTER_GUID;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, NO_ERROR, WIN32_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    DNS_INTERFACE_SETTINGS, DNS_INTERFACE_SETTINGS_VERSION1, DNS_SETTING_IPV6,
    DNS_SETTING_NAMESERVER, SetInterfaceDnsSettings,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RegDeleteTreeW};
use windows::core::{GUID, PCWSTR, PWSTR};

/// Sets our adapter's servers: the IPv4 ones in one call, the IPv6 ones in a second call
/// with `DNS_SETTING_IPV6`. A family with no servers is cleared, so an empty list is
/// `clear_dns`.
pub fn set_dns(guid: u128, servers: &[IpAddr]) -> io::Result<()> {
    check_guid(guid)?;
    let list = |v6: bool| {
        servers
            .iter()
            .filter(|server| server.is_ipv6() == v6)
            .map(IpAddr::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    set_name_servers(guid, false, &list(false))?;
    set_name_servers(guid, true, &list(true))
}

/// Clears our adapter's servers for both families.
pub fn clear_dns(guid: u128) -> io::Result<()> {
    set_dns(guid, &[])
}

/// Deletes `Tcpip\Parameters\Interfaces\{GUID}` and `Tcpip6\Parameters\Interfaces\{GUID}`
/// for uninstall. A key that is not there is fine.
pub fn remove_interface_keys(guid: u128) -> io::Result<()> {
    check_guid(guid)?;
    for service in ["Tcpip", "Tcpip6"] {
        let key = format!(
            r"SYSTEM\CurrentControlSet\Services\{service}\Parameters\Interfaces\{}",
            braced(guid)
        );
        let key = wide(&key);
        // SAFETY: the path is NUL-terminated and outlives the call.
        let result = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr())) };
        if result != ERROR_FILE_NOT_FOUND {
            win32(result)?;
        }
    }
    Ok(())
}

fn set_name_servers(guid: u128, v6: bool, servers: &str) -> io::Result<()> {
    let mut servers = wide(servers);
    let settings = DNS_INTERFACE_SETTINGS {
        Version: DNS_INTERFACE_SETTINGS_VERSION1,
        Flags: u64::from(DNS_SETTING_NAMESERVER | if v6 { DNS_SETTING_IPV6 } else { 0 }),
        NameServer: PWSTR(servers.as_mut_ptr()),
        ..Default::default()
    };
    // SAFETY: the settings and the server string outlive the call.
    win32(unsafe { SetInterfaceDnsSettings(GUID::from_u128(guid), &settings) })
}

/// Only cellglide0's fixed GUID; anything else is refused before any call.
fn check_guid(guid: u128) -> io::Result<()> {
    if guid != WINDOWS_ADAPTER_GUID {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not cellglide0's GUID", braced(guid)),
        ));
    }
    Ok(())
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`, as the registry names interface keys.
fn braced(guid: u128) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        guid >> 96,
        (guid >> 80) & 0xFFFF,
        (guid >> 64) & 0xFFFF,
        (guid >> 48) & 0xFFFF,
        guid & 0xFFFF_FFFF_FFFF
    )
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win32(result: WIN32_ERROR) -> io::Result<()> {
    if result == NO_ERROR {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result.0 as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_our_key_like_the_registry() {
        assert_eq!(
            braced(WINDOWS_ADAPTER_GUID),
            "{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}"
        );
    }

    #[test]
    fn refuses_any_other_guid_before_calling_anything() {
        let other = WINDOWS_ADAPTER_GUID ^ 1;
        let kind = |result: io::Result<()>| result.unwrap_err().kind();
        assert_eq!(
            kind(set_dns(other, &["198.18.0.1".parse().unwrap()])),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(kind(clear_dns(other)), io::ErrorKind::InvalidInput);
        assert_eq!(
            kind(remove_interface_keys(other)),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(kind(set_dns(0, &[])), io::ErrorKind::InvalidInput);
    }
}
