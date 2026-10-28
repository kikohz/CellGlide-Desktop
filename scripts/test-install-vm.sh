#!/bin/bash
# Installs a local CellGlide package in a fresh Lima VM, checks the service, upgrades to a
# second package if given, then removes it and checks nothing is left behind.
# Usage: scripts/test-install-vm.sh <lima template> <package> [<newer package>]
# Extra checks (printed as "CHECK x: ..."):
#   a  polkit lets the service user call systemd-resolved (dummy link cgtest0, never a real one)
#   b  `ip -4 route flush table main realm 198` is accepted by this distro's iproute2
#   c  (informational only) libraries missing from `ldd /usr/bin/cellglide-gui`, and the runtime-loaded ones
#   d  `systemctl is-enabled cellglide` after install and upgrade; CYCLE=1 adds a deb
#      remove + reinstall (the reinstall must come back enabled)
#   e  CONTAINER=1 runs a systemd-less container install on the host's Docker
#   f  with a newer package: an upgrade over a disabled, stopped service leaves it disabled and
#      inactive; a reinstall over an enabled one starts it again
#   g  DEV_FIRST=1 first recreates an M1 development install (root unit in /etc/systemd/system
#      running /usr/local/bin/cellglide-service, a cellglide group with no user and a GID that
#      isn't the next free one, root-owned settings with auto_connect off, the development polkit
#      file, and the login user's ~/.config/autostart/cellglide-gui.desktop with
#      Exec=/usr/local/bin/cellglide-gui) and checks the package takes over: unit runs as
#      cellglide, development files gone, enabled and active, settings kept and owned by cellglide, the user reuses the group's GID;
#      whether the user autostart file survives is printed (informational: the controller removes it)
# Checks a, b, d, e, f, g, the service identity and a non-empty network baseline fail the run with a
# "FAIL <check>" line. Nothing privileged runs on the host: everything happens inside the VM or the container.
set -euo pipefail
T=$1; PKG=$2; NEWER=${3:-}
VM="cg-${T//./-}"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
limactl delete -f "$VM" >/dev/null 2>&1 || true
trap 'limactl delete -f "$VM" >/dev/null 2>&1 || true' EXIT
limactl create --tty=false --name "$VM" "template://$T" >/dev/null
limactl start "$VM" >/dev/null 2>&1 || { echo "start failed, retrying once"; limactl stop -f "$VM" >/dev/null 2>&1 || true; limactl start "$VM" >/dev/null; }
run() { limactl shell "$VM" -- sudo bash -c "$1"; }
limactl copy "$ROOT/scripts/network-snapshot.sh" "$VM":/tmp/snap.sh
if [ "${DEV_FIRST:-}" = 1 ]; then
    # The M1 development unit, as committed before M2.
    git -C "$ROOT" show be1fc48:packaging/cellglide.service > "${TMPDIR:-/tmp}/cg-m1.service"
    limactl copy "${TMPDIR:-/tmp}/cg-m1.service" "$VM":/tmp/cg-m1.service
    limactl copy "$ROOT/packaging/polkit/50-cellglide.rules" "$VM":/tmp/cg-dev.rules
    limactl copy "$ROOT/packaging/polkit/cellglide.pkla" "$VM":/tmp/cg-dev.pkla
