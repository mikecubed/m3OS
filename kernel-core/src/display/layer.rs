//! Phase 56 Track E.2 — `Layer` surface role geometry + exclusive zones.
//!
//! Pure-logic helpers consumed by the userspace `display_server` shim:
//!
//! * [`compute_layer_geometry`] — derive a `Layer` surface's destination
//!   rectangle from output geometry, anchor edges, intrinsic size, and
//!   four-sided pixel margins.
//! * [`derive_exclusive_rect`] — derive the "stay-out" rectangle the
//!   composer subtracts from the toplevel band so panels / docks /
//!   notification trays don't get drawn over by app windows.
//!
//! ## Anchor table
//!
//! Modeled after the wlroots / Smithay `wlr-layer-shell` semantics
//! (Phase 56 architecture doc § A.6). All margins are subtracted from
//! the anchored edge(s); horizontal margins clamp before vertical.
//!
//! | Anchor combination          | Geometry                                                           |
//! | --------------------------- | ------------------------------------------------------------------ |
//! | `CENTER` *(or empty mask)*  | Centered at intrinsic size                                         |
//! | `TOP` only                  | Top-edge bar, full width, height = intrinsic.h                     |
//! | `BOTTOM` only               | Bottom-edge bar, full width, height = intrinsic.h                  |
//! | `LEFT` only                 | Left-edge column, full height, width = intrinsic.w                 |
//! | `RIGHT` only                | Right-edge column, full height, width = intrinsic.w                |
//! | `TOP + BOTTOM`              | Centered vertical strip — full height, width = intrinsic.w         |
//! | `LEFT + RIGHT`              | Centered horizontal strip — full width, height = intrinsic.h       |
//! | `TOP + LEFT`                | Top-left corner — intrinsic size                                   |
//! | `TOP + RIGHT`               | Top-right corner — intrinsic size                                  |
//! | `BOTTOM + LEFT`             | Bottom-left corner — intrinsic size                                |
//! | `BOTTOM + RIGHT`            | Bottom-right corner — intrinsic size                               |
//! | `TOP + LEFT + RIGHT`        | Top-edge bar stretched horizontally — full width, intrinsic h      |
//! | `BOTTOM + LEFT + RIGHT`     | Bottom-edge bar stretched horizontally — full width, intrinsic h   |
//! | `TOP + BOTTOM + LEFT`       | Left-edge column stretched vertically — full height, intrinsic w   |
//! | `TOP + BOTTOM + RIGHT`      | Right-edge column stretched vertically — full height, intrinsic w  |
//! | `TOP + BOTTOM + LEFT + RIGHT` | Fill the entire output                                           |
//!
//! ## Exclusive-zone routing
//!
//! When a `Layer` surface declares `exclusive_zone > 0`, that pixel
//! count is reserved on the anchored edge and subtracted from the
//! toplevel band — toplevel windows are arranged outside it. Phase 56
//! supports the simple full-edge-tiling case wlroots' layer-shell
//! recognises:
//!
//! * `TOP` only with full-width geometry          → top exclusive strip
//! * `BOTTOM` only with full-width geometry       → bottom exclusive strip
//! * `LEFT` only with full-height geometry        → left exclusive strip
//! * `RIGHT` only with full-height geometry       → right exclusive strip
//!
//! Other anchor combinations are floating overlays (no exclusivity);
//! [`derive_exclusive_rect`] returns `None` for them.

use crate::display::protocol::{LayerConfig, Rect};

// Re-export the protocol types so downstream callers (display_server,
// future control-socket consumers, ports, etc.) need only one path.
pub use crate::display::protocol::{
    ANCHOR_ALL, ANCHOR_BOTTOM, ANCHOR_CENTER, ANCHOR_EDGES, ANCHOR_LEFT, ANCHOR_RIGHT, ANCHOR_TOP,
    KeyboardInteractivity, Layer, is_valid_anchor_mask,
};

/// Pure-logic error surfaced when a layer-related operation violates
/// Phase 56 invariants. The userspace shim wraps these into its own
/// `SurfaceShimError::Layer` variant for return through the dispatcher.
///
/// # Why a sibling enum and not a `ProtocolError` extension
///
/// `ProtocolError` describes wire-encoding violations the codec finds
/// while parsing or serialising frames (truncated buffers, bad enum
/// discriminants, body-length mismatches). Exclusive-layer conflicts
/// are *post-decode policy*: the bytes are well-formed, the frame
/// parsed cleanly, the role/configuration is internally consistent —
/// the conflict only exists in the context of *other* mapped layers
/// in the registry. Mixing those concerns into `ProtocolError` would
/// blur its definition and force every codec test fixture to handle
/// a registry-level error variant, which is the wrong layer.
///
/// Keeping `LayerError` here colocates layer-semantics errors next to
/// the anchor / geometry helpers they describe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum LayerError {
    /// A second `Layer` surface tried to map with
    /// `keyboard_interactivity == Exclusive` while another exclusive
    /// layer was already active. Phase 56 enforces a single global
    /// exclusive-keyboard claim.
    ExclusiveLayerConflict,
}

