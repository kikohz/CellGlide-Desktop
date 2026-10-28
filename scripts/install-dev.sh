#!/bin/sh
# Development install on this Linux machine: release binaries, the systemd service and
# the cellglide group. Checks other links' DNS and routes are unchanged (AGENTS.md).
# Undo with: scripts/install-dev.sh --uninstall
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
BIN=/usr/local/bin
# A read-only checkout (the Lima test VM) builds elsewhere: honour CARGO_TARGET_DIR.
TARGET=${CARGO_TARGET_DIR:-target}

if [ "${1:-}" = "--uninstall" ]; then
    # The cellglide package uses the same unit name and some of the same files; never touch them.
    if dpkg-query -W -f='${Status}' cellglide 2>/dev/null | grep -q 'install ok installed' \
        || rpm -q cellglide >/dev/null 2>&1; then
        echo "已安装 cellglide 软件包，没有卸载任何文件（--uninstall 只删除开发版）。"
        echo "如果开发版文件还在，手动删除：sudo rm -f /etc/systemd/system/cellglide.service $BIN/cellglide-service $BIN/cellglide $BIN/cellglide-gui /usr/lib/systemd/system-sleep/cellglide，然后 sudo systemctl daemon-reload && sudo systemctl reenable --now cellglide"
        exit 1
    fi
    sudo systemctl disable --now cellglide 2>/dev/null || true
    for f in "$BIN/cellglide-service" "$BIN/cellglide" "$BIN/cellglide-gui" /etc/systemd/system/cellglide.service \
        /usr/lib/systemd/system-sleep/cellglide \
        /usr/share/icons/hicolor/scalable/status/cellglide-tray-*.svg /usr/lib/sysusers.d/cellglide.conf /etc/polkit-1/rules.d/50-cellglide.rules \
        /etc/polkit-1/localauthority/50-local.d/cellglide.pkla; do
        # A file a package still owns (e.g. after apt remove without purge) stays in place.
        if dpkg -S "$f" >/dev/null 2>&1 || rpm -qf "$f" >/dev/null 2>&1; then
            echo "保留 $f（属于软件包）"
            continue
        fi
        sudo rm -f "$f"
    done
    sudo systemctl daemon-reload
    echo "已卸载。cellglide 用户和组保留，如需删除：sudo userdel cellglide（组随之删除）"
    exit 0
fi

command -v usbmuxd >/dev/null 2>&1 || echo "提示：还没有安装 usbmuxd。Ubuntu / Debian：sudo apt install usbmuxd；Fedora：sudo dnf install usbmuxd"
cargo build --release -p cg-service -p cg-cli -p cg-gui
mkdir -p "$TARGET"
scripts/network-snapshot.sh > "$TARGET/network-before.txt"

# Stop the old engine before replacing its binary; its interface must be gone.
if systemctl is-active --quiet cellglide; then
    sudo systemctl stop cellglide
fi
if [ -e /sys/class/net/cellglide0 ]; then
    echo "cellglide0 仍然存在，旧服务没有停干净，已中止安装"
    exit 1
fi

sudo install -m 0755 "$TARGET/release/cellglide-service" "$TARGET/release/cellglide" "$TARGET/release/cellglide-gui" "$BIN/"
# The service runs as the cellglide system user (created by sysusers) with CAP_NET_ADMIN only.
# The tray icons are looked up by name in the hicolor theme.
sudo install -d /usr/share/icons/hicolor/scalable/status
sudo install -m 0644 packaging/icons/tray/cellglide-tray-*.svg /usr/share/icons/hicolor/scalable/status/
sudo gtk-update-icon-cache -q /usr/share/icons/hicolor || true
# After resume, restart usbmuxd if it lost the iPhone (validation l). systemd-sleep only
# runs hooks from /usr/lib, so the dev hook goes there, calling the binary in $BIN; a
# package's own hook (calling /usr/bin) is left alone.
HOOK=/usr/lib/systemd/system-sleep/cellglide
if dpkg -S "$HOOK" >/dev/null 2>&1 || rpm -qf "$HOOK" >/dev/null 2>&1; then
    echo "保留 $HOOK（属于软件包）"
else
    sudo install -d /usr/lib/systemd/system-sleep
    sed "s#/usr/bin/cellglide-service#$BIN/cellglide-service#" packaging/system-sleep/cellglide | sudo tee "$HOOK" >/dev/null
    sudo chmod 0755 "$HOOK"
fi
sudo install -m 0644 packaging/sysusers.conf /usr/lib/sysusers.d/cellglide.conf
sudo systemd-sysusers cellglide.conf
sed 's#/usr/bin/cellglide-service#/usr/local/bin/cellglide-service#' packaging/cellglide.service | sudo tee /etc/systemd/system/cellglide.service >/dev/null
sudo chown -R cellglide:cellglide /var/lib/cellglide 2>/dev/null || true
# systemd-resolved only lets a non-root caller set DNS when polkit allows it (measured in
# docs/validation/linux-1.0-resolved-auth.md). Polkit 0.105 reads pkla files, newer versions
# read JS rules; the version decides, because rules.d exists on 22.04 too. Existing
# directories are never chmod'ed (mkdir -p only creates what is missing).
if ! command -v pkaction >/dev/null 2>&1; then
    echo "警告：没有找到 polkit，systemd-resolved 不会允许 CellGlide 设置 DNS；隧道仍可使用，但 DNS 不由 CellGlide 接管。安装 polkit 后重新运行本脚本"
elif pkaction --version 2>/dev/null | grep -q " 0\.105"; then
    sudo mkdir -p /etc/polkit-1/localauthority/50-local.d
    sudo install -m 0644 packaging/polkit/cellglide.pkla /etc/polkit-1/localauthority/50-local.d/cellglide.pkla
else
    sudo mkdir -p /etc/polkit-1/rules.d
    sudo install -m 0644 packaging/polkit/50-cellglide.rules /etc/polkit-1/rules.d/50-cellglide.rules
fi
if ! id -nG "$USER" | grep -qw cellglide; then
    sudo usermod -aG cellglide "$USER"
    echo "已把 $USER 加入 cellglide 组，注销并重新登录后生效（在此之前可用 sudo cellglide …）"
fi
sudo systemctl daemon-reload
sudo systemctl enable --now cellglide

scripts/network-snapshot.sh > "$TARGET/network-after.txt"
if diff -u "$TARGET/network-before.txt" "$TARGET/network-after.txt"; then
    echo "安装完成；其他网卡的 DNS 与路由没有变化"
else
    echo "警告：安装前后其他网卡的网络设置不同，见上方差异"
    exit 1
fi
