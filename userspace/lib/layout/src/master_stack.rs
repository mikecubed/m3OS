//! Phase 72 Track B.2 — Master/Stack tiling policy.
//!
//! Canonical "master + side stack" layout. Window 0 becomes the master
//! and takes `master_ratio` of the output width; subsequent windows
//! stack vertically on the opposite side, each receiving an equal
//! slice of the remaining height. With a single window the master
//! takes the full output rect.

extern crate alloc;
use alloc::vec::Vec;

use crate::{
    GapConfig, LayoutError, LayoutPolicy, LayoutSurface, OutputGeometry, Rect, ResizeDirection,
    SurfaceId, TiledLayoutPolicy, TiledWindow, shrink_horizontal, shrink_vertical,
};

const DEFAULT_MASTER_RATIO: f32 = 0.55;
const MIN_MASTER_RATIO: f32 = 0.10;
const MAX_MASTER_RATIO: f32 = 0.90;

/// Master/Stack tiling policy.
///
/// One master tile on the left, a vertical stack of remaining windows
/// on the right. The split fraction (`master_ratio`) is adjustable at
/// runtime via [`MasterStackLayout::set_master_ratio`] (wired to
/// `m3ctl tile set-master-ratio <f>`) and via
/// [`TiledLayoutPolicy::adjust_focused`] for resize-mode keybinds.
#[derive(Clone, Debug)]
pub struct MasterStackLayout {
    master_ratio: f32,
}

impl Default for MasterStackLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl MasterStackLayout {
    /// Construct a `MasterStackLayout` at the default ratio (0.55).
    pub fn new() -> Self {
        Self {
            master_ratio: DEFAULT_MASTER_RATIO,
        }
    }

    /// Construct a layout at a specific ratio (clamped to
    /// `[MIN_MASTER_RATIO, MAX_MASTER_RATIO]`).
    pub fn with_ratio(ratio: f32) -> Self {
        Self {
            master_ratio: clamp_ratio(ratio),
        }
    }

    /// Replace the active master ratio. Clamped to
    /// `[0.10, 0.90]` so the master / stack always have at least a
    /// 10% slice.
    pub fn set_master_ratio(&mut self, ratio: f32) {
        self.master_ratio = clamp_ratio(ratio);
    }

    /// Currently active master ratio.
    pub fn master_ratio(&self) -> f32 {
        self.master_ratio
    }
}

fn clamp_ratio(ratio: f32) -> f32 {
    if ratio.is_nan() {
        return DEFAULT_MASTER_RATIO;
    }
    if ratio < MIN_MASTER_RATIO {
        MIN_MASTER_RATIO
    } else if ratio > MAX_MASTER_RATIO {
        MAX_MASTER_RATIO
    } else {
        ratio
    }
}

impl TiledLayoutPolicy for MasterStackLayout {
    fn tile(
        &mut self,
        windows: &[TiledWindow],
        output: Rect,
        gaps: GapConfig,
    ) -> Vec<(SurfaceId, Rect)> {
        let mut out = Vec::with_capacity(windows.len());
        match windows.len() {
            0 => return out,
            1 => {
                out.push((windows[0].id, output));
                return out;
            }
            _ => {}
        }

        let half_inner = gaps.inner / 2;
        let master_w = ((output.w as f32) * self.master_ratio) as u32;
        let stack_w = output.w.saturating_sub(master_w);

        // Master tile (left).
        let master_rect = Rect {
            x: output.x,
            y: output.y,
            w: master_w,
            h: output.h,
        };
        let master_rect = shrink_horizontal(master_rect, half_inner, false, true);
        out.push((windows[0].id, master_rect));

        // Stack column (right).
        let stack_origin_x = output.x.saturating_add(master_w as i32);
        let stack_count = (windows.len() - 1) as u32;
        if stack_count == 0 {
            return out;
        }
        let per_h = output.h / stack_count;
        let leftover = output.h % stack_count;
        let mut cur_y = output.y;
        for i in 1..windows.len() {
            // Distribute the integer leftover into the first few tiles
            // so the sum matches output.h exactly.
            let extra = if ((i - 1) as u32) < leftover { 1 } else { 0 };
            let h = per_h + extra;
            let rect = Rect {
                x: stack_origin_x,
                y: cur_y,
                w: stack_w,
                h,
            };
            // Shrink to honour inner gap on the master-side edge and
            // between vertically-adjacent stack neighbours.
            let mut rect = shrink_horizontal(rect, half_inner, true, false);
            let top = i > 1;
            let bottom = i < windows.len() - 1;
            rect = shrink_vertical(rect, half_inner, top, bottom);
            out.push((windows[i].id, rect));
            cur_y = cur_y.saturating_add(h as i32);
        }
        out
    }

