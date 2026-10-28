#!/bin/bash
# Checks that the repository files inside each given package are byte-identical to the ones in
# packaging/ (never to anything else), so a stale build cannot be published.
#   deb: etc/apt/keyrings/cellglide-archive-keyring.gpg == packaging/keys/cellglide-archive-keyring.gpg
#        etc/apt/sources.list.d/cellglide.sources       == packaging/repo/cellglide.sources
#        both listed in control's conffiles, nothing under usr/share/keyrings/
#   rpm: /etc/yum.repos.d/cellglide.repo, /etc/pki/rpm-gpg/RPM-GPG-KEY-cellglide (sha256, checked in a
#        fedora:42 container)
# Cross-architecture consistency: when the arguments hold both the amd64 and the arm64 deb, these must be
# byte-identical between them: usr/lib/systemd/system/cellglide.service, usr/lib/sysusers.d/cellglide.conf,
# the polkit rule files, and preinst/postinst/prerm/postrm in control.tar.*. Likewise for the x86_64 and
# aarch64 rpms: the unit, sysusers and polkit files (compared by %{FILEDIGESTS}) and `rpm -qp --scripts`.
# This catches a stale package from one build machine.
# Usage: scripts/check-package-conffiles.sh <package.deb|package.rpm>...   Exits 1 naming package and file.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
P="$ROOT/packaging"
fail() { echo "error: $1: $2" >&2; exit 1; }

check_deb() {
    local deb=$1 tmp
    tmp=$(mktemp -d)
    ( cd "$tmp" && ar x "$deb" ) || fail "$deb" "cannot unpack with ar"
    mkdir "$tmp/data"
    tar -xf "$tmp"/data.tar.* -C "$tmp/data" || fail "$deb" "cannot extract data.tar.*"
    cmp -s "$tmp/data/etc/apt/keyrings/cellglide-archive-keyring.gpg" "$P/keys/cellglide-archive-keyring.gpg" \
        || fail "$deb" "etc/apt/keyrings/cellglide-archive-keyring.gpg is missing or differs from packaging/keys/cellglide-archive-keyring.gpg"
    cmp -s "$tmp/data/etc/apt/sources.list.d/cellglide.sources" "$P/repo/cellglide.sources" \
        || fail "$deb" "etc/apt/sources.list.d/cellglide.sources is missing or differs from packaging/repo/cellglide.sources"
    if [ -e "$tmp/data/usr/share/keyrings" ] && [ -n "$(ls -A "$tmp/data/usr/share/keyrings")" ]; then
        fail "$deb" "contains files under usr/share/keyrings/ (old keyring path)"
    fi
    local conf
    conf=$(tar -xOf "$tmp"/control.tar.* ./conffiles 2>/dev/null) || fail "$deb" "control.tar has no conffiles"
    for f in /etc/apt/keyrings/cellglide-archive-keyring.gpg /etc/apt/sources.list.d/cellglide.sources; do
        printf '%s\n' "$conf" | grep -qxF "$f" || fail "$deb" "$f is not listed in conffiles"
    done
    rm -rf "$tmp"
}

# Both debs unpacked side by side; every shared file must match.
cross_debs() {
    local a=$1 b=$2 ta tb f pair deb dir
    ta=$(mktemp -d); tb=$(mktemp -d)
    for pair in "$a:$ta" "$b:$tb"; do
        deb=${pair%%:*}; dir=${pair#*:}
        ( cd "$dir" && ar x "$deb" ) || fail "$deb" "cannot unpack with ar"
        mkdir "$dir/data" "$dir/control"
        tar -xf "$dir"/data.tar.* -C "$dir/data" || fail "$deb" "cannot extract data.tar.*"
        tar -xf "$dir"/control.tar.* -C "$dir/control" || fail "$deb" "cannot extract control.tar.*"
    done
    for f in data/usr/lib/systemd/system/cellglide.service data/usr/lib/sysusers.d/cellglide.conf \
             data/usr/share/polkit-1/rules.d/50-cellglide.rules \
             data/var/lib/polkit-1/localauthority/10-vendor.d/cellglide.pkla \
             control/preinst control/postinst control/prerm control/postrm; do
        if [ ! -e "$ta/$f" ] && [ ! -e "$tb/$f" ]; then continue; fi
        cmp -s "$ta/$f" "$tb/$f" \
            || fail "$(basename "$a") vs $(basename "$b")" "${f#*/} differs between architectures (or exists in only one); one of them is stale, rebuild both"
    done
    rm -rf "$ta" "$tb"
    echo "cross-arch ok: $(basename "$a") == $(basename "$b")"
}

DEBS=(); RPMS=()
for pkg in "$@"; do
    [ -f "$pkg" ] || fail "$pkg" "no such file"
    pkg="$(cd "$(dirname "$pkg")" && pwd -P)/$(basename "$pkg")"
    case "$pkg" in
        *.deb) DEBS+=("$pkg") ;;
        *.rpm) RPMS+=("$pkg") ;;
        *) fail "$pkg" "not a deb or rpm" ;;
    esac
