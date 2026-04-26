//! Phase 56 Track C.2 — `FramebufferOwner` trait + recording test double.
//!
//! The compositor in `display_server` does not poke at the physical
//! framebuffer directly. Instead it talks to a [`FramebufferOwner`] — an
//! abstraction that hides the difference between the real, kernel-backed
//! `KernelFramebufferOwner` (which lives in `userspace/display_server/src/fb.rs`
//! and performs the actual MMIO writes) and the in-memory
//! [`RecordingFramebufferOwner`] used by host-side compose tests.
//!
//! Putting the seam here, in `kernel-core`, lets every piece of compose math
//! be exercised on the host without booting QEMU. It also gives any future
//! framebuffer backend (a second display, a virtio-gpu shim, an offscreen
//! recorder) a single shared [`contract_suite`] to run against — the rules
//! that "writes are clipped, not panicked", "stride padding is honoured",
//! and "`present` is idempotent" are stated once and reused.
//!
//! ## Format assumptions
//!
//! Phase 56 ships only 32-bit packed pixel formats ([`PixelFormat::Bgra8888`]
//! and [`PixelFormat::Rgba8888`]). The seam intentionally does *not* perform
//! in-place format conversion: the client and the framebuffer must agree on
//! the format, which is reported through [`FbMetadata`]. This keeps the hot
//! write path branch-free; converting clients live in a higher layer.

use alloc::vec;
use alloc::vec::Vec;

use crate::display::protocol::Rect;

/// Pixel layout of a framebuffer.
///
/// Phase 56 only enumerates the two packed-32-bit formats the kernel
/// framebuffer driver advertises. Extending the set requires touching this
/// enum *and* the format-aware writers in `KernelFramebufferOwner`; the
/// seam itself is format-agnostic and just propagates the chosen format
/// through [`FbMetadata`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    /// Little-endian B, G, R, A bytes per pixel.
    Bgra8888,
    /// Little-endian R, G, B, A bytes per pixel.
    Rgba8888,
}

/// Geometric and format description of a framebuffer.
///
/// `stride_bytes` is the byte distance between successive rows in the
/// destination framebuffer; it is independent of `width * bytes_per_pixel`
/// because real hardware often pads rows for alignment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FbMetadata {
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Byte distance between successive rows in the destination FB.
    pub stride_bytes: u32,
    /// Pixel layout of the destination FB.
    pub pixel_format: PixelFormat,
}

/// Errors a [`FramebufferOwner`] may return on a write or present.
///
/// Out-of-bounds rectangles are *clipped* rather than reported, so
/// [`FbError::OutOfBounds`] is reserved for future backends that may
/// reject writes synchronously (e.g., a remote framebuffer with a
/// shrinking surface). The reference [`RecordingFramebufferOwner`]
/// never returns it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FbError {
    /// Reserved for backends that choose to reject out-of-bounds writes.
    OutOfBounds,
    /// `src` was shorter than the minimum required to cover the clipped rect.
    Truncated,
    /// `src_stride` was smaller than `clipped_rect.w * bytes_per_pixel`.
    InvalidStride,
    /// Backend cannot honour the requested operation on the current FB.
    Unsupported,
}

/// Owner of (and exclusive writer to) a framebuffer.
///
/// The trait is deliberately small: report metadata, copy a row-major
/// pixel rectangle, and optionally signal a present/flush point.
/// Compositor logic sits behind this interface so it can be exercised on
/// the host with [`RecordingFramebufferOwner`] and against the real
/// kernel-backed implementation in `userspace/display_server/src/fb.rs`.
pub trait FramebufferOwner {
    /// Return the width / height / stride / format of the underlying FB.
    /// Implementations must report values that remain stable for the
    /// lifetime of the owner; a resolution change creates a new owner.
    fn metadata(&self) -> FbMetadata;

    /// Write `src` into `rect`, where `src` is row-major in the FB's
    /// `pixel_format`. `src_stride` is the byte stride of `src`; rows are
    /// tightly packed when
    /// `src_stride == rect.w * bytes_per_pixel(self.metadata().pixel_format)`.
    ///
    /// Implementations MUST clip `rect` to the framebuffer bounds before
    /// writing — never trust `rect` to be in-bounds. Negative origins are
    /// clamped to zero and the source-buffer offset is adjusted so that
    /// the right portion of `src` lands at the FB's `(0, 0)`. If the
    /// clipped rectangle has zero area the call is a successful no-op.
    ///
    /// Returns [`FbError::InvalidStride`] when `src_stride` cannot fit a
    /// row of `clipped_rect.w` pixels, or [`FbError::Truncated`] when
    /// `src` is too short to cover the clipped rectangle given that
    /// stride.
    fn write_pixels(&mut self, rect: Rect, src: &[u8], src_stride: u32) -> Result<(), FbError>;

