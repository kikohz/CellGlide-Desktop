//! Strict DNS: while connected, outbound port-53 traffic may only leave through
//! cellglide0 (or stay on the machine). Windows otherwise sends every query to the
//! physical adapters' DNS servers in parallel with ours (W0 Task 5).
//!
//! The rules live in a dynamic WFP session (`FWPM_SESSION_FLAG_DYNAMIC`): BFE deletes
//! them when the engine handle closes, which `Drop` does and which a dead process does
//! by itself. Nothing persistent is ever added, and no adapter's settings change.

use std::io;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FWP_ACTION_BLOCK, FWP_ACTION_PERMIT, FWP_ACTION_TYPE, FWP_CONDITION_FLAG_IS_LOOPBACK,
    FWP_CONDITION_VALUE0, FWP_CONDITION_VALUE0_0, FWP_MATCH_EQUAL, FWP_MATCH_FLAGS_ALL_SET,
    FWP_UINT8, FWP_UINT16, FWP_UINT32, FWP_UINT64, FWP_VALUE0, FWP_VALUE0_0, FWPM_CONDITION_FLAGS,
    FWPM_CONDITION_IP_LOCAL_INTERFACE, FWPM_CONDITION_IP_REMOTE_PORT, FWPM_DISPLAY_DATA0,
    FWPM_FILTER_CONDITION0, FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT, FWPM_FILTER_FLAG_NONE,
    FWPM_FILTER_FLAGS, FWPM_FILTER0, FWPM_LAYER_ALE_AUTH_CONNECT_V4,
    FWPM_LAYER_ALE_AUTH_CONNECT_V6, FWPM_SESSION_FLAG_DYNAMIC, FWPM_SESSION0, FWPM_SUBLAYER0,
    FwpmEngineClose0, FwpmEngineOpen0, FwpmFilterAdd0, FwpmSubLayerAdd0, FwpmTransactionAbort0,
    FwpmTransactionBegin0, FwpmTransactionCommit0,
};
use windows::Win32::System::Rpc::RPC_C_AUTHN_WINNT;
use windows::core::{GUID, PCWSTR, PWSTR};

/// Our sublayer. Only ever added inside a dynamic session.
pub const SUBLAYER_GUID: u128 = 0xc151ed32_5a98_401f_b5ca_06705db34eba;

/// Sublayer weight: evaluated before the default sublayers. Arbitration across
/// sublayers still lets any sublayer's block win, so this only orders evaluation.
const SUBLAYER_WEIGHT: u16 = u16::MAX;
/// Filter weights inside our sublayer (`FWP_UINT8`, 0-15): the highest matching wins.
const WEIGHT_OUR_INTERFACE: u8 = 15;
const WEIGHT_LOOPBACK: u8 = 14;
const WEIGHT_BLOCK: u8 = 0;

const DNS_PORT: u16 = 53;

/// The strict DNS rules; dropping it removes them.
pub struct StrictDns {
    engine: HANDLE,
}

// SAFETY: a WFP engine handle may be used and closed from any thread.
unsafe impl Send for StrictDns {}

impl std::fmt::Debug for StrictDns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrictDns").finish_non_exhaustive()
    }
}

impl Drop for StrictDns {
    fn drop(&mut self) {
        // SAFETY: the handle came from FwpmEngineOpen0 and is closed once. Closing a
        // dynamic session deletes the sublayer and filters it added.
        let result = unsafe { FwpmEngineClose0(self.engine) };
        if result != 0 {
            tracing::warn!("closing the strict DNS session: {}", wfp_error(result));
        }
    }
}

/// Opens a dynamic WFP session and, in one transaction, adds our sublayer and, on
/// `ALE_AUTH_CONNECT_V4` and `_V6`: permit `IP_LOCAL_INTERFACE == our_luid`, permit
/// loopback, block remote port 53 (any protocol).
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
    let strict = StrictDns { engine };
    // SAFETY: the engine handle is open.
    check(
        unsafe { FwpmTransactionBegin0(engine, 0) },
        "FwpmTransactionBegin0",
    )?;
    match add_rules(engine, our_luid) {
        Ok(()) => {
            // SAFETY: as above, inside the transaction we began.
            check(
                unsafe { FwpmTransactionCommit0(engine) },
                "FwpmTransactionCommit0",
            )?;
            Ok(strict)
        }
        Err(error) => {
            // SAFETY: as above.
            let _ = unsafe { FwpmTransactionAbort0(engine) };
            Err(error)
        }
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
    let mut our_interface = [FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_IP_LOCAL_INTERFACE,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT64,
            Anonymous: FWP_CONDITION_VALUE0_0 {
                uint64: &raw mut luid,
            },
        },
    }];
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
    let mut dns_port = [FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_IP_REMOTE_PORT,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT16,
            Anonymous: FWP_CONDITION_VALUE0_0 { uint16: DNS_PORT },
        },
    }];

    for layer in [
        FWPM_LAYER_ALE_AUTH_CONNECT_V4,
        FWPM_LAYER_ALE_AUTH_CONNECT_V6,
    ] {
        // Permits are soft (other sublayers' blocks still apply); the block is hard, so
        // no lower sublayer's permit overrides it.
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
            "CellGlide strict DNS: block port 53 elsewhere",
            WEIGHT_BLOCK,
            &mut dns_port,
            FWP_ACTION_BLOCK,
            FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT,
        )?;
    }
    Ok(())
}

fn add_filter(
    engine: HANDLE,
    layer: GUID,
    name: &str,
    weight: u8,
    conditions: &mut [FWPM_FILTER_CONDITION0],
    action: FWP_ACTION_TYPE,
    flags: FWPM_FILTER_FLAGS,
) -> io::Result<()> {
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
    // SAFETY: the filter, its name and its conditions (and what they point to) outlive
    // the call; BFE copies them.
    check(
        unsafe { FwpmFilterAdd0(engine, &raw const filter, None, None) },
        "FwpmFilterAdd0",
    )
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
