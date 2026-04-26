//! Phase 56 Track C.4 + E.3 — composer wiring.
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
//!
//! ## E.3 — pointer cursor rendering
//!
//! After [`compose_frame`] has blitted every regular surface, this
//! module samples a [`CursorRenderer`] at the current pointer
//! position (minus the renderer's hotspot) and writes the cursor
//! pixels into the framebuffer in the **top-most layer**. Transparent
//! samples (`0`) skip the framebuffer write so the surface beneath
//! shows through.
//!
//! When no client has set a `Cursor`-role surface, the composer falls
//! back to [`DefaultArrowCursor`] — Phase 56 always renders a visible
//! cursor so a fresh boot is not a black screen with an invisible
//! pointer.
//!
//! ### Damage tracking
//!
//! The previous pointer position is tracked across frames via
//! [`ComposeContext`]. When the pointer moves, [`cursor_damage`]
//! returns the union of "old cursor box" + "new cursor box"; this
//! marks the underlying surfaces dirty so the composer re-blits the
//! pixels under the old cursor and over the new — preventing stale
//! cursor trails.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::compose::{ComposeError, ComposeSurface, compose_frame};
use kernel_core::display::cursor::{CursorRenderer, DefaultArrowCursor, cursor_damage};
use kernel_core::display::fb_owner::{FbError, FramebufferOwner, bytes_per_pixel};
use kernel_core::display::layout::{FloatingLayout, LayoutPolicy, LayoutSurface, OutputGeometry};
use kernel_core::display::protocol::Rect;

use crate::surface::SurfaceRegistry;

/// Per-frame composer state that survives across calls.
///
/// Right now this is just the previous pointer position so
/// [`cursor_damage`] knows what to clear; the field is grouped behind
/// a struct so future per-frame state (frame-stats sample, layout
/// hash, ...) does not require a new function-arg per frame.
#[derive(Clone, Copy, Debug)]
pub struct ComposeContext {
    /// Pointer position at the end of the *previous* compose pass.
    /// `None` on the very first frame — the cursor is drawn at the
    /// current position with no "prev" damage. Subsequent frames
    /// fold this into [`cursor_damage`].
    prev_pointer: Option<(i32, i32)>,
    /// Cursor size at the end of the previous compose pass. Tracked
    /// alongside `prev_pointer` so a client-cursor swap (which
    /// changes the bitmap dimensions) computes correct damage.
    prev_cursor_size: Option<(u32, u32)>,
}

impl ComposeContext {
    /// Construct an empty context. The first frame's `cursor_damage`
    /// call returns `None` for `prev`, so only the new cursor's box
    /// is damaged on frame 1.
    pub const fn new() -> Self {
        Self {
            prev_pointer: None,
            prev_cursor_size: None,
        }
    }
}

