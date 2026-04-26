//! Phase 56 Track E.3 — pointer cursor rendering and motion-damage math.
//!
//! This module is **pure logic**. It declares the [`CursorRenderer`]
//! trait that the composer (`userspace/display_server/src/compose.rs`)
//! samples once per frame, plus two impls:
//!
//! * [`DefaultArrowCursor`] — a built-in, software-drawn 12×16 BGRA
//!   arrow used whenever no client has set a `Cursor`-role surface.
//!   Without this, a fresh boot would have an invisible pointer.
//! * [`ClientCursor`] — wraps a client-supplied pixel buffer (the
//!   bytes most recently committed against a `SurfaceRole::Cursor`)
//!   plus the client's hotspot, taken from
//!   [`crate::display::protocol::CursorConfig`].
//!
//! [`cursor_damage`] returns the union of damage rectangles for a
//! pointer-motion event so the composer knows exactly which output
//! pixels need to be re-blitted.
//!
//! ## Design notes
//!
//! Geometry math widens to `i64` so an adversarial pointer position
//! near `i32::MAX` cannot wrap silently — same defense the rest of the
//! display module uses (see `compose::rect_intersect` and
//! `dispatch::rect_contains`).
//!
//! [`cursor_renderer_contract_suite`] is the LSP-style assertion bundle
//! every [`CursorRenderer`] impl must satisfy. `DefaultArrowCursor`
//! and `ClientCursor` are both invoked through it in the unit tests
//! below; future themed-cursor impls reuse the same suite without
//! copy-pasting the assertions.

extern crate alloc;

use heapless::Vec as HeaplessVec;

use crate::display::protocol::{CursorConfig, Rect};

// ---------------------------------------------------------------------------
// Constants for the built-in arrow cursor
// ---------------------------------------------------------------------------

/// Width in pixels of the [`DefaultArrowCursor`].
pub const DEFAULT_ARROW_WIDTH: u32 = 12;

/// Height in pixels of the [`DefaultArrowCursor`].
pub const DEFAULT_ARROW_HEIGHT: u32 = 16;

/// BGRA8888 packed `u32` for opaque black. Using little-endian byte
/// order — `to_le_bytes` yields `[B, G, R, A]` = `[0x00, 0x00, 0x00,
/// 0xFF]`.
const ARROW_BLACK: u32 = 0xFF00_0000;

/// BGRA8888 packed `u32` for opaque white. The arrow's anti-bleeding
/// outline.
const ARROW_WHITE: u32 = 0xFFFF_FFFF;

/// Transparent — sampled pixel value `0` instructs the composer to
/// leave the underlying surface visible (no framebuffer write).
const ARROW_TRANSPARENT: u32 = 0;

/// 12×16 classic-arrow bitmap. Byte values:
/// * `0` → transparent
/// * `1` → white outline
/// * `2` → black body
const ARROW_PIXELS: [[u8; 12]; 16] = [
    [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 2, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 2, 2, 2, 1, 0, 0, 0, 0, 0, 0, 0],
    [1, 2, 2, 2, 2, 1, 0, 0, 0, 0, 0, 0],
    [1, 2, 2, 2, 2, 2, 1, 0, 0, 0, 0, 0],
    [1, 2, 2, 2, 2, 2, 2, 1, 0, 0, 0, 0],
    [1, 2, 2, 2, 2, 2, 2, 2, 1, 0, 0, 0],
    [1, 2, 2, 2, 2, 2, 1, 1, 1, 1, 0, 0],
    [1, 2, 2, 1, 2, 2, 1, 0, 0, 0, 0, 0],
    [1, 2, 1, 0, 1, 2, 2, 1, 0, 0, 0, 0],
    [1, 1, 0, 0, 1, 2, 2, 1, 0, 0, 0, 0],
    [1, 0, 0, 0, 0, 1, 2, 2, 1, 0, 0, 0],
    [0, 0, 0, 0, 0, 1, 2, 2, 1, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0],
];

// ---------------------------------------------------------------------------
// CursorRenderer trait
// ---------------------------------------------------------------------------

