//! Phase 57 Track G.4 — screen state machine + ANSI-parser consumer.
//!
//! Red commit: this file declares the public types so the tests
//! compile.  Method bodies that need to produce typed results return
//! a sentinel so the tests fail; the green commit lands the real
//! state machine.

use alloc::vec::Vec;

use crate::{DEFAULT_COLS, DEFAULT_ROWS, SCROLLBACK_LINES};

/// Errors observable on the screen public surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScreenError {
    /// Render command requested an `(row, col)` outside the cell grid.
    OutOfBounds,
}

/// Default foreground colour (white, BGRA8888 packed).
pub const DEFAULT_FG: u32 = 0xFFFF_FFFF;

/// Default background colour (black, BGRA8888 packed).
pub const DEFAULT_BG: u32 = 0x0000_0000;

/// Output of the screen state machine.  Each command is a single typed
/// hint to the renderer; the renderer batches commands per frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderCommand {
    /// Write `codepoint` at `(row, col)` with the given colours.
    PutGlyph {
        row: u16,
        col: u16,
        codepoint: u32,
        fg: u32,
        bg: u32,
    },
    /// Scroll the visible region by `amount` rows (positive = down).
    Scroll { amount: i16 },
    /// Update the active foreground/background colour.
    SetColor { fg: u32, bg: u32 },
    /// BEL (0x07): the renderer rings the audio bell.
    Bell,
    /// Move the cursor to `(row, col)`, both 0-based.
    MoveCursor { row: u16, col: u16 },
    /// Clear the entire screen to the active background colour.
    Clear,
}

/// One cell in the screen buffer.  `codepoint` is the glyph; `fg`/`bg`
/// are the BGRA8888 packed colours at the time the cell was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub codepoint: u32,
    pub fg: u32,
    pub bg: u32,
}

impl Cell {
    /// Empty cell painted with the active colours.
    const fn blank(fg: u32, bg: u32) -> Self {
        Self {
            codepoint: 0x20,
            fg,
            bg,
        }
    }
}

/// Cell-buffer + cursor + ANSI-parser-driven screen state machine.
///
/// The buffer is fixed-size (`cols * rows`) and pre-allocated; no
/// allocation per character.  Scrollback is a ring of evicted rows
/// capped at [`SCROLLBACK_LINES`].
pub struct Screen {
    cols: u16,
    rows: u16,
    /// `(row * cols + col)`-indexed cell buffer.
    buf: Vec<Cell>,
    /// Evicted rows, oldest first, capped at [`SCROLLBACK_LINES`].
    scrollback: Vec<Vec<Cell>>,
    /// Cursor row, 0-based.
    cursor_row: u16,
    /// Cursor col, 0-based.
    cursor_col: u16,
    /// Active foreground colour.
    fg: u32,
    /// Active background colour.
    bg: u32,
    /// ANSI parser state.
    parser: kernel_core::fb::AnsiParser,
}

impl Screen {
    /// Create a new screen with the documented default geometry
    /// ([`DEFAULT_COLS`] × [`DEFAULT_ROWS`]).
    pub fn new() -> Self {
        Self::with_geometry(DEFAULT_COLS, DEFAULT_ROWS)
    }

    /// Create a screen with the supplied geometry. Used by tests; the
    /// production binary always calls [`Screen::new`].
    pub fn with_geometry(cols: u16, rows: u16) -> Self {
        let total = cols as usize * rows as usize;
        let buf = alloc::vec![Cell::blank(DEFAULT_FG, DEFAULT_BG); total];
        Self {
            cols,
            rows,
            buf,
            scrollback: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            parser: kernel_core::fb::AnsiParser::new(),
        }
    }

    /// Cell-grid columns.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Cell-grid rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Cursor position (row, col), both 0-based.
    pub fn cursor(&self) -> (u16, u16) {
        (self.cursor_row, self.cursor_col)
    }

    /// Active colours, `(fg, bg)`.
    pub fn colors(&self) -> (u32, u32) {
        (self.fg, self.bg)
    }

