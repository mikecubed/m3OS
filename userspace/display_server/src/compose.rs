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

    /// Phase 72b — request a full-screen repaint on the next compose
    /// pass. Called from the surface-lifecycle path in `main.rs` when
    /// a Toplevel surface is destroyed, so stale pixels left in the
    /// framebuffer by the dying surface (or by greeter on logout) get
    /// cleared instead of bleeding through the gaps between live
    /// tiles. `run_compose_filtered`'s first-compose branch already
    /// understands `mark_full_repaint` — flipping it once here is
    /// enough; the next pass clears the whole output and re-blits
    /// every live surface.
    pub fn force_full_repaint(&mut self) {
        self.damage_tracker.mark_full_repaint();
        // Resetting `prev_pointer`/`prev_cursor_size` to `None` also
        // triggers `is_first_compose` in `run_compose_filtered`, which
        // re-runs `clear_rect_to_background(output)` on the whole FB
        // before the surface pass. Belt-and-suspenders.
        self.prev_pointer = None;
        self.prev_cursor_size = None;
    }

    /// Phase 72 review-resolution — invalidate the cached arrangement
    /// so the next compose pass calls `layout.arrange(..)` again. The
    /// id-set check in `run_compose_filtered` only catches arrangement
    /// changes when surfaces are added or removed; SetLayout / SetMaster
    /// Ratio / TileFullscreen / resize-mode adjustments mutate the
    /// active policy's internal state with the same id set, so the
    /// cached `Vec<(SurfaceId, Rect)>` would otherwise survive across
    /// the policy switch and the screen would stay tiled under the
    /// previous policy until something else (a new toplevel, a destroy)
    /// happened to change the id set.
    ///
    /// Clearing `cached_toplevel_ids` is the minimal invalidation: the
    /// next compose pass's id-set comparison will report a mismatch
    /// even if the live id set is identical, forcing the recompute.
    pub fn invalidate_arrangement_cache(&mut self) {
        self.cached_toplevel_ids.clear();
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
    run_compose_filtered(
        owner,
        layout,
        registry,
        ctx,
        pointer_position,
        |_| true,
        None,
        None,
    )
}

