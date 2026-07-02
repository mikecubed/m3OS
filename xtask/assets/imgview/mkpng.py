#!/usr/bin/env python3
"""Emit a tiny 32x24 8-bit RGBA PNG used as the `imgview` PNG fixture for
`imgview-render-probe`. Uses only the stdlib `zlib`/`struct` — no external
image library. Regenerate with:

    python3 xtask/assets/imgview/mkpng.py   # writes sample.png beside it
"""
import os
import struct
import zlib

W, H = 32, 24

def chunk(tag, data):
    return (struct.pack(">I", len(data)) + tag + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff))

raw = bytearray()
for y in range(H):
    raw.append(0)  # filter: None
    for x in range(W):
        r = (x * 255) // (W - 1)
        g = (y * 255) // (H - 1)
        b = ((x + y) * 255) // (W + H - 2)
        raw += bytes((r, g, b, 255))

ihdr = struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0)  # RGBA8
png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", ihdr)
       + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
       + chunk(b"IEND", b""))

dst = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sample.png")
with open(dst, "wb") as f:
    f.write(png)
print(f"wrote {dst} ({len(png)} bytes, {W}x{H} RGBA8)")
