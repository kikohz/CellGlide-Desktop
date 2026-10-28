#!/bin/sh
# Prints the workspace version in deb/rpm spelling ("-" becomes "~", so 1.0.0-beta.1
# prints 1.0.0~beta.1). The Cargo.toml workspace version is the single source.
# cargo-deb derives the same spelling by itself (1.0.0~beta.1-1); cargo-generate-rpm 0.21
# rejects "-" in a version, so Task 6's build script passes this value to it:
#   cargo generate-rpm -p crates/cg-service --set-metadata "version = \"$(scripts/pkg-version.sh)\""
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' "$ROOT/Cargo.toml" | head -n1 | tr - '~'
