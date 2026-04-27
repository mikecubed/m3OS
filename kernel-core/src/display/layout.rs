//! Phase 56 Track A.7 / E.1 — `LayoutPolicy` trait + default
//! `FloatingLayout` + `StubLayout` for tests + a shared contract suite.
//!
//! The compositor delegates window placement to a [`LayoutPolicy`]. The
//! Phase 56 default is [`FloatingLayout`] — it centers each new toplevel
//! at its preferred size and applies a small cascade offset so successive
//! windows do not perfectly overlap. [`StubLayout`] is a deterministic
//! test fixture that returns rectangles from a pre-loaded script.
//!
//! [`layout_contract_suite`] is the shared assertion suite every
//! `LayoutPolicy` impl must pass; it is invoked from this crate's tests
//! against both built-in implementations and is exported so downstream
//! crates (e.g. the `display_server` binary) can reuse it.

use alloc::vec::Vec;

use crate::display::protocol::{Rect, SurfaceId};

/// Geometry of an output (monitor) supplied to the layout policy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutputGeometry {
    /// The output's visible rectangle, expressed in compositor coordinates.
    pub rect: Rect,
}

/// A surface eligible for layout. Implementations may consume any subset
/// of these fields. The compositor passes the *same* value to every
/// `LayoutPolicy` impl it ever holds, so adding a field here is a
/// breaking change for layout authors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayoutSurface {
    /// Stable identity for the surface.
    pub id: SurfaceId,
    /// The surface's intrinsic preferred size in pixels (`(width, height)`).
    /// A layout may treat zero as "no preference".
    pub preferred_size: (u32, u32),
}

/// Window-placement policy. The compositor calls [`arrange`] every time
/// the toplevel set or output geometry changes, and may call the
/// notification hooks to give the layout incremental information about
/// surface lifecycle and focus changes.
///
/// [`arrange`]: LayoutPolicy::arrange
pub trait LayoutPolicy {
    /// Compute one rectangle per toplevel. The returned `Vec` must have
    /// exactly `toplevels.len()` entries; each tuple's `SurfaceId` must
    /// equal the corresponding entry in `toplevels` (i.e. layouts may not
    /// drop or reorder surfaces).
    fn arrange(
        &mut self,
        toplevels: &[LayoutSurface],
        output: OutputGeometry,
        exclusive_zones: &[Rect],
    ) -> Vec<(SurfaceId, Rect)>;

    /// Notify the layout that a surface joined the toplevel set. Default
    /// is a no-op so simple layouts only override what they need.
    fn on_surface_added(&mut self, _surface: LayoutSurface) {}

    /// Notify the layout that a surface left the toplevel set.
    fn on_surface_removed(&mut self, _surface: SurfaceId) {}

    /// Notify the layout that input focus moved to `surface` (or to
    /// no surface, when `None`).
    fn on_focus_changed(&mut self, _surface: Option<SurfaceId>) {}

    /// True iff `arrange` will return different rectangles depending on
    /// which surface is focused. The compositor uses this to decide
    /// whether a focus change requires re-running layout.
    fn focus_affects_geometry(&self) -> bool {
        false
    }
}

/// Number of distinct cascade slots used by [`FloatingLayout`] before the
/// offset wraps back to zero.
pub const CASCADE_SLOTS: u32 = 8;
/// Pixel offset between consecutive cascade slots.
pub const CASCADE_OFFSET_PX: i32 = 32;

/// Minimum width used when `preferred_size.0` is zero or doesn't fit.
const MIN_FALLBACK_W: u32 = 200;
/// Minimum height used when `preferred_size.1` is zero or doesn't fit.
const MIN_FALLBACK_H: u32 = 150;

