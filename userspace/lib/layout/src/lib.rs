//! Phase 72 — Pure-logic tiling layout policies.
//!
//! This crate provides the [`LayoutPolicy`] trait plus the concrete
//! layout policies (`MasterStackLayout`, `DwindleLayout`, `GridLayout`,
//! `TabbedLayout`, `SpiralLayout`, `FullscreenLayout`) that the
//! Phase 56 compositor delegates window placement to.
//!
//! The crate is `no_std` so it can compile into `display_server` (the
//! userspace compositor) and host-side `cargo test -p layout` runs at
//! the same time without a target shuffle. All policies are I/O-free
//! pure functions of (`TiledWindow` list, output `Rect`, gap config),
//! which is why their tests live next to the code instead of under
//! the QEMU harness.
//!
//! The legacy [`kernel_core::display::layout::LayoutPolicy`] trait
//! (Phase 56) is the contract surface every layout in the system
//! continues to satisfy: this trait is implemented for every policy
//! type below, plus a [`GapConfig`]-aware [`TiledLayoutPolicy`] super-
//! trait for the policies that observe gaps and per-tile resize hooks.
//! `display_server` selects a policy through a [`PolicyKind`] enum and
//! invokes [`TiledLayoutPolicy::tile`] from the compose loop.

#![no_std]

extern crate alloc;

pub mod dwindle;
pub mod fullscreen;
pub mod grid;
pub mod master_stack;
pub mod spiral;
pub mod tabbed;

pub use kernel_core::display::layout::{
    FloatingLayout, LayoutPolicy, LayoutSurface, OutputGeometry, layout_contract_suite,
};
pub use kernel_core::display::protocol::{Rect, SurfaceId};

pub use dwindle::DwindleLayout;
pub use fullscreen::FullscreenLayout;
pub use grid::GridLayout;
pub use master_stack::MasterStackLayout;
pub use spiral::SpiralLayout;
pub use tabbed::TabbedLayout;

use alloc::vec::Vec;

/// One window participating in a tiling layout. Mirrors
/// [`LayoutSurface`] but is named to read clearly inside the tiling
/// math: a `LayoutSurface` is "a surface the floating layout might
/// place", a [`TiledWindow`] is "a window the tiling layout *will*
/// place".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TiledWindow {
    /// Stable identity for the window. Matches the surface id the
    /// compositor uses for routing.
    pub id: SurfaceId,
    /// Preferred size in pixels. Tiling policies typically ignore
    /// this except as a tie-breaker; the assignment is geometric.
    pub preferred_size: (u32, u32),
}

impl From<LayoutSurface> for TiledWindow {
    fn from(s: LayoutSurface) -> Self {
        Self {
            id: s.id,
            preferred_size: s.preferred_size,
        }
    }
}

/// Gap configuration: pixel margins around the outer edge of the tiled
/// area and between adjacent tiles. Values are *pixel counts* (not
/// percentages); zero produces edge-to-edge tiling.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct GapConfig {
    /// Pixels subtracted from every edge of the output before the
    /// layout partitions it. A 12 px outer gap means tiles never touch
    /// the screen edge.
    pub outer: u16,
    /// Pixels between adjacent tiles. Applied symmetrically so two
    /// neighboring tiles each shrink by `inner / 2`.
    pub inner: u16,
}

impl GapConfig {
    /// Construct a `GapConfig` from outer / inner pixel counts.
    pub const fn new(outer: u16, inner: u16) -> Self {
        Self { outer, inner }
    }

    /// All-zero configuration — produces pixel-exact tiling with no
    /// gap rows.
    pub const fn zero() -> Self {
        Self { outer: 0, inner: 0 }
    }
}

