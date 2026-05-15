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
