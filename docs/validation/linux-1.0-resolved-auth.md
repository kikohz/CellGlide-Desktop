# Linux 1.0: systemd-resolved authorization for a non-root service

Question: can a non-root service holding only `CAP_NET_ADMIN` set per-link DNS
through systemd-resolved (`SetLinkDNSEx` on a TUN device it owns), with and
without a polkit rule? Measured by `scripts/probe-resolved-auth.sh` in throwaway
Lima VMs (arm64), run as a transient unit under user `cgprobe`.
Raw log: `.superpowers/sdd/2026-10-15-linux-1.0-m2/resolved-auth.txt`.

| Distribution | systemd | polkit | no-capability | cap-net-admin | no-capability + polkit | cap-net-admin + polkit |
|---|---|---|---|---|---|---|
| Ubuntu 22.04.5 | 249 | 0.105 | denied | denied | allowed | allowed |
| Ubuntu 24.04.4 | 255 | 124 | denied | denied | allowed | allowed |
| Ubuntu 26.04 | 259 | 127 | denied | denied | allowed | allowed |
| Debian 12 | 252 | 122 | denied | denied | allowed | allowed |
| Debian 13 | 257 | 126 | denied | denied | allowed | allowed |
| Fedora 42 | 257 | 126 | denied | denied | allowed | allowed |
| Debian 13, **no polkit** | 257 | none | denied | denied | n/a | n/a |

On the Debian 12 and 13 rows polkitd was installed by the test setup
(`apt-get install systemd-resolved polkitd`); the stock images did not
necessarily have it. The last row is the opposite case: systemd-resolved
installed, polkitd purged, no polkit authority running.

All denials in the first six rows are authorization denials from polkit ("Interactive authentication
required"; on Ubuntu 26.04 the equivalent "requires interactive authentication"
message). Debian images ship without systemd-resolved; it was installed in the
VM before probing. Ubuntu 22.04 was exercised with a `.pkla` file, all others
with a JS `.rules` file.

## Conclusion

1. **Ship a polkit rule?** Yes. On all six distributions a non-root service with
   only `CAP_NET_ADMIN` is denied by resolved; the same call is allowed once a
   rule grants `org.freedesktop.resolve1.*` to the service user. `CAP_NET_ADMIN`
   gives no help here.
2. **Which format?** Both. Polkit 0.105 (Ubuntu 22.04) only reads `.pkla`
   (`/var/lib/polkit-1/localauthority/10-vendor.d/`); polkit 122 and newer
   (Debian 12/13, Ubuntu 24.04/26.04, Fedora 42) read the JS rule
   (`/usr/share/polkit-1/rules.d/`). Install the JS rule for polkit >= 0.106 and
   the pkla for 0.105 (the pkla is harmless but ignored on newer polkit).
3. **Any distribution denied in both cases?** No, as long as polkit is
   present. With polkit absent, resolved denies everything, and no rule can
   fix that: Debian 13 with systemd-resolved but no polkitd returned
   `Access denied` for both `no-capability` and `cap-net-admin`. (The probe's
   rule cases on that row are meaningless: there is no polkit to read them;
   the script fell through to the pkla branch only because `rules.d` is absent.)
   So Task 3's "DNS 未接管" path is the expected behavior when polkit is
   missing or resolved is not running. Advice for Task 5 (not measured): declare
   `Recommends: polkitd | policykit-1` (deb) and `Recommends: polkit` (rpm), so
   polkit gets installed, and the service should report "DNS 未接管" with a
   hint instead of failing when the D-Bus error name is
   `org.freedesktop.DBus.Error.AccessDenied` or
   `org.freedesktop.DBus.Error.InteractiveAuthorizationRequired` (match the
   error name, not the message text: Ubuntu 26.04 with polkit also says "Access denied").

Evidence (key RESULT lines):

```
Ubuntu 22.04 (polkit 0.105, pkla)
RESULT cap-net-admin: denied (Call failed: Interactive authentication required.)
RESULT cap-net-admin+polkit: allowed
Ubuntu 26.04 (polkit 127, JS)
RESULT cap-net-admin: denied (Call failed: Access denied as the requested operation requires interactive authentication. However, interactive authentication has not been enabled by the calling program.)
RESULT cap-net-admin+polkit: allowed
Debian 13 without polkitd
RESULT no-capability: denied (Call failed: Access denied)
RESULT cap-net-admin: denied (Call failed: Access denied)
```

The other distributions printed the same denied/denied/allowed/allowed
pattern as Ubuntu 22.04 (see the table).

## Notes

- The rule should be scoped to user `cellglide`, not all users.
- The probe user `cgprobe` was made with `useradd`, not sysusers; rule matching
  by user name was tested, not by group.
- Only `SetLinkDNSEx` was probed. The rule's wildcard `org.freedesktop.resolve1.*`
  also covers `SetLinkDomains`, `SetLinkDefaultRoute` and `RevertLink`, but those
  calls were not measured individually.
- The script now prints which rule format it wrote (JS or pkla).
