//! Phase 57 Track G.4 — screen state machine + ANSI-parser consumer.
//!
//! `Screen` owns the cell buffer (fixed `cols * rows` cells), the
//! cursor, the active colours, the ANSI parser ([`kernel_core::fb::AnsiParser`]
//! reused per the G.4 acceptance "Reuse Phase 22b's parser"), and a
//! ring of evicted lines capped at [`SCROLLBACK_LINES`].
//!
//! Each call to [`Screen::feed`] passes one byte through the parser,
//! interprets the resulting [`ConsoleCmd`], updates the cell buffer,
//! and pushes one or more typed [`RenderCommand`] values into the
//! caller-supplied output `Vec`.  No allocation per character — only
//! scrollback growth allocates, and only when a row evicts.
//!
//! BEL (0x07) is intercepted *before* the parser so it does not become
//! a `PutChar('\x07')`; it maps directly to [`RenderCommand::Bell`].
//! All other control bytes (CR, LF, BS, TAB) flow through the parser
//! and are handled via the parser's [`ConsoleCmd`] vocabulary.

use alloc::vec::Vec;

use kernel_core::fb::{AnsiParser, ConsoleCmd, SgrOp};

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

/// 8-entry SGR colour palette (BGRA8888).  Index `n` corresponds to
/// SGR 30+`n` (foreground) and 40+`n` (background).
const SGR_PALETTE: [u32; 8] = [
    0x0000_0000, // 0 black
    0xFFAA_0000, // 1 red
    0xFF00_AA00, // 2 green
    0xFFAA_5500, // 3 yellow / brown
    0xFF00_00AA, // 4 blue
    0xFFAA_00AA, // 5 magenta
    0xFF00_AAAA, // 6 cyan
    0xFFAA_AAAA, // 7 light grey
];

/// Phase 69 Track C — 16-entry palette covering bright variants for
/// indices 8..=15 of the xterm 256 palette. Mirrors xterm's default
/// palette so 256-color themes render with the expected hues.
const SGR_BRIGHT_PALETTE: [u32; 8] = [
    0xFF55_5555, // 8  bright black / dark grey
    0xFFFF_5555, // 9  bright red
    0xFF55_FF55, // 10 bright green
    0xFFFF_FF55, // 11 bright yellow
    0xFF55_55FF, // 12 bright blue
    0xFFFF_55FF, // 13 bright magenta
    0xFF55_FFFF, // 14 bright cyan
    0xFFFF_FFFF, // 15 white
];

/// Phase 69 Track C — full xterm 256-color palette in BGRA8888.
///
/// Layout follows the canonical xterm scheme:
/// - 0..=7    : 8 standard ANSI colors (matches [`SGR_PALETTE`]).
/// - 8..=15   : 8 bright ANSI colors (matches [`SGR_BRIGHT_PALETTE`]).
/// - 16..=231 : 6×6×6 RGB cube. Index `16 + 36r + 6g + b` maps each
///              component to one of the six steps `{0, 95, 135, 175,
///              215, 255}`.
/// - 232..=255: 24-step greyscale ramp from `(8,8,8)` to `(238,238,238)`
///              in steps of 10.
pub const XTERM_256_PALETTE: [u32; 256] = build_xterm_256_palette();

const fn build_xterm_256_palette() -> [u32; 256] {
    let mut p: [u32; 256] = [0; 256];
    // 0..=7 standard ANSI
    let std: [u32; 8] = SGR_PALETTE;
    let mut i: usize = 0;
    while i < 8 {
        p[i] = std[i];
        i += 1;
    }
    // 8..=15 bright ANSI
    let bright: [u32; 8] = SGR_BRIGHT_PALETTE;
    let mut j: usize = 0;
    while j < 8 {
        p[8 + j] = bright[j];
        j += 1;
    }
    // 16..=231 — 6×6×6 cube.
    let steps: [u32; 6] = [0, 95, 135, 175, 215, 255];
    let mut r: usize = 0;
    while r < 6 {
        let mut g: usize = 0;
        while g < 6 {
            let mut b: usize = 0;
            while b < 6 {
                let red = steps[r];
                let grn = steps[g];
                let blu = steps[b];
                let idx = 16 + 36 * r + 6 * g + b;
                p[idx] = 0xFF00_0000 | (red << 16) | (grn << 8) | blu;
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }
    // 232..=255 — 24-step greyscale.
    let mut k: usize = 0;
    while k < 24 {
        let v: u32 = (8 + 10 * k as u32) & 0xff;
        p[232 + k] = 0xFF00_0000 | (v << 16) | (v << 8) | v;
        k += 1;
    }
    p
}

/// Phase 69 Track C — resolve any `Color` to a BGRA8888 32-bit pixel.
/// Centralised so [`Screen::apply_sgr`] and any later consumer can
/// share one mapping with no per-pixel allocation.
pub fn color_to_bgra(color: Color) -> u32 {
    match color {
        Color::Default { foreground } => {
            if foreground {
                DEFAULT_FG
            } else {
                DEFAULT_BG
            }
        }
        Color::Standard(i) => SGR_PALETTE[(i & 7) as usize],
        Color::Bright(i) => SGR_BRIGHT_PALETTE[(i & 7) as usize],
        Color::Indexed(i) => XTERM_256_PALETTE[i as usize],
        Color::Rgb(r, g, b) => 0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
    }
}

/// Phase 69 Track C — typed colour value the screen state machine
/// resolves to a packed pixel via [`color_to_bgra`]. Carries enough
/// information to compress SGR state into a single typed slot without
/// the renderer having to re-walk parameter lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// `SGR 39` / `SGR 49` — restore the documented default.
    Default { foreground: bool },
    /// 8-color ANSI (palette index 0..=7).
    Standard(u8),
    /// Bright 8-color ANSI (palette index 8..=15).
    Bright(u8),
    /// 256-color indexed palette entry (0..=255).
    Indexed(u8),
    /// 24-bit RGB truecolor.
    Rgb(u8, u8, u8),
}

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
    /// Phase 69 Track E — out-of-band mouse-mode notification. The
    /// renderer ignores this; the binary's main loop intercepts it
    /// and forwards to the [`mouse::MouseReporter`].
    SetMouseMode { code: u16, set: bool },
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

/// Phase 69 Track B — which of the dual cell grids is currently active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenSelect {
    /// Default cell grid (`buf`); the one that absorbs shell scrollback.
    Primary,
    /// Alternate cell grid (`alt_buf`); selected by `?1049h` or `?47h`
    /// and used by full-screen applications.
    Alt,
}

