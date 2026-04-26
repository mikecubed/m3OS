//! Phase 56 Track C.4 — composer wiring.
//!
//! Per-frame compose pass. Drives [`kernel_core::display::compose::compose_frame`]
//! using the surfaces + buffers held in [`crate::surface::SurfaceRegistry`]
//! and the framebuffer-owner trait impl in [`crate::fb::KernelFramebufferOwner`].
//!
//! The C.4 acceptance criteria require that:
//!
//! * Composition is gated by frame-tick *and* by surface damage. A tick
//!   with no damage produces zero framebuffer writes (verified by the
//!   pure-logic compose tests in `kernel_core` against
//!   `RecordingFramebufferOwner`).
//! * Layer ordering is the canonical
//!   `Background < Bottom < Toplevel < Top < Overlay < Cursor`. The
//!   pure-logic core enforces this; the wiring just supplies surfaces.
//! * The wiring consumes [`FramebufferOwner`] and [`LayoutPolicy`] by
//!   trait, not concrete type. The same compose code therefore runs
//!   against `RecordingFramebufferOwner` on the host and
//!   `KernelFramebufferOwner` in QEMU (per C.4 acceptance bullet
//!   "no GL/GLES2 code paths").

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::compose::{ComposeError, ComposeSurface, compose_frame};
use kernel_core::display::fb_owner::FramebufferOwner;
use kernel_core::display::layout::{FloatingLayout, LayoutPolicy, LayoutSurface, OutputGeometry};
use kernel_core::display::protocol::Rect;

use crate::surface::SurfaceRegistry;

/// Runs the per-frame compose pass.
///
/// Returns the number of framebuffer writes issued (which is also the
/// number of damaged regions blitted — `compose_frame` issues at most one
/// `write_pixels` per damage rect).
pub fn run_compose<O: FramebufferOwner, L: LayoutPolicy>(
    owner: &mut O,
    layout: &mut L,
    registry: &mut SurfaceRegistry,
) -> Result<usize, ComposeError> {
    if !registry.has_damage() {
        return Ok(0);
    }
    let meta = owner.metadata();
    let output = Rect {
        x: 0,
        y: 0,
        w: meta.width,
        h: meta.height,
    };

    // Inform the layout policy about toplevel surfaces. For Phase 56 the
    // default `FloatingLayout` centers each toplevel; the result is
    // currently unused (the surface shim already centres entries) but
    // keeping the call ensures the seam is exercised on every frame.
    let toplevels: Vec<LayoutSurface> = registry
        .iter_compose(output)
        .iter()
        .filter(|e| {
            matches!(
                e.layer,
                kernel_core::display::compose::ComposeLayer::Toplevel
            )
        })
        .map(|e| LayoutSurface {
            id: e.id,
            preferred_size: (e.buf.width, e.buf.height),
        })
        .collect();
    let _arrangement = layout.arrange(&toplevels, OutputGeometry { rect: output }, &[]);

    let entries = registry.iter_compose(output);
    if entries.is_empty() {
        registry.mark_clean();
        return Ok(0);
    }

    // Build full-surface damage rectangles. Phase 56 ships full-surface
    // damage on every commit; later phases tracking partial damage will
    // replace this with the real list emitted by the surface state machine.
    // Rationale: keeps the demo simple, costs at most one full blit per
    // damaged surface per tick, and the pure-logic composer is the gate
    // that turns this into actual framebuffer writes.
    let damages: Vec<[Rect; 1]> = entries
        .iter()
        .map(|e| {
            [Rect {
                x: 0,
                y: 0,
                w: e.buf.width,
                h: e.buf.height,
            }]
        })
        .collect();

    let mut compose: Vec<ComposeSurface<'_>> = Vec::with_capacity(entries.len());
    for (entry, dmg) in entries.iter().zip(damages.iter()) {
        compose.push(ComposeSurface {
            id: entry.id,
            layer: entry.layer,
            rect: entry.rect,
            damage: &dmg[..],
            pixels: &entry.buf.pixels,
            opaque: entry.is_opaque(),
        });
    }

    let writes = compose_frame(owner, output, &mut compose)?;
    registry.mark_clean();
    Ok(writes)
}

/// Construct the default Phase 56 layout policy. Re-exported as a named
/// factory so future phases can replace it without changing callers.
pub fn default_layout() -> impl LayoutPolicy {
    FloatingLayout::new()
}
