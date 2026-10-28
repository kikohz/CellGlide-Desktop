#!/bin/bash
# Can a non-root service holding only CAP_NET_ADMIN set per-link DNS through
# systemd-resolved, with and without a polkit rule? Prints one RESULT line per case.
# Usage (as root): probe-resolved-auth.sh
set -u
U=cgprobe
IF=cgauth0
BUS="org.freedesktop.resolve1 /org/freedesktop/resolve1 org.freedesktop.resolve1.Manager"
id "$U" >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin "$U"
ip tuntap add dev "$IF" mode tun user "$U"
ip link set "$IF" up
IDX=$(cat "/sys/class/net/$IF/ifindex")
trap 'busctl call $BUS RevertLink i "$IDX" >/dev/null 2>&1; ip link del "$IF" 2>/dev/null; rm -f /usr/share/polkit-1/rules.d/49-cgprobe.rules /var/lib/polkit-1/localauthority/10-vendor.d/cgprobe.pkla /etc/polkit-1/localauthority/10-vendor.d/cgprobe.pkla' EXIT

attempt() { # $1 = label, $2 = extra systemd-run properties
    out=$(systemd-run --quiet --wait --pipe --collect -p User="$U" $2 \
        busctl call $BUS SetLinkDNSEx 'ia(iayqs)' "$IDX" 1 2 4 127 0 0 1 1053 "" 2>&1)
    if [ $? -eq 0 ]; then echo "RESULT $1: allowed"; else echo "RESULT $1: denied ($out)"; fi
    busctl call $BUS RevertLink i "$IDX" >/dev/null 2>&1 || true
}

echo "distro: $(. /etc/os-release; echo "$PRETTY_NAME")"
echo "systemd: $(systemctl --version | head -1)"
echo "polkit: $(pkaction --version 2>/dev/null || echo none)"
attempt "no-capability" ""
attempt "cap-net-admin" "-p AmbientCapabilities=CAP_NET_ADMIN -p CapabilityBoundingSet=CAP_NET_ADMIN"

# Same with a polkit rule for the probe user (JS for polkit >= 0.106, pkla for 0.105).
if [ -d /usr/share/polkit-1/rules.d ] && pkaction --version 2>/dev/null | grep -vq " 0\.105"; then
    echo "polkit rule format: JS (/usr/share/polkit-1/rules.d)"
    cat > /usr/share/polkit-1/rules.d/49-cgprobe.rules <<EOF
polkit.addRule(function (action, subject) {
    if (subject.user === "$U" && action.id.indexOf("org.freedesktop.resolve1.") === 0) { return polkit.Result.YES; }
});
EOF
else
    d=/var/lib/polkit-1/localauthority/10-vendor.d
    echo "polkit rule format: pkla ($d)"
    mkdir -p "$d"
    printf '[cgprobe]\nIdentity=unix-user:%s\nAction=org.freedesktop.resolve1.*\nResultAny=yes\nResultInactive=yes\nResultActive=yes\n' "$U" > "$d/cgprobe.pkla"
fi
sleep 2
attempt "no-capability+polkit" ""
attempt "cap-net-admin+polkit" "-p AmbientCapabilities=CAP_NET_ADMIN -p CapabilityBoundingSet=CAP_NET_ADMIN"
