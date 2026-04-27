//! Phase 56 Track G.3 — Layer-shell exclusive-zone integration test.
//!
//! Pure-host integration test that combines the three pure-logic
//! primitives the userspace `display_server` uses to enforce E.2's
//! exclusive-zone behaviour:
//!
//! * [`compute_layer_geometry`] — anchor + margin → rectangle.
//! * [`derive_exclusive_rect`]  — layer rect → "stay-out" rectangle the
//!   composer subtracts from the toplevel band.
//! * [`LayerConflictTracker`]   — global single-exclusive-keyboard claim.
//!
//! Together those three pieces are everything the `display_server`
//! `SurfaceRegistry` uses to answer the Phase 56 G.3 acceptance bullets.
//! Because `userspace/display_server` is `#![no_std] #![no_main]` it
//! cannot be host-tested directly; the G.3 task brief explicitly directs
//! the integration test to land here, against the kernel-core API
//! surface, with a synthetic in-test surface registry.
//!
//! # G.3 acceptance mapping
//!
//! | G.3 acceptance bullet                                            | Test                                                              |
//! |------------------------------------------------------------------|-------------------------------------------------------------------|
//! | Top-anchored Layer surface with `exclusive_zone = 24`            | `top_layer_with_exclusive_zone_yields_top_strip`                  |
//! | `exclusive_zones(output)` returns the 24-pixel rect              | `top_layer_with_exclusive_zone_yields_top_strip`                  |
//! | Toplevel placement (toplevel does not yet honor exclusive zones) | `toplevel_default_placement_does_not_honor_exclusive_zones`      |
//! | Destroy Layer → `exclusive_zones(output)` returns empty          | `destroying_layer_clears_exclusive_zones`                         |
//! | Two `Exclusive` keyboard claims → second returns conflict        | `second_exclusive_layer_claim_returns_conflict`                   |
//!
//! # Bulk-drain deferral
//!
//! G.3 itself is _not_ blocked on the userspace bulk-drain gap (D.3 / E.4
//! TODO `C.5-bulk-drain`): every API surface this test exercises is pure
//! logic that already executes synchronously. The remaining G.3 runtime
//! verification — that `display_server` _actually_ subtracts the band
//! when arranging toplevels via the layout policy — is gated on the
//! Phase 56b layout-engine swap (`E.1` wiring notes) and is not in
//! scope here.

#![cfg(feature = "std")]

use kernel_core::display::layer::{
    LayerConflictTracker, LayerError, compute_layer_geometry, derive_exclusive_rect,
};
use kernel_core::display::protocol::{
    ANCHOR_TOP, KeyboardInteractivity, Layer, LayerConfig, Rect, SurfaceId, SurfaceRole,
};

// ---------------------------------------------------------------------------
// Synthetic surface registry
// ---------------------------------------------------------------------------
//
// Mirrors the data structure shape `userspace/display_server::surface
// ::SurfaceRegistry` uses for layer-shell bookkeeping, but limited to
// the slices G.3 needs to verify. Holding the surfaces in a deliberately
// dumb `Vec` (rather than the production `BTreeMap<SurfaceId, _>`)
// keeps the test source easy for a reviewer to follow at a glance.

#[derive(Clone, Copy, Debug)]
struct SyntheticSurface {
    id: SurfaceId,
    role: SurfaceRole,
    /// Intrinsic preferred (w, h). Used by `compute_layer_geometry` for
    /// `Layer` surfaces; meaningless for `Toplevel`.
    intrinsic: (u32, u32),
}

#[derive(Default)]
struct SyntheticSurfaceRegistry {
    surfaces: Vec<SyntheticSurface>,
    layer_conflicts: LayerConflictTracker,
}

impl SyntheticSurfaceRegistry {
    fn new() -> Self {
        Self::default()
    }

    /// Add a layer surface. Replicates the `display_server` flow:
    /// claim the exclusive-keyboard slot if requested, then store the
    /// surface for later geometry queries.
    fn add_layer(
        &mut self,
        id: SurfaceId,
        layer_config: LayerConfig,
        intrinsic: (u32, u32),
    ) -> Result<(), LayerError> {
        self.layer_conflicts.try_claim(id, &layer_config)?;
        self.surfaces.push(SyntheticSurface {
            id,
            role: SurfaceRole::Layer(layer_config),
            intrinsic,
        });
        Ok(())
    }