    /// Optional commit/flush point. Backends that double-buffer use this
    /// to swap; in-memory backends usually ignore it. Default impl is a
    /// no-op.
    fn present(&mut self) -> Result<(), FbError> {
        Ok(())
    }

    /// Phase 56 close-out (G.1 regression) — read the framebuffer pixel
    /// at `(x, y)` in screen coordinates as a `u32` (BGRA8888 packed).
    /// Returns [`FbError::OutOfBounds`] for out-of-range coordinates;
    /// the default impl returns `Err(FbError::OutOfBounds)` so backends
    /// that cannot read (e.g. write-only display engines) opt out
    /// explicitly. Used only by the test-only `ReadBackPixel` control
    /// verb gated by `M3OS_DISPLAY_SERVER_READBACK=1`; production
    /// boots leave the verb disabled.
    fn read_pixel(&self, _x: u32, _y: u32) -> Result<u32, FbError> {
        Err(FbError::OutOfBounds)
    }
}

/// Bytes per pixel for a [`PixelFormat`]. Both Phase 56 formats are 4 bytes;
/// the function exists so that future formats can be added without rippling
/// magic numbers through the codebase.
pub const fn bytes_per_pixel(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::Bgra8888 | PixelFormat::Rgba8888 => 4,
    }
}

/// FNV-1a 64-bit constants. Used by [`fnv1a_64`] to produce a stable,
/// deterministic content hash for [`RecordedWrite::content_hash`].
const FNV1A_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Compute the FNV-1a 64-bit hash of `bytes`. Chosen for being trivially
/// deterministic across architectures (no `core::hash::Hasher` ambiguity)
/// and cheap enough to run on every recorded write in tests.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A_OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV1A_PRIME);
    }
    hash
}

/// In-memory test double for [`FramebufferOwner`].
///
/// Records every accepted write so that tests can assert both *what
/// geometry* the compositor damaged and *what bytes* it actually
/// delivered. The backing pixel buffer is flat row-major, sized exactly
/// `width * height * bytes_per_pixel(format)` — `stride_bytes` from
/// metadata is honoured by [`write_pixels`] for the *source* but the
/// recording buffer itself is always tightly packed for ease of
/// inspection.
#[derive(Clone, Debug)]
pub struct RecordingFramebufferOwner {
    metadata: FbMetadata,
    /// `width * height * 4` bytes; row-major, tightly packed.
    pixels: Vec<u8>,
    writes: Vec<RecordedWrite>,
    present_calls: u32,
}

/// A single accepted write. `clipped_rect` is the rectangle in FB
/// coordinates after clamping to bounds; `byte_count` is the number of
/// destination bytes written; `content_hash` is the FNV-1a 64-bit hash of
/// the contiguous destination region (after copy) — sufficient to assert
/// "this write delivered the expected content" without storing duplicates.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RecordedWrite {
    /// Rectangle actually written, after clipping.
    pub clipped_rect: Rect,
    /// Number of destination bytes touched by the write.
    pub byte_count: usize,
    /// FNV-1a 64-bit hash of the bytes written (tightly packed in destination order).
    pub content_hash: u64,
}

impl RecordingFramebufferOwner {
    /// Construct a recorder with the given metadata, pre-allocating the
    /// backing pixel buffer to `width * height * bytes_per_pixel`.
    pub fn new(metadata: FbMetadata) -> Self {
        let bpp = bytes_per_pixel(metadata.pixel_format) as usize;
        let len = (metadata.width as usize) * (metadata.height as usize) * bpp;
        Self {
            metadata,
            pixels: vec![0u8; len],
            writes: Vec::new(),
            present_calls: 0,
        }
    }

    /// All recorded writes in arrival order.
    pub fn writes(&self) -> &[RecordedWrite] {
        &self.writes
    }

