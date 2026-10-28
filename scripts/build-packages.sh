#!/bin/sh
# Builds the deb and rpm for this machine's architecture inside the Ubuntu 22.04 builder.
# Usage: scripts/build-packages.sh      Output: dist/<version>/ (version = scripts/pkg-version.sh,
# i.e. the workspace version in deb/rpm spelling, e.g. 1.0.0~beta.1)
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=$("$ROOT/scripts/pkg-version.sh")
ENGINE=$(command -v podman || command -v docker || true)
[ -n "$ENGINE" ] || { echo "needs podman or docker" >&2; exit 1; }
IMAGE=cellglide-builder:22.04
# --network host: no container bridge or firewall rules on the build machine (AGENTS.md: do not touch other adapters).
"$ENGINE" build --network host -t "$IMAGE" -f "$ROOT/packaging/builder.Dockerfile" "$ROOT/packaging"
mkdir -p "$ROOT/dist/$VERSION"
"$ENGINE" run --rm --network host \
    -v "$ROOT":/src \
    -v cellglide-builder-registry:/root/.cargo/registry \
    -e VERSION="$VERSION" \
    -e CARGO_TARGET_DIR=/src/target-pkg \
    "$IMAGE" sh -c '
        set -eu
        git config --global --add safe.directory /src
        cargo build --release --locked -p cg-service -p cg-cli -p cg-gui
        strip target-pkg/release/cellglide-service target-pkg/release/cellglide target-pkg/release/cellglide-gui
        arch=$(uname -m)
        final="dist/$VERSION"
        out="$final/.stage-$arch"
        rm -rf "$out"; mkdir -p "$out"
        cargo deb -p cg-service --no-build --no-strip -o "$out/"
        deb=$(ls "$out"/*.deb)
        case "$(dpkg-deb -f "$deb" Version)" in
            "$VERSION"*) ;;
            *) echo "deb version $(dpkg-deb -f "$deb" Version) does not start with $VERSION" >&2; exit 1 ;;
        esac
        cargo generate-rpm -p crates/cg-service --target-dir target-pkg \
            --set-metadata "version = \"$VERSION\"" -o "$out/"
        (cd "$out" && sha256sum *.deb *.rpm > "SHA256SUMS.$arch")
        # Replace only this architecture own files; the other architecture files stay.
        rm -f "$final/SHA256SUMS.$arch"
        mv -f "$out"/* "$final"/
        rmdir "$out"
        chown -R "$(stat -c %u:%g /src)" "$final"
    '
ls -l "$ROOT/dist/$VERSION"
