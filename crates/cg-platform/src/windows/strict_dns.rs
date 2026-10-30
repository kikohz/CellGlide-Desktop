//! Strict DNS: while connected, outbound port-53 traffic may not leave through a physical
//! adapter (Ethernet, Wi-Fi, Bluetooth PAN, mobile broadband). Windows otherwise sends
//! every query to the physical adapters' DNS servers in parallel with ours (W0 Task 5).
//! Other VPNs and virtual adapters (Tailscale/wintun, WireGuard, OpenVPN TAP, Hyper-V
//! vEthernet, ...) keep their own DNS: AGENTS.md, never change other VPNs (Task 11c).
//!
//! The rules live in a dynamic WFP session (`FWPM_SESSION_FLAG_DYNAMIC`): BFE deletes
//! them when the engine handle closes, which `Drop` does and which a dead process does
//! by itself. Nothing persistent is ever added, and no adapter's settings change.
//!
//! How "physical" is matched (Task 11c, measured on this machine): the block filter uses
//! `FWPM_CONDITION_INTERFACE_TYPE` at `ALE_AUTH_CONNECT`, which WFP accepts there and which
//! matched Wi-Fi (type 71) and not Tailscale (type 53, wintun). The interface type alone
//! can't tell a NIC from a virtual Ethernet adapter, though: OpenVPN TAP, Hyper-V vEthernet
//! and the WAN miniports are type 6 too, and Wi-Fi Direct's virtual adapters type 71. So
//! each adapter of a blocked type that isn't hardware (`HardwareInterface` clear in its
//! `MIB_IF_ROW2`, Bluetooth PAN excepted) gets a permit on its LUID, and
//! `NotifyIpInterfaceChange` keeps those permits in step with adapters that come and go
//! while connected. This way round fails closed: a NIC that appears while connected (a USB
//! Ethernet dongle) is blocked by type the moment it exists, with no window before a
//! per-LUID block could be added; only a new virtual adapter waits for the notification.
//!
//! Why the block still drops silently (Task 11c, measured): no user-mode WFP action makes a
//! UDP send fail. With a hard block at `ALE_AUTH_CONNECT`, `OUTBOUND_TRANSPORT` or
//! `DATAGRAM_DATA`, `sendto` succeeds (unconnected or connected, first or later sends) and
//! the receive just times out; only TCP fails at once (`WSAEACCES` at `ALE_AUTH_CONNECT`).
//! The DNS client queries over UDP and, after a negative answer from cellglide0, waits for
//! the physical adapters' servers to time out, so a name that doesn't exist still takes
//! about 12 s while connected. `ALE_AUTH_CONNECT` is kept: it is no slower than the other
//! layers for UDP and it refuses TCP at once.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io;
use std::sync::{Mutex, MutexGuard};

use windows::Win32::Foundation::{HANDLE, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, FreeMibTable, GetIfTable2, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
    IF_TYPE_WWANPP, IF_TYPE_WWANPP2, MIB_IF_ROW2, MIB_IF_TABLE2, MIB_IPINTERFACE_ROW,
    MIB_NOTIFICATION_TYPE, MibAddInstance, MibDeleteInstance, NotifyIpInterfaceChange,
};
use windows::Win32::NetworkManagement::Ndis::{NDIS_PHYSICAL_MEDIUM, NdisPhysicalMediumBluetooth};
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FWP_ACTION_BLOCK, FWP_ACTION_PERMIT, FWP_ACTION_TYPE, FWP_CONDITION_FLAG_IS_LOOPBACK,
    FWP_CONDITION_VALUE0, FWP_CONDITION_VALUE0_0, FWP_MATCH_EQUAL, FWP_MATCH_FLAGS_ALL_SET,
    FWP_UINT8, FWP_UINT16, FWP_UINT32, FWP_UINT64, FWP_VALUE0, FWP_VALUE0_0, FWPM_CONDITION_FLAGS,
    FWPM_CONDITION_INTERFACE_TYPE, FWPM_CONDITION_IP_LOCAL_INTERFACE,
    FWPM_CONDITION_IP_REMOTE_PORT, FWPM_DISPLAY_DATA0, FWPM_FILTER_CONDITION0,
    FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT, FWPM_FILTER_FLAG_NONE, FWPM_FILTER_FLAGS, FWPM_FILTER0,
    FWPM_LAYER_ALE_AUTH_CONNECT_V4, FWPM_LAYER_ALE_AUTH_CONNECT_V6, FWPM_SESSION_FLAG_DYNAMIC,
    FWPM_SESSION0, FWPM_SUBLAYER0, FwpmEngineClose0, FwpmEngineOpen0, FwpmFilterAdd0,
    FwpmFilterDeleteById0, FwpmSubLayerAdd0, FwpmTransactionAbort0, FwpmTransactionBegin0,
    FwpmTransactionCommit0,
};
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::System::Rpc::RPC_C_AUTHN_WINNT;
use windows::core::{GUID, PCWSTR, PWSTR};