    /// Tightly-packed backing pixel buffer (row-major).
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Read the 32-bit packed pixel at `(x, y)`. Out-of-range coordinates
    /// return 0 — callers should treat that as "no information" rather
    /// than relying on it as a sentinel.
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        if x >= self.metadata.width || y >= self.metadata.height {
            return 0;
        }
        let bpp = bytes_per_pixel(self.metadata.pixel_format) as usize;
        let row = (y as usize) * (self.metadata.width as usize) * bpp;
        let off = row + (x as usize) * bpp;
        let bytes = &self.pixels[off..off + 4];
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Number of times [`FramebufferOwner::present`] was invoked.
    pub fn present_calls(&self) -> u32 {
        self.present_calls
    }

    /// Drop the recorded-writes log without touching pixels or the
    /// present counter. Useful for "perform setup writes, clear log,
    /// perform interesting writes, assert".
    pub fn clear_writes(&mut self) {
        self.writes.clear();
    }
}

/// Result of clipping a requested rectangle to a framebuffer.
///
/// `clipped` is the actual destination rectangle in FB coordinates;
/// `src_offset_x_px` / `src_offset_y_px` are how many pixels into `src`
/// the clipped region begins (non-zero only when clipping occurred at the
/// left or top edge). `is_empty` reports whether the clipped rectangle
/// has zero area, in which case [`FramebufferOwner::write_pixels`] is a
/// successful no-op.
#[derive(Clone, Copy, Debug)]
struct ClippedRect {
    clipped: Rect,
    src_offset_x_px: u32,
    src_offset_y_px: u32,
    is_empty: bool,
}

/// Clip `rect` to the `[0, width) x [0, height)` region of a framebuffer.
/// Returns the clipped rectangle in FB coordinates plus the source-buffer
/// offsets caused by clamping a negative origin.
fn clip_rect(rect: Rect, width: u32, height: u32) -> ClippedRect {
    // Compute right/bottom in i64 to avoid overflow on extreme inputs
    // (e.g., x = i32::MAX, w = u32::MAX).
    let req_left = i64::from(rect.x);
    let req_top = i64::from(rect.y);
    let req_right = req_left + i64::from(rect.w);
    let req_bottom = req_top + i64::from(rect.h);

    let fb_right = i64::from(width);
    let fb_bottom = i64::from(height);

    let left = req_left.max(0).min(fb_right);
    let top = req_top.max(0).min(fb_bottom);
    let right = req_right.max(0).min(fb_right);
    let bottom = req_bottom.max(0).min(fb_bottom);

    let clipped_w = (right - left).max(0) as u32;
    let clipped_h = (bottom - top).max(0) as u32;
    let is_empty = clipped_w == 0 || clipped_h == 0;

    let src_offset_x_px = (left - req_left).max(0) as u32;
    let src_offset_y_px = (top - req_top).max(0) as u32;

    ClippedRect {
        clipped: Rect {
            x: left as i32,
            y: top as i32,
            w: clipped_w,
            h: clipped_h,
        },
        src_offset_x_px,
        src_offset_y_px,
        is_empty,
    }
}

impl FramebufferOwner for RecordingFramebufferOwner {
    fn metadata(&self) -> FbMetadata {
        self.metadata
    }