/// Direction argument for [`TiledLayoutPolicy::adjust_focused`]. Each
/// direction is a unit axis the policy interprets according to its
/// own geometry: master/stack treats `Left`/`Right` as ratio changes,
/// dwindle/spiral treat all four directions as parent-split-ratio
/// changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResizeDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Error returned by [`TiledLayoutPolicy::adjust_focused`]. Grid,
/// tabbed, and fullscreen policies return `Unsupported` because they
/// have no per-tile ratio to adjust; the compositor logs and discards
/// the request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutError {
    /// The active policy does not implement adjustment along this
    /// axis. Phase 72 keyboard handler logs at `debug!` and consumes
    /// the request so resize keys are silent no-ops on these layouts.
    Unsupported,
    /// The window id is not present in the layout's internal tree.
    /// This is a programming error in the compositor (which should
    /// have synced add/remove before forwarding adjust calls), not a
    /// user-facing failure. The compositor logs and continues.
    UnknownWindow,
}

/// Tiling-aware layout policy. Extends the legacy [`LayoutPolicy`]
/// (which produces a centred / cascade arrangement) with:
///
/// * A gap-aware [`TiledLayoutPolicy::tile`] method that consumes a
///   [`GapConfig`] and returns the final per-window rectangles inside
///   the post-gap output area.
/// * An [`TiledLayoutPolicy::adjust_focused`] hook so resize-mode
///   keybinds can drive per-policy ratio changes.
///
/// Implementations should treat `windows` as the call-frame source of
/// truth and avoid persistent per-tile state unless it materially
/// affects later calls (e.g. dwindle's binary tree).
pub trait TiledLayoutPolicy {
    /// Compute one [`Rect`] per input window, partitioning `output`
    /// (already trimmed by outer gaps in the caller's pre-pass) and
    /// applying `gaps.inner` between adjacent tiles. The returned
    /// `Vec` has exactly `windows.len()` entries; each tuple's
    /// `SurfaceId` matches the corresponding input entry.
    fn tile(
        &mut self,
        windows: &[TiledWindow],
        output: Rect,
        gaps: GapConfig,
    ) -> Vec<(SurfaceId, Rect)>;

    /// Resize the tile of `focused` along `direction` by `step` pixels.
    /// The default returns [`LayoutError::Unsupported`] so policies
    /// opt in by overriding.
    fn adjust_focused(
        &mut self,
        _focused: SurfaceId,
        _direction: ResizeDirection,
        _step: i16,
    ) -> Result<(), LayoutError> {
        Err(LayoutError::Unsupported)
    }

    /// Notify the layout that a window joined the tiling set. Default
    /// is a no-op; policies with internal structure (dwindle's tree)
    /// override to update bookkeeping.
    fn on_window_added(&mut self, _window: TiledWindow) {}

    /// Notify the layout that a window left the tiling set.
    fn on_window_removed(&mut self, _id: SurfaceId) {}

    /// Notify the layout that input focus moved to `id` (or no
    /// surface when `None`). Default is a no-op.
    fn on_focus_changed(&mut self, _id: Option<SurfaceId>) {}
}

/// Enum tag selecting which built-in tiling policy is active. The
/// compositor stores one of these per workspace and constructs the
/// matching trait object on demand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PolicyKind {
    MasterStack,
    Dwindle,
    Spiral,
    Grid,
    Tabbed,
    Fullscreen,
}

impl PolicyKind {
    /// Parse the case-insensitive policy name surfaced by config
    /// files and the `m3ctl layout <name>` verb. Returns `None` for
    /// unknown names.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "master-stack" | "master_stack" | "master" | "masterstack" => {
                Some(PolicyKind::MasterStack)
            }
            "dwindle" => Some(PolicyKind::Dwindle),
            "spiral" => Some(PolicyKind::Spiral),
            "grid" => Some(PolicyKind::Grid),
            "tabbed" => Some(PolicyKind::Tabbed),
            "fullscreen" => Some(PolicyKind::Fullscreen),
            _ => None,
        }
    }

    /// Stable wire / config name for this kind. Inverse of
    /// [`from_name`] up to canonical casing.
    pub fn as_name(self) -> &'static str {
        match self {
            PolicyKind::MasterStack => "master-stack",
            PolicyKind::Dwindle => "dwindle",
            PolicyKind::Spiral => "spiral",
            PolicyKind::Grid => "grid",
            PolicyKind::Tabbed => "tabbed",
            PolicyKind::Fullscreen => "fullscreen",
        }
    }
}

