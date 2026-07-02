//! Baseline (SOF0) JPEG decoder (Phase 105 Track C.2).
//!
//! A `no_std` + `alloc` decoder for the common case — the format a phone
//! or camera produces and the image viewer must open. Algorithm modeled
//! on the public-domain NanoJPEG / the `jpeg-decoder` crate structure,
//! re-expressed here for `no_std`; no code copied.
//!
//! Supported: 8-bit baseline sequential DCT (SOF0), Huffman entropy
//! coding, 1 component (grayscale) or 3 components (YCbCr) with 1×1/2×1/
//! 1×2/2×2 chroma subsampling, restart intervals (DRI/RSTn). Returns
//! `(width, height, Vec<u32>)` BGRA8888, compositor-native.
//!
//! Unsupported (returns [`crate::ImageError::Unsupported`]): progressive
//! (SOF2), arithmetic coding, 12-bit precision, and 4-component/CMYK.

use alloc::vec;
use alloc::vec::Vec;

use crate::{ImageError, MAX_IMAGE_PIXELS};

// JPEG markers (the byte following 0xFF).
const M_SOI: u8 = 0xD8;
const M_EOI: u8 = 0xD9;
const M_SOF0: u8 = 0xC0;
const M_SOF2: u8 = 0xC2; // progressive — unsupported
const M_DHT: u8 = 0xC4;
const M_DQT: u8 = 0xDB;
const M_DRI: u8 = 0xDD;
const M_SOS: u8 = 0xDA;

/// Zig-zag order: coefficient index → natural 8×8 position.
#[rustfmt::skip]
const ZIGZAG: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// A decoded Huffman table: canonical code → (symbol) lookup built from
/// the DHT counts. `min_code`/`max_code`/`val_ptr` implement the standard
/// per-length decode of Annex F.
#[derive(Clone, Default)]
struct HuffTable {
    // For code lengths 1..=16 (indexed 0..16).
    min_code: [i32; 17],
    max_code: [i32; 17],
    val_ptr: [usize; 17],
    values: Vec<u8>,
}

impl HuffTable {
    fn build(counts: &[u8; 16], values: Vec<u8>) -> HuffTable {
        let mut t = HuffTable {
            values,
            ..Default::default()
        };
        let mut code: i32 = 0;
        let mut k: usize = 0;
        for len in 1..=16 {
            let n = counts[len - 1] as usize;
            if n == 0 {
                t.max_code[len] = -1;
            } else {
                t.val_ptr[len] = k;
                t.min_code[len] = code;
                code += n as i32;
                t.max_code[len] = code - 1;
                k += n;
            }
            code <<= 1;
        }
        t
    }
}

/// Per-component frame parameters.
#[derive(Clone, Copy, Default)]
struct Component {
    id: u8,
    h: u8, // horizontal sampling factor
    v: u8, // vertical sampling factor
    quant: usize,
    // Scan-time Huffman table selectors.
    dc_table: usize,
    ac_table: usize,
    dc_pred: i32,
}