/// Our sublayer. Only ever added inside a dynamic session.
pub const SUBLAYER_GUID: u128 = 0xc151ed32_5a98_401f_b5ca_06705db34eba;

/// Interface types whose port-53 traffic is blocked: Ethernet, Wi-Fi, mobile broadband.
pub const BLOCKED_TYPES: [u32; 4] = [
    IF_TYPE_ETHERNET_CSMACD,
    IF_TYPE_IEEE80211,
    IF_TYPE_WWANPP,
    IF_TYPE_WWANPP2,
];

/// Sublayer weight: the highest, so ours is evaluated first. With our block hard
/// (`CLEAR_ACTION_RIGHT`), no lower sublayer's permit, hard or soft, can override it.
const SUBLAYER_WEIGHT: u16 = u16::MAX;
/// Filter weights inside our sublayer (`FWP_UINT8`, 0-15): the highest matching wins.
const WEIGHT_OUR_INTERFACE: u8 = 15;
const WEIGHT_LOOPBACK: u8 = 14;
const WEIGHT_VIRTUAL: u8 = 13;
const WEIGHT_BLOCK: u8 = 0;

const LAYERS: [GUID; 2] = [
    FWPM_LAYER_ALE_AUTH_CONNECT_V4,
    FWPM_LAYER_ALE_AUTH_CONNECT_V6,
];

const DNS_PORT: u16 = 53;

/// Whether an adapter of a blocked type is exempt (gets a permit): virtual, i.e. not
/// hardware, but a Bluetooth PAN link is a real network path and stays blocked. Filter
/// interfaces (lightweight filters stacked on a NIC) never carry IP traffic; skipped.
pub fn exempt(if_type: u32, hardware: bool, filter: bool, medium: NDIS_PHYSICAL_MEDIUM) -> bool {
    BLOCKED_TYPES.contains(&if_type)
        && !hardware
        && !filter
        && medium != NdisPhysicalMediumBluetooth
}

/// The strict DNS rules; dropping it removes them.
pub struct StrictDns {
    /// Boxed so the address handed to `NotifyIpInterfaceChange` stays put.
    rules: Box<Mutex<Rules>>,
    notify: HANDLE,
}

// SAFETY: a WFP engine handle and a notification handle may be used and closed from any
// thread; the rules are behind a mutex.
unsafe impl Send for StrictDns {}

impl std::fmt::Debug for StrictDns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrictDns").finish_non_exhaustive()
    }
}

impl StrictDns {
    /// The LUIDs that currently have a permit as virtual adapters (for tests).
    pub fn exempt_luids(&self) -> Vec<u64> {
        lock(&self.rules).exempt.keys().copied().collect()
    }
}

impl Drop for StrictDns {
    fn drop(&mut self) {
        if !self.notify.is_invalid() {
            // SAFETY: the handle came from NotifyIpInterfaceChange and is cancelled once;
            // the call waits for a running callback, so `rules` outlives every callback.
            let result = unsafe { CancelMibChangeNotify2(self.notify) };
            if result != NO_ERROR {
                tracing::warn!(
                    "cancelling the strict DNS interface notification: {}",
                    io::Error::from_raw_os_error(result.0 as i32)
                );
            }
        }
        let engine = lock(&self.rules).engine;
        // SAFETY: the handle came from FwpmEngineOpen0 and is closed once. Closing a
        // dynamic session deletes the sublayer and filters it added.
        let result = unsafe { FwpmEngineClose0(engine) };
        if result != 0 {
            tracing::warn!("closing the strict DNS session: {}", wfp_error(result));
        }
    }
}

/// The engine and the permits for virtual adapters, by LUID (their filter IDs).
struct Rules {
    engine: HANDLE,
    our_luid: u64,
    exempt: BTreeMap<u64, Vec<u64>>,
}

// SAFETY: see StrictDns.
unsafe impl Send for Rules {}

