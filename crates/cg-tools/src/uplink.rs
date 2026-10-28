//! Which interface carries this computer's own default route: the Wi‑Fi or Ethernet leg the
//! speed test compares the phone with. Read from `/proc/net/route` and `/sys/class/net`;
//! nothing is changed.

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
/// USB tethering) is connected, or this is not Linux.
pub fn physical_uplink() -> Option<String> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    physical_from(&text, OURS, sysfs_driver)
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
}