/// Default Phase 56 layout: places each new toplevel at its preferred
/// size, centered in the *usable* output rect (output minus full-edge
/// exclusive zones), with a small per-surface cascade offset so
/// successive windows do not perfectly overlap.
///
/// # Cascade-slot semantics
///
/// `arrange` is called every frame tick with the current toplevel set.
/// Two distinct slot-assignment policies apply, keyed by the size of
/// `toplevels`:
///
/// - **Single-surface call (`toplevels.len() <= 1`).** The persistent
///   [`cascade_slot`](Self::cascade_slot) field selects the slot for
///   the placed surface, then advances modulo [`CASCADE_SLOTS`]. This
///   keeps the legacy "successive single placements march across the
///   screen" behavior used by the layout contract suite and by the
///   single-surface entry path.
/// - **Multi-surface call (`toplevels.len() >= 2`).** Slots are
///   assigned by the surface's ordinal position within *this* call
///   (index 0 → slot 0, index 1 → slot 1, etc.). The persistent
///   counter is **not** consulted and **not** advanced. This makes
///   every surface's position stable across frames — the actual
///   contract the multi-surface compose path requires — and is the
///   behavior validated by the G.1 multi-client coexistence
///   regression.
///
/// The two policies are intentionally independent so the multi-surface
/// path's stability does not depend on whether the single-surface
/// counter has been advanced by an earlier call.
#[derive(Clone, Debug, Default)]
pub struct FloatingLayout {
    /// Wraps modulo `CASCADE_SLOTS`. Advances only on the
    /// single-surface `arrange` path; the multi-surface path leaves it
    /// untouched (see struct-level docs).
    cascade_slot: u32,
}

impl FloatingLayout {
    /// Construct a fresh `FloatingLayout` with cascade slot zero.
    pub fn new() -> Self {
        Self { cascade_slot: 0 }
    }
}

/// Compute the usable output rectangle by subtracting full-edge
/// exclusive zones from `output`. Zones that don't tile a full edge are
/// ignored — the userspace `display_server` is responsible for converting
/// layer geometry into proper edge tilings before calling `arrange`.
fn usable_rect(output: Rect, zones: &[Rect]) -> Rect {
    let ox = output.x;
    let oy = output.y;
    let mut x = ox;
    let mut y = oy;
    let mut w = output.w;
    let mut h = output.h;

    for zone in zones {
        if zone.w == 0 || zone.h == 0 {
            continue;
        }
        // Top edge: x == output.x, y == output.y, w == output.w.
        if zone.x == ox && zone.y == oy && zone.w == output.w {
            let dh = zone.h.min(h);
            y = y.saturating_add(dh as i32);
            h = h.saturating_sub(dh);
            continue;
        }
        // Bottom edge: x == output.x, y == output.y + output.h - zone.h, w == output.w.
        if zone.x == ox && zone.w == output.w {
            let zone_top = zone.y;
            let output_bottom = oy.saturating_add(output.h as i32);
            if zone_top.saturating_add(zone.h as i32) == output_bottom {
                let dh = zone.h.min(h);
                h = h.saturating_sub(dh);
                continue;
            }
        }
        // Left edge: x == output.x, y == output.y, h == output.h.
        if zone.x == ox && zone.y == oy && zone.h == output.h {
            let dw = zone.w.min(w);
            x = x.saturating_add(dw as i32);
            w = w.saturating_sub(dw);
            continue;
        }
        // Right edge: y == output.y, h == output.h, x == output.x + output.w - zone.w.
        if zone.y == oy && zone.h == output.h {
            let zone_left = zone.x;
            let output_right = ox.saturating_add(output.w as i32);
            if zone_left.saturating_add(zone.w as i32) == output_right {
                let dw = zone.w.min(w);
                w = w.saturating_sub(dw);
                continue;
            }
        }
        // Otherwise: ignore. (Non-edge-tiling zones are the display
        // server's responsibility to convert.)
    }

    Rect { x, y, w, h }
}