/// A renderable pointer cursor. The composer asks the renderer for its
/// pixel content one BGRA `u32` at a time; a returned value of `0`
/// means "transparent — skip this framebuffer write" so the underlying
/// surface remains visible.
///
/// Impls must be pure: `sample` must not mutate visible state.
///
/// # Bounds contract
///
/// Callers must respect the `size()` rectangle: `x < size().0` and
/// `y < size().1`. Out-of-bounds queries return `0` (transparent),
/// which is a safe default but indicates a caller bug — the composer
/// validates the ranges before sampling.
pub trait CursorRenderer {
    /// Width and height of the cursor bitmap in pixels. Must both be
    /// non-zero — a renderer that returned `0` would be
    /// mathematically invisible.
    fn size(&self) -> (u32, u32);

    /// Hotspot offset from the bitmap's top-left corner, in pixels.
    /// The composer subtracts this from the pointer position to find
    /// the cursor's screen origin.
    fn hotspot(&self) -> (i32, i32);

    /// Sample one pixel of the cursor bitmap at offset `(x, y)`.
    /// Format is BGRA8888 packed into a `u32` (alpha in the high byte
    /// for native little-endian targets after a `to_le_bytes` round-
    /// trip, matching `compose::FbMetadata::pixel_format =
    /// PixelFormat::Bgra8888`).
    ///
    /// A returned value of `0` (all bytes zero) signals transparency:
    /// the composer skips the write so the surface beneath shows
    /// through.
    fn sample(&self, x: u32, y: u32) -> u32;
}

// ---------------------------------------------------------------------------
// DefaultArrowCursor
// ---------------------------------------------------------------------------

/// Built-in software-drawn arrow cursor. Used by the composer whenever
/// no client has registered a `Cursor`-role surface — this prevents
/// an invisible pointer at boot before any UI has come up.
///
/// Hotspot is `(0, 0)` (top-left), so the arrow's tip sits exactly on
/// the pointer position.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultArrowCursor;

impl DefaultArrowCursor {
    /// Construct the singleton default cursor. Stateless.
    pub const fn new() -> Self {
        Self
    }
}

impl CursorRenderer for DefaultArrowCursor {
    fn size(&self) -> (u32, u32) {
        (DEFAULT_ARROW_WIDTH, DEFAULT_ARROW_HEIGHT)
    }

    fn hotspot(&self) -> (i32, i32) {
        (0, 0)
    }

    fn sample(&self, x: u32, y: u32) -> u32 {
        if x >= DEFAULT_ARROW_WIDTH || y >= DEFAULT_ARROW_HEIGHT {
            return ARROW_TRANSPARENT;
        }
        match ARROW_PIXELS[y as usize][x as usize] {
            1 => ARROW_WHITE,
            2 => ARROW_BLACK,
            _ => ARROW_TRANSPARENT,
        }
    }
}

// ---------------------------------------------------------------------------
// ClientCursor
// ---------------------------------------------------------------------------

/// Errors emitted when constructing a [`ClientCursor`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ClientCursorError {
    /// `width` or `height` was zero.
    ZeroSize,
    /// The pixel slice length did not equal `width * height`.
    PixelLengthMismatch { expected: usize, actual: usize },
}

/// A client-provided cursor bitmap. Wraps the pixel buffer the client
/// most recently committed against a `SurfaceRole::Cursor` surface,
/// plus the [`CursorConfig`] hotspot from the same `SetSurfaceRole`
/// verb.
///
/// Pixel data is stored as packed BGRA `u32`s (little-endian byte
/// order on the wire), matching the framebuffer's
/// `PixelFormat::Bgra8888` and the same shape `DefaultArrowCursor`
/// emits — so the composer never has to format-convert.
#[derive(Clone, Debug)]
pub struct ClientCursor {
    pixels: alloc::vec::Vec<u32>,
    width: u32,
    height: u32,
    hotspot_x: i32,
    hotspot_y: i32,
}

impl ClientCursor {
    /// Construct a [`ClientCursor`] from a pixel buffer + dimensions +
    /// hotspot config.
    ///
    /// Returns [`ClientCursorError::ZeroSize`] if either dimension is
    /// zero (invisible cursors are a protocol error — clients should
    /// destroy the surface instead) or
    /// [`ClientCursorError::PixelLengthMismatch`] if the slice length
    /// does not equal `width * height`.
    pub fn new(
        pixels: &[u32],
        width: u32,
        height: u32,
        cfg: CursorConfig,
    ) -> Result<Self, ClientCursorError> {
        Self::from_vec(pixels.to_vec(), width, height, cfg)
    }

