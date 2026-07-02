#!/usr/bin/env python3
"""Regenerate sample.flac — the symphonia-smoke FLAC fixture.

A minimal but standard-conformant FLAC stream (the mkjpeg.py approach:
no host flac encoder needed): STREAMINFO + fixed-blocksize frames whose
subframes are all VERBATIM (uncompressed) — every FLAC decoder must
support them, and the encoder stays ~100 lines. Same signal as
sample.wav (48 kHz stereo S16, 1.0 s, 440 Hz sine) so the audible-gate
expectations match.

Deterministic: python3 mkflac.py > sample.flac
"""
import math
import struct
import sys

RATE = 48_000
SECONDS = 1.0
FREQ = 440.0
AMP = 12_000
BLOCK = 4096

frames_total = int(RATE * SECONDS)


def samples(i):
    return int(AMP * math.sin(2.0 * math.pi * FREQ * i / RATE))


def crc8(data):
    # FLAC frame-header CRC: poly x^8+x^2+x+1 (0x07), init 0.
    crc = 0
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = ((crc << 1) ^ 0x07) & 0xFF if crc & 0x80 else (crc << 1) & 0xFF
    return crc


def crc16(data):
    # FLAC whole-frame CRC: poly x^16+x^15+x^2+1 (0x8005), init 0.
    crc = 0
    for b in data:
        crc ^= b << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x8005) & 0xFFFF if crc & 0x8000 else (crc << 1) & 0xFFFF
    return crc


def utf8_coded(n):
    # FLAC's UTF-8-style frame-number coding; our frame numbers are < 128.
    assert n < 128
    return bytes([n])


out = bytearray()
out += b"fLaC"

# STREAMINFO (type 0), last-metadata-block bit set, length 34.
si = bytearray()
si += struct.pack(">HH", BLOCK, BLOCK)  # min/max blocksize
si += b"\x00\x00\x00" * 2  # min/max framesize: unknown
# 20 bits rate | 3 bits (channels-1) | 5 bits (bps-1) | 36 bits total samples
bits = (RATE << 44) | ((2 - 1) << 41) | ((16 - 1) << 36) | frames_total
si += bits.to_bytes(8, "big")
si += b"\x00" * 16  # MD5 unset (spec-legal "unknown")
out += bytes([0x80]) + struct.pack(">I", 34)[1:] + si

frame_no = 0
pos = 0
while pos < frames_total:
    n = min(BLOCK, frames_total - pos)
    hdr = bytearray()
    hdr += b"\xff\xf8"  # sync(14) + reserved(1)=0 + blocking-strategy(1)=0
    if n == BLOCK:
        bs_code = 0xC  # 256 * 2^(12-8) = 4096
    else:
        bs_code = 0x7  # 16-bit blocksize-1 follows the frame number
    hdr.append((bs_code << 4) | 0xA)  # rate code 0b1010 = 48 kHz
    hdr.append((0x1 << 4) | (0x4 << 1))  # 2ch independent | 16-bit | reserved 0
    hdr += utf8_coded(frame_no)
    if bs_code == 0x7:
        hdr += struct.pack(">H", n - 1)
    hdr.append(crc8(hdr))

    body = bytearray()
    for ch in range(2):
        body.append(0x02)  # subframe: VERBATIM, no wasted bits
        for f in range(n):
            body += struct.pack(">h", samples(pos + f))

    frame = bytes(hdr) + bytes(body)
    out += frame + struct.pack(">H", crc16(frame))
    pos += n
    frame_no += 1

sys.stdout.buffer.write(bytes(out))