    /// Add a toplevel surface — no claim, no geometry to compute up
    /// front; the production composer derives toplevel geometry through
    /// the layout policy at compose time.
    fn add_toplevel(&mut self, id: SurfaceId) {
        self.surfaces.push(SyntheticSurface {
            id,
            role: SurfaceRole::Toplevel,
            intrinsic: (0, 0),
        });
    }

    /// Destroy a surface by id. Mirrors `SurfaceRegistry::handle_message`'s
    /// `DestroySurface` arm: drop the entry, release the conflict slot
    /// when the dropped surface held it.
    fn destroy(&mut self, id: SurfaceId) {
        if let Some(idx) = self.surfaces.iter().position(|s| s.id == id) {
            self.surfaces.remove(idx);
        }
        self.layer_conflicts.release(id);
    }

    /// All exclusive zones currently claimed against `output`. Mirrors
    /// `SurfaceRegistry::exclusive_zones`: iterate `Layer` surfaces and
    /// emit a rectangle when the anchor + zone combination qualifies as
    /// a full-edge tiling.
    fn exclusive_zones(&self, output: Rect) -> Vec<Rect> {
        self.surfaces
            .iter()
            .filter_map(|s| match s.role {
                SurfaceRole::Layer(cfg) => {
                    let geo = compute_layer_geometry(output, &cfg, s.intrinsic);
                    derive_exclusive_rect(geo, &cfg)
                }
                _ => None,
            })
            .collect()
    }

    /// Return all layer-surface geometries paired with their roles, in
    /// insertion order. The production composer sorts by `ComposeLayer`
    /// before painting; G.3 doesn't care about ordering, just that the
    /// rectangles are correct.
    fn layer_geometries(&self, output: Rect) -> Vec<(SurfaceId, Rect)> {
        self.surfaces
            .iter()
            .filter_map(|s| match s.role {
                SurfaceRole::Layer(cfg) => {
                    Some((s.id, compute_layer_geometry(output, &cfg, s.intrinsic)))
                }
                _ => None,
            })
            .collect()
    }

    /// `SurfaceId` currently holding the exclusive-keyboard slot, if any.
    fn active_exclusive_layer(&self) -> Option<SurfaceId> {
        self.layer_conflicts.active()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn output_1280x800() -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: 1280,
        h: 800,
    }
}

fn top_bar_layer_config(exclusive_zone: u32) -> LayerConfig {
    LayerConfig {
        layer: Layer::Top,
        anchor_mask: ANCHOR_TOP,
        exclusive_zone,
        keyboard_interactivity: KeyboardInteractivity::None,
        margin: [0, 0, 0, 0],
    }
}

fn exclusive_keyboard_layer_config() -> LayerConfig {
    LayerConfig {
        layer: Layer::Top,
        anchor_mask: ANCHOR_TOP,
        exclusive_zone: 0,
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        margin: [0, 0, 0, 0],
    }
}

// ---------------------------------------------------------------------------
// G.3 acceptance tests
// ---------------------------------------------------------------------------

#[test]
fn top_layer_with_exclusive_zone_yields_top_strip() {
    // G.3 acceptance bullet 1+2: top-anchored 24px exclusive zone on a
    // 1280×800 output yields exactly Rect { 0, 0, 1280, 24 } in the
    // exclusive-zone list.
    let mut registry = SyntheticSurfaceRegistry::new();
    let cfg = top_bar_layer_config(24);
    registry
        .add_layer(SurfaceId(1), cfg, (0, 24))
        .expect("first layer claim should succeed");

    let zones = registry.exclusive_zones(output_1280x800());
    assert_eq!(zones.len(), 1, "one zone for one layer surface");
    assert_eq!(
        zones[0],
        Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 24,
        },
        "top-anchored 24px zone should occupy the top strip across the full output width",
    );
}