impl Default for ComposeContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs the per-frame compose pass.
///
/// Returns the number of framebuffer writes issued. The cursor blit
/// counts as one write per non-transparent row of the cursor bitmap
/// (the cursor is sampled per pixel but written per contiguous opaque
/// run).
///
/// Phase 56 E.3 contract: the cursor is **always** rendered. If the
/// caller passes `None` for `client_cursor`, a [`DefaultArrowCursor`]
/// stands in. This prevents an invisible pointer at boot.
pub fn run_compose<O: FramebufferOwner, L: LayoutPolicy>(
    owner: &mut O,
    layout: &mut L,
    registry: &mut SurfaceRegistry,
    ctx: &mut ComposeContext,
    pointer_position: (i32, i32),
) -> Result<usize, ComposeError> {
    let meta = owner.metadata();
    let output = Rect {
        x: 0,
        y: 0,
        w: meta.width,
        h: meta.height,
    };

    // Phase 56 E.3 — cursor selection: use the client-supplied cursor
    // if any, else the built-in arrow. We clone the client cursor
    // (cheap — at most a small `Vec<u32>`) so the borrow on
    // `registry` is released before the mutating compose path runs.
    let default = DefaultArrowCursor::new();
    let client_cursor_clone = registry.client_cursor().cloned();
    let cursor_size = match &client_cursor_clone {
        Some(cc) => cc.size(),
        None => default.size(),
    };

    // Compute pointer-motion damage. If the pointer moved (or the
    // cursor swapped from default to client, changing size), we must
    // re-blit the underlying surfaces under both the old and new
    // cursor rects so stale cursor pixels are overpainted.
    let prev_pos = ctx.prev_pointer;
    let prev_size = ctx.prev_cursor_size;
    // `cursor_motion` is true when the cursor needs a redraw —
    // either there was real motion, or this is the first frame and
    // we have to draw the cursor at all.
    let cursor_motion = match (prev_pos, prev_size) {
        (Some(prev), Some(psize)) => {
            !cursor_damage(prev, psize, pointer_position, cursor_size).is_empty()
        }
        // First frame, or `prev` lost: always treat as needing a
        // redraw so the cursor is drawn even with no surface-level
        // damage.
        _ => cursor_size.0 > 0 && cursor_size.1 > 0,
    };

    // Gate: skip compose work if there is no surface damage AND no
    // cursor motion. The frame is a no-op.
    //
    // Trade-off: when only the cursor moved (no surface damage), the
    // path below still walks every mapped surface and re-blits with
    // full-surface damage rectangles. That is correct (cursor pixels
    // are composited on top, so the underlying surfaces must be
    // re-emitted under the old + new cursor rects) but wastes work
    // when the cursor box is small relative to surface area. A
    // dedicated cursor-only fast path that re-blits only the damaged
    // regions of the underlying surfaces is deferred to a Phase 56
    // follow-up; today every mouse move triggers a full repaint of
    // every mapped surface.
    let surface_damage = registry.has_damage();
    if !surface_damage && !cursor_motion {
        return Ok(0);
    }

    // Inform the layout policy about toplevel surfaces and the exclusive
    // zones declared by mapped `Layer` surfaces (E.2). The arrangement
    // is currently unused (the surface shim still centres entries) but
    // feeding `arrange` the real `exclusive_zones` list on every frame
    // keeps the seam exercised and ensures `FloatingLayout`'s
    // `usable_rect` shrinking is on for any future tiling layout that
    // honours `LayoutPolicy::arrange` output.
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
    let exclusive_zones = registry.exclusive_zones(output);
    let arrangement = layout.arrange(
        &toplevels,
        OutputGeometry { rect: output },
        &exclusive_zones,
    );

    let entries = registry.iter_compose(output);
    if entries.is_empty() && !cursor_motion {
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
        // Phase 56 close-out (G.1) — Toplevel surfaces use the layout
        // policy's arrangement for placement; Layer / Cursor surfaces
        // keep their iter_compose-derived rects (anchor-driven for
        // Layer, hotspot-driven for Cursor). Without this lookup, the
        // multi-client coexistence regression would render every
        // Toplevel at the same `centre_rect` position and only the
        // top-of-z-order surface would be observable.
        let rect = if matches!(
            entry.layer,
            kernel_core::display::compose::ComposeLayer::Toplevel
        ) {
            arrangement
                .iter()
                .find(|(id, _)| *id == entry.id)
                .map(|(_, r)| *r)
                .unwrap_or(entry.rect)
        } else {
            entry.rect
        };
        compose.push(ComposeSurface {
            id: entry.id,
            layer: entry.layer,
            rect,
            damage: &dmg[..],
            pixels: &entry.buf.pixels,
            opaque: entry.is_opaque(),
        });
    }

    let surface_writes = compose_frame(owner, output, &mut compose)?;
    registry.mark_clean();

    // Phase 56 E.3 — blit the cursor on top.
    //
    // Rationale: `compose_frame` already presented the surfaces.
    // We sample the cursor pixel-by-pixel and write directly to the
    // framebuffer, then call `present()` again. (The kernel
    // framebuffer's `present` is currently a no-op default impl, so
    // the duplicate is free; future hardware paths that swap on
    // present will need to be careful here.)
    let cursor: &dyn CursorRenderer = match &client_cursor_clone {
        Some(cc) => cc,
        None => &default,
    };
    let cursor_writes =
        blit_cursor(owner, output, cursor, pointer_position).map_err(ComposeError::from)?;
    if cursor_writes > 0 {
        owner.present().map_err(ComposeError::from)?;
    }

    // Update the per-frame state for the next call's `cursor_damage`.
    ctx.prev_pointer = Some(pointer_position);
    ctx.prev_cursor_size = Some(cursor_size);

    Ok(surface_writes + cursor_writes)
}

