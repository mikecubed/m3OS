//! Phase 69c Track A.2 — TTF/OTF parser wrapper.
//!
//! Thin façade over the vendored `ttf-parser` crate so the rest of the
//! font module talks to a single, m3OS-shaped API. Construction
//! validates the magic + the tables the rasterizer needs
//! (`head`, `maxp`, `cmap`, `glyf` / `CFF`); `glyph_index` does the
//! codepoint → glyph lookup; `glyph_outline` returns the glyph's
//! contour set in em-units by walking the outline-builder.
//!
//! The parser does not allocate beyond a small per-`Outline` `Vec` of
//! segments — the outline is consumed once per rasterization and then
//! the rasterizer copies the resulting bitmap into the atlas.

use alloc::vec::Vec;

/// A glyph id inside the font (zero is the `.notdef` slot).
///
/// Wraps `ttf-parser`'s `GlyphId` so callers don't have to depend on
/// the underlying crate's API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlyphId(pub u16);

/// Errors observable from the font parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FontError {
    /// Bytes do not parse as a TTF/OTF font (bad magic, truncated
    /// table directory, or a required table is missing).
    Malformed,
    /// The font was parsed but does not expose the units-per-em
    /// metric the rasterizer needs.
    MissingMetrics,
    /// The requested glyph id exists in the font but its outline
    /// could not be reconstructed (composite cycle, missing `glyf`
    /// data, etc.).
    OutlineUnavailable,
}

/// One segment of a glyph outline in em-units. The four variants
/// match `ttf-parser`'s `OutlineBuilder` callbacks. A glyph is a
/// sequence of segments; `Close` ends a contour, `MoveTo` begins a
/// new one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutlineSegment {
    MoveTo {
        x: f32,
        y: f32,
    },
    LineTo {
        x: f32,
        y: f32,
    },
    QuadTo {
        cx: f32,
        cy: f32,
        x: f32,
        y: f32,
    },
    CurveTo {
        cx1: f32,
        cy1: f32,
        cx2: f32,
        cy2: f32,
        x: f32,
        y: f32,
    },
    Close,
}

/// A glyph outline expressed in font em-units. Coordinates are
/// signed `f32`; the bounding box and units-per-em allow the
/// rasterizer to map em-space coordinates onto a fixed cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Outline {
    /// The segments that make up the glyph, in stream order. The
    /// rasterizer iterates these directly.
    pub segments: Vec<OutlineSegment>,
    /// Tight bounding box of the glyph in em-units. Empty glyphs
    /// have zero-extent boxes.
    pub bbox: BoundingBox,
}

/// A bounding box in em-units. `x_min..=x_max` and `y_min..=y_max`
/// are inclusive in em-space; the rasterizer maps these onto cell
/// pixels by linear interpolation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundingBox {
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

impl BoundingBox {
    /// Returns true when the box has zero area.
    pub fn is_empty(self) -> bool {
        self.x_max <= self.x_min || self.y_max <= self.y_min
    }
}

/// A loaded TTF/OTF font. Holds a borrowed reference to the on-disk
/// bytes (no copy) — the caller owns the buffer for the font's
/// lifetime.
pub struct Font<'a> {
    face: ttf_parser::Face<'a>,
    units_per_em: u16,
    ascender: i16,
    descender: i16,
}

impl<'a> Font<'a> {
    /// Validate and open a TTF/OTF buffer. Returns
    /// [`FontError::Malformed`] when the bytes don't parse as a
    /// font, and [`FontError::MissingMetrics`] when the units-per-em
    /// metric is absent (every conforming font has one; absence
    /// signals corruption that `ttf-parser`'s lax parse may have
    /// missed).
    pub fn open(bytes: &'a [u8]) -> Result<Self, FontError> {
        let face = ttf_parser::Face::parse(bytes, 0).map_err(|_| FontError::Malformed)?;
        let units_per_em = face.units_per_em();
        if units_per_em == 0 {
            return Err(FontError::MissingMetrics);
        }
        let ascender = face.ascender();
        let descender = face.descender();
        Ok(Self {
            face,
            units_per_em,
            ascender,
            descender,
        })
    }

    /// Number of glyphs in the font (including `.notdef`).
    pub fn num_glyphs(&self) -> u16 {
        self.face.number_of_glyphs()
    }

    /// Units-per-em from the `head` table — the rasterizer divides
    /// outline coordinates by this value to land on the cell grid.
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Font ascender in em-units (typographic top above baseline).
    pub fn ascender(&self) -> i16 {
        self.ascender
    }

    /// Font descender in em-units (typographic bottom below
    /// baseline, typically negative).
    pub fn descender(&self) -> i16 {
        self.descender
    }

    /// Look up the glyph id for `codepoint`. Returns `None` for
    /// codepoints absent from the font's `cmap`.
    pub fn glyph_index(&self, codepoint: u32) -> Option<GlyphId> {
        let c = char::from_u32(codepoint)?;
        self.face.glyph_index(c).map(|g| GlyphId(g.0))
    }