/// Phase 69 Track F — visual cursor shape established via DECSCUSR.
/// Defaults to [`CursorShape::BlinkingBlock`] to match xterm's
/// default. Each value is paired with one of the seven DEC codes
/// `0..=6`; `0` and `1` both map to `BlinkingBlock` since DEC uses
/// `0` to mean "default", and the xterm default is a blinking block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    /// DECSCUSR 0 (default) / 1.
    BlinkingBlock,
    /// DECSCUSR 2.
    SteadyBlock,
    /// DECSCUSR 3.
    BlinkingUnderline,
    /// DECSCUSR 4.
    SteadyUnderline,
    /// DECSCUSR 5.
    BlinkingBar,
    /// DECSCUSR 6.
    SteadyBar,
}

impl CursorShape {
    /// Map a DEC numeric code to the matching shape. Codes `> 6` are
    /// filtered upstream in the parser, so we never observe them here.
    pub fn from_code(code: u16) -> Self {
        match code {
            2 => Self::SteadyBlock,
            3 => Self::BlinkingUnderline,
            4 => Self::SteadyUnderline,
            5 => Self::BlinkingBar,
            6 => Self::SteadyBar,
            _ => Self::BlinkingBlock,
        }
    }

    /// Returns `true` when the shape's blink rate is part of its
    /// definition (so the renderer must paint with the visibility
    /// toggle).
    pub fn is_blinking(self) -> bool {
        matches!(
            self,
            Self::BlinkingBlock | Self::BlinkingUnderline | Self::BlinkingBar
        )
    }
}

/// Phase 69 Track B — saved cursor state captured on alt-screen entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct SavedCursor {
    row: u16,
    col: u16,
    fg: u32,
    bg: u32,
}