#[test]
fn toplevel_default_placement_does_not_honor_exclusive_zones() {
    // G.3 acceptance bullet 3 (with documented Phase-56-vs-Phase-56b
    // boundary): a Toplevel surface created alongside a Layer surface
    // exists in the registry but Phase 56's pure-logic registry does
    // *not* compute toplevel geometry — that responsibility belongs to
    // the production layout policy (`kernel-core::display::layout`),
    // which is wired through `display_server::main`. The G.3 task brief
    // explicitly notes this: "today does not subtract the layer band;
    // toplevel placement honoring exclusive zones is gated on the
    // layout-engine swap that lands in Phase 56b". This test therefore
    // verifies the *registry-level* contract: the Toplevel is present,
    // the exclusive-zone list still contains the layer's 24-pixel rect,
    // and the registry does not silently drop or re-arrange surfaces.
    let mut registry = SyntheticSurfaceRegistry::new();
    let cfg = top_bar_layer_config(24);
    registry
        .add_layer(SurfaceId(1), cfg, (0, 24))
        .expect("layer claim ok");
    registry.add_toplevel(SurfaceId(2));

    let geometries = registry.layer_geometries(output_1280x800());
    assert_eq!(
        geometries.len(),
        1,
        "registry holds one Layer surface; the Toplevel is not a Layer",
    );

    let zones = registry.exclusive_zones(output_1280x800());
    assert_eq!(
        zones,
        vec![Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 24,
        }],
        "Toplevel does not contribute to the exclusive-zone list, but the layer's zone is intact",
    );
}

#[test]
fn destroying_layer_clears_exclusive_zones() {
    // G.3 acceptance bullet 4: removing the Layer surface causes
    // `exclusive_zones` to return empty. In production this is what
    // unblocks the toplevel band growing back.
    let mut registry = SyntheticSurfaceRegistry::new();
    let cfg = top_bar_layer_config(24);
    registry
        .add_layer(SurfaceId(1), cfg, (0, 24))
        .expect("layer claim ok");

    assert_eq!(
        registry.exclusive_zones(output_1280x800()).len(),
        1,
        "precondition: layer is mapped",
    );

    registry.destroy(SurfaceId(1));

    assert!(
        registry.exclusive_zones(output_1280x800()).is_empty(),
        "destroying the layer surface should clear all exclusive zones",
    );
}

#[test]
fn second_exclusive_layer_claim_returns_conflict() {
    // G.3 acceptance bullet 5: a second `Exclusive` keyboard layer is
    // rejected with `LayerError::ExclusiveLayerConflict`.
    let mut registry = SyntheticSurfaceRegistry::new();
    let cfg = exclusive_keyboard_layer_config();
    registry
        .add_layer(SurfaceId(1), cfg, (0, 24))
        .expect("first exclusive claim should succeed");

    let result = registry.add_layer(SurfaceId(2), cfg, (0, 24));
    assert_eq!(
        result,
        Err(LayerError::ExclusiveLayerConflict),
        "second concurrent Exclusive claim must surface the typed conflict error",
    );
    assert_eq!(
        registry.active_exclusive_layer(),
        Some(SurfaceId(1)),
        "the first claim still holds the slot",
    );
}

#[test]
fn destroying_exclusive_layer_releases_slot_for_replacement() {
    // Round-trip the conflict path: a second exclusive claim becomes
    // possible once the first surface is destroyed. This is the
    // "panel restart" recovery path G.3 doesn't strictly require but
    // pinning here means the regression catches a release-on-destroy
    // bug if one is ever introduced.
    let mut registry = SyntheticSurfaceRegistry::new();
    let cfg = exclusive_keyboard_layer_config();
    registry
        .add_layer(SurfaceId(1), cfg, (0, 24))
        .expect("first exclusive claim should succeed");
    assert_eq!(registry.active_exclusive_layer(), Some(SurfaceId(1)));

    registry.destroy(SurfaceId(1));
    assert_eq!(
        registry.active_exclusive_layer(),
        None,
        "destroying the holder must release the exclusive-keyboard slot",
    );

    // A replacement panel can now claim Exclusive without conflict.
    registry
        .add_layer(SurfaceId(2), cfg, (0, 24))
        .expect("second exclusive claim should succeed after release");
    assert_eq!(
        registry.active_exclusive_layer(),
        Some(SurfaceId(2)),
        "new holder reflects the post-release state",
    );
}