fi
limactl copy "$PKG" "$VM":/tmp/
[ -n "$NEWER" ] && limactl copy "$NEWER" "$VM":/tmp/
# Helper functions live in the VM so the quoting stays readable.
limactl shell "$VM" -- sudo tee /tmp/checks.sh >/dev/null <<'VMEOF'
BUS="org.freedesktop.resolve1 /org/freedesktop/resolve1 org.freedesktop.resolve1.Manager"
check_a() { # $1 = label
    echo "CHECK a ($1): polkit=$(pkaction --version 2>/dev/null || echo absent) resolved=$(systemctl is-active systemd-resolved 2>&1 || true)"
    ip link add cgtest0 type dummy && ip link set cgtest0 up
    idx=$(cat /sys/class/net/cgtest0/ifindex)
    if out=$(systemd-run --quiet --wait --pipe --collect -p User=cellglide \
        busctl call $BUS SetLinkDNSEx 'ia(iayqs)' "$idx" 1 2 4 127 0 0 1 1053 "" 2>&1); then
        echo "CHECK a ($1): allowed"; ok=1
    else
        echo "CHECK a ($1): denied or failed: $out"; ok=0
    fi
    busctl call $BUS RevertLink i "$idx" >/dev/null 2>&1 || true
    ip link del cgtest0
    [ "$ok" = 1 ] || { echo "FAIL a"; return 1; }
}
want_enabled() { # $1 = when
    st=$(systemctl is-enabled cellglide 2>&1 || true)
    echo "CHECK d: $1 is-enabled=$st"
    [ "$st" = enabled ] || { echo "FAIL d ($1)"; return 1; }
}
check_bc() {
    rc=0; ip -4 route flush table main realm 198 || rc=$?
    echo "CHECK b: realm flush exit status $rc ($(ip -V 2>&1))"
    [ "$rc" = 0 ] || { echo "FAIL b"; return 1; }
    miss=$(ldd /usr/bin/cellglide-gui | grep 'not found' || true)
    echo "CHECK c (informational): ldd not found: ${miss:-none}"
    for l in libwayland-client.so.0 libxkbcommon.so.0 libX11.so.6; do
        if ldconfig -p | grep -q "$l"; then echo "CHECK c (informational): $l present"; else echo "CHECK c (informational): $l ABSENT"; fi
    done
}
VMEOF
case "$PKG" in
    *.deb) INSTALL="DEBIAN_FRONTEND=noninteractive apt-get install -y"; REMOVE="apt-get purge -y cellglide"
        REINSTALL="DEBIAN_FRONTEND=noninteractive dpkg -i"; DOWNGRADE="DEBIAN_FRONTEND=noninteractive dpkg -i"
        VERSION_OF="dpkg-query -W -f=\\\${Version} cellglide" ;;
    *.rpm) INSTALL="dnf install -y"; REMOVE="dnf remove -y cellglide"; REINSTALL="dnf reinstall -y"; DOWNGRADE="dnf downgrade -y"
        VERSION_OF="rpm -q cellglide" ;;
esac
run "bash /tmp/snap.sh > /tmp/before.txt; test -s /tmp/before.txt || { echo 'FAIL baseline empty'; exit 1; }"
if [ "${DEV_FIRST:-}" = 1 ]; then
    echo "== M1 development install"
    # shellcheck disable=SC2016  # runs in the VM; expands there
    run 'set -e
        # A free system GID that is not the next one groupadd would pick (that one is checked below).
        next=$(groupadd --system cg-probe && getent group cg-probe | cut -d: -f3; groupdel cg-probe)
        for g in $(seq 900 990); do
            if [ "$g" != "$next" ] && ! getent group "$g" >/dev/null && ! getent passwd "$g" >/dev/null; then gid=$g; break; fi
        done
        groupadd --system --gid "$gid" cellglide
        echo "$gid" > /tmp/dev-gid
        getent passwd cellglide && { echo "FAIL g (user exists before the package)"; exit 1; }
        install -m 0644 /tmp/cg-m1.service /etc/systemd/system/cellglide.service
        for b in cellglide-service cellglide cellglide-gui; do
            printf "#!/bin/sh\nexec sleep infinity\n" > /usr/local/bin/$b; chmod 0755 /usr/local/bin/$b
        done
        if pkaction --version 2>/dev/null | grep -q " 0\.105"; then
            mkdir -p /etc/polkit-1/localauthority/50-local.d
            install -m 0644 /tmp/cg-dev.pkla /etc/polkit-1/localauthority/50-local.d/cellglide.pkla
            echo /etc/polkit-1/localauthority/50-local.d/cellglide.pkla > /tmp/dev-polkit
        else
            mkdir -p /etc/polkit-1/rules.d
            install -m 0644 /tmp/cg-dev.rules /etc/polkit-1/rules.d/50-cellglide.rules
            echo /etc/polkit-1/rules.d/50-cellglide.rules > /tmp/dev-polkit
        fi
        # Autostart entry of the login user, as on the real test machine.
        uh=$(getent passwd "$SUDO_USER" | cut -d: -f6)
        install -d -o "$SUDO_USER" -g "$(id -g "$SUDO_USER")" "$uh/.config/autostart"
        printf "[Desktop Entry]\nType=Application\nName=CellGlide\nExec=/usr/local/bin/cellglide-gui\n" > "$uh/.config/autostart/cellglide-gui.desktop"
        chown "$SUDO_USER": "$uh/.config/autostart/cellglide-gui.desktop"
        echo "$uh/.config/autostart/cellglide-gui.desktop" > /tmp/dev-autostart
        systemctl daemon-reload
        systemctl enable --now cellglide
        # Type=simple: the start job ends before systemd has made StateDirectory (a race on fast VMs).
        for _ in $(seq 100); do [ -d /var/lib/cellglide ] && break; sleep 0.1; done
        # The shape the service writes (serde_json pretty, Stored), with a non-default setting.
        printf "{\n  \"version\": 1,\n  \"auto_connect\": false,\n  \"user_disconnected\": false,\n  \"chosen_serial\": null,\n  \"last_versions\": {\n    \"computer\": \"0.1.0\",\n    \"phone\": null\n  }\n}" > /var/lib/cellglide/settings.json
        echo "CHECK g (before): gid=$gid next-free=$next polkit=$(cat /tmp/dev-polkit) active=$(systemctl is-active cellglide) enabled=$(systemctl is-enabled cellglide) user=$(systemctl show -p User --value cellglide) owner=$(stat -c %U:%G /var/lib/cellglide/settings.json)"
        systemctl is-active --quiet cellglide && [ "$(stat -c %U /var/lib/cellglide/settings.json)" = root ] || { echo "FAIL g (setup)"; exit 1; }'