/// Compute the destination rectangle for a `Layer` surface in output
/// coordinates.
///
/// `output` is the output's full visible rectangle.
/// `layer_config` carries the anchor mask + margins.
/// `intrinsic_size` is the surface's preferred `(w, h)` in pixels.
///
/// All math widens through `i64` so adversarial output dimensions
/// (e.g. `i32::MAX × i32::MAX`) cannot overflow into negative-width
/// rectangles.
pub fn compute_layer_geometry(
    _output: Rect,
    _layer_config: &LayerConfig,
    _intrinsic_size: (u32, u32),
) -> Rect {
    // Stub: returns the zero rect so the test suite is intentionally
    // red — the implementation lands in a follow-up commit (matching
    // the Phase 56 D.1 / D.2 / D.4 failing-tests-first precedent).
    Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    }
}

/// Derive the rectangle to subtract from the toplevel band for a
/// `Layer` surface. Returns `None` when the layer either does not
/// declare an exclusive zone (`exclusive_zone == 0`) or is anchored in
/// a way Phase 56 does not recognise as a full-edge tiling (every
/// non-edge-tiling case is treated as a floating overlay — the
/// composer shows it but layout never reroutes around it).
///
/// The returned rectangle is what `LayoutPolicy::arrange` will subtract
/// from `output` via the existing `usable_rect` helper. Phase 56 treats
/// the *layer's full geometry* as the exclusive rect when the anchor
/// pattern matches a full-edge tiling — `exclusive_zone` itself is the
/// "claim" flag, not a separate width.
pub fn derive_exclusive_rect(
    _layer_geometry: Rect,
    _layer_config: &LayerConfig,
) -> Option<Rect> {
    // Stub — see compute_layer_geometry stub note.
    None
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::protocol::{KeyboardInteractivity, Layer, LayerConfig};

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn cfg(anchor_mask: u8, exclusive_zone: u32) -> LayerConfig {
        LayerConfig {
            layer: Layer::Top,
            anchor_mask,
            exclusive_zone,
            keyboard_interactivity: KeyboardInteractivity::None,
            margin: [0, 0, 0, 0],
        }
    }

    fn cfg_with_margin(anchor_mask: u8, exclusive_zone: u32, margin: [i32; 4]) -> LayerConfig {
        LayerConfig {
            layer: Layer::Top,
            anchor_mask,
            exclusive_zone,
            keyboard_interactivity: KeyboardInteractivity::None,
            margin,
        }
    }

    // ----- Geometry tests -------------------------------------------------

    #[test]
    fn top_anchor_stretches_full_width_intrinsic_height() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_TOP, 24);
        let geo = compute_layer_geometry(output, &cfg, (200, 24));
        assert_eq!(geo, rect(0, 0, 1280, 24));
    }

    #[test]
    fn bottom_anchor_pins_to_bottom_edge() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_BOTTOM, 32);
        let geo = compute_layer_geometry(output, &cfg, (200, 32));
        // y = 800 - 32 = 768; height = 32.
        assert_eq!(geo, rect(0, 768, 1280, 32));
    }

    #[test]
    fn left_anchor_pins_to_left_edge_full_height() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_LEFT, 48);
        let geo = compute_layer_geometry(output, &cfg, (48, 100));
        assert_eq!(geo, rect(0, 0, 48, 800));
    }

    #[test]
    fn right_anchor_pins_to_right_edge_full_height() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_RIGHT, 48);
        let geo = compute_layer_geometry(output, &cfg, (48, 100));
        // x = 1280 - 48 = 1232.
        assert_eq!(geo, rect(1232, 0, 48, 800));
    }

    #[test]
    fn no_anchor_centers_at_intrinsic_size() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(0, 0);
        let geo = compute_layer_geometry(output, &cfg, (200, 100));
        // x = (1280 - 200)/2 = 540; y = (800 - 100)/2 = 350.
        assert_eq!(geo, rect(540, 350, 200, 100));
    }

    #[test]
    fn center_anchor_centers_at_intrinsic_size() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_CENTER, 0);
        let geo = compute_layer_geometry(output, &cfg, (200, 100));
        assert_eq!(geo, rect(540, 350, 200, 100));
    }

    #[test]
    fn top_plus_bottom_full_vertical_strip() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_TOP | ANCHOR_BOTTOM, 0);
        let geo = compute_layer_geometry(output, &cfg, (60, 100));
        // Width = intrinsic.w (60), centered horizontally; height = full.
        assert_eq!(geo.h, 800);
        assert_eq!(geo.w, 60);
        assert_eq!(geo.y, 0);
    }

    #[test]
    fn left_plus_right_full_horizontal_strip() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_LEFT | ANCHOR_RIGHT, 0);
        let geo = compute_layer_geometry(output, &cfg, (100, 40));
        // Width full, height = intrinsic.
        assert_eq!(geo.w, 1280);
        assert_eq!(geo.h, 40);
        assert_eq!(geo.x, 0);
    }

    #[test]
    fn top_left_corner() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_TOP | ANCHOR_LEFT, 0);
        let geo = compute_layer_geometry(output, &cfg, (200, 100));
        assert_eq!(geo, rect(0, 0, 200, 100));
    }

    #[test]
    fn bottom_right_corner() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_BOTTOM | ANCHOR_RIGHT, 0);
        let geo = compute_layer_geometry(output, &cfg, (200, 100));
        // x = 1280 - 200 = 1080; y = 800 - 100 = 700.
        assert_eq!(geo, rect(1080, 700, 200, 100));
    }

    #[test]
    fn all_edges_fills_output() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_TOP | ANCHOR_BOTTOM | ANCHOR_LEFT | ANCHOR_RIGHT, 0);
        let geo = compute_layer_geometry(output, &cfg, (200, 100));
        assert_eq!(geo, rect(0, 0, 1280, 800));
    }

    #[test]
    fn margin_subtracts_from_anchored_edge() {
        let output = rect(0, 0, 1280, 800);
        // Top-anchored bar with 4px top margin and 8/8 left/right margins.
        let cfg = cfg_with_margin(ANCHOR_TOP, 24, [4, 8, 0, 8]);
        let geo = compute_layer_geometry(output, &cfg, (0, 24));
        // y = 4, h = 24, x = 8, w = 1280 - 8 - 8 = 1264.
        assert_eq!(geo, rect(8, 4, 1264, 24));
    }

    #[test]
    fn adversarial_output_dimensions_do_not_overflow() {
        // i32::MAX × i32::MAX output. Centered placement multiplies (ow -
        // intrinsic) which would overflow i32 if not widened to i64.
        let output = rect(0, 0, i32::MAX as u32, i32::MAX as u32);
        let cfg = cfg(0, 0);
        let geo = compute_layer_geometry(output, &cfg, (200, 100));
        // No assertion on exact value beyond "math didn't panic and
        // we got finite output". Just sanity-check w/h are intrinsic.
        assert_eq!(geo.w, 200);
        assert_eq!(geo.h, 100);
    }

    // ----- Exclusive-rect tests -------------------------------------------

    #[test]
    fn top_anchor_with_exclusive_zone_yields_top_strip() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_TOP, 24);
        let geo = compute_layer_geometry(output, &cfg, (0, 24));
        let exclusive = derive_exclusive_rect(geo, &cfg);
        // E.2 acceptance: 1280×800 output with 24-pixel top exclusive
        // zone yields exclusive rect Rect { 0, 0, 1280, 24 }.
        assert_eq!(exclusive, Some(rect(0, 0, 1280, 24)));
    }

    #[test]
    fn bottom_anchor_with_exclusive_zone_yields_bottom_strip() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_BOTTOM, 32);
        let geo = compute_layer_geometry(output, &cfg, (0, 32));
        let exclusive = derive_exclusive_rect(geo, &cfg);
        // y = 800 - 32 = 768; full width.
        assert_eq!(exclusive, Some(rect(0, 768, 1280, 32)));
    }

    #[test]
    fn left_anchor_with_exclusive_zone_yields_left_strip() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_LEFT, 48);
        let geo = compute_layer_geometry(output, &cfg, (48, 0));
        let exclusive = derive_exclusive_rect(geo, &cfg);
        assert_eq!(exclusive, Some(rect(0, 0, 48, 800)));
    }

    #[test]
    fn right_anchor_with_exclusive_zone_yields_right_strip() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_RIGHT, 48);
        let geo = compute_layer_geometry(output, &cfg, (48, 0));
        let exclusive = derive_exclusive_rect(geo, &cfg);
        // x = 1280 - 48 = 1232.
        assert_eq!(exclusive, Some(rect(1232, 0, 48, 800)));
    }

    #[test]
    fn center_anchor_with_zero_exclusive_returns_none() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_CENTER, 0);
        let geo = compute_layer_geometry(output, &cfg, (200, 100));
        let exclusive = derive_exclusive_rect(geo, &cfg);
        // E.2 acceptance: center-anchored layer with exclusive_zone == 0
        // returns None.
        assert_eq!(exclusive, None);
    }

    #[test]
    fn corner_anchor_does_not_claim_exclusive_zone() {
        // Top+Left corner is a floating overlay — even with exclusive_zone
        // set, layout should not treat it as a full-edge tiling.
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_TOP | ANCHOR_LEFT, 16);
        let geo = compute_layer_geometry(output, &cfg, (200, 100));
        let exclusive = derive_exclusive_rect(geo, &cfg);
        assert_eq!(exclusive, None);
    }

    #[test]
    fn vertical_strip_does_not_claim_exclusive_zone() {
        // top+bottom is a vertical strip, not a single-edge tiling.
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_TOP | ANCHOR_BOTTOM, 24);
        let geo = compute_layer_geometry(output, &cfg, (60, 100));
        let exclusive = derive_exclusive_rect(geo, &cfg);
        assert_eq!(exclusive, None);
    }

    #[test]
    fn zero_exclusive_zone_with_top_anchor_returns_none() {
        let output = rect(0, 0, 1280, 800);
        let cfg = cfg(ANCHOR_TOP, 0);
        let geo = compute_layer_geometry(output, &cfg, (0, 24));
        assert_eq!(derive_exclusive_rect(geo, &cfg), None);
    }
}