    /// Number of evicted lines currently in the scrollback ring.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Read one cell.  Returns [`ScreenError::OutOfBounds`] when
    /// `(row, col)` is outside the grid.
    pub fn cell(&self, row: u16, col: u16) -> Result<Cell, ScreenError> {
        if row >= self.rows || col >= self.cols {
            return Err(ScreenError::OutOfBounds);
        }
        Ok(self.buf[row as usize * self.cols as usize + col as usize])
    }

    /// Feed one byte through the ANSI parser and update the screen
    /// state.  Returns the typed render command(s) produced; callers
    /// pass each command to the renderer in order.  The function
    /// allocates only when scrollback grows, never per character.
    pub fn feed(&mut self, byte: u8, out: &mut Vec<RenderCommand>) {
        unimplemented!("G.4 green commit lands this");
        let _ = (byte, out);
    }
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn feed_str(screen: &mut Screen, s: &str) -> Vec<RenderCommand> {
        let mut out = Vec::new();
        for b in s.as_bytes() {
            screen.feed(*b, &mut out);
        }
        out
    }

    /// Phase 57 G.4 acceptance: default geometry is the documented
    /// 80×25 grid.
    #[test]
    fn default_geometry_matches_constants() {
        let s = Screen::new();
        assert_eq!(s.cols(), DEFAULT_COLS);
        assert_eq!(s.rows(), DEFAULT_ROWS);
    }

    /// Phase 57 G.4 acceptance: feeding plain ASCII produces a
    /// `PutGlyph` per character with the active colours and advances
    /// the cursor by one column.
    #[test]
    fn put_glyph_advances_cursor() {
        let mut s = Screen::with_geometry(80, 25);
        let cmds = feed_str(&mut s, "Hi");
        assert_eq!(cmds.len(), 2);
        let first = cmds[0];
        match first {
            RenderCommand::PutGlyph {
                row,
                col,
                codepoint,
                ..
            } => {
                assert_eq!(row, 0);
                assert_eq!(col, 0);
                assert_eq!(codepoint, b'H' as u32);
            }
            _ => panic!("expected PutGlyph, got {:?}", first),
        }
        match cmds[1] {
            RenderCommand::PutGlyph {
                row,
                col,
                codepoint,
                ..
            } => {
                assert_eq!(row, 0);
                assert_eq!(col, 1);
                assert_eq!(codepoint, b'i' as u32);
            }
            _ => panic!("expected PutGlyph, got {:?}", cmds[1]),
        }
        assert_eq!(s.cursor(), (0, 2));
    }

    /// Phase 57 G.4 acceptance: BEL (0x07) maps to a single
    /// `RenderCommand::Bell` and never advances the cursor.
    #[test]
    fn bel_maps_to_bell_command() {
        let mut s = Screen::with_geometry(80, 25);
        let mut out = Vec::new();
        s.feed(0x07, &mut out);
        assert_eq!(out.as_slice(), &[RenderCommand::Bell]);
        assert_eq!(s.cursor(), (0, 0));
    }

    /// Phase 57 G.4 acceptance: newline advances to the next row.
    #[test]
    fn newline_advances_row() {
        let mut s = Screen::with_geometry(80, 25);
        let _ = feed_str(&mut s, "A\n");
        assert_eq!(s.cursor(), (1, 0));
    }

    /// Phase 57 G.4 acceptance: carriage return resets the column.
    #[test]
    fn carriage_return_resets_col() {
        let mut s = Screen::with_geometry(80, 25);
        let _ = feed_str(&mut s, "ABC\r");
        assert_eq!(s.cursor(), (0, 0));
    }

    /// Phase 57 G.4 acceptance: writing past the right edge wraps to
    /// the next row.
    #[test]
    fn line_wrap_advances_row() {
        let mut s = Screen::with_geometry(4, 2);
        let _ = feed_str(&mut s, "ABCDE");
        // After 'A','B','C','D' cursor is (0, 4) which means "past
        // the right edge"; the next character ('E') wraps to (1, 1).
        let (row, col) = s.cursor();
        assert!(row >= 1, "expected row >= 1 after wrap, got {row}");
        let _ = col;
    }