/// A big-endian bit reader over the entropy-coded segment, handling
/// 0xFF00 byte-stuffing and stopping at any marker.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bits: u32,
    nbits: u32,
    /// Set when a marker (other than a stuffed 0x00 or RSTn consumed by
    /// the caller) is hit; `pos` then points at the 0xFF.
    marker: Option<u8>,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], pos: usize) -> BitReader<'a> {
        BitReader {
            data,
            pos,
            bits: 0,
            nbits: 0,
            marker: None,
        }
    }

    /// Pull the next raw byte of entropy data, resolving 0xFF stuffing.
    /// Returns `None` at a real marker (recording it).
    fn next_byte(&mut self) -> Option<u8> {
        if self.pos >= self.data.len() {
            return None;
        }
        let b = self.data[self.pos];
        if b != 0xFF {
            self.pos += 1;
            return Some(b);
        }
        // 0xFF: peek the next byte.
        if self.pos + 1 >= self.data.len() {
            return None;
        }
        let m = self.data[self.pos + 1];
        if m == 0x00 {
            self.pos += 2; // stuffed byte → literal 0xFF
            Some(0xFF)
        } else {
            // A real marker: stop here, leave pos on the 0xFF.
            self.marker = Some(m);
            None
        }
    }

    fn refill(&mut self) {
        while self.nbits <= 24 {
            match self.next_byte() {
                Some(b) => {
                    self.bits |= (b as u32) << (24 - self.nbits);
                    self.nbits += 8;
                }
                None => break,
            }
        }
    }

    /// Read one bit (0 at end of data).
    fn get_bit(&mut self) -> u32 {
        if self.nbits == 0 {
            self.refill();
            if self.nbits == 0 {
                return 0;
            }
        }
        let bit = (self.bits >> 31) & 1;
        self.bits <<= 1;
        self.nbits -= 1;
        bit
    }

    /// Read `n` bits as an unsigned value.
    fn get_bits(&mut self, n: u32) -> i32 {
        let mut v = 0i32;
        for _ in 0..n {
            v = (v << 1) | self.get_bit() as i32;
        }
        v
    }

    /// Decode one Huffman symbol (Annex F.2.2.3).
    fn decode_huff(&mut self, t: &HuffTable) -> Result<u8, ImageError> {
        let mut code: i32 = 0;
        for len in 1..=16 {
            code = (code << 1) | self.get_bit() as i32;
            if t.max_code[len] >= 0 && code <= t.max_code[len] {
                let idx = t.val_ptr[len] + (code - t.min_code[len]) as usize;
                return t.values.get(idx).copied().ok_or(ImageError::Corrupt);
            }
        }
        Err(ImageError::Corrupt)
    }

    /// Align to a byte boundary + reset (after a restart marker).
    fn reset_for_restart(&mut self) {
        self.bits = 0;
        self.nbits = 0;
        self.marker = None;
    }
}

/// Sign-extend a `size`-bit magnitude read (the JPEG "receive & extend").
fn extend(v: i32, size: u32) -> i32 {
    if size == 0 {
        return 0;
    }
    let vt = 1i32 << (size - 1);
    if v < vt { v - (1 << size) + 1 } else { v }
}

fn be16(d: &[u8], p: usize) -> Result<usize, ImageError> {
    let s = d.get(p..p + 2).ok_or(ImageError::Truncated)?;
    Ok(((s[0] as usize) << 8) | s[1] as usize)
}

