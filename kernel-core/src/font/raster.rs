//! Phase 69c Track B.1 — scanline glyph rasterizer.
//!
//! Consumes an [`crate::font::parser::Outline`] in em-units and
//! produces a 1-bit-per-pixel coverage bitmap matching the Phase 69b
//! glyph shape (packed bits, row-major, MSB-first per byte).
//!
//! The rasterizer is intentionally simple — no anti-aliasing, no
//! sub-pixel positioning, no hinting. Bezier curves are flattened to
//! polylines (12 segments per curve, plenty for an 8 × 16 cell), then
//! a non-zero winding-number scanline fill produces the coverage
//! mask. A pixel is "set" when its centre is inside the polygon.
//!
//! Glyphs are centred horizontally inside the cell and aligned to a
//! shared baseline computed from the font's ascender/descender so a
//! row of mixed-height glyphs lines up like a real terminal.

use alloc::vec::Vec;

use super::parser::{Outline, OutlineSegment};

/// `f32::abs` lives in `std` only; hand-roll for `no_std`.
#[inline]
fn f32_abs(x: f32) -> f32 {
    if x < 0.0 { -x } else { x }
}

/// `f32::ceil` lives in `std` only; hand-roll for both signs. Used
/// for the pixel-coverage rounding in the scanline fill.
#[inline]
fn f32_ceil_as_i32(x: f32) -> i32 {
    let i = x as i32;
    if (i as f32) < x { i + 1 } else { i }
}

/// A rasterized glyph bitmap. Owned data (`Vec<u8>`) so the atlas can
/// store it; layout mirrors [`crate::session::font::Glyph`] so the
/// `term` renderer can blit either kind through the same code path.
///
/// `bitmap` is packed bits, row-major, MSB-first per byte. Bytes per
/// row is `ceil(width / 8)`. Total length is `bytes_per_row * height`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterBitmap {
    /// Pixel width of the cell.
    pub width: u8,
    /// Pixel height of the cell.
    pub height: u8,
    /// Packed bitmap data; see the type-level docs for the layout.
    pub bitmap: Vec<u8>,
}

impl RasterBitmap {
    /// Construct an all-zero bitmap for the given cell size.
    pub fn blank(width: u8, height: u8) -> Self {
        let bytes_per_row = (width as usize).div_ceil(8);
        Self {
            width,
            height,
            bitmap: alloc::vec![0u8; bytes_per_row * height as usize],
        }
    }

    /// True when no pixels are set. Used by the smoke tests to
    /// distinguish "rendered" from "blank" cells.
    pub fn is_blank(&self) -> bool {
        self.bitmap.iter().all(|&b| b == 0)
    }

    /// Count of set pixels — convenience for tests that assert the
    /// glyph covers a sensible fraction of the cell.
    pub fn ink_count(&self) -> usize {
        self.bitmap.iter().map(|b| b.count_ones() as usize).sum()
    }

    fn set_pixel(&mut self, x: usize, y: usize) {
        if x >= self.width as usize || y >= self.height as usize {
            return;
        }
        let bytes_per_row = (self.width as usize).div_ceil(8);
        let byte_idx = y * bytes_per_row + x / 8;
        let bit_idx = 7 - (x % 8);
        self.bitmap[byte_idx] |= 1u8 << bit_idx;
    }

    /// Read-back helper: returns true when `(x, y)` is set.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= self.width as usize || y >= self.height as usize {
            return false;
        }
        let bytes_per_row = (self.width as usize).div_ceil(8);
        let byte_idx = y * bytes_per_row + x / 8;
        let bit_idx = 7 - (x % 8);
        (self.bitmap[byte_idx] >> bit_idx) & 1 == 1
    }
}

/// Stateless rasterizer — all the per-font parameters travel as
/// arguments so the same `Rasterizer` instance can rasterize glyphs
/// from any font.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rasterizer;