    fn adjust_focused(
        &mut self,
        _focused: SurfaceId,
        direction: ResizeDirection,
        step: i16,
    ) -> Result<(), LayoutError> {
        // Master/stack interprets Left/Right as ratio nudges and
        // ignores Up/Down (the stack always fills the available
        // vertical extent evenly).
        let delta_px = step as f32;
        // 1280 px output → ratio_step ≈ delta_px / 1280. Use a
        // synthetic reference width so the step feels consistent
        // across resolutions: 32 px nudge = ~0.025 of the screen.
        const REFERENCE_W: f32 = 1280.0;
        let dr = delta_px / REFERENCE_W;
        match direction {
            ResizeDirection::Right => {
                self.master_ratio = clamp_ratio(self.master_ratio + dr);
                Ok(())
            }
            ResizeDirection::Left => {
                self.master_ratio = clamp_ratio(self.master_ratio - dr);
                Ok(())
            }
            ResizeDirection::Up | ResizeDirection::Down => Err(LayoutError::Unsupported),
        }
    }
}

// Bridge to the legacy [`LayoutPolicy`] trait so the compose loop's
// `arrange()` path can use a master/stack policy without ceremony.
impl LayoutPolicy for MasterStackLayout {
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

    #[test]
    fn one_window_takes_full_output() {
        let mut l = MasterStackLayout::new();
        let ws = [TiledWindow {
            id: SurfaceId(1),
            preferred_size: (0, 0),
        }];
        let result = l.tile(&ws, out(1000, 800), GapConfig::zero());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, out(1000, 800));
    }

    #[test]
    fn two_windows_split_at_master_ratio() {
        let mut l = MasterStackLayout::new();
        let ws = [
            TiledWindow {
                id: SurfaceId(1),
                preferred_size: (0, 0),
            },
            TiledWindow {
                id: SurfaceId(2),
                preferred_size: (0, 0),
            },
        ];
        let result = l.tile(&ws, out(1000, 800), GapConfig::zero());
        let master_w = (1000.0 * 0.55) as u32;
        assert_eq!(
            result[0].1,
            Rect {
                x: 0,
                y: 0,
                w: master_w,
                h: 800
            }
        );
        assert_eq!(
            result[1].1,
            Rect {
                x: master_w as i32,
                y: 0,
                w: 1000 - master_w,
                h: 800
            }
        );
    }

    #[test]
    fn three_windows_stack_evenly() {
        let mut l = MasterStackLayout::new();
        let ws: Vec<TiledWindow> = (1..=3u32)
            .map(|i| TiledWindow {
                id: SurfaceId(i),
                preferred_size: (0, 0),
            })
            .collect();
        let result = l.tile(&ws, out(1000, 800), GapConfig::zero());
        // Master full height.
        assert_eq!(result[0].1.h, 800);
        // Stack tiles split 800 evenly (400 each).
        assert_eq!(result[1].1.h, 400);
        assert_eq!(result[2].1.h, 400);
        assert_eq!(result[1].1.y, 0);
        assert_eq!(result[2].1.y, 400);
    }

    #[test]
    fn set_master_ratio_clamps() {
        let mut l = MasterStackLayout::new();
        l.set_master_ratio(1.5);
        assert!(l.master_ratio() <= MAX_MASTER_RATIO + f32::EPSILON);
        l.set_master_ratio(-0.5);
        assert!(l.master_ratio() >= MIN_MASTER_RATIO - f32::EPSILON);
    }

    #[test]
    fn adjust_focused_right_increases_ratio() {
        let mut l = MasterStackLayout::new();
        let start = l.master_ratio();
        l.adjust_focused(SurfaceId(1), ResizeDirection::Right, 64)
            .unwrap();
        assert!(l.master_ratio() > start);
    }

    #[test]
    fn adjust_focused_up_unsupported() {
        let mut l = MasterStackLayout::new();
        assert_eq!(
            l.adjust_focused(SurfaceId(1), ResizeDirection::Up, 32),
            Err(LayoutError::Unsupported)
        );
    }

    #[test]
    fn passes_tile_contract_suite() {
        tile_contract_suite(MasterStackLayout::new);
    }
}
