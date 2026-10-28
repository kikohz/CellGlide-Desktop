#!/bin/bash
# Signs the CellGlide packages and repository metadata, assembles repo/ (apt + rpm) and uploads
# it to Cloudflare R2. Run it on the publisher's Mac (needs GnuPG with the secret key, `rpmsign`
# from `brew install rpm`, Docker, and Node for wrangler).
#
# Usage: CELLGLIDE_GPG_KEY=<fingerprint> scripts/sign-and-publish.sh [--dry-run]
#
# - The version is the workspace version (scripts/pkg-version.sh); there is no version argument.
# - The repository holds dist/<v>/ for every version in packaging/repo/published-versions plus the
#   current version, so packages published earlier never disappear. Test builds in other dist/
#   directories are never touched. A real upload that completes appends the current version to
#   that list; commit the change.
# - dist/ keeps exactly what was built and checksummed (a real run only adds release-linux.json beside it).
#   Signed rpms are kept in signed.noindex/<v>/
#   (git-ignored): one that is already there, has the same header digest as dist/'s rpm and verifies
#   against the public key to be published is reused byte for byte; only new or changed rpms are
#   signed. repo/ is rebuilt from scratch on every run, from those signed copies.
# - Each signed copy has a <name>.sha256 with the sha256 of the dist/ rpm it came from; reuse needs a match.
# - Published bytes never change. packaging/repo/published.sha256 records, in sha256sum format, every
#   package file of every published version (path relative to dist/; for an rpm the hash is of the
#   signed copy that was uploaded). Each listed version is checked against it: a version with no
#   record, a package without a line, a mismatch, or an rpm with no signed copy in signed.noindex/
#   (it would have to be re-signed) stops the run. A completed real upload appends the current
#   version's lines; commit them with published-versions.
# - Only the current, not yet published version is checked against packaging/ by
#   scripts/check-package-conffiles.sh (older packages keep the files they shipped with, e.g. a
#   public key from before a renewal). The same script requires the current version's amd64 and
#   arm64 debs, and its x86_64 and aarch64 rpms, to match across architectures (unit, sysusers,
#   polkit rules, maintainer scripts), so a stale package from one build machine stops the run.
# - scripts/release-linux-json.py writes the website's download data: dist/<v>/release-linux.json
#   before any upload (a failure aborts the publish), repo/release-linux.json (never uploaded) on a --dry-run.
# - Every rpm in repo/ is verified in a fedora:42 container (rpm 4.20, like the clients) after signing.
# - GNUPGHOME is honoured (gpg does it by default; it is also passed to rpmsign).
# - CELLGLIDE_KEYS_DIR (default packaging/keys), CELLGLIDE_SIGNED_DIR (default signed.noindex),
#   CELLGLIDE_PUBLISHED_LIST and CELLGLIDE_PUBLISHED_RECORD (default packaging/repo/published-versions
#   and packaging/repo/published.sha256) exist for scripts/test-publish-local.sh only; a real run
#   refuses other published-version files.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BUCKET=cellglide-pkg
REPO="$ROOT/repo"
LIST=${CELLGLIDE_PUBLISHED_LIST:-$ROOT/packaging/repo/published-versions}
RECORD=${CELLGLIDE_PUBLISHED_RECORD:-$ROOT/packaging/repo/published.sha256}
KEYS_DIR=${CELLGLIDE_KEYS_DIR:-$ROOT/packaging/keys}
SIGNED=${CELLGLIDE_SIGNED_DIR:-$ROOT/signed.noindex}

DRY=0
case "${1:-}" in
    "") ;;
    --dry-run) DRY=1 ;;
    *) echo "usage: CELLGLIDE_GPG_KEY=<fingerprint> scripts/sign-and-publish.sh [--dry-run]" >&2; exit 2 ;;