    /// Owning variant of [`new`] that avoids an extra `to_vec()` clone
    /// when the caller has already built a `Vec<u32>` (e.g.
    /// `userspace/display_server/src/surface.rs::cursor_from_committed`
    /// decoding the committed BGRA byte stream).
    pub fn from_vec(
        pixels: alloc::vec::Vec<u32>,
        width: u32,
        height: u32,
        cfg: CursorConfig,
    ) -> Result<Self, ClientCursorError> {
        if width == 0 || height == 0 {
            return Err(ClientCursorError::ZeroSize);
        }
        // Width × height widens through `saturating_mul`. On a 32-bit
        // host this cannot overflow for any cursor that fits in memory;
        // on a 64-bit host (where `usize` is 64-bit) the saturating-to-
        // `usize::MAX` fallback still produces a length-mismatch error,
        // so a malformed call cannot squeak past validation.
        let expected = (width as usize).saturating_mul(height as usize);
        if pixels.len() != expected {
            return Err(ClientCursorError::PixelLengthMismatch {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            pixels,
            width,
            height,
            hotspot_x: cfg.hotspot_x,
            hotspot_y: cfg.hotspot_y,
        })
    }

    /// Read-only access to the backing pixel buffer. The composer
    /// uses this for occasional bulk-blit fast paths; tests use it
    /// to assert the buffer round-tripped intact.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }
}

impl CursorRenderer for ClientCursor {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn hotspot(&self) -> (i32, i32) {
        (self.hotspot_x, self.hotspot_y)
    }

