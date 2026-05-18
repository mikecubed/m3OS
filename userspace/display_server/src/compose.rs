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
use kernel_core::display::damage::DamageTracker;
use kernel_core::display::fb_owner::{FbError, FramebufferOwner, bytes_per_pixel};
use kernel_core::display::layout::{FloatingLayout, LayoutPolicy, LayoutSurface, OutputGeometry};
use kernel_core::display::protocol::{Rect, SurfaceId};

use crate::surface::SurfaceRegistry;

/// Per-frame composer state that survives across calls.
///
/// Right now this is just the previous pointer position so
/// [`cursor_damage`] knows what to clear; the field is grouped behind
/// a struct so future per-frame state (frame-stats sample, layout
/// hash, ...) does not require a new function-arg per frame.
#[derive(Clone, Debug)]
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
    /// Cached toplevel id list from the previous compose pass. The
    /// arrangement is only recomputed when this changes (set of
    /// surfaces added or removed) — `FloatingLayout::arrange`
    /// advances its persistent cascade slot on every single-surface
    /// call, so calling it every frame for the same surface set
    /// would teleport the surface across the screen each frame.
    /// Caching here keeps the placement stable for an unchanged
    /// toplevel set.
    cached_toplevel_ids: Vec<SurfaceId>,
    /// Arrangement cached alongside `cached_toplevel_ids`. Empty
    /// until the first compose pass populates it.
    cached_arrangement: Vec<(SurfaceId, Rect)>,
    /// Phase 68 Track B — accumulated damage rectangles for the
    /// current frame. `mark_dirty` is called once per surface and
    /// once per cursor old/new bounding box; `union_rect` is the
    /// blit clip rectangle.
    damage_tracker: DamageTracker,
}

impl ComposeContext {
    /// Construct an empty context. The first frame's `cursor_damage`
    /// call returns `None` for `prev`, so only the new cursor's box
    /// is damaged on frame 1.
    pub fn new() -> Self {
        Self {
            prev_pointer: None,
            prev_cursor_size: None,
            cached_toplevel_ids: Vec::new(),
            cached_arrangement: Vec::new(),
            damage_tracker: DamageTracker::new(),
        }
    }

