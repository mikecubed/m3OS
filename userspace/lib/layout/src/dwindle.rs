//! Phase 72 Track B.3 — Dwindle binary-tree tiling policy.
//!
//! Hyprland's default layout: each new window splits the focused tile
//! alternately horizontal then vertical, producing a "dwindling"
//! progression where every successive window halves the previous
//! tile.
//!
//! Implementation note: the layout maintains a tree-shaped list of
//! ratios (one per internal split) so the relative geometry persists
//! across re-tile calls even as the output rect changes. The split
//! direction at depth `d` alternates: even depths are vertical splits
//! (left | right children), odd depths are horizontal (top / bottom).

extern crate alloc;
use alloc::vec::Vec;

use crate::{
    GapConfig, LayoutError, LayoutPolicy, LayoutSurface, OutputGeometry, Rect, ResizeDirection,
    SurfaceId, TiledLayoutPolicy, TiledWindow, shrink_horizontal, shrink_vertical,
};

const DEFAULT_SPLIT_RATIO: f32 = 0.5;
const MIN_SPLIT_RATIO: f32 = 0.10;
const MAX_SPLIT_RATIO: f32 = 0.90;

/// Dwindle (binary-tree) layout. The first window takes the full
/// area; each subsequent window splits the most recently added tile
/// in half, alternating horizontal then vertical.
#[derive(Clone, Debug, Default)]
pub struct DwindleLayout {
    /// Persistent split ratios — one per internal split (= `n - 1`
    /// ratios for `n` windows). Index `i` corresponds to the split
    /// between the first `i + 1` windows. Ratios are stored relative
    /// to the parent rect along its primary axis.
    split_ratios: Vec<f32>,
    rotate: bool,
}

impl DwindleLayout {
    /// Construct a fresh layout with no persistent splits.
    pub fn new() -> Self {
        Self {
            split_ratios: Vec::new(),
            rotate: false,
        }
    }

    /// Spiral variant: split direction always rotates in the same
    /// direction (i.e. each split goes clockwise). The non-rotating
    /// dwindle drops every new window into the bottom-right corner;
    /// the rotating variant cycles around the centre.
    pub fn spiral() -> Self {
        Self {
            split_ratios: Vec::new(),
            rotate: true,
        }
    }
}

impl TiledLayoutPolicy for DwindleLayout {
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
        // Ensure persistent ratio storage matches the split count.
        while self.split_ratios.len() < n.saturating_sub(1) {
            self.split_ratios.push(DEFAULT_SPLIT_RATIO);
        }
        // Don't shrink storage on window removal — keep the ratios so a
        // close+reopen reuses the user's adjusted geometry.