fi
echo "== install $(basename "$PKG") on $T"
run "$(printf '%s' "$INSTALL") /tmp/$(basename "$PKG")"
if [ "${DEV_FIRST:-}" = 1 ]; then
    # shellcheck disable=SC2016  # runs in the VM; expands there
    run 'fail=0
        no() { if [ -e "$1" ]; then echo "CHECK g: $1 still present"; fail=1; else echo "CHECK g: $1 gone"; fi; }
        for f in /usr/local/bin/cellglide-service /usr/local/bin/cellglide /usr/local/bin/cellglide-gui /etc/systemd/system/cellglide.service "$(cat /tmp/dev-polkit)"; do no "$f"; done
        u=$(systemctl show -p User --value cellglide); echo "CHECK g: User=$u ExecStart=$(systemctl show -p ExecStart --value cellglide | grep -o "path=[^ ;]*")"
        [ "$u" = cellglide ] || fail=1
        e=$(systemctl is-enabled cellglide 2>&1 || true); a=$(systemctl is-active cellglide 2>&1 || true); echo "CHECK g: is-enabled=$e is-active=$a"
        [ "$e" = enabled ] && [ "$a" = active ] || fail=1
        grep -q "\"auto_connect\": false" /var/lib/cellglide/settings.json && echo "CHECK g: settings kept (auto_connect false)" || { echo "CHECK g: settings lost"; fail=1; }
        o=$(stat -c %U /var/lib/cellglide); fo=$(stat -c %U /var/lib/cellglide/settings.json); echo "CHECK g: /var/lib/cellglide owner=$o settings.json owner=$fo"
        [ "$o" = cellglide ] && [ "$fo" = cellglide ] || fail=1
        g=$(id -g cellglide); echo "CHECK g: $(id cellglide); group created first had GID $(cat /tmp/dev-gid)"
        [ "$g" = "$(cat /tmp/dev-gid)" ] || fail=1
        if [ -e "$(cat /tmp/dev-autostart)" ]; then echo "CHECK g (informational): user autostart $(cat /tmp/dev-autostart) still exists after install (Exec=$(sed -n s/^Exec=//p "$(cat /tmp/dev-autostart)")); the controller removes it"; else echo "CHECK g (informational): user autostart $(cat /tmp/dev-autostart) was removed by the package"; fi
        [ "$fail" = 0 ] || { echo "FAIL g"; exit 1; }'
