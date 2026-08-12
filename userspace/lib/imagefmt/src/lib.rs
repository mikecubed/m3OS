//! `imagefmt` — image decoders + a scale-to-fit blitter + a PNG encoder
//! (Phase 105 Track C; the decoders originate in Phase 71 Track B's
//! greeter).
//!
//! All decoders produce a `Vec<u32>` of BGRA8888 pixels at the decoded
//! dimensions so a compositor client can paint into a surface buffer
//! without a per-pixel format conversion. Extracted from the greeter into
//! a shared crate so `greeter`, `imgview`, and `screenshot` reuse one
//! implementation instead of duplicating codecs per app.
//!
//! - [`decode_bmp`] / [`decode_png`] — the greeter's original decoders.
//! - [`jpeg::decode_jpeg`] — a `no_std` baseline JPEG decoder (Track C.2).
//! - [`png_encode::encode_png`] — the first encoder in the tree (Track C.3).
//! - [`blit_scale_to_fit`] — aspect-preserving blit into a surface.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod jpeg;
pub mod png_encode;

pub use jpeg::decode_jpeg;
pub use png_encode::encode_png;

use alloc::vec;
use alloc::vec::Vec;

/// Errors returned by [`decode_bmp`] / [`decode_png`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// File is shorter than the minimum header size or claims pixel
    /// data that extends past the end of the buffer.
    Truncated,
    /// Magic bytes or signature did not match.
    BadSignature,
    /// File header parsed but a recognised field carries an
    /// unsupported value (e.g. BMP compression != 0, PNG color type
    /// the decoder does not implement).
    Unsupported,
    /// Decoded dimensions would overflow `usize` or produce a buffer
    /// larger than [`MAX_IMAGE_PIXELS`].
    GeometryOverflow,
    /// Internal decoder failure (CRC mismatch, malformed inflate, etc.).
    Corrupt,
}

/// Hard cap on decoded pixel count. Sized to comfortably hold a
/// 2048×2048 background image (4 Mpix → 16 MiB BGRA) but bound the
/// worst-case allocation against a deliberately malicious header.
pub const MAX_IMAGE_PIXELS: usize = 2048 * 2048;

// =========================================================================
// BMP decoder
// =========================================================================