    /// Phase 68 Track B — exposed for diagnostics and tests so a
    /// reviewer can confirm the tracker is being populated each frame.
    pub fn damage_tracker(&self) -> &DamageTracker {
        &self.damage_tracker
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
    // Phase 68 Track B partially closed the "every cursor move = full
    // repaint" trade-off documented at this site through Phase 56:
    // the cursor-only fast path below now runs a *clipped* compose
    // whose damage is the union of old + new cursor boxes (so a
    // cursor-only frame writes strictly fewer pixels than a full
    // repaint). The wider trade-off is still partly open: the
    // [`DamageTracker`] accumulates per-surface dirty rects + cursor
    // motion as a diagnostic / observability seam (exposed via
    // [`ComposeContext::damage_tracker`] for tests and debug dumps),
    // but the *general-path* surface blit at the bottom of this
    // function still emits a full-surface damage rect per entry — it
    // does not yet consult [`DamageTracker::union_rect`] /
    // [`DamageTracker::is_full_repaint_needed`] to clip those blits.
    // Wiring the tracker into the general-path clip decision is a
    // documented Phase 68 follow-up.
    let surface_damage = registry.has_damage();
    if !surface_damage && !cursor_motion {
        return Ok(0);
    }

    // Phase 68 Track B — feed cursor-motion damage into the tracker so
    // a cursor-only frame's `union_rect` is the union of (old cursor
    // box + new cursor box). Surfaces feed their entry rects below.
    if cursor_motion
        && let (Some(prev_pos), Some(prev_size)) = (ctx.prev_pointer, ctx.prev_cursor_size)
    {
        for rect in cursor_damage(prev_pos, prev_size, pointer_position, cursor_size) {
            ctx.damage_tracker.mark_dirty(rect);
        }
    }
    if cursor_motion && (ctx.prev_pointer.is_none() || ctx.prev_cursor_size.is_none()) {
        // First-frame cursor — only the new cursor's bounding box is
        // dirty. The first-compose `clear_rect_to_background(output)`
        // below already triggers `mark_full_repaint` so this path is
        // mostly diagnostic, but adding the new-cursor rect keeps the
        // tracker accurate if the first-compose wipe is ever skipped.
        let (cw, ch) = cursor_size;
        if cw > 0 && ch > 0 {
            ctx.damage_tracker.mark_dirty(Rect {
                x: pointer_position.0.saturating_sub(cw as i32 / 2),
                y: pointer_position.1.saturating_sub(ch as i32 / 2),
                w: cw,
                h: ch,
            });
        }
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
    // Recompute `arrange` only when the toplevel id set actually
    // changed. `FloatingLayout` advances its persistent
    // `cascade_slot` on every single-surface call (a documented
    // legacy contract pinned by the kernel-core tests), so a
    // per-frame `arrange` call would cycle a single surface through
    // 8 cascade slots once per frame — visible as the surface
    // teleporting across the screen and `compose_frame` painting
    // its pixels at a moving target instead of a stable spot.
    let toplevel_ids: Vec<SurfaceId> = toplevels.iter().map(|s| s.id).collect();
    if toplevel_ids != ctx.cached_toplevel_ids {
        ctx.cached_arrangement = layout.arrange(
            &toplevels,
            OutputGeometry { rect: output },
            &exclusive_zones,
        );
        ctx.cached_toplevel_ids = toplevel_ids;
    }
    let arrangement = &ctx.cached_arrangement;

    let entries = registry.iter_compose(output);

    // First-compose-pass background wipe. On the first call (and again
    // any time `ComposeContext` was reset, e.g. on client close) the
    // framebuffer still holds whatever the kernel framebuffer console
    // wrote at boot. Without an explicit whole-screen clear, the
    // surface- and cursor-blit passes would paint *over* that stale
    // image, leaving boot-log text peeking through outside mapped
    // surface bounds. Mirrors the startup wipe `display_server::main`
    // does after `framebuffer_mmap` so the two cases converge on the
    // same clean background.
    let is_first_compose = ctx.prev_pointer.is_none() || ctx.prev_cursor_size.is_none();
    if is_first_compose {
        clear_rect_to_background(owner, output)?;
        // Phase 68 Track B — explicit full-repaint on the first
        // compose pass: the surface-blit path below will paint every
        // mapped surface and the tracker must reflect that so a
        // subsequent reviewer can confirm "first frame = full repaint"
        // via `damage_tracker().is_full_repaint_needed()` at the seam.
        ctx.damage_tracker.mark_full_repaint();
    } else if cursor_motion
        && let (Some(prev_pos), Some(prev_size)) = (ctx.prev_pointer, ctx.prev_cursor_size)
    {
        // Cursor-trail fix (Phase 56 follow-up): when the cursor moved,
        // `blit_cursor` only paints the *new* cursor position. The old
        // cursor's opaque pixels would remain on the framebuffer as a
        // stale "arrow trail" anywhere the underlying surface-blit pass
        // does not repaint over them — i.e. all background area outside
        // mapped surfaces. Explicitly clear the union of old + new
        // cursor damage rects before the surface and cursor passes run.
        //
        // Inside mapped-surface bounds the clear is overpainted by the
        // surface-blit pass below; outside, it stays as the cleared
        // background. Either way the old cursor pixels are gone before
        // `blit_cursor` paints the new position.
        let damage = cursor_damage(prev_pos, prev_size, pointer_position, cursor_size);
        for rect in damage {
            clear_rect_to_background(owner, rect)?;
        }
    }

    if entries.is_empty() && !cursor_motion {
        registry.mark_clean();
        return Ok(0);
    }

    // Phase 68 Track B — cursor-only fast path. When no surface
    // emitted new pixels but the cursor moved, skip the full-surface
    // compose pass: the mapped surfaces are identical to last frame,
    // so re-blitting every one is the wasted work the deferred-fast-
    // path note at this site flagged through Phase 56.
    //
    // The cursor-trail clear above wiped the union of old + new
    // cursor boxes to background. Where that union overlaps a mapped
    // surface, the cleared pixels would otherwise be left as
    // background and produce a visible "hole" trail across the
    // window. To prevent that, we still run `compose_frame` here, but
    // with the per-surface `damage` array narrowed to the cursor
    // union (translated into each surface's local coordinates). The
    // result: each mapped surface contributes at most a cursor-sized
    // re-blit, which together with the cursor blit is strictly fewer
    // pixels than the framebuffer resolution for any non-pathological
    // cursor size — the optimisation the fast path was designed for,
    // without the hole-trail regression.
    if !surface_damage && cursor_motion && !is_first_compose {
        let cursor: &dyn CursorRenderer = match &client_cursor_clone {
            Some(cc) => cc,
            None => &default,
        };

        // `cursor_motion && !is_first_compose` is the conjunction that
        // gated the cursor-trail clear above; both `prev_pointer` and
        // `prev_cursor_size` are guaranteed `Some(_)` here. The
        // `expect`s document the invariant so a future refactor that
        // weakens the gate breaks loudly instead of silently writing
        // garbage damage rects.
        let prev_pos = ctx.prev_pointer.expect(
            "cursor fast path: prev_pointer set whenever cursor_motion && !is_first_compose",
        );
        let prev_size = ctx.prev_cursor_size.expect(
            "cursor fast path: prev_cursor_size set whenever cursor_motion && !is_first_compose",
        );
        let cursor_repaint = cursor_damage(prev_pos, prev_size, pointer_position, cursor_size);

        // Per-surface damage = cursor union translated into surface-
        // local coordinates. `compose_frame` clips each local rect
        // against the surface's `[0..w] × [0..h]` extent, so a
        // surface that does not overlap the cursor union contributes
        // zero blits (its local damage list survives the clip as
        // empty / out-of-bounds and is skipped).
        let local_damages: Vec<Vec<Rect>> = entries
            .iter()
            .map(|entry| {
                let surface_rect = surface_screen_rect(entry, arrangement);
                cursor_repaint
                    .iter()
                    .filter_map(|sr| {
                        let dx = (sr.x as i64) - (surface_rect.x as i64);
                        let dy = (sr.y as i64) - (surface_rect.y as i64);
                        let lx = i32::try_from(dx).ok()?;
                        let ly = i32::try_from(dy).ok()?;
                        Some(Rect {
                            x: lx,
                            y: ly,
                            w: sr.w,
                            h: sr.h,
                        })
                    })
                    .collect()
            })
            .collect();

        let snapshots: Vec<Vec<u8>> = entries
            .iter()
            .map(|entry| {
                // 2026-05-18 less-render flake — bracket the snapshot
                // with `DC:snap-*` tags so the next investigator can
                // grep for overlaps against the term `TC:compose-*`
                // markers and decide whether the residual flake is
                // still snapshot-during-write. `snap-nonzero` reports
                // the count of non-zero bytes in the captured snapshot:
                // when that hits zero with no Clear in the recent
                // compose history, the d899f73-class buffer-leaked-to-
                // zero bug is back on a different code path.
                trace_snap_event(b"DC:snap-start sid=", entry.id);
                let snapshot = entry.buf.pixels_snapshot();
                trace_snap_event(b"DC:snap-end sid=", entry.id);
                trace_snap_nonzero(entry.id, &snapshot);
                snapshot
            })
            .collect();

        let mut compose: Vec<ComposeSurface<'_>> = Vec::with_capacity(entries.len());
        for ((entry, dmg), snapshot) in entries
            .iter()
            .zip(local_damages.iter())
            .zip(snapshots.iter())
        {
            compose.push(ComposeSurface {
                id: entry.id,
                layer: entry.layer,
                rect: surface_screen_rect(entry, arrangement),
                damage: &dmg[..],
                pixels: snapshot.as_slice(),
                opaque: entry.is_opaque(),
            });
        }

        let surface_writes = compose_frame(owner, output, &mut compose)?;
        let cursor_writes =
            blit_cursor(owner, output, cursor, pointer_position).map_err(ComposeError::from)?;
        if surface_writes + cursor_writes > 0 {
            owner.present().map_err(ComposeError::from)?;
        }
        ctx.prev_pointer = Some(pointer_position);
        ctx.prev_cursor_size = Some(cursor_size);
        // Keep the damage tracker observable to tests but clear it
        // so the next frame starts empty.
        ctx.damage_tracker.reset();
        return Ok(surface_writes + cursor_writes);
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

    // Phase 68 Track B — feed each mapped surface's screen rect into
    // the tracker so the union covers everything the compose pass
    // will touch. `union_rect()` is observable from outside this
    // function via [`ComposeContext::damage_tracker`] for tests and
    // debug dumps; the cursor-only fast path above already returned
    // by this point.
    for entry in entries.iter() {
        ctx.damage_tracker.mark_dirty(entry.rect);
    }

    // Snapshot the current pixel contents of every compose entry into
    // owned `Vec<u8>` buffers before building the `ComposeSurface`
    // borrows. SHM-backed surfaces are mapped into both the client's
    // and the compositor's address spaces; the client edits pixels
    // in place, which the Rust compiler treats as a *non-volatile*
    // memory access through `&[u8]`. Without an explicit per-compose
    // copy LLVM is free to assume that two reads of the same byte in
    // the same compose pass yield the same value — a soundness
    // assumption the cross-process editing pattern violates. Copying
    // here freezes the snapshot for the duration of `compose_frame`,
    // is observably-correct (each compose sees a coherent point-in-
    // time view of the buffer), and adds a single 1 MiB memcpy per
    // 60 Hz frame for the term surface — well below the budget the
    // chunked-pixel path used to consume.
    // `pixels_snapshot` performs a raw-pointer copy for shared-memory
    // buffers — no `&[u8]` ever aliases the producer's mapping, so
    // the snapshot is sound under Rust's aliasing model. Torn reads
    // are inherent to the cross-process editing pattern; the
    // resulting `Vec<u8>` is a single-point-in-time view that the
    // rest of the compose pass treats as stable.
    let snapshots: Vec<Vec<u8>> = entries
        .iter()
        .map(|entry| {
            // 2026-05-18 less-render flake — see the fast-path block
            // above for the rationale; the general-path snapshot
            // gets the same instrumentation so a probe run that hits
            // either compose pass produces overlap data.
            trace_snap_event(b"DC:snap-start sid=", entry.id);
            let snapshot = entry.buf.pixels_snapshot();
            trace_snap_event(b"DC:snap-end sid=", entry.id);
            trace_snap_nonzero(entry.id, &snapshot);
            snapshot
        })
        .collect();

    let mut compose: Vec<ComposeSurface<'_>> = Vec::with_capacity(entries.len());
    for ((entry, dmg), snapshot) in entries.iter().zip(damages.iter()).zip(snapshots.iter()) {
        compose.push(ComposeSurface {
            id: entry.id,
            layer: entry.layer,
            rect: surface_screen_rect(entry, arrangement),
            damage: &dmg[..],
            pixels: snapshot.as_slice(),
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

    // Phase 68 Track B — clear the damage tracker now that the
    // compose pass has consumed everything. The next frame starts
    // empty; cursor motion / surface commits repopulate it.
    ctx.damage_tracker.reset();

    Ok(surface_writes + cursor_writes)
}

/// Resolve a `ComposeEntry`'s screen-space rect, honouring the layout
/// policy's arrangement for `Toplevel` surfaces while leaving
/// `Layer` / `Cursor` / `Background` rects unchanged.
///
/// Phase 56 close-out (G.1): without this lookup, every `Toplevel`
/// would composite at the same `centre_rect` position and only the
/// top-of-z-order surface would be observable in the multi-client
/// coexistence regression. The cursor-only fast path also calls this
/// to keep the surface→cursor-damage translation consistent with the
/// slow-path placement.
fn surface_screen_rect(
    entry: &crate::surface::ComposeEntry<'_>,
    arrangement: &[(SurfaceId, Rect)],
) -> Rect {
    if matches!(
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
    }
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

/// Fill `rect` on the framebuffer with the compositor's background
/// colour ([`crate::BG_PIXEL`]). Used by the cursor-trail fix —
/// the union of old + new cursor damage rects is cleared before the
/// surface compose + cursor blit run, so the framebuffer doesn't
/// accumulate stale arrow pixels from the previous frame.
///
/// The colour matches the initial-fill value `display_server` writes
/// at startup so the cleared region blends seamlessly into the
/// untouched background. If the two were different, the cursor
/// would leave coloured rectangles wherever it had been on the
/// background (e.g. opaque-black squares on a teal background).
///
/// Phase 56 ships only 4-bpp pixel formats (BGRA8888 / RGBA8888);
/// `BG_PIXEL.to_le_bytes()` writes one little-endian u32 per pixel
/// matching either layout's interpretation of the bytes.
fn clear_rect_to_background<O: FramebufferOwner>(owner: &mut O, rect: Rect) -> Result<(), FbError> {
    let bpp = bytes_per_pixel(owner.metadata().pixel_format) as usize;
    let pixel_count = (rect.w as usize).saturating_mul(rect.h as usize);
    let total = pixel_count.saturating_mul(bpp);
    if total == 0 {
        return Ok(());
    }
    let pixel_bytes = crate::BG_PIXEL.to_le_bytes();
    let mut buf: Vec<u8> = Vec::with_capacity(total);
    for _ in 0..pixel_count {
        let take = bpp.min(pixel_bytes.len());
        buf.extend_from_slice(&pixel_bytes[..take]);
        for _ in take..bpp {
            buf.push(0);
        }
    }
    let stride = (rect.w as u32).saturating_mul(bpp as u32);
    owner.write_pixels(rect, &buf, stride)
}

/// Fill the entire framebuffer with [`crate::BG_PIXEL`].
///
/// Called by `display_server::main` once at startup, immediately after
/// `framebuffer_mmap` succeeds, so the kernel framebuffer console's
/// boot-log text is wiped before any surface composes. The first-frame
/// path inside [`run_compose`] does the same job when
/// `ComposeContext::prev_pointer` is `None` — keeping both paths means
/// a fresh boot is clean *and* a registry / context reset (e.g. on
/// client close) recovers a clean background on the very next compose.
pub fn fill_background<O: FramebufferOwner>(owner: &mut O) -> Result<(), FbError> {
    let meta = owner.metadata();
    clear_rect_to_background(
        owner,
        Rect {
            x: 0,
            y: 0,
            w: meta.width,
            h: meta.height,
        },
    )
}

/// Construct the default Phase 56 layout policy. Re-exported as a named
/// factory so future phases can replace it without changing callers.
pub fn default_layout() -> impl LayoutPolicy {
    FloatingLayout::new()
}

/// 2026-05-18 less-render flake — write a one-line probe trace to the
/// serial console tagging a snapshot boundary with the surface id and
/// the current microsecond timestamp. Format: `<tag><sid> <us>\n`.
/// Pairs with `TC:compose-*` in `userspace/term/src/main.rs` so a
/// single serial.log captures both producer and consumer timing.
fn trace_snap_event(tag: &[u8], sid: SurfaceId) {
    let (sec, nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
    let sec_u = sec.max(0) as u64;
    let nsec_u = nsec.max(0) as u64;
    let us = sec_u
        .saturating_mul(1_000_000)
        .saturating_add(nsec_u / 1_000);
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, tag);
    write_decimal_u64(sid.0 as u64);
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b" ");
    write_decimal_u64(us);
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b"\n");
}

/// 2026-05-18 less-render flake — count non-zero bytes in the captured
/// snapshot and log them as `DC:snap-nonzero sid=<n> nz=<count>/<len>\n`.
/// Catches the d899f73-class regression: if `<count>` drops to zero
/// without a Clear in the recent compose history, a buffer is being
/// leaked to the wire at SHM-create zeros. Iterates the full snapshot
/// (≈ 4 MiB for the term surface), but only on probe runs — the cost
/// is ~6 ms per compose tick at 4 MiB on QEMU TCG, easily worth the
/// diagnostic fidelity for one-off flake hunts. Remove after the
/// residual flake is closed.
fn trace_snap_nonzero(sid: SurfaceId, snapshot: &[u8]) {
    let nz = snapshot.iter().filter(|b| **b != 0).count();
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b"DC:snap-nonzero sid=");
    write_decimal_u64(sid.0 as u64);
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b" nz=");
    write_decimal_u64(nz as u64);
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b"/");
    write_decimal_u64(snapshot.len() as u64);
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, b"\n");
}

/// Inline decimal-encode + write to STDOUT. Allocation-free; the
/// 20-byte buffer covers the full `u64` range.
fn write_decimal_u64(value: u64) {
    let mut buf = [0u8; 20];
    let mut idx = buf.len();
    let mut n = value;
    loop {
        idx -= 1;
        buf[idx] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let _ = syscall_lib::write(syscall_lib::STDOUT_FILENO, &buf[idx..]);
}
