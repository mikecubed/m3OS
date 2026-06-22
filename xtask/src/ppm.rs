//! Minimal PPM (P6) reader + pixel-diff helpers for the
//! `less-render-probe` subcommand.
//!
//! QEMU's `screendump` writes a binary P6 PPM: an ASCII header
//! followed by raw `width * height * 3` bytes (R, G, B per pixel,
//! no alpha). The header looks like:
//!
//! ```text
//! P6
//! <width> <height>
//! <maxval>
//! <binary pixel data>
//! ```
//!
//! whitespace between header fields can be any combination of space,
//! tab, CR, LF. A `#` introduces a comment that runs to end-of-line.
//! This reader handles all three.
//!
//! Diff helpers compute a [`FrameHash`] (FNV-1a-64 of the raw pixel
//! bytes) and a [`pixel_diff_ratio`] that returns the fraction of
//! pixels whose RGB triple differs between two frames. Both are
//! enough to distinguish "screen looks identical", "screen differs
//! slightly (a cursor blink)", and "screen went black".

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

/// In-memory PPM frame: dimensions plus the raw RGB pixel bytes the
/// reader extracted. `pixels.len() == width * height * 3`.
pub struct PpmFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl core::fmt::Debug for PpmFrame {
    /// Summarize dimensions + byte count rather than dumping the full pixel
    /// buffer (needed so `Result<PpmFrame, _>::expect_err` works in tests).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PpmFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixels_len", &self.pixels.len())
            .finish()
    }
}

impl PpmFrame {
    /// FNV-1a 64-bit hash over the entire pixel buffer. Used to print
    /// a stable per-frame fingerprint in the probe output; two
    /// frames with matching hashes are byte-identical.
    pub fn hash(&self) -> u64 {
        fnv1a_64(&self.pixels)
    }

    /// Fraction of pixels whose `(r, g, b)` triple is all-zero
    /// (true black). Used as a quick heuristic for "screen went
    /// black" without having to compare against a known-good frame.
    pub fn black_pixel_ratio(&self) -> f64 {
        if self.pixels.is_empty() {
            return 0.0;
        }
        let mut black = 0u64;
        let mut total = 0u64;
        for chunk in self.pixels.chunks_exact(3) {
            total += 1;
            if chunk[0] == 0 && chunk[1] == 0 && chunk[2] == 0 {
                black += 1;
            }
        }
        if total == 0 {
            0.0
        } else {
            (black as f64) / (total as f64)
        }
    }

    /// Spatial-spread heuristic for non-black pixels: returns
    /// `(rows_used, cols_used)` — the number of rows and columns that
    /// contain at least one non-black pixel. Used by the render-probe
    /// report to distinguish real m3OS content (non-black spread
    /// across most of the screen) from leftover OVMF startup chrome
    /// (a few pixels clustered in the top-left corner).
    pub fn non_black_spread(&self) -> (u32, u32) {
        if self.pixels.is_empty() {
            return (0, 0);
        }
        let mut rows: Vec<bool> = vec![false; self.height as usize];
        let mut cols: Vec<bool> = vec![false; self.width as usize];
        for y in 0..self.height {
            let row_start = (y as usize) * (self.width as usize) * 3;
            for x in 0..self.width {
                let i = row_start + (x as usize) * 3;
                if i + 2 >= self.pixels.len() {
                    break;
                }
                if self.pixels[i] != 0 || self.pixels[i + 1] != 0 || self.pixels[i + 2] != 0 {
                    rows[y as usize] = true;
                    cols[x as usize] = true;
                }
            }
        }
        let row_count = rows.into_iter().filter(|b| *b).count() as u32;
        let col_count = cols.into_iter().filter(|b| *b).count() as u32;
        (row_count, col_count)
    }
}

