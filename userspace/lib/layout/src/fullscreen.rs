//! Phase 72 Track B.4 — Fullscreen-toggle layout policy.
//!
//! The focused window covers the full output; every other window
//! receives a zero-size rect (so the compositor skips painting it).
//! This is the "Tier-3-style" omarchy fullscreen-toggle behaviour:
//! `m3ctl tile fullscreen` switches the workspace's layout policy to
//! this one to maximize the focused tile.

extern crate alloc;
use alloc::vec::Vec;

use crate::{
    GapConfig, LayoutPolicy, LayoutSurface, OutputGeometry, Rect, SurfaceId, TiledLayoutPolicy,
    TiledWindow,
};

#[derive(Clone, Debug, Default)]
pub struct FullscreenLayout {
    focused: Option<SurfaceId>,
}

impl FullscreenLayout {
    pub fn new() -> Self {
        Self { focused: None }
    }

    pub fn set_focused(&mut self, id: Option<SurfaceId>) {
        self.focused = id;
    }

    pub fn focused(&self) -> Option<SurfaceId> {
        self.focused
    }
}

impl TiledLayoutPolicy for FullscreenLayout {
    fn tile(
        &mut self,
        windows: &[TiledWindow],
        output: Rect,
        _gaps: GapConfig,
    ) -> Vec<(SurfaceId, Rect)> {
        let mut out = Vec::with_capacity(windows.len());
        if windows.is_empty() {
            return out;
        }
        let focused = self
            .focused
            .filter(|id| windows.iter().any(|w| w.id == *id))
            .unwrap_or(windows[0].id);
        for w in windows {
            let rect = if w.id == focused {
                output
            } else {
                Rect {
                    x: output.x,
                    y: output.y,
                    w: 0,
                    h: 0,
                }
            };
            out.push((w.id, rect));
        }
        out
    }

    fn on_focus_changed(&mut self, id: Option<SurfaceId>) {
        if id.is_some() {
            self.focused = id;
        }
    }
}

impl LayoutPolicy for FullscreenLayout {
    fn arrange(
        &mut self,
        toplevels: &[LayoutSurface],
        output: OutputGeometry,
        _exclusive_zones: &[Rect],
    ) -> Vec<(SurfaceId, Rect)> {
        let windows: Vec<TiledWindow> = toplevels.iter().copied().map(Into::into).collect();
        self.tile(&windows, output.rect, GapConfig::zero())
    }

    fn focus_affects_geometry(&self) -> bool {
        true
    }

    fn on_focus_changed(&mut self, surface: Option<SurfaceId>) {
        TiledLayoutPolicy::on_focus_changed(self, surface);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tile_contract_suite;

    fn win(id: u32) -> TiledWindow {
        TiledWindow {
            id: SurfaceId(id),
            preferred_size: (0, 0),
        }
    }

    #[test]
    fn focused_takes_full_output() {
        let mut l = FullscreenLayout::new();
        let out_rect = Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 720,
        };
        let result = l.tile(&[win(1), win(2)], out_rect, GapConfig::zero());
        assert_eq!(result[0].1, out_rect);
        assert_eq!(result[1].1.w, 0);
    }

    #[test]
    fn passes_tile_contract_suite() {
        tile_contract_suite(FullscreenLayout::new);
    }
}
