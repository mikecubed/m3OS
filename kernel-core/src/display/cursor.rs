//! Phase 56 Track E.3 — pointer cursor rendering and motion-damage math.
//!
//! Test-first scaffold. The tests below define the behavioural
//! contract for [`CursorRenderer`], [`DefaultArrowCursor`],
//! [`ClientCursor`], and [`cursor_damage`]. The implementations are
//! intentionally stubbed (return zeros, return empty) so the test
//! suite fails — the next commit lands the real bodies and turns
//! the suite green.

extern crate alloc;

use heapless::Vec as HeaplessVec;

use crate::display::protocol::{CursorConfig, Rect};

// ---------------------------------------------------------------------------
// CursorRenderer trait
// ---------------------------------------------------------------------------

/// A renderable pointer cursor. The composer asks the renderer for its
/// pixel content one BGRA `u32` at a time; a returned value of `0`
/// means "transparent — skip this framebuffer write" so the underlying
/// surface remains visible.
pub trait CursorRenderer {
    fn size(&self) -> (u32, u32);
    fn hotspot(&self) -> (i32, i32);
    fn sample(&self, x: u32, y: u32) -> u32;
}

// ---------------------------------------------------------------------------
// DefaultArrowCursor — STUB
// ---------------------------------------------------------------------------

/// Stub: real impl lands in the next commit.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultArrowCursor;

impl DefaultArrowCursor {
    pub const fn new() -> Self {
        Self
    }
}

impl CursorRenderer for DefaultArrowCursor {
    fn size(&self) -> (u32, u32) {
        (0, 0) // STUB — real impl returns (12, 16)
    }
    fn hotspot(&self) -> (i32, i32) {
        (0, 0)
    }
    fn sample(&self, _x: u32, _y: u32) -> u32 {
        0
    }
}

/// Width in pixels of the [`DefaultArrowCursor`].
pub const DEFAULT_ARROW_WIDTH: u32 = 12;

/// Height in pixels of the [`DefaultArrowCursor`].
pub const DEFAULT_ARROW_HEIGHT: u32 = 16;

// ---------------------------------------------------------------------------
// ClientCursor — STUB
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ClientCursorError {
    ZeroSize,
    PixelLengthMismatch { expected: usize, actual: usize },
}

/// Stub: real impl lands in the next commit.
#[derive(Clone, Debug)]
pub struct ClientCursor {
    _width: u32,
    _height: u32,
    _hotspot: (i32, i32),
}

impl ClientCursor {
    pub fn new(
        _pixels: &[u32],
        width: u32,
        height: u32,
        cfg: CursorConfig,
    ) -> Result<Self, ClientCursorError> {
        // STUB: accepts everything, no validation.
        Ok(Self {
            _width: width,
            _height: height,
            _hotspot: (cfg.hotspot_x, cfg.hotspot_y),
        })
    }

    pub fn pixels(&self) -> &[u32] {
        &[]
    }
}

impl CursorRenderer for ClientCursor {
    fn size(&self) -> (u32, u32) {
        (0, 0)
    }
    fn hotspot(&self) -> (i32, i32) {
        (0, 0)
    }
    fn sample(&self, _x: u32, _y: u32) -> u32 {
        0
    }
}

// ---------------------------------------------------------------------------
// cursor_damage — STUB
// ---------------------------------------------------------------------------

/// Stub: real impl lands in the next commit.
pub fn cursor_damage(
    _prev_pos: (i32, i32),
    _prev_size: (u32, u32),
    _new_pos: (i32, i32),
    _new_size: (u32, u32),
) -> HeaplessVec<Rect, 2> {
    HeaplessVec::new()
}

// ---------------------------------------------------------------------------
// Contract suite (LSP-style)
// ---------------------------------------------------------------------------

/// Behavioural contract every [`CursorRenderer`] implementation must
/// satisfy. Every impl in the system invokes this in its tests.
pub fn cursor_renderer_contract_suite<C: CursorRenderer>(c: &C) {
    let (w, h) = c.size();
    assert!(w > 0, "cursor width must be non-zero");
    assert!(h > 0, "cursor height must be non-zero");

    for y in 0..h {
        for x in 0..w {
            let _ = c.sample(x, y);
        }
    }

    assert_eq!(c.sample(w, 0), 0, "OOB x={} y=0 must sample as transparent", w);
    assert_eq!(c.sample(0, h), 0, "OOB x=0 y={} must sample as transparent", h);
    assert_eq!(c.sample(w, h), 0, "OOB x={} y={} must sample as transparent", w, h);
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
        assert_ne!(c.sample(0, 0), 0, "(0,0) is the arrow's tip — should be opaque");
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
            other => panic!("expected PixelLengthMismatch, got {:?}", other.is_ok()),
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
