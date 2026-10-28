# CellGlide Desktop development constraints

- DNS changes must be confined to CellGlide's own resolver and its own TUN link (`cellglide0`). Never change Wi-Fi, Ethernet, other adapters, other VPNs, `/etc/resolv.conf`, or saved NetworkManager connections.
- On Linux, set DNS only through systemd-resolved per-link calls on our TUN link (`SetLinkDNSEx`, `SetLinkDomains`, `SetLinkDefaultRoute`), revert with `RevertLink` on stop, and rely on the link disappearing on crash. No legacy DNS restoration logic. Return real DNS addresses rather than fake IPs.
- Verify other links' DNS, `/etc/resolv.conf` and NetworkManager connection files are identical before and after installation/testing.
- The iPhone protocol is defined by the CellGlide iOS app; do not change the phone side from here.
- Keep tunnel constants (addresses, routes, MTU, hev settings) identical to the macOS app's tunnel configuration.