/// Apply outer gaps to `output`, returning the inner rect that
/// tiling policies should partition. The result is `output` shrunk by
/// `outer` pixels on every side, clamped to zero size.
pub fn apply_outer_gaps(output: Rect, outer: u16) -> Rect {
    let outer_i = outer as i32;
    let dw = (outer as u32).saturating_mul(2);
    let dh = (outer as u32).saturating_mul(2);
    let w = output.w.saturating_sub(dw);
    let h = output.h.saturating_sub(dh);
    Rect {
        x: output.x.saturating_add(outer_i),
        y: output.y.saturating_add(outer_i),
        w,
        h,
    }
}

/// Shrink `rect` symmetrically on each of two opposite sides by
/// `half_inner` pixels. Used internally by tile-shrinking helpers so
/// neighbouring tiles each give up half the inner gap.
pub(crate) fn shrink_horizontal(rect: Rect, half_inner: u16, left: bool, right: bool) -> Rect {
    let mut r = rect;
    let hi = half_inner as u32;
    let hii = half_inner as i32;
    if left {
        r.x = r.x.saturating_add(hii);
        r.w = r.w.saturating_sub(hi);
    }
    if right {
        r.w = r.w.saturating_sub(hi);
    }
    r
}

pub(crate) fn shrink_vertical(rect: Rect, half_inner: u16, top: bool, bottom: bool) -> Rect {
    let mut r = rect;
    let hi = half_inner as u32;
    let hii = half_inner as i32;
    if top {
        r.y = r.y.saturating_add(hii);
        r.h = r.h.saturating_sub(hi);
    }
    if bottom {
        r.h = r.h.saturating_sub(hi);
    }
    r
}

