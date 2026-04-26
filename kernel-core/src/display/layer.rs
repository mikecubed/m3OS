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

use crate::display::protocol::{
    ANCHOR_BOTTOM, ANCHOR_LEFT, ANCHOR_RIGHT, ANCHOR_TOP, LayerConfig, Rect, SurfaceId,
};

// Re-export the protocol types so downstream callers (display_server,
// future control-socket consumers, ports, etc.) need only one path.
pub use crate::display::protocol::{
    ANCHOR_ALL, ANCHOR_CENTER, ANCHOR_EDGES, KeyboardInteractivity, Layer, is_valid_anchor_mask,
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
    output: Rect,
    layer_config: &LayerConfig,
    intrinsic_size: (u32, u32),
) -> Rect {
    let mask = layer_config.anchor_mask;
    let m_top = layer_config.margin[0];
    let m_right = layer_config.margin[1];
    let m_bottom = layer_config.margin[2];
    let m_left = layer_config.margin[3];

    // Widen everything to i64 up front.
    let ox = output.x as i64;
    let oy = output.y as i64;
    let ow = output.w as i64;
    let oh = output.h as i64;
    let intrinsic_w = intrinsic_size.0 as i64;
    let intrinsic_h = intrinsic_size.1 as i64;
    let mt = m_top as i64;
    let mr = m_right as i64;
    let mb = m_bottom as i64;
    let ml = m_left as i64;

    let has_top = (mask & ANCHOR_TOP) != 0;
    let has_bottom = (mask & ANCHOR_BOTTOM) != 0;
    let has_left = (mask & ANCHOR_LEFT) != 0;
    let has_right = (mask & ANCHOR_RIGHT) != 0;
    // ANCHOR_CENTER is mutually exclusive with edge bits (validated by
    // the protocol decoder) so checking edge bits alone is sufficient.

    // ----- Width / horizontal placement -----------------------------------
    //
    // The wlroots layer-shell rule: width "stretches" when the horizontal
    // axis is fully constrained (both LEFT+RIGHT) OR when there is a
    // single horizontal-edge anchor (exactly one of TOP/BOTTOM, no LEFT
    // and no RIGHT) — a status bar anchored only to TOP, for example.
    // In every other case the width is intrinsic and the surface centers
    // or pins along the horizontal axis depending on which (if any)
    // single horizontal anchor is set.
    let single_horizontal_axis_anchor = (has_top ^ has_bottom) && !has_left && !has_right;
    let (x_i64, w_i64) = if has_left && has_right {
        // Stretched: full output width minus horizontal margins.
        let avail = (ow - ml - mr).max(0);
        (ox + ml, avail)
    } else if single_horizontal_axis_anchor {
        // Single TOP-only or BOTTOM-only anchor → stretch perpendicular
        // axis (full width minus horizontal margins).
        let avail = (ow - ml - mr).max(0);
        (ox + ml, avail)
    } else if has_left {
        // Pinned to the left edge with intrinsic width (margin pushes inward).
        (ox + ml, intrinsic_w.max(0))
    } else if has_right {
        // Pinned to the right edge with intrinsic width.
        (ox + ow - intrinsic_w - mr, intrinsic_w.max(0))
    } else {
        // No horizontal pin and no single-axis stretch trigger → center
        // horizontally with intrinsic width. Covers (no anchors), CENTER,
        // and the TOP+BOTTOM "vertical strip" case.
        let centered_x = ox + (ow - intrinsic_w) / 2;
        (centered_x, intrinsic_w.max(0))
    };

    // ----- Height / vertical placement ------------------------------------
    //
    // Symmetric to the horizontal rule: height "stretches" when the
    // vertical axis is fully constrained (both TOP+BOTTOM) OR when there
    // is a single vertical-edge anchor (exactly one of LEFT/RIGHT, no
    // TOP and no BOTTOM) — a side rail / dock anchored only to LEFT,
    // for example.
    let single_vertical_axis_anchor = (has_left ^ has_right) && !has_top && !has_bottom;
    let (y_i64, h_i64) = if has_top && has_bottom {
        // Stretched: full output height minus vertical margins.
        let avail = (oh - mt - mb).max(0);
        (oy + mt, avail)
    } else if single_vertical_axis_anchor {
        // Single LEFT-only or RIGHT-only anchor → stretch perpendicular
        // axis (full height minus vertical margins).
        let avail = (oh - mt - mb).max(0);
        (oy + mt, avail)
    } else if has_top {
        (oy + mt, intrinsic_h.max(0))
    } else if has_bottom {
        (oy + oh - intrinsic_h - mb, intrinsic_h.max(0))
    } else {
        // No vertical pin and no single-axis stretch trigger → center
        // vertically with intrinsic height. Covers (no anchors), CENTER,
        // and the LEFT+RIGHT "horizontal strip" case.
        let centered_y = oy + (oh - intrinsic_h) / 2;
        (centered_y, intrinsic_h.max(0))
    };

    Rect {
        x: clamp_to_i32(x_i64),
        y: clamp_to_i32(y_i64),
        w: clamp_to_u32(w_i64),
        h: clamp_to_u32(h_i64),
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
pub fn derive_exclusive_rect(layer_geometry: Rect, layer_config: &LayerConfig) -> Option<Rect> {
    if layer_config.exclusive_zone == 0 {
        return None;
    }
    let mask = layer_config.anchor_mask;
    let has_top = (mask & ANCHOR_TOP) != 0;
    let has_bottom = (mask & ANCHOR_BOTTOM) != 0;
    let has_left = (mask & ANCHOR_LEFT) != 0;
    let has_right = (mask & ANCHOR_RIGHT) != 0;

    // Single-edge anchors are the only patterns Phase 56's `usable_rect`
    // recognises as full-edge tilings. Multi-edge anchors are treated
    // as floating overlays — they still render, layout just doesn't
    // route around them.
    let edge_bits = (has_top as u8) + (has_bottom as u8) + (has_left as u8) + (has_right as u8);
    if edge_bits != 1 {
        return None;
    }
    if layer_geometry.w == 0 || layer_geometry.h == 0 {
        return None;
    }
    Some(layer_geometry)
}

/// Tracks which `Layer` surface (if any) currently holds the global
/// exclusive-keyboard claim. Phase 56 enforces a single global
/// exclusive-keyboard layer at a time — the second concurrent claim
/// is rejected with [`LayerError::ExclusiveLayerConflict`].
///
/// This is pure-logic state shared between `kernel-core` (for tests)
/// and the userspace `display_server` shim. The shim owns one of
/// these inside `SurfaceRegistry`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayerConflictTracker {
    active: Option<SurfaceId>,
}

impl LayerConflictTracker {
    /// Construct an empty tracker (no exclusive layer claimed).
    pub const fn new() -> Self {
        Self { active: None }
    }

    /// Identity of the surface currently holding the exclusive
    /// keyboard claim, or `None` if no surface holds it. The D.3
    /// input dispatcher reads this to gate `KeyboardInteractivity`
    /// routing.
    pub const fn active(&self) -> Option<SurfaceId> {
        self.active
    }

    /// Try to claim the exclusive keyboard slot for `surface_id` with
    /// the given `LayerConfig`. Returns `Ok(())` if either the slot
    /// was empty, or the same surface is re-asserting the same claim
    /// (idempotent). Returns
    /// [`LayerError::ExclusiveLayerConflict`] if a different surface
    /// already holds the slot.
    ///
    /// Layers whose `keyboard_interactivity` is anything other than
    /// `Exclusive` do not claim the slot — call `release` on the
    /// caller side when a previously-Exclusive layer transitions
    /// away from `Exclusive`.
    pub fn try_claim(
        &mut self,
        surface_id: SurfaceId,
        layer_config: &LayerConfig,
    ) -> Result<(), LayerError> {
        if !matches!(
            layer_config.keyboard_interactivity,
            KeyboardInteractivity::Exclusive
        ) {
            // Non-Exclusive claim — no slot needed. Idempotent across
            // re-asserts; the slot is unaffected unless the *same*
            // surface previously claimed Exclusive and is now stepping
            // down (caller is responsible for calling `release` in
            // that flow).
            return Ok(());
        }
        match self.active {
            None => {
                self.active = Some(surface_id);
                Ok(())
            }
            Some(existing) if existing == surface_id => {
                // Re-asserting the same claim is idempotent.
                Ok(())
            }
            Some(_) => Err(LayerError::ExclusiveLayerConflict),
        }
    }

    /// Release the exclusive-keyboard claim for `surface_id`. No-op if
    /// `surface_id` does not currently hold the slot. Called on
    /// `DestroySurface` for the holder, or whenever the holder
    /// transitions away from `Exclusive`.
    pub fn release(&mut self, surface_id: SurfaceId) {
        if self.active == Some(surface_id) {
            self.active = None;
        }
    }
}

fn clamp_to_i32(v: i64) -> i32 {
    if v > i32::MAX as i64 {
        i32::MAX
    } else if v < i32::MIN as i64 {
        i32::MIN
    } else {
        v as i32
    }
}

fn clamp_to_u32(v: i64) -> u32 {
    if v < 0 {
        0
    } else if v > u32::MAX as i64 {
        u32::MAX
    } else {
        v as u32
    }
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

    // ----- LayerConflictTracker tests -------------------------------------

    fn cfg_kbd(interactivity: KeyboardInteractivity) -> LayerConfig {
        LayerConfig {
            layer: Layer::Top,
            anchor_mask: ANCHOR_TOP,
            exclusive_zone: 0,
            keyboard_interactivity: interactivity,
            margin: [0, 0, 0, 0],
        }
    }

    #[test]
    fn tracker_starts_empty() {
        let tracker = LayerConflictTracker::new();
        assert_eq!(tracker.active(), None);
    }

    #[test]
    fn tracker_first_exclusive_claim_succeeds() {
        let mut tracker = LayerConflictTracker::new();
        let cfg = cfg_kbd(KeyboardInteractivity::Exclusive);
        let result = tracker.try_claim(SurfaceId(1), &cfg);
        assert_eq!(result, Ok(()));
        assert_eq!(tracker.active(), Some(SurfaceId(1)));
    }

    #[test]
    fn tracker_second_exclusive_claim_conflicts() {
        // E.2 acceptance: at least 1 test on the conflict path.
        let mut tracker = LayerConflictTracker::new();
        let cfg = cfg_kbd(KeyboardInteractivity::Exclusive);
        tracker
            .try_claim(SurfaceId(1), &cfg)
            .expect("first claim should succeed");
        let result = tracker.try_claim(SurfaceId(2), &cfg);
        assert_eq!(result, Err(LayerError::ExclusiveLayerConflict));
        // First claim still holds.
        assert_eq!(tracker.active(), Some(SurfaceId(1)));
    }

    #[test]
    fn tracker_re_asserting_same_surface_is_idempotent() {
        let mut tracker = LayerConflictTracker::new();
        let cfg = cfg_kbd(KeyboardInteractivity::Exclusive);
        tracker
            .try_claim(SurfaceId(1), &cfg)
            .expect("first claim should succeed");
        // Re-asserting from the same surface is allowed (e.g. the
        // client commits the same role twice).
        let result = tracker.try_claim(SurfaceId(1), &cfg);
        assert_eq!(result, Ok(()));
        assert_eq!(tracker.active(), Some(SurfaceId(1)));
    }

    #[test]
    fn tracker_non_exclusive_claim_does_not_take_slot() {
        let mut tracker = LayerConflictTracker::new();
        let cfg_on_demand = cfg_kbd(KeyboardInteractivity::OnDemand);
        let cfg_none = cfg_kbd(KeyboardInteractivity::None);
        assert_eq!(tracker.try_claim(SurfaceId(1), &cfg_on_demand), Ok(()));
        assert_eq!(tracker.try_claim(SurfaceId(2), &cfg_none), Ok(()));
        assert_eq!(tracker.active(), None);
    }

    #[test]
    fn tracker_release_clears_active_for_holder() {
        let mut tracker = LayerConflictTracker::new();
        let cfg = cfg_kbd(KeyboardInteractivity::Exclusive);
        tracker.try_claim(SurfaceId(1), &cfg).expect("claim ok");
        tracker.release(SurfaceId(1));
        assert_eq!(tracker.active(), None);
    }

    #[test]
    fn tracker_release_for_non_holder_is_noop() {
        let mut tracker = LayerConflictTracker::new();
        let cfg = cfg_kbd(KeyboardInteractivity::Exclusive);
        tracker.try_claim(SurfaceId(1), &cfg).expect("claim ok");
        tracker.release(SurfaceId(99));
        // Holder still active.
        assert_eq!(tracker.active(), Some(SurfaceId(1)));
    }

    #[test]
    fn tracker_after_release_other_surface_can_claim() {
        let mut tracker = LayerConflictTracker::new();
        let cfg = cfg_kbd(KeyboardInteractivity::Exclusive);
        tracker.try_claim(SurfaceId(1), &cfg).expect("claim ok");
        tracker.release(SurfaceId(1));
        let result = tracker.try_claim(SurfaceId(2), &cfg);
        assert_eq!(result, Ok(()));
        assert_eq!(tracker.active(), Some(SurfaceId(2)));
    }

    // ----- Layer-level ordering test --------------------------------------
    //
    // E.2 acceptance #6: verify that a `Layer Top` + `Toplevel` + `Layer
    // Background` combination composes in the order Background → Toplevel
    // → Top via the existing recording-FB owner. The pure-logic composer
    // already sorts by `ComposeLayer`; this test pins the role-to-band
    // mapping that E.2 wires through `SurfaceRegistry::iter_compose`.

    use crate::display::compose::{ComposeLayer, ComposeSurface, compose_frame};
    use crate::display::fb_owner::{FbMetadata, PixelFormat, RecordingFramebufferOwner};

    fn fb_meta(width: u32, height: u32) -> FbMetadata {
        FbMetadata {
            width,
            height,
            stride_bytes: width * 4,
            pixel_format: PixelFormat::Bgra8888,
        }
    }

    #[test]
    fn layer_role_compose_order_background_then_toplevel_then_top() {
        // Three small distinguishable surfaces. We use the actual
        // ComposeLayer mapping for each role:
        //   Layer { layer: Background } → ComposeLayer::Background
        //   SurfaceRole::Toplevel        → ComposeLayer::Toplevel
        //   Layer { layer: Top }         → ComposeLayer::Top
        let mut owner = RecordingFramebufferOwner::new(fb_meta(200, 200));
        let bg_pixels = alloc::vec![0x11; 200 * 200 * 4];
        let toplevel_pixels = alloc::vec![0x22; 80 * 80 * 4];
        let layer_top_pixels = alloc::vec![0x33; 40 * 40 * 4];
        let bg_damage = [rect(0, 0, 200, 200)];
        let toplevel_damage = [rect(0, 0, 80, 80)];
        let layer_top_damage = [rect(0, 0, 40, 40)];

        // Submit in *non*-canonical order to prove the composer is
        // doing the sorting, not the test fixture.
        let mut surfaces = [
            ComposeSurface {
                id: SurfaceId(2),
                layer: ComposeLayer::Top,
                rect: rect(20, 20, 40, 40),
                damage: &layer_top_damage,
                pixels: &layer_top_pixels,
                opaque: false,
            },
            ComposeSurface {
                id: SurfaceId(3),
                layer: ComposeLayer::Toplevel,
                rect: rect(60, 60, 80, 80),
                damage: &toplevel_damage,
                pixels: &toplevel_pixels,
                opaque: true,
            },
            ComposeSurface {
                id: SurfaceId(1),
                layer: ComposeLayer::Background,
                rect: rect(0, 0, 200, 200),
                damage: &bg_damage,
                pixels: &bg_pixels,
                opaque: false,
            },
        ];

        let result = compose_frame(&mut owner, rect(0, 0, 200, 200), &mut surfaces);
        assert_eq!(result, Ok(3));
        let writes = owner.writes();
        assert_eq!(writes.len(), 3);
        // Background drawn first (covers full output).
        assert_eq!(writes[0].clipped_rect, rect(0, 0, 200, 200));
        // Toplevel drawn second.
        assert_eq!(writes[1].clipped_rect, rect(60, 60, 80, 80));
        // Layer Top drawn last.
        assert_eq!(writes[2].clipped_rect, rect(20, 20, 40, 40));
    }
}
