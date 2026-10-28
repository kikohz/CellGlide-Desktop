#!/bin/bash
# Rehearses scripts/sign-and-publish.sh --dry-run with a THROWAWAY key (never the real one),
# then installs cellglide from the resulting repo/ in temporary Lima VMs over a local HTTP server.
# Test-only: nothing is uploaded and nothing privileged runs on the host.
#
# Uses the real Mac rpmsign (brew install rpm). CELLGLIDE_KEYS_DIR and CELLGLIDE_SIGNED_DIR point
# the real script at the throwaway public key and a scratch directory for signed rpms.
#
# A second, already "published" version (PREV, default 1.0.0~beta.2; arm64 packages in
# dist/$PREV/) is signed here as an earlier publish
# would have signed it and recorded in a scratch published-versions / published.sha256. The script
# must then refuse a missing record, a mismatching record and a missing signed copy, and reuse the
# copy otherwise. Both versions end up in the repository, so the Fedora VM can install the older
# of CUR and PREV (decided by rpm.vercmp in the VM, "~" sorts lower) from its URL and
# `dnf upgrade --refresh` to the newer through the packaged cellglide.repo.
#
# Usage: scripts/test-publish-local.sh [gnupg home, default /tmp/cg-gnupg-test]   (KEEP=1 keeps repo/ and the key afterwards)
# Needs: Docker (colima), Lima, python3, gpg.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
# Short path on purpose: gpg-agent socket paths are limited to ~104 bytes on macOS.
GNUPGHOME=${1:-/tmp/cg-gnupg-test}
export GNUPGHOME
PORT=${PORT:-18765}
PREV=${PREV:-1.0.0~beta.2}
CUR=$("$ROOT/scripts/pkg-version.sh")
[ -d "$ROOT/dist/$PREV" ] && [ -d "$ROOT/dist/$CUR" ] || { echo "needs dist/$CUR and dist/$PREV (set PREV)"; exit 1; }
WORK=$(mktemp -d "$ROOT/.rehearsal.XXXXXX")   # under $ROOT so containers can mount it
SERVER_PID=""
VMS="cg-repo-ubuntu cg-repo-fedora"

cleanup() {
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
    for vm in $VMS; do limactl delete -f "$vm" >/dev/null 2>&1 || true; done
    gpgconf --kill gpg-agent 2>/dev/null || true
    rm -rf "$WORK"
    if [ "${KEEP:-}" != 1 ]; then rm -rf "$ROOT/repo" "$GNUPGHOME"; fi
}
trap cleanup EXIT
step() { printf '\n== %s\n' "$*"; }

