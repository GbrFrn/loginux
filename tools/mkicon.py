#!/usr/bin/env python3
"""Draw the Loginux mark from icons/loginux.txt into an SVG and PNG sizes.

    python3 tools/mkicon.py

Writes icons/loginux.svg (one square per run of same-coloured pixels, drawn
with crisp edges) and icons/loginux-N.png for whole-number scales of the 16 x 16
grid, scaled without smoothing so every pixel stays square. Standard library
only.
"""
import struct
import zlib
from pathlib import Path

ICONS = Path(__file__).resolve().parent.parent / "icons"
SIZES = [16, 32, 48, 64, 128, 256]


def read(path):
    palette, rows = {}, []
    for line in path.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) >= 2 and len(parts[0]) == 1 and parts[1].startswith("#"):
            palette[parts[0]] = parts[1]
        else:
            rows.append(line.strip())
    side = len(rows)
    assert all(len(r) == side for r in rows), "the grid must be square"
    assert set("".join(rows)) <= set(palette) | {"."}, "a pixel uses a colour the palette lacks"
    return palette, rows


def svg(palette, rows):
    side = len(rows)
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side} {side}" shape-rendering="crispEdges">']
    for y, row in enumerate(rows):
        x = 0
        while x < side:
            c = row[x]
            run = 1
            while x + run < side and row[x + run] == c:
                run += 1
            if c != ".":
                out.append(f'<rect x="{x}" y="{y}" width="{run}" height="1" fill="{palette[c]}"/>')
            x += run
    out.append("</svg>")
    return "\n".join(out) + "\n"


def png(palette, rows, scale):
    side = len(rows) * scale
    rgba = {k: bytes.fromhex(v[1:]) + b"\xff" for k, v in palette.items()}
    rgba["."] = b"\x00\x00\x00\x00"
    lines = []
    for row in rows:
        line = b"\x00" + b"".join(rgba[c] * scale for c in row)
        lines += [line] * scale

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", side, side, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(b"".join(lines), 9)) + chunk(b"IEND", b"")


def main():
    palette, rows = read(ICONS / "loginux.txt")
    (ICONS / "loginux.svg").write_text(svg(palette, rows))
    for size in SIZES:
        assert size % len(rows) == 0, f"{size} is not a whole-number scale of the grid"
        (ICONS / f"loginux-{size}.png").write_bytes(png(palette, rows, size // len(rows)))


if __name__ == "__main__":
    main()