/// Parse a P6 PPM file from disk.
///
/// Returns an error if the magic isn't `P6`, dimensions are absurd
/// (we cap at 16 K × 16 K to avoid an OOM on a corrupted header),
/// or the binary payload is shorter than the header advertises.
pub fn read_ppm(path: &Path) -> Result<PpmFrame, String> {
    let mut file = File::open(path).map_err(|e| format!("ppm: open {}: {e}", path.display()))?;
    // Read the header byte-by-byte so we don't over-shoot into the
    // binary pixel section. PPM headers are tiny (< 32 bytes for any
    // realistic resolution) so per-byte reads are not a bottleneck.
    let header =
        read_header(&mut file).map_err(|e| format!("ppm: header from {}: {e}", path.display()))?;
    let mut pixels = vec![0u8; header.pixel_bytes()];
    file.read_exact(&mut pixels)
        .map_err(|e| format!("ppm: pixel body from {}: {e}", path.display()))?;
    if header.maxval != 255 {
        return Err(format!(
            "ppm: unsupported maxval {} in {}; only 255 is supported",
            header.maxval,
            path.display()
        ));
    }
    Ok(PpmFrame {
        width: header.width,
        height: header.height,
        pixels,
    })
}

struct PpmHeader {
    width: u32,
    height: u32,
    maxval: u32,
}

impl PpmHeader {
    fn pixel_bytes(&self) -> usize {
        (self.width as usize) * (self.height as usize) * 3
    }
}

fn read_header(file: &mut File) -> Result<PpmHeader, String> {
    // The header is `P6` + whitespace + 3 ASCII numbers (width,
    // height, maxval), each separated by whitespace, with `#`
    // comments to end-of-line allowed. After the third number's
    // trailing whitespace byte the binary section begins.
    let magic = read_token(file)?;
    if magic != "P6" {
        return Err(format!("expected P6 magic, got '{magic}'"));
    }
    let width: u32 = read_token(file)?
        .parse()
        .map_err(|e| format!("bad width: {e}"))?;
    let height: u32 = read_token(file)?
        .parse()
        .map_err(|e| format!("bad height: {e}"))?;
    let maxval: u32 = read_token(file)?
        .parse()
        .map_err(|e| format!("bad maxval: {e}"))?;
    // Sanity caps. We bound *total bytes* rather than per-dimension
    // size because the dimension cap alone is too loose to defend the
    // buffer allocation: 16 K × 16 K × 3 is ~768 MiB and would still
    // OOM on a corrupted header. Real QEMU screendumps for the m3OS
    // framebuffer are at most a few MiB (1280 × 800 × 3 ≈ 3 MiB), so a
    // 64 MiB cap is generous headroom and well below any allocation
    // we'd accept in xtask. `checked_mul` defends usize overflow on
    // 32-bit hosts even though we currently only target 64-bit dev
    // machines.
    if width == 0 || height == 0 {
        return Err(format!("nonsensical dimensions {width} x {height}"));
    }
    if maxval == 0 || maxval > 65_535 {
        return Err(format!("bad maxval {maxval}"));
    }
    const MAX_PIXEL_BYTES: usize = 64 * 1024 * 1024;
    let total_bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|wh| wh.checked_mul(3))
        .ok_or_else(|| format!("dimensions overflow usize: {width} x {height}"))?;
    if total_bytes > MAX_PIXEL_BYTES {
        return Err(format!(
            "image exceeds {} MiB cap: {width} x {height} = {total_bytes} bytes",
            MAX_PIXEL_BYTES / (1024 * 1024)
        ));
    }
    Ok(PpmHeader {
        width,
        height,
        maxval,
    })
}

