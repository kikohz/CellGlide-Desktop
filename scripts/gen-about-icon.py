#!/usr/bin/env python3
"""Turn packaging/icons/128.png into the raw RGBA the About page shows
(crates/cg-gui/assets/about-icon.rgba, 128 x 128 x 4 bytes).

iced is built with `image-without-codecs`, so the window cannot decode a PNG itself.
Run this again after packaging/icons/128.png changes. Standard library only; reads
8-bit RGBA, non-interlaced PNG, which is what the icon is.

`decode_png` is also used by scripts/gen-windows-icons.py, which loads this file by path
(so importing it writes nothing)."""
import struct
import sys
import zlib
from pathlib import Path


def decode_png(png):
    """(width, height, pixels) of an 8-bit RGBA, non-interlaced PNG; pixels are RGBA rows,
    top to bottom, 4 bytes per pixel."""
    assert png[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, header = 8, b"", None
    while pos < len(png):
        length, kind = struct.unpack(">I4s", png[pos : pos + 8])
        body = png[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    width, height, depth, color, _, _, interlace = header
    if (depth, color, interlace) != (8, 6, 0):
        sys.exit("the PNG must be 8-bit RGBA, not interlaced")

    raw = zlib.decompress(idat)
    stride = width * 4
    rows, prev = [], bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        ftype, line = raw[start], bytearray(raw[start + 1 : start + 1 + stride])
        for i in range(stride):
            a = line[i - 4] if i >= 4 else 0
            b = prev[i]
            c = prev[i - 4] if i >= 4 else 0
            if ftype == 1:
                line[i] = (line[i] + a) & 255
            elif ftype == 2:
                line[i] = (line[i] + b) & 255
            elif ftype == 3:
                line[i] = (line[i] + ((a + b) >> 1)) & 255
            elif ftype == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 255
            elif ftype != 0:
                sys.exit(f"bad PNG filter {ftype}")
        rows.append(bytes(line))
        prev = line
    return width, height, b"".join(rows)


def fnv1a64(data):
    h = 0xCBF29CE484222325
    for byte in data:
        h = ((h ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def main():
    root = Path(__file__).resolve().parent.parent
    png = (root / "packaging/icons/128.png").read_bytes()
    out = root / "crates/cg-gui/assets/about-icon.rgba"
    width, height, pixels = decode_png(png)
    out.write_bytes(pixels)
    # The about-page test checks both, so a changed 128.png or a hand-edited blob is caught.
    out.with_suffix(".rgba.fnv1a64").write_text(
        f"png {fnv1a64(png)}\nrgba {fnv1a64(pixels)}\n", newline="\n"
    )
    print(f"wrote {out.relative_to(root)} ({width}x{height})")


if __name__ == "__main__":
    main()
