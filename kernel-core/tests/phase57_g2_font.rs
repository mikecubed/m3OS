//! Phase 57 Track G.2 — `FontProvider` + `BasicBitmapFont` contract.
//!
//! Failing-tests-first discipline: this file commits **before**
//! `kernel-core/src/session/font.rs` and `font_data.rs` exist. Once the
//! green commit lands, every test here should pass without modification.
//!
//! Acceptance covered:
//! - `FontProvider::glyph(0x20)` → `Some` (space).
//! - `FontProvider::glyph(0x7E)` → `Some` (`~`, last printable ASCII).
//! - `FontProvider::glyph(0x80)` → `None` (non-ASCII rejected).
//! - `Glyph::render_into` writes the expected pixel layout for `'A'`.
//! - `Glyph::render_into` returns the typed [`FontError`] when the
//!   caller's buffer is too small.
//!
//! The contract test (last test in the file) drives the trait against a
//! mock font implementation as well, so the trait itself is exercised
//! independently of the concrete `BasicBitmapFont`.

use kernel_core::session::font::{BasicBitmapFont, FontError, FontProvider, Glyph};

/// Phase 57 G.2 acceptance: ASCII space (0x20) is in the bundled font.
#[test]
fn glyph_space_present() {
    let font = BasicBitmapFont::new();
    let g = font.glyph(0x20).expect("0x20 must be present");
    let (w, h) = font.cell_size();
    assert_eq!(g.width, w);
    assert_eq!(g.height, h);
}

/// Phase 57 G.2 acceptance: ASCII tilde (0x7E) is in the bundled font.
/// 0x7E is the last printable ASCII; 0x7F (DEL) is also covered by the
/// bundled font but the canonical printable upper bound is 0x7E.
#[test]
fn glyph_tilde_present() {
    let font = BasicBitmapFont::new();
    assert!(font.glyph(0x7E).is_some(), "0x7E must be present");
}

/// Phase 57 G.2 acceptance: codepoints outside the ASCII printable +
/// DEL range return `None` rather than panicking or returning a
/// placeholder.
#[test]
fn glyph_non_ascii_returns_none() {
    let font = BasicBitmapFont::new();
    assert!(font.glyph(0x80).is_none(), "0x80 must not be present");
    assert!(font.glyph(0x100).is_none(), "0x100 must not be present");
    assert!(
        font.glyph(0x10FFFF).is_none(),
        "U+10FFFF must not be present"
    );
}

/// Phase 57 G.2 acceptance: codepoints below the printable range
/// (control characters except DEL) return `None`. The screen layer is
/// responsible for handling control bytes (newline, BEL, etc.); the
/// font does not own that policy.
#[test]
fn glyph_control_chars_return_none() {
    let font = BasicBitmapFont::new();
    for c in 0u32..0x20 {
        assert!(
            font.glyph(c).is_none(),
            "control char 0x{:02X} must not be present in font",
            c
        );
    }
}

/// Phase 57 G.2 acceptance: cell size is the documented 8×16 (one byte
/// per row, 16 rows).
#[test]
fn cell_size_is_8x16() {
    let font = BasicBitmapFont::new();
    assert_eq!(font.cell_size(), (8, 16));
}

/// Phase 57 G.2 acceptance: `Glyph::render_into` writes BGRA8888
/// pixels into the caller's buffer. We render `'A'` at the origin of an
/// 8-pixel-wide buffer and check that *some* foreground pixels appear
/// in the rendered region (the glyph isn't all background).
#[test]
fn render_into_writes_pixels_for_uppercase_a() {
    let font = BasicBitmapFont::new();
    let g = font.glyph(b'A' as u32).expect("'A' must be present");
    let (w, h) = (g.width as usize, g.height as usize);
    let stride = w;
    let mut buf = vec![0u32; stride * h];
    let fg: u32 = 0x00FFFFFF; // white
    let bg: u32 = 0x00000000; // black
    g.render_into(&mut buf, stride, fg, bg)
        .expect("render must succeed");

    // The glyph for 'A' has pixels set in row 2 (the apex row). We do
    // not pin the exact bit pattern here because the bundled font may
    // be replaced with a different public-domain 8×16 set; we only
    // require that *some* foreground pixel exists.
    let any_fg = buf.iter().any(|&p| p == fg);
    assert!(any_fg, "rendered 'A' must contain at least one fg pixel");
    let any_bg = buf.iter().any(|&p| p == bg);
    assert!(any_bg, "rendered 'A' must contain at least one bg pixel");
}