fi
run "id cellglide && systemctl is-active cellglide && systemctl show cellglide -p User -p AmbientCapabilities | tee /tmp/ident.txt"
run "grep -qx 'User=cellglide' /tmp/ident.txt && grep -i '^AmbientCapabilities=' /tmp/ident.txt | grep -qi cap_net_admin || { echo 'FAIL identity'; exit 1; }"
run ". /tmp/checks.sh; want_enabled 'after install'"
run "cellglide settings --auto-connect off && cellglide settings"
run "cellglide doctor || true"
run "test -f /etc/xdg/autostart/cellglide-gui.desktop && test -f /usr/share/icons/hicolor/256x256/apps/cellglide.png"
run ". /tmp/checks.sh; check_bc"
run "ls -l /usr/share/polkit-1/rules.d/50-cellglide.rules /var/lib/polkit-1/localauthority/10-vendor.d/cellglide.pkla 2>&1 || true"
run ". /tmp/checks.sh; check_a 'as shipped'"
if [ -n "$NEWER" ]; then
    echo "== upgrade to $(basename "$NEWER")"
    run "$INSTALL /tmp/$(basename "$NEWER")"
    run "systemctl is-active cellglide && cellglide settings | head -1"
    run ". /tmp/checks.sh; want_enabled 'after upgrade'"
    run "grep -q '\"auto_connect\": false' /var/lib/cellglide/settings.json && echo settings kept"
    echo "== upgrade over a disabled, stopped service (back to $(basename "$PKG"), then $(basename "$NEWER"))"
    # The downgrade only puts the older version back; the upgrade after it is the step under test.
    run "systemctl disable --now cellglide >/dev/null 2>&1; $(printf '%s' "$DOWNGRADE") /tmp/$(basename "$PKG") >/dev/null 2>&1; echo \"back to \$($VERSION_OF)\""
    run "systemctl disable --now cellglide >/dev/null 2>&1; $(printf '%s' "$INSTALL") /tmp/$(basename "$NEWER") >/dev/null 2>&1
        a=\$(systemctl is-active cellglide 2>&1 || true); e=\$(systemctl is-enabled cellglide 2>&1 || true)
        echo \"CHECK f: after upgrade over a disabled service: version=\$($VERSION_OF) is-active=\$a is-enabled=\$e\"
        [ \"\$a\" = inactive ] && [ \"\$e\" = disabled ] || { echo 'FAIL f (disabled)'; exit 1; }"
    run "systemctl enable --now cellglide >/dev/null 2>&1; $(printf '%s' "$REINSTALL") /tmp/$(basename "$NEWER") >/dev/null 2>&1
        a=\$(systemctl is-active cellglide 2>&1 || true); e=\$(systemctl is-enabled cellglide 2>&1 || true)
        echo \"CHECK f: after reinstall over an enabled service: is-active=\$a is-enabled=\$e\"
        [ \"\$a\" = active ] && [ \"\$e\" = enabled ] || { echo 'FAIL f (enabled)'; exit 1; }"
fi
if [ "${CYCLE:-}" = 1 ] && [ "${PKG##*.}" = deb ]; then
    LAST=$(basename "${NEWER:-$PKG}")
    echo "== remove then reinstall $LAST"
    run "apt-get remove -y cellglide && echo \"CHECK d: after remove is-enabled=\$(systemctl is-enabled cellglide 2>&1 || true) active=\$(systemctl is-active cellglide 2>&1 || true)\""
    run "$INSTALL /tmp/$LAST"
    run ". /tmp/checks.sh; want_enabled 'after reinstall'; systemctl is-active cellglide"
fi
echo "== remove"
run "$REMOVE"
run "! systemctl is-active --quiet cellglide && ! ip link show cellglide0 2>/dev/null && ! ip route | grep -q 'realm 198' && echo clean"
run "echo \"leftovers: \$(ls -d /var/lib/cellglide /etc/systemd/system/*/cellglide.service 2>&1 | tr '\\n' ' ')\"; getent passwd cellglide || echo 'user gone'"
run "bash /tmp/snap.sh > /tmp/after.txt; test -s /tmp/after.txt || { echo 'FAIL after-snapshot empty'; exit 1; }; diff /tmp/before.txt /tmp/after.txt && echo network unchanged || { echo 'FAIL network changed'; exit 1; }"
if [ "${CONTAINER:-}" = 1 ]; then
    echo "== container install without systemd"
    D=$(cd "$(dirname "$PKG")" && pwd)
    if docker run --rm --network host -v "$D":/d ubuntu:22.04 sh -c "apt-get update -qq && apt-get install -y /d/$(basename "$PKG")" >/tmp/cg-container.log 2>&1; then
        echo "CHECK e: container install exit 0"
    else
        echo "CHECK e: container install FAILED"; tail -20 /tmp/cg-container.log; echo "FAIL e"; exit 1
    fi
fi
echo "== PASS $T"