/// Choose a window size: prefer `preferred`, but clamp into a sensible
/// range relative to `usable`. If `preferred` is zero in either
/// dimension *or* exceeds the usable area, fall back to ~60 % of the
/// usable area, with [`MIN_FALLBACK_W`] / [`MIN_FALLBACK_H`] floors when
/// the usable area itself is small.
fn pick_size(preferred: (u32, u32), usable: Rect) -> (u32, u32) {
    let target_w = if preferred.0 == 0 || preferred.0 > usable.w {
        let candidate = (usable.w * 6) / 10;
        if usable.w < MIN_FALLBACK_W {
            usable.w
        } else if candidate < MIN_FALLBACK_W {
            MIN_FALLBACK_W
        } else {
            candidate
        }
    } else {
        preferred.0
    };
    let target_h = if preferred.1 == 0 || preferred.1 > usable.h {
        let candidate = (usable.h * 6) / 10;
        if usable.h < MIN_FALLBACK_H {
            usable.h
        } else if candidate < MIN_FALLBACK_H {
            MIN_FALLBACK_H
        } else {
            candidate
        }
    } else {
        preferred.1
    };
    (target_w, target_h)
}

impl LayoutPolicy for FloatingLayout {
    fn arrange(
        &mut self,
        toplevels: &[LayoutSurface],
        output: OutputGeometry,
        exclusive_zones: &[Rect],
    ) -> Vec<(SurfaceId, Rect)> {
        let usable = usable_rect(output.rect, exclusive_zones);
        let mut out = Vec::with_capacity(toplevels.len());
        // Phase 56 close-out — assign cascade slots based on the
        // surface's ordinal position within this `arrange` call rather
        // than a persistent counter. `display_server` calls `arrange`
        // every frame tick with the same `toplevels` list (sorted by
        // surface id from `iter_compose`); using the call-local index
        // makes every surface's position stable across frames, which
        // is the actual contract the multi-surface compose path
        // requires. The persistent `self.cascade_slot` is preserved
        // for the legacy single-surface-per-call entry point exercised
        // by the contract test suite.
        let starting_slot = if toplevels.len() <= 1 {
            self.cascade_slot
        } else {
            0
        };
        for (i, surface) in toplevels.iter().enumerate() {
            let (w, h) = pick_size(surface.preferred_size, usable);
            // Center the window in the usable rect.
            let center_x = usable.x.saturating_add((usable.w as i32 - w as i32) / 2);
            let center_y = usable.y.saturating_add((usable.h as i32 - h as i32) / 2);
            let slot = (starting_slot + i as u32) % CASCADE_SLOTS;
            let cascade_off = (slot as i32) * CASCADE_OFFSET_PX;
            let rect = Rect {
                x: center_x.saturating_add(cascade_off),
                y: center_y.saturating_add(cascade_off),
                w,
                h,
            };
            out.push((surface.id, rect));
        }
        // Only advance the persistent counter when the legacy single-
        // surface path was used; the multi-surface path uses
        // call-local indices so the persistent counter stays stable.
        if toplevels.len() <= 1 {
            self.cascade_slot = (self.cascade_slot + toplevels.len() as u32) % CASCADE_SLOTS;
        }
        out
    }
}

/// Test layout that returns rectangles from a pre-loaded script. The
/// `i`-th call's surface receives `script[i]`; if the script runs out,
/// a zero-sized default rectangle is returned.
#[derive(Clone, Debug, Default)]
pub struct StubLayout {
    script: Vec<Rect>,
    cursor: usize,
}

impl StubLayout {
    /// Construct an empty stub layout. Prime it with [`StubLayout::push`]
    /// before calling [`LayoutPolicy::arrange`].
    pub fn new() -> Self {
        Self {
            script: Vec::new(),
            cursor: 0,
        }
    }

    /// Append `rect` to the script. Successive calls hand out rectangles
    /// in the order they were pushed.
    pub fn push(&mut self, rect: Rect) {
        self.script.push(rect);
    }
}

impl LayoutPolicy for StubLayout {
    fn arrange(
        &mut self,
        toplevels: &[LayoutSurface],
        _output: OutputGeometry,
        _exclusive_zones: &[Rect],
    ) -> Vec<(SurfaceId, Rect)> {
        let default_rect = Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        let mut out = Vec::with_capacity(toplevels.len());
        for surface in toplevels {
            let rect = self
                .script
                .get(self.cursor)
                .copied()
                .unwrap_or(default_rect);
            self.cursor += 1;
            out.push((surface.id, rect));
        }
        out
    }
}

