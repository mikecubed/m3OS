//! ANSI/VT100 escape sequence parser for the framebuffer console.
//!
//! This module lives in `kernel-core` so it can be unit-tested on the host
//! (`cargo test -p kernel-core`) without needing a real framebuffer or QEMU.
//!
//! The parser produces [`ConsoleCmd`] values that the kernel's `FbConsole`
//! executes against the real framebuffer.

/// Maximum number of CSI numeric parameters we track.
const MAX_PARAMS: usize = 8;

/// A command produced by the escape sequence parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleCmd {
    /// Print a visible character at the current cursor position.
    PutChar(char),
    /// Carriage return — move cursor to column 0.
    CarriageReturn,
    /// Newline — advance to next line.
    Newline,
    /// Backspace — move cursor back one column, erase cell.
    Backspace,
    /// Tab — advance to next 8-column tab stop.
    Tab,
    /// Cursor Up by `n` rows.
    CursorUp(u16),
    /// Cursor Down by `n` rows.
    CursorDown(u16),
    /// Cursor Forward by `n` columns.
    CursorForward(u16),
    /// Cursor Back by `n` columns.
    CursorBack(u16),
    /// Cursor Horizontal Absolute — move to column `n` (1-based).
    CursorHorizontalAbsolute(u16),
    /// Cursor Position — move to (row, col), both 1-based.
    CursorPosition(u16, u16),
    /// Erase in Line: 0 = cursor to end, 1 = start to cursor, 2 = entire line.
    EraseLine(u16),
    /// Erase in Display: 0 = cursor to end, 1 = start to cursor, 2 = entire screen.
    EraseDisplay(u16),
    /// Show/hide cursor (DECTCEM).
    SetCursorVisible(bool),
    /// SGR — Set Graphic Rendition. Parameters stored as a slice reference
    /// isn't possible in a Copy enum, so we use a small inline array.
    Sgr(SgrParams),
    /// Unknown/unsupported sequence — silently ignored.
    Nop,
}

/// Inline storage for SGR parameters (up to MAX_PARAMS values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SgrParams {
    pub params: [u16; MAX_PARAMS],
    pub count: usize,
}

/// Parser state for the escape sequence state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscState {
    /// Normal text mode — characters are printed directly.
    Normal,
    /// Saw ESC (0x1B) — waiting for `[` or discarding.
    Escape,
    /// Inside a CSI sequence (ESC [) — accumulating parameters.
    Csi,
    /// Inside a CSI private sequence (ESC [ ?) — accumulating parameters.
    CsiPrivate,
}

/// ANSI escape sequence parser.
///
/// Feed characters one at a time via [`process_char`]. Each call returns a
/// [`ConsoleCmd`] that the framebuffer console should execute.
#[derive(Debug, Clone)]
pub struct AnsiParser {
    state: EscState,
    params: [u16; MAX_PARAMS],
    param_count: usize,
}

impl Default for AnsiParser {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiParser {
    /// Create a new parser in the Normal state.
    pub const fn new() -> Self {
        AnsiParser {
            state: EscState::Normal,
            params: [0; MAX_PARAMS],
            param_count: 0,
        }
    }

    /// Reset the parser to initial state.
    fn reset(&mut self) {
        self.state = EscState::Normal;
        self.params = [0; MAX_PARAMS];
        self.param_count = 0;
    }

    /// Reset CSI parameter accumulation.
    fn reset_params(&mut self) {
        self.params = [0; MAX_PARAMS];
        self.param_count = 0;
    }

    /// Get parameter at index with a default value if not provided.
    fn param(&self, idx: usize, default: u16) -> u16 {
        if idx < self.param_count {
            let v = self.params[idx];
            if v == 0 {
                default
            } else {
                v
            }
        } else {
            default
        }
    }

