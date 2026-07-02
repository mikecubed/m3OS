#!/usr/bin/env python3
"""Regenerate sample.wav — the symphonia-smoke WAV fixture.

48 kHz stereo S16LE, 1.0 s of a 440 Hz sine at ~37% full scale: matches
audio_server's only accepted rate/format, and is loud enough that the
gate's assert_wav_non_silent (|sample| > 100 in >=5% of the loudest 1 s
window) passes with a wide margin after the AC'97 capture round-trip.

Deterministic (no timestamps, fixed math) so the committed fixture is
reproducible: python3 mkwav.py > sample.wav
"""
import math
import struct
import sys

RATE = 48_000
SECONDS = 1.0
FREQ = 440.0
AMP = 12_000  # ~37% of i16 full scale

frames = int(RATE * SECONDS)
pcm = bytearray()
for i in range(frames):
    s = int(AMP * math.sin(2.0 * math.pi * FREQ * i / RATE))
    pcm += struct.pack("<hh", s, s)

data_len = len(pcm)
hdr = b"RIFF" + struct.pack("<I", 36 + data_len) + b"WAVE"
hdr += b"fmt " + struct.pack("<IHHIIHH", 16, 1, 2, RATE, RATE * 4, 4, 16)
hdr += b"data" + struct.pack("<I", data_len)

sys.stdout.buffer.write(hdr + pcm)