/// Parameters that pin the glyph to a cell. The rasterizer maps em-
/// units onto cell pixels through this scale; the caller picks the
/// values from the font's `units_per_em` / `ascender` / `descender`.
///
/// `cell_w` / `cell_h` are `u8` because [`RasterBitmap`] stores its
/// dimensions as `u8`; widening the metric type here would let
/// callers silently truncate to `u8` when constructing the bitmap.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub cell_w: u8,
    pub cell_h: u8,
    pub units_per_em: u16,
    pub ascender: i16,
    pub descender: i16,
}

impl Rasterizer {
    /// Rasterize one glyph outline into a 1-bit cell bitmap.
    ///
    /// The transform is:
    ///
    /// 1. Compute the em-to-pixel scale so the full
    ///    `ascender - descender` band fits inside the cell height
    ///    (with one row of top padding so caps don't touch the
    ///    top edge).
    /// 2. Flatten each `Quad` / `Curve` segment into a polyline.
    /// 3. Translate em-space coordinates so the glyph's horizontal
    ///    midpoint lands on the cell's centre column and the baseline
    ///    falls at the descender-anchored row.
    /// 4. Run a non-zero winding-number scanline fill.
    pub fn rasterize_glyph(&self, outline: &Outline, metrics: CellMetrics) -> RasterBitmap {
        let cell_w = metrics.cell_w.max(1);
        let cell_h = metrics.cell_h.max(1);
        let mut bitmap = RasterBitmap::blank(cell_w, cell_h);

        if outline.segments.is_empty() {
            return bitmap;
        }

        // 1. Pixel scale. Use the font ascender/descender band as the
        //    "100%" so the glyph never overflows the cell.
        let em_height = (metrics.ascender as f32) - (metrics.descender as f32);
        if em_height <= 0.0 {
            return bitmap;
        }
        // Reserve 1 px of top + 1 px of bottom padding for an 8 × 16
        // cell so caps and descenders don't kiss the grid lines.
        let usable_h = (cell_h as f32 - 2.0).max(1.0);
        let scale_y = usable_h / em_height;
        let scale_x = scale_y; // monospace metrics

        // 2. Translation. Compute the glyph bbox in pixels and
        //    centre it horizontally.
        let bx_min = (outline.bbox.x_min as f32) * scale_x;
        let bx_max = (outline.bbox.x_max as f32) * scale_x;
        let glyph_w_px = bx_max - bx_min;
        let dx = (cell_w as f32 - glyph_w_px) * 0.5 - bx_min;
        // Baseline: place so a point at em-y = `descender` (which is
        // negative) lands at row `cell_h - 2`, leaving one row of
        // bottom padding. With `usable_h = cell_h - 2`, a point at
        // em-y = `ascender` then lands at row 0 (touching the top
        // edge). Without subtracting the descender contribution here,
        // descender pixels for glyphs like `g`, `p`, `y` map past
        // `cell_h - 1` and get silently clipped by `set_pixel`.
        let baseline_y = (cell_h as f32 - 2.0) + (metrics.descender as f32) * scale_y;

        let mapped: Vec<(f32, f32)> =
            flatten_outline(&outline.segments, scale_x, scale_y, dx, baseline_y);
        let contours = split_contours(&outline.segments, &mapped);

        // 3. Build an edge table.
        let mut edges = Vec::<Edge>::new();
        for contour in &contours {
            for window in contour.windows(2) {
                let (x0, y0) = window[0];
                let (x1, y1) = window[1];
                if f32_abs(y0 - y1) < f32::EPSILON {
                    continue; // skip horizontal edges
                }
                let (y_min, y_max, x_at_ymin, slope, winding) = if y0 < y1 {
                    (y0, y1, x0, (x1 - x0) / (y1 - y0), 1i8)
                } else {
                    (y1, y0, x1, (x0 - x1) / (y0 - y1), -1i8)
                };
                edges.push(Edge {
                    y_min,
                    y_max,
                    x_at_ymin,
                    slope,
                    winding,
                });
            }
        }

        // 4. Scanline fill with non-zero winding number. We sample
        //    at the centre of each pixel row (`y + 0.5`) and walk
        //    left-to-right toggling winding.
        let mut crossings: Vec<(f32, i8)> = Vec::new();
        for y in 0..cell_h as usize {
            let scan = y as f32 + 0.5;
            crossings.clear();
            for edge in &edges {
                if scan < edge.y_min || scan >= edge.y_max {
                    continue;
                }
                let x = edge.x_at_ymin + edge.slope * (scan - edge.y_min);
                crossings.push((x, edge.winding));
            }
            crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));