esac
[ $# -le 1 ] || { echo "usage: CELLGLIDE_GPG_KEY=<fingerprint> scripts/sign-and-publish.sh [--dry-run]" >&2; exit 2; }

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 not found. $2"; }

# --- 0. Preflight ---------------------------------------------------------------------------
VERSION=$("$ROOT/scripts/pkg-version.sh")
[ -n "$VERSION" ] || die "scripts/pkg-version.sh printed nothing"

# The current version must be complete before anything else happens.
CUR="$ROOT/dist/$VERSION"
[ -d "$CUR" ] || die "dist/$VERSION is missing; run scripts/build-packages.sh on both architectures"
for pat in 'cellglide_*_amd64.deb' 'cellglide_*_arm64.deb' 'cellglide-*.x86_64.rpm' 'cellglide-*.aarch64.rpm' 'SHA256SUMS.x86_64' 'SHA256SUMS.aarch64'; do
    n=$(find "$CUR" -maxdepth 1 -name "$pat" | wc -l | tr -d ' ')
    [ "$n" -eq 1 ] || die "dist/$VERSION must contain exactly one $pat (found $n); build both architectures first"
done

KEY=${CELLGLIDE_GPG_KEY:-}
[ -n "$KEY" ] || die "set CELLGLIDE_GPG_KEY to the signing key fingerprint (see docs/release/signing-key.md)"
need gpg "Install GnuPG: brew install gnupg"
need rpmsign "Install it with: brew install rpm"
need docker "Docker (colima) is needed to generate the apt and rpm metadata and to verify the rpms"
need shasum "It ships with macOS"
[ "$DRY" = 1 ] || need npx "Install Node.js for wrangler"

FPR=$(gpg --list-secret-keys --with-colons "$KEY" 2>/dev/null | awk -F: '$1 == "fpr" { print $10; exit }' || true)
[ -n "$FPR" ] || die "no secret key matches CELLGLIDE_GPG_KEY=$KEY in ${GNUPGHOME:-~/.gnupg}.
Import the offline backup of the signing key first (gpg --import <backup>), or set GNUPGHOME
to the keyring that holds it. See docs/release/signing-key.md."

for f in cellglide-archive-keyring.gpg RPM-GPG-KEY-cellglide; do
    [ -f "$KEYS_DIR/$f" ] || die "missing $KEYS_DIR/$f"
    gpg --show-keys --with-colons "$KEYS_DIR/$f" 2>/dev/null | awk -F: '$1 == "fpr" { print $10 }' | grep -qx "$FPR" \
        || die "$KEYS_DIR/$f does not contain the public key $FPR; refusing to publish a key that cannot verify our signatures"
done

# A real run publishes exactly the public keys that the packages carry.
if [ "$DRY" != 1 ]; then
    for f in cellglide-archive-keyring.gpg RPM-GPG-KEY-cellglide; do
        cmp -s "$KEYS_DIR/$f" "$ROOT/packaging/keys/$f" || die "$KEYS_DIR/$f differs from packaging/keys/$f; a real run must publish the packaged keys (CELLGLIDE_KEYS_DIR is for rehearsals only)"
    done
    [ "$LIST" = "$ROOT/packaging/repo/published-versions" ] && [ "$RECORD" = "$ROOT/packaging/repo/published.sha256" ] \
        || die "a real run must use packaging/repo/published-versions and published.sha256 (CELLGLIDE_PUBLISHED_* are for rehearsals only)"
fi

PUBLISHED=$( { [ -f "$LIST" ] && sed 's/#.*//' "$LIST" | tr -d ' \t\r' | grep -v '^$' || true; } )
VERSIONS=$PUBLISHED
if ! printf '%s\n' "$PUBLISHED" | grep -qxF "$VERSION"; then
    VERSIONS=$(printf '%s\n%s\n' "$PUBLISHED" "$VERSION" | grep -v '^$')
fi
is_published() { printf '%s\n' "$PUBLISHED" | grep -qxF "$1"; }
echo "publishing versions: $(echo "$VERSIONS" | tr "\n" " ")"

# --- 1. Every version's dist directory must exist and match its checksums ---------------------
for v in $VERSIONS; do
    d="$ROOT/dist/$v"
    [ -d "$d" ] || die "dist/$v is missing. It is listed in packaging/repo/published-versions (or is the current version), and the repository must keep every published version."
    ls "$d"/SHA256SUMS.* >/dev/null 2>&1 || die "no SHA256SUMS.* in dist/$v"
    ls "$d"/*.deb "$d"/*.rpm >/dev/null 2>&1 || die "no packages in dist/$v"
    for s in "$d"/SHA256SUMS.*; do
        (cd "$d" && shasum -a 256 -c "$(basename "$s")" >/dev/null) || die "checksum mismatch in dist/$v ($(basename "$s")); rebuild the packages"
    done
    for p in "$d"/*.deb "$d"/*.rpm; do
        name=$(basename "$p")
        awk '{ sub(/^\*/, "", $2); print $2 }' "$d"/SHA256SUMS.* | grep -qxF "$name" \
            || die "$name in dist/$v is not covered by any SHA256SUMS file"
    done
    echo "checksums ok: dist/$v"
