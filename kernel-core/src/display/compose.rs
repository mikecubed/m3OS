//! Phase 56 Track C.4 — damage-tracked software compositor (pure-logic core).
//!
//! Given a list of surfaces (geometry, layer, damage, pixel content) plus an
//! [`FramebufferOwner`] sink, this module walks surfaces in canonical layer
//! order and blits the damaged regions into the output framebuffer. Pure
//! logic — host-testable through the
//! [`crate::display::fb_owner::RecordingFramebufferOwner`].
//!
//! Phase 56 limitation: each clipped damage rectangle is staged into a
//! temporary `Vec<u8>` before handing it to [`FramebufferOwner::write_pixels`].
//! That means one allocation per damage write. A later phase may switch to a
//! borrowed strided slice once the framebuffer trait grows that capability.

use alloc::vec;
use alloc::vec::Vec;

use crate::display::fb_owner::{FbError, FramebufferOwner, bytes_per_pixel};
use crate::display::protocol::{Layer, Rect, SurfaceId};

/// Logical layer ordering used by the composer. Lower values are drawn
/// first (i.e. furthest from the viewer), higher values are drawn last.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum ComposeLayer {
    /// Wallpapers / background colour fills.
    Background = 0,
    /// Below-toplevel layer-shell anchors (e.g. status bars docked at the
    /// bottom of the desktop stack).
    Bottom = 1,
    /// Normal application windows.
    Toplevel = 2,
    /// Layer-shell surfaces that float above toplevels (panels, menus).
    Top = 3,
    /// Layer-shell `Overlay` (lock screens, notification toasts, OSDs).
    Overlay = 4,
    /// The pointer image. Always drawn last.
    Cursor = 5,
}

impl From<Layer> for ComposeLayer {
    fn from(layer: Layer) -> Self {
        match layer {
            Layer::Background => ComposeLayer::Background,
            Layer::Bottom => ComposeLayer::Bottom,
            Layer::Top => ComposeLayer::Top,
            Layer::Overlay => ComposeLayer::Overlay,
        }
    }
}

/// One surface ready for the compositor. The pixel buffer is borrowed for
/// the duration of [`compose_frame`].
#[derive(Clone, Copy, Debug)]
pub struct ComposeSurface<'a> {
    /// Stable identity for telemetry and per-surface lookup.
    pub id: SurfaceId,
    /// Where this surface sits in the canonical layer stack.
    pub layer: ComposeLayer,
    /// The surface's destination rectangle in *output* coordinates.
    pub rect: Rect,
    /// Damaged regions in *surface-local* coordinates (top-left at 0,0).
    pub damage: &'a [Rect],
    /// Tightly-packed pixels for the surface, row-major BGRA8888. Length
    /// must equal `rect.w * rect.h * 4`.
    pub pixels: &'a [u8],
    /// Whether the surface is fully opaque. Used for occlusion culling.
    pub opaque: bool,
}

/// Errors produced by [`compose_frame`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComposeError {
    /// The underlying framebuffer sink failed (e.g. truncated source,
    /// invalid stride, lost hardware) — surfaced verbatim.
    Fb(FbError),
    /// At least one [`ComposeSurface::pixels`] slice did not match the
    /// surface's `rect.w * rect.h * 4`. No writes are issued in this case.
    PixelLengthMismatch,
}

impl From<FbError> for ComposeError {
    fn from(err: FbError) -> Self {
        ComposeError::Fb(err)
    }
}