        let half_inner = gaps.inner / 2;
        // The first window occupies the full output; each subsequent
        // window splits the previous "tail" rect along the current
        // axis. `depth` tracks split depth so we alternate axes.
        let mut tail = output;
        for i in 0..n {
            if i == n - 1 {
                // Last window inherits the remaining tail.
                out.push((windows[i].id, tail));
                break;
            }
            let depth = i;
            let ratio = self
                .split_ratios
                .get(i)
                .copied()
                .unwrap_or(DEFAULT_SPLIT_RATIO);
            // Direction selection. The non-spiral dwindle alternates
            // V/H based on depth parity. The spiral variant rotates
            // through (V-left, H-top, V-right, H-bottom) so each new
            // child slot rotates around the centre.
            let split_v = if self.rotate {
                // Even depth → vertical (left | right); odd depth →
                // horizontal. The orientation flips every other cycle
                // so the new tail rotates around the spiral.
                depth % 2 == 0
            } else {
                depth % 2 == 0
            };
            let new_tail_after_carve;
            let carved;
            if split_v {
                // Vertical split. Carved (current window's tile) is
                // the left half; new tail is the right half.
                let split_w = ((tail.w as f32) * ratio) as u32;
                let left = Rect {
                    x: tail.x,
                    y: tail.y,
                    w: split_w,
                    h: tail.h,
                };
                let right = Rect {
                    x: tail.x.saturating_add(split_w as i32),
                    y: tail.y,
                    w: tail.w.saturating_sub(split_w),
                    h: tail.h,
                };
                if self.rotate && (depth / 2) % 2 == 1 {
                    // Rotate: put the new window on the LEFT, push the
                    // tail to the RIGHT? Actually swap the carve so
                    // we spiral correctly.
                    carved = right;
                    new_tail_after_carve = left;
                } else {
                    carved = left;
                    new_tail_after_carve = right;
                }
            } else {
                // Horizontal split.
                let split_h = ((tail.h as f32) * ratio) as u32;
                let top = Rect {
                    x: tail.x,
                    y: tail.y,
                    w: tail.w,
                    h: split_h,
                };
                let bottom = Rect {
                    x: tail.x,
                    y: tail.y.saturating_add(split_h as i32),
                    w: tail.w,
                    h: tail.h.saturating_sub(split_h),
                };
                if self.rotate && (depth / 2) % 2 == 1 {
                    carved = bottom;
                    new_tail_after_carve = top;
                } else {
                    carved = top;
                    new_tail_after_carve = bottom;
                }
            }
            // Apply inner-gap shrink along the split seam. The
            // carved tile gives up `half_inner` on the split-facing
            // edge; the tail gives up the symmetric `half_inner` on
            // its opposite edge.
            let (carved_g, tail_g) = if split_v {
                let carved_g = shrink_horizontal(carved, half_inner, false, true);
                let tail_g = shrink_horizontal(new_tail_after_carve, half_inner, true, false);
                (carved_g, tail_g)
            } else {
                let carved_g = shrink_vertical(carved, half_inner, false, true);
                let tail_g = shrink_vertical(new_tail_after_carve, half_inner, true, false);
                (carved_g, tail_g)
            };
            out.push((windows[i].id, carved_g));
            tail = tail_g;
        }
        out
    }

    fn adjust_focused(
        &mut self,
        focused: SurfaceId,
        direction: ResizeDirection,
        step: i16,
    ) -> Result<(), LayoutError> {
        // Without the window list cached here we approximate by
        // adjusting the most-recent split (closest to the focused
        // tile). A more sophisticated implementation would walk the
        // tree to find the parent split of the focused tile; Phase 72
        // defers that refinement.
        if self.split_ratios.is_empty() {
            return Err(LayoutError::UnknownWindow);
        }
        let _ = focused;
        let last_idx = self.split_ratios.len() - 1;
        let delta = (step as f32) / 1024.0;
        let delta = match direction {
            ResizeDirection::Right | ResizeDirection::Down => delta,
            ResizeDirection::Left | ResizeDirection::Up => -delta,
        };
        let r = (self.split_ratios[last_idx] + delta).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
        self.split_ratios[last_idx] = r;
        Ok(())
    }

    fn on_window_removed(&mut self, _id: SurfaceId) {
        // Pop the last ratio so removing a window collapses cleanly.
        if !self.split_ratios.is_empty() {
            self.split_ratios.pop();
        }
    }
}

impl LayoutPolicy for DwindleLayout {
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
    fn single_window_fills_output() {
        let mut l = DwindleLayout::new();
        let result = l.tile(&[win(1)], out(1280, 720), GapConfig::zero());
        assert_eq!(result[0].1, out(1280, 720));
    }

    #[test]
    fn two_windows_split_vertically() {
        let mut l = DwindleLayout::new();
        let result = l.tile(&[win(1), win(2)], out(1280, 720), GapConfig::zero());
        // Depth 0 = vertical split, default ratio 0.5.
        assert_eq!(
            result[0].1,
            Rect {
                x: 0,
                y: 0,
                w: 640,
                h: 720
            }
        );
        assert_eq!(
            result[1].1,
            Rect {
                x: 640,
                y: 0,
                w: 640,
                h: 720
            }
        );
    }

    #[test]
    fn four_windows_form_2x2_partition() {
        let mut l = DwindleLayout::new();
        let windows: Vec<TiledWindow> = (1..=4u32).map(win).collect();
        let result = l.tile(&windows, out(1280, 720), GapConfig::zero());
        // Window 1: left half (vertical split).
        assert_eq!(
            result[0].1,
            Rect {
                x: 0,
                y: 0,
                w: 640,
                h: 720
            }
        );
        // Window 2: right half, top quarter (horizontal split).
        assert_eq!(
            result[1].1,
            Rect {
                x: 640,
                y: 0,
                w: 640,
                h: 360
            }
        );
        // Window 3: right half, bottom-left (vertical split).
        assert_eq!(
            result[2].1,
            Rect {
                x: 640,
                y: 360,
                w: 320,
                h: 360
            }
        );
        // Window 4: right half, bottom-right (remaining).
        assert_eq!(
            result[3].1,
            Rect {
                x: 960,
                y: 360,
                w: 320,
                h: 360
            }
        );
        // No overlaps.
        for i in 0..4 {
            for j in (i + 1)..4 {
                assert!(!crate::rects_overlap(result[i].1, result[j].1));
            }
        }
    }

    #[test]
    fn passes_tile_contract_suite() {
        tile_contract_suite(DwindleLayout::new);
    }
}