/// Decode a Windows BITMAPINFOHEADER BMP into BGRA8888 pixels.
///
/// Supports 24-bit RGB and 32-bit BGRA (the two formats produced by
/// virtually every modern paint program saving as "Windows BMP").
/// Returns `(width, height, pixels)` where `pixels` is row-major,
/// top-to-bottom, BGRA8888 (compositor-native).
pub fn decode_bmp(data: &[u8]) -> Result<(u32, u32, Vec<u32>), ImageError> {
    // FILEHEADER (14 bytes) + minimum INFOHEADER (40 bytes) = 54 bytes
    if data.len() < 54 {
        return Err(ImageError::Truncated);
    }
    if &data[0..2] != b"BM" {
        return Err(ImageError::BadSignature);
    }
    let pixel_offset = u32::from_le_bytes([data[10], data[11], data[12], data[13]]) as usize;
    let header_size = u32::from_le_bytes([data[14], data[15], data[16], data[17]]);
    if header_size < 40 {
        return Err(ImageError::Unsupported);
    }
    let width = i32::from_le_bytes([data[18], data[19], data[20], data[21]]);
    let height_raw = i32::from_le_bytes([data[22], data[23], data[24], data[25]]);
    let bpp = u16::from_le_bytes([data[28], data[29]]);
    let compression = u32::from_le_bytes([data[30], data[31], data[32], data[33]]);
    if compression != 0 {
        return Err(ImageError::Unsupported);
    }
    if !(bpp == 24 || bpp == 32) {
        return Err(ImageError::Unsupported);
    }
    if width <= 0 || height_raw == 0 {
        return Err(ImageError::Unsupported);
    }
    let width_u = width as u32;
    let bottom_up = height_raw > 0;
    let height_u = height_raw.unsigned_abs();

    let total_pixels = (width_u as usize)
        .checked_mul(height_u as usize)
        .ok_or(ImageError::GeometryOverflow)?;
    if total_pixels > MAX_IMAGE_PIXELS {
        return Err(ImageError::GeometryOverflow);
    }

    let bytes_per_pixel = (bpp / 8) as usize;
    let row_bytes_unpadded = (width_u as usize)
        .checked_mul(bytes_per_pixel)
        .ok_or(ImageError::GeometryOverflow)?;
    // BMP rows are padded to a 4-byte boundary.
    let row_stride = row_bytes_unpadded.div_ceil(4) * 4;
    let total_bytes = row_stride
        .checked_mul(height_u as usize)
        .ok_or(ImageError::GeometryOverflow)?;
    if pixel_offset.saturating_add(total_bytes) > data.len() {
        return Err(ImageError::Truncated);
    }

    let mut pixels = vec![0u32; total_pixels];
    for row_in in 0..height_u as usize {
        // BMP stores bottom-up by default (positive height); negative
        // height flips to top-down. Either way we want to write
        // top-to-bottom into the output.
        let dst_row = if bottom_up {
            (height_u as usize) - 1 - row_in
        } else {
            row_in
        };
        let row_start = pixel_offset + row_in * row_stride;
        let row_end = row_start + row_bytes_unpadded;
        let row = &data[row_start..row_end];
        let dst_offset = dst_row * (width_u as usize);
        for (col, chunk) in row.chunks_exact(bytes_per_pixel).enumerate() {
            // BMP pixel order is B, G, R[, A] — already compositor-native.
            let b = chunk[0] as u32;
            let g = chunk[1] as u32;
            let r = chunk[2] as u32;
            let a = if bytes_per_pixel == 4 {
                chunk[3] as u32
            } else {
                0xFF
            };
            pixels[dst_offset + col] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
    Ok((width_u, height_u, pixels))
}

// =========================================================================
// PNG decoder
// =========================================================================

/// Decode a baseline PNG (RGB8 or RGBA8, deflate-compressed) into
/// BGRA8888 pixels.
///
/// Supports color type 2 (RGB) and 6 (RGBA) at bit depth 8 — the
/// formats produced by virtually every modern paint program saving as
/// "PNG". Filters 0..=4 are implemented. Interlacing is not supported
/// (returns [`ImageError::Unsupported`]).
pub fn decode_png(data: &[u8]) -> Result<(u32, u32, Vec<u32>), ImageError> {
    // PNG signature: 8 bytes.
    const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 8 || &data[..8] != SIG {
        return Err(ImageError::BadSignature);
    }
    let mut pos = 8usize;
    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut color_type: u8 = 0;
    let mut idat: Vec<u8> = Vec::new();
    let mut seen_ihdr = false;
    let mut seen_iend = false;

    while pos + 8 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let chunk_type = &data[pos + 4..pos + 8];
        let body_start = pos + 8;
        let body_end = body_start.checked_add(len).ok_or(ImageError::Truncated)?;
        let crc_end = body_end.checked_add(4).ok_or(ImageError::Truncated)?;
        if crc_end > data.len() {
            return Err(ImageError::Truncated);
        }
        match chunk_type {
            b"IHDR" => {
                if len != 13 {
                    return Err(ImageError::Corrupt);
                }
                let body = &data[body_start..body_end];
                width = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                height = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                let bit_depth = body[8];
                color_type = body[9];
                let interlace = body[12];
                if interlace != 0 {
                    return Err(ImageError::Unsupported);
                }
                if bit_depth != 8 {
                    return Err(ImageError::Unsupported);
                }
                if !(color_type == 2 || color_type == 6) {
                    return Err(ImageError::Unsupported);
                }
                seen_ihdr = true;
            }
            b"IDAT" => {
                if !seen_ihdr {
                    return Err(ImageError::Corrupt);
                }
                idat.extend_from_slice(&data[body_start..body_end]);
            }
            b"IEND" => {
                seen_iend = true;
                break;
            }
            _ => {} // Skip unrecognised ancillary chunks.
        }
        pos = crc_end;
    }
    if !seen_ihdr || !seen_iend {
        return Err(ImageError::Corrupt);
    }
    if width == 0 || height == 0 {
        return Err(ImageError::Unsupported);
    }

    let total_pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(ImageError::GeometryOverflow)?;
    if total_pixels > MAX_IMAGE_PIXELS {
        return Err(ImageError::GeometryOverflow);
    }

    let bytes_per_pixel = match color_type {
        2 => 3,
        6 => 4,
        _ => unreachable!(),
    };
    let row_bytes = (width as usize) * bytes_per_pixel;
    // PNG filtered scanline: 1 filter byte + row_bytes pixel bytes.
    let expected = (1 + row_bytes) * (height as usize);

    // Decompress the zlib stream (deflate + 2-byte header + 4-byte adler32).
    let raw = inflate_zlib(&idat, expected)?;
    if raw.len() != expected {
        return Err(ImageError::Corrupt);
    }
    let pixels = unfilter_png(&raw, width, height, bytes_per_pixel, color_type)?;
    Ok((width, height, pixels))
}

/// Inflate a zlib stream (RFC 1950 / RFC 1951). Expects a 2-byte
/// header, deflate body, and a 4-byte Adler-32 trailer.
///
/// `expected_len` is the decoder hint for the final output size; we
/// pre-allocate to that capacity so the deflate hot path doesn't
/// reallocate per block. The actual output length is validated by the
/// caller against the PNG row geometry.
fn inflate_zlib(data: &[u8], expected_len: usize) -> Result<Vec<u8>, ImageError> {
    if data.len() < 2 + 4 {
        return Err(ImageError::Corrupt);
    }
    let cmf = data[0];
    let flg = data[1];
    // CM == 8 (deflate); CINFO <= 7 (32 KiB window); FCHECK valid.
    if (cmf & 0x0F) != 8 {
        return Err(ImageError::Unsupported);
    }
    if !(cmf as u16 * 256 + flg as u16).is_multiple_of(31) {
        return Err(ImageError::Corrupt);
    }
    if (flg & 0x20) != 0 {
        // FDICT not supported.
        return Err(ImageError::Unsupported);
    }
    let body = &data[2..data.len() - 4];
    let mut out = Vec::with_capacity(expected_len);
    inflate_deflate(body, &mut out)?;
    Ok(out)
}

/// Inflate a raw deflate (RFC 1951) stream into `out`.
fn inflate_deflate(input: &[u8], out: &mut Vec<u8>) -> Result<(), ImageError> {
    let mut br = BitReader::new(input);
    loop {
        let bfinal = br.read_bits(1)? == 1;
        let btype = br.read_bits(2)?;
        match btype {
            0 => inflate_stored(&mut br, out)?,
            1 => inflate_fixed(&mut br, out)?,
            2 => inflate_dynamic(&mut br, out)?,
            _ => return Err(ImageError::Corrupt),
        }
        if bfinal {
            break;
        }
    }
    Ok(())
}

struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }
    fn read_bits(&mut self, n: u8) -> Result<u32, ImageError> {
        let mut result: u32 = 0;
        for i in 0..n {
            if self.byte_pos >= self.data.len() {
                return Err(ImageError::Corrupt);
            }
            let bit = ((self.data[self.byte_pos] >> self.bit_pos) & 1) as u32;
            result |= bit << i;
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }
        Ok(result)
    }
    fn align_byte(&mut self) {
        if self.bit_pos != 0 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
    }
    fn read_u16_le(&mut self) -> Result<u16, ImageError> {
        if self.byte_pos + 2 > self.data.len() {
            return Err(ImageError::Corrupt);
        }
        let v = u16::from_le_bytes([self.data[self.byte_pos], self.data[self.byte_pos + 1]]);
        self.byte_pos += 2;
        Ok(v)
    }
    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], ImageError> {
        if self.byte_pos + n > self.data.len() {
            return Err(ImageError::Corrupt);
        }
        let s = &self.data[self.byte_pos..self.byte_pos + n];
        self.byte_pos += n;
        Ok(s)
    }
}