/// Sort `surfaces` so the composer can iterate them in canonical layer
/// order (background → cursor). The relative order of two surfaces sharing
/// the same [`ComposeLayer`] is preserved (stable sort).
pub fn sort_in_layer_order(surfaces: &mut [ComposeSurface<'_>]) {
    surfaces.sort_by_key(|s| s.layer);
}

/// Intersect two axis-aligned rectangles. Returns `None` if they are
/// disjoint or if either has zero width or height.
pub fn rect_intersect(a: Rect, b: Rect) -> Option<Rect> {
    if a.w == 0 || a.h == 0 || b.w == 0 || b.h == 0 {
        return None;
    }
    let ax2 = a.x.saturating_add(a.w as i32);
    let ay2 = a.y.saturating_add(a.h as i32);
    let bx2 = b.x.saturating_add(b.w as i32);
    let by2 = b.y.saturating_add(b.h as i32);
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let x2 = ax2.min(bx2);
    let y2 = ay2.min(by2);
    if x2 <= x || y2 <= y {
        return None;
    }
    Some(Rect {
        x,
        y,
        w: (x2 - x) as u32,
        h: (y2 - y) as u32,
    })
}

/// Translate a surface-local damage rectangle into output coordinates by
/// adding the surface's origin. Width and height are preserved; the caller
/// is responsible for clipping to the output extents afterwards.
pub fn translate_damage(surface_rect: Rect, local_damage: Rect) -> Rect {
    Rect {
        x: surface_rect.x.saturating_add(local_damage.x),
        y: surface_rect.y.saturating_add(local_damage.y),
        w: local_damage.w,
        h: local_damage.h,
    }
}

/// Walk `surfaces` and collect the output-coordinate rectangles of every
/// fully-opaque surface clipped to `output`. Lower-layer surfaces consult
/// this list to skip damage that is fully obscured by a higher-layer
/// opaque surface.
///
/// The returned list is deduplicated only by rectangle equality; callers
/// inspect each entry independently when checking containment.
pub fn build_occlusion_map(surfaces: &[ComposeSurface<'_>], output: Rect) -> Vec<Rect> {
    let mut occluders = Vec::new();
    for surface in surfaces {
        if !surface.opaque {
            continue;
        }
        if let Some(clipped) = rect_intersect(surface.rect, output) {
            occluders.push(clipped);
        }
    }
    occluders
}

/// True iff `inner` is fully covered by some rectangle in `occluders`.
fn rect_fully_occluded(inner: Rect, occluders: &[Rect]) -> bool {
    if inner.w == 0 || inner.h == 0 {
        return true;
    }
    let inner_x2 = inner.x.saturating_add(inner.w as i32);
    let inner_y2 = inner.y.saturating_add(inner.h as i32);
    for occ in occluders {
        if occ.w == 0 || occ.h == 0 {
            continue;
        }
        let ox2 = occ.x.saturating_add(occ.w as i32);
        let oy2 = occ.y.saturating_add(occ.h as i32);
        if occ.x <= inner.x && occ.y <= inner.y && ox2 >= inner_x2 && oy2 >= inner_y2 {
            return true;
        }
    }
    false
}

/// Copy a sub-rectangle of a surface's pixel buffer into a freshly-
/// allocated tightly-packed `Vec<u8>`. `surface_rect` is the surface's
/// origin-and-size in output space; `sub_local` is the rectangle to copy
/// expressed in *surface-local* coordinates.
fn extract_subrect(
    surface_rect: Rect,
    surface_pixels: &[u8],
    sub_local: Rect,
    bpp: u32,
) -> Vec<u8> {
    let bpp = bpp as usize;
    let surface_w = surface_rect.w as usize;
    let sub_w = sub_local.w as usize;
    let sub_h = sub_local.h as usize;
    let sub_x = sub_local.x as usize;
    let sub_y = sub_local.y as usize;
    let mut out = vec![0u8; sub_w * sub_h * bpp];
    if sub_w == 0 || sub_h == 0 {
        return out;
    }
    for row in 0..sub_h {
        let src_row_start = ((sub_y + row) * surface_w + sub_x) * bpp;
        let src_row_end = src_row_start + sub_w * bpp;
        let dst_row_start = row * sub_w * bpp;
        let dst_row_end = dst_row_start + sub_w * bpp;
        out[dst_row_start..dst_row_end]
            .copy_from_slice(&surface_pixels[src_row_start..src_row_end]);
    }
    out
}

/// Compose one frame.
///
/// `surfaces` is sorted in canonical [`ComposeLayer`] order (background
/// first). For each surface, the union of its damage rectangles is
/// translated into output coordinates, clipped to `output`, and (if not
/// fully occluded by a higher-layer opaque surface) blitted via
/// [`FramebufferOwner::write_pixels`]. [`FramebufferOwner::present`] is
/// invoked exactly once at the end if and only if at least one
/// `write_pixels` call succeeded.
///
/// Returns the number of `write_pixels` calls issued — useful for tests
/// that want to assert "exactly one write" or "zero writes". Pixel-length
/// validation runs *before* any writes, so a malformed surface aborts the
/// frame without partial damage on screen.
pub fn compose_frame<O: FramebufferOwner>(
    owner: &mut O,
    output: Rect,
    surfaces: &mut [ComposeSurface<'_>],
) -> Result<usize, ComposeError> {
    // The pixel format is owned by the framebuffer; clients are required
    // by the Phase 56 protocol to submit pixel data in the same format.
    // Deriving bpp from the owner keeps validation, sub-rect extraction,
    // and stride math consistent if a future phase introduces additional
    // packed formats.
    let bpp = bytes_per_pixel(owner.metadata().pixel_format);
    let bpp_usize = bpp as usize;

    // 1. Validate every surface's pixel length up front. We refuse to
    //    issue any writes if a single surface is malformed; partial frames
    //    leave torn pixels on screen which is worse than a dropped frame.
    for surface in surfaces.iter() {
        let expected = (surface.rect.w as usize)
            .saturating_mul(surface.rect.h as usize)
            .saturating_mul(bpp_usize);
        if surface.pixels.len() != expected {
            return Err(ComposeError::PixelLengthMismatch);
        }
    }

    // 2. Sort into canonical layer order.
    sort_in_layer_order(surfaces);

    // 3. For each surface, build the occlusion list of *higher-layer*
    //    opaque surfaces. We walk by index so we can slice in front of
    //    `i` for the occluders.
    let mut writes = 0usize;

    for i in 0..surfaces.len() {
        let surface = surfaces[i];
        // A surface with zero width or height has no pixel content to
        // sample from, regardless of what damage rects the client posted.
        // Skip it entirely so we don't index into an empty pixel buffer.
        if surface.rect.w == 0 || surface.rect.h == 0 {
            continue;
        }
        let surface_local_extent = Rect {
            x: 0,
            y: 0,
            w: surface.rect.w,
            h: surface.rect.h,
        };
        let occluders = build_occlusion_map(&surfaces[i + 1..], output);

        for &local_damage in surface.damage {
            // Clip the damage rect to the surface's local extents before
            // translating; otherwise a client-supplied damage rect outside
            // the surface bounds would index off the end of `pixels`.
            let local_clipped = match rect_intersect(local_damage, surface_local_extent) {
                Some(r) => r,
                None => continue,
            };
            let translated = translate_damage(surface.rect, local_clipped);
            let clipped = match rect_intersect(translated, output) {
                Some(r) => r,
                None => continue,
            };
            if rect_fully_occluded(clipped, &occluders) {
                continue;
            }
            // Convert the clipped output rect back into surface-local
            // coordinates so we can index into `surface.pixels`.
            let sub_local = Rect {
                x: clipped.x - surface.rect.x,
                y: clipped.y - surface.rect.y,
                w: clipped.w,
                h: clipped.h,
            };
            let buf = extract_subrect(surface.rect, surface.pixels, sub_local, bpp);
            // The temp buffer is tightly packed: stride == w * bpp.
            let stride = clipped.w * bpp;
            owner.write_pixels(clipped, &buf, stride)?;
            writes += 1;
        }
    }

    if writes > 0 {
        owner.present()?;
    }
    Ok(writes)
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::fb_owner::{FbMetadata, PixelFormat, RecordingFramebufferOwner};
    use proptest::prelude::*;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    fn meta(width: u32, height: u32) -> FbMetadata {
        FbMetadata {
            width,
            height,
            stride_bytes: width * 4,
            pixel_format: PixelFormat::Bgra8888,
        }
    }

    fn solid_pixels(w: u32, h: u32, fill: u8) -> Vec<u8> {
        vec![fill; (w as usize) * (h as usize) * 4]
    }

    fn make_surface<'a>(
        id: u32,
        layer: ComposeLayer,
        r: Rect,
        damage: &'a [Rect],
        pixels: &'a [u8],
        opaque: bool,
    ) -> ComposeSurface<'a> {
        ComposeSurface {
            id: SurfaceId(id),
            layer,
            rect: r,
            damage,
            pixels,
            opaque,
        }
    }

    #[test]
    fn compose_layer_ordering() {
        assert!(ComposeLayer::Background < ComposeLayer::Bottom);
        assert!(ComposeLayer::Bottom < ComposeLayer::Toplevel);
        assert!(ComposeLayer::Toplevel < ComposeLayer::Top);
        assert!(ComposeLayer::Top < ComposeLayer::Overlay);
        assert!(ComposeLayer::Overlay < ComposeLayer::Cursor);
        // From<Layer> mappings.
        assert_eq!(
            ComposeLayer::from(Layer::Background),
            ComposeLayer::Background
        );
        assert_eq!(ComposeLayer::from(Layer::Bottom), ComposeLayer::Bottom);
        assert_eq!(ComposeLayer::from(Layer::Top), ComposeLayer::Top);
        assert_eq!(ComposeLayer::from(Layer::Overlay), ComposeLayer::Overlay);
    }

    #[test]
    fn rect_intersect_disjoint_returns_none() {
        let a = rect(0, 0, 10, 10);
        let b = rect(50, 50, 10, 10);
        assert_eq!(rect_intersect(a, b), None);
    }

    #[test]
    fn rect_intersect_overlapping() {
        let a = rect(0, 0, 20, 20);
        let b = rect(10, 10, 20, 20);
        assert_eq!(rect_intersect(a, b), Some(rect(10, 10, 10, 10)));
    }

    #[test]
    fn rect_intersect_one_inside_other() {
        let a = rect(0, 0, 100, 100);
        let b = rect(20, 30, 40, 50);
        assert_eq!(rect_intersect(a, b), Some(rect(20, 30, 40, 50)));
    }

    #[test]
    fn rect_intersect_zero_dim_returns_none() {
        let a = rect(0, 0, 0, 10);
        let b = rect(0, 0, 10, 10);
        assert_eq!(rect_intersect(a, b), None);
        let c = rect(0, 0, 10, 0);
        assert_eq!(rect_intersect(c, b), None);
    }

    #[test]
    fn translate_damage_adds_offset() {
        let surface = rect(10, 20, 100, 100);
        let local = rect(5, 7, 30, 40);
        assert_eq!(translate_damage(surface, local), rect(15, 27, 30, 40));
    }

    #[test]
    fn compose_frame_zero_surfaces_zero_writes() {
        let mut owner = RecordingFramebufferOwner::new(meta(800, 600));
        let result = compose_frame(&mut owner, rect(0, 0, 800, 600), &mut []);
        assert_eq!(result, Ok(0));
        assert!(owner.writes().is_empty());
        assert_eq!(owner.present_calls(), 0);
    }

    #[test]
    fn compose_frame_no_damage_zero_writes() {
        let mut owner = RecordingFramebufferOwner::new(meta(800, 600));
        let pixels = solid_pixels(50, 50, 0xAA);
        let mut surfaces = [make_surface(
            1,
            ComposeLayer::Toplevel,
            rect(10, 10, 50, 50),
            &[],
            &pixels,
            false,
        )];
        let result = compose_frame(&mut owner, rect(0, 0, 800, 600), &mut surfaces);
        assert_eq!(result, Ok(0));
        assert!(owner.writes().is_empty());
        assert_eq!(owner.present_calls(), 0);
    }

    #[test]
    fn compose_frame_full_damage_one_write() {
        let mut owner = RecordingFramebufferOwner::new(meta(800, 600));
        let pixels = solid_pixels(50, 50, 0xAA);
        let damage = [rect(0, 0, 50, 50)];
        let mut surfaces = [make_surface(
            1,
            ComposeLayer::Toplevel,
            rect(10, 10, 50, 50),
            &damage,
            &pixels,
            false,
        )];
        let result = compose_frame(&mut owner, rect(0, 0, 800, 600), &mut surfaces);
        assert_eq!(result, Ok(1));
        let writes = owner.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].clipped_rect, rect(10, 10, 50, 50));
        assert_eq!(owner.present_calls(), 1);
    }

    #[test]
    fn compose_frame_off_screen_surface_skipped() {
        let mut owner = RecordingFramebufferOwner::new(meta(100, 100));
        let pixels = solid_pixels(50, 50, 0xAA);
        let damage = [rect(0, 0, 50, 50)];
        // Surface entirely to the right of the output.
        let mut surfaces = [make_surface(
            1,
            ComposeLayer::Toplevel,
            rect(200, 200, 50, 50),
            &damage,
            &pixels,
            false,
        )];
        let result = compose_frame(&mut owner, rect(0, 0, 100, 100), &mut surfaces);
        assert_eq!(result, Ok(0));
        assert!(owner.writes().is_empty());
        assert_eq!(owner.present_calls(), 0);
    }

    #[test]
    fn compose_frame_layer_order_traversal_is_background_first() {
        let mut owner = RecordingFramebufferOwner::new(meta(100, 100));
        let bg_pixels = solid_pixels(100, 100, 0x11);
        let top_pixels = solid_pixels(20, 20, 0x22);
        let cur_pixels = solid_pixels(10, 10, 0x33);
        let bg_damage = [rect(0, 0, 100, 100)];
        let top_damage = [rect(0, 0, 20, 20)];
        let cur_damage = [rect(0, 0, 10, 10)];
        // Submit in a *non*-canonical order: cursor first, background second,
        // top third. The composer must still emit them background → cursor.
        let mut surfaces = [
            make_surface(
                3,
                ComposeLayer::Cursor,
                rect(50, 50, 10, 10),
                &cur_damage,
                &cur_pixels,
                false,
            ),
            make_surface(
                1,
                ComposeLayer::Background,
                rect(0, 0, 100, 100),
                &bg_damage,
                &bg_pixels,
                false,
            ),
            make_surface(
                2,
                ComposeLayer::Top,
                rect(40, 40, 20, 20),
                &top_damage,
                &top_pixels,
                false,
            ),
        ];
        let result = compose_frame(&mut owner, rect(0, 0, 100, 100), &mut surfaces);
        assert_eq!(result, Ok(3));
        let writes = owner.writes();
        assert_eq!(writes.len(), 3);
        // The clipped_rect of each recorded write tells us which surface drew it,
        // because each surface has a distinct rectangle.
        assert_eq!(
            writes[0].clipped_rect,
            rect(0, 0, 100, 100),
            "background drawn first"
        );
        assert_eq!(
            writes[1].clipped_rect,
            rect(40, 40, 20, 20),
            "top drawn after background"
        );
        assert_eq!(
            writes[2].clipped_rect,
            rect(50, 50, 10, 10),
            "cursor drawn last"
        );
    }

    #[test]
    fn compose_frame_occluded_surface_is_skipped() {
        let mut owner = RecordingFramebufferOwner::new(meta(100, 100));
        let bg_pixels = solid_pixels(100, 100, 0x11);
        let top_pixels = solid_pixels(100, 100, 0x22);
        let bg_damage = [rect(0, 0, 100, 100)];
        let top_damage = [rect(0, 0, 100, 100)];
        // Toplevel fully covers the background (same rect, opaque).
        let mut surfaces = [
            make_surface(
                1,
                ComposeLayer::Background,
                rect(0, 0, 100, 100),
                &bg_damage,
                &bg_pixels,
                false,
            ),
            make_surface(
                2,
                ComposeLayer::Toplevel,
                rect(0, 0, 100, 100),
                &top_damage,
                &top_pixels,
                true,
            ),
        ];
        let result = compose_frame(&mut owner, rect(0, 0, 100, 100), &mut surfaces);
        assert_eq!(result, Ok(1));
        let writes = owner.writes();
        assert_eq!(writes.len(), 1);
        // The single recorded write is the toplevel — its clipped_rect is the
        // full output (which both bg and top happen to share), so we instead
        // verify by looking at the FB pixel content: top filled with 0x22.
        assert_eq!(owner.pixel(50, 50) & 0xff, 0x22);
        assert_eq!(owner.present_calls(), 1);
    }

    #[test]
    fn compose_frame_pixel_length_mismatch_errors_no_writes() {
        let mut owner = RecordingFramebufferOwner::new(meta(100, 100));
        let bad_pixels = vec![0u8; 10]; // way too small for a 50x50x4 surface
        let damage = [rect(0, 0, 50, 50)];
        let mut surfaces = [make_surface(
            1,
            ComposeLayer::Toplevel,
            rect(0, 0, 50, 50),
            &damage,
            &bad_pixels,
            false,
        )];
        let result = compose_frame(&mut owner, rect(0, 0, 100, 100), &mut surfaces);
        assert_eq!(result, Err(ComposeError::PixelLengthMismatch));
        assert!(owner.writes().is_empty());
        assert_eq!(owner.present_calls(), 0);
    }

    #[test]
    fn compose_frame_present_called_once_when_any_write() {
        // Case A: one write → present_calls == 1.
        let mut owner = RecordingFramebufferOwner::new(meta(100, 100));
        let pixels = solid_pixels(20, 20, 0xAA);
        let damage = [rect(0, 0, 20, 20)];
        let mut surfaces = [make_surface(
            1,
            ComposeLayer::Toplevel,
            rect(0, 0, 20, 20),
            &damage,
            &pixels,
            false,
        )];
        compose_frame(&mut owner, rect(0, 0, 100, 100), &mut surfaces).expect("compose ok");
        assert_eq!(owner.present_calls(), 1);

        // Case B: zero writes → present_calls == 0.
        let mut owner2 = RecordingFramebufferOwner::new(meta(100, 100));
        let mut empty: [ComposeSurface; 0] = [];
        compose_frame(&mut owner2, rect(0, 0, 100, 100), &mut empty).expect("compose ok");
        assert_eq!(owner2.present_calls(), 0);
    }

    #[test]
    fn compose_frame_clips_damage_to_output_extents() {
        let mut owner = RecordingFramebufferOwner::new(meta(100, 100));
        let pixels = solid_pixels(100, 100, 0xAA);
        let damage = [rect(0, 0, 100, 100)];
        // Surface at (90, 0, 100, 100) — only 10px overlap with output.
        let mut surfaces = [make_surface(
            1,
            ComposeLayer::Toplevel,
            rect(90, 0, 100, 100),
            &damage,
            &pixels,
            false,
        )];
        let result = compose_frame(&mut owner, rect(0, 0, 100, 100), &mut surfaces);
        assert_eq!(result, Ok(1));
        let writes = owner.writes();
        assert_eq!(writes[0].clipped_rect, rect(90, 0, 10, 100));
        // Byte count matches clipped rect size (10 * 100 * 4), not full surface.
        assert_eq!(writes[0].byte_count, 10 * 100 * 4);
    }

    // ---- proptest -----------------------------------------------------

    fn arb_rect(max_origin: i32, max_dim: u32) -> impl Strategy<Value = Rect> {
        (
            -max_origin..=max_origin,
            -max_origin..=max_origin,
            0u32..=max_dim,
            0u32..=max_dim,
        )
            .prop_map(|(x, y, w, h)| Rect { x, y, w, h })
    }

    proptest! {
        #[test]
        fn proptest_compose_frame_writes_a_subset_of_damage_union(
            // Build up to four surfaces with up to three damage rects each.
            entries in proptest::collection::vec(
                (
                    0u32..4u32,                 // layer index
                    arb_rect(50, 60),           // surface rect
                    proptest::collection::vec(arb_rect(60, 60), 0..=3),
                    any::<bool>(),              // opaque
                ),
                0..=4,
            ),
            output in arb_rect(0, 100),
        ) {
            // Skip degenerate outputs — the trivial "empty output" case is
            // covered by a dedicated unit test.
            prop_assume!(output.w > 0 && output.h > 0);
            // Anchor output at origin so the recording owner's clip math
            // matches the composer's notion of "output rect".
            prop_assume!(output.x == 0 && output.y == 0);

            let mut owner = RecordingFramebufferOwner::new(meta(output.w, output.h));

            // Convert configs to surfaces with synthesized pixel buffers.
            let pixels: Vec<Vec<u8>> = entries
                .iter()
                .map(|(_, r, _, _)| solid_pixels(r.w, r.h, 0))
                .collect();

            let mut surfaces: Vec<ComposeSurface> = entries
                .iter()
                .enumerate()
                .map(|(i, (li, r, dmg, opaque))| {
                    let layer = match li % 4 {
                        0 => ComposeLayer::Background,
                        1 => ComposeLayer::Toplevel,
                        2 => ComposeLayer::Top,
                        _ => ComposeLayer::Overlay,
                    };
                    ComposeSurface {
                        id: SurfaceId(i as u32 + 1),
                        layer,
                        rect: *r,
                        damage: dmg.as_slice(),
                        pixels: pixels[i].as_slice(),
                        opaque: *opaque,
                    }
                })
                .collect();

            // Build the *expected* damage union before running compose,
            // mirroring the composer's clip-to-surface-then-translate-then-
            // clip-to-output sequence.
            let damage_union: Vec<Rect> = entries
                .iter()
                .flat_map(|(_, r, dmg, _)| {
                    let surface_extent = Rect { x: 0, y: 0, w: r.w, h: r.h };
                    dmg.iter().filter_map(move |d| {
                        let local_clipped = rect_intersect(*d, surface_extent)?;
                        let translated = translate_damage(*r, local_clipped);
                        rect_intersect(translated, output)
                    })
                })
                .collect();

            let _ = compose_frame(&mut owner, output, &mut surfaces);

            // Subset invariant: every recorded write rect must equal some
            // entry in the damage union (we built `damage_union` with the
            // same translate+intersect routine the composer uses).
            for w in owner.writes() {
                let found = damage_union.iter().any(|d| d == &w.clipped_rect);
                prop_assert!(
                    found,
                    "write {:?} not found in damage union {:?}",
                    w.clipped_rect, damage_union
                );
            }
        }
    }
}
