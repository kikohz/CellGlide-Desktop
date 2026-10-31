#!/usr/bin/env python3
"""Make the Windows app's icons:

- windows/CellGlide.App/Assets/Tray/cellglide-{idle,active,alert,active-alert}-{light,dark}.ico:
  the tray icon, redrawn from the geometry of packaging/icons/tray/*.svg (a 16 x 16 view box):
  the phone's rounded frame, the two short bars, the two signal arcs (active only) and the
  orange dot (#f57900, centre (12.5, 3.5), radius 3; alert only). Each pixel is sampled at
  8 x 8 points; the share of points inside the shapes is its alpha. "light" is drawn in #1F1F1F
  for a light taskbar, "dark" in #FFFFFF for a dark one.
- windows/CellGlide.App/Assets/AppIcon.ico: packaging/icons/{256,128,64}.png as they are, plus
  48, 32, 24 and 16 px scaled down from 256.png by area-weighted averaging.

Every .ico frame is a PNG. The PNGs written here use stored (uncompressed) deflate blocks, so
the bytes depend on nothing but this script: no zlib version or build changes them, and
`--check` gives the same answer on every machine.

Run:    python -I scripts/gen-windows-icons.py
Check:  python -I scripts/gen-windows-icons.py --check   (exit 1 if a committed file differs)

Standard library only. The PNG reader is scripts/gen-about-icon.py's, loaded by path because
`python -I` does not put this directory on sys.path."""
import importlib.util
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "windows/CellGlide.App/Assets"


def _decode_png():
    # No scripts/__pycache__ left behind.
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location(
        "gen_about_icon", ROOT / "scripts/gen-about-icon.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.decode_png


decode_png = _decode_png()

# ---- PNG and ICO writing ----


def _chunk(kind, body):
    return (
        struct.pack(">I", len(body))
        + kind
        + body
        + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)
    )


def _stored_zlib(data):
    """A zlib stream of stored deflate blocks (no compression), with its Adler-32."""
    out = bytearray(b"\x78\x01")
    pos = 0
    while True:
        block = data[pos : pos + 65535]
        pos += len(block)
        final = pos >= len(data)
        out += struct.pack("<BHH", 1 if final else 0, len(block), len(block) ^ 0xFFFF)
        out += block
        if final:
            break
    out += struct.pack(">I", zlib.adler32(data) & 0xFFFFFFFF)
    return bytes(out)


def encode_png(width, height, pixels):
    """An 8-bit RGBA PNG; each row with filter 0."""
    stride = width * 4
    raw = b"".join(
        b"\x00" + pixels[y * stride : (y + 1) * stride] for y in range(height)
    )
    return (
        b"\x89PNG\r\n\x1a\n"
        + _chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + _chunk(b"IDAT", _stored_zlib(raw))
        + _chunk(b"IEND", b"")
    )


def encode_ico(frames):
    """An .ico of PNG frames; `frames` is a list of (size, png bytes)."""
    header = struct.pack("<HHH", 0, 1, len(frames))
    offset = len(header) + 16 * len(frames)
    entries, data = b"", b""
    for size, png in frames:
        side = 0 if size >= 256 else size
        entries += struct.pack(
            "<BBBBHHII", side, side, 0, 0, 1, 32, len(png), offset + len(data)
        )
        data += png
    return header + entries + data


# ---- The tray icon ----

TRAY_SIZES = (16, 20, 24, 32, 40, 48)
SUBSAMPLES = 8
ORANGE = (0xF5, 0x79, 0x00)
INKS = {"light": (0x1F, 0x1F, 0x1F), "dark": (0xFF, 0xFF, 0xFF)}
# state: (signal arcs, orange dot)
STATES = {
    "idle": (False, False),
    "active": (True, False),
    "alert": (False, True),
    "active-alert": (True, True),
}


# Squares are products, never `** 2`: CPython's float power goes through the platform's pow(),
# which is not required to round correctly; * is IEEE-exact everywhere.


def in_round_rect(x, y, x0, y0, x1, y1, r):
    if x < x0 or x > x1 or y < y0 or y > y1:
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    dx, dy = x - cx, y - cy
    return dx * dx + dy * dy <= r * r


def in_rect(x, y, x0, y0, x1, y1):
    return x0 <= x <= x1 and y0 <= y <= y1


def in_arc(x, y, inner, outer):
    """The ring sector between radii `inner` and `outer` around (9, 8), 45 degrees either side of
    pointing right: the SVG's arcs end on the diagonals through that centre."""
    dx, dy = x - 9, y - 8
    if dx <= 0 or abs(dy) > dx:
        return False
    d2 = dx * dx + dy * dy
    return inner * inner <= d2 <= outer * outer