/// Sample a [`CursorRenderer`] over the screen rectangle implied by
/// `pointer_position - hotspot()` and the cursor's size, and write
/// every non-transparent pixel into the framebuffer. Pixels with
/// sample value `0` are skipped (transparent — let the underlying
/// surface show through).
///
/// Returns the number of `write_pixels` calls issued — useful for
/// frame-stats / observability.
fn blit_cursor<O: FramebufferOwner>(
    owner: &mut O,
    output: Rect,
    cursor: &dyn CursorRenderer,
    pointer_position: (i32, i32),
) -> Result<usize, FbError> {
    let bpp = bytes_per_pixel(owner.metadata().pixel_format);
    let bpp_usize = bpp as usize;
    let (cw, ch) = cursor.size();
    if cw == 0 || ch == 0 {
        return Ok(0);
    }
    let (hx, hy) = cursor.hotspot();
    // Origin of the cursor bitmap in screen coordinates. Widen to
    // i64 so adversarial pointer positions near i32::MAX can't wrap.
    let origin_x = (pointer_position.0 as i64).saturating_sub(hx as i64);
    let origin_y = (pointer_position.1 as i64).saturating_sub(hy as i64);
    let output_x = output.x as i64;
    let output_y = output.y as i64;
    let output_x2 = output_x + (output.w as i64);
    let output_y2 = output_y + (output.h as i64);

    let mut writes = 0usize;
    // Scratch buffer for the contiguous opaque-pixel run currently
    // being assembled. Hoisted out of the row / run loops so we
    // allocate at most once per call (the worst case is one full
    // row of opaque pixels: `cw * bpp_usize`). `clear()` drops the
    // length without touching capacity, so subsequent runs reuse the
    // backing storage.
    let max_run_bytes = (cw as usize).saturating_mul(bpp_usize);
    let mut run_pixels: Vec<u8> = Vec::with_capacity(max_run_bytes);
    // Walk the cursor bitmap row-by-row. Within a row, batch
    // contiguous opaque pixels into a single `write_pixels` call so
    // a fully-opaque arrow takes one call per row instead of one per
    // pixel.
    for cy in 0..ch {
        let screen_y = origin_y + (cy as i64);
        if screen_y < output_y || screen_y >= output_y2 {
            continue;
        }
        let mut cx = 0u32;
        while cx < cw {
            // Skip transparent pixels.
            while cx < cw {
                let s = cursor.sample(cx, cy);
                if s != 0 {
                    break;
                }
                cx += 1;
            }
            if cx >= cw {
                break;
            }
            let run_start = cx;
            // Collect contiguous opaque pixels into the scratch buffer.
            run_pixels.clear();
            while cx < cw {
                let s = cursor.sample(cx, cy);
                if s == 0 {
                    break;
                }
                let bytes = s.to_le_bytes();
                run_pixels.extend_from_slice(&bytes[..bpp_usize.min(bytes.len())]);
                // If the FB requires more bytes than `u32` provides
                // (e.g. 8 bytes-per-pixel), pad with zeros. Phase 56
                // only supports 4-bpp formats but the saturating
                // path keeps the math defensive.
                if bpp_usize > bytes.len() {
                    for _ in bytes.len()..bpp_usize {
                        run_pixels.push(0);
                    }
                }
                cx += 1;
            }
            // Compute the screen rectangle for this run.
            let screen_x_start = origin_x + (run_start as i64);
            let screen_x_end = origin_x + (cx as i64);
            // Clip to output.
            let clipped_x = screen_x_start.max(output_x);
            let clipped_x_end = screen_x_end.min(output_x2);
            if clipped_x >= clipped_x_end {
                continue;
            }
            let skip_left = (clipped_x - screen_x_start) as usize;
            let take_pixels = (clipped_x_end - clipped_x) as usize;
            let take_bytes = take_pixels * bpp_usize;
            let pixel_bytes_start = skip_left * bpp_usize;
            let pixel_bytes_end = pixel_bytes_start + take_bytes;
            if pixel_bytes_end > run_pixels.len() {
                continue;
            }
            let row_slice = &run_pixels[pixel_bytes_start..pixel_bytes_end];
            // i32 fits the clipped values: clipped_x ≥ output_x ≥ 0
            // (output is anchored at origin in Phase 56), and
            // clipped_x < output_x2 ≤ i32::MAX.
            let dst_x = match i32::try_from(clipped_x) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let dst_y = match i32::try_from(screen_y) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let dst_w = match u32::try_from(take_pixels) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let stride = dst_w * bpp;
            owner.write_pixels(
                Rect {
                    x: dst_x,
                    y: dst_y,
                    w: dst_w,
                    h: 1,
                },
                row_slice,
                stride,
            )?;
            writes += 1;
        }
    }
    Ok(writes)
}

/// Construct the default Phase 56 layout policy. Re-exported as a named
/// factory so future phases can replace it without changing callers.
pub fn default_layout() -> impl LayoutPolicy {
    FloatingLayout::new()
}