    /// Process a single character through the state machine.
    /// Returns the command the console should execute.
    pub fn process_char(&mut self, c: char) -> ConsoleCmd {
        match self.state {
            EscState::Normal => self.process_normal(c),
            EscState::Escape => self.process_escape(c),
            EscState::Csi => self.process_csi(c),
            EscState::CsiPrivate => self.process_csi_private(c),
        }
    }

    fn process_normal(&mut self, c: char) -> ConsoleCmd {
        match c {
            '\x1b' => {
                self.state = EscState::Escape;
                ConsoleCmd::Nop
            }
            '\r' => ConsoleCmd::CarriageReturn,
            '\n' => ConsoleCmd::Newline,
            '\x08' => ConsoleCmd::Backspace,
            '\t' => ConsoleCmd::Tab,
            _ => ConsoleCmd::PutChar(c),
        }
    }

    fn process_escape(&mut self, c: char) -> ConsoleCmd {
        match c {
            '[' => {
                self.state = EscState::Csi;
                self.reset_params();
                ConsoleCmd::Nop
            }
            _ => {
                // Unknown escape sequence — discard and return to normal.
                self.state = EscState::Normal;
                ConsoleCmd::Nop
            }
        }
    }

    fn process_csi(&mut self, c: char) -> ConsoleCmd {
        match c {
            '0'..='9' => {
                // Accumulate digit into current parameter.
                if self.param_count == 0 {
                    self.param_count = 1;
                }
                let idx = self.param_count - 1;
                if idx < MAX_PARAMS {
                    self.params[idx] = self.params[idx]
                        .saturating_mul(10)
                        .saturating_add(c as u16 - b'0' as u16);
                }
                ConsoleCmd::Nop
            }
            ';' => {
                // Advance to next parameter.
                if self.param_count == 0 {
                    self.param_count = 1; // First param was implicitly 0.
                }
                if self.param_count < MAX_PARAMS {
                    self.param_count += 1;
                }
                ConsoleCmd::Nop
            }
            '?' => {
                self.state = EscState::CsiPrivate;
                ConsoleCmd::Nop
            }
            // Final byte (0x40–0x7E) — dispatch the CSI sequence.
            c if (c as u32) >= 0x40 && (c as u32) <= 0x7E => {
                let cmd = self.dispatch_csi(c);
                self.state = EscState::Normal;
                cmd
            }
            _ => {
                // Malformed sequence — discard and return to normal.
                self.reset();
                ConsoleCmd::Nop
            }
        }
    }

    fn process_csi_private(&mut self, c: char) -> ConsoleCmd {
        match c {
            '0'..='9' => {
                if self.param_count == 0 {
                    self.param_count = 1;
                }
                let idx = self.param_count - 1;
                if idx < MAX_PARAMS {
                    self.params[idx] = self.params[idx]
                        .saturating_mul(10)
                        .saturating_add(c as u16 - b'0' as u16);
                }
                ConsoleCmd::Nop
            }
            ';' => {
                if self.param_count == 0 {
                    self.param_count = 1;
                }
                if self.param_count < MAX_PARAMS {
                    self.param_count += 1;
                }
                ConsoleCmd::Nop
            }
            c if (c as u32) >= 0x40 && (c as u32) <= 0x7E => {
                let cmd = self.dispatch_csi_private(c);
                self.state = EscState::Normal;
                cmd
            }
            _ => {
                self.reset();
                ConsoleCmd::Nop
            }
        }
    }

