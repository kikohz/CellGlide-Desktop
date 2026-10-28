#!/bin/sh
# Runs the workspace tests in a Linux container. Usage: scripts/linux-test.sh [cargo test args...]
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
docker run --rm --privileged \
    -v "$ROOT":/src -w /src \
    -v cellglide-cargo:/usr/local/cargo/registry \
    -v cellglide-target:/src/target \
    rust:1-bookworm \
    sh -c 'apt-get update -qq && apt-get install -y -qq iproute2 >/dev/null && cargo test --workspace "$@"' sh "$@"