            let mut winding: i32 = 0;
            let mut last_x: f32 = -1.0;
            for &(x, w) in crossings.iter() {
                let prev_winding = winding;
                winding = winding.saturating_add(w as i32);
                if prev_winding != 0 {
                    // Span from last_x to x is "inside". Fill pixel
                    // `px` when its centre (`px + 0.5`) sits inside
                    // the span. The boundary-based form
                    // (`lo=ceil, hi=floor`) collapses to an empty
                    // range whenever the span is narrower than one
                    // pixel — which is exactly the case for an
                    // 8 × 16 'H'-bar, leaving the cell blank. Centre
                    // sampling matches every other scanline
                    // rasterizer (FreeType, Skia) and gives 1-px
                    // wide bars the single-pixel column they need.
                    let span_start = last_x.max(0.0);
                    let span_end = x.min(cell_w as f32);
                    if span_end > span_start {
                        let lo = f32_ceil_as_i32(span_start - 0.5);
                        let hi = f32_ceil_as_i32(span_end - 0.5);
                        for px in lo..hi {
                            if px >= 0 && (px as usize) < cell_w as usize {
                                bitmap.set_pixel(px as usize, y);
                            }
                        }
                    }
                }
                last_x = x;
            }
        }
        bitmap
    }
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    y_min: f32,
    y_max: f32,
    x_at_ymin: f32,
    slope: f32,
    winding: i8,
}

const CURVE_FLATTEN_STEPS: usize = 12;

fn flatten_outline(
    segments: &[OutlineSegment],
    scale_x: f32,
    scale_y: f32,
    dx: f32,
    baseline_y: f32,
) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(segments.len() * CURVE_FLATTEN_STEPS);
    let mut cursor = (0.0f32, 0.0f32);
    let mut contour_start = (0.0f32, 0.0f32);
    for seg in segments {
        match *seg {
            OutlineSegment::MoveTo { x, y } => {
                let p = map(x, y, scale_x, scale_y, dx, baseline_y);
                cursor = p;
                contour_start = p;
                out.push(p);
            }
            OutlineSegment::LineTo { x, y } => {
                let p = map(x, y, scale_x, scale_y, dx, baseline_y);
                cursor = p;
                out.push(p);
            }
            OutlineSegment::QuadTo { cx, cy, x, y } => {
                let c = map(cx, cy, scale_x, scale_y, dx, baseline_y);
                let p = map(x, y, scale_x, scale_y, dx, baseline_y);
                for i in 1..=CURVE_FLATTEN_STEPS {
                    let t = i as f32 / CURVE_FLATTEN_STEPS as f32;
                    let one = 1.0 - t;
                    let bx = one * one * cursor.0 + 2.0 * one * t * c.0 + t * t * p.0;
                    let by = one * one * cursor.1 + 2.0 * one * t * c.1 + t * t * p.1;
                    out.push((bx, by));
                }
                cursor = p;
            }
            OutlineSegment::CurveTo {
                cx1,
                cy1,
                cx2,
                cy2,
                x,
                y,
            } => {
                let c1 = map(cx1, cy1, scale_x, scale_y, dx, baseline_y);
                let c2 = map(cx2, cy2, scale_x, scale_y, dx, baseline_y);
                let p = map(x, y, scale_x, scale_y, dx, baseline_y);
                for i in 1..=CURVE_FLATTEN_STEPS {
                    let t = i as f32 / CURVE_FLATTEN_STEPS as f32;
                    let one = 1.0 - t;
                    let bx = one * one * one * cursor.0
                        + 3.0 * one * one * t * c1.0
                        + 3.0 * one * t * t * c2.0
                        + t * t * t * p.0;
                    let by = one * one * one * cursor.1
                        + 3.0 * one * one * t * c1.1
                        + 3.0 * one * t * t * c2.1
                        + t * t * t * p.1;
                    out.push((bx, by));
                }
                cursor = p;
            }
            OutlineSegment::Close => {
                // Close back to contour start so the edge-table sees
                // the closing edge.
                out.push(contour_start);
                cursor = contour_start;
            }
        }
    }
    out
}