/// Decode a baseline JPEG into `(width, height, BGRA8888 pixels)`.
pub fn decode_jpeg(data: &[u8]) -> Result<(u32, u32, Vec<u32>), ImageError> {
    if data.len() < 2 || data[0] != 0xFF || data[1] != M_SOI {
        return Err(ImageError::BadSignature);
    }
    let mut p = 2usize;

    let mut quant: [[u16; 64]; 4] = [[0; 64]; 4];
    let mut dc_tables: [Option<HuffTable>; 4] = Default::default();
    let mut ac_tables: [Option<HuffTable>; 4] = Default::default();
    let mut components: Vec<Component> = Vec::new();
    let mut width = 0u32;
    let mut height = 0u32;
    let mut restart_interval = 0usize;

    // -- Parse marker segments up to and including SOS. --
    loop {
        // Seek the next marker (skip fill bytes).
        while p < data.len() && data[p] != 0xFF {
            p += 1;
        }
        while p < data.len() && data[p] == 0xFF {
            p += 1;
        }
        if p >= data.len() {
            return Err(ImageError::Truncated);
        }
        let marker = data[p];
        p += 1;
        match marker {
            M_SOF2 => return Err(ImageError::Unsupported), // progressive
            M_DQT => {
                let seg_len = be16(data, p)?;
                let end = p + seg_len;
                let mut q = p + 2;
                while q < end {
                    let pq_tq = *data.get(q).ok_or(ImageError::Truncated)?;
                    q += 1;
                    let precision = pq_tq >> 4; // 0 = 8-bit, 1 = 16-bit
                    let tq = (pq_tq & 0x0F) as usize;
                    if tq >= 4 {
                        return Err(ImageError::Corrupt);
                    }
                    for coeff in quant[tq].iter_mut() {
                        if precision == 0 {
                            *coeff = *data.get(q).ok_or(ImageError::Truncated)? as u16;
                            q += 1;
                        } else {
                            *coeff = be16(data, q)? as u16;
                            q += 2;
                        }
                    }
                }
                p = end;
            }
            M_DHT => {
                let seg_len = be16(data, p)?;
                let end = p + seg_len;
                let mut q = p + 2;
                while q < end {
                    let tc_th = *data.get(q).ok_or(ImageError::Truncated)?;
                    q += 1;
                    let class = tc_th >> 4; // 0 = DC, 1 = AC
                    let id = (tc_th & 0x0F) as usize;
                    if id >= 4 {
                        return Err(ImageError::Corrupt);
                    }
                    let mut counts = [0u8; 16];
                    for c in counts.iter_mut() {
                        *c = *data.get(q).ok_or(ImageError::Truncated)?;
                        q += 1;
                    }
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    let values = data
                        .get(q..q + total)
                        .ok_or(ImageError::Truncated)?
                        .to_vec();
                    q += total;
                    let table = HuffTable::build(&counts, values);
                    if class == 0 {
                        dc_tables[id] = Some(table);
                    } else {
                        ac_tables[id] = Some(table);
                    }
                }
                p = end;
            }
            M_DRI => {
                restart_interval = be16(data, p + 2)?;
                p += be16(data, p)?;
            }
            M_SOF0 => {
                let precision = *data.get(p + 2).ok_or(ImageError::Truncated)?;
                if precision != 8 {
                    return Err(ImageError::Unsupported);
                }
                height = be16(data, p + 3)? as u32;
                width = be16(data, p + 5)? as u32;
                let ncomp = *data.get(p + 7).ok_or(ImageError::Truncated)? as usize;
                if ncomp != 1 && ncomp != 3 {
                    return Err(ImageError::Unsupported);
                }
                let mut q = p + 8;
                for _ in 0..ncomp {
                    let id = *data.get(q).ok_or(ImageError::Truncated)?;
                    let hv = *data.get(q + 1).ok_or(ImageError::Truncated)?;
                    let tq = *data.get(q + 2).ok_or(ImageError::Truncated)? as usize;
                    components.push(Component {
                        id,
                        h: hv >> 4,
                        v: hv & 0x0F,
                        quant: tq.min(3),
                        ..Default::default()
                    });
                    q += 3;
                }
                p += be16(data, p)?;
            }
            M_SOS => {
                let ns = *data.get(p + 2).ok_or(ImageError::Truncated)? as usize;
                let mut q = p + 3;
                for _ in 0..ns {
                    let cs = *data.get(q).ok_or(ImageError::Truncated)?;
                    let td_ta = *data.get(q + 1).ok_or(ImageError::Truncated)?;
                    if let Some(comp) = components.iter_mut().find(|c| c.id == cs) {
                        comp.dc_table = (td_ta >> 4) as usize;
                        comp.ac_table = (td_ta & 0x0F) as usize;
                    }
                    q += 2;
                }
                // Skip Ss, Se, Ah/Al (3 bytes) → entropy data begins.
                p = q + 3;
                break;
            }
            M_EOI => return Err(ImageError::Corrupt), // SOS never seen
            _ => {
                // Any other marker segment: skip by its length.
                p += be16(data, p)?;
            }
        }
    }

    if width == 0 || height == 0 || components.is_empty() {
        return Err(ImageError::Corrupt);
    }
    let (w, h) = (width as usize, height as usize);
    if w.checked_mul(h)
        .map(|n| n > MAX_IMAGE_PIXELS)
        .unwrap_or(true)
    {
        return Err(ImageError::GeometryOverflow);
    }

    decode_scan(
        data,
        p,
        w,
        h,
        &quant,
        &dc_tables,
        &ac_tables,
        &mut components,
        restart_interval,
    )
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(
    data: &[u8],
    start: usize,
    w: usize,
    h: usize,
    quant: &[[u16; 64]; 4],
    dc_tables: &[Option<HuffTable>; 4],
    ac_tables: &[Option<HuffTable>; 4],
    components: &mut [Component],
    restart_interval: usize,
) -> Result<(u32, u32, Vec<u32>), ImageError> {
    let hmax = components.iter().map(|c| c.h).max().unwrap_or(1) as usize;
    let vmax = components.iter().map(|c| c.v).max().unwrap_or(1) as usize;
    let mcu_w = 8 * hmax;
    let mcu_h = 8 * vmax;
    let mcus_x = w.div_ceil(mcu_w);
    let mcus_y = h.div_ceil(mcu_h);

    // Per-component full-resolution plane (upsampled by nearest during
    // color conversion). Store at component sampling resolution.
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(components.len());
    let mut plane_dims: Vec<(usize, usize)> = Vec::with_capacity(components.len());
    for c in components.iter() {
        let cw = mcus_x * (c.h as usize) * 8;
        let ch = mcus_y * (c.v as usize) * 8;
        if cw
            .checked_mul(ch)
            .map(|n| n > MAX_IMAGE_PIXELS * 4)
            .unwrap_or(true)
        {
            return Err(ImageError::GeometryOverflow);
        }
        planes.push(vec![0u8; cw * ch]);
        plane_dims.push((cw, ch));
    }

    let mut br = BitReader::new(data, start);
    let mut block = [0i32; 64];
    let mut mcu_count = 0usize;

    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            // Restart handling.
            if restart_interval != 0 && mcu_count != 0 && mcu_count % restart_interval == 0 {
                // Expect an RSTn marker; skip it and reset predictors.
                skip_restart(&mut br)?;
                for c in components.iter_mut() {
                    c.dc_pred = 0;
                }
            }
            for ci in 0..components.len() {
                let (ch_samp, cv_samp) = (components[ci].h as usize, components[ci].v as usize);
                let dc_t = dc_tables[components[ci].dc_table]
                    .as_ref()
                    .ok_or(ImageError::Corrupt)?;
                let ac_t = ac_tables[components[ci].ac_table]
                    .as_ref()
                    .ok_or(ImageError::Corrupt)?;
                let q = &quant[components[ci].quant];
                for by in 0..cv_samp {
                    for bx in 0..ch_samp {
                        block.fill(0);
                        decode_block(
                            &mut br,
                            dc_t,
                            ac_t,
                            q,
                            &mut components[ci].dc_pred,
                            &mut block,
                        )?;
                        let mut out = [0u8; 64];
                        idct_8x8(&block, &mut out);
                        // Place the 8×8 into the component plane.
                        let (cw, _cheight) = plane_dims[ci];
                        let px0 = (mx * ch_samp + bx) * 8;
                        let py0 = (my * cv_samp + by) * 8;
                        for row in 0..8 {
                            let dst = (py0 + row) * cw + px0;
                            planes[ci][dst..dst + 8].copy_from_slice(&out[row * 8..row * 8 + 8]);
                        }
                    }
                }
            }
            mcu_count += 1;
        }
    }

    // -- Color convert + upsample into the final BGRA buffer. --
    let mut pixels = vec![0u32; w * h];
    let grayscale = components.len() == 1;
    for y in 0..h {
        for x in 0..w {
            if grayscale {
                let (cw, _) = plane_dims[0];
                let val = planes[0][y * cw + x] as i32;
                pixels[y * w + x] = pack_bgra(val, val, val);
            } else {
                // Y at full res; Cb/Cr sampled by their factor vs max.
                let yv = sample(&planes[0], plane_dims[0], components[0], hmax, vmax, x, y) as i32;
                let cb = sample(&planes[1], plane_dims[1], components[1], hmax, vmax, x, y) as i32;
                let cr = sample(&planes[2], plane_dims[2], components[2], hmax, vmax, x, y) as i32;
                let (r, g, b) = ycbcr_to_rgb(yv, cb, cr);
                pixels[y * w + x] = pack_bgra(r, g, b);
            }
        }
    }
    Ok((w as u32, h as u32, pixels))
}