/// Shared assertion suite every [`TiledLayoutPolicy`] impl must pass.
/// Mirrors the kernel-core [`layout_contract_suite`] but covers the
/// tiling-specific invariants:
///
/// * Empty input → empty output.
/// * `result.len() == windows.len()` and ids preserved in order.
/// * No two output rects overlap (strict).
/// * Every output rect is fully inside `output`.
/// * Determinism: two fresh instances with identical inputs return
///   identical outputs.
pub fn tile_contract_suite<P: TiledLayoutPolicy, F: Fn() -> P>(constructor: F) {
    let output = Rect {
        x: 0,
        y: 0,
        w: 1280,
        h: 720,
    };
    let gaps = GapConfig::zero();

    // 1. Empty → empty.
    {
        let mut layout = constructor();
        let result = layout.tile(&[], output, gaps);
        assert!(result.is_empty(), "empty windows must yield empty result");
    }

    // 2. Single window: 1 rect, id matches.
    {
        let mut layout = constructor();
        let windows = [TiledWindow {
            id: SurfaceId(1),
            preferred_size: (300, 200),
        }];
        let result = layout.tile(&windows, output, gaps);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, SurfaceId(1));
    }

    // 3. Length parity + id order preservation.
    {
        let mut layout = constructor();
        let windows = [
            TiledWindow {
                id: SurfaceId(1),
                preferred_size: (300, 200),
            },
            TiledWindow {
                id: SurfaceId(2),
                preferred_size: (200, 150),
            },
            TiledWindow {
                id: SurfaceId(3),
                preferred_size: (400, 300),
            },
        ];
        let result = layout.tile(&windows, output, gaps);
        assert_eq!(result.len(), windows.len());
        for (i, w) in windows.iter().enumerate() {
            assert_eq!(result[i].0, w.id);
        }
    }

    // 4. No two output rects overlap.
    {
        let mut layout = constructor();
        let windows: Vec<TiledWindow> = (1..=4u32)
            .map(|i| TiledWindow {
                id: SurfaceId(i),
                preferred_size: (300, 200),
            })
            .collect();
        let result = layout.tile(&windows, output, gaps);
        for i in 0..result.len() {
            for j in (i + 1)..result.len() {
                let (_, a) = result[i];
                let (_, b) = result[j];
                if a.w == 0 || a.h == 0 || b.w == 0 || b.h == 0 {
                    // Zero-sized rects are trivially non-overlapping
                    // (some policies like Tabbed return them
                    // intentionally for unfocused windows).
                    continue;
                }
                let overlap = rects_overlap(a, b);
                assert!(
                    !overlap,
                    "rects overlap: {:?} ∩ {:?} (idx {} vs {})",
                    a, b, i, j
                );
            }
        }
    }

    // 5. Every rect inside output.
    {
        let mut layout = constructor();
        let windows = [
            TiledWindow {
                id: SurfaceId(1),
                preferred_size: (300, 200),
            },
            TiledWindow {
                id: SurfaceId(2),
                preferred_size: (200, 150),
            },
        ];
        let result = layout.tile(&windows, output, gaps);
        for (_, rect) in &result {
            if rect.w == 0 || rect.h == 0 {
                continue;
            }
            let rx2 = rect.x.saturating_add(rect.w as i32);
            let ry2 = rect.y.saturating_add(rect.h as i32);
            let ox2 = output.x.saturating_add(output.w as i32);
            let oy2 = output.y.saturating_add(output.h as i32);
            assert!(
                rect.x >= output.x && rect.y >= output.y && rx2 <= ox2 && ry2 <= oy2,
                "rect {:?} escapes output {:?}",
                rect,
                output
            );
        }
    }

    // 6. Determinism.
    {
        let windows = [
            TiledWindow {
                id: SurfaceId(1),
                preferred_size: (300, 200),
            },
            TiledWindow {
                id: SurfaceId(2),
                preferred_size: (250, 180),
            },
        ];
        let mut a = constructor();
        let mut b = constructor();
        let ra = a.tile(&windows, output, gaps);
        let rb = b.tile(&windows, output, gaps);
        assert_eq!(ra, rb, "tile must be deterministic");
    }
}

pub(crate) fn rects_overlap(a: Rect, b: Rect) -> bool {
    let ax2 = a.x.saturating_add(a.w as i32);
    let ay2 = a.y.saturating_add(a.h as i32);
    let bx2 = b.x.saturating_add(b.w as i32);
    let by2 = b.y.saturating_add(b.h as i32);
    a.x < bx2 && b.x < ax2 && a.y < by2 && b.y < ay2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_gaps_shrink_rect() {
        let output = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 800,
        };
        let inner = apply_outer_gaps(output, 12);
        assert_eq!(
            inner,
            Rect {
                x: 12,
                y: 12,
                w: 976,
                h: 776
            }
        );
    }

    #[test]
    fn zero_outer_gap_is_noop() {
        let output = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 800,
        };
        let inner = apply_outer_gaps(output, 0);
        assert_eq!(inner, output);
    }

    #[test]
    fn policy_kind_round_trips() {
        for kind in [
            PolicyKind::MasterStack,
            PolicyKind::Dwindle,
            PolicyKind::Spiral,
            PolicyKind::Grid,
            PolicyKind::Tabbed,
            PolicyKind::Fullscreen,
        ] {
            assert_eq!(PolicyKind::from_name(kind.as_name()), Some(kind));
        }
        assert_eq!(PolicyKind::from_name("nonsense"), None);
        assert_eq!(
            PolicyKind::from_name("MASTER-STACK"),
            Some(PolicyKind::MasterStack)
        );
    }

    #[test]
    fn rects_overlap_basic() {
        let a = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        let b = Rect {
            x: 50,
            y: 50,
            w: 100,
            h: 100,
        };
        let c = Rect {
            x: 100,
            y: 0,
            w: 100,
            h: 100,
        };
        assert!(rects_overlap(a, b));
        assert!(!rects_overlap(a, c));
    }
}