/// Run the shared `LayoutPolicy` contract suite against any impl
/// constructable through `constructor`. Each invocation of `constructor`
/// must return a fresh, independent layout instance.
///
/// This is the kernel-core slice of E.1 — the assertions every layout in
/// the system must satisfy. Downstream layouts (e.g. tiling layouts in
/// later phases) reuse this function to keep behavior interchangeable.
pub fn layout_contract_suite<P: LayoutPolicy, F: Fn() -> P>(constructor: F) {
    let output = OutputGeometry {
        rect: Rect {
            x: 0,
            y: 0,
            w: 1024,
            h: 768,
        },
    };

    // 1. Empty toplevels → empty result.
    {
        let mut layout = constructor();
        let result = layout.arrange(&[], output, &[]);
        assert!(result.is_empty(), "empty toplevels must yield empty result");
    }

    // 2. Single toplevel produces exactly one rect.
    {
        let mut layout = constructor();
        let toplevels = [LayoutSurface {
            id: SurfaceId(1),
            preferred_size: (300, 200),
        }];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, SurfaceId(1));
    }

    // 3. Result count equals toplevel count.
    {
        let mut layout = constructor();
        let toplevels = [
            LayoutSurface {
                id: SurfaceId(1),
                preferred_size: (300, 200),
            },
            LayoutSurface {
                id: SurfaceId(2),
                preferred_size: (200, 150),
            },
            LayoutSurface {
                id: SurfaceId(3),
                preferred_size: (400, 300),
            },
        ];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(result.len(), toplevels.len());
        for (i, surface) in toplevels.iter().enumerate() {
            assert_eq!(result[i].0, surface.id);
        }
    }

    // 4. Returned rects are fully inside the output (zero-size rects
    //    pass trivially — the StubLayout default is the zero rect, which
    //    is still inside the output by definition).
    {
        let mut layout = constructor();
        let toplevels = [
            LayoutSurface {
                id: SurfaceId(1),
                preferred_size: (300, 200),
            },
            LayoutSurface {
                id: SurfaceId(2),
                preferred_size: (200, 150),
            },
        ];
        let result = layout.arrange(&toplevels, output, &[]);
        for (_, rect) in &result {
            // Skip the zero-size default — vacuously inside.
            if rect.w == 0 || rect.h == 0 {
                continue;
            }
            let rx2 = rect.x.saturating_add(rect.w as i32);
            let ry2 = rect.y.saturating_add(rect.h as i32);
            let ox2 = output.rect.x.saturating_add(output.rect.w as i32);
            let oy2 = output.rect.y.saturating_add(output.rect.h as i32);
            assert!(
                rect.x >= output.rect.x && rect.y >= output.rect.y && rx2 <= ox2 && ry2 <= oy2,
                "rect {:?} escapes output {:?}",
                rect,
                output.rect
            );
        }
    }

    // 5. Determinism: identical inputs → identical outputs across two
    //    fresh-instance runs.
    {
        let toplevels = [
            LayoutSurface {
                id: SurfaceId(1),
                preferred_size: (300, 200),
            },
            LayoutSurface {
                id: SurfaceId(2),
                preferred_size: (250, 180),
            },
        ];
        let mut layout_a = constructor();
        let mut layout_b = constructor();
        let result_a = layout_a.arrange(&toplevels, output, &[]);
        let result_b = layout_b.arrange(&toplevels, output, &[]);
        assert_eq!(result_a, result_b, "layout must be deterministic");
    }

    // 6. add+remove roundtrip is internally consistent.
    {
        let mut layout = constructor();
        let surface = LayoutSurface {
            id: SurfaceId(7),
            preferred_size: (300, 200),
        };
        layout.on_surface_added(surface);
        layout.on_surface_removed(SurfaceId(7));
        let toplevels = [LayoutSurface {
            id: SurfaceId(1),
            preferred_size: (300, 200),
        }];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(result.len(), 1);
    }

    // 7. focus_changed(Some) then arrange returns valid rects.
    {
        let mut layout = constructor();
        layout.on_focus_changed(Some(SurfaceId(1)));
        let toplevels = [LayoutSurface {
            id: SurfaceId(1),
            preferred_size: (300, 200),
        }];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, SurfaceId(1));
    }
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn floating_layout_centers_single_toplevel() {
        let mut layout = FloatingLayout::new();
        let output = OutputGeometry {
            rect: rect(0, 0, 1024, 768),
        };
        let toplevels = [LayoutSurface {
            id: SurfaceId(1),
            preferred_size: (300, 200),
        }];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(result.len(), 1);
        // Centered: x = (1024 - 300) / 2 = 362; y = (768 - 200) / 2 = 284.
        // Cascade slot 0 → no offset.
        assert_eq!(result[0].1, rect(362, 284, 300, 200));
    }

    #[test]
    fn floating_layout_cascades_multiple_toplevels() {
        let mut layout = FloatingLayout::new();
        let output = OutputGeometry {
            rect: rect(0, 0, 1024, 768),
        };
        let toplevels = [
            LayoutSurface {
                id: SurfaceId(1),
                preferred_size: (300, 200),
            },
            LayoutSurface {
                id: SurfaceId(2),
                preferred_size: (300, 200),
            },
            LayoutSurface {
                id: SurfaceId(3),
                preferred_size: (300, 200),
            },
        ];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(result.len(), 3);
        // First surface centered (slot 0); successive surfaces shifted by
        // CASCADE_OFFSET_PX per slot.
        assert_eq!(result[0].1, rect(362, 284, 300, 200));
        assert_eq!(
            result[1].1,
            rect(362 + CASCADE_OFFSET_PX, 284 + CASCADE_OFFSET_PX, 300, 200)
        );
        assert_eq!(
            result[2].1,
            rect(
                362 + 2 * CASCADE_OFFSET_PX,
                284 + 2 * CASCADE_OFFSET_PX,
                300,
                200
            )
        );
    }

    /// Phase 56 close-out (G.1) — pins the documented "two policies"
    /// rule: the multi-surface path is ordinal-only and does not
    /// consult or advance the persistent counter, so an interleaved
    /// `single → multi → single` sequence keeps the multi-surface
    /// placement at slots {0, 1} regardless of how many single-surface
    /// calls have advanced the counter beforehand.
    #[test]
    fn floating_layout_multi_path_ignores_persistent_counter() {
        let mut layout = FloatingLayout::new();
        let output = OutputGeometry {
            rect: rect(0, 0, 1024, 768),
        };

        // 1. Single-surface call — advances the persistent counter.
        let single = [LayoutSurface {
            id: SurfaceId(1),
            preferred_size: (300, 200),
        }];
        let _ = layout.arrange(&single, output, &[]);
        // Persistent counter is now 1.

        // 2. Multi-surface call — must still place ids 1 and 2 at slots
        //    0 and 1 (call-local indices), NOT 1 and 2 (counter-shifted).
        let multi = [
            LayoutSurface {
                id: SurfaceId(1),
                preferred_size: (300, 200),
            },
            LayoutSurface {
                id: SurfaceId(2),
                preferred_size: (300, 200),
            },
        ];
        let result = layout.arrange(&multi, output, &[]);
        assert_eq!(result.len(), 2);
        // Slot 0: centered.
        assert_eq!(result[0].1, rect(362, 284, 300, 200));
        // Slot 1: one cascade offset.
        assert_eq!(
            result[1].1,
            rect(362 + CASCADE_OFFSET_PX, 284 + CASCADE_OFFSET_PX, 300, 200)
        );

        // 3. A second multi-call later in the same sequence is also
        //    ordinal-only — its placement matches step 2 byte-for-byte
        //    even though the persistent counter has not changed.
        let again = layout.arrange(&multi, output, &[]);
        assert_eq!(again, result);

        // 4. A subsequent single-surface call resumes the persistent
        //    counter at slot 1 (the value left by step 1), proving the
        //    multi-surface path did not touch it.
        let after = layout.arrange(&single, output, &[]);
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].1,
            rect(362 + CASCADE_OFFSET_PX, 284 + CASCADE_OFFSET_PX, 300, 200)
        );
    }

    #[test]
    fn floating_layout_clamps_oversize_to_output() {
        let mut layout = FloatingLayout::new();
        let output = OutputGeometry {
            rect: rect(0, 0, 1024, 768),
        };
        // Preferred size huge — should fall back to ~60% of usable area.
        let toplevels = [LayoutSurface {
            id: SurfaceId(1),
            preferred_size: (10_000, 10_000),
        }];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(result.len(), 1);
        let (_, r) = result[0];
        assert!(
            r.w <= output.rect.w,
            "width {} > output {}",
            r.w,
            output.rect.w
        );
        assert!(
            r.h <= output.rect.h,
            "height {} > output {}",
            r.h,
            output.rect.h
        );
        // 60% of usable: 1024 * 0.6 = 614, 768 * 0.6 = 460.
        assert_eq!(r.w, (1024 * 6) / 10);
        assert_eq!(r.h, (768 * 6) / 10);
    }

    #[test]
    fn floating_layout_subtracts_top_edge_exclusive_zone() {
        let mut layout = FloatingLayout::new();
        let output = OutputGeometry {
            rect: rect(0, 0, 1024, 768),
        };
        // Top-edge bar 24px tall.
        let zones = [rect(0, 0, 1024, 24)];
        let toplevels = [LayoutSurface {
            id: SurfaceId(1),
            preferred_size: (300, 200),
        }];
        let result = layout.arrange(&toplevels, output, &zones);
        assert_eq!(result.len(), 1);
        let (_, r) = result[0];
        // Usable area: y starts at 24, height = 744. Center y =
        // 24 + (744 - 200) / 2 = 24 + 272 = 296.
        assert_eq!(r.y, 296);
        // x is unchanged.
        assert_eq!(r.x, 362);
    }

    #[test]
    fn stub_layout_returns_scripted_rects() {
        let mut layout = StubLayout::new();
        layout.push(rect(0, 0, 100, 100));
        layout.push(rect(100, 100, 200, 200));
        let output = OutputGeometry {
            rect: rect(0, 0, 1024, 768),
        };
        let toplevels = [
            LayoutSurface {
                id: SurfaceId(1),
                preferred_size: (0, 0),
            },
            LayoutSurface {
                id: SurfaceId(2),
                preferred_size: (0, 0),
            },
        ];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(
            result,
            alloc::vec![
                (SurfaceId(1), rect(0, 0, 100, 100)),
                (SurfaceId(2), rect(100, 100, 200, 200)),
            ]
        );
    }

    #[test]
    fn stub_layout_runs_out_of_script_returns_default() {
        let mut layout = StubLayout::new();
        layout.push(rect(0, 0, 100, 100));
        let output = OutputGeometry {
            rect: rect(0, 0, 1024, 768),
        };
        let toplevels = [
            LayoutSurface {
                id: SurfaceId(1),
                preferred_size: (0, 0),
            },
            LayoutSurface {
                id: SurfaceId(2),
                preferred_size: (0, 0),
            },
        ];
        let result = layout.arrange(&toplevels, output, &[]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, rect(0, 0, 100, 100));
        assert_eq!(
            result[1].1,
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0
            }
        );
    }

    #[test]
    fn floating_layout_passes_contract_suite() {
        layout_contract_suite(FloatingLayout::new);
    }

    #[test]
    fn stub_layout_passes_contract_suite() {
        layout_contract_suite(StubLayout::new);
    }

    proptest! {
        #[test]
        fn proptest_arrange_returns_one_rect_per_toplevel(
            count in 0usize..=16usize,
            output_w in 200u32..=2000u32,
            output_h in 200u32..=2000u32,
        ) {
            let mut layout = FloatingLayout::new();
            let output = OutputGeometry { rect: Rect { x: 0, y: 0, w: output_w, h: output_h } };
            let toplevels: Vec<LayoutSurface> = (0..count as u32)
                .map(|i| LayoutSurface { id: SurfaceId(i + 1), preferred_size: (300, 200) })
                .collect();
            let result = layout.arrange(&toplevels, output, &[]);
            prop_assert_eq!(result.len(), count);
            for (i, (id, _)) in result.iter().enumerate() {
                prop_assert_eq!(*id, toplevels[i].id);
            }
        }
    }
}