done

# Published versions: every package file must be byte-identical to what was uploaded.
# The record's lines for version v are "<sha256>  v/<file>".
record_of() { [ -f "$RECORD" ] && awk -v v="$1" '{ n = split($2, p, "/") } n == 2 && p[1] == v' "$RECORD" || true; }
for v in $PUBLISHED; do
    lines=$(record_of "$v")
    [ -n "$lines" ] || die "published version $v has no lines in packaging/repo/published.sha256; its bytes cannot be checked"
    for p in "$ROOT/dist/$v"/*.deb "$ROOT/dist/$v"/*.rpm; do
        [ -e "$p" ] || continue
        printf '%s\n' "$lines" | awk '{ print $2 }' | grep -qxF "$v/$(basename "$p")" \
            || die "dist/$v/$(basename "$p") is not in packaging/repo/published.sha256; a published version cannot gain packages"
    done
    while read -r sum path; do
        name=${path#*/}
        case "$name" in
            *.deb) file="$ROOT/dist/$path" ;;
            # An rpm was published as its signed copy; re-signing would change the published bytes.
            *.rpm) file="$SIGNED/$path"
                [ -f "$file" ] || die "published $path has no signed copy in $SIGNED; restore signed.noindex/ from its backup (re-signing would change published bytes)" ;;
            *) die "unexpected file in packaging/repo/published.sha256: $path" ;;
        esac
        [ -f "$file" ] || die "published $path is missing from dist/$v"
        [ "$(shasum -a 256 "$file" | cut -d' ' -f1)" = "$sum" ] || die "$path differs from the bytes recorded when it was published"
    done <<< "$lines"
    echo "published bytes ok: $v"
done

