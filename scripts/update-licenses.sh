#!/usr/bin/env bash
# Regenerates the two Rust third-party license lists:
#   crates/cg-gui/assets/third-party-licenses.txt  the Linux list: the About page of the Linux window
#                                                  and the packages' THIRD-PARTY-LICENSES.txt;
#   packaging/licenses/windows-rust.txt            the Windows list: the Rust part of the Windows
#                                                  About page (crates/cg-ffi/src/about.rs) and of
#                                                  the zip's THIRD-PARTY-LICENSES.txt.
# Run it whenever Cargo.lock changes (the cg-gui test `licenses_are_not_older_than_cargo_lock` and
# the cg-ffi test `windows_licenses_are_not_older_than_cargo_lock` fail until you do), and as part
# of the release checklist. In Git Bash on Windows it works the same.
#
# --check: generate both into temporary files and compare them with the committed ones; exit 1 if
# either differs. Nothing is written.
#
# Needs cargo-about 0.9.2 exactly (cargo install --locked --version 0.9.2 cargo-about --features cli;
# render-about.py copies its grouping rules, so another version fails here) and Python 3 (only its
# standard library): python3 on PATH, or the command in $PYTHON (words split on spaces, e.g.
# "py -3"; scripts/check-windows-core.ps1 passes the Python it found).
#
# Linux layout: line 1 is a header holding the FNV-1a 64 checksum of Cargo.lock; then the crates
# cargo-about finds (packaging/licenses/about.toml, the whole workspace for the Linux targets); then
# the C code cargo-about cannot see: hev-socks5-tunnel and what it bundles (crates/hev-sys/hev, git
# submodules), libyaml (its upstream notice is kept in packaging/licenses/libyaml-License.txt,
# because hev's own yaml/License only names hev), and the distinct per-file license notices of the
# bundled C sources (scripts/extract-notices.py).
#
# Windows layout: the same header line (same checksum); "CellGlide third-party licenses (Rust
# crates, Windows)"; a blank line; then the crates of what the Windows package ships, cg-ffi
# (cellglide_native.dll), cg-cli (cellglide.exe) and cg-service (cellglide-service.exe), for the
# Windows targets (packaging/licenses/about-windows.toml), in the about.hbs layout. cargo-about can
# only scan one crate or the whole workspace, and the whole workspace would bring cg-gui (the Linux
# window: iced, ksni); so cargo-about runs once per shipped crate with --format json and
# scripts/render-about.py merges the runs as cargo-about groups a single one (checked on every run:
# render-about.py --self-test against cargo-about's own about.hbs output for cg-ffi). No hev, no
# lwIP, no C notices: hev-socks5-tunnel is not built on Windows (cg-netstack forwards there).
#
# Release checklist (M5): re-run this after any Cargo.lock change, which includes a version
# bump of a workspace crate (the workspace version is recorded in Cargo.lock), and commit the
# result.
set -euo pipefail
cd "$(dirname "$0")/.."

linux_out=crates/cg-gui/assets/third-party-licenses.txt
windows_out=packaging/licenses/windows-rust.txt
windows_crates=(cg-ffi cg-cli cg-service)

check=0
case "${1:-}" in
  "") ;;
  --check) check=1 ;;
  *)
    echo "usage: scripts/update-licenses.sh [--check]" >&2
    exit 2
    ;;
esac

# Everything is checked and built first; the real files are replaced only when both results are
# complete, so a missing tool or source can never leave either empty or half-written.
die() {
  echo "update-licenses.sh: $*" >&2
  exit 1
}
trap 'echo "update-licenses.sh: failed at line $LINENO; $linux_out and $windows_out left as they were" >&2' ERR
read -r -a py <<<"${PYTHON:-python3}"
"${py[@]}" -I -c 'import sys; sys.exit(sys.version_info[0] != 3)' >/dev/null 2>&1 ||
  die "Python 3 not found (${py[*]}); put python3 on PATH or set PYTHON"
about_version=$(cargo about --version 2>/dev/null) ||
  die "cargo-about is missing (cargo install --locked --version 0.9.2 cargo-about --features cli)"
[ "$about_version" = "cargo-about 0.9.2" ] ||
  die "cargo-about 0.9.2 is needed, found '$about_version' (render-about.py follows 0.9.2's rules)"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo about generate --workspace --locked \
  --config packaging/licenses/about.toml packaging/licenses/about.hbs -o "$work/linux-crates.txt"
json=()
for crate in "${windows_crates[@]}"; do
  [ -f "crates/$crate/Cargo.toml" ] || die "crates/$crate/Cargo.toml not found"
  cargo about generate --locked -m "crates/$crate/Cargo.toml" \
    --config packaging/licenses/about-windows.toml --format json -o "$work/$crate.json"
  [ -s "$work/$crate.json" ] || die "cargo about produced nothing for $crate"
  json+=("$work/$crate.json")