    fn write_pixels(&mut self, rect: Rect, src: &[u8], src_stride: u32) -> Result<(), FbError> {
        let bpp = bytes_per_pixel(self.metadata.pixel_format);
        let clipped = clip_rect(rect, self.metadata.width, self.metadata.height);

        if clipped.is_empty {
            return Ok(());
        }

        let row_bytes = clipped
            .clipped
            .w
            .checked_mul(bpp)
            .ok_or(FbError::InvalidStride)?;
        if src_stride < row_bytes {
            return Err(FbError::InvalidStride);
        }

        // Minimum src length: stride * (h - 1) + last-row width. `h >= 1` here
        // because `is_empty` was false.
        let h = clipped.clipped.h;
        let src_required = (src_stride as usize)
            .checked_mul((h - 1) as usize)
            .and_then(|v| v.checked_add(row_bytes as usize))
            .ok_or(FbError::Truncated)?;

        // Account for the source-side offset caused by clipping at left/top.
        let src_skip_x = (clipped.src_offset_x_px as usize) * (bpp as usize);
        let src_skip_y_bytes = (clipped.src_offset_y_px as usize) * (src_stride as usize);
        let src_start = src_skip_y_bytes + src_skip_x;
        let src_end_required = src_start + src_required;

        if src.len() < src_end_required {
            return Err(FbError::Truncated);
        }

        // Copy, recording bytes for the content hash as we go.
        let dst_row_stride = (self.metadata.width as usize) * (bpp as usize);
        let dst_row0 = (clipped.clipped.y as usize) * dst_row_stride;
        let dst_col0 = (clipped.clipped.x as usize) * (bpp as usize);
        let row_bytes_us = row_bytes as usize;

        let mut hashed = Vec::with_capacity(row_bytes_us * (h as usize));
        for row in 0..h as usize {
            let dst_off = dst_row0 + row * dst_row_stride + dst_col0;
            let src_off = src_start + row * (src_stride as usize);
            let src_row = &src[src_off..src_off + row_bytes_us];
            self.pixels[dst_off..dst_off + row_bytes_us].copy_from_slice(src_row);
            hashed.extend_from_slice(src_row);
        }

        self.writes.push(RecordedWrite {
            clipped_rect: clipped.clipped,
            byte_count: row_bytes_us * (h as usize),
            content_hash: fnv1a_64(&hashed),
        });

        Ok(())
    }

    fn present(&mut self) -> Result<(), FbError> {
        self.present_calls = self.present_calls.saturating_add(1);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Contract test suite — sharable across every FramebufferOwner impl.
// ---------------------------------------------------------------------------

/// Run the shared [`FramebufferOwner`] contract against any implementation.
///
/// `constructor` is invoked once per test case with the metadata that case
/// requires. The suite asserts the behaviours every Phase 56 owner must
/// exhibit:
///
/// 1. Metadata round-trips.
/// 2. An in-bounds write succeeds.
/// 3. Partially clipped writes succeed and clamp.
/// 4. Fully out-of-bounds writes succeed as no-ops (Phase 56 chose
///    clipping over erroring — it is easier on clients).
/// 5. Negative origins clip correctly.
/// 6. Zero-area writes are no-op + Ok.
/// 7. Insufficient stride returns [`FbError::InvalidStride`].
/// 8. Truncated source returns [`FbError::Truncated`].
/// 9. Repeated writes to the same rect succeed (idempotency at the seam).
/// 10. [`FramebufferOwner::present`] returns Ok.
///
/// Asserts via `assert!` / `assert_eq!`; this is the intended exception to
/// the no-`panic!` rule for the file because failures are how a contract
/// suite reports breakage to the caller's test harness.
pub fn contract_suite<O, F>(constructor: F)
where
    O: FramebufferOwner,
    F: Fn(FbMetadata) -> O,
{
    fn meta(width: u32, height: u32) -> FbMetadata {
        FbMetadata {
            width,
            height,
            stride_bytes: width * 4,
            pixel_format: PixelFormat::Bgra8888,
        }
    }

    // 1. metadata_matches_constructor
    {
        let m = meta(64, 32);
        let owner = constructor(m);
        let got = owner.metadata();
        assert_eq!(got, m, "metadata returned by impl must match constructor");
    }

    // 2. in_bounds_write_succeeds
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        let src = [0xFFu8; 4 * 4 * 4]; // 4x4 BGRA pixels
        let res = owner.write_pixels(
            Rect {
                x: 10,
                y: 10,
                w: 4,
                h: 4,
            },
            &src,
            4 * 4,
        );
        assert_eq!(res, Ok(()), "in-bounds write must succeed");
    }

    // 3. partially_clipped_write_clamps_and_succeeds
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        // Rect straddles the right edge: x=98, w=10 → clipped to x=98, w=2.
        let src = [0x42u8; 10 * 4];
        let res = owner.write_pixels(
            Rect {
                x: 98,
                y: 0,
                w: 10,
                h: 1,
            },
            &src,
            10 * 4,
        );
        assert_eq!(res, Ok(()), "partially clipped write must succeed");
    }