/// Sample a component plane at output pixel (x,y), scaling by the
/// component's sampling factor relative to the max (nearest-neighbor
/// upsampling — adequate for a viewer).
fn sample(
    plane: &[u8],
    dims: (usize, usize),
    comp: Component,
    hmax: usize,
    vmax: usize,
    x: usize,
    y: usize,
) -> u8 {
    let (cw, ch) = dims;
    let sx = x * (comp.h as usize) / hmax;
    let sy = y * (comp.v as usize) / vmax;
    let sx = sx.min(cw.saturating_sub(1));
    let sy = sy.min(ch.saturating_sub(1));
    plane[sy * cw + sx]
}

/// Decode a single 8×8 block: DC (differential) + AC (run-length),
/// dequantize into natural order.
fn decode_block(
    br: &mut BitReader,
    dc_t: &HuffTable,
    ac_t: &HuffTable,
    q: &[u16; 64],
    dc_pred: &mut i32,
    block: &mut [i32; 64],
) -> Result<(), ImageError> {
    // DC coefficient.
    let s = br.decode_huff(dc_t)? as u32;
    let diff = extend(br.get_bits(s), s);
    *dc_pred += diff;
    block[0] = *dc_pred * q[0] as i32;

    // AC coefficients.
    let mut k = 1usize;
    while k < 64 {
        let rs = br.decode_huff(ac_t)?;
        let run = (rs >> 4) as usize;
        let size = (rs & 0x0F) as u32;
        if size == 0 {
            if run == 15 {
                k += 16; // ZRL: 16 zeros
                continue;
            }
            break; // EOB
        }
        k += run;
        if k >= 64 {
            break;
        }
        let val = extend(br.get_bits(size), size);
        block[ZIGZAG[k]] = val * q[k] as i32;
        k += 1;
    }
    Ok(())
}