    /// Phase 57 G.4 acceptance: writing past the last row scrolls and
    /// pushes the evicted row into scrollback.
    #[test]
    fn scroll_evicts_to_scrollback() {
        let mut s = Screen::with_geometry(4, 2);
        // Fill row 0 then row 1, then trigger another newline.
        let _ = feed_str(&mut s, "ABCD\nEFGH\n");
        // The second newline must scroll once.
        assert!(s.scrollback_len() >= 1);
    }

    /// Phase 57 G.4 acceptance: scrollback caps at SCROLLBACK_LINES;
    /// exceeding the cap drops the oldest line.
    #[test]
    fn scrollback_cap_drops_oldest() {
        let mut s = Screen::with_geometry(4, 2);
        // Force more lines than the cap.
        for _ in 0..(SCROLLBACK_LINES + 5) {
            let _ = feed_str(&mut s, "ABCD\n");
        }
        assert_eq!(s.scrollback_len(), SCROLLBACK_LINES);
    }

    /// Phase 57 G.4 acceptance: SGR 31 (red foreground) updates the
    /// active fg colour.
    #[test]
    fn sgr_red_changes_fg() {
        let mut s = Screen::with_geometry(80, 25);
        let _ = feed_str(&mut s, "\x1b[31m");
        let (fg, _bg) = s.colors();
        assert_ne!(fg, DEFAULT_FG, "fg must change after SGR 31");
    }

    /// Phase 57 G.4 acceptance: SGR 0 resets to defaults.
    #[test]
    fn sgr_reset_restores_defaults() {
        let mut s = Screen::with_geometry(80, 25);
        let _ = feed_str(&mut s, "\x1b[31m\x1b[0m");
        assert_eq!(s.colors(), (DEFAULT_FG, DEFAULT_BG));
    }

    /// Phase 57 G.4 acceptance: ED 2 (clear screen) emits a Clear
    /// command and the cursor is repositioned to the origin.
    #[test]
    fn ed_2_emits_clear() {
        let mut s = Screen::with_geometry(80, 25);
        let cmds = feed_str(&mut s, "ABC\x1b[2J");
        assert!(cmds.iter().any(|c| matches!(c, RenderCommand::Clear)));
    }

    /// Phase 57 G.4 acceptance: `out_of_bounds` cell access surfaces
    /// the typed `ScreenError::OutOfBounds`.
    #[test]
    fn out_of_bounds_cell_returns_error() {
        let s = Screen::with_geometry(4, 2);
        let err = s.cell(2, 0).expect_err("must error on out of bounds");
        assert_eq!(err, ScreenError::OutOfBounds);
    }

    /// Phase 57 G.4 property test: arbitrary ANSI byte sequences must
    /// not panic, must not produce out-of-bounds cursor positions, and
    /// must keep `scrollback_len` <= SCROLLBACK_LINES.  We use a
    /// hand-rolled fuzz loop because `proptest` is not a dev-dep of
    /// the term crate.
    #[test]
    fn property_arbitrary_bytes_dont_panic() {
        let mut s = Screen::with_geometry(8, 4);
        let mut out = Vec::new();
        // 4 KiB of pseudo-random bytes.
        let mut state = 0xCAFEBABEu32;
        for _ in 0..4096 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let b = (state >> 16) as u8;
            s.feed(b, &mut out);
            // Cursor invariants.
            let (r, c) = s.cursor();
            assert!(r < s.rows());
            assert!(c <= s.cols());
            // Scrollback cap.
            assert!(s.scrollback_len() <= SCROLLBACK_LINES);
            // Drain command buffer between iterations so the property
            // does not OOM on a 4 KiB run.
            out.clear();
        }
    }
}