/// Read one whitespace-separated ASCII token, skipping comments and
/// any leading whitespace. Stops on the first whitespace byte after
/// the token (which is consumed); subsequent bytes are still
/// available for the next `read_token` call.
///
/// The implementation reads single bytes via `Seek`-aware `read`
/// calls — clean and clearly stateful. PPM headers are short so the
/// per-byte cost is irrelevant.
fn read_token(file: &mut File) -> Result<String, String> {
    let mut byte = [0u8; 1];
    let mut token = String::new();

    // Skip leading whitespace + comment lines.
    loop {
        let n = file
            .read(&mut byte)
            .map_err(|e| format!("read header byte: {e}"))?;
        if n == 0 {
            return Err("unexpected EOF reading header".into());
        }
        match byte[0] {
            b'#' => {
                // Skip to end-of-line (LF). Comments in PPM run to
                // EOL; both LF and CR-LF terminations are accepted
                // by every reader I know of.
                loop {
                    let n = file
                        .read(&mut byte)
                        .map_err(|e| format!("read comment byte: {e}"))?;
                    if n == 0 || byte[0] == b'\n' {
                        break;
                    }
                }
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            other => {
                token.push(other as char);
                break;
            }
        }
    }

    // Now accumulate non-whitespace bytes into the token.
    loop {
        let n = file
            .read(&mut byte)
            .map_err(|e| format!("read token byte: {e}"))?;
        if n == 0 {
            break;
        }
        match byte[0] {
            b' ' | b'\t' | b'\r' | b'\n' => break,
            b'#' => {
                // Re-establish comment-skip; the byte we just read
                // ends the token, then the next token starts after
                // the comment line.
                loop {
                    let n = file
                        .read(&mut byte)
                        .map_err(|e| format!("read inline comment: {e}"))?;
                    if n == 0 || byte[0] == b'\n' {
                        break;
                    }
                }
                break;
            }
            other => token.push(other as char),
        }
    }

    // Make sure subsequent callers can still read sequentially. We
    // already advanced the cursor; the `Seek` import is here purely
    // to make the byte position observable in tests if a regression
    // ever splits a token. `file.stream_position()` is the
    // human-readable form of that.
    let _ = file.stream_position();
    Ok(token)
}

/// Fraction of *pixels* (RGB triples) whose RGB values differ between
/// the two frames. Frames with mismatched dimensions return `1.0`
/// (entirely different).
///
/// The ratio is a coarse single-number summary; for a tight
/// regression test the caller should compare hashes plus eyeball
/// the PPM artefacts. The probe uses this to print a per-pair score:
///
/// * `0.000` — frames are identical (cursor steady, nothing changed)
/// * `< 0.02` — minor pixel changes (cursor blink, cursor move)
/// * `> 0.5` — substantial repaint (the bug we're hunting)
pub fn pixel_diff_ratio(a: &PpmFrame, b: &PpmFrame) -> f64 {
    if a.width != b.width || a.height != b.height {
        return 1.0;
    }
    // Belt-and-braces guard for manually-constructed `PpmFrame`s whose
    // pixel buffer length doesn't match `width * height * 3` — the
    // `chunks_exact(3).zip` below would otherwise silently truncate to
    // the shorter buffer and under-report the difference.
    let expected = (a.width as usize)
        .checked_mul(a.height as usize)
        .and_then(|wh| wh.checked_mul(3));
    if expected != Some(a.pixels.len()) || a.pixels.len() != b.pixels.len() {
        return 1.0;
    }
    if a.pixels.is_empty() {
        return 0.0;
    }
    let mut diff_pixels = 0u64;
    let mut total = 0u64;
    let pairs = a.pixels.chunks_exact(3).zip(b.pixels.chunks_exact(3));
    for (pa, pb) in pairs {
        total += 1;
        if pa != pb {
            diff_pixels += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        (diff_pixels as f64) / (total as f64)
    }
}

/// FNV-1a 64-bit hash. The pixel-content fingerprint we print in the
/// probe report; collision-resistance is irrelevant — what matters is
/// that two byte-identical frames produce the same value.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_ppm_file(width: u32, height: u32, pixels: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("tempfile");
        write!(f, "P6\n{width} {height}\n255\n").unwrap();
        f.write_all(pixels).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn roundtrip_two_by_two() {
        let pixels = vec![
            255, 0, 0, // red
            0, 255, 0, // green
            0, 0, 255, // blue
            255, 255, 255, // white
        ];
        let f = write_ppm_file(2, 2, &pixels);
        let frame = read_ppm(f.path()).expect("read");
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        assert_eq!(frame.pixels, pixels);
    }

    #[test]
    fn header_comments_are_tolerated() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "P6").unwrap();
        writeln!(f, "# Created by xtask test").unwrap();
        writeln!(f, "2 1").unwrap();
        writeln!(f, "255").unwrap();
        f.write_all(&[10, 20, 30, 40, 50, 60]).unwrap();
        f.flush().unwrap();
        let frame = read_ppm(f.path()).expect("read");
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 1);
        assert_eq!(frame.pixels, vec![10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn black_pixel_ratio_all_black() {
        let frame = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 0, 0, 0],
        };
        assert_eq!(frame.black_pixel_ratio(), 1.0);
    }

    #[test]
    fn black_pixel_ratio_half() {
        let frame = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 255, 255, 255],
        };
        assert!((frame.black_pixel_ratio() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn diff_ratio_identical_is_zero() {
        let a = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 255, 0, 0],
        };
        let b = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 255, 0, 0],
        };
        assert_eq!(pixel_diff_ratio(&a, &b), 0.0);
    }

    #[test]
    fn diff_ratio_one_differing_pixel_of_two_is_half() {
        let a = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 255, 0, 0],
        };
        let b = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 0, 255, 0],
        };
        assert!((pixel_diff_ratio(&a, &b) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn diff_ratio_mismatched_dimensions_is_one() {
        let a = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0; 6],
        };
        let b = PpmFrame {
            width: 1,
            height: 1,
            pixels: vec![0; 3],
        };
        assert_eq!(pixel_diff_ratio(&a, &b), 1.0);
    }

    #[test]
    fn hash_is_stable_across_calls() {
        let frame = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![10, 20, 30, 40, 50, 60],
        };
        assert_eq!(frame.hash(), frame.hash());
    }

    #[test]
    fn hash_changes_on_pixel_change() {
        let a = PpmFrame {
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0],
        };
        let b = PpmFrame {
            width: 1,
            height: 1,
            pixels: vec![1, 0, 0],
        };
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn reject_bad_magic() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "P3\n2 2\n255\nfoo").unwrap();
        f.flush().unwrap();
        assert!(read_ppm(f.path()).is_err());
    }

    #[test]
    fn reject_zero_dimensions() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "P6\n0 2\n255\n").unwrap();
        f.flush().unwrap();
        assert!(read_ppm(f.path()).is_err());
    }

    #[test]
    fn reject_dimensions_over_byte_cap() {
        // 16384 x 16384 x 3 = 768 MiB, well past the 64 MiB header cap.
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "P6\n16384 16384\n255\n").unwrap();
        f.flush().unwrap();
        let err = read_ppm(f.path()).expect_err("oversized header must be rejected");
        assert!(err.contains("64 MiB cap"), "unexpected error: {err}");
    }

    #[test]
    fn diff_ratio_mismatched_buffer_length_is_one() {
        // Manually constructed PpmFrame with width/height that agree
        // but a pixel buffer that doesn't match `w*h*3`. The guard
        // must early-return 1.0 instead of silently truncating via
        // `chunks_exact(3).zip`.
        let a = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0; 6],
        };
        let b = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0; 3], // half-sized buffer
        };
        assert_eq!(pixel_diff_ratio(&a, &b), 1.0);
    }

    #[test]
    fn diff_ratio_buffer_length_disagrees_with_dimensions() {
        // Buffers equal-length but neither matches `w*h*3`. The guard
        // catches this via the `expected != Some(a.pixels.len())`
        // check, again returning 1.0.
        let a = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0; 9], // claims 2x1 but stores 3 pixels worth
        };
        let b = PpmFrame {
            width: 2,
            height: 1,
            pixels: vec![0; 9],
        };
        assert_eq!(pixel_diff_ratio(&a, &b), 1.0);
    }
}