# The version being published now must carry exactly the repository files that packaging/ has.
if ! is_published "$VERSION"; then
    "$ROOT/scripts/check-package-conffiles.sh" "$ROOT/dist/$VERSION"/*.deb "$ROOT/dist/$VERSION"/*.rpm
fi

# --- 2. Lay out the repository ----------------------------------------------------------------
rm -rf "$REPO"
mkdir -p "$REPO/apt/pool/main/c/cellglide" "$REPO/rpm/x86_64" "$REPO/rpm/aarch64" "$SIGNED"
for v in $VERSIONS; do
    cp "$ROOT/dist/$v"/*.deb "$REPO/apt/pool/main/c/cellglide/"
done

# One-step apt setup. The template must produce exactly the packaged sources file.
CELLGLIDE_PRINT_SOURCES=1 sh "$ROOT/packaging/repo/setup-apt.sh" | cmp -s - "$ROOT/packaging/repo/cellglide.sources" \
    || die "packaging/repo/setup-apt.sh and packaging/repo/cellglide.sources disagree"
cp "$ROOT/packaging/repo/setup-apt.sh" "$REPO/setup-apt.sh"
cp "$KEYS_DIR/RPM-GPG-KEY-cellglide" "$KEYS_DIR/cellglide-archive-keyring.gpg" "$REPO/"

# --- 3. rpms: reuse verified signed copies, sign the rest, verify everything ------------------
# Runs in a fedora:42 container (rpm 4.20, the generation clients use). dist/ is mounted read-only.
fedora_rpm() { # script text; arguments follow
    local script=$1; shift
    docker run --rm --network host -v "$ROOT/dist":/dist:ro -v "$SIGNED":/signed:ro -v "$REPO":/repo:ro \
        -v "$KEYS_DIR":/keys:ro fedora:42 bash -c "$script" _ "$@"
}
CANDIDATES=""
for v in $VERSIONS; do
    for p in "$ROOT/dist/$v"/*.rpm; do
        [ -e "$p" ] && CANDIDATES="$CANDIDATES $v/$(basename "$p")"
    done
done
# shellcheck disable=SC2086,SC2016
REUSE=$(fedora_rpm '
    rpm --import /keys/RPM-GPG-KEY-cellglide
    for f in "$@"; do
        [ -f "/signed/$f" ] || continue
        hs=$(rpm -qp --qf "%{SHA256HEADER}" "/signed/$f" 2>/dev/null) || continue
        hd=$(rpm -qp --qf "%{SHA256HEADER}" "/dist/$f" 2>/dev/null) || continue
        [ -n "$hs" ] && [ "$hs" = "$hd" ] || continue
        rpm -K "/signed/$f" 2>&1 | grep -q "signatures OK" && echo "REUSE $f"
    done
    true' $CANDIDATES)

RPMSIGN_ARGS=(--define "__gpg $(command -v gpg)" --define "_gpg_name $FPR" --define "_gpg_digest_algo sha256")
if [ -n "${GNUPGHOME:-}" ]; then
    RPMSIGN_ARGS+=(--define "_gpg_path $GNUPGHOME")
fi
[ -n "${GPG_TTY:-}" ] || { GPG_TTY=$(tty 2>/dev/null) && export GPG_TTY || true; }
for f in $CANDIDATES; do
    v=${f%%/*}; name=${f#*/}
    mkdir -p "$SIGNED/$v"
    dsum=$(shasum -a 256 "$ROOT/dist/$f" | cut -d' ' -f1)
    if printf '%s\n' "$REUSE" | grep -qxF "REUSE $f" && [ "$(cat "$SIGNED/$f.sha256" 2>/dev/null)" = "$dsum" ]; then
        echo "reusing signed $name"
    elif is_published "$v"; then
        die "published $f can't be reused (its signed copy doesn't match dist/ or doesn't verify with the key to publish); refusing to re-sign it"
    else
        echo "signing $name"
        tmp="$SIGNED/$v/.signing.$name"
        cp "$ROOT/dist/$f" "$tmp"
        rpmsign "${RPMSIGN_ARGS[@]}" --addsign "$tmp" >/dev/null
        mv -f "$tmp" "$SIGNED/$v/$name"
        printf '%s\n' "$dsum" > "$SIGNED/$f.sha256"
    fi
    case "$name" in
        *.x86_64.rpm) cp "$SIGNED/$f" "$REPO/rpm/x86_64/" ;;
        *.aarch64.rpm) cp "$SIGNED/$f" "$REPO/rpm/aarch64/" ;;
        *) die "unexpected rpm architecture: $name" ;;
    esac
done