impl Rules {
    /// Adds permits for virtual adapters of a blocked type that lack one and deletes the
    /// permits of adapters that are gone or no longer exempt.
    fn sync(&mut self) -> io::Result<()> {
        let want = exempt_adapters(self.our_luid)?;
        let engine = self.engine;
        self.exempt.retain(|luid, ids| {
            if want.contains(luid) {
                return true;
            }
            for id in ids.iter() {
                // SAFETY: the engine is open; the ID came from FwpmFilterAdd0 on it.
                let result = unsafe { FwpmFilterDeleteById0(engine, *id) };
                if result != 0 {
                    tracing::warn!("deleting strict DNS permit {id}: {}", wfp_error(result));
                }
            }
            false
        });
        for luid in want {
            if self.exempt.contains_key(&luid) {
                continue;
            }
            let mut ids = Vec::new();
            let mut value = luid;
            let mut condition = [luid_condition(&mut value)];
            let mut result = Ok(());
            for layer in LAYERS {
                match add_filter(
                    engine,
                    layer,
                    "CellGlide strict DNS: permit a virtual adapter",
                    WEIGHT_VIRTUAL,
                    &mut condition,
                    FWP_ACTION_PERMIT,
                    FWPM_FILTER_FLAG_NONE,
                ) {
                    Ok(id) => ids.push(id),
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                }
            }
            self.exempt.insert(luid, ids);
            result?;
        }
        Ok(())
    }
}

fn lock(rules: &Mutex<Rules>) -> MutexGuard<'_, Rules> {
    rules
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Opens a dynamic WFP session and, in one transaction, adds our sublayer and, on
/// `ALE_AUTH_CONNECT_V4` and `_V6`: permit `IP_LOCAL_INTERFACE == our_luid`, permit
/// loopback, permit each exempt (virtual) adapter's LUID, and block remote port 53 (any
/// protocol) on the blocked interface types. Then follows adapters coming and going.
pub fn start(our_luid: u64) -> io::Result<StrictDns> {
    let mut name = wide("CellGlide strict DNS");
    let session = FWPM_SESSION0 {
        displayData: FWPM_DISPLAY_DATA0 {
            name: PWSTR(name.as_mut_ptr()),
            description: PWSTR::null(),
        },
        flags: FWPM_SESSION_FLAG_DYNAMIC,
        ..Default::default()
    };
    let mut engine = HANDLE::default();
    // SAFETY: the session and its name outlive the call; `engine` receives the handle.
    let result = unsafe {
        FwpmEngineOpen0(
            PCWSTR::null(),
            RPC_C_AUTHN_WINNT,
            None,
            Some(&raw const session),
            &raw mut engine,
        )
    };
    check(result, "FwpmEngineOpen0")?;
    // From here on an early return closes the engine, which also drops anything added.
    let mut strict = StrictDns {
        rules: Box::new(Mutex::new(Rules {
            engine,
            our_luid,
            exempt: BTreeMap::new(),
        })),
        notify: HANDLE::default(),
    };
    // SAFETY: the engine handle is open.
    check(
        unsafe { FwpmTransactionBegin0(engine, 0) },
        "FwpmTransactionBegin0",
    )?;
    let added = add_rules(engine, our_luid).and_then(|()| lock(&strict.rules).sync());
    if let Err(error) = added {
        // SAFETY: as above, inside the transaction we began.
        let _ = unsafe { FwpmTransactionAbort0(engine) };
        return Err(error);
    }
    // SAFETY: as above.
    check(
        unsafe { FwpmTransactionCommit0(engine) },
        "FwpmTransactionCommit0",
    )?;

    let context: *const Mutex<Rules> = &raw const *strict.rules;
    let mut notify = HANDLE::default();
    // SAFETY: the context points into the box, which outlives the registration (Drop
    // cancels it first); the callback matches PIPINTERFACE_CHANGE_CALLBACK.
    let result = unsafe {
        NotifyIpInterfaceChange(
            AF_UNSPEC,
            Some(on_interface_change),
            Some(context.cast()),
            false,
            &raw mut notify,
        )
    };
    if result == NO_ERROR {
        strict.notify = notify;
    } else {
        tracing::warn!(
            "strict DNS won't follow new virtual adapters (NotifyIpInterfaceChange: {})",
            io::Error::from_raw_os_error(result.0 as i32)
        );
    }
    // An adapter that came between the first sync and the registration.
    if let Err(error) = lock(&strict.rules).sync() {
        tracing::warn!("strict DNS: updating the virtual adapter permits: {error}");
    }
    Ok(strict)
}

