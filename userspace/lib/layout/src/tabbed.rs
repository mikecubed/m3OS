//! Phase 72 Track B.4 — Tabbed tiling policy.
//!
//! All windows occupy the same rectangle; only the focused tile is
//! visible. Unfocused windows receive a zero-size rectangle so the
//! compositor's blit path skips them. The "tab strip" metadata that
//! a status bar would render lives on
//! [`TabbedLayout::focused`] / [`TabbedLayout::set_focused`].

extern crate alloc;
use alloc::vec::Vec;

use crate::{
    GapConfig, LayoutPolicy, LayoutSurface, OutputGeometry, Rect, SurfaceId, TiledLayoutPolicy,
    TiledWindow,
};

#[derive(Clone, Debug, Default)]
pub struct TabbedLayout {
    focused: Option<SurfaceId>,
}

impl TabbedLayout {
    pub fn new() -> Self {
        Self { focused: None }
    }

    pub fn focused(&self) -> Option<SurfaceId> {
        self.focused
    }

    pub fn set_focused(&mut self, id: Option<SurfaceId>) {
        self.focused = id;
    }
}

impl TiledLayoutPolicy for TabbedLayout {
    fn tile(
        &mut self,
        windows: &[TiledWindow],
        output: Rect,
        _gaps: GapConfig,
    ) -> Vec<(SurfaceId, Rect)> {
        let n = windows.len();
        let mut out = Vec::with_capacity(n);
        if n == 0 {
            return out;
        }
        // Default-focus to the first window if no explicit focus has
        // been set and the previous focus is no longer in the list.
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

impl LayoutPolicy for TabbedLayout {
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
    fn first_window_is_focused_by_default() {
        let mut l = TabbedLayout::new();
        let result = l.tile(&[win(1), win(2)], out(1000, 800), GapConfig::zero());
        assert_eq!(result[0].1, out(1000, 800));
        assert_eq!(result[1].1.w, 0);
        assert_eq!(result[1].1.h, 0);
    }

    #[test]
    fn explicit_focus_overrides_default() {
        let mut l = TabbedLayout::new();
        l.set_focused(Some(SurfaceId(2)));
        let result = l.tile(&[win(1), win(2)], out(1000, 800), GapConfig::zero());
        assert_eq!(result[0].1.w, 0);
        assert_eq!(result[1].1, out(1000, 800));
    }

    #[test]
    fn passes_tile_contract_suite() {
        tile_contract_suite(TabbedLayout::new);
    }
}