    fn dispatch_csi(&self, final_byte: char) -> ConsoleCmd {
        match final_byte {
            // CUU — Cursor Up
            'A' => ConsoleCmd::CursorUp(self.param(0, 1)),
            // CUD — Cursor Down
            'B' => ConsoleCmd::CursorDown(self.param(0, 1)),
            // CUF — Cursor Forward
            'C' => ConsoleCmd::CursorForward(self.param(0, 1)),
            // CUB — Cursor Back
            'D' => ConsoleCmd::CursorBack(self.param(0, 1)),
            // CHA — Cursor Horizontal Absolute
            'G' => ConsoleCmd::CursorHorizontalAbsolute(self.param(0, 1)),
            // CUP — Cursor Position
            'H' => ConsoleCmd::CursorPosition(self.param(0, 1), self.param(1, 1)),
            // ED — Erase in Display
            'J' => ConsoleCmd::EraseDisplay(self.param(0, 0)),
            // EL — Erase in Line
            'K' => ConsoleCmd::EraseLine(self.param(0, 0)),
            // SGR — Select Graphic Rendition
            'm' => {
                let count = if self.param_count == 0 {
                    1
                } else {
                    self.param_count
                };
                let mut sgr = SgrParams {
                    params: [0; MAX_PARAMS],
                    count,
                };
                // If no params were given, treat as SGR 0 (reset).
                if self.param_count == 0 {
                    sgr.params[0] = 0;
                } else {
                    sgr.params[..self.param_count]
                        .copy_from_slice(&self.params[..self.param_count]);
                }
                ConsoleCmd::Sgr(sgr)
            }
            _ => ConsoleCmd::Nop,
        }
    }

