#!/usr/bin/env python3
"""Writes the Windows release data (SHA256SUMS, latest.json, release-windows.json) into the dist folder.

Usage: python -I scripts/release-windows-json.py --dist <dir> --version <v> --date <YYYY-MM-DD> [--base URL] [--arm64-tested]

<dir> is what scripts/build-windows.ps1 wrote for both architectures (dist/windows/<v>/):
CellGlide-<v>-windows-x64.zip, CellGlide-<v>-windows-arm64.zip, SHA256SUMS.x64 and SHA256SUMS.arm64.
Each zip's actual SHA-256 must equal the one in its SHA256SUMS.<arch>, which must name that zip and
nothing else. The output (plan decision 14):
  SHA256SUMS            both zips, "<hex>  <name>", sorted by file name (install.ps1 reads it)
  latest.json           {"version", "x64": {url, sha256, size, test_build}, "arm64": {...}}
  release-windows.json  {"version", "date", "install", "uninstall", "files": [x64 zip, arm64 zip]},
                        copied to CellProxy Website/release-windows.json
ARM64 is marked test_build until --arm64-tested is given (spec 14.1); x64 never is. --base defaults
to https://pkg.cellglide.com; the only other values allowed are loopback test sources
(http://127.0.0.1:<port>, http://localhost:<port>), the same rule install.ps1 applies, for local
rehearsals only: scripts/publish-windows.ps1 refuses to upload such data. On a data error nothing
is written; if writing itself fails part way, the old files are put back as they were (see
write_all), so the folder never holds a mixed set. Either way the exit code is 1.
"""
import argparse
import datetime
import hashlib
import json
import os
import re
import sys
from pathlib import Path

DEFAULT_BASE = "https://pkg.cellglide.com"
ARCHES = ("x64", "arm64")  # the order of latest.json and of release-windows.json "files"
VERSION_RE = re.compile(r"\A\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?\Z")
LOOPBACK_RE = re.compile(r"\Ahttp://(127\.0\.0\.1|localhost):([0-9]{1,5})\Z")
SUMS_LINE_RE = re.compile(r"\A([0-9a-fA-F]{64}) [ *](\S.*)\Z")


class ReleaseDataError(Exception):
    pass


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def check_base(base: str) -> str:
    base = base.rstrip("/")
    if base == DEFAULT_BASE:
        return base
    m = LOOPBACK_RE.match(base)
    if m and 1 <= int(m.group(2)) <= 65535:
        return base
    raise ReleaseDataError(f"base {base!r} is neither {DEFAULT_BASE} nor http://127.0.0.1:<port> / http://localhost:<port>")


def recorded_sum(sums_file: Path, zip_name: str, arch: str) -> str:
    """The hash SHA256SUMS.<arch> records for zip_name; the file must name that zip and nothing else."""
    entries = []
    for line in sums_file.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        m = SUMS_LINE_RE.match(line)
        if not m:
            raise ReleaseDataError(f"{arch}: bad line in {sums_file.name}: {line!r}")
        entries.append((m.group(2), m.group(1).lower()))
    names = [name for name, _ in entries]
    if names != [zip_name]:
        raise ReleaseDataError(f"{arch}: {sums_file.name} must list exactly {zip_name}, it lists {names}")
    return entries[0][1]


