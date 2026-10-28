#!/bin/sh
# Prints everything CellGlide must never change: DNS and routes of every other link,
# plus fingerprints of resolv.conf and saved NetworkManager connections.
set -eu
EXCLUDE=${1:-cellglide0}
# systemd 255 mirrors a loopback server set on one link into the "Global:" line; strip the
# excluded link's own servers from it so only real changes to other links show up. Fields
# are compared exactly (addresses are not regular expressions).
echo "## resolvectl dns"
own=$(resolvectl dns "$EXCLUDE" 2>/dev/null | sed 's/^[^:]*://' || true)
resolvectl dns 2>/dev/null | grep -v "($EXCLUDE)" | awk -v own="$own" '
    BEGIN { n = split(own, o, " "); for (i = 1; i <= n; i++) skip[o[i]] = 1 }
    /^Global:/ {
        sub(/^Global:/, "Global: ")
        line = "Global:"; for (i = 2; i <= NF; i++) if (!($i in skip)) line = line " " $i
        print line; next
    }
    { print }' || true
echo "## resolvectl domain"; resolvectl domain 2>/dev/null | grep -v "($EXCLUDE)" || true
echo "## routes v4"; ip -4 route show table all | grep -v "dev $EXCLUDE" | grep -vE "realm 198( |$)" || true
echo "## routes v6"; ip -6 route show table all | grep -v "dev $EXCLUDE" | grep -v "^local\|^multicast\|^ff00::/8" || true
echo "## resolv.conf"; sha256sum /etc/resolv.conf "$(readlink -f /etc/resolv.conf)" 2>/dev/null || true
echo "## NetworkManager"; sha256sum /etc/NetworkManager/system-connections/* 2>/dev/null || true