    fn dispatch_csi_private(&self, final_byte: char) -> ConsoleCmd {
        match final_byte {
            // DECTCEM — cursor visibility
            'h' => {
                if self.param(0, 0) == 25 {
                    ConsoleCmd::SetCursorVisible(true)
                } else {
                    ConsoleCmd::Nop
                }
            }
            'l' => {
                if self.param(0, 0) == 25 {
                    ConsoleCmd::SetCursorVisible(false)
                } else {
                    ConsoleCmd::Nop
                }
            }
            _ => ConsoleCmd::Nop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> alloc::vec::Vec<ConsoleCmd> {
        let mut parser = AnsiParser::new();
        s.chars().map(|c| parser.process_char(c)).collect()
    }

    fn parse_str_last(s: &str) -> ConsoleCmd {
        let mut parser = AnsiParser::new();
        let mut last = ConsoleCmd::Nop;
        for c in s.chars() {
            last = parser.process_char(c);
        }
        last
    }

    #[test]
    fn test_printable_chars() {
        let cmds = parse_str("AB");
        assert_eq!(cmds, &[ConsoleCmd::PutChar('A'), ConsoleCmd::PutChar('B')]);
    }

    #[test]
    fn test_control_chars() {
        assert_eq!(parse_str_last("\r"), ConsoleCmd::CarriageReturn);
        assert_eq!(parse_str_last("\n"), ConsoleCmd::Newline);
        assert_eq!(parse_str_last("\x08"), ConsoleCmd::Backspace);
        assert_eq!(parse_str_last("\t"), ConsoleCmd::Tab);
    }

    #[test]
    fn test_erase_display_2j() {
        let cmd = parse_str_last("\x1b[2J");
        assert_eq!(cmd, ConsoleCmd::EraseDisplay(2));
    }

    #[test]
    fn test_cursor_position() {
        let cmd = parse_str_last("\x1b[10;20H");
        assert_eq!(cmd, ConsoleCmd::CursorPosition(10, 20));
    }

    #[test]
    fn test_cursor_position_default() {
        let cmd = parse_str_last("\x1b[H");
        assert_eq!(cmd, ConsoleCmd::CursorPosition(1, 1));
    }

    #[test]
    fn test_dectcem_hide() {
        let cmd = parse_str_last("\x1b[?25l");
        assert_eq!(cmd, ConsoleCmd::SetCursorVisible(false));
    }

    #[test]
    fn test_dectcem_show() {
        let cmd = parse_str_last("\x1b[?25h");
        assert_eq!(cmd, ConsoleCmd::SetCursorVisible(true));
    }

    #[test]
    fn test_sgr_reset() {
        let cmd = parse_str_last("\x1b[m");
        if let ConsoleCmd::Sgr(sgr) = cmd {
            assert_eq!(sgr.count, 1);
            assert_eq!(sgr.params[0], 0);
        } else {
            panic!("Expected Sgr, got {:?}", cmd);
        }
    }

    #[test]
    fn test_sgr_explicit_reset() {
        let cmd = parse_str_last("\x1b[0m");
        if let ConsoleCmd::Sgr(sgr) = cmd {
            assert_eq!(sgr.count, 1);
            assert_eq!(sgr.params[0], 0);
        } else {
            panic!("Expected Sgr, got {:?}", cmd);
        }
    }

    #[test]
    fn test_sgr_color() {
        let cmd = parse_str_last("\x1b[31;42m");
        if let ConsoleCmd::Sgr(sgr) = cmd {
            assert_eq!(sgr.count, 2);
            assert_eq!(sgr.params[0], 31);
            assert_eq!(sgr.params[1], 42);
        } else {
            panic!("Expected Sgr, got {:?}", cmd);
        }
    }

    #[test]
    fn test_malformed_escape_recovery() {
        // ESC followed by a non-[ character should discard and return to normal.
        let cmds = parse_str("\x1bXA");
        assert_eq!(
            cmds,
            &[
                ConsoleCmd::Nop, // ESC
                ConsoleCmd::Nop, // 'X' — discarded, back to Normal
                ConsoleCmd::PutChar('A'),
            ]
        );
    }

    #[test]
    fn test_cursor_movement() {
        assert_eq!(parse_str_last("\x1b[5A"), ConsoleCmd::CursorUp(5));
        assert_eq!(parse_str_last("\x1b[3B"), ConsoleCmd::CursorDown(3));
        assert_eq!(parse_str_last("\x1b[C"), ConsoleCmd::CursorForward(1));
        assert_eq!(parse_str_last("\x1b[2D"), ConsoleCmd::CursorBack(2));
        assert_eq!(
            parse_str_last("\x1b[15G"),
            ConsoleCmd::CursorHorizontalAbsolute(15)
        );
    }

    #[test]
    fn test_erase_in_line() {
        assert_eq!(parse_str_last("\x1b[K"), ConsoleCmd::EraseLine(0));
        assert_eq!(parse_str_last("\x1b[0K"), ConsoleCmd::EraseLine(0));
        assert_eq!(parse_str_last("\x1b[1K"), ConsoleCmd::EraseLine(1));
        assert_eq!(parse_str_last("\x1b[2K"), ConsoleCmd::EraseLine(2));
    }

    #[test]
    fn test_erase_in_display() {
        assert_eq!(parse_str_last("\x1b[J"), ConsoleCmd::EraseDisplay(0));
        assert_eq!(parse_str_last("\x1b[0J"), ConsoleCmd::EraseDisplay(0));
        assert_eq!(parse_str_last("\x1b[2J"), ConsoleCmd::EraseDisplay(2));
    }

    #[test]
    fn test_interleaved_text_and_escapes() {
        let cmds = parse_str("A\x1b[2JB");
        assert_eq!(
            cmds,
            &[
                ConsoleCmd::PutChar('A'),
                ConsoleCmd::Nop,             // ESC
                ConsoleCmd::Nop,             // [
                ConsoleCmd::Nop,             // 2
                ConsoleCmd::EraseDisplay(2), // J
                ConsoleCmd::PutChar('B'),
            ]
        );
    }

    #[test]
    fn test_unknown_csi_sequence() {
        // Unknown final byte 'z' — should produce Nop.
        assert_eq!(parse_str_last("\x1b[5z"), ConsoleCmd::Nop);
    }

    #[test]
    fn test_state_after_sequence() {
        // After a complete sequence, parser should be back in Normal.
        let mut parser = AnsiParser::new();
        for c in "\x1b[2J".chars() {
            parser.process_char(c);
        }
        assert_eq!(parser.state, EscState::Normal);
    }
}
