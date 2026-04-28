//! Phase 57 Track G.2 — bitmap font provider.
//!
//! The `term` graphical client (Phase 57 Track G) consumes [`FontProvider`]
//! to render screen cells into surface buffers. Putting the font behind a
//! trait keeps the door open for a future TrueType path without forcing
//! one in Phase 57; YAGNI says one bitmap font, one size, one color.
//!
//! The bundled implementation is [`BasicBitmapFont`], an 8×16 cell font
//! covering ASCII 0x20 through 0x7F (printable ASCII plus DEL). Glyph
//! data is statically embedded — no runtime file I/O — and lives in the
//! sibling [`crate::session::font_data`] module so this file stays
//! readable.
//!
//! ## Design notes
//!
//! - The trait's `glyph` method returns `Option<&Glyph>`: callers
//!   distinguish "missing glyph" from "rendered placeholder" themselves.
//!   The `term` screen layer paints a missing-glyph cell with the
//!   background colour rather than punching a placeholder in, matching
//!   the G.4 acceptance "no allocation per character".
//! - `Glyph::render_into` writes BGRA8888 pixels into a caller-owned
//!   buffer with caller-supplied stride. The function is a pure logic
//!   helper — no allocation, no I/O — and returns a typed
//!   [`FontError`] when the buffer or stride cannot accommodate the
//!   glyph cell.
//! - The trait is `no_std`-friendly (no `Vec`, no `Box`, no `&str`
//!   formatting); host tests in `kernel-core/tests/phase57_g2_font.rs`
//!   exercise it on `cargo test -p kernel-core --target
//!   x86_64-unknown-linux-gnu`.

use super::font_data::{ASCII_FIRST, ASCII_LAST, CELL_HEIGHT, CELL_WIDTH, GLYPH_BITMAPS};

/// Errors observable on the font public surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FontError {
    /// Caller's pixel buffer is too small to hold the glyph cell.
    BufferTooSmall,
    /// Caller's stride is smaller than the glyph width — rendering
    /// would write past the row boundary into the next row.
    InvalidStride,
}

/// A bitmap glyph cell.
///
/// `bitmap` is packed bits, row-major, MSB first within each byte (bit
/// 7 of each byte is the leftmost pixel, matching the IBM VGA 8×16
/// font convention). For an 8-pixel-wide cell this is exactly one byte
/// per row; for wider cells the layout is `((width + 7) / 8)` bytes per
/// row. The bundled font uses 8-pixel-wide cells so each row is one
/// byte.
///
/// `&'static [u8]` for the bitmap means glyph data lives in
/// statically-initialised memory; no allocation, no file I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    /// Pixel width of the cell.
    pub width: u8,
    /// Pixel height of the cell.
    pub height: u8,
    /// Packed bitmap data; see the type-level docs for the layout.
    pub bitmap: &'static [u8],
}

impl Glyph {
    /// Render this glyph into `buf` at the origin of the cell.
    ///
    /// `stride` is the width of `buf` in pixels; it must be at least
    /// `self.width`. `buf.len()` must be at least `stride * height`.
    /// `fg` and `bg` are BGRA8888 packed pixel values; the caller picks
    /// the byte order, this method only copies them through.
    ///
    /// Returns `Ok(())` on success, or a typed [`FontError`] when the
    /// buffer or stride cannot accommodate the glyph cell.
    pub fn render_into(
        &self,
        buf: &mut [u32],
        stride: usize,
        fg: u32,
        bg: u32,
    ) -> Result<(), FontError> {
        let w = self.width as usize;
        let h = self.height as usize;
        if stride < w {
            return Err(FontError::InvalidStride);
        }
        if buf.len() < stride * h {
            return Err(FontError::BufferTooSmall);
        }
        let bytes_per_row = w.div_ceil(8);
        for row in 0..h {
            let row_start = row * bytes_per_row;
            for col in 0..w {
                let byte_idx = row_start + (col / 8);
                let bit_idx = 7 - (col % 8);
                let bit_set = (self.bitmap[byte_idx] >> bit_idx) & 1 == 1;
                let dst = row * stride + col;
                buf[dst] = if bit_set { fg } else { bg };
            }
        }
        Ok(())
    }
}

/// A pluggable font surface. Implementations decide which codepoints
/// they support and what cell size they render at.
///
/// Returning `&Glyph` (rather than `Glyph` by value) keeps the `term`
/// screen layer allocation-free: the font owns the glyph data, the
/// caller borrows it for the duration of one render pass.
pub trait FontProvider {
    /// Look up the glyph for `codepoint`. Returns `None` when the font
    /// does not cover the codepoint; the caller decides what to draw
    /// in that case (the bundled `term` paints a missing cell with the
    /// background colour).
    fn glyph(&self, codepoint: u32) -> Option<&Glyph>;

    /// The font's cell size: `(width, height)` in pixels. Every glyph
    /// returned from [`glyph`] has these dimensions; the cell size is
    /// fixed for the lifetime of the font instance.
    fn cell_size(&self) -> (u8, u8);
}

/// The bundled 8×16 bitmap font.
///
/// Covers ASCII 0x20 (space) through 0x7F (DEL). Glyph data is sourced
/// from the public-domain IBM VGA BIOS 8×16 font; see
/// [`crate::session::font_data`] for the byte arrays and the source
/// reference.
///
/// `BasicBitmapFont` is a zero-sized type — there is exactly one font
/// in Phase 57 and the data is statically initialised, so the
/// "instance" carries no state. Future tracks may add additional fonts
/// behind a different concrete type implementing [`FontProvider`].
#[derive(Clone, Copy, Default, Debug)]
pub struct BasicBitmapFont;

impl BasicBitmapFont {
    /// Construct a fresh `BasicBitmapFont`. Const because the type is
    /// stateless and we want it usable from `static` contexts.
    pub const fn new() -> Self {
        Self
    }
}

impl FontProvider for BasicBitmapFont {
    fn glyph(&self, codepoint: u32) -> Option<&Glyph> {
        if !(ASCII_FIRST..=ASCII_LAST).contains(&codepoint) {
            return None;
        }
        let idx = (codepoint - ASCII_FIRST) as usize;
        Some(&GLYPH_BITMAPS[idx])
    }

    fn cell_size(&self) -> (u8, u8) {
        (CELL_WIDTH, CELL_HEIGHT)
    }
}