    // 4. fully_out_of_bounds_write_succeeds_with_no_change
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        let src = [0x77u8; 4 * 4 * 4];
        let res = owner.write_pixels(
            Rect {
                x: 1000,
                y: 1000,
                w: 4,
                h: 4,
            },
            &src,
            4 * 4,
        );
        assert_eq!(
            res,
            Ok(()),
            "fully out-of-bounds write must succeed (no-op)"
        );
    }

    // 5. negative_origin_clipped_correctly
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        let src = [0x33u8; 10 * 4];
        let res = owner.write_pixels(
            Rect {
                x: -5,
                y: 0,
                w: 10,
                h: 1,
            },
            &src,
            10 * 4,
        );
        assert_eq!(
            res,
            Ok(()),
            "negative-origin write must succeed after clipping"
        );
    }

    // 6. zero_area_write_is_noop_returns_ok
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        let res = owner.write_pixels(
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 5,
            },
            &[],
            0,
        );
        assert_eq!(res, Ok(()), "zero-width write must be no-op Ok");

        let res2 = owner.write_pixels(
            Rect {
                x: 0,
                y: 0,
                w: 5,
                h: 0,
            },
            &[],
            5 * 4,
        );
        assert_eq!(res2, Ok(()), "zero-height write must be no-op Ok");
    }

    // 7. invalid_stride_returns_error
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        let src = [0u8; 64];
        let res = owner.write_pixels(
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 1,
            },
            &src,
            // 4 pixels at 4 bpp = 16 bytes minimum; 8 is too small.
            8,
        );
        assert_eq!(
            res,
            Err(FbError::InvalidStride),
            "stride < row width must error with InvalidStride"
        );
    }

    // 8. truncated_source_returns_error
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        // Need stride * (h-1) + row_bytes = 16 * 3 + 16 = 64 bytes; give 32.
        let src = [0u8; 32];
        let res = owner.write_pixels(
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 4,
            },
            &src,
            16,
        );
        assert_eq!(
            res,
            Err(FbError::Truncated),
            "src shorter than required must error with Truncated"
        );
    }

    // 9. repeated_writes_to_same_rect_succeed
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        let src = [0xAAu8; 8 * 8 * 4];
        for _ in 0..3 {
            let res = owner.write_pixels(
                Rect {
                    x: 5,
                    y: 5,
                    w: 8,
                    h: 8,
                },
                &src,
                8 * 4,
            );
            assert_eq!(res, Ok(()), "repeated writes to same rect must succeed");
        }
    }

    // 10. present_returns_ok
    {
        let m = meta(100, 100);
        let mut owner = constructor(m);
        let res = owner.present();
        assert_eq!(res, Ok(()), "present must return Ok by default");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(width: u32, height: u32) -> FbMetadata {
        FbMetadata {
            width,
            height,
            stride_bytes: width * 4,
            pixel_format: PixelFormat::Bgra8888,
        }
    }

    #[test]
    fn bytes_per_pixel_is_4() {
        assert_eq!(bytes_per_pixel(PixelFormat::Bgra8888), 4);
        assert_eq!(bytes_per_pixel(PixelFormat::Rgba8888), 4);
    }

    #[test]
    fn recording_owner_reports_metadata_unchanged() {
        let m = meta(123, 45);
        let owner = RecordingFramebufferOwner::new(m);
        assert_eq!(owner.metadata(), m);
    }

    #[test]
    fn recording_owner_records_clipped_rect() {
        let m = meta(100, 100);
        let mut owner = RecordingFramebufferOwner::new(m);
        // Straddle the right edge: x=98, w=4 → clipped w=2.
        let src = [0x55u8; 4 * 1 * 4];
        let res = owner.write_pixels(
            Rect {
                x: 98,
                y: 10,
                w: 4,
                h: 1,
            },
            &src,
            4 * 4,
        );
        assert_eq!(res, Ok(()));
        let writes = owner.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].clipped_rect,
            Rect {
                x: 98,
                y: 10,
                w: 2,
                h: 1,
            }
        );
        assert_eq!(writes[0].byte_count, 2 * 4);
    }

    #[test]
    fn recording_owner_pixels_reflect_writes() {
        let m = meta(100, 100);
        let mut owner = RecordingFramebufferOwner::new(m);
        // Build a 4x4 "red" square. In BGRA, red is (B=0, G=0, R=255, A=255)
        // → bytes 0x00, 0x00, 0xFF, 0xFF per pixel; the LE-packed u32 is
        // 0xFFFF_0000.
        let mut src = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..(4 * 4) {
            src.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
        }
        owner
            .write_pixels(
                Rect {
                    x: 10,
                    y: 10,
                    w: 4,
                    h: 4,
                },
                &src,
                4 * 4,
            )
            .expect("in-bounds write must succeed");

        let expected: u32 = 0xFFFF_0000;
        for y in 10..14 {
            for x in 10..14 {
                assert_eq!(owner.pixel(x, y), expected, "pixel at ({x},{y})");
            }
        }
        // A pixel outside the written square must still be zero.
        assert_eq!(owner.pixel(9, 10), 0);
        assert_eq!(owner.pixel(14, 10), 0);
    }

    #[test]
    fn recording_owner_clipping_at_left_edge() {
        let m = meta(100, 100);
        let mut owner = RecordingFramebufferOwner::new(m);
        // Build a row of 10 pixels where bytes encode the source column index
        // in the first byte. After clipping the left 5 pixels, the FB should
        // start at column 5 of the source.
        let mut src = Vec::with_capacity(10 * 4);
        for col in 0..10u8 {
            src.extend_from_slice(&[col, 0, 0, 0xFF]);
        }
        owner
            .write_pixels(
                Rect {
                    x: -5,
                    y: 0,
                    w: 10,
                    h: 1,
                },
                &src,
                10 * 4,
            )
            .expect("clipped write must succeed");

        let writes = owner.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].clipped_rect,
            Rect {
                x: 0,
                y: 0,
                w: 5,
                h: 1,
            }
        );
        // FB column 0 must hold source column 5 (the first surviving pixel).
        // Pixel encoding: low byte = source column index.
        for fb_x in 0..5u32 {
            let pixel = owner.pixel(fb_x, 0);
            let low_byte = (pixel & 0xFF) as u8;
            assert_eq!(
                low_byte,
                fb_x as u8 + 5,
                "FB column {fb_x} should mirror src column {}",
                fb_x + 5
            );
        }
    }

    #[test]
    fn recording_owner_clipping_at_right_edge() {
        let m = meta(100, 100);
        let mut owner = RecordingFramebufferOwner::new(m);
        // Same source-column encoding; rect x=95, w=10 → clipped w=5,
        // and the surviving pixels are source columns 0..5.
        let mut src = Vec::with_capacity(10 * 4);
        for col in 0..10u8 {
            src.extend_from_slice(&[col, 0, 0, 0xFF]);
        }
        owner
            .write_pixels(
                Rect {
                    x: 95,
                    y: 0,
                    w: 10,
                    h: 1,
                },
                &src,
                10 * 4,
            )
            .expect("right-edge clipped write must succeed");

        let writes = owner.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].clipped_rect,
            Rect {
                x: 95,
                y: 0,
                w: 5,
                h: 1,
            }
        );
        for fb_x in 95..100u32 {
            let pixel = owner.pixel(fb_x, 0);
            let low_byte = (pixel & 0xFF) as u8;
            assert_eq!(
                low_byte,
                (fb_x - 95) as u8,
                "FB column {fb_x} should mirror src column {}",
                fb_x - 95
            );
        }
    }

    #[test]
    fn recording_owner_invalid_stride_errors() {
        let m = meta(100, 100);
        let mut owner = RecordingFramebufferOwner::new(m);
        let src = [0u8; 64];
        let res = owner.write_pixels(
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 1,
            },
            &src,
            8, // < 4 * 4
        );
        assert_eq!(res, Err(FbError::InvalidStride));
        assert!(owner.writes().is_empty());
    }

    #[test]
    fn recording_owner_truncated_source_errors() {
        let m = meta(100, 100);
        let mut owner = RecordingFramebufferOwner::new(m);
        // Need stride * 3 + 16 = 64 bytes; give 32.
        let src = [0u8; 32];
        let res = owner.write_pixels(
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 4,
            },
            &src,
            16,
        );
        assert_eq!(res, Err(FbError::Truncated));
        assert!(owner.writes().is_empty());
    }

    #[test]
    fn recording_owner_present_increments_counter() {
        let m = meta(10, 10);
        let mut owner = RecordingFramebufferOwner::new(m);
        assert_eq!(owner.present_calls(), 0);
        owner.present().expect("present always Ok");
        owner.present().expect("present always Ok");
        owner.present().expect("present always Ok");
        assert_eq!(owner.present_calls(), 3);
    }

    #[test]
    fn recording_owner_passes_contract_suite() {
        contract_suite(|m| RecordingFramebufferOwner::new(m));
    }
}
