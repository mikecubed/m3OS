//! Phase 105 Track C — pack the compositor's composited frame into a
//! client-provided capture buffer as packed BGRA8888.
//!
//! The compositor owns the framebuffer, so a screenshot must originate
//! there: on a `CaptureOutput` request the compositor blits its
//! most-recently-composed frame (`back_buffer_pixels()`, whose row stride
//! may exceed `width * 4` and whose byte order is the framebuffer's native
//! format) into a client shared-memory region as *packed* BGRA8888. Keeping
//! that blit here — pure logic over plain slices — lets it be host-tested
//! without a live framebuffer or SHM syscalls; `display_server`'s `main.rs`
//! is only responsible for mapping the SHM and calling this.

use super::fb_owner::{PixelFormat, bytes_per_pixel};

/// A read-only view of the compositor's most-recently-composed frame, in the
/// framebuffer's native byte order. Grouping the geometry keeps
/// [`pack_capture_bgra`] to a handful of arguments.
pub struct FrameView<'a> {
    /// Native framebuffer bytes, `stride_bytes * height` long.
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// Row stride in bytes (`>= width * 4`; may exceed it with padding).
    pub stride_bytes: u32,
    pub pixel_format: PixelFormat,
}

/// Blit the composited frame `src` into `dst` as packed BGRA8888.
///
/// `dst` receives packed BGRA8888 at row stride `out_width * 4`,
/// top-to-bottom, where `out_width`/`out_height` are `src` clamped to
/// `max_width × max_height` and to what `dst` (and `src`) can actually
/// hold. Returns the `(out_width, out_height)` written — `(0, 0)` if
/// nothing could be captured (non-4-bpp format, zero area, or `dst` too
/// small for even one row).
///
/// BGRA8888 memory is `[B, G, R, A]`, which is exactly the packed target,
/// so those pixels copy straight through. RGBA8888 memory is `[R, G, B, A]`,
/// so the R/B channels are swapped on the way out. This mirrors the
/// convention the PNG encoder (`imagefmt::encode_png`) reads: a packed
/// `u32` in `0xAARRGGBB` form, i.e. `[B, G, R, A]` little-endian bytes.
pub fn pack_capture_bgra(
    src: &FrameView<'_>,
    max_width: u32,
    max_height: u32,
    dst: &mut [u8],
) -> (u32, u32) {
    // Only 4-byte packed formats are capturable (Phase 56 ships exactly
    // these two; a future non-4-bpp format is a capability gap, not a
    // silent mis-blit).
    if bytes_per_pixel(src.pixel_format) != 4 {
        return (0, 0);
    }
    let src_stride = src.stride_bytes as usize;
    if src_stride < (src.width as usize).saturating_mul(4) {
        // Malformed geometry: stride cannot hold a full row of pixels.
        return (0, 0);
    }

    let out_w = src.width.min(max_width) as usize;
    if out_w == 0 || src.height.min(max_height) == 0 {
        return (0, 0);
    }
    let dst_stride = out_w * 4;

    // Clamp height to what both buffers can hold. `dst` must fit whole rows;
    // `src` must actually contain each row we read (defend against a short
    // `src` even though `back_buffer_pixels()` is sized `stride * height`).
    let mut out_h = src.height.min(max_height) as usize;
    if let Some(dst_rows) = dst.len().checked_div(dst_stride) {
        out_h = out_h.min(dst_rows);
    }
    if let Some(src_rows) = src.pixels.len().checked_div(src_stride) {
        out_h = out_h.min(src_rows);
    }
    if out_h == 0 {
        return (0, 0);
    }

    let pixels = src.pixels;
    let swap_rb = matches!(src.pixel_format, PixelFormat::Rgba8888);
    for row in 0..out_h {
        let src_row = row * src_stride;
        let dst_row = row * dst_stride;
        for col in 0..out_w {
            let s = src_row + col * 4;
            let d = dst_row + col * 4;
            if swap_rb {
                // Native [R, G, B, A] → packed [B, G, R, A].
                dst[d] = pixels[s + 2];
                dst[d + 1] = pixels[s + 1];
                dst[d + 2] = pixels[s];
                dst[d + 3] = pixels[s + 3];
            } else {
                // Native [B, G, R, A] is already the packed target.
                dst[d] = pixels[s];
                dst[d + 1] = pixels[s + 1];
                dst[d + 2] = pixels[s + 2];
                dst[d + 3] = pixels[s + 3];
            }
        }
    }
    (out_w as u32, out_h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec;

    /// Build a `w × h` BGRA source with a stride wider than the row so the
    /// packing is exercised against real padding.
    fn bgra_src(w: u32, h: u32, stride_bytes: u32) -> alloc::vec::Vec<u8> {
        let mut v = vec![0u8; (stride_bytes * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let off = (y * stride_bytes + x * 4) as usize;
                v[off] = x as u8; // B
                v[off + 1] = y as u8; // G
                v[off + 2] = (x + y) as u8; // R
                v[off + 3] = 0xFF; // A
            }
        }
        v
    }

    fn view(
        pixels: &[u8],
        width: u32,
        height: u32,
        stride_bytes: u32,
        pf: PixelFormat,
    ) -> FrameView<'_> {
        FrameView {
            pixels,
            width,
            height,
            stride_bytes,
            pixel_format: pf,
        }
    }

    #[test]
    fn bgra_packs_and_drops_stride_padding() {
        // 3×2 image, stride 16 bytes (4px) → 4 bytes of padding per row.
        let src = bgra_src(3, 2, 16);
        let mut dst = vec![0u8; 3 * 2 * 4];
        let (w, h) = pack_capture_bgra(
            &view(&src, 3, 2, 16, PixelFormat::Bgra8888),
            64,
            64,
            &mut dst,
        );
        assert_eq!((w, h), (3, 2));
        // Packed stride is 12 bytes; pixel (2,1) sits at row 1, col 2.
        let off = 1 * 12 + 2 * 4;
        assert_eq!(&dst[off..off + 4], &[2u8, 1u8, 3u8, 0xFF]); // B,G,R,A
        // No padding bytes leaked into the packed output.
        assert_eq!(dst.len(), 24);
    }

    #[test]
    fn rgba_source_swaps_red_and_blue() {
        // One RGBA pixel [R=10, G=20, B=30, A=40] must emit [B=30,G=20,R=10,A=40].
        let src = vec![10u8, 20, 30, 40];
        let mut dst = vec![0u8; 4];
        let (w, h) = pack_capture_bgra(&view(&src, 1, 1, 4, PixelFormat::Rgba8888), 1, 1, &mut dst);
        assert_eq!((w, h), (1, 1));
        assert_eq!(dst, vec![30u8, 20, 10, 40]);
    }

    #[test]
    fn clamps_to_max_dimensions() {
        let src = bgra_src(8, 8, 32);
        let mut dst = vec![0u8; 8 * 8 * 4];
        let (w, h) =
            pack_capture_bgra(&view(&src, 8, 8, 32, PixelFormat::Bgra8888), 4, 3, &mut dst);
        assert_eq!((w, h), (4, 3));
    }

    #[test]
    fn undersized_dst_captures_fewer_rows() {
        // dst holds only 2 rows of a 5×5 image (packed stride 20).
        let src = bgra_src(5, 5, 20);
        let mut dst = vec![0u8; 20 * 2];
        let (w, h) = pack_capture_bgra(
            &view(&src, 5, 5, 20, PixelFormat::Bgra8888),
            64,
            64,
            &mut dst,
        );
        assert_eq!((w, h), (5, 2));
    }

    #[test]
    fn dst_too_small_for_one_row_captures_nothing() {
        let src = bgra_src(5, 5, 20);
        let mut dst = vec![0u8; 4]; // < one packed row (20 bytes)
        let (w, h) = pack_capture_bgra(
            &view(&src, 5, 5, 20, PixelFormat::Bgra8888),
            64,
            64,
            &mut dst,
        );
        assert_eq!((w, h), (0, 0));
    }

    #[test]
    fn zero_area_captures_nothing() {
        let mut dst = vec![0u8; 64];
        assert_eq!(
            pack_capture_bgra(&view(&[], 0, 0, 0, PixelFormat::Bgra8888), 64, 64, &mut dst),
            (0, 0)
        );
    }
}