unsafe extern "system" fn on_interface_change(
    context: *const c_void,
    _row: *const MIB_IPINTERFACE_ROW,
    kind: MIB_NOTIFICATION_TYPE,
) {
    if kind != MibAddInstance && kind != MibDeleteInstance {
        return;
    }
    // SAFETY: the context is the boxed mutex, alive until the registration is cancelled.
    let rules = unsafe { &*context.cast::<Mutex<Rules>>() };
    if let Err(error) = lock(rules).sync() {
        tracing::warn!("strict DNS: updating the virtual adapter permits: {error}");
    }
}

/// The LUIDs of exempt adapters, ours aside.
fn exempt_adapters(our_luid: u64) -> io::Result<Vec<u64>> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    // SAFETY: on success the function stores a table we free with FreeMibTable.
    let result = unsafe { GetIfTable2(&raw mut table) };
    if result != NO_ERROR {
        return Err(io::Error::other(format!(
            "GetIfTable2: {}",
            io::Error::from_raw_os_error(result.0 as i32)
        )));
    }
    struct Table(*mut MIB_IF_TABLE2);
    impl Drop for Table {
        fn drop(&mut self) {
            // SAFETY: the table came from GetIfTable2 and is freed once.
            unsafe { FreeMibTable(self.0.cast()) };
        }
    }
    let table = Table(table);
    // SAFETY: the table holds NumEntries rows and lives until `table` drops.
    let rows: &[MIB_IF_ROW2] = unsafe {
        std::slice::from_raw_parts((*table.0).Table.as_ptr(), (*table.0).NumEntries as usize)
    };
    Ok(rows
        .iter()
        .filter(|row| {
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            exempt(
                row.Type,
                flags & 0x1 != 0,
                flags & 0x2 != 0,
                row.PhysicalMediumType,
            )
        })
        // SAFETY: NET_LUID_LH is a union over one u64.
        .map(|row| unsafe { row.InterfaceLuid.Value })
        .filter(|luid| *luid != our_luid)
        .collect())
}

fn luid_condition(luid: &mut u64) -> FWPM_FILTER_CONDITION0 {
    FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_IP_LOCAL_INTERFACE,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT64,
            Anonymous: FWP_CONDITION_VALUE0_0 { uint64: luid },
        },
    }
}

fn add_rules(engine: HANDLE, our_luid: u64) -> io::Result<()> {
    let mut name = wide("CellGlide strict DNS");
    let sublayer = FWPM_SUBLAYER0 {
        subLayerKey: GUID::from_u128(SUBLAYER_GUID),
        displayData: FWPM_DISPLAY_DATA0 {
            name: PWSTR(name.as_mut_ptr()),
            description: PWSTR::null(),
        },
        weight: SUBLAYER_WEIGHT,
        ..Default::default()
    };
    // SAFETY: the sublayer and its name outlive the call.
    check(
        unsafe { FwpmSubLayerAdd0(engine, &raw const sublayer, None) },
        "FwpmSubLayerAdd0",
    )?;

    let mut luid = our_luid;
    let mut our_interface = [luid_condition(&mut luid)];
    let mut loopback = [FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_FLAGS,
        matchType: FWP_MATCH_FLAGS_ALL_SET,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT32,
            Anonymous: FWP_CONDITION_VALUE0_0 {
                uint32: FWP_CONDITION_FLAG_IS_LOOPBACK,
            },
        },
    }];
    // Conditions on the same field are ORed, different fields ANDed: port 53 and one of
    // the blocked interface types.
    let mut physical_dns = vec![FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_IP_REMOTE_PORT,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT16,
            Anonymous: FWP_CONDITION_VALUE0_0 { uint16: DNS_PORT },
        },
    }];
    physical_dns.extend(BLOCKED_TYPES.map(|if_type| FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_INTERFACE_TYPE,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT32,
            Anonymous: FWP_CONDITION_VALUE0_0 { uint32: if_type },
        },
    }));

    for layer in LAYERS {
        // Permits are soft (other sublayers' blocks still apply); the block is hard, so
        // no lower sublayer's permit overrides it. cellglide0 (wintun, type 53) never
        // matches the block; its permit stays as a guard should that ever change.
        add_filter(
            engine,
            layer,
            "CellGlide strict DNS: permit cellglide0",
            WEIGHT_OUR_INTERFACE,
            &mut our_interface,
            FWP_ACTION_PERMIT,
            FWPM_FILTER_FLAG_NONE,
        )?;
        add_filter(
            engine,
            layer,
            "CellGlide strict DNS: permit loopback",
            WEIGHT_LOOPBACK,
            &mut loopback,
            FWP_ACTION_PERMIT,
            FWPM_FILTER_FLAG_NONE,
        )?;
        add_filter(
            engine,
            layer,
            "CellGlide strict DNS: block port 53 on physical adapters",
            WEIGHT_BLOCK,
            &mut physical_dns,
            FWP_ACTION_BLOCK,
            FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT,
        )?;
    }
    Ok(())
}