def build(dist: Path, version: str, date: str, base: str, arm64_test_build: bool) -> tuple:
    """Returns (sha256sums_text, latest, website); raises ReleaseDataError naming the architecture and reason."""
    dist = Path(dist)
    if not VERSION_RE.match(version):
        raise ReleaseDataError(f"bad version {version!r}")
    try:
        if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", date):
            raise ValueError
        datetime.date.fromisoformat(date)
    except ValueError:
        raise ReleaseDataError(f"bad date {date!r} (YYYY-MM-DD)") from None
    base = check_base(base)
    if not dist.is_dir():
        raise ReleaseDataError(f"{dist} is not a folder")

    entries = {}
    for arch in ARCHES:
        name = f"CellGlide-{version}-windows-{arch}.zip"
        zip_path = dist / name
        sums_file = dist / f"SHA256SUMS.{arch}"
        if not zip_path.is_file():
            raise ReleaseDataError(f"{arch}: {name} is missing from {dist}")
        if not sums_file.is_file():
            raise ReleaseDataError(f"{arch}: SHA256SUMS.{arch} is missing from {dist}")
        expected = recorded_sum(sums_file, name, arch)
        digest = sha256_of(zip_path)
        if digest != expected:
            raise ReleaseDataError(f"{arch}: {name} has sha256 {digest}, SHA256SUMS.{arch} says {expected}")
        entries[arch] = (name, {
            "url": f"{base}/windows/{version}/{name}",
            "sha256": digest,
            "size": zip_path.stat().st_size,
            "test_build": arch == "arm64" and bool(arm64_test_build),
        })

    sums_text = "".join(f"{e['sha256']}  {name}\n" for name, e in sorted(entries.values(), key=lambda t: t[0]))
    latest = {"version": version}
    for arch in ARCHES:
        latest[arch] = dict(entries[arch][1])
    website = {
        "version": version,
        "date": date,
        "install": f"irm {base}/install.ps1 | iex",
        "uninstall": f"irm {base}/uninstall.ps1 | iex",
        "files": [],
    }
    for arch in ARCHES:
        e = entries[arch][1]
        website["files"].append(
            {"kind": "zip", "arch": arch, "url": e["url"], "size": e["size"], "sha256": e["sha256"], "test_build": e["test_build"]})
    return sums_text, latest, website


def write_all(dist: Path, outputs: dict) -> None:
    """Writes the set of files so that it is never left mixed (LF, UTF-8).

    1. Every new file is written to <name>.tmp; nothing else is touched until all are written.
    2. Existing files of these names are moved aside to <name>.bak.
    3. The .tmp files are renamed into place, in order.
    If step 2 or 3 fails, the new files already in place are removed and the .bak files are moved
    back, so the folder ends with either the complete new set or the old files exactly as they
    were. The .tmp and .bak files are removed in every case (best effort).
    """
    temps = []
    backups = []
    placed = []
    try:
        for name, text in outputs.items():
            tmp = dist / f"{name}.tmp"
            temps.append((tmp, dist / name))
            with open(tmp, "w", encoding="utf-8", newline="\n") as f:
                f.write(text)
        for _, final in temps:
            if final.exists():
                bak = final.with_name(final.name + ".bak")
                os.replace(final, bak)
                backups.append((bak, final))
        for tmp, final in temps:
            os.replace(tmp, final)
            placed.append(final)
    except BaseException:
        for final in placed:
            try:
                final.unlink()
            except OSError:
                pass
        for bak, final in backups:
            try:
                os.replace(bak, final)
            except OSError:
                pass
        raise
    finally:
        for path in [t for t, _ in temps] + [b for b, _ in backups]:
            try:
                if path.exists():
                    path.unlink()
            except OSError:
                pass


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dist", required=True, type=Path)
    ap.add_argument("--version", required=True)
    ap.add_argument("--date", required=True)
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument("--arm64-tested", action="store_true", help="ARM64 has been tested on real hardware (no test_build mark)")
    args = ap.parse_args()
    try:
        sums, latest, website = build(args.dist, args.version, args.date, args.base, not args.arm64_tested)
        write_all(args.dist, {
            "SHA256SUMS": sums,
            "latest.json": json.dumps(latest, indent=2, ensure_ascii=False) + "\n",
            "release-windows.json": json.dumps(website, indent=2, ensure_ascii=False) + "\n",
        })
    except (ReleaseDataError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    for name in ("SHA256SUMS", "latest.json", "release-windows.json"):
        print(args.dist / name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