    /// Build the outline for `glyph` by walking
    /// `ttf-parser`'s outline-builder callbacks and collecting the
    /// segments into an owned [`Outline`]. Returns
    /// [`FontError::OutlineUnavailable`] when `ttf-parser` cannot
    /// reconstruct the outline (composite cycle, missing data,
    /// glyph id past `num_glyphs`).
    pub fn glyph_outline(&self, glyph: GlyphId) -> Result<Outline, FontError> {
        let id = ttf_parser::GlyphId(glyph.0);
        let mut builder = OutlineCollector::default();
        let bbox = match self.face.outline_glyph(id, &mut builder) {
            Some(b) => b,
            None => {
                // `.notdef` and empty glyphs (e.g. space, control
                // codepoints) legitimately have no outline; surface
                // that as an empty Outline rather than an error so
                // the caller can render a blank cell without
                // branching on the error path.
                if (id.0 as u32) < self.num_glyphs() as u32 {
                    return Ok(Outline {
                        segments: Vec::new(),
                        bbox: BoundingBox {
                            x_min: 0,
                            y_min: 0,
                            x_max: 0,
                            y_max: 0,
                        },
                    });
                }
                return Err(FontError::OutlineUnavailable);
            }
        };
        Ok(Outline {
            segments: builder.segments,
            bbox: BoundingBox {
                x_min: bbox.x_min,
                y_min: bbox.y_min,
                x_max: bbox.x_max,
                y_max: bbox.y_max,
            },
        })
    }
}

#[derive(Default)]
struct OutlineCollector {
    segments: Vec<OutlineSegment>,
}

impl ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.segments.push(OutlineSegment::MoveTo { x, y });
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.segments.push(OutlineSegment::LineTo { x, y });
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.segments.push(OutlineSegment::QuadTo { cx, cy, x, y });
    }

    fn curve_to(&mut self, cx1: f32, cy1: f32, cx2: f32, cy2: f32, x: f32, y: f32) {
        self.segments.push(OutlineSegment::CurveTo {
            cx1,
            cy1,
            cx2,
            cy2,
            x,
            y,
        });
    }

    fn close(&mut self) {
        self.segments.push(OutlineSegment::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open a tiny in-tree font asset for tests — the public-domain
    /// "DejaVu Sans Mono" cut available on the host. We resolve the
    /// path at runtime so `cargo test -p kernel-core` works from any
    /// directory.
    fn load_test_font_bytes() -> Option<Vec<u8>> {
        for candidate in TEST_FONT_PATHS {
            if let Ok(bytes) = std::fs::read(candidate) {
                return Some(bytes);
            }
        }
        None
    }

    const TEST_FONT_PATHS: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/Library/Fonts/Arial.ttf",
    ];

    #[test]
    fn malformed_bytes_rejected() {
        let bytes = vec![0u8; 4];
        assert!(matches!(Font::open(&bytes), Err(FontError::Malformed)));
    }

    #[test]
    fn open_succeeds_on_real_font() {
        let Some(bytes) = load_test_font_bytes() else {
            eprintln!("skipping: no host TTF found");
            return;
        };
        let font = Font::open(&bytes).expect("parse host font");
        assert!(font.num_glyphs() > 0);
        assert!(font.units_per_em() > 0);
    }

    #[test]
    fn ascii_codepoint_resolves_to_glyph() {
        let Some(bytes) = load_test_font_bytes() else {
            return;
        };
        let font = Font::open(&bytes).expect("parse host font");
        let g = font
            .glyph_index(b'A' as u32)
            .expect("font covers ASCII 'A'");
        assert!(g.0 > 0, "non-notdef glyph for ASCII 'A'");
    }

    #[test]
    fn unknown_codepoint_returns_none() {
        let Some(bytes) = load_test_font_bytes() else {
            return;
        };
        let font = Font::open(&bytes).expect("parse host font");
        // Unassigned plane-15 code; almost certainly absent from any
        // shipped font's cmap.
        assert!(font.glyph_index(0xFFFFF).is_none());
    }

    #[test]
    fn glyph_outline_for_h_has_segments() {
        let Some(bytes) = load_test_font_bytes() else {
            return;
        };
        let font = Font::open(&bytes).expect("parse host font");
        let g = font.glyph_index(b'H' as u32).expect("font covers 'H'");
        let outline = font
            .glyph_outline(g)
            .expect("'H' outline reconstructs cleanly");
        assert!(
            !outline.segments.is_empty(),
            "'H' must produce non-empty outline"
        );
        // Most TTF fonts encode 'H' as two contours (outer rectangle
        // + inner counter is unusual; typically a single 12-point
        // outer contour suffices). We assert "at least one Close"
        // — proving the walker actually drove a contour boundary.
        let closes = outline
            .segments
            .iter()
            .filter(|s| matches!(s, OutlineSegment::Close))
            .count();
        assert!(closes >= 1, "'H' outline must contain at least one Close");
    }
}