    fn sample(&self, x: u32, y: u32) -> u32 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let idx = (y as usize)
            .checked_mul(self.width as usize)
            .and_then(|row_off| row_off.checked_add(x as usize));
        match idx.and_then(|i| self.pixels.get(i)) {
            Some(p) => *p,
            None => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Damage math
// ---------------------------------------------------------------------------

/// Compute the union of damage rectangles for a pointer motion event.
///
/// Returns:
/// * an empty `Vec` if the cursor is stationary (`prev == new` and
///   the size hasn't changed),
/// * one rect (the bounding box) when the previous and new cursor
///   bitmaps overlap,
/// * two disjoint rects when they don't (or just touch — half-open
///   convention treats touching edges as non-overlapping).
///
/// Geometry math widens to `i64` so an adversarial pointer position
/// near `i32::MAX` cannot overflow.
pub fn cursor_damage(
    prev_pos: (i32, i32),
    prev_size: (u32, u32),
    new_pos: (i32, i32),
    new_size: (u32, u32),
) -> HeaplessVec<Rect, 2> {
    let mut out: HeaplessVec<Rect, 2> = HeaplessVec::new();

    // Stationary motion: identical position AND identical size →
    // nothing to redraw.
    if prev_pos == new_pos && prev_size == new_size {
        return out;
    }

    let prev_rect = make_rect_i64(prev_pos, prev_size);
    let new_rect = make_rect_i64(new_pos, new_size);

    match (prev_rect, new_rect) {
        (Some(p), Some(n)) => {
            if rects_overlap_i64(&p, &n) {
                let union = union_i64(&p, &n);
                if let Some(rect) = i64_rect_to_rect(union) {
                    let _ = out.push(rect);
                }
            } else {
                if let Some(rect) = i64_rect_to_rect(p) {
                    let _ = out.push(rect);
                }
                if let Some(rect) = i64_rect_to_rect(n) {
                    let _ = out.push(rect);
                }
            }
        }
        (Some(p), None) => {
            if let Some(rect) = i64_rect_to_rect(p) {
                let _ = out.push(rect);
            }
        }
        (None, Some(n)) => {
            if let Some(rect) = i64_rect_to_rect(n) {
                let _ = out.push(rect);
            }
        }
        (None, None) => {}
    }

    out
}

/// Internal i64-domain rect used by [`cursor_damage`].
#[derive(Clone, Copy, Debug)]
struct RectI64 {
    /// Inclusive left edge.
    x0: i64,
    /// Inclusive top edge.
    y0: i64,
    /// Exclusive right edge.
    x1: i64,
    /// Exclusive bottom edge.
    y1: i64,
}

fn make_rect_i64(pos: (i32, i32), size: (u32, u32)) -> Option<RectI64> {
    if size.0 == 0 || size.1 == 0 {
        return None;
    }
    let x0 = pos.0 as i64;
    let y0 = pos.1 as i64;
    let x1 = x0.checked_add(size.0 as i64)?;
    let y1 = y0.checked_add(size.1 as i64)?;
    Some(RectI64 { x0, y0, x1, y1 })
}

/// Two rectangles overlap iff each axis range overlaps. Touching
/// edges (right-edge of A equals left-edge of B) count as
/// non-overlapping under the half-open convention used everywhere
/// else in the display module.
fn rects_overlap_i64(a: &RectI64, b: &RectI64) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

fn union_i64(a: &RectI64, b: &RectI64) -> RectI64 {
    RectI64 {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    }
}

/// Convert an i64-domain rect back to the protocol's
/// `(i32, u32)`-domain `Rect`. Returns `None` if the rect's origin
/// or extent does not fit.
fn i64_rect_to_rect(r: RectI64) -> Option<Rect> {
    let x = i32::try_from(r.x0).ok()?;
    let y = i32::try_from(r.y0).ok()?;
    let w_i64 = r.x1 - r.x0;
    let h_i64 = r.y1 - r.y0;
    if w_i64 <= 0 || h_i64 <= 0 {
        return None;
    }
    let w = u32::try_from(w_i64).ok()?;
    let h = u32::try_from(h_i64).ok()?;
    Some(Rect { x, y, w, h })
}

// ---------------------------------------------------------------------------
// Contract suite (LSP-style)
// ---------------------------------------------------------------------------

/// Behavioural contract every [`CursorRenderer`] implementation must
/// satisfy. Mirrors `display::layout::layout_contract_suite`'s shape
/// so adding a future themed-cursor impl is a one-line registration
/// in the test module — no copy-paste of assertions.
///
/// Asserts:
/// * `size()` is non-zero in both axes,
/// * `sample()` for every in-bounds offset returns *some* `u32`
///   (no panic, no overflow path),
/// * `sample()` for an out-of-bounds offset returns `0`
///   (transparent — the safe default).
///
/// Pure observation; safe to call against any well-behaved impl.
pub fn cursor_renderer_contract_suite<C: CursorRenderer>(c: &C) {
    let (w, h) = c.size();
    assert!(w > 0, "cursor width must be non-zero");
    assert!(h > 0, "cursor height must be non-zero");

    // In-bounds sweep: every coordinate samples without panicking.
    // We don't constrain the exact value (transparency is fine) —
    // only that the call returns.
    for y in 0..h {
        for x in 0..w {
            let _ = c.sample(x, y);
        }
    }

    // Out-of-bounds returns transparent.
    assert_eq!(
        c.sample(w, 0),
        0,
        "OOB x={} y=0 must sample as transparent",
        w
    );
    assert_eq!(
        c.sample(0, h),
        0,
        "OOB x=0 y={} must sample as transparent",
        h
    );
    assert_eq!(
        c.sample(w, h),
        0,
        "OOB x={} y={} must sample as transparent",
        w,
        h
    );
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn cfg(hx: i32, hy: i32) -> CursorConfig {
        CursorConfig {
            hotspot_x: hx,
            hotspot_y: hy,
        }
    }

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    // ---- DefaultArrowCursor -------------------------------------------

    #[test]
    fn default_cursor_size_and_hotspot() {
        let c = DefaultArrowCursor::new();
        assert_eq!(c.size(), (DEFAULT_ARROW_WIDTH, DEFAULT_ARROW_HEIGHT));
        assert_eq!(c.hotspot(), (0, 0));
    }

    #[test]
    fn default_cursor_in_bounds_returns_value() {
        let c = DefaultArrowCursor::new();
        // The arrow's tip at (0, 0) is the white outline pixel — opaque.
        assert_ne!(
            c.sample(0, 0),
            0,
            "(0,0) is the arrow's tip — should be opaque"
        );
    }

    #[test]
    fn default_cursor_oob_returns_transparent() {
        let c = DefaultArrowCursor::new();
        assert_eq!(c.sample(DEFAULT_ARROW_WIDTH, 0), 0);
        assert_eq!(c.sample(0, DEFAULT_ARROW_HEIGHT), 0);
        assert_eq!(c.sample(u32::MAX, u32::MAX), 0);
    }

    #[test]
    fn default_cursor_passes_contract_suite() {
        let c = DefaultArrowCursor::new();
        cursor_renderer_contract_suite(&c);
    }

    // ---- ClientCursor -------------------------------------------------

    #[test]
    fn client_cursor_zero_size_rejected() {
        let res_w = ClientCursor::new(&[], 0, 4, cfg(0, 0));
        assert!(matches!(res_w, Err(ClientCursorError::ZeroSize)));
        let res_h = ClientCursor::new(&[], 4, 0, cfg(0, 0));
        assert!(matches!(res_h, Err(ClientCursorError::ZeroSize)));
    }

    #[test]
    fn client_cursor_pixel_length_mismatch_rejected() {
        let too_short = vec![0xFF00_FF00u32; 3];
        let res = ClientCursor::new(&too_short, 2, 2, cfg(1, 1));
        match res {
            Err(ClientCursorError::PixelLengthMismatch { expected, actual }) => {
                assert_eq!(expected, 4);
                assert_eq!(actual, 3);
            }
            other => panic!("expected PixelLengthMismatch, got ok={}", other.is_ok()),
        }
    }

    #[test]
    fn client_cursor_round_trips_pixels_and_hotspot() {
        let pixels: alloc::vec::Vec<u32> = (0..16).map(|i| 0xFF00_0000u32 | i).collect();
        let c = ClientCursor::new(&pixels, 4, 4, cfg(2, 3)).expect("valid client cursor");
        assert_eq!(c.size(), (4, 4));
        assert_eq!(c.hotspot(), (2, 3));
        for y in 0..4 {
            for x in 0..4 {
                let expected = 0xFF00_0000u32 | (y * 4 + x);
                assert_eq!(c.sample(x, y), expected, "(x={},y={})", x, y);
            }
        }
    }

    #[test]
    fn client_cursor_oob_returns_transparent() {
        let pixels: alloc::vec::Vec<u32> = vec![0xDEAD_BEEFu32; 4];
        let c = ClientCursor::new(&pixels, 2, 2, cfg(0, 0)).expect("ok");
        assert_eq!(c.sample(2, 0), 0);
        assert_eq!(c.sample(0, 2), 0);
        assert_eq!(c.sample(99, 99), 0);
    }

    #[test]
    fn client_cursor_passes_contract_suite() {
        let pixels: alloc::vec::Vec<u32> = vec![0xFF80_8080u32; 16];
        let c = ClientCursor::new(&pixels, 4, 4, cfg(1, 2)).expect("ok");
        cursor_renderer_contract_suite(&c);
    }

    // ---- cursor_damage ----------------------------------------------

    #[test]
    fn damage_stationary_motion_empty() {
        let v = cursor_damage((100, 100), (12, 16), (100, 100), (12, 16));
        assert!(v.is_empty(), "stationary motion must produce no damage");
    }

    #[test]
    fn damage_diagonal_disjoint_two_rects() {
        let v = cursor_damage((10, 10), (12, 16), (300, 300), (12, 16));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], rect(10, 10, 12, 16));
        assert_eq!(v[1], rect(300, 300, 12, 16));
    }

