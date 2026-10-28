#!/bin/sh
# Unprivileged SO_BINDTODEVICE (Linux >= 5.7): bind to the default-route interface, connect out,
# and confirm that rebinding is refused. Prints one RESULT line; exits 0 only for the expected one.
# Must run as an ordinary user: root may rebind, which would prove nothing.
set -eu
if [ "$(id -u)" = 0 ]; then
    echo "bind-device-check.sh must not run as root (root may rebind); run it as an ordinary user" >&2
    exit 2
fi
IF=$(awk '$2=="00000000" && $8=="00000000" {print $7, $1}' /proc/net/route | sort -n | head -1 | cut -d" " -f2)
uname -r
OUT=$(python3 - "$IF" <<'PY'
import socket, sys
iface = sys.argv[1].encode()
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, iface)
s.settimeout(5); s.connect(("1.1.1.1", 443)); print("connected via", iface.decode())
try:
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, b"lo"); print("RESULT: rebind allowed (unexpected)")
except PermissionError:
    print("RESULT: unprivileged bind ok, rebind refused")
PY
)
echo "$OUT"
[ "$(echo "$OUT" | tail -1)" = "RESULT: unprivileged bind ok, rebind refused" ]
