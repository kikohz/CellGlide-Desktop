//! Which interface carries this computer's own default route: the Wi‑Fi or Ethernet leg the
//! speed test compares the phone with. Read from `/proc/net/route` and `/sys/class/net` on
//! Linux, from the IP Helper route and interface tables on Windows; nothing is changed.

/// CellGlide's own TUN, never the comparison leg.
pub const OURS: &str = "cellglide0";

/// USB tethering drivers (iPhone personal hotspot, Android RNDIS/NCM/ECM). Their interfaces are
/// hardware, but a comparison through them measures a phone's cellular network again.
pub const TETHERING_DRIVERS: [&str; 4] = ["ipheth", "rndis_host", "cdc_ncm", "cdc_ether"];

const RTF_UP: u32 = 0x1;

/// One default route (`Destination == 0` and `Mask == 0`) that is up.
struct DefaultRoute<'a> {
    iface: &'a str,
    metric: u32,
}

fn defaults<'a>(text: &'a str, ours: &str) -> impl Iterator<Item = DefaultRoute<'a>> {
    text.lines().skip(1).filter_map(move |line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [
            iface,
            destination,
            _gateway,
            flags,
            _refcnt,
            _use,
            metric,
            mask,
            ..,
        ] = cols[..]
        else {
            return None;
        };
        let flags = u32::from_str_radix(flags, 16).ok()?;
        let up = flags & RTF_UP != 0;
        let default = destination == "00000000" && mask == "00000000";
        (up && default && iface != ours).then_some(DefaultRoute {
            iface,
            metric: metric.parse().ok()?,
        })
    })
}

/// The lowest-metric route among `routes`; on a tie the first, as the kernel lists them.
fn lowest<'a>(routes: impl Iterator<Item = DefaultRoute<'a>>) -> Option<String> {
    let mut best: Option<DefaultRoute> = None;
    for row in routes {
        if best.as_ref().is_none_or(|b| row.metric < b.metric) {
            best = Some(row);
        }
    }
    best.map(|b| b.iface.to_string())
}

/// The interface of the up default route with the lowest metric, leaving out `ours`.
pub fn parse_proc_route(text: &str, ours: &str) -> Option<String> {
    lowest(defaults(text, ours))
}

/// Ruling P27: the lowest-metric up default route whose interface is a real Wi‑Fi or Ethernet
/// card. `driver(iface)` gives the basename of `device/driver`, `Some("")` for hardware whose
/// driver link can't be read, and `None` when there's no `device` link at all (tun, wg, veth,
/// bridges). Virtual interfaces and USB tethering are left out before choosing, so a VPN
/// holding the best default route doesn't hide the Wi‑Fi: the bound socket measures the Wi‑Fi
/// itself.
pub fn physical_from(
    text: &str,
    ours: &str,
    driver: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    lowest(defaults(text, ours).filter(|route| {
        driver(route.iface).is_some_and(|d| !TETHERING_DRIVERS.contains(&d.as_str()))
    }))
}

/// The driver of a network interface, from `/sys/class/net/<iface>/device`.
#[cfg(not(windows))]
fn sysfs_driver(iface: &str) -> Option<String> {
    let device = std::path::Path::new("/sys/class/net")
        .join(iface)
        .join("device");
    if !device.exists() {
        return None;
    }
    let driver = std::fs::read_link(device.join("driver")).ok();
    Some(
        driver
            .and_then(|link| link.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default(),
    )
}

/// The Wi‑Fi or Ethernet interface to compare the phone with; `None` when only the phone (or
/// USB tethering) is connected, or this is neither Linux nor Windows.
#[cfg(not(windows))]
pub fn physical_uplink() -> Option<String> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    physical_from(&text, OURS, sysfs_driver)
}

/// USB tethering adapters on Windows, matched case-insensitively anywhere in the adapter's
/// description: the iPhone personal hotspot ("Apple Mobile Device Ethernet"), Android RNDIS
/// ("Remote NDIS based Internet Sharing Device", or "RNDIS" in another name) and Android NCM
/// (Windows 11's inbox driver, "UsbNcm Host Device"). They are hardware Ethernet, but a
/// comparison through them measures a phone's cellular network again.
pub const WINDOWS_TETHERING: [&str; 4] = [
    "Apple Mobile Device Ethernet",
    "Remote NDIS",
    "RNDIS",
    "UsbNcm",
];