step "throwaway key in $GNUPGHOME"
rm -rf "$GNUPGHOME" && mkdir -p "$GNUPGHOME" && chmod 700 "$GNUPGHOME"
gpg --batch --pinentry-mode loopback --passphrase '' --quick-generate-key "CellGlide Rehearsal <rehearsal@invalid>" rsa4096 sign 1d
FPR=$(gpg --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')
echo "throwaway fingerprint: $FPR"
test "$FPR" != 71082C14C8979B6C13E1B1D728A16C8C0A0D2A98
mkdir -p "$WORK/keys"
gpg --export "$FPR" > "$WORK/keys/cellglide-archive-keyring.gpg"
gpg --armor --export "$FPR" > "$WORK/keys/RPM-GPG-KEY-cellglide"
mkdir -p "$WORK/signed"
export CELLGLIDE_KEYS_DIR="$WORK/keys" CELLGLIDE_SIGNED_DIR="$WORK/signed"

step "negative: unknown key is refused with a clear message"
out=$(CELLGLIDE_GPG_KEY=0000000000000000000000000000000000000000 "$ROOT/scripts/sign-and-publish.sh" --dry-run 2>&1) && { echo "FAIL: accepted a missing key"; exit 1; }
echo "$out"; echo "$out" | grep -q 'no secret key matches' || { echo "FAIL: unexpected message"; exit 1; }
step "negative: the real public key does not match the throwaway signing key"
out=$(CELLGLIDE_KEYS_DIR="$ROOT/packaging/keys" CELLGLIDE_GPG_KEY="$FPR" "$ROOT/scripts/sign-and-publish.sh" --dry-run 2>&1) && { echo "FAIL: accepted mismatched public keys"; exit 1; }
echo "$out"; echo "$out" | grep -q 'does not contain the public key' || { echo "FAIL: unexpected message"; exit 1; }

step "negative: a real (non-dry) run refuses rehearsal keys; a fake npx guarantees nothing can upload"
mkdir -p "$WORK/fakebin"
printf '#!/bin/sh\necho FAKE-NPX-CALLED; exit 99\n' > "$WORK/fakebin/npx"; chmod +x "$WORK/fakebin/npx"
out=$(PATH="$WORK/fakebin:$PATH" CELLGLIDE_GPG_KEY="$FPR" "$ROOT/scripts/sign-and-publish.sh" 2>&1) && { echo "FAIL: real run accepted rehearsal keys"; exit 1; }
echo "$out" | tail -2
echo "$out" | grep -q 'differs from packaging/keys' || { echo "FAIL: unexpected message"; exit 1; }
echo "$out" | grep -q FAKE-NPX && { echo "FAIL: reached upload"; exit 1; }

step "negative: a package with a stale cellglide.sources is refused (doctored copy, dist/ untouched)"
mkdir -p "$WORK/bad"
docker run --rm --network host -v "$ROOT/dist":/dist:ro -v "$WORK/bad":/out ubuntu:22.04 bash -euc '
    dpkg-deb -R /dist/1.0.0~beta.1/cellglide_1.0.0~beta.1-1_amd64.deb /tmp/x
    sed -i "s#/etc/apt/keyrings#/usr/share/keyrings#" /tmp/x/etc/apt/sources.list.d/cellglide.sources
    dpkg-deb -b /tmp/x /out/cellglide_bad_amd64.deb >/dev/null
    chown "$0" /out/cellglide_bad_amd64.deb' "$(id -u):$(id -g)"
out=$("$ROOT/scripts/check-package-conffiles.sh" "$WORK/bad/cellglide_bad_amd64.deb" 2>&1) && { echo "FAIL: doctored deb accepted"; exit 1; }
echo "$out"
echo "$out" | grep -q 'cellglide_bad_amd64.deb.*cellglide.sources' || { echo "FAIL: message does not name package and file"; exit 1; }

step "sign-and-publish.sh --dry-run (first run signs, with the real rpmsign; nothing published yet)"
export CELLGLIDE_PUBLISHED_LIST="$WORK/published-versions" CELLGLIDE_PUBLISHED_RECORD="$WORK/published.sha256"
: > "$CELLGLIDE_PUBLISHED_LIST"; : > "$CELLGLIDE_PUBLISHED_RECORD"
sha_before=$(cd "$ROOT/dist" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256 | shasum -a 256)
CELLGLIDE_GPG_KEY="$FPR" "$ROOT/scripts/sign-and-publish.sh" --dry-run | tee "$WORK/run1.log"
grep -q '^signing cellglide' "$WORK/run1.log" || { echo "FAIL: nothing was signed"; exit 1; }
[ "$(grep -c "^  [0-9a-f]\{64\}  $CUR/cellglide" "$WORK/run1.log")" -eq 4 ] || { echo "FAIL: dry run did not print the four record lines"; exit 1; }
if ! grep -q 'no-cache' "$WORK/run1.log" || ! grep -q 'immutable' "$WORK/run1.log"; then echo "FAIL: cache headers missing from the plan"; exit 1; fi
rpm_sums_1=$(cd "$WORK/signed" && find . -name '*.rpm' | sort | xargs shasum -a 256)

step "second run reuses the signed rpms byte for byte"
CELLGLIDE_GPG_KEY="$FPR" "$ROOT/scripts/sign-and-publish.sh" --dry-run | tee "$WORK/run2.log" | grep -E 'reusing|signing|signatures'
grep -q '^signing cellglide' "$WORK/run2.log" && { echo "FAIL: re-signed on the second run"; exit 1; }
[ "$(grep -c '^reusing signed' "$WORK/run2.log")" -eq 2 ] || { echo "FAIL: expected 2 reused rpms"; exit 1; }
rpm_sums_2=$(cd "$WORK/signed" && find . -name '*.rpm' | sort | xargs shasum -a 256)
if [ "$rpm_sums_1" = "$rpm_sums_2" ]; then echo "signed rpm bytes unchanged"; else echo "FAIL: signed rpm bytes changed"; exit 1; fi
sha_after=$(cd "$ROOT/dist" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256 | shasum -a 256)
if [ "$sha_before" = "$sha_after" ]; then echo "dist/ unchanged"; else echo "FAIL: dist/ changed"; exit 1; fi
find "$ROOT/repo" -type f | sed "s#$ROOT/##" | sort

step "an earlier published version $PREV: signed and recorded as its publish would have done"
mkdir -p "$WORK/signed/$PREV"
for r in "$ROOT/dist/$PREV"/*.rpm; do
    n=$(basename "$r")
    cp "$r" "$WORK/signed/$PREV/.signing.$n"
    rpmsign --define "__gpg $(command -v gpg)" --define "_gpg_name $FPR" --define "_gpg_digest_algo sha256" \
        --define "_gpg_path $GNUPGHOME" --addsign "$WORK/signed/$PREV/.signing.$n" >/dev/null
    mv "$WORK/signed/$PREV/.signing.$n" "$WORK/signed/$PREV/$n"
    shasum -a 256 "$r" | cut -d' ' -f1 > "$WORK/signed/$PREV/$n.sha256"
done
echo "$PREV" > "$CELLGLIDE_PUBLISHED_LIST"
{
    for p in "$ROOT/dist/$PREV"/*.deb; do printf '%s  %s/%s\n' "$(shasum -a 256 "$p" | cut -d' ' -f1)" "$PREV" "$(basename "$p")"; done
    for p in "$WORK/signed/$PREV"/*.rpm; do printf '%s  %s/%s\n' "$(shasum -a 256 "$p" | cut -d' ' -f1)" "$PREV" "$(basename "$p")"; done
} > "$WORK/record.good"
cat "$WORK/record.good"
refuses() { # label, expected message; runs a dry run that must exit non-zero, then checks the text
    local out rc=0
    out=$(CELLGLIDE_GPG_KEY="$FPR" "$ROOT/scripts/sign-and-publish.sh" --dry-run 2>&1) || rc=$?
    [ "$rc" -ne 0 ] || { echo "FAIL: $1: exit status 0"; exit 1; }
    printf '%s\n' "$out" | tail -1
    printf '%s\n' "$out" | grep -q "$2" || { echo "FAIL: $1: unexpected message"; exit 1; }
    printf '%s\n' "$out" | grep -q '^signing cellglide' && { echo "FAIL: $1: signed something"; exit 1; }
    echo "refused ($1), exit $rc"
}
: > "$CELLGLIDE_PUBLISHED_RECORD"
refuses "no record" "has no lines in packaging/repo/published.sha256"
sed '1s/^./0/; 1s/^00/11/' "$WORK/record.good" > "$CELLGLIDE_PUBLISHED_RECORD"
refuses "record mismatch" "differs from the bytes recorded"
cp "$WORK/record.good" "$CELLGLIDE_PUBLISHED_RECORD"
mv "$WORK/signed/$PREV" "$WORK/signed-prev.away"
refuses "no signed copy" "has no signed copy"
mv "$WORK/signed-prev.away" "$WORK/signed/$PREV"
prev_rpm_sum=$(cd "$WORK/signed/$PREV" && shasum -a 256 ./*.rpm)

step "dry run with $PREV published: its bytes are checked, its signed rpm reused, $CUR added"
CELLGLIDE_GPG_KEY="$FPR" "$ROOT/scripts/sign-and-publish.sh" --dry-run | tee "$WORK/run3.log" | grep -E 'published bytes|conffiles ok|reusing|signing|signatures'
grep -qxF "published bytes ok: $PREV" "$WORK/run3.log" || { echo "FAIL: $PREV not checked against the record"; exit 1; }
grep -q "^signing cellglide" "$WORK/run3.log" && { echo "FAIL: re-signed something"; exit 1; }
[ "$(cd "$WORK/signed/$PREV" && shasum -a 256 ./*.rpm)" = "$prev_rpm_sum" ] || { echo "FAIL: $PREV signed rpm changed"; exit 1; }
find "$ROOT/repo" -name '*.deb' -o -name '*.rpm' | sed "s#$ROOT/##" | sort

step "gpgv on InRelease and Release.gpg with the throwaway keyring"
gpgv --keyring "$WORK/keys/cellglide-archive-keyring.gpg" "$ROOT/repo/apt/dists/stable/InRelease"
gpgv --keyring "$WORK/keys/cellglide-archive-keyring.gpg" "$ROOT/repo/apt/dists/stable/Release.gpg" "$ROOT/repo/apt/dists/stable/Release"

step "local HTTP server on port $PORT"
python3 -m http.server "$PORT" --directory "$ROOT/repo" --bind 0.0.0.0 >"$WORK/http.log" 2>&1 &
SERVER_PID=$!
sleep 1
curl -fsS "http://127.0.0.1:$PORT/apt/dists/stable/InRelease" | head -2
URL="http://host.lima.internal:$PORT"

step "release-linux.json: exists, four entries, every URL fetchable from the local server"
[ -s "$ROOT/repo/release-linux.json" ] || { echo "FAIL: repo/release-linux.json missing"; exit 1; }
urls=$(python3 - "$ROOT/repo/release-linux.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
assert len(d["files"]) == 4, d
print("\n".join(f["url"] for f in d["files"]))
PY
)
[ "$(printf '%s\n' "$urls" | wc -l | tr -d ' ')" -eq 4 ] || { echo "FAIL: release-linux.json does not list four files"; exit 1; }
while read -r u; do
    case "$u" in https://pkg.cellglide.com/*) ;; *) echo "FAIL: unexpected URL $u"; exit 1 ;; esac
    local_u="http://127.0.0.1:$PORT/${u#https://pkg.cellglide.com/}"
    curl -fsI "$local_u" >/dev/null || { echo "FAIL: $local_u not served"; exit 1; }
    echo "ok $local_u"
done <<< "$urls"

lima_up() { # $1 vm  $2 template
    limactl delete -f "$1" >/dev/null 2>&1 || true
    limactl create --tty=false --name "$1" "template://$2" >/dev/null
    limactl start "$1" >/dev/null 2>&1 || { limactl stop -f "$1" >/dev/null 2>&1 || true; limactl start "$1" >/dev/null; }
}

step "ubuntu-24.04 VM: setup-apt.sh, install, remove, update, reinstall, purge"
lima_up cg-repo-ubuntu ubuntu-24.04
limactl copy "$ROOT/packaging/keys/cellglide-archive-keyring.gpg" cg-repo-ubuntu:/tmp/real.gpg
limactl copy "$ROOT/packaging/repo/cellglide.sources" cg-repo-ubuntu:/tmp/prod.sources
limactl copy "$WORK/keys/cellglide-archive-keyring.gpg" cg-repo-ubuntu:/tmp/throwaway.gpg
limactl shell cg-repo-ubuntu -- sudo env URL="$URL" bash -eu <<'VM'
export DEBIAN_FRONTEND=noninteractive
curl -fsS "$URL/apt/dists/stable/InRelease" | head -1
echo "-- wrong key must fail"
printf 'Types: deb\nURIs: %s/apt\nSuites: stable\nComponents: main\nSigned-By: /tmp/real.gpg\n' "$URL" > /etc/apt/sources.list.d/cg-bad.sources
rc=0; apt-get update >/tmp/bad.log 2>&1 || rc=$?
[ "$rc" -ne 0 ] || { cat /tmp/bad.log; echo "FAIL: apt-get update exit 0 with the wrong key"; exit 1; }
grep -q 'NO_PUBKEY' /tmp/bad.log || { cat /tmp/bad.log; echo "FAIL: no NO_PUBKEY message"; exit 1; }
echo "wrong key rejected (good): exit $rc, NO_PUBKEY"
rm -f /etc/apt/sources.list.d/cg-bad.sources
echo "-- setup-apt.sh through curl | sh (installs the package)"
echo 'force-confold' > /etc/dpkg/dpkg.cfg.d/confold   # local URL/key differ from the packaged conffiles; production ones are identical
curl -fsSL "$URL/setup-apt.sh" | CELLGLIDE_REPO_URL="$URL" sh 2>&1 | tail -4
dpkg -s cellglide | grep -E '^(Status|Version)'
cat /etc/apt/sources.list.d/cellglide.sources
dpkg-query -W -f='${Conffiles}\n' cellglide | grep -E 'sources.list.d/cellglide.sources|apt/keyrings/cellglide-archive-keyring.gpg'
echo "enabled after install: $(systemctl is-enabled cellglide)"
echo "-- remove, then apt update must still work"
apt-get remove -y cellglide >/dev/null 2>&1
test -f /etc/apt/sources.list.d/cellglide.sources && test -f /etc/apt/keyrings/cellglide-archive-keyring.gpg
apt-get update 2>&1 | tee /tmp/up.log | tail -3
if grep -E '^(W|E):' /tmp/up.log; then echo "FAIL: apt update complained after remove"; exit 1; fi
echo "apt update after remove: clean"
echo "-- reinstall"
apt-get install -y cellglide >/dev/null 2>&1
echo "enabled after reinstall: $(systemctl is-enabled cellglide)"
test "$(systemctl is-enabled cellglide)" = enabled
echo "-- purge"
apt-get purge -y cellglide >/dev/null 2>&1
if test -e /etc/apt/sources.list.d/cellglide.sources || test -e /etc/apt/keyrings/cellglide-archive-keyring.gpg; then echo "FAIL: purge left repository files"; ls -l /etc/apt/sources.list.d /etc/apt/keyrings; exit 1; fi
echo "purge removed sources file and keyring"
echo "-- production case without force-confold: repository files already placed as setup-apt.sh places them"
rm -f /etc/dpkg/dpkg.cfg.d/confold
echo "dpkg options: $(cat /etc/dpkg/dpkg.cfg.d/* 2>/dev/null | grep -i conf || echo none); apt: $(apt-config dump | grep -i 'Dpkg::Options' || echo none)"
install -d -m 0755 /etc/apt/keyrings
# Byte-identical to the package's conffiles: setup-apt.sh downloads exactly packaging/keys' keyring
# and writes exactly packaging/repo/cellglide.sources (sign-and-publish.sh checks both).
install -m 644 /tmp/real.gpg /etc/apt/keyrings/cellglide-archive-keyring.gpg
install -m 644 /tmp/prod.sources /etc/apt/sources.list.d/cellglide.sources
# Nothing leaves the VM: the production host resolves to the VM itself (connection refused).
echo "127.0.0.1 pkg.cellglide.com" >> /etc/hosts
install -m 644 /tmp/throwaway.gpg /etc/apt/keyrings/cg-rehearsal.gpg
printf 'Types: deb\nURIs: %s/apt\nSuites: stable\nComponents: main\nSigned-By: /etc/apt/keyrings/cg-rehearsal.gpg\n' "$URL" > /etc/apt/sources.list.d/cg-local.sources
apt-get update >/tmp/up2.log 2>&1 || true
grep -E 'cellglide.com|host.lima.internal' /tmp/up2.log | head -4
rc=0; apt-get install -y cellglide </dev/null >/tmp/noconfold.log 2>&1 || rc=$?
tail -3 /tmp/noconfold.log
[ "$rc" -eq 0 ] || { cat /tmp/noconfold.log; echo "FAIL: install without force-confold exit $rc"; exit 1; }
if grep -E "Configuration file|conffile" /tmp/noconfold.log; then echo "FAIL: conffile prompt text in the install log"; exit 1; fi
cmp /tmp/real.gpg /etc/apt/keyrings/cellglide-archive-keyring.gpg && cmp /tmp/prod.sources /etc/apt/sources.list.d/cellglide.sources
dpkg-query -W -f='${Version} ${Conffiles}\n' cellglide | grep -E 'sources.list.d/cellglide.sources|apt/keyrings/cellglide-archive-keyring.gpg' | head -1
echo "installed without force-confold: exit 0, no conffile prompt, files unchanged"
VM
limactl delete -f cg-repo-ubuntu >/dev/null

step "fedora-42 VM: repo_gpgcheck=1, gpgcheck=1"
lima_up cg-repo-fedora fedora-42
limactl copy "$WORK/keys/RPM-GPG-KEY-cellglide" cg-repo-fedora:/tmp/throwaway.key
limactl copy "$ROOT/packaging/keys/RPM-GPG-KEY-cellglide" cg-repo-fedora:/tmp/real.key
limactl shell cg-repo-fedora -- sudo env URL="$URL" CUR="$CUR" PREV="$PREV" bash -eu <<'VM'
repo() { # $1 id  $2 key file
    printf '[%s]\nname=%s\nbaseurl=%s/rpm/$basearch\nenabled=1\ngpgcheck=1\nrepo_gpgcheck=1\ngpgkey=file://%s\n' "$1" "$1" "$URL" "$2" > "/etc/yum.repos.d/$1.repo"
}
echo "-- wrong key must fail"
repo cg-bad /tmp/real.key
# dnf5 skips a repository whose metadata fails to verify and still exits 0 by default; with
# skip_if_unavailable=False the failure must surface as a non-zero exit.
rc=0; dnf -y --disablerepo='*' --enablerepo=cg-bad --setopt=cg-bad.skip_if_unavailable=False makecache >/tmp/bad.log 2>&1 || rc=$?
[ "$rc" -ne 0 ] || { cat /tmp/bad.log; echo "FAIL: makecache exit 0 with the wrong key"; exit 1; }
tail -3 /tmp/bad.log
grep -qiE 'signature|gpg|key' /tmp/bad.log || { cat /tmp/bad.log; echo "FAIL: no signature message"; exit 1; }
echo "wrong key rejected (good): exit $rc"
rm -f /etc/yum.repos.d/cg-bad.repo
# Which of the two is older is decided by rpm itself (a "~" sorts lower), so this works whichever is newer.
cmp=$(rpm --eval "%{lua: print(rpm.vercmp('$CUR', '$PREV'))}")
case "$cmp" in
    1) OLD=$PREV; NEW=$CUR ;;
    -1) OLD=$CUR; NEW=$PREV ;;
    *) echo "FAIL: $CUR and $PREV compare equal (rpm.vercmp $cmp)"; exit 1 ;;
esac
echo "older: $OLD, newer: $NEW"
echo "-- dnf install <URL of the $OLD rpm>"
dnf install -y "$URL/rpm/aarch64/cellglide-$OLD-1.aarch64.rpm" 2>&1 | tail -6
rpm -q cellglide | grep -F "cellglide-$OLD-1" || { echo "FAIL: $OLD not installed"; exit 1; }
rpm -qi cellglide | grep -E '^Signature'
grep -E '^(baseurl|gpgcheck|repo_gpgcheck|gpgkey)=' /etc/yum.repos.d/cellglide.repo
echo "-- dnf upgrade --refresh through the packaged cellglide.repo (URL and key pointed at the rehearsal)"
dnf upgrade --refresh -y --setopt=cellglide.baseurl="$URL/rpm/\$basearch" --setopt=cellglide.gpgkey=file:///tmp/throwaway.key cellglide 2>&1 | tail -12
rpm -q cellglide | grep -F "cellglide-$NEW-1" || { echo "FAIL: not upgraded to $NEW"; exit 1; }
rpm -qi cellglide | grep -E '^Signature'
rpm -q gpg-pubkey --qf '%{SUMMARY}\n'
echo "service after upgrade: $(systemctl is-active cellglide) $(systemctl is-enabled cellglide)"
[ "$(systemctl is-active cellglide)" = active ] || { echo "FAIL: service not active after upgrade"; exit 1; }
VM
limactl delete -f cg-repo-fedora >/dev/null

step "DONE: rehearsal passed"
