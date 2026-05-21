//! Phase 72 Track B.4 — Grid tiling policy.
//!
//! Partitions N windows into a `ceil(sqrt(N))×floor(sqrt(N))` grid
//! (more columns than rows when N is not a perfect square). Windows
//! are placed left-to-right, top-to-bottom. The grid never overlaps
//! itself and fully tiles the output area for square counts.

extern crate alloc;
use alloc::vec::Vec;

use crate::{
    GapConfig, LayoutPolicy, LayoutSurface, OutputGeometry, Rect, SurfaceId, TiledLayoutPolicy,
    TiledWindow, shrink_horizontal, shrink_vertical,
};

#[derive(Clone, Debug, Default)]
pub struct GridLayout {}

impl GridLayout {
    pub fn new() -> Self {
        Self {}
    }
}

fn isqrt(n: usize) -> usize {
    // Cheap integer square root for the small n encountered here.
    let mut r = 0usize;
    while (r + 1) * (r + 1) <= n {
        r += 1;
    }
    r
}

impl TiledLayoutPolicy for GridLayout {
    fn tile(
        &mut self,
        windows: &[TiledWindow],
        output: Rect,
        gaps: GapConfig,
    ) -> Vec<(SurfaceId, Rect)> {
        let n = windows.len();
        let mut out = Vec::with_capacity(n);
        if n == 0 {
            return out;
        }
        if n == 1 {
            out.push((windows[0].id, output));
            return out;
        }
        let rows = isqrt(n);
        let cols = n.div_ceil(rows);
        let cols = cols.max(1);
        let rows = rows.max(1);

        let half_inner = gaps.inner / 2;
        let cell_w = output.w / cols as u32;
        let cell_h = output.h / rows as u32;
        // Distribute the integer remainder across the first few cols/rows so
        // the total tiled area matches `output` precisely.
        let extra_w = output.w % cols as u32;
        let extra_h = output.h % rows as u32;

        // Pre-compute per-column widths and per-row heights.
        let col_widths: Vec<u32> = (0..cols as u32)
            .map(|c| cell_w + if c < extra_w { 1 } else { 0 })
            .collect();
        let row_heights: Vec<u32> = (0..rows as u32)
            .map(|r| cell_h + if r < extra_h { 1 } else { 0 })
            .collect();
        // Prefix sums for x/y origins.
        let mut col_origins: Vec<i32> = Vec::with_capacity(cols + 1);
        col_origins.push(output.x);
        for w in &col_widths {
            let last = *col_origins.last().unwrap();
            col_origins.push(last.saturating_add(*w as i32));
        }
        let mut row_origins: Vec<i32> = Vec::with_capacity(rows + 1);
        row_origins.push(output.y);
        for h in &row_heights {
            let last = *row_origins.last().unwrap();
            row_origins.push(last.saturating_add(*h as i32));
        }

        for (i, window) in windows.iter().enumerate() {
            let row = i / cols;
            let col = i % cols;
            let mut rect = Rect {
                x: col_origins[col],
                y: row_origins[row],
                w: col_widths[col],
                h: row_heights[row],
            };
            // Inner gaps: shrink against any neighbour that exists.
            let left = col > 0;
            let right = col + 1 < cols && (row * cols + col + 1) < n;
            let top = row > 0;
            let bottom = row + 1 < rows && ((row + 1) * cols + col) < n;
            rect = shrink_horizontal(rect, half_inner, left, right);
            rect = shrink_vertical(rect, half_inner, top, bottom);
            out.push((window.id, rect));
        }
        out
    }
}

impl LayoutPolicy for GridLayout {
    fn arrange(
        &mut self,
        toplevels: &[LayoutSurface],
        output: OutputGeometry,
        _exclusive_zones: &[Rect],
    ) -> Vec<(SurfaceId, Rect)> {
        let windows: Vec<TiledWindow> = toplevels.iter().copied().map(Into::into).collect();
        self.tile(&windows, output.rect, GapConfig::zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tile_contract_suite;

    fn out(w: u32, h: u32) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    fn win(id: u32) -> TiledWindow {
        TiledWindow {
            id: SurfaceId(id),
            preferred_size: (0, 0),
        }
    }

    #[test]
    fn four_windows_form_2x2_grid() {
        let mut l = GridLayout::new();
        let result = l.tile(
            &[win(1), win(2), win(3), win(4)],
            out(1000, 800),
            GapConfig::zero(),
        );
        // rows = floor(sqrt(4)) = 2, cols = ceil(4/2) = 2.
        assert_eq!(
            result[0].1,
            Rect {
                x: 0,
                y: 0,
                w: 500,
                h: 400
            }
        );
        assert_eq!(
            result[1].1,
            Rect {
                x: 500,
                y: 0,
                w: 500,
                h: 400
            }
        );
        assert_eq!(
            result[2].1,
            Rect {
                x: 0,
                y: 400,
                w: 500,
                h: 400
            }
        );
        assert_eq!(
            result[3].1,
            Rect {
                x: 500,
                y: 400,
                w: 500,
                h: 400
            }
        );
    }

    #[test]
    fn passes_tile_contract_suite() {
        tile_contract_suite(GridLayout::new);
    }
}
