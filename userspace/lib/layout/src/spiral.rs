//! Phase 72 Track B.4 — Spiral tiling policy.
//!
//! Variant of [`crate::dwindle::DwindleLayout`] that rotates the
//! split direction so each new window spirals around the centre. The
//! geometry is otherwise identical: ratios persist across re-tile
//! calls, and resize-mode adjusts the deepest split.

extern crate alloc;

pub use crate::dwindle::DwindleLayout;

/// Alias so callers can spell `SpiralLayout` even though the
/// implementation is a single-flag variant of `DwindleLayout`.
pub type SpiralLayout = DwindleLayout;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Rect, SurfaceId, TiledLayoutPolicy, TiledWindow, tile_contract_suite};

    fn win(id: u32) -> TiledWindow {
        TiledWindow {
            id: SurfaceId(id),
            preferred_size: (0, 0),
        }
    }

    #[test]
    fn spiral_two_windows_split_vertically() {
        let mut l = SpiralLayout::spiral();
        let result = l.tile(
            &[win(1), win(2)],
            Rect {
                x: 0,
                y: 0,
                w: 1280,
                h: 720,
            },
            crate::GapConfig::zero(),
        );
        assert_eq!(result[0].1.w, 640);
        assert_eq!(result[1].1.w, 640);
    }

    #[test]
    fn passes_tile_contract_suite() {
        tile_contract_suite(SpiralLayout::spiral);
    }
}
