#!/usr/bin/env python3
"""Emit a tiny 32x24 24-bpp (BI_RGB) BMP gradient used as the `imgview`
BMP fixture for `imgview-render-probe`. No external image library needed;
this is a minimal standard-conformant BMP writer. Regenerate with:

    python3 xtask/assets/imgview/mkbmp.py   # writes sample.bmp beside it
"""
import os
import struct

W, H = 32, 24
row_stride = (W * 3 + 3) & ~3          # rows padded to a multiple of 4 bytes
pixel_bytes = row_stride * H
file_size = 14 + 40 + pixel_bytes

out = bytearray()
# --- BITMAPFILEHEADER (14 bytes) ---
out += b"BM"
out += struct.pack("<IHHI", file_size, 0, 0, 14 + 40)
# --- BITMAPINFOHEADER (40 bytes) ---
out += struct.pack("<IiiHHIIiiII", 40, W, H, 1, 24, 0, pixel_bytes, 2835, 2835, 0, 0)
# --- pixel data: bottom-up rows, BGR, a diagonal gradient so it is non-uniform ---
for y in range(H):            # BMP scanlines run bottom-to-top
    row = bytearray()
    for x in range(W):
        r = (x * 255) // (W - 1)
        g = (y * 255) // (H - 1)
        b = ((x + y) * 255) // (W + H - 2)
        row += bytes((b, g, r))    # BMP stores BGR
    row += b"\x00" * (row_stride - len(row))
    out += row

assert len(out) == file_size, (len(out), file_size)
dst = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sample.bmp")
with open(dst, "wb") as f:
    f.write(out)
print(f"wrote {dst} ({file_size} bytes, {W}x{H} 24bpp)")