/// `IF_TYPE_ETHERNET_CSMACD`
const IF_TYPE_ETHERNET: u32 = 6;
/// `IF_TYPE_IEEE80211` (Wi‑Fi)
const IF_TYPE_WIFI: u32 = 71;

/// One Windows IPv4 default route with what `GetIfEntry2` and `GetIpInterfaceEntry` say about
/// its interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfRoute {
    /// The interface alias, as localized ("Wi-Fi", "WLAN 2", "以太网").
    pub alias: String,
    /// The adapter's driver description ("Intel(R) Wi-Fi 6 AX201").
    pub description: String,
    /// `IFTYPE`: 6 is Ethernet, 71 is Wi‑Fi.
    pub if_type: u32,
    /// `InterfaceAndOperStatusFlags.HardwareInterface`.
    pub hardware: bool,
    /// `OperStatus == IfOperStatusUp`.
    pub up: bool,
    /// Route metric plus interface metric: the sum Windows compares between default routes.
    pub metric: u32,
}

impl IfRoute {
    fn is_tethering(&self) -> bool {
        let description = self.description.to_lowercase();
        WINDOWS_TETHERING
            .iter()
            .any(|t| description.contains(&t.to_lowercase()))
    }
}

/// The Windows comparison leg: among default routes on hardware Ethernet or Wi‑Fi that is up,
/// is not `ours` and is not USB tethering, the alias with the lowest metric; on a tie the
/// first. A VPN holding a better default route doesn't hide the Wi‑Fi, as on Linux.
pub fn physical_from_rows(rows: &[IfRoute], ours: &str) -> Option<String> {
    let mut best: Option<&IfRoute> = None;
    for row in rows.iter().filter(|row| {
        row.up
            && row.hardware
            && matches!(row.if_type, IF_TYPE_ETHERNET | IF_TYPE_WIFI)
            && row.alias != ours
            && !row.is_tethering()
    }) {
        if best.is_none_or(|b| row.metric < b.metric) {
            best = Some(row);
        }
    }
    best.map(|b| b.alias.clone())
}

/// Windows: the IPv4 default routes (`0.0.0.0/0` in `GetIpForwardTable2`), each with its
/// interface from `GetIfEntry2` and `GetIpInterfaceEntry`, through `physical_from_rows`.
#[cfg(windows)]
pub fn physical_uplink() -> Option<String> {
    physical_from_rows(&windows_rows::default_routes()?, OURS)
}

