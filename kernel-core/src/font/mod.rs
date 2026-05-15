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

/// Shared host-test fixture support — keeps the fixture path list
/// and the "reduced coverage" skip message in one place so the
/// three font test modules (atlas / parser / raster) cannot drift.
#[cfg(test)]
pub(crate) mod test_fixtures {
    extern crate std;
    use alloc::vec::Vec;
    use std::eprintln;

    /// Test fixture font candidates, in priority order:
    ///
    /// 1. The repository's staged Nerd Font, materialized by
    ///    `cargo xtask fetch-fonts`. This is the deterministic
    ///    fixture — every developer who builds the disk image
    ///    has this asset locally, and CI fetches it before running
    ///    `cargo xtask check`.
    /// 2. System-installed DejaVu Sans Mono at the canonical
    ///    Debian / Fedora / Arch paths. Kept as a fallback so a
    ///    fresh checkout that hasn't run `fetch-fonts` still has a
    ///    path that works on most Linux dev machines.
    /// 3. macOS Arial — last-resort fallback the parser tests rely
    ///    on for minimal coverage on Apple dev boxes.
    ///
    /// If none of these resolve, [`load_test_font_bytes`] returns
    /// `None` and the caller short-circuits so
    /// `cargo test -p kernel-core` does not hard-fail on a minimal
    /// dev box.
    pub(crate) const TEST_FONT_PATHS: &[&str] = &[
        "xtask/assets/fonts/term.ttf",
        "../xtask/assets/fonts/term.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/Library/Fonts/Arial.ttf",
    ];

    /// Walk [`TEST_FONT_PATHS`] and return the first readable file's
    /// bytes. Emits a loud `eprintln!` when none resolve so the skip
    /// is visible in `cargo test` output rather than silent.
    pub(crate) fn load_test_font_bytes() -> Option<Vec<u8>> {
        for candidate in TEST_FONT_PATHS {
            if let Ok(bytes) = std::fs::read(candidate) {
                return Some(bytes);
            }
        }
        eprintln!(
            "kernel-core font tests: no fixture font found; ran with reduced \
             coverage. Run `cargo xtask fetch-fonts` to stage the deterministic \
             fixture at xtask/assets/fonts/term.ttf."
        );
        None
    }
}

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
