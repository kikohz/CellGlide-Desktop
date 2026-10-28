#!/usr/bin/env python3
"""Prints the distinct license comment blocks of the C sources under the given directories
(used by scripts/update-licenses.sh).

For every .c/.h file the first /* ... */ comment containing "Copyright" is taken (only if it states terms), the comment
markers and leading '*' are stripped, whitespace is normalised, and identical blocks are
printed once under a heading naming the files they came from. Blocks that repeat a text in
`--skip` (a license file already in the list) are left out. Usage:
  extract-notices.py [--skip FILE]... ROOT DIR..."""
import re
import sys
from pathlib import Path

args = sys.argv[1:]
skip = []
while args and args[0] == "--skip":
    skip.append(args[1])
    args = args[2:]
root, dirs = Path(args[0]), args[1:]


def norm(text):
    return " ".join(text.split())


TERMS = re.compile(r"redistribut|permission|licen[cs]e|warrant", re.I)


def block_of(text):
    for m in re.finditer(r"/\*.*?\*/", text, re.S):
        if "Copyright" in m.group(0):
            body = m.group(0)[2:-2]
            lines = [re.sub(r"^\s*\*+ ?", "", l).rstrip() for l in body.split("\n")]
            lines = [l[1:] if l.startswith("*") else l for l in lines]
            # A file header that only names its author (hev's style) has no terms to carry.
            if not TERMS.search(body):
                return None
            return "\n".join(lines).strip("\n"), norm(" ".join(lines))
    return None


skipped = {norm(Path(s).read_text()) for s in skip}
groups = {}
for d in dirs:
    for path in sorted((root / d).rglob("*")):
        if path.suffix not in (".c", ".h") or not path.is_file():
            continue
        found = block_of(path.read_text(errors="replace"))
        if not found or any(s and s in found[1] for s in skipped):
            continue
        groups.setdefault(found[1], (found[0], []))[1].append(str(path.relative_to(root)))

print(f"{len(groups)} distinct notices", file=sys.stderr)
for text, files in sorted(groups.values(), key=lambda g: (-len(g[1]), g[0])):
    print("=" * 80)
    shown = ", ".join(files[:4]) + (f", ... ({len(files)} files)" if len(files) > 4 else "")
    print(f"Notice in: {shown}\n")
    print(text + "\n")