/// Phase 57 G.2 acceptance: render writes one foreground pixel per set
/// bit and one background pixel per cleared bit; the total cell pixels
/// equal `width * height`.
#[test]
fn render_into_total_pixels_equal_cell_size() {
    let font = BasicBitmapFont::new();
    let g = font.glyph(b'A' as u32).expect("'A' must be present");
    let (w, h) = (g.width as usize, g.height as usize);
    let stride = w;
    let mut buf = vec![0xCAFEBABEu32; stride * h];
    let fg: u32 = 0x11_22_33_44;
    let bg: u32 = 0x55_66_77_88;
    g.render_into(&mut buf, stride, fg, bg)
        .expect("render must succeed");
    // Every pixel must have been overwritten.
    for &px in &buf {
        assert!(
            px == fg || px == bg,
            "pixel was not overwritten: 0x{:08X}",
            px
        );
    }
}

/// Phase 57 G.2 acceptance: `render_into` returns
/// [`FontError::BufferTooSmall`] when the caller's buffer cannot hold
/// the glyph's cell.
#[test]
fn render_into_rejects_too_small_buffer() {
    let font = BasicBitmapFont::new();
    let g = font.glyph(b'A' as u32).expect("'A' must be present");
    let (w, h) = (g.width as usize, g.height as usize);
    let stride = w;
    let mut buf = vec![0u32; stride * (h - 1)];
    let err = g
        .render_into(&mut buf, stride, 0xFFFF_FFFF, 0)
        .expect_err("undersized buffer must error");
    assert!(matches!(err, FontError::BufferTooSmall));
}

/// Phase 57 G.2 acceptance: `render_into` rejects a stride less than the
/// glyph width (would mean overlapping rows).
#[test]
fn render_into_rejects_stride_smaller_than_width() {
    let font = BasicBitmapFont::new();
    let g = font.glyph(b'A' as u32).expect("'A' must be present");
    let (w, h) = (g.width as usize, g.height as usize);
    let mut buf = vec![0u32; w * h];
    let err = g
        .render_into(&mut buf, w - 1, 0xFFFF_FFFF, 0)
        .expect_err("stride < width must error");
    assert!(matches!(err, FontError::InvalidStride));
}

/// Phase 57 G.2 acceptance: contract test — the `FontProvider` trait is
/// driven by both the bundled font and a mock implementation so the
/// trait itself is exercised. The mock returns `Some(Glyph)` for one
/// codepoint and `None` for everything else.
#[test]
fn contract_runs_against_mock_font() {
    static MOCK_BITMAP: [u8; 16] = [0xFF; 16];

    struct MockFont;
    impl FontProvider for MockFont {
        fn glyph(&self, codepoint: u32) -> Option<&Glyph> {
            static GLYPH: Glyph = Glyph {
                width: 8,
                height: 16,
                bitmap: &MOCK_BITMAP,
            };
            if codepoint == b'M' as u32 {
                Some(&GLYPH)
            } else {
                None
            }
        }

        fn cell_size(&self) -> (u8, u8) {
            (8, 16)
        }
    }

    let mock = MockFont;
    assert!(mock.glyph(b'M' as u32).is_some());
    assert!(mock.glyph(b'X' as u32).is_none());
    assert_eq!(mock.cell_size(), (8, 16));

    // Drive the bundled font through the same trait surface.
    let bundled = BasicBitmapFont::new();
    assert!(bundled.glyph(b'M' as u32).is_some());
    assert!(bundled.glyph(0x80).is_none());
}