/// Skip a restart marker (RST0..RST7) in the entropy stream.
fn skip_restart(br: &mut BitReader) -> Result<(), ImageError> {
    // Drain to the marker. `next_byte` records a marker in `br.marker`.
    br.bits = 0;
    br.nbits = 0;
    // Advance pos to the 0xFF of the next marker.
    while br.pos + 1 < br.data.len() {
        if br.data[br.pos] == 0xFF {
            let m = br.data[br.pos + 1];
            if (0xD0..=0xD7).contains(&m) {
                br.pos += 2;
                br.reset_for_restart();
                return Ok(());
            } else if m == 0x00 {
                br.pos += 2;
            } else {
                // Some other marker before the expected RST — tolerate.
                br.reset_for_restart();
                return Ok(());
            }
        } else {
            br.pos += 1;
        }
    }
    Ok(())
}

/// Separable float IDCT (8×8), level-shifted + clamped to `u8`.
fn idct_8x8(block: &[i32; 64], out: &mut [u8; 64]) {
    // Precomputed cosine basis; c[u][x] = cos((2x+1)uπ/16) * (u==0 ? 1/√2 : 1).
    let mut tmp = [0f32; 64];
    // Rows.
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0f32;
            for u in 0..8 {
                sum += alpha(u) * block[y * 8 + u] as f32 * COS[u][x];
            }
            tmp[y * 8 + x] = sum;
        }
    }
    // Columns.
    for x in 0..8 {
        for y in 0..8 {
            let mut sum = 0f32;
            for v in 0..8 {
                sum += alpha(v) * tmp[v * 8 + x] * COS[v][y];
            }
            // Round-half-up without `f32::round` (unavailable in no_std):
            // `+0.5` then truncating cast; the clamp handles out-of-range.
            let val = (sum / 4.0) + 128.0 + 0.5;
            out[y * 8 + x] = (val as i32).clamp(0, 255) as u8;
        }
    }
}

fn alpha(u: usize) -> f32 {
    if u == 0 {
        core::f32::consts::FRAC_1_SQRT_2
    } else {
        1.0
    }
}

/// `COS[u][x] = cos((2x+1) * u * π / 16)`.
const COS: [[f32; 8]; 8] = cos_table();

