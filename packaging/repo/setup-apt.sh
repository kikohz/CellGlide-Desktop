#!/bin/sh
# One-step CellGlide apt setup for Debian / Ubuntu:
#   curl -fsSL https://pkg.cellglide.com/setup-apt.sh | sudo sh
# Installs the signing keyring, adds the repository and installs the cellglide package.
# CELLGLIDE_REPO_URL overrides the server (testing only).
# All logic sits in main, called on the last line, so a truncated download runs nothing.
set -eu

main() {
    BASE=${CELLGLIDE_REPO_URL:-https://pkg.cellglide.com}
    KEYRING=/etc/apt/keyrings/cellglide-archive-keyring.gpg
    SOURCES=/etc/apt/sources.list.d/cellglide.sources

    print_sources() {
        cat <<SRC
Types: deb
URIs: $BASE/apt
Suites: stable
Components: main
Signed-By: $KEYRING
SRC
    }

    if [ "${CELLGLIDE_PRINT_SOURCES:-}" = 1 ]; then
        print_sources
        return 0
    fi

    [ "$(id -u)" -eq 0 ] || { echo "setup-apt.sh must run as root: curl -fsSL $BASE/setup-apt.sh | sudo sh" >&2; return 1; }
    command -v curl >/dev/null 2>&1 || { echo "curl is required: apt-get install -y curl" >&2; return 1; }

    tmp=$(mktemp)
    trap 'rm -f "$tmp"' EXIT
    curl -fsSL "$BASE/cellglide-archive-keyring.gpg" -o "$tmp"
    [ -d /etc/apt/keyrings ] || install -d -m 0755 /etc/apt/keyrings
    install -m 644 "$tmp" "$KEYRING"
    print_sources > "$SOURCES"
    chmod 644 "$SOURCES"

    apt-get update </dev/null
    apt-get install -y cellglide </dev/null
}

main "$@"