fn inflate_stored(br: &mut BitReader<'_>, out: &mut Vec<u8>) -> Result<(), ImageError> {
    br.align_byte();
    let len = br.read_u16_le()?;
    let nlen = br.read_u16_le()?;
    if len != !nlen {
        return Err(ImageError::Corrupt);
    }
    let bytes = br.read_bytes(len as usize)?;
    out.extend_from_slice(bytes);
    Ok(())
}

// Static Huffman tables for fixed-block deflate.
fn build_fixed_lit_lengths() -> [u8; 288] {
    let mut lens = [0u8; 288];
    for (i, slot) in lens.iter_mut().enumerate() {
        *slot = if i < 144 {
            8
        } else if i < 256 {
            9
        } else if i < 280 {
            7
        } else {
            8
        };
    }
    lens
}

fn inflate_fixed(br: &mut BitReader<'_>, out: &mut Vec<u8>) -> Result<(), ImageError> {
    let lit_lens = build_fixed_lit_lengths();
    let lit_tree = HuffmanTable::from_lengths(&lit_lens)?;
    let dist_lens = [5u8; 30];
    let dist_tree = HuffmanTable::from_lengths(&dist_lens)?;
    decode_block(br, out, &lit_tree, &dist_tree)
}

fn inflate_dynamic(br: &mut BitReader<'_>, out: &mut Vec<u8>) -> Result<(), ImageError> {
    let hlit = br.read_bits(5)? as usize + 257;
    let hdist = br.read_bits(5)? as usize + 1;
    let hclen = br.read_bits(4)? as usize + 4;
    let code_len_order = [
        16u8, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut code_len_lens = [0u8; 19];
    for i in 0..hclen {
        code_len_lens[code_len_order[i] as usize] = br.read_bits(3)? as u8;
    }
    let code_len_tree = HuffmanTable::from_lengths(&code_len_lens)?;

    // Decode lit/dist lengths together.
    let total_lens = hlit + hdist;
    let mut all_lens = vec![0u8; total_lens];
    let mut i = 0;
    while i < total_lens {
        let sym = code_len_tree.decode_symbol(br)?;
        match sym {
            0..=15 => {
                all_lens[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err(ImageError::Corrupt);
                }
                let repeat = br.read_bits(2)? as usize + 3;
                let prev = all_lens[i - 1];
                for _ in 0..repeat {
                    if i >= total_lens {
                        return Err(ImageError::Corrupt);
                    }
                    all_lens[i] = prev;
                    i += 1;
                }
            }
            17 => {
                let repeat = br.read_bits(3)? as usize + 3;
                for _ in 0..repeat {
                    if i >= total_lens {
                        return Err(ImageError::Corrupt);
                    }
                    all_lens[i] = 0;
                    i += 1;
                }
            }
            18 => {
                let repeat = br.read_bits(7)? as usize + 11;
                for _ in 0..repeat {
                    if i >= total_lens {
                        return Err(ImageError::Corrupt);
                    }
                    all_lens[i] = 0;
                    i += 1;
                }
            }
            _ => return Err(ImageError::Corrupt),
        }
    }
    let lit_tree = HuffmanTable::from_lengths(&all_lens[..hlit])?;
    let dist_tree = HuffmanTable::from_lengths(&all_lens[hlit..])?;
    decode_block(br, out, &lit_tree, &dist_tree)
}

const LENGTH_BASES: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASES: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn decode_block(
    br: &mut BitReader<'_>,
    out: &mut Vec<u8>,
    lit_tree: &HuffmanTable,
    dist_tree: &HuffmanTable,
) -> Result<(), ImageError> {
    loop {
        let sym = lit_tree.decode_symbol(br)?;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let li = (sym - 257) as usize;
            if li >= 29 {
                return Err(ImageError::Corrupt);
            }
            let len = LENGTH_BASES[li] as usize + br.read_bits(LENGTH_EXTRA[li])? as usize;
            let dist_sym = dist_tree.decode_symbol(br)? as usize;
            if dist_sym >= 30 {
                return Err(ImageError::Corrupt);
            }
            let dist = DIST_BASES[dist_sym] as usize + br.read_bits(DIST_EXTRA[dist_sym])? as usize;
            if dist == 0 || dist > out.len() {
                return Err(ImageError::Corrupt);
            }
            let start = out.len() - dist;
            for i in 0..len {
                let b = out[start + i];
                out.push(b);
            }
        }
    }
}