const fn cos_table() -> [[f32; 8]; 8] {
    // `cos` isn't const, so bake the exact rational-angle values. These
    // are cos(k·π/16) for the eight distinct arguments, arranged per the
    // separable-IDCT indexing. Computed to f32 precision.
    // COS[u][x] = cos((2x+1)uπ/16).
    // We fill it at runtime instead — see `init` note. Placeholder zeros
    // are replaced by `cos_fill()` lazily; but const fn can't call cos,
    // so use the closed-form table below.
    [
        [1.0; 8],
        [
            0.980_785_25,
            0.831_469_6,
            0.555_570_24,
            0.195_090_32,
            -0.195_090_32,
            -0.555_570_24,
            -0.831_469_6,
            -0.980_785_25,
        ],
        [
            0.923_879_5,
            0.382_683_43,
            -0.382_683_43,
            -0.923_879_5,
            -0.923_879_5,
            -0.382_683_43,
            0.382_683_43,
            0.923_879_5,
        ],
        [
            0.831_469_6,
            -0.195_090_32,
            -0.980_785_25,
            -0.555_570_24,
            0.555_570_24,
            0.980_785_25,
            0.195_090_32,
            -0.831_469_6,
        ],
        [
            0.707_106_77,
            -0.707_106_77,
            -0.707_106_77,
            0.707_106_77,
            0.707_106_77,
            -0.707_106_77,
            -0.707_106_77,
            0.707_106_77,
        ],
        [
            0.555_570_24,
            -0.980_785_25,
            0.195_090_32,
            0.831_469_6,
            -0.831_469_6,
            -0.195_090_32,
            0.980_785_25,
            -0.555_570_24,
        ],
        [
            0.382_683_43,
            -0.923_879_5,
            0.923_879_5,
            -0.382_683_43,
            -0.382_683_43,
            0.923_879_5,
            -0.923_879_5,
            0.382_683_43,
        ],
        [
            0.195_090_32,
            -0.555_570_24,
            0.831_469_6,
            -0.980_785_25,
            0.980_785_25,
            -0.831_469_6,
            0.555_570_24,
            -0.195_090_32,
        ],
    ]
}

fn ycbcr_to_rgb(y: i32, cb: i32, cr: i32) -> (i32, i32, i32) {
    let cbf = cb - 128;
    let crf = cr - 128;
    // Fixed-point BT.601 full-range JFIF conversion.
    let r = y + ((91881 * crf) >> 16);
    let g = y - ((22554 * cbf + 46802 * crf) >> 16);
    let b = y + ((116130 * cbf) >> 16);
    (r, g, b)
}

fn pack_bgra(r: i32, g: i32, b: i32) -> u32 {
    let c = |v: i32| v.clamp(0, 255) as u32;
    0xFF00_0000 | (c(r) << 16) | (c(g) << 8) | c(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16×16 baseline JPEG generated on the host with libjpeg
    /// (`cjpeg`), embedded so the decoder is exercised against a real
    /// bitstream. Provenance + regeneration recipe: see
    /// `tests/fixtures/README` in this crate.
    const TINY_JPEG: &[u8] = include_bytes!("../tests/fixtures/tiny16.jpg");

    #[test]
    fn decodes_baseline_dimensions_and_sane_pixels() {
        let (w, h, px) = decode_jpeg(TINY_JPEG).expect("baseline JPEG must decode");
        assert_eq!((w, h), (16, 16));
        assert_eq!(px.len(), 16 * 16);
        // Every pixel opaque; the image is not uniform (it has a gradient
        // / pattern), so at least two distinct colors appear.
        assert!(px.iter().all(|p| (p >> 24) == 0xFF));
        let first = px[0];
        assert!(px.iter().any(|&p| p != first), "decoded image is uniform");
        // In-gamut by construction (pack_bgra clamps) — spot-check a few.
        for &p in px.iter().step_by(37) {
            let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
            assert!(r <= 255 && g <= 255 && b <= 255);
        }
    }

    #[test]
    fn rejects_non_jpeg() {
        assert_eq!(
            decode_jpeg(b"not a jpeg").unwrap_err(),
            ImageError::BadSignature
        );
        assert_eq!(decode_jpeg(&[0xFF]).unwrap_err(), ImageError::BadSignature);
    }

    #[test]
    fn rejects_progressive() {
        // SOI then a SOF2 (progressive) marker segment.
        let prog = [0xFF, M_SOI, 0xFF, M_SOF2, 0x00, 0x02];
        assert_eq!(decode_jpeg(&prog).unwrap_err(), ImageError::Unsupported);
    }

    #[test]
    fn truncation_never_panics() {
        for cut in 0..TINY_JPEG.len() {
            let _ = decode_jpeg(&TINY_JPEG[..cut]);
        }
    }
}