# Always: every rpm in repo/ must verify against the key that will be published.
NRPM=$(find "$REPO/rpm" -name '*.rpm' | wc -l | tr -d ' ')
RPMCHECK=$(fedora_rpm '
    rpm --import /repo/RPM-GPG-KEY-cellglide
    rpm -K /repo/rpm/*/*.rpm')
printf '%s\n' "$RPMCHECK"
if [ "$(printf '%s\n' "$RPMCHECK" | grep -c 'signatures OK')" -ne "$NRPM" ] || printf '%s\n' "$RPMCHECK" | grep -q 'NOT OK'; then
    die "rpm signature verification failed in the fedora:42 container"
fi

# --- 4. Metadata, generated in containers -----------------------------------------------------
docker run --rm --network host -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" -v "$REPO/apt":/repo -w /repo ubuntu:22.04 bash -euc '
    apt-get update -qq && apt-get install -y -qq apt-utils >/dev/null
    for arch in amd64 arm64; do
        mkdir -p dists/stable/main/binary-$arch
        apt-ftparchive --arch $arch packages pool > dists/stable/main/binary-$arch/Packages
        gzip -9kf dists/stable/main/binary-$arch/Packages
    done
    apt-ftparchive -o APT::FTPArchive::Release::Origin=CellGlide -o APT::FTPArchive::Release::Label=CellGlide \
        -o APT::FTPArchive::Release::Suite=stable -o APT::FTPArchive::Release::Codename=stable \
        -o APT::FTPArchive::Release::Architectures="amd64 arm64" -o APT::FTPArchive::Release::Components=main \
        release dists/stable > dists/stable/Release
    chown -R "$HOST_UID:$HOST_GID" /repo'
docker run --rm --network host -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" -v "$REPO/rpm":/repo -w /repo fedora:42 bash -euc '
    dnf install -y -q createrepo_c >/dev/null
    createrepo_c -q x86_64 && createrepo_c -q aarch64
    chown -R "$HOST_UID:$HOST_GID" /repo'

# --- 5. Sign the metadata on this Mac ---------------------------------------------------------
APT="$REPO/apt/dists/stable"
gpg --batch --yes --local-user "$FPR" --clearsign -o "$APT/InRelease" "$APT/Release"
gpg --batch --yes --local-user "$FPR" -abs -o "$APT/Release.gpg" "$APT/Release"
for arch in x86_64 aarch64; do
    gpg --batch --yes --local-user "$FPR" --detach-sign --armor -o "$REPO/rpm/$arch/repodata/repomd.xml.asc" "$REPO/rpm/$arch/repodata/repomd.xml"
done

# --- 6. Verify the metadata signatures: exactly one VALIDSIG, made by our key ------------------
check_sig() { # label, then the gpgv file arguments
    local label=$1; shift
    local out n good
    out=$(LC_ALL=C gpgv --status-fd 1 --keyring "$REPO/cellglide-archive-keyring.gpg" "$@" 2>/dev/null || true)
    n=$(printf '%s\n' "$out" | grep -c '^\[GNUPG:\] VALIDSIG ' || true)
    good=$(printf '%s\n' "$out" | awk -v f="$FPR" '$2 == "VALIDSIG" && $NF == f' | wc -l | tr -d ' ')
    [ "$n" -eq 1 ] && [ "$good" -eq 1 ] || die "$label: expected exactly one valid signature by $FPR (VALIDSIG count $n, by our key $good)"
}
check_sig InRelease "$APT/InRelease"
check_sig Release.gpg "$APT/Release.gpg" "$APT/Release"
for arch in x86_64 aarch64; do
    check_sig "repomd.xml ($arch)" "$REPO/rpm/$arch/repodata/repomd.xml.asc" "$REPO/rpm/$arch/repodata/repomd.xml"
done
echo "signatures verified"

# --- 7. Upload plan and upload ------------------------------------------------------------------
# Order matters: package files first, then repodata / Packages, then the signed index files that
# point at them. A client must never see metadata for files that are not there yet.
tier() {
    case "$1" in
        *.deb | *.rpm) echo 1 ;;
        apt/dists/stable/Release) echo 3 ;;
        apt/dists/stable/Release.gpg) echo 4 ;;
        apt/dists/stable/InRelease) echo 5 ;;
        rpm/*/repodata/repomd.xml) echo 6 ;;
        rpm/*/repodata/repomd.xml.asc) echo 7 ;;
        *) echo 2 ;;
    esac
}
ctype() {
    case "$1" in
        *.deb) echo application/vnd.debian.binary-package ;;
        *.rpm) echo application/x-rpm ;;
        *.gz) echo application/gzip ;;
        *.zst) echo application/zstd ;;
        *.xml) echo application/xml ;;
        *.gpg) echo application/pgp-keys ;;
        *.sh) echo text/x-shellscript ;;
        *) echo text/plain ;;
    esac
}
# Versioned package files and content-hashed repodata never change; everything a client polls must revalidate.
cache() {
    case "$1" in
        *.deb | *.rpm) echo "public, max-age=31536000, immutable" ;;
        rpm/*/repodata/repomd.xml*) echo "no-cache" ;;
        rpm/*/repodata/*) echo "public, max-age=31536000, immutable" ;;
        *) echo "no-cache" ;;
    esac
}
cd "$REPO"
ORDER=$(mktemp)
trap 'rm -f "$ORDER"' EXIT
find . -type f -not -path '*/.*' | sed 's#^\./##' | while read -r f; do echo "$(tier "$f") $f"; done | sort -s -k1,1n | cut -d' ' -f2- > "$ORDER"
echo "upload plan (order, content type, cache control):"
while read -r f; do
    printf '  %s | %s | %s\n' "$f" "$(ctype "$f")" "$(cache "$f")"