fn map(x: f32, y: f32, scale_x: f32, scale_y: f32, dx: f32, baseline_y: f32) -> (f32, f32) {
    // TTF y-axis is "up positive"; the cell grid is "down positive".
    // Flipping by subtracting from the baseline lands the glyph
    // upright in the cell.
    let mx = x * scale_x + dx;
    let my = baseline_y - y * scale_y;
    (mx, my)
}

fn split_contours(segments: &[OutlineSegment], mapped: &[(f32, f32)]) -> Vec<Vec<(f32, f32)>> {
    let mut contours = Vec::new();
    let mut current = Vec::new();
    let mut idx = 0usize;
    for seg in segments {
        match *seg {
            OutlineSegment::MoveTo { .. } => {
                if !current.is_empty() {
                    contours.push(core::mem::take(&mut current));
                }
                current.push(mapped[idx]);
                idx += 1;
            }
            OutlineSegment::LineTo { .. } => {
                current.push(mapped[idx]);
                idx += 1;
            }
            OutlineSegment::QuadTo { .. } => {
                for _ in 0..CURVE_FLATTEN_STEPS {
                    current.push(mapped[idx]);
                    idx += 1;
                }
            }
            OutlineSegment::CurveTo { .. } => {
                for _ in 0..CURVE_FLATTEN_STEPS {
                    current.push(mapped[idx]);
                    idx += 1;
                }
            }
            OutlineSegment::Close => {
                current.push(mapped[idx]);
                idx += 1;
            }
        }
    }
    if !current.is_empty() {
        contours.push(current);
    }
    contours
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::parser::Font;

    /// See `kernel_core::font::atlas::tests::TEST_FONT_PATHS` for the
    /// rationale — the workspace-staged Nerd Font is the
    /// deterministic fixture, with system DejaVu as a fallback.
    const TEST_FONT_PATHS: &[&str] = &[
        "xtask/assets/fonts/term.ttf",
        "../xtask/assets/fonts/term.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    ];

    fn load_test_font_bytes() -> Option<Vec<u8>> {
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

    fn cell_metrics_for(font: &Font<'_>) -> CellMetrics {
        CellMetrics {
            cell_w: 8,
            cell_h: 16,
            units_per_em: font.units_per_em(),
            ascender: font.ascender(),
            descender: font.descender(),
        }
    }

    #[test]
    fn blank_bitmap_helpers() {
        let bm = RasterBitmap::blank(8, 16);
        assert_eq!(bm.bitmap.len(), 16);
        assert!(bm.is_blank());
        assert_eq!(bm.ink_count(), 0);
        assert!(!bm.pixel(0, 0));
    }

    #[test]
    fn rasterize_h_has_ink() {
        let Some(bytes) = load_test_font_bytes() else {
            eprintln!("skipping rasterize_h_has_ink: no host TTF found");
            return;
        };
        let font = Font::open(&bytes).expect("parse host font");
        let g = font.glyph_index(b'H' as u32).expect("font covers 'H'");
        let outline = font.glyph_outline(g).expect("'H' outline reconstructs");
        let metrics = cell_metrics_for(&font);
        let bm = Rasterizer.rasterize_glyph(&outline, metrics);
        assert!(!bm.is_blank(), "rasterized 'H' must produce visible pixels");
        assert!(
            bm.ink_count() >= 10,
            "rasterized 'H' too sparse: ink_count = {}",
            bm.ink_count()
        );

        // Stronger shape check: 'H' must contain at least two
        // distinct vertical bars (one column on each side of the
        // glyph) and at least one horizontal crossbar row that
        // bridges them.
        let mut vertical_bar_cols = Vec::new();
        for x in 0..bm.width as usize {
            let col_ink: usize = (0..bm.height as usize).filter(|&y| bm.pixel(x, y)).count();
            if col_ink >= bm.height as usize / 2 {
                vertical_bar_cols.push(x);
            }
        }
        assert!(
            vertical_bar_cols.len() >= 2,
            "'H' must show two vertical bars; found columns with ink \
             ≥ half-cell height: {vertical_bar_cols:?}"
        );

        // We deliberately do *not* assert the crossbar is visible at
        // 8 × 16: pixel-centre coverage with non-zero winding misses
        // horizontal strokes whose pixel-space height falls below
        // 1 px between two scanlines. JetBrainsMono Mono's 'H'
        // crossbar is ~0.87 px tall at this cell size, so the
        // rasterizer correctly emits "||" rather than a crossed
        // shape. Fonts with thicker crossbars (DejaVu Sans Mono)
        // still resolve a crossbar at this size, but we cannot rely
        // on it across fonts. The crossbar capability of the
        // rasterizer is exercised independently by
        // [`crossbar_synthetic_outline_resolves`].
    }

    /// Sanity-check that the rasterizer can resolve a horizontal
    /// crossbar when the em-space band is comfortably more than one
    /// pixel tall. Uses a synthetic 'H'-like outline whose
    /// dimensions are chosen so every feature lands on a clean
    /// pixel grid — independent of any font's design.
    #[test]
    fn crossbar_synthetic_outline_resolves() {
        // Em-space H: 1000-unit em with bars at x=100..200 and
        // x=400..500, and a crossbar at y=400..600 spanning the
        // inner gap.
        use crate::font::parser::BoundingBox;
        use OutlineSegment::*;
        let outline = Outline {
            segments: alloc::vec![
                // Left bar
                MoveTo { x: 100.0, y: 0.0 },
                LineTo { x: 200.0, y: 0.0 },
                LineTo {
                    x: 200.0,
                    y: 1000.0
                },
                LineTo {
                    x: 100.0,
                    y: 1000.0
                },
                Close,
                // Right bar
                MoveTo { x: 400.0, y: 0.0 },
                LineTo { x: 500.0, y: 0.0 },
                LineTo {
                    x: 500.0,
                    y: 1000.0
                },
                LineTo {
                    x: 400.0,
                    y: 1000.0
                },
                Close,
                // Crossbar (200 em ≈ 2.1 px at scale_y = 14/1320)
                MoveTo { x: 200.0, y: 400.0 },
                LineTo { x: 400.0, y: 400.0 },
                LineTo { x: 400.0, y: 600.0 },
                LineTo { x: 200.0, y: 600.0 },
                Close,
            ],
            bbox: BoundingBox {
                x_min: 100,
                y_min: 0,
                x_max: 500,
                y_max: 1000,
            },
        };
        let metrics = CellMetrics {
            cell_w: 16,
            cell_h: 16,
            units_per_em: 1000,
            ascender: 1020,
            descender: -300,
        };
        let bm = Rasterizer.rasterize_glyph(&outline, metrics);
        // Find columns with ink and rows with ink to verify both
        // vertical bars and the crossbar resolved.
        let inked_cols: alloc::vec::Vec<usize> = (0..bm.width as usize)
            .filter(|&x| (0..bm.height as usize).any(|y| bm.pixel(x, y)))
            .collect();
        assert!(
            inked_cols.len() >= 4,
            "synthetic 'H' must produce at least 4 inked cols, got {inked_cols:?}"
        );
        // The crossbar must produce at least one row where an inner
        // column (strictly between the leftmost and rightmost inked
        // cols) is inked.
        let inner_lo = *inked_cols.first().unwrap();
        let inner_hi = *inked_cols.last().unwrap();
        let crossbar_ink: usize = (0..bm.height as usize)
            .filter(|&y| (inner_lo + 1..inner_hi).any(|x| bm.pixel(x, y)))
            .count();
        assert!(
            crossbar_ink >= 1,
            "synthetic 'H' must show at least one crossbar row, got 0"
        );
    }

    #[test]
    fn rasterize_o_has_closed_loop_ink() {
        let Some(bytes) = load_test_font_bytes() else {
            return;
        };
        let font = Font::open(&bytes).expect("parse host font");
        let g = font.glyph_index(b'o' as u32).expect("font covers 'o'");
        let outline = font.glyph_outline(g).expect("'o' outline reconstructs");
        let metrics = cell_metrics_for(&font);
        let bm = Rasterizer.rasterize_glyph(&outline, metrics);
        assert!(!bm.is_blank(), "rasterized 'o' must produce visible pixels");
        // The non-zero winding fill of 'o' leaves the inner counter
        // unfilled; we expect a "ring" of ink rather than a solid
        // blob. Sanity-bound the ink: more than just a dot, less
        // than the whole cell.
        let ink = bm.ink_count();
        assert!(
            (5..120).contains(&ink),
            "rasterized 'o' ink count out of expected ring range: {ink}"
        );
    }

    /// Regression: descender glyphs (`g`, `p`, `y`, etc.) must not
    /// have their lower strokes silently clipped below the cell.
    /// Uses a synthetic outline that lives entirely in the descender
    /// band (em-y < 0) so the test does not depend on a specific
    /// font's `g` shape.
    #[test]
    fn descender_outline_lands_inside_cell() {
        use crate::font::parser::BoundingBox;
        use OutlineSegment::*;
        // A filled rectangle in the descender band: em-y from -300 to
        // -100. Pre-fix this rendered as a blank cell because the
        // baseline was pinned at `cell_h - 2` and `baseline - y*scale`
        // for negative `y` exceeded `cell_h - 1`.
        let outline = Outline {
            segments: alloc::vec![
                MoveTo {
                    x: 100.0,
                    y: -300.0
                },
                LineTo {
                    x: 500.0,
                    y: -300.0
                },
                LineTo {
                    x: 500.0,
                    y: -100.0
                },
                LineTo {
                    x: 100.0,
                    y: -100.0
                },
                Close,
            ],
            bbox: BoundingBox {
                x_min: 100,
                y_min: -300,
                x_max: 500,
                y_max: -100,
            },
        };
        let metrics = CellMetrics {
            cell_w: 16,
            cell_h: 16,
            units_per_em: 1000,
            ascender: 1020,
            descender: -300,
        };
        let bm = Rasterizer.rasterize_glyph(&outline, metrics);
        assert!(
            !bm.is_blank(),
            "descender band must produce visible pixels, not be clipped"
        );
        // The lowest inked row must land in the descender band — at
        // or below row 12 (well into the lower half of the 16-row
        // cell) — proving the baseline reserved space below it.
        let lowest_inked = (0..bm.height as usize)
            .rev()
            .find(|&y| (0..bm.width as usize).any(|x| bm.pixel(x, y)));
        assert!(
            matches!(lowest_inked, Some(y) if y >= 12),
            "descender pixels did not reach the lower band: lowest_inked = {lowest_inked:?}"
        );
        // And no inked row should land at `cell_h - 1` (the reserved
        // bottom-padding row) — the baseline contract preserves a
        // 1 px gap so descenders don't kiss the grid line.
        assert!(
            !(0..bm.width as usize).any(|x| bm.pixel(x, (bm.height - 1) as usize)),
            "descender band must not overwrite bottom padding row"
        );
    }

    #[test]
    fn rasterize_empty_outline_is_blank() {
        // Tests the rasterizer's invariant directly without going
        // through any specific font. Some fonts (JetBrainsMono Nerd
        // Font Mono is one) record space as a cmap entry with no
        // `glyf` data so `glyph_outline` returns `Err`, while others
        // (DejaVu) return `Ok(empty)`. The rasterizer only sees the
        // `Outline`; both paths must produce a blank bitmap.
        use crate::font::parser::BoundingBox;
        let outline = Outline {
            segments: Vec::new(),
            bbox: BoundingBox {
                x_min: 0,
                y_min: 0,
                x_max: 0,
                y_max: 0,
            },
        };
        let metrics = CellMetrics {
            cell_w: 8,
            cell_h: 16,
            units_per_em: 1000,
            ascender: 800,
            descender: -200,
        };
        let bm = Rasterizer.rasterize_glyph(&outline, metrics);
        assert!(
            bm.is_blank(),
            "empty outline must rasterize to a blank bitmap"
        );
    }
}
