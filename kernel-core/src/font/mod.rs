//! Phase 69c — TTF font infrastructure.
//!
//! This module wraps the vendored `ttf-parser` crate as a thin
//! `Font` façade (`parser.rs`), provides a 1-bit-per-pixel scanline
//! rasterizer (`raster.rs`), and a bounded LRU glyph atlas
//! (`atlas.rs`).
//!
//! The atlas is the seam Phase 69b's [`crate::session::glyph_tables::
//! resolve_glyph`] accessor was built for: when a `term` instance has
//! a loaded `.ttf`, runtime glyph lookups go through
//! [`atlas::Atlas::resolve`]; when font loading fails (file missing,
//! parse error), the renderer keeps using Phase 69b's static-table
//! path so ASCII / Latin-1 / box-drawing still paint.
//!
//! All three pieces are `no_std`-friendly and host-testable on the
//! workspace dev profile (`cargo test -p kernel-core`).

pub mod atlas;
pub mod parser;
pub mod raster;

pub use atlas::{Atlas, AtlasError, DEFAULT_ATLAS_CAPACITY};
pub use parser::{Font, FontError, GlyphId};
pub use raster::{RasterBitmap, Rasterizer};

/// A borrowed glyph bitmap descriptor — the common shape the
/// renderer hands to the framebuffer owner.
///
/// Both the Phase 69b static [`crate::session::font::Glyph`] and the
/// Phase 69c atlas-rasterized [`RasterBitmap`] flatten to a
/// `GlyphView` so the framebuffer owner consumes a single shape. The
/// view borrows from its owner; no allocation is involved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphView<'a> {
    /// Pixel width of the cell.
    pub width: u8,
    /// Pixel height of the cell.
    pub height: u8,
    /// Packed bitmap data, row-major, MSB-first per byte. Bytes per
    /// row is `ceil(width / 8)`.
    pub bitmap: &'a [u8],
}

impl RasterBitmap {
    /// Borrow this bitmap as the common [`GlyphView`] shape.
    pub fn as_view(&self) -> GlyphView<'_> {
        GlyphView {
            width: self.width,
            height: self.height,
            bitmap: &self.bitmap,
        }
    }
}

impl crate::session::font::Glyph {
    /// Borrow this static glyph as the common [`GlyphView`] shape.
    pub fn as_view(&self) -> GlyphView<'_> {
        GlyphView {
            width: self.width,
            height: self.height,
            bitmap: self.bitmap,
        }
    }
}