def in_phone(x, y, arcs):
    # The frame: the outer rounded rectangle minus the inner one (fill-rule evenodd).
    if in_round_rect(x, y, 3, 1, 10, 15, 2) and not in_round_rect(x, y, 4, 2, 9, 14, 1):
        return True
    if in_rect(x, y, 5.5, 3.5, 7.5, 4.5) or in_rect(x, y, 5.5, 11.5, 7.5, 12.5):
        return True
    return arcs and (in_arc(x, y, 2.5, 3.5) or in_arc(x, y, 4.5, 5.5))


def in_dot(x, y):
    dx, dy = x - 12.5, y - 3.5
    return dx * dx + dy * dy <= 9


def tray_frame(size, ink, arcs, dot):
    """RGBA pixels of one tray frame. The dot is painted over the phone, as in the SVG."""
    n = SUBSAMPLES
    total = n * n
    # Sample i of pixel p sits at (p + (2i + 1) / 2n) * 16 / size in view-box units: one exact
    # division of two integers, so the result is the same on every machine.
    coords = [
        [(2 * n * p + 2 * i + 1) * 16 / (2 * n * size) for i in range(n)]
        for p in range(size)
    ]
    out = bytearray()
    for py in range(size):
        for px in range(size):
            orange = black = 0
            for y in coords[py]:
                for x in coords[px]:
                    if dot and in_dot(x, y):
                        orange += 1
                    elif in_phone(x, y, arcs):
                        black += 1
            covered = orange + black
            if covered == 0:
                out += b"\x00\x00\x00\x00"
                continue
            color = [
                (orange * o + black * k + covered // 2) // covered
                for o, k in zip(ORANGE, ink)
            ]
            alpha = (255 * covered + total // 2) // total
            out += bytes(color + [alpha])
    return bytes(out)


def tray_icon(state, theme):
    arcs, dot = STATES[state]
    ink = INKS[theme]
    return encode_ico(
        [(s, encode_png(s, s, tray_frame(s, ink, arcs, dot))) for s in TRAY_SIZES]
    )


# ---- The app icon ----

APP_SIZES_KEPT = (256, 128, 64)
APP_SIZES_SCALED = (48, 32, 24, 16)


def _weights(source, target):
    """For each target index, (source index, weight) pairs: the overlap of the source pixel with
    the target pixel's span, in units of 1/target source pixels; each list sums to `source`."""
    table = []
    for t in range(target):
        lo, hi = t * source, (t + 1) * source
        row = []
        for s in range(lo // target, min(source, -(-hi // target))):
            w = min(hi, (s + 1) * target) - max(lo, s * target)
            if w > 0:
                row.append((s, w))
        table.append(row)
    return table


def scale_down(width, pixels, target):
    """Area-weighted average of a square RGBA image, in premultiplied alpha, integers only."""
    table = _weights(width, target)
    whole = width * width
    out = bytearray()
    for ty in range(target):
        for tx in range(target):
            a_sum = r_sum = g_sum = b_sum = 0
            for sy, wy in table[ty]:
                base = sy * width
                for sx, wx in table[tx]:
                    i = (base + sx) * 4
                    w = wy * wx * pixels[i + 3]
                    a_sum += w
                    r_sum += w * pixels[i]
                    g_sum += w * pixels[i + 1]
                    b_sum += w * pixels[i + 2]
            if a_sum == 0:
                out += b"\x00\x00\x00\x00"
                continue
            half = a_sum // 2
            out += bytes(
                [
                    (r_sum + half) // a_sum,
                    (g_sum + half) // a_sum,
                    (b_sum + half) // a_sum,
                    (a_sum + whole // 2) // whole,
                ]
            )
    return bytes(out)


def app_icon():
    pngs = {s: (ROOT / f"packaging/icons/{s}.png").read_bytes() for s in APP_SIZES_KEPT}
    width, height, pixels = decode_png(pngs[256])
    assert (width, height) == (256, 256), "256.png must be 256 x 256"
    frames = [(s, pngs[s]) for s in APP_SIZES_KEPT]
    for s in APP_SIZES_SCALED:
        frames.append((s, encode_png(s, s, scale_down(width, pixels, s))))
    return encode_ico(frames)


# ---- Main ----


def outputs():
    files = {ASSETS / "AppIcon.ico": app_icon()}
    for state in STATES:
        for theme in INKS:
            files[ASSETS / "Tray" / f"cellglide-{state}-{theme}.ico"] = tray_icon(
                state, theme
            )
    return files


def main(argv):
    check = argv[1:] == ["--check"]
    if argv[1:] and not check:
        sys.exit("usage: gen-windows-icons.py [--check]")
    files = outputs()
    if check:
        stale = [
            p for p, data in files.items() if not p.exists() or p.read_bytes() != data
        ]
        for path in stale:
            print(f"differs: {path.relative_to(ROOT).as_posix()}")
        if stale:
            print("run: python -I scripts/gen-windows-icons.py")
            return 1
        print(f"ok: {len(files)} icons match")
        return 0
    for path, data in files.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        print(f"wrote {path.relative_to(ROOT).as_posix()} ({len(data)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
