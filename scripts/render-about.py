"""Renders cargo-about JSON output from one or more runs as packaging/licenses/about.hbs would.

Usage: python3 -I scripts/render-about.py RUN.json [RUN.json ...] > list.txt

scripts/update-licenses.sh uses it for the Windows Rust list: cargo-about can scan either the
whole workspace or a single crate, and the Windows list must cover only the crates the Windows
package ships (cg-ffi, cg-cli, cg-service), not cg-gui, the Linux window (iced, ksni). So the
script runs cargo-about once per shipped crate with `--format json`, and this merges the runs
the way cargo-about itself groups a single run (cargo-about 0.9.2, src/generate.rs): one entry
per distinct (license name, text), ordered by name then text, then stably by SPDX id; each
entry's crates deduplicated and ordered by package id. The text is then what about.hbs makes of
that list. `--self-test RUN.json HBS_OUTPUT` checks that rendering one run gives exactly what
cargo-about rendered with about.hbs from the same run.
"""

import json
import sys


def merge(runs):
    by_key = {}
    for run in runs:
        for lic in run["licenses"]:
            key = (lic["name"], lic["text"])
            entry = by_key.setdefault(
                key, {"name": lic["name"], "id": lic["id"], "text": lic["text"], "used_by": {}}
            )
            for used in lic["used_by"]:
                krate = used["crate"]
                entry["used_by"][krate["id"]] = krate
    # BTreeMap<name, BTreeMap<text, _>> flattened, then a stable sort by id, as cargo-about does.
    # Rust compares strings by bytes; so does Python for UTF-8-encoded keys.
    ordered = [by_key[k] for k in sorted(by_key, key=lambda k: (k[0].encode(), k[1].encode()))]
    ordered.sort(key=lambda e: e["id"].encode())
    return ordered


def render(licenses):
    out = []
    for lic in licenses:
        out.append("=" * 80 + "\n")
        out.append(lic["name"] + "\n")
        out.append("\n")
        out.append("Used by:\n")
        for kid in sorted(lic["used_by"], key=str.encode):
            krate = lic["used_by"][kid]
            out.append(f"  - {krate['name']} {krate['version']}\n")
        out.append("\n")
        out.append(lic["text"] + "\n")
        out.append("\n")
    return "".join(out)


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def main(argv):
    out = sys.stdout
    out.reconfigure(encoding="utf-8", newline="\n")
    if len(argv) == 3 and argv[0] == "--self-test":
        with open(argv[2], encoding="utf-8", newline="") as f:
            expected = f.read()
        got = render(merge([load(argv[1])]))
        if got != expected:
            print("render-about.py: rendering differs from cargo-about's about.hbs output", file=sys.stderr)
            return 1
        print("render-about.py: matches cargo-about's about.hbs output", file=sys.stderr)
        return 0
    if not argv or argv[0].startswith("-"):
        print(__doc__, file=sys.stderr)
        return 2
    out.write(render(merge([load(p) for p in argv])))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