done
for d in ${DEBS[@]+"${DEBS[@]}"}; do check_deb "$d"; echo "conffiles ok: $(basename "$d")"; done
DEB_AMD=""; DEB_ARM=""
for d in ${DEBS[@]+"${DEBS[@]}"}; do
    case "$d" in *_amd64.deb) DEB_AMD=$d ;; *_arm64.deb) DEB_ARM=$d ;; esac
done
if [ -n "$DEB_AMD" ] && [ -n "$DEB_ARM" ]; then cross_debs "$DEB_AMD" "$DEB_ARM"; fi

if [ ${#RPMS[@]} -gt 0 ]; then
    # Each rpm's directory is mounted read-only at /pkg/<n>, so rpms need not live in this checkout.
    MOUNTS=(); REL=(); n=0
    for r in "${RPMS[@]}"; do
        n=$((n + 1)); MOUNTS+=(-v "$(dirname "$r"):/pkg/$n:ro"); REL+=("/pkg/$n/$(basename "$r")")
    done
    docker run --rm --network host -v "$ROOT":/src:ro ${MOUNTS[@]+"${MOUNTS[@]}"} fedora:42 bash -c '
        bad=0
        for pkg in "$@"; do
            for pair in /etc/yum.repos.d/cellglide.repo:/src/packaging/repo/cellglide.repo \
                        /etc/pki/rpm-gpg/RPM-GPG-KEY-cellglide:/src/packaging/keys/RPM-GPG-KEY-cellglide; do
                path=${pair%%:*}; src=${pair#*:}
                want=$(sha256sum "$src" | cut -d" " -f1)
                got=$(rpm -qp --qf "[%{FILEDIGESTS} %{FILENAMES}\n]" "$pkg" 2>/dev/null | awk -v p="$path" "\$2 == p { print \$1 }")
                if [ "$got" != "$want" ]; then
                    echo "error: $(basename "$pkg"): $path is missing or differs from ${src#/src/}" >&2; bad=1
                fi
            done
            [ $bad = 0 ] && echo "conffiles ok: $(basename "$pkg")"
        done
        exit $bad' _ "${REL[@]}"

    RPM_X86=""; RPM_ARM=""
    for i in "${!RPMS[@]}"; do
        case "${RPMS[$i]}" in *.x86_64.rpm) RPM_X86=${REL[$i]} ;; *.aarch64.rpm) RPM_ARM=${REL[$i]} ;; esac
    done
    if [ -n "$RPM_X86" ] && [ -n "$RPM_ARM" ]; then
        docker run --rm --network host ${MOUNTS[@]+"${MOUNTS[@]}"} fedora:42 bash -c '
            a=$1; b=$2; bad=0
            digests() { rpm -qp --qf "[%{FILEDIGESTS} %{FILENAMES}\n]" "$1" 2>/dev/null; }
            da=$(digests "$a"); db=$(digests "$b")
            for path in /usr/lib/systemd/system/cellglide.service /usr/lib/sysusers.d/cellglide.conf \
                        /usr/share/polkit-1/rules.d/50-cellglide.rules /var/lib/polkit-1/localauthority/10-vendor.d/cellglide.pkla; do
                ha=$(printf "%s\n" "$da" | awk -v p="$path" "\$2 == p { print \$1 }")
                hb=$(printf "%s\n" "$db" | awk -v p="$path" "\$2 == p { print \$1 }")
                if [ "$ha" != "$hb" ]; then
                    echo "error: $(basename "$a") vs $(basename "$b"): $path differs between architectures (or exists in only one); one of them is stale, rebuild both" >&2; bad=1
                fi
            done
            if [ "$(rpm -qp --scripts "$a" 2>/dev/null)" != "$(rpm -qp --scripts "$b" 2>/dev/null)" ]; then
                echo "error: $(basename "$a") vs $(basename "$b"): rpm scriptlets (rpm -qp --scripts) differ between architectures; one of them is stale, rebuild both" >&2; bad=1
            fi
            [ $bad = 0 ] && echo "cross-arch ok: $(basename "$a") == $(basename "$b")"
            exit $bad' _ "$RPM_X86" "$RPM_ARM"
    fi
fi