done < "$ORDER"

# What a completed upload records for the current version (dist/ bytes for debs, signed copies for rpms).
record_lines() {
    local p
    for p in "$ROOT/dist/$VERSION"/*.deb; do
        printf '%s  %s/%s\n' "$(shasum -a 256 "$p" | cut -d' ' -f1)" "$VERSION" "$(basename "$p")"
    done
    for p in "$SIGNED/$VERSION"/*.rpm; do
        printf '%s  %s/%s\n' "$(shasum -a 256 "$p" | cut -d' ' -f1)" "$VERSION" "$(basename "$p")"
    done
}

# The website's download data is generated BEFORE anything goes live: a failure here aborts the publish.
# Real run: dist/<v>/release-linux.json (never uploaded). Dry run: repo/release-linux.json (repo/ is rebuilt each run).
if [ "$DRY" = 1 ]; then JSON_OUT="$REPO/release-linux.json"; else JSON_OUT="$ROOT/dist/$VERSION/release-linux.json"; fi
python3 "$ROOT/scripts/release-linux-json.py" --root "$ROOT" --signed "$SIGNED" --pkgver "$VERSION" --date "$(date -u +%F)" > "$JSON_OUT.tmp" \
    || { rm -f "$JSON_OUT.tmp"; die "could not generate release-linux.json; nothing was uploaded"; }
mv -f "$JSON_OUT.tmp" "$JSON_OUT"

if [ "$DRY" = 1 ]; then
    if ! is_published "$VERSION"; then
        echo "dry run: a completed upload would add to published.sha256:"
        record_lines | sed 's/^/  /'
    fi
    echo "官网数据（预演）：repo/release-linux.json"
    echo "dry run: repository at $REPO, nothing uploaded"
    exit 0
fi
while read -r f; do
    npx --yes wrangler@4.31.0 r2 object put "$BUCKET/$f" --file "$f" --content-type "$(ctype "$f")" \
        --cache-control "$(cache "$f")" --remote < /dev/null > /dev/null
    echo "uploaded $f"
done < "$ORDER"
rm -f "$ORDER"

# Only a completed upload makes the version part of the published set, with its bytes recorded.
if ! is_published "$VERSION"; then
    record_lines >> "$RECORD"
    echo "$VERSION" >> "$LIST"
    echo "added $VERSION to packaging/repo/published-versions and its packages to published.sha256; commit both"
fi

echo "官网数据：dist/$VERSION/release-linux.json，复制到官网仓库的 release-linux.json"