done
# render-about.py must still render a single run exactly as cargo-about does with about.hbs.
cargo about generate --locked -m crates/cg-ffi/Cargo.toml \
  --config packaging/licenses/about-windows.toml packaging/licenses/about.hbs -o "$work/cg-ffi.hbs.txt"
"${py[@]}" -I scripts/render-about.py --self-test "$work/cg-ffi.json" "$work/cg-ffi.hbs.txt" ||
  die "render-about.py no longer matches cargo-about's about.hbs output; nothing replaced"
"${py[@]}" -I scripts/render-about.py "${json[@]}" >"$work/windows-crates.txt"

header=$(
  "${py[@]}" -I - <<'PY'
import pathlib
import sys

sys.stdout.reconfigure(newline="\n")
h = 0xCBF29CE484222325
for byte in pathlib.Path("Cargo.lock").read_bytes():
    h = ((h ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
print(f"# Generated by scripts/update-licenses.sh; Cargo.lock fnv1a64 {h:016x}")
PY
)

hev=crates/hev-sys/hev
{
  echo "$header"
  echo "CellGlide third-party licenses"
  echo
  cat "$work/linux-crates.txt"
  for entry in \
    "hev-socks5-tunnel|$hev/LICENSE" \
    "hev-socks5-tunnel (core)|$hev/src/core/LICENSE" \
    "lwIP|$hev/third-part/lwip/LICENSE" \
    "hev-task-system|$hev/third-part/hev-task-system/LICENSE" \
    "yaml (hev's copy)|$hev/third-part/yaml/License" \
    "libyaml|packaging/licenses/libyaml-License.txt"; do
    echo "================================================================================"
    echo "${entry%%|*}"
    echo
    cat "${entry#*|}"
    echo
  done
  # The license comment of every bundled C source (lwIP carries PPP, polarssl, 6LoWPAN... code
  # with other holders' BSD terms), each distinct text once, with the files it came from.
  # hev-task-system and yaml are included so a notice that appears there later is not missed.
  "${py[@]}" -I scripts/extract-notices.py "$hev/third-part" lwip/src hev-task-system yaml
} | sed 's/\r$//' >"$work/linux.txt"

{
  echo "$header"
  echo "CellGlide third-party licenses (Rust crates, Windows)"
  echo
  cat "$work/windows-crates.txt"
} | sed 's/\r$//' >"$work/windows.txt"

# Some crates ship their license file with CRLF line endings; both lists keep LF only (the `sed`
# above), which is also what git stores (.gitattributes: eol=lf), so a regeneration on any platform
# gives the committed bytes and --check can compare them.
# Complete files start with the header and hold the crates (and, for Linux, the C notices).
for f in "$work/linux.txt" "$work/windows.txt"; do
  head -n 1 "$f" | grep -q "^# Generated by scripts/update-licenses.sh; Cargo.lock fnv1a64 [0-9a-f]\{16\}$" ||
    die "a generated list has no header; nothing replaced"
done
[ -s "$work/linux-crates.txt" ] || die "cargo about produced nothing (Linux); nothing replaced"
[ -s "$work/windows-crates.txt" ] || die "cargo about produced nothing (Windows); nothing replaced"
[ "$(wc -l <"$work/linux.txt")" -gt 1000 ] || die "the Linux list is too short; nothing replaced"
[ "$(wc -l <"$work/windows.txt")" -gt 1000 ] || die "the Windows list is too short; nothing replaced"
grep -q "^  - tokio " "$work/windows.txt" || die "the Windows list has no tokio; nothing replaced"
# Nothing of the Linux-only C engine or the Linux window may reach the Windows list.
if grep -n -w -e "hev-socks5" -e "hev-sys" -e "hev-socks5-tunnel" -e "lwIP" -e "ksni" -e "iced" \
  "$work/windows.txt" >&2; then
  die "the Windows list names Linux-only code (above); nothing replaced"
fi

if [ "$check" = 1 ]; then
  stale=0
  cmp -s "$work/linux.txt" "$linux_out" || {
    echo "update-licenses.sh: $linux_out is not what this script generates" >&2
    stale=1
  }
  cmp -s "$work/windows.txt" "$windows_out" || {
    echo "update-licenses.sh: $windows_out is not what this script generates" >&2
    stale=1
  }
  [ "$stale" = 0 ] || die "run scripts/update-licenses.sh and commit the result"
  echo "update-licenses.sh: $linux_out and $windows_out are up to date"
  exit 0
fi

# Copied, not moved: the files keep their own permissions rather than mktemp's 0600.
cat "$work/linux.txt" >"$linux_out"
cat "$work/windows.txt" >"$windows_out"
wc -c "$linux_out" "$windows_out"
