#!/usr/bin/env python3
"""Writes the download data the website reads (release-linux.json) to stdout.

Usage: release-linux-json.py --root <repo root> --pkgver <pkgver> --date <YYYY-MM-DD> [--base URL] [--signed DIR]

The debs come from dist/<pkgver>/ (checked against SHA256SUMS.<arch>); the rpms come from the
signed copies in <signed>/<pkgver>/ (default <root>/signed.noindex), because those are the bytes that get published.
"""
import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Optional

DEFAULT_BASE = "https://pkg.cellglide.com"

# (kind, arch, SHA256SUMS suffix of the build machine that produced the file)
ENTRIES = [
    ("deb", "amd64", "x86_64"),
    ("deb", "arm64", "aarch64"),
    ("rpm", "x86_64", "x86_64"),
    ("rpm", "aarch64", "aarch64"),
]


class ReleaseDataError(Exception):
    pass


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def only_match(directory: Path, pattern: str, what: str) -> Path:
    if not directory.is_dir():
        raise ReleaseDataError(f"{directory} does not exist ({what})")
    found = sorted(directory.glob(pattern))
    if len(found) != 1:
        raise ReleaseDataError(f"{what}: expected exactly one {pattern} in {directory}, found {len(found)}")
    return found[0]


def recorded_sums(path: Path) -> dict:
    sums = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2:
            sums[parts[1].strip().lstrip("*")] = parts[0].lower()
    return sums


def build(root: Path, pkgver: str, date: str, base: str, signed_dir: Optional[Path] = None) -> dict:
    root = Path(root)
    base = base.rstrip("/")
    dist = root / "dist" / pkgver
    signed = (Path(signed_dir) if signed_dir else root / "signed.noindex") / pkgver
    files = []
    for kind, arch, sums_suffix in ENTRIES:
        if kind == "deb":
            path = only_match(dist, f"cellglide_{pkgver}-*_{arch}.deb", f"{arch} deb")
            sums_file = dist / f"SHA256SUMS.{sums_suffix}"
            if not sums_file.is_file():
                raise ReleaseDataError(f"{sums_file} is missing")
            digest = sha256_of(path)
            expected = recorded_sums(sums_file).get(path.name)
            if expected is None:
                raise ReleaseDataError(f"{path.name} is not listed in {sums_file.name}")
            if expected != digest:
                raise ReleaseDataError(f"{path.name}: sha256 {digest} differs from {sums_file.name} ({expected})")
            url = f"{base}/apt/pool/main/c/cellglide/{path.name}"
        else:
            path = only_match(signed, f"cellglide-{pkgver}-*.{arch}.rpm", f"{arch} rpm (signed copy)")
            digest = sha256_of(path)
            url = f"{base}/rpm/{arch}/{path.name}"
        files.append({"kind": kind, "arch": arch, "url": url, "size": path.stat().st_size, "sha256": digest})
    return {"version": pkgver.replace("~", "-", 1), "date": date, "files": files}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", required=True, type=Path)
    ap.add_argument("--pkgver", required=True)
    ap.add_argument("--date", required=True)
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument("--signed", type=Path, help="directory holding <pkgver>/ with the signed rpms (default <root>/signed.noindex)")
    args = ap.parse_args()
    try:
        data = build(args.root, args.pkgver, args.date, args.base, args.signed)
    except ReleaseDataError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    sys.stdout.write(json.dumps(data, indent=2, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
