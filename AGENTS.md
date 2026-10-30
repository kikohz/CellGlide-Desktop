# CellGlide Desktop development constraints

- DNS changes must be confined to CellGlide's own resolver and its own TUN link (`cellglide0`). Never change Wi-Fi, Ethernet, other adapters, other VPNs, `/etc/resolv.conf`, or saved NetworkManager connections.
- On Linux, set DNS only through systemd-resolved per-link calls on our TUN link (`SetLinkDNSEx`, `SetLinkDomains`, `SetLinkDefaultRoute`), revert with `RevertLink` on stop, and rely on the link disappearing on crash. No legacy DNS restoration logic. Return real DNS addresses rather than fake IPs.
- Verify other links' DNS, `/etc/resolv.conf` and NetworkManager connection files are identical before and after installation/testing.
- The iPhone protocol is defined by the CellGlide iOS app; do not change the phone side from here.
- Keep tunnel constants (addresses, routes, MTU, hev settings) identical to the macOS app's tunnel configuration.
- On Windows, set DNS only through `SetInterfaceDnsSettings` on cellglide0 (fixed GUID), clear it on stop, and delete that GUID's interface key on uninstall; on a crash, rely on the adapter disappearing. Do not use NRPT, and do not change other adapters' DNS, metrics, or routes.
- On Windows, acceptance uses `scripts/network-snapshot.ps1` to compare other adapters' DNS, NRPT, metrics, routes, and Tcpip interface keys before and after.
- On Windows, the forwarding engine is cg-netstack; its MTU and timeout parameters stay consistent with the hev settings.
- On Windows, while connected, a WFP dynamic session (`FWPM_SESSION_FLAG_DYNAMIC`) blocks outbound port 53 on every interface except cellglide0 (strict DNS; loopback and cellglide0 are permitted). Its rules vanish with the session handle or the process; never add persistent WFP objects. This is the only system-wide network effect allowed.