    #[test]
    fn damage_overlapping_collapses_to_bounding_box() {
        let v = cursor_damage((10, 10), (12, 16), (15, 15), (12, 16));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], rect(10, 10, 17, 21));
    }

    #[test]
    fn damage_touching_edges_disjoint_two_rects() {
        // Half-open convention: prev right-edge at x=22 (10+12)
        // touches new left-edge at x=22 — non-overlapping.
        let v = cursor_damage((10, 10), (12, 16), (22, 10), (12, 16));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], rect(10, 10, 12, 16));
        assert_eq!(v[1], rect(22, 10, 12, 16));
    }

    #[test]
    fn damage_size_change_only_overlapping_collapses() {
        let v = cursor_damage((50, 50), (12, 16), (50, 50), (24, 32));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], rect(50, 50, 24, 32));
    }

    #[test]
    fn damage_widens_to_i64_for_extreme_positions() {
        // Must not panic at i32 extremes.
        let v = cursor_damage((i32::MAX - 4, 0), (12, 16), (i32::MIN + 4, 0), (12, 16));
        assert!(v.len() <= 2);
    }

    // ---- contract suite invocations -------------------------------

    #[test]
    fn contract_suite_exercises_default_and_client() {
        cursor_renderer_contract_suite(&DefaultArrowCursor::new());
        let pixels: alloc::vec::Vec<u32> = vec![0xFF00_FF00u32; 4];
        let c = ClientCursor::new(&pixels, 2, 2, cfg(0, 0)).expect("ok");
        cursor_renderer_contract_suite(&c);
    }
}