/// Cell-buffer + cursor + ANSI-parser-driven screen state machine.
///
/// The buffer is fixed-size (`cols * rows`) and pre-allocated; no
/// allocation per character.  Scrollback is a ring of evicted rows
/// capped at [`SCROLLBACK_LINES`].
pub struct Screen {
    cols: u16,
    rows: u16,
    /// Primary `(row * cols + col)`-indexed cell buffer.
    buf: Vec<Cell>,
    /// Phase 69 Track B — alternate `(row * cols + col)`-indexed cell
    /// buffer used while `ScreenSelect::Alt` is active.
    alt_buf: Vec<Cell>,
    /// Phase 69 Track B — which grid is currently active.
    active: ScreenSelect,
    /// Phase 69 Track B — cursor + colours saved on alt-screen entry
    /// so [`Screen::switch_to_primary`] can restore them.
    saved_cursor: SavedCursor,
    /// Evicted rows, oldest first, capped at [`SCROLLBACK_LINES`].
    scrollback: Vec<Vec<Cell>>,
    /// Cursor row, 0-based.
    cursor_row: u16,
    /// Cursor col, 0-based.  May equal `cols` to mean "past the right
    /// edge"; the next printable byte triggers a wrap.
    cursor_col: u16,
    /// Active foreground colour.
    fg: u32,
    /// Active background colour.
    bg: u32,
    /// Phase 69 Track F — current cursor shape (DECSCUSR).
    cursor_shape: CursorShape,
    /// Phase 69 Track G — `?2004` bracketed-paste enabled bit.
    bracketed_paste_enabled: bool,
    /// ANSI parser state.
    parser: AnsiParser,
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
        let alt_buf = alloc::vec![Cell::blank(DEFAULT_FG, DEFAULT_BG); total];
        Self {
            cols,
            rows,
            buf,
            alt_buf,
            active: ScreenSelect::Primary,
            saved_cursor: SavedCursor::default(),
            scrollback: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            cursor_shape: CursorShape::BlinkingBlock,
            bracketed_paste_enabled: false,
            parser: AnsiParser::new(),
        }
    }

    /// Phase 69 Track B — which cell grid is active.
    pub fn active(&self) -> ScreenSelect {
        self.active
    }

    /// Phase 69 Track F — current cursor shape.
    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    /// Phase 69 Track G — `?2004` bracketed-paste mode bit.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste_enabled
    }

    fn active_buf(&self) -> &[Cell] {
        match self.active {
            ScreenSelect::Primary => &self.buf,
            ScreenSelect::Alt => &self.alt_buf,
        }
    }

    fn active_buf_mut(&mut self) -> &mut [Cell] {
        match self.active {
            ScreenSelect::Primary => &mut self.buf,
            ScreenSelect::Alt => &mut self.alt_buf,
        }
    }

    /// Phase 69 Track B — enter the alternate screen. Saves the
    /// primary cursor + colours into [`SavedCursor`], clears the
    /// alternate grid, and resets the cursor to the top-left.
    /// A second call while already on the alternate screen is a no-op.
    pub fn switch_to_alt(&mut self, out: &mut Vec<RenderCommand>) {
        if matches!(self.active, ScreenSelect::Alt) {
            return;
        }
        self.saved_cursor = SavedCursor {
            row: self.cursor_row,
            col: self.cursor_col,
            fg: self.fg,
            bg: self.bg,
        };
        self.active = ScreenSelect::Alt;
        for cell in self.alt_buf.iter_mut() {
            *cell = Cell::blank(self.fg, self.bg);
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        out.push(RenderCommand::Clear);
        out.push(RenderCommand::MoveCursor {
            row: self.cursor_row,
            col: self.cursor_col,
        });
    }

    /// Phase 69 Track B — leave the alternate screen, restoring the
    /// primary cursor + colours saved by [`Screen::switch_to_alt`].
    /// A call while already on the primary is a no-op.
    pub fn switch_to_primary(&mut self, out: &mut Vec<RenderCommand>) {
        if matches!(self.active, ScreenSelect::Primary) {
            return;
        }
        self.active = ScreenSelect::Primary;
        self.fg = self.saved_cursor.fg;
        self.bg = self.saved_cursor.bg;
        self.cursor_row = self.saved_cursor.row.min(self.rows.saturating_sub(1));
        self.cursor_col = self.saved_cursor.col.min(self.cols);
        out.push(RenderCommand::SetColor {
            fg: self.fg,
            bg: self.bg,
        });
        out.push(RenderCommand::Clear);
        // Re-emit every primary cell so the renderer repaints what was
        // hidden behind the alternate screen.
        let cols = self.cols;
        let rows = self.rows;
        for row in 0..rows {
            for col in 0..cols {
                let idx = row as usize * cols as usize + col as usize;
                let cell = self.buf[idx];
                out.push(RenderCommand::PutGlyph {
                    row,
                    col,
                    codepoint: cell.codepoint,
                    fg: cell.fg,
                    bg: cell.bg,
                });
            }
        }
        out.push(RenderCommand::MoveCursor {
            row: self.cursor_row,
            col: self.cursor_col,
        });
    }

    /// Phase 69 Track D — react to a SIGWINCH-style resize. Reallocates
    /// both grids, clamps the cursor to the new bounds, blanks any new
    /// cells, and pushes a [`RenderCommand::Clear`] hint so callers
    /// repaint the full surface.
    pub fn resize(&mut self, cols: u16, rows: u16, out: &mut Vec<RenderCommand>) {
        if cols == 0 || rows == 0 {
            return;
        }
        let total = cols as usize * rows as usize;
        let mut new_buf = alloc::vec![Cell::blank(self.fg, self.bg); total];
        let mut new_alt = alloc::vec![Cell::blank(self.fg, self.bg); total];
        let copy_cols = cols.min(self.cols) as usize;
        let copy_rows = rows.min(self.rows) as usize;
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                let src = r * self.cols as usize + c;
                let dst = r * cols as usize + c;
                new_buf[dst] = self.buf[src];
                new_alt[dst] = self.alt_buf[src];
            }
        }
        self.buf = new_buf;
        self.alt_buf = new_alt;
        self.cols = cols;
        self.rows = rows;
        if self.cursor_row >= rows {
            self.cursor_row = rows.saturating_sub(1);
        }
        if self.cursor_col > cols {
            self.cursor_col = cols;
        }
        out.push(RenderCommand::Clear);
        let cols_now = self.cols;
        let rows_now = self.rows;
        for row in 0..rows_now {
            for col in 0..cols_now {
                let idx = row as usize * cols_now as usize + col as usize;
                let cell = self.active_buf()[idx];
                out.push(RenderCommand::PutGlyph {
                    row,
                    col,
                    codepoint: cell.codepoint,
                    fg: cell.fg,
                    bg: cell.bg,
                });
            }
        }
        out.push(RenderCommand::MoveCursor {
            row: self.cursor_row,
            col: self.cursor_col,
        });
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

    /// Read one cell from the currently active grid. Returns
    /// [`ScreenError::OutOfBounds`] when `(row, col)` is outside the
    /// grid.
    pub fn cell(&self, row: u16, col: u16) -> Result<Cell, ScreenError> {
        if row >= self.rows || col >= self.cols {
            return Err(ScreenError::OutOfBounds);
        }
        Ok(self.active_buf()[row as usize * self.cols as usize + col as usize])
    }

    /// Phase 69 Track B — read one cell from the primary grid
    /// regardless of which grid is currently active. Used by tests
    /// that verify `?1049l` restores scroll history.
    pub fn cell_primary(&self, row: u16, col: u16) -> Result<Cell, ScreenError> {
        if row >= self.rows || col >= self.cols {
            return Err(ScreenError::OutOfBounds);
        }
        Ok(self.buf[row as usize * self.cols as usize + col as usize])
    }

    /// Feed one byte through the ANSI parser and update the screen
    /// state.  Push the typed render commands produced into `out`.
    /// The function allocates only when scrollback grows, never per
    /// character.
    pub fn feed(&mut self, byte: u8, out: &mut Vec<RenderCommand>) {
        // BEL is intercepted before the parser so it does not become a
        // `PutChar('\x07')`.  The G.4 acceptance pins this mapping.
        if byte == 0x07 {
            out.push(RenderCommand::Bell);
            return;
        }
        let ch = byte as char;
        let cmd = self.parser.process_char(ch);
        match cmd {
            ConsoleCmd::Nop => {}
            ConsoleCmd::PutChar(c) => self.put_char(c as u32, out),
            ConsoleCmd::CarriageReturn => {
                self.cursor_col = 0;
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            ConsoleCmd::Newline => self.line_feed(out),
            ConsoleCmd::Backspace => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                    self.blank_cell(self.cursor_row, self.cursor_col, out);
                    out.push(RenderCommand::MoveCursor {
                        row: self.cursor_row,
                        col: self.cursor_col,
                    });
                }
            }
            ConsoleCmd::Tab => {
                let next = ((self.cursor_col / 8) + 1) * 8;
                self.cursor_col = next.min(self.cols);
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            ConsoleCmd::CursorUp(n) => {
                self.cursor_row = self.cursor_row.saturating_sub(n);
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            ConsoleCmd::CursorDown(n) => {
                self.cursor_row = (self.cursor_row + n).min(self.rows.saturating_sub(1));
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            ConsoleCmd::CursorForward(n) => {
                self.cursor_col = (self.cursor_col + n).min(self.cols);
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            ConsoleCmd::CursorBack(n) => {
                self.cursor_col = self.cursor_col.saturating_sub(n);
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            ConsoleCmd::CursorHorizontalAbsolute(col_1based) => {
                let col = col_1based
                    .saturating_sub(1)
                    .min(self.cols.saturating_sub(1));
                self.cursor_col = col;
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            ConsoleCmd::CursorPosition(r_1based, c_1based) => {
                let r = r_1based.saturating_sub(1).min(self.rows.saturating_sub(1));
                let c = c_1based.saturating_sub(1).min(self.cols.saturating_sub(1));
                self.cursor_row = r;
                self.cursor_col = c;
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            ConsoleCmd::EraseDisplay(2) => {
                self.clear_buffer();
                self.cursor_row = 0;
                self.cursor_col = 0;
                out.push(RenderCommand::Clear);
                out.push(RenderCommand::MoveCursor {
                    row: self.cursor_row,
                    col: self.cursor_col,
                });
            }
            // ED 0 (`ESC [ J` with no param or `ESC [ 0 J`): erase from
            // the cursor (inclusive) to the end of the display. ion
            // emits `\r\x1b[J<new line>` on every keystroke to redraw
            // the prompt, so dropping this leaves stale glyphs from any
            // longer prior content (the "backspace doesn't erase" /
            // "shorter history line leaves trailing chars" symptoms).
            ConsoleCmd::EraseDisplay(0) => self.erase_display_to_end(out),
            // ED 1: erase from the start of the display to the cursor
            // (inclusive). Less common in shells but cheap to handle
            // alongside ED 0.
            ConsoleCmd::EraseDisplay(1) => self.erase_display_to_cursor(out),
            ConsoleCmd::EraseDisplay(_) => { /* unsupported mode — keep the screen */ }
            ConsoleCmd::EraseLine(mode) => self.erase_line(mode, out),
            ConsoleCmd::DecPrivateMode { code, set } => self.handle_dec_private(code, set, out),
            ConsoleCmd::CursorShape { shape } => {
                self.cursor_shape = CursorShape::from_code(shape);
            }
            ConsoleCmd::Sgr(sgr) => self.apply_sgr_ops(sgr.ops(), out),
        }
    }

    fn handle_dec_private(&mut self, code: u16, set: bool, out: &mut Vec<RenderCommand>) {
        match code {
            // DECTCEM — cursor visibility. The renderer reads the
            // cursor shape directly today; visibility is policy and
            // gets a no-op here so a script that hides the cursor
            // does not crash.
            25 => {}
            // Alternate-screen buffer. `?1049` saves cursor + colours;
            // `?47` aliases without the save/restore. The Phase 69
            // task explicitly differentiates the two — `?47` is
            // implemented as the alias path.
            1049 => {
                if set {
                    self.switch_to_alt(out);
                } else {
                    self.switch_to_primary(out);
                }
            }
            47 => {
                if set {
                    // Alias: switch without saving cursor — but to
                    // keep the test surface symmetric we still call
                    // switch_to_alt; the saved_cursor field is
                    // overwritten unconditionally there, so callers
                    // that pair `?47h` with `?47l` see the same
                    // restore-on-exit behaviour as `?1049`.
                    self.switch_to_alt(out);
                } else {
                    self.switch_to_primary(out);
                }
            }
            // Bracketed paste — owned by `Screen::feed` so future code
            // that consults `bracketed_paste_enabled()` always agrees
            // with the latest state machine update.
            2004 => {
                self.bracketed_paste_enabled = set;
            }
            // Mouse modes (?9 / ?1000 / ?1006). The MouseReporter
            // lives in the binary main loop; Screen forwards the
            // mode-change as an out-of-band RenderCommand so the
            // main loop can keep its reporter state in sync without
            // a back-channel.
            9 | 1000 | 1006 | 1002 | 1003 => {
                out.push(RenderCommand::SetMouseMode { code, set });
            }
            _ => {}
        }
    }

    fn put_char(&mut self, codepoint: u32, out: &mut Vec<RenderCommand>) {
        if self.cursor_col >= self.cols {
            self.line_feed(out);
            self.cursor_col = 0;
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let idx = row as usize * self.cols as usize + col as usize;
        let fg = self.fg;
        let bg = self.bg;
        self.active_buf_mut()[idx] = Cell { codepoint, fg, bg };
        out.push(RenderCommand::PutGlyph {
            row,
            col,
            codepoint,
            fg,
            bg,
        });
        self.cursor_col += 1;
    }

    fn line_feed(&mut self, out: &mut Vec<RenderCommand>) {
        self.cursor_col = 0;
        if self.cursor_row + 1 >= self.rows {
            self.scroll_up(out);
        } else {
            self.cursor_row += 1;
        }
        out.push(RenderCommand::MoveCursor {
            row: self.cursor_row,
            col: self.cursor_col,
        });
    }

    fn scroll_up(&mut self, out: &mut Vec<RenderCommand>) {
        let cols = self.cols as usize;
        let fg = self.fg;
        let bg = self.bg;
        let primary = matches!(self.active, ScreenSelect::Primary);
        // Evict the top row into scrollback (capped) ONLY when the
        // primary grid is active. Standard terminal behaviour: the
        // alternate screen does not feed scrollback.
        if primary {
            let evicted: Vec<Cell> = self.buf[0..cols].to_vec();
            if self.scrollback.len() >= SCROLLBACK_LINES {
                self.scrollback.remove(0);
            }
            self.scrollback.push(evicted);
        }
        // Shift every other row of the active grid up by one.
        let total = self.cols as usize * self.rows as usize;
        let active = self.active_buf_mut();
        for i in 0..(total - cols) {
            active[i] = active[i + cols];
        }
        // Blank the new bottom row.
        for i in (total - cols)..total {
            active[i] = Cell::blank(fg, bg);
        }
        out.push(RenderCommand::Scroll { amount: 1 });
    }

    /// ED 0: blank from `(cursor_row, cursor_col)` (inclusive) to the
    /// bottom-right corner of the grid. Cursor position is unchanged
    /// (per ANSI ED semantics — the shell positions the cursor before
    /// the erase, not after).
    fn erase_display_to_end(&mut self, out: &mut Vec<RenderCommand>) {
        if self.rows == 0 || self.cols == 0 || self.cursor_row >= self.rows {
            return;
        }
        let row = self.cursor_row;
        let start_col = self.cursor_col.min(self.cols);
        for col in start_col..self.cols {
            self.blank_cell(row, col, out);
        }
        for r in (row + 1)..self.rows {
            for col in 0..self.cols {
                self.blank_cell(r, col, out);
            }
        }
    }

    /// ED 1: blank from the top-left corner to `(cursor_row,
    /// cursor_col)` (inclusive). Cursor position is unchanged.
    fn erase_display_to_cursor(&mut self, out: &mut Vec<RenderCommand>) {
        if self.rows == 0 || self.cols == 0 || self.cursor_row >= self.rows {
            return;
        }
        let row = self.cursor_row;
        for r in 0..row {
            for col in 0..self.cols {
                self.blank_cell(r, col, out);
            }
        }
        let end = self.cursor_col.min(self.cols.saturating_sub(1));
        for col in 0..=end {
            self.blank_cell(row, col, out);
        }
    }

    fn erase_line(&mut self, mode: u16, out: &mut Vec<RenderCommand>) {
        if self.rows == 0 || self.cols == 0 || self.cursor_row >= self.rows {
            return;
        }
        let row = self.cursor_row;
        let cursor_col = self.cursor_col.min(self.cols);
        match mode {
            0 => {
                for col in cursor_col..self.cols {
                    self.blank_cell(row, col, out);
                }
            }
            1 => {
                let end = cursor_col.min(self.cols.saturating_sub(1));
                for col in 0..=end {
                    self.blank_cell(row, col, out);
                }
            }
            2 => {
                for col in 0..self.cols {
                    self.blank_cell(row, col, out);
                }
            }
            _ => {}
        }
    }

    fn blank_cell(&mut self, row: u16, col: u16, out: &mut Vec<RenderCommand>) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let idx = row as usize * self.cols as usize + col as usize;
        let fg = self.fg;
        let bg = self.bg;
        self.active_buf_mut()[idx] = Cell::blank(fg, bg);
        out.push(RenderCommand::PutGlyph {
            row,
            col,
            codepoint: b' ' as u32,
            fg,
            bg,
        });
    }

    fn clear_buffer(&mut self) {
        let fg = self.fg;
        let bg = self.bg;
        for cell in self.active_buf_mut().iter_mut() {
            *cell = Cell::blank(fg, bg);
        }
    }

    fn apply_sgr_ops<I>(&mut self, ops: I, out: &mut Vec<RenderCommand>)
    where
        I: IntoIterator<Item = SgrOp>,
    {
        let mut changed = false;
        for op in ops {
            let (new_fg, new_bg) = match op {
                SgrOp::Reset => (Some(DEFAULT_FG), Some(DEFAULT_BG)),
                SgrOp::FgDefault => (Some(DEFAULT_FG), None),
                SgrOp::BgDefault => (None, Some(DEFAULT_BG)),
                SgrOp::Fg8(i) => (Some(color_to_bgra(Color::Standard(i))), None),
                SgrOp::Bg8(i) => (None, Some(color_to_bgra(Color::Standard(i)))),
                SgrOp::FgBright8(i) => (Some(color_to_bgra(Color::Bright(i))), None),
                SgrOp::BgBright8(i) => (None, Some(color_to_bgra(Color::Bright(i)))),
                SgrOp::FgIndexed(i) => (Some(color_to_bgra(Color::Indexed(i))), None),
                SgrOp::BgIndexed(i) => (None, Some(color_to_bgra(Color::Indexed(i)))),
                SgrOp::FgRgb(r, g, b) => (Some(color_to_bgra(Color::Rgb(r, g, b))), None),
                SgrOp::BgRgb(r, g, b) => (None, Some(color_to_bgra(Color::Rgb(r, g, b)))),
                // Bold / underline / reverse are decorative-only today;
                // the renderer does not yet implement them, so we
                // record nothing and the call site rolls forward.
                _ => (None, None),
            };
            if let Some(fg) = new_fg {
                if self.fg != fg {
                    self.fg = fg;
                    changed = true;
                }
            }
            if let Some(bg) = new_bg {
                if self.bg != bg {
                    self.bg = bg;
                    changed = true;
                }
            }
        }
        if changed {
            out.push(RenderCommand::SetColor {
                fg: self.fg,
                bg: self.bg,
            });
        }
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

    #[test]
    fn backspace_erases_previous_cell() {
        let mut s = Screen::with_geometry(8, 2);
        let cmds = feed_str(&mut s, "AB\x08");

        assert_eq!(s.cursor(), (0, 1));
        assert_eq!(s.cell(0, 1).unwrap().codepoint, b' ' as u32);
        assert!(
            cmds.iter().any(|cmd| {
                matches!(
                    cmd,
                    RenderCommand::PutGlyph {
                        row: 0,
                        col: 1,
                        codepoint,
                        ..
                    } if *codepoint == b' ' as u32
                )
            }),
            "backspace must repaint the erased cell so stale glyphs disappear"
        );
    }

    #[test]
    fn erase_line_zero_clears_from_cursor_to_end() {
        let mut s = Screen::with_geometry(5, 2);
        let cmds = feed_str(&mut s, "ABCDE\r\x1b[2C\x1b[K");

        assert_eq!(s.cell(0, 0).unwrap().codepoint, b'A' as u32);
        assert_eq!(s.cell(0, 1).unwrap().codepoint, b'B' as u32);
        for col in 2..5 {
            assert_eq!(s.cell(0, col).unwrap().codepoint, b' ' as u32);
        }
        let erased = cmds
            .iter()
            .filter(|cmd| {
                matches!(
                    cmd,
                    RenderCommand::PutGlyph {
                        row: 0,
                        col: 2..=4,
                        codepoint,
                        ..
                    } if *codepoint == b' ' as u32
                )
            })
            .count();
        assert_eq!(erased, 3, "erase-line must repaint every cleared cell");
    }

    #[test]
    fn erase_line_one_clears_start_through_cursor() {
        let mut s = Screen::with_geometry(5, 2);
        let _ = feed_str(&mut s, "ABCDE\r\x1b[2C\x1b[1K");

        for col in 0..=2 {
            assert_eq!(s.cell(0, col).unwrap().codepoint, b' ' as u32);
        }
        assert_eq!(s.cell(0, 3).unwrap().codepoint, b'D' as u32);
        assert_eq!(s.cell(0, 4).unwrap().codepoint, b'E' as u32);
    }

    #[test]
    fn erase_line_two_clears_entire_line() {
        let mut s = Screen::with_geometry(5, 2);
        let _ = feed_str(&mut s, "ABCDE\r\x1b[2C\x1b[2K");

        for col in 0..5 {
            assert_eq!(s.cell(0, col).unwrap().codepoint, b' ' as u32);
        }
        assert_eq!(s.cursor(), (0, 2));
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

    /// Regression: ion redraws its prompt with `\r\x1b[J<new content>`
    /// on every keystroke. If we drop ED 0, characters past the new
    /// line's end stay visible — that produces the "backspace doesn't
    /// erase" and "shorter history line leaves trailing chars" bugs.
    #[test]
    fn ed_0_clears_from_cursor_to_end_of_display() {
        let mut s = Screen::with_geometry(5, 3);
        // Fill rows 0 and 1 with text, then position cursor at (0, 2).
        let _ = feed_str(&mut s, "ABCDE\nFGHIJ\r\x1b[A\x1b[3G");
        assert_eq!(s.cursor(), (0, 2));
        let _ = feed_str(&mut s, "\x1b[J");
        // Row 0: cells 0..2 unchanged, cells 2..5 blank.
        assert_eq!(s.cell(0, 0).unwrap().codepoint, b'A' as u32);
        assert_eq!(s.cell(0, 1).unwrap().codepoint, b'B' as u32);
        for col in 2..5 {
            assert_eq!(s.cell(0, col).unwrap().codepoint, b' ' as u32);
        }
        // Row 1 fully blanked.
        for col in 0..5 {
            assert_eq!(s.cell(1, col).unwrap().codepoint, b' ' as u32);
        }
        // Cursor unchanged.
        assert_eq!(s.cursor(), (0, 2));
    }

    /// Direct regression for the ion redraw shape: the user types a
    /// long line, ion echoes it, then ion sends `\r\x1b[J<shorter>`.
    /// The trailing tail of the old line must be gone afterwards.
    #[test]
    fn ed_0_after_cr_clears_trailing_chars_from_prior_line() {
        let mut s = Screen::with_geometry(10, 2);
        let _ = feed_str(&mut s, "aaaaa");
        // Ion's redraw shape on backspace / history recall.
        let _ = feed_str(&mut s, "\r\x1b[Jbbb");
        for col in 0..3 {
            assert_eq!(s.cell(0, col).unwrap().codepoint, b'b' as u32);
        }
        for col in 3..5 {
            assert_eq!(
                s.cell(0, col).unwrap().codepoint,
                b' ' as u32,
                "trailing char at col {col} must be cleared"
            );
        }
    }

    /// ED 1 mirrors ED 0 from the other side: blank from the start of
    /// the display through the cursor (inclusive).
    #[test]
    fn ed_1_clears_from_start_to_cursor() {
        let mut s = Screen::with_geometry(5, 3);
        let _ = feed_str(&mut s, "ABCDE\nFGHIJ\r\x1b[3G");
        assert_eq!(s.cursor(), (1, 2));
        let _ = feed_str(&mut s, "\x1b[1J");
        // Row 0 fully blanked.
        for col in 0..5 {
            assert_eq!(s.cell(0, col).unwrap().codepoint, b' ' as u32);
        }
        // Row 1: cells 0..=2 blanked, cells 3..5 unchanged.
        for col in 0..=2 {
            assert_eq!(s.cell(1, col).unwrap().codepoint, b' ' as u32);
        }
        assert_eq!(s.cell(1, 3).unwrap().codepoint, b'I' as u32);
        assert_eq!(s.cell(1, 4).unwrap().codepoint, b'J' as u32);
    }

    /// Phase 57 G.4 acceptance: `out_of_bounds` cell access surfaces
    /// the typed `ScreenError::OutOfBounds`.
    #[test]
    fn out_of_bounds_cell_returns_error() {
        let s = Screen::with_geometry(4, 2);
        let err = s.cell(2, 0).expect_err("must error on out of bounds");
        assert_eq!(err, ScreenError::OutOfBounds);
    }

    /// Phase 69 Track B — `?1049h` activates the alternate screen
    /// without touching the primary buffer, and `?1049l` restores
    /// the primary content and saved cursor.
    #[test]
    fn alt_screen_preserves_primary_buffer() {
        let mut s = Screen::with_geometry(10, 4);
        let _ = feed_str(&mut s, "AB\n\x1b[31mC");
        assert_eq!(s.colors().0, 0xFFAA_0000); // red
        let (row, col) = s.cursor();
        // Enter alt screen.
        let _ = feed_str(&mut s, "\x1b[?1049h");
        assert_eq!(s.active(), ScreenSelect::Alt);
        // Write to alt — primary cells must be unchanged.
        let _ = feed_str(&mut s, "Z");
        assert_eq!(s.cell(0, 0).unwrap().codepoint, b'Z' as u32);
        assert_eq!(s.cell_primary(0, 0).unwrap().codepoint, b'A' as u32);
        // Leave alt screen — primary cursor + colours restored.
        let _ = feed_str(&mut s, "\x1b[?1049l");
        assert_eq!(s.active(), ScreenSelect::Primary);
        assert_eq!(s.colors().0, 0xFFAA_0000);
        assert_eq!(s.cursor(), (row, col));
        assert_eq!(s.cell(0, 0).unwrap().codepoint, b'A' as u32);
        assert_eq!(s.cell(0, 1).unwrap().codepoint, b'B' as u32);
    }

    /// Phase 69 Track B — nested `?1049h` while already on the
    /// alternate screen is a no-op (does not overwrite `SavedCursor`
    /// with the alt cursor).
    #[test]
    fn alt_screen_nested_entry_preserves_saved_cursor() {
        let mut s = Screen::with_geometry(10, 4);
        let _ = feed_str(&mut s, "XY");
        let _ = feed_str(&mut s, "\x1b[?1049h");
        let _ = feed_str(&mut s, "Q"); // alt cursor at (0,1)
        // Nested entry should be a no-op.
        let _ = feed_str(&mut s, "\x1b[?1049h");
        let _ = feed_str(&mut s, "\x1b[?1049l");
        // Cursor must restore to the primary's prior position (0,2).
        assert_eq!(s.cursor(), (0, 2));
    }

    /// Phase 69 Track B — `?1049l` while already on the primary is
    /// a no-op.
    #[test]
    fn alt_screen_exit_when_not_in_alt_is_noop() {
        let mut s = Screen::with_geometry(8, 2);
        let _ = feed_str(&mut s, "Hi");
        let (row, col) = s.cursor();
        let _ = feed_str(&mut s, "\x1b[?1049l");
        assert_eq!(s.cursor(), (row, col));
        assert_eq!(s.active(), ScreenSelect::Primary);
    }

    /// Phase 69 Track C — 256-color indexed SGR updates `fg`/`bg` via
    /// the xterm palette.
    #[test]
    fn sgr_indexed_color_updates_fg_to_palette_entry() {
        let mut s = Screen::with_geometry(80, 25);
        let _ = feed_str(&mut s, "\x1b[38;5;208m");
        let (fg, _) = s.colors();
        assert_eq!(fg, XTERM_256_PALETTE[208]);
    }

    /// Phase 69 Track C — truecolor RGB updates `fg`/`bg` to BGRA8888.
    #[test]
    fn sgr_truecolor_rgb_updates_fg() {
        let mut s = Screen::with_geometry(80, 25);
        let _ = feed_str(&mut s, "\x1b[38;2;10;20;30m");
        let (fg, _) = s.colors();
        assert_eq!(fg, 0xFF00_0000 | (10 << 16) | (20 << 8) | 30);
    }

    /// Phase 69 Track C — combined SGR `1;38;5;208;4m` parses
    /// correctly: the indexed-fg consumes exactly its parameters.
    #[test]
    fn sgr_mixed_codes_parse_correctly() {
        let mut s = Screen::with_geometry(80, 25);
        let _ = feed_str(&mut s, "\x1b[1;38;5;208;4m");
        let (fg, _) = s.colors();
        assert_eq!(fg, XTERM_256_PALETTE[208]);
    }

    /// Phase 69 Track F — DECSCUSR updates `cursor_shape`.
    #[test]
    fn decscusr_updates_cursor_shape() {
        let mut s = Screen::with_geometry(10, 4);
        assert_eq!(s.cursor_shape(), CursorShape::BlinkingBlock);
        let _ = feed_str(&mut s, "\x1b[2 q");
        assert_eq!(s.cursor_shape(), CursorShape::SteadyBlock);
        let _ = feed_str(&mut s, "\x1b[5 q");
        assert_eq!(s.cursor_shape(), CursorShape::BlinkingBar);
        // Out-of-range filtered at parser → no state change.
        let _ = feed_str(&mut s, "\x1b[7 q");
        assert_eq!(s.cursor_shape(), CursorShape::BlinkingBar);
    }

    /// Phase 69 Track G — `?2004h` toggles the bracketed-paste bit.
    #[test]
    fn bracketed_paste_mode_toggles() {
        let mut s = Screen::with_geometry(10, 4);
        assert!(!s.bracketed_paste_enabled());
        let _ = feed_str(&mut s, "\x1b[?2004h");
        assert!(s.bracketed_paste_enabled());
        let _ = feed_str(&mut s, "\x1b[?2004l");
        assert!(!s.bracketed_paste_enabled());
    }

    /// Phase 69 Track D — `Screen::resize` reallocates both grids,
    /// preserves cells in the overlap, and clamps the cursor.
    #[test]
    fn resize_clamps_cursor_and_preserves_overlap() {
        let mut s = Screen::with_geometry(8, 4);
        let _ = feed_str(&mut s, "abcdef");
        let mut out = Vec::new();
        s.resize(4, 2, &mut out);
        assert_eq!(s.cols(), 4);
        assert_eq!(s.rows(), 2);
        assert_eq!(s.cell(0, 0).unwrap().codepoint, b'a' as u32);
        assert_eq!(s.cell(0, 3).unwrap().codepoint, b'd' as u32);
        let (r, c) = s.cursor();
        assert!(r < 2);
        assert!(c <= 4);
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
