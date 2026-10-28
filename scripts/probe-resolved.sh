#!/bin/bash
# Does systemd-resolved accept 127.0.0.1:1053 as the DNS server of one TUN link,
# without touching any other link? Prints a RESULT line for the phase 0 report.
set -eu
IF=cgprobe0
DIR=$(cd "$(dirname "$0")" && pwd)
BUS="org.freedesktop.resolve1 /org/freedesktop/resolve1 org.freedesktop.resolve1.Manager"
before=$("$DIR/network-snapshot.sh" "$IF")
ip tuntap add dev "$IF" mode tun
trap 'ip link del "$IF" 2>/dev/null || true' EXIT
ip link set "$IF" up
IDX=$(cat "/sys/class/net/$IF/ifindex")
# a(iayqs): family AF_INET=2, 4 address bytes, port, server name
busctl call $BUS SetLinkDNSEx 'ia(iayqs)' "$IDX" 1 2 4 127 0 0 1 1053 ""
busctl call $BUS SetLinkDomains 'ia(sb)' "$IDX" 1 "." true
busctl call $BUS SetLinkDefaultRoute 'ib' "$IDX" true
resolvectl dns "$IF"
accepted=yes
resolvectl dns "$IF" | grep -q "127.0.0.1:1053" || accepted=no
during=$("$DIR/network-snapshot.sh" "$IF")
busctl call $BUS RevertLink 'i' "$IDX"
ip link del "$IF"
trap - EXIT
after=$("$DIR/network-snapshot.sh" "$IF")
if [ "$before" != "$during" ] || [ "$before" != "$after" ]; then
    echo "RESULT: other links changed"
    diff <(echo "$before") <(echo "$during") || true
    diff <(echo "$before") <(echo "$after") || true
    exit 1
fi
if resolvectl dns 2>/dev/null | grep -q "127.0.0.1:1053"; then
    echo "RESULT: still listed after RevertLink"
    exit 1
fi
if [ "$accepted" = no ]; then
    echo "RESULT: loopback with port rejected"
    exit 2
fi
echo "RESULT: loopback with port accepted; other links untouched"
