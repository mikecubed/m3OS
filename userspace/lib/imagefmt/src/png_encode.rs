//! PNG encoder (Phase 105 Track C.3) — the first image encoder in the
//! tree, needed by the `screenshot` tool.
//!
//! Emits a minimal but valid PNG: 8-bit RGBA, filter type 0 (None) on
//! every scanline, and a single IDAT carrying a **stored (uncompressed)**
//! zlib/DEFLATE stream. Stored blocks keep the encoder tiny and
//! allocation-predictable; the bytes still parse through any conformant
//! PNG decoder (including this crate's [`crate::decode_png`], which the
//! round-trip test exercises). Per-chunk CRC-32 and the zlib Adler-32
//! trailer are computed exactly.
//!
//! Input pixels are BGRA8888 `u32` (compositor-native, matching the
//! decoders); the encoder swaps to RGBA byte order in the IDAT.

use alloc::vec::Vec;

/// The 8-byte PNG signature.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// A DEFLATE stored block carries at most 65535 bytes of payload.
const MAX_STORED_BLOCK: usize = 0xFFFF;

/// Encode `pixels` (BGRA8888, `width * height`, row-major top-to-bottom)
/// as an 8-bit RGBA PNG. Returns the complete file bytes. A zero
/// dimension or a `pixels` length that does not match `width * height`
/// yields an empty `Vec` (nothing sensible to encode).
pub fn encode_png(width: u32, height: u32, pixels: &[u32]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || pixels.len() != w * h {
        return Vec::new();
    }

    // -- Raw image data: one filter byte (0) per row, then RGBA bytes. --
    let raw_len = h * (1 + w * 4);
    let mut raw = Vec::with_capacity(raw_len);
    for row in 0..h {
        raw.push(0u8); // filter: None
        for col in 0..w {
            let px = pixels[row * w + col];
            // BGRA8888 u32 (0xAARRGGBB) → RGBA bytes.
            raw.push((px >> 16) as u8); // R
            raw.push((px >> 8) as u8); // G
            raw.push(px as u8); // B
            raw.push((px >> 24) as u8); // A
        }
    }

    // -- zlib-wrapped stored DEFLATE stream over `raw`. --
    let idat = zlib_stored(&raw);

    // -- Assemble the file. --
    let mut out = Vec::with_capacity(PNG_SIGNATURE.len() + 12 + 13 + 12 + idat.len() + 12);
    out.extend_from_slice(&PNG_SIGNATURE);

    // IHDR: width, height, bit depth 8, color type 6 (RGBA), the rest 0.
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bit depth
    ihdr[9] = 6; // color type: RGBA
    // ihdr[10]=compression 0, [11]=filter 0, [12]=interlace 0
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    out
}

/// Write a PNG chunk: `[len u32 BE][type][data][crc u32 BE]`; the CRC
/// covers the type + data.
fn write_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(chunk_type);
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

/// Wrap `data` in a zlib stream whose DEFLATE payload is a sequence of
/// stored (BTYPE=00) blocks, followed by the Adler-32 of `data`.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / MAX_STORED_BLOCK * 5 + 16);
    // zlib header: CMF=0x78 (deflate, 32 KiB window), FLG=0x01 → the
    // 2-byte value 0x7801 is a multiple of 31 (FCHECK satisfied).
    out.push(0x78);
    out.push(0x01);

    // Stored DEFLATE blocks: [BFINAL(1) | BTYPE(00)] byte, then LEN and
    // ~LEN (16-bit LE), then LEN literal bytes.
    let mut offset = 0usize;
    if data.is_empty() {
        // A single empty final stored block.
        out.push(0x01);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    } else {
        while offset < data.len() {
            let chunk = (data.len() - offset).min(MAX_STORED_BLOCK);
            let is_final = offset + chunk >= data.len();
            out.push(if is_final { 0x01 } else { 0x00 });
            let len = chunk as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(&data[offset..offset + chunk]);
            offset += chunk;
        }
    }

    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Adler-32 checksum (zlib trailer).
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// CRC-32 (IEEE 802.3, the PNG chunk CRC), computed on the fly without a
/// static table so it stays allocation- and const-init-free.
struct Crc32 {
    state: u32,
}

impl Crc32 {
    fn new() -> Crc32 {
        Crc32 { state: 0xFFFF_FFFF }
    }

    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.state ^= byte as u32;
            for _ in 0..8 {
                let mask = (self.state & 1).wrapping_neg();
                self.state = (self.state >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }

    fn finalize(self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_png;
    use alloc::vec;

    #[test]
    fn encode_decode_round_trip() {
        // A small non-uniform image.
        let w = 5u32;
        let h = 3u32;
        let mut px = vec![0u32; (w * h) as usize];
        for (i, p) in px.iter_mut().enumerate() {
            let v = (i as u32).wrapping_mul(0x0103_070f);
            *p = 0xFF00_0000 | (v & 0x00FF_FFFF);
        }
        let png = encode_png(w, h, &px);
        assert!(!png.is_empty());
        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );

        let (dw, dh, dpx) = decode_png(&png).expect("our PNG must decode");
        assert_eq!((dw, dh), (w, h));
        assert_eq!(dpx, px, "round-trip preserves every pixel");
    }

    #[test]
    fn large_image_uses_multiple_stored_blocks() {
        // > 65535 raw bytes forces a second stored block; still round-trips.
        let w = 200u32;
        let h = 100u32; // raw = 100 * (1 + 200*4) = 80100 bytes > 65535
        let px: Vec<u32> = (0..(w * h)).map(|i| 0xFF00_0000 | i).collect();
        let png = encode_png(w, h, &px);
        let (dw, dh, dpx) = decode_png(&png).expect("multi-block PNG decodes");
        assert_eq!((dw, dh), (w, h));
        assert_eq!(dpx, px);
    }

    #[test]
    fn rejects_mismatched_dimensions() {
        assert!(encode_png(2, 2, &[0; 3]).is_empty());
        assert!(encode_png(0, 5, &[]).is_empty());
    }

    #[test]
    fn adler32_known_answer() {
        // Adler-32("Wikipedia") = 0x11E60398 (the canonical KAT).
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