/// Canonical Huffman code table.
struct HuffmanTable {
    /// Map from `(length << 16) | code` keys to symbols. Built lazily
    /// by walking lengths in canonical order. Implementation uses
    /// length-first lookup to keep code small.
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl HuffmanTable {
    fn from_lengths(lengths: &[u8]) -> Result<Self, ImageError> {
        let mut counts = [0u16; 16];
        for &len in lengths {
            if len > 15 {
                return Err(ImageError::Corrupt);
            }
            counts[len as usize] += 1;
        }
        counts[0] = 0;
        let total: u32 = counts.iter().skip(1).map(|&c| c as u32).sum();
        let mut symbols = vec![0u16; total as usize];
        let mut offsets = [0u16; 16];
        let mut running = 0u16;
        for i in 1..16 {
            offsets[i] = running;
            running += counts[i];
        }
        for (sym, &len) in lengths.iter().enumerate() {
            if len > 0 {
                let off = offsets[len as usize] as usize;
                symbols[off] = sym as u16;
                offsets[len as usize] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode_symbol(&self, br: &mut BitReader<'_>) -> Result<u32, ImageError> {
        let mut code: u32 = 0;
        let mut first: u32 = 0;
        let mut index: u32 = 0;
        for len in 1..=15u32 {
            let bit = br.read_bits(1)?;
            code = (code << 1) | bit;
            let count = self.counts[len as usize] as u32;
            if code < first + count {
                let idx = (index + (code - first)) as usize;
                if idx >= self.symbols.len() {
                    return Err(ImageError::Corrupt);
                }
                return Ok(self.symbols[idx] as u32);
            }
            index += count;
            first = (first + count) << 1;
        }
        Err(ImageError::Corrupt)
    }
}

fn unfilter_png(
    raw: &[u8],
    width: u32,
    height: u32,
    bpp_bytes: usize,
    color_type: u8,
) -> Result<Vec<u32>, ImageError> {
    let row_bytes = (width as usize) * bpp_bytes;
    let stride = 1 + row_bytes;
    let total_pixels = (width as usize) * (height as usize);
    let mut pixels = vec![0u32; total_pixels];
    let mut prev_row = vec![0u8; row_bytes];
    let mut cur_row = vec![0u8; row_bytes];

    for y in 0..height as usize {
        let row_start = y * stride;
        let filter = raw[row_start];
        let scanline = &raw[row_start + 1..row_start + 1 + row_bytes];
        match filter {
            0 => cur_row.copy_from_slice(scanline),
            1 => {
                // Sub
                for i in 0..row_bytes {
                    let left = if i >= bpp_bytes {
                        cur_row[i - bpp_bytes]
                    } else {
                        0
                    };
                    cur_row[i] = scanline[i].wrapping_add(left);
                }
            }
            2 => {
                // Up
                for i in 0..row_bytes {
                    cur_row[i] = scanline[i].wrapping_add(prev_row[i]);
                }
            }
            3 => {
                // Average
                for i in 0..row_bytes {
                    let left = if i >= bpp_bytes {
                        cur_row[i - bpp_bytes]
                    } else {
                        0
                    };
                    let up = prev_row[i];
                    let avg = ((left as u16 + up as u16) / 2) as u8;
                    cur_row[i] = scanline[i].wrapping_add(avg);
                }
            }
            4 => {
                // Paeth
                for i in 0..row_bytes {
                    let left = if i >= bpp_bytes {
                        cur_row[i - bpp_bytes]
                    } else {
                        0
                    };
                    let up = prev_row[i];
                    let up_left = if i >= bpp_bytes {
                        prev_row[i - bpp_bytes]
                    } else {
                        0
                    };
                    cur_row[i] = scanline[i].wrapping_add(paeth_predictor(left, up, up_left));
                }
            }
            _ => return Err(ImageError::Corrupt),
        }
        let dst_offset = y * width as usize;
        for (col, chunk) in cur_row.chunks_exact(bpp_bytes).enumerate() {
            let (r, g, b, a) = match color_type {
                2 => (chunk[0], chunk[1], chunk[2], 0xFFu8),
                6 => (chunk[0], chunk[1], chunk[2], chunk[3]),
                _ => unreachable!(),
            };
            pixels[dst_offset + col] =
                ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        }
        core::mem::swap(&mut prev_row, &mut cur_row);
    }
    Ok(pixels)
}

fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let a = a as i32;
    let b = b as i32;
    let c = c as i32;
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

// =========================================================================
// Scale-to-fit blitter
// =========================================================================

/// Blit `src` into `dst` scaled-to-fit with letterbox bars.
///
/// The source is scaled uniformly (preserving aspect ratio) using
/// nearest-neighbor interpolation, then centered in the destination.
/// Uncovered destination regions are filled with `0x0000_0000` (black).
pub fn blit_scale_to_fit(
    src: &[u32],
    src_w: u32,
    src_h: u32,
    dst: &mut [u32],
    dst_w: u32,
    dst_h: u32,
) {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        for px in dst.iter_mut() {
            *px = 0;
        }
        return;
    }
    // Pick the scale factor that fits both dimensions.
    let scale_x = dst_w as u64 * src_h as u64;
    let scale_y = dst_h as u64 * src_w as u64;
    let (out_w, out_h) = if scale_x < scale_y {
        // Width-limited: out_w = dst_w; out_h = src_h * dst_w / src_w.
        let out_h = ((src_h as u64) * (dst_w as u64) / (src_w as u64)) as u32;
        (dst_w, out_h.max(1))
    } else {
        // Height-limited.
        let out_w = ((src_w as u64) * (dst_h as u64) / (src_h as u64)) as u32;
        (out_w.max(1), dst_h)
    };
    let off_x = (dst_w - out_w) / 2;
    let off_y = (dst_h - out_h) / 2;

    // Letterbox fill first.
    for px in dst.iter_mut() {
        *px = 0;
    }
    let dst_stride = dst_w as usize;
    for y in 0..out_h {
        let src_y = ((y as u64) * (src_h as u64) / (out_h as u64)) as usize;
        for x in 0..out_w {
            let src_x = ((x as u64) * (src_w as u64) / (out_w as u64)) as usize;
            let pixel = src[src_y * src_w as usize + src_x];
            let dx = (off_x + x) as usize;
            let dy = (off_y + y) as usize;
            dst[dy * dst_stride + dx] = pixel;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bmp_4x4_24bit() -> Vec<u8> {
        // 4x4 RGB BMP, bottom-up. Row stride = 4*3 = 12 bytes (already 4-aligned).
        let mut data = Vec::new();
        // FILEHEADER: 14 bytes.
        data.extend_from_slice(b"BM");
        let total_size: u32 = 14 + 40 + 12 * 4;
        data.extend_from_slice(&total_size.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // reserved
        data.extend_from_slice(&54u32.to_le_bytes()); // pixel offset
        // INFOHEADER: 40 bytes.
        data.extend_from_slice(&40u32.to_le_bytes()); // header size
        data.extend_from_slice(&4i32.to_le_bytes()); // width
        data.extend_from_slice(&4i32.to_le_bytes()); // height (positive: bottom-up)
        data.extend_from_slice(&1u16.to_le_bytes()); // planes
        data.extend_from_slice(&24u16.to_le_bytes()); // bpp
        data.extend_from_slice(&0u32.to_le_bytes()); // compression
        data.extend_from_slice(&(12u32 * 4).to_le_bytes()); // image size
        data.extend_from_slice(&0i32.to_le_bytes()); // x ppm
        data.extend_from_slice(&0i32.to_le_bytes()); // y ppm
        data.extend_from_slice(&0u32.to_le_bytes()); // colors used
        data.extend_from_slice(&0u32.to_le_bytes()); // important colors
        // Pixel data: 4 rows of 12 bytes (BGR per pixel).
        // Bottom row first (BMP is bottom-up). Color = red at each.
        for _ in 0..4 {
            for _ in 0..4 {
                data.extend_from_slice(&[0x00, 0x00, 0xFF]); // B=0,G=0,R=255 → red
            }
        }
        data
    }

    fn make_bmp_4x4_32bit() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"BM");
        let total_size: u32 = 14 + 40 + 16 * 4;
        data.extend_from_slice(&total_size.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&54u32.to_le_bytes());
        data.extend_from_slice(&40u32.to_le_bytes());
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(&4i32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&32u16.to_le_bytes()); // bpp = 32
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&(16u32 * 4).to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&0i32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..16 {
            // BGRA: green pixel.
            data.extend_from_slice(&[0x00, 0xFF, 0x00, 0xFF]);
        }
        data
    }

    #[test]
    fn decode_bmp_24bit_4x4_red() {
        let data = make_bmp_4x4_24bit();
        let (w, h, px) = decode_bmp(&data).unwrap();
        assert_eq!((w, h), (4, 4));
        assert_eq!(px.len(), 16);
        // All pixels red = 0xFF0000 (alpha = 0xFF padded by decoder).
        for &p in &px {
            assert_eq!(p & 0x00FF_FFFF, 0x00FF_0000);
        }
    }

    #[test]
    fn decode_bmp_32bit_4x4_green() {
        let data = make_bmp_4x4_32bit();
        let (w, h, px) = decode_bmp(&data).unwrap();
        assert_eq!((w, h), (4, 4));
        for &p in &px {
            // Green: A=0xFF, R=0, G=0xFF, B=0
            assert_eq!(p, 0xFF00_FF00);
        }
    }

    #[test]
    fn decode_bmp_truncated_returns_error() {
        let data = vec![0u8; 20];
        assert_eq!(decode_bmp(&data), Err(ImageError::Truncated));
    }

    #[test]
    fn decode_bmp_bad_signature() {
        let mut data = make_bmp_4x4_24bit();
        data[0] = b'X';
        assert_eq!(decode_bmp(&data), Err(ImageError::BadSignature));
    }

    #[test]
    fn blit_scale_to_fit_centers_with_letterbox() {
        // 320x200 source, 1024x768 destination.
        // The image is wider-aspect than the destination, so scale-to-fit
        // is width-limited at 1024x640, with 64 rows of letterbox top/bottom.
        let src_w = 320u32;
        let src_h = 200u32;
        let dst_w = 1024u32;
        let dst_h = 768u32;
        let src = vec![0xFFFF_FFFFu32; (src_w * src_h) as usize];
        let mut dst = vec![0u32; (dst_w * dst_h) as usize];
        blit_scale_to_fit(&src, src_w, src_h, &mut dst, dst_w, dst_h);
        // Scale: 1024/320 = 3.2, 768/200 = 3.84; min is width-limited.
        // out_h = 200 * 1024 / 320 = 640.
        // off_y = (768 - 640) / 2 = 64.
        // Verify top row is letterbox (zero).
        for px in dst.iter().take(dst_w as usize) {
            assert_eq!(*px, 0, "top letterbox row");
        }
        // Verify middle row is fully painted.
        let mid = 64usize + 320usize; // any row inside [64..704)
        for x in 0..dst_w as usize {
            assert_eq!(
                dst[mid * dst_w as usize + x],
                0xFFFF_FFFF,
                "center row should be opaque white"
            );
        }
        // Verify bottom row is letterbox.
        for x in 0..dst_w as usize {
            assert_eq!(dst[((dst_h - 1) as usize) * dst_w as usize + x], 0);
        }
    }

    /// Compute Adler-32 of `data` (RFC 1950 § 9).
    fn adler32(data: &[u8]) -> u32 {
        const MOD_ADLER: u32 = 65521;
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in data {
            a = (a + byte as u32) % MOD_ADLER;
            b = (b + a) % MOD_ADLER;
        }
        (b << 16) | a
    }

    /// Compute CRC-32 (ISO 3309 / Ethernet) of `data`.
    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for n in 0..256u32 {
            let mut c = n;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB88320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            table[n as usize] = c;
        }
        let mut c: u32 = 0xFFFF_FFFF;
        for &b in data {
            c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        c ^ 0xFFFF_FFFF
    }

    /// Encode `raw` as a zlib stream using one stored (uncompressed)
    /// deflate block. Returns the full stream including the 2-byte
    /// header and the 4-byte Adler-32 trailer.
    fn zlib_stored(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        // zlib header: CMF=0x78 (deflate, 32 KiB window). FLG with
        // FDICT=0 and FCHECK chosen so (CMF*256 + FLG) % 31 == 0.
        // 0x78 * 256 = 30720 → 30720 % 31 = 30 → FLG % 31 = 1 → FLG = 0x01.
        out.push(0x78);
        out.push(0x01);
        // One stored block. BFINAL=1, BTYPE=00. With LSB-first bit
        // packing the first byte is 0b0000_0001 = 0x01.
        out.push(0x01);
        let len = raw.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(raw);
        out.extend_from_slice(&adler32(raw).to_be_bytes());
        out
    }

    /// Encode one PNG chunk: 4-byte big-endian length + 4-byte type +
    /// body + 4-byte big-endian CRC over (type ‖ body).
    fn png_chunk(chunk_type: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(chunk_type);
        out.extend_from_slice(body);
        let mut crc_input = Vec::with_capacity(4 + body.len());
        crc_input.extend_from_slice(chunk_type);
        crc_input.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        out
    }

    /// Build a minimal RGBA8 PNG of `width × height` from `pixels`
    /// (row-major, 4 bytes per pixel: R, G, B, A). Uses stored deflate
    /// blocks so the test does not depend on a working Huffman encoder.
    fn make_png_rgba(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        let mut out = Vec::new();
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        // IHDR: width(4) + height(4) + bit_depth(1) + color_type(1) +
        // compression(1) + filter(1) + interlace(1).
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        out.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        // IDAT: one filter byte (0 = None) per scanline + row pixels.
        let row_bytes = (width * 4) as usize;
        let mut raw = Vec::with_capacity((1 + row_bytes) * height as usize);
        for y in 0..height as usize {
            raw.push(0);
            raw.extend_from_slice(&pixels[y * row_bytes..(y + 1) * row_bytes]);
        }
        let idat = zlib_stored(&raw);
        out.extend_from_slice(&png_chunk(b"IDAT", &idat));
        out.extend_from_slice(&png_chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn decode_png_4x4_red() {
        // 4×4 RGBA red, A=0xFF.
        let mut pixels = Vec::with_capacity(64);
        for _ in 0..16 {
            pixels.extend_from_slice(&[0xFF, 0x00, 0x00, 0xFF]);
        }
        let png = make_png_rgba(4, 4, &pixels);
        let (w, h, px) = decode_png(&png).unwrap();
        assert_eq!((w, h), (4, 4));
        assert_eq!(px.len(), 16);
        for &p in &px {
            // BGRA8888: A=0xFF, R=0xFF, G=0, B=0 → 0xFFFF0000.
            assert_eq!(p, 0xFFFF_0000);
        }
    }

    #[test]
    fn decode_png_bad_signature() {
        let pixels = vec![0u8; 16];
        let mut png = make_png_rgba(2, 2, &pixels);
        png[0] = b'X';
        assert_eq!(decode_png(&png), Err(ImageError::BadSignature));
    }
}