/// Adds one filter to our sublayer; returns its ID.
fn add_filter(
    engine: HANDLE,
    layer: GUID,
    name: &str,
    weight: u8,
    conditions: &mut [FWPM_FILTER_CONDITION0],
    action: FWP_ACTION_TYPE,
    flags: FWPM_FILTER_FLAGS,
) -> io::Result<u64> {
    let mut name = wide(name);
    let mut filter = FWPM_FILTER0 {
        displayData: FWPM_DISPLAY_DATA0 {
            name: PWSTR(name.as_mut_ptr()),
            description: PWSTR::null(),
        },
        flags,
        layerKey: layer,
        subLayerKey: GUID::from_u128(SUBLAYER_GUID),
        weight: FWP_VALUE0 {
            r#type: FWP_UINT8,
            Anonymous: FWP_VALUE0_0 { uint8: weight },
        },
        numFilterConditions: u32::try_from(conditions.len())
            .map_err(|_| io::Error::other("too many filter conditions"))?,
        filterCondition: conditions.as_mut_ptr(),
        ..Default::default()
    };
    filter.action.r#type = action;
    let mut id = 0u64;
    // SAFETY: the filter, its name and its conditions (and what they point to) outlive
    // the call; BFE copies them.
    check(
        unsafe { FwpmFilterAdd0(engine, &raw const filter, None, Some(&raw mut id)) },
        "FwpmFilterAdd0",
    )?;
    Ok(id)
}

fn check(result: u32, call: &str) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!("{call}: {}", wfp_error(result))))
    }
}

/// WFP calls return Win32 codes or `FWP_E_*` HRESULTs (0x8032xxxx).
fn wfp_error(code: u32) -> String {
    let os = io::Error::from_raw_os_error(code as i32);
    format!("{os} (0x{code:08X})")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::NetworkManagement::Ndis::{
        NdisPhysicalMedium802_3, NdisPhysicalMediumNative802_11, NdisPhysicalMediumUnspecified,
        NdisPhysicalMediumWirelessWan,
    };

    #[test]
    fn nics_stay_blocked() {
        // Wi-Fi, Ethernet, mobile broadband: hardware.
        assert!(!exempt(71, true, false, NdisPhysicalMediumNative802_11));
        assert!(!exempt(6, true, false, NdisPhysicalMedium802_3));
        assert!(!exempt(243, true, false, NdisPhysicalMediumWirelessWan));
        assert!(!exempt(244, true, false, NdisPhysicalMediumWirelessWan));
        // Bluetooth PAN: type 6, not hardware, but a real link.
        assert!(!exempt(6, false, false, NdisPhysicalMediumBluetooth));
    }

    #[test]
    fn virtual_adapters_of_blocked_types_are_exempt() {
        // Hyper-V vEthernet, OpenVPN TAP, WAN miniports: type 6, not hardware.
        assert!(exempt(6, false, false, NdisPhysicalMediumUnspecified));
        // Wi-Fi Direct virtual adapter.
        assert!(exempt(71, false, false, NdisPhysicalMediumNative802_11));
    }

    #[test]
    fn other_types_and_filter_interfaces_need_no_permit() {
        // wintun (Tailscale, WireGuard, cellglide0) is type 53: never blocked.
        assert!(!exempt(53, false, false, NdisPhysicalMediumUnspecified));
        // PPP (built-in VPNs) and tunnel pseudo-interfaces.
        assert!(!exempt(23, false, false, NdisPhysicalMediumUnspecified));
        assert!(!exempt(131, false, false, NdisPhysicalMediumUnspecified));
        // A lightweight filter interface stacked on Wi-Fi.
        assert!(!exempt(71, false, true, NdisPhysicalMediumNative802_11));
        assert_eq!(BLOCKED_TYPES, [6, 71, 243, 244]);
    }
}