#[cfg(windows)]
mod windows_rows {
    use super::IfRoute;
    use windows::Win32::Foundation::NO_ERROR;
    use windows::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIfEntry2, GetIpForwardTable2, GetIpInterfaceEntry,
        InitializeIpInterfaceEntry, MIB_IF_ROW2, MIB_IPFORWARD_TABLE2, MIB_IPINTERFACE_ROW,
    };
    use windows::Win32::NetworkManagement::Ndis::{IfOperStatusUp, NET_LUID_LH};
    use windows::Win32::Networking::WinSock::AF_INET;

    /// `InterfaceAndOperStatusFlags`: bit 0 is `HardwareInterface`.
    const HARDWARE_INTERFACE: u8 = 0x1;

    /// Every IPv4 default route with its interface; `None` when the route table can't be read.
    /// A route whose interface vanished between the calls is left out.
    pub(super) fn default_routes() -> Option<Vec<IfRoute>> {
        let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
        // SAFETY: on success the function stores a table that `Table` frees once.
        if unsafe { GetIpForwardTable2(AF_INET, &mut table) } != NO_ERROR {
            return None;
        }
        let table = Table(table);
        // SAFETY: the table holds NumEntries rows and lives until `table` drops.
        let routes = unsafe {
            std::slice::from_raw_parts((*table.0).Table.as_ptr(), (*table.0).NumEntries as usize)
        };
        Some(
            routes
                .iter()
                .filter(|route| route.DestinationPrefix.PrefixLength == 0)
                .filter_map(|route| interface(route.InterfaceLuid, route.Metric))
                .collect(),
        )
    }

    fn interface(luid: NET_LUID_LH, route_metric: u32) -> Option<IfRoute> {
        let mut entry = MIB_IF_ROW2 {
            InterfaceLuid: luid,
            ..Default::default()
        };
        // SAFETY: the row is valid and writable; InterfaceLuid selects the interface.
        if unsafe { GetIfEntry2(&mut entry) } != NO_ERROR {
            return None;
        }
        let mut ip = MIB_IPINTERFACE_ROW::default();
        // SAFETY: the row is valid and writable.
        unsafe { InitializeIpInterfaceEntry(&mut ip) };
        ip.Family = AF_INET;
        ip.InterfaceLuid = luid;
        // SAFETY: as above; Family and InterfaceLuid select the row.
        if unsafe { GetIpInterfaceEntry(&mut ip) } != NO_ERROR {
            return None;
        }
        Some(IfRoute {
            // An alias that isn't valid UTF-16 couldn't be bound by name: leave it out.
            alias: String::from_utf16(until_nul(&entry.Alias)).ok()?,
            description: String::from_utf16_lossy(until_nul(&entry.Description)),
            if_type: entry.Type,
            hardware: entry.InterfaceAndOperStatusFlags._bitfield & HARDWARE_INTERFACE != 0,
            up: entry.OperStatus == IfOperStatusUp,
            metric: route_metric.saturating_add(ip.Metric),
        })
    }

    fn until_nul(s: &[u16]) -> &[u16] {
        &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]
    }

    /// The table from `GetIpForwardTable2`, freed with `FreeMibTable`.
    struct Table(*mut MIB_IPFORWARD_TABLE2);

    impl Drop for Table {
        fn drop(&mut self) {
            // SAFETY: the pointer came from GetIpForwardTable2 and is freed once.
            unsafe { FreeMibTable(self.0.cast()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
cellglide0\t00000000\t00000000\t0001\t0\t0\t0\t00000080\t0\t0\t0\n\
wlp2s0\t00000000\t0100A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
enp1s0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";

    #[test]
    fn lowest_metric_physical_default() {
        assert_eq!(
            parse_proc_route(ROUTE, "cellglide0").as_deref(),
            Some("enp1s0")
        );
        assert_eq!(parse_proc_route("Iface\tDestination\n", "cellglide0"), None);
    }

    #[test]
    fn down_routes_and_our_own_tun_are_left_out() {
        let only_ours = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
cellglide0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0\n\
wlp2s0\t00000000\t0100A8C0\t0002\t0\t0\t600\t00000000\t0\t0\t0\n\
wlp2s0\t0000A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(parse_proc_route(only_ours, "cellglide0"), None);
    }

    const HEADER: &str =
        "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n";

    fn table(rows: &[(&str, u32)]) -> String {
        let mut text = HEADER.to_string();
        for (iface, metric) in rows {
            text +=
                &format!("{iface}\t00000000\t0100A8C0\t0003\t0\t0\t{metric}\t00000000\t0\t0\t0\n");
        }
        text
    }

    /// A machine with Wi‑Fi (iwlwifi), Ethernet (r8169), an iPhone hotspot (ipheth), WireGuard
    /// and our TUN.
    fn driver(iface: &str) -> Option<String> {
        match iface {
            "wlp2s0" => Some("iwlwifi".into()),
            "enp1s0" => Some("r8169".into()),
            "eth1" => Some("ipheth".into()),
            "usb0" => Some("rndis_host".into()),
            "enx0" => Some("cdc_ncm".into()),
            "enx1" => Some("cdc_ether".into()),
            _ => None,
        }
    }

    #[test]
    fn iphone_hotspot_with_a_better_metric_is_skipped_for_wifi() {
        let text = table(&[("eth1", 100), ("wlp2s0", 600)]);
        assert_eq!(parse_proc_route(&text, OURS).as_deref(), Some("eth1"));
        assert_eq!(
            physical_from(&text, OURS, driver).as_deref(),
            Some("wlp2s0")
        );
    }

    #[test]
    fn only_usb_tethering_means_no_uplink() {
        let text = table(&[("eth1", 100)]);
        assert_eq!(physical_from(&text, OURS, driver), None);
        let text = table(&[("usb0", 100), ("enx0", 200), ("enx1", 300)]);
        assert_eq!(physical_from(&text, OURS, driver), None);
    }

    #[test]
    fn a_vpn_holding_the_lowest_default_route_still_finds_the_wifi() {
        let text = table(&[("wg0", 0), ("wlp2s0", 600)]);
        assert_eq!(parse_proc_route(&text, OURS).as_deref(), Some("wg0"));
        assert_eq!(
            physical_from(&text, OURS, driver).as_deref(),
            Some("wlp2s0")
        );
    }

    #[test]
    fn our_tun_is_never_chosen() {
        // Even if it looked like hardware.
        let text = table(&[(OURS, 0)]);
        assert_eq!(physical_from(&text, OURS, |_| Some("r8169".into())), None);
        let text = table(&[(OURS, 0), ("enp1s0", 100)]);
        assert_eq!(
            physical_from(&text, OURS, |_| Some("r8169".into())).as_deref(),
            Some("enp1s0")
        );
        assert_eq!(
            physical_from(ROUTE, OURS, driver).as_deref(),
            Some("enp1s0")
        );
    }

    #[test]
    fn hardware_without_a_readable_driver_still_counts() {
        let text = table(&[("enp9s0", 100)]);
        assert_eq!(
            physical_from(&text, OURS, |_| Some(String::new())).as_deref(),
            Some("enp9s0")
        );
    }

    /// A Windows default route on a hardware Wi‑Fi card that is up.
    fn row(alias: &str, metric: u32) -> IfRoute {
        IfRoute {
            alias: alias.into(),
            description: "Intel(R) Wi-Fi 6 AX201".into(),
            if_type: 71,
            hardware: true,
            up: true,
            metric,
        }
    }

    #[test]
    fn picks_the_lowest_metric_physical_route() {
        let vpn = IfRoute {
            hardware: false,
            if_type: 53,
            ..row("Tailscale", 5)
        };
        let ours = IfRoute {
            hardware: false,
            if_type: 53,
            ..row("cellglide0", 0)
        };
        assert_eq!(
            physical_from_rows(&[ours, vpn, row("Wi-Fi", 45), row("以太网", 25)], OURS).as_deref(),
            Some("以太网")
        );
    }

    #[test]
    fn tethering_is_never_the_comparison() {
        let iphone = IfRoute {
            description: "Apple Mobile Device Ethernet".into(),
            if_type: 6,
            ..row("以太网 3", 5)
        };
        let rndis = IfRoute {
            description: "Remote NDIS based Internet Sharing Device".into(),
            ..row("以太网 4", 6)
        };
        // Windows 11's own driver for Android USB tethering (NCM), and an RNDIS gadget
        // named another way.
        let ncm = IfRoute {
            description: "UsbNcm Host Device".into(),
            ..row("以太网 5", 7)
        };
        let gadget = IfRoute {
            description: "USB Ethernet/RNDIS Gadget".into(),
            ..row("以太网 6", 8)
        };
        assert_eq!(
            physical_from_rows(&[iphone, rndis, ncm, gadget, row("WLAN", 50)], OURS).as_deref(),
            Some("WLAN")
        );
    }

    #[test]
    fn down_or_virtual_rows_give_none() {
        let down = IfRoute {
            up: false,
            ..row("Wi-Fi", 10)
        };
        let virtual_eth = IfRoute {
            hardware: false,
            if_type: 6,
            ..row("vEthernet (Default Switch)", 5)
        };
        assert_eq!(physical_from_rows(&[down, virtual_eth], OURS), None);
    }

    #[test]
    fn non_ascii_alias_survives() {
        assert_eq!(
            physical_from_rows(&[row("WLAN 2 无线", 10)], OURS).as_deref(),
            Some("WLAN 2 无线")
        );
    }
}