/// Phase 72 — workspace + border-aware compose entry point.
///
/// `include_toplevel` filters `iter_compose` so only the current
/// workspace's Toplevels are blitted. `border_cfg`, when `Some`,
/// paints active / inactive borders around every Toplevel rect after
/// the surface blit completes. `focused_id` selects which border uses
/// the active colour.
#[allow(clippy::too_many_arguments)]
pub fn run_compose_filtered<O, L, F>(
    owner: &mut O,
    layout: &mut L,
    registry: &mut SurfaceRegistry,
    ctx: &mut ComposeContext,
    pointer_position: (i32, i32),
    include_toplevel: F,
    border_cfg: Option<crate::borders::BorderConfig>,
    focused_id: Option<SurfaceId>,
) -> Result<usize, ComposeError>
where
    O: FramebufferOwner,
    L: LayoutPolicy,
    F: Fn(SurfaceId) -> bool + Copy,
{
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
    // Phase 72b — bypass the no-op gate when a full repaint was
    // requested via `ComposeContext::force_full_repaint`. The gate
    // existed before per-surface clip rects: the old assumption was
    // "no surface damage and no cursor motion => nothing on screen
    // changes, so skip the frame entirely." With the clip-rect
    // change and the arrangement-change path that calls
    // `force_full_repaint` when tiles resize, a fresh window can
    // arrive in a frame where no live surface has new damage (the
    // new surface hasn't committed a buffer yet, the existing tile
    // dims changed but the buffer didn't). Without this bypass we
    // skip the `is_first_compose` clear and the now-uncovered gap
    // regions keep stale pixels from before the arrangement change.
    let force_full = ctx.damage_tracker.is_full_repaint_needed();
    if !surface_damage && !cursor_motion && !force_full {
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
        .iter_compose_filtered(output, include_toplevel)
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

    let entries = registry.iter_compose_filtered(output, include_toplevel);

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
        // Phase 72b — surface preparation for the cursor-only fast
        // path. Mirrors the main-path scaling: for Toplevels whose
        // buffer dims differ from their tile (e.g. DOOM, or a term
        // mid-resize), substitute a tile-sized scaled snapshot and
        // use full-tile damage so the cursor-overpaint pass re-blits
        // the scaled content rather than the centred native-resolution
        // surface.
        let mut snapshots: Vec<Vec<u8>> = Vec::with_capacity(entries.len());
        let mut effective_damages: Vec<Vec<Rect>> = Vec::with_capacity(entries.len());
        let mut effective_rects: Vec<Rect> = Vec::with_capacity(entries.len());
        let mut scaled_flags: Vec<bool> = Vec::with_capacity(entries.len());
        for entry in entries.iter() {
            let original = entry.buf.pixels_snapshot();
            let tile = tile_for_entry(entry, arrangement);
            let should_scale = match tile {
                Some(t) => {
                    matches!(
                        entry.layer,
                        kernel_core::display::compose::ComposeLayer::Toplevel
                    ) && entry.buf.width > 0
                        && entry.buf.height > 0
                        && t.w > 0
                        && t.h > 0
                        && (entry.buf.width != t.w || entry.buf.height != t.h)
                }
                None => false,
            };
            if should_scale {
                let tile = tile.expect("checked above");
                let scaled = nearest_neighbour_scale(
                    &original,
                    entry.buf.width,
                    entry.buf.height,
                    tile.w,
                    tile.h,
                );
                snapshots.push(scaled);
                effective_damages.push(alloc::vec![Rect {
                    x: 0,
                    y: 0,
                    w: tile.w,
                    h: tile.h,
                }]);
                effective_rects.push(tile);
                scaled_flags.push(true);
            } else {
                snapshots.push(original);
                let surface_rect = surface_screen_rect(entry, arrangement);
                let local: Vec<Rect> = cursor_repaint
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
                    .collect();
                effective_damages.push(local);
                effective_rects.push(surface_rect);
                scaled_flags.push(false);
            }
        }

        let mut compose: Vec<ComposeSurface<'_>> = Vec::with_capacity(entries.len());
        for (entry, idx) in entries.iter().zip(0..) {
            let clip_rect = if scaled_flags[idx] {
                None
            } else {
                surface_tile_clip(entry, arrangement)
            };
            compose.push(ComposeSurface {
                id: entry.id,
                layer: entry.layer,
                rect: effective_rects[idx],
                damage: effective_damages[idx].as_slice(),
                pixels: snapshots[idx].as_slice(),
                opaque: entry.is_opaque(),
                clip_rect,
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
    // Phase 72b — per-entry pixel buffer. For most surfaces this is
    // `entry.buf.pixels_snapshot()` as before. For Toplevel surfaces
    // whose buffer dimensions don't match their assigned tile (e.g.
    // DOOM, which never resizes its 1280×800 backing buffer regardless
    // of the tile it lands in) we synthesise a *scaled* snapshot via
    // nearest-neighbour sampling so the rendered output fills the
    // tile instead of being letterboxed centred at native resolution.
    //
    // The scale triggers on any dim mismatch, scaling up or down:
    // term during its post-`SurfaceResized` transient shows briefly
    // scaled content while it reallocates its SHM, then naturally
    // settles to surf == tile and the scale path becomes a no-op.
    // Aspect ratio IS preserved — for DOOM in a non-1.6:1 tile this
    // produces letterbox bars on the constrained axis rather than
    // distorting the rendered world. Games would be visibly wrong
    // under stretch-mode scaling; non-game clients tolerate the
    // brief letterbox during their resize transient just fine.
    //
    // Parallel `Vec<Vec<Rect>>` holds the override damage for scaled
    // surfaces — a single full-tile rect, replacing the surface-local
    // damage list that compose_frame would otherwise translate.
    let mut snapshots: Vec<Vec<u8>> = Vec::with_capacity(entries.len());
    let mut scaled_damages: Vec<Vec<Rect>> = Vec::with_capacity(entries.len());
    let mut effective_rects: Vec<Rect> = Vec::with_capacity(entries.len());
    let mut scaled_flags: Vec<bool> = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        let original = entry.buf.pixels_snapshot();
        let tile = tile_for_entry(entry, arrangement);
        let should_scale = match tile {
            Some(t) => {
                matches!(
                    entry.layer,
                    kernel_core::display::compose::ComposeLayer::Toplevel
                ) && entry.buf.width > 0
                    && entry.buf.height > 0
                    && t.w > 0
                    && t.h > 0
                    && (entry.buf.width != t.w || entry.buf.height != t.h)
            }
            None => false,
        };
        if should_scale {
            let tile = tile.expect("checked above");
            let scaled = nearest_neighbour_scale(
                &original,
                entry.buf.width,
                entry.buf.height,
                tile.w,
                tile.h,
            );
            snapshots.push(scaled);
            scaled_damages.push(alloc::vec![Rect {
                x: 0,
                y: 0,
                w: tile.w,
                h: tile.h,
            }]);
            effective_rects.push(tile);
            scaled_flags.push(true);
        } else {
            snapshots.push(original);
            scaled_damages.push(Vec::new());
            effective_rects.push(surface_screen_rect(entry, arrangement));
            scaled_flags.push(false);
        }
    }

    let mut compose: Vec<ComposeSurface<'_>> = Vec::with_capacity(entries.len());
    for (((entry, dmg), snapshot), idx) in entries
        .iter()
        .zip(damages.iter())
        .zip(snapshots.iter())
        .zip(0..)
    {
        // For scaled surfaces, the pixel buffer has tile dimensions
        // and a single full-tile damage rect — clip_rect is None
        // because the surface IS the tile, no overflow to clip.
        // For non-scaled surfaces, take the existing letterbox-and-
        // clip path so a too-large buffer stays bounded by the tile.
        let (damage_slice, clip_rect): (&[Rect], Option<Rect>) = if scaled_flags[idx] {
            (scaled_damages[idx].as_slice(), None)
        } else {
            (&dmg[..], surface_tile_clip(entry, arrangement))
        };
        compose.push(ComposeSurface {
            id: entry.id,
            layer: entry.layer,
            rect: effective_rects[idx],
            damage: damage_slice,
            pixels: snapshot.as_slice(),
            opaque: entry.is_opaque(),
            clip_rect,
        });
    }

    let mut surface_writes = compose_frame(owner, output, &mut compose)?;

    // Phase 72 Track E — paint borders around every Toplevel tile
    // after the surface blit so they always sit on top of the surface
    // pixels. Active / inactive colour selection is driven by the
    // `focused_id` argument; `border_cfg = None` (Phase 56-compat
    // floating layout) skips the pass entirely.
    //
    // Borders use the **tile rect** from `arrangement`, not the
    // letterboxed buffer rect from `surface_screen_rect`: a tile
    // smaller than the surface (or vice-versa) still gets a border
    // that traces the actual layout partition. Snapshot the rect /
    // id / layer triples first so we can `mark_clean` (mutable
    // registry borrow) immediately and free the immutable `entries`
    // borrow before the cursor blit needs to touch the framebuffer.
    let border_targets: Vec<(SurfaceId, Rect)> = entries
        .iter()
        .filter(|e| {
            matches!(
                e.layer,
                kernel_core::display::compose::ComposeLayer::Toplevel
            )
        })
        .map(|e| {
            // Prefer the layout policy's tile rect when the surface is
            // tiled; fall back to the surface's own rect for the Phase
            // 56-compat floating path (which passes an empty
            // `arrangement`).
            let rect = arrangement
                .iter()
                .find(|(id, _)| *id == e.id)
                .map(|(_, tile)| *tile)
                .unwrap_or(e.rect);
            (e.id, rect)
        })
        .collect();
    drop(compose);
    drop(snapshots);
    drop(entries);
    registry.mark_clean();

    if let Some(cfg) = border_cfg
        && cfg.width > 0
    {
        for (id, rect) in border_targets.iter() {
            let color = if focused_id == Some(*id) {
                cfg.active_color
            } else {
                cfg.inactive_color
            };
            let written = crate::borders::paint_border(owner, *rect, cfg.width, color)
                .map_err(ComposeError::from)?;
            surface_writes = surface_writes.saturating_add(written);
        }
    }

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
/// Phase 72 — `compose_frame` requires `surface.rect.w/h` to match
/// `surface.pixels.len() / bpp` (the buffer's intrinsic dimensions);
/// a tiling policy may assign a tile rect that differs from those
/// dimensions, so we **letterbox** the surface centred within its
/// assigned tile. The returned rect always carries the buffer's
/// intrinsic width / height; only `x` / `y` are derived from the
/// tile centre. `compose_frame`'s clip-to-output handles the case
/// where the surface is larger than its tile (it spills out and
/// gets clipped at the output edge — the documented deferral for
/// DOOM in the Phase 72 spec).
/// Phase 72b Track K.3 follow-up — per-surface clip rect for tiled
/// `Toplevel` surfaces. Returns `Some(tile_rect)` when the surface is
/// tiled and its buffer extends past the tile boundary; the composer
/// passes this through to `compose_frame` which intersects every blit
/// damage rect with it. `None` for non-Toplevel layers and for the
/// Phase 56 floating-layout fallback (empty arrangement) so existing
/// behaviour is preserved exactly when there's no tile to clip to.
fn surface_tile_clip(
    entry: &crate::surface::ComposeEntry<'_>,
    arrangement: &[(SurfaceId, Rect)],
) -> Option<Rect> {
    if !matches!(
        entry.layer,
        kernel_core::display::compose::ComposeLayer::Toplevel
    ) {
        return None;
    }
    let (_, tile) = arrangement.iter().find(|(id, _)| *id == entry.id)?;
    Some(*tile)
}

/// Phase 72b — return the assigned tile rect for a Toplevel entry, or
/// `None` if no tile is assigned (floating layout, Layer / Cursor /
/// Background surface).
fn tile_for_entry(
    entry: &crate::surface::ComposeEntry<'_>,
    arrangement: &[(SurfaceId, Rect)],
) -> Option<Rect> {
    if !matches!(
        entry.layer,
        kernel_core::display::compose::ComposeLayer::Toplevel
    ) {
        return None;
    }
    arrangement
        .iter()
        .find(|(id, _)| *id == entry.id)
        .map(|(_, tile)| *tile)
}

/// Phase 72b — nearest-neighbour scale a BGRA8888 source buffer of
/// `src_w × src_h` into a tile-sized destination buffer of `dst_w ×
/// dst_h`, **preserving the source aspect ratio**. The scaled image
/// is centred inside the destination; any unused strip on the short
/// axis is filled with `crate::BG_PIXEL`, producing horizontal or
/// vertical letterbox bars depending on which axis is constrained.
///
/// Used for client surfaces (DOOM, plus any other fixed-size client
/// that doesn't resize its buffer in response to
/// `ServerMessage::SurfaceResized`). Aspect-preserving is the correct
/// behaviour for games — squishing DOOM into a non-1.6:1 tile by
/// stretching would distort the rendered world; players notice
/// immediately.
///
/// Scales up *and* down. Pure logic: no I/O, no allocation beyond
/// the returned `Vec<u8>`. Nearest-neighbour was chosen over bilinear
/// because DOOM's chunky pixel-art aesthetic looks intentionally
/// crisp under integer scaling; bilinear would smear it. Smoother
/// filters can be added per-client when a real use case arrives.
fn nearest_neighbour_scale(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    const BPP: usize = 4;
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let src_w_usize = src_w as usize;
    let src_h_usize = src_h as usize;
    let dst_w_usize = dst_w as usize;
    let dst_h_usize = dst_h as usize;
    let src_stride = src_w_usize * BPP;
    let dst_stride = dst_w_usize * BPP;

    // Aspect-preserving fit: pick the smaller of the two per-axis
    // ratios so the scaled image fits entirely inside the destination
    // rect. Using integer math, compare `dst_w/src_w` vs `dst_h/src_h`
    // via cross-multiply (a/b ≤ c/d ⇔ ad ≤ bc).
    let (scaled_w, scaled_h) = {
        let lhs = dst_w_usize * src_h_usize;
        let rhs = dst_h_usize * src_w_usize;
        if lhs <= rhs {
            // Width axis is the constraint.
            let w = dst_w_usize;
            let h = ((src_h_usize * dst_w_usize) / src_w_usize).max(1);
            (w, h.min(dst_h_usize))
        } else {
            // Height axis is the constraint.
            let h = dst_h_usize;
            let w = ((src_w_usize * dst_h_usize) / src_h_usize).max(1);
            (w.min(dst_w_usize), h)
        }
    };
    let off_x = (dst_w_usize - scaled_w) / 2;
    let off_y = (dst_h_usize - scaled_h) / 2;

    // Pre-fill the destination with the compositor background colour
    // so the letterbox bars on the unused axis match the rest of the
    // gap-fill convention. Skipped axes (where scaled_w == dst_w or
    // scaled_h == dst_h) leave a zero-byte band that the scaled
    // content immediately overwrites — the prefill is cheap relative
    // to the per-pixel sample loop.
    let bg_bytes = crate::BG_PIXEL.to_le_bytes();
    let mut out = alloc::vec![0u8; dst_stride * dst_h_usize];
    for px in 0..(dst_w_usize * dst_h_usize) {
        let off = px * BPP;
        let take = BPP.min(bg_bytes.len());
        out[off..off + take].copy_from_slice(&bg_bytes[..take]);
    }

    // Pre-compute the source-column index for every destination
    // column inside the scaled region. Sharing this across rows keeps
    // the inner loop's arithmetic to one multiply and a 4-byte copy
    // per pixel.
    let mut src_cols: Vec<usize> = Vec::with_capacity(scaled_w);
    for dx in 0..scaled_w {
        let sx = (dx * src_w_usize) / scaled_w;
        src_cols.push(sx.min(src_w_usize - 1));
    }
    for dy in 0..scaled_h {
        let sy = ((dy * src_h_usize) / scaled_h).min(src_h_usize - 1);
        let src_row_start = sy * src_stride;
        let dst_row_start = (dy + off_y) * dst_stride;
        for dx in 0..scaled_w {
            let sx = src_cols[dx];
            let src_off = src_row_start + sx * BPP;
            let dst_off = dst_row_start + (dx + off_x) * BPP;
            if src_off + BPP <= src.len() && dst_off + BPP <= out.len() {
                out[dst_off..dst_off + BPP].copy_from_slice(&src[src_off..src_off + BPP]);
            }
        }
    }
    out
}

fn surface_screen_rect(
    entry: &crate::surface::ComposeEntry<'_>,
    arrangement: &[(SurfaceId, Rect)],
) -> Rect {
    if matches!(
        entry.layer,
        kernel_core::display::compose::ComposeLayer::Toplevel
    ) {
        if let Some((_, tile)) = arrangement.iter().find(|(id, _)| *id == entry.id) {
            // Letterbox: keep buffer dimensions, center inside the tile.
            let surf_w = entry.buf.width;
            let surf_h = entry.buf.height;
            let dx = ((tile.w as i32) - (surf_w as i32)) / 2;
            let dy = ((tile.h as i32) - (surf_h as i32)) / 2;
            Rect {
                x: tile.x.saturating_add(dx),
                y: tile.y.saturating_add(dy),
                w: surf_w,
                h: surf_h,
            }
        } else {
            entry.rect
        }
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
